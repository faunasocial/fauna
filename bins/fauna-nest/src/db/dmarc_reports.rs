//! DMARC report storage (`docs/goal/behavior/dmarc-reporting.md`
//! § Aggregate-report receive and § Forensic-report receive → *Storage*,
//! *Retention*, and § Outbound aggregate-report sender): the reports peers send
//! us, and the aggregate reports we send them. Deployment data: no actor
//! column, read only by Admin-class kinds and the daily outbound pass.
//! Timestamps are Unix **seconds**.

use super::CacheDb;
use anyhow::{Context, Result};

/// One stored report. `parsed_summary_cbor` / `source_envelope_blob` are
/// DAG-CBOR the handler encodes and decodes; the store keeps them opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcAggregateReportRow {
    pub submitter: String,
    pub our_domain: String,
    pub report_id: String,
    pub begin_at: i64,
    pub end_at: i64,
    pub received_at: i64,
    pub payload_xml_gz: Vec<u8>,
    pub parsed_summary_cbor: Vec<u8>,
    pub source_envelope_blob: Vec<u8>,
}

const ROW_COLUMNS: &str = "submitter, our_domain, report_id, begin_at, end_at, received_at, \
     payload_xml_gz, parsed_summary_cbor, source_envelope_blob";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<DmarcAggregateReportRow> {
    Ok(DmarcAggregateReportRow {
        submitter: r.get(0)?,
        our_domain: r.get(1)?,
        report_id: r.get(2)?,
        begin_at: r.get(3)?,
        end_at: r.get(4)?,
        received_at: r.get(5)?,
        payload_xml_gz: r.get(6)?,
        parsed_summary_cbor: r.get(7)?,
        source_envelope_blob: r.get(8)?,
    })
}

impl CacheDb {
    /// Store one report. `false` when `(submitter, our_domain, report_id)` is
    /// already stored — a peer re-sent it, and the first copy stands.
    pub async fn insert_dmarc_aggregate_report(
        &self,
        row: &DmarcAggregateReportRow,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                &format!(
                    "INSERT OR IGNORE INTO dmarc_inbound_aggregate_reports ({ROW_COLUMNS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                ),
                rusqlite::params![
                    row.submitter,
                    row.our_domain,
                    row.report_id,
                    row.begin_at,
                    row.end_at,
                    row.received_at,
                    row.payload_xml_gz,
                    row.parsed_summary_cbor,
                    row.source_envelope_blob,
                ],
            )
            .context("insert dmarc aggregate report")?;
        Ok(n == 1)
    }

    /// Reports received at or after `since`, optionally for one of our domains,
    /// newest first, at most `limit` rows. The archive column is not read.
    pub async fn list_dmarc_aggregate_reports(
        &self,
        since: i64,
        our_domain: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DmarcAggregateReportRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT submitter, our_domain, report_id, begin_at, end_at, received_at, \
                        X'', parsed_summary_cbor, X'' \
                 FROM dmarc_inbound_aggregate_reports \
                 WHERE received_at >= ?1 AND (?2 IS NULL OR our_domain = ?2) \
                 ORDER BY received_at DESC, submitter, report_id \
                 LIMIT ?3",
            )
            .context("prepare list dmarc aggregate reports")?;
        let rows = stmt
            .query_map(rusqlite::params![since, our_domain, limit as i64], row_from)
            .context("list dmarc aggregate reports")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read dmarc aggregate report row")?;
        Ok(rows)
    }

    /// One report by its key, archive included.
    pub async fn get_dmarc_aggregate_report(
        &self,
        our_domain: &str,
        submitter: &str,
        report_id: &str,
    ) -> Result<Option<DmarcAggregateReportRow>> {
        use rusqlite::OptionalExtension;
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {ROW_COLUMNS} FROM dmarc_inbound_aggregate_reports \
                 WHERE our_domain = ?1 AND submitter = ?2 AND report_id = ?3"
            ),
            rusqlite::params![our_domain, submitter, report_id],
            row_from,
        )
        .optional()
        .context("get dmarc aggregate report")
    }

    /// Drop reports received before `cutoff` (§ Retention). Returns the count.
    pub async fn prune_dmarc_aggregate_reports(&self, cutoff: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM dmarc_inbound_aggregate_reports WHERE received_at < ?1",
            rusqlite::params![cutoff],
        )
        .context("prune dmarc aggregate reports")
    }
}

/// One oversize aggregate report's envelope, kept minus its body for ops
/// triage (§ XML validation step 1). `id` is ignored on insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcOversizeRow {
    pub id: i64,
    pub received_at: i64,
    pub client_ip: String,
    pub helo: String,
    pub mail_from: String,
    /// JSON `AuthVerdicts` of the carrying mail.
    pub auth_results_json: String,
    pub size_bytes: i64,
}

/// At most this many triage rows rest at once: a flood of oversize mail must
/// not grow the table past what the admin could read.
pub const DMARC_OVERSIZE_ROW_CAP: i64 = 1000;

impl CacheDb {
    /// Keep one oversize report's envelope. `false` when the table is at
    /// [`DMARC_OVERSIZE_ROW_CAP`] and the row was dropped.
    pub async fn insert_dmarc_oversize(&self, row: &DmarcOversizeRow) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT INTO dmarc_inbound_aggregate_oversize \
                 (received_at, client_ip, helo, mail_from, auth_results_json, size_bytes) \
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6 \
                 WHERE (SELECT COUNT(*) FROM dmarc_inbound_aggregate_oversize) < ?7",
                rusqlite::params![
                    row.received_at,
                    row.client_ip,
                    row.helo,
                    row.mail_from,
                    row.auth_results_json,
                    row.size_bytes,
                    DMARC_OVERSIZE_ROW_CAP,
                ],
            )
            .context("insert dmarc oversize triage row")?;
        Ok(n == 1)
    }

    /// Triage rows received at or after `since`, newest first, at most
    /// `limit` rows.
    pub async fn list_dmarc_oversize(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<Vec<DmarcOversizeRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, received_at, client_ip, helo, mail_from, auth_results_json, \
                        size_bytes \
                 FROM dmarc_inbound_aggregate_oversize WHERE received_at >= ?1 \
                 ORDER BY received_at DESC, id DESC LIMIT ?2",
            )
            .context("prepare list dmarc oversize")?;
        let rows = stmt
            .query_map(rusqlite::params![since, limit as i64], |r| {
                Ok(DmarcOversizeRow {
                    id: r.get(0)?,
                    received_at: r.get(1)?,
                    client_ip: r.get(2)?,
                    helo: r.get(3)?,
                    mail_from: r.get(4)?,
                    auth_results_json: r.get(5)?,
                    size_bytes: r.get(6)?,
                })
            })
            .context("list dmarc oversize")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read dmarc oversize row")?;
        Ok(rows)
    }

    /// Drop triage rows received before `cutoff` (§ Retention). Returns the count.
    pub async fn prune_dmarc_oversize(&self, cutoff: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM dmarc_inbound_aggregate_oversize WHERE received_at < ?1",
            rusqlite::params![cutoff],
        )
        .context("prune dmarc oversize")
    }
}

/// One stored forensic report. `parsed_summary_cbor` / `source_envelope_blob`
/// are DAG-CBOR the handler encodes and decodes; the store keeps them opaque.
/// `id` is ignored on insert (the store mints it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcForensicReportRow {
    pub id: i64,
    pub submitter: String,
    pub our_domain: String,
    pub received_at: i64,
    pub reported_message_id: String,
    pub reported_arrival_date: i64,
    pub payload_sha256: Vec<u8>,
    pub payload_raw_gz: Vec<u8>,
    pub parsed_summary_cbor: Vec<u8>,
    pub source_envelope_blob: Vec<u8>,
}

const FORENSIC_COLUMNS: &str = "id, submitter, our_domain, received_at, reported_message_id, \
     reported_arrival_date, payload_sha256, payload_raw_gz, parsed_summary_cbor, \
     source_envelope_blob";

fn forensic_row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<DmarcForensicReportRow> {
    Ok(DmarcForensicReportRow {
        id: r.get(0)?,
        submitter: r.get(1)?,
        our_domain: r.get(2)?,
        received_at: r.get(3)?,
        reported_message_id: r.get(4)?,
        reported_arrival_date: r.get(5)?,
        payload_sha256: r.get(6)?,
        payload_raw_gz: r.get(7)?,
        parsed_summary_cbor: r.get(8)?,
        source_envelope_blob: r.get(9)?,
    })
}

impl CacheDb {
    /// Store one forensic report. `false` when the same message
    /// (`payload_sha256`) is already stored — a peer re-sent it, and the first
    /// copy stands.
    pub async fn insert_dmarc_forensic_report(&self, row: &DmarcForensicReportRow) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO dmarc_inbound_forensic_reports \
                 (submitter, our_domain, received_at, reported_message_id, \
                  reported_arrival_date, payload_sha256, payload_raw_gz, \
                  parsed_summary_cbor, source_envelope_blob) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    row.submitter,
                    row.our_domain,
                    row.received_at,
                    row.reported_message_id,
                    row.reported_arrival_date,
                    row.payload_sha256,
                    row.payload_raw_gz,
                    row.parsed_summary_cbor,
                    row.source_envelope_blob,
                ],
            )
            .context("insert dmarc forensic report")?;
        Ok(n == 1)
    }

    /// Forensic reports received at or after `since`, optionally for one of
    /// our domains, newest first, at most `limit` rows. Neither the archive nor
    /// the digest is read.
    pub async fn list_dmarc_forensic_reports(
        &self,
        since: i64,
        our_domain: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DmarcForensicReportRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, submitter, our_domain, received_at, reported_message_id, \
                        reported_arrival_date, X'', X'', parsed_summary_cbor, X'' \
                 FROM dmarc_inbound_forensic_reports \
                 WHERE received_at >= ?1 AND (?2 IS NULL OR our_domain = ?2) \
                 ORDER BY received_at DESC, id DESC \
                 LIMIT ?3",
            )
            .context("prepare list dmarc forensic reports")?;
        let rows = stmt
            .query_map(
                rusqlite::params![since, our_domain, limit as i64],
                forensic_row_from,
            )
            .context("list dmarc forensic reports")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read dmarc forensic report row")?;
        Ok(rows)
    }

    /// One forensic report by its id, archive included.
    pub async fn get_dmarc_forensic_report(
        &self,
        id: i64,
    ) -> Result<Option<DmarcForensicReportRow>> {
        use rusqlite::OptionalExtension;
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {FORENSIC_COLUMNS} FROM dmarc_inbound_forensic_reports WHERE id = ?1"),
            rusqlite::params![id],
            forensic_row_from,
        )
        .optional()
        .context("get dmarc forensic report")
    }

    /// Drop forensic reports received before `cutoff` (§ Retention). Returns
    /// the count.
    pub async fn prune_dmarc_forensic_reports(&self, cutoff: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM dmarc_inbound_forensic_reports WHERE received_at < ?1",
            rusqlite::params![cutoff],
        )
        .context("prune dmarc forensic reports")
    }
}

/// One outbound report part as queued to one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmarcOutboundReportRow {
    pub report_id: String,
    pub peer_domain: String,
    /// The `mailto:` address the part was queued to.
    pub destination: String,
    pub begin_at: i64,
    pub end_at: i64,
    pub sent_at: i64,
    pub payload_xml_gz: Vec<u8>,
}

impl CacheDb {
    /// Record one queued outbound report part. `false` when this
    /// `(report_id, destination)` is already recorded.
    pub async fn insert_dmarc_outbound_report(&self, row: &DmarcOutboundReportRow) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO dmarc_outbound_aggregate_reports \
                 (report_id, peer_domain, destination, begin_at, end_at, sent_at, payload_xml_gz) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    row.report_id,
                    row.peer_domain,
                    row.destination,
                    row.begin_at,
                    row.end_at,
                    row.sent_at,
                    row.payload_xml_gz,
                ],
            )
            .context("insert dmarc outbound report")?;
        Ok(n == 1)
    }

    /// Drop outbound reports sent before `cutoff`. Returns the count.
    pub async fn prune_dmarc_outbound_reports(&self, cutoff: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM dmarc_outbound_aggregate_reports WHERE sent_at < ?1",
            rusqlite::params![cutoff],
        )
        .context("prune dmarc outbound reports")
    }

    /// Test-only: every outbound report row, oldest first, archive included.
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub async fn fetch_dmarc_outbound_reports_for_test(
        &self,
    ) -> Result<Vec<DmarcOutboundReportRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT report_id, peer_domain, destination, begin_at, end_at, sent_at, \
                        payload_xml_gz \
                 FROM dmarc_outbound_aggregate_reports ORDER BY sent_at, report_id, destination",
            )
            .context("prepare fetch dmarc outbound reports")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DmarcOutboundReportRow {
                    report_id: r.get(0)?,
                    peer_domain: r.get(1)?,
                    destination: r.get(2)?,
                    begin_at: r.get(3)?,
                    end_at: r.get(4)?,
                    sent_at: r.get(5)?,
                    payload_xml_gz: r.get(6)?,
                })
            })
            .context("fetch dmarc outbound reports")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read dmarc outbound report row")?;
        Ok(rows)
    }
}
