//! One launch attempt is bounded at [`LAUNCH_ATTEMPT_BOUND`] — the domain dial
//! and the reach-hint fallback together — so the launch progress view always
//! ends in the app or the retry offer (`docs/goal/behavior/onboarding.md`
//! § Wizard exit handling → *The finished wizard holds until the session is
//! signed in*). An attempt that has not settled within it lands
//! `Offline { transient: true }`, on `start`, on Retry and on Trust, and its late
//! reply is dropped with it (§ App-launch routing, the superseded-verdict rule).
//!
//! Every test runs on a paused tokio clock: the scripted connector waits on the
//! shared cross-target sleep, which the runtime auto-advances, so no test spends
//! real time.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;
use std::time::Duration;

use fauna_launch_machine::{
    InMemoryPersistence, LAUNCH_ATTEMPT_BOUND, LaunchMachine, LaunchPhase, MockAuthConnector,
    MockReplyDelay, NullObserver, SilentChallengeOutcome,
};
use fauna_protocol::auth::VerifyReply;
use tokio::time::Instant;

fn verify_reply() -> VerifyReply {
    VerifyReply {
        token: "actor.opaque".into(),
        token_id: "0".repeat(16),
        handle: "alice".into(),
        domain: "nest.example".into(),
        tier: "free".into(),
        expires_at: 1_700_003_600,
        expires_in: 3600,
        ..Default::default()
    }
}

fn persistence() -> Arc<InMemoryPersistence> {
    Arc::new(
        InMemoryPersistence::new()
            .with_identity([0x42; 32].to_vec())
            .with_nest_url("https://nest.example"),
    )
}

fn machine(p: Arc<InMemoryPersistence>, connector: Arc<MockAuthConnector>) -> Arc<LaunchMachine> {
    LaunchMachine::new_with_connector(Arc::new(NullObserver), p, connector)
}

/// The attempt ended at the bound — not before it, and not a moment of real
/// waiting past it (the paused clock lands timers exactly).
fn assert_ended_at_the_bound(t0: Instant) {
    let elapsed = t0.elapsed();
    assert!(
        elapsed >= LAUNCH_ATTEMPT_BOUND && elapsed < LAUNCH_ATTEMPT_BOUND + Duration::from_secs(1),
        "the attempt must end at the {LAUNCH_ATTEMPT_BOUND:?} bound, ended after {elapsed:?}"
    );
}

fn assert_transient_offline(m: &LaunchMachine) {
    let s = m.snapshot();
    assert_eq!(s.phase, LaunchPhase::Offline { transient: true });
    assert!(
        s.last_error.as_deref().is_some_and(|e| !e.is_empty()),
        "a timed-out attempt carries the transient error, got {:?}",
        s.last_error
    );
}

#[test]
fn the_bound_is_thirty_seconds() {
    assert_eq!(LAUNCH_ATTEMPT_BOUND, Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn a_never_answering_launch_ends_transient_offline_at_the_bound() {
    let connector = Arc::new(MockAuthConnector::new());
    connector.set_reply_delay(Some(MockReplyDelay::Never));
    let m = machine(persistence(), connector);

    let t0 = Instant::now();
    m.start().await;

    assert_ended_at_the_bound(t0);
    assert_transient_offline(&m);
}

/// The bound covers the domain dial and the reach-hint fallback TOGETHER: a
/// domain dial that fails late leaves the fallback only what is left of the 30 s.
#[tokio::test(start_paused = true)]
async fn the_bound_covers_the_domain_dial_and_the_fallback_together() {
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity([0x42; 32].to_vec())
            .with_nest_url("https://nest.example")
            .with_reach_ipv4("203.0.113.7"),
    );
    let connector = Arc::new(
        MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Transient {
                error: "connection timed out".into(),
            })
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply())),
    );
    // Each dial takes 20 s: the domain fails at 20 s, the hint would answer at
    // 40 s — past the bound, so the attempt is over at 30 s.
    connector.set_reply_delay(Some(MockReplyDelay::After(Duration::from_secs(20))));
    let m = machine(p.clone(), connector.clone());

    let t0 = Instant::now();
    m.start().await;

    assert_ended_at_the_bound(t0);
    assert_transient_offline(&m);
    assert_eq!(
        connector.silent_challenge_hints(),
        vec![None, Some("203.0.113.7".to_string())],
        "the fallback ran inside the same attempt"
    );
    assert!(p.authenticated.lock().unwrap().is_none());
}

/// A reply that arrives after the bound belongs to an attempt already dropped:
/// it never moves the state, writes nothing and mints no bearer.
#[tokio::test(start_paused = true)]
async fn a_late_reply_changes_nothing() {
    let p = persistence();
    let connector = Arc::new(
        MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply())),
    );
    connector.set_reply_delay(Some(MockReplyDelay::After(
        LAUNCH_ATTEMPT_BOUND + Duration::from_secs(1),
    )));
    let m = machine(p.clone(), connector);

    m.start().await;
    assert_transient_offline(&m);

    // Well past the moment the late reply would have landed.
    tokio::time::sleep(LAUNCH_ATTEMPT_BOUND * 2).await;

    assert_transient_offline(&m);
    assert_eq!(m.current_bearer(), None);
    assert!(m.snapshot().identity.is_none());
    assert!(p.authenticated.lock().unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_never_answering_retry_ends_transient_offline_at_the_bound() {
    let connector = Arc::new(MockAuthConnector::new().push_silent_challenge(
        SilentChallengeOutcome::Transient {
            error: "connection refused".into(),
        },
    ));
    let m = machine(persistence(), connector.clone());
    m.start().await;
    assert_transient_offline(&m);

    connector.set_reply_delay(Some(MockReplyDelay::Never));
    let t0 = Instant::now();
    m.retry_silent_challenge().await;

    assert_ended_at_the_bound(t0);
    assert_transient_offline(&m);
}

#[tokio::test(start_paused = true)]
async fn a_never_answering_trust_ends_transient_offline_at_the_bound() {
    let connector = Arc::new(MockAuthConnector::new().push_silent_challenge(
        SilentChallengeOutcome::IdentityChanged {
            host: "https://nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
            fork: false,
        },
    ));
    let m = machine(persistence(), connector.clone());
    m.start().await;
    assert!(matches!(
        m.snapshot().phase,
        LaunchPhase::IdentityChanged { .. }
    ));

    connector.set_reply_delay(Some(MockReplyDelay::Never));
    let t0 = Instant::now();
    m.trust_nest_identity().await;

    assert_eq!(connector.forgotten_pins().len(), 1);
    assert_ended_at_the_bound(t0);
    assert_transient_offline(&m);
}
