//! Stage 1b: show the generated secret (`onboarding.md` § 1. Identity).
//!
//! ui.yaml `onboarding.identity_created` elements: `secret-key-display`,
//! `secret-key-copy-btn`, `identity-continue-button`,
//! `identity-created-back-button`.
//!
//! Continue calls `confirm_generated_identity()`, whose return is the durable
//! commit point (`onboarding.md` § 1 Identity) — `wizard::run_action` writes it
//! to the long-term store right there, moment 1 of the two-moment contract
//! (`long-term-store.md`). It does NOT wait for a nest: the slots are
//! independent by design, and `secret_key` present with `node_url` empty is
//! exactly the launch router's case 2, which resumes this wizard at
//! `HandleEntry` after a force-quit instead of destroying a secret that exists
//! nowhere else. `handle_wizard_done` completes the trio at moment 2.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::identity_created as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESC.to_string(), t::WARNING.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let secret = w
        .machine
        .generated_secret()
        .unwrap_or_else(|| t::NOT_GENERATED.to_string());
    let generated = w.machine.generated_secret().is_some();
    vec![
        Element::label(ids::SECRET_KEY_DISPLAY, secret),
        Element::button(
            ids::SECRET_KEY_COPY_BTN,
            common::COPY,
            generated,
            Action::CopySecret,
        ),
        Element::button(
            ids::IDENTITY_CONTINUE_BUTTON,
            t::CONTINUE,
            generated,
            Action::ConfirmGeneratedIdentity,
        ),
        Element::button(
            ids::IDENTITY_CREATED_BACK_BUTTON,
            common::BACK,
            true,
            Action::Back,
        ),
    ]
}
