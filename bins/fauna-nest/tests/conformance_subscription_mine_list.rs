//! Integration round-trip for `fauna.subscriptions.mine.list` — the
//! authenticated *consumer-side* read that enumerates the **calling actor's own
//! subscriptions across every creator** (`docs/goal/behavior/monetization.md`
//! § Pillar 1 — the `subscription-settings` consumer page `subscription-mine-list`).
//!
//! Why a new kind and not `status.get`: `status.get` is keyed on a
//! request-supplied `author_id` (the per-creator status on a profile), so it
//! answers "am I subscribed to *this* creator?" — there was **no** caller-scoped
//! "all my subscriptions" enumeration. The consumer page needs exactly that: one
//! row per `(creator, tier)`, active **and** pending, with the creator's handle
//! resolved nest-side (the author is a local account) so the client renders a
//! handle without an N+1 lookup.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/subscriptions.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_nest::{db::CacheDb, routes::AppState, rpc_router::RpcRouter, subscription_handlers};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    subscriptions::{MineListReply, MineListRequest},
};

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    (b.build(), state)
}

async fn mine_list(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> Result<MineListReply, RpcError> {
    let meta = router
        .kind_meta("fauna.subscriptions.mine.list")
        .expect("kind registered");
    let req = encode_canonical(&MineListRequest {}).unwrap();
    let reply_bytes = (meta.handler)(state, actor, Bytes::from(req.to_vec())).await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

#[tokio::test]
async fn mine_list_returns_active_and_pending_with_creator_handle() {
    let (router, state) = router_with_db().await;
    let alice = ActorKeypair::generate().actor_id().0;
    let bob = ActorKeypair::generate().actor_id().0;
    let me = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    state.db.create_user(&bob, "free", "bob").await.unwrap();
    state.db.create_user(&me, "free", "me").await.unwrap();
    // `create_user`'s third arg is the *label*, not the handle — set the handle
    // explicitly so the read's nest-side handle resolution is exercised.
    state.db.set_handle(&alice, "alice").await.unwrap();
    state.db.set_handle(&bob, "bob").await.unwrap();
    // The subscriber/request rows FK to subscription_tiers(author_id, name).
    state
        .db
        .create_subscription_tier(
            &alice, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    state
        .db
        .create_subscription_tier(
            &bob, "silver", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();

    // I am an APPROVED subscriber of alice's "gold" tier …
    state
        .db
        .add_subscriber(&alice, &me, "gold", None)
        .await
        .unwrap();
    // … and I have a PENDING (encrypted-mode queued) subscribe to bob's "silver".
    state
        .db
        .insert_subscribe_request(&bob, &me, "silver", "subscribe", None)
        .await
        .unwrap();

    let reply = mine_list(&router, state, me).await.expect("mine.list ok");
    assert_eq!(reply.subscriptions.len(), 2, "active + pending");

    let gold = reply
        .subscriptions
        .iter()
        .find(|s| s.tier == "gold")
        .expect("alice/gold present");
    assert_eq!(gold.author_id.0, alice);
    assert_eq!(gold.status, "active");
    assert_eq!(gold.handle.as_deref(), Some("alice"));

    let silver = reply
        .subscriptions
        .iter()
        .find(|s| s.tier == "silver")
        .expect("bob/silver present");
    assert_eq!(silver.author_id.0, bob);
    assert_eq!(silver.status, "pending");
    assert_eq!(silver.handle.as_deref(), Some("bob"));
}

#[tokio::test]
async fn mine_list_is_caller_scoped() {
    // The read keys on the *calling* actor: a creator with an active subscriber
    // sees *their own* (empty) consumer list, not the subscriber's row.
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let subscriber = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state
        .db
        .create_user(&subscriber, "free", "carol")
        .await
        .unwrap();
    state
        .db
        .create_subscription_tier(
            &author, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    state
        .db
        .add_subscriber(&author, &subscriber, "gold", None)
        .await
        .unwrap();

    let sub_reply = mine_list(&router, state.clone(), subscriber)
        .await
        .expect("subscriber mine.list ok");
    assert_eq!(sub_reply.subscriptions.len(), 1);
    assert_eq!(sub_reply.subscriptions[0].status, "active");

    let author_reply = mine_list(&router, state, author)
        .await
        .expect("author mine.list ok");
    assert!(
        author_reply.subscriptions.is_empty(),
        "the creator is not subscribed to anyone"
    );
}

#[tokio::test]
async fn mine_list_prefers_active_over_a_stale_pending_for_same_tier() {
    // Defensive: if both an approved subscriber row and a (normally-deleted)
    // pending subscribe request exist for the same (creator, tier), the active
    // entry wins and the pending duplicate is dropped — never two rows.
    let (router, state) = router_with_db().await;
    let alice = ActorKeypair::generate().actor_id().0;
    let me = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&alice, "free", "alice").await.unwrap();
    state.db.create_user(&me, "free", "me").await.unwrap();
    state
        .db
        .create_subscription_tier(
            &alice, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    state
        .db
        .add_subscriber(&alice, &me, "gold", None)
        .await
        .unwrap();
    state
        .db
        .insert_subscribe_request(&alice, &me, "gold", "subscribe", None)
        .await
        .unwrap();

    let reply = mine_list(&router, state, me).await.expect("mine.list ok");
    assert_eq!(reply.subscriptions.len(), 1, "deduped to the active row");
    assert_eq!(reply.subscriptions[0].status, "active");
}

#[tokio::test]
async fn mine_list_empty_for_actor_with_no_subscriptions() {
    let (router, state) = router_with_db().await;
    let me = ActorKeypair::generate().actor_id().0;
    state.db.create_user(&me, "free", "me").await.unwrap();

    let reply = mine_list(&router, state, me).await.expect("mine.list ok");
    assert!(reply.subscriptions.is_empty());
}
