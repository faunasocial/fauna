//! Integration test — the **degraded** ("needs-update") listener drives the same
//! server half of the WS heartbeat as the normal boot
//! (`transport.md` § Connection lifecycle → Heartbeat).
//!
//! **Why this path needs it at all.** Degraded mode is the one a nest sits in
//! *longest* unattended: it boots there when the on-disk DB was written by a
//! newer binary (`version-compatibility.md` § 2.2) and stays there until a human
//! deploys a newer nest. Meanwhile every client in the deployment reconnects
//! against it forever, on a loop, to discover whether the box is back. Without a
//! server heartbeat each of those sockets — and the per-IP permit `serve_tls`
//! took for it (`libs/fauna-conn-limit`) — survives a vanished peer until the
//! kernel's TCP keepalive gets around to it. The peer population is the same one
//! `routes::run_connection` already serves (our own apps, native and browser),
//! so the same answer applies: a Ping, never a bare idle timeout.
//!
//! Neither test asserts on wall-clock timing (convention 14). The positive case
//! polls for a *state* — the socket reached EOF — under a budget far above any
//! non-pathological delay; the negative case anchors to a causal barrier, server
//! Pings actually counted on the wire.

use std::time::Duration;

use fauna_nest::ws::WsHeartbeatPolicy;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

/// Compressed heartbeat. The *ratio* mirrors production (timeout = 2× interval);
/// the absolute values only have to be small enough to keep the run quick and
/// large enough that scheduling jitter on a loaded box cannot starve a Ping for
/// a whole window.
const PING_INTERVAL: Duration = Duration::from_millis(200);
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(400);

async fn start() -> String {
    let app = fauna_nest::degraded_serve::degraded_router_with_heartbeat(WsHeartbeatPolicy {
        ping_interval: PING_INTERVAL,
        liveness_timeout: LIVENESS_TIMEOUT,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("ws://{addr}/api/v1/ws")
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(url: &str) -> Ws {
    let mut req = url.to_string().into_client_request().unwrap();
    req.headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, "fauna.v1".parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("degraded upgrade should succeed");
    ws
}

/// A peer that never answers a Ping is reaped, and the socket under it released.
///
/// The vanished-peer shape is reproduced exactly: the WebSocket is never polled
/// again, so tungstenite never gets to emit its automatic Pong. Reading instead
/// from the **raw** stream underneath it consumes the server's Ping bytes
/// without answering them — which is what a suspended VM or a dropped link looks
/// like from the nest's side — and lets the test observe the teardown directly,
/// as EOF on the TCP connection. Dropping the socket would send a FIN and tear
/// the connection down for the ordinary reason, proving nothing.
#[tokio::test]
async fn a_degraded_peer_that_never_answers_a_ping_is_reaped() {
    let url = start().await;
    let mut ws = open(&url).await;
    // From here on `ws` is never polled as a Stream: no Pong will ever be sent.
    let raw = ws.get_mut();

    let mut buf = [0u8; 256];
    let eof = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match raw.read(&mut buf).await {
                // EOF: the nest closed the connection on the dead link.
                Ok(0) => return true,
                // Server Pings we deliberately do not answer.
                Ok(_) => continue,
                // A reset counts too — the socket is gone either way.
                Err(_) => return true,
            }
        }
    })
    .await;

    assert!(
        eof.is_ok(),
        "the degraded listener never closed a connection that answered no Ping for many \
         liveness windows; its sockets outlive their peers"
    );
}

/// A peer that answers Pings but never *sends* anything is left alone — the
/// browser case, and the reason this is a Ping rather than an idle timeout.
///
/// The barrier is causal, not temporal: server Pings are counted as they arrive
/// and the assertion only fires once enough have landed to span more than a full
/// liveness window. Reading the stream is also what makes this client
/// "responsive" — tungstenite queues the mandatory Pong when it reads a Ping —
/// so the loop below *is* the browser's below-JS auto-answer, modelled
/// faithfully. At no point does it send an application frame.
#[tokio::test]
async fn a_silent_but_responsive_degraded_peer_is_never_reaped() {
    let url = start().await;
    let mut ws = open(&url).await;

    let pings_needed = 5;
    let mut pings_seen = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while pings_seen < pings_needed {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "only saw {pings_seen} of {pings_needed} server Pings before the budget ran out; \
             the degraded listener is not pinging its clients"
        );
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Ping(_)))) => pings_seen += 1,
            // The failure this test exists to catch: an idle-timeout
            // implementation reaping a peer that answered every single Ping.
            Ok(Some(Ok(Message::Close(frame)))) => panic!(
                "the degraded listener closed a responsive-but-silent (browser-shaped) \
                 connection after {pings_seen} Pings: {frame:?}"
            ),
            Ok(Some(Ok(_))) => {} // stray frame; not what this test is about
            Ok(Some(Err(e))) => panic!("transport error on a responsive peer: {e}"),
            Ok(None) => panic!("stream ended on a responsive-but-silent peer"),
            Err(_) => panic!(
                "only saw {pings_seen} of {pings_needed} server Pings before the budget ran out"
            ),
        }
    }

    // Still *serving*, not merely still open: a Request placed after all those
    // idle windows is answered with the `fauna.nest.outdated` signal, which is
    // the whole point of the mode. A reaped connection fails here instead.
    let req = fauna_protocol::Frame::Request(fauna_protocol::Request {
        ty: fauna_protocol::Request::TYPE,
        correlation_id: 42,
        kind: "fauna.nest.info".into(),
        idempotency_key: [0u8; 16],
        payload: fauna_protocol::Value::Map(Default::default()),
        replay_forbidden: None,
        deadline_ms: None,
    });
    ws.send(Message::Binary(fauna_protocol::encode_frame(&req).unwrap()))
        .await
        .expect("a peer that answered every Ping must still be able to send");

    let reply = loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("the degraded listener must answer a Request after an idle spell")
        {
            Some(Ok(Message::Binary(bytes))) => break bytes,
            Some(Ok(Message::Ping(_))) => continue, // the heartbeat, still running
            other => panic!(
                "expected the outdated Reply on a connection that answered {pings_seen} Pings, \
                 got {other:?}"
            ),
        }
    };
    match fauna_protocol::decode_frame(&reply).unwrap() {
        fauna_protocol::Frame::Reply(r) => {
            assert_eq!(r.correlation_id, 42);
            assert!(!r.ok, "degraded mode answers every Request with an error");
        }
        other => panic!("expected Reply, got {other:?}"),
    }
}

/// The degraded upgrade bounds its frames exactly as every other nest upgrade
/// does — an oversize **valid** Request is refused at the WebSocket layer,
/// before `decode_frame` ever allocates it.
///
/// **Why this needs its own test rather than a glance at the call.** A
/// *malformed* oversize frame is not an observable: `handle_ws_outdated` closes
/// the socket on an undecodable frame anyway, so a bounded and an unbounded
/// upgrade look identical from the client. The size bound only becomes visible
/// on a frame that is *well-formed* and merely too large — bounded, the socket
/// closes with no Reply; unbounded, the server buffers the whole thing and
/// answers `fauna.nest.outdated`. That is the observable asserted here.
///
/// Degraded mode is the state a nest sits in *longest* unattended and it
/// authenticates nobody (`ws_outdated` checks no bearer — there is no DB to
/// check one against), so an unbounded frame here is reachable pre-auth by any
/// peer that can open a socket.
///
/// Convention 14: no wall-clock assertion. The refusal is observed as *state* —
/// a Reply arrived or the stream ended — under a budget far above any
/// non-pathological delay on loopback.
#[tokio::test]
async fn an_oversize_request_is_refused_at_the_websocket_layer() {
    // A heartbeat far longer than the test, so a reaped-for-silence close can
    // never be mistaken for the size refusal under test.
    let url = start_with(WsHeartbeatPolicy {
        ping_interval: Duration::from_secs(30),
        liveness_timeout: Duration::from_secs(60),
    })
    .await;

    // Control: an in-bounds Request is answered, so the harness is known good
    // and the refusal below can only be about size.
    let mut ws = open(&url).await;
    ws.send(sized_request(0, 16)).await.unwrap();
    assert!(
        next_reply(&mut ws).await.is_some(),
        "a small Request must still be answered with fauna.nest.outdated"
    );

    // The frame under test: well-formed, one byte of payload past the cap every
    // other nest upgrade sets (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`).
    let mut ws = open(&url).await;
    let over = fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE + 1;
    // A send error is itself a refusal (the peer closed under us mid-write).
    let _ = ws.send(sized_request(1, over)).await;
    assert!(
        next_reply(&mut ws).await.is_none(),
        "an oversize Request must be refused at the WS layer, not decoded and \
         answered — an upgrade that sets no max_message_size/max_frame_size \
         accepts frames every other nest upgrade rejects"
    );
}

/// [`start`] with an explicit heartbeat policy.
async fn start_with(heartbeat: WsHeartbeatPolicy) -> String {
    let app = fauna_nest::degraded_serve::degraded_router_with_heartbeat(heartbeat);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("ws://{addr}/api/v1/ws")
}

/// A well-formed `Frame::Request` whose payload carries `payload_bytes` bytes.
fn sized_request(corr: u64, payload_bytes: usize) -> Message {
    let frame = fauna_protocol::Frame::Request(fauna_protocol::Request {
        ty: fauna_protocol::Request::TYPE,
        correlation_id: corr,
        kind: "fauna.nest.describe".to_string(),
        idempotency_key: [7u8; 16],
        payload: fauna_protocol::Value::Bytes(vec![0u8; payload_bytes]),
        replay_forbidden: None,
        deadline_ms: None,
    });
    Message::Binary(fauna_protocol::encode_frame(&frame).unwrap())
}

/// The next Reply on the socket, or `None` if the stream ends / errors first.
async fn next_reply(ws: &mut Ws) -> Option<fauna_protocol::Reply> {
    let deadline = Duration::from_secs(20);
    loop {
        let msg = match tokio::time::timeout(deadline, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            // Stream ended, errored, or nothing at all inside a budget far above
            // any non-pathological loopback delay: no Reply is coming.
            _ => return None,
        };
        if let Message::Binary(b) = msg
            && let Ok(fauna_protocol::Frame::Reply(r)) = fauna_protocol::decode_frame(&b)
        {
            return Some(r);
        }
    }
}
