//! tier_3 socket-level witnesses for the pre-identity WS surface's resource
//! bounds (`deployment-threat-model.md` § Concern 1a, finding H2;
//! `transport-connection.md` § Abuse posture). Drives the real public anonymous
//! WS path, served with `into_make_service_with_connect_info` so each
//! connection carries the `ConnectInfo` peer the per-source bounds key on
//! (exactly as the production TLS path does via `WithConnectInfo`).
//!
//! Three properties, all load-bearing:
//!  1. **`fauna.auth.challenge` cannot grow the nonce store without bound** — it
//!     rides the generic per-source anonymous throttle, and the store itself
//!     refuses issue past its per-source and global caps, so a flood is refused
//!     `rate_limited` and the store's length (inspected directly) stays at the
//!     cap.
//!  2. **A frame flood on a pre-identity connection closes it** — the
//!     unthrottled signature kinds (`auth.verify` here) each cost an Ed25519
//!     verify, so the connection carries a frame budget while no identity is
//!     bound; past it the nest closes with 1008 (policy violation).
//!  3. **A well-behaved client is untouched** — the shared client's real
//!     silent-challenge mint (`WsChallengeBearer`: nest_handshake → challenge →
//!     verify) completes on a nest carrying every bound above.

mod common;
use common::recv_reply;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

use fauna_core::identity::ActorKeypair;
use fauna_nest::bridge_rate_limit::{Limiter, LimiterConfig};
use fauna_nest::challenge_auth::ChallengeStore;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::state::AuthState;
use fauna_protocol::auth::{ChallengeRequest, VerifyRequest};
use fauna_protocol::{
    Frame, Reply, Request, RpcError, Value, decode_strict, encode_canonical, encode_frame,
};

/// Per-source cap the store-bound harness gives its challenge store.
const STORE_PER_SOURCE: usize = 4;

/// Spin a nest serving the auth handlers on the anonymous endpoint, with a
/// caller-chosen anonymous limiter and challenge store. Returns the `ws://` base
/// and the state (the test inspects the store through it).
async fn start(
    anonymous_rate_limit: Limiter,
    challenge_store: ChallengeStore,
) -> (String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let base = AppState::for_test(db.clone());
    let state = Arc::new(AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        anonymous_rate_limit: Arc::new(anonymous_rate_limit),
        auth: AuthState {
            challenge_store: Arc::new(challenge_store),
            ..AuthState::default()
        },
        ..base
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
    (format!("ws://{addr}"), state)
}

fn generous_limiter() -> Limiter {
    Limiter::with_config(LimiterConfig {
        window: Duration::from_secs(60),
        max_events: 10_000,
    })
}

fn to_value<T: serde::Serialize>(t: &T) -> Value {
    decode_strict(&encode_canonical(t).unwrap()).unwrap()
}

fn req_frame(kind: &str, corr: u64, payload: Value) -> Message {
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&corr.to_be_bytes());
    let frame = Frame::Request(Request {
        ty: Request::TYPE,
        correlation_id: corr,
        kind: kind.to_string(),
        // Distinct idempotency key per request, or the per-connection cache
        // would replay the first reply before any gate ran.
        idempotency_key: key,
        payload,
        replay_forbidden: None,
        deadline_ms: None,
    });
    Message::Binary(encode_frame(&frame).unwrap())
}

fn reply_error(r: &Reply) -> RpcError {
    decode_strict(&encode_canonical(&r.payload).unwrap()).unwrap()
}

fn challenge_for(kp: &ActorKeypair) -> Value {
    to_value(&ChallengeRequest {
        actor_id: hex::encode(kp.actor_id().0),
        extra: Default::default(),
    })
}

#[tokio::test]
async fn a_challenge_flood_is_refused_past_the_store_cap_and_the_store_stays_bounded() {
    // The dispatcher throttle is opened wide here so the STORE's own cap is
    // what a flood meets — the bound must hold even for a source the throttle
    // admits (an authenticated caller's bucket, a wide NAT).
    let (base, state) = start(
        generous_limiter(),
        ChallengeStore::with_caps(STORE_PER_SOURCE, 1_000),
    )
    .await;
    let mut ws = common::open_anonymous(&base).await;

    // Every challenge names a fresh, unregistered key — the store used to take
    // any valid Ed25519 point.
    let mut refused = 0;
    for corr in 0..12u64 {
        ws.send(req_frame(
            "fauna.auth.challenge",
            corr,
            challenge_for(&ActorKeypair::generate()),
        ))
        .await
        .unwrap();
        let reply = recv_reply(&mut ws).await;
        assert_eq!(reply.correlation_id, corr);
        if corr < STORE_PER_SOURCE as u64 {
            assert!(
                reply.ok,
                "challenge {corr} is under the cap: {:?}",
                reply.payload
            );
        } else {
            assert!(
                !reply.ok,
                "challenge {corr} is past the cap and must be refused"
            );
            assert_eq!(reply_error(&reply).code, "fauna.protocol.rate_limited");
            refused += 1;
        }
    }
    assert_eq!(refused, 12 - STORE_PER_SOURCE);
    assert_eq!(
        state.auth.challenge_store.len().await,
        STORE_PER_SOURCE,
        "the store holds exactly the cap, never one entry per request"
    );
}

#[tokio::test]
async fn auth_challenge_rides_the_per_source_anonymous_throttle() {
    // The other half of the bound: the issuance RATE is metered per source by
    // the generic anonymous gate, like the escrow / veto challenges.
    let (base, state) = start(
        Limiter::with_config(LimiterConfig {
            window: Duration::from_secs(60),
            max_events: 3,
        }),
        ChallengeStore::new(),
    )
    .await;
    let mut ws = common::open_anonymous(&base).await;
    for corr in 0..3u64 {
        ws.send(req_frame(
            "fauna.auth.challenge",
            corr,
            challenge_for(&ActorKeypair::generate()),
        ))
        .await
        .unwrap();
        assert!(
            recv_reply(&mut ws).await.ok,
            "challenge {corr} under the throttle"
        );
    }
    ws.send(req_frame(
        "fauna.auth.challenge",
        99,
        challenge_for(&ActorKeypair::generate()),
    ))
    .await
    .unwrap();
    let reply = recv_reply(&mut ws).await;
    assert!(
        !reply.ok,
        "the fourth challenge from one source is throttled"
    );
    assert_eq!(reply_error(&reply).code, "fauna.protocol.rate_limited");
    assert_eq!(state.auth.challenge_store.len().await, 3);
}

#[tokio::test]
async fn a_verify_flood_on_a_pre_identity_connection_closes_it_with_1008() {
    let (base, state) = start(generous_limiter(), ChallengeStore::new()).await;
    let mut ws = common::open_anonymous(&base).await;
    let kp = ActorKeypair::generate();
    let nest_id = hex::encode(state.bound_identity());
    // A well-formed verify with a bogus nonce and signature: every one clears
    // the cheap shape checks and buys an Ed25519 verify — the unmetered work.
    let verify = to_value(&VerifyRequest {
        actor_id: hex::encode(kp.actor_id().0),
        nonce: hex::encode([7u8; 32]),
        signature: hex::encode([9u8; 64]),
        nest_id,
        client_nonce: None,
        extra: Default::default(),
    });
    for corr in 0..500u64 {
        if ws
            .send(req_frame("fauna.auth.verify", corr, verify.clone()))
            .await
            .is_err()
        {
            break; // the nest already closed on us
        }
    }
    let (code, _) = common::read_until_close(&mut ws, None).await;
    assert_eq!(
        code,
        Some(1008),
        "a pre-identity frame flood must close the connection with a policy code"
    );
}

#[tokio::test]
async fn a_well_behaved_client_still_mints_over_the_silent_challenge() {
    // Shipped-shape budgets (the default throttle and store), and the shared
    // client's real mint — the sequence every app runs at sign-in.
    let (base, state) = start(
        Limiter::with_config(fauna_nest::anonymous_rate_limit::default_config()),
        ChallengeStore::new(),
    )
    .await;
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user_with_handle(&kp.actor_id().0, "free", "resident", None)
        .await
        .unwrap();
    let http_base = base.replacen("ws://", "http://", 1);
    let bearer = fauna_client::ws_challenge_bearer::WsChallengeBearer::new(
        http_base,
        kp.actor_id().0,
        kp.signing_key().clone(),
    );
    use fauna_nest_http::BearerSource;
    let token = bearer
        .bearer()
        .await
        .expect("the silent-challenge mint completes");
    assert!(!token.is_empty());
    // A consumed nonce leaves the store: the bound counts outstanding
    // challenges, so sign-ins never accumulate against it.
    assert_eq!(state.auth.challenge_store.len().await, 0);
}
