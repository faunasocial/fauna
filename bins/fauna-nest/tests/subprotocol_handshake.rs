//! Integration test — WebSocket upgrade handshake error paths.
//! Spec Y plan 3, Task 23.

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

/// Spin up a test nest with two registered actors. Returns base URL +
/// (alice keypair, alice token, bob keypair, bob token).
async fn start() -> (String, ActorKeypair, String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let alice = ActorKeypair::generate();
    let bob = ActorKeypair::generate();
    db.create_user(&alice.actor_id().0, "free", "alice")
        .await
        .ok();
    db.create_user(&bob.actor_id().0, "free", "bob").await.ok();
    let alice_token = tokens.insert(alice.actor_id(), 3600).await;
    let bob_token = tokens.insert(bob.actor_id(), 3600).await;

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new(fauna_nest::rpc_router::RpcRouter::builder().build()),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (
        format!("ws://127.0.0.1:{}", addr.port()),
        alice,
        alice_token,
        bob,
        bob_token,
    )
}

#[tokio::test]
async fn happy_path_with_subprotocol_bearer_succeeds() {
    let (base, alice, token, _bob, _bob_token) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    let (_ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 101);
}

#[tokio::test]
async fn missing_subprotocol_returns_unauthorized() {
    let (base, alice, _token, _, _) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let req = url.into_client_request().unwrap();
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(s.contains("401") || s.contains("unauthorized"), "got: {s}");
}

#[tokio::test]
async fn wrong_subprotocol_returns_upgrade_required() {
    let (base, alice, token, _, _) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v0, bearer.{token}").parse().unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(
        s.contains("426") || s.contains("upgrade") || s.contains("subprotocol"),
        "got: {s}"
    );
}

#[tokio::test]
async fn bearer_for_other_actor_returns_forbidden() {
    let (base, alice, _alice_token, _bob, bob_token) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{bob_token}").parse().unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    let s = err.to_string().to_lowercase();
    assert!(s.contains("403") || s.contains("forbidden"), "got: {s}");
}
