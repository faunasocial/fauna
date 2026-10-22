//! `handle_check_snapshot().terms_notice_visible` — the by-continuing terms
//! notice on `handle_entry` (`terms-and-content-guidelines.md` § The decisions,
//! item 3 (iv); `onboarding.md` § 2). The apps paint the notice from this field
//! and never compare the outcome, so the getter derives it on every read: a
//! transition that forgets to set it cannot leave it stale.

use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::state::LocalizedText;
use fauna_onboarding_machine::{
    HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot, OnboardingMachine,
};

fn machine() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

#[test]
fn a_sign_in_outcome_shows_the_notice() {
    let m = machine();
    assert!(!m.handle_check_snapshot().terms_notice_visible);
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::AlreadyOnNest {
        handle_matches: true,
        current_handle: Some("alice@example.com".into()),
    });
    assert!(m.handle_check_snapshot().terms_notice_visible);
}

#[test]
fn an_outcome_leading_to_another_notice_page_hides_it() {
    let m = machine();
    for outcome in [
        HandleCheckOutcome::NestRunningUserUnregistered,
        HandleCheckOutcome::UnregisteredUnclaimedNest,
    ] {
        m.set_handle_check_outcome_for_test(outcome.clone());
        assert!(
            !m.handle_check_snapshot().terms_notice_visible,
            "{outcome:?}"
        );
    }
}

/// The stored flag is never trusted: an injected snapshot claiming the notice
/// over an outcome that does not sign in reads back hidden, and the reverse.
#[test]
fn the_getter_derives_the_field_from_the_outcome() {
    let m = machine();
    let snap = |outcome, stored| HandleCheckSnapshot {
        phase: HandleCheckPhase::Complete,
        outcome,
        message: LocalizedText {
            key: "x".into(),
            args: Default::default(),
        },
        continue_enabled: true,
        control_checkbox_visible: false,
        control_checkbox_checked: false,
        terms_notice_visible: stored,
    };
    m.set_handle_check_snapshot_for_test(snap(
        HandleCheckOutcome::NestRunningUserUnregistered,
        true,
    ));
    assert!(!m.handle_check_snapshot().terms_notice_visible);
    m.set_handle_check_snapshot_for_test(snap(
        HandleCheckOutcome::AlreadyOnNest {
            handle_matches: false,
            current_handle: None,
        },
        false,
    ));
    assert!(m.handle_check_snapshot().terms_notice_visible);
}
