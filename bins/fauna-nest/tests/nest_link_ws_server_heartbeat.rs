//! Integration test — the `nest_link` **proxy** drives the server half of the WS
//! heartbeat on its worker connection (`transport.md` § Connection lifecycle →
//! Heartbeat), and a vanished worker actually releases the socket.
//!
//! **What was wrong before, and it was not "no heartbeat".** This endpoint *did*
//! have a liveness timer — an application-level `ProxyCommand::Ping` /
//! `WorkerMessage::Pong` pair on a 30 s/90 s cadence — but detecting the dead
//! link did nothing to the socket. The three per-connection tasks were joined
//! with `tokio::select!` over their `JoinHandle`s, and **dropping a `JoinHandle`
//! does not abort its task**: when the heartbeat task broke on timeout, the
//! reader task kept `ws_rx` — and therefore the WebSocket, the TCP connection,
//! and the per-IP permit `serve_tls` took for it — alive forever. The timer
//! noticed the corpse and then walked away from it.
//!
//! The peer here is our own worker (`nest_link/client.rs`), which answers a
//! protocol `Message::Ping` with a `Pong` explicitly (`client.rs:140-141`) and
//! has no ping-driven timeout of its own, so the standard mechanism replaces the
//! bespoke one outright.
//!
//! Neither test asserts on wall-clock timing (convention 14): the positive case
//! deadline-polls a *state* (the worker left the registry) and the negative case
//! anchors to a causal barrier — server Pings counted on the wire.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use fauna_nest::db::CacheDb;
use fauna_nest::nest_link::protocol::{ProxyCommand, WorkerMessage};
use fauna_nest::routes::AppState;
use fauna_nest::ws::{WsHeartbeatPolicy, WsState};
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::Message;

const PING_INTERVAL: Duration = Duration::from_millis(200);
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(400);

struct Harness {
    url: String,
    state: Arc<AppState>,
}

async fn start() -> (Harness, SigningKey) {
    let worker_key = SigningKey::from_bytes(&[7u8; 32]);
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        ws: Arc::new(WsState::with_heartbeat(WsHeartbeatPolicy {
            ping_interval: PING_INTERVAL,
            liveness_timeout: LIVENESS_TIMEOUT,
        })),
        bridge: fauna_nest::state::BridgeState {
            worker: fauna_nest::nest_link::proxy::WorkerState::new(Some(
                worker_key.verifying_key().to_bytes(),
            )),
            ..Default::default()
        },
        ..AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .ok();
    });
    (
        Harness {
            url: format!("ws://{addr}/internal/worker/ws"),
            state,
        },
        worker_key,
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connect and complete the worker handshake (challenge → signature → Hello),
/// exactly as `nest_link/client.rs` does.
async fn connect_worker(h: &Harness, key: &SigningKey) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(&h.url)
        .await
        .expect("worker upgrade should succeed");

    let challenge = match ws.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<ProxyCommand>(&t).unwrap() {
            ProxyCommand::AuthChallenge { challenge } => challenge,
            other => panic!("expected AuthChallenge, got {other:?}"),
        },
        other => panic!("expected the auth challenge, got {other:?}"),
    };

    let challenge_bytes = hex::decode(&challenge).unwrap();
    let sig = key.sign(&challenge_bytes);
    let auth = WorkerMessage::AuthResponse {
        public_key: hex::encode(key.verifying_key().to_bytes()),
        signature: hex::encode(sig.to_bytes()),
    };
    ws.send(Message::Text(serde_json::to_string(&auth).unwrap().into()))
        .await
        .unwrap();

    let hello = WorkerMessage::Hello {
        max_storage_bytes: 1 << 30,
        current_usage_bytes: 0,
        payload_count: 0,
    };
    ws.send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    ws
}

async fn poll_until(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A worker that vanishes is reaped: it leaves the registry **and** its socket
/// is released. The second half is the one the old app-level timer failed — it
/// noticed the timeout and left every task holding the connection.
///
/// The vanished shape is reproduced exactly: the WebSocket is never polled
/// again, so tungstenite never emits its automatic Pong. Reading from the raw
/// stream underneath consumes the server's Ping bytes without answering them,
/// and lets the test observe the teardown directly as EOF.
#[tokio::test]
async fn a_worker_that_never_answers_a_ping_is_reaped_and_its_socket_released() {
    let (h, key) = start().await;
    let mut ws = connect_worker(&h, &key).await;

    poll_until("the worker to register", async || {
        h.state.bridge.worker.is_connected().await
    })
    .await;

    // The worker vanishes here.
    let raw = ws.get_mut();
    let mut buf = [0u8; 256];
    let eof = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match raw.read(&mut buf).await {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(_) => return true,
            }
        }
    })
    .await;

    assert!(
        eof.is_ok(),
        "the proxy never released the socket of a worker that answered no Ping for many \
         liveness windows — the reader task is still holding it (the JoinHandle-drop bug)"
    );
    assert!(
        !h.state.bridge.worker.is_connected().await,
        "a reaped worker must also leave the connected registry"
    );
}

/// A worker that answers Pings but sends nothing of its own is left alone. The
/// barrier is causal: server Pings counted on the wire, spanning more than a
/// full liveness window.
#[tokio::test]
async fn a_silent_but_responsive_worker_is_never_reaped() {
    let (h, key) = start().await;
    let mut ws = connect_worker(&h, &key).await;

    poll_until("the worker to register", async || {
        h.state.bridge.worker.is_connected().await
    })
    .await;

    let pings_needed = 5;
    let mut pings_seen = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while pings_seen < pings_needed {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "only saw {pings_seen} of {pings_needed} server Pings; the proxy is not pinging \
             its worker"
        );
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Ping(_)))) => pings_seen += 1,
            Ok(Some(Ok(Message::Close(frame)))) => panic!(
                "the proxy closed a responsive-but-silent worker after {pings_seen} Pings: \
                 {frame:?}"
            ),
            Ok(Some(Ok(_))) => {} // a real command; not what this test is about
            Ok(Some(Err(e))) => panic!("transport error on a responsive worker: {e}"),
            Ok(None) => panic!("stream ended on a responsive-but-silent worker"),
            Err(_) => panic!("only saw {pings_seen} of {pings_needed} server Pings"),
        }
    }

    assert!(
        h.state.bridge.worker.is_connected().await,
        "a worker that answered {pings_seen} consecutive Pings — spanning more than a full \
         liveness window — must still be connected"
    );
}

// ── the worker-WS IP gate fails CLOSED on a missing ConnectInfo ──────────────

/// `/internal/worker/ws` is on the **public** listener — the IP allowlist, not
/// the route's absence, is what protects it. `ConnectInfo` is injected on both
/// production serving paths (TLS via `serve_tls`'s `WithConnectInfo`,
/// plain-HTTP via `into_make_service_with_connect_info`), so its absence can
/// only mean a middleware-ordering regression. The gate must then reject, not
/// wave the connection through.
///
/// The shape this pins: the handler used to run its allowlist check inside an
/// `if let Some(ConnectInfo(..))` with **no `else`**, so a missing
/// `ConnectInfo` skipped the check entirely and any source IP reached the
/// worker channel's auth challenge. Its sibling gate
/// (`sidecar_channel::sidecar_ws_upgrade`, sharing the very same
/// peer-address allowlist) already failed closed and documented why.
///
/// Serving with a bare `axum::serve` is exactly the "no ConnectInfo" condition,
/// so this test drives the real production router through the real regression.
#[tokio::test]
async fn worker_ws_rejects_an_upgrade_with_no_connect_info() {
    let worker_key = SigningKey::from_bytes(&[9u8; 32]);
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState {
        bridge: fauna_nest::state::BridgeState {
            worker: fauna_nest::nest_link::proxy::WorkerState::new(Some(
                worker_key.verifying_key().to_bytes(),
            )),
            ..Default::default()
        },
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // NO `into_make_service_with_connect_info` — this is the regression the
        // gate has to survive, not a contrived request.
        axum::serve(listener, app).await.ok();
    });

    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/internal/worker/ws"))
        .await
        .expect_err("the upgrade must be refused when the peer address is unknown");

    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => assert_eq!(
            resp.status(),
            axum::http::StatusCode::FORBIDDEN,
            "a peer-address-less worker upgrade must be refused 403, not admitted"
        ),
        other => panic!("expected an HTTP 403 rejection, got {other:?}"),
    }

    assert!(
        !state.bridge.worker.is_connected().await,
        "no worker may be bound from a connection the nest cannot place"
    );
}
