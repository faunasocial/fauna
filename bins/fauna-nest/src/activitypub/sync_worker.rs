//! Background worker for outbound ActivityPub delivery.

use crate::db::CacheDb;
use std::sync::Arc;
use tokio::sync::Notify;

/// Worker that delivers queued outbound ActivityPub activities.
///
/// Producers never hand activities to the worker directly: they write them to
/// `ap_delivery_queue` (durable — an activity survives a nest restart) and then
/// nudge, and the worker drains the queue. The 30s poll is the backstop that
/// makes a missed nudge cost latency rather than the activity.
/// No shared `http_client` here on purpose: every delivery dials an inbox URL a
/// remote chose, so each one builds its own SSRF-guarded, DNS-pinned client via
/// `super::outbound`.
/// No `domain` field on purpose: the worker composes no URLs (every job carries
/// its own inbox URL and pre-rendered activity), so the only thing a boot-time
/// domain snapshot ever did here was print `localhost` in the startup log of
/// every provisioned box — a misleading diagnostic for the very bug this module
/// was fixed for (`crate::state::ActivityPubState` docs).
pub struct ApSyncWorker {
    db: Arc<CacheDb>,
    delivery_nudge: Arc<Notify>,
    nest_signing_key_bytes: [u8; 32],
}

impl ApSyncWorker {
    pub fn new(
        db: Arc<CacheDb>,
        delivery_nudge: Arc<Notify>,
        nest_signing_key_bytes: [u8; 32],
    ) -> Self {
        Self {
            db,
            delivery_nudge,
            nest_signing_key_bytes,
        }
    }

    pub async fn run(self) {
        tracing::info!("ActivityPub sync worker started");
        let mut poll_interval = tokio::time::interval(std::time::Duration::from_secs(30));
        let mut cleanup_interval = tokio::time::interval(std::time::Duration::from_secs(3600)); // hourly

        loop {
            tokio::select! {
                // A producer enqueued something and wants it delivered now.
                // `notify_one` leaves a permit if we were mid-drain, so a nudge
                // arriving during `process_delivery_queue` wakes us right after
                // rather than being lost.
                _ = self.delivery_nudge.notified() => {
                    self.process_delivery_queue().await;
                }
                _ = poll_interval.tick() => {
                    self.process_delivery_queue().await;
                }
                _ = cleanup_interval.tick() => {
                    self.run_cleanup().await;
                }
            }
        }
    }

    async fn process_delivery_queue(&self) {
        let jobs = {
            let conn = self.db.conn().await;
            match super::db_helpers::get_pending_deliveries(&conn, 50) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!(error = %e, "failed to get pending AP deliveries");
                    return;
                }
            }
        };

        for job in jobs {
            // Skip delivery to known-dead inboxes.
            {
                let conn = self.db.conn().await;
                if super::db_helpers::is_inbox_dead(&conn, &job.target_inbox).unwrap_or(false) {
                    tracing::debug!(target_inbox = %job.target_inbox, "skipping dead inbox");
                    let _ = super::db_helpers::mark_delivery_failed(&conn, job.id);
                    continue;
                }
            }

            match self.deliver(&job).await {
                Ok(()) => {
                    let conn = self.db.conn().await;
                    let _ = super::db_helpers::mark_delivery_done(&conn, job.id);
                    let _ = super::db_helpers::record_inbox_success(&conn, &job.target_inbox);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        target_inbox = %job.target_inbox,
                        attempts = job.attempts + 1,
                        "AP delivery failed",
                    );
                    let conn = self.db.conn().await;
                    let _ = super::db_helpers::record_inbox_failure(&conn, &job.target_inbox);
                    // After 3 total attempts (0-indexed: 0, 1, 2) mark permanently failed.
                    if job.attempts >= 2 {
                        if let Err(e) = super::db_helpers::mark_delivery_failed(&conn, job.id) {
                            tracing::error!(error = %e, job_id = job.id, "failed to mark AP delivery failed");
                        }
                    } else if let Err(e) = super::db_helpers::mark_delivery_retry(&conn, job.id) {
                        tracing::error!(error = %e, job_id = job.id, "failed to schedule AP delivery retry");
                    }
                }
            }
        }
    }

    /// Periodic cleanup: remove stale follows to dead inboxes, purge old delivery queue entries.
    async fn run_cleanup(&self) {
        let conn = self.db.conn().await;
        match super::db_helpers::cleanup_stale_follows(&conn) {
            Ok(n) if n > 0 => tracing::info!(
                count = n,
                "AP cleanup: removed stale follows to dead inboxes"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "AP cleanup: failed to remove stale follows"),
        }
        match super::db_helpers::cleanup_old_deliveries(&conn) {
            Ok(n) if n > 0 => {
                tracing::debug!(count = n, "AP cleanup: purged old delivery queue entries")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "AP cleanup: failed to purge old deliveries"),
        }
    }

    async fn deliver(&self, job: &super::db_helpers::DeliveryJob) -> anyhow::Result<()> {
        use fauna_bridge_activitypub::http_signatures::{build_signature_header, compute_digest};

        // `target_inbox` is whatever the remote's actor document named, so this
        // dial is caller-supplied and goes through the shared SSRF guard — see
        // `super::outbound`. Rejection here is terminal for the job (the retry
        // will re-reject), not a transient network error.
        let (client, url) = super::outbound::ap_outbound_client(&job.target_inbox).await?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("no host in inbox URL"))?
            .to_owned();
        let path = super::outbound::signing_path(&url);

        let body_bytes = job.activity_json.as_bytes();
        let digest = compute_digest(body_bytes);
        let date = format_http_date(fauna_core::data::Timestamp::now_secs_or_zero());

        // Parse the activity JSON to extract the actor URL, then derive key_id.
        let activity: serde_json::Value = serde_json::from_str(&job.activity_json)?;
        let actor_url = activity["actor"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("activity JSON missing 'actor' field"))?;
        let key_id = format!("{actor_url}#main-key");

        // The username is the last path segment of the actor URL.
        let username = actor_url.rsplit('/').next().ok_or_else(|| {
            anyhow::anyhow!("cannot extract username from actor URL: {actor_url}")
        })?;

        let account = {
            let conn = self.db.conn().await;
            super::db_helpers::get_account_by_username(&conn, username)?
                .ok_or_else(|| anyhow::anyhow!("no AP account found for username '{username}'"))?
        };

        let privkey_der = crate::activitypub::key_crypto::decrypt_rsa_privkey(
            &self.nest_signing_key_bytes,
            &account.encrypted_privkey,
        )?;

        let signature_header = build_signature_header(
            &key_id,
            &privkey_der,
            "post",
            &path,
            &host,
            &date,
            Some(&digest),
        )?;

        let resp = client
            .post(url)
            .header("Content-Type", "application/activity+json")
            .header("Date", &date)
            .header("Digest", &digest)
            .header("Signature", &signature_header)
            .header("Host", &host)
            .body(body_bytes.to_vec())
            .send()
            .await?;
        delivery_outcome(&job.target_inbox, resp).await
    }
}

/// A delivery reply's verdict. The inbox is one a remote's actor document
/// named, so an error body is read only up to the AP error-body cap.
async fn delivery_outcome(target_inbox: &str, resp: reqwest::Response) -> anyhow::Result<()> {
    let status = resp.status();
    if status.is_success() || status.as_u16() == 202 {
        tracing::debug!(%target_inbox, %status, "AP delivery succeeded");
        Ok(())
    } else {
        let body = super::outbound::error_body_snippet(resp).await;
        anyhow::bail!("HTTP {status}: {body}");
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// Format epoch seconds as an RFC 7231 HTTP-date string, e.g.
/// `"Thu, 01 Jan 1970 00:00:00 GMT"`.  The grammar belongs to
/// [`fauna_core::imf_date`] — RFC 7231's `IMF-fixdate` is the RFC 5322
/// date-time with a fixed `GMT` zone, and the mail, RSS and TLSRPT emitters
/// print the same string with `+0000`. No chrono, and no second copy.
///
/// Shared with the signed outbound `GET` (`inbox_routes::fetch_remote_actor`):
/// both directions sign a `Date` header, and Mastodon parses it with Ruby's
/// strict `Time.httpdate`, so there must be exactly one formatter — the pins
/// below are what stop a second one from drifting into a rejected signature.
pub(super) fn format_http_date(epoch_secs: i64) -> String {
    fauna_core::imf_date::format_http_date(epoch_secs)
}

/// Parse an RFC 7231 IMF-fixdate back to epoch seconds — the exact inverse of
/// [`format_http_date`], and deliberately its neighbour.
///
/// The inbound freshness window (`inbox_routes`, the replay floor) needs to
/// read the `Date` a peer signed. That is the same wire grammar this module
/// already owns, and the formatter's own doc explains why it must stay a single
/// point: "there must be exactly one formatter — the pins below are what stop a
/// second one from drifting into a rejected signature." A parser written
/// somewhere else would be that second point, one direction later.
///
/// **Strict IMF-fixdate only.** RFC 7231 tells recipients to also accept the
/// obsolete RFC 850 and asctime forms, and this deliberately does not: the
/// value is an input to a *signature freshness* decision, the codebase's own
/// signer emits IMF-fixdate, and Mastodon signs with Ruby's strict
/// `Time.httpdate`, which is the same grammar. Accepting looser forms here
/// would widen a security check's input surface to buy compatibility with a
/// sender no fediverse implementation is. A peer sending one is refused
/// loudly (`None`) rather than silently treated as fresh.
///
/// Returns `None` on any deviation — width, ordering, an unknown month, a
/// non-`GMT` zone — so a caller cannot mistake "unparseable" for "in window".
pub(super) fn parse_http_date(s: &str) -> Option<i64> {
    // "Thu, 01 Jan 1970 00:00:00 GMT" — fixed width by construction.
    let b = s.as_bytes();
    if b.len() != 29 || &b[3..5] != b", " || b[7] != b' ' || b[11] != b' ' || b[16] != b' ' {
        return None;
    }
    if &b[25..] != b" GMT" || b[19] != b':' || b[22] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if !t.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        t.parse::<i64>().ok()
    };
    let day = num(5..7)?;
    // The month vocabulary is the formatter's, read back through its owner —
    // a private table here could accept a month the emitter never writes.
    let month = i64::from(fauna_core::imf_date::month_from_abbrev(s.get(8..11)?)?);
    let year = num(12..16)?;
    let (hh, mm, ss) = (num(17..19)?, num(20..22)?, num(23..25)?);
    if !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }

    // `fauna_core::caltime::days_from_civil` is the exact inverse of the
    // `civil_from_days` the formatter above calls, so the two directions cannot
    // disagree about any instant — they are now one algorithm, not two copies.
    // Every field is already range-checked above, so the casts cannot truncate.
    let days = fauna_core::caltime::days_from_civil(year as i32, month as u32, day as u32);
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::{delivery_outcome, format_http_date, parse_http_date};

    /// An inbox that answers a delivery with an endless error body: the
    /// failure keeps a bounded prefix instead of buffering the stream.
    #[tokio::test]
    async fn an_endless_delivery_error_body_is_cut_at_the_cap() {
        use crate::ssrf::test_server::{get, head_without_length, serve_once, within};
        let url = serve_once(
            head_without_length("500 Internal Server Error"),
            vec![b'e'; 8192],
            true,
        )
        .await;
        let err = within(delivery_outcome("https://r.example/inbox", get(&url).await))
            .await
            .expect_err("a 500 is a failed delivery");
        let msg = err.to_string();
        assert!(msg.starts_with("HTTP 500"), "status lost: {msg}");
        assert!(
            msg.len() < 5 * 1024,
            "error body not capped: {} bytes",
            msg.len()
        );
    }

    #[tokio::test]
    async fn an_accepted_delivery_is_ok() {
        use crate::ssrf::test_server::{get, head_with_length, serve_once};
        let url = serve_once(head_with_length("202 Accepted", 0), Vec::new(), false).await;
        assert!(
            delivery_outcome("https://r.example/inbox", get(&url).await)
                .await
                .is_ok()
        );
    }

    // These pin the exact RFC 7231 IMF-fixdate grammar. Mastodon parses the
    // signed `Date` header with Ruby's strict `Time.httpdate` — any deviation
    // (ISO 8601, a two-digit year, a wrong field width) fails remote
    // signature verification, which no in-repo mock can surface.

    #[test]
    fn epoch_is_thu_01_jan_1970() {
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    }

    #[test]
    fn leap_day_2024() {
        // 2024-02-29 12:34:56 UTC.
        assert_eq!(
            format_http_date(1_709_210_096),
            "Thu, 29 Feb 2024 12:34:56 GMT"
        );
    }

    #[test]
    fn current_era_date() {
        // 2026-07-19 00:00:00 UTC — a Sunday.
        assert_eq!(
            format_http_date(1_784_419_200),
            "Sun, 19 Jul 2026 00:00:00 GMT"
        );
    }

    #[test]
    fn single_digit_fields_are_zero_padded() {
        // 2026-07-05 03:04:05 UTC — day, hour, minute, second all < 10.
        assert_eq!(
            format_http_date(1_783_220_645),
            "Sun, 05 Jul 2026 03:04:05 GMT"
        );
    }

    /// The inverse property, over the instants the pins above name plus a wide
    /// sweep. `parse_http_date` feeds a *security* decision (the inbound
    /// freshness window), and the failure that matters is not a wrong string —
    /// it is a wrong `i64`, which no formatting assertion can see. Round-tripping
    /// is what makes the two directions one grammar rather than two.
    #[test]
    fn parse_is_the_exact_inverse_of_format() {
        let mut t = -2_208_988_800i64; // 1900-01-01, well before any HTTP date
        while t < 4_102_444_800 {
            // …to 2100-01-01
            assert_eq!(
                parse_http_date(&format_http_date(t)),
                Some(t),
                "round trip failed at {t} ({})",
                format_http_date(t)
            );
            t += 86_400 * 13 + 3_671; // a stride coprime with day/week/year
        }
    }

    /// A parser that answered `Some` for garbage would hand the freshness
    /// window a number it invented, so every rejection path is pinned. The
    /// obsolete RFC 850 / asctime forms are refused deliberately — see
    /// [`parse_http_date`]'s doc for why a security input stays strict.
    #[test]
    fn parse_refuses_everything_that_is_not_imf_fixdate() {
        for bad in [
            "",
            "Thu, 01 Jan 1970 00:00:00 UTC",    // wrong zone
            "Thu, 01 Jan 1970 00:00:00 +0000",  // wrong zone form
            "Thursday, 01-Jan-70 00:00:00 GMT", // RFC 850
            "Thu Jan  1 00:00:00 1970",         // asctime
            "1970-01-01T00:00:00Z",             // ISO 8601
            "Thu, 1 Jan 1970 00:00:00 GMT",     // unpadded day
            "Thu, 01 Xxx 1970 00:00:00 GMT",    // unknown month
            "Thu, 01 Jan 1970 24:00:00 GMT",    // hour out of range
            "Thu, 01 Jan 1970 00:60:00 GMT",    // minute out of range
            "Thu, 01 Jan 197o 00:00:00 GMT",    // non-digit in year
            "Thu, 01 Jan 1970 00:00:00 GMT ",   // trailing space
            " Thu, 01 Jan 1970 00:00:00 GMT",   // leading space
        ] {
            assert_eq!(parse_http_date(bad), None, "must refuse {bad:?}");
        }
    }

    /// The day-of-week field is **not** consulted: RFC 7231 makes it redundant
    /// with the date, and a sender whose clock is right but whose weekday is
    /// wrong is a compatibility problem, not a freshness one. Pinned so the
    /// behaviour is a decision rather than an oversight — and so that adding a
    /// weekday check later is a deliberate, test-reddening act.
    #[test]
    fn a_wrong_day_of_week_is_ignored_not_rejected() {
        assert_eq!(parse_http_date("Mon, 01 Jan 1970 00:00:00 GMT"), Some(0));
    }
}
