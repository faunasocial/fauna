//! The mail health readout's nest-side reads and the two heartbeat stamps
//! (`docs/goal/behavior/mail-deliverability.md` § The mail health readout).
//!
//! The stamps live on ONE deployment-wide row in `mail_heartbeat_state` (the
//! `mail_outbound_warmup_state` idiom: `INSERT OR IGNORE … (id) VALUES (1)` then
//! `WHERE id = 1`). They carry no actor, no domain and no message identity, so
//! `mail-observability.md` § Cross-actor isolation holds trivially. The other
//! reads here are the latest rows of the existing history tables and the
//! pending-queue rows the shared stall predicate
//! (`fauna_mail::health::queue_row_stalled`) is applied to.
//!
//! Timestamps are Unix **seconds**.

use super::CacheDb;
use anyhow::{Context, Result};

/// The two heartbeat facts; `None` = never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MailHeartbeats {
    pub last_outbound_delivered_at: Option<i64>,
    pub last_inbound_accepted_at: Option<i64>,
}

impl CacheDb {
    fn ensure_heartbeat_row(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute(
            "INSERT OR IGNORE INTO mail_heartbeat_state (id) VALUES (1)",
            [],
        )
        .context("ensure heartbeat row")?;
        Ok(())
    }

    /// Stamp `last_outbound_delivered_at = now` (from `mark_outbound_delivered`).
    pub async fn stamp_outbound_delivered(&self, now: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        Self::ensure_heartbeat_row(&conn)?;
        conn.execute(
            "UPDATE mail_heartbeat_state SET last_outbound_delivered_at = ?1 WHERE id = 1",
            rusqlite::params![now],
        )
        .context("stamp outbound delivered")?;
        Ok(())
    }

    /// Stamp `last_inbound_accepted_at = now` (from `ingest_inbound_mail`).
    pub async fn stamp_inbound_accepted(&self, now: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        Self::ensure_heartbeat_row(&conn)?;
        conn.execute(
            "UPDATE mail_heartbeat_state SET last_inbound_accepted_at = ?1 WHERE id = 1",
            rusqlite::params![now],
        )
        .context("stamp inbound accepted")?;
        Ok(())
    }

    /// Read both heartbeat stamps.
    pub async fn read_mail_heartbeats(&self) -> Result<MailHeartbeats> {
        let conn = self.conn.lock().await;
        Self::ensure_heartbeat_row(&conn)?;
        conn.query_row(
            "SELECT last_outbound_delivered_at, last_inbound_accepted_at \
             FROM mail_heartbeat_state WHERE id = 1",
            [],
            |r| {
                Ok(MailHeartbeats {
                    last_outbound_delivered_at: r.get(0)?,
                    last_inbound_accepted_at: r.get(1)?,
                })
            },
        )
        .context("read heartbeats")
    }

    /// `(attempt_count, created_at)` of every `pending` outbound row that has
    /// been attempted at least once — the candidates the shared stall predicate
    /// is applied to. A never-attempted row (a warm-up deferral) is excluded here
    /// too, but the predicate is what decides.
    pub async fn list_failed_pending_outbound(&self) -> Result<Vec<(u32, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT attempt_count, created_at FROM outbound_mail_queue \
             WHERE status = 'pending' AND attempt_count >= 1",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The latest blocklist self-check's `(checked_at, results_json)`.
    pub async fn latest_blocklist_self_check(&self) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT checked_at, results_json FROM mail_outbound_self_blocklist_check \
             ORDER BY checked_at DESC, id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.next().transpose()?)
    }

    /// The latest diagnostics run's `(ran_at, results_json)`.
    pub async fn latest_diagnostic_run(&self) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT ran_at, results_json FROM mail_outbound_diagnostic_runs \
             ORDER BY ran_at DESC, id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.next().transpose()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn heartbeats_start_never_and_advance_independently() {
        let db = CacheDb::open_in_memory().unwrap();
        let hb = db.read_mail_heartbeats().await.unwrap();
        assert_eq!(hb.last_outbound_delivered_at, None);
        assert_eq!(hb.last_inbound_accepted_at, None);

        db.stamp_outbound_delivered(1_000).await.unwrap();
        db.stamp_inbound_accepted(2_000).await.unwrap();
        db.stamp_outbound_delivered(3_000).await.unwrap();
        let hb = db.read_mail_heartbeats().await.unwrap();
        assert_eq!(hb.last_outbound_delivered_at, Some(3_000));
        assert_eq!(hb.last_inbound_accepted_at, Some(2_000));
    }

    #[tokio::test]
    async fn latest_rows_are_newest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.latest_blocklist_self_check().await.unwrap(), None);
        assert_eq!(db.latest_diagnostic_run().await.unwrap(), None);
        db.record_blocklist_self_check(1_000, "[]").await.unwrap();
        db.record_blocklist_self_check(2_000, "[1]").await.unwrap();
        db.record_diagnostic_run(5, "[a]", &[0u8; 32])
            .await
            .unwrap();
        db.record_diagnostic_run(6, "[b]", &[0u8; 32])
            .await
            .unwrap();
        assert_eq!(
            db.latest_blocklist_self_check().await.unwrap(),
            Some((2_000, "[1]".to_string()))
        );
        assert_eq!(
            db.latest_diagnostic_run().await.unwrap(),
            Some((6, "[b]".to_string()))
        );
    }
}
