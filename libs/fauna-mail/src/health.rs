//! Pure mail-health fold for the admin readout.
//!
//! Implements the states table of `docs/goal/behavior/mail-deliverability.md`
//! § Admin-pane Deliverability surface → *The mail health readout*: one
//! categorical state over facts the nest already holds, **worst state wins** in
//! exactly the order of [`MailHealthState::ORDER`], plus the seven check rows the
//! `admin-mail-health-check` component renders, in their fixed order.
//!
//! Pure std — no clock, no DB, no I/O — so it is WASM-safe and unit-testable in
//! isolation. The nest gathers the inputs (`fauna.bridges.mail_health`) and calls
//! [`evaluate`]; the apps dumb-render the reply through the shared label functions
//! in `fauna_core::format` (`mail_health_state_label`,
//! `mail_health_check_state_label`), so the state decision exists once.
//!
//! The two heartbeat stamps (last delivered / last received) are **facts, never a
//! state input**: an idle box is not a broken box, and a wall-clock silence
//! threshold would be a guess (§ *Two heartbeat stamps*).

use crate::warmup::WARMUP_UNLIMITED_DAY;

/// The categorical readout state. The wire form ([`as_str`](Self::as_str)) is an
/// **open** string enum: an app that meets a value it does not know renders the
/// generic "needs attention" label, so a newer nest never breaks an older app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailHealthState {
    /// Mail is not enabled — neutral, not an alarm.
    Off,
    /// Mail is enabled but an approved MTA or MDA bridge has no live connection
    /// (or no mail bridge is approved at all).
    BridgeDown,
    /// The latest blocklist self-check lists the outbound IP.
    Blocklisted,
    /// At least one `pending` outbound row has tripped the delayed-delivery
    /// warning (see [`queue_row_stalled`]).
    QueueStalled,
    /// The latest diagnostics run has a failing SPF/DKIM/DMARC/rDNS check.
    RecordsFailing,
    /// Healthy, inside the fresh-IP warm-up ramp.
    WarmingUp,
    /// Healthy.
    Delivering,
}

impl MailHealthState {
    /// Worst first — the fold returns the first state whose condition holds.
    pub const ORDER: [MailHealthState; 7] = [
        Self::Off,
        Self::BridgeDown,
        Self::Blocklisted,
        Self::QueueStalled,
        Self::RecordsFailing,
        Self::WarmingUp,
        Self::Delivering,
    ];

    /// The wire token. Exhaustive by construction — never add a `_` arm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::BridgeDown => "bridge_down",
            Self::Blocklisted => "blocklisted",
            Self::QueueStalled => "queue_stalled",
            Self::RecordsFailing => "records_failing",
            Self::WarmingUp => "warming_up",
            Self::Delivering => "delivering",
        }
    }

    /// Parse a wire token; `None` for a value this build does not know.
    pub fn parse(token: &str) -> Option<Self> {
        Self::ORDER.into_iter().find(|s| s.as_str() == token)
    }
}

/// One check row's verdict. Wire tokens: `pass` / `warn` / `fail` (the
/// diagnostics checklist's vocabulary) plus `info` for a neutral fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Pass,
    Warn,
    Fail,
    Info,
}

impl CheckState {
    /// The wire token. Exhaustive by construction — never add a `_` arm.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Info => "info",
        }
    }
}

/// The i18n keys of the seven check rows, in the fixed render order
/// (`admin-mail-health-check`: bridge connection · blocklist self-check ·
/// outbound queue · DNS/auth records · warm-up ramp · last delivered · last
/// received).
pub const CHECK_LABEL_KEYS: [&str; 7] = [
    "admin.mail_page.health_check_bridge",
    "admin.mail_page.health_check_blocklist",
    "admin.mail_page.health_check_queue",
    "admin.mail_page.health_check_records",
    "admin.mail_page.health_check_warmup",
    "admin.mail_page.health_check_last_delivered",
    "admin.mail_page.health_check_last_received",
];

/// One DNSBL's outcome in the latest blocklist self-check.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlocklistEntry {
    pub server: String,
    pub listed: bool,
    /// The query did not give a clean listed/not-listed answer (resolver
    /// error, rate-limited force-refresh).
    pub errored: bool,
}

/// One row of the latest diagnostics run (`name` + `pass`/`warn`/`fail`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiagnosticEntry {
    pub name: String,
    pub status: String,
}

/// Everything the fold reads. All nest-side facts; see the states table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthInputs {
    /// `mail_enabled`.
    pub mail_enabled: bool,
    /// Approved bridge service users of role MTA or MDA.
    pub mail_bridges_approved: u32,
    /// How many of those hold a live WS connection.
    pub mail_bridges_connected: u32,
    /// The latest blocklist self-check's per-DNSBL rows; `None` = never checked.
    pub blocklist: Option<Vec<BlocklistEntry>>,
    /// Pending outbound rows for which [`queue_row_stalled`] holds.
    pub stalled_outbound: u32,
    /// The latest diagnostics run's rows; `None` = never run.
    pub diagnostics: Option<Vec<DiagnosticEntry>>,
    /// The warm-up ramp's current day (1-based).
    pub warmup_day: u32,
    /// Today's warm-up cap is in force (`today_max` is some — day < 30).
    pub warmup_capped: bool,
}

/// One check row: an i18n label key, a verdict, and a short factual detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthCheck {
    pub label_key: &'static str,
    pub state: CheckState,
    pub detail: String,
}

/// The fold's full output: the categorical state, the seven rows, and the
/// de-listing URL of the first DNSBL listing the outbound IP (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    pub state: MailHealthState,
    pub checks: Vec<HealthCheck>,
    pub delist_url: Option<&'static str>,
}

/// Is this pending outbound row a stall? The condition `mark_outbound_failed`
/// already warns on: the row has **failed at least once** AND is at least
/// `delay_warning_at_hours` old. Deliberately never a bare age test — a
/// warm-up-deferred row is `pending` with an old `created_at` and no failure,
/// and must never read as a stall.
pub fn queue_row_stalled(
    attempt_count: u32,
    created_at: i64,
    now: i64,
    delay_warning_at_hours: u32,
) -> bool {
    attempt_count >= 1 && now - created_at >= i64::from(delay_warning_at_hours) * 3_600
}

/// Does this diagnostics row name an SPF/DKIM/DMARC/rDNS record check (the
/// checks `records_failing` reads; MTA-STS, TLSRPT and the outbound-TLS probe
/// are not records failures)?
pub fn is_records_check(name: &str) -> bool {
    ["SPF ", "DKIM ", "DMARC ", "Reverse-DNS "]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The compile-time de-listing URL for a known DNSBL (`mail-deliverability.md`
/// § Blocklist self-check → *Open de-listing URL*); `None` for an unknown one.
pub fn delist_url_for(server: &str) -> Option<&'static str> {
    match server {
        "zen.spamhaus.org" => Some("https://check.spamhaus.org/"),
        "b.barracudacentral.org" => Some("https://www.barracudacentral.org/rbl/removal-request"),
        "bl.spamcop.net" => Some("https://www.spamcop.net/bl.shtml"),
        _ => None,
    }
}

/// The categorical state alone — worst wins, in [`MailHealthState::ORDER`].
pub fn fold(inputs: &HealthInputs) -> MailHealthState {
    MailHealthState::ORDER
        .into_iter()
        .find(|s| holds(*s, inputs))
        .unwrap_or(MailHealthState::Delivering)
}

/// Whether `state`'s condition holds — one arm per row of the states table.
fn holds(state: MailHealthState, i: &HealthInputs) -> bool {
    match state {
        MailHealthState::Off => !i.mail_enabled,
        MailHealthState::BridgeDown => bridge_down(i),
        MailHealthState::Blocklisted => listed_on(i).next().is_some(),
        MailHealthState::QueueStalled => i.stalled_outbound > 0,
        MailHealthState::RecordsFailing => failing_records(i).next().is_some(),
        MailHealthState::WarmingUp => i.warmup_capped,
        MailHealthState::Delivering => true,
    }
}

fn bridge_down(i: &HealthInputs) -> bool {
    i.mail_bridges_approved == 0 || i.mail_bridges_connected < i.mail_bridges_approved
}

fn listed_on(i: &HealthInputs) -> impl Iterator<Item = &BlocklistEntry> {
    i.blocklist.iter().flatten().filter(|e| e.listed)
}

fn records_rows(i: &HealthInputs) -> impl Iterator<Item = &DiagnosticEntry> {
    i.diagnostics
        .iter()
        .flatten()
        .filter(|d| is_records_check(&d.name))
}

fn failing_records(i: &HealthInputs) -> impl Iterator<Item = &DiagnosticEntry> {
    records_rows(i).filter(|d| d.status == "fail")
}

fn check(index: usize, state: CheckState, detail: impl Into<String>) -> HealthCheck {
    HealthCheck {
        label_key: CHECK_LABEL_KEYS[index],
        state,
        detail: detail.into(),
    }
}

/// The categorical state plus the seven check rows and the de-listing URL.
pub fn evaluate(inputs: &HealthInputs) -> HealthReport {
    let i = inputs;

    let bridge = if !i.mail_enabled {
        check(0, CheckState::Info, "mail is off")
    } else {
        let state = if bridge_down(i) {
            CheckState::Fail
        } else {
            CheckState::Pass
        };
        let detail = format!(
            "{} of {} connected",
            i.mail_bridges_connected, i.mail_bridges_approved
        );
        check(0, state, detail)
    };

    let listed: Vec<&str> = listed_on(i).map(|e| e.server.as_str()).collect();
    let blocklist = match &i.blocklist {
        None => check(1, CheckState::Info, "not checked yet"),
        Some(_) if !listed.is_empty() => check(
            1,
            CheckState::Fail,
            format!("listed on {}", listed.join(", ")),
        ),
        Some(rows) => match rows.iter().filter(|e| e.errored).count() {
            0 => check(1, CheckState::Pass, "not listed"),
            n => check(1, CheckState::Warn, format!("{n} list(s) did not answer")),
        },
    };

    let queue = match i.stalled_outbound {
        0 => check(2, CheckState::Pass, "no delayed messages"),
        n => check(2, CheckState::Fail, format!("{n} message(s) delayed")),
    };

    let failing: Vec<&str> = failing_records(i).map(|d| d.name.as_str()).collect();
    let records = match &i.diagnostics {
        None => check(3, CheckState::Info, "not run yet"),
        Some(_) if !failing.is_empty() => check(
            3,
            CheckState::Fail,
            format!("failing: {}", failing.join(", ")),
        ),
        Some(_) if records_rows(i).any(|d| d.status == "warn") => {
            check(3, CheckState::Warn, "some checks could not complete")
        }
        Some(_) => check(3, CheckState::Pass, "SPF, DKIM, DMARC and reverse DNS pass"),
    };

    let warmup = if i.warmup_capped {
        check(
            4,
            CheckState::Info,
            format!("day {} of {WARMUP_UNLIMITED_DAY}", i.warmup_day),
        )
    } else {
        check(4, CheckState::Pass, "complete")
    };

    // The heartbeats are facts the reply carries in its own fields; the app
    // renders them ("3 min ago" / "never"), so the rows carry no detail.
    let last_delivered = check(5, CheckState::Info, "");
    let last_received = check(6, CheckState::Info, "");

    HealthReport {
        state: fold(i),
        checks: vec![
            bridge,
            blocklist,
            queue,
            records,
            warmup,
            last_delivered,
            last_received,
        ],
        delist_url: listed.iter().find_map(|s| delist_url_for(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600;

    /// A healthy, post-ramp deployment: every fact green.
    fn healthy() -> HealthInputs {
        HealthInputs {
            mail_enabled: true,
            mail_bridges_approved: 2,
            mail_bridges_connected: 2,
            blocklist: Some(vec![BlocklistEntry {
                server: "zen.spamhaus.org".into(),
                listed: false,
                errored: false,
            }]),
            stalled_outbound: 0,
            diagnostics: Some(vec![DiagnosticEntry {
                name: "SPF record valid".into(),
                status: "pass".into(),
            }]),
            warmup_day: 31,
            warmup_capped: false,
        }
    }

    #[test]
    fn healthy_is_delivering() {
        assert_eq!(fold(&healthy()), MailHealthState::Delivering);
    }

    #[test]
    fn off_beats_everything() {
        let mut i = healthy();
        i.mail_enabled = false;
        i.mail_bridges_connected = 0;
        i.stalled_outbound = 3;
        assert_eq!(fold(&i), MailHealthState::Off);
    }

    #[test]
    fn a_disconnected_bridge_is_bridge_down() {
        let mut i = healthy();
        i.mail_bridges_connected = 1;
        i.stalled_outbound = 3;
        assert_eq!(fold(&i), MailHealthState::BridgeDown);
    }

    #[test]
    fn no_approved_bridge_is_bridge_down() {
        let mut i = healthy();
        i.mail_bridges_approved = 0;
        i.mail_bridges_connected = 0;
        assert_eq!(fold(&i), MailHealthState::BridgeDown);
    }

    #[test]
    fn a_listing_is_blocklisted_and_carries_its_delist_url() {
        let mut i = healthy();
        i.blocklist = Some(vec![
            BlocklistEntry {
                server: "zen.spamhaus.org".into(),
                listed: false,
                errored: false,
            },
            BlocklistEntry {
                server: "bl.spamcop.net".into(),
                listed: true,
                errored: false,
            },
        ]);
        i.stalled_outbound = 1;
        let r = evaluate(&i);
        assert_eq!(r.state, MailHealthState::Blocklisted);
        assert_eq!(r.delist_url, delist_url_for("bl.spamcop.net"));
        assert!(r.delist_url.is_some());
        assert_eq!(r.checks[1].state, CheckState::Fail);
    }

    #[test]
    fn a_resolver_error_is_not_a_listing() {
        let mut i = healthy();
        i.blocklist = Some(vec![BlocklistEntry {
            server: "zen.spamhaus.org".into(),
            listed: false,
            errored: true,
        }]);
        let r = evaluate(&i);
        assert_eq!(r.state, MailHealthState::Delivering);
        assert_eq!(r.checks[1].state, CheckState::Warn);
        assert_eq!(r.delist_url, None);
    }

    #[test]
    fn a_stalled_row_is_queue_stalled() {
        let mut i = healthy();
        i.stalled_outbound = 1;
        i.diagnostics = Some(vec![DiagnosticEntry {
            name: "DMARC record present".into(),
            status: "fail".into(),
        }]);
        assert_eq!(fold(&i), MailHealthState::QueueStalled);
    }

    #[test]
    fn a_failing_record_is_records_failing() {
        let mut i = healthy();
        i.warmup_capped = true;
        i.diagnostics = Some(vec![DiagnosticEntry {
            name: "DKIM record present (sel1)".into(),
            status: "fail".into(),
        }]);
        let r = evaluate(&i);
        assert_eq!(r.state, MailHealthState::RecordsFailing);
        assert_eq!(r.checks[3].state, CheckState::Fail);
    }

    #[test]
    fn a_failing_non_record_check_is_not_records_failing() {
        let mut i = healthy();
        i.diagnostics = Some(vec![
            DiagnosticEntry {
                name: "MTA-STS policy file fetchable".into(),
                status: "fail".into(),
            },
            DiagnosticEntry {
                name: "Mail enabled".into(),
                status: "fail".into(),
            },
        ]);
        assert_eq!(fold(&i), MailHealthState::Delivering);
    }

    #[test]
    fn inside_the_ramp_is_warming_up() {
        let mut i = healthy();
        i.warmup_day = 4;
        i.warmup_capped = true;
        let r = evaluate(&i);
        assert_eq!(r.state, MailHealthState::WarmingUp);
        assert_eq!(r.checks[4].state, CheckState::Info);
        assert!(r.checks[4].detail.contains('4'), "{}", r.checks[4].detail);
    }

    #[test]
    fn never_checked_and_never_run_are_neutral_not_failures() {
        let mut i = healthy();
        i.blocklist = None;
        i.diagnostics = None;
        let r = evaluate(&i);
        assert_eq!(r.state, MailHealthState::Delivering);
        assert_eq!(r.checks[1].state, CheckState::Info);
        assert_eq!(r.checks[3].state, CheckState::Info);
    }

    #[test]
    fn seven_checks_in_the_fixed_order() {
        let r = evaluate(&healthy());
        let keys: Vec<&str> = r.checks.iter().map(|c| c.label_key).collect();
        assert_eq!(keys, CHECK_LABEL_KEYS);
    }

    #[test]
    fn a_warmup_deferred_row_is_not_a_stall() {
        // Deferred by the warm-up cap: pending, a day old, never attempted.
        assert!(!queue_row_stalled(0, 0, 24 * HOUR, 4));
    }

    #[test]
    fn a_failed_row_past_the_warning_age_is_a_stall() {
        assert!(queue_row_stalled(1, 0, 4 * HOUR, 4));
        assert!(queue_row_stalled(3, 0, 5 * HOUR, 4));
    }

    #[test]
    fn a_failed_row_younger_than_the_warning_age_is_not_a_stall() {
        assert!(!queue_row_stalled(2, 0, 4 * HOUR - 1, 4));
    }

    #[test]
    fn records_checks_are_spf_dkim_dmarc_rdns() {
        for name in [
            "SPF record present",
            "SPF record valid",
            "DKIM record present",
            "DKIM record present (sel1)",
            "DMARC record present",
            "DMARC policy enforcing",
            "Reverse-DNS for outbound IP",
            "Reverse-DNS matches HELO",
        ] {
            assert!(is_records_check(name), "{name}");
        }
        for name in [
            "MTA-STS record present",
            "MTA-STS policy file fetchable",
            "TLSRPT record present",
            "Outbound TLS to gmail.com",
            "Mail enabled",
        ] {
            assert!(!is_records_check(name), "{name}");
        }
    }

    #[test]
    fn every_default_dnsbl_has_a_delist_url() {
        for s in [
            "zen.spamhaus.org",
            "b.barracudacentral.org",
            "bl.spamcop.net",
        ] {
            assert!(delist_url_for(s).is_some(), "{s}");
        }
        assert_eq!(delist_url_for("unknown.example"), None);
    }

    #[test]
    fn wire_tokens_round_trip_and_unknown_is_none() {
        for s in MailHealthState::ORDER {
            assert_eq!(MailHealthState::parse(s.as_str()), Some(s));
        }
        assert_eq!(MailHealthState::parse("on_fire"), None);
    }
}
