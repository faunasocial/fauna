//! **tier_3** — the seed-escrow plane (identity-succession slice 2):
//! `fauna.recovery.escrow.put` (USER-class) and the pre-identity,
//! RecoveryKey-challenge-gated `fauna.recovery.escrow.{challenge,fetch}`.
//!
//! Goal doc: `docs/goal/behavior/identity-succession.md` § Seed escrow — the
//! blob is the identity seed HPKE-sealed to the RecoveryKey's escrow public
//! half, stored opaque, and reopened by *"a fresh client with only the phrase"*
//! over a nonce the nest issues and the RecoveryKey signs, rate-limited.
//!
//! **The scenario under test is total device loss**, so every test drives the
//! fetch as a pre-identity caller — no account, no session, no identity seed —
//! and the seal/unseal is the real shared-Rust one
//! (`fauna_mls::wrapped_blob::{seal_seed_escrow, unseal_seed_escrow}`), not a
//! stand-in. A test that faked either half would pass over exactly the failure
//! that matters: a blob the nest stores but no kit can open.
//!
//! What each test attacks:
//!
//! - the capstone — the phrase alone recovers the *exact* identity seed;
//! - a seed thief (or anyone else) cannot fetch: authorization is the RecoveryKey
//!   signature, and nothing else opens the plane;
//! - a nonce is single-use and account-bound, so a captured exchange cannot be
//!   replayed and one account's challenge cannot fetch another's blob;
//! - a **superseded kit stops working the moment a replacement registration
//!   lands** — the verify reads the chain head, not any historical link;
//! - the blob's **lifecycle follows the key it is sealed to**: a replacement
//!   with no re-put answers `no_escrow` (never a stale blob no current kit can
//!   open), and after a succession the retired kit gets the `superseded`
//!   refusal — with the row itself deleted in the succession transaction;
//! - the nest holds ciphertext: the stored row carries no seed bytes and is
//!   served back byte-identically.

mod common;
use common::identity;

use std::sync::Arc;

use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::identity::ActorId;
use fauna_core::recovery::{EscrowChallenge, IdentitySuccession, RecoveryKey};
use fauna_mls::wrapped_blob::format::SeedEscrowBlob;
use fauna_mls::wrapped_blob::{seal_seed_escrow, unseal_seed_escrow};
use fauna_nest::db::CacheDb;
use fauna_nest::db::recovery_escrow::RECOVERY_ESCROW_MAX_LEN;
use fauna_nest::recovery_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::RpcError;
use fauna_protocol::recovery::{
    EscrowChallengeReply, EscrowChallengeRequest, EscrowFetchReply, EscrowFetchRequest,
    EscrowPutReply, EscrowPutRequest, EscrowStatusReply, EscrowStatusRequest,
    SuccessionSubmitReply, SuccessionSubmitRequest,
};
use serde_bytes::ByteBuf;

/// The anonymous connection's actor — what a pre-identity caller presents. A
/// client recovering from total device loss has nothing else.
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

async fn put_escrow(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    blob: Vec<u8>,
) -> Result<EscrowPutReply, RpcError> {
    common::call(
        router,
        state,
        actor,
        "fauna.recovery.escrow.put",
        &EscrowPutRequest {
            blob: ByteBuf::from(blob),
            extra: Default::default(),
        },
    )
    .await
}

/// Request a nonce **as a pre-identity caller** — the only way a client that
/// lost every device can start the ceremony.
async fn challenge(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> EscrowChallengeReply {
    common::call(
        router,
        state,
        ANON,
        "fauna.recovery.escrow.challenge",
        &EscrowChallengeRequest {
            actor_id: ByteBuf::from(actor.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("a challenge is issued unconditionally")
}

async fn fetch_with(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    nonce: &[u8],
    signature: Vec<u8>,
) -> Result<EscrowFetchReply, RpcError> {
    common::call(
        router,
        state,
        ANON,
        "fauna.recovery.escrow.fetch",
        &EscrowFetchRequest {
            actor_id: ByteBuf::from(actor.to_vec()),
            nonce: ByteBuf::from(nonce.to_vec()),
            signature: ByteBuf::from(signature),
            extra: Default::default(),
        },
    )
    .await
}

/// Sign the issued nonce with a recovery root, exactly as a restoring client
/// does — the whole authorization of the fetch.
fn sign_challenge(actor: [u8; 32], nonce: &[u8], recovery: &RecoveryKey) -> Vec<u8> {
    let nonce: [u8; 32] = nonce.try_into().expect("32-byte nonce");
    EscrowChallenge::new(ActorId(actor), nonce)
        .sign(recovery)
        .expect("sign the challenge")
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn the_phrase_alone_recovers_the_identity_seed_after_total_device_loss() {
    // The capstone: everything the user has left is the 64-hex recovery phrase.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    let recovery = RecoveryKey::generate();
    let phrase = recovery.to_hex();

    common::register_recovery_key(&router, &state, &seed, actor, &recovery, None, 0).await;

    // Kit creation: the client seals its identity seed to the kit and stores the
    // blob on its home nest.
    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &recovery.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, blob.clone())
        .await
        .expect("the owner stores its own blob");

    // ── Every device is now gone. A fresh client holds only the phrase. ──
    let restored_kit = RecoveryKey::from_hex(&phrase).expect("the phrase parses");
    let issued = challenge(&router, &state, actor).await;
    let reply = fetch_with(
        &router,
        &state,
        actor,
        &issued.nonce,
        sign_challenge(actor, &issued.nonce, &restored_kit),
    )
    .await
    .expect("a RecoveryKey holder fetches the blob");

    // The blob must come back byte-identical, or it will not open.
    assert_eq!(reply.blob.as_ref(), blob.as_slice());

    let recovered = unseal_seed_escrow(
        &SeedEscrowBlob::from_canonical_bytes(&reply.blob).expect("decode"),
        &restored_kit.escrow_secret(),
    )
    .expect("the phrase unseals the blob");

    assert_eq!(
        *recovered,
        seed.to_bytes(),
        "recovery must yield the exact identity seed — the account itself"
    );
}

#[tokio::test]
async fn only_the_registered_recovery_key_opens_the_escrow_plane() {
    // A seed thief holds the identity seed and can mint any RecoveryKey they
    // like; what they cannot do is sign under the one already registered.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    let owner_kit = RecoveryKey::generate();
    let thief_kit = RecoveryKey::generate();

    common::register_recovery_key(&router, &state, &seed, actor, &owner_kit, None, 0).await;
    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &owner_kit.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, blob).await.expect("put");

    let issued = challenge(&router, &state, actor).await;
    let err = fetch_with(
        &router,
        &state,
        actor,
        &issued.nonce,
        sign_challenge(actor, &issued.nonce, &thief_kit),
    )
    .await
    .expect_err("a foreign recovery root must not open the plane");
    assert_eq!(err.code, "fauna.recovery.signature_failed");

    // And an unsigned attempt is refused just as flatly.
    let issued = challenge(&router, &state, actor).await;
    assert!(
        fetch_with(&router, &state, actor, &issued.nonce, vec![0u8; 64])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_challenge_is_single_use_so_a_captured_exchange_cannot_be_replayed() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    let kit = RecoveryKey::generate();

    common::register_recovery_key(&router, &state, &seed, actor, &kit, None, 0).await;
    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &kit.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, blob).await.expect("put");

    let issued = challenge(&router, &state, actor).await;
    let signature = sign_challenge(actor, &issued.nonce, &kit);

    fetch_with(&router, &state, actor, &issued.nonce, signature.clone())
        .await
        .expect("the first redemption succeeds");

    let err = fetch_with(&router, &state, actor, &issued.nonce, signature)
        .await
        .expect_err("the same nonce must not be redeemable twice");
    assert_eq!(err.code, "fauna.recovery.invalid_nonce");

    // A nonce the nest never issued is refused the same way — the store is the
    // authority on what is outstanding, not the caller.
    assert_eq!(
        fetch_with(&router, &state, actor, &[0x5c; 32], vec![0u8; 64])
            .await
            .expect_err("unknown nonce")
            .code,
        "fauna.recovery.invalid_nonce"
    );
}

#[tokio::test]
async fn one_accounts_challenge_cannot_fetch_anothers_blob() {
    // Both accounts exist and both have kits; the attacker holds their OWN
    // valid kit and a nonce issued to them, and tries to spend it on the
    // victim's row.
    let (router, state) = nest().await;
    let (victim_seed, victim) = identity(0x11);
    let (attacker_seed, attacker) = identity(0x22);
    let victim_kit = RecoveryKey::generate();
    let attacker_kit = RecoveryKey::generate();

    common::register_recovery_key(&router, &state, &victim_seed, victim, &victim_kit, None, 0)
        .await;
    common::register_recovery_key(
        &router,
        &state,
        &attacker_seed,
        attacker,
        &attacker_kit,
        None,
        0,
    )
    .await;
    let victim_blob = seal_seed_escrow(
        &victim_seed.to_bytes(),
        &victim,
        &victim_kit.escrow_public(),
    )
    .expect("seal")
    .to_canonical_bytes()
    .expect("encode");
    put_escrow(&router, &state, victim, victim_blob)
        .await
        .expect("put");

    let mine = challenge(&router, &state, attacker).await;

    // The nonce was issued for the attacker's account, so presenting it against
    // the victim's is refused before any signature is even considered.
    assert_eq!(
        fetch_with(
            &router,
            &state,
            victim,
            &mine.nonce,
            sign_challenge(victim, &mine.nonce, &attacker_kit),
        )
        .await
        .expect_err("a nonce is bound to the account it was issued for")
        .code,
        "fauna.recovery.invalid_nonce"
    );

    // ...and that failed attempt must not have burned the attacker's own
    // outstanding nonce, since the store only consumes on a full match. Reaching
    // `no_escrow` (rather than `invalid_nonce`) is the proof: the nonce was
    // still live and the signature still verified — the caller simply has no
    // blob of their own. Without this half, a wrong-actor attempt would be a
    // free way to invalidate someone else's outstanding challenge.
    assert_eq!(
        fetch_with(
            &router,
            &state,
            attacker,
            &mine.nonce,
            sign_challenge(attacker, &mine.nonce, &attacker_kit),
        )
        .await
        .expect_err("the attacker has no blob of their own")
        .code,
        "fauna.recovery.no_escrow"
    );
}

#[tokio::test]
async fn a_superseded_recovery_kit_stops_opening_the_plane() {
    // Replacement is the whole point of a chain: once a new kit is registered,
    // the old one still produces perfectly valid signatures that authorize
    // nothing. The verify reads the chain HEAD, not any historical link.
    //
    // The flow models the full replacement ceremony: seal to the kit that
    // exists at creation, replace, then the client re-put the ceremony
    // requires (`identity-succession.md` § Seed escrow — replacement is a
    // re-write trigger; the nest deleted the stale row when the head changed).
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    let old_kit = RecoveryKey::generate();
    let new_kit = RecoveryKey::generate();

    common::register_recovery_key(&router, &state, &seed, actor, &old_kit, None, 0).await;
    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &old_kit.escrow_public())
        .expect("seal to the current kit at creation")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, blob).await.expect("put");

    // The RecoveryKey-authorized replacement lands at seq+1, co-signed by the
    // prior key, and the ceremony's client half re-puts sealed to the new kit.
    common::register_recovery_key(&router, &state, &seed, actor, &new_kit, Some(&old_kit), 1).await;
    let reput = seal_seed_escrow(&seed.to_bytes(), &actor, &new_kit.escrow_public())
        .expect("re-seal to the new kit at replacement")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, reput)
        .await
        .expect("re-put");

    let issued = challenge(&router, &state, actor).await;
    assert_eq!(
        fetch_with(
            &router,
            &state,
            actor,
            &issued.nonce,
            sign_challenge(actor, &issued.nonce, &old_kit),
        )
        .await
        .expect_err("a retired kit must not open the plane")
        .code,
        "fauna.recovery.signature_failed"
    );

    let issued = challenge(&router, &state, actor).await;
    fetch_with(
        &router,
        &state,
        actor,
        &issued.nonce,
        sign_challenge(actor, &issued.nonce, &new_kit),
    )
    .await
    .expect("the current kit opens it");
}

#[tokio::test]
async fn an_identity_with_no_registered_kit_is_refused_but_still_gets_a_challenge() {
    // The challenge must reveal nothing: an account with no kit, and an actor id
    // with no account at all, both get a nonce. Every refusal happens at fetch,
    // where a signature was required anyway.
    let (router, state) = nest().await;
    let (_, unregistered) = identity(0x33);

    let issued = challenge(&router, &state, unregistered).await;
    assert_eq!(issued.nonce.len(), 32);
    assert!(issued.expires_at > 0);

    assert_eq!(
        fetch_with(
            &router,
            &state,
            unregistered,
            &issued.nonce,
            sign_challenge(unregistered, &issued.nonce, &RecoveryKey::generate()),
        )
        .await
        .expect_err("no registered key ⇒ nothing can authorize a fetch")
        .code,
        "fauna.recovery.not_registered"
    );
}

#[tokio::test]
async fn the_nest_stores_the_blob_opaque_and_bounded() {
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    let kit = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &seed, actor, &kit, None, 0).await;

    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &kit.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    let reply = put_escrow(&router, &state, actor, blob.clone())
        .await
        .expect("put");
    assert!(reply.updated_at > 0);

    // What rests on disk is the ciphertext, and nothing else: the identity seed
    // must not appear anywhere in the stored bytes.
    let row = state
        .db
        .get_recovery_escrow(&actor[..])
        .await
        .unwrap()
        .expect("row");
    assert_eq!(row.blob, blob);
    assert!(
        !row.blob
            .windows(32)
            .any(|w| w == seed.to_bytes().as_slice()),
        "the identity seed must never rest in the clear on the nest"
    );

    // A re-put replaces in place (kit creation, then succession).
    let (seed2, _) = identity(0x44);
    let replacement = seal_seed_escrow(&seed2.to_bytes(), &actor, &kit.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, replacement.clone())
        .await
        .expect("re-put");
    assert_eq!(
        state
            .db
            .get_recovery_escrow(&actor[..])
            .await
            .unwrap()
            .unwrap()
            .blob,
        replacement
    );

    // The opaque store is not free storage: empty and oversize are refused, and
    // the refusal does not disturb the blob already there.
    assert!(
        put_escrow(&router, &state, actor, Vec::new())
            .await
            .is_err()
    );
    assert!(
        put_escrow(
            &router,
            &state,
            actor,
            vec![0u8; RECOVERY_ESCROW_MAX_LEN + 1]
        )
        .await
        .is_err()
    );
    assert_eq!(
        state
            .db
            .get_recovery_escrow(&actor[..])
            .await
            .unwrap()
            .unwrap()
            .blob,
        replacement
    );
}

#[tokio::test]
async fn a_put_writes_only_the_authenticated_actors_own_row() {
    // The put carries no actor_id — the account comes from the connection — so
    // there is structurally no way to write into another identity's row. Pin
    // that the wire cannot be talked into it anyway.
    let (router, state) = nest().await;
    let (seed_a, actor_a) = identity(0x11);
    let (seed_b, actor_b) = identity(0x22);
    let kit_a = RecoveryKey::generate();
    let kit_b = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &seed_a, actor_a, &kit_a, None, 0).await;
    common::register_recovery_key(&router, &state, &seed_b, actor_b, &kit_b, None, 0).await;

    put_escrow(&router, &state, actor_a, b"a-blob".to_vec())
        .await
        .expect("put as A");

    assert_eq!(
        state
            .db
            .get_recovery_escrow(&actor_a[..])
            .await
            .unwrap()
            .unwrap()
            .blob,
        b"a-blob".to_vec()
    );
    assert!(
        state
            .db
            .get_recovery_escrow(&actor_b[..])
            .await
            .unwrap()
            .is_none(),
        "B's row must be untouched by A's put"
    );
}

#[tokio::test]
async fn a_retired_kit_is_refused_the_old_identitys_seed_after_succession() {
    // `identity-succession.md:44` — at succession "the old one retires with the
    // old identity" — and `:99` — old-sealed corpus is "protected by the auth
    // refusal, not by the seal". The escrow plane must honor both: after a
    // succession the old identity's escrow row is deleted in the succession
    // transaction, and the fetch refuses the retired kit with `superseded`
    // naming the successor (the same routing signal every other ceremony
    // gives a superseded identity), never the old seed.
    let (router, state) = nest().await;
    let (old_seed, old_actor) = identity(0x11);
    let old_kit = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &old_seed, old_actor, &old_kit, None, 1).await;

    let blob = seal_seed_escrow(&old_seed.to_bytes(), &old_actor, &old_kit.escrow_public())
        .expect("seal to the current kit at creation")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, old_actor, blob)
        .await
        .expect("put");

    // The theft-response ceremony: the RecoveryKey re-points the account.
    let (new_seed, new_actor_id) = identity(0x22);
    let statement = IdentitySuccession {
        old_actor_id: ActorId(old_actor),
        new_actor_id: ActorId(new_actor_id),
        recovery_pubkey: old_kit.public(),
        seq: 2,
        created_at: Timestamp(1_753_200_000),
    };
    let signed = statement
        .sign(&old_kit, &new_seed, Some(&old_seed))
        .expect("sign succession");
    let _: SuccessionSubmitReply = common::call(
        &router,
        &state,
        ANON,
        "fauna.recovery.succession.submit",
        &SuccessionSubmitRequest {
            statement: ByteBuf::from(canonical_encode(&signed).expect("encode")),
            extra: Default::default(),
        },
    )
    .await
    .expect("succession lands");

    // The retired kit still signs valid bytes and is still the head of the old
    // chain — but the plane must refuse it, and the refusal must route to the
    // successor.
    let issued = challenge(&router, &state, old_actor).await;
    let err = fetch_with(
        &router,
        &state,
        old_actor,
        &issued.nonce,
        sign_challenge(old_actor, &issued.nonce, &old_kit),
    )
    .await
    .expect_err("a retired kit must not receive the old identity's seed");
    assert_eq!(err.code, "fauna.auth.superseded");
    assert_eq!(err.superseded_by(), Some(new_actor_id));

    // Defense in depth behind the refusal: the ciphertext itself no longer
    // rests on the nest — deleted inside the succession transaction.
    assert!(
        state
            .db
            .get_recovery_escrow(&old_actor[..])
            .await
            .unwrap()
            .is_none(),
        "the old identity's escrow row must be deleted by the succession"
    );
}

#[tokio::test]
async fn a_replacement_with_no_re_put_answers_no_escrow_not_a_stale_blob() {
    // `identity-succession.md:52` — "That degradation is honest and surfaced,
    // never silent." A replacement changes the sealing key, so the blob sealed
    // to the retired kit can never open again; serving it to the new kit is a
    // silent brick (the user believes they hold a working kit and does not).
    // The nest must invalidate the row when the registered pubkey changes, so
    // the fetch answers `no_escrow` — the honest signal that a re-put is owed.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x11);
    let old_kit = RecoveryKey::generate();
    let new_kit = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &seed, actor, &old_kit, None, 0).await;

    // Kit creation seals to the kit that exists at the time — the old one.
    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &old_kit.escrow_public())
        .expect("seal to the current kit at creation")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, blob).await.expect("put");

    // A RecoveryKey-authorized replacement lands; the client re-put that a
    // full ceremony performs is deliberately absent here.
    common::register_recovery_key(&router, &state, &seed, actor, &new_kit, Some(&old_kit), 1).await;

    let issued = challenge(&router, &state, actor).await;
    let err = fetch_with(
        &router,
        &state,
        actor,
        &issued.nonce,
        sign_challenge(actor, &issued.nonce, &new_kit),
    )
    .await
    .expect_err("a stale blob sealed to the retired kit must not be served");
    assert_eq!(err.code, "fauna.recovery.no_escrow");
}

// ═════════════════════════════════════════════════════════════════════════════
// The presence probe (`escrow.status`) — the signed-in half of this plane
// ═════════════════════════════════════════════════════════════════════════════

async fn escrow_status(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
) -> EscrowStatusReply {
    common::call(
        router,
        state,
        actor,
        "fauna.recovery.escrow.status",
        &EscrowStatusRequest::default(),
    )
    .await
    .expect("the presence read is a pure read and never refuses")
}

#[tokio::test]
async fn a_signed_in_device_can_see_that_its_escrow_row_is_missing() {
    // The reason this kind exists. `ui/settings.md` § Recovery kit ratifies a
    // "registered, no escrow" state and says it is surfaced on a signed-in
    // surface "because the only device that can fix it is a signed-in one" —
    // but a signed-in device cannot ask `escrow.fetch`, which is gated on a
    // RecoveryKey signature, and the RecoveryKey is offline-only by iron-clad
    // rule (§ The RecoveryKey — *Custody*). Without this probe the state is
    // invisible to the only party able to repair it.
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x31);
    let kit = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &seed, actor, &kit, None, 0).await;

    // Registered, nothing put yet — the gap the section must warn about.
    let before = escrow_status(&router, &state, actor).await;
    assert!(!before.present, "no blob has been put");
    assert_eq!(before.updated_at, None, "absent means no stamp");

    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &kit.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    let put = put_escrow(&router, &state, actor, blob).await.expect("put");

    let after = escrow_status(&router, &state, actor).await;
    assert!(after.present, "the blob it just put rests");
    assert_eq!(
        after.updated_at,
        Some(put.updated_at),
        "the stamp is the row's own, so a client can tell a re-put from a stale row"
    );
}

#[tokio::test]
async fn the_presence_probe_answers_only_about_the_authenticated_actor() {
    // The whole reason this kind is USER class rather than pre-identity: an
    // unauthenticated presence answer would be a directory oracle mapping actor
    // ids to "holds an escrow blob" — precisely what `escrow.challenge` issues
    // its nonce unconditionally to avoid. The account is taken from the
    // connection and the request carries no actor field at all, so there is no
    // input by which one caller could ask about another.
    let (router, state) = nest().await;
    let (seed_a, actor_a) = identity(0x41);
    let (_seed_b, actor_b) = identity(0x42);
    let kit_a = RecoveryKey::generate();
    let kit_b = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &seed_a, actor_a, &kit_a, None, 0).await;
    common::register_recovery_key(&router, &state, &_seed_b, actor_b, &kit_b, None, 0).await;

    let blob = seal_seed_escrow(&seed_a.to_bytes(), &actor_a, &kit_a.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor_a, blob)
        .await
        .expect("put");

    assert!(
        escrow_status(&router, &state, actor_a).await.present,
        "A sees its own row"
    );
    assert!(
        !escrow_status(&router, &state, actor_b).await.present,
        "B must not learn that A holds a blob"
    );
}

#[tokio::test]
async fn the_presence_probe_follows_the_lifecycle_that_retires_a_row() {
    // The state it reports has to track the row's real lifecycle, not the last
    // thing the client did. A replacement retires the sealing key, so the nest
    // drops the row (§ Seed escrow → *Lifecycle on the nest*) — and the probe
    // must flip to absent, which is exactly the signal that turns the Settings
    // status line into "a re-put is owed".
    let (router, state) = nest().await;
    let (seed, actor) = identity(0x51);
    let old_kit = RecoveryKey::generate();
    let new_kit = RecoveryKey::generate();
    common::register_recovery_key(&router, &state, &seed, actor, &old_kit, None, 0).await;

    let blob = seal_seed_escrow(&seed.to_bytes(), &actor, &old_kit.escrow_public())
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    put_escrow(&router, &state, actor, blob).await.expect("put");
    assert!(escrow_status(&router, &state, actor).await.present);

    // The replacement lands with no re-put — the same gap
    // `a_replacement_with_no_re_put_answers_no_escrow_not_a_stale_blob` pins
    // from the phrase holder's side, seen from the signed-in side.
    common::register_recovery_key(&router, &state, &seed, actor, &new_kit, Some(&old_kit), 1).await;

    let after = escrow_status(&router, &state, actor).await;
    assert!(
        !after.present,
        "the row died with the key it was sealed to; the signed-in surface must see the gap"
    );
    assert_eq!(after.updated_at, None);
}
