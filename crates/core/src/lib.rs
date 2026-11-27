//! Classets sync engine.
//!
//! Modules: [`crypto`] (device key, request signing), [`secrets`] (where keys and tokens
//! live), [`api`] (signed client for `docs/api.md`), [`index`] (SQLite), [`scan`] (walk +
//! hash), [`engine`] (pairing, sync passes, watch loop), [`rules`] (skip patterns).
//! Planned: `direct` (QUIC device-to-device).
//! The engine never holds R2 keys; it only ever receives one-file presigned URLs.

pub mod api;
pub mod crypto;
pub mod engine;
pub mod index;
pub mod rules;
pub mod scan;
pub mod secrets;

pub use api::{ApiClient, ApiError};
pub use engine::{
    Engine, EngineConfig, EngineError, EngineStatus, PairStart, PairStatus, SyncReport,
};
#[cfg(feature = "keyring")]
pub use secrets::KeyringStore;
pub use secrets::{FileStore, SecretStore};

/// Version of the engine crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `2 * bytes` random lowercase hex characters (temp file names).
pub(crate) fn random_hex(bytes: usize) -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}
