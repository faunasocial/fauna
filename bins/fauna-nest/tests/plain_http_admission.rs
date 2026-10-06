//! Regression guard for the 2026-08-22 socket-exhaustion incident, nest half:
//! the **plain-HTTP** listener admits connections through the same accept loop
//! as TLS (`fauna_nest::serve_plain` → `serve_admitted`), and a loopback peer
//! is bounded by the loopback ceiling instead of being exempt.
//!
//! ## Why this test exists
//!
//! One leaking same-box e2e client (an anonymous-client `Drop` that never
//! closed its WebSocket — fixed client-side) reconnected ~1/s for 4.5 h. The
//! nest it talked to served plain HTTP through a bare `axum::serve`: no global
//! cap, no per-IP permit, no keepalive, no shed line — and the per-IP
//! request-rate governor exempted loopback outright anyway (it no longer does:
//! `rate_limit.rs::LOOPBACK_MAX_RPS`, 2026-08-30, closing the last exemption
//! this incident left standing). It accepted **16,311** sockets, logged
//! nothing but its 10-minute token-GC tick, and the machine's network state
//! ran out: every new outbound TCP connection box-wide failed for hours. Since the
//! e2e suite serves plain HTTP, every accept-loop defence the TLS path had was
//! invisible to it as well.
//!
//! ## What it asserts (`transport-connection.md` § Abuse posture → *Loopback is bounded*)
//!
//! Through the real `serve_plain` with a limiter whose loopback ceiling is 2:
//! two held WebSocket connections fill the ceiling; the third connection is
//! **shed** — the server closes it without answering; releasing one held
//! connection makes the source admittable again. Plus the plain flavour's
//! basic contract now that it no longer rides `axum::serve`: it serves HTTP,
//! injects `ConnectInfo` (the loopback gate's input), and drives WebSocket
//! upgrades.
//!
//! The held connections are **WebSockets on purpose**. hyper's connection
//! future resolves at an upgrade, so a permit held by the accept loop's task
//! was released the moment a connection became a live client connection — the
//! first run of this test (2026-08-25) admitted the third connection past two
//! held WebSockets, exposing that the nest's caps had only ever counted
//! handshakes. The permits now ride the socket (`Admitted<IO>` in
//! `fauna_nest`), and holding upgraded sockets here is what witnesses that.
//!
//! ## Red-verification
//!
//! With the loopback exemption restored (or the old bare `axum::serve`), the
//! third connection is admitted and answered `200`, so the "shed unanswered"
//! assertion fails deterministically. Nothing here depends on timing: the shed
//! is observed as the server's close (EOF/RST) on a socket the client already
//! wrote to — a state that either happens or does not.
//!
//! ## Timing (convention 14)
//!
//! Every wait is a positive wait on a generous budget ([`BUDGET`]) for a state
//! that must arrive — a 101, a close, a 200 — never a sleep-then-assert. The
//! re-admission after a release is a deadline poll, because the server-side
//! task ends asynchronously after the client's close.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ConnectInfo;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::Response;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

/// Far above any non-pathological local round-trip; a wait that hits it is a
/// failure, not a flake.
const BUDGET: Duration = Duration::from_secs(30);

/// Echoes every frame back (version-agnostic — re-sends whatever arrived).
async fn ws_echo(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(msg)) = socket.recv().await {
            if socket.send(msg).await.is_err() {
                break;
            }
        }
    })
}

async fn health() -> &'static str {
    "ok"
}

/// The extractor the bridge-enrollment loopback gate relies on.
async fn peer(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> String {
    addr.ip().to_string()
}

fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/peer", get(peer))
        .route("/ws", get(ws_echo))
}

/// Serve `app()` through the real plain-HTTP accept loop with the given
/// loopback ceiling (the admin cap stays at its default — irrelevant to a
/// loopback client, which is the point).
async fn spawn_plain(loopback_ceiling: usize) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // spawn-ok(test)
    tokio::spawn(fauna_nest::serve_plain(
        listener,
        app().into_make_service(),
        fauna_conn_limit::PerIpConnLimit::with_loopback_ceiling(
            fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP,
            loopback_ceiling,
        ),
    ));
    addr
}

/// Open a WebSocket to `/ws` and hold it — a live, admitted connection whose
/// per-IP permit the server keeps for as long as the socket is open. Awaiting
/// the 101 is what makes the permit *held* before the caller moves on.
async fn hold_ws(addr: SocketAddr) -> WebSocketStream<TcpStream> {
    let tcp = TcpStream::connect(addr).await.expect("tcp connect");
    let (ws, _resp) = tokio::time::timeout(
        BUDGET,
        tokio_tungstenite::client_async("ws://localhost/ws", tcp),
    )
    .await
    .expect("WS handshake within budget")
    .expect("WS handshake over plain HTTP");
    ws
}

/// One plain HTTP/1.1 request on a fresh connection; returns the raw response
/// bytes the server sent before closing — EMPTY when the server shed the
/// connection at admission (closed it without reading the request).
async fn raw_get(addr: SocketAddr, path: &str) -> Vec<u8> {
    let mut s = TcpStream::connect(addr).await.expect("tcp connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    // A shed server may already have closed: EPIPE here is the same evidence
    // as an empty read, so the write's result is deliberately not asserted.
    let _ = s.write_all(req.as_bytes()).await;
    let mut out = Vec::new();
    match tokio::time::timeout(BUDGET, s.read_to_end(&mut out)).await {
        // EOF, or RST (a close with our unread request still in the server's
        // receive buffer): either way, `out` holds whatever arrived first.
        Ok(Ok(_)) | Ok(Err(_)) => out,
        Err(_) => panic!(
            "the server neither answered nor closed within {BUDGET:?} ({} bytes so far)",
            out.len()
        ),
    }
}

#[tokio::test]
async fn plain_listener_serves_http_connect_info_and_websocket() {
    let addr = spawn_plain(fauna_conn_limit::LOOPBACK_MAX_CONNS).await;

    let resp = String::from_utf8_lossy(&raw_get(addr, "/health").await).into_owned();
    assert!(
        resp.starts_with("HTTP/1.1 200") && resp.ends_with("ok"),
        "plain HTTP is served through the shared loop: {resp:?}"
    );

    let resp = String::from_utf8_lossy(&raw_get(addr, "/peer").await).into_owned();
    assert!(
        resp.starts_with("HTTP/1.1 200") && resp.ends_with("127.0.0.1"),
        "ConnectInfo is injected on the plain flavour (the loopback gate's input): {resp:?}"
    );

    // The upgrade path — the very regression the TLS flavour once had
    // (`tls_websocket_upgrade.rs`); the plain flavour no longer rides
    // `axum::serve`, so it needs the same proof.
    let mut ws = hold_ws(addr).await;
    ws.send(Message::Text("ping-over-plain".into()))
        .await
        .expect("send WS frame");
    let echoed = tokio::time::timeout(BUDGET, ws.next())
        .await
        .expect("echo within budget")
        .expect("a frame, not end-of-stream")
        .expect("a frame, not a WS error");
    assert_eq!(
        echoed.into_text().unwrap().as_str(),
        "ping-over-plain",
        "WebSocket upgrades are driven on the plain flavour"
    );
}

#[tokio::test]
async fn loopback_is_shed_past_its_ceiling_on_the_plain_listener() {
    let addr = spawn_plain(2).await;

    // Two admitted connections fill the loopback ceiling, and their permits
    // are held (each 101 was awaited) before the third is attempted.
    let held_a = hold_ws(addr).await;
    let _held_b = hold_ws(addr).await;

    let resp = raw_get(addr, "/health").await;
    assert!(
        resp.is_empty(),
        "the connection past the loopback ceiling must be shed unanswered, but the \
         nest admitted it and answered {:?} — the 2026-08-22 leak shape is unbounded again",
        String::from_utf8_lossy(&resp)
    );

    // Releasing one held connection frees its slot. The server-side task ends
    // asynchronously after our close, so this is a deadline poll on the state
    // "a fresh connection is served", not a wait for a duration.
    drop(held_a);
    let deadline = Instant::now() + BUDGET;
    loop {
        let resp = raw_get(addr, "/health").await;
        if String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a released loopback slot was not re-admittable within {BUDGET:?}"
        );
        // Poll interval only — the assertion above is on state, not on time.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
