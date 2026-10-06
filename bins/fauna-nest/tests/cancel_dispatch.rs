//! Integration test — Cancel aborts an in-flight handler. Spec Y § 1.4.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::rpc_router::{RpcKindMeta, RpcRouter};
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{
    Cancel, Frame, Reply, Request, RpcError, Value, decode_frame, decode_strict as decode,
    encode_canonical, encode_frame,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

/// A real, User-class kind (see the note in `start`) — a synthetic one is refused
/// by the central capability gate before the handler runs.
const SLOW_KIND: &str = "fauna.contacts.list";

async fn start() -> (String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    // Register a slow handler that sleeps 5 seconds.
    //
    // It is registered under a **real, User-class kind** rather than a synthetic
    // `fauna.test.slow`. The dispatcher gates every kind through the central
    // capability allowlist (`bridge_method_allowlist::is_permitted`), whose
    // fall-through arm is `_ => false` — so a made-up kind is refused
    // `fauna.bridges.permission_denied` *before* the handler ever runs, and this
    // test then asserts `cancelled` against a denial and fails. (It did: the
    // synthetic kind silently stopped working when the central gate landed, and
    // nothing went red because no CI job builds the nest integration binaries.)
    // Only the *routing* matters here — the body is ours either way.
    let mut b = RpcRouter::builder();
    b.add(
        SLOW_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(60),
            handler: Box::new(|_, _, _| {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok::<Bytes, RpcError>(Bytes::from_static(b"never"))
                })
            }),
        },
    );
    let router = Arc::new(b.build());
    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: router,
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
    (format!("ws://{addr}"), kp, token)
}

fn make_request(corr: u64, kind: &str) -> Bytes {
    let mut idem = [0u8; 16];
    getrandom::fill(&mut idem).unwrap();
    encode_frame(&Frame::Request(Request {
        ty: Request::TYPE,
        correlation_id: corr,
        kind: kind.into(),
        idempotency_key: idem,
        payload: Value::Null,
        replay_forbidden: None,
        deadline_ms: None,
    }))
    .unwrap()
}

#[tokio::test]
async fn cancel_aborts_in_flight_handler() {
    let (base, kp, token) = start().await;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(kp.actor_id().0));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Issue a slow Request.
    ws.send(Message::Binary(make_request(7, SLOW_KIND)))
        .await
        .unwrap();

    // Beat to let the handler start.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel correlation_id 7.
    let cancel_bytes = encode_frame(&Frame::Cancel(Cancel {
        ty: Cancel::TYPE,
        correlation_id: 7,
    }))
    .unwrap();
    ws.send(Message::Binary(cancel_bytes)).await.unwrap();

    // Read the Reply (must arrive in under 1s; handler would otherwise sleep 5s).
    let msg = tokio::time::timeout(Duration::from_secs(1), ws.next())
        .await
        .expect("cancelled reply within timeout")
        .expect("ws stream open")
        .expect("ws read ok");
    let bytes = match msg {
        Message::Binary(b) => b,
        other => panic!("expected binary, got: {other:?}"),
    };
    let frame = decode_frame(&bytes).unwrap();
    let reply: Reply = match frame {
        Frame::Reply(r) => r,
        other => panic!("expected Reply, got: {other:?}"),
    };
    assert_eq!(reply.correlation_id, 7);
    assert!(!reply.ok);
    let err_bytes = encode_canonical(&reply.payload).unwrap();
    let err: RpcError = decode(&err_bytes).unwrap();
    assert_eq!(err.code, "fauna.protocol.cancelled");
}
