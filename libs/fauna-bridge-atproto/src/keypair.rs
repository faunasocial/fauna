//! ES256 (P-256) keypair generation and JWK serialization.
//!
//! Provides [`Es256Keypair`] for generating, persisting, and loading
//! NIST P-256 keypairs in JWK format, as required by ATProto OAuth.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// An ES256 (P-256) keypair stored as a JWK JSON value.
///
/// The inner value is the **private** JWK (contains `"d"`).
/// Use [`public_jwk`](Self::public_jwk) to get the public-only form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Es256Keypair {
    jwk: serde_json::Value,
}

impl Es256Keypair {
    /// Generate a fresh random P-256 keypair.
    pub fn generate() -> Result<Self> {
        use p256::elliptic_curve::rand_core::OsRng;

        let secret_key = p256::SecretKey::random(&mut OsRng);
        let jwk_string = secret_key.to_jwk_string();
        let jwk: serde_json::Value =
            serde_json::from_str(&jwk_string).context("failed to parse generated JWK")?;

        Ok(Self { jwk })
    }

    /// Load a keypair from a JSON file, or generate a new one and save it.
    ///
    /// If `path` exists, the file is read and deserialized. Otherwise a new
    /// keypair is generated, written to `path`, and returned.
    pub fn load_or_generate(path: &Path) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read keypair from {}", path.display()))?;
            let kp: Self = serde_json::from_str(&data)
                .with_context(|| format!("failed to parse keypair from {}", path.display()))?;
            Ok(kp)
        } else {
            let kp = Self::generate()?;
            let data = serde_json::to_string_pretty(&kp).context("failed to serialize keypair")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create parent directory for {}", path.display())
                })?;
            }
            std::fs::write(path, data.as_bytes())
                .with_context(|| format!("failed to write keypair to {}", path.display()))?;
            Ok(kp)
        }
    }

    /// Return the **public** JWK (without the `"d"` private key component).
    pub fn public_jwk(&self) -> serde_json::Value {
        let mut pub_jwk = self.jwk.clone();
        if let Some(obj) = pub_jwk.as_object_mut() {
            obj.remove("d");
        }
        pub_jwk
    }

    /// Return the full private JWK value.
    pub fn private_jwk(&self) -> &serde_json::Value {
        &self.jwk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_roundtrip() {
        let kp = Es256Keypair::generate().unwrap();
        let jwk = &kp.jwk;

        // Must be an EC key on P-256 with all components present
        assert_eq!(jwk["kty"], "EC");
        assert_eq!(jwk["crv"], "P-256");
        assert!(jwk["x"].is_string(), "missing x");
        assert!(jwk["y"].is_string(), "missing y");
        assert!(jwk["d"].is_string(), "missing d (private key)");

        // Round-trip through serde
        let json = serde_json::to_string(&kp).unwrap();
        let kp2: Es256Keypair = serde_json::from_str(&json).unwrap();
        assert_eq!(kp.jwk, kp2.jwk);
    }

    #[test]
    fn public_jwk_strips_private_key() {
        let kp = Es256Keypair::generate().unwrap();
        let pub_jwk = kp.public_jwk();

        assert_eq!(pub_jwk["kty"], "EC");
        assert_eq!(pub_jwk["crv"], "P-256");
        assert!(pub_jwk["x"].is_string());
        assert!(pub_jwk["y"].is_string());
        assert!(pub_jwk.get("d").is_none(), "public JWK must not contain d");
    }

    #[test]
    fn load_or_generate_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keypair.json");

        // First call creates the file
        assert!(!path.exists());
        let kp1 = Es256Keypair::load_or_generate(&path).unwrap();
        assert!(path.exists());

        // Second call loads the same keypair
        let kp2 = Es256Keypair::load_or_generate(&path).unwrap();
        assert_eq!(kp1.jwk, kp2.jwk);
    }
}
