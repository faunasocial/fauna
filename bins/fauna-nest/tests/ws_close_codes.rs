//! Integration test — the nest actually emits its documented WS close codes
//! (`transport.md` § Connection lifecycle → Close codes).
//!
//! Two rows of that table shipped with Spec Y and had **no nest-side producer**
//! until 2026-07-31: `4400` (invalid frame / protocol violation) and `1011`
//! (internal error / Reply-channel overflow). The dispatch loop simply `break`ed
//! and `run_connection` aborted the send task, so a client that had just sent
//! garbage saw the same abrupt drop as a network blip. The doc said so in as
//! many words ("Nest never actually emits 4400, 1011, or 4426 as a WS close
//! frame today"); this file is what lets that sentence be deleted for the first
//! two.
//!
//! **What is asserted here, and what is asserted elsewhere.** These tests pin
//! the *wire* half: a committed fatal reason reaches a real client as that exact
//! `u16`. They drive it through the three protocol-violation shapes, which are
//! reachable with one frame and therefore fully deterministic — no sleeps, no
//! volume, nothing load-dependent (convention 14).
//!
//! The `1011` **trigger** is pinned separately, in `ws.rs`'s own unit tests
//! (`reply_overflow_on_{finish,send_error,idempotent_replay}_signals_fatal_1011`),
//! by filling the bounded outbound channel to its stated `WS_OUTBOUND_BOUND`.
//! That split is deliberate rather than a gap: reaching a Reply overflow *over a
//! socket* would mean out-writing the kernel's TCP buffer against a peer that
//! has stopped reading — a volume-and-timing race that gives no trustworthy
//! verdict on a heavily loaded build machine. Both halves meet at
//! `close_fatal`, the single function that turns a `FatalCloseReason` into a
//! frame, and it is covered end-to-end below by the 4400 cases. There is no
//! untested link between "the overflow is detected" and "1011 goes out".

mod common;
use common::{Ws, open_authed};

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{Frame, Reply, Value};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

struct Harness {
    url: String,
    kp: ActorKeypair,
    token: String,
    state: Arc<fauna_nest::routes::AppState>,
}

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let state = Arc::new(fauna_nest::routes::AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
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

/// Deadline-poll helper. Green runs return on the first or second pass and pay
/// nothing; the budget only has to exceed scheduling jitter on a loaded box, so
/// it is sized far above any non-pathological delay rather than tuned.
async fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Read until a Close frame arrives, returning its code. `None` means the stream
/// ended (or errored) with no close frame at all — which is precisely the
/// pre-fix behavior, so a `None` here is a real failure signal, not a flake.
///
/// The budget is generous on purpose: a green run never pays it (the close
/// frame is one small write on an already-open socket), and sizing it far above
/// any non-pathological scheduling delay is what keeps the verdict trustworthy
/// under a loaded machine rather than merely fast on an idle one.
async fn close_code(ws: &mut Ws) -> Option<u16> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Err(_) => return None,
            Ok(None) | Ok(Some(Err(_))) => return None,
            Ok(Some(Ok(Message::Close(frame)))) => return frame.map(|f| u16::from(f.code)),
            // Anything else (a stray push) is not what this test is about.
            Ok(Some(Ok(_))) => {}
        }
    }
}

fn reply_frame_from_client() -> Message {
    // A Reply is a nest→client frame. A client sending one is the canonical
    // protocol violation of `transport.md` § Frame routing step 1.
    let frame = Frame::Reply(Reply {
        ty: Reply::TYPE,
        correlation_id: 1,
        payload: Value::Null,
        ok: true,
    });
    Message::Binary(fauna_protocol::encode_frame(&frame).unwrap())
}

#[tokio::test]
async fn reply_frame_from_client_closes_with_4400() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;

    ws.send(reply_frame_from_client()).await.unwrap();

    assert_eq!(
        close_code(&mut ws).await,
        Some(4400),
        "a Reply frame from the client must close 4400, not drop the socket \
         silently — an admin reading nest logs or a wire capture has to be \
         able to tell 'this client is buggy' from 'the network blipped'"
    );
}

#[tokio::test]
async fn text_frame_from_client_closes_with_4400() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;

    // Spec Y is binary-only; a text frame is a violation regardless of content.
    ws.send(Message::Text("hello".into())).await.unwrap();

    assert_eq!(close_code(&mut ws).await, Some(4400));
}

#[tokio::test]
async fn undecodable_binary_frame_closes_with_4400() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;

    ws.send(Message::Binary(Bytes::from_static(&[
        0xff, 0xff, 0xff, 0xff,
    ])))
    .await
    .unwrap();

    assert_eq!(close_code(&mut ws).await, Some(4400));
}

/// The negative control: an ordinary disconnect must NOT be labelled a
/// violation. Without it, signalling unconditionally on every `break` in the
/// dispatch loop would pass all three tests above while mislabelling every
/// clean disconnect in the fleet as a client bug — a 4400 on the wire and a
/// "protocol violation" warning in the logs for routine traffic, which is worse
/// than the silence this whole change set out to fix.
///
/// **It asserts nest-side, deliberately.** The obvious wire-side form — close
/// from the client, then read the code — is *decorative*: measured, tungstenite
/// completes the close handshake client-side and yields `None`, so the server's
/// frame is never observed and the assertion passes against any behavior at
/// all. A mutation adding `signal_fatal` to the `Message::Close` arm went
/// undetected by exactly that shape. Reading the connection's own verdict is
/// what makes this pin bite.
#[tokio::test]
async fn a_clean_client_close_is_not_reported_as_a_violation() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    let actor = h.kp.actor_id().0;

    // Hold the Arc: it outlives removal from the registry, so the verdict is
    // still readable after teardown.
    let ws_state = &h.state.ws;
    let conn = poll_until("the connection to register", || {
        ws_state.connections_for(&actor).into_iter().next()
    })
    .await;
    assert_eq!(conn.fatal_close(), None, "healthy before the close");

    ws.close(None).await.unwrap();

    // Causal barrier rather than a settle-sleep: deregistration is what proves
    // the dispatch loop broke and `run_connection`'s teardown branch ran — the
    // exact window in which a mislabelling would be committed. Asserting before
    // it would race the very code under test.
    poll_until("the connection to deregister", || {
        (!ws_state.has_connections(&actor)).then_some(())
    })
    .await;

    assert_eq!(
        conn.fatal_close(),
        None,
        "a client-initiated close is an ordinary disconnect, not a protocol \
         violation and not an internal error"
    );
}
