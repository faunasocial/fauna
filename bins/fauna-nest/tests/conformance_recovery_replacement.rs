//! **tier_3** — the seed-initiated replacement pending window
//! (identity-succession slice 2): `fauna.recovery.replacement.request` /
//! `status` (USER-class) and the pre-identity `replacement.{challenge,veto}`
//! pair, plus the landing sweep.
//!
//! Goal doc: `docs/goal/behavior/identity-succession.md:37` — a seed-alone
//! replacement enters a 30-day pending window (`RECOVERY_REPLACE_GRACE`),
//! loudly notified; the current RecoveryKey vetoes/overrides instantly; only
//! an uncontested window lands the new registration.
//!
//! **The scenario under test is the seed-thief race**: the seed is the
//! symmetric credential a thief may hold, the registered RecoveryKey is the
//! asymmetric factor. So every veto here is driven as a **pre-identity**
//! caller (the owner may be locked out by the thief), and the tests attack:
//!
//! - a parked request confers NO authority — the chain is untouched until the
//!   window elapses;
//! - the current RecoveryKey cancels a pending instantly, and a different key
//!   cannot;
//! - the veto nonce is single-use and account-bound (a captured veto cannot
//!   cancel a future pending);
//! - a RecoveryKey-authorized registration landing during the window
//!   overrides the pending (and the sweep re-verifies against the
//!   then-current head);
//! - an uncontested window lands the record verbatim at the chain head;
//! - a replayed request cannot extend the window.
//!
//! No wall-clock waits anywhere: the sweep takes an injected `now`
//! (convention 14).

mod common;
use common::identity;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{
    RECOVERY_REPLACE_GRACE_SECS, RecoveryKey, RecoveryKeyRegistration, ReplacementVeto,
};
use fauna_nest::db::CacheDb;
use fauna_nest::recovery_handlers::{self, land_due_replacements};
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::RpcError;
use fauna_protocol::recovery::{
    ReplacementChallengeReply, ReplacementChallengeRequest, ReplacementRequestReply,
    ReplacementRequestRequest, ReplacementStatusReply, ReplacementStatusRequest,
    ReplacementVetoReply, ReplacementVetoRequest,
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
    (b.build(), state)
}

/// Build the wire bytes of a seed-alone replacement record (no prior sig).
fn seed_alone_record(
    seed: &SigningKey,
    actor: [u8; 32],
    new_recovery: &RecoveryKey,
    seq: u64,
) -> Vec<u8> {
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: new_recovery.public(),
        seq,
        created_at: Timestamp(1_753_100_000),
    };
    let signed = reg.sign(seed, new_recovery, None).expect("sign");
    canonical_encode(&signed).expect("encode")
}

async fn request_replacement(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    record: Vec<u8>,
) -> Result<ReplacementRequestReply, RpcError> {
    common::call(
        router,
        state,
        actor,
        "fauna.recovery.replacement.request",
        &ReplacementRequestRequest {
            registration: ByteBuf::from(record),
            extra: Default::default(),
        },
    )
    .await
}

async fn status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> ReplacementStatusReply {
    common::call(
        router,
        state,
        actor,
        "fauna.recovery.replacement.status",
        &ReplacementStatusRequest::default(),
    )
    .await
    .expect("status readable")
}

/// Drive the full pre-identity veto ceremony with `recovery`, returning the
/// veto reply (or the refusal).
async fn veto_with(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    recovery: &RecoveryKey,
) -> Result<ReplacementVetoReply, RpcError> {
    let challenge: ReplacementChallengeReply = common::call(
        router,
        state,
        ANON,
        "fauna.recovery.replacement.challenge",
        &ReplacementChallengeRequest {
            actor_id: ByteBuf::from(actor.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("challenge issues unconditionally");
    let nonce: [u8; 32] = challenge.nonce.as_ref().try_into().unwrap();
    let sig = ReplacementVeto::new(ActorId(actor), nonce)
        .sign(recovery)
        .expect("sign veto");
    common::call(
        router,
        state,
        ANON,
        "fauna.recovery.replacement.veto",
        &ReplacementVetoRequest {
            actor_id: ByteBuf::from(actor.to_vec()),
            nonce: ByteBuf::from(nonce.to_vec()),
            signature: ByteBuf::from(sig),
            extra: Default::default(),
        },
    )
    .await
}

/// A `now` far past every window parked in this test run — the injected clock
/// the sweep tests land with (no wall-clock waits).
fn after_the_window() -> i64 {
    let real_now = fauna_core::data::Timestamp::now_secs();
    real_now + RECOVERY_REPLACE_GRACE_SECS as i64 + 3_600
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_seed_alone_request_parks_without_touching_the_chain() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x51);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;

    let reply = request_replacement(
        &router,
        &state,
        actor,
        seed_alone_record(&seed, actor, &r2, 1),
    )
    .await
    .expect("request parks");

    // The standing banner's read: the pending is visible with its window.
    let st = status(&router, &state, actor).await;
    let pending = st.pending.expect("a replacement pends");
    assert_eq!(pending.new_recovery_pubkey.as_ref(), &r2.public());
    assert_eq!(
        pending.lands_at,
        pending.requested_at + RECOVERY_REPLACE_GRACE_SECS as i64
    );
    assert_eq!(reply.lands_at, pending.lands_at);

    // NO authority yet: the chain head is still R1 (account-data-plane.md § The ratified decisions) — a parked record must not
    // be consultable as a binding (the escrow fetch, succession validation,
    // and the veto itself all read the head).
    let head = state
        .db
        .recovery_registration_head(&actor)
        .await
        .unwrap()
        .expect("chain exists");
    assert_eq!(head.recovery_pubkey, r1.public().to_vec());
    assert_eq!(head.seq, 0);
}

#[tokio::test]
async fn the_current_recovery_key_vetoes_instantly_and_idempotently() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x52);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;
    request_replacement(
        &router,
        &state,
        actor,
        seed_alone_record(&seed, actor, &r2, 1),
    )
    .await
    .expect("request parks");

    // The pre-identity veto by the CURRENT key cancels instantly.
    let reply = veto_with(&router, &state, actor, &r1)
        .await
        .expect("veto accepted");
    assert!(reply.cancelled, "a pending replacement was cancelled");
    assert!(
        status(&router, &state, actor).await.pending.is_none(),
        "nothing pends after the veto"
    );

    // Vetoing again (fresh nonce) is an idempotent success reported honestly.
    let reply = veto_with(&router, &state, actor, &r1)
        .await
        .expect("idempotent veto accepted");
    assert!(!reply.cancelled, "nothing was pending the second time");

    // And the window can never land what was vetoed.
    let (landed, cancelled) = land_due_replacements(&state, after_the_window())
        .await
        .unwrap();
    assert_eq!((landed, cancelled), (0, 0));
    let head = state
        .db
        .recovery_registration_head(&actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.recovery_pubkey, r1.public().to_vec());
}

#[tokio::test]
async fn a_veto_by_a_different_key_is_refused() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x53);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    let thief_key = RecoveryKey::from_bytes([0x99; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;
    request_replacement(
        &router,
        &state,
        actor,
        seed_alone_record(&seed, actor, &r2, 1),
    )
    .await
    .expect("request parks");

    let err = veto_with(&router, &state, actor, &thief_key)
        .await
        .expect_err("a non-registered key must not veto");
    assert_eq!(err.code, "fauna.recovery.signature_failed");
    assert!(
        status(&router, &state, actor).await.pending.is_some(),
        "the pending survives a failed veto"
    );
}

#[tokio::test]
async fn a_veto_nonce_is_single_use_and_account_bound() {
    let (router, state) = nest().await;
    let (seed_a, actor_a) = identity(0x54);
    let (seed_b, actor_b) = identity(0x55);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    common::register_recovery_key(&router, &state, &seed_a, actor_a, &r1, None, 0).await;
    common::register_recovery_key(&router, &state, &seed_b, actor_b, &r1, None, 0).await;
    request_replacement(
        &router,
        &state,
        actor_a,
        seed_alone_record(&seed_a, actor_a, &r2, 1),
    )
    .await
    .expect("request parks");

    // Take a challenge for A, then present its nonce for B: refused (the
    // store binds nonce → account), and A's nonce is NOT burned by the
    // wrong-account attempt — the veto for A still works.
    let challenge: ReplacementChallengeReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.recovery.replacement.challenge",
        &ReplacementChallengeRequest {
            actor_id: ByteBuf::from(actor_a.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    let nonce: [u8; 32] = challenge.nonce.as_ref().try_into().unwrap();
    let sig_b = ReplacementVeto::new(ActorId(actor_b), nonce)
        .sign(&r1)
        .unwrap();
    let err: RpcError = common::call::<_, ReplacementVetoReply>(
        &router,
        &state,
        ANON,
        "fauna.recovery.replacement.veto",
        &ReplacementVetoRequest {
            actor_id: ByteBuf::from(actor_b.to_vec()),
            nonce: ByteBuf::from(nonce.to_vec()),
            signature: ByteBuf::from(sig_b),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("another account cannot spend A's nonce");
    assert_eq!(err.code, "fauna.recovery.invalid_nonce");

    // Spend it properly for A…
    let sig_a = ReplacementVeto::new(ActorId(actor_a), nonce)
        .sign(&r1)
        .unwrap();
    let reply: ReplacementVetoReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.recovery.replacement.veto",
        &ReplacementVetoRequest {
            actor_id: ByteBuf::from(actor_a.to_vec()),
            nonce: ByteBuf::from(nonce.to_vec()),
            signature: ByteBuf::from(sig_a.clone()),
            extra: Default::default(),
        },
    )
    .await
    .expect("the rightful veto lands");
    assert!(reply.cancelled);

    // …and a replay of the captured exchange is dead: the nonce is consumed.
    let err: RpcError = common::call::<_, ReplacementVetoReply>(
        &router,
        &state,
        ANON,
        "fauna.recovery.replacement.veto",
        &ReplacementVetoRequest {
            actor_id: ByteBuf::from(actor_a.to_vec()),
            nonce: ByteBuf::from(nonce.to_vec()),
            signature: ByteBuf::from(sig_a),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("a captured veto exchange must not replay");
    assert_eq!(err.code, "fauna.recovery.invalid_nonce");
}

#[tokio::test]
async fn an_uncontested_window_lands_the_replacement_verbatim() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x56);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;
    let record = seed_alone_record(&seed, actor, &r2, 1);
    request_replacement(&router, &state, actor, record.clone())
        .await
        .expect("request parks");

    // Before the window: the sweep must not land it.
    let real_now = fauna_core::data::Timestamp::now_secs();
    let (landed, cancelled) = land_due_replacements(&state, real_now).await.unwrap();
    assert_eq!((landed, cancelled), (0, 0), "the window must run first");

    // After the window: it lands, verbatim, at the head.
    let (landed, cancelled) = land_due_replacements(&state, after_the_window())
        .await
        .unwrap();
    assert_eq!((landed, cancelled), (1, 0));
    let head = state
        .db
        .recovery_registration_head(&actor)
        .await
        .unwrap()
        .expect("chain exists");
    assert_eq!(head.recovery_pubkey, r2.public().to_vec());
    assert_eq!(head.seq, 1);
    assert_eq!(
        head.record, record,
        "the landed record must be the client's bytes, byte-for-byte"
    );
    assert!(
        status(&router, &state, actor).await.pending.is_none(),
        "landing clears the pending"
    );
}

#[tokio::test]
async fn an_authorized_registration_overrides_the_pending() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x57);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r_thief = RecoveryKey::from_bytes([0x99; 32]);
    let r_new = RecoveryKey::from_bytes([0x44; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;

    // A thief holding the seed parks a replacement to their own key…
    request_replacement(
        &router,
        &state,
        actor,
        seed_alone_record(&seed, actor, &r_thief, 1),
    )
    .await
    .expect("thief's request parks");

    // …and the owner, holding R1, overrides with an immediate authorized
    // replacement instead of (or as well as) a veto.
    common::register_recovery_key(&router, &state, &seed, actor, &r_new, Some(&r1), 1).await;
    assert!(
        status(&router, &state, actor).await.pending.is_none(),
        "an authorized registration cancels the pending instantly"
    );

    // Even if the pending row had survived to the sweep, it could not land:
    // park another thief attempt BELOW the new head to prove the sweep's
    // landing-time re-verification (drive the store directly — the handler
    // already refuses a non-advancing seq at request time).
    state
        .db
        .upsert_pending_replacement(
            &actor,
            &seed_alone_record(&seed, actor, &r_thief, 1),
            &r_thief.public(),
            1,
            1_000,
        )
        .await
        .unwrap();
    let (landed, cancelled) = land_due_replacements(&state, after_the_window())
        .await
        .unwrap();
    assert_eq!(
        (landed, cancelled),
        (0, 1),
        "a superseded pending is dropped at landing, never landed"
    );
    let head = state
        .db
        .recovery_registration_head(&actor)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.recovery_pubkey, r_new.public().to_vec());
}

#[tokio::test]
async fn a_request_without_a_chain_is_refused() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x58);
    state
        .db
        .create_user_with_handle(&actor, "free", "nochain", None)
        .await
        .unwrap();
    let r2 = RecoveryKey::from_bytes([0x33; 32]);

    let err = request_replacement(
        &router,
        &state,
        actor,
        seed_alone_record(&seed, actor, &r2, 0),
    )
    .await
    .expect_err("nothing to replace and no veto authority to wait out");
    assert_eq!(err.code, "fauna.recovery.not_registered");
}

#[tokio::test]
async fn a_request_carrying_a_prior_cosignature_is_refused() {
    // A holder of the prior key has the immediate arm and must use it — the
    // window exists only for the key-less case.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x59);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;

    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: r2.public(),
        seq: 1,
        created_at: Timestamp(1_753_100_000),
    };
    let signed = reg.sign(&seed, &r2, Some(&r1)).expect("sign with prior");
    let err = request_replacement(
        &router,
        &state,
        actor,
        canonical_encode(&signed).expect("encode"),
    )
    .await
    .expect_err("a co-signed record must ride the immediate arm");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn a_replayed_request_cannot_extend_the_window_and_another_actor_cannot_park() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x5A);
    let (_, other) = identity(0x5B);
    let r1 = RecoveryKey::from_bytes([0x22; 32]);
    let r2 = RecoveryKey::from_bytes([0x33; 32]);
    common::register_recovery_key(&router, &state, &seed, actor, &r1, None, 0).await;
    state
        .db
        .create_user_with_handle(&other, "free", "other", None)
        .await
        .unwrap();

    let record = seed_alone_record(&seed, actor, &r2, 1);
    let first = request_replacement(&router, &state, actor, record.clone())
        .await
        .expect("first request parks");
    let replayed = request_replacement(&router, &state, actor, record.clone())
        .await
        .expect("replay is accepted idempotently");
    assert_eq!(
        first.lands_at, replayed.lands_at,
        "a replayed request must keep the original window"
    );

    // The connection binding: another authenticated user cannot park a
    // replacement into someone else's identity.
    let err = request_replacement(&router, &state, other, record)
        .await
        .expect_err("record names a different actor than the connection");
    assert_eq!(err.code, "fauna.protocol.malformed");
}
