//! Production-path TLSRPT outbound daily emitter (RFC 8460).
//!
//! The Go MTA bridge reports each outbound TLS attempt via
//! `fauna.bridges.report_tls_attempt`; the handler records it into
//! `AppState.email.tlsrpt_aggregator`. This module drains that aggregator
//! once per UTC day (00:00 + jitter), fetches each recipient domain's
//! `_smtp._tls.<domain>` policy, builds the RFC 8460 §4.4 report, and submits
//! it per the recipient's `rua=` URIs — `mailto:` rides the standard outbound
//! retry queue; `https:` is POSTed directly. Each shipped transport persists
//! one `tlsrpt_outbound_reports` row for the 7-day retention sweeper.
//!
//! The byte-shaped report helpers (`emit_report`, `populate_transports`,
//! `build_mailto_mime`, the date helpers) and the I/O traits live in
//! permanent `fauna_mail::outbound::tlsrpt`; this module supplies the nest DB
//! + queue wiring and the scheduling loop.
//!
//! docs/goal/behavior/smtp-server.md § TLSRPT outbound reporter.

use std::sync::Arc;
use std::time::Duration;

use fauna_mail::outbound::tlsrpt::{
    ReportDispatchContext, ReportEnvelope, ReportTransport, TlsrptHttpPoster, TlsrptPolicyFetcher,
    format_utc_date, populate_transports, rfc2822_utc, sample_jitter_offset_secs,
    seconds_until_next_utc_midnight,
};

use crate::db::CacheDb;
use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound};
use crate::routes::AppState;

/// Per-domain jitter window (RFC 8460 §4.4 anti-stampede): reports fire at
/// `00:00 UTC + rand(0..3600)s` so a deployment reporting to many recipients
/// doesn't burst every report out at the same instant.
pub const TLSRPT_JITTER_WINDOW_SECS: u32 = 3600;

/// One TLSRPT dispatch pass for a single recipient `domain`. The caller has
/// already built `envelope` via `aggregator.emit_report(...)` and resolved
/// `rua_uris` from the recipient's `_smtp._tls.<domain>` policy. Populates the
/// per-transport bytes, enqueues `mailto:` reports on the outbound queue (from
/// `tlsrpt@<reporting_mta>`, riding the standard retry curve), POSTs `https:`
/// reports, and persists one `tlsrpt_outbound_reports` row per
/// successfully-shipped transport. Returns `(attempted, persisted)`.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_tlsrpt_for_domain(
    db: &CacheDb,
    poster: &dyn TlsrptHttpPoster,
    reporting_mta: &str,
    mut envelope: ReportEnvelope,
    rua_uris: &[String],
    domain: &str,
    report_id: &str,
    report_date: &str,
    now: i64,
) -> (usize, usize) {
    if rua_uris.is_empty() {
        return (0, 0);
    }
    let boundary = format!("tlsrpt-boundary-{report_id}");
    let rfc2822_date = rfc2822_utc(now);
    let ctx = ReportDispatchContext {
        our_domain: reporting_mta,
        recipient_domain: domain,
        report_id,
        report_date,
        message_id: report_id,
        rfc2822_date: &rfc2822_date,
        boundary: &boundary,
    };
    populate_transports(&mut envelope, rua_uris, &ctx);

    let attempted = envelope.transports.len();
    let mut persisted = 0usize;
    let our_sender = format!("tlsrpt@{reporting_mta}");

    for transport in &envelope.transports {
        match transport {
            ReportTransport::Mailto { rcpt, mime_message } => {
                let recipients = [rcpt.as_str()];
                if let Err(e) = db
                    .enqueue_outbound_at(
                        NewOutbound {
                            original_msgid: report_id,
                            original_sender: &our_sender,
                            recipients: &recipients,
                            raw_message: mime_message,
                            // A machine-generated null-sender-style report: no
                            // inbound auth context applies (it originates here).
                            inbound_verdicts: InboundVerdictsSnapshot {
                                spf: String::new(),
                                dmarc: String::new(),
                                dmarc_policy: String::new(),
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
                {
                    tracing::error!(domain = %domain, "TLSRPT mailto enqueue failed: {e}");
                    continue;
                }
                let destination = format!("mailto:{rcpt}");
                if let Err(e) = db
                    .insert_tlsrpt_outbound_report(
                        report_id,
                        domain,
                        report_date,
                        "mailto",
                        &destination,
                        &envelope.json,
                        now,
                    )
                    .await
                {
                    tracing::error!(domain = %domain, "TLSRPT mailto persist failed: {e}");
                    continue;
                }
                persisted += 1;
            }
            ReportTransport::Https { uri, body, .. } => {
                let status = match poster.post_tlsrpt(uri, body.clone()).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(domain = %domain, "TLSRPT https POST failed: {e}");
                        continue;
                    }
                };
                if !(200..300).contains(&status) {
                    tracing::warn!(
                        domain = %domain,
                        status,
                        "TLSRPT https non-2xx; skipping persist row"
                    );
                    continue;
                }
                if let Err(e) = db
                    .insert_tlsrpt_outbound_report(
                        report_id,
                        domain,
                        report_date,
                        "https",
                        uri,
                        &envelope.json,
                        now,
                    )
                    .await
                {
                    tracing::error!(domain = %domain, "TLSRPT https persist failed: {e}");
                    continue;
                }
                persisted += 1;
            }
        }
    }
    (attempted, persisted)
}

/// Run one full emit pass: for every recipient domain the aggregator has
/// accumulated outcomes for, fetch its TLSRPT policy, build + dispatch the
/// report, and clear the bucket so the next day starts fresh. Aggregator
/// access is held only briefly per domain (snapshot of domains, then
/// `emit_report`, then `clear_domain`) so DNS + HTTP I/O run outside the lock
/// and never block the delivery-path → `report_tls_attempt` recorder.
///
/// Separated from the timed loop so a test-hook can fire a pass at a scripted
/// `now` without waiting for midnight.
pub async fn run_one_emit_pass(
    state: &Arc<AppState>,
    fetcher: &dyn TlsrptPolicyFetcher,
    poster: &dyn TlsrptHttpPoster,
    now: i64,
) {
    // TODO(per-domain gate): smtp-server.md § TLSRPT outbound reporter makes
    // reporting on-by-default with a per-domain mail-policy-config opt-out
    // (`tlsrpt_send_reports`). Honouring the opt-out is wired with the rest of
    // the outbound mail-policy write-path (separate deferred track); until
    // then we emit for every recipient domain (the on-by-default behaviour).
    let Some(reporting_mta) = resolve_reporting_mta(state).await else {
        return; // No mail domain configured / claimed → nothing to report from.
    };
    let report_date = format_utc_date(now);
    let report_id_prefix = report_date.replace('-', "");
    let domains = {
        let agg = state.email.tlsrpt_aggregator.lock().unwrap();
        agg.recorded_domains()
    };
    for (idx, domain) in domains.into_iter().enumerate() {
        let report_id = format!("{report_id_prefix}.{now}.{idx}@{reporting_mta}");
        let envelope = {
            let agg = state.email.tlsrpt_aggregator.lock().unwrap();
            agg.emit_report(&domain, &reporting_mta, &report_id, &report_date)
        };
        match fetcher.lookup(&domain).await {
            Ok(Some(rua_uris)) => {
                let (attempted, persisted) = dispatch_tlsrpt_for_domain(
                    &state.db,
                    poster,
                    &reporting_mta,
                    envelope,
                    &rua_uris,
                    &domain,
                    &report_id,
                    &report_date,
                    now,
                )
                .await;
                tracing::info!(domain = %domain, attempted, persisted, "TLSRPT daily dispatch");
            }
            Ok(None) => {
                // Recipient publishes no TLSRPT policy → nothing to submit.
            }
            Err(e) => {
                tracing::error!(domain = %domain, "TLSRPT policy lookup failed: {e}");
            }
        }
        // Clear the bucket whether or not we shipped — the day's aggregation
        // window is closed; a fetch failure drops this day's report rather
        // than carrying stale counts into tomorrow's window.
        state
            .email
            .tlsrpt_aggregator
            .lock()
            .unwrap()
            .clear_domain(&domain);
    }
}

/// The nest's own mail domain, used as the TLSRPT report *submitter* identity
/// (the `tlsrpt@<domain>` From address + the `Report-ID` host + the report's
/// `contact-info`/`organization-name` anchor). Resolved at emit time so it
/// always reflects the currently-claimed domain. Resolved from the active
/// primary local domain (`lookup_primary_mail_domain`) — the nest claims local
/// domains via the admin API into the `mail_domains` table (product invariant:
/// nest config comes from clients, not boot args).
///
/// `None` only when no mail domain is claimed — a nest that can't send mail, so
/// there's nothing to report from.
async fn resolve_reporting_mta(state: &Arc<AppState>) -> Option<String> {
    match state.db.lookup_primary_mail_domain().await {
        Ok(Some(d)) => Some(d.domain_name),
        Ok(None) => None,
        Err(e) => {
            tracing::error!("TLSRPT emit: lookup_primary_mail_domain failed: {e}");
            None
        }
    }
}

/// Spawn the once-per-UTC-day TLSRPT emitter loop. Each iteration sleeps until
/// `00:00:00 UTC + jitter` (jitter `0..jitter_window_secs`, RFC 8460 §4.4
/// anti-stampede), then runs one emit pass. The clock for *scheduling* + the
/// report timestamps is `AppState::outbound_now` (test-hook-overridable);
/// the `sleep` itself is real wall-clock.
pub fn spawn_tlsrpt_daily_dispatch(
    state: Arc<AppState>,
    fetcher: Arc<dyn TlsrptPolicyFetcher>,
    poster: Arc<dyn TlsrptHttpPoster>,
    jitter_window_secs: u32,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        loop {
            let now = state.outbound_now();
            let until = seconds_until_next_utc_midnight(now) as i64
                + sample_jitter_offset_secs(jitter_window_secs.max(1));
            tokio::time::sleep(Duration::from_secs(until.max(1) as u64)).await;
            run_one_emit_pass(&state, &*fetcher, &*poster, state.outbound_now()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mail::outbound::tlsrpt::{
        AttemptOutcome, NullTlsrptHttpPoster, TlsrptAggregator, TlsrptPolicy,
    };

    const NOW: i64 = 1_779_840_000; // 2026-05-27 00:00:00 UTC

    fn envelope_for(domain: &str, report_id: &str) -> ReportEnvelope {
        let mut agg = TlsrptAggregator::default();
        agg.record(AttemptOutcome {
            recipient_domain: domain.to_string(),
            policy: TlsrptPolicy {
                policy_type: "no-policy-found".into(),
                policy_string: vec![],
                policy_domain: domain.into(),
            },
            failure_type: None,
        });
        agg.emit_report(domain, "mta.test", report_id, "2026-05-27")
    }

    #[tokio::test]
    async fn dispatch_ships_and_persists_both_transports() {
        let db = CacheDb::open_in_memory().unwrap();
        let poster = NullTlsrptHttpPoster;
        let rua = vec![
            "mailto:tlsrpt@dest.test".to_string(),
            "https://reports.dest.test/v1".to_string(),
        ];
        let (attempted, persisted) = dispatch_tlsrpt_for_domain(
            &db,
            &poster,
            "mta.test",
            envelope_for("dest.test", "rid-1"),
            &rua,
            "dest.test",
            "rid-1",
            "2026-05-27",
            NOW,
        )
        .await;
        assert_eq!((attempted, persisted), (2, 2));

        let rows = db.fetch_tlsrpt_reports_for_test().await.unwrap();
        assert_eq!(rows.len(), 2, "one persisted row per shipped transport");
        let transports: Vec<String> = rows
            .iter()
            .map(|r| r["transport"].as_str().unwrap().to_string())
            .collect();
        assert!(transports.contains(&"mailto".to_string()));
        assert!(transports.contains(&"https".to_string()));

        // The mailto report rode the outbound queue (a persist row only lands
        // after a successful enqueue): one pending outbound row, from
        // tlsrpt@mta.test, due at NOW.
        let due = db.fetch_due_outbound(NOW, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].original_sender, "tlsrpt@mta.test");
        assert_eq!(due[0].recipient, "tlsrpt@dest.test");
    }

    #[tokio::test]
    async fn dispatch_no_rua_is_a_noop() {
        let db = CacheDb::open_in_memory().unwrap();
        let poster = NullTlsrptHttpPoster;
        let (attempted, persisted) = dispatch_tlsrpt_for_domain(
            &db,
            &poster,
            "mta.test",
            envelope_for("dest.test", "rid-2"),
            &[],
            "dest.test",
            "rid-2",
            "2026-05-27",
            NOW,
        )
        .await;
        assert_eq!((attempted, persisted), (0, 0));
        assert!(db.fetch_tlsrpt_reports_for_test().await.unwrap().is_empty());
    }
}
