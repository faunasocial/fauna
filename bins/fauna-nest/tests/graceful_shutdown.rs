//! Integration test — graceful shutdown (transport.md § Graceful shutdown).
//!
//! Three load-bearing assertions for a Watchtower-style redeploy (the old
//! nest gets SIGTERM, a fresh one boots on the same `/data`):
//!
//!  1. **Planned shutdown closes every WS with 1001 (Going Away), never 1000.**
//!     `1001` maps to `ReconnectSignal::Retry` on the client (the supervisor
//!     reconnects with backoff); `1000` maps to `CleanDisconnect`, which would
//!     **stop the reconnect loop on every redeploy** — the latent footgun this
//!     guards. A clean 1001 also lets the client detect the drop instantly
//!     instead of waiting for its dead-link timeout.
//!  2. **An in-flight request finishes during the drain** rather than being cut
//!     off at the swap instant: a non-idempotent write that is mid-handler when
//!     SIGTERM lands runs to completion and its Reply is delivered *before* the
//!     1001 close.
//!  3. **Step 1 itself — "stop accepting new connections" — actually stops the
//!     accept loop, and does so BEFORE the drain window, not merely by the
//!     time the whole sequence returns.** From 2026-06-08 to 2026-08-30 the
//!     accept loop did not stop at all: `main.rs` used `drop(handle)`, which
//!     detaches a `tokio::task::JoinHandle` rather than aborting it, so the
//!     accept loop kept running until process exit. Fixed to `handle.abort()` + await, then
//!     lifted lib-side as `fauna_nest::graceful_shutdown` so this file can
//!     link and drive it — a `bins/fauna-nest/tests/` integration test can
//!     never link `main.rs`, which is what let the original defect through
//!     every gate for 12 weeks. The *ordering* of abort-before-drain was itself unpinned by any
//!     test until row 514: nothing stopped a
//!     future edit from moving the abort below the drain loop, which would
//!     leave the nest accepting brand-new connections for up to
//!     `GRACEFUL_SHUTDOWN_TIMEOUT` after broadcasting the 1001 going-away.
//!
//! (1) and (2) drive `WsState::begin_shutdown()` directly (the in-process
//! stand-in for the SIGTERM path in `main.rs`, which calls exactly this). (3)
//! drives `fauna_nest::graceful_shutdown` itself — the exact function
//! `main.rs::cmd_serve`'s SIGTERM path calls, not a stand-in for it — since
//! the bug was in the accept loop's own lifecycle. The real `docker stop` →
//! SIGTERM → 1001 path is proven by the tier_4 docker test.

mod common;
use common::{open_authed, read_until_close};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::rpc_router::{RpcKindMeta, RpcRouter};
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{Frame, Request, Value, encode_canonical};
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

/// The kind the slow write is dispatched under. It **must be a real kind that
/// `bridge_method_allowlist::is_permitted` grants to `CallerClass::User`** —
/// `fauna.posts.create` is exactly that, and is genuinely non-idempotent, which
/// is what this test is about.
///
/// A synthetic name (this was `fauna.test.slow_write`) is refused by the central
/// capability gate in `routes.rs`, whose `is_permitted` has a `_ => false`
/// default. That gate landed with the suspension fix and silently
/// hollowed this test out: the dispatch was answered
/// `fauna.bridges.permission_denied` before the handler ever ran, so "an
/// in-flight write finished during the drain" was being asserted against a write
/// that never started. Its 1001-vs-1000 sibling kept passing because it never
/// dispatches anything. Keep this a real, allowlisted, User-class kind.
const SLOW_WRITE_KIND: &str = "fauna.posts.create";

/// A `completed` flag the slow-write handler flips once it has finished its
/// (simulated) non-idempotent write. The test asserts the drain let it run to
/// completion after shutdown was signalled.
struct Harness {
    url: String,
    kp: ActorKeypair,
    token: String,
    state: Arc<fauna_nest::routes::AppState>,
    write_completed: Arc<AtomicBool>,
}

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let token = tokens.insert(kp.actor_id(), 3600).await;

    // A deliberately slow, non-idempotent write handler. It sleeps long enough
    // that shutdown can be signalled while it is mid-flight, then flips the
    // completion flag and returns a valid reply.
    let write_completed = Arc::new(AtomicBool::new(false));
    let completed_for_handler = Arc::clone(&write_completed);

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            b.add(
                SLOW_WRITE_KIND,
                RpcKindMeta {
                    // Non-idempotent: the client must not silently auto-retry.
                    forbid_replay: true,
                    default_deadline: Duration::from_secs(10),
                    handler: Box::new(move |_state, _actor, _payload| {
                        let completed = Arc::clone(&completed_for_handler);
                        Box::pin(async move {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            completed.store(true, Ordering::SeqCst);
                            Ok(Bytes::from(encode_canonical(&true).unwrap().to_vec()))
                        })
                    }),
                },
            );
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
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
        write_completed,
    }
}

fn slow_write_frame(corr: u64) -> Message {
    let frame = Frame::Request(Request {
        ty: Request::TYPE,
        correlation_id: corr,
        kind: SLOW_WRITE_KIND.to_string(),
        idempotency_key: [corr as u8; 16],
        payload: Value::Null,
        replay_forbidden: Some(true),
        deadline_ms: None,
    });
    Message::Binary(encode_frame_bytes(&frame))
}

fn encode_frame_bytes(frame: &Frame) -> Bytes {
    fauna_protocol::encode_frame(frame).unwrap()
}

#[tokio::test]
async fn planned_shutdown_emits_close_1001_never_1000() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    // Let the server-side subscription land.
    tokio::time::sleep(Duration::from_millis(50)).await;

    h.state.ws.begin_shutdown();

    let (code, _) = read_until_close(&mut ws, None).await;
    let code = code.expect("server must send a Close frame on planned shutdown");
    assert_eq!(
        code, 1001,
        "planned shutdown must close with 1001 (Going Away), never 1000 (which stops the client's reconnect loop)"
    );
    assert_ne!(
        code, 1000,
        "1000 would stop the client reconnect loop forever"
    );
}

#[tokio::test]
async fn in_flight_write_completes_during_drain_then_close_1001() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fire a slow non-idempotent write, then signal shutdown while it is still
    // mid-handler (the 300ms sleep outlasts this 80ms gap).
    ws.send(slow_write_frame(7)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    h.state.ws.begin_shutdown();

    let (code, reply_seen) = read_until_close(&mut ws, Some(7)).await;

    assert!(
        h.write_completed.load(Ordering::SeqCst),
        "the in-flight write must run to completion during the drain, not be cut off at the swap"
    );
    assert!(
        reply_seen,
        "the in-flight request's Reply must be delivered before the 1001 close"
    );
    assert_eq!(
        code.expect("a Close frame must still follow the drained reply"),
        1001,
        "the drained connection must then close with 1001"
    );
}

/// The accept loop's own lifecycle, isolated from any other app/WS logic —
/// the regression this file's other two tests could not catch, since both
/// drive `begin_shutdown()` directly and never touch how the accept loop
/// itself is stopped. Drives `fauna_nest::graceful_shutdown` — the **exact
/// production function** `main.rs::cmd_serve`'s SIGTERM path calls, not a
/// stand-in for it: the shutdown sequence lives lib-side precisely so this
/// test links and exercises it verbatim (a `bins/fauna-nest/tests/`
/// integration test can never link the `main.rs` bin target, which is why
/// the sequence moved).
///
/// Step 1 IS step 1 (`transport.md` § Graceful shutdown): the accept loop
/// must already be stopped *before* the drain window ends, not merely by the
/// time the whole sequence returns. A version of this test that checked only
/// "eventually stops" against a `WsState::new()` with `connection_count() ==
/// 0` throughout could not tell "aborted before the drain" from "aborted
/// after" — its drain loop exits on the first predicate check, so a
/// `handle.abort()` moved below the drain loop left it, and both of this
/// file's other tests, green.
///
/// This version registers a connection directly via `WsState::subscribe` —
/// no real network client — so `connection_count()` is 1 under this test's
/// own control (never decremented until it calls `remove`) rather than at
/// the mercy of a real WS library's own close-handshake timing, which has no
/// bearing on the property under test and previously made the drain
/// window's length nondeterministic. The causal barrier (convention 14) is
/// `WsState::is_shutting_down()`, which flips true the instant
/// `begin_shutdown()` runs inside `graceful_shutdown` — unlike
/// `connection_count() > 0`, which is already true *before* shutdown starts
/// too and so cannot mark the moment drain begins. The probe fires the
/// instant that flag flips, while `connection_count()` is still
/// (deterministically) 1. Reordering `handle.abort()` below the drain in
/// `graceful_shutdown` reddens this test alone: the accept loop would still
/// be live at that instant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_shutdown_stops_accepting_before_the_drain_completes() {
    async fn health() -> &'static str {
        "ok"
    }
    let router = axum::Router::new().route("/health", axum::routing::get(health));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(fauna_nest::serve_plain(
        listener,
        router.into_make_service(),
        fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP),
    ));

    // Sanity: the accept loop is live before shutdown.
    tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr))
        .await
        .expect("connect within budget")
        .expect("accept loop must be live before graceful_shutdown()");

    let ws_state = Arc::new(fauna_nest::ws::WsState::new());
    let (conn, _rx) = ws_state.subscribe([7u8; 32]);
    assert_eq!(
        ws_state.connection_count(),
        1,
        "the directly-registered connection must be counted before shutdown starts"
    );
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();

    let shutdown = tokio::spawn({
        let ws_state = Arc::clone(&ws_state);
        async move { fauna_nest::graceful_shutdown(handle, &ws_state, &db).await }
    });

    // Causal barrier per convention 14, not a settle-sleep: poll
    // `is_shutting_down()` until `graceful_shutdown` flips it, which can
    // only happen once `begin_shutdown()` has run.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !ws_state.is_shutting_down() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "graceful_shutdown() never called begin_shutdown() within budget"
        );
        tokio::task::yield_now().await;
    }

    // The instant shutdown begins, `connection_count()` is still 1 (this
    // test never called `remove`, so nothing else could have changed it) —
    // the accept loop must already be stopped by now. In the ordering this
    // row guards against (abort moved below the drain), it is not yet.
    assert_eq!(
        ws_state.connection_count(),
        1,
        "this test controls the connection directly and must not have removed it yet"
    );
    match tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(_)) => panic!(
            "a NEW connection was accepted the instant begin_shutdown() ran — the accept \
             loop must already be stopped by then, which is the whole point of step 1 \
             preceding the drain"
        ),
        Ok(Err(_)) => {} // refused — correct
        Err(_) => panic!(
            "connect right after begin_shutdown() neither succeeded nor was refused within \
             budget — the accept loop may be wedged"
        ),
    }

    // Let the drain finish immediately rather than waiting out the full
    // GRACEFUL_SHUTDOWN_TIMEOUT.
    ws_state.remove(&conn.actor_id, conn.conn_id);
    shutdown.await.unwrap();
}
