//! CLASSETS sync engine.
//!
//! Planned modules (see `docs/` and the build plan): `index` (SQLite), `watch`, `scan`,
//! `hash`, `transfer` (presigned R2 links), `api` (signed requests), `direct` (QUIC).
//! The engine never holds R2 keys; it only ever receives one-file presigned URLs.

pub mod rules;

/// Version of the engine crate.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
