//! A confirm whose moment-1 write failed does not stay advanced.
//!
//! `docs/goal/architecture/apps/common.md` § Credential storage: a keyring
//! failure in a shipped build "is a loud error, not a silent fallback". The
//! confirm transitions advance the wizard before the app writes the secret
//! (the machine holds no store), so an app whose write failed has to be able
//! to put the wizard back on the screen the person confirmed on, with the
//! error on the shared `error-message` channel — otherwise the person reaches
//! handle entry holding a key that is stored nowhere, and the next launch is
//! back at identity choice with that key lost.

use fauna_onboarding_machine::{
    OnboardingMachine, OnboardingObserver, OnboardingStep, observer::CountingObserver,
};
use std::sync::Arc;

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = CountingObserver::new();
    OnboardingMachine::new(observer)
}

#[test]
fn a_failed_commit_of_a_created_identity_returns_to_its_screen_with_the_same_key() {
    let m = machine();
    m.set_renders_recovery_kit(true);
    m.begin_create_identity();
    let secret = m.confirm_generated_identity().expect("confirm");
    assert_eq!(m.step(), OnboardingStep::RecoveryKit);

    m.identity_commit_failed("the keyring is unavailable".into());

    assert_eq!(m.step(), OnboardingStep::IdentityCreated);
    assert_eq!(
        m.error_message().as_deref(),
        Some("the keyring is unavailable")
    );
    // The mint-once rule still holds: the key on screen is the one the person
    // was told to write down.
    assert_eq!(m.generated_secret().as_deref(), Some(secret.as_str()));

    // Retry: a successful commit clears the error and advances again.
    let again = m.confirm_generated_identity().expect("retry confirm");
    assert_eq!(again, secret);
    assert_eq!(m.step(), OnboardingStep::RecoveryKit);
    assert_eq!(m.error_message(), None);
}

#[test]
fn a_failed_commit_of_an_imported_identity_returns_to_the_import_screen() {
    let m = machine();
    m.begin_import_identity();
    m.confirm_imported_identity("a".repeat(64))
        .expect("confirm");
    assert_eq!(m.step(), OnboardingStep::HandleEntry);

    m.identity_commit_failed("not saved".into());

    assert_eq!(m.step(), OnboardingStep::IdentityImport);
    assert_eq!(m.error_message().as_deref(), Some("not saved"));
}

#[test]
fn a_failure_reported_after_the_wizard_moved_on_moves_nothing() {
    let m = machine();
    m.begin_import_identity();
    m.confirm_imported_identity("a".repeat(64))
        .expect("confirm");
    m.identity_commit_failed("first".into());
    // The commit was consumed by the first report: a second, late one has no
    // confirm to undo and only paints its message.
    m.identity_commit_failed("late".into());
    assert_eq!(m.step(), OnboardingStep::IdentityImport);
    assert_eq!(m.error_message().as_deref(), Some("late"));

    // A report with no confirm behind it at all leaves the step alone.
    let fresh = machine();
    fresh.identity_commit_failed("nothing to undo".into());
    assert_eq!(fresh.step(), OnboardingStep::IdentityChoice);
}
