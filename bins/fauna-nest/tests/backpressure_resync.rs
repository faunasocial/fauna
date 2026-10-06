//! Integration test — backpressure-induced ResyncRequired emit.
//! Spec Y § 1.6.

use std::sync::Arc;
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::push_events::{KnockPayload, ResyncRequiredPayload};
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
async fn drops_trigger_resync_required_on_next_emit() {
    let (base, kp, token, state) = start().await;
    let actor = kp.actor_id().0;
    let url = format!("{base}/api/v1/ws/{}", hex::encode(actor));
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        format!("fauna.v1, bearer.{token}").parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    // Wait briefly for the server-side subscription to land.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Saturate: 256 (channel cap) + some extra to ensure drops occur.
    // The bounded channel holds WS_OUTBOUND_BOUND = 256 frames; the rest are dropped.
    for i in 0..400u32 {
        state.ws.notify_push(
            &actor,
            PushEvent::Knock(KnockPayload {
                sender_id: format!("s{i}"),
                summary: "x".into(),
                ..Default::default()
            }),
        );
    }

    // Drain a batch of frames first so that the channel has room, then send
    // one more push. That push will succeed and trigger flush_resync_if_needed
    // (the on-emit fold in notify_push), which queues the ResyncRequired frame.
    let mut drained = 0u32;
    for _ in 0..300u32 {
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .ok()
            .flatten()
            .and_then(|r| r.ok());
        let Some(Message::Binary(_)) = msg else {
            break;
        };
        drained += 1;
    }
    assert!(
        drained > 0,
        "should have received some frames before trigger push"
    );

    // One more push — this succeeds (channel now has room) and triggers the
    // flush_resync_if_needed path inside notify_push.
    state.ws.notify_push(
        &actor,
        PushEvent::Knock(KnockPayload {
            sender_id: "trigger".into(),
            summary: "x".into(),
            ..Default::default()
        }),
    );

    // Continue draining. Look for the resync frame.
    let mut found = false;
    for _ in 0..400u32 {
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .ok()
            .flatten()
            .and_then(|r| r.ok());
        let Some(Message::Binary(bytes)) = msg else {
            break;
        };
        if let Ok(Frame::Push(p)) = decode_frame(&bytes)
            && p.kind == "fauna.protocol.resync_required"
        {
            let pb = encode_canonical(&p.payload).unwrap();
            let typed: ResyncRequiredPayload = decode(&pb).unwrap();
            assert!(typed.dropped_count > 0, "dropped_count should be > 0");
            found = true;
            break;
        }
    }
    assert!(
        found,
        "expected at least one fauna.protocol.resync_required frame"
    );
}
