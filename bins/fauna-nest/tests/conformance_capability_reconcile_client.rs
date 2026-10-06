//! Real-wire (tier_3) proof of `fauna.capabilities.reconcile` — the one
//! admissible owner-side nest read (`docs/goal/ui/nests.md` § Trust facet —
//! grants → *Reconcile*, ratified 2026-08-15).
//!
//! What only a real-wire test can catch (vs. the in-process
//! `db::capability_grants::tests` coverage of the DB read alone): the
//! `ReconcileGrantsRequest`/`Reply` CBOR encode/decode over the dispatcher,
//! the `bridge_method_allowlist` gate actually admitting `User` and refusing
//! a bridge/holder class, and that the handler is wired into the real
//! `fauna.capabilities.*` router group at all.
//!
//! Production data flow asserted end-to-end: a registered user deposits two
//! grants directly into `capability_grants` (standing in for two prior real
//! `fauna.capabilities.mint` deposits — one still live, one past its
//! `epoch_end`) → `fauna.capabilities.reconcile` over the real WS → the reply
//! names BOTH ids (expired included — reconcile is about rows, not
//! liveness) and NOTHING belonging to a second owner on the same nest.

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::wrapped_blob::{ReconcileGrantsReply, ReconcileGrantsRequest};

/// Spin a real in-process nest serving the auth-bootstrap + `fauna.capabilities.*`
/// WS-RPC kinds on `127.0.0.1:0`. Returns its HTTP base URL and the shared
/// `AppState` so the test can seed `state.db` directly (standing in for prior
/// real `fauna.capabilities.mint` deposits — the mint chain itself is proven
/// by `conformance_capability_trust_client.rs`; this test is about the
/// reconcile READ, not the mint write).
async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::bridge_blob_handlers::register_capability_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: tokens,
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..fauna_nest::routes::AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (format!("http://127.0.0.1:{}", addr.port()), state)
}

#[tokio::test]
async fn reconcile_returns_every_owned_id_expired_included_owner_scoped() {
    let (base, state) = start_server().await;

    let owner = ActorKeypair::generate();
    let owner_id = owner.actor_id();
    state
        .db
        .create_user(&owner_id.0, "free", "owner")
        .await
        .unwrap();

    // A second registered actor whose grants must never leak into owner's
    // reconcile reply — the owner-scoping half of the contract.
    let other = ActorKeypair::generate();
    let other_id = other.actor_id();
    state
        .db
        .create_user(&other_id.0, "free", "other")
        .await
        .unwrap();

    let holder = [0x42u8; 32];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let live_id = [1u8; 16];
    let expired_id = [2u8; 16];
    let other_id_bytes = [3u8; 16];

    state
        .db
        .put_capability_grant(&owner_id.0, &live_id, &holder, now + 3600, b"live")
        .await
        .unwrap();
    state
        .db
        .put_capability_grant(&owner_id.0, &expired_id, &holder, now - 3600, b"expired")
        .await
        .unwrap();
    state
        .db
        .put_capability_grant(&other_id.0, &other_id_bytes, &holder, now + 3600, b"other")
        .await
        .unwrap();

    let nest = connected_client(&base, owner).await;
    let reply: ReconcileGrantsReply = nest
        .request(
            "fauna.capabilities.reconcile",
            ReconcileGrantsRequest::default(),
        )
        .await
        .expect("reconcile over the real wire");

    let mut ids: Vec<Vec<u8>> = reply.grant_ids.into_iter().map(|b| b.into_vec()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![live_id.to_vec(), expired_id.to_vec()],
        "reconcile must return every owned id — expired included — and nothing \
         belonging to another owner"
    );
}
