//! Integration test — typed Push frames arrive at the client over the WS.
//! Spec Y plan 3, Task 24.

use std::sync::Arc;
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::push_events::AccountUpdatedPayload;
use fauna_protocol::{Frame, PushEvent, decode_frame, decode_strict as decode, encode_canonical};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

async fn start() -> (
    String,
    ActorKeypair,
    String,
    Arc<fauna_nest::routes::AppState>,
) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new(fauna_nest::rpc_router::RpcRouter::builder().build()),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("ws://{addr}"), kp, token, state)
}

#[tokio::test]
async fn account_updated_push_arrives_typed() {
    let (base, kp, token, state) = start().await;
    let actor_id = kp.actor_id().0;
    let actor_hex = hex::encode(actor_id);
    let url = format!("{base}/api/v1/ws/{actor_hex}");

    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Give the server a beat to subscribe — subscription happens server-side
    // after the upgrade callback runs and may race with notify_push.
    tokio::time::sleep(Duration::from_millis(50)).await;

    state.ws.notify_push(
        &actor_id,
        PushEvent::AccountUpdated(AccountUpdatedPayload {
            changes: vec!["handle".into()],
            timestamp: 1710000000,
            extra: std::collections::BTreeMap::new(),
        }),
    );

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("push within timeout")
        .expect("ws stream open")
        .expect("ws read ok");
    let bytes = match msg {
        Message::Binary(b) => b,
        other => panic!("expected binary, got: {other:?}"),
    };
    let frame = decode_frame(&bytes).unwrap();
    let push = match frame {
        Frame::Push(p) => p,
        other => panic!("expected Push, got: {other:?}"),
    };
    assert_eq!(push.kind, "fauna.account.update");
    assert_eq!(push.seq, 1);
    let payload_bytes = encode_canonical(&push.payload).unwrap();
    let typed: AccountUpdatedPayload = decode(&payload_bytes).unwrap();
    assert_eq!(typed.changes, vec!["handle".to_string()]);
    assert_eq!(typed.timestamp, 1710000000);
}
