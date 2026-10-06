//! The dial override redirects the **socket** and nothing else.
//!
//! `docs/goal/behavior/onboarding.md` § Implementation status today — *§ 3b's
//! derived-ON branch is e2e-proven locally*, second bullet. The e2e proves the
//! socket moves; this pins the half a green e2e cannot see, because a run that
//! persists the harness URL still passes: **what the launch writes back to the
//! long-term store must stay the literal typed URL.**
//!
//! Why that is worth its own test. The override exists so a domain-shaped handle
//! (`someone@fauna.test`) can reach a local nest, and the obvious simplification
//! — resolve once, high up, in `LaunchMachine::start` — would put
//! `http://127.0.0.1:<port>` into `State::Online` and from there through
//! `save_authenticated` into the account registry. Every subsequent launch on
//! that box would then dial a torn-down fixture, in a *release* build, with no
//! override installed and nothing to explain it. That is at-rest corruption
//! rather than a test-only wart, which is why resolution lives in
//! `WsAuthConnector` alone (`connector.rs`) and why this file exists to keep it
//! there.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
    SilentChallengeOutcome, set_nest_dial_override,
};
use fauna_protocol::auth::VerifyReply;

/// The literal a user typed and the client persisted: a domain no local DNS
/// resolves, which is the whole reason an override is installed at all.
const TYPED: &str = "https://fauna.test";
/// Where the harness actually serves that nest.
const HARNESS: &str = "http://127.0.0.1:8099";

fn verify_reply() -> VerifyReply {
    VerifyReply {
        token: "actor.opaque".into(),
        token_id: "0".repeat(16),
        handle: "admin".into(),
        domain: "fauna.test".into(),
        tier: "free".into(),
        expires_at: 1_700_003_600,
        expires_in: 3600,
        ..Default::default()
    }
}

/// One test, not several: the override is process-global, so separate `#[test]`
/// functions in this binary would race and the verdict would depend on the
/// scheduler.
#[tokio::test]
async fn an_installed_override_moves_the_socket_but_never_the_persisted_nest_url() {
    set_nest_dial_override(Some(HARNESS.to_string()));

    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(vec![0x42; 32])
            .with_nest_url(TYPED),
    );
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        p.clone(),
        Arc::new(
            MockAuthConnector::new()
                .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply())),
        ),
    );

    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::Online,
        "the launch must still reach Online with an override installed"
    );

    let saved = p
        .authenticated
        .lock()
        .unwrap()
        .clone()
        .expect("a successful silent challenge must write the account back");
    assert_eq!(
        saved.0, TYPED,
        "the launch persisted the RESOLVED dial URL. The override must never \
         reach the store: a box that saved it would dial a torn-down fixture on \
         every later launch, including release builds with no override at all. \
         Resolve in `WsAuthConnector`, not in the machine."
    );

    set_nest_dial_override(None);
}
