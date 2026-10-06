//! L1+L2 adapter: tungstenite WebSocket → Bytes-shaped Stream+Sink.
//!
//! A thin glue layer that wraps a connected `WebSocketStream` as a
//! `Stream<Item = Result<Bytes, AdapterError>>` + `Sink<Bytes>` ready to feed
//! [`fauna_protocol::RpcDispatcher::new`], and drives the spec heartbeat (a WS
//! Ping every [`KEEPALIVE_INTERVAL`], dead-link detection after
//! [`KEEPALIVE_TIMEOUT`] of silence). The *connect* step — building the URL,
//! attaching the auth handshake (bearer subprotocol for clients; the nest-key
//! `fauna.federation.hello` for the federation channel), and TLS pinning — is
//! the consumer's job; this module owns only what is substrate-neutral.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Period between client-initiated WS Ping frames (spec § Connection lifecycle
/// "Heartbeat": ~30 s). Keeps the link warm across idle-timeout devices on the
/// path and lets a half-open connection be detected far sooner than OS-level
/// TCP keepalive.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// If no inbound frame (Pong to our Ping, or any other traffic) arrives within
/// this window, the link is treated as dead and the stream ends with `Retry` so
/// the supervisor reconnects proactively instead of waiting for an RPC to time
/// out. 2× the ping interval gives one full miss of slack.
pub const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the adapter saw when the WebSocket terminated. The supervisor
/// uses this to decide whether to reconnect, refresh auth first, or stop.
/// Per spec § 1.7 close code table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectSignal {
    /// 1000 Normal — stop the supervisor.
    CleanDisconnect,
    /// 4401 — refresh the bearer token, then reconnect.
    AuthExpired,
    /// 4426 — version skew; surface SubprotocolMismatch and stop.
    SubprotocolMismatch,
    /// All other close reasons (1001/1006/1011/4400/network) — backoff and reconnect.
    Retry,
}

impl ReconnectSignal {
    fn from_close(close: Option<&CloseFrame>) -> Self {
        let Some(c) = close else {
            return Self::Retry;
        };
        match u16::from(c.code) {
            1000 => Self::CleanDisconnect,
            4401 => Self::AuthExpired,
            4426 => Self::SubprotocolMismatch,
            _ => Self::Retry,
        }
    }
}

/// Ping/dead-link clock, shared by every WS adapter that owns **both**
/// directions of its socket inside one `poll_next` (as opposed to a two-task
/// loop whose sink lives on the other side of a channel from the deadline
/// check — those use a `select!`-based shape instead, e.g. `fauna-nest`'s
/// `ws::ServerHeartbeat`, and are not this driver's concern).
///
/// Coalesces every elapsed ping tick into one pending flag — tracked
/// separately from actually *sending* the Ping, since the caller's sink may
/// not be ready on a given poll — and tracks an absolute liveness deadline,
/// re-armed on every inbound frame. Deliberately message-type-agnostic: this
/// is the clock and nothing else, and it never looks at a frame.
///
/// The loop that *drives* it is [`poll_ws_frames`], and it is shared too — the
/// per-transport vocabulary that kept the two `poll_next` bodies apart (which
/// `Message` enum variant is a Ping, what each inbound variant means) now lives
/// behind [`WsFrames`] rather than being duplicated by each caller.
pub struct HeartbeatDriver {
    /// Fires every keepalive period; each elapsed tick arms `pending_ping`.
    ping_interval: Interval,
    /// A Ping is due but the sink wasn't ready last poll; retry without
    /// blocking reads on the flush.
    pending_ping: bool,
    /// Deadline future, re-armed on every inbound frame. Elapses → dead link.
    liveness: Pin<Box<Sleep>>,
    /// Window the `liveness` deadline is reset to on inbound activity.
    liveness_timeout: Duration,
}

impl HeartbeatDriver {
    /// `ping_period` is the Ping cadence; `liveness_timeout` is the no-inbound
    /// window after which the link is declared dead.
    pub fn new(ping_period: Duration, liveness_timeout: Duration) -> Self {
        // Skip the immediate first tick so the first Ping lands one period in,
        // not at connect time; coalesce missed ticks rather than bursting.
        let mut ping_interval = tokio::time::interval_at(Instant::now() + ping_period, ping_period);
        ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            ping_interval,
            pending_ping: false,
            liveness: Box::pin(tokio::time::sleep(liveness_timeout)),
            liveness_timeout,
        }
    }

    /// Coalesce every ping-interval tick elapsed since the last poll into one
    /// pending Ping. Call first on every `poll_next` pass, before `ping_due`.
    pub fn poll_tick(&mut self, cx: &mut Context<'_>) {
        while self.ping_interval.poll_tick(cx).is_ready() {
            self.pending_ping = true;
        }
    }

    /// A Ping is due and hasn't been handed to the sink yet.
    pub fn ping_due(&self) -> bool {
        self.pending_ping
    }

    /// Call once the Ping has actually been handed to the sink (`start_send`
    /// succeeded) — clears `ping_due` so the same tick isn't resent.
    pub fn mark_ping_sent(&mut self) {
        self.pending_ping = false;
    }

    /// Any inbound frame proves the link is alive — call before inspecting
    /// it, so every frame (including control frames) counts as liveness.
    pub fn note_inbound(&mut self) {
        self.liveness
            .as_mut()
            .reset(Instant::now() + self.liveness_timeout);
    }

    /// True once a full liveness window has passed with no inbound frame at
    /// all — not even a Pong to our Ping. Call only when the inner poll
    /// returned `Pending`, since an ongoing read is itself progress.
    pub fn poll_dead(&mut self, cx: &mut Context<'_>) -> bool {
        self.liveness.as_mut().poll(cx).is_ready()
    }

    /// The configured liveness window, for a caller's own dead-link log line.
    pub fn liveness_timeout(&self) -> Duration {
        self.liveness_timeout
    }
}

/// What [`poll_ws_frames`] should do with one inbound frame, decided by the
/// transport's own [`WsFrames`] impl. Liveness has already been re-armed by the
/// time the classification runs, so a `Skip` frame still counted as proof the
/// link is alive — which is the whole point of a Pong.
#[derive(Debug)]
pub enum FrameAction {
    /// A data frame: yield this payload from the stream.
    Yield(Bytes),
    /// Not data (a control frame, or one this transport tolerates and
    /// ignores): keep polling for the next frame.
    Skip,
    /// End the stream. Any signal the transport wants to record about *why*
    /// it ended is its own impl's business, set before returning this.
    End,
}

/// The message-enum half of [`poll_ws_frames`] — everything the shared loop
/// cannot know, because each transport brings its own `Message` enum from its
/// own crate.
///
/// Two WebSocket transports meet in this workspace and neither one's enum is a
/// re-export of the other's: `tokio-tungstenite`'s (the client/outbound side,
/// on a raw TCP/TLS socket) and axum's (the server/inbound side, on an
/// already-upgraded connection). What they share is the *driving loop*, not the
/// vocabulary — so the loop lives once in [`poll_ws_frames`] and each side
/// supplies four things here: how to build a Ping, how to build a binary data
/// frame, what each inbound variant means, and what to do when the link goes
/// silent. Implementors are the natural home for whatever per-connection state
/// those decisions need (the client side records a [`ReconnectSignal`]; the
/// server side has none and only logs).
pub trait WsFrames {
    /// This transport's WebSocket message enum.
    type Message;

    /// The outbound keepalive Ping.
    fn ping() -> Self::Message;

    /// One outbound binary data frame — the payload half of a `Sink<Bytes>`.
    fn binary(payload: Bytes) -> Self::Message;

    /// Classify one inbound frame. Called after liveness is re-armed, so an
    /// impl never has to think about the deadline.
    fn classify(&mut self, msg: Self::Message) -> FrameAction;

    /// A full liveness window elapsed with no inbound frame at all — not even
    /// a Pong to our own Ping. Called once, immediately before the loop ends
    /// the stream, so an impl can record a reason or log one.
    fn on_dead_link(&mut self, liveness_timeout: Duration);
}

/// The heartbeat-driving `poll_next` body every adapter in this family is.
///
/// Poll the keepalive clock, best-effort emit a Ping if one is due, read the
/// inner stream, and — only when that read is `Pending`, since an in-flight
/// read is itself progress — check whether the link has gone dead. Writing
/// from inside `poll_next` is exclusive: the dispatcher's `split()` BiLock
/// serialises the Stream-half (here) against the Sink-half that carries RPC
/// traffic, which is why the Ping can be sent from a read poll at all.
///
/// The Ping is deliberately best-effort. If the sink is not ready this pass,
/// the tick stays pending in the [`HeartbeatDriver`] and the loop falls through
/// to the read rather than blocking reads on a flush.
///
/// `inner`, `heartbeat` and `frames` are taken as three separate `&mut` so a
/// caller can pass three disjoint fields of its own adapter struct.
pub fn poll_ws_frames<S, F, E>(
    inner: &mut S,
    heartbeat: &mut HeartbeatDriver,
    frames: &mut F,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<Bytes, E>>>
where
    F: WsFrames,
    S: Stream<Item = Result<F::Message, E>> + Sink<F::Message, Error = E> + Unpin,
{
    loop {
        // 1. Keepalive clock: coalesce every elapsed tick into one pending Ping.
        heartbeat.poll_tick(cx);
        // 2. Best-effort emit the periodic Ping (see the doc comment).
        if heartbeat.ping_due()
            && Pin::new(&mut *inner).poll_ready(cx).is_ready()
            && Pin::new(&mut *inner).start_send(F::ping()).is_ok()
        {
            heartbeat.mark_ping_sent();
            let _ = Pin::new(&mut *inner).poll_flush(cx);
        }
        // 3. Read the inner stream.
        match Pin::new(&mut *inner).poll_next(cx) {
            Poll::Ready(Some(Ok(msg))) => {
                // Any inbound frame proves the link is alive — re-arm the
                // liveness deadline BEFORE classifying, so control frames
                // count too.
                heartbeat.note_inbound();
                match frames.classify(msg) {
                    FrameAction::Yield(b) => return Poll::Ready(Some(Ok(b))),
                    FrameAction::Skip => continue,
                    FrameAction::End => return Poll::Ready(None),
                }
            }
            Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => {
                // No inbound data. If the liveness deadline has elapsed with no
                // frame at all, the link is dead: let the transport record or
                // log its reason, then end the stream.
                if heartbeat.poll_dead(cx) {
                    frames.on_dead_link(heartbeat.liveness_timeout());
                    return Poll::Ready(None);
                }
                return Poll::Pending;
            }
        }
    }
}

/// Install the ring rustls `CryptoProvider` as the process default, once.
/// Required before the first `wss://` handshake — rustls 0.23 (tokio-tungstenite's
/// TLS) panics in `connect_async` without a process-default provider. Idempotent;
/// a provider another component already set is left in place.
///
/// Re-exported from `fauna-tls-bootstrap` (the shared home; `fauna-mail`'s
/// native IMAP dial uses it directly rather than depending on this crate's
/// full WS-transport stack for one function) so every existing caller of
/// this path keeps compiling unchanged.
pub use fauna_tls_bootstrap::ensure_tls_provider;

/// Bytes-shaped Stream+Sink wrapping a tungstenite WebSocketStream.
/// `Stream` yields `Bytes` for each incoming binary frame (text/ping/pong are filtered).
/// `Sink<Bytes>` writes each Bytes payload as a binary WS frame.
///
/// Generic over the inner IO `S` (`MaybeTlsStream<TcpStream>` in production) so
/// tests can pair two ends with `tokio::io::duplex` and observe the keepalive
/// Ping on the wire. The adapter also drives the spec's heartbeat: it emits a
/// WS Ping every `KEEPALIVE_INTERVAL` and ends the stream with `Retry` if no
/// inbound frame arrives within `KEEPALIVE_TIMEOUT` (dead-link detection).
pub struct TungsteniteAdapter<S = MaybeTlsStream<TcpStream>> {
    inner: WebSocketStream<S>,
    frames: TungsteniteFrames,
    heartbeat: HeartbeatDriver,
}

/// The tungstenite half of [`WsFrames`], plus the one piece of per-connection
/// state its decisions produce: the close-derived [`ReconnectSignal`] the
/// supervisor reads once the stream has ended.
///
/// Unlike the server side, this transport reaches `poll_next` with Text, Ping,
/// Pong and `Frame` messages still in play (there is no framing layer
/// below it answering them), so it tolerates all of them as liveness-only
/// rather than treating an unexpected variant as an error.
#[derive(Debug, Default)]
pub struct TungsteniteFrames {
    /// Last close frame observed on the stream; used by the supervisor
    /// to classify the disconnect after the stream ends.
    last_close: Option<ReconnectSignal>,
}

impl WsFrames for TungsteniteFrames {
    type Message = Message;

    fn ping() -> Message {
        Message::Ping(Bytes::new())
    }

    fn binary(payload: Bytes) -> Message {
        Message::Binary(payload)
    }

    fn classify(&mut self, msg: Message) -> FrameAction {
        match msg {
            Message::Binary(b) => FrameAction::Yield(b),
            Message::Close(frame) => {
                self.last_close = Some(ReconnectSignal::from_close(frame.as_ref()));
                FrameAction::End
            }
            // Text/Ping/Pong/Frame are not part of Y.1; skip rather than error
            // so the stream is robust against a non-conforming peer.
            // They still count as liveness (the deadline was re-armed
            // before this call).
            _ => FrameAction::Skip,
        }
    }

    fn on_dead_link(&mut self, _liveness_timeout: Duration) {
        // The supervisor reconnects proactively rather than waiting for an RPC
        // to time out; `Retry` is what tells it so.
        self.last_close = Some(ReconnectSignal::Retry);
    }
}

impl<S> TungsteniteAdapter<S> {
    /// Wrap a connected `WebSocketStream` with the heartbeat machinery.
    /// `ping_period` is the Ping cadence; `liveness_timeout` is the no-inbound
    /// window after which the link is declared dead. Production callers pass
    /// [`KEEPALIVE_INTERVAL`] / [`KEEPALIVE_TIMEOUT`]; tests vary them.
    pub fn new(
        inner: WebSocketStream<S>,
        ping_period: Duration,
        liveness_timeout: Duration,
    ) -> Self {
        Self {
            inner,
            frames: TungsteniteFrames::default(),
            heartbeat: HeartbeatDriver::new(ping_period, liveness_timeout),
        }
    }

    /// Returns the close-code-derived reason once the stream has ended.
    /// `None` while the stream is still alive.
    pub fn last_signal(&self) -> Option<ReconnectSignal> {
        self.frames.last_close.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("ws transport: {0}")]
    Transport(String),
}

impl<S> Stream for TungsteniteAdapter<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Item = Result<Bytes, AdapterError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // The loop is [`poll_ws_frames`]; `TungsteniteFrames` supplies the
        // tungstenite-specific half. All that is left here is this transport's
        // error shape — the inner error is stringified into `AdapterError`,
        // which the axum-side twin does not do (it forwards its error as-is).
        poll_ws_frames(&mut this.inner, &mut this.heartbeat, &mut this.frames, cx)
            .map(|item| item.map(|frame| frame.map_err(|e| AdapterError::Transport(e.to_string()))))
    }
}

impl<S> Sink<Bytes> for TungsteniteAdapter<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Error = AdapterError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .poll_ready(cx)
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }

    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .start_send(TungsteniteFrames::binary(item))
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .poll_flush(cx)
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner)
            .poll_close(cx)
            .map_err(|e| AdapterError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Role;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    /// Pair two in-memory WebSocket ends over a `duplex` pipe. Returns the
    /// adapter wrapping the client end (with the given heartbeat parameters)
    /// and the raw server-side `WebSocketStream` for assertions.
    async fn paired_adapter(
        ping_period: Duration,
        liveness_timeout: Duration,
    ) -> (
        TungsteniteAdapter<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let client_ws = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let server_ws = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        (
            TungsteniteAdapter::new(client_ws, ping_period, liveness_timeout),
            server_ws,
        )
    }

    /// Phase 1: the adapter emits a WS Ping every interval. Observe the actual
    /// frame on the server end, not just that a clock ticked.
    #[tokio::test(start_paused = true)]
    async fn sends_periodic_ping_to_server() {
        // Long liveness so dead-link detection can't interfere with this test.
        let (mut adapter, mut server) =
            paired_adapter(Duration::from_millis(50), Duration::from_secs(3600)).await;

        // The adapter never yields a binary frame here, so its `next()` parks
        // forever; the server-side `next()` resolves once the Ping lands. With
        // paused time the runtime auto-advances to the interval's next tick.
        let server_msg = tokio::select! {
            _ = adapter.next() => unreachable!("adapter yielded an unexpected item"),
            msg = server.next() => msg,
        };
        assert!(
            matches!(server_msg, Some(Ok(Message::Ping(_)))),
            "expected the server to receive a WS Ping, got {server_msg:?}"
        );
    }

    /// Phase 2: if no inbound frame (not even a Pong) arrives within the
    /// liveness window, the stream ends with `Retry` so the supervisor
    /// reconnects proactively. We hold the server end open but never poll it,
    /// so it neither reads our Pings nor sends a Pong.
    #[tokio::test(start_paused = true)]
    async fn terminates_with_retry_on_pong_timeout() {
        let (mut adapter, _server) =
            paired_adapter(Duration::from_millis(50), Duration::from_millis(120)).await;

        let item = adapter.next().await;
        assert!(
            item.is_none(),
            "expected the stream to end on liveness timeout, got {item:?}"
        );
        assert_eq!(adapter.last_signal(), Some(ReconnectSignal::Retry));
    }

    /// Phase 2: inbound traffic re-arms the liveness deadline, so a live link
    /// with a responsive peer never trips the dead-link timeout. The server
    /// echoes the client's Pings as Pongs by being polled in lockstep.
    #[tokio::test(start_paused = true)]
    async fn inbound_pong_keeps_link_alive_past_timeout() {
        let (mut adapter, mut server) =
            paired_adapter(Duration::from_millis(50), Duration::from_millis(120)).await;

        // Run well past the 120 ms liveness window. The server replies to each
        // Ping with a Pong (tungstenite auto-queues it; flushing a frame sends
        // it), which the adapter treats as liveness — so it must NOT terminate.
        let result = tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                tokio::select! {
                    item = adapter.next() => return item,
                    srv = server.next() => {
                        match srv {
                            // Bounce the auto-queued Pong back to the client.
                            Some(Ok(Message::Ping(p))) => {
                                let _ = server.send(Message::Pong(p)).await;
                            }
                            Some(Ok(_)) => {}
                            // Server end closed/errored — irrelevant to this assertion.
                            Some(Err(_)) | None => return None,
                        }
                    }
                }
            }
        })
        .await;
        assert!(
            result.is_err(),
            "adapter terminated despite a responsive peer: {result:?}"
        );
    }

    /// **A stalled write does not block dead-link detection** — the real
    /// `TungsteniteAdapter`/[`poll_ws_frames`] counterpart to
    /// `fauna_protocol::dispatcher`'s `a_blocked_write_does_not_stall_inbound_reads`,
    /// which only proves the property against the fake
    /// `stalled_sink_transport` double, with no heartbeat machinery at all.
    ///
    /// Before the fix, the `RpcDispatcher` driver's `sink.send(...).await`
    /// sat inside a `select!` *arm body*, so a stalled write starved
    /// `stream.next()` — the only place [`poll_ws_frames`] ever runs its
    /// keepalive/dead-link check — and no timer of any kind covered it. This test proves the substrate's OWN
    /// dead-link timer still fires while a write to the *same* adapter is
    /// genuinely stalled — not synthetically: a real `tokio::io::duplex` pipe
    /// whose peer end is never drained, so the write pends on backpressure
    /// exactly as a peer with a shut receive window would cause.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_write_does_not_block_dead_link_detection() {
        let (adapter, _server) =
            paired_adapter(Duration::from_millis(50), Duration::from_millis(120)).await;
        let (mut sink, mut stream) = adapter.split();

        // The duplex pipe is 8 KiB; nobody ever polls `_server`, so a payload
        // well past that fills it and this send genuinely pends on
        // backpressure rather than completing.
        let stalled_write = tokio::spawn(async move {
            let _ = sink.send(Bytes::from(vec![0u8; 64 * 1024])).await;
        });

        let item = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect(
                "dead-link detection must fire even while this adapter's own \
                 write is stalled",
            );
        assert!(
            item.is_none(),
            "expected the stream to end on liveness timeout, got {item:?}"
        );

        stalled_write.abort();
    }

    /// **Integration: an `RpcDispatcher` driving a REAL adapter still detects
    /// a dead link while its own write is stalled** — the test above proves
    /// the adapter's heartbeat survives a stalled write in isolation; this
    /// wires `RpcDispatcher::new` to a genuine `TungsteniteAdapter`/duplex
    /// pipe (not the fake `stalled_sink_transport` double
    /// `fauna_protocol::dispatcher`'s own tests use) to prove the *driver*
    /// actually lets that survive through, end to end.
    ///
    /// Before the fix, the driver's `sink.send(...).await` sat inside a
    /// `select!` arm body, so a stalled write starved `stream.next()` — the
    /// only place the adapter's dead-link check ever runs — and no timer of
    /// any kind covered it. Saturate the outbound
    /// queue against a peer that never reads, so the driver's write is
    /// genuinely stalled, and confirm a pending call still resolves —
    /// `Disconnected`, once dead-link detection ends the driver — well inside
    /// the liveness window rather than hanging.
    #[tokio::test(start_paused = true)]
    async fn a_saturated_dispatcher_over_a_real_adapter_still_detects_a_dead_link() {
        let (adapter, _server) =
            paired_adapter(Duration::from_millis(50), Duration::from_millis(120)).await;
        let (dispatcher, driver) = fauna_protocol::RpcDispatcher::new(adapter);
        tokio::spawn(driver);

        // Saturate: one frame parked in the driver's hand (stuck in the
        // stalled sink), the rest filling the bounded channel behind it —
        // same shape as `fauna_protocol::dispatcher`'s `saturate_outbound`.
        // Each payload is deliberately large (4 KiB): the duplex pipe is 8
        // KiB total, and nobody drains `_server`, so a genuine wire-level
        // stall must happen well before all of these are enqueued — a small
        // payload can fit dozens of frames in 8 KiB without ever blocking the
        // driver's write, which would make this test pass for the wrong
        // reason (queue-level saturation without any actual stalled write).
        let mut held = Vec::new();
        for _ in 0..(fauna_protocol::OUTBOUND_CAPACITY + 1) {
            held.push(
                tokio::time::timeout(
                    Duration::from_secs(10),
                    dispatcher.request_raw(
                        "fauna.protocol.echo",
                        [0u8; 16],
                        fauna_protocol::Value::String("x".repeat(4096)),
                        None,
                    ),
                )
                .await
                .expect("the queue still has room for this one")
                .expect("the transport is open"),
            );
        }

        let call = held.pop().expect("at least one call was made");
        let result = tokio::time::timeout(Duration::from_secs(5), call.await_reply())
            .await
            .expect(
                "dead-link detection must resolve every pending call even with \
                 this adapter's write stalled",
            );
        assert!(
            matches!(&result, Err(e) if e.code == fauna_protocol::DISCONNECTED_CODE),
            "expected dead-link detection to resolve the call as Disconnected, got {result:?}"
        );
    }

    fn close_with(code: u16) -> Option<CloseFrame> {
        Some(CloseFrame {
            code: CloseCode::from(code),
            reason: "".into(),
        })
    }

    #[test]
    fn close_code_1000_is_clean() {
        assert_eq!(
            ReconnectSignal::from_close(close_with(1000).as_ref()),
            ReconnectSignal::CleanDisconnect
        );
    }

    #[test]
    fn close_code_4401_is_auth_expired() {
        assert_eq!(
            ReconnectSignal::from_close(close_with(4401).as_ref()),
            ReconnectSignal::AuthExpired
        );
    }

    #[test]
    fn close_code_4426_is_subprotocol_mismatch() {
        assert_eq!(
            ReconnectSignal::from_close(close_with(4426).as_ref()),
            ReconnectSignal::SubprotocolMismatch
        );
    }

    #[test]
    fn close_code_1011_falls_back_to_retry() {
        assert_eq!(
            ReconnectSignal::from_close(close_with(1011).as_ref()),
            ReconnectSignal::Retry
        );
    }

    #[test]
    fn missing_close_frame_is_retry() {
        assert_eq!(ReconnectSignal::from_close(None), ReconnectSignal::Retry);
    }
}
