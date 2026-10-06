//! The in-memory seam double — [`MemTransport`]: the [`PeerTransport`]
//! surface with no substrate, for tests that prove engine behavior over the
//! seam without paying for a real QUIC stack. Lifted from
//! `fauna-sync-engine/tests/peer_leg_convergence.rs` (its W2.6 (account-data-plane.md § Workstreams) original) when
//! the W5.7 runtime conformance became the second consumer.
//!
//! One process-wide [`Listeners`] map plays the network: `listen()` registers
//! the local key, `dial()` hands the listener one accepted [`PeerConn`] half
//! of a fresh in-memory duplex and keeps the other. Identity is asserted, not
//! proven — exactly what a seam double should do (identity *proof* is the
//! real transports' handshake job; callers bind auth witnesses to
//! `peer_identity` one layer up either way).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{
    ByteStream, EndpointKey, IncomingConns, PathCandidates, PathKind, PeerConn, PeerTransport,
    TransportError,
};

/// The shared "network": node key → the sender feeding that node's accept
/// stream. Clone freely; every [`MemTransport`] on one map can reach every
/// listener on it.
pub type Listeners =
    Arc<Mutex<HashMap<[u8; 32], mpsc::UnboundedSender<Result<Box<dyn PeerConn>, TransportError>>>>>;

/// A fresh, empty network.
pub fn listeners() -> Listeners {
    Arc::default()
}

struct MemConn {
    peer: EndpointKey,
    stream: Mutex<Option<ByteStream>>,
    /// Dial side opens the (single) stream; accept side accepts it.
    opens: bool,
}

#[async_trait]
impl PeerConn for MemConn {
    async fn open_stream(&self) -> Result<ByteStream, TransportError> {
        if !self.opens {
            return Err(TransportError::Unsupported);
        }
        self.stream
            .lock()
            .unwrap()
            .take()
            .ok_or(TransportError::Unsupported)
    }

    async fn accept_stream(&self) -> Result<ByteStream, TransportError> {
        if self.opens {
            return Err(TransportError::Unsupported);
        }
        self.stream
            .lock()
            .unwrap()
            .take()
            .ok_or(TransportError::Unsupported)
    }

    fn peer_identity(&self) -> EndpointKey {
        self.peer
    }

    fn path(&self) -> PathKind {
        PathKind::Lan
    }
}

/// The seam double itself: one node (`me`) on a shared [`Listeners`] network.
pub struct MemTransport {
    pub me: EndpointKey,
    pub listeners: Listeners,
}

#[async_trait]
impl PeerTransport for MemTransport {
    async fn dial(
        &self,
        peer: EndpointKey,
        _candidates: PathCandidates,
    ) -> Result<Box<dyn PeerConn>, TransportError> {
        let (dial_half, accept_half) = tokio::io::duplex(1 << 20);
        let tx = self
            .listeners
            .lock()
            .unwrap()
            .get(peer.as_bytes())
            .cloned()
            .ok_or(TransportError::NoPath)?;
        tx.send(Ok(Box::new(MemConn {
            peer: self.me, // the listener's view of the remote is US
            stream: Mutex::new(Some(Box::pin(accept_half))),
            opens: false,
        })))
        .map_err(|_| TransportError::NoPath)?;
        Ok(Box::new(MemConn {
            peer,
            stream: Mutex::new(Some(Box::pin(dial_half))),
            opens: true,
        }))
    }

    async fn listen(&self) -> Result<IncomingConns, TransportError> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.listeners
            .lock()
            .unwrap()
            .insert(*self.me.as_bytes(), tx);
        Ok(Box::pin(futures_util::stream::unfold(rx, |mut rx| async {
            rx.recv().await.map(|item| (item, rx))
        })))
    }

    fn local_identity(&self) -> EndpointKey {
        self.me
    }
}

/// A settable [`PathKind`], shared by every connection an [`OnPath`]
/// transport hands out — flip it mid-test to model a relayed connection the
/// substrate turns direct (or the reverse).
#[derive(Clone)]
pub struct PathCell(Arc<Mutex<PathKind>>);

impl PathCell {
    pub fn new(path: PathKind) -> Self {
        Self(Arc::new(Mutex::new(path)))
    }

    pub fn set(&self, path: PathKind) {
        *self.0.lock().unwrap() = path;
    }

    pub fn get(&self) -> PathKind {
        *self.0.lock().unwrap()
    }
}

/// Any transport whose connections — dialed and accepted alike — answer
/// [`PeerConn::path`] from a shared [`PathCell`], read live on every call.
/// [`MemTransport`] answers `Lan` unconditionally; wrap it to put a test on
/// a relayed path.
pub struct OnPath {
    pub inner: Arc<dyn PeerTransport>,
    pub path: PathCell,
}

struct OnPathConn {
    inner: Box<dyn PeerConn>,
    path: PathCell,
}

#[async_trait]
impl PeerConn for OnPathConn {
    async fn open_stream(&self) -> Result<ByteStream, TransportError> {
        self.inner.open_stream().await
    }

    async fn accept_stream(&self) -> Result<ByteStream, TransportError> {
        self.inner.accept_stream().await
    }

    fn peer_identity(&self) -> EndpointKey {
        self.inner.peer_identity()
    }

    fn path(&self) -> PathKind {
        self.path.get()
    }
}

#[async_trait]
impl PeerTransport for OnPath {
    async fn dial(
        &self,
        peer: EndpointKey,
        candidates: PathCandidates,
    ) -> Result<Box<dyn PeerConn>, TransportError> {
        let inner = self.inner.dial(peer, candidates).await?;
        Ok(Box::new(OnPathConn {
            inner,
            path: self.path.clone(),
        }))
    }

    async fn listen(&self) -> Result<IncomingConns, TransportError> {
        use futures_util::StreamExt;
        let path = self.path.clone();
        let incoming = self.inner.listen().await?;
        Ok(Box::pin(incoming.map(move |conn| {
            conn.map(|inner| {
                Box::new(OnPathConn {
                    inner,
                    path: path.clone(),
                }) as Box<dyn PeerConn>
            })
        })))
    }

    fn local_identity(&self) -> EndpointKey {
        self.inner.local_identity()
    }
}

/// A generous ceiling for "the node's accept task got as far as registering
/// its listener" — convention 14: a budget a green run never pays, not an
/// expected latency.
const LISTENING_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Block until `node`'s listener is actually in the shared map.
///
/// **This closes a real race in the double, not a load artifact.**
/// `PeerNode::start_with` calls `transport.listen()` *inside* the accept task
/// it spawns, so a bind API can return before the listener has necessarily
/// registered — and the very next `dial` then answers `NoPath`. Before this
/// barrier the W2.6 convergence file's whole-run failed **5 out of 5** times,
/// with a different victim test each run, which is exactly why it read as
/// "flaky under load" rather than as the startup race it is.
///
/// A causal barrier, never a settle-sleep: the registration is the fact being
/// waited on, the poll is bounded by a budget a green run pays one tick of,
/// and a blown budget fails loudly instead of degrading into a confusing
/// `NoPath` somewhere downstream.
///
/// **"In the map" means a LIVE sender, not a key.** The double never removes
/// an entry: a dropped node's accept task drops its receiver and leaves a
/// closed sender behind, which `dial` answers `NoPath` on. A node that stops
/// listening and binds again (the p2p participation switch going off then on)
/// still has that stale key in the map until its new accept task reaches
/// `listen()`, so a bare `contains_key` would pass before the rebind.
pub async fn await_listening(listeners: &Listeners, node: &[u8; 32]) {
    let deadline = std::time::Instant::now() + LISTENING_BUDGET;
    loop {
        if listeners
            .lock()
            .unwrap()
            .get(node)
            .is_some_and(|tx| !tx.is_closed())
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the listener for {:?} never registered — the accept task did not reach \
             transport.listen()",
            EndpointKey::from_bytes(*node)
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

/// A [`PeerTransport`] decorator that **refuses to dial without addressing** —
/// the property a co-present ceremony depends on, made testable.
///
/// [`MemTransport`] is key-addressed: it finds its peer in a process-wide map,
/// so a dial with empty [`PathCandidates`] succeeds and no test can tell
/// whether addressing was carried. A real substrate is the opposite — iroh
/// answers `No addressing information available` when it has an `EndpointId`
/// and no path, which is exactly the RED that
/// `p2p.md` § Offline share initiation measured.
///
/// So this wraps any transport and requires that the dial's candidates name
/// the address it was constructed with — the same one the seat records
/// through `CeremonyNode::with_bound_addrs`. A test that forgets to carry the
/// addressing gets `NoPath`, which is the failure it would get in production
/// rather than a false green.
pub struct RequiresAddressing<T> {
    inner: T,
    endpoint: std::net::SocketAddr,
}

impl<T> RequiresAddressing<T> {
    /// Wrap `inner`, reachable only at `endpoint`.
    pub fn new(inner: T, endpoint: std::net::SocketAddr) -> Self {
        Self { inner, endpoint }
    }
}

#[async_trait]
impl<T: PeerTransport> PeerTransport for RequiresAddressing<T> {
    async fn dial(
        &self,
        peer: EndpointKey,
        candidates: PathCandidates,
    ) -> Result<Box<dyn PeerConn>, TransportError> {
        if !candidates.lan_endpoints.contains(&self.endpoint) {
            return Err(TransportError::NoPath);
        }
        self.inner.dial(peer, candidates).await
    }

    async fn listen(&self) -> Result<IncomingConns, TransportError> {
        self.inner.listen().await
    }

    fn local_identity(&self) -> EndpointKey {
        self.inner.local_identity()
    }
}
