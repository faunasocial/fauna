//! Stage 1a: create or import an identity (`onboarding.md` § 1. Identity).
//!
//! ui.yaml `onboarding.identity_choice` elements: `create-identity-button`,
//! `import-identity-button`, plus the optional `recover-lost-box-button` — the
//! fresh-client entry to box recovery (`box-recovery.md` § Recovery UI (step 4)):
//! the admin's box is gone and they are re-onboarding on a new device, so the
//! third way out of this page is "recover the box I lost" — and
//! `restore-from-recovery-kit-button`, which restores a lost *identity* from
//! its recovery phrase. The two recovery entries sit adjacent and mean
//! different things, so `onboarding.md` § 1 Identity carries a standing warning
//! that their labels must differentiate sharply; the strings do
//! (`recover_lost_box` names the *box*, `restore_from_recovery_kit` the
//! *account*).

use fauna_i18n::strings::onboarding::identity_choice as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::SUBTITLE.to_string()]
}

pub fn elements(_w: &Wizard) -> Vec<Element> {
    vec![
        Element::button(
            ids::CREATE_IDENTITY_BUTTON,
            t::CREATE_NEW,
            true,
            Action::BeginCreateIdentity,
        ),
        Element::button(
            ids::IMPORT_IDENTITY_BUTTON,
            t::IMPORT_EXISTING,
            true,
            Action::BeginImportIdentity,
        ),
        // Recovery intent. Routes through `identity_import` first (Q2-A — the
        // identity is what unseals the account plane), then the normal nest-connect
        // step; landing on `nest_recovery` once a surviving nest the admin owns
        // resolves as already-owned.
        Element::button(
            ids::RECOVER_LOST_BOX_BUTTON,
            t::RECOVER_LOST_BOX,
            true,
            Action::BeginRecoverLostBox,
        ),
        // The other recovery: the identity itself, from the recovery phrase.
        // Routes to `recovery_entry`, whose submit runs the pre-identity escrow
        // restore and lands on `handle_entry` like an import.
        Element::button(
            ids::RESTORE_FROM_RECOVERY_KIT_BUTTON,
            t::RESTORE_FROM_RECOVERY_KIT,
            true,
            Action::BeginRecoveryEntry,
        ),
    ]
}

/// The optional `sign-out-residue` view — what the last sign-out's erase could
/// not remove, with Remove Again beside it (`account-scoping.md` § Erasure
/// follows scope → *the residue surface*).
///
/// Not part of [`elements`] because its state is the app's, not the wizard's:
/// the residue belongs to the device, not to an onboarding in progress, and its
/// retry drives no machine mutator. The app appends it on this page alone
/// (`App::page_elements_ungated`), exactly while a residue owes work.
pub fn residue_elements(residue: &crate::account_scope::ResidueSurface) -> Vec<Element> {
    vec![
        // The container: registered so a driver can ask whether the view is
        // there at all, painting nothing of its own.
        Element::label(ids::SIGN_OUT_RESIDUE, ""),
        Element::label(ids::SIGN_OUT_RESIDUE_MESSAGE, residue.line.clone())
            .within(ids::SIGN_OUT_RESIDUE, 0),
        Element::gesture_button(
            ids::SIGN_OUT_RESIDUE_RETRY_BUTTON,
            fauna_i18n::strings::settings::SIGN_OUT_RESIDUE_RETRY,
            true,
            crate::element::Gesture::RetrySignOutResidue,
        )
        .within(ids::SIGN_OUT_RESIDUE, 0),
    ]
}
