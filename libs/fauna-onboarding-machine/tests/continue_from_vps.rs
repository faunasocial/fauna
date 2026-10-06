use std::sync::Arc;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{OnboardingError, OnboardingMachine, OnboardingStep};

fn machine() -> Arc<OnboardingMachine> {
    OnboardingMachine::new(Arc::new(NullObserver))
}

#[tokio::test]
async fn continue_from_vps_refuses_when_not_verified() {
    let m = machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    let err = m.continue_from_vps().await.unwrap_err();
    assert!(matches!(err, OnboardingError::InvalidTransition { .. }));
}

#[tokio::test]
async fn continue_from_vps_refuses_without_server_type_or_location() {
    let m = machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    // Verified but missing server type and location.
    m.set_vps_state_for_test(|v| v.verified = true);
    assert!(m.continue_from_vps().await.is_err());
}

#[tokio::test]
async fn continue_from_vps_advances_to_nest_provisioning() {
    let m = machine();
    m.set_step_for_test(OnboardingStep::VpsConfig);
    m.set_vps_state_for_test(|v| {
        // A provider must be selected: `verify_vps` refuses without one, so
        // `verified == true` with no `selected_provider_id` is a state
        // production can't reach — and since `vps_continue_shortfall` grew its
        // `NoProvider` arm (the walk's wizard invariants) the
        // fixture has to stop fabricating it.
        v.selected_provider_id = Some("hetzner".into());
        v.verified = true;
        v.selected_server_type_id = Some("cx22".into());
        v.selected_location_id = Some("nbg1".into());
    });

    m.continue_from_vps()
        .await
        .expect("continue_from_vps succeeds");
    assert_eq!(m.step(), OnboardingStep::NestProvisioning);
}
