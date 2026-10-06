//! Deliverability-diagnostic + blocklist-self-check persistence
//! (`docs/goal/behavior/mail-deliverability.md` § Blocklist self-check §
//! Storage + § Admin-visible audit). Two append-only history tables:
//! `mail_outbound_self_blocklist_check` (one row per 24h-timer / admin
//! force-refresh DNSBL sweep) and `mail_outbound_diagnostic_runs` (one row per
//! admin-on-demand `run_deliverability_diagnostics` call). Both retain 90 days.
//!
//! Timestamps are Unix **seconds** (the coarse audit grain the wire reply
//! carries); callers compute `now` in seconds and the prune window matches.

use super::{CacheDb, blob_col_to_array};
use anyhow::Result;

impl CacheDb {
    /// Record one blocklist self-check (the 24h timer or an admin force-refresh).
    /// `results_json` is the per-DNSBL outcome object keyed by server. Returns
    /// the new row id.
    pub async fn record_blocklist_self_check(
        &self,
        checked_at: i64,
        results_json: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_outbound_self_blocklist_check (checked_at, results_json) \
             VALUES (?1, ?2)",
            rusqlite::params![checked_at, results_json],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Record one deliverability-diagnostic run (admin-on-demand). `results_json`
    /// is the JSON checklist; `ran_by_actor_id` the admin who ran it.
    pub async fn record_diagnostic_run(
        &self,
        ran_at: i64,
        results_json: &str,
        ran_by_actor_id: &[u8; 32],
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_outbound_diagnostic_runs (ran_at, results_json, ran_by_actor_id) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![ran_at, results_json, ran_by_actor_id.as_slice()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Blocklist self-check history newest-first, `checked_at >= since`
    /// (epoch-seconds). Returns `(checked_at, results_json)` per row; the handler
    /// parses `results_json` back into the structured per-DNSBL verdicts.
    /// `mail-deliverability.md` § Wire shapes → `list_blocklist_self_check_history`.
    pub async fn list_blocklist_self_check_history(
        &self,
        since: i64,
    ) -> Result<Vec<(i64, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT checked_at, results_json FROM mail_outbound_self_blocklist_check \
             WHERE checked_at >= ?1 ORDER BY checked_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![since], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Deliverability-diagnostic-run audit newest-first, capped at `limit`.
    /// Returns `(ran_at, results_json, ran_by_actor_id)` per row; the handler
    /// parses `results_json` back into the structured checklist.
    /// `mail-deliverability.md` § Wire shapes → `list_deliverability_diagnostic_runs`.
    pub async fn list_deliverability_diagnostic_runs(
        &self,
        limit: i64,
    ) -> Result<Vec<(i64, String, [u8; 32])>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT ran_at, results_json, ran_by_actor_id FROM mail_outbound_diagnostic_runs \
             ORDER BY ran_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit], |r| {
                let ran_at: i64 = r.get(0)?;
                let json: String = r.get(1)?;
                let actor: [u8; 32] = blob_col_to_array(r.get(2)?, 2, "ran_by_actor_id")?;
                Ok((ran_at, json, actor))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Prune blocklist-check + diagnostic-run rows older than `before`
    /// (epoch-seconds). Called from the 24h timer.
    pub async fn prune_deliverability_history(&self, before: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM mail_outbound_self_blocklist_check WHERE checked_at < ?1",
            rusqlite::params![before],
        )?;
        conn.execute(
            "DELETE FROM mail_outbound_diagnostic_runs WHERE ran_at < ?1",
            rusqlite::params![before],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn records_and_prunes_history() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        db.record_blocklist_self_check(1_000, r#"{"zen.spamhaus.org":{"listed":false}}"#)
            .await
            .unwrap();
        db.record_blocklist_self_check(2_000, r#"{"zen.spamhaus.org":{"listed":true}}"#)
            .await
            .unwrap();
        db.record_diagnostic_run(1_500, r#"[{"name":"SPF","status":"pass"}]"#, &actor)
            .await
            .unwrap();

        // Prune everything before 1_800 → drops the 1_000 blocklist row + the
        // 1_500 diagnostic row, keeps the 2_000 blocklist row.
        db.prune_deliverability_history(1_800).await.unwrap();
        let conn = db.conn.lock().await;
        let blocklist_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mail_outbound_self_blocklist_check",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let diag_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mail_outbound_diagnostic_runs",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(blocklist_rows, 1);
        assert_eq!(diag_rows, 0);
    }

    #[tokio::test]
    async fn reads_history_newest_first_with_window_and_limit() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        // Three blocklist checks at increasing times.
        db.record_blocklist_self_check(1_000, r#"[{"server":"a","listed":false}]"#)
            .await
            .unwrap();
        db.record_blocklist_self_check(2_000, r#"[{"server":"a","listed":true}]"#)
            .await
            .unwrap();
        db.record_blocklist_self_check(3_000, r#"[]"#)
            .await
            .unwrap();

        // Window cutoff 1_500 → drops the 1_000 row; newest first.
        let history = db.list_blocklist_self_check_history(1_500).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].0, 3_000);
        assert_eq!(history[1].0, 2_000);

        // Three diagnostic runs; LIMIT 2 keeps the two newest, newest first.
        db.record_diagnostic_run(10, r#"[{"name":"SPF"}]"#, &actor)
            .await
            .unwrap();
        db.record_diagnostic_run(20, r#"[{"name":"DKIM"}]"#, &actor)
            .await
            .unwrap();
        db.record_diagnostic_run(30, r#"[{"name":"DMARC"}]"#, &actor)
            .await
            .unwrap();
        let runs = db.list_deliverability_diagnostic_runs(2).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, 30);
        assert_eq!(runs[1].0, 20);
        assert_eq!(
            runs[0].2, actor,
            "actor id round-trips from the BLOB column"
        );
    }
}
