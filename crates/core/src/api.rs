//! Client for the CLASSETS sync API v1 (`docs/api.md`).
//!
//! Every endpoint except pairing is signed: `Authorization`, `X-Classets-Device`,
//! `X-Classets-Timestamp` and `X-Classets-Signature` are added automatically.
//! Presigned transfer links (upload/download) are plain, unsigned requests.

use std::collections::BTreeMap;
use std::path::Path;

use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::crypto::DeviceKey;

/// Typed API failures.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// `401`: bad token, pending device, bad signature or clock skew.
    #[error("unauthorized")]
    Unauthorized,
    /// `401 {"error":"revoked"}`: the device was revoked; `wipe` asks to delete synced files.
    #[error("device revoked (wipe: {wipe})")]
    Revoked { wipe: bool },
    /// `409`: the file changed since the version the device knew.
    #[error("conflict (current version {current_version_id:?})")]
    Conflict { current_version_id: Option<i64> },
    /// `403`: no edit access.
    #[error("forbidden")]
    Forbidden,
    /// `422 {"error":"mismatch"}`: stored object size/hash differ; upload again.
    #[error("uploaded object does not match size/hash")]
    Mismatch,
    /// Any other non-success status.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// Connection, TLS, timeout or local I/O while transferring.
    #[error("network: {0}")]
    Network(String),
    /// The response body was not what the contract describes.
    #[error("unexpected response: {0}")]
    Decode(String),
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_decode() {
            ApiError::Decode(e.to_string())
        } else {
            ApiError::Network(e.to_string())
        }
    }
}

/// Credentials for signed requests.
#[derive(Clone, Debug)]
pub struct DeviceAuth {
    pub device_id: String,
    pub token: String,
    pub key: DeviceKey,
}

// ---- request / response types ----

/// Body of `POST /pair/start`.
#[derive(Debug, Clone, Serialize)]
pub struct PairStartRequest {
    pub name: String,
    pub os: String,
    pub app_version: String,
    pub public_key: String,
    pub kind: String,
}

/// `201` of `POST /pair/start`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairStartResponse {
    pub pairing_id: String,
    pub user_code: String,
    pub verification_url: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    3
}

/// A person as returned by the API.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct User {
    #[serde(default)]
    pub id: serde_json::Value,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// A workspace as returned by the API.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Workspace {
    #[serde(default)]
    pub id: serde_json::Value,
    #[serde(default)]
    pub name: String,
}

/// Result of `POST /pair/poll`.
#[derive(Debug, Clone)]
pub enum PairPoll {
    /// `202`: not approved yet.
    Pending,
    /// `200`: approved; the token is handed out once.
    Approved {
        device_id: String,
        token: String,
        user: User,
        workspace: Workspace,
    },
    /// `410`: `expired`, `denied` or `claimed`.
    Gone { status: String },
}

/// `GET /me` device block.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MeDevice {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub kind: String,
}

/// `GET /me`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Me {
    pub device: MeDevice,
    #[serde(default)]
    pub user: User,
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub role: String,
}

/// One folder from `GET /folders`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Folder {
    pub id: i64,
    pub path: String,
    /// `edit` or `view`.
    pub access: String,
    /// `cloud_direct` or `direct_only`.
    #[serde(default)]
    pub travel: String,
    /// `sync`, `on_demand` or `off`.
    pub mode: String,
}

impl Folder {
    /// True when the device keeps a local copy of this folder.
    pub fn is_synced(&self) -> bool {
        self.mode == "sync"
    }
    /// True when local changes may be pushed.
    pub fn can_push(&self) -> bool {
        self.is_synced() && self.access == "edit"
    }
    /// True when `path` sits in this folder (or is the folder itself).
    pub fn contains(&self, path: &str) -> bool {
        let base = self.path.trim_matches('/');
        base.is_empty()
            || path == base
            || path.strip_prefix(base).is_some_and(|r| r.starts_with('/'))
    }
}

#[derive(Deserialize)]
struct FoldersResponse {
    folders: Vec<Folder>,
}

/// One entry from `GET /changes`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Change {
    pub id: i64,
    pub path: String,
    #[serde(default)]
    pub deleted: bool,
    pub version_id: i64,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub modified_at: Option<String>,
}

/// A page of `GET /changes`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChangesPage {
    pub changes: Vec<Change>,
    pub cursor: i64,
    #[serde(default)]
    pub has_more: bool,
}

/// Body of `POST /files/intent`.
#[derive(Debug, Clone, Serialize)]
pub struct FileIntent {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub modified_at: String,
    pub base_version_id: Option<i64>,
}

/// `200` of `POST /files/intent`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum IntentResponse {
    /// The server already had these bytes.
    Done { version_id: i64 },
    /// Send the bytes to `url`, then commit `upload_id`.
    Upload {
        upload_id: String,
        url: String,
        #[serde(default = "default_put")]
        method: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        expires_in: u64,
    },
}

fn default_put() -> String {
    "PUT".into()
}

/// `200` of `POST /files/commit`.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitResponse {
    pub version_id: i64,
    #[serde(default)]
    pub path: String,
}

/// `200` of `GET /files/versions/{id}/download`.
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadLink {
    pub url: String,
    #[serde(default)]
    pub expires_in: u64,
}

/// Body of `POST /heartbeat`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Heartbeat {
    pub pending_up: u64,
    pub pending_down: u64,
    pub local_bytes: u64,
}

/// A folder entry from `GET /browse`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BrowseFolder {
    pub name: String,
    pub path: String,
}

/// A file entry from `GET /browse`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BrowseFile {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub version_id: Option<i64>,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub thumb_url: Option<String>,
}

/// `GET /browse`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Browse {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub folders: Vec<BrowseFolder>,
    #[serde(default)]
    pub files: Vec<BrowseFile>,
    /// Whether this person may upload into `path`.
    #[serde(default)]
    pub can_upload: bool,
}

/// One entry of `GET /pairings/pending`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PendingPairing {
    pub user_code: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub requested_by: serde_json::Value,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Deserialize)]
struct PendingPairingsResponse {
    pairings: Vec<PendingPairing>,
}

#[derive(Deserialize)]
struct ApproveResponse {
    device_id: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    wipe: Option<bool>,
    #[serde(default)]
    current_version_id: Option<i64>,
}

// ---- client ----

/// API client bound to one server; signed when [`DeviceAuth`] is set.
#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    base: Url,
    auth: Option<DeviceAuth>,
}

impl ApiClient {
    /// A client for `server` (e.g. `https://classets.example.com`), with no auth.
    pub fn new(http: reqwest::Client, server: &str) -> Result<Self, ApiError> {
        let mut base = Url::parse(server.trim_end_matches('/'))
            .map_err(|e| ApiError::Decode(format!("bad server URL {server}: {e}")))?;
        let path = format!("{}/api/v1/", base.path().trim_end_matches('/'));
        base.set_path(&path);
        Ok(Self {
            http,
            base,
            auth: None,
        })
    }

    /// Returns a copy that signs every request with `auth`.
    pub fn with_auth(mut self, auth: DeviceAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// The shared HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn url(&self, rel: &str, query: &[(&str, String)]) -> Result<Url, ApiError> {
        let mut url = self
            .base
            .join(rel.trim_start_matches('/'))
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        if !query.is_empty() {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
        }
        Ok(url)
    }

    /// Sends a request; signs it when auth is set. Returns status + body bytes.
    async fn send(
        &self,
        method: Method,
        rel: &str,
        query: &[(&str, String)],
        body: Option<&serde_json::Value>,
    ) -> Result<(StatusCode, Vec<u8>), ApiError> {
        let url = self.url(rel, query)?;
        let body_bytes = match body {
            Some(v) => serde_json::to_vec(v).map_err(|e| ApiError::Decode(e.to_string()))?,
            None => Vec::new(),
        };
        let mut req = self
            .http
            .request(method.clone(), url.clone())
            .header(reqwest::header::ACCEPT, "application/json");
        if body.is_some() {
            req = req.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        if let Some(auth) = &self.auth {
            let mut path = url.path().to_string();
            if let Some(q) = url.query() {
                path.push('?');
                path.push_str(q);
            }
            let ts = chrono::Utc::now().timestamp();
            let sig = auth
                .key
                .sign_request(method.as_str(), &path, ts, &body_bytes);
            req = req
                .bearer_auth(&auth.token)
                .header("X-Classets-Device", &auth.device_id)
                .header("X-Classets-Timestamp", ts.to_string())
                .header("X-Classets-Signature", sig);
        }
        if body.is_some() {
            req = req.body(body_bytes);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?.to_vec();
        Ok((status, bytes))
    }

    /// Sends and decodes a success body, mapping error statuses to [`ApiError`].
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        rel: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<T, ApiError> {
        let (status, bytes) = self.send(method, rel, query, body.as_ref()).await?;
        if status.is_success() {
            decode(&bytes)
        } else {
            Err(map_error(status, &bytes))
        }
    }

    // ---- pairing (unsigned) ----

    /// `POST /pair/start`.
    pub async fn pair_start(&self, req: &PairStartRequest) -> Result<PairStartResponse, ApiError> {
        self.call(Method::POST, "pair/start", &[], Some(to_value(req)?))
            .await
    }

    /// `POST /pair/poll`.
    pub async fn pair_poll(&self, pairing_id: &str) -> Result<PairPoll, ApiError> {
        let body = serde_json::json!({ "pairing_id": pairing_id });
        let (status, bytes) = self
            .send(Method::POST, "pair/poll", &[], Some(&body))
            .await?;
        #[derive(Deserialize)]
        struct Approved {
            device_id: String,
            token: String,
            #[serde(default)]
            user: User,
            #[serde(default)]
            workspace: Workspace,
        }
        #[derive(Deserialize)]
        struct StatusOnly {
            #[serde(default)]
            status: String,
        }
        match status.as_u16() {
            202 => Ok(PairPoll::Pending),
            200 => {
                let v: serde_json::Value = decode(&bytes)?;
                if v.get("status").and_then(|s| s.as_str()) == Some("pending") {
                    return Ok(PairPoll::Pending);
                }
                let a: Approved =
                    serde_json::from_value(v).map_err(|e| ApiError::Decode(e.to_string()))?;
                Ok(PairPoll::Approved {
                    device_id: a.device_id,
                    token: a.token,
                    user: a.user,
                    workspace: a.workspace,
                })
            }
            410 => {
                let s: StatusOnly = decode(&bytes).unwrap_or(StatusOnly {
                    status: "expired".into(),
                });
                Ok(PairPoll::Gone { status: s.status })
            }
            _ => Err(map_error(status, &bytes)),
        }
    }

    // ---- signed endpoints ----

    /// `GET /me`.
    pub async fn me(&self) -> Result<Me, ApiError> {
        self.call(Method::GET, "me", &[], None).await
    }

    /// `GET /folders`.
    pub async fn folders(&self) -> Result<Vec<Folder>, ApiError> {
        let r: FoldersResponse = self.call(Method::GET, "folders", &[], None).await?;
        Ok(r.folders)
    }

    /// `GET /changes?since=&limit=`.
    pub async fn changes(&self, since: i64, limit: u32) -> Result<ChangesPage, ApiError> {
        let q = [
            ("since", since.to_string()),
            ("limit", limit.clamp(1, 1000).to_string()),
        ];
        self.call(Method::GET, "changes", &q, None).await
    }

    /// `POST /files/intent`.
    pub async fn file_intent(&self, intent: &FileIntent) -> Result<IntentResponse, ApiError> {
        self.call(Method::POST, "files/intent", &[], Some(to_value(intent)?))
            .await
    }

    /// `POST /files/commit`.
    pub async fn file_commit(&self, upload_id: &str) -> Result<CommitResponse, ApiError> {
        let body = serde_json::json!({ "upload_id": upload_id });
        self.call(Method::POST, "files/commit", &[], Some(body))
            .await
    }

    /// `GET /files/versions/{id}/download`.
    pub async fn download_url(&self, version_id: i64) -> Result<DownloadLink, ApiError> {
        self.call(
            Method::GET,
            &format!("files/versions/{version_id}/download"),
            &[],
            None,
        )
        .await
    }

    /// `POST /files/delete` (moves the file to Trash).
    pub async fn delete(&self, path: &str, version_id: i64) -> Result<(), ApiError> {
        let body = serde_json::json!({ "path": path, "version_id": version_id });
        let _: serde_json::Value = self
            .call(Method::POST, "files/delete", &[], Some(body))
            .await?;
        Ok(())
    }

    /// `POST /heartbeat`.
    pub async fn heartbeat(&self, hb: &Heartbeat) -> Result<(), ApiError> {
        let _: serde_json::Value = self
            .call(Method::POST, "heartbeat", &[], Some(to_value(hb)?))
            .await?;
        Ok(())
    }

    /// `GET /browse?path=` (empty path = root).
    pub async fn browse(&self, path: &str) -> Result<Browse, ApiError> {
        self.call(Method::GET, "browse", &[("path", path.to_string())], None)
            .await
    }

    /// `GET /pairings/pending` (owner/admin).
    pub async fn pending_pairings(&self) -> Result<Vec<PendingPairing>, ApiError> {
        let r: PendingPairingsResponse = self
            .call(Method::GET, "pairings/pending", &[], None)
            .await?;
        Ok(r.pairings)
    }

    /// `POST /pairings/{user_code}/approve`; returns the new device id.
    pub async fn approve_pairing(&self, user_code: &str) -> Result<String, ApiError> {
        let r: ApproveResponse = self
            .call(
                Method::POST,
                &format!("pairings/{}/approve", encode_segment(user_code)),
                &[],
                Some(serde_json::json!({})),
            )
            .await?;
        Ok(r.device_id)
    }

    /// `POST /pairings/{user_code}/deny`.
    pub async fn deny_pairing(&self, user_code: &str) -> Result<(), ApiError> {
        let _: serde_json::Value = self
            .call(
                Method::POST,
                &format!("pairings/{}/deny", encode_segment(user_code)),
                &[],
                Some(serde_json::json!({})),
            )
            .await?;
        Ok(())
    }

    // ---- transfers (presigned, unsigned) ----

    /// Sends the file at `local` to a presigned link with the given method and headers.
    pub async fn upload(
        &self,
        method: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
        local: &Path,
    ) -> Result<(), ApiError> {
        let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        let file = tokio::fs::File::open(local).await.map_err(io_err)?;
        let len = file.metadata().await.map_err(io_err)?.len();
        let mut req = self.http.request(method, url);
        let mut has_len = false;
        for (k, v) in headers {
            has_len |= k.eq_ignore_ascii_case("content-length");
            req = req.header(k, v);
        }
        if !has_len {
            // Presigned PUTs need a length; streaming bodies would otherwise go chunked.
            req = req.header(reqwest::header::CONTENT_LENGTH, len);
        }
        let resp = req.body(reqwest::Body::from(file)).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ApiError::Http {
                status: status.as_u16(),
                body,
            })
        }
    }

    /// Downloads `url` into `dest`: streams to a temp file next to it, checks the hash
    /// (when given), then renames into place. Returns the SHA-256 of the bytes.
    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        expect_sha256: Option<&str>,
    ) -> Result<String, ApiError> {
        let dir = dest
            .parent()
            .ok_or_else(|| ApiError::Network("destination has no parent".into()))?;
        tokio::fs::create_dir_all(dir).await.map_err(io_err)?;
        let mut resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Http {
                status: status.as_u16(),
                body,
            });
        }
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // `.tmp` keeps the partial file out of scans (rules::DEFAULT_SKIP).
        let tmp = dir.join(format!(".{name}.classets-{}.tmp", crate::random_hex(6)));
        let result: Result<String, ApiError> = async {
            let mut file = tokio::fs::File::create(&tmp).await.map_err(io_err)?;
            let mut hasher = Sha256::new();
            while let Some(chunk) = resp.chunk().await? {
                hasher.update(&chunk);
                file.write_all(&chunk).await.map_err(io_err)?;
            }
            file.flush().await.map_err(io_err)?;
            file.sync_all().await.map_err(io_err)?;
            drop(file);
            let got = hex::encode(hasher.finalize());
            if let Some(want) = expect_sha256 {
                if !want.eq_ignore_ascii_case(&got) {
                    return Err(ApiError::Mismatch);
                }
            }
            tokio::fs::rename(&tmp, dest).await.map_err(io_err)?;
            Ok(got)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        result
    }
}

fn io_err(e: std::io::Error) -> ApiError {
    ApiError::Network(format!("local I/O: {e}"))
}

fn to_value<T: Serialize>(v: &T) -> Result<serde_json::Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::Decode(e.to_string()))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let bytes = if bytes.is_empty() {
        b"null".as_slice()
    } else {
        bytes
    };
    serde_json::from_slice(bytes).map_err(|e| ApiError::Decode(e.to_string()))
}

fn encode_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.~".contains(c) {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Maps a non-success response to [`ApiError`].
fn map_error(status: StatusCode, bytes: &[u8]) -> ApiError {
    let body: Option<ErrorBody> = serde_json::from_slice(bytes).ok();
    let error = body
        .as_ref()
        .and_then(|b| b.error.clone())
        .unwrap_or_default();
    match status.as_u16() {
        401 if error == "revoked" => ApiError::Revoked {
            wipe: body.and_then(|b| b.wipe).unwrap_or(false),
        },
        401 => ApiError::Unauthorized,
        403 => ApiError::Forbidden,
        409 => ApiError::Conflict {
            current_version_id: body.and_then(|b| b.current_version_id),
        },
        422 if error == "mismatch" => ApiError::Mismatch,
        s => ApiError::Http {
            status: s,
            body: String::from_utf8_lossy(bytes).into_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_errors() {
        assert!(matches!(
            map_error(
                StatusCode::UNAUTHORIZED,
                br#"{"error":"revoked","wipe":true}"#
            ),
            ApiError::Revoked { wipe: true }
        ));
        assert!(matches!(
            map_error(StatusCode::UNAUTHORIZED, b""),
            ApiError::Unauthorized
        ));
        assert!(matches!(
            map_error(
                StatusCode::CONFLICT,
                br#"{"error":"conflict","current_version_id":90}"#
            ),
            ApiError::Conflict {
                current_version_id: Some(90)
            }
        ));
        assert!(matches!(
            map_error(StatusCode::UNPROCESSABLE_ENTITY, br#"{"error":"mismatch"}"#),
            ApiError::Mismatch
        ));
        assert!(matches!(
            map_error(StatusCode::UNPROCESSABLE_ENTITY, b"{}"),
            ApiError::Http { status: 422, .. }
        ));
    }

    #[test]
    fn folder_contains() {
        let f = Folder {
            id: 1,
            path: "Clients/Alab".into(),
            access: "edit".into(),
            travel: String::new(),
            mode: "sync".into(),
        };
        assert!(f.contains("Clients/Alab/a.jpg"));
        assert!(!f.contains("Clients/Alabaster/a.jpg"));
    }

    #[test]
    fn base_url_keeps_subpath() {
        let c = ApiClient::new(reqwest::Client::new(), "https://x.test/sub/").unwrap();
        assert_eq!(
            c.url("changes", &[("since", "0".into())]).unwrap().as_str(),
            "https://x.test/sub/api/v1/changes?since=0"
        );
    }
}
