//! Ed25519 signature verification for release artifacts.

use anyhow::{Result, bail};
use ed25519_dalek::{Signature, VerifyingKey};

/// Verify an Ed25519 signature over artifact bytes.
///
/// `public_keys` supports key rotation: the transitional release
/// compiles in two keys and accepts if either validates.
pub fn verify_signature(
    artifact: &[u8],
    signature_bytes: &[u8],
    public_keys: &[[u8; 32]],
) -> Result<()> {
    let sig = Signature::from_slice(signature_bytes)
        .map_err(|e| anyhow::anyhow!("invalid signature format: {e}"))?;

    for key_bytes in public_keys {
        if let Ok(key) = VerifyingKey::from_bytes(key_bytes)
            && key.verify_strict(artifact, &sig).is_ok()
        {
            return Ok(());
        }
    }
    bail!("signature verification failed: no matching key")
}

/// Verify SHA-256 checksum from a SHA256SUMS file.
pub fn verify_sha256(artifact: &[u8], artifact_name: &str, sums_content: &str) -> Result<()> {
    use sha2::{Digest, Sha256};

    let expected = sums_content
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?;
            if name == artifact_name || name.trim_start_matches('*') == artifact_name {
                Some(hash.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow::anyhow!("artifact {artifact_name} not found in SHA256SUMS"))?;

    let mut hasher = Sha256::new();
    hasher.update(artifact);
    let actual = hex::encode(hasher.finalize());

    if actual != expected {
        bail!("SHA-256 mismatch for {artifact_name}: expected {expected}, got {actual}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_keypair() -> (SigningKey, [u8; 32]) {
        let secret = [42u8; 32];
        let signing = SigningKey::from_bytes(&secret);
        let public = signing.verifying_key().to_bytes();
        (signing, public)
    }

    #[test]
    fn valid_signature_passes() {
        let (signing, public) = test_keypair();
        let data = b"hello world";
        let sig = signing.sign(data);
        verify_signature(data, &sig.to_bytes(), &[public]).unwrap();
    }

    #[test]
    fn invalid_signature_fails() {
        let (_signing, public) = test_keypair();
        let data = b"hello world";
        let bad_sig = [0u8; 64];
        assert!(verify_signature(data, &bad_sig, &[public]).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let (signing, _public) = test_keypair();
        let wrong_key = [99u8; 32];
        let data = b"hello world";
        let sig = signing.sign(data);
        assert!(verify_signature(data, &sig.to_bytes(), &[wrong_key]).is_err());
    }

    #[test]
    fn key_rotation_accepts_either_key() {
        let (signing_old, public_old) = test_keypair();
        let secret_new = [7u8; 32];
        let signing_new = SigningKey::from_bytes(&secret_new);
        let public_new = signing_new.verifying_key().to_bytes();

        let data = b"release artifact";
        let sig = signing_old.sign(data);

        verify_signature(data, &sig.to_bytes(), &[public_old, public_new]).unwrap();
    }

    #[test]
    fn sha256_valid_checksum_passes() {
        use sha2::{Digest, Sha256};
        let data = b"test binary content";
        let hash = hex::encode(Sha256::digest(data));
        let sums = format!("{hash}  fauna-sync-v1.0.0-linux-x86_64\n");
        verify_sha256(data, "fauna-sync-v1.0.0-linux-x86_64", &sums).unwrap();
    }

    #[test]
    fn sha256_mismatch_fails() {
        let sums = "0000000000000000000000000000000000000000000000000000000000000000  fauna-sync-v1.0.0-linux-x86_64\n";
        assert!(verify_sha256(b"actual data", "fauna-sync-v1.0.0-linux-x86_64", sums).is_err());
    }

    #[test]
    fn sha256_missing_artifact_fails() {
        let sums = "abcdef  other-binary\n";
        assert!(verify_sha256(b"data", "fauna-sync-v1.0.0-linux-x86_64", sums).is_err());
    }

    #[test]
    fn signature_of_the_wrong_length_fails_with_invalid_format() {
        let (_signing, public) = test_keypair();
        let data = b"hello world";
        let too_short = [0u8; 10];
        let err = verify_signature(data, &too_short, &[public])
            .expect_err("a 10-byte signature is not a valid Ed25519 signature format");
        assert!(
            err.to_string().contains("invalid signature format"),
            "got: {err}"
        );
    }

    #[test]
    fn sha256_matches_a_star_prefixed_binary_mode_name() {
        use sha2::{Digest, Sha256};
        let data = b"test binary content";
        let hash = hex::encode(Sha256::digest(data));
        let sums = format!("{hash} *fauna-sync-v1.0.0-linux-x86_64\n");
        verify_sha256(data, "fauna-sync-v1.0.0-linux-x86_64", &sums).unwrap();
    }
}
