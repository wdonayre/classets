//! Where the device keeps its secrets: device secret key, device id, token, server URL.
//!
//! [`FileStore`] works everywhere (a JSON file, `0600` on unix). [`KeyringStore`] uses the
//! OS keychain and is behind the `keyring` cargo feature.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Secret key names used by the engine.
pub mod keys {
    pub const DEVICE_SECRET_KEY: &str = "device_secret_key";
    pub const DEVICE_ID: &str = "device_id";
    pub const TOKEN: &str = "token";
    pub const SERVER_URL: &str = "server_url";
    /// Pairing in progress (cleared when it ends).
    pub const PAIRING_ID: &str = "pairing_id";
    /// Every key the engine may write, for a full wipe.
    pub const ALL: &[&str] = &[DEVICE_SECRET_KEY, DEVICE_ID, TOKEN, SERVER_URL, PAIRING_ID];
}

/// Errors from a secret store.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret store I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("secret store file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("keychain: {0}")]
    Keychain(String),
}

/// A small key/value store for device secrets.
pub trait SecretStore: Send + Sync {
    /// Returns the value for `key`, or `None` when unset.
    fn get(&self, key: &str) -> Result<Option<String>, SecretError>;
    /// Sets `key` to `value`.
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;
    /// Removes `key` (no error when it was not set).
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

/// Keeps what a store returned in memory, so the engine does not go back to the OS keychain
/// (which can block on a system prompt) on every request. Writes go straight through.
pub struct CachedStore {
    inner: Box<dyn SecretStore>,
    cache: Mutex<BTreeMap<String, Option<String>>>,
}

impl CachedStore {
    pub fn new(inner: Box<dyn SecretStore>) -> Self {
        Self { inner, cache: Mutex::new(BTreeMap::new()) }
    }
}

impl SecretStore for CachedStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        if let Some(v) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
            return Ok(v.clone());
        }
        let v = self.inner.get(key)?;
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), v.clone());
        Ok(v)
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.set(key, value)?;
        cache.insert(key.to_string(), Some(value.to_string()));
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.delete(key)?;
        cache.insert(key.to_string(), None);
        Ok(())
    }
}

/// Secrets in a JSON file (`secrets.json` in the engine's data dir).
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    /// A store backed by `path` (created on first write).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    /// The default file inside `data_dir`.
    pub fn in_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("secrets.json"))
    }

    fn load(&self) -> Result<BTreeMap<String, String>, SecretError> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(BTreeMap::new()),
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(map)?)?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let _g = self.guard();
        Ok(self.load()?.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let _g = self.guard();
        let mut map = self.load()?;
        map.insert(key.to_string(), value.to_string());
        self.save(&map)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let _g = self.guard();
        let mut map = self.load()?;
        if map.remove(key).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }
}

/// Secrets in the OS keychain (macOS Keychain, Windows Credential Manager, Secret Service).
#[cfg(feature = "keyring")]
pub struct KeyringStore {
    service: String,
}

#[cfg(feature = "keyring")]
impl KeyringStore {
    /// `service` namespaces the entries, e.g. `"classets"` or `"classets:<data dir hash>"`.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, key).map_err(|e| SecretError::Keychain(e.to_string()))
    }
}

#[cfg(feature = "keyring")]
impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keychain(e.to_string())),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.entry(key)?
            .set_password(value)
            .map_err(|e| SecretError::Keychain(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keychain(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts reads so the cache can be checked.
    struct Counting(FileStore, std::sync::atomic::AtomicUsize);
    impl SecretStore for Counting {
        fn get(&self, k: &str) -> Result<Option<String>, SecretError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.0.get(k)
        }
        fn set(&self, k: &str, v: &str) -> Result<(), SecretError> { self.0.set(k, v) }
        fn delete(&self, k: &str) -> Result<(), SecretError> { self.0.delete(k) }
    }

    #[test]
    fn cached_store_reads_each_key_once_and_writes_through() {
        let dir = tempfile::tempdir().unwrap();
        let store = CachedStore::new(Box::new(Counting(FileStore::in_dir(dir.path()), Default::default())));
        assert_eq!(store.get("token").unwrap(), None);
        assert_eq!(store.get("token").unwrap(), None);
        store.set("token", "abc").unwrap();
        assert_eq!(store.get("token").unwrap().as_deref(), Some("abc"));
        store.delete("token").unwrap();
        assert_eq!(store.get("token").unwrap(), None);
        // The file really changed (write-through).
        assert_eq!(FileStore::in_dir(dir.path()).get("token").unwrap(), None);
        store.set("token", "xyz").unwrap();
        assert_eq!(FileStore::in_dir(dir.path()).get("token").unwrap().as_deref(), Some("xyz"));
    }

    #[test]
    fn file_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::in_dir(dir.path());
        assert_eq!(store.get("token").unwrap(), None);
        store.set("token", "abc").unwrap();
        store.set("device_id", "d1").unwrap();
        assert_eq!(store.get("token").unwrap().as_deref(), Some("abc"));
        store.delete("token").unwrap();
        assert_eq!(store.get("token").unwrap(), None);
        assert_eq!(
            FileStore::in_dir(dir.path())
                .get("device_id")
                .unwrap()
                .as_deref(),
            Some("d1")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("secrets.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
