//! Production-path outbound retry scheduling + the 4 h delay-warning DSN.
//!
//! When the Go MTA bridge reports a **temporary** delivery failure
//! (`fauna.bridges.mark_outbound_failed` — a 4xx / connection / TLS
//! error), nest — not the bridge — owns the decision of when, or whether,
//! to retry: `docs/goal/behavior/smtp-server.md` § Outbound delivery
//! ("nest reschedules per the retry schedule below"). This module turns
//! one reported failure into a [`FailedDecision`]:
//!
//! * [`FailedDecision::Retry`] — reschedule at `next_attempt_at`; `warn`
//!   is true exactly the first time wall-clock-elapsed crosses the 4 h
//!   delay-warning mark (`smtp-server.md` § Retry schedule, :374).
//! * [`FailedDecision::GiveUp`] — retry budget / 5 d ceiling exhausted;
//!   the caller runs the permanent-failure bounce path
//!   ([`crate::outbound_bounce::generate_permfail_bounce`]).
//!
//! It consumes the **shared** curve in `fauna_mail::outbound::retry`
//! (jitter + give-up) and the shared `dsn` / `backscatter` builders; the
//! only nest-specific glue is mapping the wire `OutboundPolicy` catalog
//! into the shared `RetryPolicy` (kept here, not in `fauna-mail`, so the
//! shared crate stays free of the `fauna-protocol` wire types) and
//! enqueueing the warning DSN through the nest outbound DB. The DSN
//! formatting helpers are shared with the bounce path (`outbound_bounce`).

use std::time::Duration;

use anyhow::Result;
use fauna_mail::outbound::backscatter::{
    self, InboundVerdictsSnapshot as MailVerdicts, SuppressorToggles,
};
use fauna_mail::outbound::dsn::{DsnAction, DsnReport, build_dsn};
use fauna_mail::outbound::retry::{NextAction, RetryPolicy, RetrySchedule};
use fauna_protocol::bridge_routing::OutboundPolicy;

use crate::db::CacheDb;
use crate::db::outbound::{InboundVerdictsSnapshot as DbVerdicts, NewOutbound, OutboundRow};
use crate::outbound_bounce::{extract_headers, format_rfc2822, reporting_domain};

/// RFC 3463 enhanced status for a delay warning ("delivery time
/// expired / message still in queue") — `smtp-server.md` :374.
const DELAY_WARNING_STATUS: &str = "4.4.7";

/// Map the wire `OutboundPolicy` catalog onto the shared `RetryPolicy`.
///
/// `OutboundPolicy::retry_schedule_seconds[0]` is the (always-zero) delay
/// *before attempt 1*, whereas `RetryPolicy::schedule[i]` is the delay
/// before attempt `i + 2` — so the leading entry is dropped. The two
/// `*_hours` fields become `Duration`s; jitter has no wire knob and keeps
/// the shared default. The caller passes the **effective** (override-or-
/// default) policy — `OutboundPolicyOverrides::effective()`, which also
/// resolves a stored `permanent_failure_timeout_hours = 0` to the catalog
/// default (`mail-policy-config.md` § Implementation status today).
pub fn retry_policy_from_outbound(policy: &OutboundPolicy) -> RetryPolicy {
    let schedule = policy
        .retry_schedule_seconds
        .iter()
        .skip(1)
        .map(|s| Duration::from_secs(*s))
        .collect();
    RetryPolicy {
        schedule,
        jitter_pct: RetryPolicy::default().jitter_pct,
        permanent_failure_after: Duration::from_secs(
            policy.permanent_failure_timeout_hours as u64 * 3_600,
        ),
        delay_warning_at: Duration::from_secs(policy.delay_warning_at_hours as u64 * 3_600),
    }
}

/// What to do with a row whose latest delivery attempt just failed
/// temporarily. Returned for the handler to apply; carries no side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedDecision {
    /// Reschedule the next attempt for `next_attempt_at`. `warn` is true
    /// the one time the row first crosses the delay-warning boundary.
    Retry { next_attempt_at: i64, warn: bool },
    /// Retry budget / permanent-failure timeout exhausted — promote to a
    /// permanent-failure bounce.
    GiveUp,
}

/// Decide the next move for a row whose latest attempt failed temporarily.
///
/// `now` and `row.created_at` are on the same clock
/// ([`crate::routes::AppState::outbound_now`]). `retry_after_hint` is the
/// bridge-reported server Retry-After in seconds (0 = none); it is a
/// **floor** under the curve delay so an explicit "try again in N" from the
/// remote MX is honoured without ever shortening the spec backoff.
pub fn schedule_after_failure(
    row: &OutboundRow,
    retry_after_hint: i64,
    now: i64,
    policy: &RetryPolicy,
) -> FailedDecision {
    // The row's `attempt_count` is the count *before* the just-finished
    // attempt is recorded; include it so `RetryPolicy` indexes the curve
    // for the next attempt.
    let completed_attempts = row.attempt_count + 1;
    let elapsed = Duration::from_secs((now - row.created_at).max(0) as u64);
    let already_warned = row.delay_warned_at.is_some();
    match policy.next(completed_attempts, elapsed, already_warned) {
        NextAction::GiveUp => FailedDecision::GiveUp,
        NextAction::Retry {
            delay,
            should_warn_delay,
        } => {
            let curve = delay.as_secs() as i64;
            FailedDecision::Retry {
                next_attempt_at: now + curve.max(retry_after_hint.max(0)),
                warn: should_warn_delay,
            }
        }
    }
}

/// Build + enqueue the once-per-message 4 h delay-warning DSN
/// (`Action: delayed`, `Status: 4.4.7`) back to the original sender,
/// unless backscatter suppression applies. Returns whether a DSN was
/// enqueued (`false` = suppressed). The caller marks the row
/// `delay_warned_at` so subsequent delays in the same message don't
/// re-warn (`smtp-server.md` :374, "once-per-message").
///
/// Suppression reuses the shared bounce rule with `final_5xx = false`: a
/// delay warning to a null / forged sender is backscatter just like a
/// bounce, so the null-sender + SPF-hardfail + DMARC-reject suppressors
/// fire here too. The forwarded-and-externally-bounced suppressor needs a
/// 5xx and so correctly never fires for a transient delay.
pub async fn enqueue_delay_warning(
    db: &CacheDb,
    row: &OutboundRow,
    reason: &str,
    now: i64,
) -> Result<bool> {
    let verdicts = MailVerdicts {
        spf: row.inbound_verdicts.spf.clone(),
        dmarc: row.inbound_verdicts.dmarc.clone(),
        dmarc_policy: row.inbound_verdicts.dmarc_policy.clone(),
    };
    if let Some(reason_kind) = backscatter::should_suppress(
        &verdicts,
        &row.original_sender,
        row.is_forwarded,
        false,
        &SuppressorToggles::default(),
    ) {
        tracing::info!(
            id = row.id,
            sender = %row.original_sender,
            reason = ?reason_kind,
            "outbound delay-warning suppressed (backscatter)"
        );
        return Ok(false);
    }

    let reporting_mta = reporting_domain(db, &row.original_sender).await;
    let headers = extract_headers(&row.raw_message);
    let arrival = format_rfc2822(row.created_at);
    let last_attempt = format_rfc2822(now);
    let summary = format!(
        "Message to <{}> has not yet been delivered. Delivery is delayed and the \
         server will keep trying. The remote server said: {}",
        row.recipient, reason
    );
    let report = DsnReport {
        reporting_mta: &reporting_mta,
        arrival_date: &arrival,
        last_attempt_date: &last_attempt,
        recipient: &row.recipient,
        original_sender: &row.original_sender,
        status: DELAY_WARNING_STATUS,
        action: DsnAction::Delayed,
        diagnostic_code: reason,
        original_headers: &headers,
        failure_summary_text: &summary,
    };
    let dsn_bytes = build_dsn(&report);

    // Null envelope sender (RFC 5321 §4.5.5) addressed to the original
    // sender, riding the same outbound queue + retry curve as any other
    // message; its own null-sender backscatter rule short-circuits a
    // warning-of-a-warning on first failure.
    let warn_msgid = format!(
        "<delay-{}-{}@{}>",
        now,
        row.id,
        reporting_mta.replace('@', "")
    );
    db.enqueue_outbound_at(
        NewOutbound {
            original_msgid: &warn_msgid,
            original_sender: "",
            recipients: &[row.original_sender.as_str()],
            raw_message: &dsn_bytes,
            inbound_verdicts: DbVerdicts {
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
    .await?;
    tracing::info!(
        id = row.id,
        recipient = %row.recipient,
        sender = %row.original_sender,
        "outbound 4h delay-warning DSN enqueued"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::outbound::{InboundVerdictsSnapshot, OutboundStatus};

    const HOUR: i64 = 3_600;
    const DAY: i64 = 86_400;

    fn verdicts(spf: &str, dmarc: &str, pol: &str) -> InboundVerdictsSnapshot {
        InboundVerdictsSnapshot {
            spf: spf.into(),
            dmarc: dmarc.into(),
            dmarc_policy: pol.into(),
        }
    }

    /// Construct an in-memory `OutboundRow` for the pure-decision tests
    /// without touching the DB (the decision fn reads only these fields).
    fn row(attempt_count: u32, created_at: i64, delay_warned_at: Option<i64>) -> OutboundRow {
        OutboundRow {
            id: 1,
            original_msgid: "m@x".into(),
            original_sender: "alice@local.test".into(),
            recipient: "bob@dest.test".into(),
            raw_message: b"From: a@x\r\nTo: b@y\r\n\r\nbody".to_vec(),
            attempt_count,
            next_attempt_at: 0,
            delay_warned_at,
            status: OutboundStatus::Pending,
            last_error: None,
            last_enhanced: None,
            inbound_verdicts: verdicts("pass", "pass", "none"),
            is_forwarded: false,
            forward_actor_id: None,
            forward_rule_id: None,
            created_at,
        }
    }

    #[test]
    fn policy_conversion_drops_attempt1_delay_and_scales_hours() {
        let p = retry_policy_from_outbound(&OutboundPolicy::default());
        // Leading 0 (delay before attempt 1) dropped → 9 entries, first 5 min.
        assert_eq!(p.schedule.len(), 9);
        assert_eq!(p.schedule[0], Duration::from_secs(300));
        assert_eq!(p.schedule[8], Duration::from_secs(86_400));
        assert_eq!(
            p.permanent_failure_after,
            Duration::from_secs(5 * DAY as u64)
        );
        assert_eq!(p.delay_warning_at, Duration::from_secs(4 * HOUR as u64));
        assert_eq!(p.jitter_pct, 10);
    }

    #[test]
    fn first_failure_schedules_about_five_minutes_no_warn() {
        let p = retry_policy_from_outbound(&OutboundPolicy::default());
        let now = 1_000_000;
        let decision = schedule_after_failure(&row(0, now, None), 0, now, &p);
        match decision {
            FailedDecision::Retry {
                next_attempt_at,
                warn,
            } => {
                let delta = next_attempt_at - now;
                // 5 min ±10% jitter.
                assert!((270..=330).contains(&delta), "delta={delta}");
                assert!(!warn, "no warning before the 4h mark");
            }
            FailedDecision::GiveUp => panic!("unexpected GiveUp on first failure"),
        }
    }

    #[test]
    fn crossing_four_hours_warns_once() {
        let p = retry_policy_from_outbound(&OutboundPolicy::default());
        let now = 2_000_000;
        // attempt 2 just failed, message is 4h+1s old, never warned.
        let created = now - (4 * HOUR + 1);
        let d = schedule_after_failure(&row(1, created, None), 0, now, &p);
        assert!(
            matches!(d, FailedDecision::Retry { warn: true, .. }),
            "{d:?}"
        );

        // Same row but already warned → no re-warn.
        let d2 = schedule_after_failure(&row(1, created, Some(now - 60)), 0, now, &p);
        assert!(
            matches!(d2, FailedDecision::Retry { warn: false, .. }),
            "{d2:?}"
        );
    }

    #[test]
    fn under_four_hours_does_not_warn() {
        let p = retry_policy_from_outbound(&OutboundPolicy::default());
        let now = 2_000_000;
        let created = now - (4 * HOUR - 60); // 3h59m
        let d = schedule_after_failure(&row(1, created, None), 0, now, &p);
        assert!(
            matches!(d, FailedDecision::Retry { warn: false, .. }),
            "{d:?}"
        );
    }

    #[test]
    fn budget_or_timeout_exhausted_gives_up() {
        let p = retry_policy_from_outbound(&OutboundPolicy::default());
        let now = 3_000_000;
        // 10 attempts completed (idx 9 ≥ schedule len 9) → GiveUp.
        assert_eq!(
            schedule_after_failure(&row(9, now, None), 0, now, &p),
            FailedDecision::GiveUp
        );
        // Past the 5-day ceiling even with attempts remaining → GiveUp.
        let created = now - (5 * DAY + 1);
        assert_eq!(
            schedule_after_failure(&row(1, created, None), 0, now, &p),
            FailedDecision::GiveUp
        );
    }

    #[test]
    fn retry_after_hint_is_a_floor_not_a_ceiling() {
        let p = retry_policy_from_outbound(&OutboundPolicy::default());
        let now = 1_000_000;
        // Hint 10000s > the ~300s curve delay → hint wins.
        let d = schedule_after_failure(&row(0, now, None), 10_000, now, &p);
        match d {
            FailedDecision::Retry {
                next_attempt_at, ..
            } => {
                assert_eq!(next_attempt_at - now, 10_000);
            }
            FailedDecision::GiveUp => panic!("unexpected GiveUp"),
        }
    }

    #[tokio::test]
    async fn delay_warning_enqueues_4_4_7_dsn_to_sender() {
        let db = CacheDb::open_in_memory().unwrap();
        let now = 1_500_000;
        let ids = db
            .enqueue_outbound_at(
                NewOutbound {
                    original_msgid: "orig@local.test",
                    original_sender: "alice@local.test",
                    recipients: &["bob@dest.test"],
                    raw_message:
                        b"From: alice@local.test\r\nTo: bob@dest.test\r\nSubject: hi\r\n\r\nbody",
                    inbound_verdicts: verdicts("pass", "pass", "none"),
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                now - 4 * HOUR,
            )
            .await
            .unwrap();
        let r = db.fetch_outbound_by_id(ids[0]).await.unwrap().unwrap();

        let emitted = enqueue_delay_warning(&db, &r, "451 4.7.1 greylisted, try later", now)
            .await
            .unwrap();
        assert!(emitted);

        let rows = db.fetch_all_outbound_for_test().await.unwrap();
        let dsn = rows
            .iter()
            .find(|x| x.original_sender.is_empty() && x.recipient == "alice@local.test")
            .expect("null-sender delay-warning DSN enqueued");
        let body = String::from_utf8_lossy(&dsn.raw_message);
        assert!(body.contains("multipart/report"), "{body}");
        assert!(body.contains("Action: delayed"), "{body}");
        assert!(body.contains("Status: 4.4.7"), "{body}");
        // A warning is not a bounce — nothing recorded in bounce_history.
        assert!(db.fetch_bounce_history_for_test().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delay_warning_suppressed_for_null_sender() {
        let db = CacheDb::open_in_memory().unwrap();
        let now = 1_500_000;
        let ids = db
            .enqueue_outbound_at(
                NewOutbound {
                    original_msgid: "orig@local.test",
                    original_sender: "",
                    recipients: &["bob@dest.test"],
                    raw_message: b"From: <>\r\nTo: bob@dest.test\r\n\r\nbody",
                    inbound_verdicts: verdicts("none", "none", "none"),
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                now - 4 * HOUR,
            )
            .await
            .unwrap();
        let r = db.fetch_outbound_by_id(ids[0]).await.unwrap().unwrap();

        let emitted = enqueue_delay_warning(&db, &r, "451 4.7.1 try later", now)
            .await
            .unwrap();
        assert!(!emitted, "must not warn a null sender (backscatter)");
        // Only the original row exists; no warning DSN row.
        assert_eq!(db.fetch_all_outbound_for_test().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn delay_warning_suppressed_on_spf_hardfail() {
        let db = CacheDb::open_in_memory().unwrap();
        let now = 1_500_000;
        let ids = db
            .enqueue_outbound_at(
                NewOutbound {
                    original_msgid: "orig@local.test",
                    original_sender: "spoofed@victim.test",
                    recipients: &["bob@dest.test"],
                    raw_message: b"From: spoofed@victim.test\r\nTo: bob@dest.test\r\n\r\nbody",
                    inbound_verdicts: verdicts("fail", "none", "none"),
                    is_forwarded: false,
                    forward_actor_id: None,
                    forward_rule_id: None,
                    forward_copy_mode: None,
                    submit_actor_id: None,
                },
                now - 4 * HOUR,
            )
            .await
            .unwrap();
        let r = db.fetch_outbound_by_id(ids[0]).await.unwrap().unwrap();

        let emitted = enqueue_delay_warning(&db, &r, "451 4.7.1 try later", now)
            .await
            .unwrap();
        assert!(
            !emitted,
            "must not warn a sender that SPF-hardfailed inbound"
        );
        assert_eq!(db.fetch_all_outbound_for_test().await.unwrap().len(), 1);
    }
}
