//! Staged custody **receipts** — the owner-side store half of stage (c)
//! (`account-data-plane.md` § Replica posture → The custody grant + ceremony,
//! the device-or-nest bullet, item 6).
//!
//! One row per `(owner_actor_id, grant_id)` in `custody_receipts_staged`
//! (table defined in `migrations::MIGRATIONS_CUSTODY_RECEIPTS_STAGED`): the
//! newest signed receipt a custodian NEST deposited over
//! `fauna.custody.receipt.deposit`, staged for the owner's fleet to fetch
//! over `.list` and fold at sync. **Latest-per-grant and monotone in the
//! receipt's own `attested_at`** — an older deposit (a replay, a redrive of
//! an already-acked receipt) is a no-op, never a displacement and never a
//! duplicate. The blob rests VERBATIM: the fleet re-verifies the very
//! signature a re-encode would invalidate.
//!
//! CRUD only — the deposit door (`custody_receipt_handlers.rs`) owns the
//! capability-row re-derivation and the door-side signature verify; this
//! module never opens an envelope.

use anyhow::{Result, anyhow};

use super::{CacheDb, now_epoch_secs};

/// One staged receipt, as the owner's fleet fetches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedReceiptRow {
    pub grant_id: Vec<u8>,
    /// The signed receipt envelope, verbatim as deposited.
    pub receipt: Vec<u8>,
    /// The receipt's own `attested_at` (microseconds) — the monotone key.
    pub attested_at: u64,
    /// Unix seconds this nest staged it.
    pub staged_at: i64,
}

impl CacheDb {
    /// Stage a deposited receipt, latest-per-grant. Returns `true` when the
    /// row was written (first receipt, or strictly newer `attested_at`) and
    /// `false` for the not-newer no-op — the reply's `staged` flag.
    pub async fn stage_custody_receipt(
        &self,
        owner_actor_id: &[u8],
        grant_id: &[u8],
        receipt: &[u8],
        attested_at: u64,
    ) -> Result<bool> {
        let owner_actor_id = owner_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let receipt = receipt.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT INTO custody_receipts_staged
                    (owner_actor_id, grant_id, receipt, attested_at, staged_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(owner_actor_id, grant_id) DO UPDATE SET
                    receipt = excluded.receipt,
                    attested_at = excluded.attested_at,
                    staged_at = excluded.staged_at
                 WHERE excluded.attested_at > custody_receipts_staged.attested_at",
                rusqlite::params![owner_actor_id, grant_id, receipt, attested_at as i64, now,],
            )
            .map_err(|e| anyhow!("stage custody receipt: {e}"))?;
        Ok(n > 0)
    }

    /// Every staged receipt for one owner — the `fauna.custody.receipt.list`
    /// read. **Owner-scoped**: a caller only ever sees its own grants'.
    pub async fn list_custody_receipts(
        &self,
        owner_actor_id: &[u8],
    ) -> Result<Vec<StagedReceiptRow>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT grant_id, receipt, attested_at, staged_at
             FROM custody_receipts_staged
             WHERE owner_actor_id = ?1
             ORDER BY grant_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner_actor_id], |row| {
            Ok(StagedReceiptRow {
                grant_id: row.get(0)?,
                receipt: row.get(1)?,
                attested_at: row.get::<_, i64>(2)? as u64,
                staged_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: [u8; 32] = [0x0E; 32];
    const GRANT_1: [u8; 16] = [0x11; 16];
    const GRANT_2: [u8; 16] = [0x22; 16];

    #[tokio::test]
    async fn staging_is_latest_per_grant_and_monotone() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.stage_custody_receipt(&OWNER, &GRANT_1, b"r1", 100)
                .await
                .unwrap(),
            "first receipt stages"
        );
        assert!(
            db.stage_custody_receipt(&OWNER, &GRANT_1, b"r2", 200)
                .await
                .unwrap(),
            "a newer receipt displaces"
        );
        assert!(
            !db.stage_custody_receipt(&OWNER, &GRANT_1, b"r-old", 150)
                .await
                .unwrap(),
            "an older receipt is a no-op"
        );
        assert!(
            !db.stage_custody_receipt(&OWNER, &GRANT_1, b"r2", 200)
                .await
                .unwrap(),
            "an equal-stamp replay is a no-op, never a duplicate"
        );

        db.stage_custody_receipt(&OWNER, &GRANT_2, b"g2", 50)
            .await
            .unwrap();
        let rows = db.list_custody_receipts(&OWNER).await.unwrap();
        assert_eq!(rows.len(), 2, "one row per grant");
        let g1 = rows
            .iter()
            .find(|r| r.grant_id == GRANT_1.to_vec())
            .unwrap();
        assert_eq!(g1.receipt, b"r2".to_vec(), "the newest receipt rests");
        assert_eq!(g1.attested_at, 200);
    }

    #[tokio::test]
    async fn receipts_are_owner_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        db.stage_custody_receipt(&OWNER, &GRANT_1, b"mine", 10)
            .await
            .unwrap();
        assert!(
            db.list_custody_receipts(&[0x0F; 32])
                .await
                .unwrap()
                .is_empty(),
            "another owner sees nothing"
        );
    }
}
