//! `sessions-lockout-button` on the signed-out door — the `emergency_lockout_entry`
//! step (`docs/goal/ui/sessions.md` § Layout & flow → *The signed-out door*;
//! behavior owner `devices.md` § The signed-out door).
//!
//! These drive `OnboardingMachine::submit_emergency_lockout` over `FakeNestApi`
//! and pin the machine's own decisions: which nest the ceremony dials, what
//! reaches the wire, where each refusal leaves the user, and — the door's whole
//! contract — that **nothing is kept**: no identity slot is filled and the
//! wizard is back on `identity_choice` after a lock. The wire ceremony is proven
//! in `libs/fauna-client-recovery/tests/ceremonies.rs`.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::{FakeNestApi, LockAccountError, NestApi};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    EmergencyLockoutOutcome, OnboardingMachine, OnboardingObserver, OnboardingStep,
};

fn machine_with_fake() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    m.begin_emergency_lockout_entry();
    (m, fake)
}

fn secret_hex() -> String {
    "ab".repeat(32)
}

#[tokio::test]
async fn the_door_is_reached_from_identity_choice_and_back_returns_there() {
    let (m, _) = machine_with_fake();
    assert_eq!(m.step(), OnboardingStep::EmergencyLockoutEntry);
    m.back();
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
}

#[tokio::test]
async fn a_lock_dials_the_handles_nest_with_the_secret_and_keeps_nothing() {
    let (m, fake) = machine_with_fake();
    fake.set_lock_account_response(Ok(1_900_000_000));
    m.set_current_handle("alice@fauna.test".into());

    let outcome = m
        .submit_emergency_lockout(secret_hex(), "LOCK".into())
        .await;

    assert_eq!(
        outcome,
        EmergencyLockoutOutcome::Locked {
            locked_until: 1_900_000_000
        }
    );
    assert_eq!(
        fake.lock_account_args(),
        vec![("https://fauna.test".to_string(), secret_hex())],
        "the nest comes from the handle's domain, the secret from the field"
    );
    // The door persists nothing: no identity slot holds the pasted secret, no
    // handoff material is parked, and the wizard is back where it started.
    assert_eq!(m.effective_secret(), None);
    assert_eq!(m.generated_secret(), None);
    assert!(m.take_pending_recovery_secret().is_none());
    assert_eq!(m.step(), OnboardingStep::IdentityChoice);
    assert_eq!(m.current_handle(), "", "the typed account is dropped too");
}

#[tokio::test]
async fn the_identity_qr_form_is_accepted_and_names_the_nest_when_nothing_is_typed() {
    let (m, fake) = machine_with_fake();
    fake.set_lock_account_response(Ok(1));

    let uri = format!(
        "fauna://identity?secret={}&handle=alice@fauna.test",
        secret_hex()
    );
    let outcome = m.submit_emergency_lockout(uri, "LOCK".into()).await;

    assert!(matches!(outcome, EmergencyLockoutOutcome::Locked { .. }));
    assert_eq!(fake.lock_account_args()[0].0, "https://fauna.test");
}

#[tokio::test]
async fn the_wrong_confirm_word_sends_nothing() {
    let (m, fake) = machine_with_fake();
    m.set_current_handle("alice@fauna.test".into());

    for word in ["", "lock", "LOCKED"] {
        assert_eq!(
            m.submit_emergency_lockout(secret_hex(), word.into()).await,
            EmergencyLockoutOutcome::WrongConfirmWord
        );
    }
    assert!(fake.lock_account_args().is_empty());
    assert_eq!(m.step(), OnboardingStep::EmergencyLockoutEntry);
}

#[tokio::test]
async fn a_recovery_kit_is_not_a_secret_key() {
    let (m, fake) = machine_with_fake();
    m.set_current_handle("alice@fauna.test".into());

    let kit = fauna_core::recovery::RecoveryKitQr::to_uri(&secret_hex(), None, None);
    for pasted in [kit, "not a key".to_string(), String::new()] {
        assert_eq!(
            m.submit_emergency_lockout(pasted, "LOCK".into()).await,
            EmergencyLockoutOutcome::InvalidSecret
        );
    }
    assert!(fake.lock_account_args().is_empty());
}

#[tokio::test]
async fn a_missing_or_domainless_handle_is_refused_locally() {
    let (m, fake) = machine_with_fake();

    assert_eq!(
        m.submit_emergency_lockout(secret_hex(), "LOCK".into())
            .await,
        EmergencyLockoutOutcome::AccountNeeded
    );
    m.set_current_handle("alice".into());
    assert_eq!(
        m.submit_emergency_lockout(secret_hex(), "LOCK".into())
            .await,
        EmergencyLockoutOutcome::AccountMalformed
    );
    assert!(fake.lock_account_args().is_empty());
}

#[tokio::test]
async fn each_nest_refusal_maps_to_the_outcome_the_user_acts_on() {
    for (err, expect) in [
        (
            LockAccountError::Superseded {
                successor: "ff".repeat(32),
            },
            EmergencyLockoutOutcome::Superseded,
        ),
        (
            LockAccountError::Refused {
                reason: "no".into(),
            },
            EmergencyLockoutOutcome::Refused {
                reason: "no".into(),
            },
        ),
        (
            LockAccountError::Transient {
                reason: "down".into(),
            },
            EmergencyLockoutOutcome::Unreachable {
                reason: "down".into(),
            },
        ),
        (
            LockAccountError::IdentityMismatch {
                reason: "wrong box".into(),
            },
            EmergencyLockoutOutcome::Refused {
                reason: "wrong box".into(),
            },
        ),
    ] {
        let (m, fake) = machine_with_fake();
        fake.set_lock_account_response(Err(err));
        m.set_current_handle("alice@fauna.test".into());

        assert_eq!(
            m.submit_emergency_lockout(secret_hex(), "LOCK".into())
                .await,
            expect
        );
        assert_eq!(
            m.step(),
            OnboardingStep::EmergencyLockoutEntry,
            "a refusal leaves the user on the door to retry or leave"
        );
        assert_eq!(m.effective_secret(), None);
    }
}

#[tokio::test]
async fn the_nest_override_wins_over_the_resolved_url() {
    let (m, fake) = machine_with_fake();
    fake.set_lock_account_response(Ok(1));
    let mut urls = std::collections::HashMap::new();
    urls.insert("nest".to_string(), "http://127.0.0.1:9123".to_string());
    m.set_provider_base_urls(urls);
    m.set_current_handle("alice@fauna.test".into());

    m.submit_emergency_lockout(secret_hex(), "LOCK".into())
        .await;

    assert_eq!(fake.lock_account_args()[0].0, "http://127.0.0.1:9123");
}

/// The UniFFI door the FFI apps render through is the method, verbatim —
/// including the `Locked` unlock time the client renders itself.
#[test]
fn the_free_function_is_the_methods_table() {
    let outcomes = [
        EmergencyLockoutOutcome::Locked { locked_until: 7 },
        EmergencyLockoutOutcome::WrongConfirmWord,
        EmergencyLockoutOutcome::InvalidSecret,
        EmergencyLockoutOutcome::Refused {
            reason: "no".into(),
        },
    ];
    for o in outcomes {
        assert_eq!(
            fauna_onboarding_machine::emergency_lockout_outcome_message(o.clone(), "noon".into()),
            o.message("noon")
        );
    }
}
