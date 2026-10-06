//! Snapshot for the wizard's post-provisioning "Almost ready" surface.
//!
//! Reached after the deferred-DNS provisioning path: the wizard exits to
//! `Done` with `wizard_outcome() == WizardOutcome::AwaitingManualDns`
//! (see `continue_from_dns_post_instructions`). The per-app glue saves
//! the slot (nest_url, handle, dns_records, claim_code) and renders this
//! surface, which shows the DNS records the user must add at their
//! registrar and polls `recheck_manual_dns()` until the freshly-
//! provisioned nest comes online and can be claimed.
//!
//! Transport note: every probe on this surface is *pre-claim*, so there is no
//! authenticated *actor* session — the probes ride the **pre-identity
//! (anonymous) WS-RPC** kinds `fauna.setup.status` (reachability +
//! claim-status) and `fauna.auth.claim_admin` (the claim), behind the `NestApi`
//! seam (`WsNestApi`). The HTTP twins these once used were retired in S4c2.
//! On a successful claim the machine routes to `NatModeChoice`
//! (mirroring `wizard_submit_claim_code`), not straight to `LoggedIn`.
//!
//! Per `docs/goal/behavior/onboarding.md` § "Wizard exit handling"
//! (`AwaitingManualDns` row) and § "Manual DNS setup".

use serde::{Deserialize, Serialize};

use crate::state::{DnsRecordPlain, LocalizedText};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AwaitingDnsState {
    /// Resting state. The user has been shown the DNS records; the next
    /// `recheck_manual_dns()` will probe. Also the landing state after a
    /// *transient* probe/claim failure (nest not reachable yet, or a 5xx
    /// while claiming) — the client keeps polling.
    Pending,
    /// A reachability probe (`fauna.setup.status`) is in flight.
    Checking,
    /// The nest is reachable and unclaimed; the claim
    /// (`fauna.auth.claim_admin`) is in flight.
    Claiming,
    /// The claim succeeded (or the nest was already claimed). The machine
    /// transitions to `NatModeChoice` (fresh claim) or sets
    /// `wizard_outcome()` to `LoggedIn` (already-claimed resume); this state
    /// is set briefly before the surface changes.
    Claimed,
    /// Terminal failure: the claim was rejected (bad/expired claim code), the
    /// local identity is corrupt, or a probe/claim connection failed
    /// first-contact identity verification against the held root
    /// (`security.md` § Pre-claim surfacing — a mismatch on a box this client
    /// provisioned is MITM/bug, never "still waiting for DNS"). Transient
    /// failures do *not* land here — they return to `Pending` so polling
    /// continues.
    Error { cause: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AwaitingManualDnsSnapshot {
    pub state: AwaitingDnsState,
    /// The manual DNS records the user must add at their registrar, surfaced
    /// for display. Populated when the surface is entered
    /// (`continue_from_dns_post_instructions` / `seed_awaiting_manual_dns`)
    /// and preserved across `recheck_manual_dns()` ticks.
    pub dns_records: Vec<DnsRecordPlain>,
    pub message: LocalizedText,
}

impl AwaitingManualDnsSnapshot {
    pub fn idle() -> Self {
        Self {
            state: AwaitingDnsState::Pending,
            dns_records: Vec::new(),
            message: LocalizedText {
                key: "onboarding.awaiting_dns.pending".into(),
                args: Default::default(),
            },
        }
    }
}

/// The surface's **resting** status copy — the one line that differs between its
/// two modes (`docs/goal/behavior/onboarding.md` § "Almost ready" surface,
/// *Two modes*).
///
/// With records, the page is an instruction: add these at your registrar. With
/// none, it is a reassurance: a setup interrupted after the box was ordered has
/// nothing for the user to paste — the DNS was ours to write, or is not due yet
/// — and the honest copy says the server is starting and they may close the app.
/// Telling that user to "add the DNS records below" under an empty list is the
/// one thing the page must not do.
///
/// Derived on read from the record list rather than decided by whoever seeded
/// the surface (Architectural rule 4, the same call `invite_request`'s
/// `message_for` makes), so every app gets both modes with no per-app branch and
/// the two cannot drift apart.
pub fn resting_message_key(records: &[DnsRecordPlain]) -> &'static str {
    if records.is_empty() {
        "onboarding.awaiting_dns.server_starting"
    } else {
        "onboarding.awaiting_dns.pending"
    }
}

/// Whether the surface's "Copy all" button has anything to copy — the second
/// thing that differs between the page's two modes (`onboarding.md` § "Almost
/// ready" surface, *Two modes*).
///
/// A records-less resume has nothing to paste, so the button copies the empty
/// string [`format_dns_records`] returns: an affordance that answers a click by
/// silently doing nothing, which reads to the user as a broken page rather than
/// as an empty one. It is DISABLED there, not hidden — ui.yaml scopes
/// `awaiting-dns-copy-button` to this page's required `elements`, so removing it
/// in one mode would be a per-mode element scope no other app could match.
///
/// Derived from the record list on read, exactly like [`resting_message_key`]
/// beside it and for the same reason: seven apps each writing
/// `!records.is_empty()` at their own button is seven per-app branches of one
/// rule, and the rule that drifts is the one the user meets on the one page
/// where their nest is unreachable.
pub fn copy_all_enabled(records: &[DnsRecordPlain]) -> bool {
    !records.is_empty()
}

/// Whether the surface's exit ("Use a different nest") may be taken right now
/// (`onboarding-provisioning.md` § "Almost ready" surface → *Exit*).
///
/// Off only while a claim is in flight or has just landed: leaving under a claim
/// would race its nest-binding write and strand a claimed box behind a wizard
/// that has forgotten it. **Deliberately ON during a bare probe
/// ([`AwaitingDnsState::Checking`])** — unlike the recheck button, which is off
/// for both — because a box that will never answer spends most of its life in
/// `Checking` (every poll dials it and waits out the connect timeout), and the
/// exit exists for exactly that box: disabling it there would hide the way out
/// on the one page that needs it.
///
/// One rule, read through one machine getter (`awaiting_dns_fallthrough_enabled`),
/// for the reason [`copy_all_enabled`] gives: seven apps each spelling the
/// condition is seven chances to disagree.
pub fn fallthrough_enabled(state: &AwaitingDnsState) -> bool {
    !matches!(
        state,
        AwaitingDnsState::Claiming | AwaitingDnsState::Claimed
    )
}

/// The records the user must add at their registrar, one per line, in the shape
/// they would retype into a registrar panel.
///
/// Shared (rather than a `format!` in each app) for two reasons. Within one
/// app, the records label and the *copy* button must never disagree about
/// what the user has to add — they read this same function. Across clients, this
/// is the literal text of the instruction Fauna gives the user at the one moment
/// their nest is unreachable; six hand-rolled formatters would drift into six
/// different instructions for the same DNS record (priority #1/#2).
pub fn format_dns_records(records: &[DnsRecordPlain]) -> String {
    records
        .iter()
        .map(|r| {
            let priority = r
                .priority
                .map(|p| format!(" priority {p}"))
                .unwrap_or_default();
            format!(
                "{}  {}  {}  (TTL {}{})",
                r.record_type, r.name, r.value, r.ttl, priority
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Default for AwaitingManualDnsSnapshot {
    fn default() -> Self {
        Self::idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_is_pending_with_no_records() {
        let s = AwaitingManualDnsSnapshot::idle();
        assert_eq!(s.state, AwaitingDnsState::Pending);
        assert!(s.dns_records.is_empty());
    }

    /// The page's two modes differ in exactly two derived things — the resting
    /// copy and whether "Copy all" does anything — and both are derived HERE so
    /// no app can disagree with another about which mode it is in. Pinned
    /// together for that reason: they must never answer for different modes.
    #[test]
    fn the_two_modes_are_derived_from_the_same_empty_record_list() {
        let none: Vec<DnsRecordPlain> = Vec::new();
        assert_eq!(
            resting_message_key(&none),
            "onboarding.awaiting_dns.server_starting"
        );
        assert!(
            !copy_all_enabled(&none),
            "a records-less resume has nothing to paste, so Copy all must be \
             inert rather than answering a click with an empty clipboard"
        );

        let some = vec![DnsRecordPlain {
            record_type: "A".into(),
            name: "nest.example.com".into(),
            value: "203.0.113.7".into(),
            ttl: 3600,
            priority: None,
        }];
        assert_eq!(
            resting_message_key(&some),
            "onboarding.awaiting_dns.pending"
        );
        assert!(
            copy_all_enabled(&some),
            "with records to add, Copy all is the affordance that saves the user \
             retyping them at a registrar"
        );
    }

    /// The exit is on for every resting or probing state and off only under a
    /// claim — pinned per state so a new `AwaitingDnsState` variant is a visible
    /// decision here, not a silent default.
    #[test]
    fn the_exit_is_off_only_while_a_claim_is_in_flight() {
        assert!(fallthrough_enabled(&AwaitingDnsState::Pending));
        assert!(
            fallthrough_enabled(&AwaitingDnsState::Checking),
            "a box that never answers lives in Checking — the exit must be reachable there"
        );
        assert!(!fallthrough_enabled(&AwaitingDnsState::Claiming));
        assert!(!fallthrough_enabled(&AwaitingDnsState::Claimed));
        assert!(
            fallthrough_enabled(&AwaitingDnsState::Error {
                cause: "claim refused".into()
            }),
            "a terminal error is the surface's dead end — the exit is the way off it"
        );
    }

    /// Every app renders `awaiting-dns-records` from this and copies *this* to
    /// Every app renders `awaiting-dns-records` from this and copies *this* to
    /// the clipboard, so it is the literal instruction Fauna gives the user at the
    /// one moment their nest is unreachable. Pin it: one line per record, the
    /// optional priority appended only when present.
    #[test]
    fn format_dns_records_renders_one_line_per_record() {
        let records = vec![
            DnsRecordPlain {
                record_type: "A".into(),
                name: "nest.example.com".into(),
                value: "203.0.113.7".into(),
                ttl: 3600,
                priority: None,
            },
            DnsRecordPlain {
                record_type: "MX".into(),
                name: "example.com".into(),
                value: "mail.example.com".into(),
                ttl: 3600,
                priority: Some(10),
            },
        ];
        let out = format_dns_records(&records);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "one line per record; got {out:?}");
        assert_eq!(lines[0], "A  nest.example.com  203.0.113.7  (TTL 3600)");
        assert_eq!(
            lines[1],
            "MX  example.com  mail.example.com  (TTL 3600 priority 10)"
        );
        assert_eq!(format_dns_records(&[]), "");
    }

    #[test]
    fn unit_state_serializes_as_bare_string() {
        // Pin the wire shape: the Python e2e helper sends `{"state": "Pending"}`
        // / `"Checking"` / `"Claiming"` / `"Claimed"` as bare strings
        // (external-tagging default for unit variants). If serde's tagging
        // changes, the bridge breaks silently.
        assert_eq!(
            serde_json::to_string(&AwaitingDnsState::Pending).unwrap(),
            "\"Pending\""
        );
        assert_eq!(
            serde_json::to_string(&AwaitingDnsState::Checking).unwrap(),
            "\"Checking\""
        );
        assert_eq!(
            serde_json::to_string(&AwaitingDnsState::Claiming).unwrap(),
            "\"Claiming\""
        );
        assert_eq!(
            serde_json::to_string(&AwaitingDnsState::Claimed).unwrap(),
            "\"Claimed\""
        );
    }

    #[test]
    fn error_state_round_trips() {
        let s = AwaitingDnsState::Error {
            cause: "claim code expired".into(),
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: AwaitingDnsState = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn snapshot_round_trips_via_json() {
        let s = AwaitingManualDnsSnapshot {
            state: AwaitingDnsState::Claiming,
            dns_records: vec![DnsRecordPlain {
                record_type: "A".into(),
                name: "@".into(),
                value: "1.2.3.4".into(),
                ttl: 300,
                priority: None,
            }],
            message: LocalizedText {
                key: "onboarding.awaiting_dns.claiming".into(),
                args: Default::default(),
            },
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: AwaitingManualDnsSnapshot = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }
}
