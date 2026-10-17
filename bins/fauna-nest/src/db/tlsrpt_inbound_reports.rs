//! Inbound TLSRPT report storage (`docs/goal/behavior/smtp-server.md` § TLSRPT
//! inbound report listener; the per-domain column per `mail-multidomain.md`
//! § Per-domain TLSRPT). The DMARC aggregate store's twin (`dmarc_reports`). Deployment
//! data: no actor column, read only by Admin-class kinds. Timestamps are Unix
//! **seconds**.

use super::CacheDb;
use anyhow::{Context, Result};

/// One stored report. `parsed_summary_cbor` / `source_envelope_blob` are
/// DAG-CBOR the handler encodes and decodes; the store keeps them opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsrptInboundReportRow {
    pub submitter: String,
    /// The `policy-domain` the report is about — one of ours.
    pub policy_published_domain: String,
    pub report_id: String,
    pub begin_at: i64,
    pub end_at: i64,
    pub received_at: i64,
    pub payload_json_gz: Vec<u8>,
    pub parsed_summary_cbor: Vec<u8>,
    pub source_envelope_blob: Vec<u8>,
}

const ROW_COLUMNS: &str = "submitter, policy_published_domain, report_id, begin_at, end_at, \
     received_at, payload_json_gz, parsed_summary_cbor, source_envelope_blob";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<TlsrptInboundReportRow> {
    Ok(TlsrptInboundReportRow {
        submitter: r.get(0)?,
        policy_published_domain: r.get(1)?,
        report_id: r.get(2)?,
        begin_at: r.get(3)?,
        end_at: r.get(4)?,
        received_at: r.get(5)?,
        payload_json_gz: r.get(6)?,
        parsed_summary_cbor: r.get(7)?,
        source_envelope_blob: r.get(8)?,
    })
}

impl CacheDb {
    /// Store one report. `false` when `(submitter, policy_published_domain,
    /// report_id)` is already stored — a peer re-sent it, and the first copy
    /// stands.
    pub async fn insert_tlsrpt_inbound_report(&self, row: &TlsrptInboundReportRow) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO tlsrpt_inbound_reports ({ROW_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                ),
                rusqlite::params![
                    row.submitter,
                    row.policy_published_domain,
                    row.report_id,
                    row.begin_at,
                    row.end_at,
                    row.received_at,
                    row.payload_json_gz,
                    row.parsed_summary_cbor,
                    row.source_envelope_blob,
                ],
            )
            .context("insert tlsrpt inbound report")?;
        Ok(n == 1)
    }

    /// Reports received at or after `since`, optionally for one of our domains,
    /// newest first, at most `limit` rows. The archive column is not read.
    pub async fn list_tlsrpt_inbound_reports(
        &self,
        since: i64,
        policy_published_domain: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TlsrptInboundReportRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT submitter, policy_published_domain, report_id, begin_at, end_at, \
                        received_at, X'', parsed_summary_cbor, X'' \
                 FROM tlsrpt_inbound_reports \
                 WHERE received_at >= ?1 AND (?2 IS NULL OR policy_published_domain = ?2) \
                 ORDER BY received_at DESC, submitter, report_id \
                 LIMIT ?3",
            )
            .context("prepare list tlsrpt inbound reports")?;
        let rows = stmt
            .query_map(
                rusqlite::params![since, policy_published_domain, limit as i64],
                row_from,
            )
            .context("list tlsrpt inbound reports")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read tlsrpt inbound report row")?;
        Ok(rows)
    }

    /// One report by its key, archive included.
    pub async fn get_tlsrpt_inbound_report(
        &self,
        policy_published_domain: &str,
        submitter: &str,
        report_id: &str,
    ) -> Result<Option<TlsrptInboundReportRow>> {
        use rusqlite::OptionalExtension;
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {ROW_COLUMNS} FROM tlsrpt_inbound_reports \
                 WHERE policy_published_domain = ?1 AND submitter = ?2 AND report_id = ?3"
            ),
            rusqlite::params![policy_published_domain, submitter, report_id],
            row_from,
        )
        .optional()
        .context("get tlsrpt inbound report")
    }

    /// Drop reports received before `cutoff` (90-day retention). Returns the
    /// count.
    pub async fn prune_tlsrpt_inbound_reports(&self, cutoff: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM tlsrpt_inbound_reports WHERE received_at < ?1",
            rusqlite::params![cutoff],
        )
        .context("prune tlsrpt inbound reports")
    }
}
