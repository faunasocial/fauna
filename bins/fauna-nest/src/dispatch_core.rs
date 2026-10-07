//! Shared mechanical core of WS-RPC request dispatch (Spec Y2 slice 4 §4.C / §5).
//!
//! Both the per-actor connection (`crate::ws::RpcConnection`, `GET /api/v1/ws…`)
//! and the peer-symmetric federation connection
//! (`crate::federation_channel::FederationConnection`, `GET /api/v1/federation/ws`)
//! dispatch an inbound `Request` the same way: look up the kind for a
//! deadline + `forbid_replay` hint, re-encode the payload, spawn the handler with
//! an `AbortHandle` registered for `Cancel` under a deadline timeout, and on
//! completion cache + emit the `Reply`. This module factors that machinery out so
//! the two paths share one implementation rather than duplicating ~140 lines (the
//! slice-2 lesson: extract the shared core *with the real second consumer in
//! hand* — the federation serving loop is that consumer).
//!
//! What the two paths do **not** share is how a Reply is *carried + cached*, so
//! that is the [`DispatchSink`] trait:
//! - **per-actor** encodes a `Frame::Reply` to bytes and sends it through the
//!   outbound slot its reader reserved when it admitted the request
//!   (`RpcConnection::reply_slots` — the per-connection admission cap), caching
//!   the **frame bytes** verbatim (a replay re-sends them as-is — the
//!   established behavior);
//! - **federation** builds a `Reply` and routes it through
//!   `RpcDispatcher::send_reply`, caching the **(payload, ok)** so a replay
//!   rebuilds the `Reply` with the *current* correlation_id — a federation
//!   idempotency retry carries a fresh corr (`dispatcher.rs` allocates one per
//!   `request_raw`), so re-sending a stale corr would never match the peer's
//!   pending map (spec §4.B: "cache the Reply/(payload,ok), not raw frame bytes").
//!
//! The per-path **gates** (per-actor: anonymous allowlist / loopback / the
//! anonymous-write throttles; federation: the kind allowlist + per-nest throttle)
//! run in the respective caller *between* [`check_idempotent`] (step 1) and
//! [`spawn_dispatch`] (steps 2–7), so they are not part of this core.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;

use fauna_protocol::{Cancel, Request, RpcError, Value, encode_canonical};

use crate::routes::AppState;
use crate::rpc_router::RpcKindMeta;
use crate::ws::IdempotencyHit;

/// Global cap on concurrently-executing RPC handlers (F5). Generous — ~8× the
/// default concurrent-connection cap (256) — so normal load never blocks; it
/// only bites a flood (one socket pipelining tens of thousands of requests),
/// where `spawn_dispatch`'s `acquire` backpressures the read loop instead of
/// spawning unbounded handler tasks. The per-request deadline + SQLite write
/// serialization keep handlers short, so permits free quickly.
pub const MAX_INFLIGHT_HANDLERS: usize = 2048;

/// Which client connection, and which request on it, a handler is running for.
///
/// Handlers take `(state, subject, payload)` and nothing else — deliberately,
/// since a handler that can reach its own socket is a handler that can start
/// caring which one it is. Two questions need an answer anyway, so they ride a
/// task-local set around the handler future rather than widening
/// [`crate::rpc_router::RpcHandler`] for every kind in the nest:
///
/// - *may I keep this connection alive long enough to answer?*, asked by a
///   handler that has just stripped its own caller's authority
///   ([`AppState::revoke_actor_authority_sparing_caller`]) — `conn_id` +
///   `correlation_id`;
/// - *where is the caller?*, asked by the bearer mints for new-IP detection
///   (`login.md` § the handshake's side effects) — `peer_ip`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallerRef {
    pub conn_id: u64,
    pub correlation_id: u64,
    /// The client's address as the connection recorded it: the direct TCP peer,
    /// or the PROXY-v2 source behind the SNI router (`registration.rs`). Only
    /// the anonymous connection captures one — an authenticated connection's
    /// `peer_addr` is deliberately `None` (`ws::WsState::subscribe`), and so
    /// is this.
    pub peer_ip: Option<std::net::IpAddr>,
}

tokio::task_local! {
    /// Set by [`spawn_dispatch`] around each handler future, and only when the
    /// sink is a client connection — the federation plane carries no
    /// per-actor revocation, so there is nothing there to spare.
    static CALLER: CallerRef;
}

/// The connection + request the currently-running handler was dispatched for,
/// or `None` outside a handler (a background sweep, the eviction ladder, the
/// pending-action executor) or on a plane that has no client connection.
pub fn current_caller() -> Option<CallerRef> {
    CALLER.try_with(|c| *c).ok()
}

/// The currently-running handler's client address, rendered for storage — the
/// `client_ip` the bearer mints compare against the actor's last recorded IP.
/// `None` wherever [`current_caller`] is, and on a connection that recorded no
/// peer.
pub fn current_caller_ip() -> Option<String> {
    current_caller()
        .and_then(|c| c.peer_ip)
        .map(|ip| ip.to_string())
}

/// A per-connection dispatch target. Two impls: `crate::ws::RpcConnection`
/// (per-actor) and `crate::federation_channel::FederationConnection`
/// (peer-symmetric). See the module docs for why Reply carriage/caching is the
/// only axis that differs.
pub trait DispatchSink: Send + Sync + 'static {
    /// The in-flight handler abort registry (Cancel support). Both connection
    /// types hold a `tokio::sync::Mutex<HashMap<u64, AbortHandle>>`.
    fn pending_handlers(&self) -> &Mutex<HashMap<u64, AbortHandle>>;

    /// This sink's connection id, when it is a **client** connection whose
    /// authority a handler could strip mid-request. `None` — the default — on
    /// the federation plane, which has no per-actor revocation
    /// and therefore nothing to spare. Feeds [`CallerRef`].
    fn caller_conn_id(&self) -> Option<u64> {
        None
    }

    /// The client address this connection recorded, if any. Feeds
    /// [`CallerRef::peer_ip`]; only read when [`Self::caller_conn_id`] is
    /// `Some`.
    fn caller_peer_ip(&self) -> Option<std::net::IpAddr> {
        None
    }

    /// Idempotency lookup for `key`. Each impl owns its cache representation; the
    /// returned `IdempotencyHit::Hit { payload }` bytes are opaque to the core and
    /// handed back to [`DispatchSink::replay`] on a hit.
    fn lookup_idempotent(&self, key: [u8; 16]) -> BoxFuture<'_, IdempotencyHit>;

    /// Replay a cached idempotency `Hit` for a *new* request at `correlation_id`.
    /// Per-actor emits the cached frame verbatim (ignoring `correlation_id`,
    /// preserving the established behavior); federation rebuilds the `Reply` with
    /// `correlation_id`.
    fn replay(self: Arc<Self>, correlation_id: u64, cached: Bytes) -> BoxFuture<'static, ()>;

    /// Replay a durable-tier [`IdempotencyHit::HitRebuild`]: `payload` is the
    /// recorded Reply's payload-`Value` bytes (canonical CBOR), and the frame
    /// is rebuilt with the **current** `correlation_id` — a durable hit is by
    /// definition on a different connection than the one that recorded it, so
    /// verbatim frame bytes would carry a correlation_id the retrying client
    /// never allocated. Only the per-actor sink produces `HitRebuild` today;
    /// the other sinks' impls are unreachable by construction but total.
    fn replay_rebuilt(
        self: Arc<Self>,
        correlation_id: u64,
        payload: Bytes,
        ok: bool,
    ) -> BoxFuture<'static, ()>;

    /// Emit the final `Reply` for `correlation_id` carrying `outcome`, caching it
    /// under `idempotency_key` for replay. `kind` is the request's kind — the
    /// per-actor sink's durable tier records eligible (non-`Read`) `ok` replies
    /// by it; the peer-symmetric sinks ignore it.
    fn finish(
        self: Arc<Self>,
        correlation_id: u64,
        idempotency_key: [u8; 16],
        kind: String,
        outcome: Result<Bytes, RpcError>,
    ) -> BoxFuture<'static, ()>;

    /// Emit a plain error `Reply` (no caching) for an early gate / lookup failure.
    fn send_error(self: Arc<Self>, correlation_id: u64, err: RpcError) -> BoxFuture<'static, ()>;

    /// Test-only: end this connection **instead of** emitting a Reply — the
    /// lost-reply half of `rpc_hold_test_hook`'s drop mode. `true` when the
    /// sink could close; the default `false` is the plane that cannot (the
    /// federation channel is torn down by its own lifecycle,
    /// and no rule this exists for is about it), where the caller falls back
    /// to emitting the Reply as usual rather than leaving a request hanging.
    #[cfg(feature = "test-hooks")]
    fn close_without_reply(&self) -> bool {
        false
    }
}

/// Shared body for a peer-symmetric sink's [`DispatchSink::replay_rebuilt`] —
/// unreachable by construction (no durable tier on a peer-symmetric channel,
/// per the trait doc above) but total: rebuild the `Reply` exactly like
/// `replay` does. `FederationConnection` is the one such sink today; only
/// the per-actor sink's impl genuinely differs.
pub(crate) async fn replay_rebuilt_via_dispatcher(
    dispatcher: &fauna_protocol::RpcDispatcher,
    correlation_id: u64,
    payload: Bytes,
    ok: bool,
) {
    if let Ok(v) = fauna_cbor::decode_strict::<Value>(&payload) {
        let _ = fauna_peer_channel::send_reply_bounded(dispatcher, correlation_id, v, ok).await;
    }
}

/// Result of the idempotency pre-check (step 1).
pub enum IdempotencyOutcome {
    /// A cached Reply was replayed (or a `too_large` error emitted); the caller
    /// must stop — do not run the gates or the handler.
    Replayed,
    /// No cache hit; the caller proceeds to its gates + [`spawn_dispatch`].
    Proceed,
}

impl IdempotencyOutcome {
    pub fn is_replayed(&self) -> bool {
        matches!(self, IdempotencyOutcome::Replayed)
    }
}

/// **Step 1 — idempotency.** On a cache `Hit`, replay the cached Reply and return
/// [`IdempotencyOutcome::Replayed`]; on `TooLarge`, emit `replay_too_large` and
/// return `Replayed`; on `Miss`, return [`IdempotencyOutcome::Proceed`]. The
/// per-path gates run AFTER this, before [`spawn_dispatch`].
pub async fn check_idempotent<S: DispatchSink>(
    sink: &Arc<S>,
    correlation_id: u64,
    idempotency_key: [u8; 16],
) -> IdempotencyOutcome {
    match sink.lookup_idempotent(idempotency_key).await {
        IdempotencyHit::Hit { payload } => {
            Arc::clone(sink).replay(correlation_id, payload).await;
            IdempotencyOutcome::Replayed
        }
        IdempotencyHit::HitRebuild { payload, ok } => {
            Arc::clone(sink)
                .replay_rebuilt(correlation_id, payload, ok)
                .await;
            IdempotencyOutcome::Replayed
        }
        IdempotencyHit::TooLarge => {
            Arc::clone(sink)
                .send_error(
                    correlation_id,
                    RpcError::new(
                        "fauna.protocol.replay_too_large",
                        "error.protocol.replay_too_large",
                    ),
                )
                .await;
            IdempotencyOutcome::Replayed
        }
        IdempotencyHit::Miss => IdempotencyOutcome::Proceed,
    }
}

/// Who a dispatched request is served as.
#[derive(Clone, Debug)]
pub enum DispatchSubject {
    /// The connection's actor (the per-actor path) or the verified originating
    /// `nest_id` (federation) — handed to the kind's [`RpcKindMeta`] handler.
    Id([u8; 32]),
    /// A third-party principal session's binding — served by
    /// [`crate::principal_handlers::dispatch_principal`], never by the actor
    /// handler (`transport-connection.md` § Connection lifecycle → *The
    /// principal session*).
    Principal(crate::principal_handlers::PrincipalBinding),
}

impl From<[u8; 32]> for DispatchSubject {
    fn from(id: [u8; 32]) -> Self {
        Self::Id(id)
    }
}

impl std::fmt::Display for DispatchSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => f.write_str(&hex::encode(id)),
            Self::Principal(b) => write!(
                f,
                "principal {} of {}",
                hex::encode(&b.principal_id),
                hex::encode(b.account)
            ),
        }
    }
}

/// **Steps 2 + 5 + 6 + 7 — kind lookup → encode → spawn → finish.** Look up the
/// kind (deadline + `forbid_replay` hint), re-encode the payload, spawn the
/// handler with an `AbortHandle` registered in `sink.pending_handlers()` for
/// `Cancel` under a deadline timeout, and on completion hand the outcome to
/// `sink.finish` (cache + emit). The handler + reply run on detached tasks so the
/// caller's dispatch loop keeps reading `Cancel`/`Request` frames concurrently.
///
/// Step 1 (idempotency, via [`check_idempotent`]) + the per-path gates run in the
/// caller BEFORE this. `meta_lookup` selects the router — the per-actor
/// `rpc_router` or the federation `federation_router`; both yield `&RpcKindMeta`
/// (the handler shape is identical). `subject` is who the request is served as
/// ([`DispatchSubject`]).
pub async fn spawn_dispatch<S: DispatchSink>(
    state: Arc<AppState>,
    sink: Arc<S>,
    meta_lookup: for<'a> fn(&'a AppState, &str) -> Option<&'a RpcKindMeta>,
    subject: impl Into<DispatchSubject>,
    req: Request,
) {
    let subject = subject.into();
    let correlation_id = req.correlation_id;
    let kind = req.kind;
    let idempotency_key = req.idempotency_key;

    // Debug-level receipt beacon: "this request reached the dispatcher". The
    // crash-recovery e2e harness (tests/e2e-unified/helpers/crash_recovery.py)
    // tails the nest log for this line to time an unclean client kill *between*
    // request receipt and client settle — the per-transition verification
    // question of nest/common.md § Client-state recoverability. Debug-level so
    // production (default `info`) stays quiet; e2e nests opt in via RUST_LOG.
    //
    // `caller` is the authenticated subject the request is served AS — the
    // connection's actor on the per-actor path, the peer on the federation
    // one. It rides here rather than in any one handler because it answers the
    // same question for every kind at once: caller-scoped reads are scoped
    // entirely by it, so a client that reconnected as the wrong identity is
    // served a correct, empty answer, and no other line in the nest records
    // which identity that was. A public identifier, never secret material.
    tracing::debug!(
        kind = %kind,
        correlation_id,
        caller = %subject,
        "ws-rpc dispatch received",
    );

    // (2) kind lookup → deadline + forbid_replay hint.
    let deadline = match meta_lookup(&state, &kind) {
        Some(m) => {
            if m.forbid_replay && !req.replay_forbidden.unwrap_or(false) {
                tracing::warn!(
                    kind = %kind,
                    "caller missing replay_forbidden hint on forbid-replay kind"
                );
            }
            req.deadline_ms
                .map(|ms| Duration::from_millis(ms as u64))
                .unwrap_or(m.default_deadline)
        }
        None => {
            sink.send_error(
                correlation_id,
                RpcError::new("fauna.protocol.unknown_kind", "error.protocol.unknown_kind"),
            )
            .await;
            return;
        }
    };

    // (5) re-encode the payload as bytes for the handler.
    let payload_bytes = match encode_canonical(&req.payload) {
        Ok(b) => Bytes::from(b.to_vec()),
        Err(_) => {
            sink.send_error(
                correlation_id,
                RpcError::new("fauna.protocol.malformed", "error.protocol.malformed"),
            )
            .await;
            return;
        }
    };

    // (6) F5: acquire a global in-flight permit BEFORE spawning the handler.
    // When the cap is reached this `await` blocks the caller's dispatch loop —
    // backpressure — so a socket pipelining tens of thousands of requests can't
    // spawn unbounded handler tasks. The permit is moved into the handler task
    // and released when it completes / aborts / times out, bounding the count of
    // concurrent **handlers** to `MAX_INFLIGHT_HANDLERS`.
    //
    // The reply task at step 7 is deliberately *outside* the permit: it is
    // spawned after the handler released it, so a stalled peer's parked replies
    // do not hold permits from this **global** semaphore, which the per-actor
    // path shares — one federation peer that stopped reading would otherwise
    // starve every client's dispatch. What bounds the reply half instead is
    // `fauna_peer_channel::send_reply_bounded`'s budget in each peer-symmetric
    // sink's `finish`: every reply retires, so the tasks accumulate only for
    // that budget rather than forever. (This comment
    // said "handler+reply task pairs" for as long as the permit has been
    // released before the reply — it never covered the reply.)
    let permit = Arc::clone(&state.handler_semaphore)
        .acquire_owned()
        .await
        .expect("handler_semaphore is never closed");

    // (6) invoke the handler in a spawned task (so we hold an AbortHandle). The
    // re-lookup inside the task sidesteps borrowing `meta` across the await.
    let handler_kind = kind.clone();
    let handler_state = Arc::clone(&state);
    // Read off the sink here rather than inside the task: the task owns no
    // `Arc<S>`, and a `Copy` pair is cheaper to move than one.
    let caller = sink.caller_conn_id().map(|conn_id| CallerRef {
        conn_id,
        correlation_id,
        peer_ip: sink.caller_peer_ip(),
    });
    // spawn-ok(request-scoped): one RPC, bounded by two named mechanisms — the
    // `MAX_INFLIGHT_HANDLERS` permit moved in above, and the per-kind
    // `tokio::time::timeout` the body wraps itself in. It does hold
    // `Arc<AppState>`, so a rotation teardown can overlap one handler-timeout
    // window; that is the same window the WS drain already accepts for an
    // in-flight request, not an unbounded survival.
    let task = tokio::spawn(async move {
        let _permit = permit; // released when the handler task ends

        // (6a) Test-only: park here while this kind is armed, so a test can
        // stand inside the request's *pending* window instead of racing it —
        // the pre-fetch states `settings.md` § Privacy sub-page and
        // `family-safety.md` § Cold start legislate. Deliberately BEFORE the
        // deadline timeout below: an armed hold must not spend the handler's
        // own budget, or a long hold would surface as `fauna.protocol.timeout`
        // rather than as "still waiting". Also deliberately AFTER the in-flight
        // permit is acquired and inside the spawned task — a park before either
        // would backpressure the caller's read loop and stall every OTHER kind
        // on the socket, which is exactly what the mechanism must not do.
        // Whole rationale: `rpc_hold_test_hook`'s module doc. Compiled out of
        // every production build (`e2e-conventions.md` point 15).
        // A refusing gate answers in place of the handler — the failed request
        // a test needs when no ordinary input provokes one.
        #[cfg(feature = "test-hooks")]
        if crate::rpc_hold_test_hook::hold_if_armed(&handler_state, &handler_kind).await
            == crate::rpc_hold_test_hook::Verdict::Refuse
        {
            return Err(crate::rpc_hold_test_hook::refused_error());
        }

        let meta = match meta_lookup(&handler_state, &handler_kind) {
            Some(m) => m,
            None => {
                return Err(RpcError::new(
                    "fauna.protocol.unknown_kind",
                    "error.protocol.unknown_kind",
                ));
            }
        };
        // The kind's wire metadata (deadline, replay) is the router's for both
        // subjects — one kind, one wire contract — but a principal is never
        // handed to the actor handler: its request runs the principal gate and
        // the principal handler table (`principal_handlers`), so an actor
        // handler reached by mistake is structurally impossible here.
        let fut: futures_util::future::BoxFuture<'static, Result<Bytes, RpcError>> = match subject {
            DispatchSubject::Id(id) => {
                (meta.handler)(Arc::clone(&handler_state), id, payload_bytes)
            }
            DispatchSubject::Principal(binding) => {
                let state = Arc::clone(&handler_state);
                let kind = handler_kind.clone();
                Box::pin(async move {
                    crate::principal_handlers::dispatch_principal(
                        state,
                        &binding,
                        &kind,
                        payload_bytes,
                    )
                    .await
                })
            }
        };
        // Scoped around the handler only — not around the deadline timeout or
        // the reply emit — so `current_caller()` answers for exactly the window
        // in which a handler could revoke its own caller.
        let fut = async move {
            match caller {
                Some(caller) => CALLER.scope(caller, fut).await,
                None => fut.await,
            }
        };
        match tokio::time::timeout(deadline, fut).await {
            Ok(Ok(b)) => Ok(b),
            Ok(Err(rpc_err)) => Err(rpc_err),
            Err(_) => Err(RpcError::new(
                "fauna.protocol.timeout",
                "error.protocol.timeout",
            )),
        }
    });
    let abort = task.abort_handle();
    sink.pending_handlers()
        .lock()
        .await
        .insert(correlation_id, abort);

    // (7) detach: await the handler, remove from pending, cache + emit the Reply.
    let reply_sink = Arc::clone(&sink);
    #[cfg(feature = "test-hooks")]
    let reply_state = state;
    // spawn-ok(request-scoped): awaits the handler task above and then emits
    // one Reply, so it inherits that task's bound exactly.
    tokio::spawn(async move {
        let outcome = match task.await {
            Ok(r) => r,
            Err(join_err) if join_err.is_cancelled() => Err(RpcError::new(
                "fauna.protocol.cancelled",
                "error.protocol.cancelled",
            )),
            Err(_) => Err(RpcError::new(
                "fauna.protocol.internal",
                "error.protocol.internal",
            )),
        };
        reply_sink
            .pending_handlers()
            .lock()
            .await
            .remove(&correlation_id);
        // (7a) Test-only: the handler has run to completion — whatever it
        // committed is committed — and a dropping gate now ends the connection
        // in place of the Reply, so the caller meets a transport failure over
        // a request that DID take effect. Checked here, after the handler and
        // never before it: the state under test is "committed, unanswered",
        // which a pre-handler drop could not produce. Whole rationale:
        // `rpc_hold_test_hook`'s module doc (§ Dropping the reply).
        #[cfg(feature = "test-hooks")]
        if crate::rpc_hold_test_hook::drop_reply_if_armed(&reply_state, &kind, || {
            reply_sink.close_without_reply()
        }) {
            return;
        }
        reply_sink
            .finish(correlation_id, idempotency_key, kind, outcome)
            .await;
    });
}

/// Turn a handler `outcome` into the `(payload, ok)` a `Reply` carries — the
/// shared step-7 logic both sinks use. `Ok(bytes)` decode to the payload `Value`
/// (`ok = true`); a decode failure or an `Err` becomes a canonical-encoded
/// `RpcError` payload (`ok = false`).
pub fn outcome_to_reply_value(outcome: Result<Bytes, RpcError>) -> (Value, bool) {
    match outcome {
        Ok(bytes) => match fauna_cbor::decode_strict::<Value>(&bytes) {
            Ok(v) => (v, true),
            Err(_) => (
                err_to_value(RpcError::new(
                    "fauna.protocol.encode_failed",
                    "error.protocol.encode",
                )),
                false,
            ),
        },
        Err(err) => (err_to_value(err), false),
    }
}

/// Canonical-encode an `RpcError` into a `Reply` payload `Value` (`Value::Null`
/// on the unreachable encode failure).
pub fn err_to_value(err: RpcError) -> Value {
    let bytes = encode_canonical(&err).unwrap_or_default();
    fauna_cbor::decode_strict::<Value>(&bytes).unwrap_or(Value::Null)
}

/// Serve inbound requests over an established channel: dispatch each `Request`
/// via the caller-supplied `serve_request` (the per-connection-type gate +
/// [`spawn_dispatch`] call — federation's per-nest throttle is the one shape
/// today), and on an inbound `Cancel`
/// abort the matching in-flight handler via [`DispatchSink::pending_handlers`].
/// `serve_request` returns a boxed future (rather than
/// a plain `Fn(..) -> impl Future`) so `state`'s and `conn`'s borrows can share
/// one lifetime — the natural higher-ranked bound over two independent input
/// lifetimes has no single well-formed output type; callers wrap their async
/// fn as `|state, conn, req| Box::pin(serve_request(state, conn, req))`.
pub(crate) async fn serve_loop<S, F>(
    state: &Arc<AppState>,
    conn: &Arc<S>,
    inbound: &mut mpsc::Receiver<Request>,
    cancels: Option<mpsc::Receiver<Cancel>>,
    serve_request: F,
) where
    S: DispatchSink,
    F: for<'a> Fn(&'a Arc<AppState>, &'a Arc<S>, Request) -> BoxFuture<'a, ()>,
{
    let mut cancels = cancels;
    loop {
        tokio::select! {
            req = inbound.recv() => match req {
                Some(req) => serve_request(state, conn, req).await,
                None => break, // channel closed → connection ended
            },
            maybe_cancel = fauna_core::select_pending::recv_or_pending(&mut cancels) => match maybe_cancel {
                Some(cancel) => {
                    if let Some(h) = conn
                        .pending_handlers()
                        .lock()
                        .await
                        .remove(&cancel.correlation_id)
                    {
                        h.abort();
                    }
                }
                None => cancels = None, // cancel channel closed/absent; keep serving
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    /// F5: the dispatch in-flight semaphore is wired at the global cap, and it is
    /// REAL backpressure — once exhausted, the next acquire fails (so
    /// `spawn_dispatch`'s `acquire().await` would block the read loop rather than
    /// spawn an unbounded handler), and releasing a permit frees capacity again.
    #[tokio::test]
    async fn handler_semaphore_caps_inflight() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState::for_test(db));
        assert_eq!(
            state.handler_semaphore.available_permits(),
            MAX_INFLIGHT_HANDLERS,
            "for_test AppState wires the full in-flight cap"
        );

        // Hold every permit — emulates MAX_INFLIGHT_HANDLERS handlers in flight.
        let mut held = Vec::new();
        for _ in 0..MAX_INFLIGHT_HANDLERS {
            held.push(
                Arc::clone(&state.handler_semaphore)
                    .try_acquire_owned()
                    .expect("permit available below the cap"),
            );
        }
        assert!(
            Arc::clone(&state.handler_semaphore)
                .try_acquire_owned()
                .is_err(),
            "at the cap there is no permit → spawn_dispatch would backpressure"
        );

        // A completed handler releases its permit, restoring capacity.
        held.pop();
        assert!(
            Arc::clone(&state.handler_semaphore)
                .try_acquire_owned()
                .is_ok(),
            "releasing a permit frees capacity for the next dispatch"
        );
    }
}
