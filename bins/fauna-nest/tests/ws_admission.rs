//! Integration test — per-connection Request admission, through the real
//! reader loop (`routes::run_connection`), over a real socket.
//!
//! `transport.md` § Request lifecycle, step 1: the reader reserves one place
//! in the connection's outbound queue for each Request's Reply before anything
//! else runs, so a connection has at most `WS_OUTBOUND_BOUND` requests in
//! flight and a Reply never meets a full queue on a draining connection
//! (§ Backpressure). A Request reusing a correlation id still in flight is a
//! protocol violation, closed 4400.
//!
//! `ws.rs`'s unit tests pin the slot mechanics by calling `hold_reply_slot`
//! directly; nothing there reaches the reader's admission call, so deleting it
//! kept every test green. Each arm here reddens under exactly one mutation:
//!
//! - **Arm 1** (`a_reply_admitted_before_a_push_burst_goes_out_through_its_slot`)
//!   reddens when the admission lines (`admit_reply_slot` + `hold_reply_slot`)
//!   are deleted from `run_connection`: the Replies then `try_send` into a queue
//!   the burst filled, and the connection closes 1011.
//! - **Arm 2** (`a_request_reusing_an_in_flight_correlation_id_closes_4400`)
//!   reddens when the duplicate refusal is deleted: the second Request is then
//!   admitted and no close comes.
//!
//! **Determinism.** Every wait is on a causal signal, not a sleep: the handlers
//! count themselves in and park on a semaphore the test owns, so "the Requests
//! are admitted and in flight" is observed, not assumed; the queue is filled by
//! `try_send` until it answers `Full`, which is the very condition under test
//! and needs no timing (the client reads nothing meanwhile, so once the
//! kernel's socket buffers take what they take, the writer blocks and the queue
//! fills). A push is a `try_send` into the same queue (`RpcConnection::ws_tx`),
//! so the burst is sent the way a push is.

mod common;
use common::{Ws, open_authed};

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::rpc_router::{RpcKindMeta, RpcRouter};
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{
    Frame, Request, RpcError, Value, decode_frame, encode_canonical, encode_frame,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::Semaphore;
use tokio::sync::mpsc::error::TrySendError;
use tokio_tungstenite::tungstenite::Message;

/// A real, User-class kind: a synthetic one is refused by the central
/// capability gate before the handler runs (see `cancel_dispatch.rs`). Only
/// the routing matters; the body is ours.
const HELD_KIND: &str = "fauna.contacts.list";

/// One burst frame. Large so the kernel's loopback buffers fill after a few
/// dozen frames and the writer blocks; one shared allocation, cloned.
const BURST_FRAME_BYTES: usize = 256 * 1024;

/// A budget far above any non-pathological scheduling delay; a green run
/// never pays it.
const BUDGET: Duration = Duration::from_secs(20);

struct Harness {
    url: String,
    kp: ActorKeypair,
    token: String,
    state: Arc<fauna_nest::routes::AppState>,
    /// Handlers that have started (and are now parked on `release`).
    started: Arc<AtomicUsize>,
    /// One permit lets one parked handler finish.
    release: Arc<Semaphore>,
}

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    let started = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));
    let (h_started, h_release) = (Arc::clone(&started), Arc::clone(&release));
    let mut b = RpcRouter::builder();
    b.add(
        HELD_KIND,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(120),
            handler: Box::new(move |_, _, _| {
                let started = Arc::clone(&h_started);
                let release = Arc::clone(&h_release);
                Box::pin(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    release.acquire().await.unwrap().forget();
                    Ok::<Bytes, RpcError>(encode_canonical(&Value::Null).unwrap())
                })
            }),
        },
    );
    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new(b.build()),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
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
        started,
        release,
    }
}

fn request(corr: u64) -> Message {
    let mut idem = [0u8; 16];
    getrandom::fill(&mut idem).unwrap();
    Message::Binary(
        encode_frame(&Frame::Request(Request {
            ty: Request::TYPE,
            correlation_id: corr,
            kind: HELD_KIND.into(),
            idempotency_key: idem,
            payload: Value::Null,
            replay_forbidden: None,
            deadline_ms: None,
        }))
        .unwrap(),
    )
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + BUDGET;
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn the_connection(h: &Harness) -> Arc<fauna_nest::ws::RpcConnection> {
    let actor = h.kp.actor_id().0;
    let deadline = tokio::time::Instant::now() + BUDGET;
    loop {
        if let Some(c) = h.state.ws.connections_for(&actor).into_iter().next() {
            return c;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the connection to register"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Read until a Close frame arrives; `None` = the stream ended without one or
/// the budget ran out.
async fn close_code(ws: &mut Ws) -> Option<u16> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Err(_) | Ok(None) | Ok(Some(Err(_))) => return None,
            Ok(Some(Ok(Message::Close(frame)))) => return frame.map(|f| u16::from(f.code)),
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Arm 1 — the admission wiring. Requests pipelined onto the connection are
/// admitted and parked in their handlers; a push burst then fills every place
/// in the outbound queue the Requests do not hold. When the handlers finish,
/// each Reply must go out through the slot its Request reserved at admission —
/// so all of them arrive and the connection is not closed 1011.
#[tokio::test]
async fn a_reply_admitted_before_a_push_burst_goes_out_through_its_slot() {
    const PIPELINED: u64 = 4;
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    let conn = the_connection(&h).await;

    for corr in 1..=PIPELINED {
        ws.send(request(corr)).await.unwrap();
    }
    wait_until("every pipelined handler to start", || {
        h.started.load(Ordering::SeqCst) == PIPELINED as usize
    })
    .await;

    // The burst: fill the queue until it refuses. The client reads nothing, so
    // once the socket buffers are full the writer blocks and `Full` comes.
    let burst = Bytes::from(vec![0u8; BURST_FRAME_BYTES]);
    let deadline = tokio::time::Instant::now() + BUDGET;
    let mut queued = 0usize;
    loop {
        match conn.ws_tx.try_send(burst.clone()) {
            Ok(()) => queued += 1,
            Err(TrySendError::Full(_)) => break,
            Err(TrySendError::Closed(_)) => panic!("outbound queue closed during the burst"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the outbound queue never filled ({queued} burst frames queued)"
        );
        tokio::task::yield_now().await;
    }

    h.release.add_permits(PIPELINED as usize);

    // Drain: every Reply must arrive, behind the burst; a Close is the failure.
    let mut replied = std::collections::BTreeSet::new();
    let deadline = tokio::time::Instant::now() + BUDGET;
    while replied.len() < PIPELINED as usize {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(remaining, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out with Replies {replied:?} of 1..={PIPELINED}"));
        match msg {
            Some(Ok(Message::Binary(b))) => {
                if let Ok(Frame::Reply(r)) = decode_frame(&b) {
                    replied.insert(r.correlation_id);
                }
            }
            Some(Ok(Message::Close(frame))) => panic!(
                "connection closed {:?} with Replies {replied:?} of 1..={PIPELINED} out — \
                 a Reply admitted before the burst met a full queue instead of its slot",
                frame.map(|f| u16::from(f.code))
            ),
            Some(Ok(_)) => {}
            other => panic!("stream ended ({other:?}) with Replies {replied:?}"),
        }
    }
    assert_eq!(
        conn.fatal_close(),
        None,
        "no fatal close on a draining connection"
    );
}

/// Arm 2 — the duplicate refusal. A Request at a correlation id whose earlier
/// Request is still in flight is a protocol violation: the connection closes
/// 4400 instead of admitting it over the earlier one's slot.
#[tokio::test]
async fn a_request_reusing_an_in_flight_correlation_id_closes_4400() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;

    ws.send(request(9)).await.unwrap();
    wait_until("the first handler to start", || {
        h.started.load(Ordering::SeqCst) == 1
    })
    .await;

    ws.send(request(9)).await.unwrap();
    let code = close_code(&mut ws).await;
    h.release.add_permits(2);
    assert_eq!(
        code,
        Some(4400),
        "a Request reusing an in-flight correlation id must close 4400; admitting it \
         replaces the earlier Request's Reply slot, so a peer reusing one id never parks"
    );
}
