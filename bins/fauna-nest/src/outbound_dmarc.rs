//! Production-path DMARC aggregate-report sender (RFC 7489 §7.2).
//!
//! The Go MTA reports each inbound message it verified via
//! `fauna.bridges.report_dmarc_observation`; [`record_observation`] counts it
//! into `AppState.email.dmarc_aggregator`. Once per UTC day (00:00 + jitter)
//! [`run_one_emit_pass`] resolves the DMARC policy governing each counted From
//! domain, builds one RFC 7489 §A.1 report per policy domain, and queues it to
//! each `rua=mailto:` destination the peer published and may receive it, on
//! the standard outbound queue from `dmarc-report@<primary>` — the MTA drains
//! it with every other outbound mail and retries it on the standard curve.
//! Each queued report part persists one `dmarc_outbound_aggregate_reports` row,
//! kept 7 days.
//!
//! The pure parts (the aggregator, the record and `rua=` readers, the policy
//! walk, the external-destination check, the XML, the size-cap split, the MIME)
//! live in `fauna_mail::outbound::dmarc`; this module is the nest's DB + queue
//! wiring and the schedule — the [`crate::outbound_tlsrpt`] shape.
//!
//! docs/goal/behavior/dmarc-reporting.md § Outbound aggregate-report sender.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use fauna_core::mail_auth::{DmarcObservation, DmarcVerdict};
use fauna_mail::outbound::dmarc::{
    DmarcRecordFetcher, PeerDmarcPolicy, ReportMeta, ReportRow, applied_disposition,
    build_report_mime, build_reports, discover_policy, external_destination_authorized,
    plan_destinations,
};
use fauna_mail::outbound::tlsrpt::{
    format_utc_date, rfc2822_utc, sample_jitter_offset_secs, seconds_until_next_utc_midnight,
};

use crate::db::dmarc_reports::DmarcOutboundReportRow;
use crate::db::outbound::{InboundVerdictsSnapshot, NewOutbound};
use crate::routes::AppState;

/// Per-pass jitter window: the daily pass fires at `00:00 UTC + rand(0..3600)s`
/// so deployments do not all report at the same instant (§ step 1; the TLSRPT
/// reporter's window).
pub const DMARC_JITTER_WINDOW_SECS: u32 = 3600;

/// How long a sent report is kept for the admin's inspection (§ step 5).
pub const DMARC_OUTBOUND_RETENTION_DAYS: i64 = 7;

/// The window a pass reports on: the day that ends at the pass.
const REPORT_WINDOW_SECS: i64 = 86_400;

fn dmarc_outbound_metric(metric: &'static str, domain: &str, n: usize) {
    tracing::info!(
        target: "nest_metrics",
        metric,
        domain = %domain,
        n,
        "dmarc outbound report"
    );
}

/// Count one inbound message into the day's buckets. `false` when it was not
/// counted: it names no From domain, it claims one of our own domains (or a
/// name beneath one — we report to peers about theirs), or a bucket cap was
/// reached. The disposition is what the admin's inbound enforcement switches
/// made of `dmarc` (`fauna_mail::outbound::dmarc::applied_disposition`).
pub async fn record_observation(
    state: &Arc<AppState>,
    source_ip: IpAddr,
    dmarc: &DmarcVerdict,
    observation: &DmarcObservation,
) -> anyhow::Result<bool> {
    let from = observation
        .header_from_domain
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if from.is_empty() {
        return Ok(false);
    }
    let ours = state.db.list_active_mail_domains().await?.iter().any(|d| {
        let local = d.domain_name.to_ascii_lowercase();
        from == local || from.ends_with(&format!(".{local}"))
    });
    if ours {
        return Ok(false);
    }
    let auth = state.db.get_auth_policy().await?.effective();
    let disposition = applied_disposition(
        dmarc,
        auth.enforce_dmarc,
        auth.enforce_dmarc_quarantine,
        auth.log_only,
    );
    let counted =
        state
            .email
            .dmarc_aggregator
            .lock()
            .unwrap()
            .record(source_ip, observation, disposition);
    if !counted {
        dmarc_outbound_metric("dmarc_outbound_observation_dropped_total", &from, 1);
    }
    Ok(counted)
}

/// Queue one policy domain's report(s) to each destination it may go to.
/// Returns the number of report parts queued.
async fn dispatch_for_policy(
    state: &Arc<AppState>,
    fetcher: &dyn DmarcRecordFetcher,
    meta: &ReportMeta,
    policy: &PeerDmarcPolicy,
    rows: &[ReportRow],
    now: i64,
) -> usize {
    let (destinations, skipped) = plan_destinations(&policy.record.rua);
    if skipped > 0 {
        dmarc_outbound_metric(
            "dmarc_outbound_https_uri_skipped_total",
            &policy.domain,
            skipped,
        );
    }
    let sender = meta.email.clone();
    let rfc2822_date = rfc2822_utc(now);
    let mut queued = 0;
    for (address, cap) in destinations {
        let rua_domain = address.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
        if !external_destination_authorized(fetcher, &policy.domain, rua_domain).await {
            dmarc_outbound_metric(
                "dmarc_outbound_destination_unauthorized_total",
                &policy.domain,
                1,
            );
            continue;
        }
        for part in build_reports(meta, policy, rows, cap) {
            let part_meta = ReportMeta {
                report_id: part.report_id.clone(),
                ..meta.clone()
            };
            let mime = build_report_mime(
                &address,
                &part_meta,
                &policy.domain,
                &rfc2822_date,
                &part.xml_gz,
            );
            let recipients = [address.as_str()];
            if let Err(e) = state
                .db
                .enqueue_outbound_at(
                    NewOutbound {
                        original_msgid: &part.report_id,
                        original_sender: &sender,
                        recipients: &recipients,
                        raw_message: &mime,
                        // A machine-generated report: no inbound auth context
                        // applies (it originates here).
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
                tracing::error!(domain = %policy.domain, "DMARC report enqueue failed: {e}");
                continue;
            }
            if let Err(e) = state
                .db
                .insert_dmarc_outbound_report(&DmarcOutboundReportRow {
                    report_id: part.report_id.clone(),
                    peer_domain: policy.domain.clone(),
                    destination: address.clone(),
                    begin_at: meta.begin,
                    end_at: meta.end,
                    sent_at: now,
                    payload_xml_gz: part.xml_gz.clone(),
                })
                .await
            {
                tracing::error!(domain = %policy.domain, "DMARC report persist failed: {e}");
            }
            queued += 1;
        }
    }
    queued
}

/// Run one full daily pass at `now`: for every From domain counted, find the
/// governing policy; for every policy domain that publishes `rua=`, queue the
/// report; close every bucket whatever happened, so the next window starts
/// fresh and a failed lookup drops a day rather than carrying stale counts.
/// Returns the number of report parts queued.
///
/// Separated from the timed loop so a test hook can fire a pass at a scripted
/// `now` without waiting for midnight.
pub async fn run_one_emit_pass(
    state: &Arc<AppState>,
    fetcher: &dyn DmarcRecordFetcher,
    now: i64,
) -> usize {
    let primary = match state.db.lookup_primary_mail_domain().await {
        Ok(Some(d)) => d.domain_name,
        Ok(None) => {
            // No mail domain claimed: nothing to report from.
            state.email.dmarc_aggregator.lock().unwrap().clear();
            return 0;
        }
        Err(e) => {
            tracing::error!("DMARC emit: lookup_primary_mail_domain failed: {e}");
            return 0;
        }
    };
    let from_domains = state
        .email
        .dmarc_aggregator
        .lock()
        .unwrap()
        .recorded_domains();
    let mut by_policy: BTreeMap<String, (PeerDmarcPolicy, Vec<ReportRow>)> = BTreeMap::new();
    for from in from_domains {
        let rows = {
            let mut agg = state.email.dmarc_aggregator.lock().unwrap();
            let rows = agg.rows(&from);
            agg.clear_domain(&from);
            rows
        };
        match discover_policy(fetcher, &from).await {
            Ok(Some(policy)) if !policy.record.rua.is_empty() => {
                by_policy
                    .entry(policy.domain.clone())
                    .or_insert_with(|| (policy, Vec::new()))
                    .1
                    .extend(rows);
            }
            // No policy, or one that asks for no aggregate reports.
            Ok(_) => {}
            Err(e) => tracing::warn!(domain = %from, "DMARC policy lookup failed: {e}"),
        }
    }
    let id_prefix = format_utc_date(now).replace('-', "");
    let mut queued = 0;
    for (idx, (policy_domain, (policy, rows))) in by_policy.into_iter().enumerate() {
        let meta = ReportMeta {
            org_name: primary.clone(),
            email: format!("dmarc-report@{primary}"),
            report_id: format!("{id_prefix}.{now}.{idx}@{primary}"),
            begin: now - REPORT_WINDOW_SECS,
            end: now - 1,
        };
        let n = dispatch_for_policy(state, fetcher, &meta, &policy, &rows, now).await;
        tracing::info!(domain = %policy_domain, queued = n, "DMARC daily dispatch");
        queued += n;
    }
    queued
}

/// Drop sent reports past [`DMARC_OUTBOUND_RETENTION_DAYS`]. Rides the daily
/// mail sweep beside the inbound prune.
pub async fn prune_sent_reports(state: &Arc<AppState>) {
    prune_sent_reports_at(state, crate::db::now_epoch_secs()).await;
}

/// [`prune_sent_reports`] at a supplied `now` (Unix seconds).
pub async fn prune_sent_reports_at(state: &Arc<AppState>, now: i64) {
    let cutoff = now - DMARC_OUTBOUND_RETENTION_DAYS * 86_400;
    match state.db.prune_dmarc_outbound_reports(cutoff).await {
        Ok(0) => {}
        Ok(pruned) => tracing::info!(
            target: "nest_metrics",
            metric = "dmarc_outbound_aggregate_pruned_total",
            pruned,
            "sent dmarc aggregate reports past retention pruned"
        ),
        Err(e) => tracing::error!("dmarc outbound report prune error: {e}"),
    }
}

/// Spawn the once-per-UTC-day sender loop: sleep until `00:00:00 UTC +
/// jitter`, run one pass. Scheduling reads `AppState::outbound_now`
/// (test-hook-overridable); the sleep is wall-clock.
pub fn spawn_dmarc_daily_dispatch(
    state: Arc<AppState>,
    fetcher: Arc<dyn DmarcRecordFetcher>,
    jitter_window_secs: u32,
) -> tokio::task::JoinHandle<()> {
    // spawn-ok(returns-handle-for-scope): caller adopts via `AppState::scope_handle`
    tokio::spawn(async move {
        loop {
            let now = state.outbound_now();
            let until = seconds_until_next_utc_midnight(now) as i64
                + sample_jitter_offset_secs(jitter_window_secs.max(1));
            tokio::time::sleep(Duration::from_secs(until.max(1) as u64)).await;
            run_one_emit_pass(&state, &*fetcher, state.outbound_now()).await;
        }
    })
}

#[cfg(test)]
mod tests;
