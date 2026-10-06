//! In-process WS-RPC handler tests for the pre-identity auth-bootstrap kinds
//! `fauna.auth.{handshake,challenge,verify}` — dispatch each registered
//! handler directly (no socket), exercising the shared `auth_core` ceremony
//! and the `AuthError` → `RpcError` mapping. Socket-level coverage (the
//! anonymous endpoint + the `unauthenticated` allowlist gate) lives in
//! `pre_identity_ws.rs`. Slice tracked internally.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use common::FixedSpki;
use ed25519_dalek::Signer;

use fauna_core::identity::ActorKeypair;
use fauna_nest::auth_handlers::register_auth_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::auth::{
    ChallengeReply, ChallengeRequest, HandshakeReply, HandshakeRequest, VerifyReply, VerifyRequest,
};
use fauna_protocol::node_policy::RegistrationMode;
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_auth_handlers(&mut b);
    b.build()
}

/// `AppState` pinned to a given registration posture. Admission does **not**
/// depend on it — an unknown actor is refused in every mode — but a test that
/// claims "in every mode" has to actually set each one.
async fn state_in_mode(db: Arc<CacheDb>, mode: RegistrationMode) -> Arc<AppState> {
    let st = Arc::new(AppState::for_test(db));
    *st.registration_mode.write().await = (mode, None);
    st
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    // The anonymous connection binds no actor — the handler reads it from the
    // signed payload, so the dispatcher's actor arg is irrelevant here.
    (meta.handler)(state, [0u8; 32], payload).await
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

/// A handshake payload addressed to `state`'s own identity, with a fixed
/// per-request nonce — the shape every production signer sends
/// (`login.md` § Binding the nest).
fn handshake_payload(state: &AppState, kp: &ActorKeypair, timestamp_ms: u64) -> Bytes {
    handshake_payload_with_nonce(state, kp, timestamp_ms, &[0x42u8; 32])
}

/// Handshake payload carrying a per-request `client_nonce` — folded into the
/// signed message (`handshake_signed_message`) after the nest's identity,
/// exactly as the production signers do, so the nest verifies
/// `actor_id ‖ ts ‖ nest_id ‖ nonce`.
fn handshake_payload_with_nonce(
    state: &AppState,
    kp: &ActorKeypair,
    timestamp_ms: u64,
    nonce: &[u8],
) -> Bytes {
    let nest_id = state.bound_identity();
    let msg = fauna_protocol::auth::handshake_signed_message(
        &kp.actor_id().0,
        timestamp_ms,
        &nest_id,
        nonce,
    );
    let sig = kp.signing_key().sign(&msg);
    let req = HandshakeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: timestamp_ms,
        signature: hex::encode(sig.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        nest_id: hex::encode(nest_id),
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

/// `AppState` configured like a TLS nest: a deployment signing key + a served
/// cert whose SPKI is `spki`.
fn tls_nest_state(
    db: Arc<CacheDb>,
    signing_key: &ed25519_dalek::SigningKey,
    spki: [u8; 32],
) -> Arc<AppState> {
    Arc::new(AppState {
        nest_signing_key: Some(signing_key.clone()),
        served_cert_spki: Some(Arc::new(FixedSpki(spki))),
        ..AppState::for_test(db)
    })
}

fn challenge_payload(kp: &ActorKeypair) -> Bytes {
    let req = ChallengeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

fn verify_payload(state: &AppState, kp: &ActorKeypair, nonce_hex: &str) -> Bytes {
    let nonce: [u8; 32] = hex::decode(nonce_hex).unwrap().try_into().unwrap();
    let nest_id = state.bound_identity();
    let msg =
        fauna_protocol::auth::challenge_verify_signed_message(&kp.actor_id().0, &nonce, &nest_id);
    let sig = kp.signing_key().sign(&msg);
    let req = VerifyRequest {
        actor_id: hex::encode(kp.actor_id().0),
        nonce: nonce_hex.to_string(),
        signature: hex::encode(sig.to_bytes()),
        client_nonce: None,
        nest_id: hex::encode(nest_id),
        extra: Default::default(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

#[tokio::test]
async fn handshake_mints_token_for_registered_actor() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();

    let out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload(&state, &kp, now_ms()),
    )
    .await
    .expect("handshake ok");
    let reply: HandshakeReply = decode(&out).unwrap();
    assert!(!reply.token.is_empty());
    assert!(reply.expires_at > 0);
    // The minted token validates against the store and resolves to the actor.
    let resolved = state.auth.token_store.validate(&reply.token).await;
    assert_eq!(resolved.expect("token valid").0, kp.actor_id().0);
}

/// auth-handshake finding #1: two concurrent handshakes for the **same actor** at
/// the **same timestamp** must both mint a token when each carries a distinct
/// `client_nonce`. The nonce is folded into the signed message, so the two
/// (otherwise byte-identical, deterministic-Ed25519) signatures differ and the
/// single-use replay guard does not reject the second as a replay. Before the
/// nonce-fold fix the second collided → `fauna.auth.signature_failed`.
#[tokio::test]
async fn concurrent_same_actor_same_timestamp_distinct_nonce_both_mint() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();

    // Same actor, same timestamp, DIFFERENT per-request nonces.
    let ts = now_ms();
    let out_a = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload_with_nonce(&state, &kp, ts, &[0xa1u8; 32]),
    )
    .await
    .expect("first concurrent handshake mints");
    let out_b = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload_with_nonce(&state, &kp, ts, &[0xb2u8; 32]),
    )
    .await
    .expect("second concurrent handshake mints (no replay-guard collision)");

    let reply_a: HandshakeReply = decode(&out_a).unwrap();
    let reply_b: HandshakeReply = decode(&out_b).unwrap();
    assert!(!reply_a.token.is_empty() && !reply_b.token.is_empty());
    assert_ne!(reply_a.token, reply_b.token, "distinct sessions minted");
    // Both tokens validate to the actor.
    for t in [&reply_a.token, &reply_b.token] {
        let resolved = state.auth.token_store.validate(t).await;
        assert_eq!(resolved.expect("token valid").0, kp.actor_id().0);
    }
}

/// Conversely, a signature is still single-use: a verbatim replay of one
/// request (byte-identical signature — same actor, timestamp, nest and nonce)
/// is rejected — the replay guard the nonce fix preserves.
#[tokio::test]
async fn verbatim_replay_is_rejected() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "bob")
        .await
        .unwrap();

    let payload = handshake_payload(&state, &kp, now_ms());
    dispatch(&r, state.clone(), "fauna.auth.handshake", payload.clone())
        .await
        .expect("first mints");
    let err = dispatch(&r, state, "fauna.auth.handshake", payload)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.signature_failed");
}

/// **The auto-provision removal.** A valid self-signed token proves key
/// possession — never admission. An actor with no `users` row is refused in
/// **every** registration mode, and no row is created as a side effect.
///
/// This replaces `handshake_auto_registers_when_registration_open`, which pinned
/// the opposite. The old branch minted a handle-less `free` ghost on first
/// handshake, bypassing the invite gate, the free-tier cap, and `Closed` alike —
/// an account `public-mode.md` § User Registration says cannot exist (registering
/// *is* choosing a handle). `Open` here is the sharp end: even a nest that admits
/// anyone admits them through the **ceremony**, not through a handshake.
#[tokio::test]
async fn handshake_refuses_an_unregistered_actor_in_every_mode() {
    for mode in [
        RegistrationMode::Open,
        RegistrationMode::InviteRequired,
        RegistrationMode::Closed,
    ] {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = state_in_mode(db.clone(), mode).await;
        let r = router();
        let kp = ActorKeypair::generate();

        let err = dispatch(
            &r,
            state.clone(),
            "fauna.auth.handshake",
            handshake_payload(&state, &kp, now_ms()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code,
            "fauna.auth.not_registered",
            "an unknown actor must be refused a token in {} mode",
            mode.as_wire_str()
        );
        assert!(
            db.get_user(&kp.actor_id().0).await.unwrap().is_none(),
            "a refused handshake must not create a users row in {} mode",
            mode.as_wire_str()
        );
    }
}

/// The other half: a *registered* actor still gets its token in every mode. The
/// posture gates **registration**, not authentication — else closing registration
/// would lock out every existing user.
#[tokio::test]
async fn handshake_admits_a_registered_actor_in_every_mode() {
    for mode in [
        RegistrationMode::Open,
        RegistrationMode::InviteRequired,
        RegistrationMode::Closed,
    ] {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = state_in_mode(db.clone(), mode).await;
        let r = router();
        let kp = ActorKeypair::generate();
        db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
            .await
            .unwrap();

        let out = dispatch(
            &r,
            state.clone(),
            "fauna.auth.handshake",
            handshake_payload(&state, &kp, now_ms()),
        )
        .await
        .unwrap_or_else(|e| {
            panic!(
                "a registered actor must authenticate in {} mode: {e:?}",
                mode.as_wire_str()
            )
        });
        let _reply: HandshakeReply = decode(&out).unwrap();
    }
}

#[tokio::test]
async fn handshake_rejects_bad_signature() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();

    // Sign with the wrong key but claim kp's actor_id.
    let ts = now_ms();
    let wrong = ActorKeypair::generate();
    let mut msg = Vec::new();
    msg.extend_from_slice(&kp.actor_id().0);
    msg.extend_from_slice(&ts.to_be_bytes());
    let sig = wrong.signing_key().sign(&msg);
    let req = HandshakeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(vec![0x42u8; 32]),
        nest_id: hex::encode(state.bound_identity()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    let err = dispatch(&r, state, "fauna.auth.handshake", payload)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.signature_failed");
}

/// A bad signature must NOT trigger account creation — `direct_auth_core` verifies
/// the signature (step 3) before any db write, so an unauthenticated request can
/// never create a user row. (Ported from the deleted HTTP-twin test
/// `auth_token::bad_signature_not_auto_registered_when_no_require_registration`.)
/// Belt-and-braces since the auto-provision branch was deleted: nothing on this
/// path creates a user row now, whatever the signature.
#[tokio::test]
async fn handshake_bad_signature_does_not_auto_register() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();

    // Sign with the wrong key but claim kp's actor_id.
    let ts = now_ms();
    let wrong = ActorKeypair::generate();
    let mut msg = Vec::new();
    msg.extend_from_slice(&kp.actor_id().0);
    msg.extend_from_slice(&ts.to_be_bytes());
    let sig = wrong.signing_key().sign(&msg);
    let req = HandshakeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(vec![0x42u8; 32]),
        nest_id: hex::encode(state.bound_identity()),
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());

    let err = dispatch(&r, state, "fauna.auth.handshake", payload)
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.auth.signature_failed");
    assert!(
        db.get_user(&kp.actor_id().0).await.unwrap().is_none(),
        "bad signature must not create a user row"
    );
}

/// A registered-then-suspended (evicted) actor is rejected: `direct_auth_core`
/// step 4 runs `check_actor_active` — now unconditionally, in every mode — which
/// fails for a suspended actor → `AuthError::NotRegistered` →
/// `fauna.auth.not_registered`.
/// (Ported from the deleted HTTP-twin test `auth_token::suspended_actor_rejected`,
/// where the same path mapped to HTTP 403.)
#[tokio::test]
async fn handshake_rejects_suspended_actor() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    // Suspend via eviction (0-day warning = immediate suspend).
    db.start_eviction(&kp.actor_id().0, "test", "terms", 0, 14)
        .await
        .unwrap();
    db.transition_evictions().await.unwrap();

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload(&state, &kp, now_ms()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.auth.not_registered");
}

#[tokio::test]
async fn handshake_rejects_stale_timestamp() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();

    let stale = now_ms() - 300_000; // 5 minutes ago, well past ±30 s.
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload(&state, &kp, stale),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.auth.timestamp_drift");
}

#[tokio::test]
async fn challenge_then_verify_mints_token_with_metadata() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "bob")
        .await
        .unwrap();

    let c_out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.challenge",
        challenge_payload(&kp),
    )
    .await
    .expect("challenge ok");
    let challenge: ChallengeReply = decode(&c_out).unwrap();
    assert_eq!(challenge.nonce.len(), 64);
    assert!(challenge.expires_in > 0);

    let v_out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(&state, &kp, &challenge.nonce),
    )
    .await
    .expect("verify ok");
    let verify: VerifyReply = decode(&v_out).unwrap();
    assert!(!verify.token.is_empty());
    assert_eq!(verify.tier, "free");
}

/// The silent-challenge `verify` reply's `domain` is the deployment's **live
/// identity domain** — `handle_domain()`, the projection of the primary
/// `mail_domains` row set at claim — not the stale `--handle-domain` boot seed.
/// A domainless-then-claimed box has an empty seed but a populated identity cache;
/// reading the raw `registration.handle_domain` field left it reporting `localhost`
/// (or the seed) for a handle it serves under the claimed domain
/// (mail-multidomain.md § Multi-domain handles; login.md § Silent Challenge).
#[tokio::test]
async fn challenge_then_verify_reports_live_identity_domain() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut inner = AppState::for_test(db.clone());
    // Stale CLI seed differing from the claimed identity — discriminates the
    // cache-first `handle_domain()` from the raw seed field.
    inner.auth.registration.handle_domain = Some("stale-seed.invalid".to_string());
    let state = Arc::new(inner);
    state
        .identity_domain
        .store(Some(Arc::new("claimed.example".to_string())));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "bob")
        .await
        .unwrap();

    let c_out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.challenge",
        challenge_payload(&kp),
    )
    .await
    .expect("challenge ok");
    let challenge: ChallengeReply = decode(&c_out).unwrap();

    let v_out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(&state, &kp, &challenge.nonce),
    )
    .await
    .expect("verify ok");
    let verify: VerifyReply = decode(&v_out).unwrap();
    assert_eq!(
        verify.domain, "claimed.example",
        "verify must report the live identity domain (handle_domain()), \
         not the stale --handle-domain seed"
    );
}

#[tokio::test]
async fn verify_rejects_unregistered_actor() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate(); // never create_user

    let c_out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.challenge",
        challenge_payload(&kp),
    )
    .await
    .expect("challenge ok");
    let challenge: ChallengeReply = decode(&c_out).unwrap();

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(&state, &kp, &challenge.nonce),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.auth.not_registered");
}

/// Verify is the mint every app-held bearer rides (`login.md` § Silent
/// Challenge), so a suspended actor must not re-mint through it — the handshake
/// has always refused one (`handshake_rejects_suspended_actor`), and a verify
/// that did not left suspension's "no new connection authenticates"
/// (`admin.md` § 2 Users → *Cutting a user off*) false for every app. The code stays the opaque `not_registered` the
/// handshake answers: no suspended-vs-unregistered oracle (`login.md` § Errors).
#[tokio::test]
async fn verify_rejects_suspended_actor() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "carol")
        .await
        .unwrap();
    assert!(
        db.suspend_user_now(&kp.actor_id().0, "test", "other")
            .await
            .unwrap(),
        "precondition: the suspension must land"
    );

    let c_out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.challenge",
        challenge_payload(&kp),
    )
    .await
    .expect("challenge ok");
    let challenge: ChallengeReply = decode(&c_out).unwrap();

    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(&state, &kp, &challenge.nonce),
    )
    .await
    .expect_err("verify minted a bearer for a suspended actor");
    assert_eq!(err.code, "fauna.auth.not_registered");
    assert_eq!(
        state
            .auth
            .token_store
            .list_sessions(&kp.actor_id())
            .await
            .len(),
        0,
        "the refusal must come before the mint, not after it"
    );
}

/// Run challenge → verify for `kp` against `state`.
async fn challenge_then_verify(
    r: &RpcRouter,
    state: &Arc<AppState>,
    kp: &ActorKeypair,
) -> Result<Bytes, RpcError> {
    let c_out = dispatch(
        r,
        state.clone(),
        "fauna.auth.challenge",
        challenge_payload(kp),
    )
    .await
    .expect("challenge ok");
    let challenge: ChallengeReply = decode(&c_out).unwrap();
    dispatch(
        r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(state, kp, &challenge.nonce),
    )
    .await
}

/// Verify enforces the account lock (`login.md` § Silent Challenge, ruled
/// 2026-09-25): a locked actor is refused with `fauna.auth.account_locked`
/// naming `locked_until`, and no bearer is minted.
#[tokio::test]
async fn verify_rejects_locked_actor_naming_locked_until() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "dana")
        .await
        .unwrap();
    let locked_until = now_ms() as i64 / 1000 + 3600;
    db.set_locked_until(&kp.actor_id().0, Some(locked_until))
        .await
        .unwrap();

    let err = challenge_then_verify(&r, &state, &kp)
        .await
        .expect_err("verify minted a bearer for a locked actor");
    assert_eq!(err.code, "fauna.auth.account_locked");
    assert_eq!(
        err.details.as_deref(),
        Some(&fauna_protocol::Value::Integer(locked_until as i128))
    );
    assert_eq!(
        state
            .auth
            .token_store
            .list_sessions(&kp.actor_id())
            .await
            .len(),
        0,
        "the refusal must come before the mint"
    );
}

/// An elapsed lock does not refuse.
#[tokio::test]
async fn verify_accepts_actor_whose_lock_has_elapsed() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "erin")
        .await
        .unwrap();
    db.set_locked_until(&kp.actor_id().0, Some(now_ms() as i64 / 1000 - 10))
        .await
        .unwrap();
    challenge_then_verify(&r, &state, &kp)
        .await
        .expect("an elapsed lock must not refuse");
}

/// Admins are exempt from the mint-time lock, mirroring the use-time exemption
/// (`api-layers.md` § bearer standing) until a cannot-lock-the-last-admin guard
/// exists (user ruling 2026-10-01).
#[tokio::test]
async fn verify_mints_for_a_locked_admin() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "frank")
        .await
        .unwrap();
    db.add_admin_actor(&kp.actor_id().0[..]).await.unwrap();
    db.set_locked_until(&kp.actor_id().0, Some(now_ms() as i64 / 1000 + 3600))
        .await
        .unwrap();
    challenge_then_verify(&r, &state, &kp)
        .await
        .expect("a locked admin keeps the verify mint");
}

#[tokio::test]
async fn handshake_signs_cert_binding_over_served_spki_and_nonce() {
    use ed25519_dalek::Verifier;

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let nest_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let spki = [0x5au8; 32];
    let state = tls_nest_state(db.clone(), &nest_key, spki);
    let r = router();
    let kp = ActorKeypair::generate();
    // A handshake never provisions an account (auto-provision is gone), so the
    // actor under test must be a real registered user — otherwise this asserts
    // `not_registered` instead of the cert binding it exists to pin.
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .unwrap();
    let nonce = [0x11u8; 32];

    let out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload_with_nonce(&state, &kp, now_ms(), &nonce),
    )
    .await
    .expect("handshake ok");
    let reply: HandshakeReply = decode(&out).unwrap();
    let binding = reply.cert_binding.expect("cert_binding present");

    // nest_actor_id is the deployment key's public half.
    assert_eq!(
        binding.nest_actor_id,
        hex::encode(nest_key.verifying_key().to_bytes())
    );
    // The SPKI in the binding is the one the nest serves, verbatim.
    assert_eq!(binding.spki_sha256.as_slice(), &spki);

    // The tagged signature verifies over `CERT_BINDING_V1 ‖ served_spki ‖
    // client_nonce` against the nest's identity — the channel-binding proof,
    // through the single-source message builder.
    let signed = fauna_protocol::auth::cert_binding_signed_message(&spki, &nonce);
    let sig = ed25519_dalek::Signature::from_slice(binding.tagged_sig.as_slice()).unwrap();
    nest_key
        .verifying_key()
        .verify(&signed, &sig)
        .expect("channel-binding signature verifies");
}

/// The nest binding (`login.md` § Binding the nest): a handshake signed for
/// ANOTHER nest — what a relaying box forwards — is refused before any
/// signature work, as a verdict about the blob's target. Mutating the
/// `nest_id` compare away leaves the signature check to refuse it (the message
/// is built over the box's own identity, never the request's), so both arms
/// of the defence are pinned: the code names the compare, the refusal itself
/// names the signature.
#[tokio::test]
async fn handshake_addressed_to_another_nest_is_refused() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .unwrap();
    let other_nest = [0x5fu8; 32];
    assert_ne!(other_nest, state.bound_identity());
    let ts = now_ms();
    let nonce = [0x42u8; 32];
    let msg =
        fauna_protocol::auth::handshake_signed_message(&kp.actor_id().0, ts, &other_nest, &nonce);
    let sig = kp.signing_key().sign(&msg);
    let req = HandshakeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        nest_id: hex::encode(other_nest),
        extra: Default::default(),
    };
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a handshake addressed to another nest must not mint");
    assert_eq!(err.code, "fauna.auth.invalid_request", "{err:?}");
    // And the very same blob re-addressed to this nest is not accepted either:
    // the signature named the other nest.
    let readdressed = HandshakeRequest {
        nest_id: hex::encode(state.bound_identity()),
        ..req
    };
    let err = dispatch(
        &r,
        state,
        "fauna.auth.handshake",
        Bytes::from(encode_canonical(&readdressed).unwrap().to_vec()),
    )
    .await
    .expect_err("a signature over another nest's identity must not verify here");
    assert_eq!(err.code, "fauna.auth.signature_failed", "{err:?}");
}

/// The same binding on the silent challenge — the ceremony every app-held
/// bearer is minted over, hourly: a verify signed for another nest is refused.
#[tokio::test]
async fn verify_addressed_to_another_nest_is_refused() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .unwrap();
    let out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.challenge",
        challenge_payload(&kp),
    )
    .await
    .expect("challenge ok");
    let chal: ChallengeReply = decode(&out).unwrap();
    let nonce: [u8; 32] = hex::decode(&chal.nonce).unwrap().try_into().unwrap();
    let other_nest = [0x5fu8; 32];
    let msg = fauna_protocol::auth::challenge_verify_signed_message(
        &kp.actor_id().0,
        &nonce,
        &other_nest,
    );
    let sig = kp.signing_key().sign(&msg);
    let req = VerifyRequest {
        actor_id: hex::encode(kp.actor_id().0),
        nonce: chal.nonce.clone(),
        signature: hex::encode(sig.to_bytes()),
        client_nonce: None,
        nest_id: hex::encode(other_nest),
        extra: Default::default(),
    };
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect_err("a verify addressed to another nest must not mint");
    assert_eq!(err.code, "fauna.auth.invalid_request", "{err:?}");
    // Re-addressed: the signature still names the other nest. The nonce is
    // untouched by the refusal (the compare runs before the consume), so the
    // genuine signature would still mint — pinned last, so this test cannot
    // pass on a nest that refuses everything.
    let readdressed = VerifyRequest {
        nest_id: hex::encode(state.bound_identity()),
        ..req
    };
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        Bytes::from(encode_canonical(&readdressed).unwrap().to_vec()),
    )
    .await
    .expect_err("a signature over another nest's identity must not verify here");
    assert_eq!(err.code, "fauna.auth.signature_failed", "{err:?}");
    let out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(&state, &kp, &chal.nonce),
    )
    .await
    .expect("the genuine, correctly addressed verify mints");
    let reply: VerifyReply = decode(&out).unwrap();
    assert!(!reply.token.is_empty());
}

#[tokio::test]
async fn handshake_omits_cert_binding_on_plain_http_nest() {
    // A nonce-carrying request to a nest with no served cert (plain HTTP dev)
    // still mints a token but cannot bind — cert_binding is absent.
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone())); // no signing key, no SPKI
    let r = router();
    let kp = ActorKeypair::generate();
    // A handshake never provisions an account (auto-provision is gone), so the
    // actor under test must be a real registered user — otherwise this asserts
    // `not_registered` instead of the cert binding it exists to pin.
    db.create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .unwrap();

    let out = dispatch(
        &r,
        state.clone(),
        "fauna.auth.handshake",
        handshake_payload_with_nonce(&state, &kp, now_ms(), &[0x22u8; 32]),
    )
    .await
    .expect("handshake ok");
    let reply: HandshakeReply = decode(&out).unwrap();
    assert!(reply.cert_binding.is_none());
}

#[tokio::test]
async fn verify_rejects_unissued_nonce() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let r = router();
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "carol")
        .await
        .unwrap();

    // Sign a nonce we never asked the nest to issue — signature is valid over
    // it, but `consume` finds no matching outstanding nonce.
    let bogus_nonce = hex::encode([0x7u8; 32]);
    let err = dispatch(
        &r,
        state.clone(),
        "fauna.auth.verify",
        verify_payload(&state, &kp, &bogus_nonce),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "fauna.auth.invalid_nonce");
}
