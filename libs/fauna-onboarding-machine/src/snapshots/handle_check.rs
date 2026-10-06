use serde::{Deserialize, Serialize};

use crate::state::LocalizedText;
use fauna_provisioning::registrar::TldPriceQuote;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum HandleCheckPhase {
    Idle,
    Parsing,
    DnsLookup,
    NestProbe,
    ChallengeResponse,
    PriceLookup,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum HandleCheckOutcome {
    None,
    FormatInvalid,
    TldInvalid,
    DomainAvailable {
        buyable_via_provider: bool,
        price: Option<TldPriceQuote>,
    },
    RegisteredNoNest,
    AlreadyOnNest {
        handle_matches: bool,
        current_handle: Option<String>,
    },
    NestRunningUserUnregistered,
    /// A nest is reachable on this domain, but `GET /api/v1/setup-status`
    /// reports `claimed: false` — no admin has claimed it yet. The user's
    /// only path forward is the claim_code page (Continue routes there
    /// instead of invite_request, since there is no admin to issue
    /// invites). Discriminator is binary; no associated data.
    UnregisteredUnclaimedNest,
    ProbeError {
        phase: HandleCheckPhase,
        transient: bool,
        cause: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct HandleCheckSnapshot {
    pub phase: HandleCheckPhase,
    pub outcome: HandleCheckOutcome,
    pub message: LocalizedText,
    pub continue_enabled: bool,
    pub control_checkbox_visible: bool,
    pub control_checkbox_checked: bool,
}

impl HandleCheckSnapshot {
    pub fn idle() -> Self {
        Self {
            phase: HandleCheckPhase::Idle,
            outcome: HandleCheckOutcome::None,
            // **Not `default()` — an empty message here is a copy bug.** Idle
            // is the state every user meets first, and it is the state where
            // BOTH `handle-check-button` and `handle-entry-continue-button` are
            // dead. Every app renders `handle-message-area` from this field, so
            // a blank one left the first field of onboarding showing two DIM
            // buttons and no explanation, on all 7 apps at once (`ui/README.md`
            // § Copy comprehensibility rule 5; found by the walk's wizard
            // driver, 2026-08-05 — the `dns_status_text_key` defect one page
            // later in the same wizard was the identical shape).
            //
            // Carrying it on the SNAPSHOT rather than per app is what makes the
            // fix uniform: the apps already render this field and need no code
            // change (priority #2). The live phases overwrite it within
            // milliseconds of Check, so it reads only while it is true.
            message: LocalizedText::key("onboarding.handle_check.idle"),
            continue_enabled: false,
            control_checkbox_visible: false,
            control_checkbox_checked: false,
        }
    }
}

impl Default for HandleCheckSnapshot {
    fn default() -> Self {
        Self::idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_has_continue_disabled() {
        let s = HandleCheckSnapshot::idle();
        assert_eq!(s.phase, HandleCheckPhase::Idle);
        assert!(!s.continue_enabled);
    }

    /// Idle disables Check and Continue, so idle owes the reason — and it is
    /// carried here so all 7 apps get it from the field they already render
    /// (`ui/README.md` § Copy comprehensibility rule 5).
    #[test]
    fn the_idle_snapshot_explains_its_two_dead_buttons() {
        let s = HandleCheckSnapshot::idle();
        assert!(!s.continue_enabled, "precondition: Continue is dead");
        assert_eq!(s.message.key, "onboarding.handle_check.idle");
        assert!(
            !s.message.key.is_empty(),
            "a blank message paints a blank line, which explains nothing"
        );
    }

    #[test]
    fn unregistered_unclaimed_nest_round_trips() {
        // The Python E2E test sends `"UnregisteredUnclaimedNest"` as a unit
        // variant; pin both the JSON shape and the round-trip. If serde's
        // default external tagging changes, the e2e bridge breaks silently.
        let outcome = HandleCheckOutcome::UnregisteredUnclaimedNest;
        let raw = serde_json::to_string(&outcome).unwrap();
        assert_eq!(raw, "\"UnregisteredUnclaimedNest\"");
        let outcome2: HandleCheckOutcome = serde_json::from_str(&raw).unwrap();
        assert_eq!(outcome, outcome2);
    }

    #[test]
    fn snapshot_serializes_round_trip() {
        let s = HandleCheckSnapshot {
            phase: HandleCheckPhase::Complete,
            outcome: HandleCheckOutcome::AlreadyOnNest {
                handle_matches: true,
                current_handle: Some("a@b".into()),
            },
            message: LocalizedText {
                key: "x".into(),
                args: Default::default(),
            },
            continue_enabled: true,
            control_checkbox_visible: false,
            control_checkbox_checked: false,
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: HandleCheckSnapshot = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }
}
