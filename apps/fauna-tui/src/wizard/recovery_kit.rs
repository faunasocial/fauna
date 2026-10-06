//! Stage 1b-bis: the recovery-kit offer (`onboarding.md` § 1 Identity).
//!
//! ui.yaml `onboarding.recovery_kit` elements: `recovery-kit-description`,
//! `recovery-kit-secret-display`, `recovery-kit-secret-copy-btn`,
//! `recovery-kit-qr`, `recovery-kit-escrow-status`,
//! `recovery-kit-confirm-button`, `recovery-kit-skip-button`.
//!
//! The screen **mints and displays only** — no nest exists at this position,
//! so registration + escrow run at the wizard's signed-in handoff
//! (`identity-succession.md` § The RecoveryKey → *Creation UX*, ratified
//! 2026-08-01). `recovery-kit-escrow-status` therefore renders exactly one
//! state here: the deferred line — never an `EscrowOutcome`, never
//! "protected". tui only reaches this page because its glue declared
//! `set_renders_recovery_kit(true)` (the first app to).

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::recovery_kit as t;
use fauna_ui_ids as ids;

use super::{Action, Element, Wizard};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    // The "why offline" copy also rides the page's own
    // `recovery-kit-description` element below, where a driver can read it.
    Vec::new()
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let secret = w.machine.recovery_kit_secret_hex();
    let minted = secret.is_some();
    let mut els = vec![
        Element::label(ids::RECOVERY_KIT_DESCRIPTION, t::DESC),
        Element::label(
            ids::RECOVERY_KIT_SECRET_DISPLAY,
            secret.clone().unwrap_or_else(|| t::NOT_MINTED.to_string()),
        ),
        Element::button(
            ids::RECOVERY_KIT_SECRET_COPY_BTN,
            common::COPY,
            minted,
            Action::CopyRecoveryKitSecret,
        ),
    ];
    if let Some(matrix) =
        kit_uri(&w.machine).and_then(|uri| fauna_core::qr_matrix::qr_matrix(&uri).ok())
    {
        // Pinned dark-on-light for the identity-export QR's reason — a
        // theme-inverted QR does not scan.
        els.push(
            Element::label(ids::RECOVERY_KIT_QR, crate::settings::render_qr(&matrix))
                .colors([0, 0, 0], [255, 255, 255]),
        );
    }
    els.push(Element::label(
        ids::RECOVERY_KIT_ESCROW_STATUS,
        t::ESCROW_DEFERRED,
    ));
    els.push(Element::button(
        ids::RECOVERY_KIT_CONFIRM_BUTTON,
        t::CONFIRM,
        minted,
        Action::ConfirmRecoveryKit,
    ));
    els.push(Element::button(
        ids::RECOVERY_KIT_SKIP_BUTTON,
        t::SKIP,
        true,
        Action::SkipRecoveryKit,
    ));
    els
}

/// The kit's `fauna://recovery` URI (the deliberate twin of `fauna://identity`
/// on its own host), carrying the actor id when the identity secret can derive
/// it. No handle rides here — none is chosen yet at this position; a restore
/// from this kit asks for the account instead (`recovery-entry-account-field`).
///
/// **One builder for both the QR and the copy button** (ruling 2026-08-02, the
/// piece-4 close): the copy used to put the bare 64-hex on the clipboard, so
/// the two encodings of the same artifact carried different amounts of truth —
/// a scanned kit restored knowing its account, a copied one did not, and
/// copying is overwhelmingly what a terminal user does. `parse_kit` accepts the
/// URI and bare hex alike, so nothing that ever restored stops restoring; a
/// user who writes down the *displayed* 64-hex is on the same path as before.
/// See `identity-succession.md` § The RecoveryKey — *Kit payload*.
pub(super) fn kit_uri(machine: &fauna_onboarding_machine::OnboardingMachine) -> Option<String> {
    // The shared builder every app's kit screen calls (the handle is `None`,
    // truthfully: none is chosen yet at this position).
    machine.recovery_kit_uri()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One payload behind both the QR and the copy button — the ruling that
    /// closed the mismatch where a scanned kit knew its account and a copied
    /// one did not (2026-08-02).
    ///
    /// The handle half is deliberately absent *here*: at this screen's ratified
    /// position no handle is chosen yet, so the kit says which account and not
    /// where it lives, and a restore from it asks
    /// (`recovery-entry-account-field`). Settings, which knows both, carries
    /// both — pinned by
    /// `settings::tests::the_copy_button_carries_the_uri_so_a_restore_needs_nothing_typed`.
    #[test]
    fn the_kit_uri_names_the_account_and_truthfully_omits_the_handle() {
        let app = crate::app::tests::test_app();
        app.wizard.machine.begin_create_identity();
        app.wizard
            .machine
            .confirm_generated_identity()
            .expect("a generated secret exists after begin_create_identity");

        let uri = kit_uri(&app.wizard.machine).expect("the kit screen has minted a root");
        let parsed = fauna_client_recovery::parse_kit(&uri)
            .expect("the copied payload is a kit the shared grammar accepts");

        let expected_actor = app
            .wizard
            .machine
            .generated_secret()
            .and_then(|s| fauna_core::identity::ActorKeypair::from_secret_hex(&s).ok())
            .map(|kp| kp.actor_id_hex())
            .expect("the identity was just created");
        assert_eq!(
            parsed.actor_id.map(|a| a.to_hex()).as_deref(),
            Some(expected_actor.as_str()),
            "the kit names the account it protects — the escrow blob is \
             AAD-bound to that id"
        );
        assert_eq!(
            parsed.handle, None,
            "no handle is chosen at this position, and claiming one would be a lie"
        );
        assert_eq!(
            parsed.recovery.to_hex().as_str(),
            app.wizard
                .machine
                .recovery_kit_secret_hex()
                .expect("minted")
                .as_str(),
            "the copied payload carries the SAME root the screen displays"
        );
    }
}
