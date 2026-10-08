//! The Y.1 peer channel — length-prefixed DAG-CBOR over the substrate-agnostic
//! `fauna-transport` byte stream.
//!
//! P2P (WireGuard or iroh) yields a raw, reliable, ordered [`ByteStream`] from
//! [`fauna_transport::PeerConn::open_stream`]. The L3 protocol (`fauna-protocol`'s
//! `RpcDispatcher` + DAG-CBOR `Frame`s) is transport-agnostic — it consumes any
//! `Stream<Item = Result<Bytes>> + Sink<Bytes>` (no WebSocket dependency; the L3
//! mandate enforced by `scripts/check-protocol-deps.sh`). The only missing piece
//! between the two is **L2 framing**: a raw byte stream has no message boundaries,
//! so each DAG-CBOR frame is delimited with a 4-byte big-endian length prefix
//! (`[u32 BE len][CBOR]`, ≤ 1 MiB per frame — Spec Y.1 §1.9 / `transport.md`
//! § Layers).
//!
//! [`PeerStreamAdapter`] is that adapter — the peer analog of the nest↔nest
//! federation channel's `WsMessageAdapter` (which bridges WebSocket *message*
//! boundaries to the same `Bytes` shape). Hand it to
//! `fauna_protocol::dispatcher::RpcDispatcher::new` to run Y.1 over any
//! `PeerConn`, exactly as the federation channel runs it over a WebSocket.
//!
//! This crate is **substrate-agnostic**: it depends only on `fauna-transport`
//! (the seam trait), so the same channel rides a WireGuard `PeerConn` or an iroh
//! `PeerConn` unchanged — which is why the Y.1 channel lives here, one layer up
//! from the WireGuard-flavoured `fauna-peer` crate.

#![forbid(unsafe_code)]

pub mod hardening;
mod node;
pub use node::{HandlerFactory, PeerNode, base_peer_handlers};

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use fauna_transport::ByteStream;
use futures_util::{Sink, Stream};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

/// Maximum bytes in one framed message (Spec Y.1 §1.9: a frame is ≤ 1 MiB).
///
/// Enforced on **both** directions by the underlying [`LengthDelimitedCodec`]:
/// an outbound frame larger than this fails to encode (`InvalidInput`), and an
/// inbound length prefix larger than this fails the stream — so a peer cannot
/// make us buffer an unbounded allocation by claiming a huge frame.
pub const MAX_FRAME_LEN: usize = 1024 * 1024;

/// Adapts a raw [`ByteStream`] into the `Stream<Item = Result<Bytes>> + Sink<Bytes>`
/// shape `RpcDispatcher::new` consumes, by length-delimiting each DAG-CBOR frame
/// (`[u32 BE len][CBOR]`, ≤ [`MAX_FRAME_LEN`]).
///
/// Each `Bytes` yielded by the [`Stream`] is exactly one complete frame; each
/// `Bytes` written to the [`Sink`] is framed with its length prefix. Partial
/// reads/writes across poll boundaries, frame reassembly, and the max-frame-length
/// cap are handled by the underlying [`LengthDelimitedCodec`].
pub struct PeerStreamAdapter {
    inner: Framed<ByteStream, LengthDelimitedCodec>,
}

impl PeerStreamAdapter {
    /// Wrap a peer byte stream (from [`fauna_transport::PeerConn::open_stream`])
    /// with `[u32 BE len][CBOR]` framing.
    pub fn new(stream: ByteStream) -> Self {
        let codec = LengthDelimitedCodec::builder()
            // 4-byte length prefix; big-endian is the codec default. The prefix
            // value is the payload length (no length_adjustment).
            .length_field_length(4)
            .max_frame_length(MAX_FRAME_LEN)
            .new_codec();
        Self {
            inner: Framed::new(stream, codec),
        }
    }
}

impl Stream for PeerStreamAdapter {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `LengthDelimitedCodec` decodes into `BytesMut`; the dispatcher wants
        // `Bytes`. `freeze()` is an O(1) handoff (no copy).
        match Pin::new(&mut self.get_mut().inner).poll_next(cx) {
            Poll::Ready(Some(Ok(buf))) => Poll::Ready(Some(Ok(buf.freeze()))),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Sink<Bytes> for PeerStreamAdapter {
    type Error = std::io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        Pin::new(&mut self.get_mut().inner).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_close(cx)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PeerChannel — the ergonomic Y.1 peer channel (slice 4)
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::{
    DispatchError, OUTBOUND_CAPACITY, Reply, Request, RpcDispatcher, RpcError, Value,
    decode_strict, encode_canonical,
};
use fauna_transport::{EndpointKey, PathKind, PeerConn, TransportError};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinHandle;

/// A reply payload, or an [`RpcError`] to send back as an `ok = false` reply.
pub type HandlerResult = Result<Value, RpcError>;

/// Encode a typed payload into an L3 [`Value`] by canonical-CBOR round-trip;
/// `None` on the (unreachable for a plain struct) encode failure.
///
/// **The one home for this round-trip on the peer planes.** It was hand-rolled
/// at five sites across four crates — this crate's own `error_payload` and
/// `node_info_reply`, plus byte-identical `to_value` pairs in `fauna-peer-sync`
/// and `fauna-peer-share` — with several doc comments pointing at another copy
/// as the shape they "mirror". A mirror that is a copy drifts; these cannot.
///
/// ⚠ **The fifth copy, `fauna-nest`'s `federation_channel::to_value`, is
/// deliberately NOT folded in** and is expected to stay: `fauna-nest` takes
/// `fauna-peer-channel` as a **dev-dependency only** (its Cargo.toml says so and
/// says why — "no cycle"), so routing a *production* nest path through this fn
/// would add a runtime crate-graph edge to the nest binary and its image
/// crate-list. That is a deliberate boundary, not an oversight; if it is ever
/// reconsidered, the change is a dependency decision first and a de-duplication
/// second.
///
/// Callers that must not fail use `.unwrap_or(Value::Null)`; the two peer server
/// planes map `None` onto their own plane-scoped internal error, which is
/// genuinely per-plane (`fauna.peer.sync.internal` vs `fauna.peer.share.internal`)
/// and deliberately stays at the call site.
pub fn to_value<T: serde::Serialize>(value: &T) -> Option<Value> {
    encode_canonical(value)
        .ok()
        .and_then(|bytes| decode_strict::<Value>(&bytes).ok())
}

/// Decode an L3 [`Value`] payload into a typed `T` by canonical-CBOR round-trip,
/// refusing with the catalog's malformed-payload error.
///
/// ⚠ The code is [`fauna.protocol.malformed`], the one
/// `fauna_protocol::RpcError::localized`/`action` actually recognise. The two
/// peer server copies this replaces both minted `fauna.protocol.malformed_payload`
/// with an `error.protocol.malformed_payload` key — **a code off the catalog and
/// an i18n key that does not exist in `en.yaml`** — so a malformed payload on
/// either peer plane rendered the generic fallback string instead of "The
/// request could not be processed." (`action()` still classified it `Rejected`,
/// its fail-safe default, so nothing retry-looped.)
pub fn from_value<T: serde::de::DeserializeOwned>(payload: &Value) -> Result<T, RpcError> {
    encode_canonical(payload)
        .ok()
        .and_then(|bytes| decode_strict::<T>(&bytes).ok())
        .ok_or_else(|| RpcError::new("fauna.protocol.malformed", "error.protocol.malformed"))
}

/// A boxed async handler for one inbound request kind.
type BoxHandler =
    Arc<dyn Fn(Request) -> Pin<Box<dyn Future<Output = HandlerResult> + Send>> + Send + Sync>;

/// A kind-routed handler map for a [`PeerChannel`]'s serve side.
///
/// Deliberately minimal (the dormant-foundation scope, `p2p.md` § Transport seam):
/// route by [`Request::kind`], reply with the handler's result, answer an unmapped
/// kind with `fauna.protocol.unknown_kind`. No idempotency dedup, no cancel
/// handling, no throttle/allowlist — those are forward items the nest↔nest
/// federation channel's internal `dispatch_core` carries, and a real consumer
/// layers on when P2P gains live traffic.
///
/// **The allowlist is deliberately not here, and that is now a written rule.**
/// `p2p.md` § Inbound authorization (ratified 2026-08-02) settles it: this layer
/// proves key possession and nothing more — per-kind admission against the P2P
/// contact set belongs to the handler or to a dispatch wrapper landed with the
/// first data-plane kind. That section also names the **trigger** at which the
/// check stops being optional, and requires a working removal affordance to land
/// with it. Read it before registering any kind beyond `fauna.peer.node_info`.
#[derive(Default)]
pub struct PeerHandlers {
    map: HashMap<String, BoxHandler>,
}

impl PeerHandlers {
    /// An empty handler map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for `kind`. The handler receives the full [`Request`]
    /// (so it can read `payload`, `idempotency_key`, …) and returns the reply
    /// payload or an [`RpcError`]. Builder-style — chain `.on(..)` calls.
    pub fn on<F, Fut>(mut self, kind: impl Into<String>, handler: F) -> Self
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult> + Send + 'static,
    {
        self.map
            .insert(kind.into(), Arc::new(move |req| Box::pin(handler(req))));
        self
    }
}

/// An error originating a request over a [`PeerChannel`].
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    /// The dispatcher could not send the request (the channel is closed).
    #[error("dispatch: {0}")]
    Dispatch(#[from] DispatchError),
    /// The peer answered with an error reply (`ok = false`). `.0.code` is the
    /// stable wire code (e.g. `fauna.protocol.unknown_kind`).
    #[error("peer rpc error: {}", .0.code)]
    Rpc(RpcError),
}

/// Aborts the wrapped task on drop — ties a spawned task's lifetime to this value.
struct AbortOnDrop(JoinHandle<()>);

impl AbortOnDrop {
    /// `true` while the wrapped task is still running (not finished, not aborted).
    /// Used by [`node::PeerNode::is_active`] to report whether the accept loop lives.
    fn is_running(&self) -> bool {
        !self.0.is_finished()
    }

    /// Abort the wrapped task and wait until it has ended — its future, and
    /// everything it owned, dropped. Plain drop only *requests* the abort; a
    /// caller that must know the task is gone before it moves on awaits this.
    async fn abort_and_wait(&mut self) {
        self.0.abort();
        // A cancelled join is the expected answer; a panic in the task was
        // already the task's to report.
        let _ = (&mut self.0).await;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How many inbound requests one peer-symmetric channel may have in flight at
/// once — the per-channel analog of nest's global
/// `dispatch_core::MAX_INFLIGHT_HANDLERS`, and the bound `transport.md`
/// § Request lifecycle step 5 rules for every serving path.
///
/// Sized to [`OUTBOUND_CAPACITY`] on purpose: each in-flight request owes
/// exactly one Reply to that queue, so at this cap **admission cannot outrun
/// what the outbound queue can drain**. Per-channel rather than global so one
/// stalled peer cannot starve the others.
pub const SERVE_MAX_INFLIGHT: usize = OUTBOUND_CAPACITY;

/// How long a served `Reply` may wait for room in the outbound queue before the
/// peer counts as not draining.
///
/// Generous — an order above any healthy write stall — because losing this race
/// is not a retry but a verdict: `transport.md` § Backpressure rules that a
/// client which isn't draining has a dead connection, so [`serve_requests`]
/// stops serving the channel rather than trying again.
pub const REPLY_ENQUEUE_BUDGET: Duration = Duration::from_secs(30);

/// Route one served `Reply` over `dispatcher`, bounded by
/// [`REPLY_ENQUEUE_BUDGET`] — the single reply-carriage door every
/// peer-symmetric serving surface takes (this crate's [`serve_requests`], and
/// nest's federation, sidecar and algorithm-client channels), so the budget is
/// stated once instead of nine hand-copies of a bare `send_reply`.
///
/// `Err(DispatchError::EnqueueTimeout)` means the peer stopped draining; the
/// caller stops serving that channel. `Err(DispatchError::Closed)` means the
/// transport is already gone and there is nothing left to stop.
pub async fn send_reply_bounded(
    dispatcher: &RpcDispatcher,
    correlation_id: u64,
    payload: Value,
    ok: bool,
) -> Result<(), DispatchError> {
    dispatcher
        .send_reply_bounded(
            Reply {
                ty: Reply::TYPE,
                correlation_id,
                payload,
                ok,
            },
            tokio::time::sleep(REPLY_ENQUEUE_BUDGET),
        )
        .await
}

/// Drain `inbound`, dispatching each request via `dispatch` on its own task (so
/// one slow handler can't head-of-line-block the others) and sending the reply
/// back over `dispatcher`. Runs until `inbound` closes, or until the peer stops
/// draining its side of the channel.
///
/// The shared inner loop of every peer-symmetric request-serve surface in this
/// codebase: [`PeerChannel::serve`] wraps a call to this in its own outer
/// `tokio::spawn` (to hand back a droppable [`ServeGuard`]); a caller that
/// instead wants to block until the channel closes — e.g. the nest's
/// sidecar-channel dialer loop — awaits this directly. `dispatch` gets the raw
/// [`Request`] and returns `(reply_payload, ok)`; how it looks up / runs a
/// handler is entirely up to the caller.
///
/// **Admission is bounded two ways**, and both are load-bearing:
///
/// 1. A [`SERVE_MAX_INFLIGHT`] permit is acquired **before** each spawn and held
///    across the handler *and* its reply, so a peer cannot pile up serve tasks.
///    Once the cap is reached this `acquire` parks the loop, `inbound` fills,
///    and the driver's `try_send` drops further requests — the backpressure
///    valve `RpcDispatcher::new` documents.
/// 2. The reply enqueue is bounded by [`REPLY_ENQUEUE_BUDGET`], so a permit is
///    always returned. Without it the cap alone would wedge the channel: a peer
///    that never reads would park [`SERVE_MAX_INFLIGHT`] tasks in `send_reply`
///    forever and no permit would ever come back.
///
/// A reply that loses its budget race means the peer is not draining, which
/// `transport.md` § Backpressure rules a dead connection, so the loop **stops
/// serving**. It cannot close the transport itself — the channel's owner holds
/// that — but it admits nothing further, and dropping `inbound` makes the driver
/// discard the peer's subsequent requests at the door.
pub async fn serve_requests<F, Fut>(
    mut inbound: mpsc::Receiver<Request>,
    dispatcher: Arc<RpcDispatcher>,
    dispatch: F,
) where
    F: Fn(Request) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = (Value, bool)> + Send + 'static,
{
    let in_flight = Arc::new(Semaphore::new(SERVE_MAX_INFLIGHT));
    // Set by a serve task whose Reply could not be enqueued for the whole
    // budget. Read after `acquire` rather than before `recv`, because that is
    // the point the loop is guaranteed to reach again: a task can only signal
    // while holding a permit, and releasing it is what wakes us here.
    let peer_stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));

    while let Some(req) = inbound.recv().await {
        let permit = match Arc::clone(&in_flight).acquire_owned().await {
            Ok(p) => p,
            // Unreachable: nothing closes this semaphore. Stop serving rather
            // than spawn unpermitted work if it ever becomes reachable.
            Err(_) => break,
        };
        if peer_stalled.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }

        let dispatcher = Arc::clone(&dispatcher);
        let dispatch = dispatch.clone();
        let peer_stalled = Arc::clone(&peer_stalled);
        tokio::spawn(async move {
            // Released only after the Reply has been enqueued (or the budget
            // has run out) — holding it across the reply is what makes the cap
            // bound the *channel*, not just the handler pool.
            let _permit = permit;
            let correlation_id = req.correlation_id;
            let (payload, ok) = dispatch(req).await;
            if let Err(DispatchError::EnqueueTimeout) =
                send_reply_bounded(&dispatcher, correlation_id, payload, ok).await
            {
                peer_stalled.store(true, std::sync::atomic::Ordering::Release);
            }
        });
    }
}

/// A running peer-symmetric serve loop. Drop it to stop serving inbound requests
/// (the [`PeerChannel`] can still originate). Runs concurrently with
/// [`PeerChannel::request`] over the same channel.
pub struct ServeGuard {
    #[allow(dead_code)] // held only for its `Drop` (aborts the serve loop).
    task: AbortOnDrop,
}

/// A peer-symmetric Y.1 RPC channel hosted over one [`PeerConn`] byte stream.
///
/// Wraps a [`fauna_transport::PeerConn`]'s
/// [`open_stream`](fauna_transport::PeerConn::open_stream) (or any
/// externally-obtained [`ByteStream`]) in the L2 [`PeerStreamAdapter`] + the L3
/// `RpcDispatcher`, spawning the dispatcher driver. Each end may both
/// [`request`](Self::request) (originate) and [`serve`](Self::serve) (answer) over
/// the same channel — the peer analog of the nest↔nest federation channel, which
/// runs the same dispatcher over a WebSocket.
///
/// **PT-2/PT-3 (security review; tracked internally).** The
/// transport only proves the remote controls the key reported by
/// [`peer_identity`](Self::peer_identity); this wrapper does **not** authorize the
/// peer. The caller, one layer above the seam, MUST apply the per-pair auth witness
/// to `peer_identity()` (actor-key for client↔client, mutual nest-key for
/// nest↔nest) before trusting requests over the channel.
pub struct PeerChannel {
    dispatcher: Arc<RpcDispatcher>,
    peer: EndpointKey,
    /// The path given at [`over_stream`](Self::over_stream) — what
    /// [`path`](Self::path) answers when the channel holds no connection.
    path: PathKind,
    /// The underlying connection when the channel was opened via
    /// [`open`](Self::open) — held to keep it alive for the channel's
    /// lifetime, and read live by [`path`](Self::path). `None` for
    /// [`over_stream`](Self::over_stream), where the caller owns the connection.
    conn: Option<Box<dyn PeerConn>>,
    #[allow(dead_code)] // held only for its `Drop` (aborts the dispatcher driver).
    driver: AbortOnDrop,
    /// Flips to `true` when the dispatcher driver ends (the transport closed);
    /// its sender is dropped instead if the driver is aborted. Either wakes
    /// [`closed`](Self::closed).
    driver_done: watch::Receiver<bool>,
}

impl PeerChannel {
    /// Host a channel over an already-established byte stream plus the verified
    /// peer identity + path — the lower-level constructor. Use it on the side that
    /// *accepted* a stream (or for any externally-obtained [`ByteStream`]); the
    /// caller keeps the underlying connection alive for the channel's lifetime.
    pub fn over_stream(stream: ByteStream, peer: EndpointKey, path: PathKind) -> Self {
        Self::host(stream, peer, path, None)
    }

    /// Open a fresh byte stream over `conn` and host a channel on it — the
    /// stream-*initiating* side. `peer_identity()`/`path()` are read from `conn`,
    /// which the channel then owns (keeping the connection alive until the channel
    /// is dropped).
    pub async fn open(conn: Box<dyn PeerConn>) -> Result<Self, TransportError> {
        let peer = conn.peer_identity();
        let path = conn.path();
        let stream = conn.open_stream().await?;
        Ok(Self::host(stream, peer, path, Some(conn)))
    }

    fn host(
        stream: ByteStream,
        peer: EndpointKey,
        path: PathKind,
        conn: Option<Box<dyn PeerConn>>,
    ) -> Self {
        let (dispatcher, driver) = RpcDispatcher::new(PeerStreamAdapter::new(stream));
        let (done_tx, driver_done) = watch::channel(false);
        Self {
            dispatcher: Arc::new(dispatcher),
            peer,
            path,
            conn,
            driver: AbortOnDrop(tokio::spawn(async move {
                driver.await;
                let _ = done_tx.send(true);
            })),
            driver_done,
        }
    }

    /// Resolves once the channel's transport has ended — the peer hung up, the
    /// stream failed, or the channel was dropped. Owns its state, so the
    /// channel itself may be moved or dropped while this is awaited.
    pub(crate) fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut done = self.driver_done.clone();
        async move {
            // `Err` = the sender is gone, i.e. the driver was aborted: closed too.
            let _ = done.wait_for(|done| *done).await;
        }
    }

    /// The transport-proven remote identity. Apply the per-pair auth witness to
    /// this (PT-2/PT-3) before trusting the peer.
    pub fn peer_identity(&self) -> EndpointKey {
        self.peer
    }

    /// Which path the underlying connection runs over (LAN / WAN-direct /
    /// relay) — read from the held connection on every call, so a relayed
    /// connection the substrate has since turned direct answers direct
    /// (`fauna_transport::bytes_may_ride` re-reads it before each byte pull).
    /// An [`over_stream`](Self::over_stream) channel holds no connection and
    /// answers the path it was given.
    pub fn path(&self) -> PathKind {
        self.conn.as_ref().map_or(self.path, |conn| conn.path())
    }

    /// Originate a request over the channel and await the peer's reply.
    ///
    /// A fresh random idempotency key is generated per call (the same
    /// `getrandom::fill` shape as `fauna-sidecar-client`'s `fresh_idem`);
    /// at-least-once retry/dedup is a forward item, so no two calls reuse a key. No
    /// deadline is attached — the caller can race this against its own timeout.
    pub async fn request(&self, kind: &str, payload: Value) -> Result<Value, ChannelError> {
        let call = self
            .dispatcher
            .request_raw(kind, fresh_idem(), payload, None)
            .await?;
        call.await_reply().await.map_err(ChannelError::Rpc)
    }

    /// Spawn the peer-symmetric serve loop: drain inbound peer requests, route each
    /// by [`kind`](Request::kind) to `handlers`, and reply. An unmapped kind gets a
    /// `fauna.protocol.unknown_kind` error reply (additive-evolution safe — an old
    /// peer answers a new kind with this, never a hang). Each request is served on
    /// its own task so a slow handler can't head-of-line block the others.
    ///
    /// Returns a [`ServeGuard`]; drop it to stop serving. Call at most once per
    /// channel (`inbound_requests` is take-once) — a second call panics.
    pub fn serve(&self, handlers: PeerHandlers) -> ServeGuard {
        let inbound = self
            .dispatcher
            .inbound_requests()
            .expect("PeerChannel::serve called twice (inbound_requests is take-once)");
        let dispatcher = Arc::clone(&self.dispatcher);
        let handlers = Arc::new(handlers);
        let task = tokio::spawn(serve_requests(inbound, dispatcher, move |req| {
            let handlers = Arc::clone(&handlers);
            async move {
                let handler = handlers.map.get(&req.kind).cloned();
                match handler {
                    Some(h) => match h(req).await {
                        Ok(v) => (v, true),
                        Err(e) => (error_payload(&e), false),
                    },
                    None => (error_payload(&unknown_kind(&req.kind)), false),
                }
            }
        }));
        ServeGuard {
            task: AbortOnDrop(task),
        }
    }
}

/// Encode an [`RpcError`] into an L3 `Value` reply payload; `Value::Null` on the
/// (unreachable for a plain struct) encode failure.
fn error_payload(err: &RpcError) -> Value {
    to_value(err).unwrap_or(Value::Null)
}

/// The error reply for a request whose kind has no registered handler.
fn unknown_kind(kind: &str) -> RpcError {
    RpcError::new("fauna.protocol.unknown_kind", "error.protocol.unknown_kind")
        .with_details_text(kind)
}

/// A fresh random 16-byte idempotency key — same shape as `fauna-sidecar-client`'s
/// `fresh_idem` (priority #4: reuse the audited pattern, don't drift a new one).
fn fresh_idem() -> [u8; 16] {
    let mut key = [0u8; 16];
    getrandom::fill(&mut key).expect("getrandom failed");
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::AsyncWriteExt;

    /// A [`PeerConn`] whose path the test flips — the substrate turning a
    /// relayed connection direct after the channel opened.
    struct FlippingConn {
        stream: std::sync::Mutex<Option<ByteStream>>,
        path: Arc<std::sync::Mutex<PathKind>>,
    }

    #[async_trait::async_trait]
    impl PeerConn for FlippingConn {
        async fn open_stream(&self) -> Result<ByteStream, TransportError> {
            self.stream
                .lock()
                .unwrap()
                .take()
                .ok_or(TransportError::Unsupported)
        }
        fn peer_identity(&self) -> EndpointKey {
            EndpointKey::from_bytes([3u8; 32])
        }
        fn path(&self) -> PathKind {
            *self.path.lock().unwrap()
        }
    }

    /// `path()` on an opened channel reads the held connection live (ruling
    /// 4's "re-read before each pull"), never a value captured at `open`.
    #[tokio::test]
    async fn an_opened_channel_reads_its_path_live() {
        let (a, _b) = tokio::io::duplex(64 * 1024);
        let path = Arc::new(std::sync::Mutex::new(PathKind::Relay));
        let channel = PeerChannel::open(Box::new(FlippingConn {
            stream: std::sync::Mutex::new(Some(Box::pin(a))),
            path: Arc::clone(&path),
        }))
        .await
        .expect("open");
        assert_eq!(channel.path(), PathKind::Relay);
        *path.lock().unwrap() = PathKind::WanDirect;
        assert_eq!(
            channel.path(),
            PathKind::WanDirect,
            "a relayed connection the substrate turned direct answers direct"
        );
    }

    /// A loopback duplex gives both ends a [`ByteStream`]; frames written on one
    /// adapter arrive intact and in order on the other — including a larger frame
    /// and a zero-length frame.
    #[tokio::test]
    async fn round_trips_framed_payloads_in_order() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let mut tx = PeerStreamAdapter::new(Box::pin(a));
        let mut rx = PeerStreamAdapter::new(Box::pin(b));

        let payloads: Vec<Bytes> = vec![
            Bytes::from_static(b"first"),
            Bytes::from(vec![7u8; 4096]), // a larger, multi-read frame
            Bytes::new(),                 // a zero-length frame
            Bytes::from_static(b"last"),
        ];

        for p in &payloads {
            tx.send(p.clone()).await.expect("send frame");
        }

        for expected in &payloads {
            let got = rx.next().await.expect("a frame").expect("no io error");
            assert_eq!(&got, expected);
        }
    }

    /// Two frames written back-to-back must arrive as two distinct frames — the
    /// length prefix delimits them, the decoder must not coalesce.
    #[tokio::test]
    async fn adjacent_frames_are_not_coalesced() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let mut tx = PeerStreamAdapter::new(Box::pin(a));
        let mut rx = PeerStreamAdapter::new(Box::pin(b));

        tx.send(Bytes::from_static(b"ab")).await.unwrap();
        tx.send(Bytes::from_static(b"cd")).await.unwrap();

        assert_eq!(rx.next().await.unwrap().unwrap(), Bytes::from_static(b"ab"));
        assert_eq!(rx.next().await.unwrap().unwrap(), Bytes::from_static(b"cd"));
    }

    /// An outbound frame larger than [`MAX_FRAME_LEN`] fails to encode rather than
    /// being sent — the cap is enforced on the send side.
    #[tokio::test]
    async fn oversize_outbound_frame_is_rejected() {
        let (a, _b) = tokio::io::duplex(64 * 1024);
        let mut tx = PeerStreamAdapter::new(Box::pin(a));
        let too_big = Bytes::from(vec![0u8; MAX_FRAME_LEN + 1]);
        let err = tx.send(too_big).await.expect_err("oversize must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// An inbound length prefix larger than [`MAX_FRAME_LEN`] fails the stream
    /// rather than letting a peer force an unbounded allocation.
    #[tokio::test]
    async fn oversize_inbound_length_prefix_is_rejected() {
        let (mut writer, b) = tokio::io::duplex(64);
        let mut rx = PeerStreamAdapter::new(Box::pin(b));

        // Hand-write a length prefix claiming > 1 MiB (no payload follows).
        let bogus_len = (MAX_FRAME_LEN as u32) + 1;
        writer.write_all(&bogus_len.to_be_bytes()).await.unwrap();
        writer.flush().await.unwrap();

        let item = rx.next().await.expect("a stream item");
        assert!(item.is_err(), "oversize inbound length must error");
    }

    /// A clean EOF (the writer half dropped, no partial frame buffered) ends the
    /// stream with `None`, not an error.
    #[tokio::test]
    async fn clean_eof_ends_stream() {
        let (a, b) = tokio::io::duplex(64);
        let mut rx = PeerStreamAdapter::new(Box::pin(b));
        drop(a); // writer gone → clean EOF
        assert!(rx.next().await.is_none(), "clean EOF yields None");
    }

    /// The Y.1 "re-host" proof: the L3 `RpcDispatcher` over [`PeerStreamAdapter`]
    /// (L2) over a raw byte-stream duplex (what `PeerConn::open_stream` yields)
    /// round-trips a real Request/Reply — the length-prefix framing and the
    /// kind-routed, peer-symmetric dispatcher compose exactly as the nest↔nest
    /// federation channel composes them over a WebSocket, with **no** WS
    /// dependency. This is the claim that makes the peer migration "mechanical".
    #[tokio::test]
    async fn rpc_dispatcher_round_trips_over_peer_stream_adapter() {
        use std::sync::Arc;

        use fauna_protocol::dispatcher::RpcDispatcher;
        use fauna_protocol::envelope::Reply;

        let (a, b) = tokio::io::duplex(64 * 1024);
        let (disp_a, driver_a) = RpcDispatcher::new(PeerStreamAdapter::new(Box::pin(a)));
        let (disp_b, driver_b) = RpcDispatcher::new(PeerStreamAdapter::new(Box::pin(b)));
        tokio::spawn(driver_a);
        tokio::spawn(driver_b);

        // B serves the peer-originated request via the peer-symmetric surface
        // (inbound_requests → send_reply), the federation-channel pattern.
        let disp_b = Arc::new(disp_b);
        let mut b_requests = disp_b.inbound_requests().expect("inbound_requests");
        let disp_b_serve = Arc::clone(&disp_b);
        let serve = tokio::spawn(async move {
            let req = b_requests.recv().await.expect("a peer request");
            assert_eq!(req.kind, "fauna.peer.node_info");
            disp_b_serve
                .send_reply(Reply {
                    ty: Reply::TYPE,
                    correlation_id: req.correlation_id,
                    payload: Value::String("node-info-ok".into()),
                    ok: true,
                })
                .await
                .expect("send reply");
        });

        // A originates over the channel and gets B's reply.
        let call = disp_a
            .request_raw("fauna.peer.node_info", [0u8; 16], Value::Null, None)
            .await
            .expect("request");
        let reply = call.await_reply().await.expect("reply");
        assert_eq!(reply, Value::String("node-info-ok".into()));
        serve.await.unwrap();
    }

    // ── PeerChannel ergonomic-wrapper tests (slice 4) ──────────────────────────
    //
    // These exercise the wrapper over a `tokio::io::duplex` pair (what
    // `PeerConn::open_stream` yields). The full composition over a REAL iroh
    // loopback connection lives in `fauna-iroh`'s `transport.rs` tests (which own
    // the iroh harness), keeping this crate's dep graph — and the slice-7
    // consumers' inner loop — iroh-free.

    use fauna_protocol::{RpcError, Value};
    use fauna_transport::{EndpointKey, PathKind};

    /// A duplex pair gives both ends a `ByteStream`. Two `PeerChannel`s over it:
    /// A originates `fauna.peer.node_info`, B serves it — the ergonomic-API form
    /// of the re-host proof above.
    #[tokio::test]
    async fn request_serve_round_trips_over_channel() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let alice = PeerChannel::over_stream(
            Box::pin(a),
            EndpointKey::from_bytes([2u8; 32]),
            PathKind::Lan,
        );
        let bob = PeerChannel::over_stream(
            Box::pin(b),
            EndpointKey::from_bytes([1u8; 32]),
            PathKind::WanDirect,
        );

        let _serve = bob.serve(
            PeerHandlers::new().on("fauna.peer.node_info", |_req| async move {
                Ok(Value::String("node-info-ok".into()))
            }),
        );

        let reply = alice
            .request("fauna.peer.node_info", Value::Null)
            .await
            .expect("reply");
        assert_eq!(reply, Value::String("node-info-ok".into()));
    }

    /// `peer_identity()`/`path()` return exactly what the channel was built with —
    /// the values the caller applies the PT-2/PT-3 auth witness to.
    #[tokio::test]
    async fn exposes_peer_identity_and_path() {
        let key = EndpointKey::from_bytes([5u8; 32]);
        let (a, _b) = tokio::io::duplex(64);
        let ch = PeerChannel::over_stream(Box::pin(a), key, PathKind::Relay);
        assert_eq!(ch.peer_identity(), key);
        assert_eq!(ch.path(), PathKind::Relay);
    }

    /// A request for a kind the serving peer has no handler for comes back as a
    /// `fauna.protocol.unknown_kind` error reply (additive-evolution safe — an old
    /// peer answers a new kind with this, never a hang), surfaced as
    /// [`ChannelError::Rpc`].
    #[tokio::test]
    async fn unknown_kind_replies_with_unknown_kind_error() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let alice = PeerChannel::over_stream(
            Box::pin(a),
            EndpointKey::from_bytes([2u8; 32]),
            PathKind::Lan,
        );
        let bob = PeerChannel::over_stream(
            Box::pin(b),
            EndpointKey::from_bytes([1u8; 32]),
            PathKind::Lan,
        );
        // B serves, but registers no handlers.
        let _serve = bob.serve(PeerHandlers::new());

        let err = alice
            .request("fauna.peer.nope", Value::Null)
            .await
            .expect_err("unmapped kind must error");
        match err {
            ChannelError::Rpc(e) => assert_eq!(e.code, "fauna.protocol.unknown_kind"),
            other => panic!("expected Rpc(unknown_kind), got {other:?}"),
        }
    }

    /// A handler that returns an [`RpcError`] is answered with an `ok = false`
    /// reply carrying that error, surfaced to the originator as
    /// [`ChannelError::Rpc`] with the handler's code.
    #[tokio::test]
    async fn handler_error_surfaces_as_rpc_error() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let alice = PeerChannel::over_stream(
            Box::pin(a),
            EndpointKey::from_bytes([2u8; 32]),
            PathKind::Lan,
        );
        let bob = PeerChannel::over_stream(
            Box::pin(b),
            EndpointKey::from_bytes([1u8; 32]),
            PathKind::Lan,
        );
        let _serve = bob.serve(
            PeerHandlers::new().on("fauna.peer.boom", |_req| async move {
                Err(RpcError::new("fauna.peer.boom_failed", "error.peer.boom"))
            }),
        );

        let err = alice
            .request("fauna.peer.boom", Value::Null)
            .await
            .expect_err("handler error must surface");
        match err {
            ChannelError::Rpc(e) => assert_eq!(e.code, "fauna.peer.boom_failed"),
            other => panic!("expected Rpc(boom_failed), got {other:?}"),
        }
    }

    /// The channel is genuinely peer-symmetric: each end both serves one kind and
    /// originates the other's, over the *same* two channels.
    #[tokio::test]
    async fn channel_is_symmetric_both_serve_and_request() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let alice = PeerChannel::over_stream(
            Box::pin(a),
            EndpointKey::from_bytes([2u8; 32]),
            PathKind::Lan,
        );
        let bob = PeerChannel::over_stream(
            Box::pin(b),
            EndpointKey::from_bytes([1u8; 32]),
            PathKind::Lan,
        );

        let _serve_a = alice.serve(
            PeerHandlers::new().on("fauna.peer.ping", |_req| async move {
                Ok(Value::String("pong-from-alice".into()))
            }),
        );
        let _serve_b = bob.serve(
            PeerHandlers::new().on("fauna.peer.node_info", |_req| async move {
                Ok(Value::String("info-from-bob".into()))
            }),
        );

        let from_bob = alice
            .request("fauna.peer.node_info", Value::Null)
            .await
            .expect("bob replies");
        let from_alice = bob
            .request("fauna.peer.ping", Value::Null)
            .await
            .expect("alice replies");
        assert_eq!(from_bob, Value::String("info-from-bob".into()));
        assert_eq!(from_alice, Value::String("pong-from-alice".into()));
    }

    /// Slice 6: the **real typed** `fauna.peer.*` kinds ride the channel
    /// end-to-end. Bob serves `fauna.peer.node_info` + `fauna.peer.exchange` with
    /// the `fauna_protocol::peer` wire structs; Alice originates each and decodes
    /// the typed reply — the previous tests used ad-hoc `Value`s, this proves the
    /// kind set itself round-trips over a `PeerChannel`. The exchange is
    /// iroh-flavoured — there is no substrate key to carry, the NodeId being the
    /// field the iroh focus adds.
    #[tokio::test]
    async fn typed_peer_kinds_round_trip_over_channel() {
        use fauna_protocol::peer::{
            KIND_PEER_EXCHANGE, KIND_PEER_NODE_INFO, PEER_PROTOCOL_VERSION, PeerExchangeReply,
            PeerExchangeRequest, PeerNodeInfoReply, PeerNodeInfoRequest,
        };
        use fauna_protocol::{decode_strict, encode_canonical};

        // struct → CBOR `Value` and back (the federation channel's `to_value`).
        fn to_value<T: serde::Serialize>(t: &T) -> Value {
            decode_strict::<Value>(&encode_canonical(t).unwrap()).unwrap()
        }
        fn from_value<T: serde::de::DeserializeOwned>(v: &Value) -> T {
            decode_strict::<T>(&encode_canonical(v).unwrap()).unwrap()
        }

        let (a, b) = tokio::io::duplex(64 * 1024);
        let alice = PeerChannel::over_stream(
            Box::pin(a),
            EndpointKey::from_bytes([2u8; 32]),
            PathKind::Lan,
        );
        let bob = PeerChannel::over_stream(
            Box::pin(b),
            EndpointKey::from_bytes([1u8; 32]),
            PathKind::Lan,
        );

        let _serve = bob.serve(
            PeerHandlers::new()
                .on(KIND_PEER_NODE_INFO, |_req| async move {
                    Ok(to_value(&PeerNodeInfoReply {
                        protocol_version: PEER_PROTOCOL_VERSION,
                        display_name: "bob-node".into(),
                        ..Default::default()
                    }))
                })
                .on(KIND_PEER_EXCHANGE, |req| async move {
                    let got: PeerExchangeRequest = from_value(&req.payload);
                    assert_eq!(got.display_name, "alice");
                    Ok(to_value(&PeerExchangeReply {
                        actor_id: "ef".repeat(32),
                        display_name: "bob".into(),
                        ..Default::default()
                    }))
                }),
        );

        // fauna.peer.node_info
        let reply = alice
            .request(
                KIND_PEER_NODE_INFO,
                to_value(&PeerNodeInfoRequest::default()),
            )
            .await
            .expect("node_info reply");
        let info: PeerNodeInfoReply = from_value(&reply);
        assert_eq!(info.protocol_version, PEER_PROTOCOL_VERSION);
        assert_eq!(info.display_name, "bob-node");

        // fauna.peer.exchange
        let reply = alice
            .request(
                KIND_PEER_EXCHANGE,
                to_value(&PeerExchangeRequest {
                    actor_id: "ab".repeat(32),
                    display_name: "alice".into(),
                    nonce_signature: "cd".repeat(64),
                    ..Default::default()
                }),
            )
            .await
            .expect("exchange reply");
        let ex: PeerExchangeReply = from_value(&reply);
        assert_eq!(ex.display_name, "bob");
    }

    /// **Serve-side admission is bounded.** An authenticated peer that sends
    /// continuously and reads nothing must not be able to spawn serve tasks
    /// without limit.
    ///
    /// The concurrency fix that split the dispatcher driver into independent
    /// read/write halves removed the *only* thing that had bounded this:
    /// before it, a parked `sink.send` stopped `stream.next()` from being
    /// polled at all, so a stalled peer's own backpressure reached the wire and
    /// nothing further was admitted. Afterwards `read_half` ingested at full
    /// rate while `write_half` was parked, and `serve_requests` spawned one
    /// unconstrained task per inbound Request — each ending in an unbudgeted
    /// `send_reply` that could never complete. The bound is now stated on this
    /// side, where it belongs: reverting the driver split would only bring back
    /// the DoS it closed (`transport.md` § Request lifecycle step 5).
    ///
    /// Latency-independent by construction: every admitted request parks in its
    /// handler, so the in-flight count *is* the admission count, and the paused
    /// clock's auto-advance is a quiescence barrier — it moves only once every
    /// task is idle, so the assertion reads a settled state rather than racing
    /// one (`testing.md` convention 14).
    /// Drive `serve_requests` against a peer that accepts no bytes, feeding
    /// `flood` requests, and return how many were concurrently admitted.
    ///
    /// Every handler parks, so the count is *concurrent admissions*, not a
    /// running total — which is the number the bound is about.
    #[cfg(test)]
    async fn admitted_under_a_stalled_peer(flood: usize) -> usize {
        use fauna_protocol::envelope::{Frame, encode_frame};
        use fauna_protocol::test_transport::stalled_sink_transport;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (transport, feed) = stalled_sink_transport();
        let (dispatcher, driver) = RpcDispatcher::new(transport);
        let dispatcher = Arc::new(dispatcher);
        tokio::spawn(driver);

        // Fill the outbound queue first, so every reply this serve loop
        // produces has nowhere to go — the stalled-peer condition itself.
        let mut _held = Vec::new();
        for _ in 0..(OUTBOUND_CAPACITY + 1) {
            _held.push(
                dispatcher
                    .request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None)
                    .await
                    .expect("the queue still has room for this one"),
            );
        }

        let in_flight = Arc::new(AtomicUsize::new(0));
        let inbound = dispatcher.inbound_requests().expect("inbound_requests");
        let counter = Arc::clone(&in_flight);
        tokio::spawn(serve_requests(
            inbound,
            Arc::clone(&dispatcher),
            move |_req| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Park in the handler: an admitted request stays admitted,
                    // so the counter reads concurrent admissions, not a total.
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            },
        ));

        for corr in 0..flood {
            let frame = encode_frame(&Frame::Request(Request {
                ty: Request::TYPE,
                correlation_id: corr as u64,
                kind: "fauna.peer.node_info".into(),
                idempotency_key: [0u8; 16],
                payload: Value::Null,
                replay_forbidden: None,
                deadline_ms: None,
            }))
            .expect("encode request frame");
            if feed.send(frame).await.is_err() {
                break;
            }
        }

        // Quiescence barrier, not a settle-sleep: under `start_paused` the
        // clock advances only when every task is idle, and this deadline is far
        // shorter than `REPLY_ENQUEUE_BUDGET`, so it is reached first.
        tokio::time::sleep(Duration::from_millis(1)).await;
        in_flight.load(Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_reads_cannot_admit_unbounded_serve_tasks() {
        // Two feed sizes, an order apart, because the number that proves a
        // bound is one the *system* chose: a count that tracks the feed is a
        // measurement of the probe, not of the code under test. Before the
        // bound existed these read 256 and 2048 respectively.
        let small = admitted_under_a_stalled_peer(SERVE_MAX_INFLIGHT * 4).await;
        let large = admitted_under_a_stalled_peer(SERVE_MAX_INFLIGHT * 32).await;

        assert_eq!(
            (small, large),
            (SERVE_MAX_INFLIGHT, SERVE_MAX_INFLIGHT),
            "admission must stop at the SERVE_MAX_INFLIGHT ({SERVE_MAX_INFLIGHT}) \
             permits and not move with the feed size, so it cannot outrun what \
             the outbound queue can drain; got {small} admitted at a feed of {} \
             and {large} at a feed of {}",
            SERVE_MAX_INFLIGHT * 4,
            SERVE_MAX_INFLIGHT * 32,
        );
    }

    /// **A served request must not spawn work that never retires.** The cap
    /// above is only half the bound: held across an *unbudgeted* reply it would
    /// wedge the channel instead of saving it — `SERVE_MAX_INFLIGHT` tasks
    /// parked forever in `send_reply`, no permit ever returned, and the peer
    /// served nothing again for the life of the process.
    ///
    /// So the reply enqueue is bounded too, and `transport.md` § Backpressure
    /// rules what losing that race means: *replies cannot be dropped; if the
    /// client isn't draining, the connection is dead*. Every serve task
    /// therefore retires, and the loop stops serving the channel.
    #[tokio::test(start_paused = true)]
    async fn a_served_reply_that_cannot_be_enqueued_retires_the_task() {
        use fauna_protocol::envelope::{Frame, encode_frame};
        use fauna_protocol::test_transport::stalled_sink_transport;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (transport, feed) = stalled_sink_transport();
        let (dispatcher, driver) = RpcDispatcher::new(transport);
        let dispatcher = Arc::new(dispatcher);
        tokio::spawn(driver);

        let mut _held = Vec::new();
        for _ in 0..(OUTBOUND_CAPACITY + 1) {
            _held.push(
                dispatcher
                    .request_raw("fauna.protocol.echo", [0u8; 16], Value::Null, None)
                    .await
                    .expect("the queue still has room for this one"),
            );
        }

        // Handlers return immediately, so every admitted task goes straight to
        // the reply enqueue and parks there against the full outbound queue.
        let served = Arc::new(AtomicUsize::new(0));
        let inbound = dispatcher.inbound_requests().expect("inbound_requests");
        let counter = Arc::clone(&served);
        let serve = tokio::spawn(serve_requests(
            inbound,
            Arc::clone(&dispatcher),
            move |_req| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    (Value::Null, true)
                }
            },
        ));

        for corr in 0..(SERVE_MAX_INFLIGHT * 2) {
            let frame = encode_frame(&Frame::Request(Request {
                ty: Request::TYPE,
                correlation_id: corr as u64,
                kind: "fauna.peer.node_info".into(),
                idempotency_key: [0u8; 16],
                payload: Value::Null,
                replay_forbidden: None,
                deadline_ms: None,
            }))
            .expect("encode request frame");
            if feed.send(frame).await.is_err() {
                break;
            }
        }

        // A generous ceiling on the declared budget, not a settle-sleep: the
        // only correct outcome is that the loop gives up on this peer once the
        // budget it declared has been spent.
        let stopped = tokio::time::timeout(REPLY_ENQUEUE_BUDGET * 10, serve).await;
        assert!(
            stopped.is_ok(),
            "the serve loop must stop serving a peer that never drains its \
             replies, not park on it forever; {} requests reached a handler",
            served.load(Ordering::SeqCst),
        );
    }
}
