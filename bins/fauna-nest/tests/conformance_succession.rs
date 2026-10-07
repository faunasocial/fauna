//! **tier_3** — succession enforcement (identity-succession slice 3):
//! the pre-identity `fauna.recovery.succession.{submit,lookup}` kinds, the
//! one-transaction re-point, and the `superseded` refusal that makes a stolen
//! seed inert.
//!
//! Goal doc: `docs/goal/behavior/identity-succession.md` § Enforcement on the
//! home nest (`:66-72`) + § Re-key scope (`:90-102`).
//!
//! **The scenario under test is seed theft.** The thief holds the identity
//! seed — every signature the owner can make, the thief can make — so every
//! test here is written from the position that the seed is *shared*, and the
//! only asymmetry is the offline RecoveryKey. That shapes the whole suite:
//!
//! - the ceremony must work with **no session and through an active lockout**
//!   (the thief can revoke one and invoke the other);
//! - a statement without the RecoveryKey signature must be refused, however
//!   perfectly the seed signed the rest;
//! - after it lands, the old key must be refused *everywhere* it could still
//!   turn a signature into authority — handshake, challenge/verify, the
//!   renewal-grant mint, the emergency lockout, and inbound content;
//! - the handle, tier and admin role must be on the successor, and the
//!   revoked grants gone, or the recovery is cosmetic.
//!
//! Every assertion is on state, never on wall-clock timing (convention 14).

mod common;
use common::identity;
use common::signed_inbox_payload;
// Submitted on the ANONYMOUS connection throughout this suite — that is the
// property under test, not a shortcut.
use common::{submit_succession, succession_bytes};

use std::sync::Arc;

use ed25519_dalek::{Signer, SigningKey};
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{RecoveryKey, RecoveryKeyRegistration};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{
    account_handlers, auth_handlers, discovery_handlers, inbox_handlers, recovery_handlers,
};
use fauna_protocol::RpcError;
use fauna_protocol::generation_escrow::{
    EscrowDeleteReply, EscrowDeleteRequest, EscrowGetReply, EscrowGetRequest, EscrowPutReply,
    EscrowPutRequest, KIND_ESCROW_DELETE, KIND_ESCROW_GET, KIND_ESCROW_PUT,
};
use fauna_protocol::recovery::{
    OwedNest, RegistrationChainReply, RegistrationChainRequest, RegistrationSubmitReply,
    RegistrationSubmitRequest, SUCCESSION_OWED_SETTLE_KIND, SUCCESSION_STATUS_KIND,
    SuccessionLookupReply, SuccessionLookupRequest, SuccessionOwedSettleReply,
    SuccessionOwedSettleRequest, SuccessionStatusReply, SuccessionStatusRequest,
};
use serde_bytes::ByteBuf;

/// The anonymous connection's actor — what a pre-identity caller presents.
const ANON: [u8; 32] = [0u8; 32];

// ═════════════════════════════════════════════════════════════════════════════
// Harness
// ═════════════════════════════════════════════════════════════════════════════

async fn nest() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    recovery_handlers::register_recovery_handlers(&mut b);
    auth_handlers::register_auth_handlers(&mut b);
    account_handlers::register_account_handlers(&mut b);
    discovery_handlers::register_discovery_handlers(&mut b);
    inbox_handlers::register_inbox_handlers(&mut b);
    (b.build(), state)
}

/// An account with a handle and a registered RecoveryKey — the state every
/// succession starts from.
async fn account_with_recovery_key(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    actor: [u8; 32],
    recovery: &RecoveryKey,
    handle: &str,
) {
    state
        .db
        .create_user_with_handle(&actor, "free", handle, None)
        .await
        .expect("account created");
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: recovery.public(),
        seq: 1,
        created_at: Timestamp(1_753_000_000),
    };
    let signed = reg.sign(seed, recovery, None).expect("sign");
    let _: RegistrationSubmitReply = common::call(
        router,
        state,
        actor,
        "fauna.recovery.registration.submit",
        &RegistrationSubmitRequest {
            registration: ByteBuf::from(canonical_encode(&signed).expect("encode")),
            extra: Default::default(),
        },
    )
    .await
    .expect("registration lands");
}

/// Drive a real `fauna.auth.handshake` for `actor`, signing with `seed`.
async fn handshake(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    actor: [u8; 32],
) -> Result<fauna_protocol::auth::HandshakeReply, RpcError> {
    let ts = fauna_core::data::Timestamp::now_millis();
    let nest_id = state.bound_identity();
    // A fresh nonce per handshake, as every real client folds in: a fixed one
    // made two same-millisecond sign-ins byte-identical, and the replay guard
    // refused the second as `signature_failed`.
    let nonce: [u8; 32] = rand::random();
    let msg = fauna_protocol::auth::handshake_signed_message(&actor, ts, &nest_id, &nonce);
    let sig = seed.sign(&msg);
    common::call(
        router,
        state,
        ANON,
        "fauna.auth.handshake",
        &fauna_protocol::auth::HandshakeRequest {
            actor_id: hex::encode(actor),
            timestamp: ts,
            signature: hex::encode(sig.to_bytes()),
            client_nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
            nest_id: hex::encode(nest_id),
            extra: Default::default(),
        },
    )
    .await
}

/// Drive the emergency `fauna.account.lockout` for `actor`, signing with `seed`.
async fn lockout(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    actor: [u8; 32],
) -> Result<fauna_protocol::account::AccountLockoutReply, RpcError> {
    let ts = fauna_core::data::Timestamp::now_secs() as u64;
    let msg = fauna_protocol::account::account_lockout_signed_message(&actor, ts);
    let sig = seed.sign(&msg);
    common::call(
        router,
        state,
        ANON,
        "fauna.account.lockout",
        &fauna_protocol::account::AccountLockoutRequest {
            actor_id: hex::encode(actor),
            timestamp: ts,
            signature: hex::encode(sig.to_bytes()),
            ..Default::default()
        },
    )
    .await
}

// ═════════════════════════════════════════════════════════════════════════════
// Authorization — what the RecoveryKey buys, and what the seed alone does not
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_statement_without_the_recovery_signature_is_refused() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, _new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // The thief holds the seed and can sign both the old-key and successor
    // roles perfectly — but not the RecoveryKey role, which never touched a
    // device. Model that by signing the statement under a RecoveryKey the
    // account did not register.
    let thief_key = RecoveryKey::from_bytes([0x99; 32]);
    let forged = succession_bytes(&thief_key, old, &new_seed, Some(&seed), 2);

    let err = submit_succession(&router, &state, forged)
        .await
        .expect_err("a statement not signed by the REGISTERED RecoveryKey must be refused");
    assert_eq!(err.code, "fauna.recovery.signature_failed");

    // Nothing moved: the account is still the old identity's.
    assert!(state.db.succession_for(&old[..]).await.unwrap().is_none());
    assert_eq!(
        state.db.resolve_handle("alice").await.unwrap(),
        Some(old),
        "the handle must not move on a refused statement"
    );
}

#[tokio::test]
async fn a_statement_that_does_not_advance_the_chain_is_refused() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, _) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // The chain head is at seq 1. A statement at seq 1 is a replay of that
    // position; the archived-registration replay window the review
    // named is what this closes.
    let stale = succession_bytes(&recovery, old, &new_seed, None, 1);
    let err = submit_succession(&router, &state, stale)
        .await
        .expect_err("a non-advancing seq must be refused");
    assert_eq!(err.code, "fauna.recovery.signature_failed");
    assert!(state.db.succession_for(&old[..]).await.unwrap().is_none());
}

#[tokio::test]
async fn an_identity_with_no_registered_recovery_key_cannot_be_succeeded() {
    let (router, state) = nest().await;
    let (_seed, old) = identity(0x11);
    let (new_seed, _) = identity(0x33);
    state
        .db
        .create_user_with_handle(&old, "free", "alice", None)
        .await
        .unwrap();

    // "No RecoveryKey ⇒ no succession capability" (`identity-succession.md:109`
    // — never a seed-only succession path; between two seed holders there is no
    // winner, only DoS or a coin flip).
    let attempt = succession_bytes(
        &RecoveryKey::from_bytes([0x44; 32]),
        old,
        &new_seed,
        None,
        2,
    );
    let err = submit_succession(&router, &state, attempt)
        .await
        .expect_err("no registered RecoveryKey means no succession");
    assert_eq!(err.code, "fauna.recovery.not_registered");
}

#[tokio::test]
async fn a_missing_successor_signature_is_refused() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, _) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // `new_sig` is mandatory (`identity-succession.md:57`): it prevents
    // pointing an account at a key the claimant does not hold. Strip it.
    let bytes = succession_bytes(&recovery, old, &new_seed, None, 2);
    let mut signed: fauna_core::recovery::SignedIdentitySuccession =
        fauna_core::encoding::canonical_decode(&bytes).unwrap();
    signed.new_sig = vec![0u8; 64];
    let tampered = canonical_encode(&signed).unwrap();

    let err = submit_succession(&router, &state, tampered)
        .await
        .expect_err("a bad successor signature must be refused");
    assert_eq!(err.code, "fauna.recovery.signature_failed");
    assert!(state.db.succession_for(&old[..]).await.unwrap().is_none());
}

// ═════════════════════════════════════════════════════════════════════════════
// The ceremony works in the conditions the thief creates
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_succession_lands_through_an_active_lockout_and_with_no_session() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // The thief does exactly what the emergency channel lets any seed holder
    // do: locks the account. This is the state the real owner is in when they
    // reach for their recovery kit.
    lockout(&router, &state, &seed, old)
        .await
        .expect("the thief can invoke the lockout");
    assert!(
        state.db.get_locked_until(&old).await.unwrap().is_some(),
        "precondition: the account is locked"
    );
    // And the owner cannot sign in — so any bearer-gated remedy is unreachable.
    assert!(handshake(&router, &state, &seed, old).await.is_err());

    // The RecoveryKey-signed submission is exempt from both
    // (`identity-succession.md:66`): no bearer, straight through the lockout.
    let reply = submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 2),
    )
    .await
    .expect("succession lands through the lockout on an anonymous connection");
    assert_eq!(reply.new_actor_id.as_ref(), &new[..]);

    // And the successor is not born locked — the thief's lockout does not ride
    // across, or the recovered account would still be frozen.
    assert!(state.db.get_locked_until(&new).await.unwrap().is_none());
    handshake(&router, &state, &new_seed, new)
        .await
        .expect("the successor can sign in immediately");
}

#[tokio::test]
async fn a_succession_cancels_a_pending_seed_initiated_replacement() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, _) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // A thief who holds the seed parks a replacement of the RecoveryKey — the
    // 30-day window that would eventually hand them the recovery root too.
    let thief_key = RecoveryKey::from_bytes([0x99; 32]);
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(old),
        recovery_pubkey: thief_key.public(),
        seq: 2,
        created_at: Timestamp(1_753_100_000),
    };
    let record = canonical_encode(&reg.sign(&seed, &thief_key, None).unwrap()).unwrap();
    state
        .db
        .upsert_pending_replacement(&old[..], &record, &thief_key.public(), 2, 1_753_100_000)
        .await
        .expect("pending parked");
    assert!(
        state
            .db
            .get_pending_replacement(&old[..])
            .await
            .unwrap()
            .is_some(),
        "precondition: a replacement is pending"
    );

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 3),
    )
    .await
    .expect("succession lands");

    // Succession outranks (`identity-succession.md:70`) — and because the
    // cancel rides the same transaction, no sweep can race it.
    assert!(
        state
            .db
            .get_pending_replacement(&old[..])
            .await
            .unwrap()
            .is_none(),
        "the pending replacement must die in the succession transaction"
    );
}

#[tokio::test]
async fn an_identity_is_succeeded_only_once() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    let (other_seed, _other) = identity(0x44);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("first succession lands");

    // The old RecoveryKey keeps producing perfectly valid signatures forever —
    // retiring it is a *decision the nest stores*, not a property of the key.
    // So a second statement at a higher seq must be refused, or whoever holds
    // the retired kit could re-point the account away from the successor at any
    // later time.
    let err = submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &other_seed, None, 9),
    )
    .await
    .expect_err("a second succession of the same identity must be refused");
    assert_eq!(err.code, "fauna.recovery.already_succeeded");

    assert_eq!(state.db.resolve_handle("alice").await.unwrap(), Some(new));
}

// ═════════════════════════════════════════════════════════════════════════════
// The re-point (§ Re-key scope)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn the_account_handle_tier_and_admin_role_move_to_the_successor() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;
    state
        .db
        .add_admin_actor(&old[..])
        .await
        .expect("admin added");

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 2),
    )
    .await
    .expect("succession lands");

    // Handle (the two-step move) — this is also what makes `by_handle` resolve
    // the successor with no extra indirection.
    assert_eq!(state.db.resolve_handle("alice").await.unwrap(), Some(new));
    assert_eq!(
        state.db.get_handle(&new).await.unwrap().as_deref(),
        Some("alice")
    );

    // Tier (quota) rode across.
    assert_eq!(
        state
            .db
            .get_user(&new)
            .await
            .unwrap()
            .expect("successor account")
            .tier,
        "free"
    );

    // Admin role is future authority, so it moves — and must not be left
    // behind, or a stolen key would keep administering the box.
    assert!(state.db.is_admin(&new[..]).await.unwrap());
    assert!(!state.db.is_admin(&old[..]).await.unwrap());
}

/// Ownership moves in the succession transaction, and the actor-scoped
/// segment directory moves with it (`succession-aftermath.md` § Re-key scope,
/// the ownership blockquote): after the real submit, the successor owns the
/// reserved folder by ordinary reads and the mail segment files serve under
/// the successor's scope — no second authorization rule anywhere.
#[tokio::test]
async fn the_corpus_and_its_segment_directory_move_to_the_successor() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    state
        .db
        .create_folder("__config", &old)
        .await
        .expect("reserved set");
    let env = b"opaque-mail-envelope".to_vec();
    state
        .mail_segments
        .append_record_with_bucket(
            &old,
            fauna_cbor::Cid::of_dag_cbor(&env),
            &env,
            b"",
            "2026-08",
        )
        .await
        .expect("mail segment on disk");

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 2),
    )
    .await
    .expect("succession lands");

    // The reserved set is reachable under the successor exactly the way any
    // owner reaches their own set — and gone from the old id.
    assert!(
        state
            .db
            .get_folder_for_actor("__config", &new)
            .await
            .expect("lookup")
            .is_some()
    );
    assert!(
        state
            .db
            .get_folder_for_actor("__config", &old)
            .await
            .expect("lookup")
            .is_none()
    );

    // The segment files moved with the rows: the successor's scope serves
    // what the old identity wrote, and the old directory is gone.
    assert!(state.mail_segments.scope_dir(&new).exists());
    assert!(!state.mail_segments.scope_dir(&old).exists());
    let manifest = state
        .mail_segments
        .load_manifest(&new)
        .await
        .expect("successor manifest");
    assert_eq!(manifest.kind_manifest.live_segments, vec![1]);
}

#[tokio::test]
async fn by_handle_resolves_the_successor() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // Before: the directory points at the old identity.
    let before: fauna_protocol::discovery::ActorByHandleReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.actor.by_handle",
        &fauna_protocol::discovery::ActorByHandleRequest {
            handle: "alice".into(),
            domain: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("resolves");
    assert_eq!(before.actor_id, hex::encode(old));

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    // After: anyone resolving the handle reaches the successor
    // (`identity-succession.md:72`). Driven over the real pre-identity kind,
    // because that is how every peer and client asks.
    let after: fauna_protocol::discovery::ActorByHandleReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.actor.by_handle",
        &fauna_protocol::discovery::ActorByHandleRequest {
            handle: "alice".into(),
            domain: None,
            extra: Default::default(),
        },
    )
    .await
    .expect("resolves");
    assert_eq!(after.actor_id, hex::encode(new));
}

// ═════════════════════════════════════════════════════════════════════════════
// The refusal (`identity-succession.md:71`)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn the_old_key_is_refused_by_every_seed_signature_ceremony() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // The old key works before the succession — otherwise the assertions below
    // would pass for the wrong reason.
    handshake(&router, &state, &seed, old)
        .await
        .expect("precondition: the old key can sign in");

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 2),
    )
    .await
    .expect("succession lands");

    // 1. Handshake.
    let err = handshake(&router, &state, &seed, old)
        .await
        .expect_err("a superseded key must not mint");
    assert_eq!(err.code, RpcError::CODE_SUPERSEDED);
    // The refusal names the successor and where to fetch the proof — the whole
    // point of not collapsing it into the opaque `not_registered`.
    assert_eq!(err.superseded_by(), Some(new));

    // 2. Challenge/verify — the other seed-signature sign-in ceremony.
    let challenge: fauna_protocol::auth::ChallengeReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.auth.challenge",
        &fauna_protocol::auth::ChallengeRequest {
            actor_id: hex::encode(old),
            extra: Default::default(),
        },
    )
    .await
    .expect("a challenge is issued unconditionally");
    let nonce: [u8; 32] = hex::decode(&challenge.nonce).unwrap().try_into().unwrap();
    let nest_id = state.bound_identity();
    let msg = fauna_protocol::auth::challenge_verify_signed_message(&old, &nonce, &nest_id);
    let sig = seed.sign(&msg);
    let err: RpcError = common::call::<_, fauna_protocol::auth::VerifyReply>(
        &router,
        &state,
        ANON,
        "fauna.auth.verify",
        &fauna_protocol::auth::VerifyRequest {
            actor_id: hex::encode(old),
            nonce: challenge.nonce.clone(),
            signature: hex::encode(sig.to_bytes()),
            client_nonce: None,
            nest_id: hex::encode(nest_id),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("a superseded key must not mint via verify");
    assert_eq!(err.code, RpcError::CODE_SUPERSEDED);
    assert_eq!(err.superseded_by(), Some(new));

    // 3. The emergency lockout — named explicitly in the goal doc, and the one
    //    the thief would reach for after losing the account.
    let err = lockout(&router, &state, &seed, old)
        .await
        .expect_err("a superseded key must not lock the account");
    assert_eq!(err.code, RpcError::CODE_SUPERSEDED);
    assert_eq!(err.superseded_by(), Some(new));

    // And the successor is unaffected by all of it.
    handshake(&router, &state, &new_seed, new)
        .await
        .expect("the successor signs in normally");
}

#[tokio::test]
async fn live_sessions_of_the_old_identity_are_revoked() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, _) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // The thief is signed in right now — a live bearer, exactly the state a
    // theft response has to end.
    let reply = handshake(&router, &state, &seed, old)
        .await
        .expect("the thief holds a session");
    let token = reply.token.clone();
    assert!(
        state.auth.token_store.validate(&token).await.is_some(),
        "precondition: the bearer is live"
    );

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    assert!(
        state.auth.token_store.validate(&token).await.is_none(),
        "every session of the old identity must be revoked (`identity-succession.md:70`)"
    );
}

#[tokio::test]
async fn new_content_from_a_superseded_author_is_refused_at_the_inbox() {
    // Content writes are refused *independently of the token layer*
    // (`identity-succession.md:71`). That separation is the point: signature
    // verification here is self-describing, and the shared
    // `deliver_inbox_payload_core` this drives is the same core the
    // unauthenticated federation leg uses — where there is no bearer to have
    // revoked. Driving it as an actor the harness hands straight to the handler
    // models a bearer that outlived the revocation sweep, which is exactly the
    // case the table consult (rather than the sweep) has to cover.
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, _) = identity(0x33);
    let (recipient_seed, recipient) = identity(0x55);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;
    account_with_recovery_key(
        &router,
        &state,
        &recipient_seed,
        recipient,
        &RecoveryKey::from_bytes([0x66; 32]),
        "bob",
    )
    .await;

    let payload = signed_inbox_payload(&seed, old, recipient);
    let send_req = fauna_protocol::inbox::InboxSendRequest {
        recipient_actor_id: hex::encode(recipient),
        recipient_nest_url: None,
        payload_bytes: payload.clone(),
        extra: Default::default(),
    };

    // Before the succession the same bytes deliver — so the assertion after it
    // cannot pass for an unrelated reason (a malformed payload, a closed
    // inbox, a missing account).
    let _: fauna_protocol::inbox::InboxSendReply =
        common::call(&router, &state, old, "fauna.inbox.send", &send_req)
            .await
            .expect("precondition: the payload delivers before succession");

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    let err = common::call::<_, fauna_protocol::inbox::InboxSendReply>(
        &router,
        &state,
        old,
        "fauna.inbox.send",
        &send_req,
    )
    .await
    .expect_err("content authored by a superseded key must be refused");
    // Rejected as a bad payload rather than an auth failure: the *authorship*
    // is what is no longer acceptable, and this path has no auth verdict to
    // give (the federation leg reaching the same core has no caller at all).
    assert!(
        format!("{:?}", err).contains("superseded"),
        "the refusal must say why: {err:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The superseded RECIPIENT on the delivery path — accept-and-re-point
// (`succession-repoint-axis.md` § Re-key scope, the delivery-plane blockquote;
// ruled 2026-09-26)
// ═════════════════════════════════════════════════════════════════════════════
//
// The sender-side refusal above stops a stolen key *posting*. The other half
// of the same consult is the recipient: a peer that has not propagated the
// statement goes on addressing the retired id, and the nest keeps accepting
// (refusing would black-hole mail in flight during propagation) — but lands the
// arrival where the account now lives. Before the re-point the retired id's
// `inbox_modes` and `contacts` rows had moved with the account, so such an
// arrival fell back to `allow_knock` and no edge and became a knock plus a
// notification on an identity nobody can sign in as. Each test below was
// red against that behaviour before the re-point landed.

/// The fixture every recipient-side pin starts from: a recipient with an
/// `open` inbox, a third party with an ordinary account, one arrival from the
/// third party *delivered* (not knocked) before the ceremony — so the assertion
/// after it cannot pass or fail for an unrelated reason — and the succession
/// landed.
struct SucceededRecipient {
    old: [u8; 32],
    new: [u8; 32],
    third_seed: SigningKey,
    third: [u8; 32],
}

async fn a_succeeded_recipient_with_a_prior_delivery(
    router: &RpcRouter,
    state: &Arc<AppState>,
) -> SucceededRecipient {
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    let (third_seed, third) = identity(0x77);
    account_with_recovery_key(router, state, &seed, old, &recovery, "alice").await;
    state
        .db
        .create_user_with_handle(&third, "free", "carol", None)
        .await
        .expect("the third party's account");
    state
        .db
        .set_inbox_mode(&old, "open")
        .await
        .expect("the recipient opens their inbox");

    let reply = send_inbox_post(router, state, &third_seed, third, old, "before")
        .await
        .expect("precondition: a stranger's post reaches an open inbox");
    assert!(
        reply.inbox_id.is_some(),
        "precondition: delivered onto the open inbox, not knocked"
    );

    submit_succession(
        router,
        state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");
    SucceededRecipient {
        old,
        new,
        third_seed,
        third,
    }
}

/// One `fauna.inbox.send` from `sender` to `recipient` on this nest, saying
/// `content` (distinct texts make distinct posts).
async fn send_inbox_post(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    sender: [u8; 32],
    recipient: [u8; 32],
    content: &str,
) -> Result<fauna_protocol::inbox::InboxSendReply, RpcError> {
    common::call(
        router,
        state,
        sender,
        "fauna.inbox.send",
        &fauna_protocol::inbox::InboxSendRequest {
            recipient_actor_id: hex::encode(recipient),
            recipient_nest_url: None,
            payload_bytes: common::signed_inbox_payload_saying(seed, sender, recipient, content),
            extra: Default::default(),
        },
    )
    .await
}

#[tokio::test]
async fn an_arrival_addressed_to_a_superseded_recipient_lands_on_the_successor() {
    let (router, state) = nest().await;
    let r = a_succeeded_recipient_with_a_prior_delivery(&router, &state).await;

    // A peer that has not propagated the statement still addresses the OLD id.
    let reply = send_inbox_post(&router, &state, &r.third_seed, r.third, r.old, "after")
        .await
        .expect("an arrival addressed to a retired id is accepted, never refused");
    let landed = reply.inbox_id.expect(
        "delivered onto the successor's `open` inbox (the mode moved with the account) — \
         not a knock on the retired id",
    );

    // The successor sees it as their own.
    let fetched: fauna_protocol::inbox::InboxFetchReply = common::call(
        &router,
        &state,
        r.new,
        "fauna.inbox.fetch",
        &fauna_protocol::inbox::InboxFetchRequest {
            limit: 0,
            ..Default::default()
        },
    )
    .await
    .expect("the successor reads their own inbox");
    assert!(
        fetched.items.iter().any(|i| i.id == landed),
        "the successor must see the arrival addressed to their predecessor"
    );
    // And nothing strands on the retired identity.
    assert!(
        state.db.poll_knocks(&r.old).await.unwrap().is_empty(),
        "no knock on the retired id"
    );
}

#[tokio::test]
async fn a_sender_the_successor_blocked_is_refused_when_addressing_the_predecessor() {
    // The re-point runs BEFORE the routing floor — the ordering is the point:
    // the successor's block must hold for the predecessor's address too, or the
    // retired id becomes a side door around every reach decision.
    let (router, state) = nest().await;
    let r = a_succeeded_recipient_with_a_prior_delivery(&router, &state).await;
    state
        .db
        .block_contact(&r.new, &r.third)
        .await
        .expect("the successor blocks the sender");

    let err = send_inbox_post(&router, &state, &r.third_seed, r.third, r.old, "after")
        .await
        .expect_err("a blocked sender must not reach the successor via the predecessor's id");
    assert_eq!(err.code, "fauna.inbox.forbidden", "{err:?}");
    assert!(
        state.db.poll_knocks(&r.old).await.unwrap().is_empty(),
        "and no knock strands on the retired id either"
    );
}

#[tokio::test]
async fn an_arrival_for_a_deleted_successor_is_refused_not_stranded_on_the_predecessor() {
    // Deletion reaches the account's predecessors
    // (`account-data-plane.md` § Nest-side requirements item 1) while
    // `actor_successions` is retained — so the walk still re-points, and
    // existence is then judged at the terminal id like any recipient's. The
    // alternative was a knock plus a notification on a ghost.
    let (router, state) = nest().await;
    let r = a_succeeded_recipient_with_a_prior_delivery(&router, &state).await;
    assert!(
        state.db.delete_user(&r.new).await.expect("delete"),
        "the successor's account is deleted"
    );
    assert!(
        !state.db.is_actor_registered(&r.old).await.unwrap(),
        "precondition: deletion reached the predecessor"
    );

    let err = send_inbox_post(&router, &state, &r.third_seed, r.third, r.old, "after")
        .await
        .expect_err("no account is left to receive it");
    assert_eq!(err.code, "fauna.inbox.forbidden", "{err:?}");
    for id in [r.old, r.new] {
        assert!(
            state.db.poll_knocks(&id).await.unwrap().is_empty(),
            "no knock strands on a deleted id"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// An identity registered on two nests (`identity-succession.md` § Enforcement
// on the home nest → *Every nest the identity is linked to*)
// ═════════════════════════════════════════════════════════════════════════════

/// The same identity as an account on a second nest, holding that nest's own
/// pairing row for the first: the state a link leaves on the linked side.
async fn linked_account(state: &Arc<AppState>, actor: [u8; 32], handle: &str, peer: [u8; 32]) {
    state
        .db
        .create_user_with_handle(&actor, "free", handle, None)
        .await
        .expect("account created on the linked nest");
    state
        .db
        .store_pairing(
            &actor,
            &peer,
            &fauna_protocol::pair::default_self_sync(),
            None,
            Some("https://home.example"),
            None,
        )
        .await
        .expect("the linked nest's own pairing row");
}

async fn lookup(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> Vec<ByteBuf> {
    let reply: SuccessionLookupReply = common::call(
        router,
        state,
        ANON,
        "fauna.recovery.succession.lookup",
        &SuccessionLookupRequest {
            actor_id: ByteBuf::from(actor.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("the lookup answers");
    reply.statements
}

/// A nest applies a succession only against the registration chain it holds
/// itself. One that holds an account for the identity and no chain can verify
/// nothing: the statement that retired the identity at its other nest is
/// refused here, and the retired key keeps signing in. That is the residual
/// the chain-follows-the-link rule exists to close, pinned so it cannot be
/// "fixed" by honoring a statement no local chain authorizes.
#[tokio::test]
async fn a_second_nest_holding_no_chain_cannot_retire_the_identity() {
    let (home_router, home) = nest().await;
    let (linked_router, linked) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&home_router, &home, &seed, old, &recovery, "alice").await;
    linked_account(&linked, old, "alice", home.bound_identity()).await;

    let statement = succession_bytes(&recovery, old, &new_seed, Some(&seed), 2);
    submit_succession(&home_router, &home, statement.clone())
        .await
        .expect("succession lands at the nest that holds the chain");

    let err = handshake(&home_router, &home, &seed, old)
        .await
        .expect_err("control: the nest that ran the ceremony refuses the old key");
    assert_eq!(err.code, RpcError::CODE_SUPERSEDED);

    let err = submit_succession(&linked_router, &linked, statement)
        .await
        .expect_err("no chain here, so nothing authorizes the statement");
    assert_eq!(err.code, "fauna.recovery.not_registered");

    handshake(&linked_router, &linked, &seed, old)
        .await
        .expect("the retired key still signs in where the statement could not land");
    handshake(&linked_router, &linked, &new_seed, new)
        .await
        .expect_err("and the successor holds no account there");
    assert!(lookup(&linked_router, &linked, old).await.is_empty());
    assert!(
        linked
            .db
            .is_paired(&old, &home.bound_identity())
            .await
            .unwrap(),
        "the linked nest's own pairing row stands under the retired identity"
    );
}

/// The registration records and the statement are signed end to end and name
/// no nest, so the same bytes do the same work at every nest that holds an
/// account for the identity: the chain replays verbatim over the account's own
/// session, and the one door (`succession.submit`) then runs the same
/// transaction there — account moved, old key refused, that nest's own
/// pairing rows burned, the statement served onward.
#[tokio::test]
async fn the_same_chain_and_statement_retire_the_identity_at_a_second_nest() {
    let (home_router, home) = nest().await;
    let (linked_router, linked) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&home_router, &home, &seed, old, &recovery, "alice").await;
    linked_account(&linked, old, "alice", home.bound_identity()).await;

    // The chain follows the link: what the first nest serves to anyone, the
    // account's own session submits at the second, byte for byte.
    let chain: RegistrationChainReply = common::call(
        &home_router,
        &home,
        ANON,
        "fauna.recovery.registration.chain",
        &RegistrationChainRequest {
            actor_id: ByteBuf::from(old.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("the chain is served pre-identity");
    assert_eq!(chain.registrations.len(), 1);
    for registration in chain.registrations {
        let _: RegistrationSubmitReply = common::call(
            &linked_router,
            &linked,
            old,
            "fauna.recovery.registration.submit",
            &RegistrationSubmitRequest {
                registration,
                extra: Default::default(),
            },
        )
        .await
        .expect("a record registered at one nest registers verbatim at another");
    }

    let statement = succession_bytes(&recovery, old, &new_seed, Some(&seed), 2);
    submit_succession(&home_router, &home, statement.clone())
        .await
        .expect("succession lands at the first nest");
    handshake(&linked_router, &linked, &seed, old)
        .await
        .expect("precondition: the second nest has not heard yet");

    submit_succession(&linked_router, &linked, statement.clone())
        .await
        .expect("the same statement lands at the second nest, from no session");

    let err = handshake(&linked_router, &linked, &seed, old)
        .await
        .expect_err("the retired key is refused at the second nest too");
    assert_eq!(err.code, RpcError::CODE_SUPERSEDED);
    assert_eq!(err.superseded_by(), Some(new));
    handshake(&linked_router, &linked, &new_seed, new)
        .await
        .expect("the successor owns the account there");
    assert_eq!(linked.db.resolve_handle("alice").await.unwrap(), Some(new));
    assert!(
        !linked
            .db
            .is_paired(&old, &home.bound_identity())
            .await
            .unwrap(),
        "the second nest burns its own pairing rows in its own transaction"
    );
    assert!(
        !linked
            .db
            .is_paired(&new, &home.bound_identity())
            .await
            .unwrap(),
        "and never carries them to the successor: re-linking is the successor's gesture"
    );
    let served = lookup(&linked_router, &linked, old).await;
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].as_ref(), &statement[..]);
}

// ═════════════════════════════════════════════════════════════════════════════
// The lookup (`identity-succession.md:72`)
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn the_lookup_serves_the_statement_verbatim_to_an_anonymous_caller() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    let bytes = succession_bytes(&recovery, old, &new_seed, Some(&seed), 2);
    submit_succession(&router, &state, bytes.clone())
        .await
        .expect("succession lands");

    let reply: SuccessionLookupReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.recovery.succession.lookup",
        &SuccessionLookupRequest {
            actor_id: ByteBuf::from(old.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("a peer holding the old id can look it up with no account here");

    assert_eq!(reply.statements.len(), 1);
    // Verbatim: a re-encode could invalidate a statement the nest itself
    // accepted, and the consumer verifies signatures over these exact bytes.
    assert_eq!(reply.statements[0].as_ref(), &bytes[..]);

    // And the served bytes actually verify under the registered RecoveryKey —
    // the property the whole plane exists to give a peer.
    let signed: fauna_core::recovery::SignedIdentitySuccession =
        fauna_core::encoding::canonical_decode(reply.statements[0].as_ref()).unwrap();
    signed
        .verify(&fauna_core::recovery::ChainHead::new(recovery.public(), 1))
        .expect("the served statement verifies for a peer");
    assert_eq!(signed.statement.new_actor_id.0, new);
}

#[tokio::test]
async fn the_lookup_walks_a_multi_hop_chain_and_answers_empty_for_an_unsucceeded_id() {
    let (router, state) = nest().await;
    let (seed, first) = identity(0x11);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let (second_seed, second) = identity(0x33);
    let r2 = RecoveryKey::from_bytes([0x44; 32]);
    let (third_seed, third) = identity(0x55);
    account_with_recovery_key(&router, &state, &seed, first, &r1, "alice").await;

    submit_succession(
        &router,
        &state,
        succession_bytes(&r1, first, &second_seed, None, 2),
    )
    .await
    .expect("first succession");

    // The successor mints a FRESH RecoveryKey as part of the ceremony
    // (`identity-succession.md:42`) — its own chain, starting at seq 1.
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(second),
        recovery_pubkey: r2.public(),
        seq: 1,
        created_at: Timestamp(1_753_300_000),
    };
    let signed = reg.sign(&second_seed, &r2, None).expect("sign");
    let _: RegistrationSubmitReply = common::call(
        &router,
        &state,
        second,
        "fauna.recovery.registration.submit",
        &RegistrationSubmitRequest {
            registration: ByteBuf::from(canonical_encode(&signed).unwrap()),
            extra: Default::default(),
        },
    )
    .await
    .expect("successor registers its own kit");

    submit_succession(
        &router,
        &state,
        succession_bytes(&r2, second, &third_seed, None, 2),
    )
    .await
    .expect("second succession");

    // A peer that only ever saw the FIRST identity gets both hops in one call —
    // "discovery must work *from* them" (`identity-succession.md:72`).
    let reply: SuccessionLookupReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.recovery.succession.lookup",
        &SuccessionLookupRequest {
            actor_id: ByteBuf::from(first.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("lookup");
    assert_eq!(reply.statements.len(), 2);

    // The terminal identity holds the handle.
    assert_eq!(state.db.resolve_handle("alice").await.unwrap(), Some(third));

    // An identity that was never succeeded gets an empty list, not an error —
    // the same non-oracle shape `registration.chain` uses.
    let (_unrelated_seed, unrelated) = identity(0x77);
    let reply: SuccessionLookupReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.recovery.succession.lookup",
        &SuccessionLookupRequest {
            actor_id: ByteBuf::from(unrelated.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("an unsucceeded identity is an honest empty answer, never an error");
    assert!(reply.statements.is_empty());
}

/// `succession.status` — the authenticated read that closes the lost-reply
/// residual, and the three properties that keep it from being the oracle the
/// obvious home (`succession.lookup`) would have made of it.
///
/// Goal doc: `succession-aftermath.md` § Adjudicating what the aftermath
/// carries across — the declared residual and the additive wire change that
/// closes it.
#[tokio::test]
async fn the_status_read_serves_a_successor_its_own_commit_stamp_and_nobody_elses() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // A second, unrelated account that never ran the ceremony — the control for
    // "self-scoped", and it must hold a `users` row or it would be refused by
    // the class gate for the wrong reason.
    let (bob_seed, bob) = identity(0x44);
    let bob_recovery = RecoveryKey::from_bytes([0x55; 32]);
    account_with_recovery_key(&router, &state, &bob_seed, bob, &bob_recovery, "bob").await;

    // An ordinary account that never ran the ceremony gets an honest `None`,
    // never an error — `succession.lookup`'s non-oracle shape.
    assert_eq!(
        succession_status(&router, &state, bob).await,
        None,
        "an account that is not a successor is an honest None, not a refusal"
    );

    let submit_reply = submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 2),
    )
    .await
    .expect("succession lands");

    // (1) The stamp served is the ROW's, not a fresh clock read. This is the
    // whole reason the read exists: the client compares `email_filters
    // .created_at` against it, and a value bound at reply-encode time is a
    // different bound from the one the transaction committed under.
    let row = state
        .db
        .succession_path(&old[..])
        .await
        .expect("path")
        .into_iter()
        .next()
        .expect("one hop");
    assert_eq!(
        succession_status(&router, &state, new).await,
        Some(row.succeeded_at),
        "the read must serve `actor_successions.succeeded_at` verbatim"
    );

    // (1b) The SUBMIT reply carries the same stamp — it used to
    // recompute `now_epoch_secs()` at encode time, after the transaction (and
    // after the period-key rotation loop) had already run, instead of the
    // value that transaction committed. `filter_marks.rs`'s classifier bound
    // is threaded from this exact reply, so a drifted stamp here silently
    // shifts the email-filter adjudication boundary the ceremony promises.
    assert_eq!(
        submit_reply.succeeded_at, row.succeeded_at,
        "the submit reply must serve the transaction's own committed stamp, \
         not an independent clock read taken after it"
    );

    // (2) Self-scoped: an unrelated account learns nothing about alice's
    // ceremony. The request carries no actor id, so this is structural — but
    // pin it, because the failure mode (leaking "this account was compromised
    // and recovered at second T", plus a same-second correlation handle) is
    // silent and unrecoverable once served.
    assert_eq!(
        succession_status(&router, &state, bob).await,
        None,
        "a bystander must not be able to read another account's commit stamp"
    );

    // (2b) **The PREDECESSOR — the seed thief's exact position — buys nothing
    // here.** Distinct from the bystander case and easy to conflate with it:
    // the thief holds `old`'s key, and `old` is the id every peer and every
    // stale session still names, so a leak here would hand them the commit
    // second of the ceremony that dispossessed them.
    //
    // ⚠ **It is a `None`, not a refusal, and the layering is the point.** This
    // helper invokes the handler directly, so it exercises the class gate
    // alone; the retired identity still resolves to a class because the
    // succession re-points the account rather than deleting the old row. What
    // stops the thief in production is one layer up and is already pinned:
    // `refuse_if_superseded` (`bins/fauna-nest/src/auth_core.rs:133-153`)
    // refuses the retired key at *every* session mint — see
    // `the_old_key_is_refused_by_every_seed_signature_ceremony` above — so no
    // authenticated connection presenting `old` can exist to reach this kind at
    // all. This assertion pins the second belt: even with that layer bypassed,
    // the self-scoped query answers about `old`, and `old` is nobody's
    // successor, so the ceremony's stamp is not in the reply. A future edit
    // that "helpfully" widened the lookup to the chain — serving a predecessor
    // its successor's row — would red exactly here.
    let to_predecessor: SuccessionStatusReply = common::call(
        &router,
        &state,
        old,
        SUCCESSION_STATUS_KIND,
        &SuccessionStatusRequest::default(),
    )
    .await
    .expect("the class gate alone does not refuse the retired id — see the comment");
    assert_eq!(
        to_predecessor.succeeded_at, None,
        "the retired identity must not learn when the ceremony that dispossessed \
         it committed — it is the key the thief holds"
    );

    // (3) An unauthenticated caller is refused outright. Gate 1b in
    // `routes::dispatch_request` already refuses the anonymous *connection*
    // (pinned in `pre_identity_allowlist`'s unit tests, which is where the
    // routing boundary lives); this covers the handler's own class check, which
    // is what an authenticated-but-revoked or non-account caller meets.
    let denied: Result<SuccessionStatusReply, RpcError> = common::call(
        &router,
        &state,
        ANON,
        SUCCESSION_STATUS_KIND,
        &SuccessionStatusRequest::default(),
    )
    .await;
    let err = denied.expect_err("a caller with no account must be refused, not answered");
    assert!(
        err.code.contains("permission") || err.code.contains("denied"),
        "expected a permission refusal, got {}",
        err.code
    );
}

/// Read the caller's own commit stamp over the authenticated kind.
async fn succession_status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> Option<i64> {
    let reply: SuccessionStatusReply = common::call(
        router,
        state,
        actor,
        SUCCESSION_STATUS_KIND,
        &SuccessionStatusRequest::default(),
    )
    .await
    .expect("an account may always read its own succession status");
    reply.succeeded_at
}

// ═════════════════════════════════════════════════════════════════════════════
// Owed nests (`identity-succession.md` § Enforcement on the home nest → *Every
// nest the identity is linked to*, **The road**): the transaction burns the
// pairings and keeps their destinations for the successor
// ═════════════════════════════════════════════════════════════════════════════

/// A 32-byte nest id distinguishable in an assertion message.
fn nest_id(tag: u8) -> [u8; 32] {
    [tag; 32]
}

/// Pair `actor` with the nest `peer` — the row `fauna.pair.add` writes, here
/// with an explicit address and expiry.
async fn pair(
    state: &Arc<AppState>,
    actor: [u8; 32],
    peer: [u8; 32],
    url: Option<&str>,
    expires_at: Option<i64>,
) {
    state
        .db
        .store_pairing(
            &actor,
            &peer,
            &fauna_protocol::pair::default_self_sync(),
            expires_at,
            url,
            None,
        )
        .await
        .expect("pairing stored");
}

/// The caller's owed nests, over the real status handler.
async fn owed_nests(router: &RpcRouter, state: &Arc<AppState>, actor: [u8; 32]) -> Vec<OwedNest> {
    let reply: SuccessionStatusReply = common::call(
        router,
        state,
        actor,
        SUCCESSION_STATUS_KIND,
        &SuccessionStatusRequest::default(),
    )
    .await
    .expect("an account may always read its own succession status");
    reply.owed_nests
}

/// `(old_actor_id, nest_id, nest_url)` of each entry, in served order.
fn owed_keys(entries: &[OwedNest]) -> Vec<(Vec<u8>, Vec<u8>, Option<String>)> {
    entries
        .iter()
        .map(|e| {
            (
                e.old_actor_id.to_vec(),
                e.nest_id.to_vec(),
                e.nest_url.clone(),
            )
        })
        .collect()
}

async fn owed_settle(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    old: [u8; 32],
    nest: [u8; 32],
) -> Result<SuccessionOwedSettleReply, RpcError> {
    common::call(
        router,
        state,
        caller,
        SUCCESSION_OWED_SETTLE_KIND,
        &SuccessionOwedSettleRequest {
            old_actor_id: ByteBuf::from(old.to_vec()),
            nest_id: ByteBuf::from(nest.to_vec()),
            extra: Default::default(),
        },
    )
    .await
}

/// Register `recovery` as the first kit of an account that already exists —
/// what a successor does before it can itself be succeeded.
async fn register_first_kit(
    router: &RpcRouter,
    state: &Arc<AppState>,
    seed: &SigningKey,
    actor: [u8; 32],
    recovery: &RecoveryKey,
) {
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: recovery.public(),
        seq: 1,
        created_at: Timestamp(1_753_300_000),
    };
    let signed = reg.sign(seed, recovery, None).expect("sign");
    let _: RegistrationSubmitReply = common::call(
        router,
        state,
        actor,
        "fauna.recovery.registration.submit",
        &RegistrationSubmitRequest {
            registration: ByteBuf::from(canonical_encode(&signed).unwrap()),
            extra: Default::default(),
        },
    )
    .await
    .expect("successor registers its own kit");
}

/// The burn keeps what it burns. The pairing rows are gone after the ceremony
/// — that is unchanged — and their destinations are served to the successor,
/// and to nobody else, on the status read.
#[tokio::test]
async fn a_succession_keeps_its_burned_pairing_destinations_for_the_successor_alone() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;
    let (_bob_seed, bob) = identity(0x77);
    state
        .db
        .create_user_with_handle(&bob, "free", "bob", None)
        .await
        .expect("bystander account");

    let (x, y, gone) = (nest_id(0xa1), nest_id(0xa2), nest_id(0xa3));
    pair(&state, old, x, Some("https://x.example"), None).await;
    // A row with no address is still an owed nest: the id is what is owed.
    pair(&state, old, y, None, None).await;
    // A pairing that had already expired was no standing link when the
    // statement landed, so nothing is owed there.
    pair(&state, old, gone, Some("https://gone.example"), Some(1)).await;
    // A bystander's own pairing with the same nest is not the retired
    // identity's, and must be neither burned nor kept.
    pair(&state, bob, x, Some("https://x.example"), None).await;

    assert!(
        owed_nests(&router, &state, old).await.is_empty(),
        "nothing is owed before a succession"
    );

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    // The pairing rows themselves are still burned.
    for peer in [x, y, gone] {
        assert!(
            state.db.get_pairing(&old, &peer).await.unwrap().is_none(),
            "the retired identity's pairing rows are burned in the transaction"
        );
        assert!(
            state.db.get_pairing(&new, &peer).await.unwrap().is_none(),
            "a pairing never moves to the successor — re-pairing is its own gesture"
        );
    }
    assert!(
        state.db.get_pairing(&bob, &x).await.unwrap().is_some(),
        "another account's pairing is untouched"
    );

    // The successor reads both destinations, oldest pairing first.
    assert_eq!(
        owed_keys(&owed_nests(&router, &state, new).await),
        vec![
            (
                old.to_vec(),
                x.to_vec(),
                Some("https://x.example".to_string())
            ),
            (old.to_vec(), y.to_vec(), None),
        ],
    );

    // Nobody else does: not a bystander, and not the retired identity — the
    // key a thief holds — which is nobody's successor.
    assert!(owed_nests(&router, &state, bob).await.is_empty());
    assert!(owed_nests(&router, &state, old).await.is_empty());
    let denied: Result<SuccessionStatusReply, RpcError> = common::call(
        &router,
        &state,
        ANON,
        SUCCESSION_STATUS_KIND,
        &SuccessionStatusRequest::default(),
    )
    .await;
    assert!(denied.is_err(), "a caller with no account is refused");
}

/// The seed can add pairing rows, so a thief can add them faster than anyone
/// would deliver to them. The kept list is capped, and it is filled oldest
/// first: the rows that were there before the theft are the owner's.
#[tokio::test]
async fn the_owed_nests_are_capped_oldest_pairing_first() {
    const CAP: usize = 16;
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // Stored in this order, so the rows age with the tag.
    let tags: Vec<u8> = (0..(CAP as u8 + 4)).map(|i| 0x80 + i).collect();
    for tag in &tags {
        pair(&state, old, nest_id(*tag), None, None).await;
    }

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    let served: Vec<Vec<u8>> = owed_nests(&router, &state, new)
        .await
        .into_iter()
        .map(|e| e.nest_id.to_vec())
        .collect();
    let expected: Vec<Vec<u8>> = tags[..CAP].iter().map(|t| nest_id(*t).to_vec()).collect();
    assert_eq!(
        served, expected,
        "exactly the cap, and the oldest rows rather than the newest"
    );
    // Over the cap or under it, every pairing row is burned.
    for tag in &tags {
        assert!(
            state
                .db
                .get_pairing(&old, &nest_id(*tag))
                .await
                .unwrap()
                .is_none()
        );
    }
}

/// Settling clears one entry, only for a successor of the identity the entry
/// belongs to, and a second settle of the same entry is a success.
#[tokio::test]
async fn owed_settle_clears_one_entry_refuses_a_caller_off_the_path_and_is_idempotent() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;
    let (_bob_seed, bob) = identity(0x77);
    state
        .db
        .create_user_with_handle(&bob, "free", "bob", None)
        .await
        .expect("bystander account");

    let (x, y) = (nest_id(0xa1), nest_id(0xa2));
    pair(&state, old, x, Some("https://x.example"), None).await;
    pair(&state, old, y, Some("https://y.example"), None).await;
    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    // Off the path: a bystander, and the retired identity itself (the thief's
    // key — it must not be able to hide a nest it still signs in at). A caller
    // with no account at all is refused by the class gate. None of them clears
    // anything.
    for caller in [bob, old] {
        let err = owed_settle(&router, &state, caller, old, x)
            .await
            .expect_err("only a successor of the retired identity may settle its entries");
        assert!(
            err.code.contains("permission"),
            "expected a permission refusal, got {}",
            err.code
        );
    }
    assert!(owed_settle(&router, &state, ANON, old, x).await.is_err());
    assert_eq!(owed_nests(&router, &state, new).await.len(), 2);

    // The successor settles X and is served Y alone.
    owed_settle(&router, &state, new, old, x)
        .await
        .expect("the successor settles its own entry");
    assert_eq!(
        owed_keys(&owed_nests(&router, &state, new).await),
        vec![(
            old.to_vec(),
            y.to_vec(),
            Some("https://y.example".to_string())
        )],
    );

    // Idempotent: the same settle again, and a settle of a nest that was
    // never owed, both succeed and change nothing.
    owed_settle(&router, &state, new, old, x)
        .await
        .expect("settling an entry that is already gone is a success");
    owed_settle(&router, &state, new, old, nest_id(0xee))
        .await
        .expect("so is settling one that never existed");
    assert_eq!(owed_nests(&router, &state, new).await.len(), 1);

    // A malformed key is the caller's error, not a silent no-op.
    let malformed: Result<SuccessionOwedSettleReply, RpcError> = common::call(
        &router,
        &state,
        new,
        SUCCESSION_OWED_SETTLE_KIND,
        &SuccessionOwedSettleRequest {
            old_actor_id: ByteBuf::from(vec![1, 2, 3]),
            nest_id: ByteBuf::from(y.to_vec()),
            extra: Default::default(),
        },
    )
    .await;
    assert!(malformed.is_err());
}

/// A first successor that never settled an entry does not lose it by being
/// succeeded in turn: the status read answers for every hop on the caller's
/// predecessor path.
#[tokio::test]
async fn a_second_successions_successor_is_still_served_the_first_hops_unsettled_entry() {
    let (router, state) = nest().await;
    let (seed, first) = identity(0x11);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let (second_seed, second) = identity(0x33);
    let r2 = RecoveryKey::from_bytes([0x44; 32]);
    let (third_seed, third) = identity(0x55);
    account_with_recovery_key(&router, &state, &seed, first, &r1, "alice").await;

    let (x, y, z) = (nest_id(0xa1), nest_id(0xa2), nest_id(0xa3));
    pair(&state, first, x, Some("https://x.example"), None).await;
    pair(&state, first, y, Some("https://y.example"), None).await;
    submit_succession(
        &router,
        &state,
        succession_bytes(&r1, first, &second_seed, None, 2),
    )
    .await
    .expect("first succession");

    // The first successor settles X, leaves Y owed, and pairs a nest of its
    // own before it is succeeded too.
    owed_settle(&router, &state, second, first, x)
        .await
        .expect("first successor settles X");
    pair(&state, second, z, Some("https://z.example"), None).await;
    register_first_kit(&router, &state, &second_seed, second, &r2).await;
    submit_succession(
        &router,
        &state,
        succession_bytes(&r2, second, &third_seed, None, 2),
    )
    .await
    .expect("second succession");

    // The second successor is served the first hop's unsettled entry and the
    // second hop's own, each under the identity it was burned from, the
    // earliest hop first.
    assert_eq!(
        owed_keys(&owed_nests(&router, &state, third).await),
        vec![
            (
                first.to_vec(),
                y.to_vec(),
                Some("https://y.example".to_string())
            ),
            (
                second.to_vec(),
                z.to_vec(),
                Some("https://z.example".to_string())
            ),
        ],
    );

    // The first successor is retired now. It is served nothing and settles
    // nothing: "the successor alone" is the identity the account belongs to.
    assert!(owed_nests(&router, &state, second).await.is_empty());
    assert!(
        owed_settle(&router, &state, second, first, y)
            .await
            .is_err()
    );

    // And the second successor can settle an entry two hops back.
    owed_settle(&router, &state, third, first, y)
        .await
        .expect("a successor settles any hop on its predecessor path");
    assert_eq!(
        owed_keys(&owed_nests(&router, &state, third).await),
        vec![(
            second.to_vec(),
            z.to_vec(),
            Some("https://z.example".to_string())
        )],
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Helpers that need the inbox wire shape
// ═════════════════════════════════════════════════════════════════════════════

/// The **socket** half of the guarantee
/// [`live_sessions_of_the_old_identity_are_revoked`] names but does not test.
///
/// That test asserts the bearer is gone from the `TokenStore`. The nest
/// validates a bearer **exactly once, at the WS upgrade** (`routes.rs:1568`),
/// then bakes the actor into `RpcConnection` for the connection's lifetime —
/// `dispatch_core` never re-reads the token store. So killing tokens ends only
/// the *next* connection and leaves the one the thief already holds fully
/// functional; `transport.md` § Connection lifecycle → *Revocation teardown* is
/// explicit that "neither half alone suffices", and
/// `AppState::revoke_actor_authority` exists to do both.
///
/// Every other authority-stripping path pairs the two halves (lockout, suspend,
/// the eviction ladder, the pending-action executor, the bridge doors). The
/// succession ceremony — the one path whose entire purpose is evicting a thief
/// who holds the seed — did only the token half, so the thief's open socket kept
/// full `User`-class dispatch after the ceremony that was supposed to undo them.
/// The per-RPC authority gate does not cover it either: `caller_class_for_actor`
/// reads `users.suspended` and `users.locked_until`, and the ceremony
/// deliberately **keeps** the old `users` row unsuspended and unlocked
/// (`db/successions.rs:29`), so it resolves to `Some(CallerClass::User)`.
///
/// The observable here is that the ceremony **performs** the teardown; that a
/// revoked connection then closes 4401 and stops dispatching is proved on the
/// wire by `conformance_revocation_teardown.rs`.
#[tokio::test]
async fn a_live_socket_of_the_old_identity_is_torn_down_by_the_ceremony() {
    let (router, state) = nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;

    // The thief's live WebSocket, registered exactly as `handle_ws` registers one
    // after the upgrade-time bearer check.
    let (thief_conn, _thief_rx) = state.ws.subscribe(old);
    // The successor's own socket, to prove the teardown is scoped to the retired
    // identity and does not sweep the account it just handed over.
    let (successor_conn, _successor_rx) = state.ws.subscribe(new);
    assert!(
        !thief_conn.is_revoked(),
        "precondition: the thief's socket is live"
    );

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, None, 2),
    )
    .await
    .expect("succession lands");

    assert!(
        thief_conn.is_revoked(),
        "the ceremony must close the sockets the retired identity already holds — \
         tokens stop only the NEXT connection (`transport.md` § Revocation teardown)"
    );
    assert!(
        !successor_conn.is_revoked(),
        "the teardown is scoped to the retired identity"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The road: the successor's devices deliver to every owed nest
// (`identity-succession.md` § Enforcement on the home nest → *Every nest the
// identity is linked to*, **The road** and **The stated bounds**) — the shared
// client delivery (`fauna_client_core::succession_delivery`) over two nests'
// real handlers: the keeping nest H, and L, the nest it is owed at.
// ═════════════════════════════════════════════════════════════════════════════

mod road {
    use super::*;
    use fauna_client_core::succession_delivery::{
        Delivery, OwedNestReach, OwedReason, SignIn, deliver_owed_nests,
    };
    use fauna_protocol::{RpcErrorClass, RpcRequester};

    const L_URL: &str = "https://l.example";

    /// The wire refusal a real connection would carry.
    #[derive(Debug)]
    struct Refusal(RpcError);

    impl std::fmt::Display for Refusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0.code)
        }
    }

    impl RpcErrorClass for Refusal {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    /// A connection to one nest's handlers, as `actor` (`ANON`: anonymous).
    struct Conn {
        router: Arc<RpcRouter>,
        state: Arc<AppState>,
        actor: [u8; 32],
    }

    impl RpcRequester for Conn {
        type Error = Refusal;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Refusal>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            common::call(&self.router, &self.state, self.actor, kind, &payload)
                .await
                .map_err(Refusal)
        }
    }

    /// The successor device's reach to L: an anonymous connection presenting
    /// L's own identity, and a sign-in — a real `fauna.auth.handshake` — as
    /// any retired identity whose seed the device holds.
    struct Reach {
        router: Arc<RpcRouter>,
        state: Arc<AppState>,
        seeds: Vec<SigningKey>,
    }

    impl OwedNestReach for Reach {
        type Anon = Conn;
        type Signed = Conn;

        async fn anonymous(&self, url: &str) -> Result<(Conn, [u8; 32]), String> {
            assert_eq!(url, L_URL, "the address the burned pairing row carried");
            let conn = Conn {
                router: Arc::clone(&self.router),
                state: Arc::clone(&self.state),
                actor: ANON,
            };
            Ok((conn, self.state.bound_identity()))
        }

        async fn sign_in_as(&self, _url: &str, actor_id: &[u8; 32]) -> SignIn<Conn> {
            let Some(seed) = self
                .seeds
                .iter()
                .find(|s| s.verifying_key().to_bytes() == *actor_id)
            else {
                return SignIn::NoSeed;
            };
            match handshake(&self.router, &self.state, seed, *actor_id).await {
                Ok(_) => SignIn::Connected(Conn {
                    router: Arc::clone(&self.router),
                    state: Arc::clone(&self.state),
                    actor: *actor_id,
                }),
                Err(e) => SignIn::from_error(&Refusal(e)),
            }
        }
    }

    /// The two nests of one identity: its account and kit at H, paired there
    /// with L by L's address — the row the succession burns and keeps as
    /// owed — and the succession applied at H. `account_at_l`: the identity
    /// is registered at L too; `chain_at_l`: L holds its chain (the link
    /// carried it).
    struct Pair {
        h: (Arc<RpcRouter>, Arc<AppState>),
        l: (Arc<RpcRouter>, Arc<AppState>),
        seed: SigningKey,
        old: [u8; 32],
        new_seed: SigningKey,
        new: [u8; 32],
        statement: Vec<u8>,
    }

    impl Pair {
        async fn new(account_at_l: bool, chain_at_l: bool) -> Self {
            let (h_router, h) = nest().await;
            let (l_router, l) = nest().await;
            let (seed, old) = identity(0x11);
            let (new_seed, new) = identity(0x33);
            let recovery = RecoveryKey::from_bytes([0x22; 32]);
            account_with_recovery_key(&h_router, &h, &seed, old, &recovery, "alice").await;
            if account_at_l {
                linked_account(&l, old, "alice", h.bound_identity()).await;
            }
            if chain_at_l {
                for registration in registration_chain(&h_router, &h, old).await {
                    let _: RegistrationSubmitReply = common::call(
                        &l_router,
                        &l,
                        old,
                        "fauna.recovery.registration.submit",
                        &RegistrationSubmitRequest {
                            registration,
                            extra: Default::default(),
                        },
                    )
                    .await
                    .expect("the chain registers at L");
                }
            }
            pair(&h, old, l.bound_identity(), Some(L_URL), None).await;
            let statement = succession_bytes(&recovery, old, &new_seed, Some(&seed), 2);
            submit_succession(&h_router, &h, statement.clone())
                .await
                .expect("the succession lands at H");
            Self {
                h: (Arc::new(h_router), h),
                l: (Arc::new(l_router), l),
                seed,
                old,
                new_seed,
                new,
                statement,
            }
        }

        /// The successor's device delivering H's owed list to L.
        async fn deliver(&self, holds_retired_seed: bool) -> Vec<Delivery> {
            let keeper = Conn {
                router: Arc::clone(&self.h.0),
                state: Arc::clone(&self.h.1),
                actor: self.new,
            };
            let reach = Reach {
                router: Arc::clone(&self.l.0),
                state: Arc::clone(&self.l.1),
                seeds: if holds_retired_seed {
                    vec![self.seed.clone()]
                } else {
                    vec![]
                },
            };
            deliver_owed_nests(&keeper, &reach)
                .await
                .expect("H serves the successor its owed nests")
                .into_iter()
                .map(|(_, delivery)| delivery)
                .collect()
        }

        async fn owed_at_h(&self) -> usize {
            owed_nests(&self.h.0, &self.h.1, self.new).await.len()
        }

        /// L refuses the retired key as superseded and signs the successor in.
        async fn assert_retired_at_l(&self) {
            let (router, l) = (&self.l.0, &self.l.1);
            let err = handshake(router, l, &self.seed, self.old)
                .await
                .expect_err("L refuses the retired key");
            assert_eq!(err.code, RpcError::CODE_SUPERSEDED);
            assert_eq!(err.superseded_by(), Some(self.new));
            handshake(router, l, &self.new_seed, self.new)
                .await
                .expect("the successor signs in at L");
            let served = lookup(router, l, self.old).await;
            assert_eq!(served.len(), 1);
            assert_eq!(served[0].as_ref(), &self.statement[..]);
        }
    }

    async fn registration_chain(
        router: &RpcRouter,
        state: &Arc<AppState>,
        actor: [u8; 32],
    ) -> Vec<ByteBuf> {
        let chain: RegistrationChainReply = common::call(
            router,
            state,
            ANON,
            "fauna.recovery.registration.chain",
            &RegistrationChainRequest {
                actor_id: ByteBuf::from(actor.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .expect("the chain is served pre-identity");
        chain.registrations
    }

    /// The road's main arm: the successor's device reads H's owed list,
    /// submits the statement at L anonymously, L verifies it against its own
    /// chain and moves the account, and the entry is settled at H.
    #[tokio::test]
    async fn the_successors_device_delivers_the_statement_to_the_owed_nest_and_settles_it() {
        let pair = Pair::new(true, true).await;
        assert_eq!(pair.owed_at_h().await, 1, "precondition: L is owed");
        handshake(&pair.l.0, &pair.l.1, &pair.seed, pair.old)
            .await
            .expect("precondition: L has not heard yet");

        assert_eq!(
            pair.deliver(false).await,
            vec![Delivery::Landed {
                landed: 1,
                replayed: 0
            }]
        );
        pair.assert_retired_at_l().await;
        assert_eq!(pair.owed_at_h().await, 0, "the entry is settled at H");
        assert!(
            pair.deliver(false).await.is_empty(),
            "a settled entry is not delivered again"
        );
    }

    /// The chain-less arm: L holds the account and no chain, so it cannot
    /// verify. The device holds the retired seed: it signs in at L as the
    /// retired identity, replays H's chain there, and the statement lands.
    #[tokio::test]
    async fn a_nest_holding_no_chain_is_replayed_it_with_the_retired_seed_and_the_statement_lands()
    {
        let pair = Pair::new(true, false).await;
        assert_eq!(
            pair.deliver(false).await,
            vec![Delivery::Owed(OwedReason::NoSeed {
                code: "fauna.recovery.not_registered".into()
            })],
            "without the retired seed nothing can verify there, and the entry stays owed"
        );
        assert_eq!(pair.owed_at_h().await, 1);
        handshake(&pair.l.0, &pair.l.1, &pair.seed, pair.old)
            .await
            .expect("the retired key still signs in at L");

        assert_eq!(
            pair.deliver(true).await,
            vec![Delivery::Landed {
                landed: 1,
                replayed: 1
            }]
        );
        assert_eq!(
            registration_chain(&pair.l.0, &pair.l.1, pair.old).await,
            registration_chain(&pair.h.0, &pair.h.1, pair.old).await,
            "H's chain for the retired identity was carried verbatim"
        );
        pair.assert_retired_at_l().await;
        assert_eq!(pair.owed_at_h().await, 0);
    }

    /// A nest that holds no account for the retired identity is settled: the
    /// sign-in as that identity answers `not_registered`.
    #[tokio::test]
    async fn a_nest_holding_no_account_for_the_retired_identity_is_settled() {
        let pair = Pair::new(false, false).await;
        assert_eq!(pair.deliver(true).await, vec![Delivery::NoAccount]);
        assert_eq!(pair.owed_at_h().await, 0);
    }

    /// Stated bound 4, witnessed: a successor registered by hand at L before
    /// the statement landed there is refused as `successor_exists`, and the
    /// entry stays owed until that account is deleted at L — after which the
    /// next delivery lands it.
    #[tokio::test]
    async fn a_hand_registered_successor_keeps_the_entry_owed_until_that_account_is_deleted() {
        let pair = Pair::new(true, true).await;
        pair.l
            .1
            .db
            .create_user_with_handle(&pair.new, "free", "alice-new", None)
            .await
            .expect("a successor registered at L by hand");

        assert_eq!(
            pair.deliver(true).await,
            vec![Delivery::Owed(OwedReason::SuccessorExists)]
        );
        assert_eq!(pair.owed_at_h().await, 1, "owed, never dropped");
        handshake(&pair.l.0, &pair.l.1, &pair.seed, pair.old)
            .await
            .expect("the retired key still signs in at L meanwhile");

        // The account's own deletion, as the executor finalizes it once the
        // window has run.
        fauna_nest::pending_actions::finalize_self_deletion(&pair.l.1, &pair.new)
            .await
            .expect("the hand-registered successor is deleted at L");

        assert_eq!(
            pair.deliver(true).await,
            vec![Delivery::Landed {
                landed: 1,
                replayed: 0
            }],
            "once that account is gone, the statement lands"
        );
        pair.assert_retired_at_l().await;
        assert_eq!(pair.owed_at_h().await, 0);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// The kept wrap (`owner-key-material.md` § Path A-sibling-2 → *Rotation*, the
// succession rider → *The kept wrap*): the ceremony burns no generation's last
// escrow copy
// ═════════════════════════════════════════════════════════════════════════════

/// The escrow doors beside the ceremony's, on a nest holding the deployment
/// identity a holder needs to sign receipts.
async fn escrow_nest() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let mut state = AppState::for_test(db);
    state.nest_signing_key = Some(SigningKey::from_bytes(&[0x66; 32]));
    let state = Arc::new(state);
    let mut b = RpcRouter::builder();
    recovery_handlers::register_recovery_handlers(&mut b);
    auth_handlers::register_auth_handlers(&mut b);
    account_handlers::register_account_handlers(&mut b);
    fauna_nest::generation_escrow_handlers::register_generation_escrow_handlers(&mut b);
    (b.build(), state)
}

async fn escrow_put(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    generation: [u8; 32],
    wrap: &[u8],
) {
    let _: EscrowPutReply = common::call(
        router,
        state,
        actor,
        KIND_ESCROW_PUT,
        &EscrowPutRequest {
            generation_id: ByteBuf::from(generation.to_vec()),
            wrap: ByteBuf::from(wrap.to_vec()),
            target_key: "identity/test".into(),
            ..Default::default()
        },
    )
    .await
    .expect("escrow put");
}

/// `actor`'s served wraps, as (generation, wrap bytes), optionally filtered.
async fn escrow_get(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    generation: Option<[u8; 32]>,
) -> Vec<([u8; 32], Vec<u8>)> {
    let reply: EscrowGetReply = common::call(
        router,
        state,
        actor,
        KIND_ESCROW_GET,
        &EscrowGetRequest {
            generation_id: generation.map(|g| ByteBuf::from(g.to_vec())),
            ..Default::default()
        },
    )
    .await
    .expect("escrow get");
    reply
        .wraps
        .into_iter()
        .map(|w| {
            (
                <[u8; 32]>::try_from(w.generation_id.as_ref()).unwrap(),
                w.wrap.into_vec(),
            )
        })
        .collect()
}

/// The canonical theft's escrow, through the real ceremony on an anonymous
/// connection: the wraps the predecessor deposited survive it under the
/// retired id; `escrow.get` as the successor serves them, filtered or not;
/// the successor's deposit of one generation sweeps the predecessor's wrap of
/// exactly that generation and leaves its other one; and `escrow.delete` as
/// the successor takes a generation across the chain.
#[tokio::test]
async fn the_ceremony_keeps_the_predecessors_escrow_wraps_for_the_successor() {
    let (router, state) = escrow_nest().await;
    let (seed, old) = identity(0x11);
    let recovery = RecoveryKey::from_bytes([0x22; 32]);
    let (new_seed, new) = identity(0x33);
    account_with_recovery_key(&router, &state, &seed, old, &recovery, "alice").await;
    let (g1, g2) = ([0x61u8; 32], [0x62u8; 32]);
    escrow_put(&router, &state, old, g1, b"old-wrap-of-g1").await;
    escrow_put(&router, &state, old, g2, b"old-wrap-of-g2").await;

    submit_succession(
        &router,
        &state,
        succession_bytes(&recovery, old, &new_seed, Some(&seed), 2),
    )
    .await
    .expect("succession lands");

    // Kept, under the retired id: the actor id is the identity the wrap seals to.
    assert_eq!(
        state
            .db
            .get_generation_escrow_wraps(&old, None)
            .await
            .unwrap()
            .len(),
        2,
        "the ceremony burns no predecessor-keyed wrap"
    );
    // Served to the successor, unfiltered and filtered. The serve orders by
    // (deposit second, wrap hash), so two same-second deposits come back in
    // hash order: compare the set, ordered by generation.
    let mut served = escrow_get(&router, &state, new, None).await;
    served.sort();
    assert_eq!(
        served,
        vec![
            (g1, b"old-wrap-of-g1".to_vec()),
            (g2, b"old-wrap-of-g2".to_vec())
        ],
        "the successor's get serves the chain's kept wraps"
    );
    assert_eq!(
        escrow_get(&router, &state, new, Some(g2)).await,
        vec![(g2, b"old-wrap-of-g2".to_vec())]
    );

    // The successor's deposit of g1 is the one the kept wrap waited for.
    escrow_put(&router, &state, new, g1, b"new-wrap-of-g1").await;
    let old_rows: Vec<[u8; 32]> = state
        .db
        .get_generation_escrow_wraps(&old, None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.generation_id)
        .collect();
    assert_eq!(
        old_rows,
        vec![g2],
        "the deposit swept the predecessor's g1 and only it"
    );
    let mut served = escrow_get(&router, &state, new, None).await;
    served.sort();
    assert_eq!(
        served,
        vec![
            (g1, b"new-wrap-of-g1".to_vec()),
            (g2, b"old-wrap-of-g2".to_vec())
        ],
        "the successor's own g1 and the predecessor's still-kept g2"
    );

    // `escrow.delete` as the successor takes g2 across the chain.
    let del: EscrowDeleteReply = common::call(
        &router,
        &state,
        new,
        KIND_ESCROW_DELETE,
        &EscrowDeleteRequest {
            generation_id: ByteBuf::from(g2.to_vec()),
            ..Default::default()
        },
    )
    .await
    .expect("escrow delete");
    assert_eq!(del.deleted, 1, "the predecessor's kept wrap of g2");
    assert!(
        state
            .db
            .get_generation_escrow_wraps(&old, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        escrow_get(&router, &state, new, None).await,
        vec![(g1, b"new-wrap-of-g1".to_vec())]
    );
}
