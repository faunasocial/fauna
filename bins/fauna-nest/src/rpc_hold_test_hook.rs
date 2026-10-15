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
//! `…/refuse?as=outdated` refuses with the nest's own typed update code
//! (`fauna.nest.outdated`) instead — the refusal a nest that needs updating
//! sends, for the rule that a send turned away for that reason says so and
//! offers no retry (`version-compatibility.md` § Dimension 4).
//! `…/refuse?as=unauthenticated` refuses with `fauna.protocol.unauthenticated`
//! — exactly what an OLDER peer nest's federation allowlist answers for a kind
//! it predates (`federation_channel.rs`, the allowlist-first gate). Armed on a
//! federation kind of the peer, it stands that peer in for an outdated one, so a
//! two-nest test can observe the originating nest's typed answer to the user
//! (`version-compatibility.md` § Dimension 5). The flavour is a
//! closed set ([`Refusal`]), never a caller-supplied code: a test names a
//! refusal the product can really meet, not a string it invented.
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
//! ## Passing the first N — a window AFTER some progress
//!
//! `POST …/{kind}?pass=N` arms the hold but lets the next `N` requests of the
//! kind run first, then parks every later one. It exists for the rules about
//! what a loop shows *part way through* — a transfer's time left, which only
//! exists once something has been processed (`mail-export.md` § Wizard steps
//! step 4: "elapsed + ETA") — where releasing a plain hold and re-arming it
//! before the loop's next request would be the very race this module ends. The
//! budget is spent at the gate, one request at a time, so "the second fetch
//! parks" is decided by arrival order alone, never by how fast the test can
//! re-arm. `passing` in the status reports what is left of it; `release`
//! zeroes it.
//!
//! ## Holding until shutdown — a request in flight when the nest stops
//!
//! `POST …/{kind}/hold-until-shutdown` arms a hold that ends by ITSELF when the
//! nest begins a graceful shutdown (`WsState::begin_shutdown`, the SIGTERM
//! path), as well as on `release`. It exists for the rule that a request an
//! app had already sent when the nest begins to stop is still finished and
//! answered before the connection closes (`transport-connection.md`
//! § Graceful shutdown). A plain hold cannot stage that: its release is an
//! HTTP call, and the first step of the shutdown stops the nest accepting
//! connections, so nothing could let the held request go once the stop had
//! begun. Woken by the shutdown flag, the parked request runs its real handler
//! while the connection drains, and its Reply is what the drain flushes ahead
//! of the 1001 close. `holding` is the arrival proof the test stops the nest
//! on, so "in flight" is a state the test has observed, never an interval it
//! timed. A request arriving after the shutdown began passes straight through.
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
//! `tests/e2e-unified/tests/test_settings.py` (the hold),
//! `tests/e2e-unified/tests/api/test_graceful_shutdown_in_flight.py` (the hold
//! until shutdown) and the other `rpc_hold` callers. Production never compiles
//! this module.

#![cfg(feature = "test-hooks")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
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
    /// Park until the mode changes OR the nest begins a graceful shutdown
    /// (`POST …/{kind}/hold-until-shutdown`, module doc § Holding until
    /// shutdown) — whichever comes first.
    HoldUntilShutdown,
    /// Answer with an error instead of running the handler (`POST
    /// …/{kind}/refuse`) — both the requests parked under `Hold` when it is
    /// set and every later arrival, until `release`. The failure a user meets
    /// when the nest turns a request away, as a state a test can stand in: a
    /// composer's failed send (`feed.md` § Errors & edge cases) has no ordinary
    /// input that provokes it.
    Refuse(Refusal),
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

/// Which error a refusing gate answers with — a closed set, chosen by the
/// `as` query of `POST …/{kind}/refuse` (module doc § Refusal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// `fauna.test.refused` — a code no app recognises, rendered as the generic
    /// `error.unexpected`: the refusal an app too old to know a newer nest's
    /// code meets.
    #[default]
    Unknown,
    /// `fauna.nest.outdated` — the nest's own "needs updating" refusal.
    Outdated,
    /// `fauna.protocol.unauthenticated` — what an older peer nest's federation
    /// allowlist answers for a kind it predates.
    Unauthenticated,
}

/// What [`HoldGate::park`] decided for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Run the handler, exactly as an un-armed request would.
    Pass,
    /// Reply with [`refused_error`] of this flavour and never run the handler.
    Refuse(Refusal),
}

/// The error a refused request answers with. [`Refusal::Unknown`] is the
/// generic `error.unexpected` string, so the app renders exactly what it renders
/// for any nest failure it has no specific copy for; [`Refusal::Outdated`] is
/// the degraded nest's own [`fauna_protocol::RpcError::nest_outdated`].
pub fn refused_error(refusal: Refusal) -> fauna_protocol::RpcError {
    match refusal {
        Refusal::Unknown => fauna_protocol::RpcError::new("fauna.test.refused", "error.unexpected"),
        Refusal::Outdated => fauna_protocol::RpcError::nest_outdated(),
        Refusal::Unauthenticated => crate::rpc_errors::unauthenticated(),
    }
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
    /// Requests a held gate still lets through before it starts parking
    /// (`POST …/{kind}?pass=N`, module doc § Passing the first N).
    passing: AtomicUsize,
}

impl HoldGate {
    fn new() -> Self {
        Self {
            mode: watch::channel(Mode::Open).0,
            holding: AtomicUsize::new(0),
            released: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            passing: AtomicUsize::new(0),
        }
    }

    /// Arm with no pass budget — the unit tests' shorthand; the route always
    /// passes its (possibly zero) budget through [`Self::arm_after`].
    #[cfg(test)]
    fn arm(&self) {
        self.arm_after(0);
    }

    /// Arm the hold, letting the next `pass` requests run first. The budget is
    /// stored BEFORE the mode flips, so a request arriving in between sees the
    /// gate still open rather than a hold with no budget.
    fn arm_after(&self, pass: usize) {
        self.passing.store(pass, Ordering::SeqCst);
        // `send_replace`, never `send`: `Sender::send` FAILS — and leaves the
        // value untouched — when no receiver is currently subscribed, which is
        // the normal state of a gate nothing is parked in. Armed with `send`
        // this gate silently stayed disarmed and every hold was a no-op; the
        // unit tests below caught it, and they are the reason to keep them.
        self.mode.send_replace(Mode::Hold);
    }

    /// Arm a hold that the nest's graceful shutdown ends by itself (module doc
    /// § Holding until shutdown). No pass budget: the window under test is the
    /// first request's.
    fn arm_until_shutdown(&self) {
        self.passing.store(0, Ordering::SeqCst);
        self.mode.send_replace(Mode::HoldUntilShutdown);
    }

    /// Refuse every request of this kind until `release` — waking the parked
    /// ones into the same refusal.
    fn refuse(&self, refusal: Refusal) {
        self.mode.send_replace(Mode::Refuse(refusal));
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
        self.passing.store(0, Ordering::SeqCst);
        // `send_replace` for the same reason as `arm` — and here it also wakes
        // every current waiter, which is the whole point.
        self.mode.send_replace(Mode::Open);
        self.released.load(Ordering::SeqCst)
    }

    fn current(&self) -> Mode {
        *self.mode.borrow()
    }

    fn is_armed(&self) -> bool {
        matches!(self.current(), Mode::Hold | Mode::HoldUntilShutdown)
    }

    /// Park while held, then pass or refuse. Returns immediately when the gate
    /// is open or refusing — the un-armed path is one load, which is what keeps
    /// this callable unconditionally from the dispatch hot path.
    ///
    /// `shutdown` is the nest's graceful-shutdown flag
    /// (`WsState::subscribe_shutdown`); only [`Mode::HoldUntilShutdown`] reads
    /// it. A receiver whose sender is gone counts as "never shutting down".
    async fn park(&self, mut shutdown: watch::Receiver<bool>) -> Verdict {
        let verdict = match self.current() {
            // Already stopping: the request this mode stages is one sent
            // BEFORE the stop, so a later arrival runs as usual.
            Mode::HoldUntilShutdown if *shutdown.borrow() => return Verdict::Pass,
            // A dropping gate lets the handler run; the drop is the reply
            // task's decision, made once the handler is done.
            Mode::Open | Mode::DropReply | Mode::DropReplyOnce => return Verdict::Pass,
            Mode::Refuse(refusal) => Verdict::Refuse(refusal),
            // One request of the pass budget: spent here, atomically, so two
            // arriving together cannot both take the last one.
            Mode::Hold
                if self
                    .passing
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok() =>
            {
                return Verdict::Pass;
            }
            Mode::Hold | Mode::HoldUntilShutdown => {
                // `_guard` decrements `holding` on ANY exit — including the task
                // being aborted mid-park, which is exactly what a client `Cancel`
                // frame does. Without it a cancelled request leaves the count
                // permanently high and the next test's arrival poll passes on a
                // ghost.
                let _guard = HoldingGuard::new(self);
                let mut rx = self.mode.subscribe();
                let mut shutdown_gone = false;
                // Both watches are re-read each pass with `borrow_and_update`,
                // so a flip landing between the read and the `changed()` await
                // is still seen: `changed()` resolves for any value newer than
                // the last one marked seen.
                let woke_to = loop {
                    let mode = *rx.borrow_and_update();
                    let shutting_down = !shutdown_gone && *shutdown.borrow_and_update();
                    match mode {
                        Mode::Hold => {}
                        Mode::HoldUntilShutdown if !shutting_down => {}
                        // The stop began: let the parked request go, to be
                        // finished and answered inside the connection's drain.
                        Mode::HoldUntilShutdown => break Mode::Open,
                        other => break other,
                    }
                    tokio::select! {
                        // Errors only if the sender is dropped; the registry
                        // owns it for the process's life, and a vanished
                        // sender means "nothing can re-arm this", so passing
                        // is the right reading either way.
                        changed = rx.changed() => {
                            if changed.is_err() {
                                break Mode::Open;
                            }
                        }
                        changed = shutdown.changed(), if !shutdown_gone => {
                            if changed.is_err() {
                                shutdown_gone = true;
                            }
                        }
                    }
                };
                self.released.fetch_add(1, Ordering::SeqCst);
                match woke_to {
                    Mode::Refuse(refusal) => Verdict::Refuse(refusal),
                    _ => Verdict::Pass,
                }
            }
        };
        if matches!(verdict, Verdict::Refuse(_)) {
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
            Mode::Open | Mode::Hold | Mode::HoldUntilShutdown | Mode::Refuse(_) => return false,
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
        Some(gate) => gate.park(state.ws.subscribe_shutdown()).await,
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

/// The `pass` query of `POST …/{kind}`; absent means park from the next request.
#[derive(serde::Deserialize, Default)]
struct ArmQuery {
    #[serde(default)]
    pass: usize,
}

/// `POST /api/v1/test/rpc-hold/{kind}[?pass=N]` — arm the hold. Every subsequent
/// request of `kind` — after the first `N`, which run as usual (module doc
/// § Passing the first N) — parks until `release`. Returns
/// `{"ok": true, "kind": <kind>}`.
async fn arm_hold(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
    Query(query): Query<ArmQuery>,
) -> impl IntoResponse {
    if !known_kind(&state, &kind) {
        return ApiError::bad_request(format!(
            "no RPC kind {kind:?} is registered on this nest — check the spelling \
             against the router registration, not the client-side method name"
        ))
        .into_response();
    }
    state.rpc_hold.gate(&kind).arm_after(query.pass);
    Json(json!({ "ok": true, "kind": kind })).into_response()
}

/// The `as` query of `POST …/{kind}/refuse`; absent means [`Refusal::Unknown`].
#[derive(serde::Deserialize, Default)]
struct RefuseQuery {
    #[serde(default, rename = "as")]
    refusal: Refusal,
}

/// `POST /api/v1/test/rpc-hold/{kind}/hold-until-shutdown` — park every
/// subsequent request of `kind` until `release` or until the nest begins a
/// graceful shutdown, whichever comes first (module doc § Holding until
/// shutdown). Returns `{"ok": true, "kind": <kind>}`.
async fn arm_hold_until_shutdown(
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
    state.rpc_hold.gate(&kind).arm_until_shutdown();
    Json(json!({ "ok": true, "kind": kind })).into_response()
}

/// `POST /api/v1/test/rpc-hold/{kind}/refuse[?as=outdated|unauthenticated]` — answer every
/// request of `kind` with [`refused_error`] of that flavour until `release`,
/// including any parked by an earlier hold. Returns `{"ok": true, "kind": <kind>}`.
async fn refuse_kind(
    State(state): State<Arc<AppState>>,
    Path(kind): Path<String>,
    Query(query): Query<RefuseQuery>,
) -> impl IntoResponse {
    if !known_kind(&state, &kind) {
        return ApiError::bad_request(format!(
            "no RPC kind {kind:?} is registered on this nest — check the spelling \
             against the router registration, not the client-side method name"
        ))
        .into_response();
    }
    state.rpc_hold.gate(&kind).refuse(query.refusal);
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
/// Returns `{"armed": bool, "until_shutdown": bool, "refusing": bool,
/// "dropping": bool, "holding": N,
/// "passing": P, "released": M, "refused": R, "dropped": D}`, where `passing` is
/// what is left of an armed hold's pass budget and `holding` is the number of
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
            "until_shutdown": gate.current() == Mode::HoldUntilShutdown,
            "refusing": matches!(gate.current(), Mode::Refuse(_)),
            "dropping": matches!(gate.current(), Mode::DropReply | Mode::DropReplyOnce),
            "holding": gate.holding.load(Ordering::SeqCst),
            "passing": gate.passing.load(Ordering::SeqCst),
            "released": gate.released.load(Ordering::SeqCst),
            "refused": gate.refused.load(Ordering::SeqCst),
            "dropped": gate.dropped.load(Ordering::SeqCst),
        })),
        // Never armed is a legitimate answer, not an error: a test polling a
        // kind it has not armed should see zeros, not a 404 it has to special-case.
        None => Json(json!({
            "armed": false,
            "until_shutdown": false,
            "refusing": false,
            "dropping": false,
            "holding": 0,
            "passing": 0,
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
            "/api/v1/test/rpc-hold/{kind}/hold-until-shutdown",
            post(arm_hold_until_shutdown),
        )
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
        tokio::time::timeout(Duration::from_secs(5), gate.park(no_shutdown()))
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
            async move { gate.park(no_shutdown()).await }
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
                tokio::spawn(async move { gate.park(no_shutdown()).await })
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

    /// `?pass=N`: the first N requests run, the next one parks — decided by
    /// arrival order at the gate, so a test can stand in the window after a
    /// loop's first request without racing a re-arm. Release zeroes an unspent
    /// budget, so a later plain `arm` parks from its first request.
    #[tokio::test]
    async fn a_pass_budget_lets_the_first_n_through_then_parks() {
        let gate = Arc::new(HoldGate::new());
        gate.arm_after(2);
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(5), gate.park(no_shutdown()))
                .await
                .expect("a request inside the pass budget does not park");
        }
        assert_eq!(gate.passing.load(Ordering::SeqCst), 0);
        // spawn-ok(test): a test-local park the test awaits after release
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park(no_shutdown()).await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;
        assert!(
            !parked.is_finished(),
            "the request after the budget must park"
        );
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("release wakes the parked request")
            .expect("the parked task did not panic");

        gate.arm_after(5);
        gate.release();
        assert_eq!(
            gate.passing.load(Ordering::SeqCst),
            0,
            "release zeroes the budget"
        );
        gate.arm();
        // spawn-ok(test): a test-local park the test awaits after release
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park(no_shutdown()).await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("release wakes the parked request")
            .expect("the parked task did not panic");
    }

    /// A request that arrives after the release must not park — otherwise the
    /// convergence half of the journey (release, then watch the UI settle)
    /// hangs on the app's next read.
    #[tokio::test]
    async fn a_request_arriving_after_release_passes_straight_through() {
        let gate = HoldGate::new();
        gate.arm();
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), gate.park(no_shutdown()))
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
            async move { gate.park(no_shutdown()).await }
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
            async move { gate.park(no_shutdown()).await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;

        gate.refuse(Refusal::Unknown);
        let verdict = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("refuse wakes the parked request")
            .expect("the parked task did not panic");
        assert_eq!(verdict, Verdict::Refuse(Refusal::Unknown));

        let later = tokio::time::timeout(Duration::from_secs(5), gate.park(no_shutdown()))
            .await
            .expect("a refusing gate does not park later arrivals");
        assert_eq!(later, Verdict::Refuse(Refusal::Unknown));
        assert_eq!(gate.refused.load(Ordering::SeqCst), 2);
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);

        gate.release();
        assert_eq!(gate.park(no_shutdown()).await, Verdict::Pass);
        assert_eq!(gate.refused.load(Ordering::SeqCst), 2);
    }

    /// `?as=outdated` refuses with the nest's own typed update code, which the
    /// shared classifier routes to the update prompt; the default stays a code
    /// no app recognises.
    #[tokio::test]
    async fn refuse_as_outdated_answers_the_typed_update_code() {
        let gate = HoldGate::new();
        gate.refuse(Refusal::Outdated);
        assert_eq!(
            gate.park(no_shutdown()).await,
            Verdict::Refuse(Refusal::Outdated)
        );
        let outdated = refused_error(Refusal::Outdated);
        assert_eq!(outdated.code, fauna_protocol::RpcError::CODE_NEST_OUTDATED);
        assert_eq!(
            outdated.action(),
            fauna_protocol::RpcErrorAction::NeedsUpdate
        );
        let unknown = refused_error(Refusal::Unknown);
        assert_eq!(unknown.code, "fauna.test.refused");
        assert_eq!(unknown.action(), fauna_protocol::RpcErrorAction::Rejected);

        let parsed: RefuseQuery = serde_json::from_value(json!({"as": "outdated"})).unwrap();
        assert_eq!(parsed.refusal, Refusal::Outdated);
        let absent: RefuseQuery = serde_json::from_value(json!({})).unwrap();
        assert_eq!(absent.refusal, Refusal::Unknown);
    }

    /// `?as=unauthenticated` refuses with the very error an older peer's
    /// federation allowlist sends for a kind it predates — the input
    /// `rpc_errors::map_peer_relay_error` turns into `peer_nest_outdated`.
    #[test]
    fn refuse_as_unauthenticated_answers_the_old_peer_allowlist_code() {
        let e = refused_error(Refusal::Unauthenticated);
        assert_eq!(e.code, "fauna.protocol.unauthenticated");
        let mapped = crate::rpc_errors::map_peer_relay_error(e, "a relay");
        assert_eq!(mapped.code, "fauna.federation.peer_nest_outdated");
        let parsed: RefuseQuery = serde_json::from_value(json!({"as": "unauthenticated"})).unwrap();
        assert_eq!(parsed.refusal, Refusal::Unauthenticated);
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
        let verdict = tokio::time::timeout(Duration::from_secs(5), gate.park(no_shutdown()))
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
        assert_eq!(gate.park(no_shutdown()).await, Verdict::Pass);
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
        assert_eq!(
            gate.park(no_shutdown()).await,
            Verdict::Pass,
            "the handler must run"
        );
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
            async move { gate.park(no_shutdown()).await }
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

    /// A hold-until-shutdown parks like a hold and is let go by the shutdown
    /// flag ALONE — no release, which is the property the graceful-shutdown
    /// witness rests on: once the stop has begun, no HTTP release could reach
    /// the nest.
    #[tokio::test]
    async fn a_hold_until_shutdown_parks_until_the_shutdown_flag_flips() {
        let gate = Arc::new(HoldGate::new());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        gate.arm_until_shutdown();
        assert!(gate.is_armed());
        // spawn-ok(test): a test-local park the test awaits after the flip
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park(shutdown_rx).await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;
        assert!(!parked.is_finished(), "the gate must park before the stop");

        shutdown_tx.send_replace(true);
        let verdict = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("the shutdown flag wakes the parked request")
            .expect("the parked task did not panic");
        assert_eq!(verdict, Verdict::Pass, "the handler must run for real");
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);
        assert_eq!(gate.released.load(Ordering::SeqCst), 1);
    }

    /// `release` still ends a hold-until-shutdown — the `finally` a test runs
    /// when it fails before it stops the nest — and a plain hold is NOT ended by
    /// the shutdown flag, so the existing hold keeps its meaning.
    #[tokio::test]
    async fn release_ends_a_hold_until_shutdown_and_shutdown_leaves_a_plain_hold_parked() {
        let gate = Arc::new(HoldGate::new());
        gate.arm_until_shutdown();
        // spawn-ok(test): a test-local park the test awaits after release
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park(no_shutdown()).await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("release wakes a hold-until-shutdown")
            .expect("the parked task did not panic");

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        gate.arm();
        // spawn-ok(test): a test-local park the test releases below
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.park(shutdown_rx).await }
        });
        wait_for(|| gate.holding.load(Ordering::SeqCst) == 1).await;
        shutdown_tx.send_replace(true);
        tokio::task::yield_now().await;
        assert!(
            !parked.is_finished(),
            "a plain hold is ended by release only, never by the shutdown flag"
        );
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("release wakes the plain hold")
            .expect("the parked task did not panic");
    }

    /// A request arriving after the stop began was not in flight when it
    /// began, so it does not park.
    #[tokio::test]
    async fn a_hold_until_shutdown_passes_a_request_arriving_after_the_stop() {
        let gate = HoldGate::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        shutdown_tx.send_replace(true);
        gate.arm_until_shutdown();
        let verdict = tokio::time::timeout(Duration::from_secs(5), gate.park(shutdown_rx))
            .await
            .expect("an arrival after the stop does not park");
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(gate.holding.load(Ordering::SeqCst), 0);
    }

    /// The shutdown flag of a nest that never stops: its sender is already
    /// gone, which `park` reads as "never shutting down".
    fn no_shutdown() -> watch::Receiver<bool> {
        watch::channel(false).1
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
