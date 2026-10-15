//! Feeder #5 — the **eviction warning** (`critical-alerts.md` § Feeders; what
//! the alert says, when it is raised and cleared and why it clears the
//! severity bar are owned by `admin.md` § Cutting a user off — eviction and
//! suspension → *The member is told*).
//!
//! An admin's eviction starts a hard-coded two-week `warning` phase in which
//! the member keeps full access — the window in which they can export
//! everything — and then the account is suspended and later deleted. The nest
//! serves the record on the member's own `fauna.account.get` and re-announces
//! it with an `AccountUpdated` push, but an app that never reads it leaves the
//! member to find out when sign-in is refused, after the export window has
//! closed. This feeder is that read.
//!
//! # Why the body lives in the sweep crate
//!
//! The same reason as feeder #4 (`domain_expiry`'s module note): the alert has
//! no page of its own — the export is the member's ordinary Settings action and
//! the banner is text — so the sweep is the feeder's entire client-side
//! existence. The read is `fauna_client_account::AccountClient::get`, the typed
//! call every app's Settings already uses.
//!
//! # No skip, and no floor
//!
//! **An account with no eviction is *checked*, never skipped.** The nest's
//! answer is complete either way — there is no "nothing to check" state, only
//! "no eviction stands", which clears.
//!
//! **The gate is a value the nest computes, and no floor is owed under it**
//! (`critical-alerts.md` § Feeders, the audited-party rule). The rule exists
//! where the nest is the *audited* party and could decline an audit by
//! answering with nothing. Here the nest is the party carrying the eviction
//! out: a nest that hides an eviction from its own member is the nest attacking
//! that member, and it could as well delete the account with no warning at
//! all. This alert defends a member against not knowing about an honest nest's
//! process, never against the nest — so there is nothing independent for a
//! floor to stand on, and the owner doc says so rather than leaving it silent.
//!
//! # Dates in UTC
//!
//! The sweep is wasm-safe and takes no UTC offset from its seven callers (a new
//! parameter would be seven app changes, which this crate exists to avoid), so
//! the lines give each deadline as a whole-day countdown — offset-free — beside
//! an explicit UTC `YYYY-MM-DD HH:MM`, never a local time it cannot know.

use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_protocol::account::{AccountEviction, AccountGetReply, EVICTION_STATUS_WARNING};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::{FeederFailure, SweepReport, feeders};

// i18n keys. The headline carries both dates and the export remedy; the
// reason is its own line because an eviction may carry none, and an English
// "Reason: (none)" composed in Rust would not translate.
const WARNING: &str = "critical_alerts.eviction_warning";
const WARNING_UNDATED: &str = "critical_alerts.eviction_warning_undated";
const REASON: &str = "critical_alerts.eviction_warning_reason";

/// The alert's registry key — actor-scoped, like every alert about one
/// account (`critical-alerts.md` § Feeders, key `eviction:<actor_id>`).
pub fn eviction_alert_key(actor_id: &ActorId) -> String {
    format!("eviction:{}", actor_id.to_hex())
}

/// A fixed `YYYY-MM-DD HH:MM` rendering of unix seconds, in UTC (the module
/// note says why UTC).
fn utc_minute(secs: i64) -> String {
    let (days, hh, mm, _) = fauna_core::caltime::epoch_secs_to_days_and_time(secs);
    let (y, m, d) = fauna_core::caltime::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// The alert's lines for an eviction, as of `now` (unix seconds).
///
/// Pure — no clock read, no I/O — so the copy is testable on every platform.
/// `None` means "no alert": anything but the `warning` phase. A suspended or
/// deleting account has no signed-in member left to warn, and a direct
/// suspension carries no deletion timeline (`admin.md`, *The member is told*).
pub fn eviction_alert_lines(eviction: &AccountEviction, now: i64) -> Option<Vec<LocalizedText>> {
    if eviction.status != EVICTION_STATUS_WARNING {
        return None;
    }
    let headline = match (eviction.suspend_at, eviction.delete_at) {
        (Some(suspend_at), Some(delete_at)) => {
            let days = fauna_client_recovery::replacement::days_remaining_from(suspend_at, now);
            LocalizedText::key_args(
                WARNING,
                [
                    ("days", days.to_string()),
                    ("suspend_at", utc_minute(suspend_at)),
                    ("delete_at", utc_minute(delete_at)),
                ],
            )
        }
        // The nest stamps both dates in the eviction's own write, so this is a
        // malformed record, not a state — but an eviction stands, and saying so
        // without the dates beats saying nothing.
        _ => LocalizedText::key(WARNING_UNDATED),
    };
    let mut lines = vec![headline];
    let reason = eviction.reason.trim();
    if !reason.is_empty() {
        lines.push(LocalizedText::key_arg(REASON, "reason", reason));
    }
    Some(lines)
}

/// Post or clear the member's eviction alert from an already-read account
/// reply, and record the check.
///
/// Both outcomes route through one call, for the reason every feeder's `sync_*`
/// does: a called-off eviction must take its banner with it.
pub fn sync_eviction_alert(
    alerts: &fauna_client_alerts::CriticalAlerts,
    actor_id: &ActorId,
    reply: &AccountGetReply,
    report: &mut SweepReport,
    now: i64,
) {
    let key = eviction_alert_key(actor_id);
    match reply
        .eviction
        .as_ref()
        .and_then(|e| eviction_alert_lines(e, now))
    {
        Some(lines) => {
            tracing::warn!(
                suspend_at = ?reply.eviction.as_ref().and_then(|e| e.suspend_at),
                delete_at = ?reply.eviction.as_ref().and_then(|e| e.delete_at),
                "this account is under eviction — raising the critical alert"
            );
            alerts.post(key, lines);
        }
        None => alerts.clear(&key),
    }
    report.check(feeders::EVICTION_WARNING);
}

/// Read the caller's own account and sync the alert from it.
pub(crate) async fn run_eviction_feeder<R>(
    rpc: R,
    alerts: &fauna_client_alerts::CriticalAlerts,
    actor_id: &ActorId,
    report: &mut SweepReport,
    now: i64,
) where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    match fauna_client_account::AccountClient::new(rpc).get().await {
        Ok(reply) => sync_eviction_alert(alerts, actor_id, &reply, report, now),
        Err(e) => {
            // Unreachable is not resolved: a standing warning stays up, the
            // next sweep retries — the sweep's fail-safe direction.
            tracing::warn!(
                error = %e,
                "session-start sweep: account read failed; \
                 any standing eviction alert is left as-is and the next sweep retries"
            );
            report.failures.push(FeederFailure {
                feeder: feeders::EVICTION_WARNING,
                error: e.to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;
    /// 2027-01-15 08:00:00 UTC.
    const NOW: i64 = 1_800_000_000;

    fn eviction(status: &str, reason: &str) -> AccountEviction {
        AccountEviction {
            status: status.into(),
            reason: reason.into(),
            category: "other".into(),
            warned_at: Some(NOW),
            suspend_at: Some(NOW + 14 * DAY),
            delete_at: Some(NOW + 28 * DAY),
            export_token: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn the_key_is_actor_scoped() {
        let a = ActorId([1; 32]);
        assert_eq!(eviction_alert_key(&a), format!("eviction:{}", a.to_hex()));
    }

    #[test]
    fn a_warning_names_both_dates_in_utc_and_the_countdown() {
        let lines = eviction_alert_lines(&eviction("warning", ""), NOW).expect("alarms");
        assert_eq!(lines.len(), 1, "no reason line when the admin gave none");
        assert_eq!(lines[0].key, WARNING);
        assert_eq!(lines[0].args["days"], "14");
        assert_eq!(lines[0].args["suspend_at"], utc_minute(NOW + 14 * DAY));
        assert_eq!(lines[0].args["delete_at"], utc_minute(NOW + 28 * DAY));
        assert_eq!(utc_minute(0), "1970-01-01 00:00");
        assert_eq!(utc_minute(NOW), "2027-01-15 08:00");
    }

    #[test]
    fn the_admins_reason_is_its_own_line() {
        let lines = eviction_alert_lines(&eviction("warning", "  spam  "), NOW).expect("alarms");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].key, REASON);
        assert_eq!(lines[1].args["reason"], "spam");
    }

    #[test]
    fn only_the_warning_phase_alarms() {
        for status in ["suspended", "deleting", ""] {
            assert!(
                eviction_alert_lines(&eviction(status, "x"), NOW).is_none(),
                "{status}"
            );
        }
    }

    #[test]
    fn a_record_missing_its_dates_still_alarms_undated() {
        let mut e = eviction("warning", "");
        e.delete_at = None;
        let lines = eviction_alert_lines(&e, NOW).expect("an eviction stands");
        assert_eq!(lines[0].key, WARNING_UNDATED);
        assert!(lines[0].args.is_empty());
    }

    #[test]
    fn the_countdown_rounds_up_and_saturates() {
        let mut e = eviction("warning", "");
        e.suspend_at = Some(NOW + 1);
        assert_eq!(eviction_alert_lines(&e, NOW).unwrap()[0].args["days"], "1");
        e.suspend_at = Some(NOW - DAY);
        assert_eq!(eviction_alert_lines(&e, NOW).unwrap()[0].args["days"], "0");
    }
}
