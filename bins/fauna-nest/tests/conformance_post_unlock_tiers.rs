//! Integration round-trips for **per-post pay-to-unlock** — the
//! `unlocks_post` tier designation of `docs/goal/behavior/monetization.md`
//! § Per-post pay-to-unlock (design ratified 2026-07-22, Q9).
//!
//! The ratified shape is a *degenerate single-post tier*: an ordinary
//! subscription tier plus a creator-set, **create-time-immutable**
//! designation naming the post it unlocks. Nothing about the entitlement
//! waist changes — `PaymentEntitlement` still names *(payee, tier)* — which
//! is the whole point of the resolution, and what the last test here pins.
//!
//! Covered:
//! - the designation round-trips `tiers.create` → `tiers.list` (the author's
//!   own read carries it; clients filter their §1 list client-side);
//! - it is **absent** from both generic offer surfaces — the authenticated
//!   `offers.list` and the unauthenticated HTTP twin
//!   `GET /api/v1/subscriptions/tiers/{author_id}` (`monetization.md:128`);
//! - `tiers.update` **refuses** to re-point or to newly attach a designation
//!   (`monetization.md:131` — re-pointing a sold unlock is a rug-pull), while
//!   staying idempotent for the unchanged value and for the `None` merge;
//! - a malformed designation is refused at create (the post id is
//!   `blake3(body)` hex — `posts.rs:54-57`);
//! - a designated tier's paid entitlement rides the **unchanged waist**,
//!   enqueuing the `payment_entitled` request exactly like the ordinary
//!   client-minted tier does (mirrors `conformance_payments.rs::
//!   client_minted_tier_grant_enqueues_payment_entitled_request`).
//!
//! The nest deliberately does **not** check that `unlocks_post` names an
//! existing post: the client's ordering is *mint period key → build the birth
//! KeyBlob → build the gated post body (which carries the tier name) →
//! `post_id = blake3(body)` → `tiers.create` → `posts.create`, so the post
//! does not exist yet at create time. Format validation only. This also
//! matches `monetization.md:131` — no FK, and the tier row outlives the post.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/subscriptions.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::{
    db::CacheDb,
    payment_core::{self, PaymentApplied},
    routes::AppState,
    rpc_router::RpcRouter,
    subscription_handlers,
};
use fauna_payments::{Buyer, PaymentEntitlement};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    subscriptions::{
        OffersListRequest, PostUnlockGetReply, PostUnlockGetRequest, RequestsListReply,
        RequestsListRequest, TierClearFieldReply, TierClearFieldRequest, TierClearableField,
        TierCreateReply, TierCreateRequest, TierUpdateReply, TierUpdateRequest, TiersListReply,
        TiersListRequest,
    },
};

/// A syntactically valid post id: `blake3(body)` rendered as 64 hex chars.
const POST_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const POST_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

mod common;

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    (b.build(), state)
}

/// `tiers.create`, optionally carrying the post-unlock designation.
async fn create_tier(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: &ActorKeypair,
    name: &str,
    rank: u32,
    unlocks_post: Option<&str>,
) -> Result<TierCreateReply, RpcError> {
    let req = TierCreateRequest {
        unlocks_post: unlocks_post.map(str::to_string),
        ..common::tier_create_request(author, name, rank)
    };
    common::create_tier(router, state, author, req).await
}

/// `tiers.update` carrying only the designation field under test.
async fn update_designation(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
    name: &str,
    unlocks_post: Option<&str>,
) -> Result<TierUpdateReply, RpcError> {
    let req = TierUpdateRequest {
        name: name.into(),
        unlocks_post: unlocks_post.map(str::to_string),
        ..Default::default()
    };
    let bytes = encode_canonical(&req).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.tiers.update",
        author,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode update reply"))
}

async fn own_tiers(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
) -> Result<TiersListReply, RpcError> {
    let bytes = encode_canonical(&TiersListRequest {}).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.tiers.list",
        author,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode tiers.list reply"))
}

async fn offers(
    router: &RpcRouter,
    state: Arc<AppState>,
    caller: [u8; 32],
    author: [u8; 32],
) -> Result<TiersListReply, RpcError> {
    let bytes = encode_canonical(&OffersListRequest {
        author_id: ActorId(author),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.offers.list",
        caller,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode offers.list reply"))
}

/// Seed a user row and keep the keypair — `tiers.create` carries a birth
/// `KeyBlob` the author must sign.
async fn seed_author(state: &Arc<AppState>, handle: &str) -> ActorKeypair {
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", handle)
        .await
        .unwrap();
    kp
}

/// Write a post gated to `tier` through the production `put_post` path, so
/// `content_meta.gated_tier` is projected by `extract_post_metadata` rather
/// than fabricated. Returns the post's real content-addressed id (hex).
///
/// The price read requires **both** directions of the sold-post binding — the
/// post gated to the tier *and* the tier designating the post
/// (`monetization.md` § Per-post pay-to-unlock → *The designation is
/// corroboration*) — so a bare constant post id can no longer stand in for a
/// sold post: it would make every read empty and every assertion below vacuous.
async fn gate_post_to(state: &Arc<AppState>, author: [u8; 32], tier: &str) -> String {
    use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
    use fauna_core::subscription::types::{GatedInfo, KeyAccess};

    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let post = Post {
        author: ActorId(author),
        created_at: Timestamp(1_700_000_000_000_000 + n),
        body: PostBody::Text {
            content: format!("public teaser {n}"),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: Some(GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([9u8; 32]),
            key_access: KeyAccess::Broadcast {
                key_blob_ref: ContentHash::from_digest_raw([8u8; 32]),
            },
            tier: tier.to_string(),
            tier_rank: 9,
            seal_id: ContentHash::from_digest_raw([7u8; 32]),
            attachment_refs: vec![],
        }),
        content_warning: None,
        origin: None,
    };
    let cid = fauna_core::encoding::compute_post_id(&post).expect("compute post id");
    let digest: [u8; 32] = cid.as_bytes()[4..].try_into().expect("32-byte digest");
    let bytes = fauna_core::encoding::canonical_encode(&post).expect("encode post");
    state.db.put_post(&digest, &bytes, None).await.unwrap();
    hex::encode(digest)
}

fn names(reply: &TiersListReply) -> Vec<&str> {
    reply.tiers.iter().map(|t| t.name.as_str()).collect()
}

// ── The designation round-trip ─────────────────────────────────

#[tokio::test]
async fn the_designation_round_trips_through_create_and_the_authors_own_read() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;

    create_tier(&router, state.clone(), &author_kp, "gold", 1, None)
        .await
        .expect("ordinary tier creates");
    create_tier(
        &router,
        state.clone(),
        &author_kp,
        "unlock-a",
        9,
        Some(POST_A),
    )
    .await
    .expect("designated tier creates");

    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");

    // The author's OWN read carries both — they need their unlock tiers (to
    // gate the post and to audit sales); the §1 management-list exclusion of
    // monetization.md:128 is a client-side filter over this field.
    assert_eq!(names(&list), vec!["gold", "unlock-a"]);
    let gold = &list.tiers[0];
    let unlock = &list.tiers[1];
    assert_eq!(
        gold.unlocks_post, None,
        "an ordinary tier carries no designation"
    );
    assert_eq!(
        unlock.unlocks_post.as_deref(),
        Some(POST_A),
        "the designation must survive the create → list round-trip"
    );
}

#[tokio::test]
async fn tiers_create_refuses_a_malformed_designation() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;

    for bad in ["", "not-hex", "abc", &"11".repeat(31), &"11".repeat(33)] {
        let err = create_tier(
            &router,
            state.clone(),
            &author_kp,
            "unlock-bad",
            9,
            Some(bad),
        )
        .await
        .expect_err("a malformed post id must be refused");
        assert_eq!(
            err.code, "fauna.subscriptions.malformed_upload",
            "unexpected error for designation {bad:?}"
        );
    }

    // …and nothing was persisted by the refused creates.
    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert!(
        list.tiers.is_empty(),
        "a refused create must persist nothing"
    );
}

// ── The generic-surface exclusions (monetization.md:128) ───────

#[tokio::test]
async fn a_designated_tier_is_absent_from_another_actors_offers_view() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    let browser = seed_author(&state, "browser").await.actor_id().0;

    create_tier(&router, state.clone(), &author_kp, "gold", 1, None)
        .await
        .expect("ordinary tier creates");
    create_tier(
        &router,
        state.clone(),
        &author_kp,
        "unlock-a",
        9,
        Some(POST_A),
    )
    .await
    .expect("designated tier creates");

    let view = offers(&router, state.clone(), browser, author)
        .await
        .expect("offers.list ok");

    assert_eq!(
        names(&view),
        vec!["gold"],
        "a designated tier must never appear in a generic offers browse — \
         the unlock affordance renders on the post"
    );
}

#[tokio::test]
async fn a_designated_tier_is_absent_from_the_public_http_tier_read() {
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;

    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;

    create_tier(&router, state.clone(), &author_kp, "gold", 1, None)
        .await
        .expect("ordinary tier creates");
    create_tier(
        &router,
        state.clone(),
        &author_kp,
        "unlock-a",
        9,
        Some(POST_A),
    )
    .await
    .expect("designated tier creates");

    // The unauthenticated twin of the same generic offers surface: filtering
    // one and not the other would leave the enumeration hole open.
    let response = fauna_nest::subscription_routes::list_tiers(
        State(state.clone()),
        Path(hex::encode(author)),
    )
    .await
    .into_response();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("read body");
    let items: Vec<serde_json::Value> = serde_json::from_slice(&body).expect("json array");

    let listed: Vec<&str> = items
        .iter()
        .map(|i| i["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        listed,
        vec!["gold"],
        "the public tier read is the unauthenticated generic offers surface \
         and must exclude designated tiers too"
    );
}

// ── Create-time immutability (monetization.md:131) ─────────────

#[tokio::test]
async fn tiers_update_refuses_to_repoint_a_designation() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    create_tier(
        &router,
        state.clone(),
        &author_kp,
        "unlock-a",
        9,
        Some(POST_A),
    )
    .await
    .expect("designated tier creates");

    let err = update_designation(&router, state.clone(), author, "unlock-a", Some(POST_B))
        .await
        .expect_err("re-pointing a sold unlock is a rug-pull and must be refused");
    assert_eq!(err.code, "fauna.subscriptions.designation_immutable");

    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert_eq!(
        list.tiers[0].unlocks_post.as_deref(),
        Some(POST_A),
        "the refused update must not have moved the designation"
    );
}

#[tokio::test]
async fn tiers_update_refuses_to_designate_a_plain_tier_after_the_fact() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    create_tier(&router, state.clone(), &author_kp, "gold", 1, None)
        .await
        .expect("ordinary tier creates");

    // The designation is CREATE-time: attaching one later is the same
    // rug-pull class (subscribers bought a tier that gated nothing extra).
    let err = update_designation(&router, state.clone(), author, "gold", Some(POST_A))
        .await
        .expect_err("attaching a designation after create must be refused");
    assert_eq!(err.code, "fauna.subscriptions.designation_immutable");

    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert_eq!(list.tiers[0].unlocks_post, None);
}

#[tokio::test]
async fn tiers_update_is_idempotent_for_the_unchanged_designation() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    create_tier(
        &router,
        state.clone(),
        &author_kp,
        "unlock-a",
        9,
        Some(POST_A),
    )
    .await
    .expect("designated tier creates");

    // Re-sending the SAME value is a no-op accept — an auto-retrying or
    // replaying client must not be turned into an error.
    update_designation(&router, state.clone(), author, "unlock-a", Some(POST_A))
        .await
        .expect("an unchanged designation is accepted");

    // …and the `None` merge keeps the current value, exactly like every
    // other field on this handler's `or_else(current)` shape.
    update_designation(&router, state.clone(), author, "unlock-a", None)
        .await
        .expect("None keeps the current designation");

    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert_eq!(list.tiers[0].unlocks_post.as_deref(), Some(POST_A));
}

// ── tiers.clear_field ────────────────────

/// `tiers.clear_field` for the field under test.
async fn clear_field(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
    name: &str,
    field: TierClearableField,
) -> Result<TierClearFieldReply, RpcError> {
    let req = TierClearFieldRequest {
        name: name.into(),
        field,
        ..Default::default()
    };
    let bytes = encode_canonical(&req).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.tiers.clear_field",
        author,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode clear_field reply"))
}

/// The real RPC round trip this row exists for: `tiers.update` can SET
/// `description`/`price_hint`, but has no way to CLEAR either back to
/// unset (its `None` means "keep current"). `tiers.clear_field` is the
/// dedicated verb — real DAG-CBOR wire encode/decode through the real
/// registered router, not a direct handler call.
#[tokio::test]
async fn clear_field_wipes_description_and_price_hint_independently() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    create_tier(&router, state.clone(), &author_kp, "gold", 1, None)
        .await
        .expect("tier creates");

    let req = TierUpdateRequest {
        name: "gold".into(),
        description: Some("a real description".into()),
        price_hint: Some("$5/mo".into()),
        ..Default::default()
    };
    let bytes = encode_canonical(&req).unwrap();
    common::call_raw(
        &router,
        state.clone(),
        "fauna.subscriptions.tiers.update",
        author,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("tiers.update sets both fields");

    clear_field(
        &router,
        state.clone(),
        author,
        "gold",
        TierClearableField::Description,
    )
    .await
    .expect("clear description ok");

    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert_eq!(
        list.tiers[0].description, None,
        "description must be cleared"
    );
    assert_eq!(
        list.tiers[0].price_hint.as_deref(),
        Some("$5/mo"),
        "price_hint must survive a description clear"
    );

    clear_field(
        &router,
        state.clone(),
        author,
        "gold",
        TierClearableField::PriceHint,
    )
    .await
    .expect("clear price_hint ok");

    let list = own_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert_eq!(list.tiers[0].price_hint, None, "price_hint must be cleared");
}

#[tokio::test]
async fn clear_field_returns_tier_not_found_for_an_unknown_tier() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state, "author").await.actor_id().0;

    let err = clear_field(
        &router,
        state.clone(),
        author,
        "ghost",
        TierClearableField::AskingPrice,
    )
    .await
    .expect_err("unknown tier must fail");
    assert_eq!(err.code, "fauna.subscriptions.tier_not_found");
}

// ── The unchanged waist ────────────────────────────────────────

#[tokio::test]
async fn a_designated_tiers_paid_entitlement_rides_the_unchanged_waist() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    let buyer = seed_author(&state, "buyer").await.actor_id().0;
    create_tier(
        &router,
        state.clone(),
        &author_kp,
        "unlock-a",
        9,
        Some(POST_A),
    )
    .await
    .expect("designated tier creates");

    // A purchase is an ORDINARY entitlement naming (payee, tier) — the
    // engine must not learn that this tier sells a post.
    let entitlement = PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(author),
        buyer: Buyer::Actor(ActorId(buyer)),
        tier: "unlock-a".into(),
        valid_until_secs: None,
        external_ref: "pay_unlock_1".into(),
    };
    let applied = payment_core::apply_payment(&state, &entitlement)
        .await
        .expect("apply ok");
    let queued = match applied {
        PaymentApplied::Granted { queued } => queued,
        other => panic!("a bound buyer must grant, got {other:?}"),
    };
    assert!(
        queued,
        "a client-minted tier enqueues for the author's client — a designated \
         tier is an ordinary client-minted tier"
    );

    // The author's requests.list carries the same payment marker the drain
    // pump keys on; nothing here is unlock-specific.
    let bytes = encode_canonical(&RequestsListRequest {}).unwrap();
    let reply = common::call_raw(
        &router,
        state.clone(),
        "fauna.subscriptions.requests.list",
        author,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("requests.list ok");
    let list: RequestsListReply = decode(&reply).expect("decode");
    assert_eq!(list.requests.len(), 1);
    assert!(list.requests[0].payment_entitled);
    assert_eq!(list.requests[0].tier_name, "unlock-a");
}

// ── The buyer's price read (`monetization.md` § Per-post pay-to-unlock —
//    *the buyer's price read is post-addressed*, ruled 2026-07-29) ──────────

/// `tiers.create` carrying the public purchase fields the price read serves.
#[allow(clippy::too_many_arguments)]
async fn create_priced_tier(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: &ActorKeypair,
    name: &str,
    rank: u32,
    unlocks_post: Option<&str>,
    price_hint: Option<&str>,
    payment_url: Option<&str>,
) -> TierCreateReply {
    let req = TierCreateRequest {
        name: name.into(),
        rank,
        auto_approve: false,
        unlocks_post: unlocks_post.map(str::to_string),
        price_hint: price_hint.map(str::to_string),
        payment_url: payment_url.map(str::to_string),
        description: None,
        encrypted_upload: common::encrypted_keyblob::birth_upload(author, name),
        asking_price: None,
        hidden: false,
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.tiers.create",
        author.actor_id().0,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("create priced tier");
    decode(&reply).expect("decode create reply")
}

async fn price_read(
    router: &RpcRouter,
    state: Arc<AppState>,
    caller: [u8; 32],
    author: [u8; 32],
    post_id: &str,
) -> Result<PostUnlockGetReply, RpcError> {
    let bytes = encode_canonical(&PostUnlockGetRequest {
        author_id: ActorId(author),
        post_id: post_id.to_string(),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.post_unlock.get",
        caller,
        Bytes::from(bytes.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode post_unlock.get reply"))
}

#[tokio::test]
async fn the_price_read_answers_the_designated_tiers_public_purchase_fields() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    let buyer = seed_author(&state, "buyer").await.actor_id().0;
    create_tier(&router, state.clone(), &author_kp, "regular", 1, None)
        .await
        .unwrap();
    // A real sold post: gated to the tier that designates it.
    let sold = gate_post_to(&state, author, "post-unlock-aaaa").await;
    create_priced_tier(
        &router,
        state.clone(),
        &author_kp,
        "post-unlock-aaaa",
        9,
        Some(&sold),
        Some("$3"),
        Some("https://pay.example/aaaa"),
    )
    .await;

    let reply = price_read(&router, state.clone(), buyer, author, &sold)
        .await
        .expect("a prospective buyer holding the post id gets a read");
    let offer = reply.offer.expect("the designated tier is offered");
    assert_eq!(offer.tier_name, "post-unlock-aaaa");
    assert_eq!(offer.price_hint.as_deref(), Some("$3"));
    assert_eq!(
        offer.payment_url.as_deref(),
        Some("https://pay.example/aaaa")
    );
}

/// The anti-enumeration property: an undesignated id, a foreign author's
/// designation, and an unknown author are all the SAME empty reply — the read
/// confirms nothing except what possession of a designated post id already
/// proves.
#[tokio::test]
async fn the_price_read_reveals_nothing_for_an_undesignated_or_foreign_id() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author(&state, "author").await;
    let author = author_kp.actor_id().0;
    let other_kp = seed_author(&state, "other").await;
    let other = other_kp.actor_id().0;
    let buyer = seed_author(&state, "buyer").await.actor_id().0;
    // `author` sells post A; `other` sells post B. Both are REAL sold posts,
    // so the cross-author read below is empty because the designation belongs
    // to somebody else — not merely because no post exists.
    let post_a = gate_post_to(&state, author, "post-unlock-aaaa").await;
    let post_b = gate_post_to(&state, other, "post-unlock-bbbb").await;
    create_priced_tier(
        &router,
        state.clone(),
        &author_kp,
        "post-unlock-aaaa",
        9,
        Some(&post_a),
        Some("$3"),
        None,
    )
    .await;
    create_priced_tier(
        &router,
        state.clone(),
        &other_kp,
        "post-unlock-bbbb",
        9,
        Some(&post_b),
        Some("$5"),
        None,
    )
    .await;

    // A genuinely-sold post — but sold by ANOTHER author.
    let cross = price_read(&router, state.clone(), buyer, author, &post_b)
        .await
        .unwrap();
    assert!(
        cross.offer.is_none(),
        "another author's designation must not answer under this author"
    );
    // Sanity that the read is not simply broken: `other` does answer for it.
    assert!(
        price_read(&router, state.clone(), buyer, other, &post_b)
            .await
            .unwrap()
            .offer
            .is_some(),
        "the same id answers under the author who actually sells it"
    );

    // A never-designated (well-formed) id.
    let phantom = "3333333333333333333333333333333333333333333333333333333333333333";
    let none = price_read(&router, state.clone(), buyer, author, phantom)
        .await
        .unwrap();
    assert!(none.offer.is_none());

    // An author with no tiers at all — indistinguishable from the above.
    let stranger_author = ActorKeypair::generate().actor_id().0;
    let unknown = price_read(&router, state.clone(), buyer, stranger_author, phantom)
        .await
        .unwrap();
    assert!(unknown.offer.is_none());

    // A REGULAR (undesignated) tier is never surfaced through this read, no
    // matter what id is asked — the read is not a fourth generic tier surface.
    create_tier(&router, state.clone(), &author_kp, "regular", 1, None)
        .await
        .unwrap();
    let still_none = price_read(&router, state.clone(), buyer, author, phantom)
        .await
        .unwrap();
    assert!(still_none.offer.is_none());
}

#[tokio::test]
async fn the_price_read_refuses_a_malformed_post_id() {
    let (router, state) = router_with_db().await;
    let author = seed_author(&state, "author").await.actor_id().0;
    let buyer = seed_author(&state, "buyer").await.actor_id().0;

    let err = price_read(&router, state.clone(), buyer, author, "not-a-post-id")
        .await
        .unwrap_err();
    assert_eq!(err.code, "fauna.subscriptions.malformed_upload");
}
