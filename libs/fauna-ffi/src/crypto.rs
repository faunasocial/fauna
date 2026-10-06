//! UniFFI export for backup-key derivation.
//!
//! Exposes [`backup_key_derive`] so native apps (Windows/macOS/iOS/Android)
//! can derive the owner's [`fauna_core::crypto::BackupKey`] from the 32-byte
//! Ed25519 identity seed without the seed leaving the app process.
//!
//! Phase-1 capability doctrine: only the derived `BackupKey` is handed to
//! the sync service; the seed stays in the client.
//! See `docs/goal/behavior/file-sync.md` § On-Demand Files and
//! `docs/goal/architecture/key-material-hierarchy.md` rule #7.

use crate::FfiError;

/// Derive the owner's `BackupKey` from a 32-byte Ed25519 identity seed.
///
/// Uses BLAKE3 key derivation with the domain-separated context string
/// `"fauna backup encryption key 2026-03-12"` so the backup key is
/// cryptographically independent from the signing key even though both
/// come from the same seed.
///
/// # Errors
/// Returns [`FfiError::General`] when `secret` is not exactly 32 bytes.
#[uniffi::export]
pub fn backup_key_derive(secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let arr = secret32(&secret)?;
    Ok(fauna_core::crypto::BackupKey::derive(&arr)
        .to_bytes()
        .to_vec())
}

/// The one owner-secret length check for the whole crate — every FFI surface
/// that takes the 32-byte identity seed as `Vec<u8>`/`&[u8]` coerces through
/// here, rather than keeping a per-module copy that could disagree about what
/// "32 bytes" means. (Five modules had grown their own by 2026-08-03, and two
/// glob-exported copies made `crate::secret32` ambiguous — one owner is the
/// structural fix, not just the polite one.)
pub(crate) fn secret32(owner_secret: &[u8]) -> Result<[u8; 32], FfiError> {
    owner_secret.try_into().map_err(|_| FfiError::General {
        msg: "owner secret must be 32 bytes".to_string(),
    })
}

/// `secret32`'s twin for the 32-byte device id. Gated to the union of its
/// consumer surfaces (they sit behind two *different* features, which is how
/// the crate grew two copies of it) so the Go mail-bridge
/// `--no-default-features` build, which drops both, does not carry a dead
/// helper.
#[cfg(any(feature = "sync-engine-host", feature = "backup-destinations"))]
pub(crate) fn device32(device_id: &[u8]) -> Result<[u8; 32], FfiError> {
    device_id.try_into().map_err(|_| FfiError::General {
        msg: "device id must be 32 bytes".to_string(),
    })
}

/// The general form of `secret32`/`device32`: a fail-closed 32-byte length
/// check for any other FFI input, labeled with the caller's own field name in
/// the error message. `mail.rs` (`array_32`) and `file_provider_host.rs`
/// (`to_32`) had each grown a byte-for-byte identical private copy of this —
/// the same anti-pattern `secret32`'s own doc comment above already names.
pub(crate) fn bytes32(bytes: &[u8], name: &str) -> Result<[u8; 32], FfiError> {
    bytes.try_into().map_err(|_| FfiError::General {
        msg: format!("{name} must be 32 bytes, got {}", bytes.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::crypto::BackupKey;

    #[test]
    fn backup_key_derive_matches_core() {
        let seed = [0x01u8; 32];
        let ffi_result = backup_key_derive(seed.to_vec()).unwrap();
        let core_result = BackupKey::derive(&seed).to_bytes().to_vec();
        assert_eq!(ffi_result, core_result);
    }

    #[test]
    fn backup_key_derive_rejects_non_32_byte_input() {
        for bad_len in [0usize, 31, 33] {
            let input = vec![0x00u8; bad_len];
            assert!(
                backup_key_derive(input).is_err(),
                "expected Err for input of length {bad_len}"
            );
        }
    }

    #[test]
    fn secret32_rejects_wrong_length() {
        assert!(secret32(&[0u8; 31]).is_err());
        assert!(secret32(&[0u8; 33]).is_err());
        assert!(secret32(&[7u8; 32]).is_ok());
    }

    #[test]
    fn bytes32_rejects_wrong_length() {
        assert!(bytes32(&[0u8; 31], "x").is_err());
        assert!(bytes32(&[0u8; 33], "x").is_err());
        assert!(bytes32(&[7u8; 32], "x").is_ok());
    }

    #[cfg(any(feature = "sync-engine-host", feature = "backup-destinations"))]
    #[test]
    fn device32_rejects_wrong_length() {
        assert!(device32(&[0u8; 16]).is_err());
        assert!(device32(&[0u8; 33]).is_err());
        assert!(device32(&[7u8; 32]).is_ok());
    }
}
