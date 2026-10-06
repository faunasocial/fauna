use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;

use fauna_core::identity::ActorKeypair;
use fauna_protocol::reconnect::Backoff;
use fauna_protocol::{KindRegistry, RpcDispatcher};

use crate::auth_client::AuthClient;
use crate::error::NestClientError;
use crate::push::PushBroker;
use crate::types::*;

/// The shared request path's transport-neutral classification
/// ([`fauna_protocol::TypedRequestError`]) rendered in this crate's public error
/// vocabulary. The dispatcher decides *what went wrong*; naming it is ours.
fn typed_request_error(e: fauna_protocol::TypedRequestError) -> NestClientError {
    use fauna_protocol::TypedRequestError as T;
    match e {
        T::Codec(msg) => NestClientError::Decode(msg),
        T::Dispatch(e) => NestClientError::WebSocket(format!("dispatch: {e}")),
        // Always in-flight here: the shared path is only reached once a frame
        // has been handed to the transport. The `was_in_flight:false` case is
        // `request_inner`'s own wait-for-reconnect step, which never sends.
        T::Disconnected => NestClientError::RpcDisconnected {
            was_in_flight: true,
        },
        T::Rpc(e) => NestClientError::Rpc(*e),
        T::Timeout => NestClientError::RpcTimeout,
    }
}

/// Background reconnect-supervisor task spawned by [`NestClient::connect`].
/// `Ok(())` on a clean disconnect; `Err` on a terminal connect refusal, a 4426
/// subprotocol mismatch or a failed bearer refresh after 4401 (see
/// [`fauna_ws_substrate::SupervisorError`]). Either way the task records why
/// ([`NestClient::supervisor_stop`]) before it returns.
type SupervisorJoinHandle =
    JoinHandle<Result<(), fauna_ws_substrate::SupervisorError<NestClientError>>>;

/// Why a client's reconnect supervisor stopped for good, kept in a form every
/// later request can be failed with. `NestClientError` itself is not `Clone`
/// (its `Http` arm wraps a `reqwest::Error`), and each request needs its own.
#[derive(Debug, Clone)]
enum SupervisorStop {
    /// A clean disconnect (close 1000): nothing to name beyond "no connection
    /// will come", the answer a torn-down client gives too.
    Clean,
    /// The nest's pinned identity changed (`security.md` § Post-auth
    /// surfacing).
    IdentityChanged {
        host: String,
        pinned_hex: String,
        seen_hex: Option<String>,
    },
    /// The nest rejected this client's subprotocol: a version skew.
    SubprotocolMismatch,
    /// A wire refusal no retry clears, e.g. this identity was succeeded
    /// (`fauna.auth.superseded`, read off the bearer mint's latch).
    Refused(fauna_protocol::RpcError),
    /// A bearer refresh after 4401 that could not obtain a token, by its cause.
    AuthFailed(String),
}

impl SupervisorStop {
    /// Name how the supervisor ended. A superseded identity is read off
    /// `auth`'s latch first: by the time that refusal crosses the bearer seam
    /// it is a flattened error (`reconnect.rs`'s `connect_error_is_terminal`,
    /// case 1).
    ///
    /// A post-4401 refresh the nest *refused* is read off the mint's
    /// last-refusal channel for the same reason — above all
    /// `fauna.auth.not_registered`, a user suspended or removed mid-session,
    /// which the apps route to the launch surface's previously-signed-in row
    /// (`onboarding.md` § App-launch routing). The channel is overwritten per
    /// mint, so on this arm it names the refresh that just failed. A
    /// caller-supplied bearer has no such channel; `LaunchMachineBearer`
    /// carries the verdict typed instead, arriving here as `Rpc`.
    fn of(
        ended: &Result<(), fauna_ws_substrate::SupervisorError<NestClientError>>,
        auth: &AuthClient,
    ) -> Self {
        use fauna_ws_substrate::SupervisorError as S;
        let (e, refresh) = match ended {
            Ok(()) => return Self::Clean,
            Err(S::SubprotocolMismatch) => return Self::SubprotocolMismatch,
            Err(S::TerminalRefusal(e)) => (e, false),
            Err(S::AuthRefresh(e)) => (e, true),
        };
        if let Some(refusal) = auth.superseded_refusal() {
            return Self::Refused(refusal);
        }
        if refresh
            && !matches!(e, NestClientError::NestIdentityChanged { .. })
            && let Some(refusal) = auth.last_auth_refusal()
        {
            return Self::Refused(refusal);
        }
        match e {
            NestClientError::NestIdentityChanged {
                host,
                pinned_hex,
                seen_hex,
            } => Self::IdentityChanged {
                host: host.clone(),
                pinned_hex: pinned_hex.clone(),
                seen_hex: seen_hex.clone(),
            },
            NestClientError::SubprotocolMismatch => Self::SubprotocolMismatch,
            NestClientError::Rpc(refusal) => Self::Refused(refusal.clone()),
            NestClientError::Auth(msg) => Self::AuthFailed(msg.clone()),
            other => Self::AuthFailed(other.to_string()),
        }
    }

    /// The error a request on the stopped client fails with.
    fn error(&self) -> NestClientError {
        match self {
            Self::Clean => NestClientError::RpcDisconnected {
                was_in_flight: false,
            },
            Self::IdentityChanged {
                host,
                pinned_hex,
                seen_hex,
            } => NestClientError::NestIdentityChanged {
                host: host.clone(),
                pinned_hex: pinned_hex.clone(),
                seen_hex: seen_hex.clone(),
            },
            Self::SubprotocolMismatch => NestClientError::SubprotocolMismatch,
            Self::Refused(refusal) => NestClientError::Rpc(refusal.clone()),
            Self::AuthFailed(msg) => NestClientError::Auth(msg.clone()),
        }
    }
}

/// Record why the supervisor stopped, then wake every request parked for a
/// reconnect so it reads the record: stored first, announced second, the order
/// `NestClient::disconnect` marks `torn_down` in. `send_replace` notifies even
/// when the state already reads `Disconnected`.
fn record_supervisor_stop(
    slot: &std::sync::Mutex<Option<SupervisorStop>>,
    connection_state_tx: &watch::Sender<ConnectionState>,
    stop: SupervisorStop,
) {
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(stop);
    connection_state_tx.send_replace(ConnectionState::Disconnected);
}

pub struct NestClient {
    auth: Arc<AuthClient>,
    pushes: Arc<PushBroker>,
    /// Current dispatcher; set by the reconnect supervisor on connect,
    /// cleared on each WS termination. `request*()` methods read this.
    dispatcher: Arc<RwLock<Option<Arc<RpcDispatcher>>>>,
    kind_registry: Arc<KindRegistry>,
    connection_state_tx: watch::Sender<ConnectionState>,
    /// Reconnect counter, bumped on every reconnect (a `Connected` transition
    /// AFTER the first connect). Clients watch this (`subscribe_reconnects`) to
    /// re-pull their visible snapshot surfaces — the transport.md § Push events
    /// contract that observers re-pull on reconnect. Derived from
    /// `connection_state` by a task spawned in `connect`.
    reconnect_tx: watch::Sender<u64>,
    /// Handle to the supervisor task spawned by `connect`. `disconnect`
    /// aborts it.
    supervisor_handle: Mutex<Option<SupervisorJoinHandle>>,
    /// Handle to the reconnect-derive task spawned by `connect`. `disconnect`
    /// aborts it.
    reconnect_derive_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Set by [`Self::disconnect`], cleared by [`Self::connect`]: the
    /// supervisor is gone and nothing will reconnect, so a request must fail
    /// at once instead of waiting out its deadline for a connection that
    /// cannot come. `connection_state` alone cannot say this — the supervisor
    /// passes through `Disconnected` between dial attempts too. Until
    /// 2026-08-27 every RPC issued after a sign-out's `disconnect()` parked for
    /// its full deadline (30 s by default), and on tui that parked the
    /// outgoing session's launch — and its MLS store lock — across an account
    /// switch (`account-scoping.md` § Implementation status → the
    /// `tui (in-memory)` ledger row).
    torn_down: std::sync::atomic::AtomicBool,
    /// Why the reconnect supervisor stopped for good, if it has: recorded when
    /// its task returns, cleared by the next [`Self::connect`]. Like
    /// `torn_down` it means no connection will come, so a request fails at
    /// once, here with the reason instead of a bare disconnect after its whole
    /// deadline (`transport-connection.md` § Connection lifecycle).
    stopped_by: Arc<std::sync::Mutex<Option<SupervisorStop>>>,
    /// The reconnect-pace override every supervisor this client spawns reads
    /// (`crate::reconnect::BackoffOverride`); written only by the e2e seam
    /// [`Self::set_reconnect_backoff_for_test`].
    backoff_override: crate::reconnect::BackoffOverride,
    /// The requests parked in [`Self::wait_for_connected`], shared with every
    /// supervisor this client spawns so a refused dial is retried promptly
    /// while one waits ([`fauna_ws_substrate::DialDemand`]).
    dial_demand: Arc<fauna_ws_substrate::DialDemand>,
    /// The push device id every connection announces (`fauna.push.presence`),
    /// shared with each supervisor's channel; `None` until
    /// [`Self::set_push_presence`] names one.
    push_presence: crate::reconnect::PushPresence,
}

/// **No dialer outlives its client.** The supervisor task holds the channel,
/// never the `NestClient`, so the last handle to a client can drop while its
/// supervisor keeps dialling — and dropping a `JoinHandle` does not abort the
/// task. Every holder that let go of a client without calling `disconnect()`
/// (a cache invalidated after a failed request, a mount whose assembly failed
/// half way) therefore left one live reconnect loop behind, and a process that
/// did so once a minute ran thousands of them against one nest. That is the
/// 2026-09-24 sync-agent flood: a dead credential, and a new stranded loop per
/// failed read. Binding the loop's life to its owner makes the stranded shape
/// unrepresentable instead of a rule every holder must remember
/// (`transport-connection.md` § Connection lifecycle). Pinned by
/// `client::tests::dropping_a_client_stops_its_dialling`.
impl Drop for NestClient {
    fn drop(&mut self) {
        if let Some(h) = self.supervisor_handle.get_mut().take() {
            h.abort();
        }
        if let Some(h) = self.reconnect_derive_handle.get_mut().take() {
            h.abort();
        }
    }
}

impl NestClient {
    /// Create with a new AuthClient and the default protocol-kind registry.
    /// Feature crates extending the kind set should construct via
    /// `with_registry(...)`.
    pub fn new(nest_url: String, keypair: ActorKeypair) -> Arc<Self> {
        let auth = Arc::new(AuthClient::new(nest_url, keypair));
        Self::with_auth(auth)
    }

    /// Create with a shared AuthClient; uses the default protocol-kind
    /// registry (`fauna.protocol.echo` only).
    pub fn with_auth(auth: Arc<AuthClient>) -> Arc<Self> {
        Self::with_registry(auth, Arc::new(KindRegistry::full()))
    }

    /// Create with a shared AuthClient and a feature-extended kind
    /// registry. Feature crates (e.g. fauna-client-bridges) should pass
    /// a registry that includes their kinds so `request_auto_retry`
    /// honours the per-kind metadata (forbid_replay, default_deadline).
    pub fn with_registry(auth: Arc<AuthClient>, kind_registry: Arc<KindRegistry>) -> Arc<Self> {
        let pushes = PushBroker::new(256);
        let dispatcher = Arc::new(RwLock::new(None));
        let (connection_state_tx, _) = watch::channel(ConnectionState::Disconnected);
        let (reconnect_tx, _) = watch::channel(0u64);

        Arc::new(Self {
            auth,
            pushes,
            dispatcher,
            kind_registry,
            connection_state_tx,
            reconnect_tx,
            supervisor_handle: Mutex::new(None),
            reconnect_derive_handle: Mutex::new(None),
            torn_down: std::sync::atomic::AtomicBool::new(false),
            stopped_by: Arc::new(std::sync::Mutex::new(None)),
            backoff_override: Default::default(),
            dial_demand: fauna_ws_substrate::DialDemand::new(),
            push_presence: Default::default(),
        })
    }

    /// Pace this client's reconnect retries at `bounds` (`(initial, max)`)
    /// instead of the production 1 s → 60 s, or restore them with `None` — the
    /// e2e seam behind `fauna_e2e_agent::RECONNECT_BACKOFF`.
    ///
    /// What it is for: the "a connection that keeps failing reads Cannot
    /// connect, until one succeeds" journey (`transport-connection.md`
    /// § `Unreachable`) needs the supervisor's real threshold of consecutive
    /// failures, which at production pace costs a minute or two of wall clock —
    /// a wait convention 14 forbids a test to spend. Only the pace moves: the
    /// threshold, the collapse to `Unreachable` and its stickiness are the
    /// production code (`SupervisedChannel::backoff_override`). Takes effect on
    /// the running supervisor's next backed-off failure; no reconnect needed.
    ///
    /// Compiled out of release artifacts (convention 15 rule (a)): debug builds
    /// reach it through `debug_assertions`, a release-profile e2e build through
    /// the `e2e-agent` feature.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    pub fn set_reconnect_backoff_for_test(
        &self,
        bounds: Option<(std::time::Duration, std::time::Duration)>,
    ) {
        *self
            .backoff_override
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = bounds;
    }

    /// Get the underlying AuthClient (for sharing with SyncClient etc.).
    pub fn auth(&self) -> &Arc<AuthClient> {
        &self.auth
    }

    /// The nest base URL (current value — swapped on an SRV-reconnect
    /// serving-port change). Owned `String` because it lives behind a lock.
    pub fn nest_url(&self) -> String {
        self.auth.nest_url()
    }

    pub fn actor_id_hex(&self) -> String {
        self.auth.actor_id_hex()
    }

    /// Watch connection state changes. Updates from the supervisor.
    pub fn connection_state(&self) -> watch::Receiver<ConnectionState> {
        self.connection_state_tx.subscribe()
    }

    /// Why this client's reconnect supervisor stopped for good, if it has: the
    /// error every request on it now fails with at once. `None` while a
    /// supervisor runs, and again after the next [`Self::connect`], which
    /// starts a fresh one — so a caller retrying across a stop a nest update
    /// may clear (a version skew) reconnects on seeing `Some`.
    pub fn supervisor_stop(&self) -> Option<NestClientError> {
        self.stopped_by
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(SupervisorStop::error)
    }

    /// Watch reconnects. The returned receiver's value increments on every
    /// reconnect — a `Connected` transition AFTER the first connect. The initial
    /// connect does NOT bump it (the app loads its surfaces then anyway), so a
    /// `changed()` here is always a genuine reconnect. Clients `await
    /// rx.changed()` and re-pull their visible snapshot surfaces: the transport.md
    /// § Push events contract that "application observers re-pull through their
    /// snapshot-refresh path" on reconnect. Without this the feed — which has no
    /// poll backstop — stays stale after a reconnect until a manual refresh.
    pub fn subscribe_reconnects(&self) -> watch::Receiver<u64> {
        self.reconnect_tx.subscribe()
    }

    /// Test-only: the dispatcher slot and connection-state sender this
    /// client's `request*` methods observe. A reconnect test drives
    /// `fauna_ws_substrate::run_supervisor` against these so it can exercise the
    /// `request_inner` wait-for-reconnect path without a real WS. Not part
    /// of the public API.
    ///
    /// **Compiled out of release artifacts** (convention 15 rule (a),
    /// `e2e-automation-surface-gating.md` § The convention): `#[doc(hidden)]`
    /// alone only hides it from rendered docs — it is not a compile-time
    /// gate, so this was reachable from a plain release build with no
    /// feature opt-in at all until this fix. The
    /// `test-util` arm (dev-dependency-only, like the `testing` module it
    /// sits beside) lets a dependent crate's `--release` test build reach it.
    #[doc(hidden)]
    #[allow(clippy::type_complexity)] // mirrors the substrate Supervisor's own slot type
    #[cfg(any(test, debug_assertions, feature = "test-util"))]
    pub fn supervisor_channels_for_test(
        &self,
    ) -> (
        Arc<RwLock<Option<Arc<RpcDispatcher>>>>,
        watch::Sender<ConnectionState>,
    ) {
        (
            Arc::clone(&self.dispatcher),
            self.connection_state_tx.clone(),
        )
    }

    /// Name the push device this client's connections serve: from now on every
    /// (re)connect announces `fauna.push.presence { device_id }`, and a live
    /// connection announces it at once (`common.md` § Registration → *Every
    /// connection announces*). `device_id` must be the id this install's push
    /// row is keyed under — the install's derived device id for this actor
    /// (`fauna_core::device_id::derive_device_id`), the one the sync agent
    /// presents as `SyncCapability::device_id` — or the nest reads the app's
    /// own row as absent and dials it while the app is open. Callers that
    /// never call this announce nothing, which keeps the pre-announce
    /// behaviour.
    pub async fn set_push_presence(&self, device_id: impl Into<String>) {
        let device_id = device_id.into();
        *self
            .push_presence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(device_id.clone());
        // A connection already up announced nothing (or an older id) at its
        // connect; tell it now. One that comes up later reads the slot itself.
        let live = self.dispatcher.read().await.as_ref().map(Arc::clone);
        if let Some(dispatcher) = live {
            tokio::spawn(crate::reconnect::announce_presence(dispatcher, device_id));
        }
    }

    // ── Push events (new in Y.3) ──

    /// Subscribe to all typed push events. Survives reconnects (the broker
    /// is long-lived). Returns a `tokio::broadcast::Receiver`; use
    /// `recv().await` in a loop.
    pub fn subscribe_pushes(&self) -> tokio::sync::broadcast::Receiver<fauna_protocol::PushEvent> {
        self.pushes.subscribe()
    }

    /// Subscribe to one specific push kind. Filtering happens broker-side.
    /// For many kind-filtered consumers, prefer `subscribe_pushes` once
    /// and match on `PushEvent::kind()` in your consumer.
    pub fn subscribe_kind(&self, kind: &'static str) -> crate::push::KindSubscriber {
        self.pushes.subscribe_kind(kind)
    }

    // ── Auth (delegated; unchanged) ──

    pub async fn authenticate(&self) -> Result<(), NestClientError> {
        self.auth.authenticate().await
    }

    pub async fn ensure_auth(&self) -> Result<String, NestClientError> {
        self.auth.ensure_auth().await
    }

    /// Authenticate and start the reconnect supervisor. The supervisor
    /// runs in a background task until `disconnect()` is called or it stops
    /// for good: a clean close, a refusal no retry clears (a changed nest
    /// identity, a superseded identity, a subprotocol mismatch) or a failed
    /// bearer refresh after 4401. A stop is recorded, and every request then
    /// fails at once with it ([`Self::supervisor_stop`]). An `Err` here is
    /// therefore only ever the authentication step; what the supervisor meets
    /// later arrives on requests.
    ///
    /// On a successful return, the WS handshake has been initiated but
    /// not necessarily completed; observe `connection_state()` to wait
    /// for `ConnectionState::Connected`.
    pub async fn connect(self: &Arc<Self>) -> Result<(), NestClientError> {
        self.auth.authenticate().await?;
        // A client may be connected again after a `disconnect()`; from here on
        // a request waits for the supervisor spawned below, as it should.
        self.torn_down
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // The client's half of the shared reconnect supervisor: bearer-subprotocol
        // connect + push bridge + bearer refresh (see `reconnect::ClientChannel`).
        let channel = Arc::new(crate::reconnect::ClientChannel {
            auth: Arc::clone(&self.auth),
            pushes: Arc::clone(&self.pushes),
            kind_registry: Arc::clone(&self.kind_registry),
            backoff_override: Arc::clone(&self.backoff_override),
            dial_demand: Arc::clone(&self.dial_demand),
            push_presence: Arc::clone(&self.push_presence),
        });
        let cfg = fauna_ws_substrate::Supervisor::new(
            channel,
            Arc::clone(&self.dispatcher),
            self.connection_state_tx.clone(),
        );

        let mut handle = self.supervisor_handle.lock().await;
        // Retire the supervisor this call replaces. Assigning over the slot only
        // drops its `JoinHandle`, and **dropping a `JoinHandle` does not abort
        // its task** — the stranded supervisor keeps its connection and keeps
        // dialling forever, and nothing holds a handle to stop it any more, so
        // even `disconnect()` can only reach the newest. That is how the
        // 2026-08-22 mac incident accumulated ~16,300 sockets held ESTABLISHED
        // on both ends. `transport.md` § Connection lifecycle records the same
        // class for `nest_link`'s proxy. Pinned by
        // `connect_twice_does_not_strand_the_first_supervisor`.
        if let Some(previous) = handle.take() {
            previous.abort();
            // Awaited, so a stop the retired supervisor was recording as it was
            // aborted lands before the clear below, never after it.
            let _ = previous.await;
        }
        // A fresh supervisor: whatever stopped the last one no longer answers
        // for this client's requests.
        *self.stopped_by.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let stopped_by = Arc::clone(&self.stopped_by);
        let connection_state_tx = self.connection_state_tx.clone();
        let auth = Arc::clone(&self.auth);
        *handle = Some(tokio::spawn(async move {
            let ended = fauna_ws_substrate::run_supervisor(cfg).await;
            record_supervisor_stop(
                &stopped_by,
                &connection_state_tx,
                SupervisorStop::of(&ended, &auth),
            );
            ended
        }));

        // Derive the reconnect signal from `connection_state`: bump `reconnect_tx`
        // on every `Connected` after the first, so clients re-pull their snapshot
        // surfaces uniformly (see `subscribe_reconnects`). Kept here rather than in
        // the shared supervisor so the federation channel — which has no UI to
        // re-hydrate — is unaffected (#2, divergence-minimal).
        let mut cs_rx = self.connection_state_tx.subscribe();
        let reconnect_tx = self.reconnect_tx.clone();
        let derive = tokio::spawn(async move {
            let mut ever_connected =
                matches!(*cs_rx.borrow_and_update(), ConnectionState::Connected);
            while cs_rx.changed().await.is_ok() {
                if matches!(*cs_rx.borrow_and_update(), ConnectionState::Connected) {
                    if ever_connected {
                        reconnect_tx.send_modify(|n| *n = n.wrapping_add(1));
                    } else {
                        ever_connected = true;
                    }
                }
            }
        });
        // Same retirement as the supervisor above — this slot was assigned over
        // too, stranding one reconnect-derive task per extra `connect()`. Each
        // survivor keeps bumping `reconnect_tx` off the same `connection_state`
        // watch, so every app observing `subscribe_reconnects` would re-pull its
        // snapshot surfaces N times per reconnect.
        let mut derive_handle = self.reconnect_derive_handle.lock().await;
        if let Some(previous) = derive_handle.take() {
            previous.abort();
        }
        *derive_handle = Some(derive);

        Ok(())
    }

    /// [`Self::connect`] unless a supervisor is already running — the idempotent
    /// form, for a component that needs the connection up but does not own it
    /// (an engine whose builder already connected its control plane). A running
    /// supervisor re-dials on its own, so a second `connect` would only tear a
    /// live connection down and re-authenticate. One that stopped for good, or
    /// was never started, is started.
    pub async fn ensure_connected(self: &Arc<Self>) -> Result<(), NestClientError> {
        if self
            .supervisor_handle
            .lock()
            .await
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return Ok(());
        }
        self.connect().await
    }

    /// Stop the reconnect supervisor. Pending RPCs see `RpcDisconnected`
    /// once the dispatcher is dropped. Auth token is *not* cleared
    /// (AuthClient may be shared).
    pub async fn disconnect(&self) {
        let mut handle = self.supervisor_handle.lock().await;
        if let Some(h) = handle.take() {
            h.abort();
        }
        if let Some(h) = self.reconnect_derive_handle.lock().await.take() {
            h.abort();
        }
        self.dispatcher.write().await.take();
        // Marked BEFORE the state change below, so a request already parked in
        // `wait_for_connected` wakes on that change and reads the mark: it
        // fails now, not at its deadline (`torn_down`'s own doc).
        self.torn_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // `send_replace`, never `send`: the two `abort()`s above have just
        // retired the reconnect-derive task, which holds the only receiver this
        // client keeps alive on its own behalf (the channel's original one is
        // dropped in `with_registry`). A plain `send` with nobody else
        // subscribed reports `Err` and **leaves the stored value alone**, so the
        // next subscriber — an app re-reading the connection-status indicator —
        // would read a stale `Connected` on a client that has been torn down.
        self.connection_state_tx
            .send_replace(ConnectionState::Disconnected);
    }

    // ── RPC (new in Y.3) ──

    /// Send an RPC request, await the reply. Uses the kind's registered
    /// `default_deadline` and a fresh random `idempotency_key`. Does NOT
    /// auto-retry on disconnect — the caller decides via either
    /// `request_with_key` (replay-via-cache) or `request_auto_retry`
    /// (convenience).
    pub async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let mut idem = [0u8; 16];
        getrandom::fill(&mut idem)
            .map_err(|e| NestClientError::Decode(format!("idempotency_key: {e}")))?;

        self.request_inner::<Req, Reply>(kind, idem, payload, None)
            .await
    }

    async fn request_inner<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
        deadline_override: Option<std::time::Duration>,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        // Look up per-kind metadata for the default deadline. Unknown
        // kinds get the spec § 1.4 default of 30 s.
        let deadline = deadline_override
            .or_else(|| self.kind_registry.meta(kind).map(|m| m.default_deadline))
            .unwrap_or(std::time::Duration::from_secs(30));

        // CBOR-encode the typed Req → canonical bytes → Value (RpcDispatcher
        // wants the generic dag-cbor node). Deliberately *before* the
        // wait-for-reconnect step below, so no caller-typed value is held
        // across that await.
        let payload_value =
            fauna_protocol::encode_payload(&payload).map_err(typed_request_error)?;

        // The whole logical request — the wait for the reconnect supervisor to
        // re-establish the WS, the enqueue onto the dispatcher's bounded
        // outbound queue, and the reply wait — is bounded by `deadline`. The
        // middle term is the one that had to be earned: a peer that stopped
        // reading its socket used to fill that queue and park the enqueue with
        // no timer covering it, upstream of the backstop this budget becomes
        // (`RpcDispatcher::request_raw_bounded`).
        let overall_deadline = tokio::time::Instant::now() + deadline;

        // Snapshot the dispatcher pointer. If it's `None` the supervisor is
        // mid-reconnect (a transient idle-drop), so wait for it to come back
        // rather than failing fast with a spurious `RpcDisconnected` the UI
        // would surface as an error banner. `was_in_flight:false` means
        // nothing was sent on the wire, so re-snapshotting and sending fresh
        // is safe for *every* kind — including `forbid_replay` ones — because
        // it cannot double-apply anything. (The `was_in_flight:true`
        // sent-then-dropped case is left untouched: it still surfaces, and
        // `request_auto_retry` handles replay-via-idempotency-key for it.)
        let dispatcher = loop {
            {
                let guard = self.dispatcher.read().await;
                if let Some(d) = guard.as_ref() {
                    break Arc::clone(d);
                }
            }
            // Disconnected — wait for the supervisor to reconnect, bounded by
            // the remaining deadline budget; on elapse, fail as before. A client
            // torn down by `disconnect()`, or whose supervisor stopped for good,
            // has nothing to wait for and fails here at once
            // (`wait_for_connected`'s `Err`).
            let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(NestClientError::RpcDisconnected {
                    was_in_flight: false,
                });
            }
            match tokio::time::timeout(remaining, self.wait_for_connected()).await {
                Ok(Ok(())) => {}
                Ok(Err(no_connection_can_come)) => return Err(no_connection_can_come),
                Err(_elapsed) => {
                    return Err(NestClientError::RpcDisconnected {
                        was_in_flight: false,
                    });
                }
            }
            // Reconnected (the supervisor populates the slot before flipping
            // state to Connected) — re-snapshot via the loop.
        };

        let remaining = overall_deadline.saturating_duration_since(tokio::time::Instant::now());
        // Steps 4-5 of `transport.md` § Request lifecycle — send, await bounded
        // by the *remaining* budget (the server enforces its own deadline too;
        // ours is the backstop for a server that hangs), decode — are the
        // ceremony every fauna transport shares, so they live once on the
        // dispatcher. `tokio::time::sleep` is this runtime's half of the
        // deadline backstop; the wasm client passes a `TimeoutFuture` instead.
        dispatcher
            .request_encoded(
                kind,
                idempotency_key,
                payload_value,
                remaining,
                tokio::time::sleep(remaining),
            )
            .await
            .map_err(typed_request_error)
    }

    /// Send an RPC request with a caller-supplied idempotency_key. Used
    /// for explicit re-issue after `RpcDisconnected{was_in_flight:true}` —
    /// the server's idempotency cache replays the prior reply if it had
    /// succeeded.
    pub async fn request_with_key<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.request_inner::<Req, Reply>(kind, idempotency_key, payload, None)
            .await
    }

    /// Send an RPC request with a per-call deadline override (replaces
    /// the kind's default_deadline).
    pub async fn request_with_deadline<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
        deadline: std::time::Duration,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let mut idem = [0u8; 16];
        getrandom::fill(&mut idem)
            .map_err(|e| NestClientError::Decode(format!("idempotency_key: {e}")))?;
        self.request_inner::<Req, Reply>(kind, idem, payload, Some(deadline))
            .await
    }

    /// Convenience: auto-retry on `RpcDisconnected{was_in_flight:true}`
    /// using the same idempotency_key. Bounded to 3 attempts with exponential
    /// backoff between attempts.
    ///
    /// ⚠️ The retry waits for the reconnect (see `wait_for_connected` below),
    /// and the nest's idempotency cache is per-`RpcConnection` — so the
    /// re-issue lands on a fresh connection with an empty cache and the handler
    /// **runs again for real**. Using this method is therefore an assertion
    /// that the kind's handler is naturally idempotent; kinds whose handlers
    /// are not must be registered `forbid_replay = true`, which turns this
    /// method back into plain `request`. See `transport.md` § Idempotency and
    /// reconnect-with-resume.
    ///
    /// **Honours `forbid_replay`:** if the kind is registered with
    /// `forbid_replay=true`, this method behaves identically to
    /// `request` — no auto-retry — and surfaces the disconnect to the
    /// caller for explicit decision.
    pub async fn request_auto_retry<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize + Clone,
        Reply: serde::de::DeserializeOwned,
    {
        let forbid_replay = self
            .kind_registry
            .meta(kind)
            .map(|m| m.forbid_replay)
            .unwrap_or(false);

        let mut idem = [0u8; 16];
        getrandom::fill(&mut idem)
            .map_err(|e| NestClientError::Decode(format!("idempotency_key: {e}")))?;

        let mut attempt: u32 = 0;
        let mut backoff = Backoff::new(
            std::time::Duration::from_millis(200),
            std::time::Duration::from_secs(2),
        );

        loop {
            let result = self
                .request_inner::<Req, Reply>(kind, idem, payload.clone(), None)
                .await;

            match result {
                Ok(r) => return Ok(r),
                Err(NestClientError::RpcDisconnected { was_in_flight }) if !forbid_replay => {
                    if attempt >= 2 {
                        return Err(NestClientError::RpcDisconnected { was_in_flight });
                    }
                    // Wait until reconnected before re-issuing — unless no
                    // reconnect can come: torn down meanwhile, when the
                    // disconnect is the answer, or the supervisor stopped for
                    // good, when its reason is.
                    if let Err(e) = self.wait_for_connected().await {
                        return Err(match e {
                            NestClientError::RpcDisconnected { .. } => {
                                NestClientError::RpcDisconnected { was_in_flight }
                            }
                            stopped => stopped,
                        });
                    }
                    tokio::time::sleep(backoff.ceiling()).await;
                    backoff.grow();
                    attempt += 1;
                }
                Err(other) => return Err(other),
            }
        }
    }

    /// Park until the supervisor reports `Connected` — `Ok` — or, at once,
    /// until no connection can come: the client was torn down by
    /// [`Self::disconnect`] (`RpcDisconnected { was_in_flight: false }`), or its
    /// supervisor stopped for good (why, [`Self::supervisor_stop`]). The caller
    /// bounds the wait by the request's remaining deadline.
    ///
    /// For as long as it parks, the wait counts on the supervisor's
    /// [`DialDemand`](fauna_ws_substrate::DialDemand): a refused dial is then
    /// retried within the initial ceiling instead of after the grown backoff,
    /// which after a few seconds' gap is already longer than a read's whole
    /// deadline (`transport-connection.md` § Connection lifecycle).
    async fn wait_for_connected(&self) -> Result<(), NestClientError> {
        let _waiting = self.dial_demand.begin_wait();
        let mut rx = self.connection_state_tx.subscribe();
        loop {
            if self.torn_down.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(NestClientError::RpcDisconnected {
                    was_in_flight: false,
                });
            }
            if let Some(stopped) = self.supervisor_stop() {
                return Err(stopped);
            }
            if *rx.borrow_and_update() == ConnectionState::Connected {
                return Ok(());
            }
            if rx.changed().await.is_err() {
                return Err(NestClientError::RpcDisconnected {
                    was_in_flight: false,
                });
            }
        }
    }

    // Legacy HTTP read methods (`node_info` / `check_handle` / `get_inbox` /
    // `get_account` / `get_groups` / `get_knocks` / `get_contacts`) and their
    // `parse_response` helper were deleted in the WS-RPC-everywhere rip: their
    // HTTP routes are gone nest-side, and every live caller reads the
    // successor kinds (`fauna.nest.info`, `fauna.handle.available`,
    // `fauna.inbox.fetch`, `fauna.account.get`, `fauna.knocks.list`,
    // `fauna.contacts.list`; `get_groups`' successor, `fauna.conversations.group.*`,
    // was itself retired with the group plane) via the per-feature client
    // crates (`fauna-anon-client`, `fauna-client-inbox`, `fauna-client-account`,
    // `fauna-client-contacts`, `fauna-client-conversations`) over the inherent
    // `request()` / `RpcRequester` impl below. `NestClient` now carries no HTTP
    // surface of its own — bulk-binary HTTP (blob/segment) lives in `AuthClient`.
}

/// The native arm of the shared `RpcRequester` seam (see
/// `fauna_protocol::requester`). The per-feature wrapper crates
/// (`fauna-client-bridges`, `fauna-client-email`) are generic over
/// `R: RpcRequester` and consume this impl on native; the wasm `WsRpcClient`
/// provides the other arm.
impl fauna_protocol::RpcRequester for NestClient {
    type Error = NestClientError;

    /// Delegates to the inherent [`NestClient::request`] — fully qualified so
    /// the path resolves to the inherent method, not back into this trait
    /// method. The future is `Send` (the payload is encoded before the first
    /// await), so native callers can `tokio::spawn` feature-crate calls.
    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        NestClient::request(self, kind, payload).await
    }
}

/// A [`NestClient`] whose holder is its only owner — nothing else ever calls
/// [`NestClient::connect`] on it — as a requester: each request brings the
/// supervisor up first ([`NestClient::ensure_connected`], a no-op once one
/// runs), on the runtime the request runs on. For a component that builds its
/// own connection from an `AuthClient` and drives it from a runtime of its own
/// (a capability host's throwaway fleet replica).
pub struct SelfConnecting(pub Arc<NestClient>);

impl fauna_protocol::RpcRequester for SelfConnecting {
    type Error = NestClientError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        self.0.ensure_connected().await?;
        NestClient::request(&self.0, kind, payload).await
    }
}

/// The native arm of the outbox-drain seam: the envelope carries the caller's
/// key (the outbox intent id), so a replayed drain re-presents the same
/// logical request. Delegates to the inherent [`NestClient::request_with_key`].
impl fauna_protocol::KeyedRpcRequester for NestClient {
    async fn request_keyed<Req, Reply>(
        &self,
        kind: &'static str,
        idempotency_key: [u8; 16],
        payload: Req,
    ) -> Result<Reply, NestClientError>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        NestClient::request_with_key(self, kind, idempotency_key, payload).await
    }
}

/// The native arm of the own-session seam (`fauna_protocol::auth::
/// OwnSessionSource`): answered by this client's one bearer source — the same
/// holder whose token upgraded the socket the sessions kinds ride, so the row
/// marked "This app" is the session this client is actually using
/// (`docs/goal/behavior/devices.md` § The client's own session).
impl fauna_protocol::auth::OwnSessionSource for NestClient {
    async fn own_token_ids(&self) -> Vec<String> {
        self.auth.bearer().own_token_ids().await
    }
    async fn current_token_id(&self) -> Option<String> {
        self.auth.bearer().current_token_id().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_nest_http::StaticBearer;

    /// A client whose auth needs no network (`StaticBearer`) pointed at a port
    /// nothing listens on, so `connect()` always succeeds at the auth step and
    /// the supervisor it spawns stays in its dial-retry loop forever — which is
    /// exactly the state in which a second `connect()` must not strand it.
    fn unreachable_bearer_only_client() -> Arc<NestClient> {
        let bearer: Arc<dyn fauna_nest_http::BearerSource> = Arc::new(StaticBearer("tok".into()));
        let auth = Arc::new(AuthClient::bearer_only(
            "http://127.0.0.1:1".into(),
            [0x5au8; 32],
            bearer,
            reqwest::Client::new(),
        ));
        NestClient::with_auth(auth)
    }

    /// Poll a state predicate across scheduler passes. `JoinHandle::abort`
    /// *schedules* cancellation, so a task's `Drop` — and the `Arc` release the
    /// assertion reads — lands a pass or two later. This is a bounded poll on
    /// state, never a settle-sleep: a genuinely leaked `Arc` is never released
    /// no matter how many passes it is given (convention 14).
    async fn yield_until(mut done: impl FnMut() -> bool) {
        for _ in 0..1_000 {
            if done() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    #[derive(serde::Serialize)]
    struct NoPayload {}

    /// **A request parked in the in-gap wait is what the supervisor sees as
    /// demand.** The flow the substrate's `DialDemand` tests assume from the
    /// client side: `request` finds no dispatcher → `wait_for_connected` parks
    /// → the client's `DialDemand` (the same `Arc` every `ClientChannel` it
    /// builds hands the supervisor) reads waiting → the request's deadline ends
    /// the wait → it no longer does. Without this link the supervisor keeps its
    /// idle pace and a request in a short gap still spends its whole deadline on
    /// a grown nap.
    #[tokio::test(start_paused = true)]
    async fn a_request_waiting_out_a_gap_counts_as_dial_demand() {
        let client = unreachable_bearer_only_client();
        let parked = Arc::clone(&client);
        let request = tokio::spawn(async move {
            parked
                .request_with_deadline::<NoPayload, fauna_protocol::Value>(
                    "fauna.nest.info",
                    NoPayload {},
                    std::time::Duration::from_secs(5),
                )
                .await
        });
        yield_until(|| client.dial_demand.is_waiting()).await;
        assert!(
            client.dial_demand.is_waiting(),
            "a request parked for a connection must count as a waiter"
        );

        let err = request
            .await
            .expect("request task")
            .expect_err("no connection ever comes");
        assert!(
            matches!(
                err,
                NestClientError::RpcDisconnected {
                    was_in_flight: false
                }
            ),
            "the deadline ends the in-gap wait, got {err:?}"
        );
        assert!(
            !client.dial_demand.is_waiting(),
            "a request that stopped waiting must stop counting"
        );
    }

    /// **A request on a disconnected client fails at once, not at its
    /// deadline.** `disconnect()` aborts the supervisor, so nothing can ever
    /// satisfy `wait_for_connected` — yet until 2026-08-27 a request issued
    /// after it parked for its whole deadline (30 s by default) waiting for
    /// that reconnect. Measured on tui: an account switch disconnects the
    /// outgoing client, and every RPC still owed by the outgoing session's
    /// launch (the receive loop's prologue) held that session — and the
    /// one-engine-per-store lock on its MLS store — for 30 s each, so switching
    /// back inside that window found the store "served in another instance of
    /// this app" (`account-scoping.md` § Implementation status → the
    /// `tui (in-memory)` ledger row).
    ///
    /// Paused time makes the verdict exact and instant: a waiter that sleeps
    /// out the deadline advances the virtual clock by exactly that deadline; a
    /// fast failure moves it by nothing.
    #[tokio::test(start_paused = true)]
    async fn a_request_after_disconnect_fails_without_waiting_out_its_deadline() {
        let client = unreachable_bearer_only_client();
        client
            .connect()
            .await
            .expect("connect (the auth is static)");
        client.disconnect().await;

        let before = tokio::time::Instant::now();
        let err = client
            .request::<NoPayload, fauna_protocol::Value>("fauna.nest.info", NoPayload {})
            .await
            .expect_err("no nest ever answers this client");

        assert!(
            matches!(
                err,
                NestClientError::RpcDisconnected {
                    was_in_flight: false
                }
            ),
            "a torn-down client answers RpcDisconnected, got {err:?}"
        );
        assert_eq!(
            tokio::time::Instant::now().duration_since(before),
            std::time::Duration::ZERO,
            "the request waited for a reconnect that `disconnect()` made impossible"
        );
    }

    /// **A request on a client whose supervisor stopped for good fails at once,
    /// with why.** A nest answering HTTP 426 to the upgrade stops the
    /// supervisor terminally (`reconnect.rs`'s `connect_error_is_terminal`), and
    /// until 2026-09-15 nothing read the reason: every later request waited out
    /// its whole deadline for a reconnect that could not come and failed
    /// `RpcDisconnected`, which the since-removed headless sync daemon's
    /// register hold logged as "could not reach the nest" forever. The deadline here is an hour, so
    /// only the recorded stop can end the request; the outer timeout is a hang
    /// guard for a regression, not a timing assertion (convention 14).
    #[tokio::test]
    async fn a_request_after_the_supervisor_stops_fails_at_once_with_why() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await; // drain the upgrade request
                let _ = stream
                    .write_all(b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });
        let bearer: Arc<dyn fauna_nest_http::BearerSource> = Arc::new(StaticBearer("tok".into()));
        let client = NestClient::with_auth(Arc::new(AuthClient::bearer_only(
            format!("http://{addr}"),
            [0x5au8; 32],
            bearer,
            reqwest::Client::new(),
        )));
        client
            .connect()
            .await
            .expect("connect (the auth is static)");

        let err = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            client.request_with_deadline::<NoPayload, fauna_protocol::Value>(
                "fauna.nest.info",
                NoPayload {},
                std::time::Duration::from_secs(3600),
            ),
        )
        .await
        .expect("the request kept waiting for a reconnect the stopped supervisor cannot make")
        .expect_err("no nest ever serves this client");

        assert!(
            matches!(err, NestClientError::SubprotocolMismatch),
            "a stopped supervisor's reason must reach the request, got {err:?}"
        );
        assert!(
            matches!(
                client.supervisor_stop(),
                Some(NestClientError::SubprotocolMismatch)
            ),
            "the stop must stay readable for a caller that retries across it"
        );
    }

    /// Every way the supervisor ends names the error the requests after it
    /// fail with: a clean close is a plain disconnect, like a torn-down client;
    /// a changed nest identity stays that typed verdict whether the dial or the
    /// 4401 refresh met it (`security.md` § Post-auth surfacing: the verdict
    /// survives every seam); a version skew stays a skew, from either source;
    /// a refresh that could not mint keeps its cause.
    #[tokio::test]
    async fn every_supervisor_ending_names_the_error_requests_then_fail_with() {
        use fauna_ws_substrate::SupervisorError as S;
        let client = unreachable_bearer_only_client();
        let identity = || NestClientError::NestIdentityChanged {
            host: "nest.example".into(),
            pinned_hex: "aa".repeat(32),
            seen_hex: Some("bb".repeat(32)),
        };
        let named = |ended: Result<(), S<NestClientError>>| {
            SupervisorStop::of(&ended, client.auth()).error()
        };

        assert!(matches!(
            named(Ok(())),
            NestClientError::RpcDisconnected {
                was_in_flight: false
            }
        ));
        assert!(matches!(
            named(Err(S::TerminalRefusal(identity()))),
            NestClientError::NestIdentityChanged { .. }
        ));
        assert!(matches!(
            named(Err(S::AuthRefresh(identity()))),
            NestClientError::NestIdentityChanged { .. }
        ));
        assert!(matches!(
            named(Err(S::TerminalRefusal(
                NestClientError::SubprotocolMismatch
            ))),
            NestClientError::SubprotocolMismatch
        ));
        assert!(matches!(
            named(Err(S::SubprotocolMismatch)),
            NestClientError::SubprotocolMismatch
        ));
        let refresh_failed = named(Err(S::AuthRefresh(NestClientError::Auth(
            "(401) fauna.auth.signature_failed".into(),
        ))));
        assert!(
            matches!(&refresh_failed, NestClientError::Auth(m) if m == "(401) fauna.auth.signature_failed"),
            "{refresh_failed:?}"
        );
    }

    /// A user suspended mid-session: the nest closes the socket with 4401 and
    /// refuses the re-mint with `fauna.auth.not_registered`. Whichever bearer
    /// shape minted, the supervisor must end as that typed refusal — the form
    /// the apps route to the launch surface's previously-signed-in row
    /// (`onboarding.md` § App-launch routing) — never the flattened
    /// "couldn't obtain a token" string.
    #[tokio::test]
    async fn a_refused_re_mint_after_4401_ends_the_supervisor_as_the_typed_refusal() {
        use fauna_ws_substrate::SupervisorError as S;

        // `WsChallengeBearer` (the FFI clients' path): its mapping flattens the
        // refusal into `Auth`, and the supervisor reads the verdict off the
        // mint's last-refusal channel.
        let owned = NestClient::new(
            "http://127.0.0.1:1".into(),
            fauna_core::identity::ActorKeypair::generate(),
        );
        owned
            .auth()
            .latch_last_auth_refusal_for_test(fauna_protocol::RpcError::not_registered());
        let flattened = NestClientError::Auth("(403) fauna.auth.not_registered".into());
        let stop = SupervisorStop::of(&Err(S::AuthRefresh(flattened)), owned.auth()).error();
        assert!(
            matches!(&stop, NestClientError::Rpc(r) if r.is_not_registered()),
            "{stop:?}"
        );
        assert_eq!(
            stop.session_ending_verdict(),
            Some(crate::error::SessionEndingVerdict::SignInRefused)
        );

        // A caller-supplied bearer (linux and tui's `LaunchMachineBearer`) has
        // no channel; it carries the verdict typed, which `map_api_err` hands
        // the supervisor as the same wire refusal.
        let supplied = unreachable_bearer_only_client();
        let typed = NestClientError::Rpc(fauna_protocol::RpcError::not_registered());
        let stop = SupervisorStop::of(&Err(S::AuthRefresh(typed)), supplied.auth()).error();
        assert_eq!(
            stop.session_ending_verdict(),
            Some(crate::error::SessionEndingVerdict::SignInRefused)
        );

        // A dial-time terminal refusal is not a refresh: the last-refusal
        // channel does not rename it.
        let stop = SupervisorStop::of(
            &Err(S::TerminalRefusal(NestClientError::SubprotocolMismatch)),
            owned.auth(),
        )
        .error();
        assert!(
            matches!(stop, NestClientError::SubprotocolMismatch),
            "{stop:?}"
        );
    }

    /// **Row 58 — `connect()` must not strand the supervisor it replaces.**
    ///
    /// `connect()` assigned `*handle = Some(tokio::spawn(...))`, which drops the
    /// previous `JoinHandle` — and **dropping a `JoinHandle` does not abort its
    /// task** (the class `transport.md` § Connection lifecycle already records
    /// for `nest_link`'s proxy). So every extra `connect()` left a whole live
    /// supervisor behind, each owning its own connection and each dialling on
    /// its own schedule, with nothing anywhere holding a handle to stop it:
    /// `disconnect()` can only abort the newest. That is the accumulation half
    /// of the 2026-08-22 mac incident — ~16,300 connections held ESTABLISHED on
    /// both ends, none of them released.
    ///
    /// Counts live supervisors structurally rather than by timing: each one owns
    /// a `ClientChannel` holding a clone of the client's `PushBroker`, so the
    /// broker's strong count *is* the number of supervisors alive.
    #[tokio::test]
    async fn connect_twice_does_not_strand_the_first_supervisor() {
        let client = unreachable_bearer_only_client();
        let idle = Arc::strong_count(&client.pushes);

        client.connect().await.expect("first connect");
        let one_running = Arc::strong_count(&client.pushes);
        assert!(
            one_running > idle,
            "expected the first connect to spawn a supervisor holding a PushBroker clone \
             (idle={idle}, after one connect={one_running})"
        );

        client.connect().await.expect("second connect");
        yield_until(|| Arc::strong_count(&client.pushes) <= one_running).await;
        let two_connects = Arc::strong_count(&client.pushes);

        assert_eq!(
            two_connects, one_running,
            "a second connect() left the first supervisor running: PushBroker strong count is \
             {two_connects}, expected {one_running}. Each stranded supervisor keeps its own \
             connection and keeps dialling, and disconnect() can only reach the newest — the \
             shape that accumulated ~16,300 unreleased sockets on 2026-08-22"
        );

        client.disconnect().await;
        yield_until(|| Arc::strong_count(&client.pushes) <= idle).await;
        assert_eq!(
            Arc::strong_count(&client.pushes),
            idle,
            "disconnect() must leave no supervisor holding the PushBroker"
        );
    }

    /// Letting go of a client without `disconnect()` must take its supervisor
    /// with it. Before `impl Drop for NestClient` the supervisor held only the
    /// channel, so a dropped client left a reconnect loop dialling forever with
    /// no handle anywhere to stop it — the 2026-09-24 sync-agent flood, one
    /// stranded loop per invalidated cache entry. Counted structurally as
    /// above: the supervisor's `ClientChannel` holds a `PushBroker` clone.
    #[tokio::test]
    async fn dropping_a_client_stops_its_dialling() {
        let client = unreachable_bearer_only_client();
        let pushes = Arc::clone(&client.pushes);
        client.connect().await.expect("connect");
        assert!(
            Arc::strong_count(&pushes) > 2,
            "expected the connect to spawn a supervisor holding a PushBroker clone"
        );

        drop(client);
        yield_until(|| Arc::strong_count(&pushes) == 1).await;
        assert_eq!(
            Arc::strong_count(&pushes),
            1,
            "a dropped client left its supervisor running — a reconnect loop nothing can stop"
        );
    }

    /// After `disconnect()` a subscriber that attaches *afterwards* must read
    /// `Disconnected` — the client is torn down and no connection can come.
    ///
    /// Before the `send_replace` fix it read `Connected`. `disconnect()` aborts
    /// the reconnect-derive task — which holds the only receiver this client
    /// keeps alive on its own behalf, the channel's original one having been
    /// dropped in `with_registry` — and only *then* published the new state, so
    /// with nobody subscribed the plain `watch::send` was discarded and the
    /// stored value stayed whatever the live connection had last written. An
    /// app re-reading the connection-status indicator (`transport.md`
    /// § Connection-status indicator) therefore showed "Connected" on a dead
    /// client.
    ///
    /// The `Connected` starting state is written straight onto the watch rather
    /// than earned from a server: what is under test is the teardown
    /// publication, and a real connection would only add a nest to the fixture.
    #[tokio::test]
    async fn disconnect_is_visible_to_a_subscriber_that_attaches_afterwards() {
        let client = unreachable_bearer_only_client();
        let (_slot, state_tx) = client.supervisor_channels_for_test();
        state_tx.send_replace(ConnectionState::Connected);

        client.disconnect().await;

        assert_eq!(
            *client.connection_state().borrow(),
            ConnectionState::Disconnected,
            "a client torn down by disconnect() still reported Connected to a late subscriber"
        );
    }
}
