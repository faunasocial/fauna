//! Stage 1c: paste an existing secret (`onboarding.md` § 1. Identity).
//!
//! ui.yaml `onboarding.identity_import` elements: `paste-secret-field`,
//! `import-submit-button`, `identity-import-back-button`. `qr-camera-view` is
//! optional and scoped to clients with a camera — a terminal has none.
//!
//! The import-field grammar (bare 64-hex · `fauna://identity?secret=&handle=`
//! · colon form) is the shared `fauna_core::identity_qr` parser, which linux
//! also calls directly as native Rust (`onboarding.md:25`).

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::identity_import as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Field, Wizard, WizardField};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![
        t::PASTE_SUBTITLE.to_string(),
        t::PASTE_PLACEHOLDER.to_string(),
    ]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let typed = w.field(WizardField::ImportSecret);
    vec![
        // Labelled, or the paint falls back to the element id and this field
        // prompts "paste-secret-field: _" (`apps/tui.md` § Rendering → *Control
        // vocabulary*: every input MUST carry a label). `paste_label` is the
        // string the other apps already use for this same field — priority #3.
        Element::input(
            ids::PASTE_SECRET_FIELD,
            typed.clone(),
            Field::Wizard(WizardField::ImportSecret),
        )
        .labelled(t::PASTE_LABEL),
        Element::button(
            ids::IMPORT_SUBMIT_BUTTON,
            t::IMPORT,
            !typed.is_empty(),
            Action::ConfirmImportedIdentity,
        ),
        Element::button(
            ids::IDENTITY_IMPORT_BACK_BUTTON,
            common::BACK,
            true,
            Action::Back,
        ),
    ]
}

/// The localized message for an unparseable paste.
pub fn invalid_secret_message() -> String {
    t::INVALID_SECRET.to_string()
}
