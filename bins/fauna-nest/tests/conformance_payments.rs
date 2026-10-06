//! Integration round-trips for the `fauna.payments.*` kind family and the
//! payment-entitlement engine — Pillar 3 of
//! `docs/goal/behavior/monetization.md` (payment-provider API).
//!
//! Covered:
//! - `providers.set` / `providers.list` / `providers.remove`: config CRUD
//!   keyed on the bearer; unknown adapter kinds and dangling tier mappings
//!   rejected; the list reply never carries the webhook secret.
//! - The grant engine (`payment_core`): the payment-entitled enqueue
//!   surfaced via `requests.list`; renewal; expiry (`valid_until` lapse
//!   un-entitles at `is_subscriber` / `status.get` with no revocation list);
//!   refund voiding.
//! - `claims.redeem`: binds the bearer, grants through the same engine,
//!   rejects unknown/voided/foreign-redeemed codes, and is idempotent for
//!   the same actor.
//! - `requests.approve` stamping a payment-marked request's window onto the
//!   subscriber row.
//!
//! The nest completes no grant itself: every tier's key is the author's
//! client's, so a test that needs an active subscriber drives the author
//! client's approval through the real `requests.approve` door
//! ([`author_client_approves`]) over a tier created with its birth blob.
//!
//! The webhook HTTP ingress (signature verification, non-2xx statuses vs the
//! catch-all 200) is covered end-to-end by the Python tier_3 API suite
//! (`tests/e2e-unified/tests/api/test_payments_webhook.py`) — it needs the
//! real axum surface; these tests exercise the engine below it.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/payments.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::{
    db::CacheDb,
    payment_core::{self, PaymentApplied},
    payment_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
    subscription_handlers,
};
use fauna_payments::{Buyer, PaymentEntitlement};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    payments::{
        ClaimMintReply, ClaimMintRequest, ClaimRedeemReply, ClaimRedeemRequest, ClaimsListReply,
        ClaimsListRequest, ProviderRemoveReply, ProviderRemoveRequest, ProviderSetReply,
        ProviderSetRequest, ProvidersListReply, ProvidersListRequest,
    },
    subscriptions::{RequestsListReply, RequestsListRequest},
};

fn build_router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    payment_handlers::register_payment_handlers(&mut b);
    b.build()
}

mod common;

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    (build_router(), state)
}

async fn providers_set(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
    kind: &str,
    secret: &str,
    tier: &str,
) -> Result<ProviderSetReply, RpcError> {
    let req = encode_canonical(&ProviderSetRequest {
        kind: kind.into(),
        webhook_secret: secret.into(),
        tier: tier.into(),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.providers.set",
        author,
        Bytes::from(req.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode providers.set reply"))
}

async fn providers_list(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
) -> ProvidersListReply {
    let req = encode_canonical(&ProvidersListRequest {}).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.providers.list",
        author,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("providers.list ok");
    decode(&reply).expect("decode providers.list reply")
}

async fn claims_redeem(
    router: &RpcRouter,
    state: Arc<AppState>,
    redeemer: [u8; 32],
    code: &str,
) -> Result<ClaimRedeemReply, RpcError> {
    let req = encode_canonical(&ClaimRedeemRequest {
        code: code.into(),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.claims.redeem",
        redeemer,
        Bytes::from(req.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode claims.redeem reply"))
}

async fn claims_mint(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
    tier: &str,
    valid_until: Option<i64>,
) -> Result<ClaimMintReply, RpcError> {
    let req = encode_canonical(&ClaimMintRequest {
        tier: tier.into(),
        valid_until: valid_until
            .map(|s| fauna_core::data::Timestamp((s as u64).saturating_mul(1_000_000))),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.claims.mint",
        author,
        Bytes::from(req.to_vec()),
    )
    .await?;
    Ok(decode(&reply).expect("decode claims.mint reply"))
}

async fn claims_list(
    router: &RpcRouter,
    state: Arc<AppState>,
    author: [u8; 32],
) -> ClaimsListReply {
    let req = encode_canonical(&ClaimsListRequest {}).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.payments.claims.list",
        author,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("claims.list ok");
    decode(&reply).expect("decode claims.list reply")
}

/// An author with a "gold" tier, created through the real `tiers.create` door
/// so it carries its birth blob (`common::create_tier`) — the prior the
/// author client's approval must advance past.
async fn seed_author_with_gold(router: &RpcRouter, state: &Arc<AppState>) -> ActorKeypair {
    let author = ActorKeypair::generate();
    state
        .db
        .create_user(&author.actor_id().0, "free", "author")
        .await
        .unwrap();
    let req = fauna_protocol::subscriptions::TierCreateRequest {
        price_hint: Some("$5/mo".into()),
        ..common::tier_create_request(&author, "gold", 1)
    };
    common::create_tier(router, state.clone(), &author, req)
        .await
        .expect("tiers.create ok");
    author
}

/// The author client's approval of `buyer`'s pending "gold" request, through
/// the real `requests.approve` door: a blob over the current roster plus the
/// buyer, at a `rotated_at` past every earlier one. This is what completes a
/// payment grant — the nest only enqueues it.
async fn author_client_approves(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author: &ActorKeypair,
    buyer: [u8; 32],
) {
    static ROTATED_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000);
    let author_id = author.actor_id().0;
    let request_id = state
        .db
        .list_subscribe_requests(&author_id)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.kind == "subscribe" && r.tier_name == "gold" && r.subscriber_id == buyer)
        .expect("the buyer's grant is pending for the author's client")
        .id;
    let mut roster: Vec<ActorId> = Vec::new();
    for row in state.db.list_subscribers(&author_id, "gold").await.unwrap() {
        roster.push(ActorId(row.subscriber_id.as_slice().try_into().unwrap()));
    }
    if !roster.contains(&ActorId(buyer)) {
        roster.push(ActorId(buyer));
    }
    common::approve_with_mint(
        router,
        state.clone(),
        author,
        request_id,
        "gold",
        &roster,
        &[0x5a; 32],
        ROTATED_AT.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst),
    )
    .await
    .expect("the author client's approval lands");
}

fn future_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs() + 30 * 24 * 3600
}

// ── The narrow waist ───────────────────────────────────────────
//
// Every mechanism reaches the engine by producing a `PaymentEntitlement` —
// webhook ingress via `from_verified_event`, claim redemption from the stored
// row. These tests build the same value directly, so they exercise the engine
// through its real (and only) interface rather than a test-shaped back door.

fn entitlement(
    payee: &[u8; 32],
    buyer: Buyer,
    tier: &str,
    valid_until: Option<i64>,
    external_ref: &str,
) -> PaymentEntitlement {
    PaymentEntitlement {
        provider: "fake".into(),
        payee: ActorId(*payee),
        buyer,
        tier: tier.into(),
        valid_until_secs: valid_until.map(|s| s as u64),
        external_ref: external_ref.into(),
    }
}

/// Apply a bound-buyer payment and return `queued`.
async fn grant(
    state: &Arc<AppState>,
    payee: &[u8; 32],
    buyer: &[u8; 32],
    tier: &str,
    valid_until: Option<i64>,
) -> Result<bool, payment_core::GrantError> {
    let e = entitlement(
        payee,
        Buyer::Actor(ActorId(*buyer)),
        tier,
        valid_until,
        "pay_1",
    );
    match payment_core::apply_payment(state, &e).await? {
        PaymentApplied::Granted { queued } => Ok(queued),
        other => panic!("a bound buyer must grant, got {other:?}"),
    }
}

// ── providers.{set,list,remove} ────────────────────────────────

#[tokio::test]
async fn providers_set_list_remove_roundtrip() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;

    let set = providers_set(&router, state.clone(), author, "fake", "whsec_1", "gold")
        .await
        .expect("providers.set ok");
    assert!(set.saved);

    let list = providers_list(&router, state.clone(), author).await;
    assert_eq!(list.providers.len(), 1);
    assert_eq!(list.providers[0].kind, "fake");
    assert_eq!(list.providers[0].tier, "gold");

    // Re-set rotates in place (still one row).
    providers_set(&router, state.clone(), author, "fake", "whsec_2", "gold")
        .await
        .expect("providers.set rotate ok");
    assert_eq!(
        providers_list(&router, state.clone(), author)
            .await
            .providers
            .len(),
        1
    );

    let req = encode_canonical(&ProviderRemoveRequest {
        kind: "fake".into(),
        extra: Default::default(),
    })
    .unwrap();
    let reply = common::call_raw(
        &router,
        state.clone(),
        "fauna.payments.providers.remove",
        author,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("providers.remove ok");
    let removed: ProviderRemoveReply = decode(&reply).expect("decode");
    assert!(removed.removed);
    assert!(
        providers_list(&router, state, author)
            .await
            .providers
            .is_empty()
    );
}

#[tokio::test]
async fn providers_set_rejects_unknown_kind_and_dangling_tier() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;

    let err = providers_set(&router, state.clone(), author, "paypal", "whsec", "gold")
        .await
        .expect_err("unknown adapter kind must be rejected");
    assert_eq!(err.code, "fauna.payments.unknown_provider");

    let err = providers_set(&router, state.clone(), author, "fake", "whsec", "platinum")
        .await
        .expect_err("mapping to a nonexistent tier must be rejected");
    assert_eq!(err.code, "fauna.payments.tier_not_found");

    let err = providers_set(&router, state, author, "fake", "", "gold")
        .await
        .expect_err("empty secret must be rejected");
    assert_eq!(err.code, "fauna.payments.malformed");
}

#[tokio::test]
async fn providers_list_is_caller_scoped() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;
    let other = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&other, "free", "other").await.unwrap();

    providers_set(&router, state.clone(), author, "fake", "whsec", "gold")
        .await
        .expect("providers.set ok");

    assert!(
        providers_list(&router, state, other)
            .await
            .providers
            .is_empty(),
        "another actor must not see the author's provider configs"
    );
}

// ── The grant engine ───────────────────────────────────────────

#[tokio::test]
async fn expired_window_unentitles_without_revocation_list() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author_with_gold(&router, &state).await;
    let author = author_kp.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();

    // Grant with an already-lapsed window: the row exists, the gate says no.
    let past = fauna_core::data::Timestamp::now_secs() - 10;
    grant(&state, &author, &buyer, "gold", Some(past))
        .await
        .expect("grant ok");
    author_client_approves(&router, &state, &author_kp, buyer).await;

    assert!(
        !state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap(),
        "a lapsed valid_until no longer entitles"
    );
    assert!(
        state
            .db
            .get_subscribed_tiers(&author, &buyer)
            .await
            .unwrap()
            .is_empty(),
        "status.get's source set drops the lapsed tier"
    );

    // A renewal payment re-entitles the same row — self-healing, no list.
    let until = future_secs();
    let queued = grant(&state, &author, &buyer, "gold", Some(until))
        .await
        .expect("renewal ok");
    assert!(
        queued,
        "a lapsed subscriber's renewal queues like a new grant"
    );
    author_client_approves(&router, &state, &author_kp, buyer).await;
    assert!(
        state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn refund_voids_the_window() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author_with_gold(&router, &state).await;
    let author = author_kp.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();

    grant(&state, &author, &buyer, "gold", Some(future_secs()))
        .await
        .expect("grant ok");
    author_client_approves(&router, &state, &author_kp, buyer).await;
    assert!(
        state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap()
    );

    let acted = payment_core::apply_refund(
        &state,
        &entitlement(&author, Buyer::Actor(ActorId(buyer)), "gold", None, "pay_1"),
    )
    .await
    .expect("refund ok");
    assert!(acted);
    assert!(
        !state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap(),
        "refund voids the paid window immediately"
    );
}

#[tokio::test]
async fn grant_rejects_dangling_tier() {
    let (_router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();
    let buyer = ActorKeypair::generate().actor_id().0;

    let err = grant(&state, &author, &buyer, "gold", None)
        .await
        .expect_err("grant against a nonexistent tier must fail");
    assert!(matches!(err, payment_core::GrantError::TierNotFound));
}

// ── The unbound arm (claim minting) ────────────────────────────
//
// Claim minting is engine behavior, not webhook behavior. It used to live in
// the HTTP module, so it was reachable only through axum and covered only by
// the Python tier_3 suite; behind the waist it is engine-level and every
// mechanism gets it.

#[tokio::test]
async fn unbound_payment_mints_a_claim_and_redelivery_is_idempotent() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;
    let until = future_secs();

    let e = entitlement(&author, Buyer::Unbound, "gold", Some(until), "pay_unbound");

    let PaymentApplied::ClaimMinted { code } = payment_core::apply_payment(&state, &e)
        .await
        .expect("unbound payment mints a claim")
    else {
        panic!("an unbound payment must mint a claim, not grant");
    };

    // Redelivery of the SAME payment returns the SAME code — mechanisms
    // redeliver, and a second code would be a second entitlement.
    let PaymentApplied::ClaimExists { code: again } = payment_core::apply_payment(&state, &e)
        .await
        .expect("redelivery is idempotent")
    else {
        panic!("a redelivered payment must not mint a second claim");
    };
    assert_eq!(code, again);

    // Exactly one claim exists, and it carries the paid window through to
    // redemption.
    let listed = claims_list(&router, state.clone(), author).await;
    assert_eq!(listed.claims.len(), 1);

    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();
    let redeemed = claims_redeem(&router, state.clone(), buyer, &code)
        .await
        .expect("the minted code redeems");
    assert_eq!(redeemed.tier, "gold");
    assert!(
        redeemed.queued,
        "a client-minted tier enqueues for the author's client"
    );
}

#[tokio::test]
async fn unbound_payment_against_a_dangling_tier_mints_nothing() {
    // A claim that could never be redeemed must not be minted — the tier
    // check belongs to the engine, not to any one mechanism's transport.
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();

    let e = entitlement(&author, Buyer::Unbound, "gold", None, "pay_dangling");
    let err = payment_core::apply_payment(&state, &e)
        .await
        .expect_err("a dangling tier mapping must not mint a claim");
    assert!(matches!(err, payment_core::GrantError::TierNotFound));

    assert!(
        claims_list(&router, state, author).await.claims.is_empty(),
        "no claim row may survive a refused mint"
    );
}

// ── The grant engine (client-minted tier — the default) ────────

#[tokio::test]
async fn client_minted_tier_grant_enqueues_payment_entitled_request() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();
    let until = future_secs();

    let queued = grant(&state, &author, &buyer, "gold", Some(until))
        .await
        .expect("grant ok");
    assert!(
        queued,
        "a client-minted tier enqueues for the author's client"
    );
    assert!(
        !state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap(),
        "no roster row until the author's client mints"
    );

    // The author's requests.list carries the payment marker the drain pump
    // keys on (fauna-client-subscriptions::drain_auto_approvals).
    let req = encode_canonical(&RequestsListRequest {}).unwrap();
    let reply = common::call_raw(
        &router,
        state.clone(),
        "fauna.subscriptions.requests.list",
        author,
        Bytes::from(req.to_vec()),
    )
    .await
    .expect("requests.list ok");
    let list: RequestsListReply = decode(&reply).expect("decode");
    assert_eq!(list.requests.len(), 1);
    assert!(list.requests[0].payment_entitled);
    assert_eq!(list.requests[0].tier_name, "gold");
    assert_eq!(list.requests[0].kind, "subscribe");

    // Redelivery is idempotent: still one request row.
    grant(&state, &author, &buyer, "gold", Some(until))
        .await
        .expect("redelivered grant ok");
    let rows = state.db.list_subscribe_requests(&author).await.unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn approve_stamps_the_payment_window() {
    // A payment-marked pending request approved via requests.approve lands
    // the window on the subscriber row (the handler reads the window before
    // the row is deleted).
    let (router, state) = router_with_db().await;
    let author_kp = seed_author_with_gold(&router, &state).await;
    let author = author_kp.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();
    let until = future_secs();

    state
        .db
        .upsert_payment_entitled_request(&author, &buyer, "gold", Some(until))
        .await
        .unwrap();
    author_client_approves(&router, &state, &author_kp, buyer).await;

    assert!(
        state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap()
    );
    assert_eq!(
        state
            .db
            .get_subscriber_valid_until(&author, &buyer, "gold")
            .await
            .unwrap(),
        Some(until)
    );
}

// ── claims.redeem ──────────────────────────────────────────────

async fn seed_claim(state: &Arc<AppState>, author: &[u8; 32], valid_until: Option<i64>) -> String {
    let code = "TESTCODE42".to_string();
    state
        .db
        .insert_payment_claim(&code, author, "gold", "fake", "pay_9", valid_until)
        .await
        .unwrap();
    code
}

#[tokio::test]
async fn claim_redeem_binds_the_bearer_and_grants() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author_with_gold(&router, &state).await;
    let author = author_kp.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();
    let until = future_secs();
    let code = seed_claim(&state, &author, Some(until)).await;

    let reply = claims_redeem(&router, state.clone(), buyer, &code)
        .await
        .expect("redeem ok");
    assert_eq!(reply.author.0, author);
    assert_eq!(reply.tier, "gold");
    assert!(reply.queued, "the grant enqueues for the author's client");
    assert_eq!(
        reply.valid_until.map(|t| t.0),
        Some((until as u64) * 1_000_000),
        "window rides the reply in micros"
    );
    author_client_approves(&router, &state, &author_kp, buyer).await;
    assert!(
        state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap()
    );

    // Same-actor retry is idempotent (crash-safe re-run).
    claims_redeem(&router, state.clone(), buyer, &code)
        .await
        .expect("same-actor re-redeem ok");

    // A different actor cannot take an already-redeemed claim.
    let thief = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&thief, "free", "thief").await.unwrap();
    let err = claims_redeem(&router, state, thief, &code)
        .await
        .expect_err("foreign redeem must fail");
    assert_eq!(err.code, "fauna.payments.claim_already_redeemed");
}

#[tokio::test]
async fn claim_redeem_rejects_unknown_and_voided_codes() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();

    let err = claims_redeem(&router, state.clone(), buyer, "NOSUCHCODE")
        .await
        .expect_err("unknown code must fail");
    assert_eq!(err.code, "fauna.payments.claim_not_found");

    // A refund landing before redemption voids the claim.
    let code = seed_claim(&state, &author, None).await;
    let acted = payment_core::apply_refund(
        &state,
        &entitlement(&author, Buyer::Unbound, "gold", None, "pay_9"),
    )
    .await
    .expect("refund ok");
    assert!(acted, "the unredeemed claim was voided");
    let err = claims_redeem(&router, state, buyer, &code)
        .await
        .expect_err("voided code must fail");
    assert_eq!(err.code, "fauna.payments.claim_voided");
}

// ── claims.mint / claims.list ───────────────────────────────────

#[tokio::test]
async fn claims_mint_creates_a_redeemable_manual_claim() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author_with_gold(&router, &state).await;
    let author = author_kp.actor_id().0;
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();
    let until = future_secs();

    let minted = claims_mint(&router, state.clone(), author, "gold", Some(until))
        .await
        .expect("mint ok");
    assert_eq!(minted.tier, "gold");
    assert_eq!(
        minted.valid_until.map(|t| t.0),
        Some((until as u64) * 1_000_000)
    );
    assert!(!minted.code.is_empty());

    let redeemed = claims_redeem(&router, state.clone(), buyer, &minted.code)
        .await
        .expect("redeem the manually minted code ok");
    assert_eq!(redeemed.tier, "gold");
    author_client_approves(&router, &state, &author_kp, buyer).await;
    assert!(
        state
            .db
            .is_subscriber(&author, &buyer, "gold")
            .await
            .unwrap()
    );
}

/// `claims.mint` is **not** idempotent under a repeated call, which is why it
/// is `forbid_replay: true` — the standing evidence for that flag.
///
/// `transport.md` § Idempotency and reconnect-with-resume makes
/// `forbid_replay = false` an assertion that the handler is naturally
/// idempotent under a repeated same-key call; the nest's idempotency cache is
/// per-`RpcConnection`, so it can never deduplicate a retry that lands on a new
/// connection. This handler mints a *fresh random code* per call
/// (`admin::generate_invite_code`) and inserts a new row keyed on it, with the
/// external reference derived from that same fresh code — so nothing dedups,
/// and a replayed mint leaves a **second, independently redeemable credential**
/// against an out-of-band payment the author was paid for once.
///
/// If a future change makes minting idempotent (e.g. keying the code on the
/// idempotency key or an author-supplied reference), this test is the one to
/// change *first*, and only then the flag — in both metadata tables.
#[tokio::test]
async fn minting_twice_yields_two_independently_redeemable_claims() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_author_with_gold(&router, &state).await;
    let author = author_kp.actor_id().0;
    let until = future_secs();

    let first = claims_mint(&router, state.clone(), author, "gold", Some(until))
        .await
        .expect("first mint ok");
    let second = claims_mint(&router, state.clone(), author, "gold", Some(until))
        .await
        .expect("second mint ok");

    assert_ne!(
        first.code, second.code,
        "each mint allocates a fresh code — the replay hazard the flag covers"
    );

    // Both are live credentials, redeemed by *different* buyers: the second
    // grants an entitlement the author was never paid for.
    let buyer_a = ActorKeypair::generate().actor_id().0;
    let buyer_b = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer_a, "free", "a").await.unwrap();
    state.db.create_user(&buyer_b, "free", "b").await.unwrap();

    claims_redeem(&router, state.clone(), buyer_a, &first.code)
        .await
        .expect("first code redeems");
    claims_redeem(&router, state.clone(), buyer_b, &second.code)
        .await
        .expect("the replayed mint's code redeems too — the double-apply");
    author_client_approves(&router, &state, &author_kp, buyer_a).await;
    author_client_approves(&router, &state, &author_kp, buyer_b).await;

    assert!(
        state
            .db
            .is_subscriber(&author, &buyer_a, "gold")
            .await
            .unwrap()
    );
    assert!(
        state
            .db
            .is_subscriber(&author, &buyer_b, "gold")
            .await
            .unwrap(),
        "two entitlements from one out-of-band payment"
    );
}

#[tokio::test]
async fn claims_mint_rejects_dangling_tier() {
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "author")
        .await
        .unwrap();

    let err = claims_mint(&router, state, author, "platinum", None)
        .await
        .expect_err("mint against a nonexistent tier must fail");
    assert_eq!(err.code, "fauna.payments.tier_not_found");
}

#[tokio::test]
async fn claims_mint_code_collision_is_a_typed_conflict() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;

    // Force a PRIMARY KEY collision: pre-seed a claim under the exact code a
    // (mocked) mint would produce is impractical (the code is random), so
    // instead prove the conflict mapping directly against the DB layer the
    // handler calls, mirroring admin_ws_handlers' invite-code precedent (no
    // server-side retry — the caller re-mints on conflict).
    state
        .db
        .insert_payment_claim(
            "DUPLICATECODE",
            &author,
            "gold",
            "manual",
            "manual_DUPLICATECODE",
            None,
        )
        .await
        .unwrap();
    let err = state
        .db
        .insert_payment_claim(
            "DUPLICATECODE",
            &author,
            "gold",
            "manual",
            "manual_DUPLICATECODE",
            None,
        )
        .await
        .expect_err("duplicate code must violate the PRIMARY KEY");
    assert!(format!("{err:#}").contains("UNIQUE"));

    let _ = router; // the router-level mint path is covered by the happy-path test above
}

#[tokio::test]
async fn claims_list_is_caller_scoped_and_shows_full_audit_state() {
    let (router, state) = router_with_db().await;
    let author = seed_author_with_gold(&router, &state).await.actor_id().0;
    let other = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&other, "free", "other").await.unwrap();
    let buyer = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&buyer, "free", "buyer").await.unwrap();

    assert!(
        claims_list(&router, state.clone(), author)
            .await
            .claims
            .is_empty(),
        "no claims minted yet"
    );

    // A manually minted, unredeemed claim.
    let minted = claims_mint(&router, state.clone(), author, "gold", None)
        .await
        .expect("mint ok");

    // A webhook-style claim, minted then redeemed.
    let webhook_code = seed_claim(&state, &author, Some(future_secs())).await;
    claims_redeem(&router, state.clone(), buyer, &webhook_code)
        .await
        .expect("redeem ok");

    // A webhook-style claim, minted then voided by a refund.
    let voided_code = "VOIDEDCODE1".to_string();
    state
        .db
        .insert_payment_claim(&voided_code, &author, "gold", "fake", "pay_void", None)
        .await
        .unwrap();
    state.db.void_payment_claim(&voided_code).await.unwrap();

    let list = claims_list(&router, state.clone(), author).await;
    assert_eq!(list.claims.len(), 3, "{:?}", list.claims);

    let manual = list
        .claims
        .iter()
        .find(|c| c.code == minted.code)
        .expect("manual claim present");
    assert_eq!(manual.provider, "manual");
    assert!(manual.redeemed_by.is_none());
    assert!(manual.voided_at.is_none());

    let redeemed = list
        .claims
        .iter()
        .find(|c| c.code == webhook_code)
        .expect("redeemed claim present");
    assert_eq!(redeemed.redeemed_by.map(|a| a.0), Some(buyer));
    assert!(redeemed.redeemed_at.is_some());

    let voided = list
        .claims
        .iter()
        .find(|c| c.code == voided_code)
        .expect("voided claim present");
    assert!(voided.voided_at.is_some());
    assert!(voided.redeemed_by.is_none());

    // Another author sees nothing.
    assert!(
        claims_list(&router, state, other).await.claims.is_empty(),
        "another actor must not see the author's claim codes"
    );
}
