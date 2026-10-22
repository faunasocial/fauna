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
    /// Paint the by-continuing terms notice (`onboarding-terms-notice` +
    /// `onboarding-terms-link`) above the bottom row — true iff `outcome` is
    /// `AlreadyOnNest`, the one outcome whose Continue signs in rather than
    /// leading to a page with its own notice (`terms-and-content-guidelines.md`
    /// § The decisions, item 3 (iv)). Derived from `outcome` on every read
    /// ([`terms_notice_visible_for`]), so the apps never compare the outcome.
    /// Additive: absent in older JSON reads as hidden.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub terms_notice_visible: bool,
}

/// Whether `handle_entry` shows the terms notice for this outcome — see
/// [`HandleCheckSnapshot::terms_notice_visible`]. Exhaustive on purpose: a new
/// outcome must decide whether its Continue makes an account here.
pub fn terms_notice_visible_for(outcome: &HandleCheckOutcome) -> bool {
    match outcome {
        HandleCheckOutcome::AlreadyOnNest { .. } => true,
        HandleCheckOutcome::None
        | HandleCheckOutcome::FormatInvalid
        | HandleCheckOutcome::TldInvalid
        | HandleCheckOutcome::DomainAvailable { .. }
        | HandleCheckOutcome::RegisteredNoNest
        | HandleCheckOutcome::NestRunningUserUnregistered
        | HandleCheckOutcome::UnregisteredUnclaimedNest
        | HandleCheckOutcome::ProbeError { .. } => false,
    }
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
            terms_notice_visible: false,
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

    fn already_on_nest(handle_matches: bool) -> HandleCheckOutcome {
        HandleCheckOutcome::AlreadyOnNest {
            handle_matches,
            current_handle: Some("a@b".into()),
        }
    }

    /// `terms-and-content-guidelines.md` § The decisions, item 3 (iv): on
    /// `handle_entry` the by-continuing notice shows only where Continue signs
    /// in — the one outcome whose Continue leads to no page with its own
    /// notice. Both spellings of the handle match sign in alike.
    #[test]
    fn terms_notice_shows_on_already_on_nest() {
        assert!(terms_notice_visible_for(&already_on_nest(true)));
        assert!(terms_notice_visible_for(&already_on_nest(false)));
    }

    /// Continue leads on to `invite_request`, which carries its own notice.
    #[test]
    fn terms_notice_hidden_on_nest_running_user_unregistered() {
        assert!(!terms_notice_visible_for(
            &HandleCheckOutcome::NestRunningUserUnregistered
        ));
    }

    /// Continue leads on to `claim_code`, which carries its own notice.
    #[test]
    fn terms_notice_hidden_on_unregistered_unclaimed_nest() {
        assert!(!terms_notice_visible_for(
            &HandleCheckOutcome::UnregisteredUnclaimedNest
        ));
    }

    /// The new-nest paths reach `nest_provisioning`, which carries its own
    /// notice.
    #[test]
    fn terms_notice_hidden_on_domain_outcomes() {
        assert!(!terms_notice_visible_for(
            &HandleCheckOutcome::DomainAvailable {
                buyable_via_provider: true,
                price: None,
            }
        ));
        assert!(!terms_notice_visible_for(
            &HandleCheckOutcome::RegisteredNoNest
        ));
    }

    /// Nothing to continue to: no check yet, a bad handle, a failed probe.
    #[test]
    fn terms_notice_hidden_without_a_usable_outcome() {
        for outcome in [
            HandleCheckOutcome::None,
            HandleCheckOutcome::FormatInvalid,
            HandleCheckOutcome::TldInvalid,
            HandleCheckOutcome::ProbeError {
                phase: HandleCheckPhase::NestProbe,
                transient: true,
                cause: "x".into(),
            },
        ] {
            assert!(!terms_notice_visible_for(&outcome), "{outcome:?}");
        }
        assert!(!HandleCheckSnapshot::idle().terms_notice_visible);
    }

    /// The field is additive: a snapshot serialized before it existed (the e2e
    /// bridge's injected JSON) still reads, as hidden.
    #[test]
    fn a_snapshot_without_the_field_reads_as_hidden() {
        let mut v = serde_json::to_value(HandleCheckSnapshot::idle()).unwrap();
        v.as_object_mut().unwrap().remove("terms_notice_visible");
        let s: HandleCheckSnapshot = serde_json::from_value(v).unwrap();
        assert!(!s.terms_notice_visible);
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
            terms_notice_visible: true,
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: HandleCheckSnapshot = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }
}
