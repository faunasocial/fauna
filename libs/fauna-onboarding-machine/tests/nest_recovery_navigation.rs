//! Total-box-loss recovery wizard branch — navigation + transitions.
//!
//! Per `docs/goal/architecture/nest/box-recovery.md` § Recovery UI (step 4) —
//! approved shape (user decision 2026-07-01) and `tests/e2e-unified/ui.yaml`
//! pages `nest_recovery` / `recover_selfhosted_instructions`. These are the
//! shared-Rust transitions every app's recovery UI drives; the seed
//! resolution + recovery-mode re-provision drive are a separate slice (C2).

use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{BoxRecoveryEntry, OnboardingMachine, OnboardingStep};

fn machine() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

/// A valid 64-hex Ed25519 secret for `confirm_imported_identity` /
/// `seed_identity_for_recovery` (content is irrelevant to navigation).
const SECRET_HEX: &str = "11111111111111111111111111111111111111111111111111111111111111ab";

// ── Fresh-client entry: identity_choice → recover-lost-box-button ───────────

/// `recover-lost-box-button` on `identity_choice` routes through
/// `identity_import` first (recovery intent) so the identity is loaded to
/// read the custody, then lands on `nest_recovery`.
#[test]
fn begin_recover_lost_box_routes_to_identity_import_with_recovery_intent() {
    let m = machine();
    m.begin_recover_lost_box();

    assert_eq!(m.step(), OnboardingStep::IdentityImport);
    assert!(m.recovery_intent());
    assert_eq!(m.recovery_came_from(), Some(BoxRecoveryEntry::Identity));
}

/// After the identity is imported under recovery intent, the wizard lands on
/// `handle_entry` — same as a normal import (Q2-A, box-recovery.md § Recovery
/// UI): the admin must still connect to a SURVIVING nest before the box list
/// is readable, so recovery routes through the same nest-connect step.
#[test]
fn import_under_recovery_intent_lands_on_handle_entry() {
    let m = machine();
    m.begin_recover_lost_box();

    let out = m.confirm_imported_identity(SECRET_HEX.into());
    assert!(out.is_ok());
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert!(m.recovery_intent());
}

/// Once on `handle_entry` under recovery intent, resolving the typed
/// handle/@domain to a nest the admin already owns (`AlreadyOnNest` — the
/// admin runs this box too) lands on `nest_recovery`, NOT `Done`/`LoggedIn`:
/// reachability + owning the identity is all `deploymentSeeds()` needs, not
/// registering on this nest (Q2-A).
#[tokio::test]
async fn handle_entry_already_on_nest_under_recovery_intent_lands_on_nest_recovery() {
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};

    let m = machine();
    m.begin_recover_lost_box();
    m.confirm_imported_identity(SECRET_HEX.into()).unwrap();
    assert_eq!(m.step(), OnboardingStep::HandleEntry);

    m.set_current_handle("alice@example.com".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::AlreadyOnNest {
            handle_matches: true,
            current_handle: Some("alice@example.com".into()),
        },
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });

    let next = m.submit_handle_check_continue().await;
    assert_eq!(next, OnboardingStep::NestRecovery);
    assert_eq!(m.step(), OnboardingStep::NestRecovery);
    assert!(m.recovery_intent());
    // Recovery never sets the normal login outcome — the admin isn't
    // registering on this nest, just reading its synced config.
    assert!(m.wizard_outcome().is_none());
}

/// A normal (non-recovery) import still lands on `handle_entry`.
#[test]
fn import_without_recovery_intent_lands_on_handle_entry() {
    let m = machine();
    m.begin_import_identity();

    let out = m.confirm_imported_identity(SECRET_HEX.into());
    assert!(out.is_ok());
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert!(!m.recovery_intent());
}

// ── Surviving-device entry: launch_retry → launch-recover-button ────────────

/// `launch-recover-button` on the launch retry surface seeds the identity the
/// client already holds and drops straight into `nest_recovery` (the synced
/// `fauna.state.deployment-seeds` map is guaranteed local on a surviving device).
#[test]
fn seed_identity_for_recovery_lands_on_nest_recovery_from_launch() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());

    assert_eq!(m.step(), OnboardingStep::NestRecovery);
    assert!(m.recovery_intent());
    assert_eq!(m.recovery_came_from(), Some(BoxRecoveryEntry::Launch));
    // The seeded identity is the effective secret the re-provision drive uses.
    assert_eq!(m.effective_secret().as_deref(), Some(SECRET_HEX));
}

// ── Box list (recover-box-item, fetched via deploymentSeeds()) ──────────────

#[test]
fn recovery_boxes_default_empty_then_reflects_the_pushed_list() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());
    assert!(m.recovery_boxes().is_empty());

    m.set_recovery_boxes(vec!["aa00bb11".into(), "cc22dd33".into()]);
    assert_eq!(m.recovery_boxes(), vec!["aa00bb11", "cc22dd33"]);
}

/// A selection that vanishes from a refreshed box list is dropped, so the method
/// buttons can't act on a box no longer present.
#[test]
fn set_recovery_boxes_clears_a_selection_no_longer_present() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());
    m.set_recovery_boxes(vec!["aa00bb11".into(), "cc22dd33".into()]);
    m.select_recovery_box("cc22dd33".into());
    assert_eq!(m.recovery_selected_nest_id().as_deref(), Some("cc22dd33"));

    // Refresh drops cc22dd33 → the selection is cleared.
    m.set_recovery_boxes(vec!["aa00bb11".into()]);
    assert_eq!(m.recovery_selected_nest_id(), None);
}

// ── Box selection + method choice ───────────────────────────────────────────

#[test]
fn select_recovery_box_records_the_nest_actor_id() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());
    assert_eq!(m.recovery_selected_nest_id(), None);

    m.select_recovery_box("aa00bb11".into());
    assert_eq!(m.recovery_selected_nest_id().as_deref(), Some("aa00bb11"));
}

/// The method buttons are enabled only once a box row is selected — the machine
/// refuses to advance without a selection.
#[test]
fn recover_via_cloud_requires_a_selected_box() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());

    assert!(m.recover_via_cloud().is_err());
    assert_eq!(m.step(), OnboardingStep::NestRecovery);

    m.select_recovery_box("aa00bb11".into());
    assert!(m.recover_via_cloud().is_ok());
    assert_eq!(m.step(), OnboardingStep::VpsConfig);
}

#[test]
fn recover_via_selfhosted_requires_a_selected_box() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());

    assert!(m.recover_via_selfhosted().is_err());
    assert_eq!(m.step(), OnboardingStep::NestRecovery);

    m.select_recovery_box("aa00bb11".into());
    assert!(m.recover_via_selfhosted().is_ok());
    assert_eq!(m.step(), OnboardingStep::RecoverSelfhostedInstructions);
}

// ── Back navigation (recover-back-button conditions) ────────────────────────

/// `recover-back-button` with `came-from-identity` returns to `handle_entry`
/// (Q2-A: that's the immediate predecessor now that recovery routes through
/// the nest-connect step, not `identity_import`).
#[tokio::test]
async fn back_from_nest_recovery_identity_entry_returns_to_handle_entry() {
    use fauna_onboarding_machine::state::LocalizedText;
    use fauna_onboarding_machine::{HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot};

    let m = machine();
    m.begin_recover_lost_box();
    m.confirm_imported_identity(SECRET_HEX.into()).unwrap();
    assert_eq!(m.step(), OnboardingStep::HandleEntry);

    m.set_current_handle("alice@example.com".into());
    m.set_handle_check_snapshot_for_test(HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome: HandleCheckOutcome::AlreadyOnNest {
            handle_matches: true,
            current_handle: Some("alice@example.com".into()),
        },
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
    });
    m.submit_handle_check_continue().await;
    assert_eq!(m.step(), OnboardingStep::NestRecovery);

    m.back();
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
}

/// `recover-back-button` with `came-from-launch` is a wizard exit — the launch
/// glue tears the wizard down and re-shows `launch_retry` (not an onboarding
/// step), so the machine stays put and lets the glue own the boundary.
#[test]
fn back_from_nest_recovery_launch_entry_stays_put_for_glue() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());
    assert_eq!(m.step(), OnboardingStep::NestRecovery);

    m.back();
    assert_eq!(m.step(), OnboardingStep::NestRecovery);
    assert_eq!(m.recovery_came_from(), Some(BoxRecoveryEntry::Launch));
}

/// Back from the self-hosted instructions returns to the box-selection hub.
#[test]
fn back_from_selfhosted_instructions_returns_to_nest_recovery() {
    let m = machine();
    m.seed_identity_for_recovery(SECRET_HEX.into());
    m.select_recovery_box("aa00bb11".into());
    m.recover_via_selfhosted().unwrap();
    assert_eq!(m.step(), OnboardingStep::RecoverSelfhostedInstructions);

    m.back();
    assert_eq!(m.step(), OnboardingStep::NestRecovery);
}

/// Backing all the way out of the recovery import to `identity_choice` clears
/// the recovery intent, so a subsequent normal import isn't mis-routed.
#[test]
fn back_out_of_recovery_import_clears_recovery_intent() {
    let m = machine();
    m.begin_recover_lost_box();
    assert_eq!(m.step(), OnboardingStep::IdentityImport);
    assert!(m.recovery_intent());

    m.back();
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
    assert!(!m.recovery_intent());
    assert_eq!(m.recovery_came_from(), None);
}
