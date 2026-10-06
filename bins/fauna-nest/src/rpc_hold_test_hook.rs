//! Test-only HTTP endpoints that hold **one named RPC kind's** dispatch pending
//! until a second call releases it — the seam that makes a client's *pre-fetch*
//! window observable at all.
//!
//! ## The class of rule this exists to test
//!
//! Several goal-doc rules are about what an app shows **before** a read has
//! answered, not after: `settings.md` § Privacy sub-page ("The selector shows
//! the account's stored mode, never a default … Until `inbox_mode_get` has
//! answered, the mode is *unknown*"), `family-safety.md` § Cold start (an
//! unresolved supervision read must not collapse into "unsupervised"). Their
//! failure mode is a client that paints a plausible **guess** and then quietly
//! corrects itself a round trip later — which every post-fetch assertion in the
//! suite passes, because by the time it looks the guess is gone.
//!
//! Pinning the guess needs the pre-fetch window to be a *state you can stand
//! in*, not an interval you race. Three routes were rejected before this one
//! :
//!
//! * **Freezing the nest process** (`SIGSTOP`) is forbidden outright by the
//!   suite's process-safety rule — a test signals only process groups its own
//!   run created, never a process it found by name or shares
//!   (`e2e-conventions.md` § The conventions) — and `nest_instance` is a
//!   session-scoped fixture every other test shares, so the blast radius is the
//!   whole run, not the one test.
//! * **Shortening or suppressing** — the shape of every existing `FAUNA_E2E_*`
//!   knob (`FAUNA_E2E_DEBOUNCE_MS`, `FAUNA_E2E_SUPPRESS_CONV_PUSH`) — moves the
//!   window or deletes it; neither lets a test *sit inside* it.
//! * **Reading with no wait right after navigating** is the wall-clock race
//!   `e2e-conventions.md` point 14 calls DEFUNCT: a fast local nest can answer
//!   before the next Python line runs, so the assertion passes by accident on
//!   the runs where it passes at all.
//!
//! ## What this does instead
//!
//! `hold_if_armed` sits in `dispatch_core::spawn_dispatch`, at the one chokepoint
//! every WS-RPC request of every kind crosses — per-actor and federation alike —
//! and parks the dispatch task while the kind is armed. The request is genuinely
//! in flight the whole time: the frame arrived, the connection is healthy, no
//! reply exists yet. That is precisely the state the rules above legislate, and
//! it now **persists until the test says otherwise** rather than for however long
//! a round trip happens to take.
//!
//! Three properties make it a convention-14 mechanism rather than another timing
//! trick:
//!
//! 1. **The park is unbounded and test-ended, so no budget can expire under the
//!    assertion.** It runs *before* the per-kind `tokio::time::timeout`, so the
//!    handler's own deadline clock starts at release, not at arm.
//! 2. **Arrival is observable, not assumed.** `GET` reports `holding` — how many
//!    requests of the kind are parked *right now*. A test deadline-polls that to
//!    ≥ 1 before asserting, so it never reads the UI before the app has even sent
//!    the request. Without this the test would still be a race, just a quieter
//!    one.
//! 3. **Selection is by kind, decided per request.** There is no ordering to lose
//!    a race against and no count of calls to get wrong — the same reason
//!    `channel_refusal_test_hook` selects by envelope *class*.
//!
//! Deliberately general (any registered kind, not `fauna.inbox.mode.get`): the
//! untestable-pre-fetch shape is a whole class of rule, and a per-rule hook would
//! be re-derived once per rule.
//!
//! ## Refusal — the failed-request sibling
//!
//! `POST …/{kind}/refuse` switches the same gate to answering every request of
//! the kind with an error reply (`fauna.test.refused`, rendered as the generic
//! `error.unexpected`) instead of running its handler — the parked ones and
//! every later arrival — until `release`. It exists for the rules about what an
//! app does when the nest turns a request away (`feed.md` § Errors & edge
//! cases: a failed send says so in the composer and keeps what was written),
//! which no ordinary input provokes: the nest has no length or tag limit a user
//! could trip. Like the hold, it is selected by kind, observable (`refused`
//! counts the requests turned away), and touches no at-rest state — a refused
//! request never reaches its handler.
//!
//! ## Dropping the reply — the lost-reply sibling
//!
//! `POST …/{kind}/drop-reply` switches the gate to the third failure a client
//! can meet, and the only one where the request **took effect**: every request
//! of the kind runs its handler to completion — the real handler, committing
//! whatever it commits — and then, in place of the Reply, the connection is
//! closed (the revocation teardown, 4401, with the frame never written). The
//! caller sees a transport failure over a request the nest has in fact applied,
//! until `release`.
//!
//! It exists for the rules about what an app does when it cannot know whether
//! its request landed: `identity-succession.md` § Implementation status today
//! (*a lost submit reply no longer destroys the account*) — the nest commits a
//! succession before it encodes its reply, so a dropped connection in that gap
//! leaves the app holding the only copy of the key the account now belongs to.
//! The crate-level arm is pinned by an in-process lossy transport
//! (`libs/fauna-client-recovery/tests/support/mod.rs::LossyNest`); only a real
//! nest dropping a real reply can show a whole app composes it. Combined with
//! `refuse` on the reconcile's read (`fauna.recovery.succession.lookup`), it
//! also stages the undecidable arm — committed, unanswered, and not checkable.
//!
//! The drop is decided **after** the handler, in `dispatch_core`'s reply task,
//! never before it: "committed but unanswered" is the whole state under test,
//! and a pre-handler drop could only produce "never ran", which `refuse`
//! already covers. It is the per-actor client plane only
//! (`DispatchSink::close_without_reply`) — the federation and sidecar channels
//! answer as usual — and `dropped` counts the replies actually thrown away.
//!
//! **Once.** `POST …/{kind}/drop-reply-once` drops the NEXT reply only and
//! reopens the gate by itself — no `release`. It stages the *brief* drop a
//! client is meant to ride out (`mail-credentials.md` § Partial-state-during-
//! minting: the enable's idempotent steps are retried within the gesture). The
//! open-ended drop cannot: it drops the retry too, so "the retry finishes"
//! would hinge on releasing inside the client's backoff — a wall-clock race.
//!
//! ## What it does NOT do
//!
//! It holds the *dispatch*, never the connection: other kinds on the same socket
//! keep flowing, so a test can drive the rest of the app while one read hangs. It
//! touches no at-rest state and no handler logic — a released request runs
//! exactly the code an un-armed one would. It is per-process in-memory state,
//! cleared when the nest exits.
//!
//! An armed kind holds one `MAX_INFLIGHT_HANDLERS` permit per parked request
//! (the permit is acquired before the spawn, so the park cannot deadlock the
//! read loop the way a pre-permit park would); at 2048 permits against the
//! handful a test parks, that is a rounding error, but it is why `release` is a
//! `finally`-shaped duty on the Python side rather than a nicety.
//!
//! Gated on `test-hooks` **alone**, like its sibling hooks, so it is present in
//! the standard `cargo build -p fauna-nest --features test-hooks` e2e build and
//! in no production build whatsoever (`e2e-conventions.md` point 15 — the
//! feature is the boundary, not a runtime env gate). A test driving it is
//! therefore standalone-nest only: the docker/live artifacts do not compile it.
//!
//! Consumers: `tests/e2e-unified/helpers/rpc_hold.py`, and through it
//! `tests/e2e-unified/tests/test_settings.py`. Production never compiles this
//! module.

#![cfg(feature = "test-hooks")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;
use tokio::sync::watch;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// What a gate does to a request of its kind right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Pass straight through — every gate no test is using.
    Open,
    /// Park until the mode changes (`POST …/{kind}`).
    Hold,
    /// Answer with an error instead of running the handler (`POST
    /// …/{kind}/refuse`) — both the requests parked under `Hold` when it is
    /// set and every later arrival, until `release`. The failure a user meets
    /// when the nest turns a request away, as a state a test can stand in: a
    /// composer's failed send (`feed.md` § Errors & edge cases) has no ordinary
    /// input that provokes it.
    Refuse,
    /// Run the handler, then close the connection instead of replying (`POST
    /// …/{kind}/drop-reply`) — a request that took effect and was never
    /// answered. Does not park: the hold's `park` passes it straight through,
    /// and the reply task asks [`drop_reply_if_armed`] once the handler is done.
    DropReply,
    /// [`Mode::DropReply`] for exactly ONE reply, after which the gate is
    /// [`Mode::Open`] again with no release (`POST …/{kind}/drop-reply-once`)
    /// — a brief connection drop the caller's own retry recovers from.
    DropReplyOnce,
}

/// What [`HoldGate::park`] decided for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Run the handler, exactly as an un-armed request would.
    Pass,
    /// Reply with [`refused_error`] and never run the handler.
    Refuse,
}

/// The error a refused request answers with. The generic `error.unexpected`
/// string, so the app renders exactly what it renders for any nest failure it
/// has no specific copy for.
pub fn refused_error() -> fauna_protocol::RpcError {
    fauna_protocol::RpcError::new("fauna.test.refused", "error.unexpected")
}

/// One kind's gate. Held by `POST`, refused by `POST .../refuse`, opened by
/// `POST .../release`.
///
/// The mode is a `watch` channel rather than an atomic + `Notify` on purpose:
/// `Receiver::wait_for` evaluates the *current* value before it waits, so a
/// request that reaches the gate in the window between `release` flipping the
/// mode and the waiter subscribing cannot miss the wake-up. The hand-rolled
/// flag-plus-notify shape has to order those two steps correctly to get the
/// same guarantee, and gets it wrong silently.
pub struct HoldGate {
    mode: watch::Sender<Mode>,
    /// Requests parked in this gate *right now*. The test's arrival signal.
    holding: AtomicUsize,
    /// Cumulative requests this gate has let go, over the process's life.
    released: AtomicUsize,
    /// Cumulative requests this gate has refused — the arrival signal of a
    /// refusal, which parks nothing to count in `holding`.
    refused: AtomicUsize,
    /// Cumulative replies this gate has thrown away after their handler ran —
    /// the arrival signal of a drop, and the proof the handler DID run.
    dropped: AtomicUsize,
}

impl HoldGate {
    fn new() -> Self {
        Self {
            mode: watch::channel(Mode::Open).0,
            holding: AtomicUsize::new(0),
            released: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    fn arm(&self) {
        // `send_replace`, never `send`: `Sender::send` FAILS — and leaves the
        // value untouched — when no receiver is currently subscribed, which is
        // the normal state of a gate nothing is parked in. Armed with `send`
        // this gate silently stayed disarmed and every hold was a no-op; the
        // unit tests below caught it, and they are the reason to keep them.
        self.mode.send_replace(Mode::Hold);
    }

    /// Refuse every request of this kind until `release` — waking the parked
    /// ones into the same refusal.
    fn refuse(&self) {
        self.mode.send_replace(Mode::Refuse);
    }

    /// Drop the reply of every request of this kind until `release` — waking
    /// any parked ones to run their handler, whose reply is then dropped too.
    fn drop_replies(&self) {
        self.mode.send_replace(Mode::DropReply);
    }

    /// Drop the reply of the NEXT request of this kind only — the gate reopens
    /// by itself once one reply has been thrown away (module doc § Dropping
    /// the reply → *Once*).
    fn drop_one_reply(&self) {
        self.mode.send_replace(Mode::DropReplyOnce);
    }

    /// Open the gate and wake every parked request. Returns the cumulative release
    /// count *after* the wake-up has been signalled — the waiters may not have
    /// been scheduled yet, so this is the count so far, not a completion proof.
    fn release(&self) -> usize {
        // `send_replace` for the same reason as `arm` — and here it also wakes
        // every current waiter, which is the whole point.
        self.mode.send_replace(Mode::Open);
        self.released.load(Ordering::SeqCst)
    }

    fn current(&self) -> Mode {
        *self.mode.borrow()
    }

    fn is_armed(&self) -> bool {
        self.current() == Mode::Hold
    }

    /// Park while held, then pass or refuse. Returns immediately when the gate
    /// is open or refusing — the un-armed path is one load, which is what keeps
    /// this callable unconditionally from the dispatch hot path.
    async fn park(&self) -> Verdict {
        let verdict = match self.current() {
            // A dropping gate lets the handler run; the drop is the reply
            // task's decision, made once the handler is done.
            Mode::Open | Mode::DropReply | Mode::DropReplyOnce => return Verdict::Pass,
            Mode::Refuse => Verdict::Refuse,
            Mode::Hold => {
                // `_guard` decrements `holding` on ANY exit — including the task
                // being aborted mid-park, which is exactly what a client `Cancel`
                // frame does. Without it a cancelled request leaves the count
                // permanently high and the next test's arrival poll passes on a
                // ghost.
                let _guard = HoldingGuard::new(self);
                let mut rx = self.mode.subscribe();
                // Errors only if the sender is dropped; the registry owns it for
                // the process's life, and a vanished sender means "nothing can
                // re-arm this", so passing is the right reading either way.
                let woke_to = rx
                    .wait_for(|mode| *mode != Mode::Hold)
                    .await
                    .map(|mode| *mode)
                    .unwrap_or(Mode::Open);
                self.released.fetch_add(1, Ordering::SeqCst);
                if woke_to == Mode::Refuse {
                    Verdict::Refuse
                } else {
                    Verdict::Pass
                }
            }
        };
        if verdict == Verdict::Refuse {
            self.refused.fetch_add(1, Ordering::SeqCst);
        }
        verdict
    }
}

impl HoldGate {
    /// The reply task's question, asked once the handler is done: while this
    /// gate is dropping, run `close` (end the connection in place of the
    /// Reply) and count the drop only if it actually closed — a plane that
    /// cannot close answers as usual and must not read as a dropped reply.
    ///
    /// A drop-once gate hands its one drop to the first reply that claims it —
    /// flipping to [`Mode::Open`] in the same step, so two replies finishing
    /// together cannot both be dropped — and gives the drop back when that
    /// reply's plane could not close.
    fn drop_reply(&self, close: impl FnOnce() -> bool) -> bool {
        match self.current() {
            Mode::DropReply => {
                if !close() {
                    return false;
                }
            }
            Mode::DropReplyOnce => {
                let claimed = self.mode.send_if_modified(|mode| {
                    let is_once = *mode == Mode::DropReplyOnce;
                    if is_once {
                        *mode = Mode::Open;
                    }
                    is_once
                });
                if !claimed {
                    return false;
                }
                if !close() {
                    // Only if nothing re-armed the gate in between.
                    self.mode.send_if_modified(|mode| {
                        let reopened = *mode == Mode::Open;
                        if reopened {
                            *mode = Mode::DropReplyOnce;
                        }
                        reopened
                    });
                    return false;
                }
            }
            Mode::Open | Mode::Hold | Mode::Refuse => return false,
        }
        self.dropped.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// Decrements `holding` on drop — see [`HoldGate::park`].
struct HoldingGuard<'a>(&'a HoldGate);

impl HoldingGuard<'_> {
    fn new(gate: &HoldGate) -> HoldingGuard<'_> {
        gate.holding.fetch_add(1, Ordering::SeqCst);
        HoldingGuard(gate)
    }
}

impl Drop for HoldingGuard<'_> {
    fn drop(&mut self) {
        self.0.holding.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Every armed/known kind's gate. Lives in `AppState`; empty in a nest no test
/// has armed, which is every nest outside an e2e run.
#[derive(Default)]
pub struct RpcHoldRegistry {
    gates: Mutex<HashMap<String, Arc<HoldGate>>>,
}

impl RpcHoldRegistry {
    fn gate(&self, kind: &str) -> Arc<HoldGate> {
        let mut gates = self.gates.lock().expect("rpc_hold gates mutex poisoned");
        Arc::clone(
            gates
                .entry(kind.to_string())
                .or_insert_with(|| Arc::new(HoldGate::new())),
        )
    }

    /// The gate for `kind` **only if one already exists** — the dispatch-path
    /// lookup, which must not allocate a gate for every kind the nest serves.
    fn existing(&self, kind: &str) -> Option<Arc<HoldGate>> {
        self.gates
            .lock()
            .expect("rpc_hold gates mutex poisoned")
            .get(kind)
            .map(Arc::clone)
    }
}

/// **The dispatch-path call.** Park this request while its kind is held, and
/// say whether it may run: [`Verdict::Refuse`] means reply with
/// [`refused_error`] instead of running the handler.
///
/// Called from `dispatch_core::spawn_dispatch` inside the spawned handler task,
/// before the per-kind deadline timeout is started, so an armed hold does not
/// consume the handler's own budget. A kind no test has ever named costs one
/// mutex-guarded map lookup that misses.
pub async fn hold_if_armed(state: &AppState, kind: &str) -> Verdict {
    match state.rpc_hold.existing(kind) {
        Some(gate) => gate.park().await,
        None => Verdict::Pass,
    }
}

/// **The reply-path call.** `true` means the reply for this request of `kind`
/// was dropped — `close` ended the connection — and must not be emitted.
///
/// Called from `dispatch_core::spawn_dispatch`'s reply task AFTER the handler
/// completed, so whatever the handler committed stays committed. `close` is
/// the sink's [`crate::dispatch_core::DispatchSink::close_without_reply`]; it is
/// only invoked while the kind is dropping. A kind no test has named costs one
/// map lookup that misses, like [`hold_if_armed`].
pub fn drop_reply_if_armed(state: &AppState, kind: &str, close: impl FnOnce() -> bool) -> bool {
    match state.rpc_hold.existing(kind) {
        Some(gate) => gate.drop_reply(close),
        None => false,
    }
}

/// Reject a kind neither router serves. Arming a typo parks nothing, and the
/// test then fails several steps later on an arrival poll that can never
/// succeed — a failure that reads as "the app never sent the request".
fn known_kind(state: &AppState, kind: &str) -> bool {
    state.rpc_router.contains(kind) || state.federation_router.contains(kind)
}

/// `POST /api/v1/test/rpc-hold/{kind}` — arm the hold. Every subsequent request
/// of `kind` parks until `release`. Returns `{"ok": true, "kind": <kind>}`.
async fn arm_hold(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    if !known_kind(&state, &kind) {
        return ApiError::bad_request(format!(
            "no RPC kind {kind:?} is registered on this nest — check the spelling \
             against the router registration, not the client-side method name"
        ))
        .into_response();
    }
    state.rpc_hold.gate(&kind).arm();
    Json(json!({ "ok": true, "kind": kind })).into_response()
}

/// `POST /api/v1/test/rpc-hold/{kind}/refuse` — answer every request of `kind`
/// with [`refused_error`] until `release`, including any parked by an earlier
/// hold. Returns `{"ok": true, "kind": <kind>}`.
async fn refuse_kind(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    if !known_kind(&state, &kind) {
        return ApiError::bad_request(format!(
            "no RPC kind {kind:?} is registered on this nest — check the spelling \
             against the router registration, not the client-side method name"
        ))
        .into_response();
    }
    state.rpc_hold.gate(&kind).refuse();
    Json(json!({ "ok": true, "kind": kind })).into_response()
}

/// `POST /api/v1/test/rpc-hold/{kind}/drop-reply` — run every request of
/// `kind`'s handler, then close its connection instead of replying, until
/// `release` (module doc § Dropping the reply). Client-plane kinds only: the
/// federation and sidecar sinks cannot close in place of a reply, so arming
/// one of their kinds would drop nothing. Returns `{"ok": true, "kind": <kind>}`.
async fn drop_reply_kind(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    if !state.rpc_router.contains(&kind) {
        return ApiError::bad_request(format!(
            "no client-plane RPC kind {kind:?} is registered on this nest — a \
             reply can only be dropped on the per-actor connection, and the \
             spelling is the router registration's, not the client-side method name"
        ))
        .into_response();
    }
    state.rpc_hold.gate(&kind).drop_replies();
    Json(json!({ "ok": true, "kind": kind })).into_response()
}

/// `POST /api/v1/test/rpc-hold/{kind}/drop-reply-once` — like
/// [`drop_reply_kind`], but for the next request of `kind` only: its handler
/// runs, its connection closes in place of the reply, and the gate is open
/// again for the caller's retry with no release (module doc § Dropping the
/// reply → *Once*). Returns `{"ok": true, "kind": <kind>}`.
async fn drop_reply_once_kind(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    if !state.rpc_router.contains(&kind) {
        return ApiError::bad_request(format!(
            "no client-plane RPC kind {kind:?} is registered on this nest — a \
             reply can only be dropped on the per-actor connection, and the \
             spelling is the router registration's, not the client-side method name"
        ))
        .into_response();
    }
    state.rpc_hold.gate(&kind).drop_one_reply();
    Json(json!({ "ok": true, "kind": kind })).into_response()
}

/// `GET /api/v1/test/rpc-hold/{kind}` — the arrival observable.
/// Returns `{"armed": bool, "refusing": bool, "dropping": bool, "holding": N,
/// "released": M, "refused": R, "dropped": D}`, where `holding` is the number of
/// requests of `kind` parked right now, `refused` the cumulative number turned
/// away and `dropped` the cumulative number whose handler ran and whose reply
/// was thrown away. A test deadline-polls `holding >= 1` (or `refused >= 1`,
/// `dropped >= 1`) before asserting anything about the UI.
async fn hold_status(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    match state.rpc_hold.existing(&kind) {
        Some(gate) => Json(json!({
            "armed": gate.is_armed(),
            "refusing": gate.current() == Mode::Refuse,
            "dropping": matches!(gate.current(), Mode::DropReply | Mode::DropReplyOnce),
            "holding": gate.holding.load(Ordering::SeqCst),
            "released": gate.released.load(Ordering::SeqCst),
            "refused": gate.refused.load(Ordering::SeqCst),
            "dropped": gate.dropped.load(Ordering::SeqCst),
        })),
        // Never armed is a legitimate answer, not an error: a test polling a
        // kind it has not armed should see zeros, not a 404 it has to special-case.
        None => Json(json!({
            "armed": false,
            "refusing": false,
            "dropping": false,
            "holding": 0,
            "released": 0,
            "refused": 0,
            "dropped": 0,
        })),
    }
}

/// `POST /api/v1/test/rpc-hold/{kind}/release` — disarm and wake every parked
/// request. Returns `{"ok": true, "released": M}`.
async fn release_hold(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    let released = match state.rpc_hold.existing(&kind) {
        Some(gate) => gate.release(),
        None => 0,
    };
    Json(json!({ "ok": true, "released": released }))
}

/// Mount the `/api/v1/test/rpc-hold/{kind}` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/rpc-hold/{kind}",
            post(arm_hold).get(hold_status),
        )
        .route("/api/v1/test/rpc-hold/{kind}/release", post(release_hold))
        .route("/api/v1/test/rpc-hold/{kind}/refuse", post(refuse_kind))
        .route(
            "/api/v1/test/rpc-hold/{kind}/drop-reply",
            post(drop_reply_kind),
        )
        .route(
            "/api/v1/test/rpc-hold/{kind}/drop-reply-once",
            post(drop_reply_once_kind),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The un-armed path must be a no-op, or every RPC the nest serves pays for
    /// a mechanism one test uses.
    #[tokio::test]
    async fn an_unarmed_gate_does_not_park() {
        let gate = HoldGate::new();
        tokio::time::timeout(Duration::from_secs(5), gate.park())
            .await
            .expect("an unarmed gate returns immediately");
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);
        assert_eq!(gate.released.load(Ordering::SeqCst), 0);
    }

    /// The whole mechanism in one assertion: armed parks, and stays parked with
    /// no deadline of its own — the property that makes the pre-fetch window a
    /// state to stand in rather than an interval to race.
    #[tokio::test]
    async fn an_armed_gate_parks_until_released() {
        let gate = Arc::new(HoldGate::new());
        gate.arm();

        // spawn-ok(test): a test-local park the test awaits after release
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park().await }
        });

        // `holding` is the test's arrival signal, so it must become 1 without
        // anything releasing — polled, not slept on (convention 14).
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;
        assert!(!parked.is_finished(), "an armed gate must still be parking");
        assert_eq!(gate.released.load(Ordering::SeqCst), 0);

        gate.release();
        tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("release wakes the parked request")
            .expect("the parked task did not panic");
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);
        assert_eq!(gate.released.load(Ordering::SeqCst), 1);
    }

    /// Release must wake EVERY parked request, not just the first — an app that
    /// retries its read parks more than one, and a test that released only the
    /// head would hang on the survivors.
    #[tokio::test]
    async fn release_wakes_every_parked_request() {
        let gate = Arc::new(HoldGate::new());
        gate.arm();

        let parked: Vec<_> = (0..4)
            .map(|_| {
                let gate = Arc::clone(&gate);
                // spawn-ok(test): test-local parks the test awaits after release
                tokio::spawn(async move { gate.park().await })
            })
            .collect();

        wait_for(|| gate.holding.load(Ordering::SeqCst) == 4).await;
        gate.release();
        for task in parked {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("every parked request wakes")
                .expect("the parked task did not panic");
        }
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);
        assert_eq!(gate.released.load(Ordering::SeqCst), 4);
    }

    /// A request that arrives after the release must not park — otherwise the
    /// convergence half of the journey (release, then watch the UI settle)
    /// hangs on the app's next read.
    #[tokio::test]
    async fn a_request_arriving_after_release_passes_straight_through() {
        let gate = HoldGate::new();
        gate.arm();
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), gate.park())
            .await
            .expect("a released gate does not park later arrivals");
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);
    }

    /// An aborted park (the client sent `Cancel`) must give its `holding` slot
    /// back. A leaked count makes the NEXT test's arrival poll pass on a ghost
    /// and assert against a UI nothing is holding.
    #[tokio::test]
    async fn an_aborted_park_releases_its_holding_slot() {
        let gate = Arc::new(HoldGate::new());
        gate.arm();
        // spawn-ok(test): a test-local park the test itself aborts below
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park().await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;

        parked.abort();
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 0).await;
        // The abort is not a release: nothing was let through.
        assert_eq!(gate.released.load(Ordering::SeqCst), 0);
    }

    /// Refusing wakes a parked request into a refusal, and turns later
    /// arrivals away without parking them, until release reopens the gate.
    #[tokio::test]
    async fn refuse_turns_parked_and_later_requests_away_until_release() {
        let gate = Arc::new(HoldGate::new());
        gate.arm();
        // spawn-ok(test): a test-local park the test awaits after the refusal
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park().await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;

        gate.refuse();
        let verdict = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("refuse wakes the parked request")
            .expect("the parked task did not panic");
        assert_eq!(verdict, Verdict::Refuse);

        let later = tokio::time::timeout(Duration::from_secs(5), gate.park())
            .await
            .expect("a refusing gate does not park later arrivals");
        assert_eq!(later, Verdict::Refuse);
        assert_eq!(gate.refused.load(Ordering::SeqCst), 2);
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);

        gate.release();
        assert_eq!(gate.park().await, Verdict::Pass);
        assert_eq!(gate.refused.load(Ordering::SeqCst), 2);
    }

    /// A dropping gate lets the handler run (it never parks and never refuses)
    /// and then drops the reply by closing — and counts only drops that closed.
    #[tokio::test]
    async fn a_dropping_gate_runs_the_handler_then_drops_the_reply_until_release() {
        let gate = HoldGate::new();
        assert!(
            !gate.drop_reply(|| panic!("an open gate must not close anything")),
            "an open gate answers as usual"
        );

        gate.drop_replies();
        let verdict = tokio::time::timeout(Duration::from_secs(5), gate.park())
            .await
            .expect("a dropping gate does not park");
        assert_eq!(verdict, Verdict::Pass, "the handler must run for real");
        assert!(gate.drop_reply(|| true), "the reply is dropped once closed");
        assert!(
            !gate.drop_reply(|| false),
            "a plane that cannot close answers as usual"
        );
        assert_eq!(
            gate.dropped.load(Ordering::SeqCst),
            1,
            "only a drop that actually closed counts"
        );
        assert_eq!(gate.refused.load(Ordering::SeqCst), 0);

        gate.release();
        assert!(!gate.drop_reply(|| panic!("a released gate must not close")));
        assert_eq!(gate.park().await, Verdict::Pass);
    }

    /// A drop-once gate drops exactly ONE reply and then answers as usual
    /// without a release — so the caller's own retry, re-issuing the same
    /// request, goes through. A dropping gate cannot stage that: it drops every
    /// attempt until a test-timed release, which turns "the retry finishes"
    /// into a race against the client's backoff.
    #[tokio::test]
    async fn a_drop_once_gate_drops_one_reply_then_reopens_by_itself() {
        let gate = HoldGate::new();
        gate.drop_one_reply();
        assert_eq!(gate.park().await, Verdict::Pass, "the handler must run");
        assert!(
            !gate.drop_reply(|| false),
            "a plane that cannot close neither drops nor spends the one drop"
        );
        assert!(
            gate.drop_reply(|| true),
            "the first closable reply is dropped"
        );
        assert!(
            !gate.drop_reply(|| panic!("a spent drop-once gate must not close")),
            "the retry's reply is answered"
        );
        assert_eq!(gate.current(), Mode::Open, "the gate reopened by itself");
        assert_eq!(gate.dropped.load(Ordering::SeqCst), 1);
    }

    /// Dropping a HELD gate wakes the parked request into its handler, not a
    /// refusal — so a test can stand in the pending window first and still
    /// end in the committed-but-unanswered state.
    #[tokio::test]
    async fn dropping_a_held_gate_wakes_the_parked_request_to_run() {
        let gate = Arc::new(HoldGate::new());
        gate.arm();
        // spawn-ok(test): a test-local park the test awaits after the switch
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park().await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;

        gate.drop_replies();
        let verdict = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("switching to drop wakes the parked request")
            .expect("the parked task did not panic");
        assert_eq!(verdict, Verdict::Pass);
        assert!(gate.drop_reply(|| true));
    }

    /// Gates are per kind — arming one read must not stall every other read the
    /// page issues, or the test cannot drive the app while it holds.
    #[tokio::test]
    async fn arming_one_kind_leaves_another_kind_open() {
        let registry = RpcHoldRegistry::default();
        registry.gate("fauna.inbox.mode.get").arm();

        assert!(registry.existing("fauna.inbox.mode.get").is_some());
        assert!(
            registry.existing("fauna.inbox.mode.set").is_none(),
            "an un-named kind must not even allocate a gate"
        );
    }

    /// A generous deadline poll, not a settle-sleep (`e2e-conventions.md`
    /// point 14): a green run returns on the first pass and only a genuine
    /// failure spends the budget.
    async fn wait_for(mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("condition never held within the 10s budget");
    }
}
