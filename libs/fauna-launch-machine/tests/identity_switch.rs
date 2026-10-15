//! The identity a launch machine publishes belongs to the account it was
//! confirmed for. Android reuses one machine across in-process account
//! switches (and tui keeps one for the app's life), so an identity left
//! standing by account A must never read as account B's
//! (`transport-connection.md` § The dial budget; `onboarding.md` § Multi-account).
//!
//! Each test pins one guard: reverting that guard — and only that guard —
//! reddens it.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    InMemoryPersistence, LaunchMachine, LaunchPhase, MockAuthConnector, NullObserver,
    SilentChallengeOutcome,
};
use fauna_protocol::auth::VerifyReply;

const ACCOUNT_A: [u8; 32] = [0x42; 32];
const ACCOUNT_B: [u8; 32] = [0x43; 32];

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

fn transient() -> SilentChallengeOutcome {
    SilentChallengeOutcome::Transient {
        error: "network down".into(),
    }
}

/// A machine on account A whose first `start()` confirmed `alice`, with
/// `then` queued as the next challenge's answer.
async fn confirmed_a_then(
    then: SilentChallengeOutcome,
) -> (Arc<LaunchMachine>, Arc<InMemoryPersistence>) {
    let p = Arc::new(
        InMemoryPersistence::new()
            .with_identity(ACCOUNT_A.to_vec())
            .with_nest_url("https://nest.example"),
    );
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()))
        .push_silent_challenge(then);
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));
    m.start().await;
    assert_eq!(m.snapshot().identity.expect("A confirmed").handle, "alice");
    assert!(m.subscribe_identity().borrow().is_some());
    (m, p)
}

fn assert_no_identity(m: &LaunchMachine) {
    assert_eq!(
        m.snapshot().identity,
        None,
        "snapshot still names the old account"
    );
    assert_eq!(
        *m.subscribe_identity().borrow(),
        None,
        "the self-identity watch still names the old account"
    );
}

/// Launch path: the persisted account changed under a reused machine and the
/// new account's challenge ends `Offline`.
#[tokio::test]
async fn switching_account_then_ending_offline_publishes_no_identity() {
    let (m, p) = confirmed_a_then(transient()).await;
    p.switch_identity(ACCOUNT_B.to_vec());
    m.start().await;
    assert!(matches!(
        m.snapshot().phase,
        LaunchPhase::Offline { transient: true }
    ));
    assert_no_identity(&m);
}

/// `start()`'s own reset: the account is unchanged, so the `Offline` guard
/// would keep the identity — only the reset can account for `None`.
#[tokio::test]
async fn start_clears_the_identity_until_its_own_challenge_confirms_one() {
    let (m, _p) = confirmed_a_then(transient()).await;
    m.start().await;
    assert_no_identity(&m);
}

/// Refresh path, same account: the identity survives an offline dip (the
/// "survives offline dips" contract the guard must not break).
#[tokio::test]
async fn a_refresh_dip_of_the_same_account_keeps_the_identity() {
    let (m, _p) = confirmed_a_then(transient()).await;
    m.refresh_token().await;
    assert!(matches!(
        m.snapshot().phase,
        LaunchPhase::Offline { transient: true }
    ));
    assert_eq!(m.snapshot().identity.expect("kept").handle, "alice");
    assert!(m.subscribe_identity().borrow().is_some());
}

/// Refresh path after an account switch: confirmed for A, the store now names B.
#[tokio::test]
async fn a_refresh_dip_after_an_account_switch_clears_the_identity() {
    let (m, p) = confirmed_a_then(transient()).await;
    p.switch_identity(ACCOUNT_B.to_vec());
    m.refresh_token().await;
    assert_no_identity(&m);
}

#[tokio::test]
async fn a_changed_nest_identity_verdict_clears_the_identity() {
    let (m, _p) = confirmed_a_then(SilentChallengeOutcome::IdentityChanged {
        host: "https://nest.example".into(),
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
    assert_no_identity(&m);
}

#[tokio::test]
async fn a_superseded_verdict_clears_the_identity() {
    let (m, _p) = confirmed_a_then(SilentChallengeOutcome::Superseded {
        new_actor_id_hex: "cc".repeat(32),
    })
    .await;
    m.refresh_token().await;
    assert_no_identity(&m);
}
