//! At-rest seal/unseal for an `__mls` replica blob (`provider` /
//! `history/<channel_hex>`).
//!
//! # The at-rest pipeline (matched byte-for-byte against the nest)
//!
//! `docs/goal/behavior/file-sync.md` § MLS state replica: each replica blob is
//! sealed on the client through the standard blob pipeline (`compress_chunk` =
//! zstd → `encrypt_backup_chunk` = ChaCha20-Poly1305 under the owner's
//! `BackupKey`), and the nest stores the result **raw-opaque** per `(actor_id,
//! path_hash)`. This crate is the client half — the *first and only* consumer of
//! the `fauna.mls.{get,put}` plane:
//!
//! ```text
//! seal:    canonical bytes  →  compress_chunk (zstd)  →  encrypt_backup_chunk (ChaCha20-Poly1305)
//! unseal:  decrypt_backup_chunk  →  decompress_chunk  →  canonical bytes
//! ```
//!
//! The input is already the canonical CBOR that `ProviderReplica::to_bytes` /
//! `ChannelHistorySlice::to_bytes` produced (`fauna-mls` / `fauna-conversations`),
//! so — like `fauna-client-drafts`' seal — there is no struct-encode step here
//! and **no signature wrapper**: replica blobs are owner-only content
//! (`key-material-hierarchy.md` § Audience: owner only) and the ChaCha20-Poly1305
//! AEAD already authenticates the bytes. There is **no HTTP** here.
//!
//! **Zeroize the plaintext.** The
//! `provider` snapshot's plaintext is the openMLS provider KV = live group
//! secrets (ratchet/secret trees, epoch keys). [`unseal_replica`] therefore
//! returns the plaintext in a [`Zeroizing`] buffer so it is wiped on drop, and
//! callers seal from a `Zeroizing` plaintext too (`store.rs`), closing the
//! window the raw `Vec<u8>` from `to_bytes()` (`state_replica.rs:98`) would
//! otherwise leave.

use fauna_core::crypto::{BackupKey, seal_backup_chunk, unseal_backup_chunk};
use thiserror::Error;
use zeroize::Zeroizing;

/// Failures from sealing/unsealing an `__mls` replica blob. Mirrors
/// `fauna_client_drafts::DraftSealError`; `compress_chunk` and the input bytes
/// are infallible, so there is no encode/compress-failed variant on the seal
/// path.
#[derive(Debug, Error)]
pub enum ReplicaSealError {
    /// ChaCha20-Poly1305 sealing failed (internal AEAD fault).
    #[error("sealing mls replica blob failed: {0}")]
    Encrypt(String),

    /// ChaCha20-Poly1305 unsealing failed — wrong `BackupKey`, a
    /// tampered/truncated blob, or an unrecognized version byte.
    #[error("unsealing mls replica blob failed (wrong key or tampered data): {0}")]
    Decrypt(String),

    /// zstd decompression of the unsealed payload failed.
    #[error("decompressing mls replica payload failed: {0}")]
    Decompress(String),
}

/// Derive a [`BackupKey`] from a 32-byte Ed25519 identity seed.
///
/// Thin wrapper over [`fauna_core::crypto::BackupKey::derive`] so callers need
/// not reach into `fauna_core::crypto` directly. The same seed always yields the
/// same key, so every device in the user's fleet seals/unseals against an
/// identical key — the cross-device property this whole plane rests on. Mirrors
/// `fauna_client_drafts::backup_key_from_seed`.
pub fn backup_key_from_seed(seed: &[u8; 32]) -> BackupKey {
    BackupKey::derive(seed)
}

/// Produce the raw-opaque blob bytes the nest stores for an `__mls` replica path.
///
/// Pipeline: `compress_chunk` (zstd, self-describing prefix) →
/// `encrypt_backup_chunk` (ChaCha20-Poly1305 under `key`). Byte-for-byte what
/// the nest content-addresses on, and the inverse of [`unseal_replica`]. The
/// returned bytes are ciphertext (safe to hold/log-length); the caller should
/// pass `plaintext` from a [`Zeroizing`] buffer.
pub fn seal_replica(plaintext: &[u8], key: &BackupKey) -> Result<Vec<u8>, ReplicaSealError> {
    seal_backup_chunk(plaintext, key, ReplicaSealError::Encrypt)
}

/// Inverse of [`seal_replica`]: decrypt → decompress → the canonical plaintext,
/// wrapped in [`Zeroizing`] so the group secrets it may carry are wiped on drop.
/// The caller decodes it into a `ProviderReplica` /
/// `ChannelHistorySlice` and drops the buffer.
pub fn unseal_replica(
    blob: &[u8],
    key: &BackupKey,
) -> Result<Zeroizing<Vec<u8>>, ReplicaSealError> {
    let plain = unseal_backup_chunk(
        blob,
        key,
        ReplicaSealError::Decrypt,
        ReplicaSealError::Decompress,
    )?;
    Ok(Zeroizing::new(plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_unseal_round_trips() {
        let key = BackupKey::derive(&[3u8; 32]);
        let plain = b"canonical provider replica snapshot bytes".repeat(8);
        let sealed = seal_replica(&plain, &key).unwrap();
        // Sealed carries the ChaCha20 version byte and differs from plaintext.
        assert_eq!(sealed[0], 0x01);
        assert_ne!(sealed, plain);
        assert_eq!(unseal_replica(&sealed, &key).unwrap().as_slice(), plain);
    }

    #[test]
    fn unseal_wrong_key_fails_rather_than_masking() {
        let key = BackupKey::derive(&[3u8; 32]);
        let other = BackupKey::derive(&[4u8; 32]);
        let sealed = seal_replica(b"provider secrets", &key).unwrap();
        assert!(matches!(
            unseal_replica(&sealed, &other),
            Err(ReplicaSealError::Decrypt(_))
        ));
    }

    /// A second device (same identity seed → same `BackupKey`) unseals what the
    /// first sealed — the cross-device property, at the seal layer.
    #[test]
    fn second_device_same_seed_unseals() {
        let key_a = backup_key_from_seed(&[9u8; 32]);
        let key_b = backup_key_from_seed(&[9u8; 32]);
        let sealed = seal_replica(b"from device A", &key_a).unwrap();
        assert_eq!(
            unseal_replica(&sealed, &key_b).unwrap().as_slice(),
            b"from device A"
        );
    }
}
