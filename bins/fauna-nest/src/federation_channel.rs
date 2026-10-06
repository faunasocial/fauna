//! The long-lived nest↔nest WS-RPC channel (Spec Y2 slice 4 / hub Track D).
//!
//! This is the **carrier** for all Fauna↔Fauna federation residue rows: one
//! long-lived, peer-symmetric WS-RPC channel per peer nest, reusing the Y.1 L3
//! envelopes (`Frame`/`Request`/`Reply`/`Push`/`Cancel` + `RpcDispatcher`). The
//! **auth model** is settled in Y2 (mutual nest-key sign-over-CID,
//! `federation_sig`) and is **not** redesigned here — slice 4 changes only *when*
//! the signature is checked (per-channel, not per-request) and *what carries it*
//! (an L3 `fauna.federation.hello` Request, not HTTP headers).
//!
//! Design tracked internally (§4.A–§4.F); goal-doc authority
//! `docs/goal/architecture/federation.md`
//! § Transport / § Federation residue surface.
//!
//! This module holds, as of slice 3: the connection-level `fauna.federation.hello`
//! handshake primitive (§4.B); the [`FederationConnection`] channel end (§4.C);
//! the axum-WS [`WsMessageAdapter`]; the listener upgrade
//! [`federation_ws_handler`] + serving loop; and the [`dial`] entry point with
//! capability-negotiation detection (§4.F). The `FederationRouter` + kind
//! allowlist + the 13 handlers + per-nest throttle + the shared
//! `dispatch_request` core (`dispatch_core`) all landed in slice 4; `wss://`
//! dialing now captures the served-cert SPKI via the shared
//! `fauna-ws-substrate::tls_verify` verifier (see [`connect_federation_ws`]).
//! **Still deferred** (with its genuine originator consumers, per the slice-2
//! lesson): the `FederationChannelPool` (dial/reuse/reconnect + symmetric
//! reuse, §4.D) wired into `AppState`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use bytes::Bytes;
use ed25519_dalek::{SigningKey, VerifyingKey};
use fauna_cbor::EncodeError;
use fauna_protocol::{Request, RpcDispatcher, RpcError, Value, decode_strict, encode_canonical};
use fauna_ws_substrate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, TungsteniteAdapter};
use futures_util::future::BoxFuture;
use futures_util::{Sink, Stream};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::dispatch_core::DispatchSink;
use crate::federation_sig::{sign_payload, verify_payload};
use crate::routes::AppState;
use crate::ws::{IdempotencyCache, IdempotencyHit};

/// How long the listener waits for the inbound `fauna.federation.hello` before
/// tearing the connection down (§4.B: "the connection is dropped after a short
/// deadline").
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// How long the initiator waits for `connect_federation_ws`'s TCP/TLS/WS
/// upgrade to complete. `get_or_dial` (`federation_pool.rs`) races the dial
/// against `ORIGINATE_DEADLINE` (30s) only *after* the connect returns, so
/// this is the piece of that ceiling connect actually owns — matching
/// `HANDSHAKE_DEADLINE` / `succession_pull::HINT_DIAL_TIMEOUT` keeps connect +
/// handshake comfortably inside `ORIGINATE_DEADLINE`. `tokio_tungstenite`'s
/// own connect futures take no deadline of their own (see
/// `fauna-anon-client::ws::CONNECT_DEADLINE`'s identical rationale), so
/// bounding must happen at this call site regardless of what any one branch
/// does internally.
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// The reserved handshake kind: the **first** L3 Request on a federation
/// channel, in either direction. No other federation kind flows until it
/// verifies (§4.B / §4.C connection-class table).
pub const HELLO_KIND: &str = "fauna.federation.hello";

/// The WebSocket subprotocol identifying the federation connection class,
/// distinct from the per-actor `fauna.v1` and the anonymous `fauna.v1` planes
/// (§4.C connection-class table).
pub const FEDERATION_SUBPROTOCOL: &str = "fauna.federation.v1";

/// The tuple **both** ends sign in the mutual handshake (§4.B). Binding all four
/// fields is what makes the handshake mutual + channel-bound:
/// - `initiator_nest_id` / `listener_nest_id` — proves which two nests are party
///   to the channel (the verifier reconstructs with **its own** id, so a hello
///   addressed to the wrong nest fails to verify);
/// - `channel_nonce` — a fresh per-channel nonce; bound to the live TLS SPKI it
///   defeats cross-session replay (a captured frame replayed on a fresh
///   connection requires a fresh signature the attacker can't produce);
/// - `spki_sha256` — the served-cert channel binding (the same mechanism the
///   client↔nest plane uses), so a MITM that can't present the bound cert can't
///   complete the handshake even on a self-signed / LAN cert.
///
/// Fields are hex strings, matching the existing `federation_sig` payload style
/// (`SyncPullSig` et al.): canonical dag-cbor of identical structs is
/// byte-identical on both ends, so the signed CID matches.
#[derive(Serialize)]
struct FederationHelloSig<'a> {
    initiator_nest_id: &'a str,
    listener_nest_id: &'a str,
    channel_nonce: &'a str,
    spki_sha256: &'a str,
}

/// Wire payload of the `fauna.federation.hello` **Request** (initiator →
/// listener). Carries no `listener_nest_id` — the listener supplies its own
/// `nest_identity` when reconstructing the signed tuple, so a hello signed over
/// the wrong listener fails to verify (§4.B).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FederationHello {
    pub initiator_nest_id: String,
    pub channel_nonce: String,
    pub spki_sha256: String,
    /// 100-byte-hex sign-over-CID envelope over [`FederationHelloSig`].
    pub envelope: String,
}

/// Wire payload of the hello **Reply** (listener → initiator): the listener
/// proves it holds the discovered `listener_nest_id` by signing the **same**
/// tuple with its own key.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FederationHelloReply {
    pub listener_nest_id: String,
    /// 100-byte-hex sign-over-CID envelope over the same [`FederationHelloSig`].
    pub envelope: String,
}

/// Reasons a handshake step rejects. A rejection always tears the connection
/// down (§4.B) — none of these are recoverable in-band.
#[derive(Debug, thiserror::Error)]
pub enum HelloError {
    #[error("malformed nest_id (not 32-byte hex / invalid Ed25519 point)")]
    BadNestId,
    #[error("malformed channel_nonce (not 32-byte hex)")]
    BadNonce,
    #[error("spki_sha256 mismatch — channel binding failed (wrong served cert / MITM)")]
    SpkiMismatch,
    #[error("listener_nest_id mismatch — dialed a different nest than discovered")]
    ListenerMismatch,
    #[error("federation hello signature verification failed")]
    BadSignature,
    #[error("hello payload encode failed: {0}")]
    Encode(#[from] EncodeError),
}

/// Parse a 32-byte hex `nest_id` into a verifying key (the `nest_id` **is** the
/// Ed25519 public key — `federation.md` § Peer-auth model).
fn parse_nest_id(nest_id_hex: &str) -> Result<VerifyingKey, HelloError> {
    let arr = fauna_core::hex32::decode(nest_id_hex).map_err(|_| HelloError::BadNestId)?;
    VerifyingKey::from_bytes(&arr).map_err(|_| HelloError::BadNestId)
}

/// Validate that `nonce_hex` decodes to exactly 32 bytes (a cheap malformed-input
/// gate; the nonce is otherwise opaque to verification).
fn check_nonce(nonce_hex: &str) -> Result<(), HelloError> {
    fauna_core::hex32::decode(nonce_hex)
        .map(|_| ())
        .map_err(|_| HelloError::BadNonce)
}

/// Mint a fresh 32-byte channel nonce (hex), bound into the handshake tuple by
/// the dialer for each new channel / reconnect (§4.B, §4.D).
pub fn fresh_channel_nonce() -> String {
    fauna_core::identity::random_hex(32)
}

/// Initiator side: build the `fauna.federation.hello` Request payload, signing
/// the bound tuple with the initiator nest's key (§4.B). `listener_nest_id` and
/// `spki_sha256` are the values the initiator discovered for / observed from the
/// peer over authenticated TLS before dialing.
pub fn build_hello(
    initiator_sk: &SigningKey,
    initiator_nest_id: &str,
    listener_nest_id: &str,
    channel_nonce: &str,
    spki_sha256: &str,
) -> Result<FederationHello, HelloError> {
    let sig = FederationHelloSig {
        initiator_nest_id,
        listener_nest_id,
        channel_nonce,
        spki_sha256,
    };
    let envelope = sign_payload(&sig, initiator_sk)?;
    Ok(FederationHello {
        initiator_nest_id: initiator_nest_id.to_string(),
        channel_nonce: channel_nonce.to_string(),
        spki_sha256: spki_sha256.to_string(),
        envelope,
    })
}

/// Listener side: verify an inbound `fauna.federation.hello` and, on success,
/// build the reply proving the listener holds `listener_nest_id` (§4.B).
///
/// Checks, in order:
/// 1. the carried `spki_sha256` matches the cert **this** nest is serving
///    (`served_spki_sha256`) — the channel binding;
/// 2. `initiator_nest_id` / `channel_nonce` are well-formed;
/// 3. the envelope verifies against `initiator_nest_id`'s key over the tuple
///    reconstructed with **this** nest's `listener_nest_id` (so a hello
///    addressed to a different nest fails here).
///
/// Returns the verified initiator `nest_id` bytes (to bind to the connection)
/// and the reply payload. Open federation: any valid signature is accepted — it
/// buys attribution/throttling, not authorization.
pub fn verify_hello_and_build_reply(
    hello: &FederationHello,
    listener_sk: &SigningKey,
    listener_nest_id: &str,
    served_spki_sha256: &str,
) -> Result<([u8; 32], FederationHelloReply), HelloError> {
    if hello.spki_sha256 != served_spki_sha256 {
        return Err(HelloError::SpkiMismatch);
    }
    let initiator_key = parse_nest_id(&hello.initiator_nest_id)?;
    check_nonce(&hello.channel_nonce)?;

    let sig = FederationHelloSig {
        initiator_nest_id: &hello.initiator_nest_id,
        listener_nest_id,
        channel_nonce: &hello.channel_nonce,
        spki_sha256: &hello.spki_sha256,
    };
    if !verify_payload(&hello.envelope, &sig, &initiator_key) {
        return Err(HelloError::BadSignature);
    }

    let reply_envelope = sign_payload(&sig, listener_sk)?;
    let reply = FederationHelloReply {
        listener_nest_id: listener_nest_id.to_string(),
        envelope: reply_envelope,
    };
    Ok((*initiator_key.as_bytes(), reply))
}

/// Initiator side: verify the listener's hello reply (§4.B). Proves the nest the
/// initiator reached is the one it discovered, even on a self-signed / LAN cert
/// where TLS alone doesn't bind domain→key.
///
/// `expected_listener_nest_id` is the `nest_id` the initiator discovered (via the
/// anonymous chain); the remaining args are the values the initiator put in its
/// own hello. Returns the verified listener `nest_id` bytes.
pub fn verify_reply(
    reply: &FederationHelloReply,
    expected_listener_nest_id: &str,
    initiator_nest_id: &str,
    channel_nonce: &str,
    spki_sha256: &str,
) -> Result<[u8; 32], HelloError> {
    if reply.listener_nest_id != expected_listener_nest_id {
        return Err(HelloError::ListenerMismatch);
    }
    let listener_key = parse_nest_id(&reply.listener_nest_id)?;
    let sig = FederationHelloSig {
        initiator_nest_id,
        listener_nest_id: &reply.listener_nest_id,
        channel_nonce,
        spki_sha256,
    };
    if !verify_payload(&reply.envelope, &sig, &listener_key) {
        return Err(HelloError::BadSignature);
    }
    Ok(*listener_key.as_bytes())
}

// ── FederationConnection: one peer-symmetric channel end ───────────────────────

/// One end of a long-lived nest↔nest WS-RPC channel, keyed on the verified peer
/// `nest_id` (§4.C — analogous to `RpcConnection` but keyed on a peer nest, not
/// an actor). Created on **both** sides after a successful `fauna.federation.hello`
/// handshake; held in the [pool](FederationChannelPool) (§4.D, later slice step).
///
/// It is fully peer-symmetric: the single [`RpcDispatcher`] both **originates**
/// (`request_raw` — the home nest relaying a KP-fetch, the nest-sync worker, …)
/// and **serves** (drain `inbound_requests()` → dispatch → `send_reply()`),
/// realising §4.A over one duplex. Per §4.C it reuses `RpcConnection`'s
/// mechanical core via the shared [`IdempotencyCache`] (serving-side Reply
/// replay) and a `pending_handlers` abort registry (inbound Cancel aborts the
/// in-flight served handler). Push routing (per-actor) does not apply.
pub struct FederationConnection {
    /// The verified peer `nest_id` (32-byte Ed25519 public key), bound by the
    /// handshake. Every inbound Request on this connection is attributed to it
    /// (§4.B) — no per-request signature.
    pub peer_nest_id: [u8; 32],
    /// The L3 dispatcher driving this channel end (origination + serving).
    pub dispatcher: Arc<RpcDispatcher>,
    /// Serving-side idempotency cache — a repeated `idempotency_key` replays the
    /// prior Reply instead of re-running the op. Shared impl with `RpcConnection`.
    pub idempotency_cache: IdempotencyCache,
    /// Serving-side in-flight handler abort registry, keyed by `correlation_id`;
    /// an inbound `Cancel` removes + aborts the matching handler (§4.C).
    pub pending_handlers: Mutex<HashMap<u64, AbortHandle>>,
    /// PROXY-v2-resolved source IP of the inbound peer (the real internet client,
    /// per `lib.rs` `WithConnectInfo`), or `None` on the dialer side / when no
    /// peer addr is available. Used to rate-limit on the *attacker-uncontrolled*
    /// source IP in addition to the attacker-chosen `peer_nest_id` (F4) — a fresh
    /// keypair per connection otherwise mints a fresh throttle bucket.
    pub source_ip: Option<std::net::IpAddr>,
}

impl FederationConnection {
    /// Wrap a handshake-verified dispatcher as a federation channel end.
    pub fn new(peer_nest_id: [u8; 32], dispatcher: Arc<RpcDispatcher>) -> Self {
        Self {
            peer_nest_id,
            dispatcher,
            idempotency_cache: IdempotencyCache::new(),
            pending_handlers: Mutex::new(HashMap::new()),
            source_ip: None,
        }
    }

    /// Attach the inbound peer's resolved source IP (serving side only).
    pub fn with_source_ip(mut self, source_ip: Option<std::net::IpAddr>) -> Self {
        self.source_ip = source_ip;
        self
    }

    /// The peer `nest_id` as a hex string (for logging / pool keys / throttle
    /// keys).
    pub fn peer_nest_id_hex(&self) -> String {
        hex::encode(self.peer_nest_id)
    }
}

/// Cached federation Reply for idempotency replay — the `(payload, ok)` pair, NOT
/// raw frame bytes (the per-actor path's choice). A federation idempotency retry
/// carries a **fresh** `correlation_id` (`RpcDispatcher::request_raw` allocates
/// one per call), so a replay must rebuild the `Reply` with the *current* corr
/// rather than re-send a stale one the peer's pending map would never match (spec
/// §4.B). Canonical-CBOR-encoded into the shared [`IdempotencyCache`].
#[derive(Serialize, Deserialize)]
struct CachedFedReply {
    payload: Value,
    ok: bool,
}

/// The federation side of the shared dispatch core (Spec Y2 slice 4 §4.C / §5).
/// Reply carriage: build a `Reply` and route it through `RpcDispatcher::send_reply`
/// over the same outbound sink as origination; the idempotency cache stores
/// `(payload, ok)` so a replay rebuilds the `Reply` with the current
/// correlation_id. See [`crate::dispatch_core`].
impl DispatchSink for FederationConnection {
    fn pending_handlers(&self) -> &Mutex<HashMap<u64, AbortHandle>> {
        &self.pending_handlers
    }

    fn lookup_idempotent(&self, key: [u8; 16]) -> BoxFuture<'_, IdempotencyHit> {
        Box::pin(async move { self.idempotency_cache.lookup(&key).await })
    }

    fn replay(self: Arc<Self>, correlation_id: u64, cached: Bytes) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            // `cached` is the canonical CBOR of a `CachedFedReply`; rebuild the
            // Reply with the *current* corr (the retry's), not the cached one.
            if let Ok(c) = decode_strict::<CachedFedReply>(&cached) {
                let _ = fauna_peer_channel::send_reply_bounded(
                    &self.dispatcher,
                    correlation_id,
                    c.payload,
                    c.ok,
                )
                .await;
            }
        })
    }

    fn replay_rebuilt(
        self: Arc<Self>,
        correlation_id: u64,
        payload: Bytes,
        ok: bool,
    ) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            crate::dispatch_core::replay_rebuilt_via_dispatcher(
                &self.dispatcher,
                correlation_id,
                payload,
                ok,
            )
            .await;
        })
    }

    fn finish(
        self: Arc<Self>,
        correlation_id: u64,
        idempotency_key: [u8; 16],
        _kind: String,
        outcome: Result<Bytes, RpcError>,
    ) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let (payload, ok) = crate::dispatch_core::outcome_to_reply_value(outcome);
            // Cache `(payload, ok)` for replay (rebuildable with a fresh corr).
            if let Ok(blob) = encode_canonical(&CachedFedReply {
                payload: payload.clone(),
                ok,
            }) {
                self.idempotency_cache
                    .insert(idempotency_key, Bytes::from(blob.to_vec()))
                    .await;
            }
            let _ = fauna_peer_channel::send_reply_bounded(
                &self.dispatcher,
                correlation_id,
                payload,
                ok,
            )
            .await;
        })
    }

    fn send_error(self: Arc<Self>, correlation_id: u64, err: RpcError) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            send_error(&self.dispatcher, correlation_id, err).await;
        })
    }
}

/// Federation router lookup, passed to [`crate::dispatch_core::spawn_dispatch`]
/// (the per-actor path passes its own `rpc_router` lookup; both yield the same
/// `&RpcKindMeta`).
fn federation_router_meta<'a>(
    state: &'a AppState,
    kind: &str,
) -> Option<&'a crate::rpc_router::RpcKindMeta> {
    state.federation_router.kind_meta(kind)
}

// ── axum WebSocket ⇄ Bytes adapter (listener side) ─────────────────────────────

/// Adapt a `Message`-framed WebSocket-like transport into the
/// `Stream<Item = Result<Bytes>> + Sink<Bytes>` shape that
/// [`RpcDispatcher::new`] consumes (the dialer side uses the substrate's
/// `TungsteniteAdapter`; the **listener** side has an already-upgraded axum
/// `WebSocket`, which is `Message`-framed — this bridges it).
///
/// Generic over the inner transport `W` so it is unit-testable with a mock
/// `Message` transport; production instantiates `W = axum::extract::ws::WebSocket`.
/// Inbound `Binary` → `Bytes`; `Ping`/`Pong` are transparently skipped (axum
/// answers Ping at the framing layer); `Close` and any `Text` (no text frames in
/// Spec Y — a protocol violation) end the stream so the connection tears down.
/// Outbound `Bytes` → `Message::Binary`.
///
/// **It also drives the server half of the heartbeat** (`transport.md`
/// § Connection lifecycle), which is why it needs the `Sink` half even to be
/// read: every listener-side channel that carries a `RpcDispatcher` — the
/// sidecar channels and the federation channel — hands its socket to the
/// dispatcher's driver, so this adapter is the *only* place left that touches
/// both directions. That makes it the exact mirror of where the client half
/// lives, `fauna_ws_substrate::adapter`'s `TungsteniteAdapter::poll_next` —
/// and the two now share the driving loop itself, not merely the clock:
/// [`fauna_ws_substrate::poll_ws_frames`] IS both `poll_next` bodies (coalesce
/// elapsed ticks into one pending Ping, emit it best-effort without ever
/// blocking reads on the flush, re-arm an absolute liveness deadline on **any**
/// inbound frame, end the stream when a full window passes in silence). All
/// that remains per-side is the message vocabulary, behind
/// [`fauna_ws_substrate::WsFrames`] — [`AxumFrames`] below is this side's.
pub struct WsMessageAdapter<W> {
    inner: W,
    frames: AxumFrames,
    heartbeat: fauna_ws_substrate::HeartbeatDriver,
}

/// The axum half of [`fauna_ws_substrate::WsFrames`]. Stateless, unlike the
/// client side's: there is no reconnect supervisor on this end to hand a
/// close-code signal to, so a dead link is a log line and nothing more.
#[derive(Debug, Default)]
pub struct AxumFrames;

impl fauna_ws_substrate::WsFrames for AxumFrames {
    type Message = Message;

    fn ping() -> Message {
        Message::Ping(Bytes::new())
    }

    fn binary(payload: Bytes) -> Message {
        Message::Binary(payload)
    }

    fn classify(&mut self, msg: Message) -> fauna_ws_substrate::FrameAction {
        use fauna_ws_substrate::FrameAction;
        match msg {
            Message::Binary(b) => FrameAction::Yield(b),
            // Control frames: axum handles Ping/Pong at the framing layer, so
            // these are liveness-only and never reach a consumer.
            Message::Ping(_) | Message::Pong(_) => FrameAction::Skip,
            // Close, or a Text frame (protocol violation — Spec Y is binary
            // only): end the stream → the driver tears down.
            Message::Close(_) | Message::Text(_) => FrameAction::End,
        }
    }

    fn on_dead_link(&mut self, liveness_timeout: std::time::Duration) {
        // Ending the stream is how this layer says the peer is gone: the driver
        // tears the connection down exactly as it does on a peer close, and the
        // socket (with the per-IP permit under it) is released. `warn`, because
        // a connection reaped in silence is otherwise indistinguishable from a
        // quiet night.
        tracing::warn!(
            timeout_ms = liveness_timeout.as_millis() as u64,
            "WS peer answered no heartbeat within the liveness window; closing dead link",
        );
    }
}

impl<W> WsMessageAdapter<W> {
    /// Production cadence — the shared `WsHeartbeatPolicy` default, which mirrors
    /// the client half's own constants.
    pub fn new(inner: W) -> Self {
        Self::with_heartbeat(inner, crate::ws::WsHeartbeatPolicy::default())
    }

    /// [`Self::new`] with an explicit cadence; tests run the real loop on a
    /// compressed clock.
    pub fn with_heartbeat(inner: W, policy: crate::ws::WsHeartbeatPolicy) -> Self {
        Self {
            inner,
            frames: AxumFrames,
            heartbeat: fauna_ws_substrate::HeartbeatDriver::new(
                policy.ping_interval,
                policy.liveness_timeout,
            ),
        }
    }
}

impl<W, E> Stream for WsMessageAdapter<W>
where
    W: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // The loop is shared with the client half; `AxumFrames` supplies this
        // side's message vocabulary. The inner error forwards untouched, which
        // is the one shape difference from the tungstenite twin.
        fauna_ws_substrate::poll_ws_frames(
            &mut this.inner,
            &mut this.heartbeat,
            &mut this.frames,
            cx,
        )
    }
}

impl<W, E> Sink<Bytes> for WsMessageAdapter<W>
where
    W: Sink<Message, Error = E> + Unpin,
{
    type Error = E;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        use fauna_ws_substrate::WsFrames as _;
        Pin::new(&mut self.get_mut().inner).start_send(AxumFrames::binary(item))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_close(cx)
    }
}

// ── Listener: GET /api/v1/federation/ws ────────────────────────────────────────

/// `GET /api/v1/federation/ws` — the third nest WS connection class (§4.C),
/// distinct from the per-actor `GET /api/v1/ws/{actor_id}` (bearer) and the
/// anonymous `GET /api/v1/ws` (pre-identity). **No bearer**: the connection
/// authenticates at the L3 layer via the `fauna.federation.hello` handshake (the
/// FIRST frame), not at upgrade time, and is keyed on the peer `nest_id` once the
/// handshake verifies.
pub async fn federation_ws_handler(
    axum::extract::State(state): axum::extract::State<Arc<crate::routes::AppState>>,
    headers: axum::http::HeaderMap,
    crate::registration::OptionalConnectInfo(peer_addr): crate::registration::OptionalConnectInfo,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !crate::routes::subprotocol_offers(&headers, FEDERATION_SUBPROTOCOL) {
        // No `fauna.federation.v1` offered — 426 (UPGRADE_REQUIRED), the same
        // pre-upgrade shape the other two WS endpoints use on subprotocol miss.
        // A peer that gets this surfaces it as `ChannelUnsupported` (no HTTP
        // fallback since Spec Y2 slice 5; §4.F).
        return (
            axum::http::StatusCode::UPGRADE_REQUIRED,
            "subprotocol fauna.federation.v1 required",
        )
            .into_response();
    }
    let source_ip = peer_addr.map(|a| a.ip());
    ws.max_message_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .max_frame_size(crate::routes::MAX_WS_MESSAGE_SIZE)
        .protocols([FEDERATION_SUBPROTOCOL])
        // The whole connection future is generation-scoped, not just the
        // driver below: axum's per-connection task is detached (`lib.rs`'s accept
        // loop), so `server_handle.abort()` at teardown never reaches an
        // established federation channel. Scoping here is what makes
        // `teardown_serving_generation`'s "no task of this generation signs
        // anything" true of this route — the task holds `Arc<AppState>`, and with
        // it the deployment signing key.
        .on_upgrade(move |socket| {
            let scope = Arc::clone(&state);
            async move {
                let _ = scope
                    .spawn_scoped(serve_listener(state, socket, source_ip))
                    .await;
            }
        })
}

/// Drive one inbound federation channel (we are the listener). Runs the
/// `fauna.federation.hello` handshake on the first frame (§4.B), binds the peer
/// `nest_id`, then serves the peer over the peer-symmetric dispatcher.
async fn serve_listener(
    state: Arc<crate::routes::AppState>,
    socket: WebSocket,
    source_ip: Option<std::net::IpAddr>,
) {
    let adapter = WsMessageAdapter::with_heartbeat(socket, state.ws.heartbeat());
    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    // Same hint table as the dialer side (see `dial`). The listener serves
    // today, but slice 3.5's symmetric channel reuse will originate over this
    // dispatcher too — attaching now means that reuse cannot silently lose the
    // wire hint.
    dispatcher
        .set_kind_registry(state.federation_router.hint_registry())
        .expect("fresh dispatcher has no registry");
    let dispatcher = Arc::new(dispatcher);
    // Generation-scoped, not bare: the driver holds the socket and outlives
    // every request on it, so a bare spawn would keep pumping this channel — and
    // holding the superseded key through `state` — for as long as the PEER
    // chooses, after a rotation teardown returned.
    //
    // The handle is deliberately NOT retained: this listener never aborts its
    // driver. A rejected `fauna.federation.hello` *enqueues* its `unauthenticated`
    // reply (`send_reply` only puts it on `out_tx`), so aborting on the next line
    // raced that frame and usually won — the peer then read
    // `fauna.protocol.disconnected` and could not tell a refusal from a dropped
    // socket. Dropping the last dispatcher handle instead closes `out_tx`, and the
    // driver drains what is queued before exiting on its own
    // (`RpcDispatcher::new`; `transport.md` § Layers). The generation
    // scope still bounds the task, which is what the paragraph above is about.
    state.spawn_scoped(driver);

    let Some(mut inbound) = dispatcher.inbound_requests() else {
        // Unreachable (fresh dispatcher) — defensive.
        return;
    };

    // §4.B/§4.C: no federation kind flows until the handshake verifies. The first
    // inbound Request MUST be `fauna.federation.hello`, within a short deadline.
    let listener_nest_id = hex::encode(state.nest_identity.public_key_bytes());
    let served_spki = state
        .served_cert_spki
        .as_ref()
        .and_then(|s| s.current_spki_sha256())
        .map(hex::encode)
        .unwrap_or_default();

    let peer_nest_id = match tokio::time::timeout(HANDSHAKE_DEADLINE, inbound.recv()).await {
        Ok(Some(req)) => match listener_handshake(
            &dispatcher,
            &state.nest_identity.signing_key,
            &listener_nest_id,
            &served_spki,
            req,
        )
        .await
        {
            Ok(id) => id,
            // The rejection is already queued; returning drops `dispatcher`,
            // which is what gets it written. See the spawn above.
            Err(()) => return,
        },
        // Peer closed before sending hello.
        Ok(None) => return,
        Err(_elapsed) => {
            tracing::warn!("federation: no hello within deadline; dropping connection");
            return;
        }
    };

    let conn = Arc::new(
        FederationConnection::new(peer_nest_id, Arc::clone(&dispatcher)).with_source_ip(source_ip),
    );
    tracing::info!(
        peer = %conn.peer_nest_id_hex(),
        "federation channel established (listener side)"
    );

    // TODO(slice 3.5): register `conn` in `AppState`'s `FederationChannelPool` so
    // nest-side originators reuse this inbound channel for origination to the peer
    // (symmetric reuse, §4.D), rather than dialing a second channel.

    let cancels = dispatcher.inbound_cancels();
    crate::dispatch_core::serve_loop(&state, &conn, &mut inbound, cancels, |state, conn, req| {
        Box::pin(serve_request(state, conn, req))
    })
    .await;

    // `conn` and `dispatcher` drop here — the driver drains and exits.
}

/// Verify the inbound `fauna.federation.hello` and reply (§4.B). On success
/// returns the verified peer `nest_id`; on any failure replies `unauthenticated`
/// (or `malformed`) and returns `Err(())` so the caller tears the connection
/// down. Takes the listener's identity + served SPKI explicitly (decoupled from
/// `AppState`) so it is unit-testable over an in-memory duplex.
async fn listener_handshake(
    dispatcher: &Arc<RpcDispatcher>,
    listener_sk: &SigningKey,
    listener_nest_id: &str,
    served_spki: &str,
    req: Request,
) -> Result<[u8; 32], ()> {
    if req.kind != HELLO_KIND {
        tracing::warn!(kind = %req.kind, "federation: first frame was not hello; rejecting");
        send_error(dispatcher, req.correlation_id, unauthenticated()).await;
        return Err(());
    }

    let hello: FederationHello = match value_to(&req.payload) {
        Ok(h) => h,
        Err(()) => {
            send_error(dispatcher, req.correlation_id, malformed()).await;
            return Err(());
        }
    };

    match verify_hello_and_build_reply(&hello, listener_sk, listener_nest_id, served_spki) {
        Ok((peer_nest_id, reply)) => {
            let payload = to_value(&reply);
            let _ = fauna_peer_channel::send_reply_bounded(
                dispatcher,
                req.correlation_id,
                payload,
                true,
            )
            .await;
            Ok(peer_nest_id)
        }
        Err(e) => {
            tracing::warn!(error = %e, "federation hello rejected");
            send_error(dispatcher, req.correlation_id, unauthenticated()).await;
            Err(())
        }
    }
}

// ── Dialer: originate the handshake (initiator side) ───────────────────────────

/// Outcome of a federation dial + handshake, distinguishing the
/// **capability-negotiation** result (§4.F) from a hard failure. The pool maps
/// [`ChannelUnsupported`](DialError::ChannelUnsupported) to a hard
/// [`PoolError::Unsupported`](crate::federation_pool::PoolError::Unsupported)
/// (the HTTP interim was retired in Spec Y2 slice 5); any other variant is a
/// transient/real error to back off on.
#[derive(Debug, thiserror::Error)]
pub enum DialError {
    /// The peer does not support the channel — the upgrade returned 404/426, or
    /// `fauna.federation.hello` came back `unknown_kind`. There is no HTTP
    /// fallback (retired in Spec Y2 slice 5); the pool surfaces this as
    /// `PoolError::Unsupported` (§4.F).
    #[error("peer does not support the federation channel")]
    ChannelUnsupported,
    /// The handshake reached the peer but failed (bad signature, listener
    /// mismatch, malformed reply, transport error mid-handshake). NOT a fallback
    /// condition — the peer offers the channel but the handshake didn't complete.
    #[error("federation handshake failed: {0}")]
    Handshake(String),
}

/// Initiator side: originate the `fauna.federation.hello` over `dispatcher` and
/// verify the reply (§4.B). `expected_listener_nest_id` is the peer `nest_id`
/// discovered over authenticated TLS; `spki_sha256` is the served-cert SPKI the
/// initiator observed on the dial (empty for a loopback `ws://` test peer).
/// Returns the verified peer (listener) `nest_id` to bind to the connection.
async fn dialer_handshake(
    dispatcher: &Arc<RpcDispatcher>,
    initiator_sk: &SigningKey,
    initiator_nest_id: &str,
    expected_listener_nest_id: &str,
    channel_nonce: &str,
    spki_sha256: &str,
) -> Result<[u8; 32], DialError> {
    let hello = build_hello(
        initiator_sk,
        initiator_nest_id,
        expected_listener_nest_id,
        channel_nonce,
        spki_sha256,
    )
    .map_err(|e| DialError::Handshake(e.to_string()))?;
    let payload = to_value(&hello);

    let call = dispatcher
        .request_raw(
            HELLO_KIND,
            hello_idempotency_key(channel_nonce),
            payload,
            Some(HANDSHAKE_DEADLINE),
        )
        .await
        .map_err(|e| DialError::Handshake(format!("hello send failed: {e}")))?;

    // `HANDSHAKE_DEADLINE` above is a WIRE-ONLY field: it tells the peer how
    // long we'll wait, but nothing local ever raced this reply wait against
    // it — the only thing that could end it was the substrate's dead-link
    // timer, which stays armed by *any* inbound frame (including a Pong), not
    // just the hello reply. A peer that answers pings but withholds the reply
    // could hang this call forever. Race it against the same deadline we
    // already declared on the wire, matching `request_encoded`'s shape.
    let reply_value = match tokio::time::timeout(HANDSHAKE_DEADLINE, call.await_reply()).await {
        Ok(Ok(v)) => v,
        Ok(Err(rpc_err)) => {
            // `unknown_kind` ⇒ the peer's WS-RPC has no `fauna.federation.hello`
            // kind → the channel is unsupported there (§4.F capability fallback).
            if rpc_err.code == "fauna.protocol.unknown_kind" {
                return Err(DialError::ChannelUnsupported);
            }
            return Err(DialError::Handshake(rpc_err.code));
        }
        Err(_elapsed) => {
            return Err(DialError::Handshake(format!(
                "hello reply timed out after {HANDSHAKE_DEADLINE:?}"
            )));
        }
    };

    let reply: FederationHelloReply = value_to(&reply_value)
        .map_err(|()| DialError::Handshake("malformed hello reply".to_string()))?;
    verify_reply(
        &reply,
        expected_listener_nest_id,
        initiator_nest_id,
        channel_nonce,
        spki_sha256,
    )
    .map_err(|e| DialError::Handshake(e.to_string()))
}

/// Derive the hello's idempotency key from the channel nonce (the first 16 bytes
/// of the 32-byte nonce). The hello is the first frame on a fresh connection, so
/// this only needs to be well-formed, not globally unique.
fn hello_idempotency_key(channel_nonce: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    if let Ok(bytes) = hex::decode(channel_nonce)
        && bytes.len() >= 16
    {
        key.copy_from_slice(&bytes[..16]);
    }
    key
}

/// Dial a peer nest's federation channel: validate the URL (TLS-only), connect, run the `fauna.federation.hello`
/// handshake (initiator side, §4.B), and return the established peer-symmetric
/// [`FederationConnection`] (usable for origination **and** serving — the serving
/// loop is spawned in the background).
///
/// `peer_url` is the peer's canonical base URL (`https://…` in production,
/// `http://127.0.0.1:…` for loopback tests); `expected_peer_nest_id` is the peer
/// `nest_id` discovered over authenticated TLS. A peer that does not offer the
/// channel — 404/426 on the upgrade, or `hello` → `unknown_kind` — returns
/// [`DialError::ChannelUnsupported`], which the pool surfaces as
/// [`crate::federation_pool::PoolError::Unsupported`] (no HTTP fallback since
/// slice 5).
pub async fn dial(
    state: &Arc<crate::routes::AppState>,
    peer_url: &str,
    expected_peer_nest_id: &str,
) -> Result<Arc<FederationConnection>, DialError> {
    validate_peer_url(peer_url, state.federation_pool.peer_url_origin(peer_url))
        .await
        .map_err(|e| DialError::Handshake(format!("invalid peer url: {e}")))?;

    let ws_url = federation_ws_url(peer_url);
    let is_wss = ws_url.starts_with("wss://");
    let (adapter, spki_sha256) = connect_federation_ws(&ws_url, is_wss).await?;

    let (dispatcher, driver) = RpcDispatcher::new(adapter);
    // The federation channel's own hint table, derived from the serving router
    // so it can never drift from it: every originated Request now carries the
    // `replay_forbidden` wire hint for forbid-replay kinds, which is what keeps
    // the peer's "caller missing replay_forbidden hint" warning meaning
    // "stale caller" instead of firing on every keypackage fetch.
    dispatcher
        .set_kind_registry(state.federation_router.hint_registry())
        .expect("fresh dispatcher has no registry");
    let dispatcher = Arc::new(dispatcher);
    // Generation-scoped for the same reason as the listener side: this
    // driver holds `state` (and its signing key) for the channel's whole life,
    // which the peer controls.
    state.spawn_scoped(driver);

    let initiator_nest_id = hex::encode(state.nest_identity.public_key_bytes());
    let nonce = fresh_channel_nonce();
    let peer_nest_id = dialer_handshake(
        &dispatcher,
        &state.nest_identity.signing_key,
        &initiator_nest_id,
        expected_peer_nest_id,
        &nonce,
        &spki_sha256,
    )
    .await?;

    let conn = Arc::new(FederationConnection::new(
        peer_nest_id,
        Arc::clone(&dispatcher),
    ));

    // Peer-symmetric: we serve inbound peer requests over the same channel.
    if let Some(mut inbound) = dispatcher.inbound_requests() {
        let cancels = dispatcher.inbound_cancels();
        let serve_conn = Arc::clone(&conn);
        let serve_state = Arc::clone(state);
        // The symmetric serve loop is a third long-lived `AppState` holder
        // on this channel — the finding named the two drivers, but this one has
        // the same lifetime and the same key.
        state.spawn_scoped(async move {
            crate::dispatch_core::serve_loop(
                &serve_state,
                &serve_conn,
                &mut inbound,
                cancels,
                |state, conn, req| Box::pin(serve_request(state, conn, req)),
            )
            .await;
        });
    }

    tracing::info!(
        peer = %conn.peer_nest_id_hex(),
        "federation channel established (dialer side)"
    );
    Ok(conn)
}

/// Where a peer base URL came from — which decides whether
/// [`validate_peer_url`]'s SSRF arm applies to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerUrlOrigin {
    /// Named by a request or a peer — `fauna.inbox.send`'s
    /// `recipient_nest_url`, a relay target, a handle domain. The default, and
    /// the only origin the SSRF guard exists for.
    Supplied,
    /// The deployment's own topology — the `nest_url` of a pairing row whose
    /// actor is an admin of this nest at dial time (`private-mode.md`
    /// § Pairing Flow, re-decided 2026-10-01), dialed by the relay workers
    /// every cycle. The admin is the one human who chooses where the box's
    /// relay sits, so a private or CGNAT address there is the deployment's
    /// topology, not an SSRF reach: a home box that reaches its relay over a
    /// LAN, a VPN, or the docker bridge network of the two-box witness is
    /// refused nothing. Decided from the row's actor whenever the dialer's
    /// table is rebuilt (`nest_sync_worker::refresh_pairing_targets`), never a
    /// stored field, so it lapses with the admin role; any other user's row is
    /// [`Self::Supplied`]. It must still be https (or a loopback literal).
    Configured,
}

/// Reject a peer base URL that isn't TLS, unless it targets a loopback host
/// (allowed only for in-process tests). The federation hop must ride
/// authenticated TLS so the certificate binds the peer's domain to the
/// `nest_id` it serves (`federation.md` § Security). Used by [`dial`] and the
/// [`crate::federation_pool::FederationChannelPool`] before dialing.
///
/// SSRF guard: the peer URL is **caller-supplied** via
/// `fauna.inbox.send`'s `recipient_nest_url`, so an https target must not be a
/// private / link-local / ULA / cloud-metadata address (loopback **literals**
/// ride the test-only carve-out below, http and https alike). We reject
/// other internal IP **literals** outright and, for hostnames, reject if DNS
/// resolves to any non-global address (static internal-pointing DNS / IMDS). An
/// unresolvable host is allowed here — the dial then fails with a plain
/// connection error, so there is no SSRF reach. (Full pin-the-socket
/// rebinding-safety for the WS dial is a residual hardening: the connect layer
/// re-resolves.)
///
/// A [`PeerUrlOrigin::Configured`] URL skips only the global-address arm — it
/// still must be https or a loopback literal.
pub(crate) async fn validate_peer_url(base: &str, origin: PeerUrlOrigin) -> anyhow::Result<()> {
    let trimmed = base.trim_end_matches('/');
    let url = url::Url::parse(trimmed).map_err(|_| anyhow::anyhow!("invalid peer URL: {base}"))?;
    let scheme = url.scheme();
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("peer URL has no host: {base}"))?;

    // Loopback (http OR https) is a TEST-ONLY affordance: an in-process /
    // tier_3 peer on `http://127.0.0.1:PORT` (the legacy plain fixtures), or on
    // `https://127.0.0.1:PORT` (the floor-TLS fixtures — since Pillar C
    // `730303718` the shared resolver derives uniform https for every
    // host-class, so a faithful in-process peer serves the self-signed floor,
    // e.g. `conformance_cross_nest_conversations_client.rs`). Production
    // federation peers are always globally-routable https, so this carve-out is
    // never produced by a deploy path. Match the host EXACTLY — the old prefix
    // match let `http://127.0.0.1.attacker.com` (an arbitrary,
    // possibly-internal host) through.
    //
    // CONSCIOUSLY ACCEPTED, deliberately NOT compiled out of production. The
    // threat: a malicious authed `fauna.inbox.send` caller passes
    // `recipient_nest_url="http://127.0.0.1:<port>/"` to drive a WS handshake at
    // a co-located loopback service (mail bridge, CalDAV `127.0.0.1:8444`). It
    // is a WS upgrade + anon `nest.info` only — non-fauna services reject the
    // upgrade and fauna loopback services self-auth, so the reach is a
    // constrained blind port-reachability oracle, not data exfil (the dial is
    // `ws`, so HTTP-only IMDS never answers), and only an already-authenticated
    // actor can reach it. Gating was rejected because there is NO build-time
    // signal that separates a production fauna-nest build from the in-process
    // build the conformance suite links: `conformance_federation_channel`,
    // `conformance_inbox`, and `conformance_cross_nest_mail_relay` all dial
    // `http://127.0.0.1:PORT` (`start_nest` returns `http://{addr}`) and run under
    // default-feature `cargo test --workspace`, where fauna-nest is a *dependency*
    // — so `cfg(test)` is unset and no `test-hooks` feature is on, identical to
    // the production lib build. A `cfg(test)` / `feature = "test-hooks"` gate would
    // therefore evict the entire federation conformance suite from the default
    // test lane (a real coverage regression on a security-critical subsystem) to
    // remove a localhost oracle. Same accept-with-comment posture as this
    // function's disclosed rebinding-TOCTOU residual (see the doc comment above)
    // and the LP-1 arbitrary-port link-preview residual.
    //
    // The https+loopback arm (2026-07-08, Pillar-C half-cleanup) widens this
    // oracle to co-located loopback services that speak TLS (a wss handshake at
    // e.g. the CalDAV `127.0.0.1:8444` listener instead of a plain ws one) —
    // same class, same bound: a blind WS-upgrade reachability oracle for an
    // already-authenticated actor; non-federation endpoints reject the upgrade.
    let is_loopback_literal = matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if (scheme == "http" || scheme == "https") && is_loopback_literal {
        return Ok(());
    }
    if scheme != "https" {
        anyhow::bail!(
            "federation peer URL must be https (loopback http allowed only for tests): {base}"
        );
    }

    // The deployment's own pull target is its topology, not a request's reach.
    if origin == PeerUrlOrigin::Configured {
        return Ok(());
    }

    // https: the target must be globally routable.
    let port = url.port_or_known_default().unwrap_or(443);
    match crate::ssrf::resolve_global_addrs(host, port).await {
        Ok(_) => Ok(()),
        Err(crate::ssrf::SsrfError::NonGlobal) => {
            anyhow::bail!("federation peer URL resolves to a non-global address: {base}")
        }
        // Unresolvable placeholder / offline host — allow; the dial fails later
        // with a connection error (no SSRF reach).
        Err(crate::ssrf::SsrfError::Resolve) => Ok(()),
        Err(e) => anyhow::bail!("federation peer URL rejected ({e}): {base}"),
    }
}

/// Convert a peer base URL (`https://…` / loopback `http://…`) into the channel
/// WebSocket URL (`wss|ws://…/api/v1/federation/ws`).
fn federation_ws_url(peer_url: &str) -> String {
    let base = fauna_core::web::http_to_ws(peer_url);
    let base = base.trim_end_matches('/');
    format!("{base}/api/v1/federation/ws")
}

/// Open the WS connection to the peer's federation endpoint, offering the
/// `fauna.federation.v1` subprotocol. Returns the [`TungsteniteAdapter`] to drive
/// plus the served-cert `spki_sha256` (hex) the initiator observed (empty for a
/// loopback `ws://` peer — no TLS).
async fn connect_federation_ws(
    ws_url: &str,
    is_wss: bool,
) -> Result<(TungsteniteAdapter, String), DialError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    let mut request = ws_url
        .into_client_request()
        .map_err(|e| DialError::Handshake(format!("invalid ws url: {e}")))?;
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(FEDERATION_SUBPROTOCOL),
    );

    // The whole TCP connect + TLS/WS-upgrade phase (whichever branch runs
    // below) is wrapped in one `CONNECT_DEADLINE` bound from the outside —
    // deliberately external to `tokio_tungstenite`'s own connect futures, none
    // of which take a deadline (see `fauna-anon-client::ws::CONNECT_DEADLINE`'s
    // identical rationale), so bounding must happen at this call site
    // regardless of what either branch does internally. A timeout here is
    // NEVER `DialError::ChannelUnsupported` — that would wrongly trigger the
    // capability fallback (§4.F) for a peer that is merely slow, not one that
    // has actually declined the channel.
    let connect = async move {
        if is_wss {
            // Production `wss://`: dial with the shared capturing TLS verifier
            // (`fauna-ws-substrate::tls_verify`, lifted from `fauna-anon-client` per
            // §5). It provisionally accepts the peer's served cert (encrypt-only) and
            // records its leaf SPKI; the `fauna.federation.hello` handshake then binds
            // that SPKI as the channel binding (§4.B), so the peer's reply (which
            // carries its own served SPKI) must match — defeating a MITM that relays
            // the channel through a different TLS endpoint. Authentication of *which*
            // nest_id is the peer comes from `expected_peer_nest_id` — resolved by
            // `FederationChannelPool::resolve_peer_nest_id` under the discovery
            // trust rule (`federation_pool::discovery_dial_admits`: the discovery
            // dial's served cert must be WebPKI-valid for the resolved authority,
            // or fall under the loopback-fixture / configured-private-target
            // carve-outs) — exactly as the anon client's pre-identity connection
            // authenticates via its channel binding rather than the WebPKI chain.
            fauna_ws_substrate::ensure_tls_provider();
            let (config, capture) = fauna_ws_substrate::capturing_client_config();
            let connector = tokio_tungstenite::Connector::Rustls(config);
            // The same 2 MiB inbound cap the listener applies (`max_message_size`
            // in `federation_ws_handler`): the paired peer is not trusted to size
            // its own replies.
            let (ws_stream, _resp) = tokio_tungstenite::connect_async_tls_with_config(
                request,
                Some(fauna_ws_substrate::rpc_ws_config()),
                false,
                Some(connector),
            )
            .await
            .map_err(map_connect_err)?;
            // The verifier callback ran during the handshake; read the captured SPKI.
            let spki_hex = capture
                .lock()
                .ok()
                .and_then(|c| c.spki)
                .map(hex::encode)
                .unwrap_or_default();
            Ok((
                TungsteniteAdapter::new(ws_stream, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT),
                spki_hex,
            ))
        } else {
            // Loopback `ws://` (in-process tests): no TLS, so no served-cert SPKI.
            let (ws_stream, _resp) = tokio_tungstenite::connect_async_with_config(
                request,
                Some(fauna_ws_substrate::rpc_ws_config()),
                false,
            )
            .await
            .map_err(map_connect_err)?;
            Ok((
                TungsteniteAdapter::new(ws_stream, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT),
                String::new(),
            ))
        }
    };
    match tokio::time::timeout(CONNECT_DEADLINE, connect).await {
        Ok(result) => result,
        Err(_elapsed) => Err(DialError::Handshake(format!(
            "ws connect timed out after {CONNECT_DEADLINE:?}"
        ))),
    }
}

/// Map a tungstenite connect error to a [`DialError`]: a 404/426 HTTP response on
/// the upgrade means the peer does not offer the channel → capability fallback
/// (§4.F); anything else is a real/transient handshake failure.
fn map_connect_err(e: tokio_tungstenite::tungstenite::Error) -> DialError {
    use tokio_tungstenite::tungstenite::Error;
    if let Error::Http(resp) = &e {
        let status = resp.status().as_u16();
        if status == 404 || status == 426 {
            return DialError::ChannelUnsupported;
        }
    }
    DialError::Handshake(format!("ws connect: {e}"))
}

/// Gate + dispatch one inbound peer Request (§4.C). Each inbound `Request`
/// flows, in order, through: (1) **idempotency** — a repeated
/// `idempotency_key` replays the cached Reply (`dispatch_core::check_idempotent`);
/// (2) the **kind allowlist** — the peer may invoke ONLY registered
/// `fauna.federation.*` kinds (`federation_router.contains`); anything else (a
/// client-actor kind) → `unauthenticated`, connection stays open (the structural
/// authz boundary against actor impersonation); (3) the **per-nest throttle** —
/// keyed on the connection's *verified* peer `nest_id` + kind
/// (`federation_rate_limit`); a trip → `rate_limited`, connection stays open; (4)
/// **dispatch** — `dispatch_core::spawn_dispatch` runs the registered handler
/// (with the originating `nest_id` as its subject) and emits the Reply. Passed
/// to [`crate::dispatch_core::serve_loop`], which owns the `select!` shape
/// (dispatch on `Request`, abort the matching handler on `Cancel`).
async fn serve_request(state: &Arc<AppState>, conn: &Arc<FederationConnection>, req: Request) {
    let correlation_id = req.correlation_id;
    let kind = req.kind.clone();

    // (1) idempotency replay (shared core).
    if crate::dispatch_core::check_idempotent(conn, correlation_id, req.idempotency_key)
        .await
        .is_replayed()
    {
        return;
    }

    // (2) federation kind allowlist — the structural authz boundary (§4.C). The
    // allowlist IS the set of registered `fauna.federation.*` kinds; a peer
    // invoking anything else gets `unauthenticated` (connection stays open).
    if !state.federation_router.contains(&kind) {
        tracing::debug!(
            peer = %conn.peer_nest_id_hex(),
            kind = %kind,
            "federation: kind not on allowlist; rejecting unauthenticated"
        );
        send_error(&conn.dispatcher, correlation_id, unauthenticated()).await;
        return;
    }

    // (2.5) per-SOURCE-IP throttle (F4). The per-nest throttle below is keyed on
    // the peer `nest_id`, which the peer CHOOSES — a fresh keypair per connection
    // mints a fresh bucket, so a Sybil sprays unboundedly under distinct ids.
    // Also cap on the PROXY-v2-resolved source IP, which the attacker does not
    // control (it can't forge its source past the loopback-only PROXY trust).
    // Checked FIRST so a throttled IP cannot even create per-nest buckets,
    // bounding `federation_rate_limit`'s map growth. Distinct key slot
    // (`FED_IP_SLOT`) so an IP bucket can never collide with a `(nest_id, 0)` one.
    if let Some(ip) = conn.source_ip {
        const FED_IP_SLOT: [u8; 32] = [0xffu8; 32];
        if !state.federation_rate_limit.check(
            &crate::anonymous_rate_limit::ip_key_bytes(ip),
            &FED_IP_SLOT,
            &kind,
        ) {
            tracing::debug!(kind = %kind, "federation: per-source-IP throttle tripped");
            send_error(&conn.dispatcher, correlation_id, rate_limited()).await;
            return;
        }
    }

    // (3) per-originating-nest throttle, keyed on the verified peer `nest_id` +
    // the kind (the slice-2 `federation_rate_limit` analogue). A trip →
    // `rate_limited`, connection stays open.
    if !state
        .federation_rate_limit
        .check(&conn.peer_nest_id, &[0u8; 32], &kind)
    {
        tracing::debug!(
            peer = %conn.peer_nest_id_hex(),
            kind = %kind,
            "federation: per-nest throttle tripped"
        );
        send_error(&conn.dispatcher, correlation_id, rate_limited()).await;
        return;
    }

    // (4)–(7) dispatch via the shared core: the handler receives the verified
    // originating `nest_id` as its subject (where the per-actor path passes an
    // actor).
    crate::dispatch_core::spawn_dispatch(
        Arc::clone(state),
        Arc::clone(conn),
        federation_router_meta,
        conn.peer_nest_id,
        req,
    )
    .await;
}

fn rate_limited() -> RpcError {
    crate::rpc_errors::rate_limited()
}

fn unauthenticated() -> RpcError {
    crate::rpc_errors::unauthenticated()
}

fn malformed() -> RpcError {
    RpcError::new("fauna.protocol.malformed", "error.protocol.malformed")
}

/// Decode a typed payload from an L3 `Value` (canonical-CBOR round-trip).
pub(crate) fn value_to<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, ()> {
    let bytes = encode_canonical(value).map_err(|_| ())?;
    decode_strict::<T>(&bytes).map_err(|_| ())
}

/// Encode a typed payload into an L3 `Value` (canonical-CBOR round-trip);
/// `Value::Null` on the (unreachable for plain structs) encode failure.
pub(crate) fn to_value<T: Serialize>(t: &T) -> Value {
    match encode_canonical(t) {
        Ok(bytes) => decode_strict::<Value>(&bytes).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

/// Send an error Reply (`ok = false`) over the dispatcher for a served Request.
/// Shared with `sidecar_channel`'s relay listener — both route an error Reply through
/// `RpcDispatcher::send_reply` identically; only per-actor (`ws.rs`) differs,
/// since it encodes its own `Frame::Reply` onto a bounded outbound channel
/// instead (see `dispatch_core`'s module doc on why Reply carriage differs).
pub(crate) async fn send_error(
    dispatcher: &Arc<RpcDispatcher>,
    correlation_id: u64,
    err: RpcError,
) {
    let payload = to_value(&err);
    let _ =
        fauna_peer_channel::send_reply_bounded(dispatcher, correlation_id, payload, false).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> SigningKey {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).unwrap();
        SigningKey::from_bytes(&secret)
    }

    fn nest_id_hex(sk: &SigningKey) -> String {
        hex::encode(sk.verifying_key().as_bytes())
    }

    #[tokio::test]
    async fn peer_url_requires_tls_except_loopback() {
        // Unresolvable placeholder hostnames are allowed (the dial fails later).
        assert!(
            validate_peer_url("https://other-nest.test", PeerUrlOrigin::Supplied)
                .await
                .is_ok()
        );
        assert!(
            validate_peer_url("https://other-nest.test:8443/", PeerUrlOrigin::Supplied)
                .await
                .is_ok()
        );
        // Loopback test affordance (exact host match), http AND https — the
        // floor-TLS fixture twin (`conformance_cross_nest_conversations_client`
        // relays to `https://127.0.0.1:<port>`, the uniform-https derivation).
        assert!(
            validate_peer_url("http://127.0.0.1:53110", PeerUrlOrigin::Supplied)
                .await
                .is_ok()
        );
        assert!(
            validate_peer_url("http://localhost:8080", PeerUrlOrigin::Supplied)
                .await
                .is_ok()
        );
        assert!(
            validate_peer_url("https://127.0.0.1:53110", PeerUrlOrigin::Supplied)
                .await
                .is_ok()
        );
        assert!(
            validate_peer_url("https://localhost:8443", PeerUrlOrigin::Supplied)
                .await
                .is_ok()
        );
        // Plain HTTP to a non-loopback host is rejected (no TLS domain→key bind).
        assert!(
            validate_peer_url("http://other-nest.test", PeerUrlOrigin::Supplied)
                .await
                .is_err()
        );
        assert!(
            validate_peer_url("http://10.0.0.5", PeerUrlOrigin::Supplied)
                .await
                .is_err()
        );
    }

    /// F4: the per-source-IP federation throttle is keyed on the source IP, NOT
    /// the attacker-chosen `nest_id`, so requests from one IP share a bucket
    /// regardless of how many distinct `nest_id`s spray them — and the IP bucket's
    /// distinct key slot (`[0xff; 32]`) never collides with a `(nest_id, [0; 32])`
    /// per-nest bucket.
    #[test]
    fn per_source_ip_throttle_is_nest_id_independent() {
        use crate::bridge_rate_limit::Limiter;
        const FED_IP_SLOT: [u8; 32] = [0xffu8; 32];
        let lim = Limiter::new(); // default: 30 events / 60s window
        let ip = "203.0.113.7".parse::<std::net::IpAddr>().unwrap();
        let ip_key = crate::anonymous_rate_limit::ip_key_bytes(ip);
        let kind = "fauna.federation.welcome.deliver";

        // Spray 100 requests "from" the same IP — the per-IP bucket must cap them
        // regardless of nest_id (the IP key is the same for every connection).
        let allowed = (0..100)
            .filter(|_| lim.check(&ip_key, &FED_IP_SLOT, kind))
            .count();
        assert!(allowed >= 1, "first requests pass");
        assert!(
            allowed <= 30,
            "per-source-IP bucket must cap the spray; allowed {allowed}"
        );

        // The same IP under the per-NEST key slot ([0;32]) is a SEPARATE bucket —
        // proving an IP bucket can't be mistaken for / collide with a nest bucket.
        assert!(
            lim.check(&ip_key, &[0u8; 32], kind),
            "the per-nest key slot is an independent bucket (no collision)"
        );
    }

    /// F7: a caller-supplied `recipient_nest_url` must not reach internal /
    /// metadata addresses via the federation dialer.
    #[tokio::test]
    async fn peer_url_rejects_ssrf_targets() {
        // Cloud metadata + RFC1918 via https — internal IP literals (loopback
        // literals are the test carve-out, asserted OK below).
        assert!(
            validate_peer_url("https://169.254.169.254/", PeerUrlOrigin::Supplied)
                .await
                .is_err(),
            "cloud IMDS must be rejected"
        );
        assert!(
            validate_peer_url("https://10.0.0.5/", PeerUrlOrigin::Supplied)
                .await
                .is_err()
        );
        assert!(
            validate_peer_url("https://192.168.1.1/", PeerUrlOrigin::Supplied)
                .await
                .is_err()
        );
        assert!(
            validate_peer_url("https://127.0.0.1/", PeerUrlOrigin::Supplied)
                .await
                .is_ok(),
            "https loopback rides the test carve-out since 2026-07-08 (the \
             floor-TLS fixture twin of the http arm — same blind WS-upgrade \
             oracle class; see the carve-out comment in validate_peer_url)"
        );
        assert!(
            validate_peer_url("https://[::ffff:169.254.169.254]/", PeerUrlOrigin::Supplied)
                .await
                .is_err(),
            "IPv4-mapped IMDS must be rejected"
        );
        // The old prefix match let `http://127.0.0.1.<attacker>` through; exact
        // host match now rejects it (non-loopback host, http scheme).
        assert!(
            validate_peer_url(
                "http://127.0.0.1.attacker.example/",
                PeerUrlOrigin::Supplied
            )
            .await
            .is_err(),
            "prefix-match loopback bypass must be closed"
        );
    }

    /// The deployment's own pull target (an admin's pairing row) may sit on a
    /// private address — a LAN, a VPN, the docker bridge network — and is
    /// refused nothing for it; the SSRF arm stays whole for every URL a request
    /// names, and a configured target still has to be https.
    #[tokio::test]
    async fn a_configured_pair_target_may_be_private_but_must_be_https() {
        for private in [
            "https://172.18.0.2:3000",
            "https://10.0.0.5/",
            "https://100.64.1.2/",
        ] {
            assert!(
                validate_peer_url(private, PeerUrlOrigin::Configured)
                    .await
                    .is_ok(),
                "{private}: the configured pull target is the deployment's topology"
            );
            assert!(
                validate_peer_url(private, PeerUrlOrigin::Supplied)
                    .await
                    .is_err(),
                "{private}: a request-named URL keeps the SSRF guard"
            );
        }
        assert!(
            validate_peer_url("http://172.18.0.2:3000", PeerUrlOrigin::Configured)
                .await
                .is_err(),
            "a configured target still rides TLS"
        );
    }

    /// The happy path: I builds a hello, L verifies + replies, I verifies the
    /// reply; both ends recover the peer's nest_id bytes.
    #[test]
    fn mutual_handshake_round_trips() {
        let i_sk = keypair();
        let l_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_id = nest_id_hex(&l_sk);
        let nonce = fresh_channel_nonce();
        let spki = hex::encode([7u8; 32]);

        let hello = build_hello(&i_sk, &i_id, &l_id, &nonce, &spki).unwrap();

        let (got_i, reply) = verify_hello_and_build_reply(&hello, &l_sk, &l_id, &spki).unwrap();
        assert_eq!(got_i, *i_sk.verifying_key().as_bytes());

        let got_l = verify_reply(&reply, &l_id, &i_id, &nonce, &spki).unwrap();
        assert_eq!(got_l, *l_sk.verifying_key().as_bytes());
    }

    /// The listener checks the carried SPKI against the cert it actually serves;
    /// a MITM presenting a different cert can't complete the handshake.
    #[test]
    fn listener_rejects_spki_mismatch() {
        let i_sk = keypair();
        let l_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_id = nest_id_hex(&l_sk);
        let nonce = fresh_channel_nonce();
        let claimed_spki = hex::encode([7u8; 32]);

        let hello = build_hello(&i_sk, &i_id, &l_id, &nonce, &claimed_spki).unwrap();
        let served_spki = hex::encode([9u8; 32]); // L serves a different cert
        let err = verify_hello_and_build_reply(&hello, &l_sk, &l_id, &served_spki).unwrap_err();
        assert!(matches!(err, HelloError::SpkiMismatch));
    }

    /// A hello signed for a *different* listener_nest_id fails when L
    /// reconstructs the tuple with its own id.
    #[test]
    fn listener_rejects_hello_addressed_to_another_nest() {
        let i_sk = keypair();
        let l_sk = keypair();
        let other_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_id = nest_id_hex(&l_sk);
        let other_id = nest_id_hex(&other_sk);
        let nonce = fresh_channel_nonce();
        let spki = hex::encode([7u8; 32]);

        // I signs the tuple binding `other_id`, but the wire carries it to L.
        let hello = build_hello(&i_sk, &i_id, &other_id, &nonce, &spki).unwrap();
        let err = verify_hello_and_build_reply(&hello, &l_sk, &l_id, &spki).unwrap_err();
        assert!(matches!(err, HelloError::BadSignature));
    }

    /// A tampered nonce on the wire invalidates the signature.
    #[test]
    fn listener_rejects_tampered_nonce() {
        let i_sk = keypair();
        let l_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_id = nest_id_hex(&l_sk);
        let nonce = fresh_channel_nonce();
        let spki = hex::encode([7u8; 32]);

        let mut hello = build_hello(&i_sk, &i_id, &l_id, &nonce, &spki).unwrap();
        hello.channel_nonce = fresh_channel_nonce(); // swap to a fresh, valid-shaped nonce
        let err = verify_hello_and_build_reply(&hello, &l_sk, &l_id, &spki).unwrap_err();
        assert!(matches!(err, HelloError::BadSignature));
    }

    /// A malformed nest_id is rejected before signature work.
    #[test]
    fn listener_rejects_malformed_initiator_id() {
        let l_sk = keypair();
        let l_id = nest_id_hex(&l_sk);
        let spki = hex::encode([7u8; 32]);
        let hello = FederationHello {
            initiator_nest_id: "not-hex".to_string(),
            channel_nonce: fresh_channel_nonce(),
            spki_sha256: spki.clone(),
            envelope: "00".repeat(100),
        };
        let err = verify_hello_and_build_reply(&hello, &l_sk, &l_id, &spki).unwrap_err();
        assert!(matches!(err, HelloError::BadNestId));
    }

    /// The initiator rejects a reply whose listener_nest_id isn't the discovered
    /// one (reached a different nest than intended).
    #[test]
    fn initiator_rejects_listener_mismatch() {
        let i_sk = keypair();
        let l_sk = keypair();
        let wrong_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_id = nest_id_hex(&l_sk);
        let nonce = fresh_channel_nonce();
        let spki = hex::encode([7u8; 32]);

        let hello = build_hello(&i_sk, &i_id, &l_id, &nonce, &spki).unwrap();
        let (_got_i, mut reply) =
            verify_hello_and_build_reply(&hello, &l_sk, &l_id, &spki).unwrap();
        // The initiator expected to reach `wrong_sk`'s nest.
        let wrong_id = nest_id_hex(&wrong_sk);
        let err = verify_reply(&reply, &wrong_id, &i_id, &nonce, &spki).unwrap_err();
        assert!(matches!(err, HelloError::ListenerMismatch));

        // And a forged reply *claiming* the expected id but signed by the wrong
        // key fails on the signature, not the id check.
        reply.listener_nest_id = l_id.clone();
        reply.envelope = sign_payload(
            &FederationHelloSig {
                initiator_nest_id: &i_id,
                listener_nest_id: &l_id,
                channel_nonce: &nonce,
                spki_sha256: &spki,
            },
            &wrong_sk,
        )
        .unwrap();
        let err = verify_reply(&reply, &l_id, &i_id, &nonce, &spki).unwrap_err();
        assert!(matches!(err, HelloError::BadSignature));
    }

    // ── WsMessageAdapter ───────────────────────────────────────────────────

    /// A `Message` transport that is *both* halves. A `stream::iter` mock no
    /// longer stands in for a socket here: the adapter drives the heartbeat from
    /// its Stream half, so it needs somewhere to put the Ping.
    struct MockWs {
        inbound: std::collections::VecDeque<Message>,
        sent: std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
        /// When the scripted inbound runs out: `false` ends the stream (the
        /// frame-mapping tests), `true` goes quiet forever, which is what a
        /// vanished peer looks like (the heartbeat tests).
        idle_when_drained: bool,
    }

    impl MockWs {
        fn new(inbound: Vec<Message>, idle_when_drained: bool) -> Self {
            Self {
                inbound: inbound.into(),
                sent: Default::default(),
                idle_when_drained,
            }
        }
        fn sent_handle(&self) -> std::sync::Arc<std::sync::Mutex<Vec<Message>>> {
            self.sent.clone()
        }
    }

    impl Stream for MockWs {
        type Item = Result<Message, std::convert::Infallible>;
        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            match this.inbound.pop_front() {
                Some(m) => Poll::Ready(Some(Ok(m))),
                None if this.idle_when_drained => Poll::Pending,
                None => Poll::Ready(None),
            }
        }
    }

    impl Sink<Message> for MockWs {
        type Error = std::convert::Infallible;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.get_mut().sent.lock().unwrap().push(item);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn test_heartbeat() -> crate::ws::WsHeartbeatPolicy {
        crate::ws::WsHeartbeatPolicy {
            ping_interval: std::time::Duration::from_secs(30),
            liveness_timeout: std::time::Duration::from_secs(60),
        }
    }

    /// Inbound `Binary` frames pass through as `Bytes`; `Ping`/`Pong` are
    /// skipped; a `Close` ends the stream (frames after it are never seen).
    #[tokio::test]
    async fn adapter_maps_binary_and_skips_control_frames() {
        use futures_util::StreamExt;

        let msgs = vec![
            Message::Ping(Bytes::from_static(b"p")),
            Message::Binary(Bytes::from_static(b"frame1")),
            Message::Pong(Bytes::from_static(b"q")),
            Message::Binary(Bytes::from_static(b"frame2")),
            Message::Close(None),
            Message::Binary(Bytes::from_static(b"after-close")),
        ];
        let adapter = WsMessageAdapter::new(MockWs::new(msgs, false));
        let got: Vec<Bytes> = adapter.map(|r| r.unwrap()).collect().await;
        assert_eq!(
            got,
            vec![Bytes::from_static(b"frame1"), Bytes::from_static(b"frame2")]
        );
    }

    /// A `Text` frame (no text frames in Spec Y — a protocol violation) ends
    /// the stream so the connection tears down.
    #[tokio::test]
    async fn adapter_text_frame_ends_stream() {
        use futures_util::StreamExt;

        let msgs = vec![
            Message::Binary(Bytes::from_static(b"f")),
            Message::Text("nope".into()),
            Message::Binary(Bytes::from_static(b"unreached")),
        ];
        let adapter = WsMessageAdapter::new(MockWs::new(msgs, false));
        let got: Vec<Bytes> = adapter.map(|r| r.unwrap()).collect().await;
        assert_eq!(got, vec![Bytes::from_static(b"f")]);
    }

    /// The server half, at the adapter level: a peer that goes quiet is pinged,
    /// and once a full liveness window passes with no answer the stream ends —
    /// which is how this layer tells the dispatcher's driver to tear the
    /// connection down and release the socket.
    #[tokio::test(start_paused = true)]
    async fn adapter_pings_then_ends_the_stream_on_a_peer_that_never_answers() {
        use futures_util::StreamExt;

        let ws = MockWs::new(vec![], true); // upgrades, then says nothing ever
        let sent = ws.sent_handle();
        let mut adapter = WsMessageAdapter::with_heartbeat(ws, test_heartbeat());

        // The stream ends of its own accord — no inbound frame ever arrives.
        assert!(
            adapter.next().await.is_none(),
            "a peer that answers no Ping for a full liveness window must end the stream"
        );
        let pings = sent.lock().unwrap();
        assert!(
            pings.iter().any(|m| matches!(m, Message::Ping(_))),
            "the adapter must have pinged before giving up, not merely idled out; sent: {pings:?}"
        );
    }

    /// The load-bearing counterpart: a peer that answers every Ping but sends
    /// nothing of its own is never reaped. An inbound-idle timeout would pass
    /// the test above and fail this one.
    #[tokio::test(start_paused = true)]
    async fn adapter_never_ends_the_stream_on_a_silent_peer_that_answers() {
        use futures_util::StreamExt;

        // Ten windows' worth of Pongs and nothing else — no application frame at
        // any point — then a Binary frame to give the stream something to yield.
        let mut msgs: Vec<Message> = (0..20).map(|_| Message::Pong(Bytes::new())).collect();
        msgs.push(Message::Binary(Bytes::from_static(b"alive")));
        let mut adapter =
            WsMessageAdapter::with_heartbeat(MockWs::new(msgs, true), test_heartbeat());

        assert_eq!(
            adapter.next().await.transpose().unwrap(),
            Some(Bytes::from_static(b"alive")),
            "a peer that answered every Ping must still be delivering frames; reaping it \
             would cut every legitimately quiet connection"
        );
    }

    // ── End-to-end handshake over an in-memory duplex ──────────────────────
    //
    // These drive the real channel mechanics (two `RpcDispatcher`s over one
    // duplex; `dialer_handshake` originates via `request_raw`, `listener_handshake`
    // serves via `inbound_requests` + `send_reply`) without axum or a full nest —
    // the two-nest tier_3 capstone over real WS is slice 3.6.

    use fauna_protocol::test_transport::make_pair;

    /// The full mutual handshake completes over the channel: the dialer
    /// originates `fauna.federation.hello`, the listener verifies + replies, the
    /// dialer verifies the reply; both ends recover the peer's `nest_id`.
    #[tokio::test]
    async fn channel_handshake_over_duplex_round_trips() {
        let (dialer_t, listener_t) = make_pair();
        let (disp_dialer, drv_d) = RpcDispatcher::new(dialer_t);
        let (disp_listener, drv_l) = RpcDispatcher::new(listener_t);
        // spawn-ok(test)
        tokio::spawn(drv_d);
        tokio::spawn(drv_l);
        let disp_dialer = Arc::new(disp_dialer);
        let disp_listener = Arc::new(disp_listener);

        let l_sk = keypair();
        let l_id = nest_id_hex(&l_sk);
        let i_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let nonce = fresh_channel_nonce();
        let spki = ""; // loopback: no TLS, empty SPKI on both ends

        // Listener: take the inbound stream, run the handshake on the first frame.
        let mut inbound = disp_listener.inbound_requests().unwrap();
        let disp_l2 = Arc::clone(&disp_listener);
        let l_id2 = l_id.clone();
        // spawn-ok(test)
        let listener_task = tokio::spawn(async move {
            let req = inbound.recv().await.unwrap();
            listener_handshake(&disp_l2, &l_sk, &l_id2, "", req)
                .await
                .unwrap()
        });

        // Dialer: originate the hello and verify the reply.
        let got_listener = dialer_handshake(&disp_dialer, &i_sk, &i_id, &l_id, &nonce, spki)
            .await
            .unwrap();
        assert_eq!(hex::encode(got_listener), l_id);

        let got_initiator = listener_task.await.unwrap();
        assert_eq!(hex::encode(got_initiator), i_id);
    }

    /// After a verified handshake, any non-`fauna.federation.*` kind over the
    /// channel is rejected `unauthenticated` (slice-3 contract; the connection
    /// stays open).
    #[tokio::test]
    async fn channel_rejects_non_federation_kind_after_handshake() {
        let (dialer_t, listener_t) = make_pair();
        let (disp_dialer, drv_d) = RpcDispatcher::new(dialer_t);
        let (disp_listener, drv_l) = RpcDispatcher::new(listener_t);
        // spawn-ok(test)
        tokio::spawn(drv_d);
        tokio::spawn(drv_l);
        let disp_dialer = Arc::new(disp_dialer);
        let disp_listener = Arc::new(disp_listener);

        let l_sk = keypair();
        let l_id = nest_id_hex(&l_sk);
        let i_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let nonce = fresh_channel_nonce();

        let mut inbound = disp_listener.inbound_requests().unwrap();
        let cancels = disp_listener.inbound_cancels();
        let disp_l2 = Arc::clone(&disp_listener);
        let l_id2 = l_id.clone();
        // A `for_test` state has an EMPTY federation_router, so its allowlist
        // (`contains`) rejects every kind — exactly the "non-federation kind →
        // unauthenticated" path under test. (The real serving of a registered
        // kind is the tier_3 `conformance_federation_channel` capstone, which
        // builds the populated router.)
        let state = Arc::new(crate::routes::AppState::for_test(Arc::new(
            crate::db::CacheDb::open_in_memory().unwrap(),
        )));
        // Detached: the serve_loop runs for the connection's life; the assertion
        // below completes before it would naturally end, and the `#[tokio::test]`
        // runtime aborts this task on return.
        // spawn-ok(test)
        tokio::spawn(async move {
            let req = inbound.recv().await.unwrap();
            let peer = listener_handshake(&disp_l2, &l_sk, &l_id2, "", req)
                .await
                .unwrap();
            let conn = Arc::new(FederationConnection::new(peer, Arc::clone(&disp_l2)));
            crate::dispatch_core::serve_loop(
                &state,
                &conn,
                &mut inbound,
                cancels,
                |state, conn, req| Box::pin(serve_request(state, conn, req)),
            )
            .await;
        });

        dialer_handshake(&disp_dialer, &i_sk, &i_id, &l_id, &nonce, "")
            .await
            .unwrap();

        // A client-actor kind over the federation channel → unauthenticated.
        let call = disp_dialer
            .request_raw("fauna.conversations.send", [1u8; 16], Value::Null, None)
            .await
            .unwrap();
        let err = call.await_reply().await.unwrap_err();
        assert_eq!(err.code, "fauna.protocol.unauthenticated");
    }

    /// A peer whose WS-RPC has no `fauna.federation.hello` kind (returns
    /// `unknown_kind`) is surfaced as `ChannelUnsupported` (the pool turns it
    /// into `PoolError::Unsupported`; no HTTP fallback, §4.F).
    #[tokio::test]
    async fn dialer_maps_unknown_hello_kind_to_channel_unsupported() {
        let (dialer_t, listener_t) = make_pair();
        let (disp_dialer, drv_d) = RpcDispatcher::new(dialer_t);
        let (disp_listener, drv_l) = RpcDispatcher::new(listener_t);
        // spawn-ok(test)
        tokio::spawn(drv_d);
        tokio::spawn(drv_l);
        let disp_dialer = Arc::new(disp_dialer);
        let disp_listener = Arc::new(disp_listener);

        // The "peer" answers every request with `unknown_kind` (it has no hello).
        let mut inbound = disp_listener.inbound_requests().unwrap();
        let disp_l2 = Arc::clone(&disp_listener);
        // spawn-ok(test)
        let peer_task = tokio::spawn(async move {
            let req = inbound.recv().await.unwrap();
            send_error(
                &disp_l2,
                req.correlation_id,
                RpcError::new("fauna.protocol.unknown_kind", "error.protocol.unknown_kind"),
            )
            .await;
        });

        let i_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_sk = keypair();
        let l_id = nest_id_hex(&l_sk);
        let nonce = fresh_channel_nonce();

        let err = dialer_handshake(&disp_dialer, &i_sk, &i_id, &l_id, &nonce, "")
            .await
            .unwrap_err();
        assert!(matches!(err, DialError::ChannelUnsupported));
        let _ = peer_task.await;
    }

    /// A peer that accepts the hello frame — the connection stays
    /// "alive" from the transport's point of view, e.g. by answering Pings —
    /// but never sends the `fauna.federation.hello` reply must not hang
    /// `dialer_handshake` forever. Only the substrate's dead-link timer could
    /// previously end this wait; here there is no substrate at all (an
    /// in-memory duplex), so a pre-fix run has *nothing* to end it on, which is
    /// the sharpest form of the bug: `dialer_handshake` must supply its own
    /// `HANDSHAKE_DEADLINE`-scoped backstop, not rely on the transport.
    #[tokio::test(start_paused = true)]
    async fn dialer_handshake_reply_wait_is_bounded_by_handshake_deadline() {
        let (dialer_t, listener_t) = make_pair();
        let (disp_dialer, drv_d) = RpcDispatcher::new(dialer_t);
        let (disp_listener, drv_l) = RpcDispatcher::new(listener_t);
        // spawn-ok(test)
        tokio::spawn(drv_d);
        tokio::spawn(drv_l);
        let disp_dialer = Arc::new(disp_dialer);
        let disp_listener = Arc::new(disp_listener);

        // The "peer" takes the hello request and then withholds the reply
        // indefinitely — never calling `send_reply`.
        let mut inbound = disp_listener.inbound_requests().unwrap();
        // spawn-ok(test)
        let peer_task = tokio::spawn(async move {
            let _req = inbound.recv().await.unwrap();
            std::future::pending::<()>().await
        });

        let i_sk = keypair();
        let i_id = nest_id_hex(&i_sk);
        let l_sk = keypair();
        let l_id = nest_id_hex(&l_sk);
        let nonce = fresh_channel_nonce();

        // A generous outer bound. Under paused time, tokio auto-advances to
        // whichever timer is soonest when every task is stalled — so this
        // resolves instantly either way, it never waits a real hour. Pre-fix,
        // `dialer_handshake` races nothing, so this outer timeout is the only
        // pending timer and always wins. Post-fix, `dialer_handshake`'s own
        // `HANDSHAKE_DEADLINE` timer is sooner and wins instead.
        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(120),
            dialer_handshake(&disp_dialer, &i_sk, &i_id, &l_id, &nonce, ""),
        )
        .await;
        let elapsed = started.elapsed();

        peer_task.abort();

        let result = outcome.unwrap_or_else(|_| {
            panic!(
                "dialer_handshake must not hang past HANDSHAKE_DEADLINE \
                 ({HANDSHAKE_DEADLINE:?}) when the peer withholds the hello reply"
            )
        });
        let err = result.expect_err("a withheld hello reply must not be treated as success");
        assert!(
            matches!(err, DialError::Handshake(_)),
            "expected a bounded handshake error, got {err:?}"
        );
        // The 120s outer bound only proves the wait ends, not that it ends AT
        // the deadline: a regression to a longer bound (e.g. `KEEPALIVE_TIMEOUT`,
        // 60s) or to a fresh per-stage timer would still resolve well inside
        // it and pass. Pin the elapsed paused time to `HANDSHAKE_DEADLINE`
        // itself, with a small tolerance for the reply-select's own overhead.
        assert!(
            elapsed >= HANDSHAKE_DEADLINE
                && elapsed < HANDSHAKE_DEADLINE + Duration::from_millis(20),
            "expected the wait to end at HANDSHAKE_DEADLINE ({HANDSHAKE_DEADLINE:?}), got {elapsed:?}"
        );
    }

    /// `connect_federation_ws` itself — the TCP/TLS/WS-upgrade phase, one step
    /// before `dialer_handshake` above — must not hang forever either. Before
    /// this fix neither `connect_async_tls_with_config` nor `connect_async` is
    /// raced against any local deadline; `tokio_tungstenite`'s own connect
    /// futures take no deadline (see `fauna-anon-client::ws::connect_anonymous`,
    /// which bounds the identical shape for the anon-client's own dial), so a
    /// peer that completes the TCP handshake but withholds the WS upgrade
    /// response hangs this call indefinitely. Exercises the loopback `ws://`
    /// branch — no TLS needed to reproduce the missing bound.
    #[tokio::test(start_paused = true)]
    async fn connect_federation_ws_bounds_a_blackholed_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral listener");
        let addr = listener.local_addr().expect("listener local_addr");

        // Accept the TCP connection but never write the WS-upgrade response —
        // a real peer that goes silent mid-handshake, not a fast
        // connection-refused.
        // spawn-ok(test)
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            std::future::pending::<()>().await;
            drop(stream); // unreachable — keeps `stream` alive for the compiler
        });

        let ws_url = format!("ws://{addr}/api/v1/federation/ws");
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            connect_federation_ws(&ws_url, false),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "connect_federation_ws must not hang past CONNECT_DEADLINE \
                 ({CONNECT_DEADLINE:?}) when the peer withholds the WS upgrade"
            )
        });
        let elapsed = started.elapsed();

        // `result`'s `Ok` side (`TungsteniteAdapter`) isn't `Debug`, so match
        // instead of formatting the whole `Result` via `expect_err`.
        match &result {
            Err(DialError::Handshake(_)) => {}
            Err(other) => panic!(
                "a connect timeout must map to DialError::Handshake, not \
                 ChannelUnsupported (that would wrongly trigger capability \
                 fallback for a merely slow peer): got {other:?}"
            ),
            Ok(_) => panic!(
                "expected a connect-timeout error, but connect succeeded — the \
                 blackholed listener didn't actually block the WS upgrade"
            ),
        }
        assert!(
            elapsed >= CONNECT_DEADLINE,
            "timeout fired before the deadline: {elapsed:?} < {CONNECT_DEADLINE:?}"
        );
        // A lower bound alone also passes a regression to a longer bound (the
        // 120s outer ceiling, or `KEEPALIVE_TIMEOUT`) or to a fresh timer —
        // pin the upper side too, with a small tolerance for the connect
        // future's own select overhead.
        assert!(
            elapsed < CONNECT_DEADLINE + Duration::from_millis(20),
            "timeout fired well after the deadline: {elapsed:?} >= {CONNECT_DEADLINE:?} + 20ms"
        );
    }

    /// The dialer reads with the same 2 MiB inbound cap the listener applies
    /// (`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`), not tungstenite's
    /// 64 MiB default: a paired peer answering with an oversized message ends
    /// the connection instead of being buffered whole.
    #[tokio::test]
    async fn connect_federation_ws_refuses_an_oversized_inbound_message() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::{
            Message,
            handshake::server::{Request, Response},
            http::HeaderValue,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral listener");
        let addr = listener.local_addr().expect("listener local_addr");
        let oversized = fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE + 1024;
        // spawn-ok(test)
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_hdr_async(
                stream,
                |_req: &Request, mut resp: Response| {
                    resp.headers_mut().insert(
                        "Sec-WebSocket-Protocol",
                        HeaderValue::from_static(FEDERATION_SUBPROTOCOL),
                    );
                    Ok(resp)
                },
            )
            .await
            .expect("ws accept");
            let _ = ws.send(Message::Binary(vec![0u8; oversized].into())).await;
            std::future::pending::<()>().await;
        });

        let ws_url = format!("ws://{addr}/api/v1/federation/ws");
        let Ok((mut adapter, _spki)) = connect_federation_ws(&ws_url, false).await else {
            panic!("the loopback upgrade should succeed");
        };
        let first = tokio::time::timeout(Duration::from_secs(30), adapter.next())
            .await
            .expect("the read ends, one way or the other");
        if let Some(Ok(frame)) = first {
            panic!(
                "a {} byte message was accepted past the {} byte cap",
                frame.len(),
                fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE
            );
        }
    }

    /// A federation dispatcher carrying the router-derived hint registry (what
    /// `dial` / `serve_listener` attach) emits the `replay_forbidden` wire hint
    /// for a forbid-replay federation kind — asserted on the **encoded frame
    /// the peer would receive**, exactly like the client-side twin
    /// (`dispatcher.rs::a_forbid_replay_kind_carries_the_wire_hint`). This is
    /// the half that turns the peer's "caller missing replay_forbidden hint"
    /// warning from firing-on-every-KP-fetch into a stale-peer detector.
    #[tokio::test]
    async fn a_federation_dispatcher_carries_the_wire_hint() {
        use futures_util::StreamExt;

        let (dialer_t, peer_t) = make_pair();
        let (dispatcher, driver) = RpcDispatcher::new(dialer_t);
        // spawn-ok(test)
        tokio::spawn(driver);

        let mut b = crate::federation_router::FederationRouter::builder();
        crate::federation_handlers::register_federation_handlers(&mut b);
        dispatcher
            .set_kind_registry(b.build().hint_registry())
            .unwrap();

        let _call = dispatcher
            .request_raw(
                "fauna.federation.keypackage.fetch",
                [0u8; 16],
                Value::Null,
                None,
            )
            .await
            .unwrap();
        let (_peer_sink, mut peer_stream) = futures_util::StreamExt::split(peer_t);
        let bytes = peer_stream.next().await.unwrap().unwrap();
        let req = match fauna_protocol::decode_frame(&bytes).unwrap() {
            fauna_protocol::Frame::Request(r) => r,
            other => panic!("expected Request, got {other:?}"),
        };
        assert_eq!(
            req.replay_forbidden,
            Some(true),
            "a forbid-replay federation kind must carry the wire hint"
        );

        // And the presence stays meaningful: a replay-permitted kind omits it.
        let (dialer_t2, peer_t2) = make_pair();
        let (dispatcher2, driver2) = RpcDispatcher::new(dialer_t2);
        // spawn-ok(test)
        tokio::spawn(driver2);
        let mut b2 = crate::federation_router::FederationRouter::builder();
        crate::federation_handlers::register_federation_handlers(&mut b2);
        dispatcher2
            .set_kind_registry(b2.build().hint_registry())
            .unwrap();
        let _call2 = dispatcher2
            .request_raw("fauna.federation.sync.pull", [1u8; 16], Value::Null, None)
            .await
            .unwrap();
        let (_s2, mut peer_stream2) = futures_util::StreamExt::split(peer_t2);
        let bytes2 = peer_stream2.next().await.unwrap().unwrap();
        let req2 = match fauna_protocol::decode_frame(&bytes2).unwrap() {
            fauna_protocol::Frame::Request(r) => r,
            other => panic!("expected Request, got {other:?}"),
        };
        assert_eq!(
            req2.replay_forbidden, None,
            "a replay-permitted federation kind must not carry the hint"
        );
    }
}
