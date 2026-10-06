//! Integration round-trip for `fauna.subscriptions.tiers.list` — the
//! authenticated author-side read that returns the *calling* actor's own
//! subscription-tier definitions (`docs/goal/behavior/monetization.md`
//! § Pillar 1 — the profile Tiers tab SELF §1 "My tiers" list).
//!
//! Why a WS-RPC kind and not the public HTTP read: the per-author
//! `GET /api/v1/subscriptions/tiers/{author_id}` route is the *unauthenticated*
//! / browser / another-creator path (and it omits `auto_approve`). The SELF
//! author view holds an authenticated WS-RPC connection, so it reads its own
//! tiers over the wire uniformly with the other subscription reads
//! (`requests.list` / `subscribers.list`) — wasm-safe, no per-app HTTP glue
//! (priority #2). The reply carries `auto_approve` so the edit form round-trips
//! it.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/subscriptions.rs`.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use bytes::Bytes;

use fauna_core::identity::ActorId;
use fauna_core::identity::ActorKeypair;
use fauna_nest::{db::CacheDb, routes::AppState, rpc_router::RpcRouter, subscription_handlers};
use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    subscriptions::{OffersListRequest, TiersListReply, TiersListRequest},
};

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    subscription_handlers::register_subscription_handlers(&mut b);
    (b.build(), state)
}

async fn list_tiers(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
) -> Result<TiersListReply, RpcError> {
    let meta = router
        .kind_meta("fauna.subscriptions.tiers.list")
        .expect("kind registered");
    let req = encode_canonical(&TiersListRequest {}).unwrap();
    let reply_bytes = (meta.handler)(state, actor, Bytes::from(req.to_vec())).await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

async fn list_offers(
    router: &RpcRouter,
    state: Arc<AppState>,
    caller: [u8; 32],
    author: [u8; 32],
) -> Result<TiersListReply, RpcError> {
    let meta = router
        .kind_meta("fauna.subscriptions.offers.list")
        .expect("kind registered");
    let req = encode_canonical(&OffersListRequest {
        author_id: ActorId(author),
        extra: Default::default(),
    })
    .unwrap();
    let reply_bytes = (meta.handler)(state, caller, Bytes::from(req.to_vec())).await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

#[tokio::test]
async fn tiers_list_returns_callers_own_tiers_ordered_by_rank() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "alice").await.unwrap();

    // Seed out of rank order to prove the read sorts ascending by rank.
    state
        .db
        .create_subscription_tier(
            &actor,
            "gold",
            20,
            Some("Top tier"),
            Some("$10/mo"),
            Some("https://pay.example/gold"),
            false,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    state
        .db
        .create_subscription_tier(
            &actor,
            "followers",
            0,
            None,
            None,
            None,
            true,
            None,
            None,
            false,
        )
        .await
        .unwrap();

    let reply = list_tiers(&router, state, actor)
        .await
        .expect("tiers.list ok");

    assert_eq!(reply.tiers.len(), 2);
    // Ascending rank: the free followers tier (rank 0) first.
    assert_eq!(reply.tiers[0].name, "followers");
    assert_eq!(reply.tiers[0].rank, 0);
    assert!(reply.tiers[0].auto_approve);
    assert_eq!(reply.tiers[0].description, None);

    assert_eq!(reply.tiers[1].name, "gold");
    assert_eq!(reply.tiers[1].rank, 20);
    assert!(!reply.tiers[1].auto_approve);
    assert_eq!(reply.tiers[1].description.as_deref(), Some("Top tier"));
    assert_eq!(reply.tiers[1].price_hint.as_deref(), Some("$10/mo"));
    assert_eq!(
        reply.tiers[1].payment_url.as_deref(),
        Some("https://pay.example/gold")
    );
}

#[tokio::test]
async fn tiers_list_empty_for_author_with_no_tiers() {
    let (router, state) = router_with_db().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    state.db.create_user(&actor, "free", "bob").await.unwrap();

    let reply = list_tiers(&router, state, actor)
        .await
        .expect("tiers.list ok");
    assert!(reply.tiers.is_empty());
}

#[tokio::test]
async fn tiers_list_is_caller_scoped() {
    // The read keys on the *calling* actor, never a request-supplied id: a
    // second actor sees their own (empty) list, not the author's tiers.
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let other = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state.db.create_user(&other, "free", "carol").await.unwrap();
    state
        .db
        .create_subscription_tier(
            &author, "gold", 20, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();

    let author_reply = list_tiers(&router, state.clone(), author)
        .await
        .expect("author tiers.list ok");
    assert_eq!(author_reply.tiers.len(), 1);

    let other_reply = list_tiers(&router, state, other)
        .await
        .expect("other tiers.list ok");
    assert!(other_reply.tiers.is_empty());
}

#[tokio::test]
async fn offers_list_returns_another_authors_tiers_ordered_by_rank() {
    // The subscriber-browse read: a *different* actor reads the author's offered
    // tiers by request-supplied author_id (the own-read `tiers.list` cannot —
    // it's bearer-keyed). Tier definitions are public; this is the authenticated
    // WS-RPC successor to the public HTTP per-author read.
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let viewer = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state.db.create_user(&viewer, "free", "bob").await.unwrap();

    // Seed out of rank order to prove the read sorts ascending by rank.
    state
        .db
        .create_subscription_tier(
            &author,
            "gold",
            20,
            Some("Top tier"),
            Some("$10/mo"),
            Some("https://pay.example/gold"),
            false,
            None,
            None,
            false,
        )
        .await
        .unwrap();
    state
        .db
        .create_subscription_tier(
            &author,
            "followers",
            0,
            None,
            None,
            None,
            true,
            None,
            None,
            false,
        )
        .await
        .unwrap();

    let reply = list_offers(&router, state, viewer, author)
        .await
        .expect("offers.list ok");

    assert_eq!(reply.tiers.len(), 2);
    assert_eq!(reply.tiers[0].name, "followers");
    assert_eq!(reply.tiers[0].rank, 0);
    assert!(reply.tiers[0].auto_approve);
    assert_eq!(reply.tiers[1].name, "gold");
    assert_eq!(reply.tiers[1].rank, 20);
    assert_eq!(reply.tiers[1].price_hint.as_deref(), Some("$10/mo"));
    assert_eq!(
        reply.tiers[1].payment_url.as_deref(),
        Some("https://pay.example/gold")
    );
}

#[tokio::test]
async fn offers_list_empty_for_author_with_no_tiers() {
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let viewer = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state.db.create_user(&viewer, "free", "bob").await.unwrap();

    let reply = list_offers(&router, state, viewer, author)
        .await
        .expect("offers.list ok");
    assert!(reply.tiers.is_empty());
}

/// A `hidden` tier (`monetization.md` § The unifying model — *A tier may be
/// hidden*) rides the author's OWN read, flag set, and NO offer surface: the
/// authenticated `offers.list` and the unauthenticated HTTP read both omit it.
/// Filtering one and not the other would leave the enumeration open to any
/// browser — the same two-surface rule the per-post unlock tier already has.
#[tokio::test]
async fn a_hidden_tier_rides_only_the_authors_own_read() {
    let (router, state) = router_with_db().await;
    let author = ActorKeypair::generate().actor_id().0;
    let viewer = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user(&author, "free", "alice")
        .await
        .unwrap();
    state.db.create_user(&viewer, "free", "bob").await.unwrap();

    state
        .db
        .create_subscription_tier(
            &author, "gold", 1, None, None, None, false, None, None, false,
        )
        .await
        .unwrap();
    state
        .db
        .create_subscription_tier(
            &author,
            fauna_core::subscription::OWNER_ONLY_TIER,
            i64::from(fauna_core::subscription::OWNER_ONLY_TIER_RANK),
            None,
            None,
            None,
            false,
            None,
            None,
            true,
        )
        .await
        .unwrap();

    // Own read: both, the hidden one flagged, ascending rank.
    let own = list_tiers(&router, state.clone(), author)
        .await
        .expect("tiers.list ok");
    assert_eq!(
        own.tiers
            .iter()
            .map(|t| (t.name.as_str(), t.hidden))
            .collect::<Vec<_>>(),
        vec![("gold", false), ("only-me", true)]
    );
    assert_eq!(own.tiers[1].rank, u32::MAX);

    // Offer surface: the hidden tier is not there.
    let offers = list_offers(&router, state.clone(), viewer, author)
        .await
        .expect("offers.list ok");
    assert_eq!(
        offers
            .tiers
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["gold"]
    );

    // Unauthenticated HTTP twin: same exclusion.
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
    let listed: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(listed, vec!["gold"]);
}
