//! Integration test — the nest drives the **server half** of the WS heartbeat
//! (`transport.md` § Connection lifecycle → Heartbeat).
//!
//! **What was missing.** The heartbeat shipped client-initiated only: the native
//! adapter (`fauna_ws_substrate::adapter`) sends a Ping every `KEEPALIVE_INTERVAL`
//! and gives up after `KEEPALIVE_TIMEOUT` of silence, but the nest's own loop had
//! no ping and no timeout at all — it simply parked in `ws_rx.next()` forever. A
//! peer that vanished without a clean TCP close (a suspended VM, a dropped link, a
//! killed process) therefore held its connection — and the per-IP permit under it
//! — until the kernel's TCP keepalive got around to it. That is the residual left
//! by the 2026-08-01 connection-cap incident:
//! keepalive bounds the damage at ~4 min, this bounds it at the heartbeat.
//!
//! **The two tests below are a matched pair, and the second is the load-bearing
//! one.** It is easy to "fix" the first with a bare inbound-idle timeout — and
//! that would cut every idle **browser** tab, because the W3C WebSocket API
//! exposes no ping primitive, so a web client sends nothing at all while idle
//! (the sanctioned priority-#1 divergence, `transport.md` § Connection
//! lifecycle). A server-initiated Ping is what distinguishes the two cases: RFC
//! 6455 makes the Pong mandatory and the browser answers it *below* JavaScript.
//! So `a_silent_but_responsive_peer_is_never_reaped` is the regression lock that
//! keeps the cheap-but-wrong implementation from passing.
//!
//! Both run the real loop on a compressed clock (`WsHeartbeatPolicy`), and
//! neither asserts on wall-clock timing: the positive case deadline-polls a
//! *state* (the connection left the registry) with a budget far above any
//! non-pathological delay, and the negative case anchors to a **causal barrier**
//! — server Pings actually observed on the wire — rather than sleeping and
//! hoping (convention 14).

mod common;
use common::open_authed;
use common::poll_until;

use std::sync::Arc;
use std::time::Duration;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::token_store::TokenStore;
use fauna_nest::ws::{WsHeartbeatPolicy, WsState};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

/// Compressed heartbeat for the tests. The *ratio* is what matters and it
/// mirrors production (timeout = 2× interval); the absolute values only have to
/// be small enough that a test run is quick and large enough that ordinary
/// scheduling jitter on a loaded box cannot starve a Ping for a whole window.
const PING_INTERVAL: Duration = Duration::from_millis(200);
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(400);

struct Harness {
    url: String,
    kp: ActorKeypair,
    token: String,
    state: Arc<AppState>,
}

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ws: Arc::new(WsState::with_heartbeat(WsHeartbeatPolicy {
            ping_interval: PING_INTERVAL,
            liveness_timeout: LIVENESS_TIMEOUT,
        })),
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Harness {
        url: format!("ws://{addr}"),
        kp,
        token,
        state,
    }
}

/// A peer that stops answering is reaped, and its registry slot released.
///
/// The vanished-peer shape is reproduced exactly: the socket is held **open**
/// but never polled, so tungstenite never gets to emit its automatic Pong. The
/// nest sees an established TCP connection that has simply gone quiet forever —
/// which, before the server heartbeat, it would have waited on indefinitely.
/// Dropping the socket instead would send a FIN and tear the connection down for
/// the ordinary reason, proving nothing.
#[tokio::test]
async fn a_peer_that_never_answers_a_ping_is_reaped() {
    let h = start().await;
    let ws = open_authed(&h.url, &h.kp, &h.token).await;

    poll_until("the connection to register", || {
        h.state.ws.connection_count() == 1
    })
    .await;

    // The peer vanishes here: `ws` stays alive (socket open) but is never polled
    // again, so no Pong — and no frame of any kind — will ever reach the nest.
    poll_until("the unresponsive peer to be reaped", || {
        h.state.ws.connection_count() == 0
    })
    .await;

    drop(ws);
}

/// A peer that answers Pings but never *sends* anything is left alone — the
/// browser case, and the reason this is a Ping rather than an idle timeout.
///
/// The barrier is causal, not temporal: we count server Pings as they arrive and
/// only assert once enough have landed to span more than a full liveness window.
/// Reading the stream is also what makes the client "responsive" — tungstenite
/// queues the mandatory Pong when it reads a Ping — so the loop below *is* the
/// browser's below-JS auto-answer, modelled faithfully. At no point does this
/// client send an application frame.
#[tokio::test]
async fn a_silent_but_responsive_peer_is_never_reaped() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;

    poll_until("the connection to register", || {
        h.state.ws.connection_count() == 1
    })
    .await;

    // Span more than one liveness window, measured in Pings rather than sleeps.
    // With timeout = 2× interval, three Pings already cover a full window; five
    // clears it with room to spare on a loaded box.
    let pings_needed = 5;
    let mut pings_seen = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while pings_seen < pings_needed {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "only saw {pings_seen} of {pings_needed} server Pings before the budget ran out; \
             the nest is not pinging its clients"
        );
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Ping(_)))) => pings_seen += 1,
            // A close here is the failure this test exists to catch: the nest
            // reaped a peer that was answering every single Ping.
            Ok(Some(Ok(Message::Close(frame)))) => panic!(
                "nest closed a responsive-but-silent (browser-shaped) connection \
                 after {pings_seen} Pings: {frame:?}"
            ),
            Ok(Some(Ok(_))) => {} // stray push; not what this test is about
            Ok(Some(Err(e))) => panic!("transport error on a responsive peer: {e}"),
            Ok(None) => panic!("stream ended on a responsive-but-silent peer"),
            Err(_) => panic!(
                "only saw {pings_seen} of {pings_needed} server Pings before the budget ran out"
            ),
        }
    }

    assert_eq!(
        h.state.ws.connection_count(),
        1,
        "a peer that answered {pings_seen} consecutive Pings — spanning more than a full \
         liveness window — must still be connected; reaping it would cut every idle browser tab"
    );
}
