//! Integration pins for the **per-post pay-to-unlock rank DELIVERY** — the
//! grant-time fan-out enqueue of `docs/goal/behavior/monetization.md:126`
//! (the at-or-below-rank cascade proved unreachable for client-minted
//! tiers) and its
//! build contract at § Implementation status (2d).
//!
//! The ruled mechanism: wherever a rank-R entitlement to an **undesignated**
//! tier lands — request approve, or the payment-grant path — the nest
//! additionally enqueues a `payment_entitled` subscribe request for every
//! `unlocks_post`-designated tier with rank ≤ R, and the author's client
//! drains those through the existing queue, minting the KeyBlob wraps. A
//! designated tier is always client-minted (`tiers.create` writes no period
//! key), so this enqueue is the *only* delivery mechanism, not an
//! optimization.
//!
//! The closing pins assert **readability** — the subscriber ends up holding a
//! KeyBlob entry they can decrypt to the designated tier's period key — never
//! the rank integer (a rank-integer assertion passes against the pre-fix code
//! and proves nothing: that is exactly how the defect survived).
//!
//! Covered:
//! - grant-time fan-out at `requests.approve` → the subscriber becomes
//!   *readable* on the rank-1 unlock tier (the mandated closing pin);
//! - creation-time reconcile: a designated tier created *after* the
//!   subscriber's grant enqueues for the existing eligible subscribers
//!   (what makes `monetization.md:126` true for pre-sale subscribers);
//! - the payment-grant entry point enqueues the fan-out rows at payment
//!   time (both entry points, obligation (i));
//! - a buyer of one unlock tier is **not** fanned into another rank-1
//!   unlock tier (the fan-out fires only for undesignated grants — one
//!   cheap post must never unlock every sold post);
//! - a pay-per-view tier (rank above the author's highest regular tier)
//!   is not delivered to a lower-rank subscriber;
//! - the free rank-0 follower is not fanned into any rank-≥1 unlock tier;
//! - a lapsed (expired-window) subscriber is not enqueued by the
//!   creation-time reconcile;
//! - and the `status.get` pick the fan-out arms:
//!   one pin per unlock-tier class, each asserting the reported tier's real
//!   `expires_at` and each biting a different one-line mutation of the
//!   handler — see the section comment above them;
//! - and a hidden tier is never a grant-time cascade target — arm A of the
//!   security review finding; the boot-reconcile twin of this same
//!   exclusion is pinned in `db/migrations.rs`, since it needs a real
//!   `Connection`, not the RPC router this file drives.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb`, real KeyBlob
//! crypto over the wire shapes — no mocks).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::Timestamp;
use fauna_core::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::FOLLOWERS_TIER;
use fauna_core::subscription::crypto::decrypt_key_blob_entry_for;
use fauna_core::subscription::types::KeyBlob;
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
        PendingRequest, RequestsListReply, RequestsListRequest, StatusGetReply, StatusGetRequest,
        SubscribeReply, SubscribeRequest, TierCreateReply, TierCreateRequest,
    },
};

/// Syntactically valid post ids: `blake3(body)` rendered as 64 hex chars.
const POST_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const POST_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    (b.build(), state)
}

/// Seed a user row and keep the keypair — the readability pins need the
/// subscriber's secret to actually decrypt their KeyBlob entry.
async fn seed_actor(state: &Arc<AppState>, handle: &str) -> ActorKeypair {
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", handle)
        .await
        .unwrap();
    kp
}

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

/// `fauna.subscriptions.subscribe` as the subscriber.
async fn subscribe(
    router: &RpcRouter,
    state: Arc<AppState>,
    subscriber: [u8; 32],
    author: [u8; 32],
    tier: &str,
) -> SubscribeReply {
    let req = SubscribeRequest {
        author_id: ActorId(author),
        tier: tier.into(),
        mlkem_encaps_key: None,
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.subscribe",
        subscriber,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("subscribe ok");
    decode(&reply).expect("decode subscribe reply")
}

/// The author's pending-request view — where the fan-out rows must appear.
async fn pending(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
) -> Vec<PendingRequest> {
    let bytes = encode_canonical(&RequestsListRequest {}).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.requests.list",
        author,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("requests.list ok");
    let list: RequestsListReply = decode(&reply).expect("decode requests.list");
    list.requests
}

fn pending_for<'a>(rows: &'a [PendingRequest], tier: &str) -> Option<&'a PendingRequest> {
    rows.iter().find(|r| r.tier_name == tier)
}

/// `pending_for` narrowed to one subscriber — required wherever a second actor
/// legitimately holds a row on the same tier (the reserved `followers` tier is
/// the standing case: every follow enqueues one, because
/// `ensure_followers_tier` writes no period key, so a tier-only lookup would
/// return the wrong actor's request and hide the row under test).
fn pending_for_sub<'a>(
    rows: &'a [PendingRequest],
    tier: &str,
    subscriber: ActorId,
) -> Option<&'a PendingRequest> {
    rows.iter()
        .find(|r| r.tier_name == tier && r.subscriber_id == subscriber)
}

/// `fauna.subscriptions.status.get` as the subscriber — the surface the
/// pins below interrogate.
async fn status_get(
    router: &RpcRouter,
    state: Arc<AppState>,
    subscriber: [u8; 32],
    author: [u8; 32],
) -> StatusGetReply {
    let bytes = encode_canonical(&StatusGetRequest {
        author_id: ActorId(author),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.status.get",
        subscriber,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("status.get ok");
    decode(&reply).expect("decode status")
}

/// An absolute epoch-second window a year out. Absolute, never a duration
/// measured against the run: no machine load can lapse it, and the pins that
/// read it back compare an exact value (testing.md § point 14 — assert
/// latency-independent state, with ceilings far above any non-pathological
/// delay).
fn far_future_window_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64 + 86_400 * 365
}

/// The production route to a `subscribers` row carrying a **non-NULL**
/// `valid_until`: a verified payment carrying a window enqueues a
/// `payment_entitled` request, and `requests.approve` stamps the window on
/// the requested tier's row after the grant lands
/// (`subscription_handlers.rs` § the payment-window read, monetization.md
/// § Pillar 3). Returns nothing — the pins read the window back through
/// `status.get`, never through the DB.
#[allow(clippy::too_many_arguments)] // one call site per pin; a struct would just relocate the list
async fn pay_and_approve(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author_kp: &ActorKeypair,
    buyer: &ActorKeypair,
    tier: &str,
    valid_until_secs: Option<u64>,
    period_key: &[u8; 32],
    rotated_at: u64,
    external_ref: &str,
) {
    let author_id = author_kp.actor_id().0;
    let entitlement = PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(author_id),
        buyer: Buyer::Actor(buyer.actor_id()),
        tier: tier.into(),
        valid_until_secs,
        external_ref: external_ref.into(),
    };
    payment_core::apply_payment(state, &entitlement)
        .await
        .expect("apply ok");
    let rows = pending(router, state.clone(), author_id).await;
    let row = pending_for(&rows, tier).expect("the paid request is enqueued");
    common::approve_with_mint(
        router,
        state.clone(),
        author_kp,
        row.request_id,
        tier,
        &[buyer.actor_id()],
        period_key,
        rotated_at,
    )
    .await
    .expect("paid approve ok");
}

/// The current KeyBlob's decoded entries for a tier (empty if no blob).
async fn blob_entries(
    state: &Arc<AppState>,
    author: &[u8; 32],
    tier: &str,
) -> Vec<fauna_core::subscription::types::KeyBlobEntry> {
    let Some((_v, _h, stored)) = state.db.get_current_key_blob(author, tier).await.unwrap() else {
        return Vec::new();
    };
    let wire: EmbedAsBytes = canonical_decode(&stored).expect("decode blob wire");
    let blob: KeyBlob = decode_signed_bytes(&wire.bytes).expect("decode blob");
    blob.entries
}

/// The mandated readability assertion: the subscriber holds an entry in the
/// tier's current KeyBlob that decrypts to the tier's period key.
async fn assert_readable(
    state: &Arc<AppState>,
    author: &[u8; 32],
    tier: &str,
    subscriber: &ActorKeypair,
    period_key: &[u8; 32],
) {
    let entries = blob_entries(state, author, tier).await;
    let entry = entries
        .iter()
        .find(|e| e.subscriber == subscriber.actor_id())
        .unwrap_or_else(|| panic!("subscriber has no KeyBlob entry on {tier}"));
    let opened = decrypt_key_blob_entry_for(subscriber, entry).expect("entry decrypts");
    assert_eq!(
        &opened, period_key,
        "the entry must open to {tier}'s period key — readability, not roster cosmetics"
    );
}

// ── The mandated closing pin: grant-time fan-out → readability ──

/// A subscriber granted a rank-2 regular tier lands in the rank-1 unlock
/// tier's KeyBlob roster and can decrypt its period key. This is the
/// verify-back check 1 shape: readability, never the rank integer.
#[tokio::test]
async fn a_rank2_grant_makes_the_rank1_unlock_tier_readable() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("a client-minted tier must enqueue");
    };

    let gold_key = [0x11u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &gold_key,
        1_000,
    )
    .await
    .expect("gold approve ok");

    // The grant landed → the nest must have enqueued the author-client mint
    // for the included rank-1 unlock tier, payment-marked so the drain pump
    // approves it without creator judgment.
    let rows = pending(&router, state.clone(), author_id).await;
    let fanned = pending_for(&rows, "post-unlock-a")
        .expect("the rank-1 unlock tier's mint request is enqueued at grant time");
    assert!(
        fanned.payment_entitled,
        "the fan-out row must ride the payment_entitled drain lane"
    );
    assert_eq!(fanned.subscriber_id, sub.actor_id());

    // The author's drain approves it (hand-driven here over the real arm).
    let unlock_key = [0x22u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        fanned.request_id,
        "post-unlock-a",
        &[sub.actor_id()],
        &unlock_key,
        2_000,
    )
    .await
    .expect("unlock approve ok");

    assert_readable(&state, &author_id, "post-unlock-a", &sub, &unlock_key).await;
}

// ── Arm A: a hidden tier is never a cascade target ──

/// A grant on an undesignated rank-2 tier fans out to every tier at or below
/// its rank (rule 1) — **except** a hidden one, which ruling 4 makes not
/// offered and not subscribable at every door, this one included. The
/// granted tier is excluded from its own fan-out (rule 2), so the positive
/// control here is a SEPARATE ordinary tier at the hidden tier's own rank:
/// without it, a fixture asserting only "the hidden tier is absent" would
/// pass identically whether the fix works or the cascade never fires at all.
#[tokio::test]
async fn a_hidden_tier_is_never_a_grant_time_cascade_target() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    // The positive control: an ordinary undesignated tier at rank 1.
    create_tier(&router, state.clone(), &author, "silver", 1, None)
        .await
        .expect("silver creates");

    // A hidden tier at the SAME rank — `create_tier` has no `hidden`
    // parameter (29 other call sites in this file shouldn't pay for one), so
    // this issues the raw RPC directly, exactly as `create_tier` does under
    // the hood.
    let hidden_req = TierCreateRequest {
        name: "backstage".into(),
        rank: 1,
        auto_approve: false,
        hidden: true,
        description: None,
        price_hint: None,
        payment_url: None,
        encrypted_upload: common::encrypted_keyblob::birth_upload(&author, "backstage"),
        unlocks_post: None,
        asking_price: None,
        extra: Default::default(),
    };
    let bytes = encode_canonical(&hidden_req).unwrap();
    common::call_raw(
        &router,
        state.clone(),
        "fauna.subscriptions.tiers.create",
        author_id,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("hidden tier creates");

    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("a client-minted tier must enqueue");
    };
    let gold_key = [0x44u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &gold_key,
        1_000,
    )
    .await
    .expect("gold approve ok");

    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for(&rows, "silver").is_some(),
        "the positive control must be fanned in — proves the grant genuinely \
         cascades to rank 1, which is what makes the negative assertion below meaningful"
    );
    assert!(
        pending_for(&rows, "backstage").is_none(),
        "a hidden tier must never be a cascade target — ruling 4, not offered and not subscribable"
    );
}

// ── Creation-time reconcile: the sale enrolls existing subscribers ──

/// `monetization.md:126` promises the unlock tier to "every paid
/// subscription", not only future ones: a designated tier created AFTER the
/// subscriber's grant must enqueue for them too.
#[tokio::test]
async fn a_sale_after_the_grant_enqueues_for_existing_subscribers() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");
    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("queued");
    };
    let gold_key = [0x11u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &gold_key,
        1_000,
    )
    .await
    .expect("gold approve ok");

    // The sale happens later — existing rank-2 subscriber must be enqueued.
    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");

    let rows = pending(&router, state.clone(), author_id).await;
    let fanned = pending_for(&rows, "post-unlock-a")
        .expect("creating a designated tier reconciles existing eligible subscribers");
    assert!(fanned.payment_entitled);
    assert_eq!(fanned.subscriber_id, sub.actor_id());

    let unlock_key = [0x22u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        fanned.request_id,
        "post-unlock-a",
        &[sub.actor_id()],
        &unlock_key,
        2_000,
    )
    .await
    .expect("unlock approve ok");
    assert_readable(&state, &author_id, "post-unlock-a", &sub, &unlock_key).await;
}

// ── Both entry points: the payment-grant path enqueues at payment time ──

/// A verified payment for a rank-2 client-minted tier enqueues BOTH the paid
/// tier's own request and the included unlock tier's fan-out row in the same
/// grant — one drain pass delivers everything (obligation (i), second entry
/// point).
#[tokio::test]
async fn a_payment_grant_enqueues_the_fanout_with_the_paid_request() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let buyer = seed_actor(&state, "buyer").await;
    let author_id = author.actor_id().0;

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    let entitlement = PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(author_id),
        buyer: Buyer::Actor(buyer.actor_id()),
        tier: "gold".into(),
        valid_until_secs: None,
        external_ref: "pay_gold_1".into(),
    };
    let applied = payment_core::apply_payment(&state, &entitlement)
        .await
        .expect("apply ok");
    assert!(matches!(applied, PaymentApplied::Granted { queued: true }));

    let rows = pending(&router, state.clone(), author_id).await;
    let paid = pending_for(&rows, "gold").expect("the paid tier's request is enqueued");
    assert!(paid.payment_entitled);
    let fanned = pending_for(&rows, "post-unlock-a")
        .expect("the payment-grant entry point enqueues the fan-out at payment time");
    assert!(fanned.payment_entitled);
    assert_eq!(fanned.subscriber_id, buyer.actor_id());
}

// ── Negative pins ──────────────────────────────────────────────

/// Buying ONE rank-1 unlock tier must not fan the buyer into another rank-1
/// unlock tier: the fan-out fires only when the granted tier is
/// UNDESIGNATED. (`monetization.md:126` derives the cascade from "included
/// in every paid *subscription*" — one cheap post must never unlock every
/// sold post.)
#[tokio::test]
async fn buying_one_unlock_tier_does_not_unlock_another() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let buyer = seed_actor(&state, "buyer").await;
    let author_id = author.actor_id().0;

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock a creates");
    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-b",
        1,
        Some(POST_B),
    )
    .await
    .expect("unlock b creates");

    // The buyer pays for unlock-a (the ordinary waist), and the author's
    // drain approves it.
    let entitlement = PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(author_id),
        buyer: Buyer::Actor(buyer.actor_id()),
        tier: "post-unlock-a".into(),
        valid_until_secs: None,
        external_ref: "pay_unlock_a".into(),
    };
    payment_core::apply_payment(&state, &entitlement)
        .await
        .expect("apply ok");
    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for(&rows, "post-unlock-b").is_none(),
        "a designated-tier payment must not fan out to sibling unlock tiers"
    );

    let req = pending_for(&rows, "post-unlock-a").expect("the purchase itself is enqueued");
    let key_a = [0x33u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        req.request_id,
        "post-unlock-a",
        &[buyer.actor_id()],
        &key_a,
        1_000,
    )
    .await
    .expect("unlock-a approve ok");

    // Approving the designated tier's own grant must not enqueue siblings
    // either, and unlock-b's roster stays empty of the buyer.
    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for(&rows, "post-unlock-b").is_none(),
        "approving a designated tier must not fan out to sibling unlock tiers"
    );
    assert!(
        blob_entries(&state, &author_id, "post-unlock-b")
            .await
            .iter()
            .all(|e| e.subscriber != buyer.actor_id()),
        "the buyer of post A must not become readable on post B"
    );
}

/// A pay-per-view unlock tier (rank above the author's highest regular
/// tier) is never included in a lower-rank subscriber's grant.
#[tokio::test]
async fn pay_per_view_is_not_delivered_to_a_lower_rank_subscriber() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");
    // Pay-per-view: rank 3 = highest regular (2) + 1.
    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-ppv",
        3,
        Some(POST_A),
    )
    .await
    .expect("ppv tier creates");

    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("queued");
    };
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &[0x11u8; 32],
        1_000,
    )
    .await
    .expect("gold approve ok");

    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for(&rows, "post-unlock-ppv").is_none(),
        "rank 3 > granted rank 2: pay-per-view stays paid"
    );
}

/// The free rank-0 follow never includes a rank-≥1 unlock tier — designated
/// tiers are only ever inside PAID subscriptions.
#[tokio::test]
async fn a_free_follower_is_not_fanned_into_an_unlock_tier() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let follower = seed_actor(&state, "follower").await;
    let author_id = author.actor_id().0;

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");

    // Follow = subscribe to the reserved free tier (auto-provisioned rank 0,
    // client-minted ⇒ enqueues).
    let SubscribeReply::Queued { request_id } = subscribe(
        &router,
        state.clone(),
        follower.actor_id().0,
        author_id,
        FOLLOWERS_TIER,
    )
    .await
    else {
        panic!("a client-minted followers tier enqueues");
    };
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        FOLLOWERS_TIER,
        &[follower.actor_id()],
        &[0x44u8; 32],
        1_000,
    )
    .await
    .expect("follow approve ok");

    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for(&rows, "post-unlock-a").is_none(),
        "a rank-0 follow must not deliver any rank-1 unlock tier"
    );
}

// ── the `status.get` pick, one pin per unlock-tier class ──
//
// Review finding has TWO independent halves and the pick
// must survive both, so each of the three pins below bites a different
// one-line mutation of `status_get_handler`:
//
//   - the DIRECTION half (pick the *highest* paid rank, not the lowest):
//     bitten by `..._not_the_cheapest_tier_the_subscriber_also_holds`;
//   - the EXCLUSION half (`unlocks_post`-designated tiers never win the
//     pick): bitten by `..._when_a_pay_per_view_tier_outranks_it`;
//   - both at once (the pre-fix handler): bitten by
//     `..._not_the_fanned_unlock_tier`.
//
// Neither half substitutes for the other, which is the trap the finding
// names: a direction-only fix makes pay-per-view tiers (minted at `max+1`)
// win, and an exclusion-only fix leaves the cheapest-tier pick masking a
// real paid expiry. Every pin therefore asserts the reported tier's
// `expires_at` as well as its name — a name-only assert passes against a
// handler that reports the right tier with the wrong (or no) window, which
// is precisely the user-visible failure: `offer_status` is exact string
// equality (`libs/fauna-core/src/format.rs`), so a wrong pick renders the
// subscriber's live paid tier as "Not subscribed".

/// **Both halves wrong (the pre-fix handler).** `status.get` reports the PAID
/// tier with its real window, not the rank-1 unlock tier fanned in beneath
/// it. The pre-fix pick read "lowest rank = top tier" — inverted against the
/// ratified direction (`monetization.md:19`: higher ranks subsume lower; the
/// readability pin above proves it) — which was invisible while the cascade
/// was dark (a subscriber held exactly one paid tier) and becomes a
/// user-visible regression the moment fan-out lands: every gold subscriber
/// also holds the machine-named rank-1 unlock tier.
#[tokio::test]
async fn status_get_reports_the_paid_tier_not_the_fanned_unlock_tier() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    // gold is bought with a real payment window — the value the reply must
    // carry back, and the one an unlock-tier pick would silently drop
    // (`add_subscriber` NULLs `valid_until`, and the window is stamped on the
    // requested tier only).
    let window = far_future_window_secs();
    pay_and_approve(
        &router,
        &state,
        &author,
        &sub,
        "gold",
        Some(window),
        &[0x11u8; 32],
        1_000,
        "pay_gold_window",
    )
    .await;

    // The fan-out row lands and the author's drain approves it — the
    // subscriber now holds gold AND the unlock tier.
    let rows = pending(&router, state.clone(), author_id).await;
    let fanned = pending_for(&rows, "post-unlock-a").expect("fan-out row enqueued");
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        fanned.request_id,
        "post-unlock-a",
        &[sub.actor_id()],
        &[0x22u8; 32],
        2_000,
    )
    .await
    .expect("unlock approve ok");

    let status = status_get(&router, state.clone(), sub.actor_id().0, author_id).await;
    assert_eq!(
        status.tier.as_deref(),
        Some("gold"),
        "the subscription status is the paid tier the user chose, never a \
         machine-named unlock tier fanned in beneath it"
    );
    assert_eq!(
        status.expires_at,
        Some(Timestamp(window.saturating_mul(1_000_000))),
        "and it carries gold's real paid window — an unlock-tier pick reports \
         `expires_at = None` and the buyer's live subscription reads as lapsed"
    );
}

/// **The EXCLUSION half alone.** A pay-per-view unlock tier is minted ABOVE
/// the author's highest regular tier (`prepare_sell_post`'s pay-per-view arm
/// derives `max + 1`), so it *wins* a highest-rank pick. That is the trap: fixing only the direction moves the defect from one class
/// of unlock tier to the other. The designated-tier exclusion is what makes
/// the pick correct for both, so this pin holds the direction right and
/// deletes only the exclusion's justification.
#[tokio::test]
async fn status_get_reports_the_regular_tier_when_a_pay_per_view_tier_outranks_it() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let buyer = seed_actor(&state, "buyer").await;
    let author_id = author.actor_id().0;

    // gold is the author's only regular tier, so a pay-per-view post is sold
    // at rank 2 — above it, exactly as `prepare_sell_post` mints it.
    create_tier(&router, state.clone(), &author, "gold", 1, None)
        .await
        .expect("gold creates");
    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-b",
        2,
        Some(POST_B),
    )
    .await
    .expect("pay-per-view tier creates");

    let window = far_future_window_secs();
    pay_and_approve(
        &router,
        &state,
        &author,
        &buyer,
        "gold",
        Some(window),
        &[0x11u8; 32],
        1_000,
        "pay_gold_pv",
    )
    .await;
    // …and the same person buys the single post outright. Perpetual by
    // design: an unlock purchase stamps no window.
    pay_and_approve(
        &router,
        &state,
        &author,
        &buyer,
        "post-unlock-b",
        None,
        &[0x22u8; 32],
        2_000,
        "pay_unlock_b",
    )
    .await;

    let status = status_get(&router, state.clone(), buyer.actor_id().0, author_id).await;
    assert_eq!(
        status.tier.as_deref(),
        Some("gold"),
        "a single-post purchase is not the subscription relationship this read \
         reports — even when it outranks every regular tier"
    );
    assert_eq!(
        status.expires_at,
        Some(Timestamp(window.saturating_mul(1_000_000))),
        "and the window reported is gold's, not the perpetual purchase's `None`"
    );
}

/// **The DIRECTION half alone.** A subscriber holding two *regular* paid
/// tiers must be reported at the higher one, with that tier's window. This is
/// the secondary, pre-existing half: the at-or-below
/// cascade enrols a rank-R subscriber in every rank ≤ R with `valid_until`
/// NULL on all but the requested tier, so a lowest-rank pick names the
/// cheapest tier AND masks the real paid expiry. No unlock tier is involved,
/// so the exclusion cannot rescue it.
#[tokio::test]
async fn status_get_reports_the_top_paid_tier_not_the_cheapest_tier_the_subscriber_also_holds() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(&router, state.clone(), &author, "silver", 1, None)
        .await
        .expect("silver creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    // The subscriber holds silver indefinitely (an ungated grant) and then
    // upgrades to gold with a real payment window — the row shape the
    // cascade produces, reached here through the ordinary grant paths.
    let SubscribeReply::Queued { request_id } = subscribe(
        &router,
        state.clone(),
        sub.actor_id().0,
        author_id,
        "silver",
    )
    .await
    else {
        panic!("queued");
    };
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "silver",
        &[sub.actor_id()],
        &[0x33u8; 32],
        1_000,
    )
    .await
    .expect("silver approve ok");

    let window = far_future_window_secs();
    pay_and_approve(
        &router,
        &state,
        &author,
        &sub,
        "gold",
        Some(window),
        &[0x11u8; 32],
        2_000,
        "pay_gold_upgrade",
    )
    .await;

    let status = status_get(&router, state.clone(), sub.actor_id().0, author_id).await;
    assert_eq!(
        status.tier.as_deref(),
        Some("gold"),
        "higher ranks subsume lower (`monetization.md:19`), so the top tier \
         held is the subscription — not the cheapest one still on the roster"
    );
    assert_eq!(
        status.expires_at,
        Some(Timestamp(window.saturating_mul(1_000_000))),
        "and the cheapest tier's NULL window must not mask gold's real expiry"
    );
}

/// The other side of the readability guard: once the wrap IS delivered, a
/// further grant must NOT re-enqueue that tier. Keying suppression on
/// readability has to suppress something, or every grant (and every boot
/// reconcile) would re-queue every tier forever — a storm the author's client
/// pays for in full blob re-mints. Not red-first; it is the witness that the
/// mutation "drop the readability filter" reddens.
#[tokio::test]
async fn a_delivered_wrap_suppresses_the_next_fanout() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(&router, state.clone(), &author, "silver", 1, None)
        .await
        .expect("silver creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    // The gold grant: subscribe, then the author's client approves it — the
    // grant that enqueues the silver fan-out.
    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("queued");
    };
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &[0x55u8; 32],
        1_000,
    )
    .await
    .expect("gold approve ok");
    let rows = pending(&router, state.clone(), author_id).await;
    let fanned =
        pending_for_sub(&rows, "silver", sub.actor_id()).expect("the first grant enqueues silver");
    let period_key = [0x92u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        fanned.request_id,
        "silver",
        &[sub.actor_id()],
        &period_key,
        // Above the birth blob's `rotated_at` (1): the create already stored v1.
        2,
    )
    .await
    .expect("drain ok");
    assert_readable(&state, &author_id, "silver", &sub, &period_key).await;

    // A second grant landing (the idempotent already-subscribed `subscribe`
    // return, one of the enqueue's call sites) must find nothing to do.
    subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await;
    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for_sub(&rows, "silver", sub.actor_id()).is_none(),
        "a readable subscriber must not be re-enqueued — that is the storm the \
         readability key has to avoid"
    );
}

/// The creation-time reconcile is expiry-aware: a subscriber whose paid
/// window has lapsed no longer qualifies.
#[tokio::test]
async fn a_lapsed_subscriber_is_not_enqueued_at_creation() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");
    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("queued");
    };
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &[0x11u8; 32],
        1_000,
    )
    .await
    .expect("gold approve ok");
    // Lapse the window: one second into the past.
    state
        .db
        .set_subscriber_valid_until(&author_id, &sub.actor_id().0, "gold", Some(1))
        .await
        .unwrap();

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");

    let rows = pending(&router, state.clone(), author_id).await;
    assert!(
        pending_for(&rows, "post-unlock-a").is_none(),
        "a lapsed subscription no longer entitles — the reconcile must be expiry-aware"
    );
}

// ── the at-or-below-rank cascade delivered NOTHING
//    undesignated, so two ratified sentences were false at code level ─────────
//
// `monetization.md:19` — "the auto-approve cascade (`rank <= R`) enrolls every
// paid subscriber as a follower too" — and `:126` — "buyers of it also become
// followers, the ratified rank-0 cascade". Both were structurally dead for the
// DEFAULT author type: the (since retired) nest-side cascade could only mint
// from a nest-held period key, which a client-minted tier permanently lacks,
// and the fan-out enqueue that replaced it selected `unlocks_post IS NOT NULL`
// only — designated tiers and nothing else.
//
// The fix generalizes the enqueue to every tier at rank ≤ R the subscriber does
// not hold, narrowing the designation guard to designated→designated (the one
// edge that would make "one cheap post unlocks every sold post" true). The
// nest-side cascade itself is retired: the nest holds no period key, so every
// tier is the author's client's to mint.

/// Face 1: for a wholly client-minted author — the default type since
/// the storage-mode axis retired — a rank-2 grant must deliver the rank-1
/// undesignated tier too. Asserts **readability**, never the rank integer: a
/// rank assertion passes against the pre-fix code, which is how the defect
/// survived (`monetization.md:19`).
#[tokio::test]
async fn a_client_minted_grant_delivers_the_undesignated_cascade() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let sub = seed_actor(&state, "sub").await;
    let author_id = author.actor_id().0;

    // Two ordinary client-minted tiers: `tiers.create` writes no period key, so
    // the nest holds none for either and the nest-side cascade is inert.
    create_tier(&router, state.clone(), &author, "bronze", 1, None)
        .await
        .expect("bronze creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    let SubscribeReply::Queued { request_id } =
        subscribe(&router, state.clone(), sub.actor_id().0, author_id, "gold").await
    else {
        panic!("queued");
    };
    let gold_key = [0x51u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        request_id,
        "gold",
        &[sub.actor_id()],
        &gold_key,
        1_000,
    )
    .await
    .expect("gold approve ok");

    // The rank-1 tier the subscription includes must be enqueued for the
    // author's client — the only party that can mint its KeyBlob.
    let rows = pending(&router, state.clone(), author_id).await;
    let bronze = pending_for(&rows, "bronze")
        .expect("a rank-2 grant must fan out to the rank-1 undesignated tier (monetization.md:19)");
    assert!(
        bronze.payment_entitled,
        "the fanned-in row must be payment_entitled so the drain pump approves it without creator judgment"
    );
    assert_eq!(
        bronze.subscriber_id,
        sub.actor_id(),
        "the fan-out enqueues for the granted subscriber"
    );

    // Closing assertion: drain it the way the author's client does, and the
    // subscriber genuinely READS the included tier.
    let bronze_key = [0x52u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        bronze.request_id,
        "bronze",
        &[sub.actor_id()],
        &bronze_key,
        1_001,
    )
    .await
    .expect("bronze fan-out approve ok");
    assert_readable(&state, &author_id, "bronze", &sub, &bronze_key).await;
}

/// Face 2: `monetization.md:126` ratifies that "buyers of it also
/// become followers, the ratified rank-0 cascade" — but the pre-fix enqueue
/// returned `Ok(0)` for a designated grant, so a buyer of a sold post was
/// enrolled in nothing. Designated→undesignated is the edge that must fire;
/// designated→designated stays blocked (see
/// `buying_one_unlock_tier_does_not_unlock_another`, green through this change).
#[tokio::test]
async fn a_buyer_of_a_sold_post_becomes_a_follower() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let follower = seed_actor(&state, "follower").await;
    let buyer = seed_actor(&state, "buyer").await;
    let author_id = author.actor_id().0;

    // A first follow auto-provisions the reserved rank-0 `followers` tier
    // (`ensure_followers_tier` writes the ROW only — never a period key), which
    // is what the rank-0 cascade enrols into.
    subscribe(
        &router,
        state.clone(),
        follower.actor_id().0,
        author_id,
        FOLLOWERS_TIER,
    )
    .await;

    create_tier(
        &router,
        state.clone(),
        &author,
        "post-unlock-a",
        1,
        Some(POST_A),
    )
    .await
    .expect("unlock tier creates");

    // The buyer pays for the sold post through the unchanged waist.
    let entitlement = PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(author_id),
        buyer: Buyer::Actor(buyer.actor_id()),
        tier: "post-unlock-a".into(),
        valid_until_secs: None,
        external_ref: "pay_unlock_a".into(),
    };
    payment_core::apply_payment(&state, &entitlement)
        .await
        .expect("apply ok");

    let rows = pending(&router, state.clone(), author_id).await;
    let followers_row = pending_for_sub(&rows, FOLLOWERS_TIER, buyer.actor_id()).expect(
        "a buyer of a sold post becomes a follower — the ratified rank-0 cascade (monetization.md:126)",
    );
    assert!(
        followers_row.payment_entitled,
        "the fanned-in rank-0 row must be payment_entitled — the drain pump approves it without creator judgment"
    );

    // And it is a real follow: the author's client mints, the buyer reads.
    let followers_key = [0x61u8; 32];
    common::approve_with_mint(
        &router,
        state.clone(),
        &author,
        followers_row.request_id,
        FOLLOWERS_TIER,
        // Only the buyer: the seeding follower's own follow is still *pending*
        // (it enqueued — `followers` is keyless), so it is not on the roster the
        // approve arm reconciles against.
        &[buyer.actor_id()],
        &followers_key,
        2_000,
    )
    .await
    .expect("followers approve ok");
    assert_readable(&state, &author_id, FOLLOWERS_TIER, &buyer, &followers_key).await;
}

/// Boundary rule 2 — the granted tier is excluded from its own fan-out. The
/// caller has already written that tier's request carrying the real paid
/// window; re-selecting it here drives the `ON CONFLICT` arm, whose
/// `valid_until = NULL` would blank the window and silently turn a time-boxed
/// purchase perpetual. Read back through `status.get`, never the DB.
#[tokio::test]
async fn the_fanout_does_not_blank_the_granted_tiers_paid_window() {
    let (router, state) = router_with_db().await;
    let author = seed_actor(&state, "author").await;
    let buyer = seed_actor(&state, "buyer").await;
    let author_id = author.actor_id().0;
    let window = far_future_window_secs();

    // A rank-1 sibling exists so the fan-out genuinely runs a pass; the tier
    // under test is the rank-2 one the buyer actually pays for.
    create_tier(&router, state.clone(), &author, "bronze", 1, None)
        .await
        .expect("bronze creates");
    create_tier(&router, state.clone(), &author, "gold", 2, None)
        .await
        .expect("gold creates");

    pay_and_approve(
        &router,
        &state,
        &author,
        &buyer,
        "gold",
        Some(window),
        &[0x81u8; 32],
        1_000,
        "pay_gold_windowed",
    )
    .await;

    let status = status_get(&router, state.clone(), buyer.actor_id().0, author_id).await;
    assert_eq!(
        status.expires_at,
        Some(Timestamp(window.saturating_mul(1_000_000))),
        "the paid window must survive the fan-out — the granted tier is excluded from its own pass"
    );
}
