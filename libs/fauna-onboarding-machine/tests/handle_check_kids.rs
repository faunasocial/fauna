//! The kids build's handle entry: joining a nest by invite is the only
//! onboarding the kids app keeps — nest setup and claim are excised
//! (`docs/goal/behavior/account-age-band.md` § The account age band, the
//! kids-app bullet, item (4)). Under the machine's kids input (the `kids`
//! cargo feature the kids FFI flavor turns on, `set_kids_for_test` here) a
//! handle check that would offer to set a nest up or claim one ends instead in
//! the terminal `kids_no_joinable_nest` line: no control checkbox, Continue
//! off, and a forced Continue stays on handle entry.
//!
//! The probe legs are served as in `handle_check_local_nest.rs`: wiremock for
//! the HTTP health check, a `FakeNestApi` for the WS-RPC legs (defaults: not
//! registered, an unclaimed nest).

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::SetupStatus;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    FakeNestApi, HandleCheckOutcome, HandleCheckPhase, HandleCheckSnapshot, OnboardingMachine,
    OnboardingObserver, OnboardingStep,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KIDS_KEY: &str = "onboarding.handle_check.outcome.kids_no_joinable_nest";

async fn healthy_nest() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})))
        .mount(&server)
        .await;
    server
}

fn machine(api: Arc<FakeNestApi>, kids: bool) -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::with_nest_api(observer, api);
    m.seed_identity("01".repeat(32));
    m.set_kids_for_test(kids);
    m
}

fn assert_kids_terminal(snap: &HandleCheckSnapshot) {
    assert_eq!(
        snap.message.key, KIDS_KEY,
        "kids line expected, got {snap:?}"
    );
    assert!(
        matches!(
            snap.outcome,
            HandleCheckOutcome::ProbeError {
                transient: false,
                ..
            }
        ),
        "a terminal no-joinable-nest outcome, got {:?}",
        snap.outcome
    );
    assert_eq!(snap.phase, HandleCheckPhase::Complete);
    assert!(!snap.continue_enabled, "Continue must be off");
    assert!(!snap.control_checkbox_visible, "no set-up checkbox");
    assert!(
        snap.message.args.contains_key("domain"),
        "the line names the domain"
    );
}

/// An unclaimed nest is a claim offer everywhere else; on the kids build it is
/// the terminal kids line.
#[tokio::test]
async fn kids_unclaimed_nest_is_no_claim_offer() {
    let server = healthy_nest().await;
    let port = server.address().port();
    let m = machine(Arc::new(FakeNestApi::new()), true);
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("kid@127.0.0.1:{port}")).await;
    assert_kids_terminal(&m.handle_check_snapshot());
}

/// The same probe without the kids input still offers the claim — the input,
/// not the probe, makes the difference.
#[tokio::test]
async fn non_kids_unclaimed_nest_still_offers_claim() {
    let server = healthy_nest().await;
    let port = server.address().port();
    let m = machine(Arc::new(FakeNestApi::new()), false);
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("kid@127.0.0.1:{port}")).await;
    assert_eq!(
        m.handle_check_snapshot().outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest
    );
}

/// A claimed nest the kid is not on is the invite path — kept on the kids build.
#[tokio::test]
async fn kids_claimed_nest_keeps_the_invite_path() {
    let server = healthy_nest().await;
    let port = server.address().port();
    let api = Arc::new(FakeNestApi::new());
    api.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    let m = machine(api, true);
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("kid@127.0.0.1:{port}")).await;
    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::NestRunningUserUnregistered
    );
    assert!(snap.continue_enabled);
    assert_eq!(
        m.submit_handle_check_continue().await,
        OnboardingStep::InviteRequest
    );
}

/// A local target with nothing listening tells everyone else to start a nest
/// there; the kids build says there is no nest to join.
#[tokio::test]
async fn kids_local_no_nest_says_no_joinable_nest() {
    // Bind and drop a listener: its port now refuses connections.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let m = machine(Arc::new(FakeNestApi::new()), true);
    m.start_handle_check(format!("kid@127.0.0.1:{port}")).await;
    let snap = m.handle_check_snapshot();
    if snap.message.key == "onboarding.handle_check.outcome.local_no_nest" {
        panic!("kids build must not tell the kid to start a nest: {snap:?}");
    }
    assert_kids_terminal(&snap);
}

/// Defense in depth: whatever outcome the snapshot holds, a kids Continue
/// never routes to DNS set-up or the claim page.
#[tokio::test]
async fn kids_continue_never_routes_to_setup_or_claim() {
    for (outcome, checked) in [
        (
            HandleCheckOutcome::DomainAvailable {
                buyable_via_provider: true,
                price: None,
            },
            false,
        ),
        (HandleCheckOutcome::RegisteredNoNest, true),
        (HandleCheckOutcome::UnregisteredUnclaimedNest, false),
    ] {
        let m = machine(Arc::new(FakeNestApi::new()), true);
        m.set_step_for_test(OnboardingStep::HandleEntry);
        let mut snap = HandleCheckSnapshot::idle();
        snap.phase = HandleCheckPhase::Complete;
        snap.outcome = outcome.clone();
        snap.continue_enabled = true;
        snap.control_checkbox_checked = checked;
        m.set_handle_check_snapshot_for_test(snap);
        assert_eq!(
            m.submit_handle_check_continue().await,
            OnboardingStep::HandleEntry,
            "kids Continue on {outcome:?} must stay on handle entry"
        );
    }
}
