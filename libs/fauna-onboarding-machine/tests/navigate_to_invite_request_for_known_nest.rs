use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    LocalizedText, OnboardingMachine, OnboardingStep,
    snapshots::{InviteRequestSnapshot, InviteRequestState, OobCodeState},
};

fn machine() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

/// Per target docs/goal/behavior/onboarding.md §"App-launch routing": when the
/// silent-challenge handshake reports the secret is unregistered on an
/// otherwise-running nest, drop the user directly on the invite-request
/// page rather than make them re-type their handle on handle_entry.
#[test]
fn lands_on_invite_request_with_handle_and_nest_url_pre_set() {
    let m = machine();
    // Precondition: identity stage (typical post-seedIdentity state).
    m.set_step_for_test(OnboardingStep::HandleEntry);

    m.navigate_to_invite_request_for_known_nest(
        "https://example.test".into(),
        "alice@example.test".into(),
    );

    assert_eq!(m.step(), OnboardingStep::InviteRequest);
    assert_eq!(m.current_handle(), "alice@example.test");
    assert_eq!(m.nest_url(), "https://example.test");
}

#[test]
fn snapshot_resets_to_idle_with_continue_disabled() {
    let m = machine();
    m.navigate_to_invite_request_for_known_nest(
        "https://example.test".into(),
        "alice@example.test".into(),
    );

    let snap = m.invite_request_snapshot();
    assert!(matches!(snap.state, InviteRequestState::Idle));
    assert!(!snap.continue_enabled);
    assert!(!snap.recheck_visible);
}

/// The mutator is meant to overwrite a stale snapshot left over from an
/// earlier wizard pass (e.g. user backed out of invite_request, then
/// silent-challenge dropped them back). A pre-existing snapshot is
/// replaced wholesale.
#[test]
fn overwrites_stale_snapshot_from_previous_attempt() {
    let m = machine();
    m.set_invite_request_snapshot_for_test(InviteRequestSnapshot {
        state: InviteRequestState::Denied {
            reason: "spam".into(),
            request_id: "req-old".into(),
        },
        message: LocalizedText::default(),
        continue_enabled: false,
        recheck_visible: false,
        age_notice: None,
        out_of_band_code_state: OobCodeState::Idle,
        oob_message: LocalizedText::default(),
    });

    m.navigate_to_invite_request_for_known_nest(
        "https://example.test".into(),
        "alice@example.test".into(),
    );

    let snap = m.invite_request_snapshot();
    assert!(matches!(snap.state, InviteRequestState::Idle));
}
