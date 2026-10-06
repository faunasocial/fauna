//! Stage 1d: the phrase-only identity restore (`onboarding.md` § 1 Identity).
//!
//! ui.yaml `onboarding.recovery_entry` elements: `recovery-entry-phrase-field`,
//! `recovery-entry-submit-button`, `recovery-entry-back-button`;
//! `recovery-entry-account-field` optional (needed when the payload names no
//! account). `qr-camera-view` is scoped to clients with a camera — a terminal
//! has none.
//!
//! Reached from `identity_choice`'s `restore-from-recovery-kit-button`. Submit
//! runs `OnboardingMachine::submit_recovery_entry` — the pre-identity escrow
//! restore — and on success lands on `handle_entry` holding the recovered
//! seed, exactly where an import lands.
//!
//! The account field asks for a **handle** (`user@domain`), not an actor id:
//! the ceremony has no session to ask where the account lives, so what follows
//! the `@` is the only thing that can locate the home nest. A Settings-minted
//! kit carries its own `handle=` and the field can stay empty; the
//! onboarding-minted kit and a hand-typed 64-hex code do not, and the field is
//! what supplies it.
//!
//! What follows the `@` may equally be the nest's **own address** —
//! `alice@192.0.2.10`, `alice@nest.local`, `alice@[2001:db8::5]:8443` — the
//! locator of last resort (`onboarding.md` § 1 Identity → `recovery_entry`),
//! for the case recovery was built for: the domain is dead while the escrow
//! blob, the actor and the nest are all intact. The field's two jobs split —
//! the address part *locates* (probed verbatim through the shared
//! classification), the handle part *names* (the bare handle is what actor
//! resolution gets). `t::ACCOUNT_HINT` is what tells the user so; the machine
//! has accepted both forms since the ceremony landed.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::recovery_entry as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Field, Wizard, WizardField};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESC.to_string(), t::ACCOUNT_HINT.to_string()]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let phrase = w.field(WizardField::RecoveryPhrase);
    let account = w.field(WizardField::RecoveryAccount);
    vec![
        // Both labelled — an unlabelled input prompts with its raw element id
        // (`apps/tui.md` § Rendering → *Control vocabulary*: every input MUST
        // carry a label). The account field takes the shared `common::HANDLE`
        // rather than a page-local twin: it is the same field concept as
        // `handle_entry`'s (priority #3).
        Element::input(
            ids::RECOVERY_ENTRY_PHRASE_FIELD,
            phrase,
            Field::Wizard(WizardField::RecoveryPhrase),
        )
        .labelled(t::PHRASE_LABEL),
        Element::input(
            ids::RECOVERY_ENTRY_ACCOUNT_FIELD,
            account,
            Field::Wizard(WizardField::RecoveryAccount),
        )
        .labelled(common::HANDLE),
        // Enabled unconditionally: every way the input can be wrong is an
        // answer the ceremony gives on `error-message` (an unparseable phrase
        // and a missing account are refused locally, before anything is sent).
        // A disabled submit would make "why can't I click this?" the user's
        // problem instead of telling them.
        Element::button(
            ids::RECOVERY_ENTRY_SUBMIT_BUTTON,
            t::SUBMIT,
            true,
            Action::SubmitRecoveryEntry,
        ),
        Element::button(
            ids::RECOVERY_ENTRY_BACK_BUTTON,
            common::BACK,
            true,
            Action::Back,
        ),
    ]
}
