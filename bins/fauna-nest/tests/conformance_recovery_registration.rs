//! **tier_3** — the RecoveryKey registration chain (identity-succession slice 2):
//! `fauna.recovery.registration.submit` (USER-class) and
//! `fauna.recovery.registration.chain` (pre-identity).
//!
//! Goal doc: `docs/goal/behavior/identity-succession.md` § The RecoveryKey
//! (registration + replacement) and § Enforcement on the home nest (the nest is
//! *enforcer and distributor, never authorizer*, `:104`).
//!
//! **The property under test is that the nest cannot be talked into rewriting a
//! chain.** A registration is authorized entirely by signatures over keys the
//! nest does not hold, so every test here attacks that boundary rather than the
//! happy path:
//!
//! - a first registration lands and is served back **byte-identically** (the
//!   verbatim-bytes contract the serve path depends on);
//! - **a seed thief cannot replace the RecoveryKey** — the arm this whole plane
//!   exists for. Holding the identity seed is enough to *sign* a fresh
//!   registration, and the record's own signatures verify; it is refused solely
//!   because it cannot carry the prior RecoveryKey's co-signature;
//! - the legitimate holder of the prior RecoveryKey *can* replace it, and the
//!   chain keeps both links;
//! - a registration naming another actor is refused even when perfectly signed
//!   (the connection binds the account);
//! - a replay of the current head is refused (`seq` must advance);
//! - the chain read answers for an actor with no account on this nest — the
//!   pre-identity case a verifying peer is in — and answers *empty*, not an
//!   error, for an identity that never registered.

mod common;
use common::identity;
use common::register_user;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{RecoveryKey, RecoveryKeyRegistration};
use fauna_nest::db::CacheDb;
use fauna_nest::recovery_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::RpcError;
use fauna_protocol::recovery::{
    RegistrationChainReply, RegistrationChainRequest, RegistrationSubmitReply,
    RegistrationSubmitRequest,
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

/// Build and sign a registration exactly as a client would, and return the
/// canonical bytes that ride the wire.
fn signed_registration_bytes(
    identity: &SigningKey,
    actor: [u8; 32],
    recovery: &RecoveryKey,
    prior: Option<&RecoveryKey>,
    seq: u64,
) -> ByteBuf {
    let reg = RecoveryKeyRegistration {
        actor_id: ActorId(actor),
        recovery_pubkey: recovery.public(),
        seq,
        created_at: Timestamp(1_753_000_000),
    };
    let signed = reg.sign(identity, recovery, prior).expect("sign");
    ByteBuf::from(canonical_encode(&signed).expect("encode signed registration"))
}

async fn submit(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    registration: ByteBuf,
) -> Result<RegistrationSubmitReply, RpcError> {
    common::call(
        router,
        state,
        actor,
        "fauna.recovery.registration.submit",
        &RegistrationSubmitRequest {
            registration,
            extra: Default::default(),
        },
    )
    .await
}

async fn chain(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    of_actor: [u8; 32],
) -> Result<RegistrationChainReply, RpcError> {
    common::call(
        router,
        state,
        caller,
        "fauna.recovery.registration.chain",
        &RegistrationChainRequest {
            actor_id: ByteBuf::from(of_actor.to_vec()),
            extra: Default::default(),
        },
    )
    .await
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn a_first_registration_lands_and_is_served_back_byte_identically() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    register_user(&state, actor, "owner").await;

    let recovery = RecoveryKey::generate();
    let bytes = signed_registration_bytes(&seed, actor, &recovery, None, 1);

    let reply = submit(&router, &state, actor, bytes.clone()).await.unwrap();
    assert_eq!(reply.seq, 1);

    let served = chain(&router, &state, actor, actor).await.unwrap();
    assert_eq!(served.registrations.len(), 1);
    // Byte-identical: the serve path replays what was signed rather than
    // re-encoding a record the nest did not author. A consumer verifies
    // signatures over exactly these bytes.
    assert_eq!(
        served.registrations[0], bytes,
        "the chain must serve the submitted bytes verbatim"
    );
}

#[tokio::test]
async fn a_seed_thief_cannot_replace_the_registered_recovery_key() {
    // The arm this entire plane exists for. The thief holds the identity seed —
    // everything the account previously *was* — and can therefore mint a fresh
    // RecoveryKey and sign a structurally perfect registration for it. What
    // they cannot do is co-sign with the RecoveryKey they do not hold.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x22);
    register_user(&state, actor, "victim").await;

    let owners_recovery = RecoveryKey::generate();
    submit(
        &router,
        &state,
        actor,
        signed_registration_bytes(&seed, actor, &owners_recovery, None, 1),
    )
    .await
    .unwrap();

    // The thief, holding `seed`, mints their own RecoveryKey and registers it
    // at seq 2 with no prior co-signature.
    let thiefs_recovery = RecoveryKey::generate();
    let err = submit(
        &router,
        &state,
        actor,
        signed_registration_bytes(&seed, actor, &thiefs_recovery, None, 2),
    )
    .await
    .expect_err("a seed-only replacement must be refused");
    assert_eq!(err.code, "fauna.protocol.malformed", "got: {err:?}");

    // Signing the prior co-signature with the *thief's own* key does not help
    // either — it must verify under the key the nest already knows.
    let err = submit(
        &router,
        &state,
        actor,
        signed_registration_bytes(&seed, actor, &thiefs_recovery, Some(&thiefs_recovery), 2),
    )
    .await
    .expect_err("a self-co-signed replacement must be refused");
    assert_eq!(err.code, "fauna.protocol.malformed", "got: {err:?}");

    // The owner's key is still the head — the thief changed nothing.
    let served = chain(&router, &state, actor, actor).await.unwrap();
    assert_eq!(
        served.registrations.len(),
        1,
        "the chain must be untouched by the refused attempts"
    );
    let head = state
        .db
        .recovery_registration_head(&actor[..])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.recovery_pubkey, owners_recovery.public().to_vec());
}

#[tokio::test]
async fn the_prior_recovery_key_holder_can_replace_it_and_the_chain_keeps_both_links() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x33);
    register_user(&state, actor, "rotator").await;

    let first = RecoveryKey::generate();
    let second = RecoveryKey::generate();
    submit(
        &router,
        &state,
        actor,
        signed_registration_bytes(&seed, actor, &first, None, 1),
    )
    .await
    .unwrap();

    // The legitimate replacement: co-signed by the prior RecoveryKey.
    let reply = submit(
        &router,
        &state,
        actor,
        signed_registration_bytes(&seed, actor, &second, Some(&first), 2),
    )
    .await
    .unwrap();
    assert_eq!(reply.seq, 2);

    let served = chain(&router, &state, actor, actor).await.unwrap();
    assert_eq!(
        served.registrations.len(),
        2,
        "history is kept, not replaced"
    );
    let head = state
        .db
        .recovery_registration_head(&actor[..])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.recovery_pubkey, second.public().to_vec());
}

#[tokio::test]
async fn a_registration_naming_another_actor_is_refused() {
    // The record is perfectly signed — by the *attacker's* own seed, for the
    // attacker's own actor_id — but submitted on a connection authenticated as
    // someone else. Without the connection binding this would append a
    // stranger's key material into the victim's chain namespace.
    let (router, state) = nest().await;
    let (attacker_seed, attacker) = identity(0x44);
    let (_victim_seed, victim) = identity(0x55);
    register_user(&state, attacker, "attacker").await;
    register_user(&state, victim, "victim2").await;

    let recovery = RecoveryKey::generate();
    let bytes = signed_registration_bytes(&attacker_seed, attacker, &recovery, None, 1);

    // Submitted on the victim's connection, carrying the attacker's actor_id.
    let err = submit(&router, &state, victim, bytes)
        .await
        .expect_err("actor_id must match the authenticated actor");
    assert_eq!(err.code, "fauna.protocol.malformed", "got: {err:?}");

    assert!(
        chain(&router, &state, victim, victim)
            .await
            .unwrap()
            .registrations
            .is_empty()
    );
}

#[tokio::test]
async fn a_replay_of_the_head_is_refused() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x66);
    register_user(&state, actor, "replayed").await;

    let recovery = RecoveryKey::generate();
    let bytes = signed_registration_bytes(&seed, actor, &recovery, None, 1);
    submit(&router, &state, actor, bytes.clone()).await.unwrap();

    // Re-delivering the identical submit (the auto-retry case) must not fork or
    // duplicate the chain. This is why the kind is safe as
    // `forbid_replay = false`.
    let err = submit(&router, &state, actor, bytes)
        .await
        .expect_err("a replayed registration must be refused");
    assert_eq!(err.code, "fauna.protocol.malformed", "got: {err:?}");

    assert_eq!(
        chain(&router, &state, actor, actor)
            .await
            .unwrap()
            .registrations
            .len(),
        1
    );
}

#[tokio::test]
async fn the_chain_answers_a_caller_with_no_account_on_this_nest() {
    // The pre-identity case: a federation peer or another user's client
    // verifying a succession statement holds an old actor_id and nothing else.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x77);
    register_user(&state, actor, "published").await;

    let recovery = RecoveryKey::generate();
    submit(
        &router,
        &state,
        actor,
        signed_registration_bytes(&seed, actor, &recovery, None, 1),
    )
    .await
    .unwrap();

    // Called as the all-zero anonymous actor — no account, no bearer.
    let served = chain(&router, &state, ANON, actor).await.unwrap();
    assert_eq!(served.registrations.len(), 1);

    // And an identity that never registered gets an empty chain, not an error:
    // "no RecoveryKey, no succession capability" is an honest answer
    // (`identity-succession.md:46`).
    let (_, unregistered) = identity(0x78);
    let served = chain(&router, &state, ANON, unregistered).await.unwrap();
    assert!(served.registrations.is_empty());
}

#[tokio::test]
async fn a_malformed_chain_request_is_refused_rather_than_queried() {
    let (router, state) = nest().await;
    let err = common::call::<_, RegistrationChainReply>(
        &router,
        &state,
        ANON,
        "fauna.recovery.registration.chain",
        &RegistrationChainRequest {
            actor_id: ByteBuf::from(vec![0u8; 31]),
            extra: Default::default(),
        },
    )
    .await
    .expect_err("a short actor_id must be refused");
    assert_eq!(err.code, "fauna.protocol.malformed", "got: {err:?}");
}

#[tokio::test]
async fn a_registration_that_is_not_a_signed_record_is_refused() {
    let (router, state) = nest().await;
    let (_, actor) = identity(0x79);
    register_user(&state, actor, "garbage").await;

    let err = submit(&router, &state, actor, ByteBuf::from(vec![0xff; 16]))
        .await
        .expect_err("garbage must not decode into a registration");
    assert_eq!(err.code, "fauna.protocol.malformed", "got: {err:?}");
}
