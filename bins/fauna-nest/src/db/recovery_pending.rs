//! The **pending seed-initiated RecoveryKey replacement** store
//! (identity-succession slice 2; `identity-succession.md:37`).
//!
//! One row per identity (table in `migrations::MIGRATIONS_RECOVERY_PENDING`):
//! a seed-alone replacement record parked for the 30-day veto window. A row
//! here confers **no authority** — the chain in `recovery_registrations` is
//! untouched until the landing sweep re-verifies the record against the
//! then-current head after an uncontested window.
//!
//! **Replay cannot extend the window.** The idempotence key is `record_digest`
//! (BLAKE3 of the verbatim record bytes): an upsert with an unchanged digest
//! keeps the ORIGINAL `requested_at`, so a re-delivered request — client
//! auto-retry or deliberate replay — lands the same window it started. A
//! *different* record (a genuinely new request) replaces the row and restarts
//! the clock; that is the latest-wins semantic that keeps an honest owner from
//! ever being parked behind a thief's stale pending — they veto and re-request.
//!
//! Like the registration store, this module never verifies a signature —
//! verification is `fauna_core::recovery::verify_seed_alone`, run by the
//! handler at request time and by the sweep again at landing time.

use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;

use super::CacheDb;
use super::recovery_registrations::RECOVERY_PUBKEY_LEN;

/// One pending seed-initiated replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReplacementRow {
    /// The identity the replacement targets.
    pub actor_id: Vec<u8>,
    /// The verbatim canonical DAG-CBOR bytes the client submitted — appended
    /// to the chain byte-for-byte at landing, never re-encoded.
    pub record: Vec<u8>,
    /// BLAKE3 of `record` — the idempotence key.
    pub record_digest: Vec<u8>,
    /// The Ed25519 public half of the RecoveryKey that would be registered
    /// (denormalized for the status projection).
    pub new_recovery_pubkey: Vec<u8>,
    /// The chain `seq` the record claims (denormalized for the sweep's cheap
    /// supersession pre-check).
    pub seq: u64,
    /// Unix seconds the request was (first) accepted; the window runs from
    /// here.
    pub requested_at: i64,
}

impl CacheDb {
    /// Park (or refresh) an identity's pending replacement, returning the
    /// `requested_at` the window runs from.
    ///
    /// Same digest ⇒ the existing row is kept untouched (replay-idempotent);
    /// different digest ⇒ the row is replaced and the clock restarts at `now`.
    /// `now` is caller-supplied so the window logic stays testable without
    /// wall-clock waits (convention 14).
    pub async fn upsert_pending_replacement(
        &self,
        actor_id: &[u8],
        record: &[u8],
        new_recovery_pubkey: &[u8],
        seq: u64,
        now: i64,
    ) -> Result<i64> {
        if new_recovery_pubkey.len() != RECOVERY_PUBKEY_LEN {
            return Err(anyhow!(
                "recovery pubkey must be {RECOVERY_PUBKEY_LEN} bytes, got {}",
                new_recovery_pubkey.len()
            ));
        }
        if record.is_empty() {
            return Err(anyhow!("replacement record must not be empty"));
        }
        let digest = blake3::hash(record).as_bytes().to_vec();
        let actor_id = actor_id.to_vec();
        let record = record.to_vec();
        let new_recovery_pubkey = new_recovery_pubkey.to_vec();
        let conn = self.conn.lock().await;

        let existing: Option<(Vec<u8>, i64)> = conn
            .query_row(
                "SELECT record_digest, requested_at FROM recovery_pending_replacements
                 WHERE actor_id = ?1",
                rusqlite::params![actor_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| anyhow!("read pending replacement: {e}"))?;
        if let Some((stored_digest, requested_at)) = existing
            && stored_digest == digest
        {
            return Ok(requested_at);
        }

        conn.execute(
            "INSERT INTO recovery_pending_replacements
                (actor_id, record, record_digest, new_recovery_pubkey, seq, requested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(actor_id) DO UPDATE SET
                record = excluded.record,
                record_digest = excluded.record_digest,
                new_recovery_pubkey = excluded.new_recovery_pubkey,
                seq = excluded.seq,
                requested_at = excluded.requested_at",
            rusqlite::params![
                actor_id,
                record,
                digest,
                new_recovery_pubkey,
                seq as i64,
                now
            ],
        )
        .map_err(|e| anyhow!("park pending replacement: {e}"))?;
        Ok(now)
    }

    /// The identity's pending replacement, if any.
    pub async fn get_pending_replacement(
        &self,
        actor_id: &[u8],
    ) -> Result<Option<PendingReplacementRow>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT actor_id, record, record_digest, new_recovery_pubkey, seq, requested_at
             FROM recovery_pending_replacements WHERE actor_id = ?1",
            rusqlite::params![actor_id],
            row_to_pending,
        )
        .optional()
        .map_err(|e| anyhow!("read pending replacement: {e}"))
    }

    /// Cancel an identity's pending replacement (veto, supersession, or
    /// landing). Returns whether a row existed — the veto reply's honest
    /// `cancelled` bit.
    pub async fn delete_pending_replacement(&self, actor_id: &[u8]) -> Result<bool> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM recovery_pending_replacements WHERE actor_id = ?1",
                rusqlite::params![actor_id],
            )
            .map_err(|e| anyhow!("delete pending replacement: {e}"))?;
        Ok(n > 0)
    }

    /// Every pending replacement whose window has elapsed at `now` — the
    /// landing sweep's work list.
    pub async fn list_due_pending_replacements(
        &self,
        now: i64,
        grace_secs: u64,
    ) -> Result<Vec<PendingReplacementRow>> {
        let cutoff = now - grace_secs as i64;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT actor_id, record, record_digest, new_recovery_pubkey, seq, requested_at
             FROM recovery_pending_replacements
             WHERE requested_at <= ?1
             ORDER BY requested_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![cutoff], row_to_pending)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn row_to_pending(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingReplacementRow> {
    Ok(PendingReplacementRow {
        actor_id: row.get(0)?,
        record: row.get(1)?,
        record_digest: row.get(2)?,
        new_recovery_pubkey: row.get(3)?,
        seq: row.get::<_, i64>(4)? as u64,
        requested_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: [u8; 32] = [0xA1; 32];
    const ACTOR_B: [u8; 32] = [0xB2; 32];
    const RK_NEW: [u8; 32] = [0x33; 32];

    #[tokio::test]
    async fn a_request_parks_and_projects_and_deletes() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.get_pending_replacement(&ACTOR_A)
                .await
                .unwrap()
                .is_none()
        );

        let at = db
            .upsert_pending_replacement(&ACTOR_A, b"record-1", &RK_NEW, 1, 1_000)
            .await
            .unwrap();
        assert_eq!(at, 1_000);

        let row = db
            .get_pending_replacement(&ACTOR_A)
            .await
            .unwrap()
            .expect("parked");
        assert_eq!(row.record, b"record-1");
        assert_eq!(row.record_digest, blake3::hash(b"record-1").as_bytes());
        assert_eq!(row.seq, 1);
        assert_eq!(row.requested_at, 1_000);
        // Another identity is untouched.
        assert!(
            db.get_pending_replacement(&ACTOR_B)
                .await
                .unwrap()
                .is_none()
        );

        assert!(db.delete_pending_replacement(&ACTOR_A).await.unwrap());
        assert!(
            db.get_pending_replacement(&ACTOR_A)
                .await
                .unwrap()
                .is_none()
        );
        // The delete is idempotent and reports honestly.
        assert!(!db.delete_pending_replacement(&ACTOR_A).await.unwrap());
    }

    #[tokio::test]
    async fn a_replayed_identical_request_cannot_extend_the_window() {
        // The forbid_replay=false judgment on `replacement.request` rests on
        // exactly this behavior — if this test changes, that flag must too.
        let db = CacheDb::open_in_memory().unwrap();
        let first = db
            .upsert_pending_replacement(&ACTOR_A, b"record-1", &RK_NEW, 1, 1_000)
            .await
            .unwrap();
        let replayed = db
            .upsert_pending_replacement(&ACTOR_A, b"record-1", &RK_NEW, 1, 9_000)
            .await
            .unwrap();
        assert_eq!(first, 1_000);
        assert_eq!(replayed, 1_000, "same digest must keep the original clock");

        // A genuinely different record replaces the row and restarts the clock.
        let restarted = db
            .upsert_pending_replacement(&ACTOR_A, b"record-2", &RK_NEW, 2, 9_000)
            .await
            .unwrap();
        assert_eq!(restarted, 9_000);
        let row = db.get_pending_replacement(&ACTOR_A).await.unwrap().unwrap();
        assert_eq!(row.record, b"record-2");
        assert_eq!(row.seq, 2);
    }

    #[tokio::test]
    async fn only_elapsed_windows_are_due() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_pending_replacement(&ACTOR_A, b"old", &RK_NEW, 1, 1_000)
            .await
            .unwrap();
        db.upsert_pending_replacement(&ACTOR_B, b"fresh", &RK_NEW, 1, 5_000)
            .await
            .unwrap();

        // now = 1_000 + grace ⇒ A due (inclusive), B not.
        let due = db.list_due_pending_replacements(1_100, 100).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].actor_id, ACTOR_A);

        let due = db.list_due_pending_replacements(10_000, 100).await.unwrap();
        assert_eq!(due.len(), 2);
    }

    #[tokio::test]
    async fn malformed_rows_are_refused_at_the_storage_layer() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.upsert_pending_replacement(&ACTOR_A, b"", &RK_NEW, 1, 1_000)
                .await
                .is_err()
        );
        assert!(
            db.upsert_pending_replacement(&ACTOR_A, b"record", &[0x33; 31], 1, 1_000)
                .await
                .is_err()
        );
    }
}
