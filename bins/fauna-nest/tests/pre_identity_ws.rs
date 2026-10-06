//! Socket-level integration for the anonymous (pre-identity) WS connection —
//! `GET /api/v1/ws` (no bearer) + the `fauna.protocol.unauthenticated`
//! allowlist gate. Proves the end-to-end plumbing a real client hits: open
//! anonymous, run `fauna.auth.handshake`, get a minted bearer; an off-allowlist
//! kind is refused without tearing the connection down; the authenticated
//! endpoint still demands a bearer. Handler-level error mapping lives in
//! `conformance_auth.rs`. (Pre-identity slice; tracked internally.)

mod common;
use common::recv_reply;

use std::sync::Arc;

use ed25519_dalek::Signer;
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::auth::HandshakeReply;
use fauna_protocol::{
    Frame, Reply, Request, RpcError, Value, decode_strict, encode_canonical, encode_frame,
};

async fn start() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            // Discovery kinds ride the same anonymous connection — register them
            // so the end-to-end "allowlisted AND registered → real reply" path
            // is exercised over the socket (A2).
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            // Account registration rides the same anonymous connection (A3) —
            // register it so the "allowlisted AND registered → real reply" path
            // is exercised over the socket.
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            // One-time admin claim rides the same anonymous connection (A4).
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            // The in-band invite flow rides the same anonymous connection (A5).
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            // Register an authenticated content kind too, so the off-allowlist
            // test targets a *registered* (but non-pre-identity) kind — proving
            // the gate refuses even known kinds, not just unknown ones.
            fauna_nest::posts_handlers::register_posts_handlers(&mut b);
            b.build()
        }),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("ws://{addr}"), state)
}

fn to_value<T: serde::Serialize>(t: &T) -> Value {
    decode_strict(&encode_canonical(t).unwrap()).unwrap()
}

fn req_frame(kind: &str, corr: u64, key: u8, payload: Value) -> Message {
    let frame = Frame::Request(Request {
        ty: Request::TYPE,
        correlation_id: corr,
        kind: kind.to_string(),
        idempotency_key: [key; 16],
        payload,
        replay_forbidden: None,
        deadline_ms: None,
    });
    Message::Binary(encode_frame(&frame).unwrap())
}

fn reply_error(r: &Reply) -> RpcError {
    decode_strict(&encode_canonical(&r.payload).unwrap()).unwrap()
}

#[tokio::test]
async fn anonymous_handshake_mints_token() {
    let (base, state) = start().await;
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();

    let mut ws = common::open_anonymous(&base).await;
    ws.send(req_frame(
        "fauna.auth.handshake",
        1,
        1,
        to_value(&common::handshake_request(&kp, state.bound_identity())),
    ))
    .await
    .unwrap();

    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(reply.ok, "handshake should succeed: {:?}", reply.payload);
    let hs: HandshakeReply = decode_strict(&encode_canonical(&reply.payload).unwrap()).unwrap();
    assert!(!hs.token.is_empty());
    // The minted bearer is real — it validates and resolves to the actor.
    let resolved = state.auth.token_store.validate(&hs.token).await;
    assert_eq!(resolved.expect("token valid").0, kp.actor_id().0);
}

#[tokio::test]
async fn anonymous_discovery_nest_info_resolves() {
    // A discovery kind is allowlisted AND now registered — over the anonymous
    // connection it returns a real reply (not `unauthenticated`, not
    // `unknown_kind`), closing the A1 "allowlisted but unregistered" gap.
    use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};

    let (base, _state) = start().await;
    let mut ws = common::open_anonymous(&base).await;
    ws.send(req_frame(
        "fauna.nest.info",
        1,
        1,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();

    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(reply.ok, "nest.info should succeed: {:?}", reply.payload);
    let info: NestInfoReply = decode_strict(&encode_canonical(&reply.payload).unwrap()).unwrap();
    assert_eq!(info.software, "fauna");
    assert_eq!(info.nest_id.len(), 64);
}

#[tokio::test]
async fn anonymous_register_routes_to_handler() {
    // `fauna.account.register` is allowlisted AND now registered — over the
    // anonymous connection the gate passes and the handler runs, returning a
    // real account-policy reply (here `registration_closed`, since the
    // `for_test` default disables open registration), not `unauthenticated`
    // or `unknown_kind`. This closes the A3 "allowlisted but unregistered" gap.
    // The `open` check fires before any signature/timestamp validation, so the
    // payload's signature is irrelevant to this routing assertion.
    use fauna_protocol::account::RegisterRequest;

    let (base, _state) = start().await;
    let mut ws = common::open_anonymous(&base).await;
    let ts = fauna_core::data::Timestamp::now_millis();
    let req = RegisterRequest {
        actor_id: hex::encode(ActorKeypair::generate().actor_id().0),
        handle: "alice".into(),
        timestamp: ts,
        signature: hex::encode([0u8; 64]),
        invite_code: None,
        age_claim: None,
        extra: Default::default(),
    };
    ws.send(req_frame("fauna.account.register", 1, 1, to_value(&req)))
        .await
        .unwrap();

    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.account.registration_closed"
    );
}

#[tokio::test]
async fn anonymous_lockout_routes_to_handler_and_locks() {
    // `fauna.account.lockout` is allowlisted AND registered — over the anonymous
    // connection the gate passes and the handler runs end-to-end: a correctly
    // signed request locks the account and revokes its tokens, returning the
    // clamped `locked_until` (not `unauthenticated`/`unknown_kind`). This is the
    // no-token recovery channel — it must work without any bearer.
    use fauna_core::identity::ActorId;
    use fauna_protocol::account::{AccountLockoutReply, AccountLockoutRequest};

    let (base, state) = start().await;
    let kp = ActorKeypair::generate();
    let actor = kp.actor_id().0;
    // set_locked_until is an UPDATE — the user row must exist to persist.
    state.db.create_user(&actor, "free", "test").await.unwrap();
    state
        .auth
        .token_store
        .insert_with_metadata(ActorId(actor), 3600, None, None)
        .await;

    let ts = fauna_core::data::Timestamp::now_secs() as u64;
    let msg = fauna_protocol::account::account_lockout_signed_message(&actor, ts);
    let sig = kp.signing_key().sign(&msg);
    let req = AccountLockoutRequest {
        actor_id: hex::encode(actor),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        ..Default::default()
    };

    let mut ws = common::open_anonymous(&base).await;
    ws.send(req_frame("fauna.account.lockout", 1, 1, to_value(&req)))
        .await
        .unwrap();

    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(reply.ok, "lockout routed + succeeded over the anon socket");
    let body: AccountLockoutReply =
        decode_strict(&encode_canonical(&reply.payload).unwrap()).unwrap();
    assert!(body.ok);
    assert!(body.locked_until >= ts as i64 + 3600);
    // The account is actually locked + its tokens revoked.
    assert_eq!(
        state.db.get_locked_until(&actor).await.unwrap(),
        Some(body.locked_until)
    );
    assert_eq!(
        state
            .auth
            .token_store
            .list_sessions(&ActorId(actor))
            .await
            .len(),
        0
    );
}

#[tokio::test]
async fn anonymous_claim_admin_routes_to_handler() {
    // `fauna.auth.claim_admin` is allowlisted AND now registered — over the
    // anonymous connection the gate passes and the handler runs, returning a
    // real claim error (here `signature_failed`, since the zero signature fails
    // verification — which fires before the claim-code file is read), not
    // `unauthenticated` or `unknown_kind`. This closes the A4 "allowlisted but
    // unregistered" gap without depending on a claim-code file on disk.
    use fauna_protocol::claim::ClaimAdminRequest;

    let (base, _state) = start().await;
    let mut ws = common::open_anonymous(&base).await;
    let ts = fauna_core::data::Timestamp::now_millis();
    let req = ClaimAdminRequest {
        claim_code: "ABCDEF".into(),
        actor_id: hex::encode(ActorKeypair::generate().actor_id().0),
        timestamp: ts,
        signature: hex::encode([0u8; 64]),
        handle: "admin".into(),
        mail_domain: None,
        ..Default::default()
    };
    ws.send(req_frame("fauna.auth.claim_admin", 1, 1, to_value(&req)))
        .await
        .unwrap();

    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(!reply.ok);
    assert_eq!(reply_error(&reply).code, "fauna.auth.signature_failed");
}

#[tokio::test]
async fn anonymous_invite_status_routes_to_handler() {
    // `fauna.account.invite_request.status` is allowlisted AND registered — over
    // the anonymous connection the gate passes and the handler runs, returning a
    // real invite error (here `invite_request_not_found`, since no row exists for
    // the queried actor), not `unauthenticated` or `unknown_kind`. A pure read
    // needs no signature, so this is a clean A5 routing assertion.
    use fauna_protocol::invite::InviteRequestStatusQuery;

    let (base, _state) = start().await;
    let mut ws = common::open_anonymous(&base).await;
    let req = InviteRequestStatusQuery {
        actor_id: hex::encode(ActorKeypair::generate().actor_id().0),
        extra: Default::default(),
    };
    ws.send(req_frame(
        "fauna.account.invite_request.status",
        1,
        1,
        to_value(&req),
    ))
    .await
    .unwrap();

    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.account.invite_request_not_found"
    );
}

#[tokio::test]
async fn off_allowlist_kind_is_unauthenticated_and_connection_stays_open() {
    let (base, state) = start().await;
    let mut ws = common::open_anonymous(&base).await;

    // A registered authenticated kind is still refused on the anonymous conn.
    // (Payload is irrelevant — the gate fires before any handler decode.)
    ws.send(req_frame("fauna.posts.create", 1, 1, Value::Null))
        .await
        .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 1);
    assert!(!reply.ok);
    assert_eq!(reply_error(&reply).code, "fauna.protocol.unauthenticated");

    // The connection stayed open: a subsequent allowlisted request succeeds.
    // The actor needs a real `users` row — a handshake never provisions one (the
    // auto-provision branch is gone), so an unregistered actor would be refused
    // `not_registered` here and this would prove nothing about the connection.
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .unwrap();
    ws.send(req_frame(
        "fauna.auth.handshake",
        2,
        2,
        to_value(&common::handshake_request(&kp, state.bound_identity())),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 2);
    assert!(reply.ok, "handshake after rejection should succeed");
}

#[tokio::test]
async fn authenticated_endpoint_rejects_missing_bearer() {
    let (base, _state) = start().await;
    let actor_hex = hex::encode(ActorKeypair::generate().actor_id().0);
    let url = format!("{base}/api/v1/ws/{actor_hex}");
    let mut req = url.into_client_request().unwrap();
    // Only fauna.v1, no bearer — the authenticated endpoint must reject (401).
    req.headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, "fauna.v1".parse().unwrap());
    let result = tokio_tungstenite::connect_async(req).await;
    assert!(
        result.is_err(),
        "authenticated endpoint must reject a bearer-less upgrade"
    );
}
