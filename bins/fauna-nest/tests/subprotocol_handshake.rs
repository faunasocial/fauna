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

/// Read until a Close frame arrives and return its code; `None` when the
/// stream ends (or errors) with no close frame — the pre-4426 behavior, so a
/// `None` is a real failure, not a flake. Same shape as `ws_close_codes.rs`.
async fn close_code<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Option<u16>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Err(_) | Ok(None) | Ok(Some(Err(_))) => return None,
            Ok(Some(Ok(Message::Close(frame)))) => return frame.map(|f| u16::from(f.code)),
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Assert `err` is the pre-upgrade HTTP refusal with `status`.
fn assert_http_refusal(err: tokio_tungstenite::tungstenite::Error, status: u16) {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status().as_u16(), status)
        }
        other => panic!("expected an HTTP {status} refusal, got: {other}"),
    }
}

/// An app offering a Fauna client subprotocol the nest does not serve gets the
/// upgrade COMPLETED, selecting its own token, and then a 4426 close — the one
/// refusal a browser can read (`transport-connection.md` § Connection
/// lifecycle, the 4426 paragraph).
#[tokio::test]
async fn unserved_fauna_subprotocol_completes_the_upgrade_then_closes_4426() {
    let (base, alice, token, _, _) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v0, bearer.{token}").parse().unwrap(),
    );
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 101);
    assert_eq!(
        resp.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
        "fauna.v0",
        "the nest must select the token the app offered, or the app fails the connection"
    );
    assert_eq!(close_code(&mut ws).await, Some(4426));
}

/// The 4426 is answered before the bearer is consulted: a too-old app learns
/// it is too old whatever the state of its session.
#[tokio::test]
async fn unserved_fauna_subprotocol_closes_4426_before_the_bearer_is_checked() {
    let (base, alice, _token, _, _) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        "fauna.v0, bearer.not-a-real-token".parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(close_code(&mut ws).await, Some(4426));
}

/// No Fauna client subprotocol at all keeps the pre-upgrade HTTP 426.
#[tokio::test]
async fn non_fauna_subprotocol_returns_upgrade_required() {
    let (base, alice, token, _, _) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(alice.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("nope.v1, bearer.{token}").parse().unwrap(),
    );
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    assert_http_refusal(err, 426);
}

/// The anonymous endpoint's twin: an unserved `fauna.v<N>` → 101 then 4426.
#[tokio::test]
async fn anonymous_unserved_fauna_subprotocol_closes_4426() {
    let (base, _, _, _, _) = start().await;
    let mut req = format!("{base}/api/v1/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, "fauna.v0".parse().unwrap());
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status().as_u16(), 101);
    assert_eq!(
        resp.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
        "fauna.v0"
    );
    assert_eq!(close_code(&mut ws).await, Some(4426));
}

/// The anonymous endpoint keeps the 426 for an offer with no Fauna token.
#[tokio::test]
async fn anonymous_non_fauna_subprotocol_returns_upgrade_required() {
    let (base, _, _, _, _) = start().await;
    let mut req = format!("{base}/api/v1/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, "nope.v1".parse().unwrap());
    let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
    assert_http_refusal(err, 426);
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
