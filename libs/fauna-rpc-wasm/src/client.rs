//! `WsRpcClient` — the wasm transport client: owns an `RpcDispatcher` over a
//! gloo-net `WebSocket`, drives it via `spawn_local`, runs a minimal reconnect
//! loop, and implements `fauna_protocol::RpcRequester` so the shared
//! `BridgesClient`/`EmailClient` wrappers run unchanged on web.
//!
//! Single-threaded by construction (browser), so everything is `Rc`/`RefCell`
//! and the `RpcRequester` future is `!Send` — exactly what the runtime-agnostic
//! dispatcher (Phase 1) and the per-impl-`Send` `RpcRequester` seam (Phase 2)
//! were built to allow.
//!
//! ## What this crate does *not* re-implement
//!
//! The typed request ceremony — encode → `request_raw` → `await_reply` → decode
//! → classify — was mirrored here from `fauna_client::NestClient::request_inner`
//! until it moved onto the dispatcher itself as
//! [`RpcDispatcher::request_typed`]. Both transports
//! now call that; the only wasm-shaped part left is the deadline backstop, since
//! `tokio::time` does not exist on `wasm32` — so this crate hands the shared
//! path a `gloo_timers` `TimeoutFuture` where native hands it a
//! `tokio::time::sleep`.
//!
//! [`run_reconnect_loop`] still mirrors `fauna_ws_substrate::supervisor`'s
//! close-code reaction table, and that one stays mirrored on purpose: the
//! *policy* matches, but the drivers do not (tokio-spawned and `Send` there,
//! cooperative single-thread and `Rc` here), and `transport.md`'s ratified
//! wasm-only divergences — the force-refresh bearer mint and the establish
//! probe — are real behavioral differences, not incidental drift. Unifying the
//! two would need a spawner abstraction injected into a policy-only core for a
//! two-call-site win; the shared *policy* pieces that factor cleanly already
//! have (`fauna_protocol::reconnect::{Backoff, probe_established}`, and the
//! unreachable threshold in `fauna_core::format`).

use std::cell::{Cell, RefCell};
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use futures_util::future::{Either, select};
use gloo_timers::future::TimeoutFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use tokio::sync::{broadcast, oneshot};

use fauna_core::format::CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES;
use fauna_protocol::reconnect::{
    Backoff, ESTABLISH_PROBE, Establish, demand_nap, probe_established,
};
use fauna_protocol::{KindRegistry, PushEvent, RpcDispatcher, RpcError, RpcRequester};

use crate::adapter::{GlooAdapter, ReconnectSignal};
use crate::error::WsRpcError;
use crate::shared_port::SharedPort;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How long a freshly-dialled browser `WebSocket` must survive before the loop
/// treats it as **established** — resets its backoff and announces `Connected`.
/// The browser `WebSocket` constructor is synchronous handle creation and its
/// `Ok` proves nothing (a down nest hands back a live handle whose failure
/// surfaces asynchronously on the stream), so the loop can't reset on the dial
/// the way native resets on its real `connect().await`. Survival for this window
/// is the dep-agnostic proxy (`transport.md` § Connection lifecycle,
/// established-signal option (a)). The window is the shared
/// [`ESTABLISH_PROBE`], in the milliseconds `TimeoutFuture` takes.
const ESTABLISH_PROBE_MS: u32 = ESTABLISH_PROBE.as_millis() as u32;

/// Full-jitter the current backoff ceiling into a concrete nap, in ms, using the
/// browser RNG. Delegates the formula to the shared `Backoff::jittered`
/// (`fauna_protocol::reconnect`) — the same code the native supervisor's jitter
/// is being unified onto — so the "uniform draw in `[0, ceiling]`" rule has one
/// definition, not a wasm twin that can drift from native. `js_sys::Math::random`
/// supplies the `[0, 1)` draw; jitter is not security-sensitive, and each tab's
/// independent draw is what decorrelates the thundering herd a nest redeploy
/// creates (every tab drops at once carrying the same reset ceiling).
fn jittered_backoff_ms(backoff: &Backoff) -> u32 {
    backoff.jittered(jitter_unit()).as_millis() as u32
}

/// The `[0, 1)` draw every nap of the reconnect loop jitters with:
/// `Math::random()`, or under this crate's own tests a fixed unit
/// (`tests::FIXED_JITTER`), so a test can count dials against an exact curve.
fn jitter_unit() -> f64 {
    #[cfg(test)]
    if let Some(unit) = tests::FIXED_JITTER.get() {
        return unit;
    }
    js_sys::Math::random()
}
pub(crate) const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);
/// Poll interval while waiting for the reconnect loop to repopulate the
/// dispatcher. The wasm client has no watch channel to await on (unlike the
/// native `connection_state()`), so `request` polls the `RefCell` instead.
const RECONNECT_POLL_MS: u32 = 50;

/// Connection lifecycle state, mirrored to JS via `connection_state()`. Local
/// copy of `fauna_client::types::ConnectionState` (that type lives in the
/// native-only `fauna-client`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
    /// Connecting has failed
    /// [`CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES`] times in a row with
    /// no established connection in between — the wasm twin of
    /// `fauna_ws_substrate::supervisor::ConnectionState::Unreachable` (which
    /// carries the full rationale). Reached here by the *same* shared threshold,
    /// so web and native agree on when a gap stops being transient.
    ///
    /// This is the state the browser most needs and can least explain: the
    /// `WebSocket` API hides both the upgrade status and the TLS failure reason
    /// (see the `Retry` arm below), so the SPA can prove the connection is
    /// persistently failing but never *why* — which is exactly why the state is
    /// generic rather than naming a certificate.
    Unreachable,
}

impl ConnectionState {
    /// The JS-facing string the SPA's `connection-status` indicator reads —
    /// the same `"connecting" | "connected" | "disconnected" | "unreachable"`
    /// spelling `fauna-wasm`'s `connectionState()` snapshot getter emits, so the
    /// live `setOnConnectionStateChanged` callback and the snapshot read agree.
    /// These are also the words `fauna_core::format::connection_state_label`
    /// keys on, so every app family maps them identically.
    fn as_js_str(self) -> &'static str {
        match self {
            ConnectionState::Connecting => "connecting",
            ConnectionState::Connected => "connected",
            ConnectionState::Disconnected => "disconnected",
            ConnectionState::Unreachable => "unreachable",
        }
    }

    /// The inverse of [`Self::as_js_str`] — how a chunk client reads the state
    /// word the shared rpc port's owner reports (`crate::shared_port`). `None`
    /// for a word this build does not know, which the caller reads as the
    /// weaker `Disconnected`.
    pub(crate) fn from_js_str(word: &str) -> Option<Self> {
        match word {
            "connecting" => Some(ConnectionState::Connecting),
            "connected" => Some(ConnectionState::Connected),
            "disconnected" => Some(ConnectionState::Disconnected),
            "unreachable" => Some(ConnectionState::Unreachable),
            _ => None,
        }
    }
}

/// Why this client's reconnect loop stopped for good, kept in a form every
/// later request can be failed with. The wasm twin of `fauna_client`'s
/// `SupervisorStop`, holding only the three endings this loop has: it backs off
/// on any other token-provider failure rather than stopping, and the browser
/// shows it neither the nest's identity nor an upgrade refusal.
///
/// **No current nest reaches either ending.** A nest never closes a per-actor
/// socket with 1000 (it shuts down with 1001), and never with 4426: it refuses
/// a subprotocol mismatch with an HTTP 426 at the upgrade, which the browser
/// reports as an ordinary close, `Retry`. So a version-skewed web tab keeps
/// retrying and ends on "Cannot connect", not on this error. The record is
/// parity with native and a pin for the close-frame path, should a nest ever
/// send either code (`transport-connection.md` § Connection lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorStop {
    /// A clean close (1000): nothing to name beyond "no connection will come",
    /// the answer a closed client gives too.
    Clean,
    /// A 4426 close: the nest rejected this client's subprotocol.
    SubprotocolMismatch,
    /// The token provider's re-mint was refused `fauna.auth.not_registered`:
    /// the nest stopped signing this identity in (suspended or removed while
    /// signed in), met on the re-mint after the nest's revocation teardown
    /// closed the socket with 4401. No retry can clear it, so the loop stops
    /// rather than backing off — the twin of native's `SupervisorStop::Refused`
    /// (`onboarding.md` § App-launch routing, the previously-signed-in row:
    /// the same verdict mid-session lands the same surface).
    Refused,
}

impl SupervisorStop {
    /// The error a request on the stopped client fails with.
    fn error(self) -> WsRpcError {
        match self {
            Self::Clean => WsRpcError::NotConnected,
            Self::SubprotocolMismatch => WsRpcError::SubprotocolMismatch,
            Self::Refused => WsRpcError::from(RpcError::not_registered()),
        }
    }
}

/// Wraps the JS token-provider callback `(forceRefresh: boolean) =>
/// Promise<string>`. Called on each connect, with `force = true` after a 4401
/// so the TS side busts its bearer cache (mirrors native `clear_token` +
/// `ensure_auth`).
struct TokenProvider(js_sys::Function);

impl TokenProvider {
    async fn fetch(&self, force_refresh: bool) -> Result<String, WsRpcError> {
        let ret = self
            .0
            .call1(&JsValue::NULL, &JsValue::from_bool(force_refresh))
            .map_err(|e| WsRpcError::Token(format!("callback threw: {e:?}")))?;
        // Accept either a Promise or a bare value (resolve() is a no-op on a
        // Promise, and wraps a plain value into a resolved one).
        let promise = js_sys::Promise::resolve(&ret);
        let value = JsFuture::from(promise).await.map_err(|e| {
            if is_sign_in_refusal(&e) {
                WsRpcError::from(RpcError::not_registered())
            } else {
                WsRpcError::Token(format!("promise rejected: {e:?}"))
            }
        })?;
        value
            .as_string()
            .ok_or_else(|| WsRpcError::Token("provider returned a non-string".into()))
    }
}

/// Whether a token provider's rejection is the nest's
/// `fauna.auth.not_registered` refusal: the SPA's `getAuthToken` rejects with
/// a `SignInRefusedError` whose `code` property is the wire code
/// (`apps/fauna-web/src/lib/auth-errors.ts`). Keyed on the wire code, never
/// the human message, so the text can change without breaking the stop.
fn is_sign_in_refusal(rejection: &JsValue) -> bool {
    js_sys::Reflect::get(rejection, &JsValue::from_str("code"))
        .ok()
        .and_then(|code| code.as_string())
        .is_some_and(|code| code == RpcError::CODE_NOT_REGISTERED)
}

struct Inner {
    /// The nest base URL — a `RefCell` so the reconnect loop can SRV-swap it to a
    /// new client-facing port after an admin serving-port change (the wasm twin
    /// of native `AuthClient`'s shared URL cell). Single-threaded (browser), so a
    /// plain `RefCell` suffices.
    nest_url: RefCell<String>,
    actor_id_hex: String,
    token_provider: TokenProvider,
    kind_registry: KindRegistry,
    /// Current dispatcher; set by the reconnect loop on connect, cleared on
    /// each WS termination. `request` reads this.
    dispatcher: RefCell<Option<Rc<RpcDispatcher>>>,
    connection_state: RefCell<ConnectionState>,
    /// JS `() => void` fired on every *reconnect* — a `Connected` after the
    /// first connect, never the initial one — so the SPA re-pulls surfaces with
    /// no poll backstop (the feed). The wasm twin of native
    /// `fauna_client::NestClient::subscribe_reconnects` (`transport.md`
    /// § Push events: "application observers re-pull through their
    /// snapshot-refresh path" on reconnect). `None` until the SPA registers one.
    on_reconnected: RefCell<Option<js_sys::Function>>,
    /// JS `(state: "connecting"|"connected"|"disconnected") => void` fired on
    /// every connection-state *transition* (not just reconnects, unlike
    /// `on_reconnected`), so the SPA's global `connection-status` indicator
    /// reflects Connecting/Connected/Disconnected live. The wasm twin of a
    /// native app observing `fauna_client::NestClient::connection_state()`'s
    /// `watch` (as linux does for its top-of-sidebar indicator). `None` until
    /// the SPA registers one.
    on_connection_state_changed: RefCell<Option<js_sys::Function>>,
    /// JS `(kind: string, payload: object) => void` fired for every server-pushed
    /// frame the dispatcher decodes. The wasm twin of native
    /// `fauna_client::NestClient::subscribe_pushes` (`transport.md` § Push
    /// events) — same `fauna_protocol::PushEvent` set, same kind strings, so a
    /// web surface reacts to a push exactly as its linux/apple twin does.
    /// `payload` is the variant's payload object bare (`PushEvent` serializes
    /// untagged), so no per-kind marshalling exists here to drift. `None` until
    /// the SPA registers one.
    on_push_event: RefCell<Option<js_sys::Function>>,
    /// The reconnect counter's Rust face — bumped beside `on_reconnected`
    /// (every `Connected` after the first), the wasm twin of native
    /// `NestClient::subscribe_reconnects`. The account driver hosted in this
    /// tab (`fauna_account_plane::web_host`) re-publishes and re-walks on it.
    reconnects: tokio::sync::watch::Sender<u64>,
    /// Every decoded push, re-broadcast to Rust subscribers beside the JS
    /// callback — the wasm twin of native `NestClient::subscribe_pushes`, the
    /// account driver's push→nudge arm. One sender for the client's life, so
    /// a subscription survives reconnects (each connection's own broadcast
    /// dies with it).
    pushes: broadcast::Sender<PushEvent>,
    /// `false` until the first successful connect, then permanently `true`. Gates
    /// `on_reconnected` so the initial connect doesn't count as a reconnect
    /// (mirrors native's watch, which bumps on every `Connected` *after the
    /// first*).
    has_connected: Cell<bool>,
    /// Set once by [`WsRpcClient::close`]; the reconnect loop observes it and
    /// exits instead of reconnecting, and `request` fails fast instead of
    /// waiting out its deadline on a dispatcher that will never return.
    closed: Cell<bool>,
    /// Why the reconnect loop stopped for good, if it has: recorded as the loop
    /// returns and never cleared, since a client is one-way and a new one starts
    /// clean. Like `closed` it means no connection will come, so a request fails
    /// at once, here with the reason. The wasm twin of native `NestClient`'s
    /// `stopped_by` (`transport-connection.md` § Connection lifecycle).
    stopped_by: Cell<Option<SupervisorStop>>,
    /// Wakes the reconnect loop out of whatever it is parked on (a live
    /// connection's driver, a backoff sleep) so `close()` takes effect
    /// immediately rather than at the next natural wake. One-shot by design:
    /// closing is one-way.
    close_tx: RefCell<Option<oneshot::Sender<()>>>,
    /// The e2e reconnect PACE (`fauna_e2e_agent::RECONNECT_BACKOFF`): `Some`
    /// replaces the loop's backoff bounds at its next backed-off nap, `None`
    /// restores the production ones. The wasm twin of native
    /// `SupervisedChannel::backoff_override` — the pace only, never the
    /// `Unreachable` threshold, so every failure the threshold counts is still
    /// a real refused dial. Compiled out of a production bundle.
    #[cfg(any(test, feature = "test-helpers"))]
    backoff_override: Cell<Option<(Duration, Duration)>>,
    /// Requests parked on this client's connection, as the reconnect loop sees
    /// them ([`DialDemand`]).
    demand: DialDemand,
}

/// Requests parked on a client's connection — `transport.md` § Request
/// lifecycle step 3, the in-gap wait — as the reconnect loop sees them. The
/// wasm twin of `fauna_ws_substrate::DialDemand`, which carries the rationale
/// (`transport-connection.md` § Connection lifecycle): while anything waits, a
/// refused dial is retried within the initial ceiling, and a request that starts
/// waiting cuts a longer nap short; those extra dials neither grow the curve nor
/// count toward `Unreachable`. Single-threaded, so a `Cell` count; the wake-up
/// is a `Notify`, whose stored permit covers a waiter arriving while the loop is
/// not parked on it.
#[derive(Default)]
struct DialDemand {
    waiting: Cell<usize>,
    arrived: tokio::sync::Notify,
}

impl DialDemand {
    fn is_waiting(&self) -> bool {
        self.waiting.get() > 0
    }
}

/// One waiter's hold on its client's [`DialDemand`]; dropping it ends the wait.
struct DialDemandGuard(Rc<Inner>);

impl DialDemandGuard {
    fn begin(inner: &Rc<Inner>) -> Self {
        inner.demand.waiting.set(inner.demand.waiting.get() + 1);
        inner.demand.arrived.notify_one();
        Self(Rc::clone(inner))
    }
}

impl Drop for DialDemandGuard {
    fn drop(&mut self) {
        let demand = &self.0.demand;
        demand.waiting.set(demand.waiting.get().saturating_sub(1));
    }
}

impl Inner {
    /// A client's state before its reconnect loop has run: `Connecting`, no
    /// dispatcher, nothing closed or stopped.
    fn new(nest_url: String, actor_id_hex: String, token_provider: js_sys::Function) -> Self {
        Self {
            nest_url: RefCell::new(nest_url),
            actor_id_hex,
            token_provider: TokenProvider(token_provider),
            // Same registry as native NestClient::with_auth — protocol kinds
            // only; bridges/email deadlines fall back to 30 s, matching native.
            kind_registry: KindRegistry::full(),
            dispatcher: RefCell::new(None),
            connection_state: RefCell::new(ConnectionState::Connecting),
            on_reconnected: RefCell::new(None),
            on_connection_state_changed: RefCell::new(None),
            on_push_event: RefCell::new(None),
            reconnects: tokio::sync::watch::channel(0).0,
            pushes: broadcast::channel(256).0,
            has_connected: Cell::new(false),
            closed: Cell::new(false),
            stopped_by: Cell::new(None),
            close_tx: RefCell::new(None),
            #[cfg(any(test, feature = "test-helpers"))]
            backoff_override: Cell::new(None),
            demand: DialDemand::default(),
        }
    }
}

/// Which socket a [`WsRpcClient`] speaks over. One type, two dial modes, so
/// every consumer generic over the client — the page machines' `nest_api`
/// seams, the blob uploaders, `arc_from_secret_hex` — is
/// built the same way whichever chunk builds it.
#[derive(Clone)]
enum Transport {
    /// This client dials: it owns the socket and the reconnect loop. The SPA
    /// core chunk's singleton, and the both-ends pairing seam's client to a
    /// *peer* nest (a second nest is a second socket by design).
    Own(Rc<Inner>),
    /// This client borrows the core chunk's socket through the shared rpc port
    /// (`crate::shared_port`): no loop, no socket, no bearer of its own. Every
    /// other wasm chunk's client — `transport.md` § Goal's one socket per
    /// actor holds across chunks because of this arm.
    Shared(Rc<SharedPort>),
}

/// The wasm WS-RPC client. Cheaply cloneable handle (`Rc` inside).
#[derive(Clone)]
pub struct WsRpcClient {
    transport: Transport,
}

impl WsRpcClient {
    /// Open a WS-RPC connection and start the reconnect loop. Returns
    /// immediately; the loop runs on `spawn_local`. `token_provider` is a JS
    /// `(forceRefresh: boolean) => Promise<string>` yielding the bearer.
    ///
    /// **Only the SPA core chunk (and the pairing seam's peer-nest client)
    /// dial.** A page-machine chunk builds its client with [`Self::over_port`]
    /// instead, so web keeps one socket per actor.
    pub fn connect(
        nest_url: String,
        actor_id_hex: String,
        token_provider: js_sys::Function,
    ) -> Self {
        let inner = Rc::new(Inner::new(nest_url, actor_id_hex, token_provider));
        let (close_tx, close_rx) = oneshot::channel();
        *inner.close_tx.borrow_mut() = Some(close_tx);
        spawn_local(run_reconnect_loop(Rc::clone(&inner), close_rx));
        Self {
            transport: Transport::Own(inner),
        }
    }

    /// Build a client over the SPA core chunk's socket, lent through `port` —
    /// a `SharedRpcPort` (`crate::shared_port`; the SPA's `rpc.ts` builds it
    /// over its singleton). Refuses an object missing any of the port's five
    /// methods, by name. The client has no reconnect loop of its own: its
    /// requests run on the owner's socket through the owner's reconnect-wait,
    /// so it is exactly as connected as the app's `connection` indicator says.
    pub fn over_port(port: JsValue) -> Result<Self, WsRpcError> {
        Ok(Self {
            transport: Transport::Shared(Rc::new(SharedPort::new(port)?)),
        })
    }

    /// The owning transport, for the loop-side paths that only exist there.
    fn own(&self) -> Option<&Rc<Inner>> {
        match &self.transport {
            Transport::Own(inner) => Some(inner),
            Transport::Shared(_) => None,
        }
    }

    /// A hook that only the socket's owner can honour, registered on a
    /// port-built client: loud, never silent — a chunk that needs pushes or
    /// reconnect ticks extends the port rather than registering here.
    fn warn_owner_only(&self, what: &str) {
        tracing::warn!(
            "`{what}` registered on a shared-port client — the socket's owner \
             (the SPA core chunk) holds the callbacks; this registration is inert"
        );
    }

    /// This client's reconnect counter — `None` on a port-built client, whose
    /// socket (and so its reconnects) are the owner's.
    pub fn subscribe_reconnects(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.own().map(|inner| inner.reconnects.subscribe())
    }

    /// This client's decoded pushes, across reconnects — `None` on a
    /// port-built client (the owner holds the socket's pushes).
    pub fn subscribe_pushes(&self) -> Option<broadcast::Receiver<PushEvent>> {
        self.own().map(|inner| inner.pushes.subscribe())
    }

    /// Permanently tear the client down: stop the reconnect loop, close any
    /// live socket (dropping the connection driver drops the gloo `WebSocket`,
    /// which closes it), and make pending/future `request`s fail fast with
    /// `NotConnected`. One-way — a closed client never reconnects; build a new
    /// client to talk again.
    ///
    /// This is the singleton-swap seam the web SPA was missing: when the
    /// target nest URL or the identity changes, the superseded client used to
    /// be "left to idle" — its reconnect loop kept re-minting bearers and
    /// dialing the stale nest forever (measured in the mid-claim crash-recovery
    /// journey: an origin-fallback client outlived the corrected nest URL
    /// and dialed the wrong nest in a permanent auth-fail loop). Idempotent.
    ///
    /// On a port-built client this is a no-op: the socket is the owner's, and
    /// a chunk that borrowed it must never close it under the pages built over
    /// the owner's singleton.
    pub fn close(&self) {
        let Some(inner) = self.own() else {
            return;
        };
        inner.closed.set(true);
        if let Some(tx) = inner.close_tx.borrow_mut().take() {
            let _ = tx.send(());
        }
    }

    /// Pace this client's reconnect retries — `Some((initial, max))` — or
    /// restore the production bounds with `None`; the e2e seam behind
    /// `fauna_e2e_agent::RECONNECT_BACKOFF`, the twin of native
    /// `NestClient::set_reconnect_backoff_for_test`. Takes effect at the loop's
    /// next backed-off nap; no reconnect needed.
    /// A port-built client has no loop to pace — the owner's is paced through
    /// the owner (the SPA singleton), which the e2e seam already reaches.
    #[cfg(feature = "test-helpers")]
    pub fn set_reconnect_backoff_for_test(&self, bounds: Option<(Duration, Duration)>) {
        if let Some(inner) = self.own() {
            inner.backoff_override.set(bounds);
        }
    }

    /// Current connection state (for the SPA's status surface). A port-built
    /// client reports the owner's socket's state — the one state the app's
    /// `connection` indicator shows.
    pub fn connection_state(&self) -> ConnectionState {
        match &self.transport {
            Transport::Own(inner) => *inner.connection_state.borrow(),
            Transport::Shared(port) => port.connection_state(),
        }
    }

    /// Register the JS callback fired on every reconnect (a `Connected` after
    /// the first connect — never the initial one), so the SPA can re-hydrate
    /// surfaces with no poll backstop (the feed). The wasm twin of native
    /// `NestClient::subscribe_reconnects` (`transport.md` § Push events).
    /// Replaces any previously-registered callback.
    pub fn set_on_reconnected(&self, cb: js_sys::Function) {
        match self.own() {
            Some(inner) => *inner.on_reconnected.borrow_mut() = Some(cb),
            None => self.warn_owner_only("set_on_reconnected"),
        }
    }

    /// Register the JS callback fired on every connection-state *transition*
    /// (Connecting/Connected/Disconnected, with the new state as
    /// `"connecting"|"connected"|"disconnected"`), so the SPA's global
    /// `connection-status` indicator updates live — distinct from
    /// `set_on_reconnected`, which fires only on a reconnect (a `Connected`
    /// after the first connect). The wasm twin of a native app observing
    /// `NestClient::connection_state()`. Replaces any previously-registered
    /// callback.
    pub fn set_on_connection_state_changed(&self, cb: js_sys::Function) {
        match self.own() {
            Some(inner) => *inner.on_connection_state_changed.borrow_mut() = Some(cb),
            None => self.warn_owner_only("set_on_connection_state_changed"),
        }
    }

    /// Register the JS callback fired for every server push, as
    /// `(kind: string, payload: object)`. The wasm twin of native
    /// `NestClient::subscribe_pushes` (`transport.md` § Push events): the SPA
    /// sees the same `fauna_protocol::PushEvent` set, under the same kind
    /// strings (`"fauna.calendar.changed"`, `"fauna.notification"`, …), that a native
    /// app matches on. Registering survives reconnects — the forwarder is
    /// re-subscribed to each new dispatcher by the reconnect loop. Replaces any
    /// previously-registered callback.
    pub fn set_on_push_event(&self, cb: js_sys::Function) {
        match self.own() {
            Some(inner) => *inner.on_push_event.borrow_mut() = Some(cb),
            None => self.warn_owner_only("set_on_push_event"),
        }
    }

    /// The nest address this client is connected on (the SPA's single origin).
    /// The *current* value — the reconnect loop SRV-swaps it on an admin
    /// serving-port change; a port-built client reads the owner's. Owned
    /// `String` because it lives behind a `RefCell`. Mirrors native
    /// `NestClient::nest_url`.
    pub fn nest_url(&self) -> String {
        match &self.transport {
            Transport::Own(inner) => inner.nest_url.borrow().clone(),
            Transport::Shared(port) => port.nest_url(),
        }
    }

    /// The hex actor id this client authenticates as. The both-ends pairing seam
    /// reads it to open a *second* authenticated client to a peer nest with the
    /// user's *same* identity (registered on both) — see
    /// `fauna-client-pair`'s wasm `connect_peer`.
    pub fn actor_id_hex(&self) -> &str {
        match &self.transport {
            Transport::Own(inner) => &inner.actor_id_hex,
            Transport::Shared(port) => port.actor_id_hex(),
        }
    }

    /// A current bearer token from the SPA's JS token-provider callback. Mirrors
    /// native `AuthState::ensure_auth`: `force_refresh = false` accepts the TS
    /// side's cached bearer, `true` busts it (e.g. after a 4401). Used by HTTP
    /// surfaces that ride alongside the WS-RPC connection rather than over it —
    /// the FaunaMls-rail attachment blob client (`WsConversationsRpc::blob_put`
    /// over the nest's content-addressed `/api/v1/blob`).
    pub async fn bearer(&self, force_refresh: bool) -> Result<String, WsRpcError> {
        match &self.transport {
            Transport::Own(inner) => inner.token_provider.fetch(force_refresh).await,
            Transport::Shared(port) => port.bearer(force_refresh).await,
        }
    }

    /// Wait until the reconnect loop brings the socket up, or fail fast once a
    /// connect attempt is seen to fail. The wasm twin of native
    /// `NestClient::connect`'s connect-and-wait (whose `auth.authenticate()`
    /// errors immediately on an unreachable nest): the both-ends pairing seam's
    /// `connect_peer` must hand back a *connected* peer client, so a link to an
    /// unreachable peer surfaces a connect error promptly here instead of
    /// stalling the first `this_nest` request for the full per-request deadline
    /// (~30 s, far past the user-settings error window).
    ///
    /// Returns `Ok` on [`ConnectionState::Connected`]; at once, the error a
    /// closed or stopped client answers ([`Self::supervisor_stop`] says why);
    /// `Err(NotConnected)` the moment the loop reports
    /// [`ConnectionState::Disconnected`] (a failed attempt — matching native's
    /// single-attempt connect) or `timeout_ms` elapses while still `Connecting`
    /// (the backstop for a hung, SYN-dropped connect). On the success path it
    /// returns in well under a second; the initial connect of a reachable peer
    /// goes `Connecting → Connected` with no intervening `Disconnected`, so a
    /// spurious fast-fail is unreachable.
    pub async fn wait_until_connected(&self, timeout_ms: u32) -> Result<(), WsRpcError> {
        let mut waited_ms = 0u32;
        loop {
            // Before the state: a stopped loop reports `Disconnected` too, and
            // the stop says why.
            if let Some(e) = self.no_connection_can_come() {
                return Err(e);
            }
            match self.connection_state() {
                ConnectionState::Connected => return Ok(()),
                // A seen failure, transient or settled — either way this attempt
                // did not connect, so fail fast rather than stall the caller.
                ConnectionState::Disconnected | ConnectionState::Unreachable => {
                    return Err(WsRpcError::NotConnected);
                }
                ConnectionState::Connecting => {}
            }
            if waited_ms >= timeout_ms {
                return Err(WsRpcError::NotConnected);
            }
            let step = RECONNECT_POLL_MS.min(timeout_ms - waited_ms);
            TimeoutFuture::new(step).await;
            waited_ms += step;
        }
    }

    /// Why this client's reconnect loop stopped for good, if it has: the error
    /// every request on it now fails with at once. `None` while the loop runs,
    /// and for a client [`close`](Self::close)d, which is torn down rather than
    /// stopped. Mirrors native `NestClient::supervisor_stop`. A stopped client
    /// never reconnects, so a caller retrying across the stop builds a new one.
    pub fn supervisor_stop(&self) -> Option<WsRpcError> {
        self.own()
            .and_then(|inner| inner.stopped_by.get())
            .map(SupervisorStop::error)
    }

    /// Whether the reconnect loop stopped on the nest's sign-in refusal — the
    /// one session-ending verdict this loop can reach (the nest-identity and
    /// succession verdicts reach web through `challengeVerify`'s own typed
    /// errors). The typed reading of [`Self::supervisor_stop`] the SPA routes
    /// to the launch surface, mirroring native
    /// `NestClientError::session_ending_verdict`'s `SignInRefused`.
    pub fn stopped_by_sign_in_refusal(&self) -> bool {
        self.own()
            .and_then(|inner| inner.stopped_by.get())
            .is_some_and(|stop| stop == SupervisorStop::Refused)
    }

    /// Run one request whose payload is ALREADY canonical DAG-CBOR bytes and
    /// hand back the reply's canonical bytes — the socket owner's half of the
    /// shared rpc port (`crate::shared_port`): the SPA core chunk exposes it
    /// as `WsRpcClient.requestRaw`, and a port-built chunk client's every
    /// typed request arrives here. It takes the same path a typed request on
    /// this client takes — the kind's registry deadline, the wait for a
    /// mid-reconnect dispatcher gap within it, the idempotency key the caller
    /// chose — so a chunk's request is bounded and retried exactly like the
    /// core's own. On a port-built client it forwards to the port (a port over
    /// a port is still one socket).
    pub async fn request_raw_bytes(
        &self,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: &[u8],
    ) -> Result<Vec<u8>, WsRpcError> {
        match &self.transport {
            Transport::Own(inner) => {
                let payload: fauna_protocol::Value = fauna_protocol::decode_strict(payload)
                    .map_err(|e| WsRpcError::Codec(format!("decode request bytes: {e}")))?;
                let (dispatcher, remaining_ms) = dispatcher_within_deadline(inner, kind).await?;
                let reply: fauna_protocol::Value = dispatch_encoded_keyed(
                    &dispatcher,
                    kind,
                    idempotency_key,
                    payload,
                    remaining_ms,
                )
                .await?;
                fauna_protocol::encode_canonical(&reply)
                    .map(|b| b.to_vec())
                    .map_err(|e| WsRpcError::Codec(format!("encode reply bytes: {e}")))
            }
            Transport::Shared(port) => {
                port.request_bytes(kind, idempotency_key, payload.to_vec())
                    .await
            }
        }
    }

    /// `Some` once no connection can come, the answer both waits give at once
    /// instead of polling to their deadline: a closed client is
    /// `NotConnected`, a stopped one says why. The wasm twin of the checks
    /// native `NestClient::wait_for_connected` makes before it parks. A
    /// port-built client can always ask — the owner's stop reaches it as the
    /// port's rejection, with why.
    fn no_connection_can_come(&self) -> Option<WsRpcError> {
        self.own().and_then(own_no_connection_can_come)
    }

    /// A typed request over the shared port: encode, cross as bytes, decode.
    /// The deadline, the reconnect-wait and the idempotency semantics are the
    /// owner's (`request_raw_bytes` on the other side of the port).
    async fn request_over_port<Req, Reply>(
        port: &SharedPort,
        kind: &str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, WsRpcError>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        let bytes = fauna_protocol::encode_canonical(&payload)
            .map_err(|e| WsRpcError::Codec(format!("encode request: {e}")))?;
        let reply = port
            .request_bytes(kind, idempotency_key, bytes.to_vec())
            .await?;
        fauna_protocol::decode_strict(&reply)
            .map_err(|e| WsRpcError::Codec(format!("decode reply: {e}")))
    }
}

/// [`WsRpcClient::no_connection_can_come`] for the owning transport.
fn own_no_connection_can_come(inner: &Rc<Inner>) -> Option<WsRpcError> {
    if inner.closed.get() {
        return Some(WsRpcError::NotConnected);
    }
    inner.stopped_by.get().map(SupervisorStop::error)
}

/// The shared request preamble of an owning client: bound the whole logical
/// request by the kind's deadline and wait out a mid-reconnect dispatcher gap
/// within it.
///
/// If the dispatcher is `None` the reconnect loop is mid-reconnect (a
/// transient idle-drop), so poll-wait (bounded by the deadline) for it to
/// come back rather than failing fast with a spurious `NotConnected` the
/// SPA would surface as an error. Nothing was sent on the wire yet, so
/// waiting and sending fresh is safe for every kind. Mirrors
/// `fauna_client::NestClient::request_inner`'s wait-for-reconnect (see
/// this module's duplication note). Clones the `Rc` out before any await
/// so the `RefCell` borrow is never held across one.
///
/// Returns the dispatcher and the remaining budget after any wait, so the
/// total request stays bounded by the deadline.
async fn dispatcher_within_deadline(
    inner: &Rc<Inner>,
    kind: &str,
) -> Result<(Rc<RpcDispatcher>, u32), WsRpcError> {
    let deadline = inner
        .kind_registry
        .meta(kind)
        .map(|m| m.default_deadline)
        .unwrap_or(DEFAULT_DEADLINE);
    let deadline_ms = deadline.as_millis() as u32;
    let mut waited_ms = 0u32;
    // Held from the first park until this wait ends, either way: while it is
    // held the reconnect loop dials for this request at the initial pace
    // ([`DialDemand`]), the twin of native `NestClient::wait_for_connected`.
    let mut waiting: Option<DialDemandGuard> = None;

    let dispatcher = loop {
        // A closed or stopped client's dispatcher never comes back — fail
        // fast, with why for a stop, rather than waiting out the deadline.
        if let Some(e) = own_no_connection_can_come(inner) {
            return Err(e);
        }
        let snapshot = inner.dispatcher.borrow().as_ref().map(Rc::clone);
        if let Some(d) = snapshot {
            break d;
        }
        if waited_ms >= deadline_ms {
            return Err(WsRpcError::NotConnected);
        }
        waiting.get_or_insert_with(|| DialDemandGuard::begin(inner));
        let step = RECONNECT_POLL_MS.min(deadline_ms - waited_ms);
        TimeoutFuture::new(step).await;
        waited_ms += step;
    };
    Ok((dispatcher, deadline_ms.saturating_sub(waited_ms)))
}

impl RpcRequester for WsRpcClient {
    type Error = WsRpcError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, WsRpcError>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        match &self.transport {
            Transport::Own(inner) => {
                let (dispatcher, remaining_ms) = dispatcher_within_deadline(inner, kind).await?;
                dispatch_typed(&dispatcher, kind, payload, remaining_ms).await
            }
            Transport::Shared(port) => {
                let mut idem = [0u8; 16];
                getrandom::fill(&mut idem)
                    .map_err(|e| WsRpcError::Codec(format!("idempotency_key: {e}")))?;
                Self::request_over_port(port, kind, idem, payload).await
            }
        }
    }
}

/// The wasm arm of the outbox-drain seam (`fauna_protocol::KeyedRpcRequester`):
/// the envelope carries the caller's key (the outbox intent id) instead of a
/// fresh random one, so a replayed drain re-presents the same logical request.
impl fauna_protocol::KeyedRpcRequester for WsRpcClient {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, WsRpcError>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        match &self.transport {
            Transport::Own(inner) => {
                let (dispatcher, remaining_ms) = dispatcher_within_deadline(inner, kind).await?;
                dispatch_typed_keyed(&dispatcher, kind, idempotency_key, payload, remaining_ms)
                    .await
            }
            Transport::Shared(port) => {
                Self::request_over_port(port, kind, idempotency_key, payload).await
            }
        }
    }
}

/// This crate's entry to the shared request core: mint an envelope key, then
/// [`dispatch_typed_keyed`]. Both the authenticated [`WsRpcClient`] (after its
/// reconnect-wait) and the pre-identity [`crate::AnonymousWsRpcClient`] (no
/// reconnect loop) come through here — they differ only in how they obtain the
/// `dispatcher` and `budget_ms`.
pub(crate) async fn dispatch_typed<Req, Reply>(
    dispatcher: &RpcDispatcher,
    kind: &'static str,
    payload: Req,
    budget_ms: u32,
) -> Result<Reply, WsRpcError>
where
    Req: Serialize,
    Reply: DeserializeOwned,
{
    let mut idem = [0u8; 16];
    getrandom::fill(&mut idem).map_err(|e| WsRpcError::Codec(format!("idempotency_key: {e}")))?;
    dispatch_typed_keyed(dispatcher, kind, idem, payload, budget_ms).await
}

/// [`dispatch_typed`] with a caller-supplied envelope idempotency key — the
/// keyed-requester seam's core. The random-key path above is one mint away.
///
/// The encode/dispatch/await/decode itself is
/// [`RpcDispatcher::request_typed`], shared with every other fauna transport.
/// What is wasm's own is the deadline backstop — a `gloo_timers`
/// [`TimeoutFuture`], because `tokio::time` does not exist on `wasm32` — and the
/// mapping onto [`WsRpcError`]. Dropping the reply future on timeout still sends
/// a Cancel via `RpcCall`'s `Drop`.
pub(crate) async fn dispatch_typed_keyed<Req, Reply>(
    dispatcher: &RpcDispatcher,
    kind: &str,
    idem: [u8; 16],
    payload: Req,
    budget_ms: u32,
) -> Result<Reply, WsRpcError>
where
    Req: Serialize,
    Reply: DeserializeOwned,
{
    let payload = fauna_protocol::encode_payload(&payload).map_err(typed_err)?;
    dispatch_encoded_keyed(dispatcher, kind, idem, payload, budget_ms).await
}

/// [`dispatch_typed_keyed`] over an already-encoded payload `Value` — the
/// half the shared rpc port's owner takes (`WsRpcClient::request_raw_bytes`),
/// whose payload arrived as bytes from another chunk and needs no typed
/// encode. `Reply` is still decoded here; the owner asks for a `Value` and
/// re-encodes it for the crossing.
pub(crate) async fn dispatch_encoded_keyed<Reply>(
    dispatcher: &RpcDispatcher,
    kind: &str,
    idem: [u8; 16],
    payload: fauna_protocol::Value,
    budget_ms: u32,
) -> Result<Reply, WsRpcError>
where
    Reply: DeserializeOwned,
{
    dispatcher
        .request_encoded(
            kind,
            idem,
            payload,
            Duration::from_millis(budget_ms as u64),
            TimeoutFuture::new(budget_ms),
        )
        .await
        .map_err(typed_err)
}

/// The one mapping of the shared request core's classification onto this
/// transport's error.
fn typed_err(e: fauna_protocol::TypedRequestError) -> WsRpcError {
    use fauna_protocol::TypedRequestError as T;
    match e {
        T::Codec(msg) => WsRpcError::Codec(msg),
        // No live transport to send on, and a dropped connection while
        // the request was outstanding, are one thing to the SPA.
        T::Dispatch(_) | T::Disconnected => WsRpcError::Disconnected,
        // The nest's `details` are already logged operator-side by the
        // shared path — reaching the browser console via the `tracing`
        // → console bridge `fauna-wasm/src/logs.rs` installs — and are
        // never surfaced.
        T::Rpc(e) => WsRpcError::Rpc(e),
        T::Timeout => WsRpcError::Timeout,
    }
}

#[cfg(feature = "test-helpers")]
thread_local! {
    /// Every connection-state report this tab's reconnect loops published — one
    /// counter per process, as the contract has it (a superseded client's reports
    /// still happened). `test-helpers` only.
    static CONNECTION_REPORTS: RefCell<fauna_e2e_contract::ConnectionReports> =
        RefCell::new(fauna_e2e_contract::ConnectionReports::default());
}

/// The `fauna_e2e_agent::CONNECTION_REPORTS_KEY` value, serialized.
#[cfg(feature = "test-helpers")]
pub fn connection_reports_json() -> String {
    CONNECTION_REPORTS.with_borrow(|r| r.json().to_string())
}

fn set_state(inner: &Inner, state: ConnectionState) {
    // Every report counts, repeats included — the dedup below is exactly what
    // the stickiness proof must see past (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`).
    #[cfg(feature = "test-helpers")]
    CONNECTION_REPORTS.with_borrow_mut(|r| r.observe(state.as_js_str()));
    // Update first, recording whether this is a genuine transition (the loop
    // re-asserts `Connecting` on each retry, so dedup keeps the callback from
    // firing redundantly). Drop the borrow before any JS call — the SPA's
    // callback re-enters wasm via the store update.
    let changed = {
        let mut cur = inner.connection_state.borrow_mut();
        let changed = *cur != state;
        *cur = state;
        changed
    };
    if changed {
        let cb = inner.on_connection_state_changed.borrow().clone();
        if let Some(cb) = cb {
            let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(state.as_js_str()));
        }
    }
}

/// Mark the socket `Connected` and, if this is a *reconnect* (a `Connected`
/// after the first connect), fire the SPA's re-hydrate callback. The initial
/// connect only flips `has_connected` — it doesn't fire — matching native's
/// `subscribe_reconnects`, which bumps on every `Connected` *after the first*.
/// The callback is cloned out before invocation so we never hold the `RefCell`
/// borrow across the JS call (it re-enters wasm via the surface re-fetches).
fn mark_connected(inner: &Inner) {
    set_state(inner, ConnectionState::Connected);
    if inner.has_connected.replace(true) {
        inner.reconnects.send_modify(|n| *n = n.wrapping_add(1));
        let cb = inner.on_reconnected.borrow().clone();
        if let Some(cb) = cb {
            let _ = cb.call0(&JsValue::NULL);
        }
    }
}

/// Record why the reconnect loop stopped for good, then announce
/// `Disconnected`: stored first, announced second, the order native's
/// `record_supervisor_stop` keeps, so a state callback that re-enters wasm
/// already reads the stop. Like native's, the announcement also replaces an
/// `Unreachable` that a failing run had reported.
fn record_supervisor_stop(inner: &Inner, stop: SupervisorStop) {
    inner.stopped_by.set(Some(stop));
    set_state(inner, ConnectionState::Disconnected);
}

/// Forward one connection's decoded pushes to the SPA's registered JS callback
/// as `(kind, payload)` — the wasm twin of a native consumer looping over
/// `NestClient::subscribe_pushes` (`transport.md` § Push events).
///
/// Runs for the life of one dispatcher: it ends when the connection drops and
/// the broadcast sender goes with it (`Closed`), and the reconnect loop spawns a
/// fresh one against the next dispatcher.
async fn forward_pushes(inner: Rc<Inner>, mut pushes: broadcast::Receiver<PushEvent>) {
    loop {
        match pushes.recv().await {
            Ok(event) => {
                // Rust subscribers first (none is a normal state — send's
                // error is "no receivers", not a fault).
                let _ = inner.pushes.send(event.clone());
                // Read the callback per event, not once up front: the SPA may
                // register or replace it after the connection is already up. The
                // `Ref` dies with the statement, so no borrow spans the JS call.
                let Some(cb) = inner.on_push_event.borrow().clone() else {
                    continue;
                };
                // `PushEvent` is untagged (`push_events.rs`), so this is the
                // variant's payload object bare — no per-kind marshalling here to
                // drift as variants are added.
                let payload = match serde_wasm_bindgen::to_value(&event) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(kind = event.kind(), error = %e, "push payload → JS failed");
                        continue;
                    }
                };
                if let Err(e) = cb.call2(&JsValue::NULL, &JsValue::from_str(event.kind()), &payload)
                {
                    tracing::warn!(kind = event.kind(), error = ?e, "push callback threw");
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                // The SPA fell behind the dispatcher's 256-slot broadcast. A push
                // is a hint, never the only path to a value (every surface has a
                // fetch), so drop the gap and keep going rather than tear the
                // connection down.
                tracing::warn!(skipped, "push receiver lagged; dropped pushes");
            }
        }
    }
}

/// Park on the jittered backoff sleep unless `close()` fires first. Returns
/// `true` when the client was closed — the loop must exit instead of retrying.
async fn backoff_or_closed(backoff: &Backoff, close_rx: &mut oneshot::Receiver<()>) -> bool {
    let sleep = TimeoutFuture::new(jittered_backoff_ms(backoff));
    futures_util::pin_mut!(sleep);
    matches!(select(sleep, close_rx).await, Either::Right(_))
}

/// How a [`nap_until_due_or_demand`] ended.
enum NapEnd {
    /// The curve's next dial is due.
    Due,
    /// Cut short for a waiting request: the dial that follows is an extra one.
    Demand,
    /// `close()` fired: the loop must exit.
    Closed,
}

/// The nap after a refused attempt: until the curve's next dial is due at
/// `due_ms` (`Date.now()` milliseconds), or — while a request waits on the
/// connection — only a jittered draw within `demand_ceiling`
/// (`fauna_protocol::reconnect::demand_nap`); a request that starts waiting
/// mid-nap cuts it short. The wasm twin of native
/// `fauna_ws_substrate::supervisor::nap_until_due_or_demand`.
async fn nap_until_due_or_demand(
    inner: &Inner,
    due_ms: f64,
    demand_ceiling: Duration,
    close_rx: &mut oneshot::Receiver<()>,
) -> NapEnd {
    let ms = |d: Duration| d.as_millis().min(u128::from(u32::MAX)) as u32;
    loop {
        let remaining = Duration::from_millis((due_ms - js_sys::Date::now()).max(0.0) as u64);
        if inner.demand.is_waiting() {
            let (nap, end) = match demand_nap(remaining, demand_ceiling, jitter_unit()) {
                Some(short) => (short, NapEnd::Demand),
                None => (remaining, NapEnd::Due),
            };
            let sleep = TimeoutFuture::new(ms(nap));
            futures_util::pin_mut!(sleep);
            return match select(sleep, &mut *close_rx).await {
                Either::Left(_) => end,
                Either::Right(_) => NapEnd::Closed,
            };
        }
        let sleep = TimeoutFuture::new(ms(remaining));
        let arrived = inner.demand.arrived.notified();
        futures_util::pin_mut!(sleep);
        futures_util::pin_mut!(arrived);
        match select(select(sleep, arrived), &mut *close_rx).await {
            Either::Left((Either::Left(_), _)) => return NapEnd::Due,
            // A waiter arrived: re-read the remaining time and take its nap.
            Either::Left((Either::Right(_), _)) => {}
            Either::Right(_) => return NapEnd::Closed,
        }
    }
}

/// The reconnect loop. Drives the dispatcher driver inline (`driver.await`) on
/// a single-threaded executor — concurrent `request` futures are polled in
/// their own tasks. Mirrors the native supervisor's close-code reaction table
/// (`transport.md` § Connection lifecycle). Exits for good when
/// [`WsRpcClient::close`] fires `close_rx` (or sets `closed` while the loop is
/// between parks).
async fn run_reconnect_loop(inner: Rc<Inner>, mut close_rx: oneshot::Receiver<()>) {
    let mut backoff = Backoff::new(INITIAL_BACKOFF, MAX_BACKOFF);
    // The bounds `backoff` was built with, so the e2e pace override rebuilds it
    // only when the bounds actually change (native's `adopt_backoff_override!`);
    // its initial is also the ceiling a waiting request's dials are paced within
    // ([`DialDemand`]), so a test pace reaches them too.
    #[cfg_attr(not(any(test, feature = "test-helpers")), allow(unused_mut))]
    let mut backoff_bounds = (INITIAL_BACKOFF, MAX_BACKOFF);
    // Adopt the e2e pace (`WsRpcClient::set_reconnect_backoff_for_test`) at each
    // backed-off nap — all four alike, so the seam never depends on which way
    // the nest happened to fail; `None` restores the production bounds. A no-op
    // in a production bundle.
    macro_rules! adopt_backoff_override {
        () => {
            #[cfg(any(test, feature = "test-helpers"))]
            {
                let wanted = inner
                    .backoff_override
                    .get()
                    .unwrap_or((INITIAL_BACKOFF, MAX_BACKOFF));
                if wanted != backoff_bounds {
                    backoff_bounds = wanted;
                    backoff = Backoff::new(wanted.0, wanted.1);
                }
            }
        };
    }
    let mut force_refresh = false;
    // Consecutive attempts that failed to reach `Connected`, cleared by every
    // proven connection. Past the shared threshold the reported transient states
    // collapse to `Unreachable` and stay there, so a browser that can never open
    // the socket says so instead of showing "Connecting…" forever. Native twin:
    // `fauna_ws_substrate::supervisor::run_supervisor`.
    let mut consecutive_failures: u32 = 0;
    macro_rules! report {
        ($transient:expr) => {
            set_state(
                &inner,
                if consecutive_failures >= CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES {
                    ConnectionState::Unreachable
                } else {
                    $transient
                },
            );
        };
    }
    // When the curve's next dial is due (`Date.now()` ms), while a nap on it has
    // been cut short for a waiting request ([`DialDemand`]); and whether the
    // attempt now being made is such an extra one. A refused extra attempt
    // leaves the curve where it was: no growth, no count toward `Unreachable`,
    // same due time. Native twin: `run_supervisor`'s `curve_due`/`demand_dial`.
    let mut curve_due: Option<f64> = None;
    let mut demand_dial = false;
    let mut extra_dial: bool;
    // The nap after a REFUSED attempt — a failed token fetch, a failed handle
    // creation, or a socket that died inside the establish probe — the one
    // kind of failure a waiting request's dials are for (`transport-connection.md`
    // § Connection lifecycle); a dropped connection and a 4401 keep the curve.
    // The answered-refusal exemption reaches only what this loop can see: a
    // 4401 keeps the curve and a `not_registered` mint refusal stops the loop
    // before it gets here, but the browser hides an upgrade's 401/429, so such
    // a dial reads as refused.
    macro_rules! refused_nap {
        ($what:expr) => {{
            let due = match curve_due {
                Some(due) if extra_dial => {
                    tracing::debug!("{} for a waiting request", $what);
                    report!(ConnectionState::Disconnected);
                    due
                }
                _ => {
                    adopt_backoff_override!();
                    let nap = jittered_backoff_ms(&backoff);
                    tracing::warn!(
                        "{}; backing off {nap} ms (ceiling {:?})",
                        $what,
                        backoff.ceiling()
                    );
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    report!(ConnectionState::Disconnected);
                    backoff.grow();
                    js_sys::Date::now() + f64::from(nap)
                }
            };
            match nap_until_due_or_demand(&inner, due, backoff_bounds.0, &mut close_rx).await {
                NapEnd::Closed => return,
                NapEnd::Demand => {
                    demand_dial = true;
                    curve_due = Some(due);
                }
                NapEnd::Due => curve_due = None,
            }
            continue;
        }};
    }

    loop {
        if inner.closed.get() {
            set_state(&inner, ConnectionState::Disconnected);
            return;
        }
        report!(ConnectionState::Connecting);
        extra_dial = std::mem::take(&mut demand_dial);

        let token = match inner.token_provider.fetch(force_refresh).await {
            Ok(t) => t,
            // The nest refused the re-mint outright: no bearer will ever come
            // for this identity here, so stop for good — with why — instead of
            // backing off into a retry loop indistinguishable from a blip.
            Err(WsRpcError::Rpc(e)) if e.is_not_registered() => {
                tracing::warn!("token fetch refused ({}); stopping", e.code);
                record_supervisor_stop(&inner, SupervisorStop::Refused);
                return;
            }
            Err(e) => refused_nap!(format!("token fetch failed: {e}")),
        };
        // `close()` may have fired while the token promise was in flight (the
        // one park above that a oneshot can't interrupt — it's a JS promise);
        // don't dial the nest with a connection nobody will ever use.
        if inner.closed.get() {
            set_state(&inner, ConnectionState::Disconnected);
            return;
        }

        let nest_url = inner.nest_url.borrow().clone();
        let adapter = match GlooAdapter::connect(&nest_url, &inner.actor_id_hex, &token) {
            Ok(a) => a,
            Err(e) => {
                // SRV self-heal: an admin may have changed the nest's client-facing
                // serving port, leaving the persisted URL on a dead port. Re-resolve
                // `_fauna._tcp` via DoH and, if it now points elsewhere, retry
                // immediately on the new port (nest/common.md § Serving ports,
                // path 2 — the wasm twin of native `ClientChannel::recover_endpoint`).
                if try_srv_recover(&inner).await {
                    tracing::info!(
                        "ws connect failed: {e}; endpoint re-resolved via SRV (serving-port \
                         change); retrying immediately"
                    );
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    report!(ConnectionState::Disconnected);
                    backoff.reset();
                    continue;
                }
                refused_nap!(format!("ws connect failed: {e}"));
            }
        };

        // NB: the backoff reset and the `mark_connected` announcement are NOT
        // done here — they moved past the establish probe below. `force_refresh`
        // is deliberately NOT reset here either: the initial connect uses the
        // cached bearer (`force_refresh` starts false); once any established
        // connection drops, the Retry/AuthExpired arms below set it true and it
        // stays true, so every subsequent reconnect re-mints. That is required
        // because the browser can't see a WS-upgrade 401 (see the Retry arm) — a
        // wiped-token redeploy must re-mint to recover, and a healthy reconnect
        // just pays one extra (cheap) handshake.

        let signal_cell = adapter.signal_cell();
        let (dispatcher, driver) = RpcDispatcher::new(adapter);
        // Re-attached on every reconnect, because the dispatcher is rebuilt per
        // connection: the wire `replay_forbidden` hint comes off this registry,
        // so a reconnect that skipped it would silently stop sending the hint
        // (`transport.md` § Idempotency and reconnect-with-resume).
        let _ = dispatcher.set_kind_registry(inner.kind_registry.clone());
        let dispatcher = Rc::new(dispatcher);
        *inner.dispatcher.borrow_mut() = Some(Rc::clone(&dispatcher));

        // Re-subscribe the push forwarder to *this* connection's dispatcher. The
        // dispatcher owns the broadcast (it decodes `Frame::Push` into a typed
        // `PushEvent`) and is rebuilt on every reconnect, so the subscription is
        // per-connection while the JS callback registered on `Inner` outlives it.
        // The task ends when the broadcast sender drops with the dispatcher below.
        spawn_local(forward_pushes(
            Rc::clone(&inner),
            dispatcher.push_subscriber(),
        ));

        // Prove the connection is really established before resetting the backoff
        // or announcing `Connected`. `GlooAdapter::connect` returning `Ok` is only
        // synchronous browser-`WebSocket` handle creation — a down / refusing /
        // unreachable nest still hands back a live handle whose failure surfaces
        // asynchronously on the stream — so, unlike native's real `connect().await`,
        // that `Ok` proves nothing. Race the dispatcher driver against a short
        // probe timer (the shared `probe_established`): if the driver outlives the
        // probe, the socket carried a live connection for the whole window and we
        // treat it as established; if it dies first, the handle never came up.
        // This gates BOTH bugs the wasm loop used to have — resetting the backoff
        // (making `MAX_BACKOFF` reachable at last) and firing the SPA re-hydrate
        // callback — on a real connection instead of on every dial.
        futures_util::pin_mut!(driver);
        let verdict = probe_established(
            driver.as_mut(),
            TimeoutFuture::new(ESTABLISH_PROBE_MS),
            Pin::new(&mut close_rx),
        )
        .await;

        // Whether the handle never came up: a refused attempt, as opposed to a
        // proven connection that later dropped.
        let mut died = false;
        let was_closed = match verdict {
            Establish::Closed => true,
            // Handle never came up (or died within the probe window). The driver
            // already finished, so the stream's close signal is captured; fall
            // through to the signal read + backoff GROWTH below with no reset and
            // no `mark_connected`.
            Establish::Died => {
                // The handle never carried a live connection — a failed attempt,
                // counting toward `Unreachable` exactly like a refused dial. This
                // is the arm a firewalled/untrusted nest takes on every retry:
                // `GlooAdapter::connect` always returns `Ok`, so the browser's
                // silent TLS/upgrade rejection can only be observed here — and
                // so this, not the `Err` arm above, is where a waiting request's
                // extra dials are refused. Counted below, once the close signal
                // says which nap follows.
                died = true;
                false
            }
            // Proven up: reset the backoff and announce `Connected` (firing the
            // SPA re-hydrate on a reconnect), then keep driving the SAME
            // connection until its stream ends (close/error/network) or `close()`
            // fires — dropping the driver, and with it the adapter's gloo
            // `WebSocket`, closes the live socket.
            Establish::Established => {
                curve_due = None;
                backoff.reset();
                // A proven connection clears the run, so the next outage starts
                // counting from zero.
                consecutive_failures = 0;
                mark_connected(&inner);
                matches!(select(driver, &mut close_rx).await, Either::Right(_))
            }
        };

        inner.dispatcher.borrow_mut().take();
        drop(dispatcher);
        let signal = signal_cell
            .borrow_mut()
            .take()
            .unwrap_or(ReconnectSignal::Retry);
        // A handle that never came up is a failed attempt, counting toward
        // `Unreachable` like a refused dial — a plain refusal (`Retry`) is
        // counted by `refused_nap!` below, which leaves a waiting request's
        // extra dial uncounted.
        // That nap reports the attempt itself, once counted, so the threshold
        // attempt goes straight to `Unreachable`.
        let refused = died && matches!(signal, ReconnectSignal::Retry);
        if died && !refused {
            consecutive_failures = consecutive_failures.saturating_add(1);
        }
        if !refused {
            report!(ConnectionState::Disconnected);
        }
        if was_closed {
            tracing::info!("ws client closed; reconnect loop exiting");
            return;
        }

        let stop = match signal {
            ReconnectSignal::CleanDisconnect => {
                tracing::info!("ws clean disconnect; reconnect loop exiting");
                SupervisorStop::Clean
            }
            ReconnectSignal::SubprotocolMismatch => {
                tracing::error!("ws subprotocol mismatch (4426); reconnect loop exiting");
                SupervisorStop::SubprotocolMismatch
            }
            ReconnectSignal::AuthExpired => {
                // Backoff floor, same curve as Retry (native twin:
                // `fauna-ws-substrate`'s supervisor). Zero-delay reconnect
                // here made spin-safety entirely load-bearing on the mint
                // gate refusing the revoked actor — and on wasm the loop
                // re-mints *before* connecting, so a 4401 producer the gate
                // does not refuse would hammer both the mint endpoint and
                // the WS upgrade with no delay. Costs a legitimate
                // token-expiry reconnect one jittered initial backoff
                // (~1 s); `backoff` resets on the next *established* connect
                // (the probe below), not on mere handle creation.
                force_refresh = true;
                curve_due = None;
                tracing::info!(
                    "ws auth expired (4401); re-minting bearer + backing off (ceiling {:?})",
                    backoff.ceiling()
                );
                adopt_backoff_override!();
                if backoff_or_closed(&backoff, &mut close_rx).await {
                    return;
                }
                backoff.grow();
                continue;
            }
            ReconnectSignal::Retry => {
                // Re-mint the bearer before retrying. The browser `WebSocket` API
                // hides the WS-upgrade HTTP status, so a bearer the nest no longer
                // accepts — e.g. its in-memory `token_store` was wiped by a
                // redeploy — surfaces as an ordinary 1001/1006 close → Retry, NOT a
                // 4401 `AuthExpired`. The native supervisor distinguishes the two
                // (`SupervisedChannel::connect_error_is_auth_rejection` sees the HTTP
                // 401 and re-mints once); wasm cannot see it, so once any
                // post-initial connection drops it re-mints on **every** subsequent
                // reconnect — the divergence-minimal twin of native's re-mint. Only
                // the very first connect uses the cached bearer: `force_refresh` is
                // set here and deliberately never cleared (see the establish block
                // above; `transport.md` § Connection lifecycle). So a healthy
                // reconnect pays one extra (cheap, persisted-key) handshake, while a
                // wiped-token flip actually recovers instead of looping forever on
                // the stale bearer. (The wasm keepalive exception is keepalive-only —
                // reconnect recovery must reach parity.)
                force_refresh = true;
                // A handle that never came up was a refused dial, which a
                // waiting request may hurry; a proven connection that dropped
                // keeps the curve.
                if refused {
                    refused_nap!("ws connect refused");
                }
                tracing::info!(
                    "ws disconnect; re-minting bearer + backing off (ceiling {:?})",
                    backoff.ceiling()
                );
                adopt_backoff_override!();
                if backoff_or_closed(&backoff, &mut close_rx).await {
                    return;
                }
                backoff.grow();
                continue;
            }
        };
        // The one way out of the match that does not `continue`, so the loop
        // cannot stop for good without telling every request why.
        record_supervisor_stop(&inner, stop);
        return;
    }
}

/// SRV self-heal for the wasm reconnect loop: re-resolve `_fauna._tcp.<host>`
/// via **DoH** (`fauna_provisioning::probe::fauna_srv_port`, the cross-platform
/// twin of native hickory) and, if it advertises a different client-facing port
/// than the failed `nest_url`, swap the shared cell to it so the next connect +
/// bearer mint target the new port. Returns `true` if the URL changed; a no-op
/// (`false`) for a local target (loopback / IP / `.local`, no public SRV zone),
/// an absent / errored lookup, or an unchanged port. The pure host-gate +
/// URL-rewrite decision is shared with native
/// (`fauna_core::resolve::{srv_recovery_host, srv_recovered_url}`).
///
/// **Web single-origin caveat:** the SPA is served from the nest's own origin,
/// so this heals the *running* page's live WebSocket onto a moved port (a
/// browser WS is not origin-locked). A full page *reload* still fetches the SPA
/// from its origin — fine on the common domain box, where the SNI router fronts
/// `:443` and `serving_port` is inert anyway, and a non-concern for the running
/// session this path serves.
async fn try_srv_recover(inner: &Rc<Inner>) -> bool {
    let current = inner.nest_url.borrow().clone();
    let Some(host) = fauna_core::resolve::srv_recovery_host(&current) else {
        return false;
    };
    let client = reqwest::Client::new();
    let srv_port = fauna_provisioning::probe::fauna_srv_port(&client, &host).await;
    match fauna_core::resolve::srv_recovered_url(&current, srv_port) {
        Some(new_url) => {
            *inner.nest_url.borrow_mut() = new_url;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use wasm_bindgen::prelude::wasm_bindgen;
    use wasm_bindgen_test::wasm_bindgen_test;

    // `run_in_browser` is configured once for this crate's `--lib` test binary,
    // by `adapter.rs`'s tests: one `wasm_bindgen_test_configure!` per binary.

    // A fake global `WebSocket` constructor for the two tests below that drive
    // the real reconnect loop (`connect_and_stop_on_a_close_the_loop_records`).
    // `GlooAdapter::connect` calls `gloo-net`'s `WebSocket::open_with_protocols`,
    // which — like every browser-`WebSocket`-backed crate — can only ever reach
    // the socket through the one JS entry point the platform provides: `new
    // WebSocket(url, protocols)`, resolved off `globalThis` on every call.
    // Swapping that global therefore intercepts the dial regardless of whether
    // `gloo-net` wires its listeners with `.onclose =` or `addEventListener`
    // (this fake supports both, and fires a real `CloseEvent` through
    // `EventTarget::dispatchEvent` rather than a bare object literal, so any
    // `web_sys` binding that expects a genuine `CloseEvent` instance still
    // works) — no `gloo-net` source read, only the WHATWG-specified constructor
    // contract every WS library is bound by.
    #[wasm_bindgen(inline_js = r#"
        let __real_ws = null;

        export function install_fake_ws(close_code) {
            if (__real_ws === null) {
                __real_ws = globalThis.WebSocket;
            }
            class FakeWebSocket extends EventTarget {
                constructor(url, _protocols) {
                    super();
                    this.url = url;
                    this.protocol = "";
                    this.binaryType = "blob";
                    this.readyState = FakeWebSocket.CONNECTING;
                    this._onclose = null;
                    this._onopen = null;
                    this._onmessage = null;
                    this._onerror = null;
                    // A real browser WebSocket never fails inside its own
                    // constructor turn; queue the close for the next microtask
                    // so this stands in for the async failure it simulates (a
                    // nest that answers the upgrade with a close frame, or an
                    // HTTP refusal the browser reports as an ordinary close).
                    Promise.resolve().then(() => {
                        this.readyState = FakeWebSocket.CLOSED;
                        this.dispatchEvent(new CloseEvent("close", {
                            code: close_code,
                            reason: "",
                            wasClean: close_code === 1000,
                        }));
                    });
                }
                get onclose() { return this._onclose; }
                set onclose(fn) {
                    if (this._onclose) this.removeEventListener("close", this._onclose);
                    this._onclose = fn;
                    if (fn) this.addEventListener("close", fn);
                }
                get onopen() { return this._onopen; }
                set onopen(fn) {
                    if (this._onopen) this.removeEventListener("open", this._onopen);
                    this._onopen = fn;
                    if (fn) this.addEventListener("open", fn);
                }
                get onmessage() { return this._onmessage; }
                set onmessage(fn) {
                    if (this._onmessage) this.removeEventListener("message", this._onmessage);
                    this._onmessage = fn;
                    if (fn) this.addEventListener("message", fn);
                }
                get onerror() { return this._onerror; }
                set onerror(fn) {
                    if (this._onerror) this.removeEventListener("error", this._onerror);
                    this._onerror = fn;
                    if (fn) this.addEventListener("error", fn);
                }
                send(_data) {}
                close(_code, _reason) {}
            }
            FakeWebSocket.CONNECTING = 0;
            FakeWebSocket.OPEN = 1;
            FakeWebSocket.CLOSING = 2;
            FakeWebSocket.CLOSED = 3;
            globalThis.WebSocket = FakeWebSocket;
        }

        // The gap fake: a nest per port that refuses every dial (a 1006 close
        // before the establish probe elapses, the browser's face of a down
        // nest) until `set_gap_up` brings it back, after which a dial opens and
        // stays open. Every dial's `Date.now()` is recorded per port, so each
        // test reads only its own client's dials — a loop another test left
        // running dials a port of its own.
        const __gap_up = new Set();
        const __gap_dials = new Map();

        export function install_gap_ws() {
            if (__real_ws === null) {
                __real_ws = globalThis.WebSocket;
            }
            class GapWebSocket extends EventTarget {
                constructor(url, _protocols) {
                    super();
                    this.url = url;
                    this.protocol = "";
                    this.binaryType = "blob";
                    this.readyState = GapWebSocket.CONNECTING;
                    const port = new URL(url).port;
                    if (!__gap_dials.has(port)) __gap_dials.set(port, []);
                    __gap_dials.get(port).push(Date.now());
                    const up = __gap_up.has(port);
                    for (const ev of ["open", "close", "message", "error"]) {
                        let handler = null;
                        Object.defineProperty(this, "on" + ev, {
                            get: () => handler,
                            set: (fn) => {
                                if (handler) this.removeEventListener(ev, handler);
                                handler = fn;
                                if (fn) this.addEventListener(ev, fn);
                            },
                        });
                    }
                    Promise.resolve().then(() => {
                        if (up) {
                            this.readyState = GapWebSocket.OPEN;
                            this.dispatchEvent(new Event("open"));
                        } else {
                            this.readyState = GapWebSocket.CLOSED;
                            this.dispatchEvent(new CloseEvent("close", {
                                code: 1006,
                                reason: "",
                                wasClean: false,
                            }));
                        }
                    });
                }
                send(_data) {}
                close(_code, _reason) {
                    this.readyState = GapWebSocket.CLOSED;
                }
            }
            GapWebSocket.CONNECTING = 0;
            GapWebSocket.OPEN = 1;
            GapWebSocket.CLOSING = 2;
            GapWebSocket.CLOSED = 3;
            globalThis.WebSocket = GapWebSocket;
        }

        export function set_gap_up(port) {
            __gap_up.add(String(port));
        }

        export function gap_dial_times(port) {
            return new Float64Array(__gap_dials.get(String(port)) ?? []);
        }

        export function restore_real_ws() {
            if (__real_ws !== null) {
                globalThis.WebSocket = __real_ws;
                __real_ws = null;
            }
        }
    "#)]
    extern "C" {
        fn install_fake_ws(close_code: u16);
        fn install_gap_ws();
        fn set_gap_up(port: u16);
        fn gap_dial_times(port: u16) -> Vec<f64>;
        fn restore_real_ws();
    }

    thread_local! {
        /// A fixed `[0, 1)` draw for the reconnect loop's jitter
        /// ([`super::jitter_unit`]); `None` is `Math::random()`.
        pub(super) static FIXED_JITTER: Cell<Option<f64>> = const { Cell::new(None) };
    }

    /// A client against the gap fake on `port`, its reconnect curve paced at
    /// `initial` (the e2e pace override, compiled into this crate's tests) and
    /// its jitter fixed at the full ceiling, so every nap — the curve's and a
    /// waiter's — is exact and a test counts dials against a known curve.
    fn gap_client(port: u16, initial: Duration) -> WsRpcClient {
        FIXED_JITTER.set(Some(1.0));
        let client = WsRpcClient::connect(
            format!("http://127.0.0.1:{port}"),
            "00".repeat(32),
            js_sys::Function::new_no_args("return ''"),
        );
        // Set before the loop's first poll: `connect` only queues it.
        inner_of(&client)
            .backoff_override
            .set(Some((initial, Duration::from_secs(60))));
        client
    }

    /// Tear a gap test down whatever it asserted, so its fake and fixed
    /// jitter never leak into the next test.
    fn end_gap(client: &WsRpcClient) {
        client.close();
        restore_real_ws();
        FIXED_JITTER.set(None);
    }

    fn dials(port: u16) -> usize {
        gap_dial_times(port).len()
    }

    /// Poll (10 ms) until `client` reports `Connected` or `within_ms` passes;
    /// the milliseconds it took, or `None`.
    async fn connected_within(client: &WsRpcClient, within_ms: f64) -> Option<f64> {
        let start = js_sys::Date::now();
        while js_sys::Date::now() - start <= within_ms {
            if client.connection_state() == ConnectionState::Connected {
                return Some(js_sys::Date::now() - start);
            }
            TimeoutFuture::new(10).await;
        }
        None
    }

    /// Poll for the loop's recorded stop, bounded well past the 1 s establish
    /// probe (`ESTABLISH_PROBE_MS`) so a broken fake or a regression shows up
    /// as a failed assertion, not a stuck test. Latency-independent (e2e
    /// convention 14): the bound is a hang backstop, not the pass condition —
    /// a working fake converges within a couple of microtask turns, since the
    /// close fires on the very first `Promise.resolve().then()`.
    async fn wait_for_stop(client: &WsRpcClient) -> Option<WsRpcError> {
        for _ in 0..300 {
            if let Some(err) = client.supervisor_stop() {
                return Some(err);
            }
            TimeoutFuture::new(10).await;
        }
        None
    }

    /// A client whose reconnect loop never runs, so its dispatcher slot stays
    /// empty: the state a request parks on while a reconnect is under way. Its
    /// token provider is never called.
    fn loopless_client() -> WsRpcClient {
        WsRpcClient {
            transport: Transport::Own(Rc::new(Inner::new(
                "http://127.0.0.1:9".into(),
                "00".repeat(32),
                js_sys::Function::new_no_args("return ''"),
            ))),
        }
    }

    /// The owning transport of a test client (every client built here owns).
    fn inner_of(client: &WsRpcClient) -> &Rc<Inner> {
        client.own().expect("test clients own their transport")
    }

    async fn echo(client: &WsRpcClient) -> Result<fauna_protocol::Value, WsRpcError> {
        client.request("fauna.protocol.echo", ()).await
    }

    /// **A request on a stopped client fails at once, with why**, for both
    /// endings the loop records, through the request path and the connect
    /// wait. Until 2026-09-15 the loop returned on a 1000 or 4426 close without
    /// recording anything, so a later request polled out its whole deadline and
    /// failed `NotConnected`, the answer a passing gap gives. No current nest
    /// sends either close (see [`SupervisorStop`]); this pins the record's
    /// *consumer* by planting the stop on a [`loopless_client`] by hand — the
    /// loop's own close arm that *produces* the record is pinned separately,
    /// below, by `the_loops_own_close_arm_records_a_clean_stop` and
    /// `the_loops_own_close_arm_records_a_subprotocol_mismatch_stop`.
    /// Latency-independent (convention 14): the answer must be ready on the
    /// FIRST poll, before any timer could fire.
    #[wasm_bindgen_test]
    fn a_request_on_a_stopped_client_fails_at_once_with_why() {
        let skewed = loopless_client();
        record_supervisor_stop(inner_of(&skewed), SupervisorStop::SubprotocolMismatch);
        let err = echo(&skewed)
            .now_or_never()
            .expect("a request on a stopped client parked instead of failing")
            .expect_err("no nest ever serves this client");
        assert!(
            matches!(err, WsRpcError::SubprotocolMismatch),
            "got {err:?}"
        );
        assert_eq!(
            err.to_string(),
            fauna_i18n::strings::errors::SUBPROTOCOL_MISMATCH
        );
        let waited = skewed
            .wait_until_connected(60_000)
            .now_or_never()
            .expect("the connect wait on a stopped client parked instead of failing");
        assert!(
            matches!(waited, Err(WsRpcError::SubprotocolMismatch)),
            "got {waited:?}"
        );
        assert!(
            matches!(
                skewed.supervisor_stop(),
                Some(WsRpcError::SubprotocolMismatch)
            ),
            "the stop must stay readable for the SPA's connect wait"
        );

        let cleanly_closed = loopless_client();
        record_supervisor_stop(inner_of(&cleanly_closed), SupervisorStop::Clean);
        let err = echo(&cleanly_closed)
            .now_or_never()
            .expect("a request on a stopped client parked instead of failing")
            .expect_err("no nest ever serves this client");
        assert!(matches!(err, WsRpcError::NotConnected), "got {err:?}");
    }

    /// **A request already parked for a reconnect wakes to the stop.** It is
    /// issued while nothing has stopped, so it parks on the empty slot; the
    /// stop recorded after it must end it with why. A request that never read
    /// the stop would run to the kind's deadline and fail `NotConnected`, so
    /// the assertion is on the answer, not on how long it took.
    #[wasm_bindgen_test]
    async fn a_request_parked_for_a_reconnect_fails_with_the_stop_recorded_after_it() {
        let client = loopless_client();
        let mut parked = Box::pin(echo(&client));
        assert!(
            (&mut parked).now_or_never().is_none(),
            "nothing has stopped yet, so the request must park"
        );
        record_supervisor_stop(inner_of(&client), SupervisorStop::SubprotocolMismatch);
        let err = parked.await.expect_err("no nest ever serves this client");
        assert!(
            matches!(err, WsRpcError::SubprotocolMismatch),
            "got {err:?}"
        );
    }

    /// **A closed client's request fails at once**: the one fast-fail this
    /// client had before the stop record, and no test pinned it until now.
    /// Closing tears a client down rather than stopping it, as native's
    /// `disconnect()` does, so the request answers `NotConnected` and no stop
    /// is reported.
    #[wasm_bindgen_test]
    fn a_request_on_a_closed_client_fails_at_once() {
        let client = loopless_client();
        client.close();
        let err = echo(&client)
            .now_or_never()
            .expect("a request on a closed client parked instead of failing")
            .expect_err("a closed client serves nothing");
        assert!(matches!(err, WsRpcError::NotConnected), "got {err:?}");
        assert!(client.supervisor_stop().is_none(), "closing is not a stop");
    }

    /// **The loop's own close arm reaches the stop record.** Every test above
    /// pins the record's *consumers* by planting a stop with
    /// `record_supervisor_stop` called by hand on a [`loopless_client`], whose
    /// reconnect loop never runs — so nothing exercised the loop's own
    /// `let stop = match signal` (`:864-923`) or the one call that records it
    /// (`:926`) reached from inside the loop. This test drives the real
    /// [`WsRpcClient::connect`], letting a fake global `WebSocket` (above)
    /// close with 1000 before the establish probe elapses, so the driver ends
    /// in [`Establish::Died`] and the loop itself must map the close to
    /// [`SupervisorStop::Clean`] and record it. Before this test, deleting the
    /// `:926` call or swapping the `Clean`/`SubprotocolMismatch` mapping left
    /// every pin in this file green.
    #[wasm_bindgen_test]
    async fn the_loops_own_close_arm_records_a_clean_stop() {
        install_fake_ws(1000);
        let client = WsRpcClient::connect(
            "http://127.0.0.1:1".into(),
            "00".repeat(32),
            js_sys::Function::new_no_args("return ''"),
        );
        let stop = wait_for_stop(&client).await;
        restore_real_ws();
        assert!(
            matches!(stop, Some(WsRpcError::NotConnected)),
            "got {stop:?}"
        );
    }

    /// The 4426 twin of the above: the loop's own close arm must map a
    /// subprotocol-mismatch close to [`SupervisorStop::SubprotocolMismatch`],
    /// not just the by-hand pin in
    /// [`a_request_on_a_stopped_client_fails_at_once_with_why`].
    #[wasm_bindgen_test]
    async fn the_loops_own_close_arm_records_a_subprotocol_mismatch_stop() {
        install_fake_ws(4426);
        let client = WsRpcClient::connect(
            "http://127.0.0.1:1".into(),
            "00".repeat(32),
            js_sys::Function::new_no_args("return ''"),
        );
        let stop = wait_for_stop(&client).await;
        restore_real_ws();
        assert!(
            matches!(stop, Some(WsRpcError::SubprotocolMismatch)),
            "got {stop:?}"
        );
    }

    /// **A refused re-mint stops the loop, typed.** The token provider rejects
    /// the way the SPA's `getAuthToken` does for `fauna.auth.not_registered`
    /// (an error whose `code` is the wire code); the loop must stop for good
    /// on it — never back off and retry — and report the stop as the sign-in
    /// refusal the SPA routes to the launch surface. Any other rejection stays
    /// a retryable token failure: the second client, rejecting with a plain
    /// error, is still running after the first has stopped.
    #[wasm_bindgen_test]
    async fn a_refused_token_mint_stops_the_loop_as_a_sign_in_refusal() {
        let refused = WsRpcClient::connect(
            "http://127.0.0.1:1".into(),
            "00".repeat(32),
            js_sys::Function::new_no_args(
                "return Promise.reject(Object.assign(new Error('refused'), \
                 { code: 'fauna.auth.not_registered' }))",
            ),
        );
        let blip = WsRpcClient::connect(
            "http://127.0.0.1:1".into(),
            "00".repeat(32),
            js_sys::Function::new_no_args("return Promise.reject(new Error('offline'))"),
        );
        let stop = wait_for_stop(&refused).await;
        assert!(
            matches!(&stop, Some(WsRpcError::Rpc(e)) if e.is_not_registered()),
            "got {stop:?}"
        );
        assert!(refused.stopped_by_sign_in_refusal());
        assert!(
            blip.supervisor_stop().is_none() && !blip.stopped_by_sign_in_refusal(),
            "an ordinary token failure backs off; it never stops the loop"
        );
        refused.close();
        blip.close();
    }

    /// **Composes the two halves above: a request against the loop's OWN
    /// recorded stop, not one planted by hand.** The two tests above prove
    /// the loop *produces* the record, but assert only `supervisor_stop()`;
    /// the by-hand tests further up prove a *planted* record is *consumed*,
    /// but never one the loop itself wrote — so nothing proved a request
    /// against a live-loop-produced stop fails with why. This is
    /// this crate's only assertion on a request's error following a
    /// loop-driven close, deliberately: bolting it onto a test that already
    /// asserts `wait_for_stop`'s record would let a broken request path red
    /// through that pre-existing assert instead of this one.
    ///
    /// Uses the 4426 close, not 1000: [`WsRpcError::SubprotocolMismatch`] is
    /// the one answer no path can give without reading the record (every
    /// other path a request can take — the deadline elapsing, a state read
    /// of `Disconnected` — answers plain `NotConnected`, indistinguishable
    /// from what a 1000/`Clean` stop's own mapping also answers). The
    /// request is issued after `restore_real_ws()` (matching the sibling
    /// tests above), so a failed assertion here cannot leak the fake
    /// `WebSocket` into another test.
    #[wasm_bindgen_test]
    async fn a_request_against_the_loops_own_recorded_stop_fails_with_why() {
        install_fake_ws(4426);
        let client = WsRpcClient::connect(
            "http://127.0.0.1:1".into(),
            "00".repeat(32),
            js_sys::Function::new_no_args("return ''"),
        );
        wait_for_stop(&client).await;
        restore_real_ws();
        let err = echo(&client)
            .now_or_never()
            .expect("a request against the loop's own recorded stop parked instead of failing")
            .expect_err("no nest ever serves this client");
        assert!(
            matches!(err, WsRpcError::SubprotocolMismatch),
            "got {err:?}"
        );
    }

    /// **A request waiting out a gap is not left behind a grown nap.** Two
    /// seconds of refused dials put the curve, at a 50 ms initial ceiling and
    /// full-ceiling jitter, at dials 0, 50, 150, 350, 750 and 1550 ms with the
    /// next not before 3150 ms: in the last of those seconds it dials once. With
    /// a request parked the whole time the loop must still be dialling at the
    /// initial pace, and find the nest's return within the initial ceiling (plus
    /// the establish probe a browser connection needs). The wasm twin of native
    /// `supervisor::tests::a_waiting_request_is_dialled_for_within_the_initial_ceiling`;
    /// real timers, so the bounds are loose against the 50 ms pace, and a
    /// curve-only loop misses them by seconds.
    #[wasm_bindgen_test]
    async fn a_waiting_request_is_dialled_for_within_the_initial_ceiling() {
        const PORT: u16 = 21;
        install_gap_ws();
        let client = gap_client(PORT, Duration::from_millis(50));
        let waiter = DialDemandGuard::begin(inner_of(&client));
        TimeoutFuture::new(2_000).await;
        let now = js_sys::Date::now();
        let recent: Vec<f64> = gap_dial_times(PORT)
            .into_iter()
            .filter(|t| *t >= now - 1_000.0)
            .collect();
        let widest = recent
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(0.0_f64, f64::max);
        let state = client.connection_state();

        set_gap_up(PORT);
        let took = connected_within(&client, 50.0 + ESTABLISH_PROBE_MS as f64 + 1_000.0).await;
        drop(waiter);
        end_gap(&client);
        assert_ne!(state, ConnectionState::Connected);
        assert!(
            recent.len() >= 5 && widest <= 250.0,
            "a parked request must be dialled for at the initial pace: {} dials in the last \
             second, widest gap {widest} ms",
            recent.len()
        );
        assert!(
            took.is_some(),
            "the nest came back while a request waited, yet the loop had not reconnected \
             within the initial ceiling and the establish probe"
        );
    }

    /// **A request that starts waiting cuts a nap already under way short.**
    /// After seven refused dials the curve sleeps its full 3.2 s ceiling; the
    /// nest returns and a request parks just as that nap begins. The loop must
    /// dial for the request within the initial ceiling, not when the idle nap
    /// ends. The wasm twin of native
    /// `supervisor::tests::a_request_that_starts_waiting_cuts_a_grown_nap_short`.
    #[wasm_bindgen_test]
    async fn a_request_that_starts_waiting_cuts_a_grown_nap_short() {
        const PORT: u16 = 22;
        install_gap_ws();
        let client = gap_client(PORT, Duration::from_millis(50));
        // Dials at 0, 50, 150, 350, 750, 1550, 3150 ms; the seventh starts a
        // 3.2 s nap.
        let mut waited = 0;
        while dials(PORT) < 7 && waited < 5_000 {
            TimeoutFuture::new(10).await;
            waited += 10;
        }
        let before = dials(PORT);
        set_gap_up(PORT);
        let start = js_sys::Date::now();
        let waiter = DialDemandGuard::begin(inner_of(&client));
        while dials(PORT) == before && js_sys::Date::now() - start < 1_000.0 {
            TimeoutFuture::new(5).await;
        }
        let redial = js_sys::Date::now() - start;
        let took = connected_within(&client, ESTABLISH_PROBE_MS as f64 + 1_000.0).await;
        drop(waiter);
        end_gap(&client);
        assert_eq!(before, 7, "the curve never reached its seventh dial");
        assert!(
            redial <= 400.0,
            "a request began waiting at the start of a 3.2 s nap, yet the next dial came \
             {redial} ms later — the loop slept on in its idle nap"
        );
        assert!(
            took.is_some(),
            "the dial made for the waiting request did not connect"
        );
    }

    /// **The waiter's dials are extra, not the curve's.** A second of a request
    /// parked against a down nest, at a 100 ms initial ceiling, must produce
    /// more dials than the curve can (it dials at 0, 100, 300 and 700 ms), yet
    /// leave the indicator short of `Unreachable` (eight counted failures) —
    /// and once nobody waits, the idle curve is back where its own dials left
    /// it: its next dial at 1500 ms, one dial in the following second, where a
    /// curve the waiter's dials had grown would sleep for minutes. The wasm
    /// twin of native
    /// `supervisor::tests::demand_dials_neither_grow_the_curve_nor_count_toward_unreachable`.
    #[wasm_bindgen_test]
    async fn demand_dials_neither_grow_the_curve_nor_count_toward_unreachable() {
        const PORT: u16 = 23;
        install_gap_ws();
        let client = gap_client(PORT, Duration::from_millis(100));
        let waiter = DialDemandGuard::begin(inner_of(&client));
        TimeoutFuture::new(1_000).await;
        let during = dials(PORT);
        let state = client.connection_state();

        drop(waiter);
        TimeoutFuture::new(1_000).await;
        let after = dials(PORT) - during;
        end_gap(&client);
        assert!(
            during >= 8,
            "a parked request must be dialled for at the initial pace: {during} dials in 1 s"
        );
        assert_ne!(
            state,
            ConnectionState::Unreachable,
            "{during} dials made for a waiting request tripped 'Cannot connect' in 1 s — the \
             waiter's dials were counted toward the unreachable run"
        );
        assert!(
            (1..=3).contains(&after),
            "with nobody waiting the idle curve must be back at its own pace: {after} dials in \
             the second after the wait ended"
        );
    }

    /// **A request parked for the connection is what counts as dial demand**,
    /// for as long as it is parked: the wasm twin of native
    /// `client::tests::a_request_waiting_out_a_gap_counts_as_dial_demand`.
    #[wasm_bindgen_test]
    fn a_request_waiting_out_a_gap_counts_as_dial_demand() {
        let client = loopless_client();
        let inner = inner_of(&client);
        assert!(!inner.demand.is_waiting());
        let mut parked = Box::pin(echo(&client));
        assert!(
            (&mut parked).now_or_never().is_none(),
            "nothing is connected, so the request must park"
        );
        assert!(
            inner.demand.is_waiting(),
            "a request parked for the connection must count as dial demand"
        );
        drop(parked);
        assert!(
            !inner.demand.is_waiting(),
            "a request that stopped waiting must stop counting"
        );
    }
}
