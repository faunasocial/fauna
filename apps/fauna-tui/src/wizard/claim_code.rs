//! Stage 3a: one-time claim code for an unclaimed nest (`onboarding.md` § 3a).
//!
//! ui.yaml `onboarding.claim_code` elements: `claim-code-input`,
//! `claim-code-submit-button`, `claim-code-status`, `claim-code-back-button`.
//! There is no Continue button — a successful Submit is terminal and the
//! machine advances on its own.
//!
//! `submit_enabled` on the snapshot is the canonical gate (it encodes both
//! "not in flight" and the machine's own rules); the client adds nothing. The
//! code is passed to `wizard_submit_claim_code` **verbatim** — no length or
//! charset filter, since the nest normalizes on both sides (`onboarding.md:93`).

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::claim_code as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Field, Wizard, WizardField, localized};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESCRIPTION.to_string(), t::PLACEHOLDER.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let snap = w.machine.claim_code_snapshot();
    vec![
        // Labelled: an unlabelled input prompts with its raw element id
        // (`apps/tui.md` § Rendering → *Control vocabulary*).
        Element::input(
            ids::CLAIM_CODE_INPUT,
            w.field(WizardField::ClaimCode),
            Field::Wizard(WizardField::ClaimCode),
        )
        .labelled(t::LABEL),
        Element::button(
            ids::CLAIM_CODE_SUBMIT_BUTTON,
            t::SUBMIT_BUTTON,
            snap.submit_enabled,
            Action::SubmitClaimCode,
        ),
        Element::label(ids::CLAIM_CODE_STATUS, localized(&snap.message)),
        Element::button(
            ids::CLAIM_CODE_BACK_BUTTON,
            common::BACK,
            true,
            Action::Back,
        ),
    ]
}
