//! Tests for FakeNestApi: verifies default responses and setter overrides.
//!
//! FakeNestApi is the test fake used by machine-lifecycle tests in
//! Phase B1 onwards. These tests pin the default-response shape and
//! confirm the setter API works.

use fauna_onboarding_machine::nest_api::{
    FakeNestApi, InviteRequestBody, NestApi, RegisterBody, SetupStatus,
};

#[tokio::test]
async fn default_probe_setup_status_returns_unclaimed() {
    let fake = FakeNestApi::new();
    let r = fake
        .probe_setup_status("https://example.com")
        .await
        .unwrap();
    assert!(!r.claimed);
}

#[tokio::test]
async fn set_probe_setup_status_response_overrides_default() {
    let fake = FakeNestApi::new();
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    let r = fake
        .probe_setup_status("https://example.com")
        .await
        .unwrap();
    assert!(r.claimed);
}

#[tokio::test]
async fn default_claim_admin_returns_ok() {
    let fake = FakeNestApi::new();
    let r = fake
        .claim_admin(
            "https://example.com",
            "code",
            &"00".repeat(32),
            "admin",
            None,
        )
        .await;
    assert!(r.is_ok());
    let resp = r.unwrap();
    assert!(resp.ok);
}

#[tokio::test]
async fn default_submit_invite_request_returns_pending() {
    let fake = FakeNestApi::new();
    let body = InviteRequestBody {
        actor_id: "aa".repeat(32),
        handle: "alice@example.com".into(),
        message: "".into(),
        timestamp: 0,
        signature: "bb".repeat(64),
        age_claim: None,
    };
    let r = fake
        .submit_invite_request("https://example.com", body)
        .await
        .unwrap();
    assert_eq!(r.status, "pending");
}

#[tokio::test]
async fn default_register_returns_ok() {
    let fake = FakeNestApi::new();
    let body = RegisterBody {
        actor_id: "aa".repeat(32),
        handle: "alice@example.com".into(),
        timestamp: 0,
        signature: "bb".repeat(64),
        invite_code: None,
        age_claim: None,
    };
    let r = fake.register("https://example.com", body).await.unwrap();
    assert!(r.ok);
}

/// Smoke test: `OnboardingMachine::with_nest_api` wires a FakeNestApi
/// through the wizard. We submit a claim code and verify the fake's
/// canned response steers the wizard to `NatModeChoice`, proving the trait
/// dispatch picks up the fake rather than the production `WsNestApi`.
#[tokio::test]
async fn with_nest_api_routes_claim_admin_through_fake() {
    use fauna_onboarding_machine::nest_api::{ClaimAdminResponse, NestApi};
    use fauna_onboarding_machine::observer::NullObserver;
    use fauna_onboarding_machine::{OnboardingMachine, OnboardingStep};
    use std::sync::Arc;

    let fake = Arc::new(FakeNestApi::new());
    fake.set_claim_admin_response(Ok(ClaimAdminResponse {
        ok: true,
        token: Some("tok".into()),
        expires_at: Some(0),
        domain: Some("example.com".into()),
        deployment_seed: None,
    }));
    let observer: Arc<dyn fauna_onboarding_machine::OnboardingObserver> = Arc::new(NullObserver);
    let nest_api: Arc<dyn NestApi> = fake.clone();
    let m = OnboardingMachine::with_nest_api(observer, nest_api);
    m.seed_identity("01".repeat(32));
    m.set_current_handle("alice@example.com".into());
    m.set_nest_url("https://example.com".into());
    let step = m.wizard_submit_claim_code("ABC123".into()).await;
    assert_eq!(step, OnboardingStep::NatModeChoice);
    // The claim is the one nest call this path makes (the post-claim
    // storage-mode compat commit, with its probe, left with the
    // compat-remnant sweep 2026-09-24); it proves trait dispatch picks up
    // the fake.
    assert_eq!(fake.calls(), vec!["claim_admin".to_string()]);
}

#[tokio::test]
async fn calls_log_records_method_names_in_order() {
    let fake = FakeNestApi::new();
    let _ = fake.probe_setup_status("https://example.com").await;
    let _ = fake
        .claim_admin(
            "https://example.com",
            "code",
            &"00".repeat(32),
            "admin",
            None,
        )
        .await;
    let _ = fake.verify_invite_code("https://example.com", "C").await;
    assert_eq!(
        fake.calls(),
        vec![
            "probe_setup_status".to_string(),
            "claim_admin".into(),
            "verify_invite_code".into(),
        ]
    );
}
