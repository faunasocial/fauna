//! Integration round-trip for the push-subscription-management surface —
//! `fauna.push.{vapid_key,subscribe,unsubscribe,presence}`. A behavior-preserving
//! transport migration of the three push-management HTTP routes
//! (`push_routes::{get_vapid_key, subscribe, unsubscribe}`). The handlers reuse
//! the same `CacheDb` push methods + `PushService` the twins call; these tests
//! exercise the WS-RPC layer: request decode, the reply shapes, the
//! `User | Admin` allowlist, the `unavailable` (no push service) / `malformed` /
//! `invalid_request` mappings, and replay metadata.
//!
//! `AppState::for_test` installs `push_service = None`, so `vapid_key` exercises
//! the `fauna.push.unavailable` path (the twin's 404), while `subscribe` /
//! `unsubscribe` (which only touch `CacheDb`) run fully.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/push.rs`.
//! Slice: tracked internally (Track B22 of the WS-RPC-everywhere
//! migration).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    push_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode,
    push::{
        PresenceRequest, SubscribeReply, SubscribeRequest, UnsubscribeReply, UnsubscribeRequest,
        VapidKeyRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    push_handlers::register_push_handlers(&mut b);
    (b.build(), state)
}

// ── subscribe / unsubscribe (touch CacheDb only — a plain User actor is OK) ─

#[tokio::test]
async fn subscribe_then_unsubscribe_ok() {
    let (router, state) = router_and_state().await;
    let actor = [11u8; 32];

    let reply: SubscribeReply = decode(
        &dispatch(
            &router,
            Arc::clone(&state),
            actor,
            "fauna.push.subscribe",
            encode(&SubscribeRequest {
                device_id: "dev-1".into(),
                endpoint: "https://push.example.com/abc".into(),
                key_p256dh: Some("p256dh".into()),
                key_auth: Some("auth".into()),
                transport: Some("web-push".into()),
                extra: Default::default(),
            }),
        )
        .await
        .expect("subscribe ok"),
    )
    .unwrap();
    assert!(reply.ok);

    let reply: UnsubscribeReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.push.unsubscribe",
            encode(&UnsubscribeRequest {
                device_id: "dev-1".into(),
                extra: Default::default(),
            }),
        )
        .await
        .expect("unsubscribe ok"),
    )
    .unwrap();
    assert!(reply.ok);
}

#[tokio::test]
async fn subscribe_rejects_empty_device_id() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [12u8; 32],
        "fauna.push.subscribe",
        encode(&SubscribeRequest {
            device_id: String::new(),
            endpoint: "https://push.example.com/abc".into(),
            key_p256dh: None,
            key_auth: None,
            transport: None,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("empty device_id rejected");
    assert_eq!(err.code, "fauna.push.invalid_request");
}

#[tokio::test]
async fn subscribe_rejects_unknown_transport() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [13u8; 32],
        "fauna.push.subscribe",
        encode(&SubscribeRequest {
            device_id: "dev-x".into(),
            endpoint: "https://push.example.com/abc".into(),
            key_p256dh: None,
            key_auth: None,
            transport: Some("carrier-pigeon".into()),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("unknown transport rejected");
    assert_eq!(err.code, "fauna.push.invalid_request");
}

// ── ws-device (ruled 2026-09-26) ────────────────────────────────────

fn ws_device(device_id: &str, endpoint: &str, key_auth: Option<&str>) -> Bytes {
    encode(&SubscribeRequest {
        device_id: device_id.into(),
        endpoint: endpoint.into(),
        key_p256dh: None,
        key_auth: key_auth.map(Into::into),
        transport: Some("ws-device".into()),
        extra: Default::default(),
    })
}

/// A `ws-device` row names its own device and carries no keys
/// (`apps/common.md` § Registration); it upserts under the same
/// `(actor_id, device_id)` key as every other transport.
#[tokio::test]
async fn subscribe_accepts_a_ws_device_row() {
    let (router, state) = router_and_state().await;
    let actor = [16u8; 32];
    for _ in 0..2 {
        let reply: SubscribeReply = decode(
            &dispatch(
                &router,
                Arc::clone(&state),
                actor,
                "fauna.push.subscribe",
                ws_device("desk", "desk", None),
            )
            .await
            .expect("ws-device subscribe ok"),
        )
        .unwrap();
        assert!(reply.ok);
    }
    let rows = state.db.list_push_subscriptions(&actor).await.unwrap();
    assert_eq!(rows.len(), 1, "a re-subscribe upserts the one row");
    assert_eq!(
        (rows[0].transport.as_str(), rows[0].endpoint.as_str()),
        ("ws-device", "desk")
    );
    assert!(rows[0].key_p256dh.is_none() && rows[0].key_auth.is_none());
}

#[tokio::test]
async fn subscribe_refuses_a_misshapen_ws_device_row() {
    let (router, state) = router_and_state().await;
    for (endpoint, key_auth) in [
        ("https://push.example.com/abc", None),
        ("desk", Some("auth")),
    ] {
        let err = dispatch(
            &router,
            Arc::clone(&state),
            [17u8; 32],
            "fauna.push.subscribe",
            ws_device("desk", endpoint, key_auth),
        )
        .await
        .expect_err("misshapen ws-device row refused");
        assert_eq!(err.code, "fauna.push.invalid_request");
        let reason = format!("{:?}", err.details);
        assert!(
            reason.contains("own device_id"),
            "the refusal names the accepted shape: {reason}"
        );
    }
}

#[tokio::test]
async fn presence_rejects_empty_device_id() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [18u8; 32],
        "fauna.push.presence",
        encode(&PresenceRequest {
            device_id: String::new(),
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("empty device_id rejected");
    assert_eq!(err.code, "fauna.push.invalid_request");
}

// ── vapid_key without a push service → unavailable ──────────────────

#[tokio::test]
async fn vapid_key_without_push_service_is_unavailable() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [14u8; 32],
        "fauna.push.vapid_key",
        encode(&VapidKeyRequest {
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("no push service → unavailable");
    assert_eq!(err.code, "fauna.push.unavailable");
}

// ── malformed payload ───────────────────────────────────────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [15u8; 32],
        "fauna.push.subscribe",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata ─────────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    for kind in [
        "fauna.push.vapid_key",
        "fauna.push.subscribe",
        "fauna.push.unsubscribe",
        "fauna.push.presence",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

// ── allowlist ───────────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for kind in [
        "fauna.push.vapid_key",
        "fauna.push.subscribe",
        "fauna.push.unsubscribe",
        "fauna.push.presence",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}
