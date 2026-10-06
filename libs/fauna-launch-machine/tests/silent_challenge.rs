//! Tests the silent-challenge fast path's state transitions against a scripted
//! [`MockAuthConnector`] (no network).
//!
//! Per docs/goal/behavior/onboarding.md § App-launch routing (lines 246–252):
//! - happy path: challenge → verify → Online with token + handle/domain/tier
//! - `fauna.auth.not_registered` on verify → WizardAt(InviteRequest | ClaimCode)
//!   depending on the `fauna.setup.status` claimed probe
//! - transient network/server failure → Offline { transient: true }
//!
//! The `fauna.auth.{challenge,verify}` ceremony itself is unit-tested in
//! `fauna-protocol::auth` (the shared `run_silent_challenge`); the real WS
//! round-trip in the tier_3
//! `bins/fauna-nest/tests/launch_machine_auth_roundtrip.rs`.
//!
//! Run with `cargo test -p fauna-launch-machine --features test-helpers,test-observer`.
#![cfg(all(feature = "test-helpers", feature = "test-observer"))]

use std::sync::Arc;

use fauna_launch_machine::{
    ClaimProbe, InMemoryPersistence, LaunchMachine, LaunchPhase, LaunchWizardEntry,
    MockAuthConnector, NullObserver, SilentChallengeOutcome, TokenStatus,
};
use fauna_protocol::auth::VerifyReply;

fn test_secret() -> [u8; 32] {
    [0x42; 32]
}

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
            .with_identity(test_secret().to_vec())
            .with_nest_url("https://nest.example"),
    )
}

#[tokio::test]
async fn happy_path_lands_online_with_metadata() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let p = persistence();
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));
    m.start().await;

    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    // The deadline is anchored on THIS clock at receipt (`expires_in`), never
    // the nest's absolute `expires_at`.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    match m.snapshot().token {
        TokenStatus::Valid { expires_at_secs } => assert!(
            (now + 3600 - 5..=now + 3600 + 5).contains(&expires_at_secs),
            "deadline must be ~now+3600 on this clock, got {expires_at_secs} (now {now})"
        ),
        other => panic!("expected Valid, got {other:?}"),
    }
    assert_eq!(m.current_bearer(), Some("actor.opaque".into()));
    let saved = p.authenticated.lock().unwrap().clone();
    assert_eq!(
        saved,
        Some((
            "https://nest.example".into(),
            "alice".into(),
            "nest.example".into(),
            "free".into()
        ))
    );
}

/// The resolved identity reaches the OBSERVABLE snapshot, not only the store
/// (`conversations.md` § State & data shape → *Self-address: live, never
/// baked*).
///
/// `save_authenticated` writes handle/domain/tier into the long-term store, but
/// a store write is not an event: an app that only observes `on_changed()` — the
/// native apps' one live identity channel — could not see that the identity had
/// resolved or changed, and so had nothing to drive `set_self_address` from. On
/// android that was the whole gap: its `<handle>@<domain>` was assembled from a
/// legacy cache mirrored once at boot, i.e. from the PREVIOUS run's challenge.
#[tokio::test]
async fn the_resolved_identity_lands_on_the_snapshot() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let p = persistence();
    let m =
        LaunchMachine::new_with_connector(Arc::new(NullObserver), p.clone(), Arc::new(connector));

    assert_eq!(
        m.snapshot().identity,
        None,
        "before the challenge resolves there is no nest-confirmed identity to report — the app \
         keeps rendering its own cached 'Welcome back' value, and a send still refuses locally"
    );

    m.start().await;

    let identity = m.snapshot().identity.expect("resolved identity");
    assert_eq!(identity.handle, "alice");
    assert_eq!(identity.domain, "nest.example");
    assert_eq!(identity.tier, "free");
}

/// The field is **replaced** on every resolution, never written once — the case
/// the § calls "a server-side handle rename". An app that re-reads on
/// `on_changed()` therefore heals without a restart, which is the entire point:
/// a write-once field would reproduce the baked-address bug one level up.
#[tokio::test]
async fn a_later_resolution_replaces_the_snapshot_identity() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()))
        .push_silent_challenge(SilentChallengeOutcome::Success(VerifyReply {
            handle: "alice2".into(),
            domain: "other.example".into(),
            ..verify_reply()
        }));
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;
    assert_eq!(m.snapshot().identity.unwrap().handle, "alice");

    // A second hydrate — the machine re-runs the challenge from any state, so
    // this is the same code path a relaunch or a re-auth takes.
    m.start().await;
    let identity = m.snapshot().identity.expect("re-resolved identity");
    assert_eq!(identity.handle, "alice2");
    assert_eq!(identity.domain, "other.example");
}

#[tokio::test]
async fn not_registered_on_a_claimed_nest_is_the_sign_in_refused_surface_not_the_wizard() {
    // `onboarding.md` § App-launch routing — the previously-signed-in row. The
    // stored identity + nest_url this row runs on say the app signed in here
    // before, so an opaque `not_registered` from a CLAIMED nest means the nest
    // no longer signs this identity in (suspended, removed — the client cannot
    // tell, by design). Never the invite wizard: the nest already holds the
    // account, and a suspended actor's invite submit is refused outright.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::NotRegistered)
        .with_claim_probe(ClaimProbe::Claimed);
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    let snap = m.snapshot();
    assert_eq!(
        snap.phase,
        LaunchPhase::Offline { transient: false },
        "terminal on the existing phase, on the `account_index_refusal` pattern — \
         an app that never reads the new field still stops and shows `last_error`"
    );
    assert!(
        snap.sign_in_refused,
        "the additive side channel is what upgrades a dead end into the honest surface"
    );
    assert_eq!(
        snap.last_error.as_deref(),
        Some(fauna_i18n::strings::onboarding::launch::SIGN_IN_REFUSED),
        "localized copy, never the raw wire code"
    );
    assert!(m.current_bearer().is_none());
}

#[tokio::test]
async fn retry_from_sign_in_refused_reruns_the_ceremony_and_lands_online_once_restored() {
    // The admin's restore is a button on THEIR app; the way back in for the
    // user is Retry — so unlike every other terminal offline, this one honours
    // `retry_silent_challenge()`. First verify refuses, the retry succeeds.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::NotRegistered)
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()))
        .with_claim_probe(ClaimProbe::Claimed);
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;
    assert!(m.snapshot().sign_in_refused);

    m.retry_silent_challenge().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, LaunchPhase::Online);
    assert!(
        !snap.sign_in_refused,
        "the refusal clears with the state that carried it"
    );
    assert_eq!(snap.last_error, None);
    assert!(m.current_bearer().is_some());
}

#[tokio::test]
async fn not_registered_with_unclaimed_routes_to_claim_code() {
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::NotRegistered)
        .with_claim_probe(ClaimProbe::Unclaimed);
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::WizardAt {
            entry: LaunchWizardEntry::ClaimCode
        }
    );
    assert!(
        !m.snapshot().sign_in_refused,
        "an UNCLAIMED nest is the factory-reset-from-another-device case: the claim \
         the user owes, never a refusal"
    );
}

#[tokio::test]
async fn not_registered_with_probe_failure_falls_back_to_sign_in_refused() {
    // An unreachable claim probe → assume claimed → the refused surface (the
    // safer default *for this caller*; a claim_code route on a nest that's
    // actually claimed would mislead the user — and verify DID answer, so the
    // nest is up). Note the pending-factory-reset reconcile takes the opposite
    // default on `Unreachable` — it keeps its slot rather than trusting
    // "claimed" — which is why the probe reports three outcomes, not a bool.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::NotRegistered)
        .with_claim_probe(ClaimProbe::Unreachable);
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    let snap = m.snapshot();
    assert_eq!(snap.phase, LaunchPhase::Offline { transient: false });
    assert!(snap.sign_in_refused);
}

#[tokio::test]
async fn transient_failure_yields_transient_offline() {
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "server error / unreachable nest".into(),
        });
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    assert_eq!(m.snapshot().phase, LaunchPhase::Offline { transient: true });
}

#[tokio::test]
async fn retry_silent_challenge_after_transient_recovers_to_online() {
    // First attempt transient, second (the retry) succeeds.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Transient {
            error: "transient".into(),
        })
        .push_silent_challenge(SilentChallengeOutcome::Success(VerifyReply {
            token: "post-retry.token".into(),
            ..verify_reply()
        }));
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    assert_eq!(
        m.snapshot().phase,
        LaunchPhase::Offline { transient: true },
        "expected first attempt to land in transient Offline; got {:?}",
        m.snapshot().phase,
    );

    // User clicks "retry" → LaunchMachine re-runs the silent challenge.
    m.retry_silent_challenge().await;

    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    assert_eq!(m.current_bearer(), Some("post-retry.token".into()));
}

#[tokio::test]
async fn retry_silent_challenge_outside_transient_offline_is_noop() {
    // Already Online — retry shouldn't redo the silent challenge.
    let connector = MockAuthConnector::new()
        .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply()));
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);

    let phase_before = m.snapshot().phase.clone();
    let bearer_before = m.current_bearer();
    m.retry_silent_challenge().await;
    assert_eq!(m.snapshot().phase, phase_before);
    assert_eq!(m.current_bearer(), bearer_before);
}

#[tokio::test]
async fn retry_silent_challenge_after_terminal_offline_is_noop() {
    // Offline{transient:false} is terminal (e.g. malformed secret). Retry
    // should be a no-op — the user's intervention is needed.
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::SecretInvalid {
            error: "expected 32-byte secret".into(),
        });
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;
    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected terminal Offline, got {other:?}"),
    }

    let phase_before = m.snapshot().phase.clone();
    m.retry_silent_challenge().await;
    assert_eq!(m.snapshot().phase, phase_before);
}

#[tokio::test]
async fn nest_outdated_lands_terminal_offline_and_retry_is_noop() {
    // The most realistic version-mismatch path: a degraded nest rejects the
    // very first connect with `fauna.nest.outdated`. The client must land in a
    // *terminal* (non-transient) Offline with the localized actionable banner —
    // NOT the transient/retry state a connectivity blip gets — and a Retry must
    // be a no-op (updating the nest is the only fix). version-compatibility.md
    // Dim 4 / Track 3 client proof.
    let banner = fauna_protocol::RpcError::nest_outdated()
        .localized()
        .to_string();
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::NeedsUpdate {
            message: banner.clone(),
        });
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected terminal Offline {{ transient: false }}, got {other:?}"),
    }
    assert_eq!(m.snapshot().last_error.as_deref(), Some(banner.as_str()));
    assert!(m.current_bearer().is_none());

    // Retry is a no-op on a terminal offline — the nest, not the client, must
    // change. (No second silent_challenge is queued, so a retry that re-ran the
    // ceremony would land Transient and fail the assertion.)
    let phase_before = m.snapshot().phase.clone();
    m.retry_silent_challenge().await;
    assert_eq!(m.snapshot().phase, phase_before);
}

#[tokio::test]
async fn secret_invalid_lands_offline_terminal() {
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::SecretInvalid {
            error: "expected 32-byte secret, got 16 bytes".into(),
        });
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    match m.snapshot().phase {
        LaunchPhase::Offline { transient: false } => {}
        other => panic!("expected Offline {{ transient: false }}, got {other:?}"),
    }
    assert!(m.snapshot().last_error.is_some());
}

// ---------------------------------------------------------------------------
// The nest-identity-pin (TOFU) seam — security.md § Transport trust. A changed/withdrawn pinned identity BLOCKS auto-entry on the
// `launch_identity_changed` surface; the only ways out are the explicit
// re-trust (`trust_nest_identity`) and the wizard fallthrough. Never a retry
// loop, never a silent re-pin.
// ---------------------------------------------------------------------------

fn identity_changed() -> SilentChallengeOutcome {
    SilentChallengeOutcome::IdentityChanged {
        host: "https://nest.example".into(),
        pinned_hex: "aa".repeat(32),
        seen_hex: Some("bb".repeat(32)),
        fork: false,
    }
}

#[tokio::test]
async fn identity_changed_blocks_on_the_warning_surface_and_ignores_retry() {
    let connector = MockAuthConnector::new().push_silent_challenge(identity_changed());
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    match m.snapshot().phase {
        LaunchPhase::IdentityChanged {
            ref pinned_hex,
            ref seen_hex,
        } => {
            assert_eq!(pinned_hex, &"aa".repeat(32));
            assert_eq!(seen_hex.as_deref(), Some("bb".repeat(32).as_str()));
        }
        other => panic!("expected IdentityChanged, got {other:?}"),
    }
    // No bearer over an untrusted connection.
    assert!(m.current_bearer().is_none());
    assert!(m.snapshot().last_error.is_some());

    // `retry_silent_challenge` acts only from Offline{transient:true} — the
    // warning is NOT a retry surface (a retry would just re-warn; recovery is
    // the explicit re-trust or the wizard fallthrough).
    let phase_before = m.snapshot().phase.clone();
    m.retry_silent_challenge().await;
    assert_eq!(m.snapshot().phase, phase_before);
}

#[tokio::test]
async fn withdrawn_identity_surfaces_with_no_seen_fingerprint() {
    let connector =
        MockAuthConnector::new().push_silent_challenge(SilentChallengeOutcome::IdentityChanged {
            host: "https://nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: None,
            fork: false,
        });
    let m = LaunchMachine::new_with_connector(
        Arc::new(NullObserver),
        persistence(),
        Arc::new(connector),
    );
    m.start().await;

    match m.snapshot().phase {
        LaunchPhase::IdentityChanged { seen_hex: None, .. } => {}
        other => panic!("expected IdentityChanged with seen_hex None, got {other:?}"),
    }
}

#[tokio::test]
async fn trust_nest_identity_forgets_the_pin_and_relands_online() {
    let mock = Arc::new(
        MockAuthConnector::new()
            .push_silent_challenge(identity_changed())
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply())),
    );
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), persistence(), mock.clone());
    m.start().await;
    assert!(matches!(
        m.snapshot().phase,
        LaunchPhase::IdentityChanged { .. }
    ));
    assert!(mock.forgotten_pins().is_empty());

    m.trust_nest_identity().await;

    // The pin was forgotten through the connector's trust seam, keyed by the
    // nest URL the challenge ran against, and the re-run re-TOFU'd + landed.
    assert_eq!(
        mock.forgotten_pins(),
        vec!["https://nest.example".to_string()]
    );
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
    assert!(m.current_bearer().is_some());
    assert_eq!(m.snapshot().last_error, None);
}

#[tokio::test]
async fn trust_nest_identity_is_a_no_op_outside_the_warning_surface() {
    let mock = Arc::new(
        MockAuthConnector::new()
            .push_silent_challenge(SilentChallengeOutcome::Success(verify_reply())),
    );
    let m = LaunchMachine::new_with_connector(Arc::new(NullObserver), persistence(), mock.clone());
    m.start().await;
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);

    // From Online the action must not touch the pin — a pin is only ever
    // forgotten via the user-approved warning-surface action.
    m.trust_nest_identity().await;
    assert!(mock.forgotten_pins().is_empty());
    assert_eq!(m.snapshot().phase, LaunchPhase::Online);
}
