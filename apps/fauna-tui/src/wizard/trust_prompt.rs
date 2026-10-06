//! Stage 3b-ter: the one-tap "trust this box" offer (`onboarding.md`
//! § 3b-ter) — the one ratified survivor of the retired claim-time trust
//! question (`storage-modes.md` § What replaced each piece of the axis).
//!
//! ui.yaml `onboarding.trust_prompt` elements: `trust-box-summary`,
//! `trust-box-grant-button`, `trust-box-skip-button` (+ `error-message`).
//!
//! **No Back button** — like the NAT page before it, the admin is already
//! server-committed by the time this shows; grant and skip are its only exits
//! and both conclude the wizard identically.
//!
//! The screen **asks only.** Minting the default set needs an authenticated
//! session and the nest's content-processor roster, neither of which the
//! wizard holds, so the answer is latched (`grant_default_trust` →
//! `take_trust_prompt_granted`) and the mint runs at the signed-in handoff —
//! the same deferral `recovery_kit` uses for registration + escrow. tui only
//! reaches this page because its glue declared `set_renders_trust_prompt(true)`
//! (the first app to).

use fauna_i18n::strings::onboarding::trust_prompt as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    // The summary rides the page's own `trust-box-summary` element below,
    // where a driver can read it — the `recovery_kit` shape.
    Vec::new()
}

pub fn elements(_w: &Wizard) -> Vec<Element> {
    vec![
        Element::label(ids::TRUST_BOX_SUMMARY, t::SUMMARY),
        Element::button(
            ids::TRUST_BOX_GRANT_BUTTON,
            t::GRANT_BUTTON,
            true,
            Action::GrantDefaultTrust,
        ),
        Element::button(
            ids::TRUST_BOX_SKIP_BUTTON,
            t::SKIP_BUTTON,
            true,
            Action::SkipTrustPrompt,
        ),
    ]
}
