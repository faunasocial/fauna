//! A handle whose domain points at a server that does not answer as a nest.
//!
//! The commonest way to meet it: a nest box that was deleted while its domain
//! still points at the old address. For a public domain onboarding goes on
//! from what exists — `RegisteredNoNest`, Continue enabled, no "I control this
//! domain" checkbox (`docs/goal/behavior/onboarding.md` § 2, *A public domain
//! whose address answers nothing*). A local target that answers nothing, and
//! any address answering a server error, name the address in the message —
//! never the generic "Temporary error: nest unreachable" these arms used to
//! fold into (`docs/goal/ui/README.md` § Copy comprehensibility rule 2).
//!
//! Driven like `handle_check_local_nest.rs`: the real `start_handle_check`
//! against a wiremock health endpoint reached through the `nest` override.

use std::sync::Arc;
use std::time::Duration;

use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    FakeNestApi, HandleCheckOutcome, HandleCheckPhase, OnboardingMachine, OnboardingObserver,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn machine(server: &MockServer) -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let m = OnboardingMachine::with_nest_api(observer, Arc::new(FakeNestApi::new()));
    m.seed_identity("01".repeat(32));
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m
}

async fn silent_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(8)))
        .mount(&server)
        .await;
    server
}

/// The deleted-box case: a public domain whose address answers nothing goes on
/// to set up a nest there, Continue live at once.
#[tokio::test]
async fn silent_public_domain_goes_on_to_set_up_a_nest() {
    let server = silent_server().await;
    let m = machine(&server);
    m.start_handle_check("test@deleted-box.example".into())
        .await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::RegisteredNoNest,
        "msg {:?}",
        snap.message.key
    );
    assert_eq!(
        snap.message.key,
        "onboarding.handle_check.outcome.registered_no_nest"
    );
    assert!(snap.continue_enabled, "no checkbox gates Continue any more");
}

/// A local target that answers nothing: the message is `nest_unreachable`,
/// naming the address — not the generic transient line.
#[tokio::test]
async fn silent_local_nest_names_the_address_not_a_generic_temporary_error() {
    let server = silent_server().await;
    let port = server.address().port();

    let m = machine(&server);
    m.start_handle_check(format!("test@127.0.0.1:{port}")).await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.message.key, "onboarding.handle_check.error.nest_unreachable",
        "outcome {:?}",
        snap.outcome
    );
    assert_eq!(
        snap.message.args.get("domain").map(String::as_str),
        Some(format!("127.0.0.1:{port}").as_str())
    );
    assert!(matches!(
        snap.outcome,
        HandleCheckOutcome::ProbeError {
            phase: HandleCheckPhase::NestProbe,
            transient: true,
            ..
        }
    ));
    assert!(!snap.continue_enabled);
}

/// The address answers with a server error: `nest_misbehaving`, naming the
/// domain.
#[tokio::test]
async fn nest_server_error_names_the_domain() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let port = server.address().port();

    let m = machine(&server);
    m.start_handle_check(format!("test@127.0.0.1:{port}")).await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.message.key, "onboarding.handle_check.error.nest_misbehaving",
        "outcome {:?}",
        snap.outcome
    );
    assert_eq!(
        snap.message.args.get("domain").map(String::as_str),
        Some(format!("127.0.0.1:{port}").as_str())
    );
    assert!(!snap.continue_enabled);
}
