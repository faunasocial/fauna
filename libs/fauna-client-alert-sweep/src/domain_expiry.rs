//! Feeder #4 — the **domain-expiry watch** (`critical-alerts.md` § Feeders;
//! mechanism owner `domains-and-tls-bootstrap.md` § Domain loss → *Detection*).
//!
//! # Why this feeder's body lives in the sweep crate
//!
//! Every other feeder's check lives in its own `fauna-client-*` crate because it
//! has a page too — feeder #1 and #3 also run from the ATProto settings machine,
//! feeder #2's projection is Settings' own. This one has no page anywhere: the
//! sweep is its entire client-side existence, so a dedicated crate would have
//! exactly one caller and no independent surface. What *is* shared sits where it
//! belongs — the record and the two-arm decision are
//! `fauna_protocol::domain_expiry`, so a future admin page rendering "your
//! domain expires in N days" cannot drift from what the banner alarms on. If
//! that page arrives, the read lifts out then.
//!
//! # Two things that make this feeder unlike the other three
//!
//! **1. It is the first DEPLOYMENT-scoped feeder, so its alert key is bare.**
//! The others key on the identity they accuse (`atproto-custody:<did>`,
//! `recovery-replacement-pending:<actor>`) because two identities can be in
//! different states at once. A domain lapse is one fact about the box every
//! resident shares, so `critical-alerts.md` § Feeders gives it the unqualified
//! key `domain-expiry`. It still clears at identity teardown like every other
//! alert — `clear_all` takes it — which is correct rather than merely tolerable:
//! the next session's first sweep re-posts it within one round trip.
//!
//! **2. It reads a value the NEST computed, which the directory feeders may
//! not.** The audit-floor rule (`critical-alerts.md` § Feeders) exists because a
//! box must not decide whether it is audited. It does not bind here, and the
//! reason is in § Detection: the audited party is the **registry**, not the
//! nest. A nest lying about its own domain's expiry defeats only its own users'
//! warning — the same trust class as it serving their mail at all — so there is
//! no adversarial gap for a floor to close.

use fauna_core::localized::LocalizedText;
use fauna_protocol::domain_expiry::{
    AlarmReason, DomainExpiryRecord, DomainExpiryReply, DomainExpiryRequest, Verdict, evaluate,
    outcomes, skip_reasons as wire_skip_reasons,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::{FeederFailure, SweepReport, feeders, skip_reasons};

/// The kind this feeder reads.
const KIND: &str = "fauna.domain.expiry.get";

/// The alert's registry key — deliberately **not** actor-scoped (see the module
/// note).
pub const DOMAIN_EXPIRY_ALERT_KEY: &str = "domain-expiry";

// i18n keys, one per (finding × role). Six rather than three because the whole
// audience ruling is that a resident gets a *different sentence*, not the
// admin's with a caveat — and an English fallback composed in Rust would not
// translate.
const EXPIRING_ADMIN: &str = "critical_alerts.domain_expiring_admin";
const EXPIRING_RESIDENT: &str = "critical_alerts.domain_expiring_resident";
const EXPIRED_ADMIN: &str = "critical_alerts.domain_expired_admin";
const EXPIRED_RESIDENT: &str = "critical_alerts.domain_expired_resident";
const LAPSING_ADMIN: &str = "critical_alerts.domain_lapsing_admin";
const LAPSING_RESIDENT: &str = "critical_alerts.domain_lapsing_resident";

/// The alert's lines for an alarming record, as of `now` (unix seconds).
///
/// Pure — no clock read, no I/O — so the copy is testable on every platform and
/// the caller owns the time source, the same discipline
/// `fauna_client_recovery::alerts` keeps. `None` means "not alarming"; the
/// caller clears.
pub fn domain_expiry_alert_lines(
    record: &DomainExpiryRecord,
    admin: bool,
    now: i64,
) -> Option<Vec<LocalizedText>> {
    let Verdict::Alarm(reason) = evaluate(record, now) else {
        return None;
    };
    let key = match (&reason, admin) {
        (AlarmReason::ExpiringSoon { .. }, true) => EXPIRING_ADMIN,
        (AlarmReason::ExpiringSoon { .. }, false) => EXPIRING_RESIDENT,
        (AlarmReason::Expired, true) => EXPIRED_ADMIN,
        (AlarmReason::Expired, false) => EXPIRED_RESIDENT,
        (AlarmReason::LapseStatus(_), true) => LAPSING_ADMIN,
        (AlarmReason::LapseStatus(_), false) => LAPSING_RESIDENT,
    };
    let mut line = LocalizedText::key(key);
    line.args.insert("domain".into(), record.domain.clone());
    match &reason {
        AlarmReason::ExpiringSoon { days } => {
            line.args.insert("days".into(), days.to_string());
        }
        AlarmReason::LapseStatus(status) => {
            line.args.insert("status".into(), status.clone());
        }
        AlarmReason::Expired => {}
    }
    Some(vec![line])
}

/// Post or clear the deployment's domain-expiry alert from an already-read
/// reply, and record the outcome.
///
/// Routing both outcomes through one call is the point (the same reason
/// `sync_pending_replacement_alert` exists): a renewed domain must take its
/// banner with it, and a caller that only knew how to post would leave a
/// non-dismissable alarm about a lapse that no longer exists.
pub fn sync_domain_expiry_alert(
    alerts: &fauna_client_alerts::CriticalAlerts,
    reply: &DomainExpiryReply,
    report: &mut SweepReport,
    now: i64,
) {
    // No record at all: the watch has not completed a single attempt yet (a box
    // in its first minutes). Nothing is known, so nothing is posted — and
    // nothing is CLEARED either, for the same reason a failure does not clear.
    let Some(record) = &reply.record else {
        report.skip(feeders::DOMAIN_EXPIRY, skip_reasons::WATCH_NOT_YET_RUN);
        return;
    };

    match evaluate(record, now) {
        // The nest reached RDAP and it errored. The client cannot retry the
        // registry itself, so this is a *failure*, not a skip: unreachable is not
        // resolved, the next sweep re-reads, and any standing alert stands.
        Verdict::Unreachable { detail } => {
            tracing::warn!(
                domain = %record.domain,
                detail = ?detail,
                "session-start sweep: the nest could not reach RDAP; \
                 any standing domain-expiry alert is left as-is"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::DOMAIN_EXPIRY,
                error: detail.unwrap_or_else(|| "RDAP unreachable".into()),
            });
        }
        // A skip is silent on the banner but never silent in the report.
        // Deliberately does NOT clear: a deployment that just went domainless is
        // a different story from one whose domain is healthy, and neither is a
        // reason to tear down an alarm the last real check raised.
        _ if record.outcome == outcomes::SKIPPED => {
            report.skip(feeders::DOMAIN_EXPIRY, skip_reason_token(record));
        }
        Verdict::Quiet => {
            alerts.clear(DOMAIN_EXPIRY_ALERT_KEY);
            report.check(feeders::DOMAIN_EXPIRY);
        }
        Verdict::Alarm(_) => {
            if let Some(lines) = domain_expiry_alert_lines(record, reply.admin, now) {
                tracing::warn!(
                    domain = %record.domain,
                    expires_at = ?record.expires_at,
                    statuses = ?record.statuses,
                    "the deployment's primary domain registration is lapsing — \
                     raising the critical alert"
                );
                alerts.post(DOMAIN_EXPIRY_ALERT_KEY.to_string(), lines);
            }
            report.check(feeders::DOMAIN_EXPIRY);
        }
    }
}

/// Map the record's skip `detail` back to one of this crate's stable tokens.
///
/// The wire carries a `String` and [`crate::FeederSkip`] wants a `&'static str`,
/// so an unrecognized token folds to [`skip_reasons::UNKNOWN_NEST_SKIP`] rather
/// than being dropped — a *newer* nest naming a skip reason this build has never
/// heard of must still show up in the report as a skip, not vanish into a
/// silence indistinguishable from health.
fn skip_reason_token(record: &DomainExpiryRecord) -> &'static str {
    match record.detail.as_deref() {
        Some(wire_skip_reasons::UNSERVED_TLD) => skip_reasons::RDAP_UNSERVED_TLD,
        Some(wire_skip_reasons::NO_PRIMARY_DOMAIN) => skip_reasons::NO_PRIMARY_DOMAIN,
        _ => skip_reasons::UNKNOWN_NEST_SKIP,
    }
}

/// Read the watch's record and sync the alert from it.
pub(crate) async fn run_domain_expiry_feeder<R>(
    rpc: R,
    alerts: &fauna_client_alerts::CriticalAlerts,
    report: &mut SweepReport,
    now: i64,
) where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    match rpc
        .request::<_, DomainExpiryReply>(KIND, DomainExpiryRequest::default())
        .await
    {
        Ok(reply) => sync_domain_expiry_alert(alerts, &reply, report, now),
        Err(e) => {
            // The nest is unreachable. Same fail-safe direction as every other
            // feeder: retry next sweep, leave any standing alert alone.
            tracing::warn!(
                error = %e,
                "session-start sweep: domain-expiry unreachable; \
                 any standing alert is left as-is and the next sweep retries"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::DOMAIN_EXPIRY,
                error: e.to_string(),
            });
        }
    }
}
