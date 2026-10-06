//! Wire contract for the queued-unsubscribe commit path (`monetization.md`
//! § Pillar 1 — the unsubscribe-commit rule, ratified 2026-08-01): a
//! subscriber's leave on a client-minted tier enqueues a `kind='unsubscribe'`
//! row, and that row is committed by the author client's removal rotation
//! (`subscribers.remove`) — NEVER approve-minted. `requests.approve` on an
//! unsubscribe row must refuse (`fauna.subscriptions.wrong_request_kind`):
//! the approve upload's KeyBlob covers the roster INCLUDING the leaver, so
//! accepting it would silently cancel the leave (the pre-2026-08-01 behavior —
//! every app's §2 Approve button did exactly this misroute; the shared
//! orchestration now refuses it client-side too, `AuthorError::WrongRequestKind`).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb`, real KeyBlob
//! crypto over the wire shapes — no mocks).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::{db::CacheDb, routes::AppState, rpc_router::RpcRouter, subscription_handlers};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    subscriptions::{
        PendingRequest, RequestsListReply, RequestsListRequest, StatusGetReply, StatusGetRequest,
        SubscribeReply, SubscribeRequest, TierCreateReply, UnsubscribeReply, UnsubscribeRequest,
    },
};

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    (b.build(), state)
}

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
) -> Result<TierCreateReply, RpcError> {
    common::create_tier(
        router,
        state,
        author,
        common::tier_create_request(author, name, rank),
    )
    .await
}

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

async fn unsubscribe(
    router: &RpcRouter,
    state: Arc<AppState>,
    subscriber: [u8; 32],
    author: [u8; 32],
) -> UnsubscribeReply {
    let req = UnsubscribeRequest {
        author_id: ActorId(author),
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).unwrap();
    let reply = common::call_raw(
        router,
        state,
        "fauna.subscriptions.unsubscribe",
        subscriber,
        Bytes::from(bytes.to_vec()),
    )
    .await
    .expect("unsubscribe ok");
    decode(&reply).expect("decode unsubscribe reply")
}

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

fn pending_for_sub<'a>(
    rows: &'a [PendingRequest],
    tier: &str,
    subscriber: ActorId,
) -> Option<&'a PendingRequest> {
    rows.iter()
        .find(|r| r.tier_name == tier && r.subscriber_id == subscriber)
}

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

const PERIOD_KEY: [u8; 32] = [7u8; 32];

#[tokio::test]
async fn an_unsubscribe_request_cannot_be_approve_minted() {
    let (router, state) = router_with_db().await;
    let author_kp = seed_actor(&state, "unsub-author").await;
    let sub_kp = seed_actor(&state, "unsub-leaver").await;
    let author = author_kp.actor_id().0;
    let subscriber = sub_kp.actor_id().0;

    // Client-minted tier; the subscribe queues; the author approve-mints it.
    create_tier(&router, state.clone(), &author_kp, "gold", 1)
        .await
        .expect("create tier");
    let s = subscribe(&router, state.clone(), subscriber, author, "gold").await;
    assert!(
        matches!(s, SubscribeReply::Queued { .. }),
        "a client-minted tier's subscribe queues, got {s:?}"
    );
    let rows = pending(&router, state.clone(), author).await;
    let join = pending_for_sub(&rows, "gold", ActorId(subscriber)).expect("queued subscribe row");
    assert_eq!(join.kind, "subscribe");
    common::approve_with_mint(
        &router,
        state.clone(),
        &author_kp,
        join.request_id,
        "gold",
        &[ActorId(subscriber)],
        &PERIOD_KEY,
        1_700_000_000_000_000,
    )
    .await
    .expect("approve the join");
    let st = status_get(&router, state.clone(), subscriber, author).await;
    assert_eq!(st.tier.as_deref(), Some("gold"), "active before the leave");

    // The leave queues a kind='unsubscribe' row.
    let u = unsubscribe(&router, state.clone(), subscriber, author).await;
    let leave_id = match u {
        UnsubscribeReply::Queued { request_id } => request_id,
        other => panic!("a client-minted tier's unsubscribe queues, got {other:?}"),
    };
    let rows = pending(&router, state.clone(), author).await;
    let leave =
        pending_for_sub(&rows, "gold", ActorId(subscriber)).expect("queued unsubscribe row");
    assert_eq!(leave.kind, "unsubscribe");
    assert_eq!(leave.request_id, leave_id);

    // The misroute every app's §2 Approve button used to make: approve-mint the
    // unsubscribe row with a KeyBlob still covering the leaver. Must refuse.
    let err = common::approve_with_mint(
        &router,
        state.clone(),
        &author_kp,
        leave_id,
        "gold",
        &[ActorId(subscriber)],
        &PERIOD_KEY,
        1_700_000_000_000_001,
    )
    .await
    .expect_err("an unsubscribe row must never be approve-minted");
    assert_eq!(err.code, "fauna.subscriptions.wrong_request_kind");

    // The refusal cancelled nothing: the row survives for the pump's removal
    // rotation to commit, and the leaver is still (honestly) subscribed.
    let rows = pending(&router, state.clone(), author).await;
    let survived =
        pending_for_sub(&rows, "gold", ActorId(subscriber)).expect("the leave row survives");
    assert_eq!(survived.kind, "unsubscribe");
    let st = status_get(&router, state.clone(), subscriber, author).await;
    assert_eq!(
        st.tier.as_deref(),
        Some("gold"),
        "still subscribed until the author client commits the removal"
    );
}
