use fauna_onboarding_machine::{
    OnboardingMachine, OnboardingObserver, OnboardingStep, observer::CountingObserver,
};
use std::sync::Arc;

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = CountingObserver::new();
    OnboardingMachine::new(observer)
}

#[test]
fn confirm_generated_identity_returns_secret() {
    let m = machine();
    m.begin_create_identity();
    let secret = m
        .confirm_generated_identity()
        .expect("confirm_generated_identity must succeed after begin_create_identity");
    // 32 bytes hex-encoded = 64 chars; must match the wizard's snapshot of generated_secret().
    assert_eq!(secret.len(), 64);
    assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(m.generated_secret().as_deref(), Some(secret.as_str()));
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
}

#[test]
fn confirm_imported_identity_validates_and_echoes() {
    let m = machine();
    m.begin_import_identity();

    // Bad input — too short — must error.
    let err = m.confirm_imported_identity("deadbeef".into());
    assert!(err.is_err(), "short hex must be rejected");

    // Bad input — non-hex chars — must error.
    let err = m.confirm_imported_identity("z".repeat(64));
    assert!(err.is_err(), "non-hex chars must be rejected");

    // Good input — 64 ASCII-hex — returns the same string and advances.
    let valid = "a".repeat(64);
    let echoed = m
        .confirm_imported_identity(valid.clone())
        .expect("64-char hex must validate");
    assert_eq!(echoed, valid);
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert_eq!(m.effective_secret().as_deref(), Some(valid.as_str()));
}

#[test]
fn seed_identity_jumps_to_handle_entry() {
    let m = machine();
    let secret = "b".repeat(64);
    m.seed_identity(secret.clone());

    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    // seed_identity stores into imported_secret slot; effective_secret prefers it.
    assert_eq!(m.effective_secret().as_deref(), Some(secret.as_str()));
    // Origin is intentionally None so Back from HandleEntry routes to
    // IdentityChoice — the seeded user never visited Created/Import in
    // this session, so routing them there would show an empty form.
    assert_eq!(m.identity_origin(), None);
}

#[test]
fn back_from_seeded_handle_entry_routes_to_identity_choice() {
    let m = machine();
    m.seed_identity("c".repeat(64));
    assert_eq!(m.step(), OnboardingStep::HandleEntry);

    m.back();

    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
}

#[test]
fn reset_clears_in_memory_state() {
    let m = machine();
    m.begin_create_identity();
    let _ = m.confirm_generated_identity().unwrap();
    m.set_current_handle("alice@example.com".into());

    m.reset();

    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
    assert_eq!(m.current_handle(), "");
    assert!(m.generated_secret().is_none());
    assert!(m.effective_secret().is_none());
}
