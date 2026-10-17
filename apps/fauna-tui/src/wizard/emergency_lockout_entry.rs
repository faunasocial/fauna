//! The signed-out lock door (`ui/sessions.md` § Layout & flow → *The signed-out
//! door*; behavior `devices.md` § The signed-out door).
//!
//! ui.yaml `onboarding.emergency_lockout_entry` elements: `paste-secret-field`,
//! `recovery-entry-account-field`, `sessions-lockout-warning`,
//! `sessions-lockout-confirm-field`, `sessions-lockout-button`,
//! `emergency-lockout-back-button`.
//!
//! Reached from `identity_choice`'s `emergency-lockout-entry-button`. Submit
//! runs `OnboardingMachine::submit_emergency_lockout` — the seed-signed
//! `fauna.account.lockout` over one anonymous connection — and **persists
//! nothing**: the three buffers below are wizard-memory only, scrubbed the
//! moment the wizard leaves this step (`Wizard::prepare`), and a lock returns
//! to `identity_choice` with the confirmation line on `error-message`.
//!
//! The lock warning and the `LOCK` prompt are the shared `sessions.*` strings,
//! the same text the signed-in Sessions page paints — one copy of the REQUIRED
//! warning (`devices.md` § The two panic buttons).

use fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD;
use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::emergency_lockout as t;
use fauna_i18n::strings::sessions as s;
use fauna_ui_ids as ids;

use super::{Action, Element, Field, Wizard, WizardField};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESC.to_string(), t::ACCOUNT_HINT.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let confirm = w.field(WizardField::LockoutConfirm);
    // The word gates the button here AND in the machine — a wrong word is
    // refused identically on all seven apps (`submit_emergency_lockout`).
    let armed = confirm == LOCKOUT_CONFIRM_WORD;
    vec![
        Element::input(
            ids::PASTE_SECRET_FIELD,
            w.field(WizardField::LockoutSecret),
            Field::Wizard(WizardField::LockoutSecret),
        )
        .labelled(t::SECRET_LABEL),
        Element::input(
            ids::RECOVERY_ENTRY_ACCOUNT_FIELD,
            w.field(WizardField::LockoutAccount),
            Field::Wizard(WizardField::LockoutAccount),
        )
        .labelled(common::HANDLE),
        Element::label(ids::SESSIONS_LOCKOUT_WARNING, s::LOCKOUT_WARNING),
        Element::input(
            ids::SESSIONS_LOCKOUT_CONFIRM_FIELD,
            confirm.clone(),
            Field::Wizard(WizardField::LockoutConfirm),
        )
        .labelled(s::LOCKOUT_CONFIRM_PLACEHOLDER),
        Element::button(
            ids::SESSIONS_LOCKOUT_BUTTON,
            s::LOCKOUT_BUTTON,
            armed,
            Action::SubmitEmergencyLockout {
                confirm_word: confirm,
            },
        ),
        Element::button(
            ids::EMERGENCY_LOCKOUT_BACK_BUTTON,
            common::BACK,
            true,
            Action::Back,
        ),
    ]
}
