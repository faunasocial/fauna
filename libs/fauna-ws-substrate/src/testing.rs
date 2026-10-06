//! In-memory test harness for driving [`run_supervisor`](crate::run_supervisor)
//! without a real WebSocket. Gated behind the `test-util` feature so it never
//! ships in a production build.
//!
//! - [`mpsc_pair`] → (client-side [`MpscAdapter`], server-side [`ServerSide`]).
//! - [`QueueChannel`] → a [`SupervisedChannel`] that yields the next queued
//!   adapter on each connect attempt — the supervisor analogue of a mock
//!   connector. Reused by `fauna-client`'s reconnect tests and (Spec Y2 slice
//!   4) the federation channel tests.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use bytes::Bytes;
use fauna_protocol::RpcDispatcher;
use futures_util::{Sink, Stream};
use tokio::sync::mpsc;

use crate::adapter::{AdapterError, ReconnectSignal};
use crate::supervisor::{ConnectedAdapter, SupervisedChannel};

/// Bidirectional in-memory transport — one side wraps as a [`ConnectedAdapter`],
/// the other ([`ServerSide`]) gives the test direct mpsc handles.
pub struct MpscAdapter {
    rx: mpsc::Receiver<Bytes>,
    tx: mpsc::Sender<Bytes>,
    /// The signal the adapter reports once the test ends the stream (by setting
    /// this then dropping the [`ServerSide`]).
    pub closed_with: Arc<Mutex<Option<ReconnectSignal>>>,
}

impl Stream for MpscAdapter {
    type Item = Result<Bytes, AdapterError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|opt| opt.map(Ok))
    }
}

impl Sink<Bytes> for MpscAdapter {
    type Error = AdapterError;
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

impl ConnectedAdapter for MpscAdapter {
    fn last_signal(&self) -> Option<ReconnectSignal> {
        self.closed_with.lock().ok().and_then(|g| g.clone())
    }
}

/// Server-side handle to push frames to the client and receive frames sent by it.
pub struct ServerSide {
    pub tx_to_client: mpsc::Sender<Bytes>,
    pub rx_from_client: mpsc::Receiver<Bytes>,
}

/// In-memory adapter pair with a default 64-frame buffer per direction.
pub fn mpsc_pair() -> (MpscAdapter, ServerSide) {
    mpsc_pair_with_capacity(64)
}

/// In-memory adapter pair with an explicit per-direction buffer (for
/// backpressure tests).
pub fn mpsc_pair_with_capacity(cap: usize) -> (MpscAdapter, ServerSide) {
    let (c2s_tx, c2s_rx) = mpsc::channel::<Bytes>(cap);
    let (s2c_tx, s2c_rx) = mpsc::channel::<Bytes>(cap);
    let closed_with = Arc::new(Mutex::new(None));
    (
        MpscAdapter {
            rx: s2c_rx,
            tx: c2s_tx,
            closed_with: Arc::clone(&closed_with),
        },
        ServerSide {
            tx_to_client: s2c_tx,
            rx_from_client: c2s_rx,
        },
    )
}

/// A simple [`std::error::Error`] for the queue-backed mock channel.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TestChannelError(pub String);

/// A [`SupervisedChannel`] that yields the next [`MpscAdapter`] from a queue on
/// each [`connect`](SupervisedChannel::connect) attempt; an empty queue yields
/// an error (so the supervisor backs off). No per-connection serving and no
/// auth refresh — the supervisor's own machinery is what these tests exercise.
pub struct QueueChannel {
    queue: Arc<Mutex<VecDeque<Result<MpscAdapter, TestChannelError>>>>,
}

impl QueueChannel {
    pub fn new(queue: Arc<Mutex<VecDeque<Result<MpscAdapter, TestChannelError>>>>) -> Self {
        Self { queue }
    }
}

#[async_trait]
impl SupervisedChannel for QueueChannel {
    type Session = ();
    type Error = TestChannelError;

    async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, TestChannelError> {
        let next = {
            let mut q = self.queue.lock().unwrap();
            q.pop_front()
        };
        match next {
            Some(Ok(adapter)) => Ok(Box::new(adapter) as Box<dyn ConnectedAdapter>),
            Some(Err(e)) => Err(e),
            None => Err(TestChannelError("test queue exhausted".into())),
        }
    }

    async fn on_connect(
        &self,
        _dispatcher: &Arc<RpcDispatcher>,
    ) -> Result<Self::Session, TestChannelError> {
        Ok(())
    }
}
