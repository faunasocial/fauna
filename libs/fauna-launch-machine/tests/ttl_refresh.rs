//! Tests the TTL-scheduled background refresh loop. Closes the Phase 1
//! deferral noted in the original brief: "Token refresh loop:
//! scheduled before TTL expiry, plus reactive on notify_401()".
//!
//! Caller spawns `ttl_refresh_loop` on its long-lived runtime
//! (FaunaClient's tokio runtime on Linux; the SPA's main task on web).
//! The loop wakes `fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS` before
//! each cached bearer's expires_at and calls `refresh_token()`, then re-arms
//! with the new token's expiry.
//! Exits when the machine moves out of Online/Refreshing.
//!
//! Refresh outcomes are scripted via a [`MockAuthConnector`] (no network).
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::time::Duration;

use fauna_launch_machine::LaunchPhase;

mod common;
use common::{online_with_expiry, refreshed};

fn unix_now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero() as u64
}

/// Poll up to ~5 seconds for `predicate()` to return true. Yields between
/// iterations so spawned tasks (the TTL loop, the refresh) can run.
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

#[tokio::test]
async fn ttl_refresh_fires_when_token_already_within_buffer() {
    // expires_at = now + 5 s — inside `BEARER_REFRESH_BUFFER_SECS`, so
    // sleep_secs saturates to 0 — the loop's first iteration calls
    // refresh_token immediately.
    let now = unix_now_secs();
    let m = online_with_expiry(
        now + 5,
        vec![refreshed("refreshed.token", "refreshed0000001", now + 3600)],
    )
    .await;
    assert_eq!(m.current_bearer(), Some("initial.token".into()));

    let m_clone = m.clone();
    let handle = tokio::spawn(async move {
        m_clone.ttl_refresh_loop().await;
    });

    let observed = poll_until(|| m.current_bearer().as_deref() == Some("refreshed.token")).await;
    assert!(
        observed,
        "expected ttl_refresh_loop to refresh; current_bearer = {:?}",
        m.current_bearer()
    );

    handle.abort();
}

#[tokio::test]
async fn ttl_refresh_loop_exits_when_machine_leaves_online() {
    // After moving to Offline (via set_phase_for_test), the loop should
    // notice and return.
    let now = unix_now_secs();
    let m = online_with_expiry(now + 3600, vec![]).await;

    let m_clone = m.clone();
    let handle = tokio::spawn(async move {
        m_clone.ttl_refresh_loop().await;
    });

    // The loop is parked in a long sleep (expires_at - the pre-expiry buffer).
    // Aborting here only verifies it started; the immediate-exit semantic is
    // asserted by the second case below.
    tokio::task::yield_now().await;
    m.set_phase_for_test(LaunchPhase::Offline { transient: false });
    handle.abort();

    // If the loop hadn't even started (we set Offline before it ran), it
    // exits immediately. Validate that path:
    let m2 = online_with_expiry(now + 3600, vec![]).await;
    m2.set_phase_for_test(LaunchPhase::Offline { transient: false });
    let m2_clone = m2.clone();
    let handle2 = tokio::spawn(async move {
        m2_clone.ttl_refresh_loop().await;
    });
    let exit_observed = poll_until(|| handle2.is_finished()).await;
    assert!(
        exit_observed,
        "loop didn't exit when state was already Offline"
    );
}

#[tokio::test]
async fn ttl_refresh_re_arms_after_successful_refresh_and_never_spins() {
    // The launch token is inside its buffer, so the first iteration refreshes at
    // once: "initial" → "first-refresh". The refreshed token's deadline is ALSO
    // inside the buffer — the shape a nest-absolute deadline read on a client
    // hours ahead produced — so before 2026-09-21 the second iteration fired
    // immediately too, and this test asserted exactly that ("re-arms"), which is
    // the hot re-mint loop `login.md` § Token lifetime on the client's clock
    // rules out. Now the loop must re-arm (stay alive, not exit) AND wait one
    // buffer interval before its next mint: the mock's call log must show the
    // launch plus exactly one refresh, and the loop task must still be running.
    //
    // A short settle, not a wait for an outcome: the spin this guards against
    // minted again within microseconds of the first refresh, so a 200 ms window
    // with no second mint is two hundred thousand times the margin, while the
    // legitimate re-arm is 60 s away. Nothing here is a wall-clock race.
    let now = unix_now_secs();
    let m = online_with_expiry(
        now + 3,
        vec![
            refreshed("first-refresh", "firstrefresh0001", now + 5),
            refreshed("second-refresh", "secondrefresh001", now + 3600),
        ],
    )
    .await;

    let m_clone = m.clone();
    let handle = tokio::spawn(async move {
        m_clone.ttl_refresh_loop().await;
    });

    let saw_first = poll_until(|| m.current_bearer().as_deref() == Some("first-refresh")).await;
    assert!(
        saw_first,
        "expected ttl_refresh_loop to refresh the in-buffer launch token; current_bearer = {:?}",
        m.current_bearer()
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        m.current_bearer().as_deref(),
        Some("first-refresh"),
        "a refresh that still computes to 'spent' must wait one buffer interval, \
         not re-mint at once"
    );
    assert!(
        !handle.is_finished(),
        "the loop must have re-armed (still Online), not exited"
    );
    assert_eq!(
        m.own_token_ids().len(),
        2,
        "the mint count: the launch session and ONE refresh — a spin would have \
         recorded a third id by now"
    );

    handle.abort();
}
