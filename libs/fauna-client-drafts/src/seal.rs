//! At-rest seal/unseal for a `__drafts` rail blob.
//!
//! # The at-rest pipeline (matched byte-for-byte against the nest)
//!
//! `docs/goal/behavior/file-sync.md` § Drafts Sync step 2: the serialised draft
//! bytes go through the standard blob pipeline (`encode_blob()` =
//! zstd-compress → ChaCha20-Poly1305 under the owner's `BackupKey`), and the
//! nest stores the result opaque. This crate is the client half:
//!
//! ```text
//! seal:    snapshot_bytes  →  compress_chunk (zstd)  →  encrypt_backup_chunk (ChaCha20-Poly1305)
//! unseal:  decrypt_backup_chunk  →  decompress_chunk  →  snapshot_bytes
//! ```
//!
//! The input is already the canonical bytes [`DraftStore::snapshot_bytes`]
//! produced (`libs/fauna-conversations`), so there is no
//! struct-encode step here, and **no signature wrapper**: drafts are owner-only
//! content (`key-material-hierarchy.md` § Audience: owner only Path A) and the
//! ChaCha20-Poly1305 AEAD already authenticates the bytes. The unsealed
//! plaintext is handed straight back to `DraftStore::restore_from_bytes`, which
//! owns the canonical decode.

use fauna_core::crypto::{BackupKey, seal_backup_chunk, unseal_backup_chunk};
use thiserror::Error;

/// Failures from sealing/unsealing a `__drafts` rail blob. The variants map onto
/// the at-rest pipeline stages; `compress_chunk` and the input bytes are
/// infallible, so there is no encode/compress-failed variant on the seal path.
#[derive(Debug, Error)]
pub enum DraftSealError {
    /// ChaCha20-Poly1305 sealing failed (internal AEAD fault).
    #[error("sealing drafts blob failed: {0}")]
    Encrypt(String),

    /// ChaCha20-Poly1305 unsealing failed — wrong `BackupKey`, a
    /// tampered/truncated blob, or an unrecognized version byte.
    #[error("unsealing drafts blob failed (wrong key or tampered data): {0}")]
    Decrypt(String),

    /// zstd decompression of the unsealed payload failed.
    #[error("decompressing drafts payload failed: {0}")]
    Decompress(String),
}

/// Derive a [`BackupKey`] from a 32-byte Ed25519 identity seed.
///
/// Thin wrapper over [`fauna_core::crypto::BackupKey::derive`] so callers need
/// not reach into `fauna_core::crypto` directly. The same seed always yields the
/// same key, so every device in the user's fleet seals/unseals against an
/// identical key. Mirrors `fauna_client_config::backup_key_from_seed`.
pub fn backup_key_from_seed(seed: &[u8; 32]) -> BackupKey {
    BackupKey::derive(seed)
}

/// Produce the at-rest blob bytes the nest stores for a `__drafts` rail.
///
/// Pipeline: `compress_chunk` (zstd, self-describing prefix) →
/// `encrypt_backup_chunk` (ChaCha20-Poly1305 under `key`). Byte-for-byte what
/// `encode_blob(snapshot_bytes, Some(key), true)` produces in
/// `bins/fauna-nest/src/backup/mod.rs` (modulo the random AEAD nonce), and the
/// inverse of [`unseal_drafts`].
pub fn seal_drafts(snapshot_bytes: &[u8], key: &BackupKey) -> Result<Vec<u8>, DraftSealError> {
    seal_backup_chunk(snapshot_bytes, key, DraftSealError::Encrypt)
}

/// Inverse of [`seal_drafts`]: decrypt → decompress → the canonical snapshot
/// bytes. The caller hands the result to `DraftStore::restore_from_bytes`.
pub fn unseal_drafts(blob: &[u8], key: &BackupKey) -> Result<Vec<u8>, DraftSealError> {
    unseal_backup_chunk(
        blob,
        key,
        DraftSealError::Decrypt,
        DraftSealError::Decompress,
    )
}

/// Re-seal a predecessor-sealed rail blob under the successor's key — the
/// `__drafts` half of the post-succession corpus re-key
/// (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row).
///
/// Opens `blob` under `old_key` and returns the same plaintext sealed under
/// `new_key`. The plaintext is passed through untouched:
/// there is no owner field to re-point — a drafts blob
/// carries no `actor_id` (the wire derives the owner from the authenticated
/// connection), so the re-key is purely a change of seal.
///
/// **It refuses rather than writes when `old_key` does not open the blob**, and
/// that refusal is the whole safety property: a device that never held the
/// predecessor's seed must leave the bytes alone, not seal its own empty view
/// over drafts it merely cannot read. The same reason `DraftsClient::load` treats an undecryptable blob as a
/// hard error instead of "no drafts". AEAD cannot tell a wrong key from damaged
/// ciphertext, so genuine corruption lands here too — and leaving the bytes
/// alone is the right answer to both.
pub fn rekey_drafts_blob(
    blob: &[u8],
    old_key: &BackupKey,
    new_key: &BackupKey,
) -> Result<Vec<u8>, DraftSealError> {
    let plaintext = unseal_drafts(blob, old_key)?;
    seal_drafts(&plaintext, new_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_unseal_round_trips() {
        let key = BackupKey::derive(&[3u8; 32]);
        let plain = b"canonical draft snapshot bytes".repeat(8);
        let sealed = seal_drafts(&plain, &key).unwrap();
        // Sealed carries the ChaCha20 version byte and differs from plaintext.
        assert_eq!(sealed[0], 0x01);
        assert_ne!(sealed, plain);
        assert_eq!(unseal_drafts(&sealed, &key).unwrap(), plain);
    }

    #[test]
    fn unseal_wrong_key_fails() {
        let key = BackupKey::derive(&[3u8; 32]);
        let other = BackupKey::derive(&[4u8; 32]);
        let sealed = seal_drafts(b"hello drafts", &key).unwrap();
        assert!(matches!(
            unseal_drafts(&sealed, &other),
            Err(DraftSealError::Decrypt(_))
        ));
    }

    /// The aftermath primitive: a predecessor-sealed rail blob opens under the
    /// retired key and comes back sealed under the successor's, **with the
    /// plaintext byte-identical**. The draft prose is what the user would lose,
    /// so the round-trip is asserted on the payload, not on "it re-sealed".
    #[test]
    fn rekey_carries_the_plaintext_across_unchanged() {
        let old = BackupKey::derive(&[7u8; 32]);
        let new = BackupKey::derive(&[8u8; 32]);
        let plain = b"a half-written post the user cares about".repeat(3);

        let predecessor_sealed = seal_drafts(&plain, &old).unwrap();
        let successor_sealed = rekey_drafts_blob(&predecessor_sealed, &old, &new).unwrap();

        assert_eq!(unseal_drafts(&successor_sealed, &new).unwrap(), plain);
        assert!(
            unseal_drafts(&successor_sealed, &old).is_err(),
            "re-keying must retire the old key, or the aftermath bought nothing"
        );
    }

    /// ⚠ The refusal that keeps the pass from destroying drafts: a blob the
    /// offered key cannot open is **not** re-sealed into an empty one. Without
    /// this, a device that never held the predecessor seed would "succeed" by
    /// sealing its own empty view over the user's real, just-unreadable drafts
    /// — the exact clobber `DraftsClient::load`'s hard-error contract exists to
    /// prevent, one layer down.
    #[test]
    fn rekey_refuses_a_blob_the_old_key_does_not_open() {
        let old = BackupKey::derive(&[7u8; 32]);
        let new = BackupKey::derive(&[8u8; 32]);
        let stranger = BackupKey::derive(&[9u8; 32]);
        let sealed = seal_drafts(b"someone else's drafts", &stranger).unwrap();

        assert!(matches!(
            rekey_drafts_blob(&sealed, &old, &new),
            Err(DraftSealError::Decrypt(_))
        ));
    }
}
