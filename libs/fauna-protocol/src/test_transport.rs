//! The `MpscTransport` + `make_pair()` in-memory dispatcher test double.
//!
//! Four crates hand-rolled the identical `Stream`/`Sink` pair over a bounded
//! `tokio::sync::mpsc` channel to drive dispatcher/RPC-channel unit tests
//! without a real socket (this crate's own [`crate::dispatcher`] tests, plus
//! three `bins/fauna-nest` in-file test modules) — this is the one definition
//! they now all share.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::{Sink, Stream};
use tokio::sync::{mpsc, oneshot};

/// One end of an in-memory bidirectional transport pair — a bounded mpsc
/// channel in each direction, framed as raw [`Bytes`].
pub struct MpscTransport {
    rx: mpsc::Receiver<Bytes>,
    tx: mpsc::Sender<Bytes>,
}

impl Stream for MpscTransport {
    type Item = Result<Bytes, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

impl Sink<Bytes> for MpscTransport {
    type Error = Infallible;
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let _ = self.tx.try_send(item);
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Build both ends of an in-memory transport pair, each side's outbound
/// channel wired to the other's inbound.
pub fn make_pair() -> (MpscTransport, MpscTransport) {
    let (a_tx, b_rx) = mpsc::channel(64);
    let (b_tx, a_rx) = mpsc::channel(64);
    (
        MpscTransport { rx: a_rx, tx: a_tx },
        MpscTransport { rx: b_rx, tx: b_tx },
    )
}

/// One end of a transport whose **sink never accepts a frame** while its
/// stream stays open — the "peer accepted the connection and then stopped
/// reading its socket, without closing it" shape (TCP zero-window).
///
/// [`MpscTransport`] cannot express it: its `poll_ready` is unconditionally
/// `Ready` and `start_send` drops on a full channel, so a write there never
/// pends. This one pends forever, which is the only way to exercise a driver
/// *while a write is outstanding* — the window in which both the per-request
/// deadline and the dead-link timer still have to work.
///
/// The inbound half is a live channel, so a test can feed frames to the driver
/// *during* the blocked write and assert that reads still make progress.
pub struct StalledSinkTransport {
    rx: mpsc::Receiver<Bytes>,
}

impl Stream for StalledSinkTransport {
    type Item = Result<Bytes, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

impl Sink<Bytes> for StalledSinkTransport {
    type Error = Infallible;
    /// Never ready — and deliberately never registers a waker, exactly like a
    /// peer whose receive window stays shut: nothing will ever wake this write.
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }
    fn start_send(self: Pin<&mut Self>, _item: Bytes) -> Result<(), Self::Error> {
        unreachable!("poll_ready never resolves, so start_send is never reached")
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Build a [`StalledSinkTransport`] plus the sender that feeds its inbound
/// half, so a test can deliver frames while every write is stalled.
pub fn stalled_sink_transport() -> (StalledSinkTransport, mpsc::Sender<Bytes>) {
    let (tx, rx) = mpsc::channel(64);
    (StalledSinkTransport { rx }, tx)
}

/// One end of a transport pair whose **sink stays not-ready until explicitly
/// released, then behaves like [`MpscTransport`]** — unlike
/// [`StalledSinkTransport`] (never ready) or `MpscTransport` (always ready),
/// this can let a write through *partway into a wait*. It's the "the peer's
/// receive window opens after a while" shape, for a test that needs to spend
/// a bounded, deliberate slice of a caller's shared budget stalled at the
/// enqueue and then let the send complete, so the call's *reply* wait can be
/// observed to race whatever budget is left — not a fresh one.
pub struct DelayedSinkTransport {
    rx: mpsc::Receiver<Bytes>,
    tx: mpsc::Sender<Bytes>,
    release: oneshot::Receiver<()>,
    released: bool,
}

impl Stream for DelayedSinkTransport {
    type Item = Result<Bytes, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

impl Sink<Bytes> for DelayedSinkTransport {
    type Error = Infallible;
    /// Pending until `release` fires (send or drop), then `Ready` forever —
    /// exactly like `MpscTransport` from that point on.
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if !self.released {
            match Pin::new(&mut self.release).poll(cx) {
                Poll::Ready(_) => self.released = true,
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Bytes) -> Result<(), Self::Error> {
        let _ = self.tx.try_send(item);
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

/// Build a [`DelayedSinkTransport`] paired with a normal [`MpscTransport`]
/// peer (mirroring [`make_pair`]'s wiring), plus the sender that releases the
/// stalled side's sink.
pub fn delayed_sink_pair() -> (DelayedSinkTransport, MpscTransport, oneshot::Sender<()>) {
    let (a_tx, b_rx) = mpsc::channel(64);
    let (b_tx, a_rx) = mpsc::channel(64);
    let (release_tx, release_rx) = oneshot::channel();
    (
        DelayedSinkTransport {
            rx: a_rx,
            tx: a_tx,
            release: release_rx,
            released: false,
        },
        MpscTransport { rx: b_rx, tx: b_tx },
        release_tx,
    )
}
