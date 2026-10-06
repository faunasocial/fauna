//! Integration test — fauna.protocol.echo round-trip over a real
//! WebSocket. Per Spec Y plan 3, Task 22.

use std::sync::Arc;
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{
    EchoReply, EchoRequest, Frame, Reply, Request, Value, decode_frame, decode_strict as decode,
    encode_canonical, encode_frame,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

async fn start_nest() -> (String, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();

    // Register the actor so require_registration passes.
    db.create_user(&actor_id.0, "free", "test").await.unwrap();

    // Mint a bearer token for this actor (TTL = 1 hour).
    let token = tokens.insert(actor_id, 3600).await;

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::protocol_test::register_protocol_handlers(&mut b);
            b.build()
        }),
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
    let actor_hex = hex::encode(actor_id.0);
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (
        format!("ws://127.0.0.1:{}/api/v1/ws/{actor_hex}", addr.port()),
        token,
    )
}

fn make_request(corr: u64, kind: &str, payload: Value) -> bytes::Bytes {
    let mut idempotency_key = [0u8; 16];
    getrandom::fill(&mut idempotency_key).unwrap();
    let req = Request {
        ty: Request::TYPE,
        correlation_id: corr,
        kind: kind.to_string(),
        idempotency_key,
        payload,
        replay_forbidden: None,
        deadline_ms: None,
    };
    encode_frame(&Frame::Request(req)).unwrap()
}

#[tokio::test]
async fn echo_round_trip_via_real_websocket() {
    let (ws_url, token) = start_nest().await;

    let mut request = ws_url.into_client_request().unwrap();
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );

    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("WS upgrade with subprotocol");

    // Build the echo payload.
    let req = EchoRequest {
        data: vec![1, 2, 3],
        extra: std::collections::BTreeMap::new(),
    };
    let req_bytes = encode_canonical(&req).unwrap();
    let payload: Value = decode(&req_bytes).unwrap();
    let frame_bytes = make_request(1, "fauna.protocol.echo", payload);
    ws.send(Message::Binary(frame_bytes)).await.unwrap();

    // Receive the Reply frame.
    let reply_msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("reply within 5s timeout")
        .expect("ws stream still open")
        .expect("ws message read ok");

    let reply_bytes = match reply_msg {
        Message::Binary(b) => b,
        other => panic!("expected binary reply frame, got: {other:?}"),
    };

    let frame = decode_frame(&reply_bytes).expect("decode reply frame");
    let reply: Reply = match frame {
        Frame::Reply(r) => r,
        other => panic!("expected Reply frame, got: {other:?}"),
    };

    assert_eq!(reply.correlation_id, 1, "correlation_id must match");
    assert!(reply.ok, "reply.ok must be true");

    let reply_payload_bytes = encode_canonical(&reply.payload).unwrap();
    let echo_reply: EchoReply = decode(&reply_payload_bytes).unwrap();
    assert_eq!(echo_reply.data, vec![1, 2, 3], "echo data must round-trip");
}
