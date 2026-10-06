//! The **seed-escrow blob** store (identity-succession slice 2;
//! `docs/goal/behavior/identity-succession.md` § Seed escrow).
//!
//! One row per identity, holding the identity seed HPKE-sealed to the
//! RecoveryKey's derived X25519 public half. **The nest cannot read it** — it
//! holds no half of that key (key-material rule #4), so this module deliberately
//! treats the value as an opaque byte string: nothing here decodes, validates or
//! re-encodes the blob's structure, and the fetch path replays the exact bytes
//! that were put. A blob whose bytes the nest perturbed is a blob no recovery
//! kit can open, and the user would discover that only after losing every
//! device.
//!
//! What the store *does* enforce is the two things opacity cannot excuse: a
//! non-empty value, and a size ceiling. Without the ceiling an authenticated
//! user could park arbitrary data in an unmetered, admin-invisible row — the
//! blob is ~100 bytes by construction, so a generous cap costs a legitimate
//! client nothing (see [`RECOVERY_ESCROW_MAX_LEN`]).

use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_secs};

/// Ceiling on a stored escrow blob.
///
/// A real blob is a `SeedEscrowBlob` around a 32-byte plaintext: 32-byte HPKE
/// `enc` + 48-byte ciphertext + the CBOR envelope ≈ 130 bytes. 4 KiB is ~30×
/// that — room for a format that grows (a post-quantum `enc` is ~1.2 KiB, so the
/// hybrid successor already fits) while keeping the row far from a storage
/// primitive. Not a user-facing knob: a bucket-1 constant per the
/// configuration-surface invariant.
pub const RECOVERY_ESCROW_MAX_LEN: usize = 4096;

/// A stored escrow blob and when the nest recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEscrowRow {
    /// The opaque sealed bytes, exactly as submitted.
    pub blob: Vec<u8>,
    /// Unix seconds of the last write.
    pub updated_at: i64,
}

impl CacheDb {
    /// Store (or replace) an identity's escrow blob, returning the recorded
    /// timestamp.
    ///
    /// Replacement is the normal path, not an edge case: the blob is rewritten
    /// whenever the sealed value or the sealing key changes — at kit creation,
    /// at RecoveryKey replacement, and at succession
    /// (`identity-succession.md:42`). Keeping one row per identity is what makes
    /// "the current kit opens the current blob" true by construction; a history
    /// of blobs would be a set of ciphertexts whose matching kits are gone.
    pub async fn put_recovery_escrow(&self, actor_id: &[u8], blob: &[u8]) -> Result<i64> {
        if blob.is_empty() {
            return Err(anyhow!("escrow blob must not be empty"));
        }
        if blob.len() > RECOVERY_ESCROW_MAX_LEN {
            return Err(anyhow!(
                "escrow blob must be at most {RECOVERY_ESCROW_MAX_LEN} bytes, got {}",
                blob.len()
            ));
        }
        let actor_id = actor_id.to_vec();
        let blob = blob.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO recovery_escrow (actor_id, blob, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET blob = excluded.blob,
                                                 updated_at = excluded.updated_at",
            rusqlite::params![actor_id, blob, now],
        )
        .map_err(|e| anyhow!("put recovery escrow: {e}"))?;
        Ok(now)
    }

    /// Read an identity's escrow blob, or `None` if it has none.
    pub async fn get_recovery_escrow(&self, actor_id: &[u8]) -> Result<Option<RecoveryEscrowRow>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob, updated_at FROM recovery_escrow WHERE actor_id = ?1",
            rusqlite::params![actor_id],
            |row| {
                Ok(RecoveryEscrowRow {
                    blob: row.get(0)?,
                    updated_at: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|e| anyhow!("read recovery escrow: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: [u8; 32] = [0xA1; 32];
    const ACTOR_B: [u8; 32] = [0xB2; 32];

    #[tokio::test]
    async fn an_identity_with_no_kit_has_no_blob() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.get_recovery_escrow(&ACTOR_A).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_blob_round_trips_byte_for_byte() {
        let db = CacheDb::open_in_memory().unwrap();
        // Bytes chosen to catch any text/UTF-8 handling: NUL, high bytes, and a
        // lone continuation byte. Ciphertext is uniformly random, so a store
        // that mangled any of these would corrupt roughly every real blob.
        let blob = vec![0x00, 0xff, 0x80, 0x7f, 0xc3, 0x00];
        db.put_recovery_escrow(&ACTOR_A, &blob).await.unwrap();

        let row = db.get_recovery_escrow(&ACTOR_A).await.unwrap().unwrap();
        assert_eq!(row.blob, blob);
    }

    #[tokio::test]
    async fn a_re_put_replaces_the_previous_blob() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"old-kit").await.unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"new-kit").await.unwrap();

        let row = db.get_recovery_escrow(&ACTOR_A).await.unwrap().unwrap();
        assert_eq!(
            row.blob,
            b"new-kit".to_vec(),
            "the current kit must open what the store serves"
        );
    }

    #[tokio::test]
    async fn blobs_are_per_identity() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"a-blob").await.unwrap();
        db.put_recovery_escrow(&ACTOR_B, b"b-blob").await.unwrap();

        assert_eq!(
            db.get_recovery_escrow(&ACTOR_A)
                .await
                .unwrap()
                .unwrap()
                .blob,
            b"a-blob".to_vec()
        );
        assert_eq!(
            db.get_recovery_escrow(&ACTOR_B)
                .await
                .unwrap()
                .unwrap()
                .blob,
            b"b-blob".to_vec()
        );
    }

    #[tokio::test]
    async fn an_empty_or_oversized_blob_is_refused_and_leaves_the_row_untouched() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"good").await.unwrap();

        assert!(db.put_recovery_escrow(&ACTOR_A, b"").await.is_err());
        assert!(
            db.put_recovery_escrow(&ACTOR_A, &vec![0u8; RECOVERY_ESCROW_MAX_LEN + 1])
                .await
                .is_err()
        );
        // A refused write must not have destroyed the blob that was already
        // there — losing it costs the user their only loss-recovery path.
        assert_eq!(
            db.get_recovery_escrow(&ACTOR_A)
                .await
                .unwrap()
                .unwrap()
                .blob,
            b"good".to_vec()
        );

        // The cap itself is inclusive.
        assert!(
            db.put_recovery_escrow(&ACTOR_A, &vec![0u8; RECOVERY_ESCROW_MAX_LEN])
                .await
                .is_ok()
        );
    }
}
