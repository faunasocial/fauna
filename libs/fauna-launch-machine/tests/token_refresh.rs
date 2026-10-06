//! Tests the bearer-token refresh path: manual `refresh_token()` and
//! 401-reactive `notify_401()`.
//!
//! Headline new behavior — none of the 6 clients today have a 401-reactive
//! interceptor; they all rely on TTL pre-expiry refresh with a 60-second
//! buffer. The LaunchMachine adds 401-reactive handling so HTTP layers
//! don't carry their own retry logic.
//!
//! These tests drive **state transitions** against a scripted
//! [`MockAuthConnector`] (no network). Every refresh is the silent challenge —
//! the same `fauna.auth.challenge` + `verify` ceremony launch takes, so a wrong
//! client clock cannot refuse it (`login.md` § When to use which) — whose
//! ceremony is exercised in `fauna_protocol::auth` and whose real WS round-trip
//! is the tier_3 `bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs`.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
    SilentChallengeOutcome, TokenStatus,
};

mod common;
use common::{online_with_expiry, refreshed, test_secret, verify_reply};

/// Drive the machine to Online via a scripted silent-challenge success, with a
/// given outcome queued for the subsequent refresh (the same ceremony).
async fn machine_online_with_refresh(refresh: SilentChallengeOutcome) -> Arc<LaunchMachine> {
    online_with_expiry(1_700_003_600, vec![refresh]).await
}

/// Unix seconds far enough ahead that nothing minted against it is pruned
/// mid-test. The fixture expiry above (`1_700_003_600`, Nov 2023) is in the
/// PAST, which is fine for the phase/bearer assertions but would make an
/// own-session-id assertion vacuous — the set prunes by `expires_at`, so a
/// lapsed id is correctly forgotten the moment it is recorded.
fn far_future() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_secs()
        + 3600
}

#[tokio::test]
async fn refresh_token_replaces_bearer_in_online() {
    // Two hours out on THIS clock: the deadline the machine records is the
    // reply's `expires_in` anchored at receipt, so the assertion below reads a
    // window around it rather than the fixture's absolute `expires_at`.
    let two_hours_out = far_future() + 3600;
    let m = machine_online_with_refresh(refreshed(
        "refreshed.token",
        "refreshed0000001",
        two_hours_out,
    ))
    .await;

    assert_eq!(m.current_bearer(), Some("initial.token".into()));
    m.refresh_token().await;
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    assert_eq!(m.current_bearer(), Some("refreshed.token".into()));
    match m.snapshot().token {
        TokenStatus::Valid { expires_at_secs } => assert!(
            (two_hours_out - 5..=two_hours_out + 5).contains(&expires_at_secs),
            "deadline must be ~two hours out on this clock, got {expires_at_secs}"
        ),
        other => panic!("expected Valid, got {other:?}"),
    }
}

/// **The machine's own-session-id set across a renewal**
/// (`docs/goal/behavior/devices.md` § The client's own session) — the
/// `LaunchMachine` twin of the `TokenCache` pins in
/// `libs/fauna-client/src/token_cache.rs`.
///
/// Both arms must record: the launch mint and the renewal are both the silent
/// challenge, but land through different code paths. If either dropped its id the app
/// would paint one of its own two live rows as a stranger, and *sign out
/// everywhere else* would appear to find and kill it.
#[tokio::test]
async fn own_token_ids_keep_both_arms_ids_across_a_renewal() {
    let expiry = far_future();
    // `verify_reply` (tests/common) mints the launch session as `0`*16.
    let m = online_with_expiry(
        expiry,
        vec![refreshed("refreshed.token", "refreshed0000001", expiry)],
    )
    .await;

    // The silent-challenge arm recorded the launch session.
    assert_eq!(m.own_token_ids(), vec!["0".repeat(16)]);
    assert_eq!(m.current_token_id(), Some("0".repeat(16)));

    m.refresh_token().await;

    // The refresh arm recorded the successor — and the predecessor stays,
    // because the nest still lists it until it expires.
    assert_eq!(
        m.own_token_ids(),
        vec!["0".repeat(16), "refreshed0000001".to_string()],
        "a renewal must not orphan the session it replaced"
    );
    assert_eq!(
        m.current_token_id().as_deref(),
        Some("refreshed0000001"),
        "keep_token_id must name the NEW token, read at call time"
    );
    // The live bearer and the current id name the same session.
    assert_eq!(m.current_bearer().as_deref(), Some("refreshed.token"));
}

/// A lapsed own id is forgotten: the nest stopped listing that row, so folding
/// it into "this app" would name a session nobody can see.
#[tokio::test]
async fn own_token_ids_drop_a_session_that_has_expired() {
    // The default fixture expiry is in the past, so the launch id lapses the
    // moment it is recorded.
    let m = machine_online_with_refresh(refreshed(
        "refreshed.token",
        "refreshed0000001",
        1_700_007_200,
    ))
    .await;
    assert!(
        m.own_token_ids().is_empty(),
        "an already-lapsed launch session is not a live own session"
    );
    assert_eq!(m.current_token_id(), None);
}

#[tokio::test]
async fn notify_401_triggers_refresh_and_returns_to_online() {
    let m = machine_online_with_refresh(refreshed(
        "post-401.token",
        "post401000000001",
        1_700_007_200,
    ))
    .await;

    // HTTP layer received a 401 on a content endpoint and tells the
    // machine. Machine refreshes the bearer; no caller intervention.
    m.notify_401().await;

    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    assert_eq!(m.current_bearer(), Some("post-401.token".into()));
}

/// The account this session belonged to is gone from the nest — the verify
/// kind's `fauna.auth.not_registered`. Terminal: no re-sign brings it back.
#[tokio::test]
async fn refresh_not_registered_lands_offline_terminal() {
    let m = machine_online_with_refresh(SilentChallengeOutcome::NotRegistered).await;

    m.refresh_token().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected Offline {{ transient: false }}, got {other:?}"),
    }
    assert!(m.current_bearer().is_none());
    // The mid-session twin of the launch row (`onboarding.md` § App-launch
    // routing → the previously-signed-in row; `security.md` § Post-auth
    // surfacing: a post-auth verdict lands the SAME surface the launch renders):
    // the refusal rides the side channel with the localized copy, never the
    // raw wire code an earlier build painted here.
    let snap = m.snapshot();
    assert!(
        snap.sign_in_refused,
        "the refresh arm carries the same verdict as the launch arm"
    );
    assert_eq!(
        snap.last_error.as_deref(),
        Some(fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED)
    );
}

/// The account is locked out — verify's `fauna.auth.account_locked`
/// (`login.md` § Silent Challenge; `devices.md` § The locked state). Terminal
/// until `locked_until`: the phase stays `Offline { transient: false }` and the
/// unlock time rides the additive `locked_until_secs` side channel.
#[tokio::test]
async fn refresh_with_locked_lands_offline_terminal() {
    // Still running: a lock already lapsed would have the machine's own
    // scheduled refresh (`locked_refresh.rs`) leave this state under the test.
    let locked_until_secs = far_future();
    let m = machine_online_with_refresh(SilentChallengeOutcome::Locked { locked_until_secs }).await;

    m.refresh_token().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected Offline {{ transient: false }}, got {other:?}"),
    }
    assert!(m.current_bearer().is_none());
    let snap = m.snapshot();
    assert_eq!(snap.locked_until_secs, Some(locked_until_secs));
    assert!(!snap.sign_in_refused, "a lock is not the suspended verdict");
    assert_eq!(
        snap.last_error.as_deref(),
        Some(fauna_i18n::strings::onboarding::launch::ACCOUNT_LOCKED)
    );
}

#[tokio::test]
async fn refresh_with_an_invalid_secret_lands_offline_terminal() {
    // The stored secret is not a 32-byte key — the ceremony cannot even sign.
    // Terminal; the user needs to re-authenticate.
    let m = machine_online_with_refresh(SilentChallengeOutcome::SecretInvalid {
        error: "secret must be 32 bytes".into(),
    })
    .await;

    m.refresh_token().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected Offline {{ transient: false }}, got {other:?}"),
    }
    assert!(m.current_bearer().is_none());
}

#[tokio::test]
async fn refresh_superseded_is_terminal_and_names_the_successor() {
    // The identity was succeeded mid-session: the old key still signs valid
    // ceremonies forever, so a retry only re-earns the refusal. Park in the
    // non-retry phase and carry the claimed successor for the import flow.
    let m = machine_online_with_refresh(SilentChallengeOutcome::Superseded {
        new_actor_id_hex: "ab".repeat(32),
    })
    .await;

    m.refresh_token().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected Offline {{ transient: false }}, got {other:?}"),
    }
    assert_eq!(
        m.snapshot().superseded_successor.as_deref(),
        Some("ab".repeat(32).as_str())
    );
    assert!(m.current_bearer().is_none());
}

#[tokio::test]
async fn refresh_nest_outdated_lands_offline_terminal_with_localized_banner() {
    // A degraded nest answers `fauna.nest.outdated`. The machine must land in
    // the *non-transient* offline state (an actionable update prompt, NOT the
    // retry spin a transient blip gets) and paint the localized banner — the
    // client-facing proof of version-compatibility.md Dim 4 / Track 3.
    let banner = fauna_protocol::RpcError::nest_outdated()
        .localized()
        .to_string();
    let m = machine_online_with_refresh(SilentChallengeOutcome::NeedsUpdate {
        message: banner.clone(),
    })
    .await;

    m.refresh_token().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected Offline {{ transient: false }}, got {other:?}"),
    }
    assert!(m.current_bearer().is_none());
    let err = m.snapshot().last_error.unwrap_or_default();
    assert_eq!(
        err, banner,
        "banner must be the localized, actionable message"
    );
    assert!(
        !err.contains("fauna.nest.outdated"),
        "banner must render the localized message, not the raw wire code: {err:?}"
    );
}

#[tokio::test]
async fn refresh_transient_is_transient_offline() {
    let m = machine_online_with_refresh(SilentChallengeOutcome::Transient {
        error: "disconnect".into(),
    })
    .await;

    m.refresh_token().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: true } => {}
        other => panic!("expected Offline {{ transient: true }}, got {other:?}"),
    }
}

#[tokio::test]
async fn refresh_outside_online_is_noop() {
    // A LaunchMachine that hasn't gone Online has no bearer to refresh.
    // refresh_token() / notify_401() should be no-ops.
    let p = Arc::new(InMemoryPersistence::new());
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        p,
        Arc::new(MockAuthConnector::new()),
    );
    let phase_before = m.snapshot().phase.clone();
    m.refresh_token().await;
    m.notify_401().await;
    assert_eq!(m.snapshot().phase, phase_before);
}

#[tokio::test]
async fn token_status_is_refreshing_during_in_flight_refresh() {
    // A refresh fires ≥2 transitions (Online → Refreshing → Online), so
    // TokenStatus::Refreshing is observable to HTTP layers mid-flight.
    use fauna_launch_machine::CountingObserver;
    let observer = CountingObserver::new();
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply(
            "initial.token",
            1_700_003_600,
        )))
        .push_silent_challenge(refreshed(
            "refreshed.token",
            "refreshed0000001",
            1_700_007_200,
        ));
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example"),
    );
    let m = LaunchMachine::new_with_connector(observer.clone(), p, Arc::new(connector));
    m.start().await;
    let count_after_start = observer.count();
    m.refresh_token().await;
    let count_after_refresh = observer.count();
    assert!(
        count_after_refresh >= count_after_start + 2,
        "refresh should fire ≥2 transitions (Online → Refreshing → Online); got {} → {}",
        count_after_start,
        count_after_refresh
    );
}

#[tokio::test]
async fn refresh_identity_changed_blocks_on_the_warning_surface() {
    // A mid-session identity change (the refresh path's graduation caught a
    // changed pin) is the same MITM signal as a launch-time one: drop the
    // token, block on the explicit re-trust surface — never keep talking.
    let m = machine_online_with_refresh(SilentChallengeOutcome::IdentityChanged {
        host: "nest.example".into(),
        pinned_hex: "aa".repeat(32),
        seen_hex: Some("bb".repeat(32)),
        fork: false,
    })
    .await;

    m.refresh_token().await;

    assert!(matches!(
        m.snapshot().phase,
        LaunchPhase::IdentityChanged { .. }
    ));
    assert_eq!(m.snapshot().token, TokenStatus::Expired);
    assert!(m.current_bearer().is_none());
}
