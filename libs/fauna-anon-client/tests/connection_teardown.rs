//! Dropping an [`AnonymousNestClient`] must close its WebSocket.
//!
//! The type's own contract says so twice — the `_driver` field is documented
//! "aborted on drop (so dropping the client closes the WS)", and
//! `graduate_handshake`'s § Connection-teardown rule tells callers to *drop the
//! client on `Err`* precisely as the way to tear a connection down. Neither held:
//! the driver was stored as a bare `JoinHandle`, and **dropping a `JoinHandle`
//! detaches its task rather than aborting it**, so the task kept running with the
//! whole adapter — and the socket — alive for the life of the process.
//!
//! This is the mint path. `WsDeviceHandshakeBearer::fetch_token` opens one
//! anonymous client per bearer refresh, so a client reconnecting in a loop leaked
//! one permanently-`ESTABLISHED` socket per attempt, on both ends, with no owner
//! anywhere that could close it. That is the accumulation behind the 2026-08-22
//! mac incident: ~16,300 sockets held for four and a half hours until the VM
//! could open no new outbound connection at all.
//!
//! Asserts state, not timing (convention 14): the server side awaits its stream
//! actually ending, under a budget far above any scheduling delay. A leaked
//! socket never ends, so no budget makes this pass by luck.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::TcpListener;

use fauna_anon_client::AnonymousNestClient;

/// Generous relative to a loopback close (microseconds); a leak never satisfies
/// it at any budget.
const CLOSE_BUDGET: Duration = Duration::from_secs(10);

#[tokio::test]
async fn dropping_the_client_closes_its_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    // A minimal nest-shaped WS peer: accept the upgrade, then read until the
    // client hangs up. `saw_close` resolves only when the stream genuinely ends.
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        // The client requires the `fauna.v1` subprotocol to be echoed, exactly
        // as the nest echoes it, or it refuses the upgrade.
        // result_large_err: the callback's Err type is tungstenite's own
        // `ErrorResponse` (fixed by the `Callback` trait signature) — same
        // rationale as `fauna-anon-client/src/tls_dial.rs`.
        #[allow(clippy::result_large_err)]
        let echo_fauna_subprotocol =
            |_req: &tokio_tungstenite::tungstenite::handshake::server::Request,
             mut res: tokio_tungstenite::tungstenite::handshake::server::Response| {
                res.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    tokio_tungstenite::tungstenite::http::HeaderValue::from_static("fauna.v1"),
                );
                Ok(res)
            };
        let mut ws = tokio_tungstenite::accept_hdr_async(stream, echo_fauna_subprotocol)
            .await
            .expect("ws upgrade");
        // Drain until the peer closes. Returns on end-of-stream, which is the
        // observable under test.
        while let Some(msg) = ws.next().await {
            if msg.is_err() {
                break;
            }
        }
    });

    let client = AnonymousNestClient::connect(&format!("http://127.0.0.1:{port}"))
        .await
        .expect("anonymous connect");

    // The whole test: let it go, exactly as every mint path does.
    drop(client);

    let closed = tokio::time::timeout(CLOSE_BUDGET, server).await;
    assert!(
        closed.is_ok(),
        "the server never saw the WebSocket close within {CLOSE_BUDGET:?} after the client was \
         dropped — the connection's driver task outlived its client and is still holding the \
         socket. Every bearer mint opens one of these, so a reconnecting client leaks one \
         permanently-ESTABLISHED socket per attempt (the 2026-08-22 mac network exhaustion)"
    );
    closed.unwrap().expect("server task panicked");
}
