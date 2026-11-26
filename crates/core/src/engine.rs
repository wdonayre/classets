//! The sync engine: pairing, one-pass two-way sync, and the watch loop.
//!
//! A pass pulls remote changes first (from the stored `/changes` cursor), then scans the
//! sync root and pushes local new/changed/deleted files. When both sides changed a file,
//! both copies are kept: the local one is renamed to a conflict copy and uploaded as new.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::api::{
    ApiClient, ApiError, Change, DeviceAuth, FileIntent, Folder, Heartbeat, IntentResponse,
    PairPoll, PairStartRequest,
};
use crate::crypto::{CryptoError, DeviceKey};
use crate::index::{FileRow, Index};
use crate::scan::{self, Known, LocalFile, abs_path, normalize, rel_path};
use crate::secrets::{FileStore, SecretError, SecretStore, keys};

const CURSOR_KEY: &str = "cursor";
const LAST_SYNC_KEY: &str = "last_sync";
const LAST_REPORT_KEY: &str = "last_report";
const CHANGES_PAGE: u32 = 500;
const IDLE_WAIT: Duration = Duration::from_secs(30);
const DEBOUNCE: Duration = Duration::from_millis(1500);

/// Engine failures that stop a pass. Per-file problems go into [`SyncReport::errors`].
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("this device is not paired")]
    NotPaired,
    #[error("no pairing in progress; call pair_start first")]
    NoPairing,
    #[error("device revoked (synced files wiped: {wipe})")]
    Revoked { wipe: bool },
    #[error(transparent)]
    Api(ApiError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error(transparent)]
    Secrets(#[from] SecretError),
    #[error("device key: {0}")]
    Crypto(#[from] CryptoError),
    #[error("file watcher: {0}")]
    Watch(#[from] notify::Error),
    #[error("config: {0}")]
    Config(String),
}

impl From<ApiError> for EngineError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Revoked { wipe } => EngineError::Revoked { wipe },
            other => EngineError::Api(other),
        }
    }
}

impl EngineError {
    /// True when the pass must stop (auth lost, local storage broken).
    fn is_fatal(&self) -> bool {
        matches!(
            self,
            EngineError::NotPaired
                | EngineError::Revoked { .. }
                | EngineError::Api(ApiError::Unauthorized)
                | EngineError::Index(_)
                | EngineError::Secrets(_)
        )
    }
}

/// Engine settings. Saved as `config.json` in the data dir after pairing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineConfig {
    /// Server origin, e.g. `https://classets.example.com` (the client adds `/api/v1`).
    pub server_url: String,
    /// Local folder that mirrors the workspace.
    pub sync_root: PathBuf,
    /// Engine state: `index.db`, `secrets.json`, `config.json`.
    pub data_dir: PathBuf,
    /// Device name shown on the web and used in conflict copies.
    pub device_name: String,
}

impl EngineConfig {
    fn file(data_dir: &Path) -> PathBuf {
        data_dir.join("config.json")
    }

    /// Loads `config.json` from `data_dir`.
    pub fn load(data_dir: &Path) -> Result<Self, EngineError> {
        let bytes = std::fs::read(Self::file(data_dir))?;
        serde_json::from_slice(&bytes).map_err(|e| EngineError::Config(e.to_string()))
    }

    /// Writes `config.json` into the data dir.
    pub fn save(&self) -> Result<(), EngineError> {
        std::fs::create_dir_all(&self.data_dir)?;
        let json =
            serde_json::to_vec_pretty(self).map_err(|e| EngineError::Config(e.to_string()))?;
        std::fs::write(Self::file(&self.data_dir), json)?;
        Ok(())
    }
}

/// What the person sees while pairing.
#[derive(Debug, Clone, Serialize)]
pub struct PairStart {
    pub user_code: String,
    pub verification_url: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
}

/// Outcome of [`Engine::pair_poll`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PairStatus {
    Pending,
    Approved {
        device_id: String,
    },
    /// Expired, denied or already claimed (`reason` is the server's status).
    Expired {
        reason: String,
    },
}

/// Result of one sync pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncReport {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub conflicts: u32,
    pub errors: Vec<String>,
}

/// Snapshot for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct EngineStatus {
    pub paired: bool,
    pub device_id: Option<String>,
    /// ISO-8601 UTC time of the last completed pass.
    pub last_sync: Option<String>,
    pub last_report: Option<SyncReport>,
    /// Last pass-stopping error, if the last attempt failed.
    pub last_error: Option<String>,
}

/// The sync engine. Cheap to share behind an `Arc`; passes are serialised internally.
pub struct Engine {
    config: EngineConfig,
    http: reqwest::Client,
    secrets: Box<dyn SecretStore>,
    index: Mutex<Index>,
    last_error: Mutex<Option<String>>,
    pass_lock: tokio::sync::Mutex<()>,
}

impl Engine {
    /// Opens the engine with a [`FileStore`] for secrets in the data dir.
    pub fn new(config: EngineConfig) -> Result<Self, EngineError> {
        let store = FileStore::in_dir(&config.data_dir);
        Self::with_secret_store(config, Box::new(store))
    }

    /// Opens the engine with a custom secret store (e.g. `KeyringStore`).
    pub fn with_secret_store(
        config: EngineConfig,
        secrets: Box<dyn SecretStore>,
    ) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&config.data_dir)?;
        let index = Index::open(&config.data_dir.join("index.db"))?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("classets/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| EngineError::Api(ApiError::Network(e.to_string())))?;
        Ok(Self {
            config,
            http,
            secrets,
            index: Mutex::new(index),
            last_error: Mutex::new(None),
            pass_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// The engine's configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    // ---- pairing ----

    /// Creates a device key and starts pairing. Show `user_code` and `verification_url`.
    pub async fn pair_start(&self) -> Result<PairStart, EngineError> {
        let key = DeviceKey::generate();
        let os = std::env::consts::OS.to_string();
        let kind = if matches!(os.as_str(), "ios" | "android") {
            "mobile"
        } else {
            "desktop"
        };
        let req = PairStartRequest {
            name: self.config.device_name.clone(),
            os,
            app_version: crate::version().to_string(),
            public_key: key.public_key_b64(),
            kind: kind.to_string(),
        };
        let resp = ApiClient::new(self.http.clone(), &self.config.server_url)?
            .pair_start(&req)
            .await?;
        for k in [keys::DEVICE_ID, keys::TOKEN] {
            self.secrets.delete(k)?;
        }
        self.secrets
            .set(keys::DEVICE_SECRET_KEY, &key.secret_b64())?;
        self.secrets.set(keys::PAIRING_ID, &resp.pairing_id)?;
        self.secrets
            .set(keys::SERVER_URL, &self.config.server_url)?;
        Ok(PairStart {
            user_code: resp.user_code,
            verification_url: resp.verification_url,
            expires_in: resp.expires_in,
            interval: resp.interval,
        })
    }

    /// Polls the pairing once. On approval stores device id + token (the key is already stored).
    pub async fn pair_poll(&self) -> Result<PairStatus, EngineError> {
        let pairing_id = self
            .secrets
            .get(keys::PAIRING_ID)?
            .ok_or(EngineError::NoPairing)?;
        let api = ApiClient::new(self.http.clone(), &self.server_url()?)?;
        match api.pair_poll(&pairing_id).await? {
            PairPoll::Pending => Ok(PairStatus::Pending),
            PairPoll::Approved {
                device_id, token, ..
            } => {
                self.secrets.set(keys::DEVICE_ID, &device_id)?;
                self.secrets.set(keys::TOKEN, &token)?;
                self.secrets.delete(keys::PAIRING_ID)?;
                self.with_index(|i| i.clear())?;
                Ok(PairStatus::Approved { device_id })
            }
            PairPoll::Gone { status } => {
                self.secrets.delete(keys::PAIRING_ID)?;
                self.secrets.delete(keys::DEVICE_SECRET_KEY)?;
                Ok(PairStatus::Expired { reason: status })
            }
        }
    }

    /// True when a device id, token and key are stored.
    pub fn is_paired(&self) -> bool {
        self.auth().is_ok()
    }

    /// A signed API client for this device.
    pub fn api(&self) -> Result<ApiClient, EngineError> {
        Ok(ApiClient::new(self.http.clone(), &self.server_url()?)?.with_auth(self.auth()?))
    }

    // ---- status ----

    /// Paired state plus the last pass's time and report.
    pub fn status(&self) -> EngineStatus {
        let auth = self.auth().ok();
        let last_sync = self
            .with_index(|i| i.get_state(LAST_SYNC_KEY))
            .ok()
            .flatten();
        let last_report = self
            .with_index(|i| i.get_state(LAST_REPORT_KEY))
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok());
        EngineStatus {
            paired: auth.is_some(),
            device_id: auth.map(|a| a.device_id),
            last_sync,
            last_report,
            last_error: lock(&self.last_error).clone(),
        }
    }

    // ---- sync ----

    /// One full pass: pull remote changes, then push local changes.
    pub async fn sync_once(&self) -> Result<SyncReport, EngineError> {
        let _pass = self.pass_lock.lock().await;
        let result = self.pass().await;
        match &result {
            Ok(report) => {
                let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
                let json = serde_json::to_string(report).unwrap_or_default();
                self.with_index(|i| {
                    i.set_state(LAST_SYNC_KEY, &now)?;
                    i.set_state(LAST_REPORT_KEY, &json)
                })?;
                *lock(&self.last_error) = None;
            }
            Err(EngineError::Revoked { wipe }) => {
                self.handle_revoked(*wipe)?;
                *lock(&self.last_error) = Some(format!("device revoked (wipe: {wipe})"));
            }
            Err(e) => *lock(&self.last_error) = Some(e.to_string()),
        }
        result
    }

    /// Sends a heartbeat with the local byte count.
    pub async fn heartbeat(&self) -> Result<(), EngineError> {
        let local_bytes = self.with_index(|i| i.total_bytes())?;
        let hb = Heartbeat {
            pending_up: 0,
            pending_down: 0,
            local_bytes,
        };
        match self.api()?.heartbeat(&hb).await {
            Ok(()) => Ok(()),
            Err(ApiError::Revoked { wipe }) => {
                self.handle_revoked(wipe)?;
                Err(EngineError::Revoked { wipe })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Syncs, waits for a file change (debounced) or 30 s, and repeats until `shutdown`
    /// resolves. Sends a heartbeat after each pass. Returns on shutdown, unpairing or revoke.
    pub async fn run_loop(&self, shutdown: impl Future<Output = ()>) -> Result<(), EngineError> {
        use notify::Watcher as _;
        tokio::pin!(shutdown);
        std::fs::create_dir_all(&self.config.sync_root)?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let root = self.config.sync_root.clone();
        let data_dir = self.config.data_dir.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let relevant = ev.paths.iter().any(|p| {
                        !p.starts_with(&data_dir)
                            && rel_path(&root, p).is_some_and(|rel| !scan::skipped(&rel, false))
                    });
                    if relevant {
                        let _ = tx.send(());
                    }
                }
            })?;
        watcher.watch(&self.config.sync_root, notify::RecursiveMode::Recursive)?;

        loop {
            match self.sync_once().await {
                Ok(_) => {}
                Err(e @ (EngineError::Revoked { .. } | EngineError::NotPaired)) => return Err(e),
                Err(_) => {} // recorded in status().last_error; retry next round
            }
            if let Err(e @ EngineError::Revoked { .. }) = self.heartbeat().await {
                return Err(e);
            }
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                _ = tokio::time::sleep(IDLE_WAIT) => {}
                Some(()) = rx.recv() => {
                    // Debounce: wait until the tree is quiet for DEBOUNCE.
                    loop {
                        tokio::select! {
                            _ = &mut shutdown => return Ok(()),
                            r = tokio::time::timeout(DEBOUNCE, rx.recv()) => {
                                if !matches!(r, Ok(Some(()))) { break; }
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- internals ----

    fn with_index<T>(
        &self,
        f: impl FnOnce(&Index) -> rusqlite::Result<T>,
    ) -> Result<T, EngineError> {
        Ok(f(&lock(&self.index))?)
    }

    fn server_url(&self) -> Result<String, EngineError> {
        Ok(self
            .secrets
            .get(keys::SERVER_URL)?
            .unwrap_or_else(|| self.config.server_url.clone()))
    }

    fn auth(&self) -> Result<DeviceAuth, EngineError> {
        let get = |k| {
            self.secrets
                .get(k)
                .ok()
                .flatten()
                .ok_or(EngineError::NotPaired)
        };
        let key = DeviceKey::from_secret_b64(&get(keys::DEVICE_SECRET_KEY)?)?;
        Ok(DeviceAuth {
            device_id: get(keys::DEVICE_ID)?,
            token: get(keys::TOKEN)?,
            key,
        })
    }

    /// Sign this device out: forget its key, token and index. Synced files stay on disk.
    pub fn sign_out(&self) -> Result<(), EngineError> {
        self.handle_revoked(false)
    }

    /// Revoked: drop secrets and index; with `wipe`, also delete the sync root's contents.
    fn handle_revoked(&self, wipe: bool) -> Result<(), EngineError> {
        if wipe && self.config.sync_root.is_dir() {
            for entry in std::fs::read_dir(&self.config.sync_root)? {
                let p = entry?.path();
                if p == self.config.data_dir || self.config.data_dir.starts_with(&p) {
                    continue; // never delete our own state dir if it sits inside the root
                }
                let meta = std::fs::symlink_metadata(&p)?;
                if meta.is_dir() {
                    std::fs::remove_dir_all(&p)?
                } else {
                    std::fs::remove_file(&p)?
                }
            }
        }
        for k in keys::ALL {
            self.secrets.delete(k)?;
        }
        self.with_index(|i| i.clear())?;
        Ok(())
    }

    async fn pass(&self) -> Result<SyncReport, EngineError> {
        let api = self.api()?;
        let mut report = SyncReport::default();
        let folders = api.folders().await?;
        tokio::fs::create_dir_all(&self.config.sync_root).await?;
        self.pull(&api, &folders, &mut report).await?;
        self.push(&api, &folders, &mut report).await?;
        Ok(report)
    }

    /// Records a per-file error, or returns it when the pass must stop.
    fn soft(report: &mut SyncReport, what: &str, e: EngineError) -> Result<(), EngineError> {
        if e.is_fatal() {
            return Err(e);
        }
        report.errors.push(format!("{what}: {e}"));
        Ok(())
    }

    // ---- pull ----

    async fn pull(
        &self,
        api: &ApiClient,
        folders: &[Folder],
        report: &mut SyncReport,
    ) -> Result<(), EngineError> {
        let mut cursor: i64 = self
            .with_index(|i| i.get_state(CURSOR_KEY))?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        loop {
            let page = api.changes(cursor, CHANGES_PAGE).await?;
            for change in &page.changes {
                let path = normalize(&change.path);
                let synced = folders.iter().any(|f| f.is_synced() && f.contains(&path));
                if synced && !scan::skipped(&path, false) {
                    if let Err(e) = self.apply_change(api, change, &path, report).await {
                        // Stop at the first failure so the change is retried next pass.
                        Self::soft(report, &format!("pull {path}"), e)?;
                        self.with_index(|i| i.set_state(CURSOR_KEY, &cursor.to_string()))?;
                        return Ok(());
                    }
                }
                cursor = cursor.max(change.id);
            }
            cursor = cursor.max(page.cursor);
            self.with_index(|i| i.set_state(CURSOR_KEY, &cursor.to_string()))?;
            if !page.has_more || page.changes.is_empty() {
                return Ok(());
            }
        }
    }

    async fn apply_change(
        &self,
        api: &ApiClient,
        c: &Change,
        path: &str,
        report: &mut SyncReport,
    ) -> Result<(), EngineError> {
        let local = abs_path(&self.config.sync_root, path);
        let row = self.with_index(|i| i.get(path))?;
        let local_state = if local.is_file() {
            Some(stat_async(&local, row.as_ref().map(known_of)).await?)
        } else {
            None
        };

        if c.deleted {
            match local_state {
                None => self.with_index(|i| i.remove(path))?,
                Some((size, mtime_ns, sha)) => {
                    if row.as_ref().and_then(|r| r.synced_sha256.as_deref()) == Some(sha.as_str()) {
                        tokio::fs::remove_file(&local).await?;
                        self.with_index(|i| i.remove(path))?;
                        report.deleted += 1;
                    } else {
                        // Local edits survive a remote delete; they upload as a new file.
                        let r = FileRow {
                            path: path.into(),
                            sha256: sha,
                            size,
                            mtime_ns,
                            version_id: None,
                            synced_sha256: None,
                        };
                        self.with_index(|i| i.upsert(&r))?;
                    }
                }
            }
            return Ok(());
        }

        let remote_sha = c
            .sha256
            .clone()
            .ok_or_else(|| ApiError::Decode(format!("change {} has no sha256", c.id)))?;
        // Already at (or past) this version, e.g. the echo of our own upload.
        if row
            .as_ref()
            .and_then(|r| r.version_id)
            .is_some_and(|v| v >= c.version_id)
        {
            return Ok(());
        }
        match local_state {
            Some((size, mtime_ns, sha)) if sha.eq_ignore_ascii_case(&remote_sha) => {
                let r = FileRow {
                    path: path.into(),
                    sha256: sha.clone(),
                    size,
                    mtime_ns,
                    version_id: Some(c.version_id),
                    synced_sha256: Some(sha),
                };
                self.with_index(|i| i.upsert(&r))?;
            }
            None => {
                self.download_to(api, c.version_id, path, Some(&remote_sha))
                    .await?;
                report.downloaded += 1;
            }
            Some((_, _, sha)) => {
                let local_edits = row
                    .as_ref()
                    .is_none_or(|r| r.synced_sha256.as_deref() != Some(sha.as_str()));
                if local_edits {
                    // Both changed: keep both. Local becomes a conflict copy, pushed later.
                    let conflict = self.unique_conflict_path(path);
                    tokio::fs::rename(&local, abs_path(&self.config.sync_root, &conflict)).await?;
                    report.conflicts += 1;
                }
                self.download_to(api, c.version_id, path, Some(&remote_sha))
                    .await?;
                report.downloaded += 1;
            }
        }
        Ok(())
    }

    /// Downloads a version into `path` and records it as synced.
    async fn download_to(
        &self,
        api: &ApiClient,
        version_id: i64,
        path: &str,
        sha: Option<&str>,
    ) -> Result<(), EngineError> {
        let link = api.download_url(version_id).await?;
        let dest = abs_path(&self.config.sync_root, path);
        let got = api.download(&link.url, &dest, sha).await?;
        let meta = tokio::fs::metadata(&dest).await?;
        let r = FileRow {
            path: path.into(),
            sha256: got.clone(),
            size: meta.len(),
            mtime_ns: scan::mtime_ns(&meta),
            version_id: Some(version_id),
            synced_sha256: Some(got),
        };
        self.with_index(|i| i.upsert(&r))?;
        Ok(())
    }

    // ---- push ----

    async fn push(
        &self,
        api: &ApiClient,
        folders: &[Folder],
        report: &mut SyncReport,
    ) -> Result<(), EngineError> {
        let rows: HashMap<String, FileRow> = self
            .with_index(|i| i.all())?
            .into_iter()
            .map(|r| (r.path.clone(), r))
            .collect();
        let known: HashMap<String, Known> =
            rows.iter().map(|(p, r)| (p.clone(), known_of(r))).collect();
        let root = self.config.sync_root.clone();
        let scanned = tokio::task::spawn_blocking(move || scan::scan(&root, &known))
            .await
            .map_err(|e| EngineError::Io(std::io::Error::other(e)))?;
        report.errors.extend(scanned.errors);
        let folder_of = |p: &str| {
            folders
                .iter()
                .filter(|f| f.is_synced() && f.contains(p))
                .max_by_key(|f| f.path.len())
        };

        let mut seen = HashSet::new();
        for file in &scanned.files {
            seen.insert(file.path.clone());
            let Some(folder) = folder_of(&file.path) else {
                continue;
            };
            let row = rows.get(&file.path);
            let unchanged =
                row.is_some_and(|r| r.synced_sha256.as_deref() == Some(file.sha256.as_str()));
            if unchanged || !folder.can_push() {
                // Refresh size/mtime/hash so the next scan can skip hashing.
                if let Some(r) = row.filter(|r| {
                    r.size != file.size || r.mtime_ns != file.mtime_ns || r.sha256 != file.sha256
                }) {
                    let r = FileRow {
                        sha256: file.sha256.clone(),
                        size: file.size,
                        mtime_ns: file.mtime_ns,
                        ..r.clone()
                    };
                    self.with_index(|i| i.upsert(&r))?;
                }
                continue;
            }
            let base = row.and_then(|r| r.version_id);
            if let Err(e) = self.push_file(api, file, base, report).await {
                Self::soft(report, &format!("push {}", file.path), e)?;
            }
        }

        // Local deletions: known, last synced, gone from disk.
        for (path, row) in &rows {
            if seen.contains(path) || abs_path(&self.config.sync_root, path).exists() {
                continue;
            }
            let pushable = folder_of(path).is_some_and(|f| f.can_push());
            match (pushable, row.version_id, row.synced_sha256.is_some()) {
                (true, Some(version_id), true) => match api.delete(path, version_id).await {
                    Ok(()) => {
                        self.with_index(|i| i.remove(path))?;
                        report.deleted += 1;
                    }
                    // Changed remotely since: drop our row; the newer version arrives via /changes.
                    Err(ApiError::Conflict { current_version_id }) => {
                        self.with_index(|i| i.remove(path))?;
                        if let Some(v) = current_version_id {
                            match self.download_to(api, v, path, None).await {
                                Ok(()) => report.downloaded += 1,
                                Err(e) => Self::soft(report, &format!("restore {path}"), e)?,
                            }
                        }
                    }
                    Err(e) => Self::soft(report, &format!("delete {path}"), e.into())?,
                },
                // Never synced, or a view-only folder: just forget the row.
                _ => self.with_index(|i| i.remove(path))?,
            }
        }
        Ok(())
    }

    /// Uploads one new/changed file; on `409` keeps both copies.
    async fn push_file(
        &self,
        api: &ApiClient,
        file: &LocalFile,
        base: Option<i64>,
        report: &mut SyncReport,
    ) -> Result<(), EngineError> {
        match self.upload_file(api, file, base).await {
            Ok(version_id) => {
                self.record_synced(file, version_id)?;
                report.uploaded += 1;
                Ok(())
            }
            Err(ApiError::Conflict { current_version_id }) => {
                report.conflicts += 1;
                let conflict = self.unique_conflict_path(&file.path);
                let root = &self.config.sync_root;
                tokio::fs::rename(abs_path(root, &file.path), abs_path(root, &conflict)).await?;
                self.with_index(|i| i.remove(&file.path))?;
                let renamed = LocalFile {
                    path: conflict,
                    ..file.clone()
                };
                let version_id = self.upload_file(api, &renamed, None).await?;
                self.record_synced(&renamed, version_id)?;
                report.uploaded += 1;
                if let Some(v) = current_version_id {
                    self.download_to(api, v, &file.path, None).await?;
                    report.downloaded += 1;
                }
                Ok(())
            }
            Err(ApiError::Forbidden) => {
                report
                    .errors
                    .push(format!("push {}: no edit access (skipped)", file.path));
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// intent → (PUT → commit) → new version id. Retries the PUT once on a mismatch.
    async fn upload_file(
        &self,
        api: &ApiClient,
        file: &LocalFile,
        base: Option<i64>,
    ) -> Result<i64, ApiError> {
        let intent = FileIntent {
            path: file.path.clone(),
            sha256: file.sha256.clone(),
            size: file.size,
            modified_at: iso_from_ns(file.mtime_ns),
            base_version_id: base,
        };
        match api.file_intent(&intent).await? {
            IntentResponse::Done { version_id } => Ok(version_id),
            IntentResponse::Upload {
                upload_id,
                url,
                method,
                headers,
                ..
            } => {
                let local = abs_path(&self.config.sync_root, &file.path);
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    api.upload(&method, &url, &headers, &local).await?;
                    match api.file_commit(&upload_id).await {
                        Ok(c) => return Ok(c.version_id),
                        Err(ApiError::Mismatch) if attempts < 2 => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }

    fn record_synced(&self, file: &LocalFile, version_id: i64) -> Result<(), EngineError> {
        let r = FileRow {
            path: file.path.clone(),
            sha256: file.sha256.clone(),
            size: file.size,
            mtime_ns: file.mtime_ns,
            version_id: Some(version_id),
            synced_sha256: Some(file.sha256.clone()),
        };
        self.with_index(|i| i.upsert(&r))
    }

    /// A conflict-copy path for `path` that does not exist on disk yet.
    fn unique_conflict_path(&self, path: &str) -> String {
        let today = chrono::Local::now().date_naive();
        let mut n = 1;
        loop {
            let candidate = conflict_name(path, &self.config.device_name, today, n);
            if !abs_path(&self.config.sync_root, &candidate).exists() {
                return candidate;
            }
            n += 1;
        }
    }
}

/// Conflict copy name from `docs/api.md`: `{name} (conflict {DEVICE} {YYYY-MM-DD}){.ext}`.
/// `n > 1` appends a counter inside the brackets when that name is taken.
pub fn conflict_name(path: &str, device: &str, date: NaiveDate, n: u32) -> String {
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let device: String = device
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let counter = if n > 1 {
        format!(" {n}")
    } else {
        String::new()
    };
    format!(
        "{dir}{stem} (conflict {device} {}{counter}){ext}",
        date.format("%Y-%m-%d")
    )
}

fn known_of(r: &FileRow) -> Known {
    Known {
        size: r.size,
        mtime_ns: r.mtime_ns,
        sha256: r.sha256.clone(),
    }
}

async fn stat_async(path: &Path, known: Option<Known>) -> std::io::Result<(u64, i64, String)> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || scan::stat_file(&path, known.as_ref()))
        .await
        .map_err(std::io::Error::other)?
}

fn iso_from_ns(ns: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(
        ns.div_euclid(1_000_000_000),
        ns.rem_euclid(1_000_000_000) as u32,
    )
    .unwrap_or_default()
    .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        assert_eq!(
            conflict_name("Clients/Alab/cover.jpg", "WILLIAM-PC", d, 1),
            "Clients/Alab/cover (conflict WILLIAM-PC 2026-11-27).jpg"
        );
        assert_eq!(
            conflict_name("notes", "Mac", d, 1),
            "notes (conflict Mac 2026-11-27)"
        );
        assert_eq!(
            conflict_name(".env", "Mac", d, 1),
            ".env (conflict Mac 2026-11-27)"
        );
        assert_eq!(
            conflict_name("a/b.tar.gz", "My/PC", d, 2),
            "a/b.tar (conflict My-PC 2026-11-27 2).gz"
        );
    }

    #[test]
    fn iso_times() {
        assert_eq!(
            iso_from_ns(1_764_033_240_000_000_000),
            "2025-11-25T01:14:00Z"
        );
    }
}
