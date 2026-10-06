//! Round-trip for the **post-quantum** branch of the broadcast subscription
//! `KeyBlob` — surface B, slice S4 (`post-quantum.md` § Implementation
//! status → Subscription `KeyBlobEntry`).
//!
//! **tier_3** — exercises the real `fauna.subscriptions.*` handler dispatch
//! for the whole client-minted loop: the subscriber publishes an ML-KEM ek on
//! `subscribe`, the nest carries it (pending `subscribe_requests` row →
//! `requests.list`; `subscribers` row → `subscribers.list`), the author's
//! client mints over it with the real shared-Rust selector
//! (`mint_key_blob` → `create_key_blob_entry_auto`), the nest verifies and
//! stores the blob (`requests.approve` / `key_blob.rotate`), and the
//! subscriber unwraps the stored, signed blob (`decrypt_key_blob_entry_for`).
//! The nest wraps nothing itself; only this depth catches the publish → carry
//! → wrap → unwrap seam end-to-end.
//!
//! What it pins:
//! - A subscriber who **publishes** a valid ML-KEM ek gets an `Xwing` entry
//!   (no capability token gates it), and that subscriber unwraps the exact
//!   period key from it.
//! - A subscriber who publishes **no** ek gets a `Classical` entry (degrade),
//!   and unwraps the same period key — proving mixed-suite rosters read
//!   uniformly (design `:91-95`).
//! - A subscriber who publishes an ek only on a later re-subscribe is upgraded
//!   to `Xwing` at the author's next rotation.
//! - A subscriber whose published ek is the **wrong length** is rejected at the
//!   handler (`fauna.subscriptions.malformed_upload`) before any DB write.
//!
//! Authority: `docs/goal/architecture/security/post-quantum.md`
//! § Post-quantum key publication and derivation (subscriptions),
//! § Implementation status — Subscription `KeyBlobEntry`.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::{Capability, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::crypto::{
    MLKEM768_ENCAPS_KEY_LEN, decrypt_key_blob_entry_for, subscriber_mlkem_encaps_key,
};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob, KeyBlobEntry};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::subscription_handlers::register_subscription_handlers;
use fauna_protocol::subscriptions::{
    ApproveRequestReply, ApproveRequestRequest, PendingRequest, RequestsListReply,
    RequestsListRequest, RotateKeyBlobReply, RotateKeyBlobRequest, SubscribeReply,
    SubscribeRequest, SubscribersListReply, SubscribersListRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};
use serde_bytes::ByteBuf;

use common::encrypted_keyblob::{make_device_authorization, mint_test_key_blob_suite};

const TIER: &str = "gold";
/// The broadcast period key the author's client wraps to each subscriber.
/// Distinct bytes so an unwrap that returns it can't be a zeroed-buffer false
/// positive.
const PERIOD_KEY: [u8; 32] = [0xAB; 32];

async fn fresh_state() -> Arc<AppState> {
    Arc::new(AppState::for_test(Arc::new(
        CacheDb::open_in_memory().expect("in-memory db"),
    )))
}

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_subscription_handlers(&mut b);
    b.build()
}

async fn dispatch<T: serde::Serialize>(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    kind: &str,
    req: &T,
) -> Result<Bytes, RpcError> {
    common::seed_dispatch_actor(&state.db, &caller).await;
    let payload = Bytes::from(encode_canonical(req).unwrap().to_vec());
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state.clone(), caller, payload).await
}

/// The author's `gold` tier, created through the real `tiers.create` door so it
/// carries its birth blob (`common::create_tier`).
async fn create_gold(router: &RpcRouter, state: &Arc<AppState>, author: &ActorKeypair) {
    let reply = common::create_tier(
        router,
        state.clone(),
        author,
        common::tier_create_request(author, TIER, 1),
    )
    .await
    .expect("tiers.create ok");
    assert!(reply.created);
}

/// Dispatch `fauna.subscriptions.subscribe` for `subscriber` under `author`,
/// optionally publishing the subscriber's ML-KEM ek.
async fn subscribe(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: ActorId,
    subscriber: &ActorKeypair,
    mlkem_encaps_key: Option<Vec<u8>>,
) -> Result<SubscribeReply, RpcError> {
    let req = SubscribeRequest {
        author_id: author,
        tier: TIER.to_string(),
        mlkem_encaps_key: mlkem_encaps_key.map(ByteBuf::from),
        extra: Default::default(),
    };
    let reply = dispatch(
        router,
        state,
        subscriber.actor_id().0,
        "fauna.subscriptions.subscribe",
        &req,
    )
    .await?;
    Ok(decode(&reply).expect("decode subscribe reply"))
}

/// The author's pending request from `subscriber`, as `requests.list` serves it
/// — the ek the author's mint reads rides here.
async fn pending_from(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    subscriber: &ActorKeypair,
) -> PendingRequest {
    let reply = dispatch(
        router,
        state,
        author.actor_id().0,
        "fauna.subscriptions.requests.list",
        &RequestsListRequest {},
    )
    .await
    .expect("requests.list ok");
    let list: RequestsListReply = decode(&reply).unwrap();
    list.requests
        .into_iter()
        .find(|r| r.subscriber_id == subscriber.actor_id() && r.tier_name == TIER)
        .expect("the subscribe is pending for the author")
}

fn ek_of(published: &Option<ByteBuf>) -> Option<[u8; MLKEM768_ENCAPS_KEY_LEN]> {
    published
        .as_ref()
        .map(|b| b.as_ref().try_into().expect("a stored ek is 1184 bytes"))
}

/// The author client's mint over `roster` (each with its published ek), as
/// `requests.approve` or `key_blob.rotate` carries it.
fn author_mint(
    author: &ActorKeypair,
    rotated_at: u64,
    roster: &[(ActorId, Option<[u8; MLKEM768_ENCAPS_KEY_LEN]>)],
) -> fauna_protocol::subscriptions::EncryptedKeyBlobUpload {
    let auth = make_device_authorization(author, author, vec![Capability::ManageSubscribers]);
    let ids: Vec<ActorId> = roster.iter().map(|(id, _)| *id).collect();
    let eks: Vec<_> = roster.iter().map(|(_, ek)| *ek).collect();
    mint_test_key_blob_suite(
        author,
        &auth,
        TIER,
        Timestamp(rotated_at),
        &ids,
        &eks,
        &PERIOD_KEY,
    )
    .1
}

async fn approve(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    request_id: i64,
    upload: fauna_protocol::subscriptions::EncryptedKeyBlobUpload,
) -> ApproveRequestReply {
    let req = ApproveRequestRequest {
        request_id,
        encrypted_upload: Some(upload),
        extra: Default::default(),
    };
    let reply = dispatch(
        router,
        state,
        author.actor_id().0,
        "fauna.subscriptions.requests.approve",
        &req,
    )
    .await
    .expect("requests.approve ok");
    decode(&reply).unwrap()
}

/// Read the latest stored broadcast `KeyBlob` for `(author, TIER)`, decoding the
/// dag-cbor `EmbedAsBytes` → signed inner `KeyBlob` (sign-over-CID), and return
/// its entries.
async fn stored_keyblob_entries(state: &Arc<AppState>, author: &[u8; 32]) -> Vec<KeyBlobEntry> {
    let (_v, _hash, blob_data) = state
        .db
        .get_current_key_blob(author, TIER)
        .await
        .unwrap()
        .expect("the tier has a stored KeyBlob");
    let wire: EmbedAsBytes =
        canonical_decode(&blob_data).expect("blob_data is dag-cbor EmbedAsBytes");
    let blob: KeyBlob = decode_signed_bytes(&wire.bytes).expect("inner bytes decode as KeyBlob");
    blob.entries
}

fn entry_for<'a>(entries: &'a [KeyBlobEntry], subscriber: &ActorKeypair) -> &'a KeyBlobEntry {
    entries
        .iter()
        .find(|e| e.subscriber.0 == subscriber.actor_id().0)
        .expect("subscriber has an entry in the roster")
}

/// A subscriber who published a valid ML-KEM ek gets an `Xwing` entry, and
/// unwraps the exact period key from it; one who published none gets a
/// `Classical` entry and unwraps the same key — a mixed-suite roster reading
/// uniformly.
#[tokio::test]
async fn a_published_ek_rides_to_the_authors_mint_as_xwing_and_unwraps() {
    let state = fresh_state().await;
    let router = router();

    let author = ActorKeypair::from_secret([0x11; 32]);
    let pq_sub = ActorKeypair::from_secret([0x22; 32]);
    let classical_sub = ActorKeypair::from_secret([0x33; 32]);
    create_gold(&router, &state, &author).await;

    // PQ subscriber publishes the ek derived from their identity seed.
    let pq_ek = subscriber_mlkem_encaps_key(&pq_sub).to_vec();
    assert_eq!(pq_ek.len(), 1184, "ML-KEM-768 ek is 1184 bytes");
    let reply = subscribe(&router, &state, author.actor_id(), &pq_sub, Some(pq_ek))
        .await
        .expect("pq subscribe ok");
    assert!(matches!(reply, SubscribeReply::Queued { .. }));
    // Classical subscriber publishes nothing.
    let reply = subscribe(&router, &state, author.actor_id(), &classical_sub, None)
        .await
        .expect("classical subscribe ok");
    assert!(matches!(reply, SubscribeReply::Queued { .. }));

    // The author's client approves each from what `requests.list` carries.
    let pq_req = pending_from(&router, &state, &author, &pq_sub).await;
    let pq_ek = ek_of(&pq_req.mlkem_encaps_key);
    assert!(
        pq_ek.is_some(),
        "the published ek rides the pending request"
    );
    approve(
        &router,
        &state,
        &author,
        pq_req.request_id,
        author_mint(
            &author,
            1_700_000_100_000_000,
            &[(pq_sub.actor_id(), pq_ek)],
        ),
    )
    .await;
    let classical_req = pending_from(&router, &state, &author, &classical_sub).await;
    assert!(classical_req.mlkem_encaps_key.is_none());
    approve(
        &router,
        &state,
        &author,
        classical_req.request_id,
        author_mint(
            &author,
            1_700_000_200_000_000,
            &[(pq_sub.actor_id(), pq_ek), (classical_sub.actor_id(), None)],
        ),
    )
    .await;

    let entries = stored_keyblob_entries(&state, &author.actor_id().0).await;
    assert_eq!(entries.len(), 2, "both subscribers in the roster");

    // PQ entry: X-Wing suite, X-Wing frame length, unwraps to the period key.
    let pq_entry = entry_for(&entries, &pq_sub);
    assert_eq!(pq_entry.suite, KemSuiteId::Xwing, "published ek ⇒ X-Wing");
    // 1120 (X-Wing ct) + 12 (nonce) + 32 (key) + 16 (tag).
    assert_eq!(pq_entry.encrypted_key.len(), 1180, "X-Wing frame length");
    let unwrapped = decrypt_key_blob_entry_for(&pq_sub, pq_entry).expect("pq unwrap ok");
    assert_eq!(unwrapped, PERIOD_KEY, "X-Wing unwrap yields the period key");

    // Classical entry: classical suite, classical frame length, same key.
    let classical_entry = entry_for(&entries, &classical_sub);
    assert_eq!(
        classical_entry.suite,
        KemSuiteId::Classical,
        "no published ek ⇒ Classical"
    );
    // 32 (ephemeral pk) + 12 (nonce) + 32 (key) + 16 (tag).
    assert_eq!(
        classical_entry.encrypted_key.len(),
        92,
        "classical frame length"
    );
    let unwrapped =
        decrypt_key_blob_entry_for(&classical_sub, classical_entry).expect("classical unwrap ok");
    assert_eq!(
        unwrapped, PERIOD_KEY,
        "classical unwrap yields the period key"
    );

    // Cross-suite negative: the classical subscriber cannot open the PQ entry
    // — each entry is bound to its own subscriber.
    assert!(
        decrypt_key_blob_entry_for(&classical_sub, pq_entry).is_err(),
        "wrong subscriber cannot open the X-Wing entry"
    );
}

/// SUB-1 (surface-B PQ-migration completeness): an already-subscribed subscriber
/// who FIRST joined classically (no published ek) and LATER re-subscribes carrying
/// a freshly published ML-KEM ek is upgraded classical → X-Wing. The idempotency
/// early-return in `subscribe_handler` must persist the late-published ek, so
/// `subscribers.list` serves it and the author's next rotation re-wraps an
/// `Xwing` entry (not the stale `Classical`).
///
/// Without the fix the re-subscribe early-returns `Approved` and silently drops
/// the published ek (the `subscribers` row keeps a NULL ek), so the subscriber
/// stays `Classical` (HNDL-exposed) forever — the author can never upgrade them.
#[tokio::test]
async fn a_late_published_ek_upgrades_a_classical_subscriber_at_the_next_rotation() {
    let state = fresh_state().await;
    let router = router();

    let author = ActorKeypair::from_secret([0x66; 32]);
    let sub = ActorKeypair::from_secret([0x77; 32]);
    create_gold(&router, &state, &author).await;

    // 1) `sub` joins classically — no published ek → Classical entry.
    subscribe(&router, &state, author.actor_id(), &sub, None)
        .await
        .expect("classical subscribe ok");
    let req = pending_from(&router, &state, &author, &sub).await;
    approve(
        &router,
        &state,
        &author,
        req.request_id,
        author_mint(&author, 1_700_000_100_000_000, &[(sub.actor_id(), None)]),
    )
    .await;
    let entries = stored_keyblob_entries(&state, &author.actor_id().0).await;
    assert_eq!(
        entry_for(&entries, &sub).suite,
        KemSuiteId::Classical,
        "initial join with no ek is classical"
    );

    // 2) `sub` re-subscribes, now publishing their identity-derived ek. They are
    //    already a subscriber, so this hits the idempotency early-return — which
    //    must persist the freshly published ek rather than drop it.
    let sub_ek = subscriber_mlkem_encaps_key(&sub).to_vec();
    let reply = subscribe(&router, &state, author.actor_id(), &sub, Some(sub_ek))
        .await
        .expect("re-subscribe ok");
    assert!(
        matches!(reply, SubscribeReply::Approved { .. }),
        "re-subscribe of an existing subscriber returns Approved"
    );

    // 3) The author's next rotation reads the roster's eks from
    //    `subscribers.list` and must now wrap `sub` as X-Wing.
    let listed = dispatch(
        &router,
        &state,
        author.actor_id().0,
        "fauna.subscriptions.subscribers.list",
        &SubscribersListRequest {
            tier_name: TIER.into(),
            extra: Default::default(),
        },
    )
    .await
    .expect("subscribers.list ok");
    let listed: SubscribersListReply = decode(&listed).unwrap();
    let roster: Vec<_> = listed
        .subscribers
        .iter()
        .map(|s| (s.subscriber_id, ek_of(&s.mlkem_encaps_key)))
        .collect();
    assert!(
        roster.iter().all(|(_, ek)| ek.is_some()),
        "the late-published ek is served to the author"
    );
    let rotate = RotateKeyBlobRequest {
        tier_name: TIER.into(),
        encrypted_upload: author_mint(&author, 1_700_000_200_000_000, &roster),
        extra: Default::default(),
    };
    let reply = dispatch(
        &router,
        &state,
        author.actor_id().0,
        "fauna.subscriptions.key_blob.rotate",
        &rotate,
    )
    .await
    .expect("key_blob.rotate ok");
    let _: RotateKeyBlobReply = decode(&reply).unwrap();

    let entries = stored_keyblob_entries(&state, &author.actor_id().0).await;
    let sub_entry = entry_for(&entries, &sub);
    assert_eq!(
        sub_entry.suite,
        KemSuiteId::Xwing,
        "late-published ek upgrades the existing subscriber to X-Wing"
    );
    assert_eq!(sub_entry.encrypted_key.len(), 1180, "X-Wing frame length");
    let unwrapped = decrypt_key_blob_entry_for(&sub, sub_entry).expect("upgraded X-Wing unwrap ok");
    assert_eq!(
        unwrapped, PERIOD_KEY,
        "upgraded entry still unwraps the period key"
    );
}

/// A published ek of the wrong length is rejected at the handler before any DB
/// write (mirrors the mail leg-A length gate).
#[tokio::test]
async fn subscribe_with_malformed_ek_is_rejected() {
    let state = fresh_state().await;
    let router = router();

    let author = ActorKeypair::from_secret([0x44; 32]);
    let sub = ActorKeypair::from_secret([0x55; 32]);
    create_gold(&router, &state, &author).await;

    // 1183 bytes — one short of ML-KEM-768.
    let bad_ek = vec![0u8; 1183];
    let err = subscribe(&router, &state, author.actor_id(), &sub, Some(bad_ek))
        .await
        .expect_err("malformed ek must be rejected");
    assert_eq!(err.code, "fauna.subscriptions.malformed_upload");

    // Neither a subscriber row nor a pending request was written.
    assert!(
        !state
            .db
            .is_subscriber(&author.actor_id().0, &sub.actor_id().0, TIER)
            .await
            .unwrap(),
        "rejected subscribe must not persist a subscriber"
    );
    assert!(
        state
            .db
            .list_subscribe_requests(&author.actor_id().0)
            .await
            .unwrap()
            .is_empty(),
        "rejected subscribe must not enqueue a request"
    );
}
