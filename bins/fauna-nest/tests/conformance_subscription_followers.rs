//! Integration round-trip for the free **followers** tier — the nest
//! auto-provisions the reserved rank-0 `followers` tier on the first follow, so
//! `subscribe(author, "followers")` succeeds for any author with no pre-created
//! tier (`docs/goal/behavior/monetization.md` § Pillar 1: "follow = subscribe
//! to the free 'followers' tier"; `feed.md` § Encryption at rest: "the
//! always-present default tier is the user's followers").
//!
//! Before this, a fresh actor had no `followers` tier, so the follow button's
//! `subscribe(author, "followers")` returned `fauna.subscriptions.tier_not_found`
//! and an explicit `tiers.create` of a rank-0 tier was rejected (`rank >= 1`).
//!
//! The nest **never mints a period key** — not from `tiers.create`, not from
//! the lazy `followers` provisioning — so every `subscribe`, the first follow
//! included, enqueues (`Queued`); the author's client completes the grant by
//! minting + uploading the `KeyBlob` on approval. The lazily provisioned
//! `followers` tier has no birth blob, so its first approval lands its first
//! blob.
//!
//! Covered:
//! - First follow auto-provisions the reserved tier (rank 0, auto_approve,
//!   free) and enqueues — `tier_not_found` is gone. (Completing the grant —
//!   the author's client minting the followers `KeyBlob` — is the shared-Rust
//!   follow-on.)
//! - Idempotency: a second follow returns the same `Queued { request_id }`,
//!   one pending `subscribe_requests` row.
//! - The rank-0 followers tier does not shadow a paid subscription in
//!   `status.get` (the paid tier is reported, so its offer badge stays
//!   Active) — both grants completed through `requests.approve`, the
//!   followers one landing that tier's first blob.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/subscriptions.rs`.
//! The reserved name/rank: `fauna_core::subscription::{FOLLOWERS_TIER,
//! FOLLOWERS_TIER_RANK}`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::{FOLLOWERS_TIER, FOLLOWERS_TIER_RANK};
use fauna_nest::{db::CacheDb, routes::AppState, rpc_router::RpcRouter, subscription_handlers};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    subscriptions::{
        StatusGetReply, StatusGetRequest, SubscribeReply, SubscribeRequest, TierCreateReply,
        TierCreateRequest,
    },
};

/// `AppState::for_test` already installs the one `Storage` impl
/// (`fauna_nest::storage::SealedStorage`) — no swap needed any more.
async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    (build_router(), state)
}

fn build_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    b.build()
}

async fn subscribe(
    router: &RpcRouter,
    state: Arc<AppState>,
    subscriber: [u8; 32],
    author: [u8; 32],
    tier: &str,
) -> Result<SubscribeReply, RpcError> {
    let req = encode_canonical(&SubscribeRequest {
        author_id: ActorId(author),
        tier: tier.to_string(),
        mlkem_encaps_key: None,
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.subscribe",
        subscriber,
        Bytes::from(req.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode subscribe reply"))
}

async fn status_get(
    router: &RpcRouter,
    state: Arc<AppState>,
    subscriber: [u8; 32],
    author: [u8; 32],
) -> StatusGetReply {
    let req = encode_canonical(&StatusGetRequest {
        author_id: ActorId(author),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.status.get",
        subscriber,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("status.get ok");
    decode(&reply).expect("decode status reply")
}

async fn create_tier(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: &ActorKeypair,
    name: &str,
    rank: u32,
    auto_approve: bool,
) -> Result<TierCreateReply, RpcError> {
    let req = TierCreateRequest {
        auto_approve,
        ..common::tier_create_request(author, name, rank)
    };
    common::create_tier(router, state, author, req).await
}

#[tokio::test]
async fn first_follow_auto_provisions_tier_and_enqueues() {
    // The nest holds no period key, so the first follow enqueues rather than
    // server-side-approving — but crucially NOT `tier_not_found`: the row is auto-provisioned so the
    // `subscribe_requests` FK on `subscription_tiers` holds. The author's
    // client completes the grant later (mints the followers `KeyBlob`).
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let follower = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state
        .db
        .create_user(&follower, "free", "bob")
        .await
        .unwrap();

    // Precondition: the author has no `followers` tier — a bare `subscribe`
    // would have failed with `tier_not_found` before this feature.
    assert!(
        state
            .db
            .get_subscription_tier(&author, FOLLOWERS_TIER)
            .await
            .unwrap()
            .is_none(),
        "no followers tier should exist before the first follow"
    );

    let reply = subscribe(&router, state.clone(), follower, author, FOLLOWERS_TIER)
        .await
        .expect("follow must succeed (auto-provisioned tier), not tier_not_found");
    assert!(
        matches!(reply, SubscribeReply::Queued { .. }),
        "a follow enqueues for the author's client, got {reply:?}"
    );

    // The reserved tier was provisioned: rank 0, auto_approve, free.
    let tier = state
        .db
        .get_subscription_tier(&author, FOLLOWERS_TIER)
        .await
        .unwrap()
        .expect("followers tier auto-provisioned");
    assert_eq!(tier.rank, FOLLOWERS_TIER_RANK);
    assert!(tier.auto_approve);
    assert_eq!(tier.price_hint, None);
    assert_eq!(tier.payment_url, None);

    // Not yet in the roster — the grant completes on author-client approval.
    assert!(
        !state
            .db
            .is_subscriber(&author, &follower, FOLLOWERS_TIER)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn second_follow_is_idempotent() {
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let follower = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state
        .db
        .create_user(&follower, "free", "bob")
        .await
        .unwrap();

    let first = subscribe(&router, state.clone(), follower, author, FOLLOWERS_TIER)
        .await
        .expect("first follow ok");
    let first_request_id = match first {
        SubscribeReply::Queued { request_id } => request_id,
        other => panic!("first follow expected Queued, got {other:?}"),
    };

    let second = subscribe(&router, state.clone(), follower, author, FOLLOWERS_TIER)
        .await
        .expect("second follow ok (idempotent)");
    match second {
        SubscribeReply::Queued { request_id } => assert_eq!(
            request_id, first_request_id,
            "re-follow returns the SAME pending request, not a duplicate"
        ),
        other => panic!("re-follow expected Queued, got {other:?}"),
    }

    // Exactly one pending subscribe_requests row (the auto-provision +
    // subscribe are idempotent).
    let pending = state.db.list_subscribe_requests(&author).await.unwrap();
    let matching: Vec<_> = pending
        .iter()
        .filter(|r| r.kind == "subscribe" && r.tier_name == FOLLOWERS_TIER)
        .collect();
    assert_eq!(matching.len(), 1);
}

#[tokio::test]
async fn paid_subscription_status_not_shadowed_by_followers() {
    // A paid subscriber also holds `followers`, but `status.get` must report
    // the *paid* tier, not "followers" — otherwise the paid
    // `subscription-offer-status` badge (`fauna_core::format::offer_status`)
    // would read inactive.
    let (router, state) = router_with_db().await;
    let author_kp = ActorKeypair::generate();
    let author = author_kp.actor_id().0;
    let subscriber_kp = ActorKeypair::generate();
    let subscriber = subscriber_kp.actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state
        .db
        .create_user(&subscriber, "free", "bob")
        .await
        .unwrap();

    // Author offers a paid, auto-approve "gold" tier at rank 1.
    create_tier(&router, state.clone(), &author_kp, "gold", 1, true)
        .await
        .expect("create gold tier");

    // The subscriber follows (provisions `followers`) then subscribes to gold;
    // both enqueue for the author's client.
    let follow = subscribe(&router, state.clone(), subscriber, author, FOLLOWERS_TIER)
        .await
        .expect("follow ok (enqueues)");
    let gold = subscribe(&router, state.clone(), subscriber, author, "gold")
        .await
        .expect("subscribe gold ok (enqueues)");
    let (
        SubscribeReply::Queued {
            request_id: follow_id,
        },
        SubscribeReply::Queued {
            request_id: gold_id,
        },
    ) = (follow, gold)
    else {
        panic!("both subscribes enqueue for the author's client");
    };

    // The author's client approves both. The followers tier was provisioned
    // with no blob, so its approval lands the tier's first one (version 1);
    // gold carries its birth blob, so its first approval is version 2.
    let roster = [subscriber_kp.actor_id()];
    let followed = common::approve_with_mint(
        &router,
        state.clone(),
        &author_kp,
        follow_id,
        FOLLOWERS_TIER,
        &roster,
        &[0x71; 32],
        1_700_000_100_000_000,
    )
    .await
    .expect("approve the follow");
    assert_eq!(followed.key_version, 1, "the followers tier's first blob");
    let paid = common::approve_with_mint(
        &router,
        state.clone(),
        &author_kp,
        gold_id,
        "gold",
        &roster,
        &[0x72; 32],
        1_700_000_100_000_000,
    )
    .await
    .expect("approve gold");
    assert_eq!(paid.key_version, 2, "one past gold's birth blob");
    assert!(
        state
            .db
            .is_subscriber(&author, &subscriber, FOLLOWERS_TIER)
            .await
            .unwrap(),
        "gold subscriber is also a follower"
    );

    let status = status_get(&router, state, subscriber, author).await;
    assert_eq!(
        status.tier.as_deref(),
        Some("gold"),
        "status reports the paid tier, not the rank-0 followers fallback"
    );
}

/// Birth-KeyBlob flow (`ui/feed.md` § Encryption at rest — broadcast tiers):
/// the author's create carries an empty-roster KeyBlob (`encrypted_upload`)
/// → the handler verifies + stores it as version 1 → `key_blob.get` serves
/// it to the AUTHOR (the bearer that minted it — the gated-compose
/// `key_blob_ref` read), while a non-subscriber stranger is still refused.
#[tokio::test]
async fn tiers_create_stores_birth_key_blob_and_author_reads_it() {
    use common::encrypted_keyblob::{make_device_authorization, mint_test_key_blob};
    use fauna_core::data::{Capability, Timestamp};
    use fauna_protocol::subscriptions::{KeyBlobGetReply, KeyBlobGetRequest};

    let (router, state) = router_with_db().await;
    let author_kp = ActorKeypair::generate();
    let author = author_kp.actor_id().0;
    let stranger = [0x5Au8; 32];
    let period_key = [0x42u8; 32];

    let auth =
        make_device_authorization(&author_kp, &author_kp, vec![Capability::ManageSubscribers]);
    let (_blob, upload) = mint_test_key_blob(
        &author_kp,
        &auth,
        "gold",
        Timestamp(1_700_000_000_000_000),
        &[],
        &period_key,
    );

    let req = encode_canonical(&TierCreateRequest {
        name: "gold".to_string(),
        rank: 2,
        description: None,
        price_hint: None,
        payment_url: None,
        auto_approve: false,
        encrypted_upload: upload.clone(),
        unlocks_post: None,
        asking_price: None,
        hidden: false,
        extra: Default::default(),
    })
    .unwrap();
    let reply: TierCreateReply = decode(
        &common::call_raw(
            &router,
            state.clone(),
            "fauna.subscriptions.tiers.create",
            author,
            Bytes::from(req.to_vec()),
        )
        .await
        .expect("tiers.create with birth blob ok"),
    )
    .expect("decode create reply");
    assert!(reply.created);

    let get_req = encode_canonical(&KeyBlobGetRequest {
        author_id: ActorId(author),
        tier_name: "gold".to_string(),
        extra: Default::default(),
    })
    .unwrap();

    // The author reads their own tier's blob: version 1, content-addressed to
    // the minted inner bytes.
    let got: KeyBlobGetReply = decode(
        &common::call_raw(
            &router,
            state.clone(),
            "fauna.subscriptions.key_blob.get",
            author,
            Bytes::from(get_req.to_vec()),
        )
        .await
        .expect("author key_blob.get ok"),
    )
    .expect("decode key_blob reply");
    assert_eq!(got.version, 1, "birth blob is version 1");
    assert_eq!(
        got.blob_hash.as_ref(),
        blake3::hash(&upload.key_blob.bytes).as_bytes(),
        "blob_hash is BLAKE3 of the inner canonical KeyBlob bytes (the \
         gated-compose key_blob_ref)"
    );

    // A stranger (not subscribed, not the author) is still refused.
    let err = common::call_raw(
        &router,
        state,
        "fauna.subscriptions.key_blob.get",
        stranger,
        Bytes::from(get_req.to_vec()),
    )
    .await
    .expect_err("stranger must not read the blob");
    assert_eq!(err.code, "fauna.subscriptions.not_subscribed");
}

/// An author subscribing to their own **first-follow** tier is
/// refused at the wire door — the same rule `FeedManager::is_local_actor`
/// already enforces client-side for a post purchase, now
/// enforced at the one place every app and any third-party client goes
/// through. Exercised via the real `fauna.subscriptions.subscribe` RPC
/// (tier_3: real `AppState` + real `CacheDb`), not just the handler unit.
#[tokio::test]
async fn self_subscribe_is_refused_at_the_wire_door() {
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();

    // Precondition mirrors `first_follow_auto_provisions_tier_and_enqueues`:
    // no followers tier exists yet, so a self-follow reaching the
    // auto-provision would otherwise mint one.
    assert!(
        state
            .db
            .get_subscription_tier(&author, FOLLOWERS_TIER)
            .await
            .unwrap()
            .is_none(),
        "no followers tier should exist before the self-subscribe attempt"
    );

    let err = subscribe(&router, state.clone(), author, author, FOLLOWERS_TIER)
        .await
        .expect_err("an author may not subscribe to their own tier");
    assert_eq!(err.code, "fauna.subscriptions.forbidden");

    assert!(
        state
            .db
            .get_subscription_tier(&author, FOLLOWERS_TIER)
            .await
            .unwrap()
            .is_none(),
        "the refused self-subscribe must not lazily mint the followers tier"
    );
    assert!(
        !state
            .db
            .is_subscriber(&author, &author, FOLLOWERS_TIER)
            .await
            .unwrap(),
        "no subscribers row for the self-follow"
    );
}
