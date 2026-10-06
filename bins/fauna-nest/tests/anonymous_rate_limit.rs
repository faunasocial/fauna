//! tier_3 socket-level integration for the anonymous (pre-identity) discovery
//! surface rate limit. Drives the real public WS path — served with
//! `into_make_service_with_connect_info` so each connection carries a
//! `ConnectInfo` peer (exactly as the production TLS path does via
//! `WithConnectInfo`) — and proves the per-source sliding-window limiter trips
//! on a throttled discovery kind, returns `fauna.protocol.rate_limited`, leaves
//! the connection open, and does NOT throttle the non-discovery pre-identity
//! kinds (auth bootstrap). Module-level keying/masking logic is unit-tested in
//! `src/anonymous_rate_limit.rs`.

mod common;
use common::recv_reply;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::Signer;
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

use fauna_core::identity::ActorKeypair;
use fauna_nest::bridge_rate_limit::{Limiter, LimiterConfig};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::account::{AccountLockoutRequest, RegisterRequest};
use fauna_protocol::auth::HandshakeReply;
use fauna_protocol::claim::ClaimAdminRequest;
use fauna_protocol::discovery::NestInfoRequest;
use fauna_protocol::invite::{InviteCodeVerify, InviteRequestSubmit};
use fauna_protocol::{
    Frame, Reply, Request, RpcError, Value, decode_strict, encode_canonical, encode_frame,
};

/// Window the test trips after 3 events per source/kind (vs the generous prod
/// default) so we don't have to fire 60 requests over a socket.
const TEST_MAX_EVENTS: u32 = 3;

async fn start() -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        // Tiny window so the limiter trips after a few requests.
        anonymous_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: TEST_MAX_EVENTS,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // `into_make_service_with_connect_info` mirrors the production listener so
    // the anonymous connection carries a real peer addr (here loopback) — the
    // source the limiter keys on.
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
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
        // Distinct idempotency_key per request — a repeated key would be served
        // from the per-connection idempotency cache (gate 1a) before ever
        // reaching the rate-limit gate (1b′).
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

/// A signed `claim_admin` payload with an arbitrary code. The code need not be
/// valid: the rate-limit gate (1b″) fires in the dispatcher *before* the claim
/// handler runs, so every attempt — valid code, wrong code, or unregistered
/// kind — counts against the per-source claim budget. We only assert on the
/// throttle outcome, never on the claim result.
fn claim_request(kp: &ActorKeypair, code: &str) -> ClaimAdminRequest {
    let ts = fauna_core::data::Timestamp::now_millis();
    let mut msg = Vec::with_capacity(40);
    msg.extend_from_slice(&kp.actor_id().0);
    msg.extend_from_slice(&ts.to_be_bytes());
    let sig = kp.signing_key().sign(&msg);
    ClaimAdminRequest {
        claim_code: code.to_string(),
        actor_id: hex::encode(kp.actor_id().0),
        timestamp: ts,
        signature: hex::encode(sig.to_bytes()),
        handle: "admin".into(),
        mail_domain: None,
        ..Default::default()
    }
}

#[tokio::test]
async fn anonymous_discovery_surface_is_rate_limited_per_source() {
    let (base, _state) = start().await;
    let mut ws = common::open_anonymous(&base).await;

    // The first TEST_MAX_EVENTS discovery calls succeed.
    for i in 0..TEST_MAX_EVENTS {
        ws.send(req_frame(
            "fauna.nest.info",
            i as u64,
            i as u8,
            to_value(&NestInfoRequest::default()),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert!(
            reply.ok,
            "discovery call {i} should succeed before the limit: {:?}",
            reply.payload
        );
    }

    // The next one from the same source (same loopback IP, this same
    // connection) trips the limiter.
    ws.send(req_frame(
        "fauna.nest.info",
        99,
        99,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok, "the over-limit call must be refused");
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit discovery call returns the throttle error"
    );

    // The connection stays open — a non-throttled pre-identity kind (auth
    // bootstrap) on the SAME connection still works after the discovery bucket
    // is exhausted (separate concern: throttling discovery must not lock an
    // actor out of bootstrap).
    let kp = ActorKeypair::generate();
    _state
        .db
        .create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();
    ws.send(req_frame(
        "fauna.auth.handshake",
        100,
        100,
        to_value(&common::handshake_request(&kp, _state.bound_identity())),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 100);
    assert!(
        reply.ok,
        "auth bootstrap must not be throttled by the discovery limiter: {:?}",
        reply.payload
    );
    let hs: HandshakeReply = decode_strict(&encode_canonical(&reply.payload).unwrap()).unwrap();
    assert!(!hs.token.is_empty());
}

/// Harness with a tiny `claim_rate_limit` (trips after `CLAIM_MAX`) and the claim
/// handler registered, so over-limit `claim_admin` attempts are throttled by the
/// dispatcher before the handler. No claim-code file is provisioned — attempts
/// under the limit simply fail the claim (a non-throttle error), which is all we
/// assert is *not* `rate_limited`.
const CLAIM_MAX: u32 = 2;

async fn start_claim() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        claim_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: CLAIM_MAX,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    format!("ws://{addr}")
}

#[tokio::test]
async fn admin_claim_attempts_are_rate_limited_per_source() {
    let base = start_claim().await;
    let mut ws = common::open_anonymous(&base).await;
    let kp = ActorKeypair::generate();

    // The first CLAIM_MAX attempts pass the throttle gate (and then fail the
    // claim for some non-throttle reason — wrong/absent code).
    for i in 0..CLAIM_MAX {
        ws.send(req_frame(
            "fauna.auth.claim_admin",
            i as u64,
            i as u8,
            to_value(&claim_request(&kp, "AAAAAA")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "claim attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same source trips the claim limiter.
    ws.send(req_frame(
        "fauna.auth.claim_admin",
        99,
        99,
        to_value(&claim_request(&kp, "AAAAAA")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit admin-claim attempt is throttled"
    );

    // The discovery limiter is a separate budget — nest.info still resolves on
    // the same connection after the claim budget is exhausted.
    ws.send(req_frame(
        "fauna.nest.info",
        100,
        100,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 100);
    assert!(
        reply.ok,
        "discovery must not share the claim budget: {:?}",
        reply.payload
    );
}

/// Authenticated twin of `start_claim` — same tiny `claim_rate_limit`, plus a
/// token store and a registered/authenticated actor so the bearer WS upgrade
/// (`/api/v1/ws/{actor}`) is reachable. This actor is the CALLING connection,
/// not the claimant `claim_request` names — the throttle keys on the calling
/// connection (`check_conn` → `actor_id`), unrelated to the claim payload.
async fn start_claim_authenticated() -> (String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(fauna_nest::token_store::TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        claim_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: CLAIM_MAX,
        })),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    (format!("ws://{addr}"), kp, token)
}

/// The authenticated twin of `admin_claim_attempts_are_rate_limited_per_source`
/// — `claim_admin`'s per-source cap is the primary bound licensing the 40-bit
/// claim code (`federation.md` § Security), so an authenticated connection
/// must not convert it into an unbounded surface. Uses the per-source budget
/// (`start_claim_authenticated`, not the global-cap harness) so the per-actor
/// cap is the one under test, mirroring the anonymous case it twins.
#[tokio::test]
async fn admin_claim_attempts_are_rate_limited_on_an_authenticated_connection() {
    let (base, kp, token) = start_claim_authenticated().await;
    let mut ws = common::open_authed(&base, &kp, &token).await;
    let claimant = ActorKeypair::generate();

    // The first CLAIM_MAX attempts pass the throttle gate (and then fail the
    // claim for some non-throttle reason — wrong/absent code).
    for i in 0..CLAIM_MAX {
        ws.send(req_frame(
            "fauna.auth.claim_admin",
            i as u64,
            i as u8,
            to_value(&claim_request(&claimant, "AAAAAA")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "authenticated claim attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same (authenticated) connection trips the
    // claim limiter — keyed on this connection's actor.
    ws.send(req_frame(
        "fauna.auth.claim_admin",
        99,
        99,
        to_value(&claim_request(&claimant, "AAAAAA")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(
        !reply.ok,
        "the over-limit AUTHENTICATED claim_admin call must be refused — an \
         account must not convert claim_admin's per-source cap into unbounded"
    );
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit authenticated claim_admin call returns the throttle error"
    );
}

/// Harness that exercises the **global** claim cap end-to-end: a generous
/// per-source `claim_rate_limit` (so the per-source gate never trips first) and
/// a tiny `global_claim_rate_limit`. Because every loopback connection shares
/// one source IP, the only way to prove the source-independent global cap from a
/// socket is to make it the binding limiter — here it trips after `GLOBAL_MAX`
/// across the connection while the per-source budget is far from exhausted.
const GLOBAL_MAX: u32 = 2;

async fn start_global_claim() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::claim_handlers::register_claim_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        // Generous per-source budget so the per-source gate is NOT what trips ...
        claim_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: 100,
        })),
        // ... the tiny global cap is the binding limiter.
        global_claim_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: GLOBAL_MAX,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    format!("ws://{addr}")
}

#[tokio::test]
async fn admin_claim_attempts_are_rate_limited_globally() {
    let base = start_global_claim().await;
    let mut ws = common::open_anonymous(&base).await;
    let kp = ActorKeypair::generate();

    // The first GLOBAL_MAX attempts pass the (generous) per-source gate AND the
    // (tiny) global cap, reaching the handler (which then fails the claim for a
    // non-throttle reason — no claim-code file).
    for i in 0..GLOBAL_MAX {
        ws.send(req_frame(
            "fauna.auth.claim_admin",
            i as u64,
            i as u8,
            to_value(&claim_request(&kp, "AAAAAA")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "claim attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt trips the GLOBAL cap even though this source is nowhere
    // near its own per-source budget — proving the source-independent bound is
    // wired into the dispatcher.
    ws.send(req_frame(
        "fauna.auth.claim_admin",
        99,
        99,
        to_value(&claim_request(&kp, "AAAAAA")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-global-cap admin-claim attempt is throttled"
    );
}

/// Harness with a tiny `invite_verify_rate_limit` and the invite handlers
/// registered, so over-limit `invite_code.verify` attempts are throttled by the
/// dispatcher (gate 1b‴) before the handler. No invite code is provisioned —
/// attempts under the limit simply fail verification (a non-throttle error),
/// which is all we assert is *not* `rate_limited`.
const INVITE_MAX: u32 = 2;

async fn start_invite() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        invite_verify_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: INVITE_MAX,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    format!("ws://{addr}")
}

fn invite_verify(code: &str) -> InviteCodeVerify {
    InviteCodeVerify {
        code: code.to_string(),
        extra: Default::default(),
    }
}

#[tokio::test]
async fn invite_code_verify_is_rate_limited_per_source() {
    let base = start_invite().await;
    let mut ws = common::open_anonymous(&base).await;

    // The first INVITE_MAX attempts pass the throttle gate (then fail
    // verification for a non-throttle reason — the code is not registered).
    for i in 0..INVITE_MAX {
        ws.send(req_frame(
            "fauna.account.invite_code.verify",
            i as u64,
            i as u8,
            to_value(&invite_verify("NOPECODE99")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "verify attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same source trips the invite limiter.
    ws.send(req_frame(
        "fauna.account.invite_code.verify",
        99,
        99,
        to_value(&invite_verify("NOPECODE99")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit invite-verify attempt is throttled"
    );

    // The discovery limiter is a separate budget — nest.info still resolves on
    // the same connection after the invite budget is exhausted.
    ws.send(req_frame(
        "fauna.nest.info",
        100,
        100,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 100);
    assert!(
        reply.ok,
        "discovery must not share the invite budget: {:?}",
        reply.payload
    );
}

/// Authenticated twin of `start_invite` — same tiny `invite_verify_rate_limit`,
/// plus a token store and a registered/authenticated actor so the bearer WS
/// upgrade (`/api/v1/ws/{actor}`) is reachable.
async fn start_invite_authenticated() -> (String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(fauna_nest::token_store::TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        invite_verify_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: INVITE_MAX,
        })),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    (format!("ws://{addr}"), kp, token)
}

/// The authenticated twin of `invite_code_verify_is_rate_limited_per_source` —
/// an authenticated connection must not convert `invite_code.verify` into an
/// unbounded surface.
#[tokio::test]
async fn invite_code_verify_is_rate_limited_on_an_authenticated_connection() {
    let (base, kp, token) = start_invite_authenticated().await;
    let mut ws = common::open_authed(&base, &kp, &token).await;

    // The first INVITE_MAX attempts pass the throttle gate (then fail
    // verification for a non-throttle reason — the code is not registered).
    for i in 0..INVITE_MAX {
        ws.send(req_frame(
            "fauna.account.invite_code.verify",
            i as u64,
            i as u8,
            to_value(&invite_verify("NOPECODE99")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "authenticated verify attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same (authenticated) connection trips the
    // invite limiter — keyed on this connection's actor.
    ws.send(req_frame(
        "fauna.account.invite_code.verify",
        99,
        99,
        to_value(&invite_verify("NOPECODE99")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(
        !reply.ok,
        "the over-limit AUTHENTICATED invite-verify call must be refused — an \
         account must not convert invite_code.verify's per-source cap into unbounded"
    );
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit authenticated invite-verify call returns the throttle error"
    );
}

/// Harness with a tiny `register_rate_limit` and the account handler registered,
/// so over-limit `fauna.account.register` attempts are throttled by the
/// dispatcher (gate 1b⁗) before the handler. Registration is closed in the
/// `for_test` default, so under-limit attempts simply fail the register for a
/// non-throttle reason — which is all we assert is *not* `rate_limited`.
const REGISTER_MAX: u32 = 2;

async fn start_register() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        register_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: REGISTER_MAX,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    format!("ws://{addr}")
}

fn now_ms() -> u64 {
    fauna_core::data::Timestamp::now_millis()
}

/// A `RegisterRequest` with a bogus signature: the throttle gate (1b⁗) fires in
/// the dispatcher *before* the handler, so an over-limit attempt is refused
/// regardless of payload validity; under-limit attempts only need to fail with
/// something other than `rate_limited` (here: closed-registration / bad-sig).
fn register_request(kp: &ActorKeypair, handle: &str) -> RegisterRequest {
    RegisterRequest {
        actor_id: hex::encode(kp.actor_id().0),
        handle: handle.to_string(),
        timestamp: now_ms(),
        signature: hex::encode([0u8; 64]),
        invite_code: None,
        age_claim: None,
        extra: Default::default(),
    }
}

#[tokio::test]
async fn register_is_rate_limited_per_source() {
    let base = start_register().await;
    let mut ws = common::open_anonymous(&base).await;
    let kp = ActorKeypair::generate();

    // The first REGISTER_MAX attempts pass the throttle gate (then fail the
    // register for a non-throttle reason).
    for i in 0..REGISTER_MAX {
        ws.send(req_frame(
            "fauna.account.register",
            i as u64,
            i as u8,
            to_value(&register_request(&kp, "alice")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "register attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same source trips the register limiter.
    ws.send(req_frame(
        "fauna.account.register",
        99,
        99,
        to_value(&register_request(&kp, "alice")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit registration is throttled"
    );

    // Separate budget — discovery still resolves on the same connection.
    ws.send(req_frame(
        "fauna.nest.info",
        100,
        100,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 100);
    assert!(
        reply.ok,
        "discovery must not share the register budget: {:?}",
        reply.payload
    );
}

/// Authenticated twin of `start_register` — same tiny `register_rate_limit`,
/// plus a token store and a registered/authenticated actor so the bearer WS
/// upgrade (`/api/v1/ws/{actor}`) is reachable. This actor is the CALLING
/// connection, not the registrant `register_request` names.
async fn start_register_authenticated() -> (String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(fauna_nest::token_store::TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        register_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: REGISTER_MAX,
        })),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    (format!("ws://{addr}"), kp, token)
}

/// The authenticated twin of `register_is_rate_limited_per_source` — an
/// authenticated connection must not convert `fauna.account.register` into an
/// unbounded surface.
#[tokio::test]
async fn register_is_rate_limited_on_an_authenticated_connection() {
    let (base, kp, token) = start_register_authenticated().await;
    let mut ws = common::open_authed(&base, &kp, &token).await;
    let registrant = ActorKeypair::generate();

    // The first REGISTER_MAX attempts pass the throttle gate (then fail the
    // register for a non-throttle reason).
    for i in 0..REGISTER_MAX {
        ws.send(req_frame(
            "fauna.account.register",
            i as u64,
            i as u8,
            to_value(&register_request(&registrant, "alice")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "authenticated register attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same (authenticated) connection trips the
    // register limiter — keyed on this connection's actor.
    ws.send(req_frame(
        "fauna.account.register",
        99,
        99,
        to_value(&register_request(&registrant, "alice")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(
        !reply.ok,
        "the over-limit AUTHENTICATED register call must be refused — an \
         account must not convert register's per-source cap into unbounded"
    );
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit authenticated register call returns the throttle error"
    );
}

/// Harness with a tiny `invite_request_rate_limit` and the invite handlers
/// registered, so over-limit `fauna.account.invite_request.submit` attempts are
/// throttled by the dispatcher (gate 1b⁗⁗) before the handler. Under-limit
/// attempts fail the submit for a non-throttle reason (bogus signature), which
/// is all we assert is *not* `rate_limited`.
const SUBMIT_MAX: u32 = 2;

async fn start_invite_submit() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        invite_request_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: SUBMIT_MAX,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    format!("ws://{addr}")
}

fn submit_request(kp: &ActorKeypair, handle: &str) -> InviteRequestSubmit {
    InviteRequestSubmit {
        actor_id: hex::encode(kp.actor_id().0),
        handle: handle.to_string(),
        message: "please let me in".to_string(),
        timestamp: now_ms(),
        signature: hex::encode([0u8; 64]),
        // No declared band: this suite pins the anonymous rate limiter, not
        // admission (`family-safety.md` § The account age band — absence is
        // the by-construction 18+/none case).
        age_claim: None,
        extra: Default::default(),
    }
}

#[tokio::test]
async fn invite_request_submit_is_rate_limited_per_source() {
    let base = start_invite_submit().await;
    let mut ws = common::open_anonymous(&base).await;
    let kp = ActorKeypair::generate();

    // The first SUBMIT_MAX attempts pass the throttle gate (then fail the submit
    // for a non-throttle reason — bogus signature).
    for i in 0..SUBMIT_MAX {
        ws.send(req_frame(
            "fauna.account.invite_request.submit",
            i as u64,
            i as u8,
            to_value(&submit_request(&kp, "alice")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "submit attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same source trips the submit limiter.
    ws.send(req_frame(
        "fauna.account.invite_request.submit",
        99,
        99,
        to_value(&submit_request(&kp, "alice")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit invite-request submit is throttled"
    );
}

/// Authenticated twin of `start_invite_submit` — same tiny
/// `invite_request_rate_limit`, plus a token store and a
/// registered/authenticated actor so the bearer WS upgrade
/// (`/api/v1/ws/{actor}`) is reachable.
async fn start_invite_submit_authenticated() -> (String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(fauna_nest::token_store::TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::invite_handlers::register_invite_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        invite_request_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: SUBMIT_MAX,
        })),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    (format!("ws://{addr}"), kp, token)
}

/// The authenticated twin of `invite_request_submit_is_rate_limited_per_source`
/// — an authenticated connection must not convert
/// `fauna.account.invite_request.submit` into an unbounded surface.
#[tokio::test]
async fn invite_request_submit_is_rate_limited_on_an_authenticated_connection() {
    let (base, kp, token) = start_invite_submit_authenticated().await;
    let mut ws = common::open_authed(&base, &kp, &token).await;
    let requester = ActorKeypair::generate();

    // The first SUBMIT_MAX attempts pass the throttle gate (then fail the
    // submit for a non-throttle reason — bogus signature).
    for i in 0..SUBMIT_MAX {
        ws.send(req_frame(
            "fauna.account.invite_request.submit",
            i as u64,
            i as u8,
            to_value(&submit_request(&requester, "alice")),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_ne!(
            reply_error(&reply).code,
            "fauna.protocol.rate_limited",
            "authenticated submit attempt {i} should reach the handler, not be throttled"
        );
    }

    // The next attempt from the same (authenticated) connection trips the
    // submit limiter — keyed on this connection's actor.
    ws.send(req_frame(
        "fauna.account.invite_request.submit",
        99,
        99,
        to_value(&submit_request(&requester, "alice")),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(
        !reply.ok,
        "the over-limit AUTHENTICATED invite-request submit must be refused — \
         an account must not convert invite_request.submit's per-source cap \
         into unbounded"
    );
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit authenticated invite-request submit returns the throttle error"
    );
}

/// Harness for the emergency lockout kind: the account handlers registered and
/// the *generic* `anonymous_rate_limit` shrunk, because `fauna.account.lockout`
/// rides `is_throttled_anonymous_kind` (gate 1b′) alongside the escrow / veto /
/// succession ceremonies rather than carrying a dedicated limiter.
async fn start_lockout() -> (String, Arc<CacheDb>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        anonymous_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: TEST_MAX_EVENTS,
        })),
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    (format!("ws://{addr}"), db)
}

/// A **genuinely valid** signed lockout over the domain-tagged
/// `account_lockout_signed_message` — the exact bytes `account_core::lockout_core`
/// verifies. Deliberately not the bogus-signature shortcut the register/submit
/// tests use: the point here is that throttling the panic button does not cost
/// the legitimate owner their first call, and a payload that fails for its own
/// reasons could not show it.
fn lockout_request(kp: &ActorKeypair) -> AccountLockoutRequest {
    let timestamp = fauna_core::data::Timestamp::now_secs() as u64;
    let msg = fauna_protocol::account::account_lockout_signed_message(&kp.actor_id().0, timestamp);
    AccountLockoutRequest {
        actor_id: hex::encode(kp.actor_id().0),
        timestamp,
        signature: hex::encode(kp.signing_key().sign(&msg).to_bytes()),
        ..Default::default()
    }
}

/// The emergency lockout is signature-bound, so an Ed25519 forgery is not the
/// threat — the throttle bounds the **verification work an anonymous source can
/// conscript**, exactly as `recovery.escrow.fetch` and `recovery.succession.submit`
/// do (`pre_identity_allowlist::is_throttled_anonymous_kind`). This pins both
/// halves at once: the legitimate owner's *first* call still lands (the panic
/// button is not what the limit costs), and a flood from the same source is
/// refused `rate_limited` while an unrelated kind on the same connection is
/// untouched.
#[tokio::test]
async fn lockout_is_rate_limited_per_source() {
    let (base, db) = start_lockout().await;
    let mut ws = common::open_anonymous(&base).await;
    let kp = ActorKeypair::generate();
    // `set_locked_until` is an UPDATE, so the row must exist for the lockout to
    // persist — this is the *legitimate owner* arm, not a bogus payload.
    db.create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();

    // The owner's real emergency lockout succeeds on the first call.
    ws.send(req_frame(
        "fauna.account.lockout",
        0,
        0,
        to_value(&lockout_request(&kp)),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert!(
        reply.ok,
        "a single valid lockout must still succeed under the throttle: {:?}",
        reply.payload
    );

    // Repeats stay under the window's budget and keep reaching the handler. A
    // re-lock is legitimate (the owner may raise the duration), so these land
    // `ok` rather than failing for a non-throttle reason the way the register /
    // invite-submit tests' deliberately-bogus payloads do.
    for i in 1..TEST_MAX_EVENTS {
        ws.send(req_frame(
            "fauna.account.lockout",
            i as u64,
            i as u8,
            to_value(&lockout_request(&kp)),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert!(
            reply.ok,
            "lockout attempt {i} should reach the handler, not be throttled: {:?}",
            reply.payload
        );
    }

    // The next one trips the anonymous-surface limiter, before the unmetered
    // Ed25519 verify in `lockout_core`.
    ws.send(req_frame(
        "fauna.account.lockout",
        99,
        99,
        to_value(&lockout_request(&kp)),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(!reply.ok);
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit lockout is throttled"
    );

    // Per-kind buckets — a lockout flood must not spend discovery's budget.
    ws.send(req_frame(
        "fauna.nest.info",
        100,
        100,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 100);
    assert!(
        reply.ok,
        "discovery must not share the lockout budget: {:?}",
        reply.payload
    );
}

// ---------------------------------------------------------------------------
// The pre-identity throttles bind on the AUTHENTICATED class too (2026-08-24).
//
// Every kind the five throttle gates guard is a *pre-identity* kind, and the
// dispatcher's gate (1d) deliberately exempts pre-identity kinds from the
// authenticated class allowlist — "an authed connection may still call them".
// While those gates were additionally scoped `conn.anonymous && …`, one account
// of any tier converted all 18 of them from "N events / window per source" to
// **unlimited on a single connection**: the unmetered Ed25519 verifies, the
// unbounded nonce mints, the directory enumeration, and `claim_admin`, whose two
// caps are what `federation.md` § Security names as the primary bound licensing
// the 40-bit claim code.
//
// ⚠ THIS TEST EXISTS TO SURVIVE A NO-OP TRAP. The obvious fix — "drop
// `conn.anonymous &&`" — changes nothing, because an authenticated connection
// carries `peer_addr: None` (its upgrade captures no `ConnectInfo`) and the
// IP-keyed `check` fails open on `None`. So this test is red against BOTH the
// original shape and that one, and only goes green once the authenticated class
// has a bucket key of its own (`check_conn` → `actor_id`). It drives a genuine
// authenticated WS connection; injecting a synthetic `peer_addr` instead would
// be a false green on a path production never takes.

/// Same tiny-window state as `start`, plus a token store and a registered user,
/// so the AUTHENTICATED upgrade (`/api/v1/ws/{actor}` + `bearer.` subprotocol)
/// is reachable.
async fn start_authenticated() -> (String, ActorKeypair, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(fauna_nest::token_store::TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice")
        .await
        .unwrap();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            b.build()
        }),
        anonymous_rate_limit: Arc::new(Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: TEST_MAX_EVENTS,
        })),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Served WITH `ConnectInfo`, exactly like the anonymous cases above — so the
    // test cannot pass merely because the peer is unknown to the server. The
    // authenticated upgrade still declines to capture it; that is the point.
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    (format!("ws://{addr}"), kp, token)
}

#[tokio::test]
async fn discovery_surface_is_rate_limited_on_an_authenticated_connection() {
    let (base, kp, token) = start_authenticated().await;
    let mut ws = common::open_authed(&base, &kp, &token).await;

    // A throttled pre-identity kind, called over an AUTHENTICATED connection —
    // permitted by gate (1d), and therefore something the throttle must bound.
    for i in 0..TEST_MAX_EVENTS {
        ws.send(req_frame(
            "fauna.nest.info",
            i as u64,
            i as u8,
            to_value(&NestInfoRequest::default()),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert!(
            reply.ok,
            "authenticated discovery call {i} should succeed before the limit: {:?}",
            reply.payload
        );
    }

    // The next one trips the limiter — keyed on this connection's actor, since
    // an authenticated connection has no peer address to key on.
    ws.send(req_frame(
        "fauna.nest.info",
        99,
        99,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert_eq!(reply.correlation_id, 99);
    assert!(
        !reply.ok,
        "the over-limit AUTHENTICATED discovery call must be refused — an \
         account must not convert a pre-identity surface into an unbounded one"
    );
    assert_eq!(
        reply_error(&reply).code,
        "fauna.protocol.rate_limited",
        "over-limit authenticated discovery call returns the throttle error"
    );
}

#[tokio::test]
async fn authenticated_and_anonymous_callers_do_not_share_a_bucket() {
    // The two classes key on different things (actor vs IP), so one must not
    // spend the other's budget. Without a disjoint key namespace an actor id
    // could land in an IP bucket; and a shared bucket would let one
    // authenticated client throttle every anonymous caller arriving from the
    // same address, which is every caller behind one NAT.
    let (base, kp, token) = start_authenticated().await;
    let mut authed = common::open_authed(&base, &kp, &token).await;

    // Exhaust the authenticated actor's bucket.
    for i in 0..=TEST_MAX_EVENTS {
        authed
            .send(req_frame(
                "fauna.nest.info",
                i as u64,
                i as u8,
                to_value(&NestInfoRequest::default()),
            ))
            .await
            .unwrap();
        let _ = recv_reply(&mut authed).await;
    }

    // An anonymous connection from the same loopback address still has its full
    // budget: its first call succeeds.
    let mut anon = common::open_anonymous(&base).await;
    anon.send(req_frame(
        "fauna.nest.info",
        200,
        200,
        to_value(&NestInfoRequest::default()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut anon).await;
    assert!(
        reply.ok,
        "an anonymous caller must not pay for an authenticated actor's spend: {:?}",
        reply.payload
    );
}
