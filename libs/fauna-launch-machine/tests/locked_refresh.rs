//! Tests the one refresh a locked account's machine schedules at
//! `locked_until` (`docs/goal/behavior/devices.md` § The locked state).
//!
//! `fauna.auth.account_locked` is terminal until its time: the machine parks
//! in `Offline { transient: false }` with `LaunchSnapshot::locked_until_secs`
//! set, and arms **exactly one** refresh for the moment the lock lapses. The
//! machine arms it itself — no caller spawns anything — so every app gets it.
//!
//! Outcomes are scripted via a [`MockAuthConnector`] (no network); its call
//! log (`silent_challenge_hints`) is the ceremony count.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;
use std::time::Duration;

use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
    SilentChallengeOutcome,
};

fn test_secret() -> [u8; 32] {
    [0x42; 32]
}

/// The verify reply a lapsed lock's refresh is answered with.
fn verify_reply(token: &str, expires_at: u64) -> fauna_protocol::auth::VerifyReply {
    fauna_protocol::auth::VerifyReply {
        token: token.into(),
        token_id: "0".repeat(16),
        handle: "alice".into(),
        domain: "nest.example".into(),
        tier: "free".into(),
        expires_at,
        expires_in: 3600,
        ..Default::default()
    }
}

fn unix_now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

/// Poll up to ~5 seconds for `predicate()` to return true, yielding so the
/// machine's own scheduled task can run.
async fn poll_until<F: Fn() -> bool>(predicate: F) -> bool {
    for _ in 0..250 {
        if predicate() {
            return true;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// A short settle for the "nothing more happens" assertions: a refresh that
/// fires when it should not fires within microseconds of the park, so 200 ms
/// with no further ceremony is ample margin and no wall-clock race.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
}

/// Launch a machine whose silent challenge answers `outcomes` in order (the
/// last one sticks), returning the connector so a test can count ceremonies.
async fn launched(
    outcomes: Vec<SilentChallengeOutcome>,
) -> (Arc<LaunchMachine>, Arc<MockAuthConnector>) {
    let mut connector = MockAuthConnector::new();
    for o in outcomes {
        connector = connector.push_silent_challenge(o);
    }
    let connector = Arc::new(connector);
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example"),
    );
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), p, connector.clone());
    m.start().await;
    (m, connector)
}

fn locked(locked_until_secs: u64) -> SilentChallengeOutcome {
    SilentChallengeOutcome::Locked { locked_until_secs }
}

fn success() -> SilentChallengeOutcome {
    SilentChallengeOutcome::Success(verify_reply("unlocked.token", unix_now_secs() + 3600))
}

fn is_locked(m: &LaunchMachine) -> bool {
    let snap = m.snapshot();
    snap.phase == LaunchPhase::Offline { transient: false } && snap.locked_until_secs.is_some()
}

/// The lock has already lapsed by the time the refusal is parked (a device
/// that slept through it): the one refresh fires at once and lands Online,
/// with no caller having spawned or called anything.
#[tokio::test]
async fn a_lapsed_lock_refreshes_itself_and_lands_online() {
    let (m, connector) = launched(vec![locked(unix_now_secs() - 60), success()]).await;

    let online = poll_until(|| m.snapshot().phase == LaunchPhase::Online).await;
    assert!(
        online,
        "the machine must refresh itself once the lock lapsed; snapshot = {:?}",
        m.snapshot()
    );
    assert_eq!(m.current_bearer().as_deref(), Some("unlocked.token"));
    assert_eq!(m.snapshot().locked_until_secs, None);
    assert_eq!(
        connector.silent_challenge_hints().len(),
        2,
        "the launch ceremony plus exactly one refresh"
    );
}

/// Terminal until then: a lock still running earns no ceremony before its
/// time — re-signing only re-earns the refusal.
#[tokio::test]
async fn a_running_lock_signs_nothing_before_its_time() {
    let until = unix_now_secs() + 3600;
    let (m, connector) = launched(vec![locked(until), success()]).await;

    settle().await;

    assert!(is_locked(&m), "snapshot = {:?}", m.snapshot());
    assert_eq!(m.snapshot().locked_until_secs, Some(until));
    assert_eq!(
        connector.silent_challenge_hints().len(),
        1,
        "only the launch ceremony — nothing is signed before locked_until"
    );
}

/// **Exactly one.** The nest still answers locked with the same unlock time
/// (this device's clock runs ahead of the nest's): the refresh is spent, and
/// the machine stays parked rather than re-signing in a loop.
#[tokio::test]
async fn the_one_refresh_is_spent_when_the_nest_still_answers_locked() {
    let until = unix_now_secs() - 60;
    let (m, connector) = launched(vec![locked(until), locked(until)]).await;

    let refreshed = poll_until(|| connector.silent_challenge_hints().len() == 2).await;
    assert!(refreshed, "the one refresh must fire");
    settle().await;

    assert_eq!(
        connector.silent_challenge_hints().len(),
        2,
        "a second refresh for the same lock would be a re-sign loop"
    );
    assert!(is_locked(&m), "snapshot = {:?}", m.snapshot());
}

/// One refresh **per lock**: a refresh answered by a *different* unlock time
/// is a new lock (the owner locked again), and it earns its own refresh.
#[tokio::test]
async fn a_new_lock_earns_its_own_refresh() {
    let now = unix_now_secs();
    let (m, connector) = launched(vec![locked(now - 120), locked(now - 60), success()]).await;

    let online = poll_until(|| m.snapshot().phase == LaunchPhase::Online).await;
    assert!(online, "snapshot = {:?}", m.snapshot());
    assert_eq!(connector.silent_challenge_hints().len(), 3);
}

/// Cancelled by any state change: the machine left the locked state before the
/// refresh came due, so the refresh never runs.
#[tokio::test]
async fn a_state_change_cancels_the_scheduled_refresh() {
    // Due about a second out — inside the lapse grace, so the task is asleep
    // when the state changes under it.
    let (m, connector) = launched(vec![locked(unix_now_secs() - 4), success()]).await;
    assert!(is_locked(&m), "snapshot = {:?}", m.snapshot());

    m.set_phase_for_test(LaunchPhase::Offline { transient: true });
    tokio::time::sleep(Duration::from_millis(2500)).await;

    assert_eq!(
        connector.silent_challenge_hints().len(),
        1,
        "a refresh scheduled for a state the machine has left must not run"
    );
    assert_eq!(m.snapshot().phase, LaunchPhase::Offline { transient: true });
}
