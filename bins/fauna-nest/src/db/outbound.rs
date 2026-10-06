//! Outbound mail queue, bounce history, and TLSRPT report rows.
//!
//! Implements `docs/goal/behavior/smtp-server.md` § Outbound delivery. One
//! row per (message, recipient) pair so each recipient runs its own retry
//! curve. Bounce history backs the NDR rate-limit; `tlsrpt_outbound_reports`
//! retains gzipped report payloads for 7 days.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use super::{CacheDb, now_epoch_secs};

/// Default retention for `tlsrpt_outbound_reports` per
/// `docs/goal/behavior/smtp-server.md` § TLSRPT outbound reporter
/// ("retain the raw JSON for 7 days").
pub const DEFAULT_TLSRPT_OUTBOUND_RETENTION: Duration = Duration::from_secs(7 * 86_400);

/// Spawn a tokio task that periodically sweeps TLSRPT outbound report
/// rows older than `retention` per the 7-day retention rule.
///
/// Cadence mirrors `spawn_audit_retention_sweeper` — 1/24 of the
/// retention. With the 7-day default that sweeps every ~7 hours, rare
/// enough that the SQLite lock is held only briefly each tick.
pub fn spawn_tlsrpt_outbound_retention_sweeper(
    db: Arc<CacheDb>,
    retention: Duration,
) -> tokio::task::JoinHandle<()> {
    super::spawn_retention_sweeper(retention, move || {
        let db = db.clone();
        async move {
            let now = now_epoch_secs();
            let retention_secs = retention.as_secs() as i64;
            match db.sweep_tlsrpt_outbound_reports(now, retention_secs).await {
                Ok(n) if n > 0 => tracing::info!(
                    target: "tlsrpt_outbound",
                    pruned = n,
                    retention_secs,
                    "swept aged TLSRPT outbound reports"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "tlsrpt_outbound",
                    error = %e,
                    "tlsrpt outbound retention sweep failed"
                ),
            }
        }
    })
}

#[derive(Debug, Clone)]
pub struct InboundVerdictsSnapshot {
    pub spf: String,
    pub dmarc: String,
    pub dmarc_policy: String,
}

#[derive(Debug, Clone)]
pub struct NewOutbound<'a> {
    pub original_msgid: &'a str,
    pub original_sender: &'a str,
    pub recipients: &'a [&'a str],
    pub raw_message: &'a [u8],
    pub inbound_verdicts: InboundVerdictsSnapshot,
    pub is_forwarded: bool,
    /// Forward attribution (`mail-forwarding.md` N2). `None` on a normal
    /// outbound submission; on a forwarded row, the forwarding actor — read
    /// by the SRS rewrite at queue-out (N3) and NDR routing (N4).
    pub forward_actor_id: Option<&'a [u8; 32]>,
    /// `"forward-all"` or a filter rule id; `None` on a normal submission.
    pub forward_rule_id: Option<&'a str>,
    /// A forwarded row's copy mode — a second copy (`Copy`) or the message's
    /// only copy (`Redirect`); `None` on every non-forward row.
    pub forward_copy_mode: Option<fauna_protocol::bridge_routing::ForwardCopyMode>,
    /// Submission attribution: the **authenticated actor** that put this row on
    /// the queue through `fauna.email.send`. Read by
    /// [`CacheDb::count_outbound_by_actor_window`], the per-hour outbound
    /// ceiling's key — deliberately not `original_sender`, which is the
    /// caller's own `From:` header and therefore a value the caller chooses
    /// (`mail-app-surface.md` § Outbound metering). `None` from every other
    /// producer: each carries its own metering, and none of them is the
    /// ceiling's subject.
    pub submit_actor_id: Option<&'a [u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundStatus {
    Pending,
    Sent,
    PermFail,
    Bounced,
    SuppressedRate,
    SuppressedBackscatter,
}

impl OutboundStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::PermFail => "permfail",
            Self::Bounced => "bounced",
            Self::SuppressedRate => "suppressed_rate",
            Self::SuppressedBackscatter => "suppressed_backscatter",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "sent" => Self::Sent,
            "permfail" => Self::PermFail,
            "bounced" => Self::Bounced,
            "suppressed_rate" => Self::SuppressedRate,
            "suppressed_backscatter" => Self::SuppressedBackscatter,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuppressReason {
    SpfHardfail,
    DmarcRejectQuarantine,
    NullSender,
    ForwardedAndExternallyBounced,
    NdrRateLimit,
}

impl SuppressReason {
    fn target_status(&self) -> OutboundStatus {
        match self {
            Self::NdrRateLimit => OutboundStatus::SuppressedRate,
            _ => OutboundStatus::SuppressedBackscatter,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutboundRow {
    pub id: i64,
    pub original_msgid: String,
    pub original_sender: String,
    pub recipient: String,
    pub raw_message: Vec<u8>,
    pub attempt_count: u32,
    pub next_attempt_at: i64,
    pub delay_warned_at: Option<i64>,
    pub status: OutboundStatus,
    pub last_error: Option<String>,
    pub last_enhanced: Option<String>,
    pub inbound_verdicts: InboundVerdictsSnapshot,
    pub is_forwarded: bool,
    /// Forward attribution (`mail-forwarding.md` N2): the forwarding actor on
    /// a forwarded row, `None` otherwise. Read by N3 (SRS) / N4 (NDR).
    pub forward_actor_id: Option<[u8; 32]>,
    pub forward_rule_id: Option<String>,
    pub created_at: i64,
}

impl CacheDb {
    /// Insert one row per recipient into `outbound_mail_queue`. Returns the
    /// new row ids in the same order as `recipients`.
    pub async fn enqueue_outbound(&self, fields: NewOutbound<'_>) -> Result<Vec<i64>> {
        self.enqueue_outbound_at(fields, now_epoch_secs()).await
    }

    /// Insert one row per recipient with an explicit `now` for both
    /// `created_at` and `next_attempt_at`. Used by the test-hooks driver so
    /// the e2e mock clock owns the row's birth-time (and the bridge's
    /// `wall_clock_elapsed = now - created_at` retry math stays consistent
    /// with the test's `advance_clock` steps).
    pub async fn enqueue_outbound_at(&self, fields: NewOutbound<'_>, now: i64) -> Result<Vec<i64>> {
        self.enqueue_outbound_split(fields, now, now).await
    }

    /// Insert one row per recipient with `created_at` and `next_attempt_at`
    /// supplied **separately**. The fresh-IP warm-up deferral
    /// (`mail-deliverability.md` § Enforcement at submission time → "queued for
    /// tomorrow") uses this to stamp `next_attempt_at = next_utc_midnight(now)`
    /// while keeping `created_at = now` (so the permanent-failure-timeout clock
    /// is honest about when the user submitted, not when delivery starts).
    pub async fn enqueue_outbound_split(
        &self,
        fields: NewOutbound<'_>,
        created_at: i64,
        next_attempt_at: i64,
    ) -> Result<Vec<i64>> {
        let msgid = fields.original_msgid.to_string();
        let sender = fields.original_sender.to_string();
        let recipients: Vec<String> = fields.recipients.iter().map(|s| s.to_string()).collect();
        let raw = fields.raw_message.to_vec();
        let spf = fields.inbound_verdicts.spf.clone();
        let dmarc = fields.inbound_verdicts.dmarc.clone();
        let dmarc_pol = fields.inbound_verdicts.dmarc_policy.clone();
        let is_forwarded = fields.is_forwarded;
        let forward_actor_id: Option<Vec<u8>> =
            fields.forward_actor_id.map(|a| a.as_slice().to_vec());
        let forward_rule_id: Option<String> = fields.forward_rule_id.map(str::to_string);
        let forward_copy_mode: Option<&'static str> = fields
            .forward_copy_mode
            .map(super::forward_queue::copy_mode_sql);
        let submit_actor_id: Option<Vec<u8>> =
            fields.submit_actor_id.map(|a| a.as_slice().to_vec());

        let conn = self.conn.lock().await;
        let mut ids = Vec::with_capacity(recipients.len());
        for recipient in &recipients {
            conn.execute(
                "INSERT INTO outbound_mail_queue (
                    original_msgid, original_sender, recipient, raw_message,
                    attempt_count, next_attempt_at, status,
                    inbound_spf, inbound_dmarc, inbound_dmarc_pol, is_forwarded,
                    forward_actor_id, forward_rule_id,
                    created_at, submit_actor_id, forward_copy_mode
                ) VALUES (?1, ?2, ?3, ?4, 0, ?5, 'pending', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    msgid,
                    sender,
                    recipient,
                    raw,
                    next_attempt_at,
                    spf,
                    dmarc,
                    dmarc_pol,
                    is_forwarded as i64,
                    forward_actor_id,
                    forward_rule_id,
                    created_at,
                    submit_actor_id,
                    forward_copy_mode,
                ],
            )
            .context("insert outbound_mail_queue row")?;
            ids.push(conn.last_insert_rowid());
        }
        Ok(ids)
    }

    /// Fetch up to `limit` outbound rows whose `next_attempt_at <= now` and
    /// whose status is `pending`.
    pub async fn fetch_due_outbound(&self, now: i64, limit: u32) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, original_msgid, original_sender, recipient, raw_message,
                        attempt_count, next_attempt_at, delay_warned_at, status,
                        last_error, last_enhanced,
                        inbound_spf, inbound_dmarc, inbound_dmarc_pol, is_forwarded,
                        forward_actor_id, forward_rule_id,
                        created_at
                 FROM outbound_mail_queue
                 WHERE status = 'pending' AND next_attempt_at <= ?1
                 ORDER BY next_attempt_at LIMIT ?2",
            )
            .context("prepare fetch_due_outbound")?;
        let rows = stmt
            .query_map(rusqlite::params![now, limit], row_to_outbound)
            .context("query fetch_due_outbound")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read outbound_mail_queue row")?);
        }
        Ok(out)
    }

    /// Fetch one outbound row by id, regardless of status. Used by the
    /// production bounce path (`mark_outbound_bounced` handler →
    /// `outbound_bounce::generate_permfail_bounce`) to read the row's
    /// stored sender / msgid / raw message / inbound verdicts when
    /// building the NDR.
    pub async fn fetch_outbound_by_id(&self, id: i64) -> Result<Option<OutboundRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, original_msgid, original_sender, recipient, raw_message,
                        attempt_count, next_attempt_at, delay_warned_at, status,
                        last_error, last_enhanced,
                        inbound_spf, inbound_dmarc, inbound_dmarc_pol, is_forwarded,
                        forward_actor_id, forward_rule_id,
                        created_at
                 FROM outbound_mail_queue WHERE id = ?1",
            )
            .context("prepare fetch_outbound_by_id")?;
        let mut rows = stmt
            .query_map(rusqlite::params![id], row_to_outbound)
            .context("query fetch_outbound_by_id")?;
        match rows.next() {
            Some(row) => Ok(Some(row.context("read outbound_mail_queue row")?)),
            None => Ok(None),
        }
    }

    /// Schedule the next attempt for a row. Bumps `attempt_count`, sets
    /// `next_attempt_at`, and records the diagnostic from the failed wire
    /// response.
    pub async fn mark_outbound_attempt(
        &self,
        id: i64,
        next_attempt_at: i64,
        last_error: Option<&str>,
        last_enhanced: Option<&str>,
    ) -> Result<()> {
        let last_error = last_error.map(str::to_string);
        let last_enhanced = last_enhanced.map(str::to_string);
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue
             SET attempt_count = attempt_count + 1,
                 next_attempt_at = ?1,
                 last_error = ?2,
                 last_enhanced = ?3
             WHERE id = ?4",
            rusqlite::params![next_attempt_at, last_error, last_enhanced, id],
        )
        .context("mark outbound attempt")?;
        Ok(())
    }

    pub async fn mark_outbound_sent(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue SET status = 'sent' WHERE id = ?1",
            rusqlite::params![id],
        )
        .context("mark outbound sent")?;
        Ok(())
    }

    pub async fn mark_outbound_permfail(
        &self,
        id: i64,
        last_error: &str,
        last_enhanced: &str,
    ) -> Result<()> {
        let last_error = last_error.to_string();
        let last_enhanced = last_enhanced.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue
             SET status = 'permfail', last_error = ?1, last_enhanced = ?2
             WHERE id = ?3",
            rusqlite::params![last_error, last_enhanced, id],
        )
        .context("mark outbound permfail")?;
        Ok(())
    }

    pub async fn mark_outbound_bounced(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue SET status = 'bounced' WHERE id = ?1",
            rusqlite::params![id],
        )
        .context("mark outbound bounced")?;
        Ok(())
    }

    /// Same as `mark_outbound_bounced` but also persists the bridge's
    /// final-reason diagnostic into `last_error` for NDR formatting and
    /// log correlation. Used by the I4 Go-MTA bridge's
    /// `mark_outbound_bounced` handler, where the wire RPC carries a
    /// `reason` field. The legacy in-process queue keeps using the
    /// no-reason form because it builds a separate DSN message at the
    /// same call site.
    pub async fn mark_outbound_bounced_with_reason(&self, id: i64, reason: &str) -> Result<()> {
        let reason = reason.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue SET status = 'bounced', last_error = ?1 WHERE id = ?2",
            rusqlite::params![reason, id],
        )
        .context("mark outbound bounced with reason")?;
        Ok(())
    }

    pub async fn mark_outbound_suppressed(&self, id: i64, reason: SuppressReason) -> Result<()> {
        let status = reason.target_status();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue SET status = ?1 WHERE id = ?2",
            rusqlite::params![status.as_str(), id],
        )
        .context("mark outbound suppressed")?;
        Ok(())
    }

    pub async fn mark_outbound_delay_warned(&self, id: i64, now: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE outbound_mail_queue SET delay_warned_at = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )
        .context("mark outbound delay-warned")?;
        Ok(())
    }

    /// Count rows enqueued by `sender` within the last `window_secs` seconds.
    /// Backs the per-sender outbound rate-limit at the submission boundary.
    pub async fn count_outbound_by_sender_window(
        &self,
        sender: &str,
        window_secs: i64,
    ) -> Result<u32> {
        let sender = sender.to_string();
        let conn = self.conn.lock().await;
        let cutoff = now_epoch_secs() - window_secs;
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbound_mail_queue
                 WHERE original_sender = ?1 AND created_at >= ?2",
                rusqlite::params![sender, cutoff],
                |row| row.get(0),
            )
            .context("count_outbound_by_sender_window")?;
        Ok(count)
    }

    /// Count rows this **authenticated actor** put on the outbound queue
    /// through `fauna.email.send` within the last `window_secs` seconds — the
    /// per-actor sliding window behind the per-hour outbound ceiling
    /// (`mail-app-surface.md` § Outbound metering).
    ///
    /// Same shape as [`Self::count_forward_dispatched_window`] and for the same
    /// reason: a row is the increment regardless of delivery status, and rows
    /// are not pruned within the hour, so `created_at` is an accurate window
    /// count.
    ///
    /// This exists because the ceiling's *previous* key was
    /// [`Self::count_outbound_by_sender_window`] — `original_sender`, the
    /// caller's own `From:` header. An off-domain `From:` deliberately bypasses
    /// the handle gate (the deployment is not authoritative for it), so the
    /// caller could put any string there and a fresh string each message minted
    /// a fresh counter each message. The authenticated
    /// actor is the one value on this path the caller cannot choose.
    ///
    /// Rows with a NULL `submit_actor_id` — system-originated rows (DSN,
    /// TLSRPT, retry) — are therefore *not* counted: `= ?1` never matches NULL
    /// in SQL. That is the intended reading: a system-originated row is not a
    /// user submission, and miscounting it against the wrong actor would
    /// refuse a user mail they never sent.
    pub async fn count_outbound_by_actor_window(
        &self,
        submit_actor_id: &[u8; 32],
        window_secs: i64,
    ) -> Result<u32> {
        let conn = self.conn.lock().await;
        let cutoff = now_epoch_secs() - window_secs;
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbound_mail_queue
                 WHERE submit_actor_id = ?1 AND created_at >= ?2",
                rusqlite::params![submit_actor_id.as_slice(), cutoff],
                |row| row.get(0),
            )
            .context("count_outbound_by_actor_window")?;
        Ok(count)
    }

    /// Count forwarded rows **dispatched** for `forward_actor_id` within the
    /// last `window_secs` seconds — the per-actor sliding window behind the N5
    /// forward rate-cap (`mail-forwarding.md` § Per-account forward rate-limit
    /// `:177`, "same shape as the submission rate-limiter"). Counts every
    /// `is_forwarded` row this actor put on the outbound queue in the window,
    /// regardless of delivery status (a row is the "increment"); rows are not
    /// pruned within the hour, so `created_at` is an accurate window count.
    pub async fn count_forward_dispatched_window(
        &self,
        forward_actor_id: &[u8; 32],
        window_secs: i64,
    ) -> Result<u32> {
        let conn = self.conn.lock().await;
        let cutoff = now_epoch_secs() - window_secs;
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbound_mail_queue
                 WHERE is_forwarded = 1 AND forward_actor_id = ?1 AND created_at >= ?2",
                rusqlite::params![forward_actor_id.as_slice(), cutoff],
                |row| row.get(0),
            )
            .context("count_forward_dispatched_window")?;
        Ok(count)
    }

    /// Check whether a bounce for `(sender, msgid)` has already been sent
    /// within `window_secs` of `now`. Implements the 7-day window from
    /// docs/goal/behavior/smtp-server.md § NDR rate-limit per recipient.
    pub async fn bounce_rate_limit_hit(
        &self,
        sender: &str,
        msgid: &str,
        now: i64,
        window_secs: i64,
    ) -> Result<bool> {
        let sender = sender.to_string();
        let msgid = msgid.to_string();
        let cutoff = now - window_secs;
        let conn = self.conn.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bounce_history
                 WHERE original_sender = ?1 AND original_msgid = ?2
                   AND sent_at >= ?3",
                rusqlite::params![sender, msgid, cutoff],
                |row| row.get(0),
            )
            .context("query bounce_history window")?;
        Ok(count > 0)
    }

    /// Record that a bounce for `(sender, msgid)` was emitted at `now`.
    pub async fn record_bounce(&self, sender: &str, msgid: &str, now: i64) -> Result<()> {
        let sender = sender.to_string();
        let msgid = msgid.to_string();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO bounce_history (original_sender, original_msgid, sent_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![sender, msgid, now],
        )
        .context("insert bounce_history")?;
        Ok(())
    }

    /// Insert a TLSRPT outbound report row. Idempotent on
    /// (recipient_domain, report_date, transport, destination_uri) per
    /// the unique constraint.
    pub async fn insert_tlsrpt_outbound_report(
        &self,
        report_id: &str,
        recipient_domain: &str,
        report_date: &str,
        transport: &str,
        destination_uri: &str,
        payload_json: &[u8],
        submitted_at: i64,
    ) -> Result<()> {
        let report_id = report_id.to_string();
        let recipient_domain = recipient_domain.to_string();
        let report_date = report_date.to_string();
        let transport = transport.to_string();
        let destination_uri = destination_uri.to_string();
        let payload = payload_json.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO tlsrpt_outbound_reports
                 (report_id, recipient_domain, report_date, transport,
                  destination_uri, payload_json, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                report_id,
                recipient_domain,
                report_date,
                transport,
                destination_uri,
                payload,
                submitted_at,
            ],
        )
        .context("insert tlsrpt_outbound_reports row")?;
        Ok(())
    }

    /// Sweep TLSRPT reports older than `retention_secs` per the 7-day
    /// retention rule. Returns the number of rows removed.
    pub async fn sweep_tlsrpt_outbound_reports(
        &self,
        now: i64,
        retention_secs: i64,
    ) -> Result<u64> {
        let conn = self.conn.lock().await;
        let cutoff = now - retention_secs;
        let removed = conn
            .execute(
                "DELETE FROM tlsrpt_outbound_reports WHERE submitted_at < ?1",
                rusqlite::params![cutoff],
            )
            .context("sweep tlsrpt_outbound_reports")? as u64;
        Ok(removed)
    }

    /// Test-only: wipe the three outbound tables so the e2e `/reset`
    /// endpoint starts each test from a clean slate. Gated here directly
    /// (convention 15 rule (a)) as well as on
    /// `test-hooks` at the route layer that calls it — the route gate alone
    /// left this destructive method itself compiled into every release
    /// artifact, reachable by any future in-process caller.
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub async fn clear_outbound_for_test(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM outbound_mail_queue", [])
            .context("clear outbound_mail_queue")?;
        conn.execute("DELETE FROM bounce_history", [])
            .context("clear bounce_history")?;
        conn.execute("DELETE FROM tlsrpt_outbound_reports", [])
            .context("clear tlsrpt_outbound_reports")?;
        Ok(())
    }

    /// Test-only: dump every bounce_history row as a JSON-friendly map.
    /// The e2e `/bounce_history` endpoint returns this verbatim.
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub async fn fetch_bounce_history_for_test(&self) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT original_sender, original_msgid, sent_at FROM bounce_history
                 ORDER BY sent_at",
            )
            .context("prepare fetch_bounce_history_for_test")?;
        let rows = stmt
            .query_map([], |row| {
                let sender: String = row.get(0)?;
                let msgid: String = row.get(1)?;
                let sent_at: i64 = row.get(2)?;
                Ok(serde_json::json!({
                    "original_sender": sender,
                    "original_msgid": msgid,
                    "sent_at": sent_at,
                }))
            })
            .context("query fetch_bounce_history_for_test")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read bounce_history row")?);
        }
        Ok(out)
    }

    /// Test-only: dump every tlsrpt_outbound_reports row (no payload bytes
    /// — the test only inspects metadata).
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub async fn fetch_tlsrpt_reports_for_test(&self) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT report_id, recipient_domain, report_date, transport,
                        destination_uri, submitted_at, length(payload_json)
                 FROM tlsrpt_outbound_reports
                 ORDER BY submitted_at, id",
            )
            .context("prepare fetch_tlsrpt_reports_for_test")?;
        let rows = stmt
            .query_map([], |row| {
                let report_id: String = row.get(0)?;
                let recipient_domain: String = row.get(1)?;
                let report_date: String = row.get(2)?;
                let transport: String = row.get(3)?;
                let destination_uri: String = row.get(4)?;
                let submitted_at: i64 = row.get(5)?;
                let payload_len: i64 = row.get(6)?;
                Ok(serde_json::json!({
                    "report_id": report_id,
                    "recipient_domain": recipient_domain,
                    "report_date": report_date,
                    "transport": transport,
                    "destination_uri": destination_uri,
                    "submitted_at": submitted_at,
                    "payload_len": payload_len,
                }))
            })
            .context("query fetch_tlsrpt_reports_for_test")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read tlsrpt_outbound_reports row")?);
        }
        Ok(out)
    }

    /// Test-only: read every row regardless of status. Used by unit tests
    /// to assert status transitions after `mark_*` calls.
    #[cfg(any(test, debug_assertions, feature = "test-hooks"))]
    pub async fn fetch_all_outbound_for_test(&self) -> Result<Vec<OutboundRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, original_msgid, original_sender, recipient, raw_message,
                        attempt_count, next_attempt_at, delay_warned_at, status,
                        last_error, last_enhanced,
                        inbound_spf, inbound_dmarc, inbound_dmarc_pol, is_forwarded,
                        forward_actor_id, forward_rule_id,
                        created_at
                 FROM outbound_mail_queue
                 ORDER BY id",
            )
            .context("prepare fetch_all_outbound_for_test")?;
        let rows = stmt
            .query_map([], row_to_outbound)
            .context("query fetch_all_outbound_for_test")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read outbound_mail_queue row")?);
        }
        Ok(out)
    }
}

fn row_to_outbound(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboundRow> {
    let status_str: String = row.get(8)?;
    let spf: String = row.get(11)?;
    let dmarc: String = row.get(12)?;
    let dmarc_pol: String = row.get(13)?;
    let attempt_count: i64 = row.get(5)?;
    let is_forwarded_int: i64 = row.get(14)?;
    let forward_actor_id: Option<[u8; 32]> = row
        .get::<_, Option<Vec<u8>>>(15)?
        .and_then(|v| v.try_into().ok());
    let forward_rule_id: Option<String> = row.get(16)?;
    Ok(OutboundRow {
        id: row.get(0)?,
        original_msgid: row.get(1)?,
        original_sender: row.get(2)?,
        recipient: row.get(3)?,
        raw_message: row.get(4)?,
        attempt_count: attempt_count.max(0) as u32,
        next_attempt_at: row.get(6)?,
        delay_warned_at: row.get(7)?,
        status: OutboundStatus::parse(&status_str),
        last_error: row.get(9)?,
        last_enhanced: row.get(10)?,
        inbound_verdicts: InboundVerdictsSnapshot {
            spf,
            dmarc,
            dmarc_policy: dmarc_pol,
        },
        is_forwarded: is_forwarded_int != 0,
        forward_actor_id,
        forward_rule_id,
        created_at: row.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_tlsrpt_outbound_retention_sweeper_prunes_aged_rows() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        // Insert a row well in the past (submitted_at = 1 epoch second).
        db.insert_tlsrpt_outbound_report(
            "report-1",
            "example.com",
            "2026-05-14",
            "mailto",
            "mailto:tlsrpt@example.com",
            b"{}",
            1,
        )
        .await
        .unwrap();
        // Retention = 60ms ⇒ tick = 60/24 ≈ 2ms (floored to 1ms by
        // tokio::interval), so on an idle box the prune lands in single-digit
        // milliseconds. The 1-epoch-sec row sweeps because
        // `now - retention_secs` ≫ 1.
        //
        // The assertion is a DEADLINE POLL, not a settle-sleep (testing.md
        // § point 14). This test previously slept a fixed 30ms and then asserted
        // — a budget that holds only on an idle machine, while the primary dev
        // VM routinely runs 20+ concurrent builds at double-digit load, so it
        // was in the defunct wall-clock class and would fail under exactly the
        // conditions it is normally run in. The budget below is sized far above any
        // non-pathological scheduling delay; a green run pays only for the first
        // poll, because the loop exits as soon as the prune is observed.
        const PRUNE_BUDGET: Duration = Duration::from_secs(30);
        let handle = spawn_tlsrpt_outbound_retention_sweeper(db.clone(), Duration::from_millis(60));
        let deadline = std::time::Instant::now() + PRUNE_BUDGET;
        loop {
            let rows = db.fetch_tlsrpt_reports_for_test().await.unwrap();
            if rows.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sweeper did not prune the aged row within {PRUNE_BUDGET:?} \
                 ({} row(s) still present)",
                rows.len()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        handle.abort();
    }

    #[tokio::test]
    async fn spawn_tlsrpt_outbound_retention_sweeper_preserves_fresh_rows() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let now = now_epoch_secs();
        db.insert_tlsrpt_outbound_report(
            "report-1",
            "example.com",
            "2026-05-14",
            "mailto",
            "mailto:tlsrpt@example.com",
            b"{}",
            now,
        )
        .await
        .unwrap();
        // Long retention vs. ~now row → sweeper leaves it alone.
        let handle =
            spawn_tlsrpt_outbound_retention_sweeper(db.clone(), Duration::from_secs(7 * 86_400));
        // One tick of the long-cadence ticker (≈7h/24 ≈ 17 min) won't
        // fire in test time; this just exercises that the spawned task
        // doesn't accidentally prune fresh rows on startup.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let rows = db.fetch_tlsrpt_reports_for_test().await.unwrap();
        assert_eq!(rows.len(), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn split_enqueue_defers_delivery_until_tomorrow() {
        // The fresh-IP warm-up deferral seam (mail-deliverability.md
        // § Enforcement at submission time → "queued for tomorrow"): an
        // over-cap submission is enqueued with next_attempt_at = the next
        // 00:00 UTC while created_at stays the honest submission time, so the
        // row is NOT due today but IS due tomorrow (the MTA already returned
        // 250 OK — the message lands within 24h, never bounced).
        let db = CacheDb::open_in_memory().unwrap();
        let now = 100 * 86_400 + 12 * 3600; // day 100, noon UTC
        let tomorrow = fauna_mail::warmup::next_utc_midnight(now);
        let ids = db
            .enqueue_outbound_split(
                NewOutbound {
                    original_msgid: "deferred-1",
                    original_sender: "alice@example.test",
                    recipients: &["bob@example.com"],
                    raw_message: b"raw",
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "none".into(),
                        dmarc: "none".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                now,
                tomorrow,
            )
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
        // Not due the same day.
        assert!(
            db.fetch_due_outbound(now, 10).await.unwrap().is_empty(),
            "a warm-up-deferred row must not be due the same day"
        );
        // Due tomorrow; created_at is the honest submission time.
        let due = db.fetch_due_outbound(tomorrow, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].next_attempt_at, tomorrow);
        assert_eq!(due[0].created_at, now);
    }

    #[tokio::test]
    async fn count_forward_dispatched_window_is_per_actor_forwarded_only_within_window() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        let other = [8u8; 32];
        let now = now_epoch_secs();

        // Two forwarded rows for `actor` ~now.
        for i in 0..2 {
            db.enqueue_outbound_at(
                NewOutbound {
                    original_msgid: &format!("f{i}"),
                    original_sender: "alice@example.com",
                    recipients: &["bob@other.example"],
                    raw_message: b"raw",
                    inbound_verdicts: InboundVerdictsSnapshot {
                        spf: "none".into(),
                        dmarc: "none".into(),
                        dmarc_policy: "none".into(),
                    },
                    is_forwarded: true,
                    forward_actor_id: Some(&actor),
                    forward_rule_id: Some("forward-all"),
                    forward_copy_mode: Some(fauna_protocol::bridge_routing::ForwardCopyMode::Copy),
                    submit_actor_id: None,
                },
                now,
            )
            .await
            .unwrap();
        }
        // A forwarded row for a *different* actor (must not count).
        db.enqueue_outbound_at(
            NewOutbound {
                original_msgid: "fo",
                original_sender: "alice@example.com",
                recipients: &["x@x.test"],
                raw_message: b"raw",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "none".into(),
                    dmarc: "none".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: true,
                forward_actor_id: Some(&other),
                forward_rule_id: Some("forward-all"),
                forward_copy_mode: Some(fauna_protocol::bridge_routing::ForwardCopyMode::Copy),
                submit_actor_id: None,
            },
            now,
        )
        .await
        .unwrap();
        // A normal (non-forwarded) submission row (must not count).
        db.enqueue_outbound_at(
            NewOutbound {
                original_msgid: "sub",
                original_sender: "alice@example.com",
                recipients: &["y@y.test"],
                raw_message: b"raw",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "none".into(),
                    dmarc: "none".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            },
            now,
        )
        .await
        .unwrap();
        // An aged forwarded row for `actor`, 2h ago (outside a 1h window).
        db.enqueue_outbound_at(
            NewOutbound {
                original_msgid: "aged",
                original_sender: "alice@example.com",
                recipients: &["z@z.test"],
                raw_message: b"raw",
                inbound_verdicts: InboundVerdictsSnapshot {
                    spf: "none".into(),
                    dmarc: "none".into(),
                    dmarc_policy: "none".into(),
                },
                is_forwarded: true,
                forward_actor_id: Some(&actor),
                forward_rule_id: Some("forward-all"),
                forward_copy_mode: Some(fauna_protocol::bridge_routing::ForwardCopyMode::Copy),
                submit_actor_id: None,
            },
            now - 7200,
        )
        .await
        .unwrap();

        // Only the two fresh forwarded rows for `actor` count.
        assert_eq!(
            db.count_forward_dispatched_window(&actor, 3600)
                .await
                .unwrap(),
            2
        );
        // Widen the window past the aged row → 3.
        assert_eq!(
            db.count_forward_dispatched_window(&actor, 10_800)
                .await
                .unwrap(),
            3
        );
    }
}
