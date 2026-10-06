//! Reconnect supervisor — owns the WS-loop lifecycle, parameterised over the
//! auth handshake.
//!
//! [`run_supervisor`] drives a [`SupervisedChannel`] in a backoff loop: it
//! connects, hands the adapter to [`RpcDispatcher::new`] + `tokio::spawn`s the
//! returned driver, runs the per-connection setup hook
//! ([`SupervisedChannel::on_connect`]), parks the dispatcher in the shared
//! slot, awaits termination, reads the close-code-derived [`ReconnectSignal`],
//! and reacts:
//!
//! | Signal | Action |
//! |---|---|
//! | CleanDisconnect | exit loop; connection_state=Disconnected |
//! | AuthExpired | [`SupervisedChannel::refresh_auth`] + reconnect immediately |
//! | SubprotocolMismatch | exit loop; surface [`SupervisorError::SubprotocolMismatch`] |
//! | Retry | backoff (1s → 60s ceiling, exp, full-jittered), reconnect |
//!
//! The two variation points across consumers are exactly the trait's hooks:
//! how a connection is *established + authed* ([`connect`](SupervisedChannel::connect)),
//! what per-connection *serving* is set up ([`on_connect`](SupervisedChannel::on_connect)),
//! and how an [`AuthExpired`](ReconnectSignal::AuthExpired) close is *recovered*
//! ([`refresh_auth`](SupervisedChannel::refresh_auth)). The client supplies
//! bearer-subprotocol connect + push bridging + bearer refresh; the federation
//! channel supplies the `fauna.federation.hello` handshake + inbound serving
//! (and no bearer, so it keeps the default no-op `refresh_auth`).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use fauna_protocol::reconnect::{Backoff, JitterRng, demand_nap};
use fauna_protocol::{KindRegistry, RpcDispatcher};
use futures_util::{Sink, Stream};
use tokio::sync::{Notify, RwLock, watch};

use crate::adapter::{AdapterError, ReconnectSignal};

/// Default initial backoff per spec § 3.3 ("Initial backoff 1 s"). On the swap
/// case targeted here — a nest redeploy that drops a *connected* client —
/// the ceiling is at this reset value, so the first reconnect is jittered across
/// `[0, 1s]`: prompt **and** herd-spread (see [`Backoff::jittered`]).
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Default maximum backoff per spec § 3.3 ("doubles to max 60 s"). Kept at 60 s
/// rather than lowered for the client↔nest path: with full jitter the *expected*
/// wait at the ceiling is ~30 s and the worst case 60 s, which only bites after a
/// long *outage* (not a redeploy, where the ceiling is still 1 s), and a lower cap
/// would only raise the reconnect-attempt rate against a still-down nest. The
/// const is shared with the nest↔nest federation channel (both build via
/// [`Supervisor::new`]), so it is deliberately not specialised per path.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Consecutive failed attempts to *establish* a connection before the supervisor
/// stops calling the gap transient and reports [`ConnectionState::Unreachable`].
///
/// Re-exported from `fauna_core::format`, which is the single owner — the wasm
/// reconnect loop (`fauna_rpc_wasm`) is the other implementation of this same
/// contract and reads the identical constant, so the two cannot drift.
///
/// Sized to sit comfortably above any planned redeploy: the graceful-shutdown
/// budget is bounded by the container `stop_grace_period` (15 s — § Graceful
/// shutdown), while eight failures cost roughly a minute of jittered backoff
/// (expected ≈60 s, worst case ≈2 min). So a Watchtower swap still reads as
/// "Connecting…", and only a box that is genuinely not coming back trips it.
pub use fauna_core::format::CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES as UNREACHABLE_AFTER_CONSECUTIVE_FAILURES;

/// Lifecycle state of the supervised connection, driven by [`run_supervisor`]
/// over a `watch` channel. Consumers observe it to gate UI / pool readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Connecting,
    Disconnected,
    /// Connecting has failed [`UNREACHABLE_AFTER_CONSECUTIVE_FAILURES`] times in
    /// a row with no established connection in between — the gap is **not**
    /// transient, and the client says so instead of showing an indefinite
    /// "Connecting…".
    ///
    /// Deliberately *derived client-side from failure to connect alone*, never
    /// from anything the nest tells us: the whole point is that it must be
    /// reachable on a box whose socket is exactly what is failing (a firewalled
    /// :80 leaves the nest on its self-signed floor, which a browser will not
    /// carry a WSS handshake over — and the admin-only `fauna.tls.cert_status`
    /// projection that would name the cause lives *behind* that same socket).
    ///
    /// It is likewise **generic on purpose**. The cause is not uniformly
    /// knowable: a native app reads its own rustls error, but the browser
    /// `WebSocket` API hides both the upgrade status and the TLS reason
    /// (`fauna-rpc-wasm`'s `Retry` arm says so), so a state that named the
    /// certificate would be a per-app divergence (priority #1). Naming the
    /// cause needs a separate, additive channel — not this state.
    ///
    /// The supervisor keeps retrying throughout; this is a *reporting* state,
    /// not a terminal one, and it clears on the next proven connection.
    Unreachable,
}

impl ConnectionState {
    /// The canonical lowercase wire word for this state — the string
    /// [`fauna_core::format::connection_state_label`] and
    /// [`fauna_protocol::offline_class::affordance`] both take.
    ///
    /// **Lives on the enum so it cannot drift from the variants.** Every app
    /// family needs this mapping (the `connection-status` indicator renders from
    /// it, and W4 (account-data-plane.md § Workstreams) phase 4's offline gate asks the affordance rule with it —
    /// `account-data-plane.md` § The offline-mutation contract → *How a surface
    /// asks*), and before this existed each one hand-rolled its own `match`. That
    /// is the per-app copy priority #2 forbids, and it is *quietly* dangerous
    /// here rather than merely duplicative: the gate's ruling 3 reads an
    /// unrecognised word as **online**, so a single mistyped or stale arm does
    /// not fail loudly — it silently ungates every online-only control on that
    /// app. One owner, and a new variant is a compile error at this match instead
    /// of a wrong word at four call sites.
    pub fn as_wire_word(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Connecting => "connecting",
            Self::Disconnected => "disconnected",
            Self::Unreachable => "unreachable",
        }
    }
}

#[cfg(test)]
mod connection_state_word_tests {
    use super::*;

    /// The words are a wire contract shared with `connection_state_label` and the
    /// offline gate, so they are pinned literally rather than round-tripped —
    /// a round-trip test would happily agree with itself on a renamed word.
    #[test]
    fn every_state_maps_to_its_canonical_word() {
        assert_eq!(ConnectionState::Connected.as_wire_word(), "connected");
        assert_eq!(ConnectionState::Connecting.as_wire_word(), "connecting");
        assert_eq!(ConnectionState::Disconnected.as_wire_word(), "disconnected");
        assert_eq!(ConnectionState::Unreachable.as_wire_word(), "unreachable");
    }

    /// `connection_state_label` maps three words explicitly and treats everything
    /// else as `disconnected`; the offline gate treats every *unknown* word as
    /// online. Both are only safe while our words are exactly the ones they know,
    /// so assert the agreement rather than trusting two files to stay in step.
    #[test]
    fn the_label_resolver_recognises_every_word_we_emit() {
        for (state, expected) in [
            (ConnectionState::Connected, "common.connected"),
            (ConnectionState::Connecting, "common.connecting"),
            (ConnectionState::Unreachable, "common.cannot_connect"),
            (ConnectionState::Disconnected, "common.disconnected"),
        ] {
            assert_eq!(
                fauna_core::format::connection_state_label(state.as_wire_word()).key,
                expected,
                "{state:?} must resolve to its own label, not the catch-all"
            );
        }
    }
}

/// Trait object–shaped result of one connect attempt: the connected adapter
/// (Stream+Sink+last_signal) wrapped as a generic boxed type so the supervisor
/// doesn't need to be generic over the adapter type.
///
/// Implemented for [`crate::adapter::TungsteniteAdapter`] in production; tests
/// substitute their own mpsc-backed implementations
/// ([`crate::testing::MpscAdapter`]).
pub trait ConnectedAdapter:
    Stream<Item = Result<Bytes, AdapterError>>
    + Sink<Bytes, Error = AdapterError>
    + Send
    + Unpin
    + 'static
{
    /// Reason the underlying transport ended, populated after the stream
    /// returns `Poll::Ready(None)`. Returns `None` while still connected.
    fn last_signal(&self) -> Option<ReconnectSignal>;
}

impl<S> ConnectedAdapter for crate::adapter::TungsteniteAdapter<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    fn last_signal(&self) -> Option<ReconnectSignal> {
        Self::last_signal(self)
    }
}

/// The parameterisation seam: one impl per consumer supplies how a connection
/// is established + authed, what per-connection serving is set up, and how an
/// `AuthExpired` close is recovered. Everything else (backoff, dispatcher
/// spawn, slot parking, state transitions, signal demux) is shared in
/// [`run_supervisor`].
#[async_trait]
pub trait SupervisedChannel: Send + Sync + 'static {
    /// Per-connection guard returned by [`on_connect`](Self::on_connect) and
    /// torn down by [`on_disconnect`](Self::on_disconnect) after the driver
    /// ends. The client uses the push-bridge `JoinHandle`; the federation
    /// channel uses the inbound-serving `JoinHandle`. `()` if there is none.
    type Session: Send + 'static;

    /// Error surfaced by [`connect`](Self::connect) / [`refresh_auth`](Self::refresh_auth).
    type Error: std::error::Error + Send + 'static;

    /// Establish one connection: the WS upgrade plus any in-band auth (the
    /// client's bearer-subprotocol handshake; the federation channel's dial +
    /// TLS validation). Returns the adapter to drive. A returned `Err` is
    /// logged and the supervisor backs off and retries.
    async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, Self::Error>;

    /// Classify a [`connect`](Self::connect) failure: did the server *reject our
    /// auth* (a `401` on the WS upgrade) rather than fail at the transport layer?
    /// When `true`, [`run_supervisor`] calls [`refresh_auth`](Self::refresh_auth)
    /// and retries immediately instead of backing off with the same — now stale —
    /// credential. This is the upgrade-time twin of the `4401` `AuthExpired`
    /// close path: a nest factory-reset wipes its token store, so the client's
    /// cached bearer is rejected at the *upgrade* itself (the WS never opens, so
    /// no `4401` close code is ever seen — without this the loop would back off
    /// forever with the dead token). Default `false` — the federation channel
    /// carries no bearer, so a connect failure there is always transport.
    fn connect_error_is_auth_rejection(_err: &Self::Error) -> bool {
        false
    }

    /// Classify a [`connect`](Self::connect) failure as a refusal the far side
    /// **answered** — it is up and said no (an auth rejection, a rate limit) —
    /// rather than a gap in reaching it. Dial-on-demand ([`DialDemand`]) exists
    /// to catch a nest coming *back* the moment it does; a nest that is answering
    /// is not coming back, so after such a refusal the supervisor sleeps out its
    /// curve even while requests wait. Without this a dead credential with a
    /// request always waiting redialled within the initial ceiling for as long as
    /// requests kept coming. Default: [`connect_error_is_auth_rejection`](Self::connect_error_is_auth_rejection).
    fn connect_error_is_answered_refusal(err: &Self::Error) -> bool {
        Self::connect_error_is_auth_rejection(err)
    }

    /// Classify a [`connect`](Self::connect) failure as **terminal**: a refusal
    /// no retry and no credential refresh can ever clear, because the account
    /// this channel authenticates as has stopped existing on the far side.
    ///
    /// When `true`, [`run_supervisor`] stops with
    /// [`SupervisorError::TerminalRefusal`] instead of backing off. Retrying
    /// such a refusal is not merely useless — it is an unbounded stream of
    /// doomed handshakes against the nest, and it leaves the user staring at
    /// "Connecting…" forever with the real reason nowhere on screen.
    ///
    /// Deliberately **domain-free here**: this substrate serves both the client
    /// channel and the federation channel, so it classifies nothing itself. The
    /// consumer decides — `fauna-client` returns `true` once its bearer mint has
    /// latched a `fauna.auth.superseded` refusal (the identity was succeeded;
    /// `identity-succession.md` § Propagation). Default `false`.
    ///
    /// Takes `&self` because the deciding evidence is usually not in the error
    /// value: by the time a mint failure has crossed the bearer-source boundary
    /// it is a flattened status, and the structured refusal lives on the channel.
    fn connect_error_is_terminal(&self, _err: &Self::Error) -> bool {
        false
    }

    /// Classify a [`connect`](Self::connect) failure as a refusal that is
    /// terminal **until a known time**, answering how long is left.
    ///
    /// When `Some`, [`run_supervisor`] neither stops nor backs off: it holds —
    /// no dial, no credential refresh, no endpoint re-resolution, and no early
    /// dial for a waiting request — and asks again every [`HOLD_RECHECK`] until
    /// the channel answers `None`, then dials. Stopping would be wrong because
    /// the refusal clears by itself and nothing would restart the loop;
    /// backing off would be wrong because every dial before the time re-earns
    /// the same refusal.
    ///
    /// Domain-free for the reason [`connect_error_is_terminal`](Self::connect_error_is_terminal)
    /// is: `fauna-client` answers from its bearer mint's latched
    /// `fauna.auth.account_locked` refusal (`devices.md` § The locked state).
    /// Default `None`. Consulted after the terminal test and ahead of every
    /// retry path.
    fn connect_error_hold(&self, _err: &Self::Error) -> Option<Duration> {
        None
    }

    /// The kind metadata this channel's requests are described by, attached to
    /// every dispatcher this supervisor builds — including each reconnect's,
    /// since the dispatcher is rebuilt per connection and the wire
    /// `replay_forbidden` hint is read off it.
    ///
    /// Declarative on purpose: a reconnect cannot silently stop sending the
    /// hint by forgetting a setup line, because there is no line to forget.
    /// Default `None` — correct for the federation channel, whose
    /// `fauna.federation.*` kinds the client `KindRegistry` deliberately does
    /// not declare (`bins/fauna-nest/src/rpc_router.rs`, the parity test's
    /// scope note).
    fn kind_registry(&self) -> Option<KindRegistry> {
        None
    }

    /// Backoff bounds `(initial, max)` to use instead of the supervisor's own,
    /// read afresh on every backed-off failure — the e2e seam that lets a test
    /// reach [`ConnectionState::Unreachable`] without spending the minute or two
    /// of real backoff the production bounds cost (convention 14: never wait
    /// out a clock a test can poke).
    ///
    /// `None` — the default, and the only answer a production channel gives —
    /// means [`Supervisor::initial_backoff`]/[`Supervisor::max_backoff`], so a
    /// channel that stops answering `Some` is back on the production pace. Only
    /// the *pace* moves: the state still needs
    /// [`UNREACHABLE_AFTER_CONSECUTIVE_FAILURES`] real failed attempts, and the
    /// collapse to `Unreachable` and its stickiness are the production code
    /// either way. A change of bounds restarts the ceiling at the new `initial`.
    fn backoff_override(&self) -> Option<(Duration, Duration)> {
        None
    }

    /// The count of requests waiting on this channel's connection, if the
    /// consumer keeps one (see [`DialDemand`]). While it is non-zero a refused
    /// dial is retried at the initial pace instead of sleeping out the grown
    /// ceiling. Default `None` — the federation pool has no parked requests.
    fn dial_demand(&self) -> Option<Arc<DialDemand>> {
        None
    }

    /// Set up per-connection serving once the dispatcher is live, *before* the
    /// connection is announced `Connected` — so a federation `hello` handshake
    /// that fails tears the connection down without ever exposing it. The
    /// client bridges pushes (infallible); the federation channel runs the
    /// `hello` handshake then spawns the inbound-request serving loop. A
    /// returned `Err` is treated like a failed connect (backoff + retry).
    async fn on_connect(
        &self,
        dispatcher: &Arc<RpcDispatcher>,
    ) -> Result<Self::Session, Self::Error>;

    /// Tear down the per-connection session after the driver ends (the stream
    /// closed and the dispatcher was dropped, so any broadcast/serving channel
    /// is already closing). Default: drop. The client overrides to `await` the
    /// push-bridge task so its exit is confirmed before the next cycle.
    async fn on_disconnect(&self, session: Self::Session) {
        let _ = session;
    }

    /// React to a 4401 `AuthExpired` close before reconnecting. The client
    /// clears + re-mints its bearer; the federation channel has no bearer and
    /// keeps this default no-op. A returned `Err` stops the supervisor.
    async fn refresh_auth(&self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Try to recover a *moved* endpoint after a transport [`connect`](Self::connect)
    /// failure (as opposed to an auth rejection): an admin may have changed the
    /// nest's client-facing serving port, so the persisted `nest_url` now points
    /// at a dead port. The client re-resolves `_fauna._tcp.<host>` and, if it
    /// advertises a different port, atomically swaps its live nest URL to it and
    /// returns `true` — [`run_supervisor`] then resets the backoff and retries
    /// **immediately** on the new endpoint (the offline-client SRV self-heal,
    /// `docs/goal/architecture/nest/common.md` § Serving ports, path 2). Returns
    /// `false` when there is nothing to recover — a local target (loopback / IP /
    /// `.local`, no public SRV zone), an unchanged SRV port, or a lookup error —
    /// and the supervisor backs off as usual. Default no-op `false`: the
    /// federation channel re-resolves its peer at dial time and the test channels
    /// have no endpoint to move.
    async fn recover_endpoint(&self) -> bool {
        false
    }
}

/// Requests parked on a channel's connection — `transport.md` § Request
/// lifecycle step 3, the in-gap wait — as the supervisor sees them.
///
/// The backoff curve paces an *idle* reconnect: after a few refused dials its
/// ceiling is already past a read's whole deadline (1 → 2 → 4 → 8 s against a
/// 5 s read), so a request parked in a gap of three seconds could wait out its
/// budget while the nest was already back and the supervisor still asleep —
/// the error a passing gap must never raise. So while anything waits, a refused
/// dial is retried within the initial ceiling instead, and a request that
/// starts waiting cuts a longer nap short. Those extra dials are the waiter's,
/// not the curve's: they neither grow the ceiling nor count toward the
/// `Unreachable` run, so the idle pace and the "Cannot connect" threshold are
/// what they were the moment nobody is waiting. Every waiter's wait is bounded
/// by its own deadline, so the extra rate is too.
#[derive(Debug, Default)]
pub struct DialDemand {
    waiting: AtomicUsize,
    notify: Notify,
}

impl DialDemand {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Count one waiter until the returned guard drops, and wake a supervisor
    /// that is napping on the curve.
    pub fn begin_wait(self: &Arc<Self>) -> DialDemandGuard {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        // `notify_one` keeps a permit when the supervisor is not parked on
        // `notified()` right now, so a waiter arriving between its check and its
        // park is not lost.
        self.notify.notify_one();
        DialDemandGuard(Arc::clone(self))
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst) > 0
    }
}

/// One waiter's hold on a [`DialDemand`]; dropping it ends the wait.
#[derive(Debug)]
pub struct DialDemandGuard(Arc<DialDemand>);

impl Drop for DialDemandGuard {
    fn drop(&mut self) {
        self.0.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Sleep until the curve's next dial is `due`, or — while a request waits on the
/// connection — only a jittered draw within `demand_ceiling`. Returns `true`
/// when the nap was cut short for a waiter, so the dial that follows is an
/// extra one ([`DialDemand`]).
async fn nap_until_due_or_demand(
    due: tokio::time::Instant,
    demand: Option<Arc<DialDemand>>,
    demand_ceiling: Duration,
    jitter_rng: &mut JitterRng,
) -> bool {
    let Some(demand) = demand else {
        tokio::time::sleep_until(due).await;
        return false;
    };
    loop {
        if demand.is_waiting() {
            let remaining = due.saturating_duration_since(tokio::time::Instant::now());
            let Some(short) = demand_nap(remaining, demand_ceiling, jitter_rng.next_unit()) else {
                tokio::time::sleep_until(due).await;
                return false;
            };
            tokio::time::sleep(short).await;
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(due) => return false,
            _ = demand.notify.notified() => {}
        }
    }
}

/// The longest single nap of a [`SupervisedChannel::connect_error_hold`]. The
/// channel's remaining time is re-read off its own clock after each nap, since
/// a timer does not advance while the device is suspended and one long sleep
/// would overrun the hold by however long that was.
pub const HOLD_RECHECK: Duration = Duration::from_secs(60);

/// Errors that terminate the supervisor loop. Transient connect/serving
/// failures are *not* here — they are logged and retried.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError<E: std::error::Error + 'static> {
    /// Server closed with WS code 4426 — the consumer's protocol version is
    /// incompatible with the peer's supported subprotocols.
    #[error("ws subprotocol mismatch (4426): client/server version skew")]
    SubprotocolMismatch,
    /// [`SupervisedChannel::refresh_auth`] failed after a 4401 close; the loop
    /// cannot recover (the application surfaces re-login UI).
    #[error("channel auth refresh failed: {0}")]
    AuthRefresh(#[source] E),
    /// [`SupervisedChannel::connect_error_is_terminal`] said this connect
    /// failure can never be retried away — the loop stops and the application
    /// surfaces whatever the channel latched as the reason.
    #[error("channel refused terminally: {0}")]
    TerminalRefusal(#[source] E),
}

/// Inputs the supervisor needs. Construct via [`Supervisor::new`] for the
/// default backoff, or set the fields directly to drive backoff faster in
/// tests.
pub struct Supervisor<C: SupervisedChannel> {
    /// The consumer's lifecycle hooks (connect / serve / refresh).
    pub channel: Arc<C>,
    /// Shared slot the supervisor parks the live dispatcher in; originators
    /// (the client's `request*`; the federation pool) read it.
    pub dispatcher_slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>>,
    /// Connection-state broadcast the supervisor drives.
    pub connection_state_tx: watch::Sender<ConnectionState>,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl<C: SupervisedChannel> Supervisor<C> {
    /// Construct with the default backoff parameters (1 s → 60 s).
    pub fn new(
        channel: Arc<C>,
        dispatcher_slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>>,
        connection_state_tx: watch::Sender<ConnectionState>,
    ) -> Self {
        Self {
            channel,
            dispatcher_slot,
            connection_state_tx,
            initial_backoff: INITIAL_BACKOFF,
            max_backoff: MAX_BACKOFF,
        }
    }
}

/// Shared cell that [`BoxedAdapter::poll_next`] writes into when the inner
/// stream yields `Poll::Ready(None)`. The supervisor clones this before handing
/// the adapter to the dispatcher and reads it after the driver task ends — a
/// side-channel because once the adapter is handed to the dispatcher we can no
/// longer call `last_signal()` on it directly (the dispatcher's spawned task
/// owns it, possibly on another thread).
type SignalCell = Arc<StdMutex<Option<ReconnectSignal>>>;

/// Adapter shim wrapped around `Box<dyn ConnectedAdapter>`. Forwards
/// Stream+Sink to the inner adapter and, on `poll_next` returning
/// `Poll::Ready(None)`, captures `inner.last_signal()` into the shared
/// [`SignalCell`].
struct BoxedAdapter {
    inner: Box<dyn ConnectedAdapter>,
    signal_cell: SignalCell,
}

impl BoxedAdapter {
    fn new(inner: Box<dyn ConnectedAdapter>) -> (Self, SignalCell) {
        let cell: SignalCell = Arc::new(StdMutex::new(None));
        (
            Self {
                inner,
                signal_cell: Arc::clone(&cell),
            },
            cell,
        )
    }
}

impl Stream for BoxedAdapter {
    type Item = Result<Bytes, AdapterError>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match Pin::new(&mut *self.inner).poll_next(cx) {
            std::task::Poll::Ready(None) => {
                let sig = self.inner.last_signal().unwrap_or(ReconnectSignal::Retry);
                if let Ok(mut g) = self.signal_cell.lock() {
                    *g = Some(sig);
                }
                std::task::Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl Sink<Bytes> for BoxedAdapter {
    type Error = AdapterError;
    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut *self.inner).poll_ready(cx)
    }
    fn start_send(mut self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        Pin::new(&mut *self.inner).start_send(item)
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut *self.inner).poll_flush(cx)
    }
    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut *self.inner).poll_close(cx)
    }
}

/// Type-erase a future into the boxed form `tokio::spawn` consumes here.
type Driver = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Run the supervisor loop. Returns when the loop terminates
/// (`CleanDisconnect` → `Ok`, `SubprotocolMismatch` / auth-refresh-failure →
/// `Err`).
pub async fn run_supervisor<C: SupervisedChannel>(
    cfg: Supervisor<C>,
) -> Result<(), SupervisorError<C::Error>> {
    // `backoff` is the exponential **ceiling** (doubles 1s → 60s); the actual
    // sleep is `backoff.jittered(unit)` so simultaneously-dropped devices don't
    // reconnect in lockstep (§ Reconnect & resync). Seeded once per run.
    let mut backoff = Backoff::new(cfg.initial_backoff, cfg.max_backoff);
    // The bounds `backoff` was built from, so a channel's
    // `backoff_override` rebuilds it only when the bounds actually change.
    let mut backoff_bounds = (cfg.initial_backoff, cfg.max_backoff);
    let mut jitter_rng = JitterRng::from_entropy();
    // Guards the upgrade-time 401 refresh path against a busy refresh→401 loop:
    // set once we re-mint after a rejected upgrade, cleared on the next
    // successful connect. While set, a further 401 backs off instead of
    // re-minting (a freshly minted bearer that is *still* rejected won't be
    // fixed by minting it again).
    let mut refreshed_without_progress = false;
    // Consecutive attempts that failed to reach `Connected`, reset by every
    // proven connection. Once it crosses
    // `UNREACHABLE_AFTER_CONSECUTIVE_FAILURES` the reported state becomes
    // `Unreachable` and *stays* there across further attempts — an indicator
    // that oscillated back to "Connecting…" on each retry would tell the user
    // nothing (§ Connection-status indicator).
    let mut consecutive_failures: u32 = 0;
    // The state to report while an attempt is in flight / between attempts:
    // `Connecting`/`Disconnected` normally, both collapsed to `Unreachable` once
    // the run is long enough to be worth naming.
    // `send_replace`, never `send`: a `watch` send reports `Err` **and discards
    // the value** when no receiver happens to be alive at that instant, and the
    // supervisor is routinely started before its first subscriber exists —
    // `NestClient::with_registry` drops the channel's original receiver, and
    // `connect()` only subscribes *after* the `tokio::spawn` that starts this
    // loop. A state dropped there is not merely unobserved: every later
    // subscriber, the app's connection-status indicator included, then reads the
    // stale initial `Disconnected` instead of the truth, which
    // `transport.md` § Connection-status indicator makes this watch the source
    // of. A connection state is a *state*, not an event — publishing it must not
    // depend on somebody happening to be looking. Pinned by
    // `a_state_sent_with_no_receiver_alive_is_lost_to_later_subscribers` and
    // `a_supervisor_started_with_no_subscriber_still_reports_the_truth`.
    macro_rules! report {
        ($transient:expr) => {
            cfg.connection_state_tx.send_replace(
                if consecutive_failures >= UNREACHABLE_AFTER_CONSECUTIVE_FAILURES {
                    ConnectionState::Unreachable
                } else {
                    $transient
                },
            );
        };
    }

    // Every backed-off nap goes through this first, so a channel's
    // `backoff_override` paces all four retry paths alike (a refused dial, a
    // refused per-connection setup, a 4401, a dropped connection) and the seam
    // never depends on which way the nest happened to fail.
    //
    // `None` means the supervisor's own bounds, so clearing an override
    // restores the production pace rather than leaving the last test pace in
    // force for the life of the client.
    macro_rules! adopt_backoff_override {
        () => {
            let wanted = cfg
                .channel
                .backoff_override()
                .unwrap_or((cfg.initial_backoff, cfg.max_backoff));
            if wanted != backoff_bounds {
                backoff_bounds = wanted;
                backoff = Backoff::new(wanted.0, wanted.1);
            }
        };
    }

    // When the curve's next dial is due, while a nap on it has been cut short
    // for a waiting request ([`DialDemand`]); and whether the dial now being
    // made is such an extra one. An extra dial that is refused leaves the curve
    // where it was: no growth, no count toward `Unreachable`, same due time.
    let mut curve_due: Option<tokio::time::Instant> = None;
    let mut demand_dial = false;
    // Whether the loop is inside a `connect_error_hold`, so the hold is logged
    // when it begins rather than at every recheck.
    let mut holding = false;

    loop {
        report!(ConnectionState::Connecting);
        let extra_dial = std::mem::take(&mut demand_dial);

        let adapter_box = match cfg.channel.connect().await {
            Ok(a) => a,
            Err(e) => {
                // Terminal first: a refusal no retry can clear (the account was
                // succeeded out from under this identity) must stop the loop
                // rather than enter the refresh-or-backoff paths below, both of
                // which would retry forever. Checked ahead of the 401 arm on
                // purpose — a superseded identity can also *look* like a stale
                // bearer, and re-minting it just re-earns the same refusal.
                if cfg.channel.connect_error_is_terminal(&e) {
                    tracing::error!("ws connect refused terminally: {e}; supervisor exiting");
                    report!(ConnectionState::Disconnected);
                    return Err(SupervisorError::TerminalRefusal(e));
                }
                // Terminal until a known time: hold rather than stop (the
                // refusal clears by itself) or back off (every dial before then
                // re-earns it). Ahead of the 401 and endpoint arms for the
                // reason the terminal test is — neither a re-mint nor an SRV
                // re-resolve changes the answer. The curve is left untouched:
                // a hold says nothing about how reachable the far side is.
                if let Some(hold) = cfg.channel.connect_error_hold(&e) {
                    if !holding {
                        tracing::warn!(
                            "ws connect refused until a known time: {e}; holding {hold:?} \
                             before the next dial"
                        );
                        holding = true;
                    }
                    report!(ConnectionState::Disconnected);
                    tokio::time::sleep(hold.min(HOLD_RECHECK)).await;
                    continue;
                }
                holding = false;
                // A `401` on the WS *upgrade* (vs a `4401` close on an already
                // established connection) means our bearer is stale/revoked —
                // e.g. a nest factory-reset wiped its token store. Re-mint once
                // and retry immediately rather than backing off forever with the
                // dead token (the upgrade-time twin of the `AuthExpired` path).
                if C::connect_error_is_auth_rejection(&e) && !refreshed_without_progress {
                    tracing::info!("ws upgrade rejected (401); refreshing bearer before retry");
                    match cfg.channel.refresh_auth().await {
                        Ok(()) => {
                            refreshed_without_progress = true;
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            report!(ConnectionState::Disconnected);
                            continue; // immediate retry with the fresh bearer
                        }
                        Err(re) => {
                            // Re-mint failed (e.g. the actor is mid-re-claim
                            // after a factory-reset) — transient; fall through to
                            // back off and retry, not stop the loop (unlike 4401).
                            tracing::warn!("bearer refresh after 401 failed: {re}; backing off");
                        }
                    }
                }
                // The transport connect failed (connection refused / timeout /
                // TLS error on the old port) — the nest's client-facing serving
                // port may have moved (an admin changed `serving_port`). Ask the
                // channel to re-resolve `_fauna._tcp` and, if it now points
                // elsewhere, retry immediately on the new endpoint before backing
                // off (the offline-client SRV self-heal, nest/common.md
                // § Serving ports). No-op `false` for channels with no movable
                // endpoint (federation, tests).
                if cfg.channel.recover_endpoint().await {
                    tracing::info!(
                        "ws endpoint re-resolved via SRV (serving-port change); retrying immediately"
                    );
                    backoff.reset();
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    report!(ConnectionState::Disconnected);
                    continue;
                }
                let due = match curve_due {
                    Some(due) if extra_dial => {
                        tracing::debug!("ws connect for a waiting request failed: {e}");
                        report!(ConnectionState::Disconnected);
                        due
                    }
                    _ => {
                        adopt_backoff_override!();
                        let nap = backoff.jittered(jitter_rng.next_unit());
                        tracing::warn!(
                            "ws connect failed: {e}; backing off {nap:?} (ceiling {}s)",
                            backoff.ceiling().as_secs()
                        );
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        report!(ConnectionState::Disconnected);
                        backoff.grow();
                        tokio::time::Instant::now() + nap
                    }
                };
                // The initial ceiling in force (an e2e override's included)
                // paces a waiter's dials, so a test pace reaches them too — but
                // only across a gap: a nest that answered no is not a gap
                // (`connect_error_is_answered_refusal`).
                let demand = if C::connect_error_is_answered_refusal(&e) {
                    None
                } else {
                    cfg.channel.dial_demand()
                };
                demand_dial =
                    nap_until_due_or_demand(due, demand, backoff_bounds.0, &mut jitter_rng).await;
                curve_due = demand_dial.then_some(due);
                continue;
            }
        };
        curve_due = None;
        holding = false;

        // NB: the backoff is deliberately **not** reset here. A transport
        // connect is not yet a proven connection — `on_connect` below can still
        // refuse it — and resetting on the dial alone makes the ceiling
        // unreachable for any channel that fails *after* connecting: each
        // iteration wiped the growth the previous failure had just applied, so
        // the `backoff.grow()` on that path was dead code and the loop dialled
        // forever at the initial 1 s rate. That is the 2026-08-22 mac
        // network-exhaustion incident — 32,617 ESTABLISHED sockets at ~1/s for
        // 4.5 h, with every backoff constant correct and none of them running.
        // The reset now sits at the proven-connection point below, beside
        // `consecutive_failures = 0`, which already had exactly this meaning.
        // Pinned by `on_connect_failure_still_grows_the_backoff`.
        //
        // A successful upgrade proves the bearer is good again — re-arm the
        // 401-refresh path for the next disconnect cycle. This one *is* proven
        // by the dial alone: the 401 it guards is an upgrade-time status.
        refreshed_without_progress = false;
        let (boxed, signal_cell) = BoxedAdapter::new(adapter_box);
        let (dispatcher, driver) = RpcDispatcher::new(boxed);
        // Before the dispatcher is published to `dispatcher_slot` below, so no
        // request can be issued off a registry-less dispatcher.
        if let Some(registry) = cfg.channel.kind_registry() {
            let _ = dispatcher.set_kind_registry(registry);
        }
        let driver_handle = tokio::spawn(Box::pin(driver) as Driver);
        let dispatcher = Arc::new(dispatcher);

        // Per-connection serving setup, with the dispatcher live but before the
        // connection is announced Connected. A failure here (e.g. a rejected
        // federation `hello` handshake) tears the connection down — abort the
        // driver and back off — without ever exposing the connection.
        let session = match cfg.channel.on_connect(&dispatcher).await {
            Ok(s) => s,
            Err(e) => {
                adopt_backoff_override!();
                let nap = backoff.jittered(jitter_rng.next_unit());
                tracing::warn!(
                    "ws per-connection setup failed: {e}; backing off {nap:?} (ceiling {}s)",
                    backoff.ceiling().as_secs()
                );
                driver_handle.abort();
                drop(dispatcher);
                // A rejected per-connection setup never reached `Connected`, so it
                // counts toward the unreachable run exactly like a refused dial.
                consecutive_failures = consecutive_failures.saturating_add(1);
                report!(ConnectionState::Disconnected);
                tokio::time::sleep(nap).await;
                backoff.grow();
                continue;
            }
        };

        cfg.dispatcher_slot
            .write()
            .await
            .replace(Arc::clone(&dispatcher));
        // A proven connection clears the run: the *next* outage starts counting
        // from zero, so an old failure streak can never make a fresh transient
        // gap report `Unreachable` on its first retry. The backoff ceiling
        // clears on exactly the same event and for the same reason — this is
        // the first point at which the connection has actually served, so it is
        // the only safe place to forget how hard the last one was to get. A
        // redeploy still reconnects promptly: it drops a *proven* connection,
        // so the ceiling it starts from is this reset's 1 s.
        consecutive_failures = 0;
        backoff.reset();
        cfg.connection_state_tx
            .send_replace(ConnectionState::Connected);

        // Wait for the dispatcher's driver task to finish (i.e. the stream
        // ends). The driver holds the last sender clone of the push broadcast;
        // once it exits we can drop all remaining Arc<RpcDispatcher> references
        // so the broadcast fully closes and any serving task terminates.
        let _ = driver_handle.await;

        // Drain pending RPCs by clearing the dispatcher slot, then drop our
        // local Arc. Any future request*() call sees None and returns
        // RpcDisconnected{was_in_flight:false}. RPCs already in flight were
        // resolved by the driver's own pending-table cleanup with
        // RpcError("fauna.protocol.disconnected").
        cfg.dispatcher_slot.write().await.take();
        drop(dispatcher); // fully releases Arc<RpcDispatcher>; broadcast closes
        cfg.connection_state_tx
            .send_replace(ConnectionState::Disconnected);

        // Session teardown now (any broadcast/serving channel is closed).
        cfg.channel.on_disconnect(session).await;

        let signal = signal_cell
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or(ReconnectSignal::Retry);

        match signal {
            ReconnectSignal::CleanDisconnect => {
                tracing::info!("ws clean disconnect; supervisor exiting");
                return Ok(());
            }
            ReconnectSignal::SubprotocolMismatch => {
                tracing::error!("ws subprotocol mismatch (4426); supervisor exiting");
                return Err(SupervisorError::SubprotocolMismatch);
            }
            ReconnectSignal::AuthExpired => {
                tracing::info!("ws auth expired (4401); refreshing auth");
                if let Err(e) = cfg.channel.refresh_auth().await {
                    tracing::error!("auth refresh after 4401 failed: {e}");
                    // Stop the loop — application surfaces re-login UI.
                    return Err(SupervisorError::AuthRefresh(e));
                }
                // Backoff floor, same curve as Retry. Zero-delay reconnect
                // here made spin-safety entirely load-bearing on the mint
                // gate refusing the revoked actor: any 4401 producer whose
                // actor the gate does NOT refuse (a future revocation
                // reason, a gate divergence) would hot-loop the WS upgrade
                // with no delay at all. The floor costs a legitimate
                // token-expiry reconnect at most one jittered initial
                // backoff (~1 s); `backoff` resets on the next successful
                // connection as usual.
                adopt_backoff_override!();
                let nap = backoff.jittered(jitter_rng.next_unit());
                tracing::info!(
                    "ws 4401 handled; reconnecting after {nap:?} (ceiling {}s)",
                    backoff.ceiling().as_secs()
                );
                tokio::time::sleep(nap).await;
                backoff.grow();
            }
            ReconnectSignal::Retry => {
                adopt_backoff_override!();
                let nap = backoff.jittered(jitter_rng.next_unit());
                tracing::info!(
                    "ws disconnect; backing off {nap:?} (ceiling {}s)",
                    backoff.ceiling().as_secs()
                );
                tokio::time::sleep(nap).await;
                backoff.grow();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MpscAdapter, QueueChannel, TestChannelError, mpsc_pair};
    use fauna_protocol::{Frame, Reply, Value, decode_frame, encode_frame};
    use serde::{Deserialize, Serialize};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::watch;

    #[derive(Debug, Serialize, Deserialize)]
    struct EchoIn {
        msg: String,
    }
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct EchoOut {
        msg: String,
    }
    fn encode_value<T: Serialize>(v: &T) -> Value {
        let bytes = fauna_cbor::encode_canonical(v).unwrap();
        fauna_cbor::decode_strict(&bytes).unwrap()
    }

    /// The supervisor retries failed connect attempts with backoff, succeeds on
    /// the third, parks the dispatcher in the slot, and exits cleanly on a
    /// `CleanDisconnect` close — clearing the slot.
    #[tokio::test]
    async fn supervisor_retries_then_succeeds() {
        let (adapter_ok, server_ok) = mpsc_pair();
        let closed_cell = Arc::clone(&adapter_ok.closed_with);

        let queue = Arc::new(Mutex::new(VecDeque::from([
            Err(TestChannelError("attempt 1 failed".into())),
            Err(TestChannelError("attempt 2 failed".into())),
            Ok(adapter_ok),
        ])));

        let dispatcher_slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, mut rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel: Arc::new(QueueChannel::new(Arc::clone(&queue))),
            dispatcher_slot: Arc::clone(&dispatcher_slot),
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        // Wait for Connected.
        loop {
            let s = *rx.borrow_and_update();
            if s == ConnectionState::Connected {
                break;
            }
            rx.changed().await.unwrap();
        }
        assert!(dispatcher_slot.read().await.is_some());

        // Server closes with CleanDisconnect → supervisor exits.
        *closed_cell.lock().unwrap() = Some(ReconnectSignal::CleanDisconnect);
        drop(server_ok);

        let result = sup.await.unwrap();
        assert!(result.is_ok(), "supervisor returned: {result:?}");
        assert!(dispatcher_slot.read().await.is_none());
    }

    /// A channel that rejects the WS upgrade with an *auth* failure until
    /// [`refresh_auth`](SupervisedChannel::refresh_auth) is called, then hands
    /// out a live adapter — the in-memory analogue of a stale bearer rejected at
    /// the upgrade after a nest factory-reset wiped its token store.
    struct AuthRejectChannel {
        /// Flipped `true` by `refresh_auth`; `connect` succeeds once it is set
        /// (unless `always_reject`).
        refreshed: Arc<AtomicBool>,
        /// How many times `refresh_auth` was invoked.
        refresh_calls: Arc<AtomicUsize>,
        /// Adapter handed out on the first successful (post-refresh) connect.
        adapter: Mutex<Option<MpscAdapter>>,
        /// When set, `connect` rejects even after a refresh — to exercise the
        /// busy-loop guard.
        always_reject: bool,
    }

    #[async_trait]
    impl SupervisedChannel for AuthRejectChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            if !self.always_reject
                && self.refreshed.load(Ordering::SeqCst)
                && let Some(a) = self.adapter.lock().unwrap().take()
            {
                return Ok(Box::new(a) as Box<dyn ConnectedAdapter>);
            }
            // Mirror the production mapping: an HTTP 401 on the upgrade.
            Err(TestChannelError(
                "ws upgrade rejected: HTTP 401 Unauthorized".into(),
            ))
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Ok(())
        }

        async fn refresh_auth(&self) -> Result<(), TestChannelError> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            self.refreshed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn connect_error_is_auth_rejection(_err: &TestChannelError) -> bool {
            true
        }
    }

    /// A `401` on the WS upgrade must trigger a bearer refresh + immediate retry
    /// (the factory-reset reconnect bug) — not an endless backoff with the dead
    /// token. Without the fix the supervisor never reaches `Connected`.
    #[tokio::test]
    async fn connect_401_refreshes_bearer_then_reconnects() {
        let (adapter_ok, _server_ok) = mpsc_pair();
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(AuthRejectChannel {
            refreshed: Arc::new(AtomicBool::new(false)),
            refresh_calls: Arc::clone(&refresh_calls),
            adapter: Mutex::new(Some(adapter_ok)),
            always_reject: false,
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, mut rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        let connected = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *rx.borrow_and_update() == ConnectionState::Connected {
                    break;
                }
                rx.changed().await.unwrap();
            }
        })
        .await;
        assert!(
            connected.is_ok(),
            "supervisor never reconnected after a 401 upgrade rejection (no bearer refresh)"
        );
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            1,
            "a 401 on the upgrade must refresh the bearer exactly once, then reconnect"
        );
        sup.abort();
    }

    /// A channel whose every connect is refused terminally — the shape
    /// `fauna-client` takes on once its bearer mint has latched a
    /// `fauna.auth.superseded` refusal.
    struct TerminalRefusalChannel {
        connect_calls: Arc<AtomicUsize>,
        refresh_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SupervisedChannel for TerminalRefusalChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            self.connect_calls.fetch_add(1, Ordering::SeqCst);
            Err(TestChannelError("identity was succeeded".into()))
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Ok(())
        }

        async fn refresh_auth(&self) -> Result<(), TestChannelError> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        /// The same refusal also *looks* like a stale bearer, so this channel
        /// asserts both — proving the terminal arm is consulted first.
        fn connect_error_is_auth_rejection(_err: &TestChannelError) -> bool {
            true
        }

        fn connect_error_is_terminal(&self, _err: &TestChannelError) -> bool {
            true
        }
    }

    /// A terminal refusal stops the loop instead of retrying forever.
    ///
    /// This is the succeeded-identity case (`identity-succession.md`
    /// § Propagation): the account belongs to a different keypair now, so every
    /// future handshake earns the same refusal. Before the terminal arm the
    /// supervisor backed off and retried indefinitely, which left the user on a
    /// permanent "Connecting…" with the real reason nowhere on screen — and kept
    /// re-signing a doomed handshake at the nest for as long as the app ran.
    ///
    /// Also pins the ordering: the refusal classifies as an auth rejection too,
    /// so a terminal arm checked *after* the 401 arm would re-mint the bearer and
    /// re-earn the refusal instead of stopping.
    #[tokio::test]
    async fn a_terminal_refusal_stops_the_supervisor_instead_of_retrying() {
        let connect_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(TerminalRefusalChannel {
            connect_calls: Arc::clone(&connect_calls),
            refresh_calls: Arc::clone(&refresh_calls),
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, _rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(10),
        };

        // A generous ceiling, not a timing assertion (convention 14): the
        // supervisor returns as soon as it classifies, and only a *looping*
        // supervisor can exhaust this.
        let outcome = tokio::time::timeout(Duration::from_secs(5), run_supervisor(cfg)).await;

        let result =
            outcome.expect("supervisor kept retrying a terminal refusal instead of exiting");
        assert!(
            matches!(result, Err(SupervisorError::TerminalRefusal(_))),
            "a terminal refusal must surface as TerminalRefusal, got: {result:?}"
        );
        assert_eq!(
            connect_calls.load(Ordering::SeqCst),
            1,
            "a terminal refusal must not be retried even once"
        );
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            0,
            "a terminal refusal must not re-mint the bearer — the new one earns the same refusal"
        );
    }

    /// A channel whose every connect is refused until a known time — the shape
    /// `fauna-client` takes on while its bearer mint holds a standing
    /// `fauna.auth.account_locked` refusal.
    struct HeldChannel {
        connect_calls: Arc<AtomicUsize>,
        refresh_calls: Arc<AtomicUsize>,
        held_until: tokio::time::Instant,
    }

    #[async_trait]
    impl SupervisedChannel for HeldChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            self.connect_calls.fetch_add(1, Ordering::SeqCst);
            Err(TestChannelError("account locked".into()))
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Ok(())
        }

        async fn refresh_auth(&self) -> Result<(), TestChannelError> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        /// Also *looks* like a stale bearer, so the hold must be consulted
        /// ahead of the 401 arm — the same ordering pin the terminal test has.
        fn connect_error_is_auth_rejection(_err: &TestChannelError) -> bool {
            true
        }

        fn connect_error_hold(&self, _err: &TestChannelError) -> Option<Duration> {
            let left = self
                .held_until
                .saturating_duration_since(tokio::time::Instant::now());
            (!left.is_zero()).then_some(left)
        }
    }

    /// A refusal that is terminal until a known time holds the loop until then:
    /// it neither stops (the refusal clears by itself, and nothing would restart
    /// a stopped loop) nor backs off (every dial before the time re-earns the
    /// refusal — for a locked account, by signing a ceremony the nest must
    /// refuse). Against a 10 ms backoff, a loop that did not hold would dial
    /// thousands of times in the window below; the held one dials only at each
    /// [`HOLD_RECHECK`], and re-mints nothing. Once the time passes the hold is
    /// gone and the ordinary paths resume.
    #[tokio::test(start_paused = true)]
    async fn a_held_refusal_waits_out_its_time_instead_of_backing_off() {
        let connect_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(HeldChannel {
            connect_calls: Arc::clone(&connect_calls),
            refresh_calls: Arc::clone(&refresh_calls),
            held_until: tokio::time::Instant::now() + Duration::from_secs(150),
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, rx) = watch::channel(ConnectionState::Connecting);

        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(10),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        // Inside the hold: one dial at the start and one per recheck (0 s,
        // 60 s, 120 s), each answered by the channel without a re-mint.
        tokio::time::sleep(Duration::from_secs(149)).await;
        assert!(!sup.is_finished(), "a hold must not stop the supervisor");
        assert_eq!(
            connect_calls.load(Ordering::SeqCst),
            3,
            "a held loop dials only at each recheck, never on the backoff curve"
        );
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            0,
            "a held refusal must not re-mint — the new bearer earns the same refusal"
        );
        assert_eq!(*rx.borrow(), ConnectionState::Disconnected);

        // Past it: the hold is gone, so the ordinary retry paths run again.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            refresh_calls.load(Ordering::SeqCst) >= 1,
            "once the time passes the loop resumes its ordinary retry paths"
        );
        sup.abort();
    }

    /// If a *freshly minted* bearer is still rejected (the upgrade always 401s),
    /// the supervisor must refresh at most once and then back off — never a tight
    /// refresh→401→refresh loop.
    #[tokio::test]
    async fn connect_401_refresh_is_guarded_against_busy_loop() {
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(AuthRejectChannel {
            refreshed: Arc::new(AtomicBool::new(false)),
            refresh_calls: Arc::clone(&refresh_calls),
            adapter: Mutex::new(None),
            always_reject: true,
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, _rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(15),
            max_backoff: Duration::from_millis(15),
        };
        let sup = tokio::spawn(run_supervisor(cfg));
        // Span many backoff cycles; the guard must hold refresh to exactly one.
        tokio::time::sleep(Duration::from_millis(150)).await;
        sup.abort();
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            1,
            "guard: a still-rejected fresh bearer must refresh once, then back off (no busy loop)"
        );
    }

    /// A channel whose transport connect fails until [`recover_endpoint`](
    /// SupervisedChannel::recover_endpoint) is called (which re-resolves the
    /// moved port), then hands out a live adapter — the in-memory analogue of an
    /// admin changing the nest's serving port: the old port is dead, and only an
    /// SRV re-resolve recovers it. The failure is a *transport* error, NOT an
    /// auth rejection (the default `connect_error_is_auth_rejection` = false), so
    /// the recovery path — not the 401 path — must drive the reconnect.
    struct MovedEndpointChannel {
        recovered: Arc<AtomicBool>,
        recover_calls: Arc<AtomicUsize>,
        adapter: Mutex<Option<MpscAdapter>>,
    }

    #[async_trait]
    impl SupervisedChannel for MovedEndpointChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            if self.recovered.load(Ordering::SeqCst)
                && let Some(a) = self.adapter.lock().unwrap().take()
            {
                return Ok(Box::new(a) as Box<dyn ConnectedAdapter>);
            }
            Err(TestChannelError("connection refused: old port dead".into()))
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Ok(())
        }

        async fn recover_endpoint(&self) -> bool {
            self.recover_calls.fetch_add(1, Ordering::SeqCst);
            self.recovered.store(true, Ordering::SeqCst);
            true // the SRV now advertises a new port
        }
    }

    /// A transport connect-failure (the nest moved its client-facing serving
    /// port) must trigger `recover_endpoint`; once it re-resolves the new port
    /// the supervisor reconnects immediately — it does NOT back off forever
    /// against the dead old port. Without the recovery hook the loop never
    /// reaches `Connected`.
    #[tokio::test]
    async fn transport_failure_recovers_moved_endpoint_then_reconnects() {
        let (adapter_ok, _server_ok) = mpsc_pair();
        let recover_calls = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(MovedEndpointChannel {
            recovered: Arc::new(AtomicBool::new(false)),
            recover_calls: Arc::clone(&recover_calls),
            adapter: Mutex::new(Some(adapter_ok)),
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, mut rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        let connected = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *rx.borrow_and_update() == ConnectionState::Connected {
                    break;
                }
                rx.changed().await.unwrap();
            }
        })
        .await;
        assert!(
            connected.is_ok(),
            "supervisor never reconnected after the serving port moved (no SRV recovery)"
        );
        assert!(
            recover_calls.load(Ordering::SeqCst) >= 1,
            "a transport connect-failure must call recover_endpoint to re-resolve the moved port"
        );
        sup.abort();
    }

    /// A 4426 SubprotocolMismatch close exits the loop with the typed error.
    #[tokio::test]
    async fn supervisor_exits_on_subprotocol_mismatch() {
        let (adapter, server) = mpsc_pair();
        let closed_cell = Arc::clone(&adapter.closed_with);

        let queue = Arc::new(Mutex::new(VecDeque::from([Ok(adapter)])));
        let slot = Arc::new(RwLock::new(None));
        let (tx, _rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel: Arc::new(QueueChannel::new(queue)),
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        // Give the supervisor a moment to connect, then close 4426.
        tokio::time::sleep(Duration::from_millis(50)).await;
        *closed_cell.lock().unwrap() = Some(ReconnectSignal::SubprotocolMismatch);
        drop(server);

        let result = sup.await.unwrap();
        match result {
            Err(SupervisorError::SubprotocolMismatch) => {}
            other => panic!("expected SubprotocolMismatch, got {other:?}"),
        }
    }

    /// A *persistently* failing connect settles on
    /// [`ConnectionState::Unreachable`] and **stays** there — it does not flicker
    /// back to `Connecting`/`Disconnected` on each further attempt.
    ///
    /// This is the state the three-state model could not express: the indicator
    /// showed a firewalled/untrusted nest exactly like a one-second Watchtower
    /// swap, so a permanently unreachable box read as "still connecting" forever
    /// with no diagnostic anywhere (transport.md § Connection-status indicator).
    #[tokio::test]
    async fn persistent_connect_failure_settles_on_unreachable() {
        // Exhausted queue → `connect()` errors forever.
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let slot = Arc::new(RwLock::new(None));
        let (tx, mut rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel: Arc::new(QueueChannel::new(queue)),
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        // Deadline poll on the state itself — no wall-clock assertion (testing.md
        // § point 14): a green run settles in milliseconds and pays nothing.
        tokio::time::timeout(Duration::from_secs(10), async {
            while *rx.borrow_and_update() != ConnectionState::Unreachable {
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("supervisor never reported Unreachable under a permanently failing connect");

        // …and it is sticky: every subsequent transition stays Unreachable, so the
        // indicator does not oscillate once it has told the truth.
        for _ in 0..UNREACHABLE_AFTER_CONSECUTIVE_FAILURES {
            rx.changed().await.unwrap();
            assert_eq!(
                *rx.borrow_and_update(),
                ConnectionState::Unreachable,
                "Unreachable must be sticky until a connection is actually established"
            );
        }

        sup.abort();
    }

    /// The counter resets on a *proven* connection: a box that fails past the
    /// threshold, recovers, then fails again must report `Unreachable` only after
    /// a fresh run of failures — never carry the old run's count forward.
    #[tokio::test]
    async fn a_proven_connection_clears_the_unreachable_run() {
        let (adapter, server) = mpsc_pair();
        let mut q: VecDeque<Result<MpscAdapter, TestChannelError>> = (0
            ..UNREACHABLE_AFTER_CONSECUTIVE_FAILURES)
            .map(|i| Err(TestChannelError(format!("attempt {i} failed"))))
            .collect();
        q.push_back(Ok(adapter));
        let queue = Arc::new(Mutex::new(q));
        let slot = Arc::new(RwLock::new(None));
        let (tx, mut rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel: Arc::new(QueueChannel::new(Arc::clone(&queue))),
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        // Unreachable first (the queued run of failures), then Connected once the
        // adapter is handed out — proving the state is not terminal.
        tokio::time::timeout(Duration::from_secs(10), async {
            while *rx.borrow_and_update() != ConnectionState::Unreachable {
                rx.changed().await.unwrap();
            }
            while *rx.borrow_and_update() != ConnectionState::Connected {
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("supervisor never recovered from Unreachable to Connected");

        // The queue is now exhausted, so the post-drop reconnects fail again. The
        // run must restart from zero: at least one non-Unreachable state
        // (`Connecting`/`Disconnected`) has to be observed before Unreachable
        // returns — if the count had carried over, the very next transition would
        // be Unreachable.
        drop(server);
        let mut saw_fresh_attempt = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                rx.changed().await.unwrap();
                match *rx.borrow_and_update() {
                    ConnectionState::Unreachable => break,
                    _ => saw_fresh_attempt = true,
                }
            }
        })
        .await
        .expect("supervisor never re-reported Unreachable after the second failure run");
        assert!(
            saw_fresh_attempt,
            "the failure counter carried over a proven connection — Unreachable returned \
             without a fresh Connecting/Disconnected run"
        );

        sup.abort();
    }

    /// End-to-end through the supervisor: once connected, an RPC originated via
    /// the parked dispatcher reaches the echo server and the typed reply
    /// round-trips. Proves the dispatcher the supervisor parks is usable.
    #[tokio::test]
    async fn parked_dispatcher_round_trips_an_rpc() {
        let (adapter, mut server) = mpsc_pair();
        let queue = Arc::new(Mutex::new(VecDeque::from([Ok(adapter)])));
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, mut rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel: Arc::new(QueueChannel::new(queue)),
            dispatcher_slot: Arc::clone(&slot),
            connection_state_tx: tx,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        };
        let _sup = tokio::spawn(run_supervisor(cfg));

        // Echo server: reply to one Request.
        let server_task = tokio::spawn(async move {
            let bytes = server.rx_from_client.recv().await.unwrap();
            let Frame::Request(req) = decode_frame(&bytes).unwrap() else {
                panic!("expected a Request frame");
            };
            let payload_bytes = fauna_cbor::encode_canonical(&req.payload).unwrap();
            let echo: EchoIn = fauna_cbor::decode_strict(&payload_bytes).unwrap();
            let reply = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id: req.correlation_id,
                payload: encode_value(&EchoOut { msg: echo.msg }),
                ok: true,
            });
            server
                .tx_to_client
                .send(encode_frame(&reply).unwrap())
                .await
                .unwrap();
        });

        // Wait for Connected, then originate via the parked dispatcher.
        loop {
            if *rx.borrow_and_update() == ConnectionState::Connected {
                break;
            }
            rx.changed().await.unwrap();
        }
        let dispatcher = slot.read().await.as_ref().unwrap().clone();
        let call = dispatcher
            .request_raw(
                "fauna.protocol.echo",
                [7u8; 16],
                encode_value(&EchoIn { msg: "hi".into() }),
                Some(Duration::from_secs(2)),
            )
            .await
            .unwrap();
        let reply_value = call.await_reply().await.unwrap();
        let bytes = fauna_cbor::encode_canonical(&reply_value).unwrap();
        let reply: EchoOut = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(reply, EchoOut { msg: "hi".into() });
        server_task.await.unwrap();
    }

    /// A channel whose transport `connect()` always succeeds and whose
    /// per-connection `on_connect()` always fails — the shape behind the
    /// 2026-08-22 mac network-exhaustion incident, where an orphaned e2e
    /// agent/nest pair accumulated 32,617 ESTABLISHED sockets at ~1/s for
    /// 4.5 h and exhausted the VM's network state.
    struct OnConnectAlwaysFailsChannel {
        connects: Arc<AtomicUsize>,
        /// The server ends, held for the life of the test: dropping one would
        /// end its stream and retire that connection on its own, which is not
        /// the failure under test.
        servers: Mutex<Vec<crate::testing::ServerSide>>,
    }

    #[async_trait]
    impl SupervisedChannel for OnConnectAlwaysFailsChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            // A *fresh* socket every attempt, exactly as the real dialer does —
            // every socket the incident accumulated had reached ESTABLISHED.
            let (adapter, server) = mpsc_pair();
            self.servers.lock().unwrap().push(server);
            Ok(Box::new(adapter) as Box<dyn ConnectedAdapter>)
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Err(TestChannelError("per-connection setup refused".into()))
        }
    }

    /// **Row 58 — the reconnect-attempt rate must stay bounded when the failure
    /// is `on_connect`, not `connect`.**
    ///
    /// `backoff.reset()` fires the moment the *transport* connects, but a
    /// connection is not proven until `on_connect` has also succeeded. A channel
    /// whose `on_connect` fails deterministically therefore had its ceiling wiped
    /// on every single iteration: the `backoff.grow()` on that failure path was
    /// dead code, the ceiling never left its 1 s initial value, and the loop
    /// dialled forever at the initial rate. That is the 4.5 h x ~1/s the incident
    /// measured — every backoff constant was correct and none of them ran.
    ///
    /// Asserts latency-independent state under a virtual clock (convention 14):
    /// the number of dials inside a fixed virtual window, against a budget sized
    /// far above a healthy growing backoff and far below a stuck one. With the
    /// ceiling growing 1->2->...->60 s a five-minute window admits ~10 dials;
    /// with the ceiling pinned at 1 s (full-jittered, ~0.5 s mean) it admits ~600.
    #[tokio::test(start_paused = true)]
    async fn on_connect_failure_still_grows_the_backoff() {
        let connects = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(OnConnectAlwaysFailsChannel {
            connects: Arc::clone(&connects),
            servers: Mutex::new(Vec::new()),
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, _rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: INITIAL_BACKOFF,
            max_backoff: MAX_BACKOFF,
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        // Auto-advancing virtual time: this sleep and the supervisor's own
        // sleeps share one clock, so the window costs no wall-clock seconds.
        tokio::time::sleep(Duration::from_secs(300)).await;
        sup.abort();

        let dials = connects.load(Ordering::SeqCst);
        assert!(
            dials <= 50,
            "the supervisor dialled {dials} times in 5 virtual minutes against a channel whose \
             on_connect always fails; a growing 1s->60s ceiling admits ~10. The backoff is being \
             reset on transport-connect success, before the connection is proven, so it never \
             grows — the shape that produced 32,617 ESTABLISHED sockets on 2026-08-22"
        );
    }

    /// A channel whose dial always fails and which asks for its own backoff
    /// bounds — the e2e seam's shape (`SupervisedChannel::backoff_override`).
    struct PacedFailingChannel {
        connects: Arc<AtomicUsize>,
        /// Shared with the test, so it can clear the override mid-run.
        bounds: Arc<Mutex<Option<(Duration, Duration)>>>,
    }

    #[async_trait]
    impl SupervisedChannel for PacedFailingChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            Err(TestChannelError("connection refused".into()))
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Ok(())
        }

        fn backoff_override(&self) -> Option<(Duration, Duration)> {
            *self.bounds.lock().unwrap()
        }
    }

    /// `backoff_override` paces a supervisor built with the PRODUCTION bounds,
    /// and the pace is all it changes: the run still settles on `Unreachable`
    /// through the ordinary threshold.
    ///
    /// Asserts latency-independent state under a virtual clock (convention 14):
    /// dials inside one virtual second. With the production 1 s → 60 s ceiling
    /// that window admits a handful at most — full jitter can land the first few
    /// naps early (three dials about a quarter of the time), but ten would need
    /// nine naps under 1 s against ceilings reaching 60 s; with the override's
    /// 1 ms → 2 ms it admits hundreds. The unpaced control proves the window
    /// really does separate the two.
    #[tokio::test(start_paused = true)]
    async fn a_backoff_override_paces_the_production_bounds() {
        async fn dials_in_one_virtual_second(
            bounds: Option<(Duration, Duration)>,
        ) -> (usize, ConnectionState) {
            let connects = Arc::new(AtomicUsize::new(0));
            let channel = Arc::new(PacedFailingChannel {
                connects: Arc::clone(&connects),
                bounds: Arc::new(Mutex::new(bounds)),
            });
            let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
            let (tx, rx) = watch::channel(ConnectionState::Disconnected);
            let cfg = Supervisor {
                channel,
                dispatcher_slot: slot,
                connection_state_tx: tx,
                initial_backoff: INITIAL_BACKOFF,
                max_backoff: MAX_BACKOFF,
            };
            let sup = tokio::spawn(run_supervisor(cfg));
            tokio::time::sleep(Duration::from_secs(1)).await;
            sup.abort();
            let state = *rx.borrow();
            (connects.load(Ordering::SeqCst), state)
        }

        let (paced, paced_state) =
            dials_in_one_virtual_second(Some((Duration::from_millis(1), Duration::from_millis(2))))
                .await;
        assert!(
            paced >= 50,
            "the override must pace the retries: only {paced} dials in a virtual second"
        );
        assert_eq!(
            paced_state,
            ConnectionState::Unreachable,
            "a paced run past the threshold settles on Unreachable exactly as an unpaced one does"
        );

        let (unpaced, _) = dials_in_one_virtual_second(None).await;
        assert!(
            unpaced <= 10,
            "the control must stay on the production bounds: {unpaced} dials in a virtual second"
        );
    }

    /// Clearing the override puts a RUNNING supervisor back on the production
    /// pace — a test that paced a shared app's client must not leave every later
    /// test on that client retrying at a test pace forever.
    ///
    /// Same virtual-clock measure as above: hundreds of dials while paced, then
    /// no more than the production handful in the virtual second after the clear.
    #[tokio::test(start_paused = true)]
    async fn clearing_the_backoff_override_restores_the_production_pace() {
        let connects = Arc::new(AtomicUsize::new(0));
        let bounds = Arc::new(Mutex::new(Some((
            Duration::from_millis(1),
            Duration::from_millis(2),
        ))));
        let channel = Arc::new(PacedFailingChannel {
            connects: Arc::clone(&connects),
            bounds: Arc::clone(&bounds),
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, _rx) = watch::channel(ConnectionState::Disconnected);
        let cfg = Supervisor {
            channel,
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: INITIAL_BACKOFF,
            max_backoff: MAX_BACKOFF,
        };
        let sup = tokio::spawn(run_supervisor(cfg));

        tokio::time::sleep(Duration::from_secs(1)).await;
        let paced = connects.load(Ordering::SeqCst);
        assert!(
            paced >= 50,
            "precondition: the override paced the run ({paced} dials)"
        );

        *bounds.lock().unwrap() = None;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let after_clear = connects.load(Ordering::SeqCst) - paced;
        sup.abort();
        assert!(
            after_clear <= 10,
            "a cleared override must restore the production pace; {after_clear} dials in the \
             virtual second after the clear"
        );
    }

    /// A nest that refuses every dial until the test brings it back, behind a
    /// consumer that counts its parked requests — the client channel's shape.
    struct GapChannel {
        connects: Arc<AtomicUsize>,
        up: Arc<AtomicBool>,
        /// While down, refuse as a nest that *answered* (a 401-shaped refusal)
        /// rather than one that could not be reached.
        answers: Arc<AtomicBool>,
        demand: Arc<DialDemand>,
        /// Held so an accepted connection stays open for the test's life.
        servers: Mutex<Vec<crate::testing::ServerSide>>,
    }

    #[async_trait]
    impl SupervisedChannel for GapChannel {
        type Session = ();
        type Error = TestChannelError;

        async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            if !self.up.load(Ordering::SeqCst) {
                if self.answers.load(Ordering::SeqCst) {
                    return Err(TestChannelError("answered: refused".into()));
                }
                return Err(TestChannelError("connection refused".into()));
            }
            let (adapter, server) = mpsc_pair();
            self.servers.lock().unwrap().push(server);
            Ok(Box::new(adapter) as Box<dyn ConnectedAdapter>)
        }

        async fn on_connect(
            &self,
            _dispatcher: &Arc<RpcDispatcher>,
        ) -> Result<Self::Session, TestChannelError> {
            Ok(())
        }

        fn dial_demand(&self) -> Option<Arc<DialDemand>> {
            Some(Arc::clone(&self.demand))
        }

        fn connect_error_is_answered_refusal(err: &TestChannelError) -> bool {
            err.0.starts_with("answered")
        }
    }

    struct Gap {
        connects: Arc<AtomicUsize>,
        up: Arc<AtomicBool>,
        answers: Arc<AtomicBool>,
        demand: Arc<DialDemand>,
        state: watch::Receiver<ConnectionState>,
        sup: tokio::task::JoinHandle<Result<(), SupervisorError<TestChannelError>>>,
    }

    /// A production-paced supervisor against a nest that starts down.
    fn start_gap() -> Gap {
        let connects = Arc::new(AtomicUsize::new(0));
        let up = Arc::new(AtomicBool::new(false));
        let answers = Arc::new(AtomicBool::new(false));
        let demand = DialDemand::new();
        let channel = Arc::new(GapChannel {
            connects: Arc::clone(&connects),
            up: Arc::clone(&up),
            answers: Arc::clone(&answers),
            demand: Arc::clone(&demand),
            servers: Mutex::new(Vec::new()),
        });
        let (tx, state) = watch::channel(ConnectionState::Disconnected);
        let cfg = Supervisor::new(channel, Arc::new(RwLock::new(None)), tx);
        let sup = tokio::spawn(run_supervisor(cfg));
        Gap {
            connects,
            up,
            answers,
            demand,
            state,
            sup,
        }
    }

    /// **A request waiting out a gap is not left behind a grown nap.** Twenty
    /// virtual seconds of refused dials put the curve's ceiling at 16–32 s, far
    /// past a read's 5 s deadline; with a request parked the whole time, the
    /// supervisor must still be dialling at the initial pace, so the nest's
    /// return is found within the initial 1 s ceiling. A virtual clock and a
    /// state read (convention 14): deterministic, since every demand nap is at
    /// most that ceiling.
    #[tokio::test(start_paused = true)]
    async fn a_waiting_request_is_dialled_for_within_the_initial_ceiling() {
        let gap = start_gap();
        let _waiter = gap.demand.begin_wait();
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert_ne!(*gap.state.borrow(), ConnectionState::Connected);

        gap.up.store(true, Ordering::SeqCst);
        tokio::time::sleep(INITIAL_BACKOFF + Duration::from_millis(50)).await;
        let state = *gap.state.borrow();
        gap.sup.abort();
        assert_eq!(
            state,
            ConnectionState::Connected,
            "the nest came back while a request waited, yet the supervisor had not redialled \
             within the initial ceiling — the request is left to spend its deadline on the \
             grown nap ({} dials in all)",
            gap.connects.load(Ordering::SeqCst)
        );
    }

    /// **A nest that answers no is not a gap: waiting requests do not hurry the
    /// next dial.** Dial-on-demand is for catching a nest the moment it comes
    /// back; a nest refusing this client's credential (or rate-limiting it) is
    /// already there. With a request parked for ten virtual minutes against such
    /// a nest the supervisor must dial on its own curve — a few dozen dials at
    /// most — not once per initial ceiling (six hundred), which is what a dead
    /// credential with a request always waiting cost before.
    #[tokio::test(start_paused = true)]
    async fn an_answered_refusal_gets_no_demand_dials() {
        let gap = start_gap();
        gap.answers.store(true, Ordering::SeqCst);
        let _waiter = gap.demand.begin_wait();
        tokio::time::sleep(Duration::from_secs(600)).await;
        let dials = gap.connects.load(Ordering::SeqCst);
        gap.sup.abort();
        assert!(
            dials <= 60,
            "{dials} dials in ten minutes against a nest that answered every one with a \
             refusal — a waiting request is still hurrying the dials"
        );
    }

    /// **A request that starts waiting cuts a nap already under way short.** The
    /// supervisor is forty virtual seconds into an idle outage (ceiling 32–64 s)
    /// when the nest returns and a request parks: it must dial for that request
    /// within the initial ceiling, not when the idle nap happens to end.
    #[tokio::test(start_paused = true)]
    async fn a_request_that_starts_waiting_cuts_a_grown_nap_short() {
        let gap = start_gap();
        tokio::time::sleep(Duration::from_secs(40)).await;
        gap.up.store(true, Ordering::SeqCst);
        let _waiter = gap.demand.begin_wait();

        tokio::time::sleep(INITIAL_BACKOFF + Duration::from_millis(50)).await;
        let state = *gap.state.borrow();
        gap.sup.abort();
        assert_eq!(
            state,
            ConnectionState::Connected,
            "a request began waiting after the nest returned, yet no dial came within the \
             initial ceiling — the supervisor slept on in its idle nap"
        );
    }

    /// **The waiter's dials are extra, not the curve's.** Ten virtual seconds of
    /// a request parked against a down nest must produce more dials than the
    /// curve can (at least eleven, one per initial ceiling, where the curve's
    /// growing ceilings admit a handful), yet leave the indicator short of
    /// `Unreachable` (sized at about a minute of idle backoff) — and once nobody
    /// waits, the idle pace is back, its ceiling grown by the curve's own dials
    /// alone.
    #[tokio::test(start_paused = true)]
    async fn demand_dials_neither_grow_the_curve_nor_count_toward_unreachable() {
        let gap = start_gap();
        let waiter = gap.demand.begin_wait();
        tokio::time::sleep(Duration::from_secs(10)).await;
        let during = gap.connects.load(Ordering::SeqCst);
        let state = *gap.state.borrow();
        assert!(
            during >= 11,
            "a parked request must be dialled for at the initial pace: {during} dials in 10 s"
        );
        assert_ne!(
            state,
            ConnectionState::Unreachable,
            "{during} dials made for a waiting request tripped 'Cannot connect' in 10 s — the \
             waiter's dials were counted toward the unreachable run"
        );

        drop(waiter);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let after = gap.connects.load(Ordering::SeqCst) - during;
        gap.sup.abort();
        assert!(
            after <= 5,
            "with nobody waiting the idle curve must be back: {after} dials in the 10 s after \
             the wait ended"
        );
    }

    /// **Row 58 — an abandoned connection must be released, not accumulated.**
    ///
    /// The sibling defect to the backoff above, and the one that turned a fast
    /// retry loop into a machine-wide outage. The backoff sets the *rate*; this
    /// sets whether anything is ever given back. The incident's arithmetic
    /// separates them cleanly: ~16,300 connections over 16,200 s is ~1/s
    /// **accumulating**, with not one of the previous sockets released — both
    /// ends sat ESTABLISHED for four and a half hours. A bounded rate alone
    /// would still leak, only slower.
    ///
    /// Latency-independent by construction (convention 14): it asserts a
    /// *state* — the server end observing its client half hung up — never a
    /// duration. `tx_to_client.is_closed()` is the in-memory analogue of the
    /// socket leaving ESTABLISHED.
    #[tokio::test(start_paused = true)]
    async fn a_refused_connection_releases_its_socket() {
        let connects = Arc::new(AtomicUsize::new(0));
        let channel = Arc::new(OnConnectAlwaysFailsChannel {
            connects: Arc::clone(&connects),
            servers: Mutex::new(Vec::new()),
        });
        let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
        let (tx, _rx) = watch::channel(ConnectionState::Disconnected);

        let cfg = Supervisor {
            channel: Arc::clone(&channel),
            dispatcher_slot: slot,
            connection_state_tx: tx,
            initial_backoff: INITIAL_BACKOFF,
            max_backoff: MAX_BACKOFF,
        };
        let sup = tokio::spawn(run_supervisor(cfg));
        tokio::time::sleep(Duration::from_secs(300)).await;
        sup.abort();
        // Let the aborted supervisor's tasks actually unwind before reading the
        // ends: `JoinHandle::abort` schedules cancellation, it does not perform
        // it. This is a scheduler yield, not a settle-sleep — the assertion
        // below is on state, and no amount of extra time changes a leaked end.
        tokio::task::yield_now().await;

        let servers = channel.servers.lock().unwrap();
        let dials = connects.load(Ordering::SeqCst);
        assert!(dials >= 2, "test made only {dials} dials — nothing to leak");
        // Every connection but the one in flight at abort must have been
        // released. The newest is exempt: the supervisor may legitimately still
        // own it at the moment the test pulled the plug.
        let leaked = servers
            .iter()
            .take(servers.len().saturating_sub(1))
            .filter(|s| !s.tx_to_client.is_closed())
            .count();
        assert_eq!(
            leaked,
            0,
            "{leaked} of {} refused connections were never released — their client ends are still \
             open after the supervisor moved on. This is the accumulation half of the 2026-08-22 \
             incident: ~16,300 sockets held ESTABLISHED on both ends for 4.5 h because a \
             connection refused by `on_connect` is abandoned rather than closed",
            servers.len()
        );
    }

    /// **Measured, not assumed.** `watch::Sender::send` reports `Err` *and
    /// leaves the stored value untouched* when no receiver happens to be alive
    /// at that instant — so a state published into a momentarily receiverless
    /// channel is not merely unobserved, it is **lost**, and every later
    /// subscriber reads the stale initial value instead of the truth. This is
    /// the semantic the `report!` macro below is built on, so it is pinned
    /// here rather than trusted from memory.
    #[test]
    fn a_state_sent_with_no_receiver_alive_is_lost_to_later_subscribers() {
        let (tx, rx) = watch::channel(ConnectionState::Disconnected);
        drop(rx);
        assert!(
            tx.send(ConnectionState::Connecting).is_err(),
            "send into a receiverless watch is expected to report Err"
        );
        assert_eq!(
            *tx.subscribe().borrow(),
            ConnectionState::Disconnected,
            "the discarded send left the stale initial value behind"
        );

        // The remedy, measured on the same channel: `send_replace` stores
        // unconditionally, so the late subscriber reads the truth.
        tx.send_replace(ConnectionState::Connecting);
        assert_eq!(
            *tx.subscribe().borrow(),
            ConnectionState::Connecting,
            "send_replace must publish even with nobody subscribed"
        );
    }

    /// A supervisor is routinely started **before anyone subscribes** to its
    /// connection-state watch: `NestClient::with_registry` drops the channel's
    /// original receiver, and `connect()` subscribes only *after* the
    /// `tokio::spawn` that starts this loop. Whatever the supervisor publishes
    /// in that window must still be readable by the first subscriber to arrive.
    ///
    /// Before the `send_replace` fix it was not: a plain `watch::send` with no
    /// receiver alive is discarded, so both `Connecting` and `Connected` could
    /// vanish and a late subscriber read the channel's stale initial
    /// `Disconnected` — a client presenting "disconnected" with a live,
    /// serving connection behind it.
    ///
    /// The wait below deliberately polls `dispatcher_slot`, **not** the watch
    /// channel: subscribing in order to wait would itself create the receiver
    /// whose absence is the thing under test.
    #[tokio::test]
    async fn a_supervisor_started_with_no_subscriber_still_reports_the_truth() {
        let (adapter_ok, _server_ok) = mpsc_pair();
        let queue = Arc::new(Mutex::new(VecDeque::from([Ok(adapter_ok)])));
        let dispatcher_slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));

        // Exactly `NestClient::with_registry`'s shape: keep the sender, drop the
        // receiver, so the channel has none when the supervisor starts.
        let (tx, rx) = watch::channel(ConnectionState::Disconnected);
        drop(rx);

        let cfg = Supervisor {
            channel: Arc::new(QueueChannel::new(Arc::clone(&queue))),
            dispatcher_slot: Arc::clone(&dispatcher_slot),
            connection_state_tx: tx.clone(),
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        };
        let _sup = tokio::spawn(run_supervisor(cfg));

        // Generous ceiling, deadline poll, no settle-sleep: what is asserted is
        // that the connection *is* served, never that it is served quickly.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while dispatcher_slot.read().await.is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "supervisor never parked a dispatcher"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            *tx.subscribe().borrow(),
            ConnectionState::Connected,
            "a subscriber attaching after the supervisor served read a stale state"
        );
    }
}
