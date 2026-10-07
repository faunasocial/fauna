//! Per-connection WebSocket state for Spec Y. Hosts the bounded outbound
//! channel, per-connection `seq`, idempotency cache, and pending-handler
//! abort registry. Per spec § 5.1.
//!
//! Push emit goes through `WsState::notify_push(actor, PushEvent::...)`,
//! which canonical-CBOR-encodes the frame, allocates `seq` per connection,
//! and `try_send`s; on `Full` for a Push, the connection's
//! `dropped_pushes` increments and `needs_resync` is set. The
//! ResyncRequired coalesce timer is owned by the per-connection driver
//! task spawned by `routes::handle_ws`.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use lru::LruCache;
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;

use fauna_protocol::{Frame, Reply, RpcError, encode_canonical, encode_frame};

use crate::dispatch_core::{DispatchSink, err_to_value, outcome_to_reply_value};

/// Bound for the per-connection outbound channel. Per spec § 1.6.
pub const WS_OUTBOUND_BOUND: usize = 256;
/// Idempotency cache size (entries) per connection. Per spec § 1.3.
pub const IDEMPOTENCY_CACHE_ENTRIES: usize = 1000;
/// Idempotency cache TTL. Per spec § 1.3.
pub const IDEMPOTENCY_CACHE_TTL: Duration = Duration::from_secs(300);

/// Per-connection in-flight drain grace on graceful shutdown. After SIGTERM the
/// connection stops reading new requests and waits up to this long for its
/// in-flight handlers to finish + flush their replies before sending the WS
/// 1001 close. Must fit comfortably inside the container `stop_grace_period`
/// (10 s default; raised to 15 s in `docker-compose.yml`). Per `transport.md`
/// § Graceful shutdown.
pub const SHUTDOWN_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// How long `main` waits for every connection to drain + close before it stops
/// accepting and flushes the DB. Slightly longer than the per-connection grace
/// so a connection that drains right at the limit still gets to send its 1001.
pub const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(7);
/// Reply payload threshold above which we cache a `too_large` placeholder
/// instead of the full payload. Per spec § 1.3.
pub const REPLY_TOO_LARGE_THRESHOLD: usize = 64 * 1024;

/// Cached Reply frame body for idempotency replay. Per spec § 1.3.
pub struct CachedReply {
    /// Encoded Reply frame body (canonical CBOR), or empty if `too_large`.
    pub payload: Bytes,
    /// Insertion time, used for 5-minute TTL eviction.
    pub inserted_at: Instant,
    /// `true` ⇒ the payload exceeded `REPLY_TOO_LARGE_THRESHOLD`; replay
    /// returns `RpcError { code: "fauna.protocol.replay_too_large" }`.
    pub too_large: bool,
}

/// The per-connection idempotency cache + its lookup/insert recipe, factored out
/// of `RpcConnection` so the per-actor connection (`RpcConnection`) **and** the
/// peer-symmetric federation connection (`federation_channel::FederationConnection`)
/// share one implementation (Spec Y2 slice 4 §4.C: the federation connection
/// "reuses `RpcConnection`'s mechanical core: the per-connection idempotency
/// cache"). 1000 entries / 5-min TTL / `too_large` placeholder above 64 KiB.
pub struct IdempotencyCache {
    cache: Mutex<LruCache<[u8; 16], CachedReply>>,
}

impl Default for IdempotencyCache {
    fn default() -> Self {
        Self::new()
    }
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(IDEMPOTENCY_CACHE_ENTRIES).unwrap(),
            )),
        }
    }

    /// Look up an `idempotency_key`; expired entries are evicted lazily.
    pub async fn lookup(&self, key: &[u8; 16]) -> IdempotencyHit {
        let mut cache = self.cache.lock().await;
        // Lazy eviction: peek without promoting; remove if expired.
        if let Some(entry) = cache.peek(key)
            && entry.inserted_at.elapsed() > IDEMPOTENCY_CACHE_TTL
        {
            cache.pop(key);
            return IdempotencyHit::Miss;
        }
        match cache.get(key) {
            None => IdempotencyHit::Miss,
            Some(entry) => {
                if entry.too_large {
                    IdempotencyHit::TooLarge
                } else {
                    IdempotencyHit::Hit {
                        payload: entry.payload.clone(),
                    }
                }
            }
        }
    }

    /// Insert a Reply payload. Payloads larger than `REPLY_TOO_LARGE_THRESHOLD`
    /// are stored as `too_large` markers only.
    pub async fn insert(&self, key: [u8; 16], payload: Bytes) {
        let too_large = payload.len() > REPLY_TOO_LARGE_THRESHOLD;
        let entry = CachedReply {
            payload: if too_large { Bytes::new() } else { payload },
            inserted_at: Instant::now(),
            too_large,
        };
        self.cache.lock().await.put(key, entry);
    }
}

/// Per-WS-connection state. Created on subscribe; held in `Arc`.
pub struct RpcConnection {
    pub conn_id: u64,
    pub actor_id: [u8; 32],
    /// `true` for the anonymous (pre-identity) connection (`GET /api/v1/ws`,
    /// no bearer). No actor is bound, so it is not in `WsState.subs` and emits
    /// no Push events; `actor_id` is a placeholder. The dispatcher gates it to
    /// the `pre_identity_allowlist`. Per `transport.md` § Pre-identity
    /// (anonymous) connection.
    pub anonymous: bool,
    /// The **principal binding** of a third-party principal's session
    /// (`GET /api/v1/principal/ws`): the account it acts for, the principal id
    /// and the scopes of the token it was opened with. `None` on every other
    /// connection.
    ///
    /// A bound connection's [`Self::actor_id`] is the zero placeholder and it
    /// is never in `WsState.subs`: it lives in the principal registry instead
    /// ([`WsState::subscribe_principal`]), so it receives no Push, counts
    /// toward no presence decision and never reaches an actor handler — the
    /// dispatcher routes every request it carries to
    /// [`crate::principal_handlers::dispatch_principal`], which resolves the
    /// binding per call (`transport-connection.md` § Connection lifecycle →
    /// *The principal session*).
    pub principal: Option<crate::principal_handlers::PrincipalBinding>,
    /// The short `token_id` of the bearer this connection was upgraded with —
    /// **which session** it is, not merely which actor.
    ///
    /// [`Self::actor_id`] answers "whose connection is this", which is all the
    /// per-actor teardown ([`WsState::disconnect_actor`]) ever needs. Its
    /// per-token twin — `fauna.sessions.{revoke,revoke_all}`, which ends **one
    /// bearer** and must leave the actor's other sessions dispatching — needs
    /// the finer question answered, and nothing else on this struct can answer
    /// it: the raw bearer is dropped after the upgrade handshake, and the token
    /// row it names is already gone by the time the teardown runs (the revoke
    /// deletes it first). So the id is captured at the upgrade and kept here
    /// for the connection's lifetime. `devices.md` § What a session is, and
    /// what revoking one does; `transport-connection.md` § Connection lifecycle
    /// → *Revocation teardown*.
    ///
    /// `None` on the anonymous (pre-identity) connection, which carries no
    /// bearer at all, and on connections built through the plain
    /// [`WsState::subscribe`] — tests. A `None` is **not** a wildcard in either
    /// direction, and each teardown resolves it toward its own conservative
    /// arm: [`WsState::disconnect_token_id`] (end this one session) never
    /// matches it, so an unattributable connection is not over-killed;
    /// [`WsState::disconnect_actor_except_token_id`] (end all but the kept one)
    /// does close it, so a session the nest cannot vouch for is not silently
    /// spared by *sign out everywhere else*.
    pub token_id: Option<String>,
    /// The device this connection is **bound to**: the renewal device key that
    /// minted its bearer over `fauna.auth.device_handshake` — the same key the
    /// device's `sync_devices` row carries as `auth_device_key` (the roster's
    /// `principal`). What `fauna.sync.devices.list` joins on to answer
    /// `online` for the app seats and per-user sync agents (`devices.md`
    /// § Listing Devices → *The binding*).
    ///
    /// Captured at the upgrade beside [`Self::token_id`], for the same reason:
    /// the bearer is validated once and its row is never re-read, and a socket
    /// outlives its bearer's hour by design — so a read-time join through the
    /// token store would paint a still-connected device offline the moment its
    /// row expired. The join runs **inside the actor's own subscription
    /// entry** ([`WsState::has_connection_bound_to`]), so a key on another
    /// actor's connection reaches nothing here.
    ///
    /// Nest-verified by construction: the mint that set the tag verified the
    /// device key's signature over the domain-tagged handshake message. No
    /// client asserts a device id anywhere on this path. `None` for a
    /// seed-minted bearer (handshake, challenge/verify — an identity, not a
    /// device), for the custody handshake's overloaded tag (excluded by
    /// [`bound_device_key_for`]), on the anonymous connection, and on
    /// test-subscribed connections.
    pub bound_device_key: Option<[u8; 32]>,
    /// The push `device_id` this connection **announced** it serves, over
    /// `fauna.push.presence` — `None` until it does (no app announces presence yet). The input to the per-device push decision
    /// ([`WsState::device_presence`]; `apps/common.md` § Dispatch Logic).
    ///
    /// Deliberately a separate fact from [`Self::bound_device_key`]: that one
    /// is nest-verified but absent on every seed-minted bearer, and names a
    /// renewal key, not the subscription row's id. This one is client-asserted,
    /// which is enough — all a connection can do with it is suppress or receive
    /// its own actor's push (`apps/common.md` § Registration → *Every
    /// connection announces*). A later announce replaces an earlier one.
    announced_device: std::sync::Mutex<Option<String>>,
    /// The folders this connection announced it serves relay reads for, over
    /// `fauna.sync.serve.announce` (`file-sync.md` § Relay serving, step (1)) —
    /// `None` until it does. Already admitted: the handler keeps only folder
    /// rows the actor may read as owner or member, on a device of its own
    /// account, at most `SERVE_ANNOUNCE_MAX_FOLDERS` of them. A later announce
    /// replaces an earlier one whole; the state dies with the connection, so
    /// every teardown — close, revocation ([`Self::revoke`]) — ends it with no
    /// sweep of its own. Read by the relay's seat walk and the reachability
    /// verdict through [`WsState::announced_for_folder`].
    announced_serving: std::sync::Mutex<Option<ServingAnnounce>>,
    /// Stops the task renewing this connection's foreign seats on their home
    /// nests (`sync_handlers::keep_foreign_seats`) — replaced by each announce
    /// that admits a foreign folder, cancelled by the next announce, which
    /// takes the renewing over.
    pub(crate) foreign_renewal: std::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// Per-connection ascending Push `seq` (envelope key 8). Per spec § 1.5.
    pub seq: AtomicU64,
    /// Count of Push frames dropped on `try_send` overflow since the last
    /// `ResyncRequired` emit. Per spec § 1.6.
    pub dropped_pushes: AtomicU64,
    /// Set when `dropped_pushes` last incremented; cleared after a
    /// `ResyncRequired` is sent. Per spec § 1.6.
    pub needs_resync: AtomicBool,
    /// Idempotency cache for Reply replay. Per spec § 1.3. Shared
    /// implementation with the federation connection (see [`IdempotencyCache`]).
    pub idempotency_cache: IdempotencyCache,
    /// In-flight RPC handler abort registry. Cancel removes + aborts.
    /// Per spec § 1.4.
    pub pending_handlers: Mutex<HashMap<u64, AbortHandle>>,
    /// Bounded outbound channel toward the WS writer.
    pub ws_tx: mpsc::Sender<Bytes>,
    /// One outbound slot per admitted request, keyed by `correlation_id`:
    /// reserved from [`Self::ws_tx`]'s capacity when the reader loop admits
    /// the Request (`routes::run_connection`), consumed by whichever Reply
    /// emitter answers it ([`Self::emit_reply`]).
    ///
    /// This is the per-connection admission cap and the reason a Reply cannot
    /// overflow a draining connection. Replies and pushes share the one
    /// 256-frame queue; a push `try_send`s into the capacity no admitted
    /// request holds, so a push burst degrades to `ResyncRequired` and never
    /// takes the place a Reply was promised, and at most
    /// [`WS_OUTBOUND_BOUND`] requests are in flight because the 257th
    /// admission parks the reader until a Reply has gone out — the
    /// per-channel cap the peer-symmetric planes take as `SERVE_MAX_INFLIGHT`
    /// (`transport.md` § Request lifecycle). A slot dropped without a send
    /// (an encode failure, the connection's own drop) returns its capacity.
    reply_slots: std::sync::Mutex<HashMap<u64, mpsc::OwnedPermit<Bytes>>>,
    /// The peer's socket address, when known. Populated on **both** listener
    /// paths: the plain-HTTP listener (axum's
    /// `into_make_service_with_connect_info`) and the TLS-terminating listener
    /// (`serve_tls`'s `WithConnectInfo` middleware injects the accepted TCP
    /// peer — `lib.rs`). On the public TLS path this is the **real client IP**
    /// (nest terminates TLS directly, no reverse proxy — `transport.md`), which
    /// is what the anonymous discovery-surface throttle keys on. **`None` on
    /// every authenticated connection** (that upgrade captures no `ConnectInfo`
    /// — see `subscribe`), and in tests without one; it is populated only on the
    /// anonymous path. Read by the dispatcher's loopback gate for
    /// `requires_loopback_peer` kinds (bridge self-enrollment, per
    /// `mail-bridge-lifecycle.md` § Cold boot), which fails closed on `None`,
    /// and by the pre-identity rate limiter's anonymous arm
    /// (`anonymous_rate_limit::check_conn`, whose authenticated arm keys on
    /// [`Self::actor_id`] instead precisely because this is absent there).
    pub peer_addr: Option<std::net::SocketAddr>,
    /// Flips `true` when this connection's actor loses its authority — account
    /// lockout, suspension, or deletion. Each live `run_connection` watches it;
    /// on the flip it stops reading new requests and closes the socket with WS
    /// **4401**, per `transport.md` § Connection lifecycle → *Revocation
    /// teardown*. The per-actor twin of [`WsState`]'s shutdown watch.
    ///
    /// The bearer is validated exactly once, at the WS upgrade, and the actor is
    /// then baked into this struct for the connection's lifetime — nothing in
    /// `dispatch_core` re-reads the token store. So revoking tokens alone leaves
    /// an already-open socket fully functional; this signal is what closes it.
    revoked_tx: tokio::sync::watch::Sender<bool>,
    /// Set when a **handler running on this very connection** revoked its own
    /// caller's authority: `Some(correlation_id)` of the request that did it.
    ///
    /// The connection stops dispatching immediately, exactly as [`Self::
    /// revoked_tx`] would — but the outbound task forwards frames until the
    /// Reply carrying this correlation id has gone out, and only then flips
    /// `revoked_tx` and closes 4401. One frame of grace, for the answer the
    /// caller was already waiting on.
    ///
    /// It exists for the paths that revoke **the caller itself**, which is the
    /// minority: every other authority-stripping path revokes somebody who is
    /// not asking, so the 4401 *is* the whole message. Two families qualify.
    /// `fauna.recovery.succession.submit`, which it was built for, retires the
    /// identity whose socket carries it, and its Reply — `new_actor_id` +
    /// `succeeded_at` — is the ceremony's entire product; drop it and the
    /// client cannot tell "the account moved" from "nothing happened"
    /// (`identity-succession.md` § Enforcement on the home nest). And, since
    /// 2026-09-20, `fauna.sessions.{revoke,revoke_all}`, whose per-token
    /// teardown regularly closes the asking connection — `revoke` is always
    /// self-directed, and `revoke_all` reaches the caller through the documented
    /// renewal race — where the Reply is the app's only evidence that the
    /// revoke committed. Both in `transport-connection.md` § Connection
    /// lifecycle → *Revocation teardown*.
    ///
    /// The grace cannot outlive the request: every terminal outcome of a
    /// dispatch — success, handler error, deadline timeout, Cancel — emits a
    /// `Reply` at that correlation id, so the frame the outbound task is
    /// watching for always comes. Should one somehow not (a panicked reply
    /// task), the connection is already refusing every request and the
    /// heartbeat's liveness window reaps it like any other dead link.
    spare_until_tx: tokio::sync::watch::Sender<Option<u64>>,
    /// Set once this connection has committed a fatal protocol/transport fault
    /// that `transport.md` § Close codes says must be reported as a specific WS
    /// close code (4400 or 1011). `run_connection`'s outbound task watches it
    /// and emits the frame; the inbound task watches it so an overflow raised by
    /// a *handler* tears the loop down promptly instead of waiting for a peer
    /// that has stopped reading.
    ///
    /// `Option`, not a second pair of bools: every future fatal close code is
    /// another variant here rather than another watch channel and another
    /// `tokio::select!` arm — the arm count is where the `biased;` ordering gets
    /// subtle. Deliberately separate from [`Self::revoked_tx`] because
    /// revocation outranks it (see the send task's `biased;` order) and folding
    /// them would put an authority control and a protocol fault behind one
    /// priority.
    fatal_tx: tokio::sync::watch::Sender<Option<FatalCloseReason>>,
    /// The durable idempotency tier (`db/rpc_idempotency.rs`) — consulted on a
    /// per-connection-cache miss, written on each recorded `ok` Reply.
    /// `None` on the anonymous connection (its placeholder actor would alias
    /// every anonymous caller into one namespace) and in unit tests built
    /// through `WsState::new()`; production wires it via
    /// [`WsState::with_durable`].
    durable: Option<std::sync::Arc<crate::db::CacheDb>>,
}

/// Why a connection is being closed with a specific non-1001 code. Each variant
/// is one row of `transport.md` § Close codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FatalCloseReason {
    /// The client sent a frame the protocol forbids — a Reply or Push (only the
    /// nest sends those), a text frame (Spec Y is binary-only), or bytes that
    /// failed to decode as a frame at all.
    ProtocolViolation,
    /// The bounded outbound channel was full when a Reply had to go out.
    /// **Fatal by design** (`transport.md` § Backpressure): a Reply cannot be
    /// dropped, so a client that is not draining has a dead connection. The
    /// alternative — dropping it silently — leaves the caller waiting out its
    /// full deadline for an answer that will never come.
    ReplyOverflow,
}

impl FatalCloseReason {
    /// The WS close code this reason maps to, per `transport.md` § Close codes.
    pub fn code(self) -> u16 {
        match self {
            FatalCloseReason::ProtocolViolation => 4400,
            FatalCloseReason::ReplyOverflow => 1011,
        }
    }

    /// The close frame's reason text. Deliberately terse and non-specific: it
    /// reaches an unauthenticated-at-this-layer peer, and the *code* is what
    /// every client actually branches on.
    pub fn text(self) -> &'static str {
        match self {
            FatalCloseReason::ProtocolViolation => "protocol violation",
            FatalCloseReason::ReplyOverflow => "reply channel overflow",
        }
    }
}

/// The device binding a validated bearer confers on its connection
/// ([`RpcConnection::bound_device_key`]), from the token row's
/// `minted_by_device` tag.
///
/// The tag is overloaded by one mint: `fauna.auth.custody_handshake` files
/// the **custodian's own actor id** there so the sessions list can label the
/// session, and that session's actor *is* the custodian — so a tag equal to
/// the connection's own actor is the custody signature and never a device key
/// (`devices.md` § Listing Devices → *The binding*). Every other `Some` is a
/// `device_handshake` mint's renewal key. A pure function so the exclusion
/// has a witness without an HTTP upgrade (`conformance_device_online.rs`).
pub fn bound_device_key_for(
    actor_id: &[u8; 32],
    minted_by_device: Option<[u8; 32]>,
) -> Option<[u8; 32]> {
    minted_by_device.filter(|key| key != actor_id)
}

/// What one connection announced over `fauna.sync.serve.announce`, after the
/// handler admitted it ([`RpcConnection::announced_serving`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServingAnnounce {
    /// The registered sync device of the connection's actor the announce named.
    pub device_id: [u8; 32],
    /// The admitted folder rows (`folders.id`), each once.
    pub folder_ids: Vec<i64>,
    /// The admitted folders homed on another nest, each once — the ones that
    /// nest leased a seat for (`file-sync.md` § Relay serving → *A member on
    /// another nest*, step (2)).
    pub foreign: Vec<ForeignServing>,
}

/// One foreign folder a connection serves, as its home nest admitted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignServing {
    /// The folder's channel (`FolderRef::Foreign`).
    pub channel_id: [u8; 32],
    /// The home nest's URL the announce was forwarded to.
    pub nest_url: String,
    /// The home nest's verified `nest_id` — the one nest whose ask for this
    /// seat is pushed (`federation.md` § … → *Relay serving across nests*).
    pub home_nest_id: [u8; 32],
}

impl RpcConnection {
    fn new(
        conn_id: u64,
        actor_id: [u8; 32],
        ws_tx: mpsc::Sender<Bytes>,
        anonymous: bool,
        principal: Option<crate::principal_handlers::PrincipalBinding>,
        token_id: Option<String>,
        bound_device_key: Option<[u8; 32]>,
        peer_addr: Option<std::net::SocketAddr>,
        durable: Option<std::sync::Arc<crate::db::CacheDb>>,
    ) -> Self {
        Self {
            conn_id,
            actor_id,
            anonymous,
            principal,
            token_id,
            bound_device_key,
            announced_device: std::sync::Mutex::new(None),
            announced_serving: std::sync::Mutex::new(None),
            foreign_renewal: std::sync::Mutex::new(None),
            seq: AtomicU64::new(0),
            dropped_pushes: AtomicU64::new(0),
            needs_resync: AtomicBool::new(false),
            idempotency_cache: IdempotencyCache::new(),
            pending_handlers: Mutex::new(HashMap::new()),
            ws_tx,
            reply_slots: std::sync::Mutex::new(HashMap::new()),
            peer_addr,
            revoked_tx: tokio::sync::watch::channel(false).0,
            spare_until_tx: tokio::sync::watch::channel(None).0,
            fatal_tx: tokio::sync::watch::channel(None).0,
            durable,
        }
    }

    /// The push `device_id` this connection announced, if any
    /// ([`Self::announced_device`]).
    pub fn announced_device(&self) -> Option<String> {
        self.announced_device.lock().unwrap().clone()
    }

    /// Whether this connection announced it serves relay reads of the folder
    /// row `folder_id` ([`Self::announced_serving`]) and is still a live
    /// candidate — a revoked connection is closing and is asked nothing.
    pub fn serves_folder(&self, folder_id: i64) -> bool {
        !self.is_revoked()
            && self
                .announced_serving
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|s| s.folder_ids.contains(&folder_id))
    }

    /// Allocate the next ascending `seq` for an outbound Push frame.
    pub fn next_push_seq(&self) -> u64 {
        // Pre-increment: spec § 1.5 says "increments on every successful
        // Push frame send"; we allocate the value to use, then send.
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Wait for one free place in the outbound queue and reserve it for a
    /// Reply ([`Self::reply_slots`]). `None` once the writer is gone. The
    /// reader loop races this against its revoke, fatal, shutdown and
    /// liveness watches, so a parked admission never outlives the connection.
    pub async fn reserve_reply_slot(&self) -> Option<mpsc::OwnedPermit<Bytes>> {
        self.ws_tx.clone().reserve_owned().await.ok()
    }

    /// Hold `slot` for the Reply to `correlation_id`. A client reusing a
    /// correlation id still in flight replaces the earlier slot, whose Reply
    /// then falls back to a plain `try_send` — the client's own bug, and the
    /// queue's capacity is still never over-promised.
    pub fn hold_reply_slot(&self, correlation_id: u64, slot: mpsc::OwnedPermit<Bytes>) {
        self.reply_slots
            .lock()
            .unwrap()
            .insert(correlation_id, slot);
    }

    /// Give back the slot held for `correlation_id` without sending anything —
    /// a Reply that could not be encoded.
    pub fn release_reply_slot(&self, correlation_id: u64) {
        self.reply_slots.lock().unwrap().remove(&correlation_id);
    }

    /// Put the encoded Reply frame for `correlation_id` on the outbound queue
    /// — the one carriage every per-actor Reply emitter shares.
    ///
    /// Through the slot reserved at admission when there is one, which cannot
    /// fail; a send into a closed queue then drops the frame with its
    /// connection. Without one (a request no reader admitted — a test driving
    /// the sink directly) it is a `try_send`, and the two failures mean
    /// different things: **Full** is a peer that is not draining, fatal 1011
    /// by `transport.md` § Backpressure; **Closed** is a writer that is
    /// already gone (the socket closed, a 4401 revoke), with nothing left to
    /// tear down — the same split the Push path makes.
    fn emit_reply(&self, correlation_id: u64, frame: Bytes) {
        let slot = self.reply_slots.lock().unwrap().remove(&correlation_id);
        if let Some(slot) = slot {
            slot.send(frame);
            return;
        }
        match self.ws_tx.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::error!(
                    actor = %hex::encode(self.actor_id),
                    correlation_id,
                    "outbound queue saturated on Reply; connection will be torn down"
                );
                // Until 2026-07-31 the log was the *whole* handling: the line
                // claimed a teardown that no code performed, so the connection
                // carried on and the caller waited out its full deadline for a
                // Reply that had already been dropped.
                self.signal_fatal(FatalCloseReason::ReplyOverflow);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(
                    actor = %hex::encode(self.actor_id),
                    correlation_id,
                    "outbound queue closed; Reply dropped with its connection"
                );
            }
        }
    }

    /// Signal that this connection's actor has lost its authority. Idempotent.
    /// `run_connection` responds by closing the socket with WS 4401.
    ///
    /// `send_replace`, not `send`: `send` fails (and stores nothing) while no
    /// receiver exists, and there is a window where the connection is already
    /// registered in `WsState.subs` but `run_connection` has not yet called
    /// [`RpcConnection::subscribe_revoked`] — a revoke landing there would be
    /// silently lost, and even `run_connection`'s pre-select `is_revoked()`
    /// check would read a stale `false`. `send_replace` stores the flag
    /// unconditionally, so the pre-select check closes that gap.
    pub fn revoke(&self) {
        self.revoked_tx.send_replace(true);
    }

    /// `true` once [`RpcConnection::revoke`] has been called.
    pub fn is_revoked(&self) -> bool {
        *self.revoked_tx.borrow()
    }

    /// A receiver `run_connection` awaits to learn its actor was revoked.
    pub fn subscribe_revoked(&self) -> tokio::sync::watch::Receiver<bool> {
        self.revoked_tx.subscribe()
    }

    /// Revoke this connection, but let the Reply for `correlation_id` out
    /// first — see [`Self::spare_until_tx`] for why exactly one path wants
    /// this. Dispatch stops now; the socket closes 4401 the moment that one
    /// frame is on the wire.
    ///
    /// **First sparing wins**, like [`Self::signal_fatal`]: a connection can
    /// only be answering one revoking request at a time, and a second call
    /// would move the goalpost to a frame that may never be emitted.
    /// `send_if_modified` rather than `send_replace` so the no-op case wakes
    /// no receiver.
    pub fn revoke_after_reply(&self, correlation_id: u64) {
        self.spare_until_tx.send_if_modified(|slot| {
            if slot.is_some() {
                false
            } else {
                *slot = Some(correlation_id);
                true
            }
        });
    }

    /// The correlation id this connection is being kept open for, if any.
    /// `Some` means "revoked, but still owes one answer".
    pub fn spared_until(&self) -> Option<u64> {
        *self.spare_until_tx.borrow()
    }

    /// A receiver `run_connection`'s outbound task holds so it can tell, for
    /// each frame it forwards, whether that frame is the one the grace was for.
    pub fn subscribe_spared(&self) -> tokio::sync::watch::Receiver<Option<u64>> {
        self.spare_until_tx.subscribe()
    }

    /// Commit this connection to closing with `reason`'s WS close code.
    /// `run_connection` responds by emitting the frame and tearing down.
    ///
    /// **First reason wins** — a later call is a no-op. The first fault is the
    /// cause; the ones after it are consequences (a protocol violation makes the
    /// peer stop reading, which then overflows the Reply channel), and reporting
    /// the consequence would tell the client to reconnect-with-backoff over what
    /// was actually its own bug.
    ///
    /// `send_replace` rather than `send`, for the same reason
    /// [`RpcConnection::revoke`] uses it: `send` stores nothing while no
    /// receiver exists, and a fault raised before `run_connection` subscribes
    /// would be lost — the pre-select `fatal_close()` check is what closes that
    /// window, and it can only work if the value was actually stored.
    pub fn signal_fatal(&self, reason: FatalCloseReason) {
        self.fatal_tx.send_if_modified(|slot| {
            if slot.is_some() {
                return false;
            }
            *slot = Some(reason);
            true
        });
    }

    /// The committed fatal close reason, if any.
    pub fn fatal_close(&self) -> Option<FatalCloseReason> {
        *self.fatal_tx.borrow()
    }

    /// A receiver `run_connection`'s tasks await to learn this connection must
    /// close with a specific code.
    pub fn subscribe_fatal(&self) -> tokio::sync::watch::Receiver<Option<FatalCloseReason>> {
        self.fatal_tx.subscribe()
    }
}

/// Return value of `RpcConnection::lookup_idempotent`. Distinguishes
/// "no entry" / "have payload bytes" / "have a too-large marker" — plus the
/// durable tier's rebuild form.
pub enum IdempotencyHit {
    Miss,
    Hit {
        payload: Bytes,
    },
    TooLarge,
    /// A **durable-tier** hit (`db/rpc_idempotency.rs`): the recorded Reply's
    /// payload `Value` bytes + `ok` bit, NOT frame bytes. A durable hit is by
    /// definition served on a *different* connection than the one that
    /// recorded it, so the Reply frame must be **rebuilt with the current
    /// request's correlation_id** — replaying recorded frame bytes verbatim
    /// would carry the original connection's correlation_id, which the
    /// retrying client's pending map has never heard of (the exact reasoning
    /// the federation sink documents for its own (payload, ok) cache).
    HitRebuild {
        payload: Bytes,
        ok: bool,
    },
}

impl RpcConnection {
    /// Look up an idempotency_key: the per-connection [`IdempotencyCache`]
    /// first (the in-connection fast path, frame bytes replayed verbatim),
    /// then — authenticated connections only — the **durable tier**
    /// (`db/rpc_idempotency.rs`), which is what catches a replay arriving on a
    /// *fresh* connection after a reconnect (`account-data-plane.md` § The
    /// offline-mutation contract → *Nest-side durable idempotency*). A durable
    /// hit comes back [`IdempotencyHit::HitRebuild`] because the Reply frame
    /// must be rebuilt with the *current* correlation_id (see the variant).
    pub async fn lookup_idempotent(&self, key: &[u8; 16]) -> IdempotencyHit {
        match self.idempotency_cache.lookup(key).await {
            IdempotencyHit::Miss => {}
            hit => return hit,
        }
        let Some(db) = self.durable.as_ref() else {
            return IdempotencyHit::Miss;
        };
        let cutoff = crate::db::now_epoch_secs()
            - crate::db::rpc_idempotency::DEFAULT_RPC_IDEMPOTENCY_RETENTION.as_secs() as i64;
        match db.lookup_rpc_idempotent(&self.actor_id, key, cutoff).await {
            Ok(Some(rec)) => match rec.payload {
                Some(bytes) => IdempotencyHit::HitRebuild {
                    payload: Bytes::from(bytes),
                    ok: rec.ok,
                },
                None => IdempotencyHit::TooLarge,
            },
            Ok(None) => IdempotencyHit::Miss,
            Err(e) => {
                // Fail open to a handler re-run — the pre-tier behavior for
                // every kind, and the registry audits mutations as naturally
                // idempotent; failing the request on a cache read would turn a
                // degraded cache into an outage.
                tracing::warn!(error = %e, "durable idempotency lookup failed; treating as miss");
                IdempotencyHit::Miss
            }
        }
    }

    /// Insert a Reply payload into the cache. Payloads larger than
    /// `REPLY_TOO_LARGE_THRESHOLD` are stored as too_large markers only.
    /// Delegates to the shared [`IdempotencyCache`].
    pub async fn insert_idempotent(&self, key: [u8; 16], payload: Bytes) {
        self.idempotency_cache.insert(key, payload).await
    }

    /// Record an `ok` Reply's payload `Value` bytes in the durable tier —
    /// no-op on the anonymous connection, on `Read`-class kinds (a re-run read
    /// has no effect and the freshest answer is the better reply), and when no
    /// durable handle is wired (unit tests). Payloads over
    /// [`REPLY_TOO_LARGE_THRESHOLD`] record the too-large marker, mirroring
    /// the LRU. First write wins at the DB layer.
    async fn record_durable_idempotent(&self, key: [u8; 16], kind: &str, payload_bytes: &[u8]) {
        let Some(db) = self.durable.as_ref() else {
            return;
        };
        if self.anonymous {
            return;
        }
        if matches!(
            fauna_protocol::offline_class::offline_class(kind),
            Some(fauna_protocol::offline_class::OfflineClass::Read)
        ) {
            return;
        }
        let stored = (payload_bytes.len() <= REPLY_TOO_LARGE_THRESHOLD).then_some(payload_bytes);
        if let Err(e) = db
            .insert_rpc_idempotent(
                &self.actor_id,
                &key,
                kind,
                stored,
                true,
                crate::db::now_epoch_secs(),
            )
            .await
        {
            // Same fail-open posture as the lookup: a missed record costs one
            // future handler re-run, never the request in hand.
            tracing::warn!(error = %e, kind, "durable idempotency record failed");
        }
    }
}

/// The per-actor side of the shared dispatch core (Spec Y2 slice 4 §4.C / §5).
/// Reply carriage: encode a `Frame::Reply` to bytes and send it through the
/// outbound slot reserved when the request was admitted
/// ([`RpcConnection::emit_reply`]); the idempotency cache stores the **frame bytes** verbatim, so
/// a replay re-sends them as-is (the established behavior — the cached frame's
/// correlation_id is preserved, not rebuilt). See [`crate::dispatch_core`].
impl DispatchSink for RpcConnection {
    fn pending_handlers(&self) -> &Mutex<HashMap<u64, AbortHandle>> {
        &self.pending_handlers
    }

    /// The per-actor plane is the one where a handler can revoke its own
    /// caller, so it is the one that answers this.
    fn caller_conn_id(&self) -> Option<u64> {
        Some(self.conn_id)
    }

    fn caller_peer_ip(&self) -> Option<std::net::IpAddr> {
        self.peer_addr.map(|a| a.ip())
    }

    fn lookup_idempotent(&self, key: [u8; 16]) -> BoxFuture<'_, IdempotencyHit> {
        // The inherent method — LRU first, then the durable tier. (Same name,
        // two methods: routing the trait through the inherent one is what
        // makes the durable fallback reachable from `check_idempotent`.)
        Box::pin(async move { RpcConnection::lookup_idempotent(self, &key).await })
    }

    fn replay(self: Arc<Self>, correlation_id: u64, cached: Bytes) -> BoxFuture<'static, ()> {
        // Per-actor replays the cached Reply *frame* verbatim (the cached frame
        // already carries the original correlation_id; this matches the existing
        // behavior pre-extraction). The slot is the *current* request's: it is
        // the one this frame answers.
        Box::pin(async move { self.emit_reply(correlation_id, cached) })
    }

    fn replay_rebuilt(
        self: Arc<Self>,
        correlation_id: u64,
        payload: Bytes,
        ok: bool,
    ) -> BoxFuture<'static, ()> {
        // A durable-tier hit is served on a different connection than the one
        // that recorded it, so the frame is REBUILT with the current request's
        // correlation_id (see `IdempotencyHit::HitRebuild`). The recorded
        // bytes are the canonical CBOR of the Reply's payload `Value`.
        Box::pin(async move {
            let payload = match fauna_cbor::decode_strict::<fauna_protocol::Value>(&payload) {
                Ok(v) => v,
                Err(e) => {
                    // A row we wrote that no longer decodes is corruption; the
                    // recoverable direction is the handler re-run we can no
                    // longer offer for THIS request — answer internal so the
                    // caller retries (a fresh key path) rather than hanging.
                    tracing::error!(error = %e, "durable idempotency payload undecodable");
                    return self
                        .send_error(
                            correlation_id,
                            RpcError::new("fauna.protocol.internal", "error.protocol.internal"),
                        )
                        .await;
                }
            };
            let frame = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id,
                payload,
                ok,
            });
            match encode_frame(&frame) {
                Ok(bytes) => self.emit_reply(correlation_id, bytes),
                Err(_) => self.release_reply_slot(correlation_id),
            }
        })
    }

    fn finish(
        self: Arc<Self>,
        correlation_id: u64,
        idempotency_key: [u8; 16],
        kind: String,
        outcome: Result<Bytes, RpcError>,
    ) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let (payload, ok) = outcome_to_reply_value(outcome);
            let frame = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id,
                payload: payload.clone(),
                ok,
            });
            let frame_bytes = match encode_frame(&frame) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(error = %e, "Reply frame encode failed");
                    self.release_reply_slot(correlation_id);
                    return;
                }
            };
            self.insert_idempotent(idempotency_key, frame_bytes.clone())
                .await;
            // Durable tier: `ok` replies only — an error Reply implies no
            // effect, so a replay may (and should) re-run the handler rather
            // than durably re-serve a possibly-transient failure.
            if ok && let Ok(payload_bytes) = encode_canonical(&payload) {
                self.record_durable_idempotent(idempotency_key, &kind, &payload_bytes)
                    .await;
            }
            self.emit_reply(correlation_id, frame_bytes);
        })
    }

    fn send_error(self: Arc<Self>, correlation_id: u64, err: RpcError) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let frame = Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id,
                payload: err_to_value(err),
                ok: false,
            });
            // An error Reply is still a Reply: same slot, same fatal rule, or a
            // saturated connection would go on silently swallowing exactly the
            // frames that tell a caller its request failed.
            match encode_frame(&frame) {
                Ok(bytes) => self.emit_reply(correlation_id, bytes),
                Err(_) => self.release_reply_slot(correlation_id),
            }
        })
    }

    /// The revocation teardown, minus the one frame a spared connection would
    /// still have owed: the outbound task ranks `revoked` above every queued
    /// frame, so the socket closes 4401 with the Reply never written. That is
    /// the close a succession's own connection gets a moment later anyway
    /// ([`RpcConnection::revoke_after_reply`]), which keeps the lost-reply
    /// state as close to the production one as a test can stage.
    #[cfg(feature = "test-hooks")]
    fn close_without_reply(&self) -> bool {
        self.revoke();
        true
    }
}

/// The **server half** of the spec heartbeat: how often the nest sends a WS
/// Ping on a client connection, and how long it tolerates hearing nothing back
/// before treating the link as dead. Per `transport.md` § Connection lifecycle.
///
/// **Why a server-initiated Ping and not an inbound-idle timeout.** A bare
/// "no inbound frame for N seconds ⇒ close" rule is wrong here for the reason
/// `transport-connection.md` § Abuse posture already records for the L4 layer: quiet is
/// legitimate, and a *browser* client is quiet by construction — the W3C
/// WebSocket API exposes no ping primitive, so a web client sends nothing at
/// all on an idle connection (the sanctioned priority-#1 divergence, same §).
/// An idle timeout would therefore cut every idle browser tab. A Ping does not:
/// RFC 6455 makes the Pong mandatory and the browser's WS stack answers it
/// *below* JavaScript, so a live-but-silent browser re-arms the deadline
/// without its application code participating at all. The nest asks; every
/// live peer answers whether or not it has anything to say.
#[derive(Clone, Copy, Debug)]
pub struct WsHeartbeatPolicy {
    /// Period between nest-initiated WS Ping frames.
    pub ping_interval: std::time::Duration,
    /// Close the connection if no inbound frame of any kind (the Pong, or any
    /// other traffic) arrives within this window.
    pub liveness_timeout: std::time::Duration,
}

impl Default for WsHeartbeatPolicy {
    /// Mirrors the *client* half's constants rather than restating them, so the
    /// two halves of one heartbeat cannot drift apart:
    /// `fauna_ws_substrate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT}`
    /// (30 s Ping / 60 s dead-link, the latter 2× the former).
    fn default() -> Self {
        Self {
            ping_interval: fauna_ws_substrate::KEEPALIVE_INTERVAL,
            liveness_timeout: fauna_ws_substrate::KEEPALIVE_TIMEOUT,
        }
    }
}

/// What the heartbeat wants a connection loop to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beat {
    /// Send the peer a `Message::Ping`.
    Ping,
    /// A full `liveness_timeout` passed with no inbound frame of any kind: the
    /// peer is gone. The loop should tear the connection down exactly as it does
    /// on a peer close — a peer this silent would never read a close frame.
    Dead,
}

/// The server half of the heartbeat ([`WsHeartbeatPolicy`]) for the connection
/// loops that own **both** directions of their socket in a single task.
///
/// The two-task loop `routes::run_connection` cannot use this and is not
/// expected to: its sink is owned by a separate outbound
/// task, so the Ping and the deadline necessarily live on opposite sides of a
/// channel. The *mechanism* is identical either way, and `transport.md`
/// § Connection lifecycle describes it once for both shapes.
///
/// Usage is one `tokio::select!` arm, not two — that is why [`Self::next_beat`]
/// returns a [`Beat`] rather than exposing the ping timer and the deadline
/// separately: two arms would each need `&mut self`.
///
/// ```ignore
/// let mut hb = ServerHeartbeat::new(policy);
/// loop {
///     tokio::select! {
///         beat = hb.next_beat() => match beat {
///             Beat::Ping => if socket.send(Message::Ping(Bytes::new())).await.is_err() { break },
///             Beat::Dead => { tracing::warn!("…"); break }
///         },
///         msg = socket.next() => { hb.re_arm(); /* … */ }
///     }
/// }
/// ```
pub struct ServerHeartbeat {
    ping_tick: tokio::time::Interval,
    /// Absolute, so the window measures silence *from the peer* rather than
    /// restarting on every pass of the caller's loop.
    deadline: tokio::time::Instant,
    liveness_timeout: std::time::Duration,
}

impl ServerHeartbeat {
    pub fn new(policy: WsHeartbeatPolicy) -> Self {
        // First Ping lands one interval into the connection, not at handshake
        // time — `interval_at` rather than `interval`, whose first tick is
        // immediate. `Delay` (not the default `Burst`) because a runtime stall
        // must not produce a rapid volley of Pings once the task is scheduled
        // again: one Ping per elapsed window is the whole signal.
        let mut ping_tick = tokio::time::interval_at(
            tokio::time::Instant::now() + policy.ping_interval,
            policy.ping_interval,
        );
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            ping_tick,
            deadline: tokio::time::Instant::now() + policy.liveness_timeout,
            liveness_timeout: policy.liveness_timeout,
        }
    }

    /// Resolve when the loop should ping, or when it should give up.
    ///
    /// **Cancel-safe**, so it is sound as a `tokio::select!` arm: both inner
    /// futures are cancel-safe and neither holds progress that dropping would
    /// discard (the deadline is absolute state on `self`, and `Interval` keeps
    /// its own).
    pub async fn next_beat(&mut self) -> Beat {
        tokio::select! {
            biased;
            // Ranked first: once the window has passed the peer is gone, and
            // pinging a corpse only delays the teardown by another arm.
            _ = tokio::time::sleep_until(self.deadline) => Beat::Dead,
            _ = self.ping_tick.tick() => Beat::Ping,
        }
    }

    /// Any inbound frame proves the peer is alive. Call this the moment a frame
    /// arrives, *before* inspecting it — the Pong to our Ping, a stray Ping, and
    /// a real request all count exactly the same.
    pub fn re_arm(&mut self) {
        self.deadline = tokio::time::Instant::now() + self.liveness_timeout;
    }

    /// The configured window, for the `warn` a reaping loop logs.
    pub fn liveness_timeout(&self) -> std::time::Duration {
        self.liveness_timeout
    }
}

/// Principal sessions by `(account, principal_id)`.
type PrincipalRegistry = HashMap<([u8; 32], Vec<u8>), Vec<Arc<RpcConnection>>>;

/// Manages WebSocket connections keyed by ActorId. Lives in `AppState`.
pub struct WsState {
    next_conn_id: std::sync::Mutex<u64>,
    subs: std::sync::Mutex<HashMap<[u8; 32], Vec<Arc<RpcConnection>>>>,
    /// The principal registry: third-party principal sessions, keyed
    /// `(account, principal_id)` and kept apart from [`Self::subs`] so no
    /// account-scoped walk — Push, presence, the device roster — ever reaches
    /// one (`transport-connection.md` § *The principal session*).
    principals: std::sync::Mutex<PrincipalRegistry>,
    /// One wake per account for the HTTP events door's long-polls
    /// (`transport.md` § Push events → *Third-party event doors*): a
    /// scope-tagged change wakes every poll waiting on that account, which
    /// re-reads its own filtered answer.
    event_waiters: std::sync::Mutex<HashMap<[u8; 32], Arc<tokio::sync::Notify>>>,
    /// One webhook delivery in flight per principal (`events_webhook`), keyed
    /// by `principal_id`; the value is whether another reachable change
    /// landed while it ran, owing one more delivery when it ends — so a burst
    /// of writes costs the publisher's server at most two notifications.
    webhook_inflight: std::sync::Mutex<HashMap<Vec<u8>, bool>>,
    /// Flips `true` once `begin_shutdown` is called. Each live `run_connection`
    /// watches this; on the flip it stops reading new requests, drains its
    /// in-flight handlers, then closes the socket with WS 1001 (Going Away).
    /// Per `transport.md` § Graceful shutdown.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Heartbeat cadence for every connection `run_connection` drives.
    heartbeat: WsHeartbeatPolicy,
    /// The durable idempotency tier every **authenticated** connection this
    /// state subscribes inherits (`db/rpc_idempotency.rs`). `None` from
    /// [`WsState::new`] (unit tests — the per-connection LRU alone, the
    /// pre-tier behavior); production wires it with [`WsState::with_durable`].
    durable: Option<std::sync::Arc<crate::db::CacheDb>>,
}

impl Default for WsState {
    fn default() -> Self {
        Self::new()
    }
}

impl WsState {
    pub fn new() -> Self {
        Self::with_heartbeat(WsHeartbeatPolicy::default())
    }

    /// [`WsState::new`] with an explicit heartbeat cadence. Tests use it to run
    /// the real loop on a compressed clock; production always takes the
    /// [`Default`].
    pub fn with_heartbeat(heartbeat: WsHeartbeatPolicy) -> Self {
        Self {
            next_conn_id: std::sync::Mutex::new(0),
            subs: std::sync::Mutex::new(HashMap::new()),
            principals: std::sync::Mutex::new(HashMap::new()),
            event_waiters: std::sync::Mutex::new(HashMap::new()),
            webhook_inflight: std::sync::Mutex::new(HashMap::new()),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            heartbeat,
            durable: None,
        }
    }

    /// [`WsState::new`] with the durable idempotency tier wired — what
    /// production (`AppState` construction) uses, so every authenticated
    /// connection consults + feeds `rpc_idempotency` and a replay across a
    /// reconnect returns the original Reply (`account-data-plane.md` § The
    /// offline-mutation contract → *Nest-side durable idempotency*).
    pub fn with_durable(db: std::sync::Arc<crate::db::CacheDb>) -> Self {
        Self {
            durable: Some(db),
            ..Self::new()
        }
    }

    /// The heartbeat cadence `run_connection` drives on each connection.
    pub fn heartbeat(&self) -> WsHeartbeatPolicy {
        self.heartbeat
    }

    /// Begin a graceful shutdown: signal every live connection to drain its
    /// in-flight requests and close with WS 1001. Idempotent. Called from the
    /// `main.rs` SIGTERM path. Per `transport.md` § Graceful shutdown.
    pub fn begin_shutdown(&self) {
        // `send_replace`, never `send`: this watch carries a *state*, and a
        // plain `watch::send` reports `Err` and **leaves the stored value
        // alone** when no receiver is alive at that instant. `with_heartbeat`
        // drops the channel's original receiver, so an idle nest — SIGTERM
        // arriving with no connection subscribed, the ordinary case for a
        // redeploy between clients — discarded the flag outright:
        // `is_shutting_down` kept answering `false`, and every connection
        // accepted *after* the signal subscribed to a `false` it would never
        // see flip, serving normally instead of draining and closing 1001. The
        // same class as the client's connection-state watch
        // (`transport.md` § Connection lifecycle). Pinned by
        // `begin_shutdown_is_visible_with_no_connection_subscribed`.
        self.shutdown_tx.send_replace(true);
    }

    /// `true` once `begin_shutdown` has been called.
    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown_tx.borrow()
    }

    /// A receiver each connection awaits to learn when shutdown begins.
    pub fn subscribe_shutdown(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    /// Subscribe a new connection for `actor_id`, with no session identity and
    /// no device binding — [`Self::subscribe_with_session`] with `None, None`.
    ///
    /// Production has no caller: the authenticated upgrade always knows the
    /// `token_id` it validated, and a connection without one is invisible to
    /// the per-token teardown ([`RpcConnection::token_id`]). It stays for the
    /// dozen tests that only ever cared which actor a Push went to.
    pub fn subscribe(&self, actor_id: [u8; 32]) -> (Arc<RpcConnection>, mpsc::Receiver<Bytes>) {
        self.subscribe_with_session(actor_id, None, None)
    }

    /// Subscribe a new connection for `actor_id`, remembering **which session**
    /// (`token_id`) its bearer was and **which device** (`bound_device_key`)
    /// minted it. Returns the connection handle plus the receive end of the
    /// outbound bounded channel.
    ///
    /// Both come from `TokenStore::validate_with_session` at the upgrade: the
    /// id is what makes `fauna.sessions.{revoke,revoke_all}` able to close the
    /// sockets of *one* session rather than all of the actor's
    /// ([`RpcConnection::token_id`]); the key is what makes
    /// `fauna.sync.devices.list` able to answer `online` for the device this
    /// session belongs to ([`RpcConnection::bound_device_key`]).
    ///
    /// The WS upgrade does not call this directly: it goes through
    /// [`crate::routes::AppState::register_upgraded_connection`], which re-reads
    /// the session right after this registration so a revoke that swept `subs`
    /// before the connection joined it still closes the connection.
    pub fn subscribe_with_session(
        &self,
        actor_id: [u8; 32],
        token_id: Option<String>,
        bound_device_key: Option<[u8; 32]>,
    ) -> (Arc<RpcConnection>, mpsc::Receiver<Bytes>) {
        let (ws_tx, ws_rx) = mpsc::channel::<Bytes>(WS_OUTBOUND_BOUND);
        let conn_id = {
            let mut next = self.next_conn_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        // Authenticated connections do not capture `ConnectInfo`; `peer_addr` is
        // `None` here, and that is deliberate rather than pending.
        //
        // ⚠ The old reason given here — "no loopback-gated kind is reachable on
        // them, those are pre-identity" — was FALSE: the dispatcher's gate (1d)
        // exempts pre-identity kinds from the class allowlist precisely so an
        // authenticated connection *may* call them. What actually holds is the
        // opposite, and it is why capturing `ConnectInfo` here would be a
        // regression: the loopback gate fails CLOSED on `None`, so leaving
        // `peer_addr` absent is what refuses an authenticated caller of
        // `bridges.request_enrollment`. The pre-identity throttles no longer
        // depend on this field for authenticated callers either — they key on
        // `actor_id` (`anonymous_rate_limit::check_conn`). 2026-08-24.
        let conn = Arc::new(RpcConnection::new(
            conn_id,
            actor_id,
            ws_tx,
            false,
            None,
            token_id,
            bound_device_key,
            None,
            self.durable.clone(),
        ));
        self.subs
            .lock()
            .unwrap()
            .entry(actor_id)
            .or_default()
            .push(Arc::clone(&conn));
        (conn, ws_rx)
    }

    /// Subscribe an **anonymous** (pre-identity) connection. No actor is bound,
    /// so it is NOT registered in `subs` (it receives no Push events) and needs
    /// no `remove` on teardown. The `actor_id` is a zero placeholder; the
    /// dispatcher routes it against `pre_identity_allowlist`. Per `transport.md`
    /// § Pre-identity (anonymous) connection.
    pub fn subscribe_anonymous(
        &self,
        peer_addr: Option<std::net::SocketAddr>,
    ) -> (Arc<RpcConnection>, mpsc::Receiver<Bytes>) {
        let (ws_tx, ws_rx) = mpsc::channel::<Bytes>(WS_OUTBOUND_BOUND);
        let conn_id = {
            let mut next = self.next_conn_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        // No durable tier: the placeholder actor would alias every anonymous
        // caller into one (actor, key) namespace.
        let conn = Arc::new(RpcConnection::new(
            conn_id, [0u8; 32], ws_tx, true, None, None, None, peer_addr, None,
        ));
        (conn, ws_rx)
    }

    /// Subscribe a third-party **principal session** under `binding`. It joins
    /// the principal registry, never [`Self::subs`]: the actor slot is the zero
    /// placeholder, no Push reaches it, and the durable idempotency tier is
    /// off (its key is per actor, and the placeholder would alias every
    /// principal into one namespace — the anonymous connection's reason).
    ///
    /// The upgrade does not call this directly: it goes through
    /// [`crate::principal_session::register_principal_connection`], which
    /// re-reads the principal row right after this registration so a revoke
    /// that swept the registry before the connection joined still closes it.
    pub fn subscribe_principal(
        &self,
        binding: crate::principal_handlers::PrincipalBinding,
    ) -> (Arc<RpcConnection>, mpsc::Receiver<Bytes>) {
        let (ws_tx, ws_rx) = mpsc::channel::<Bytes>(WS_OUTBOUND_BOUND);
        let conn_id = {
            let mut next = self.next_conn_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        let key = (binding.account, binding.principal_id.clone());
        let conn = Arc::new(RpcConnection::new(
            conn_id,
            [0u8; 32],
            ws_tx,
            false,
            Some(binding),
            None,
            None,
            None,
            None,
        ));
        self.principals
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(Arc::clone(&conn));
        (conn, ws_rx)
    }

    /// Every live principal session acting for `account` — what the events
    /// doors' push walks (`crate::events_doors::on_scope_changed`), which
    /// resolves and filters each one before a frame goes out.
    pub fn account_principal_sessions(&self, account: &[u8; 32]) -> Vec<Arc<RpcConnection>> {
        self.principals
            .lock()
            .unwrap()
            .iter()
            .filter(|((a, _), _)| a == account)
            .flat_map(|(_, v)| v.iter().map(Arc::clone))
            .collect()
    }

    /// Wake every HTTP events long-poll waiting on `account`; each re-reads
    /// its own filtered answer.
    pub fn wake_event_waiters(&self, account: &[u8; 32]) {
        if let Some(notify) = self.event_waiters.lock().unwrap().get(account) {
            notify.notify_waiters();
        }
    }

    /// Claim the webhook delivery for `principal_id`: `true` when the caller
    /// now owns it and must deliver; `false` when one is already in flight,
    /// which is then owed one more delivery ([`Self::webhook_end`]).
    pub fn webhook_begin(&self, principal_id: &[u8]) -> bool {
        match self
            .webhook_inflight
            .lock()
            .unwrap()
            .entry(principal_id.to_vec())
        {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                *e.get_mut() = true;
                false
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(false);
                true
            }
        }
    }

    /// A delivery for `principal_id` ended: `true` when a change landed
    /// meanwhile — the caller keeps ownership and delivers once more —
    /// `false` when it is released.
    pub fn webhook_end(&self, principal_id: &[u8]) -> bool {
        let mut inflight = self.webhook_inflight.lock().unwrap();
        match inflight.get_mut(principal_id) {
            Some(again) if *again => {
                *again = false;
                true
            }
            _ => {
                inflight.remove(principal_id);
                false
            }
        }
    }

    /// The wake an HTTP events long-poll for `account` waits on — see
    /// [`Self::wake_event_waiters`]. The caller enables its `Notified`
    /// BEFORE reading, so a change landing between the read and the wait is
    /// never lost.
    pub fn event_waiter(&self, account: &[u8; 32]) -> Arc<tokio::sync::Notify> {
        let mut waiters = self.event_waiters.lock().unwrap();
        // Drop the wakes no poll holds any more, so the map is bounded by
        // the accounts with a poll in flight.
        waiters.retain(|_, n| Arc::strong_count(n) > 1);
        Arc::clone(waiters.entry(*account).or_default())
    }

    /// Remove a principal session by its `conn_id`. Idempotent.
    pub fn remove_principal(&self, account: &[u8; 32], principal_id: &[u8], conn_id: u64) {
        let mut principals = self.principals.lock().unwrap();
        let key = (*account, principal_id.to_vec());
        if let Some(conns) = principals.get_mut(&key) {
            conns.retain(|c| c.conn_id != conn_id);
            if conns.is_empty() {
                principals.remove(&key);
            }
        }
    }

    /// How many sessions `(account, principal_id)` holds open right now — what
    /// the upgrade's per-principal concurrent-session cap reads.
    pub fn principal_sessions(&self, account: &[u8; 32], principal_id: &[u8]) -> usize {
        self.principals
            .lock()
            .unwrap()
            .get(&(*account, principal_id.to_vec()))
            .map_or(0, Vec::len)
    }

    /// Close every live session of one principal with **4401** — the socket
    /// half of `fauna.principals.revoke`, run after the row's delete commits
    /// (`transport-connection.md` § *The principal session* → *Revocation —
    /// three doors*, door (a)). Returns how many were signalled.
    pub fn disconnect_principal(&self, account: &[u8; 32], principal_id: &[u8]) -> usize {
        let conns: Vec<_> = self
            .principals
            .lock()
            .unwrap()
            .get(&(*account, principal_id.to_vec()))
            .map(|v| v.iter().map(Arc::clone).collect())
            .unwrap_or_default();
        Self::close_principal_sessions(account, &conns, "principal revoked")
    }

    /// Close every live principal session acting for `account` with **4401** —
    /// door (b): the account lost its own authority, so nothing acting for it
    /// keeps a socket. Called by every per-actor teardown
    /// ([`Self::disconnect_actor_sparing`]), so no authority-stripping path can
    /// close the account's own sockets and forget its principals'.
    pub fn disconnect_account_principals(&self, account: &[u8; 32]) -> usize {
        let conns: Vec<_> = self
            .principals
            .lock()
            .unwrap()
            .iter()
            .filter(|((a, _), _)| a == account)
            .flat_map(|(_, v)| v.iter().map(Arc::clone))
            .collect();
        Self::close_principal_sessions(account, &conns, "account authority revoked")
    }

    fn close_principal_sessions(
        account: &[u8; 32],
        conns: &[Arc<RpcConnection>],
        reason: &'static str,
    ) -> usize {
        for conn in conns {
            conn.revoke();
        }
        if !conns.is_empty() {
            tracing::info!(
                account = %hex::encode(account),
                sessions = conns.len(),
                reason,
                "closing live principal sessions with 4401"
            );
        }
        conns.len()
    }

    /// Remove a connection by its `conn_id`. Idempotent.
    pub fn remove(&self, actor_id: &[u8; 32], conn_id: u64) {
        let mut subs = self.subs.lock().unwrap();
        if let Some(conns) = subs.get_mut(actor_id) {
            conns.retain(|c| c.conn_id != conn_id);
            if conns.is_empty() {
                subs.remove(actor_id);
            }
        }
    }

    /// `true` if `actor_id` has at least one active connection.
    pub fn has_connections(&self, actor_id: &[u8; 32]) -> bool {
        let subs = self.subs.lock().unwrap();
        subs.get(actor_id).is_some_and(|c| !c.is_empty())
    }

    /// Tag `actor_id`'s live connection `conn_id` with the push device it serves
    /// (`fauna.push.presence`), replacing any earlier announce. `false` when no
    /// such connection is registered — the caller's socket is already gone.
    pub fn announce_device(&self, actor_id: &[u8; 32], conn_id: u64, device_id: &str) -> bool {
        let subs = self.subs.lock().unwrap();
        let Some(conn) = subs
            .get(actor_id)
            .and_then(|conns| conns.iter().find(|c| c.conn_id == conn_id))
        else {
            return false;
        };
        *conn.announced_device.lock().unwrap() = Some(device_id.to_owned());
        true
    }

    /// Replace what `actor_id`'s live connection `conn_id` serves for the relay
    /// (`fauna.sync.serve.announce`) with the already-admitted `announce`.
    /// `false` when no such connection is registered — the socket is gone, and
    /// its announce with it.
    pub fn announce_serving(
        &self,
        actor_id: &[u8; 32],
        conn_id: u64,
        announce: ServingAnnounce,
    ) -> bool {
        self.replace_serving(actor_id, conn_id, announce).is_some()
    }

    /// [`Self::announce_serving`], handing back the connection and the
    /// announce it replaced — what a foreign folder's withdrawal and renewal
    /// need. `None` when the socket is gone.
    pub fn replace_serving(
        &self,
        actor_id: &[u8; 32],
        conn_id: u64,
        announce: ServingAnnounce,
    ) -> Option<(Arc<RpcConnection>, Option<ServingAnnounce>)> {
        let subs = self.subs.lock().unwrap();
        let conn = subs
            .get(actor_id)
            .and_then(|conns| conns.iter().find(|c| c.conn_id == conn_id))?;
        let previous = conn.announced_serving.lock().unwrap().replace(announce);
        Some((Arc::clone(conn), previous))
    }

    /// The live, unrevoked connection of `actor_id` that announced it serves
    /// the foreign folder `channel_id` from `device_id` with `home_nest_id` as
    /// its home — the one connection a `fauna.federation.folder.chunk.wanted`
    /// from that nest is pushed on (`federation.md` § … → *Relay serving
    /// across nests*). `None` for an ask from any other nest.
    pub fn foreign_serving_connection(
        &self,
        actor_id: &[u8; 32],
        channel_id: &[u8; 32],
        device_id: &[u8; 32],
        home_nest_id: &[u8; 32],
    ) -> Option<Arc<RpcConnection>> {
        let subs = self.subs.lock().unwrap();
        subs.get(actor_id)?
            .iter()
            .find(|c| {
                !c.is_revoked()
                    && c.announced_serving
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|s| {
                            s.device_id == *device_id
                                && s.foreign.iter().any(|f| {
                                    f.channel_id == *channel_id && f.home_nest_id == *home_nest_id
                                })
                        })
            })
            .map(Arc::clone)
    }

    /// Every live, unrevoked connection — of any actor — that announced it
    /// serves the folder row `folder_id`: the relay's second candidate kind
    /// and a holder for the reachability verdict (`file-sync.md` § Relay
    /// serving; § Content reachability). Any actor, because a roster member's
    /// seat counts on the owner's terms; the announce admitted only owners and
    /// members of the row.
    pub fn announced_for_folder(&self, folder_id: i64) -> Vec<Arc<RpcConnection>> {
        let subs = self.subs.lock().unwrap();
        subs.values()
            .flatten()
            .filter(|c| c.serves_folder(folder_id))
            .map(Arc::clone)
            .collect()
    }

    /// Whether any connection is an [`Self::announced_for_folder`] candidate —
    /// the same set, without the snapshot.
    pub fn has_announced_for_folder(&self, folder_id: i64) -> bool {
        let subs = self.subs.lock().unwrap();
        subs.values().flatten().any(|c| c.serves_folder(folder_id))
    }

    /// Which of `actor_id`'s devices are present right now — the per-device
    /// push oracle (`apps/common.md` § Dispatch Logic): the device ids its live
    /// connections announced, and whether any live connection announced none
    /// (the compatibility rider's trigger). A snapshot, taken under one lock:
    /// the dispatch decides on the moment of the event, never on a later one.
    pub fn device_presence(&self, actor_id: &[u8; 32]) -> crate::push::DevicePresence {
        let subs = self.subs.lock().unwrap();
        let mut presence = crate::push::DevicePresence::default();
        for conn in subs.get(actor_id).into_iter().flatten() {
            match conn.announced_device() {
                Some(device_id) => {
                    presence.announced.insert(device_id);
                }
                None => presence.any_unannounced = true,
            }
        }
        presence
    }

    /// `true` if `actor_id` has at least one live connection **bound to** the
    /// device whose granted key is `device_key` — the WS-RPC half of
    /// `fauna.sync.devices.list`'s `online` ([`RpcConnection::bound_device_key`]).
    ///
    /// Scoped to the actor's own entry by construction, exactly as every
    /// per-token teardown is: a key is a 32-byte value carrying no actor, and
    /// the roster asking about it is one actor's roster.
    pub fn has_connection_bound_to(&self, actor_id: &[u8; 32], device_key: &[u8; 32]) -> bool {
        let subs = self.subs.lock().unwrap();
        subs.get(actor_id).is_some_and(|conns| {
            conns
                .iter()
                .any(|c| c.bound_device_key.as_ref() == Some(device_key))
        })
    }

    /// Total number of active connections across all actors.
    /// Every live authenticated connection — the account sockets and the
    /// principal sessions alike, since a serving-generation teardown drains
    /// both.
    pub fn connection_count(&self) -> usize {
        let actors: usize = self.subs.lock().unwrap().values().map(Vec::len).sum();
        let principals: usize = self.principals.lock().unwrap().values().map(Vec::len).sum();
        actors + principals
    }

    /// Terminate every live connection held by `actor_id`, closing each with WS
    /// **4401** (auth expired / invalid). Returns how many were signalled.
    /// Idempotent, non-blocking, and safe to call for an actor with no
    /// connections. Per `transport.md` § Connection lifecycle → *Revocation
    /// teardown*.
    ///
    /// The per-actor twin of [`WsState::begin_shutdown`]: same `watch`-flag
    /// mechanism, but scoped to one actor, and it closes **4401 rather than
    /// 1001** and does **not** drain. 1001 tells the client to reconnect with
    /// the bearer it has; 4401 tells it to clear that bearer and re-authenticate
    /// — which is what a revoked actor must do, and which then fails at the
    /// handshake (`auth_core` refuses a suspended, locked-out, or unregistered
    /// actor a token), so the reconnect loop backs off instead of spinning.
    ///
    /// The drain is deliberately skipped: `fauna.sessions.lockout` is an
    /// *emergency* control, so the socket dies now rather than after in-flight
    /// replies flush. In-flight handlers are left to finish rather than aborted
    /// — each was already authorized when it was dispatched, each is bounded by
    /// its deadline, and each writes into a channel whose reader is gone; the
    /// alternative (aborting mid-handler at an arbitrary `.await`) could tear a
    /// multi-statement write for no security gain.
    ///
    /// **Every caller must revoke the actor's tokens too** — this closes the
    /// sockets an actor *has*, while `TokenStore::revoke_actor` stops it opening
    /// a new one with the bearer it already holds. Neither alone is sufficient.
    pub fn disconnect_actor(&self, actor_id: &[u8; 32]) -> usize {
        self.disconnect_actor_sparing(actor_id, None)
    }

    /// [`Self::disconnect_actor`], with one connection allowed to finish
    /// answering the request that caused the revocation.
    ///
    /// `spare` is `(conn_id, correlation_id)` — the connection a handler is
    /// running on, and the request it is running for. That connection gets
    /// [`RpcConnection::revoke_after_reply`] instead of
    /// [`RpcConnection::revoke`]: it stops dispatching at the same instant as
    /// every other, but its answer goes out before the 4401. Every other
    /// connection of the actor — the second socket a seed thief is holding, the
    /// case the succession ceremony exists to end — is torn down unsparingly.
    ///
    /// A `spare` naming a connection this actor does not hold is not an error:
    /// the caller learns its own identity from the dispatch task-local, and a
    /// handler revoking an actor that is *not* its caller (an admin suspending
    /// a user) passes one that simply matches nothing.
    pub fn disconnect_actor_sparing(
        &self,
        actor_id: &[u8; 32],
        spare: Option<(u64, u64)>,
    ) -> usize {
        // Door (b) of the principal session: the account's principals lose
        // their socket with it. Swept here, in the one per-actor body every
        // actor-wide teardown reaches, rather than at each call site — a site
        // that closed the account's sockets and forgot its principals' is the
        // un-adopted-mechanism failure `transport-connection.md` § *Revocation
        // teardown* records three times over. No spare: a principal session
        // never carries the request that revoked its account.
        self.disconnect_where(actor_id, |_| true, spare, "authority revoked")
            + self.disconnect_account_principals(actor_id)
    }

    /// Close every live connection of `actor_id` that was upgraded with the
    /// bearer filed under `token_id` — the socket half of
    /// `fauna.sessions.revoke`, and the **per-token twin** of
    /// [`Self::disconnect_actor`].
    ///
    /// The actor's other sessions are untouched, which is the entire
    /// difference and the whole reason this cannot be `disconnect_actor`:
    /// dismissing one session the user does not recognize must not sign every
    /// device they own out. Connections with no recorded `token_id` never
    /// match (see [`RpcConnection::token_id`]).
    ///
    /// `spare` is `(conn_id, correlation_id)` with exactly the meaning
    /// [`Self::disconnect_actor_sparing`] gives it, and matters here for a
    /// case the per-actor form barely meets: `fauna.sessions.revoke` is always
    /// self-directed (the handler's ownership check refuses any other actor's
    /// token), so the session being ended is regularly the caller's **own**,
    /// and its Reply is the only evidence the app gets that the revoke
    /// committed. A `spare` naming a connection outside the closed set simply
    /// matches nothing.
    ///
    /// **Every caller must revoke the token too** —
    /// `TokenStore::revoke_by_token_id`; the helper that does both halves by
    /// construction is [`crate::routes::AppState::revoke_session_authority`].
    pub fn disconnect_token_id(
        &self,
        actor_id: &[u8; 32],
        token_id: &str,
        spare: Option<(u64, u64)>,
    ) -> usize {
        self.disconnect_where(
            actor_id,
            |conn| conn.token_id.as_deref() == Some(token_id),
            spare,
            "session revoked",
        )
    }

    /// Close every live connection of `actor_id` **except** those upgraded with
    /// `keep_token_id` — the socket half of `fauna.sessions.revoke_all`.
    ///
    /// A `keep_token_id` matching none of the actor's sessions closes them all,
    /// exactly as `TokenStore::revoke_all_except_token_id` revokes them all:
    /// that is the documented renewal race (`devices.md` § The client's own
    /// session), where the caller's own newest bearer is the one revoked. It is
    /// harmless *because the next request re-mints* — which is true only if the
    /// caller gets its answer, so `spare` is load-bearing on this arm rather
    /// than incidental.
    ///
    /// **Every caller must revoke the tokens too** —
    /// `TokenStore::revoke_all_except_token_id`; the helper that does both
    /// halves by construction is
    /// [`crate::routes::AppState::revoke_other_sessions_authority`].
    pub fn disconnect_actor_except_token_id(
        &self,
        actor_id: &[u8; 32],
        keep_token_id: &str,
        spare: Option<(u64, u64)>,
    ) -> usize {
        self.disconnect_where(
            actor_id,
            |conn| conn.token_id.as_deref() != Some(keep_token_id),
            spare,
            "other sessions revoked",
        )
    }

    /// Close every live connection of `actor_id` whose bearer `device_key`
    /// minted ([`RpcConnection::bound_device_key`]) — the socket half of device
    /// removal (`fauna.sync.devices.delete`, `fauna.sync.device_grant.revoke`,
    /// graduation's severance of a marked guardian device), and the
    /// **per-device twin** of [`Self::disconnect_token_id`].
    ///
    /// Keyed on the binding rather than on the `token_id`s the removal dropped,
    /// because the binding outlives the rows: a socket keeps its binding past
    /// its bearer's expiry, so a socket whose row had already aged out is
    /// still closed. The actor's direct sign-ins and other devices are
    /// untouched. The custody handshake's overloaded tag never matches — it
    /// was excluded from the binding at the upgrade ([`bound_device_key_for`]).
    ///
    /// `spare` is `(conn_id, correlation_id)` with the meaning
    /// [`Self::disconnect_actor_sparing`] gives it: the grant revoke and the
    /// delete are both presentable by the device being removed, over its own
    /// session, and their Reply is its only evidence the removal committed.
    ///
    /// **Every caller must revoke the tokens too** —
    /// `TokenStore::revoke_minted_by`; the helper that does both halves by
    /// construction is [`crate::routes::AppState::revoke_device_authority`].
    pub fn disconnect_device_key(
        &self,
        actor_id: &[u8; 32],
        device_key: &[u8; 32],
        spare: Option<(u64, u64)>,
    ) -> usize {
        self.disconnect_where(
            actor_id,
            |conn| conn.bound_device_key.as_ref() == Some(device_key),
            spare,
            "device revoked",
        )
    }

    /// The one teardown body, shared by the per-actor, per-token and
    /// per-device forms.
    ///
    /// `select` runs over the actor's connections only — the map is indexed by
    /// actor and this never widens past that entry, which is what keeps a
    /// teardown keyed on a `token_id` (a 16-hex string carrying no actor) from
    /// reaching another identity's socket.
    fn disconnect_where(
        &self,
        actor_id: &[u8; 32],
        select: impl Fn(&RpcConnection) -> bool,
        spare: Option<(u64, u64)>,
        reason: &'static str,
    ) -> usize {
        let conns: Vec<_> = self
            .connections_for(actor_id)
            .into_iter()
            .filter(|c| select(c))
            .collect();
        let mut spared = 0usize;
        for conn in &conns {
            match spare {
                Some((conn_id, correlation_id)) if conn.conn_id == conn_id => {
                    conn.revoke_after_reply(correlation_id);
                    spared += 1;
                }
                _ => conn.revoke(),
            }
        }
        if !conns.is_empty() {
            tracing::info!(
                actor = %hex::encode(actor_id),
                connections = conns.len(),
                spared,
                reason,
                "closing live connections with 4401"
            );
        }
        conns.len()
    }

    /// Snapshot the live connection set for `actor_id`. Returns a clone
    /// of each `Arc<RpcConnection>` so the caller can drop the lock
    /// before doing any awaits.
    pub fn connections_for(&self, actor_id: &[u8; 32]) -> Vec<Arc<RpcConnection>> {
        let subs = self.subs.lock().unwrap();
        subs.get(actor_id)
            .map(|v| v.iter().map(Arc::clone).collect())
            .unwrap_or_default()
    }

    /// Emit a typed `PushEvent` to every subscribed connection for `actor_id`.
    /// Per spec § 1.5 (push events + seq) and § 1.6 (backpressure handling).
    ///
    /// On per-connection bounded-channel overflow: increments
    /// `dropped_pushes` and sets `needs_resync`. The next successful emit
    /// (or the per-connection ResyncRequired coalesce timer) sends a
    /// `fauna.protocol.resync_required` Push frame in response.
    pub fn notify_push(&self, actor_id: &[u8; 32], event: fauna_protocol::PushEvent) {
        Self::emit_push(self.connections_for(actor_id), &event);
    }

    /// Emit `event` only to `actor_id`'s live connections that announced
    /// `device_id` (`fauna.push.presence`) — a `ws-device` push row's delivery
    /// (`apps/common.md` § Push Notifications → *Transports*). Returns how many
    /// connections it was offered to; `0` means the device is not live, and
    /// nothing is queued for it.
    pub fn notify_push_to_device(
        &self,
        actor_id: &[u8; 32],
        device_id: &str,
        event: fauna_protocol::PushEvent,
    ) -> usize {
        let conns: Vec<_> = self
            .connections_for(actor_id)
            .into_iter()
            .filter(|c| c.announced_device().as_deref() == Some(device_id))
            .collect();
        let offered = conns.len();
        Self::emit_push(conns, &event);
        offered
    }

    fn emit_push(conns: Vec<Arc<RpcConnection>>, event: &fauna_protocol::PushEvent) {
        // Encode the inner payload once. The `seq` differs per connection,
        // so the full Push frame is encoded per-connection below.
        let kind = event.kind().to_string();
        let payload = match encode_push_payload(event) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(kind = %kind, error = %e, "notify_push: payload encode failed");
                return;
            }
        };
        for conn in conns {
            Self::send_push_frame(&conn, &kind, &payload);
        }
    }

    /// Emit `event` on the one connection `conn` and say whether it was
    /// queued — the relay's ask (`fauna.sync.chunk.wanted`), which goes to the
    /// seat it names and nowhere else, and whose caller stops waiting at once
    /// on `false` rather than at the fetch deadline. Same encoding, `seq` and
    /// overflow accounting as [`Self::notify_push`].
    pub fn push_to_connection(
        conn: &Arc<RpcConnection>,
        event: &fauna_protocol::PushEvent,
    ) -> bool {
        let kind = event.kind().to_string();
        match encode_push_payload(event) {
            Ok(payload) => Self::send_push_frame(conn, &kind, &payload),
            Err(e) => {
                tracing::warn!(kind = %kind, error = %e, "push_to_connection: payload encode failed");
                false
            }
        }
    }

    /// Frame and queue one already-encoded push payload on `conn`; `true` iff
    /// it reached the outbound queue.
    fn send_push_frame(
        conn: &Arc<RpcConnection>,
        kind: &str,
        payload: &fauna_protocol::Value,
    ) -> bool {
        let seq = conn.next_push_seq();
        let frame = fauna_protocol::Frame::Push(fauna_protocol::Push {
            ty: fauna_protocol::Push::TYPE,
            kind: kind.to_string(),
            payload: payload.clone(),
            seq,
        });
        let bytes = match fauna_protocol::encode_frame(&frame) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(kind = %kind, error = %e, "notify_push: frame encode failed");
                return false;
            }
        };
        match conn.ws_tx.try_send(bytes) {
            Ok(()) => {
                // On every successful Push emit, coalesce + flush
                // any pending ResyncRequired marker into the same
                // outbound queue. Per spec § 1.6.
                Self::flush_resync_if_needed(conn);
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                conn.dropped_pushes.fetch_add(1, Ordering::Relaxed);
                conn.needs_resync.store(true, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Connection writer is gone; reaper will remove on socket close.
                false
            }
        }
    }

    /// If the connection's `needs_resync` flag is set, encode and queue a
    /// single `fauna.protocol.resync_required` Push frame with the
    /// accumulated `dropped_count`. The flag and counter are cleared after
    /// a successful queue. Returns `true` if a resync frame was queued.
    ///
    /// Caller: the on-emit fold inside `notify_push` AND the per-connection
    /// 5-second coalesce timer in `routes::handle_ws`.
    pub fn flush_resync_if_needed(conn: &Arc<RpcConnection>) -> bool {
        if !conn.needs_resync.load(Ordering::Relaxed) {
            return false;
        }
        // Atomically claim the current drop count and clear flag/counter.
        let dropped = conn.dropped_pushes.swap(0, Ordering::Relaxed);
        conn.needs_resync.store(false, Ordering::Relaxed);
        if dropped == 0 {
            return false;
        }
        let payload = fauna_protocol::push_events::ResyncRequiredPayload {
            dropped_count: dropped,
            extra: std::collections::BTreeMap::new(),
        };
        let cbor = match fauna_protocol::encode_canonical(&payload) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let cbor_value: fauna_protocol::Value = match fauna_cbor::decode_strict(&cbor) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let seq = conn.next_push_seq();
        let frame = fauna_protocol::Frame::Push(fauna_protocol::Push {
            ty: fauna_protocol::Push::TYPE,
            kind: "fauna.protocol.resync_required".to_string(),
            payload: cbor_value,
            seq,
        });
        let bytes = match fauna_protocol::encode_frame(&frame) {
            Ok(b) => b,
            Err(_) => return false,
        };
        // Use try_send; if even the resync emit fails, leave needs_resync=false
        // (next push will not re-set it unless a new drop happens) — the
        // 5-second timer in handle_ws will retry on next tick if drops resume.
        match conn.ws_tx.try_send(bytes) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Re-arm: next push (or next timer tick) will retry.
                conn.dropped_pushes.fetch_add(dropped, Ordering::Relaxed);
                conn.needs_resync.store(true, Ordering::Relaxed);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

/// Compute a content_id hex from inbox body bytes. Used as the typed
/// pointer in `PushEvent::InboxItem`.
pub fn inbox_content_id_hex(body: &[u8]) -> String {
    hex::encode(blake3::hash(body).as_bytes())
}

fn encode_push_payload(
    event: &fauna_protocol::PushEvent,
) -> Result<fauna_protocol::Value, Box<dyn std::error::Error + Send + Sync>> {
    use fauna_protocol::{PushEvent, Value, encode_canonical};
    // Encode → re-decode as Value: gives us a generic dag-cbor node payload
    // with canonical encoding of the inner struct fields.
    macro_rules! enc {
        ($p:expr) => {{
            let bytes = encode_canonical($p)?;
            fauna_cbor::decode_strict::<Value>(&bytes).map_err(Into::into)
        }};
    }
    match event {
        PushEvent::Knock(p) => enc!(p),
        PushEvent::AccountUpdated(p) => enc!(p),
        PushEvent::Notification(p) => enc!(p),
        PushEvent::PeerWake(p) => enc!(p),
        PushEvent::CalendarChanged(p) => enc!(p),
        PushEvent::AddressBookChanged(p) => enc!(p),
        PushEvent::ChannelMessage(p) => enc!(p),
        PushEvent::Welcome(p) => enc!(p),
        PushEvent::InboxItem(p) => enc!(p),
        PushEvent::ResyncRequired(p) => enc!(p),
        PushEvent::SegmentsChanged(p) => enc!(p),
        PushEvent::MailReceived(p) => enc!(p),
        PushEvent::MailFlagsChanged(p) => enc!(p),
        PushEvent::PushNotification(p) => enc!(p),
        PushEvent::SyncChanged(p) => enc!(p),
        PushEvent::SyncChunkWanted(p) => enc!(p),
        PushEvent::BridgeMailboxState(p) => enc!(p),
        PushEvent::BridgeConfigChanged(p) => enc!(p),
        PushEvent::BridgeAtprotoSessionsChanged(p) => enc!(p),
        PushEvent::AtprotoConsentRequested(p) => enc!(p),
        PushEvent::BridgeAtprotoPermissionSetRequested(p) => enc!(p),
        PushEvent::BridgeAtprotoProjectionReady(p) => enc!(p),
        PushEvent::BridgeAtprotoIssuerKeyRotated(p) => enc!(p),
        PushEvent::BridgeOutboundReady(p) => enc!(p),
        PushEvent::BridgeRescoreReady(p) => enc!(p),
        PushEvent::BridgeSpamBaselinePublish(p) => enc!(p),
        PushEvent::BridgeSpamModelUpdated(p) => enc!(p),
        PushEvent::BridgeSpamModelReset(p) => enc!(p),
        PushEvent::LeaseChanged(p) => enc!(p),
        PushEvent::BridgeImportProgress(p) => enc!(p),
        PushEvent::BridgeImportError(p) => enc!(p),
        PushEvent::BridgeImportComplete(p) => enc!(p),
        PushEvent::BridgeExportProgress(p) => enc!(p),
        PushEvent::BridgeExportError(p) => enc!(p),
        PushEvent::BridgeExportComplete(p) => enc!(p),
        PushEvent::BridgeConversationChanged(p) => enc!(p),
        PushEvent::Unknown(u) => Ok(u.payload.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_connections_returns_false_for_unknown_actor() {
        let ws = WsState::new();
        let actor = [1u8; 32];
        assert!(!ws.has_connections(&actor));
    }

    /// One `ok` payload as the `Ok(Bytes)` outcome `finish` takes — valid
    /// canonical CBOR, since `outcome_to_reply_value` strict-decodes it.
    fn ok_outcome(s: &str) -> Result<Bytes, RpcError> {
        Ok(Bytes::from(encode_canonical(&s).unwrap().to_vec()))
    }

    /// The durable tier end-to-end at the sink layer: an `ok` Reply recorded
    /// by one connection is served — REBUILT, not verbatim — to a *fresh*
    /// connection presenting the same key. This is the tier_1 twin of
    /// `tests/e2e-unified/tests/api/test_rpc_durable_idempotency.py`, and it
    /// reds if the `DispatchSink::lookup_idempotent` impl stops delegating to
    /// the inherent (durable-aware) lookup — the exact defect the tier_3
    /// caught on this slice's first run.
    #[tokio::test]
    async fn a_recorded_ok_reply_replays_rebuilt_on_a_fresh_connection() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let ws = WsState::with_durable(db);
        let actor = [7u8; 32];
        let key = [9u8; 16];

        let (conn1, mut _rx1) = ws.subscribe(actor);
        conn1
            .clone()
            .finish(
                1,
                key,
                // A mutation-class kind (OnlineOnly) — durably eligible.
                "fauna.admin.invite_codes.create".to_string(),
                ok_outcome("minted"),
            )
            .await;

        // A fresh connection = empty per-connection LRU; only the durable
        // tier can answer, and it must answer HitRebuild (frame bytes would
        // carry conn1's correlation_id).
        let (conn2, mut _rx2) = ws.subscribe(actor);
        match RpcConnection::lookup_idempotent(&conn2, &key).await {
            IdempotencyHit::HitRebuild { payload, ok } => {
                assert!(ok);
                let v: fauna_protocol::Value = fauna_cbor::decode_strict(&payload).unwrap();
                assert_eq!(
                    v,
                    fauna_cbor::decode_strict::<fauna_protocol::Value>(
                        &encode_canonical(&"minted").unwrap()
                    )
                    .unwrap()
                );
            }
            IdempotencyHit::Hit { .. } => panic!("durable hit must be HitRebuild, not frame bytes"),
            IdempotencyHit::TooLarge => panic!("small payload recorded as too_large"),
            IdempotencyHit::Miss => {
                panic!("the durable tier did not serve the recorded reply on a fresh connection")
            }
        }
    }

    /// Rule 2 (`db/rpc_idempotency.rs`): a `Read`-class kind is never durably
    /// recorded — a re-run read has no effect and the freshest answer is the
    /// better reply. Deleting the exemption turns every read into a durable
    /// row write; this pin is what reds.
    #[tokio::test]
    async fn a_read_class_kind_is_not_durably_recorded() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let ws = WsState::with_durable(db.clone());
        let actor = [7u8; 32];
        let key = [10u8; 16];

        let (conn, mut _rx) = ws.subscribe(actor);
        conn.clone()
            .finish(
                1,
                key,
                // Read per `fauna_protocol::offline_class`.
                "fauna.admin.invite_codes.list".to_string(),
                ok_outcome("listing"),
            )
            .await;
        assert_eq!(
            db.lookup_rpc_idempotent(&actor, &key, 0).await.unwrap(),
            None,
            "a Read-class reply must not be durably recorded"
        );
    }

    /// Rule 1 (`db/rpc_idempotency.rs`): an error Reply is never durably
    /// recorded — durably replaying a transient error would wedge the
    /// retrying intent on that error for the whole retention window.
    #[tokio::test]
    async fn an_error_reply_is_not_durably_recorded() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let ws = WsState::with_durable(db.clone());
        let actor = [7u8; 32];
        let key = [11u8; 16];

        let (conn, mut _rx) = ws.subscribe(actor);
        conn.clone()
            .finish(
                1,
                key,
                "fauna.admin.invite_codes.create".to_string(),
                Err(RpcError::new(
                    "fauna.protocol.timeout",
                    "error.protocol.timeout",
                )),
            )
            .await;
        assert_eq!(
            db.lookup_rpc_idempotent(&actor, &key, 0).await.unwrap(),
            None,
            "an error reply must not be durably recorded (a replay should re-run)"
        );
    }

    /// Rule 3's connection half: the anonymous connection has no durable tier
    /// at all — its placeholder actor would alias every anonymous caller into
    /// one (actor, key) namespace.
    #[tokio::test]
    async fn an_anonymous_connection_never_touches_the_durable_tier() {
        let db = std::sync::Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let ws = WsState::with_durable(db.clone());
        let key = [12u8; 16];

        let (conn, mut _rx) = ws.subscribe_anonymous(None);
        conn.clone()
            .finish(
                1,
                key,
                "fauna.admin.invite_codes.create".to_string(),
                ok_outcome("minted"),
            )
            .await;
        assert_eq!(
            db.lookup_rpc_idempotent(&[0u8; 32], &key, 0).await.unwrap(),
            None,
            "the anonymous placeholder actor must never gain durable rows"
        );
    }

    /// Cadence on a paused clock: Pings come one interval apart, and the first
    /// one lands an interval *into* the connection rather than at handshake.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_pings_one_interval_apart_starting_one_interval_in() {
        let mut hb = ServerHeartbeat::new(WsHeartbeatPolicy {
            ping_interval: std::time::Duration::from_secs(30),
            liveness_timeout: std::time::Duration::from_secs(60),
        });
        let start = tokio::time::Instant::now();
        assert_eq!(hb.next_beat().await, Beat::Ping);
        assert_eq!(
            tokio::time::Instant::now() - start,
            std::time::Duration::from_secs(30)
        );
        // A responsive peer answers, so the loop re-arms before the next beat.
        hb.re_arm();
        assert_eq!(hb.next_beat().await, Beat::Ping);
        assert_eq!(
            tokio::time::Instant::now() - start,
            std::time::Duration::from_secs(60)
        );
    }

    /// A peer that never answers is declared dead exactly one `liveness_timeout`
    /// after the connection opened — the Ping at 30 s does not postpone it,
    /// because only *inbound* traffic re-arms the deadline.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_declares_an_unanswering_peer_dead_after_one_window() {
        let mut hb = ServerHeartbeat::new(WsHeartbeatPolicy {
            ping_interval: std::time::Duration::from_secs(30),
            liveness_timeout: std::time::Duration::from_secs(60),
        });
        let start = tokio::time::Instant::now();
        assert_eq!(
            hb.next_beat().await,
            Beat::Ping,
            "the 30 s Ping comes first"
        );
        assert_eq!(
            hb.next_beat().await,
            Beat::Dead,
            "then the 60 s window closes"
        );
        assert_eq!(
            tokio::time::Instant::now() - start,
            std::time::Duration::from_secs(60)
        );
    }

    /// The load-bearing property, and the one that separates this from an
    /// inbound-idle timeout: a peer that sends nothing of its own but answers
    /// every Ping is never declared dead, however long it stays quiet.
    #[tokio::test(start_paused = true)]
    async fn heartbeat_never_reaps_a_silent_peer_that_answers() {
        let mut hb = ServerHeartbeat::new(WsHeartbeatPolicy {
            ping_interval: std::time::Duration::from_secs(30),
            liveness_timeout: std::time::Duration::from_secs(60),
        });
        // Ten windows' worth of an idle-but-responsive browser tab.
        for i in 0..20 {
            assert_eq!(
                hb.next_beat().await,
                Beat::Ping,
                "beat {i} must be a Ping: the peer answered every previous one"
            );
            hb.re_arm(); // the Pong, arriving below the peer's application code
        }
    }

    #[tokio::test]
    async fn subscribe_returns_handle_and_receiver() {
        let ws = WsState::new();
        let actor = [2u8; 32];
        let (conn, _rx) = ws.subscribe(actor);
        assert_eq!(conn.actor_id, actor);
        assert!(ws.has_connections(&actor));
        assert_eq!(ws.connection_count(), 1);
        ws.remove(&actor, conn.conn_id);
        assert!(!ws.has_connections(&actor));
        assert_eq!(ws.connection_count(), 0);
    }

    #[tokio::test]
    async fn connection_count_tracks_subscribe_and_remove() {
        let ws = WsState::new();
        let actor_a = [3u8; 32];
        let actor_b = [4u8; 32];
        assert_eq!(ws.connection_count(), 0);
        let (a1, _r1) = ws.subscribe(actor_a);
        let (a2, _r2) = ws.subscribe(actor_a);
        let (b1, _r3) = ws.subscribe(actor_b);
        assert_eq!(ws.connection_count(), 3);
        ws.remove(&actor_a, a1.conn_id);
        assert_eq!(ws.connection_count(), 2);
        ws.remove(&actor_a, a2.conn_id);
        assert_eq!(ws.connection_count(), 1);
        ws.remove(&actor_b, b1.conn_id);
        assert_eq!(ws.connection_count(), 0);
    }

    #[tokio::test]
    async fn next_push_seq_starts_at_one_and_ascends() {
        let ws = WsState::new();
        let actor = [6u8; 32];
        let (conn, _rx) = ws.subscribe(actor);
        assert_eq!(conn.next_push_seq(), 1);
        assert_eq!(conn.next_push_seq(), 2);
        assert_eq!(conn.next_push_seq(), 3);
    }

    use fauna_protocol::push_events::{AccountUpdatedPayload, KnockPayload};
    use fauna_protocol::{Frame, PushEvent, decode_frame};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn notify_push_emits_canonical_cbor_with_per_conn_seq() {
        let ws = WsState::new();
        let actor = [7u8; 32];
        let (conn, mut rx) = ws.subscribe(actor);

        let p1 = KnockPayload {
            sender_id: "abc".into(),
            summary: "hi".into(),
            ..Default::default()
        };
        ws.notify_push(&actor, PushEvent::Knock(p1.clone()));
        ws.notify_push(
            &actor,
            PushEvent::AccountUpdated(AccountUpdatedPayload {
                changes: vec!["handle".into()],
                timestamp: 1710000000,
                extra: BTreeMap::new(),
            }),
        );

        let bytes1 = rx.recv().await.unwrap();
        let bytes2 = rx.recv().await.unwrap();

        let f1 = decode_frame(&bytes1).unwrap();
        let f2 = decode_frame(&bytes2).unwrap();
        match (f1, f2) {
            (Frame::Push(a), Frame::Push(b)) => {
                assert_eq!(a.seq, 1);
                assert_eq!(b.seq, 2);
                assert_eq!(a.kind, "fauna.knock");
                assert_eq!(b.kind, "fauna.account.update");
            }
            _ => panic!("expected two Push frames"),
        }
        assert_eq!(conn.dropped_pushes.load(Ordering::Relaxed), 0);
        assert!(!conn.needs_resync.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn notify_push_overflow_increments_dropped_and_sets_resync_flag() {
        let ws = WsState::new();
        let actor = [8u8; 32];
        let (conn, _rx) = ws.subscribe(actor);
        // Saturate the bounded channel — capacity is 256.
        for _ in 0..(WS_OUTBOUND_BOUND + 5) {
            ws.notify_push(
                &actor,
                PushEvent::Knock(KnockPayload {
                    sender_id: "x".into(),
                    summary: "y".into(),
                    ..Default::default()
                }),
            );
        }
        // 256 should have landed; 5 should have been dropped.
        assert!(conn.dropped_pushes.load(Ordering::Relaxed) >= 5);
        assert!(conn.needs_resync.load(Ordering::Relaxed));
        // A *Push* overflow is explicitly NOT fatal — it degrades to
        // ResyncRequired (transport.md § Backpressure). Only a Reply is fatal.
        assert_eq!(
            conn.fatal_close(),
            None,
            "push overflow must not close the connection"
        );
    }

    // ── Reply-overflow → fatal 1011 (transport.md § Close codes) ──────
    //
    // These pin the *trigger* deterministically: a bounded channel filled to
    // its stated capacity, no wall-clock and no volume guessing (convention
    // 14). The other half — a committed fatal reason actually reaching the wire
    // as that u16 — is pinned over a real WebSocket in
    // `tests/ws_close_codes.rs`, which drives the identical `close_fatal` path
    // via the 4400 reasons. Neither half needs a saturated TCP buffer.

    /// Fill `ws_tx` to capacity so the next `try_send` is guaranteed to fail.
    /// `_rx` must be held (undrained) by the caller.
    fn saturate_outbound(conn: &Arc<RpcConnection>) {
        for _ in 0..WS_OUTBOUND_BOUND {
            conn.ws_tx
                .try_send(Bytes::from_static(b"x"))
                .expect("channel should accept up to its bound");
        }
    }

    #[tokio::test]
    async fn reply_overflow_on_finish_signals_fatal_1011() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([9u8; 32]);
        saturate_outbound(&conn);
        assert_eq!(conn.fatal_close(), None, "no fault before the overflow");

        Arc::clone(&conn)
            .finish(
                1,
                [0u8; 16],
                "fauna.test.kind".to_string(),
                Ok(Bytes::from_static(b"payload")),
            )
            .await;

        assert_eq!(
            conn.fatal_close(),
            Some(FatalCloseReason::ReplyOverflow),
            "a dropped Reply must be fatal, not merely logged — before 2026-07-31 \
             this site logged 'connection will be torn down' and tore down nothing, \
             so the caller waited out its whole deadline for an answer already lost"
        );
        assert_eq!(FatalCloseReason::ReplyOverflow.code(), 1011);
    }

    #[tokio::test]
    async fn reply_overflow_on_send_error_signals_fatal_1011() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([10u8; 32]);
        saturate_outbound(&conn);

        Arc::clone(&conn)
            .send_error(1, RpcError::new("fauna.test.boom", "boom"))
            .await;

        // An error Reply is still a Reply. Exempting it would leave a saturated
        // connection silently swallowing exactly the frames that tell a caller
        // its request failed.
        assert_eq!(
            conn.fatal_close(),
            Some(FatalCloseReason::ReplyOverflow),
            "an error Reply that cannot be delivered is fatal too"
        );
    }

    #[tokio::test]
    async fn reply_overflow_on_idempotent_replay_signals_fatal_1011() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([11u8; 32]);
        saturate_outbound(&conn);

        Arc::clone(&conn)
            .replay(1, Bytes::from_static(b"cached-reply-frame"))
            .await;

        // The replay path carries a Reply frame like any other; a client
        // auto-retrying into a saturated connection must learn the connection
        // is dead rather than hang.
        assert_eq!(
            conn.fatal_close(),
            Some(FatalCloseReason::ReplyOverflow),
            "a replayed Reply that cannot be delivered is fatal too"
        );
    }

    // ── A Reply's slot is reserved at admission (transport.md § Backpressure) ──
    //
    // The fatal rule above is for a peer that is *not draining*. A draining
    // peer whose queue was momentarily filled by pushes, or by its own burst of
    // replies, must never meet it: the slot its Reply goes out through was
    // taken from the queue's capacity when the request was admitted, so pushes
    // only ever fill what no admitted request holds.

    fn knock() -> PushEvent {
        PushEvent::Knock(KnockPayload {
            sender_id: "x".into(),
            summary: "y".into(),
            ..Default::default()
        })
    }

    /// A durable-tier record as `replay_rebuilt` takes it: the canonical CBOR
    /// of the Reply's payload `Value`.
    fn recorded_payload() -> Bytes {
        Bytes::from(encode_canonical(&"v").unwrap().to_vec())
    }

    #[tokio::test]
    async fn an_admitted_reply_survives_a_queue_filled_by_pushes() {
        let ws = WsState::new();
        let actor = [12u8; 32];
        let (conn, mut rx) = ws.subscribe(actor);
        let slot = conn
            .reserve_reply_slot()
            .await
            .expect("an open queue grants a slot");
        conn.hold_reply_slot(7, slot);

        // More pushes than the queue holds: everything past the unreserved
        // capacity degrades to ResyncRequired, as a push overflow should.
        for _ in 0..(WS_OUTBOUND_BOUND + 5) {
            ws.notify_push(&actor, knock());
        }
        assert!(conn.needs_resync.load(Ordering::Relaxed));

        Arc::clone(&conn)
            .finish(
                7,
                [1u8; 16],
                "fauna.test.kind".to_string(),
                ok_outcome("answer"),
            )
            .await;

        assert_eq!(
            conn.fatal_close(),
            None,
            "a Reply whose slot was reserved at admission cannot overflow"
        );
        let mut replies = 0;
        while let Ok(bytes) = rx.try_recv() {
            if let Ok(Frame::Reply(r)) = fauna_protocol::decode_frame(&bytes) {
                assert_eq!(r.correlation_id, 7);
                replies += 1;
            }
        }
        assert_eq!(replies, 1, "the Reply went out through its reserved slot");
    }

    #[tokio::test]
    async fn every_reply_emitter_sends_through_the_reserved_slot() {
        let ws = WsState::new();
        let (conn, mut rx) = ws.subscribe([13u8; 32]);
        for cid in 1..=3 {
            let slot = conn.reserve_reply_slot().await.expect("slot");
            conn.hold_reply_slot(cid, slot);
        }
        // Fill every unreserved place, so only the reservations remain.
        while conn.ws_tx.try_send(Bytes::from_static(b"x")).is_ok() {}

        Arc::clone(&conn)
            .send_error(1, RpcError::new("fauna.test.boom", "boom"))
            .await;
        Arc::clone(&conn)
            .replay(2, Bytes::from_static(b"cached-reply-frame"))
            .await;
        Arc::clone(&conn)
            .replay_rebuilt(3, recorded_payload(), true)
            .await;

        assert_eq!(conn.fatal_close(), None, "no emitter overflowed");
        let mut drained = 0;
        while rx.try_recv().is_ok() {
            drained += 1;
        }
        assert_eq!(
            drained, WS_OUTBOUND_BOUND,
            "the three Replies used the three reserved slots"
        );
    }

    #[tokio::test]
    async fn reserved_slots_cap_admission_at_the_queue_bound() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([14u8; 32]);
        for cid in 0..WS_OUTBOUND_BOUND as u64 {
            let slot = conn.reserve_reply_slot().await.expect("slot");
            conn.hold_reply_slot(cid, slot);
        }
        // The next admission parks — that is the per-connection cap, and the
        // reader loop parked on it is the backpressure.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), conn.reserve_reply_slot())
                .await
                .is_err(),
            "admission beyond the queue bound waits for a slot to free"
        );
        // A held slot dropped without a Reply (an encode failure) frees it.
        conn.release_reply_slot(1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), conn.reserve_reply_slot())
                .await
                .is_ok(),
            "a released slot admits the next request"
        );
    }

    #[tokio::test]
    async fn a_reply_into_a_closed_queue_is_not_an_overflow() {
        // The writer is gone (the socket closed — a 4401 revoke, a peer
        // close): there is nothing left to tear down, and reporting it as
        // saturation put every such close into the overflow log.
        for emitter in 0..4u8 {
            let ws = WsState::new();
            let (conn, rx) = ws.subscribe([20 + emitter; 32]);
            drop(rx);
            let c = Arc::clone(&conn);
            match emitter {
                0 => {
                    c.finish(1, [2u8; 16], "fauna.test.kind".into(), ok_outcome("a"))
                        .await
                }
                1 => {
                    c.send_error(1, RpcError::new("fauna.test.boom", "boom"))
                        .await
                }
                2 => c.replay(1, Bytes::from_static(b"cached")).await,
                _ => c.replay_rebuilt(1, recorded_payload(), true).await,
            }
            assert_eq!(
                conn.fatal_close(),
                None,
                "emitter {emitter}: a Closed queue is not a ReplyOverflow"
            );
        }
    }

    #[tokio::test]
    async fn the_first_fatal_reason_wins_and_later_ones_are_ignored() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([12u8; 32]);

        conn.signal_fatal(FatalCloseReason::ProtocolViolation);
        conn.signal_fatal(FatalCloseReason::ReplyOverflow);

        // The first fault is the cause; the rest are consequences. A protocol
        // violation makes the peer stop reading, which then overflows the Reply
        // channel — reporting 1011 there would blame the nest for what was the
        // client's own bug, and send it into reconnect-with-backoff instead of
        // surfacing the violation.
        assert_eq!(
            conn.fatal_close(),
            Some(FatalCloseReason::ProtocolViolation)
        );
        assert_eq!(FatalCloseReason::ProtocolViolation.code(), 4400);
    }

    #[tokio::test]
    async fn a_healthy_connection_has_no_fatal_reason() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([13u8; 32]);
        Arc::clone(&conn)
            .finish(
                1,
                [0u8; 16],
                "fauna.test.kind".to_string(),
                Ok(Bytes::from_static(b"ok")),
            )
            .await;
        assert_eq!(conn.fatal_close(), None);
    }

    #[tokio::test]
    async fn notify_push_with_two_connections_separate_seq_spaces() {
        let ws = WsState::new();
        let actor = [9u8; 32];
        let (c1, mut r1) = ws.subscribe(actor);
        let (c2, mut r2) = ws.subscribe(actor);
        let p = KnockPayload {
            sender_id: "abc".into(),
            summary: "hello".into(),
            ..Default::default()
        };
        ws.notify_push(&actor, PushEvent::Knock(p));

        let f1 = decode_frame(&r1.recv().await.unwrap()).unwrap();
        let f2 = decode_frame(&r2.recv().await.unwrap()).unwrap();
        let s1 = if let Frame::Push(p) = f1 {
            p.seq
        } else {
            panic!()
        };
        let s2 = if let Frame::Push(p) = f2 {
            p.seq
        } else {
            panic!()
        };
        assert_eq!(s1, 1);
        assert_eq!(s2, 1);
        assert_eq!(c1.seq.load(Ordering::Relaxed), 1);
        assert_eq!(c2.seq.load(Ordering::Relaxed), 1);
    }

    use fauna_protocol::push_events::ResyncRequiredPayload;

    #[tokio::test]
    async fn resync_emitted_after_drops_on_next_successful_push() {
        let ws = WsState::new();
        let actor = [10u8; 32];
        let (conn, mut rx) = ws.subscribe(actor);
        // Saturate by sending 256 + 5 = 261 pushes; receiver hasn't drained.
        for i in 0..(WS_OUTBOUND_BOUND + 5) {
            ws.notify_push(
                &actor,
                PushEvent::Knock(KnockPayload {
                    sender_id: format!("x{i}"),
                    summary: "y".into(),
                    ..Default::default()
                }),
            );
        }
        assert!(conn.needs_resync.load(Ordering::Relaxed));
        assert!(conn.dropped_pushes.load(Ordering::Relaxed) >= 5);
        // Drain a slot, then push once more — that successful push should
        // queue a ResyncRequired frame after the regular Push.
        rx.recv().await.unwrap(); // drains one slot
        rx.recv().await.unwrap(); // and another, to ensure room
        ws.notify_push(
            &actor,
            PushEvent::Knock(KnockPayload {
                sender_id: "trigger".into(),
                summary: "y".into(),
                ..Default::default()
            }),
        );
        // Drain the rest of the buffer searching for the resync frame.
        let mut found_resync = false;
        let mut frames_seen = 0;
        while let Ok(Some(b)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            frames_seen += 1;
            if let Ok(fauna_protocol::Frame::Push(p)) = fauna_protocol::decode_frame(&b)
                && p.kind == "fauna.protocol.resync_required"
            {
                let bytes = fauna_protocol::encode_canonical(&p.payload).unwrap();
                let payload: ResyncRequiredPayload = fauna_cbor::decode_strict(&bytes).unwrap();
                assert!(payload.dropped_count >= 5);
                found_resync = true;
                break;
            }
        }
        assert!(
            found_resync,
            "expected a resync frame after {frames_seen} frames"
        );
        // Flag should be cleared.
        assert!(!conn.needs_resync.load(Ordering::Relaxed));
        assert_eq!(conn.dropped_pushes.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn flush_resync_no_op_when_flag_clear() {
        let ws = WsState::new();
        let actor = [11u8; 32];
        let (conn, _rx) = ws.subscribe(actor);
        assert!(!WsState::flush_resync_if_needed(&conn));
    }

    #[tokio::test]
    async fn idempotent_miss_on_unknown_key() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([12u8; 32]);
        let key = [0u8; 16];
        match conn.lookup_idempotent(&key).await {
            IdempotencyHit::Miss => {}
            _ => panic!("expected Miss"),
        }
    }

    #[tokio::test]
    async fn idempotent_hit_returns_cached_payload() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([13u8; 32]);
        let key = [1u8; 16];
        let payload = Bytes::from_static(b"reply-bytes");
        conn.insert_idempotent(key, payload.clone()).await;
        match conn.lookup_idempotent(&key).await {
            IdempotencyHit::Hit { payload: got } => assert_eq!(got, payload),
            _ => panic!("expected Hit"),
        }
    }

    #[tokio::test]
    async fn idempotent_too_large_stored_as_marker() {
        let ws = WsState::new();
        let (conn, _rx) = ws.subscribe([14u8; 32]);
        let key = [2u8; 16];
        let big = Bytes::from(vec![0u8; REPLY_TOO_LARGE_THRESHOLD + 1]);
        conn.insert_idempotent(key, big).await;
        match conn.lookup_idempotent(&key).await {
            IdempotencyHit::TooLarge => {}
            _ => panic!("expected TooLarge"),
        }
    }

    /// A SIGTERM arriving while **no connection is subscribed** — an idle nest
    /// between clients, the ordinary redeploy case — must still put the state
    /// into shutdown, both for `is_shutting_down` and for every connection
    /// accepted afterwards.
    ///
    /// Before the `send_replace` fix it did not: `with_heartbeat` drops the
    /// watch channel's original receiver, so the publication had no receiver to
    /// reach, `watch::send` discarded it, and the nest went on serving new
    /// connections as though it had never been told to stop.
    #[test]
    fn begin_shutdown_is_visible_with_no_connection_subscribed() {
        let ws = WsState::new();
        assert!(
            !ws.is_shutting_down(),
            "fresh state must not be shutting down"
        );

        ws.begin_shutdown();

        assert!(
            ws.is_shutting_down(),
            "begin_shutdown() did not take effect with nobody subscribed"
        );
        assert!(
            *ws.subscribe_shutdown().borrow(),
            "a connection accepted after begin_shutdown() saw a live nest"
        );
    }
}
