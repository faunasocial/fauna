//! The "Almost ready" surface (`onboarding.md` § "Almost ready" surface).
//!
//! ui.yaml `onboarding.awaiting_manual_dns` elements: `awaiting-dns-records`,
//! `awaiting-dns-status`, `awaiting-dns-recheck-button`, `awaiting-dns-copy-button`,
//! `awaiting-dns-fallthrough-button`.
//!
//! **Not an `OnboardingStep`.** The surface is keyed on `wizard_outcome() ==
//! AwaitingManualDns`, which is true on both paths that reach it — the
//! same-session exit (`continue_from_dns_post_instructions`) and the relaunch
//! hydration (`seed_awaiting_manual_dns`, driven by the launch machine's
//! `WizardAt{AwaitingManualDns}` row). One outcome, one surface, so the two
//! paths cannot drift.
//!
//! The user provisioned a nest but chose "Set up later" for DNS. Until the
//! records they must add at their registrar take effect the nest is unreachable,
//! so the surface shows the records and polls `recheck_manual_dns()` — the
//! machine owns the probe + claim; the client owns only the cadence (mirroring
//! `recheck_invite_status`, so native and wasm behave identically).

use fauna_i18n::strings::onboarding::awaiting_dns as t;
use fauna_i18n::strings::onboarding::launch as launch_t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

/// How often the surface re-probes while it is shown. The nest is waiting on DNS
/// propagation, so the useful range is seconds-to-minutes; 10s keeps the status
/// line honest without hammering a nest that is still booting.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    Vec::new()
}

// The records text comes from the shared machine
// (`awaiting_dns_records_text()`), so the label here, the copy action below, and
// every other app all hand the user the same instruction. It is one element
// (not one per record) because ui.yaml scopes a single `awaiting-dns-records` ID
// to the page — an indexed per-record ID would be an invented app-specific
// element.

pub fn elements(w: &Wizard) -> Vec<Element> {
    let snap = w.machine.awaiting_manual_dns_snapshot();
    vec![
        Element::label(
            ids::AWAITING_DNS_RECORDS,
            w.machine.awaiting_dns_records_text(),
        ),
        // The machine's `LocalizedText` — `Pending`/`Checking`/`Claiming`/
        // `Claimed`/`Error{cause}` each carry their own key, so the state is
        // never re-derived here.
        Element::label(ids::AWAITING_DNS_STATUS, super::localized(&snap.message)),
        Element::button(
            ids::AWAITING_DNS_RECHECK_BUTTON,
            t::RECHECK_BUTTON,
            // The probe is single-shot; disabling it while one is in flight is
            // what stops a keyboard-mashing user from stacking probes.
            !matches!(
                snap.state,
                fauna_onboarding_machine::AwaitingDnsState::Checking
                    | fauna_onboarding_machine::AwaitingDnsState::Claiming
            ),
            Action::RecheckManualDns,
        ),
        Element::button(
            ids::AWAITING_DNS_COPY_BUTTON,
            t::COPY_BUTTON,
            // Nothing to copy in the records-less mode, so the button is inert
            // there rather than answering a click with an empty clipboard. The
            // machine decides; this page does not re-derive the rule.
            w.machine.awaiting_dns_copy_enabled(),
            Action::CopyDnsRecords,
        ),
        // The exit for a box that will never answer (`onboarding-provisioning.md`
        // § "Almost ready" surface → *Exit*): the mirror of `launch_retry`'s
        // `launch-fallthrough-button`, so it borrows that surface's label rather
        // than minting a third copy of the words. Whether it is live is the
        // machine's answer (off only under a claim), not re-derived here — and it
        // is deliberately NOT gated on the probe state the recheck button reads,
        // since a box that never answers spends its life in `Checking`.
        Element::button(
            ids::AWAITING_DNS_FALLTHROUGH_BUTTON,
            launch_t::USE_DIFFERENT_NEST,
            w.machine.awaiting_dns_fallthrough_enabled(),
            Action::AbandonAwaitingManualDns,
        ),
    ]
}
