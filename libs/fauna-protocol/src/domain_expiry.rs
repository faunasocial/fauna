//! The **domain-expiry watch** — the deployment's primary-domain registration
//! record, and the pure decision that turns it into an alarm.
//!
//! Owner: `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Domain
//! loss → *Detection* (the two-plane shape, the signal set, the threshold
//! rationale, the audience, the three-outcome rules). The feeder row and why it
//! clears the severity bar: `docs/goal/behavior/critical-alerts.md` § Feeders.
//!
//! # Why the decision lives here rather than in the feeder
//!
//! The nest fetches and persists; the client decides. Putting [`evaluate`] beside
//! the wire type it reads keeps the two arms in one place, testable without a
//! nest, a network, or a clock — and shared, so a future admin page that renders
//! "your domain expires in N days" cannot drift from what the banner alarms on.
//!
//! # The two arms, and why the date alone is not enough
//!
//! 1. **Pre-expiry** — within [`DOMAIN_EXPIRY_ALERT_THRESHOLD_SECS`] (7 days) of
//!    the registration's expiry, or already past it.
//! 2. **Status-driven** — any of the lapse-class statuses in [`LAPSE_STATUSES`],
//!    **regardless of the date**.
//!
//! Arm 2 is the reliable one, and arm 1 is deliberately short. RDAP cannot see
//! auto-renew *intent*, so an at-date auto-renewing registrar shows an
//! approaching expiry every single year: a 30-day window would put a month-long
//! false banner on healthy deployments annually, which is the severity bar's
//! crying-wolf failure verbatim. Conversely a *lapsing* domain can present a
//! healthy future date, because gTLD registry auto-renewal can bump the RDAP
//! expiry a year forward at expiry even when the registrant has not paid (the
//! registrar deletes for credit later). The hold/redemption statuses are what
//! actually announce "this name is dying", and they persist through the grace
//! and redemption windows — the phase where renewal still works and the warning
//! is worth the most.

use crate::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How close to expiry the registration must be before the pre-expiry arm
/// alarms — **7 days**.
///
/// A bucket-1 hard constant (`principles.md` § One configuration surface): there
/// is nothing here a user or admin would ever choose, because the only human act
/// in this whole story is renewing the domain, and that happens at the
/// registrar. The number's rationale is this module's header — it is short *on
/// purpose*, and shortening or lengthening it is a decision the owner doc makes,
/// not a tuning knob.
pub const DOMAIN_EXPIRY_ALERT_THRESHOLD_SECS: i64 = 7 * 24 * 60 * 60;

/// The lapse-class registration statuses, **normalized** (see [`normalize_status`]).
///
/// These are the EPP statuses `redemptionPeriod`, `pendingDelete`, `serverHold`
/// and `clientHold`. They are stored here in normalized form because the wire
/// spelling is not one thing: RFC 9083 § 10.2.2 defines the RDAP vocabulary with
/// spaces and lowercase (`"redemption period"`, `"pending delete"`, `"client
/// hold"`, `"server hold"`), while plenty of registries emit the raw camelCase
/// EPP token instead. Matching one spelling would silently lose the reliable arm
/// on half the world's registries.
pub const LAPSE_STATUSES: [&str; 4] = [
    "redemptionperiod",
    "pendingdelete",
    "serverhold",
    "clienthold",
];

/// Stable outcome tokens for a watch attempt — the persisted half of the
/// three-outcome contract (`domains-and-tls-bootstrap.md` § Detection).
pub mod outcomes {
    /// RDAP answered and the record below is what it said.
    pub const CHECKED: &str = "checked";
    /// Nothing to check. Carries a `skip_reasons` token in `detail`.
    pub const SKIPPED: &str = "skipped";
    /// RDAP was present but unreachable or erroring. `detail` carries why. The
    /// next sweep retries, and any standing alert deliberately stands —
    /// unreachable is not resolved.
    pub const FAILED: &str = "failed";
}

/// Stable reason tokens for the skip outcome. Never alarm, never fail.
pub mod skip_reasons {
    /// The TLD is not served by the RDAP bootstrap (many ccTLDs). Absence of
    /// data must not alarm.
    pub const UNSERVED_TLD: &str = "rdap-unserved-tld";
    /// The deployment has no primary domain at all (a domainless box).
    pub const NO_PRIMARY_DOMAIN: &str = "no-primary-domain";
}

/// What the nest last learned about its primary domain's registration.
///
/// One row per deployment — this is the first deployment-scoped alert input, so
/// it is not actor-keyed anywhere: not in the nest's table, not on the wire, and
/// not in the alert key (`critical-alerts.md` § Feeders gives it the bare key
/// `domain-expiry`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainExpiryRecord {
    /// The primary domain the record is about. Empty when the outcome is a
    /// [`skip_reasons::NO_PRIMARY_DOMAIN`] skip.
    pub domain: String,
    /// Registration expiry, unix seconds. `None` when RDAP served the domain but
    /// published no expiry event, which is legitimate for some registries —
    /// arm 1 simply has nothing to say, and arm 2 still decides.
    pub expires_at: Option<i64>,
    /// The registration's RDAP status strings, **verbatim as served** (the
    /// normalization happens at comparison time in [`evaluate`], so a support
    /// read shows what the registry actually said).
    #[serde(default)]
    pub statuses: Vec<String>,
    /// When this record was written, unix seconds — the last *attempt*, whatever
    /// its outcome, which is what a staleness read wants.
    pub fetched_at: i64,
    /// One of [`outcomes`].
    pub outcome: String,
    /// A [`skip_reasons`] token for a skip, the rendered error for a failure,
    /// `None` for a successful check.
    pub detail: Option<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are preserved
    /// here and re-emitted on encode (transport.md § Schema and forward-compat
    /// discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Request for `fauna.domain.expiry.get`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainExpiryRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply for `fauna.domain.expiry.get`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainExpiryReply {
    /// The watch's record, or `None` if the watch has not completed a single
    /// attempt yet (a box in its first minutes). `None` is **not** an alarm and
    /// **not** a failure — there is simply nothing known yet.
    pub record: Option<DomainExpiryRecord>,
    /// Whether the **calling** actor is a nest admin.
    ///
    /// Carried here rather than left to a second `fauna.account.am_i_admin` call
    /// on purpose: the audience is every authenticated user with
    /// role-differentiated lines, so a feeder that learned the domain is lapsing
    /// but could not learn its own role would have no good move — failing the
    /// feeder suppresses a real alarm, and guessing tells an admin to "contact
    /// your admin". One read makes that state unrepresentable.
    pub admin: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Fold an RDAP status string to the form [`LAPSE_STATUSES`] is written in:
/// lowercase, with spaces, hyphens and underscores removed.
///
/// See [`LAPSE_STATUSES`] for why this exists at all.
pub fn normalize_status(status: &str) -> String {
    status
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Why the watch is alarming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlarmReason {
    /// The registration carries a lapse-class status. Carries the status
    /// **as served**, for the alert copy.
    LapseStatus(String),
    /// The registration has already expired.
    Expired,
    /// The registration expires within [`DOMAIN_EXPIRY_ALERT_THRESHOLD_SECS`],
    /// in `days` whole days, rounded **up**.
    ExpiringSoon { days: i64 },
}

/// What one look at a record concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to say — the registration looks healthy, or the record is a skip,
    /// or nothing has been fetched yet. The feeder **clears**.
    Quiet,
    /// The watch could not reach RDAP. The feeder neither posts nor clears:
    /// unreachable is not resolved, so a standing alert stands.
    Unreachable {
        /// The persisted failure detail, for the log line.
        detail: Option<String>,
    },
    /// Alarm.
    Alarm(AlarmReason),
}

/// Decide what `record` means as of `now` (unix seconds).
///
/// Pure — no clock read, no I/O — so both arms are testable on every platform
/// and the caller owns the time source (the same discipline
/// `fauna_client_recovery::alerts` keeps).
///
/// **Arm order is load-bearing.** The status arm is checked *first* and does not
/// consult the date at all, because the lapse case is exactly the one where the
/// date lies: a registry auto-renewal can push the RDAP expiry a year out on a
/// domain that is already in `redemptionPeriod`. Checking the date first would
/// let that record fall through to `Quiet`.
pub fn evaluate(record: &DomainExpiryRecord, now: i64) -> Verdict {
    if record.outcome == outcomes::FAILED {
        return Verdict::Unreachable {
            detail: record.detail.clone(),
        };
    }
    // A skip is silent on the banner (`domains-and-tls-bootstrap.md` § Detection:
    // absence of data must not alarm) but never silent in the report — the
    // caller records the token; this decision only refuses to raise a banner.
    if record.outcome != outcomes::CHECKED {
        return Verdict::Quiet;
    }

    // Arm 2 first — see the doc comment above.
    for status in &record.statuses {
        let normalized = normalize_status(status);
        if LAPSE_STATUSES.contains(&normalized.as_str()) {
            return Verdict::Alarm(AlarmReason::LapseStatus(status.clone()));
        }
    }

    // Arm 1. No published expiry is legitimate; it simply leaves this arm with
    // nothing to say.
    let Some(expires_at) = record.expires_at else {
        return Verdict::Quiet;
    };
    let remaining = expires_at - now;
    if remaining <= 0 {
        return Verdict::Alarm(AlarmReason::Expired);
    }
    if remaining <= DOMAIN_EXPIRY_ALERT_THRESHOLD_SECS {
        // Whole days, rounded **up**: a registration with 47 hours left must not
        // read as "1 day", and one in its final hours must never round down to
        // "0 days" and read as already lost. (`fauna_client_recovery::alerts`
        // renders its own countdown the same way, for the same reason.)
        // `remaining` is > 0 here, so the cast is lossless and `div_ceil` is the
        // stable unsigned one.
        return Verdict::Alarm(AlarmReason::ExpiringSoon {
            days: (remaining as u64).div_ceil(24 * 60 * 60) as i64,
        });
    }
    Verdict::Quiet
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 24 * 60 * 60;
    const NOW: i64 = 1_800_000_000;

    fn checked(expires_at: Option<i64>, statuses: &[&str]) -> DomainExpiryRecord {
        DomainExpiryRecord {
            domain: "example.org".into(),
            expires_at,
            statuses: statuses.iter().map(|s| (*s).to_string()).collect(),
            fetched_at: NOW,
            outcome: outcomes::CHECKED.into(),
            detail: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_healthy_registration_is_quiet() {
        let r = checked(Some(NOW + 90 * DAY), &["active"]);
        assert_eq!(evaluate(&r, NOW), Verdict::Quiet);
    }

    #[test]
    fn the_pre_expiry_arm_alarms_inside_seven_days_and_not_outside() {
        // Just outside the window: silent. This is the whole point of the short
        // threshold — a healthy at-date auto-renewer sits here every year.
        let outside = checked(Some(NOW + 8 * DAY), &["active"]);
        assert_eq!(evaluate(&outside, NOW), Verdict::Quiet);

        let inside = checked(Some(NOW + 3 * DAY), &["active"]);
        assert_eq!(
            evaluate(&inside, NOW),
            Verdict::Alarm(AlarmReason::ExpiringSoon { days: 3 })
        );
    }

    #[test]
    fn the_countdown_rounds_up_so_a_final_hour_never_reads_as_zero_days() {
        let r = checked(Some(NOW + 3600), &["active"]);
        assert_eq!(
            evaluate(&r, NOW),
            Verdict::Alarm(AlarmReason::ExpiringSoon { days: 1 })
        );
        // 47 hours left must say 2 days, not 1.
        let r = checked(Some(NOW + 47 * 3600), &["active"]);
        assert_eq!(
            evaluate(&r, NOW),
            Verdict::Alarm(AlarmReason::ExpiringSoon { days: 2 })
        );
    }

    #[test]
    fn an_already_expired_registration_alarms() {
        let r = checked(Some(NOW - 1), &["active"]);
        assert_eq!(evaluate(&r, NOW), Verdict::Alarm(AlarmReason::Expired));
    }

    /// The status arm's whole reason for existing: a lapsing domain whose
    /// registry auto-renewal already pushed the date a year out. The date arm
    /// says "healthy"; the status says "dying". The status must win.
    #[test]
    fn a_future_dated_registration_in_redemption_still_alarms() {
        let r = checked(Some(NOW + 365 * DAY), &["redemption period"]);
        assert_eq!(
            evaluate(&r, NOW),
            Verdict::Alarm(AlarmReason::LapseStatus("redemption period".into()))
        );
    }

    /// Both wire spellings must hit — RFC 9083's spaced-lowercase vocabulary and
    /// the raw EPP camelCase many registries emit instead.
    #[test]
    fn both_rdap_and_epp_status_spellings_are_recognized() {
        for spelling in [
            "redemption period",
            "redemptionPeriod",
            "pending delete",
            "pendingDelete",
            "client hold",
            "clientHold",
            "server hold",
            "serverHold",
            // Belt and braces: a registry that hyphenates or shouts.
            "PENDING-DELETE",
        ] {
            let r = checked(Some(NOW + 365 * DAY), &[spelling]);
            assert!(
                matches!(
                    evaluate(&r, NOW),
                    Verdict::Alarm(AlarmReason::LapseStatus(_))
                ),
                "{spelling} should be recognized as a lapse-class status"
            );
        }
    }

    /// Ordinary healthy statuses must not trip the arm — `clientTransferProhibited`
    /// is on nearly every registered domain in the world, and it contains
    /// neither "hold" nor "delete" but is adjacent enough to catch a sloppy
    /// substring match.
    #[test]
    fn ordinary_statuses_do_not_alarm() {
        for spelling in [
            "active",
            "client transfer prohibited",
            "clientTransferProhibited",
            "clientUpdateProhibited",
            "server delete prohibited",
            "associated",
        ] {
            let r = checked(Some(NOW + 90 * DAY), &[spelling]);
            assert_eq!(
                evaluate(&r, NOW),
                Verdict::Quiet,
                "{spelling} must not alarm"
            );
        }
    }

    #[test]
    fn a_served_domain_with_no_published_expiry_is_quiet_but_still_status_checked() {
        let quiet = checked(None, &["active"]);
        assert_eq!(evaluate(&quiet, NOW), Verdict::Quiet);

        let loud = checked(None, &["serverHold"]);
        assert!(matches!(
            evaluate(&loud, NOW),
            Verdict::Alarm(AlarmReason::LapseStatus(_))
        ));
    }

    #[test]
    fn a_skip_is_quiet() {
        let r = DomainExpiryRecord {
            domain: String::new(),
            expires_at: None,
            statuses: vec![],
            fetched_at: NOW,
            outcome: outcomes::SKIPPED.into(),
            detail: Some(skip_reasons::NO_PRIMARY_DOMAIN.into()),
            extra: Default::default(),
        };
        assert_eq!(evaluate(&r, NOW), Verdict::Quiet);
    }

    /// A failure must be distinguishable from health, or an unreachable RDAP
    /// server would silently clear a standing alarm — the exact fail-open the
    /// sweep's "a failure never clears" rule exists to prevent.
    #[test]
    fn a_failure_is_unreachable_not_quiet() {
        let r = DomainExpiryRecord {
            domain: "example.org".into(),
            expires_at: Some(NOW + DAY),
            statuses: vec!["redemptionPeriod".into()],
            fetched_at: NOW,
            outcome: outcomes::FAILED.into(),
            detail: Some("503 from rdap.example".into()),
            extra: Default::default(),
        };
        assert_eq!(
            evaluate(&r, NOW),
            Verdict::Unreachable {
                detail: Some("503 from rdap.example".into())
            }
        );
    }

    #[test]
    fn record_and_reply_round_trip() {
        let record = checked(Some(NOW + DAY), &["clientHold", "active"]);
        let bytes = crate::encode_canonical(&record).unwrap();
        let back: DomainExpiryRecord = crate::decode_strict(&bytes).unwrap();
        assert_eq!(back, record);

        let reply = DomainExpiryReply {
            record: Some(record),
            admin: true,
            extra: Default::default(),
        };
        let bytes = crate::encode_canonical(&reply).unwrap();
        let back: DomainExpiryReply = crate::decode_strict(&bytes).unwrap();
        assert_eq!(back, reply);
    }
}
