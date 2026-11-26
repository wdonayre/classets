//! Device key pair and the signed-request scheme from `docs/api.md`.
//!
//! String to sign: `METHOD\nPATH_WITH_QUERY\nTIMESTAMP\nsha256hex(body)` (no trailing newline).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use sha2::{Digest, Sha256};

/// Errors decoding stored keys or signatures.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid key or signature length")]
    Length,
    #[error("bad signature")]
    BadSignature,
}

/// The device's ed25519 key pair. The secret key never leaves the device.
#[derive(Clone)]
pub struct DeviceKey {
    signing: SigningKey,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey")
            .field("public_key", &self.public_key_b64())
            .finish_non_exhaustive()
    }
}

impl DeviceKey {
    /// Generates a fresh key pair from the OS random source.
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Restores a key from its base64 32-byte secret (as produced by [`Self::secret_b64`]).
    pub fn from_secret_b64(s: &str) -> Result<Self, CryptoError> {
        let bytes = B64.decode(s.trim())?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| CryptoError::Length)?;
        Ok(Self {
            signing: SigningKey::from_bytes(&arr),
        })
    }

    /// Base64 of the 32-byte secret key, for the secret store.
    pub fn secret_b64(&self) -> String {
        B64.encode(self.signing.to_bytes())
    }

    /// Base64 of the 32-byte public key, sent to `/pair/start`.
    pub fn public_key_b64(&self) -> String {
        B64.encode(self.signing.verifying_key().to_bytes())
    }

    /// Signs `message`; returns the base64 signature.
    pub fn sign_b64(&self, message: &[u8]) -> String {
        B64.encode(self.signing.sign(message).to_bytes())
    }

    /// Signs a request as the API expects; returns the base64 signature.
    pub fn sign_request(
        &self,
        method: &str,
        path_with_query: &str,
        timestamp: i64,
        body: &[u8],
    ) -> String {
        self.sign_b64(string_to_sign(method, path_with_query, timestamp, body).as_bytes())
    }
}

/// Builds the API's string to sign.
pub fn string_to_sign(method: &str, path_with_query: &str, timestamp: i64, body: &[u8]) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path_with_query,
        timestamp,
        sha256_hex(body)
    )
}

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Verifies a base64 signature against a base64 public key (used by tests and tools).
pub fn verify_b64(
    public_key_b64: &str,
    message: &[u8],
    signature_b64: &str,
) -> Result<(), CryptoError> {
    let pk: [u8; 32] = B64
        .decode(public_key_b64)?
        .try_into()
        .map_err(|_| CryptoError::Length)?;
    let sig: [u8; 64] = B64
        .decode(signature_b64)?
        .try_into()
        .map_err(|_| CryptoError::Length)?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|_| CryptoError::Length)?;
    vk.verify(message, &Signature::from_bytes(&sig))
        .map_err(|_| CryptoError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_round_trip() {
        let key = DeviceKey::generate();
        let sts = string_to_sign("post", "/api/v1/files/intent", 1_764_000_000, b"{\"a\":1}");
        let sig = key.sign_b64(sts.as_bytes());
        verify_b64(&key.public_key_b64(), sts.as_bytes(), &sig).expect("valid signature");
        assert!(verify_b64(&key.public_key_b64(), b"tampered", &sig).is_err());
        assert_eq!(B64.decode(key.public_key_b64()).unwrap().len(), 32);
    }

    #[test]
    fn secret_round_trip() {
        let key = DeviceKey::generate();
        let again = DeviceKey::from_secret_b64(&key.secret_b64()).unwrap();
        assert_eq!(key.public_key_b64(), again.public_key_b64());
    }

    #[test]
    fn string_to_sign_format() {
        let s = string_to_sign("GET", "/api/v1/changes?since=0&limit=500", 42, b"");
        assert_eq!(
            s,
            "GET\n/api/v1/changes?since=0&limit=500\n42\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
