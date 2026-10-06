//! Integration test for the local "trying out the app" handle-check path.
//!
//! A handle whose domain is a loopback / IP target (`test@localhost`,
//! `test@127.0.0.1:<port>`) must skip the DNS / TLD / price probes and hit the
//! nest directly — the bug this fixes was `test@localhost` → `tld_invalid`.
//! These drive the REAL `run_handle_check_phases` (resolve-domain → silent
//! sign-in) against a wiremock nest standing in for a local `fauna-nest`, and
//! are the only tests exercising the resolve-domain → probe path end to end.
//!
//! `localhost` resolves to `127.0.0.1`, where wiremock binds, so a
//! `…@localhost:<wiremock-port>` handle reaches the mock — keeping the test
//! hermetic while exercising the literal `localhost` hostname.
//!
//! **Transport note.** Only the nest **health** probe still rides HTTP, so
//! wiremock answers it. Both the silent-sign-in (`fauna.auth.{challenge,verify}`)
//! and the claimed/unclaimed `setup-status` discrimination now ride **WS-RPC**
//! (`NestApi::{silent_challenge,probe_setup_status}` → `WsNestApi`), which
//! wiremock can't serve — so we inject a `FakeNestApi` whose defaults model a
//! fresh, unclaimed nest the actor isn't on: `silent_challenge` →
//! `NotRegistered`, `probe_setup_status` → `claimed: false`. Those WS-RPC
//! transports are proven against a real nest in
//! `bins/fauna-nest/tests/onboarding_ws_rpc_roundtrip.rs`; here we exercise the
//! resolve-domain + outcome-routing logic around them.
//!
//! See `docs/goal/behavior/onboarding.md` §2 "Local / loopback targets".

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::SetupStatus;
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    FakeNestApi, HandleCheckOutcome, HandleCheckPhase, OnboardingMachine, OnboardingObserver,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount the one remaining HTTP probe endpoint of a fresh, unclaimed nest: the
/// health check. The silent-sign-in (`fauna.auth.{challenge,verify}`) and
/// `setup-status` legs now ride WS-RPC (`WsNestApi`), not HTTP, so they are
/// answered by the injected `FakeNestApi` (defaults: `silent_challenge` →
/// `NotRegistered`, `probe_setup_status` → `claimed: false`) rather than by this
/// server — see `machine()`.
async fn mount_unclaimed_nest(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v1/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})))
        .mount(server)
        .await;
}

fn machine() -> Arc<OnboardingMachine> {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    // The health probe still goes over HTTP (→ wiremock). The silent-challenge
    // and setup-status legs moved to WS-RPC (`WsNestApi`), which a wiremock HTTP
    // server can't answer, so inject a `FakeNestApi` for them; its defaults
    // (`silent_challenge` → NotRegistered, `probe_setup_status` → claimed:false)
    // model a fresh unclaimed nest → UnregisteredUnclaimedNest.
    let m = OnboardingMachine::with_nest_api(observer, Arc::new(FakeNestApi::new()));
    m.seed_identity("01".repeat(32));
    m
}

/// `127.0.0.1:<port>` is a loopback target → resolves to
/// `http://127.0.0.1:<port>`, skips DNS, probes the nest directly. The pre-fix
/// behavior returned `TldInvalid` here without any network I/O.
#[tokio::test]
async fn loopback_ip_handle_reaches_unclaimed_nest_not_tld_invalid() {
    let server = MockServer::start().await;
    mount_unclaimed_nest(&server).await;
    let port = server.address().port();

    let m = machine();
    // Pillar C (uniform https): an explicit-port loopback handle now resolves to
    // `https://…`, but wiremock serves plain HTTP — so point the probe at the
    // mock via the `nest` override seam (the production same-box / e2e path).
    // `state.nest_url` still records the *resolved* https URL, never the override.
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("admin@127.0.0.1:{port}"))
        .await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest,
        "loopback-IP handle should reach the unclaimed-nest claim path (got {:?}, msg {:?})",
        snap.outcome,
        snap.message.key,
    );
    // The resolved https URL (uniform scheme; not the http override) is recorded
    // for the claim step, host + explicit port preserved.
    assert_eq!(m.nest_url(), format!("https://127.0.0.1:{port}"));
}

/// The literal `localhost` hostname (with an explicit port pointed at the mock)
/// takes the same local path — proving `localhost` is no longer `tld_invalid`.
#[tokio::test]
async fn localhost_handle_reaches_unclaimed_nest_not_tld_invalid() {
    let server = MockServer::start().await;
    mount_unclaimed_nest(&server).await;
    let port = server.address().port();

    let m = machine();
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("admin@localhost:{port}"))
        .await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest,
        "localhost handle should reach the unclaimed-nest claim path (got {:?}, msg {:?})",
        snap.outcome,
        snap.message.key,
    );
    // Uniform https: the resolved nest_url is https even though the probe hit the
    // plain-HTTP wiremock nest via the `nest` override.
    assert_eq!(m.nest_url(), format!("https://localhost:{port}"));
}

/// Regression: a completed handle-check's result must NOT survive the user
/// going Back and re-importing an identity. Before the fix,
/// `confirm_imported_identity` left the stale `HandleCheckSnapshot` (and the
/// resolved `nest_url`) in place, so the HandleEntry page showed the prior
/// probe's conclusion (e.g. "unregistered at example.com") until the user
/// clicked Check again. See docs/goal/behavior/onboarding.md § Identity stage.
#[tokio::test]
async fn reimporting_identity_resets_stale_handle_check() {
    let server = MockServer::start().await;
    mount_unclaimed_nest(&server).await;
    let port = server.address().port();

    let m = machine();
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("admin@127.0.0.1:{port}"))
        .await;
    // Precondition: a non-idle outcome and a resolved nest_url are recorded.
    assert_eq!(
        m.handle_check_snapshot().outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest
    );
    assert_eq!(m.nest_url(), format!("https://127.0.0.1:{port}"));

    // User goes Back and imports a *different* identity.
    m.begin_import_identity();
    m.confirm_imported_identity("ab".repeat(32)).unwrap();

    // The handle page must start fresh: snapshot idle, nest_url + domain_status cleared.
    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::None,
        "stale outcome must reset to idle on identity re-import"
    );
    assert_eq!(
        snap.phase,
        HandleCheckPhase::Idle,
        "phase must reset to idle"
    );
    assert_eq!(m.nest_url(), "", "resolved nest_url must clear");
    assert!(m.domain_status().is_none(), "domain_status must clear");
}

/// Regression (Blocker 3): a handle-check OUTCOME
/// message must carry its interpolation args. Before the fix, `complete()`
/// hard-coded `args: Default::default()`, so on every app that RESOLVES the
/// i18n template (macos/ios `renderLocalizedText`) the user saw the literal
/// `{domain}` — "{domain} but no one has claimed it yet." linux/web/windows
/// merely masked it (their lookup misses the `onboarding.`-prefixed key → they
/// render the raw key, never substituting). Assert the `domain` arg is present
/// (== the parsed domain) so every app interpolates it. The unclaimed-nest
/// branch is the default-FakeNestApi path.
#[tokio::test]
async fn unclaimed_nest_outcome_carries_domain_arg() {
    let server = MockServer::start().await;
    mount_unclaimed_nest(&server).await;
    let port = server.address().port();

    let m = machine();
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("admin@127.0.0.1:{port}"))
        .await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest,
        "precondition: default FakeNestApi → unclaimed-nest claim path (msg {:?})",
        snap.message.key,
    );
    let expected = format!("127.0.0.1:{port}");
    assert_eq!(
        snap.message.args.get("domain"),
        Some(&expected),
        "unregistered_unclaimed_nest message must carry the `domain` arg so \
         `{{domain}}` interpolates on every app (args were {:?})",
        snap.message.args,
    );
}

/// Same guarantee on the claimed-but-user-unregistered branch — a distinct
/// `complete()` call site (`handle_check.outcome.user_unregistered`).
#[tokio::test]
async fn user_unregistered_outcome_carries_domain_arg() {
    let server = MockServer::start().await;
    mount_unclaimed_nest(&server).await;
    let port = server.address().port();

    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    // A claimed nest (the actor still isn't registered → NestRunningUserUnregistered).
    fake.set_probe_setup_status_response(Ok(SetupStatus {
        claimed: true,
        ..Default::default()
    }));
    let m = OnboardingMachine::with_nest_api(observer, fake);
    m.seed_identity("01".repeat(32));
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("admin@127.0.0.1:{port}"))
        .await;

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::NestRunningUserUnregistered,
        "precondition: claimed nest + unregistered actor (msg {:?})",
        snap.message.key,
    );
    let expected = format!("127.0.0.1:{port}");
    assert_eq!(
        snap.message.args.get("domain"),
        Some(&expected),
        "user_unregistered message must carry the `domain` arg (args were {:?})",
        snap.message.args,
    );
}

/// Same guarantee for the *create* path (`confirm_generated_identity`).
#[tokio::test]
async fn recreating_identity_resets_stale_handle_check() {
    let server = MockServer::start().await;
    mount_unclaimed_nest(&server).await;
    let port = server.address().port();

    let m = machine();
    m.set_provider_base_urls(std::collections::HashMap::from([(
        "nest".to_string(),
        server.uri(),
    )]));
    m.start_handle_check(format!("admin@127.0.0.1:{port}"))
        .await;
    assert_eq!(
        m.handle_check_snapshot().outcome,
        HandleCheckOutcome::UnregisteredUnclaimedNest
    );

    // User goes Back and generates a fresh identity instead.
    m.begin_create_identity();
    m.confirm_generated_identity().unwrap();

    let snap = m.handle_check_snapshot();
    assert_eq!(
        snap.outcome,
        HandleCheckOutcome::None,
        "stale outcome must reset to idle on identity re-create"
    );
    assert_eq!(
        snap.phase,
        HandleCheckPhase::Idle,
        "phase must reset to idle"
    );
    assert_eq!(m.nest_url(), "", "resolved nest_url must clear");
}
