//! [`PeerNode`] — a running P2P node lifecycle over the substrate-agnostic
//! [`fauna_transport::PeerTransport`] seam.
//!
//! A node owns one transport (WireGuard *or* iroh — the seam makes the choice
//! invisible above L1), runs a `listen()` accept loop that stands ready to serve
//! the base `fauna.peer.*` kinds on each inbound connection, and can [`dial`] an
//! outbound [`PeerChannel`]. Dropping the node aborts the accept loop and drops
//! every inbound channel. It is the shared lifecycle **both** native P2P
//! consumers hold (`apps/fauna-linux`'s `P2pService` and `fauna-ffi`'s
//! `peer_tunnel_*`), so the "bring up an endpoint + listen + dial" logic lives
//! once here (priority #2) instead of being hand-rolled per client (priority #1).
//!
//! **Substrate-agnostic by construction.** [`start`](PeerNode::start) takes an
//! `Arc<dyn PeerTransport>`; the consumer constructs the concrete transport
//! (today `fauna_iroh::IrohTransport` — the adopted production substrate, iroh
//! focus 2026-07-01) and hands it in, so this crate — and every consumer's inner
//! loop that only touches `PeerNode` — stays iroh-free (the `fauna-iroh` dep
//! enters only where the transport is *built*).
//!
//! **Dormant foundation (`docs/goal/behavior/p2p.md` § Transport seam).** The
//! P2P data plane carries no live traffic yet — file-sync is 100% nest-mediated.
//! This is the clean Y.1 lifecycle future P2P features ride, *not* a live
//! cutover. Two consequences shape the scope, both honestly bounded rather than
//! blindly built out (the "capture, don't build blind" discipline):
//!
//! - **The base serve set is just `fauna.peer.node_info`** (the connectivity
//!   check). Richer kinds (`fauna.peer.exchange`, …) are served when a feature
//!   actually rides P2P.
//! - **The accept side *accepts* the requester-opened stream.** A request/reply's
//!   requester opens the stream + writes first, so the server must
//!   [`accept_stream`](fauna_transport::PeerConn::accept_stream), not open its
//!   own (opening would deadlock — nothing writes first over QUIC's `accept_bi`).
//!   Slice 7 added `accept_stream()` to the seam for exactly this consumer (the
//!   deferral's trigger); the iroh impl provides it, the parked
//!   WireGuard impl inherits the `Unsupported` default, so a WG-inbound conn is
//!   simply not served (its dormant fallback stays untouched).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_protocol::Value;
use fauna_protocol::peer::{KIND_PEER_NODE_INFO, PEER_PROTOCOL_VERSION, PeerNodeInfoReply};
use fauna_transport::{EndpointKey, PathCandidates, PeerConn, PeerTransport, TransportError};
use futures_util::StreamExt;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{AbortOnDrop, PeerChannel, PeerHandlers, ServeGuard};

/// One accepted inbound peer, held for its lifetime: the [`PeerConn`] (dropping
/// it would close the connection + kill the accepted stream), the serving
/// [`PeerChannel`] (its dispatcher driver), and the [`ServeGuard`] (its serve
/// loop). All three are kept alive together for the node's lifetime.
type InboundPeer = (Box<dyn PeerConn>, PeerChannel, ServeGuard);

/// The per-connection handler factory [`PeerNode::start_with`] consumes:
/// `(proven peer identity, path) → Some(that connection's serve set)`, or
/// `None` to refuse the connection outright (the per-peer connection-quota
/// hook).
pub type HandlerFactory =
    Arc<dyn Fn(EndpointKey, fauna_transport::PathKind) -> Option<PeerHandlers> + Send + Sync>;

/// How long an admitted inbound connection may take to open its first stream
/// before the node drops it. A real requester opens its stream and writes its
/// first request (the admission exchange) the moment its dial lands, so this
/// only ever fires on a peer that is holding the connection open without
/// speaking (`p2p.md` § Wormability posture rule 8).
pub const INBOUND_STREAM_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many admitted inbound connections may wait for their first stream at
/// once. A connection arriving while this many are waiting is dropped on the
/// spot — its dialer redials — so silent dialers cannot grow the node's task
/// set without bound. Minting a fresh NodeId is free, so a per-peer count
/// would not bound this; the cap is node-wide.
pub const MAX_PENDING_INBOUND_ACCEPTS: usize = 64;

/// A running P2P node over a [`PeerTransport`] substrate.
///
/// Create with [`start`](Self::start); the node runs until it is dropped. It is
/// `Send + Sync`, so a consumer can hold it in an `Arc` (linux) or a registry
/// behind a `Mutex` (the FFI). See the module docs for the dormant-foundation
/// scope + the accept-side-accepts-the-requester's-stream note.
pub struct PeerNode {
    transport: Arc<dyn PeerTransport>,
    /// Accepted inbound peers ([`InboundPeer`] each), keyed by accept order and
    /// kept alive until their connection ends (or the node drops → stop
    /// serving). Each connection's task inserts its entry and removes it again
    /// once the channel closes, hence the `Mutex`.
    inbound: Arc<Mutex<HashMap<u64, InboundPeer>>>,
    /// The `listen()` accept loop; aborted on drop (which also drops `inbound`).
    accept: AbortOnDrop,
}

impl PeerNode {
    /// Bring the node up: start accepting inbound peer connections and serving
    /// the base `fauna.peer.*` kinds on each. `display_name` is the human label
    /// this node reports in its `fauna.peer.node_info` reply.
    ///
    /// Async only so `transport.listen()` can be awaited inside the ambient tokio
    /// runtime the accept loop is spawned on; it returns as soon as the loop is
    /// spawned (it does not block on an inbound connection). A dial-only substrate
    /// whose `listen()` is [`Unsupported`](TransportError::Unsupported) yields a
    /// node that still [`dial`](Self::dial)s but serves nothing (and reports
    /// [`is_active`](Self::is_active) `false` once the loop has exited).
    pub async fn start(transport: Arc<dyn PeerTransport>, display_name: String) -> Self {
        Self::start_with(
            transport,
            Arc::new(move |_peer, _path| Some(base_peer_handlers(display_name.clone()))),
        )
        .await
    }

    /// [`start`](Self::start) with a **per-connection handler factory**: for
    /// each inbound connection the factory receives the transport-proven peer
    /// identity + path and returns that connection's serve set — which is what
    /// lets a consumer hold per-connection state (an admission verdict slot,
    /// per-peer quota counters) inside its handlers. Returning `None` refuses
    /// the connection before any stream is accepted — the cheap-refusal hook a
    /// per-peer connection quota needs (`p2p.md` § Wormability posture
    /// rule 8).
    ///
    /// The factory runs on the accept loop, so it must be quick and
    /// non-blocking; witness verification belongs in a handler (the admission
    /// exchange), never here.
    pub async fn start_with(transport: Arc<dyn PeerTransport>, factory: HandlerFactory) -> Self {
        let inbound: Arc<Mutex<HashMap<u64, InboundPeer>>> = Arc::new(Mutex::new(HashMap::new()));
        let accept = {
            let transport = Arc::clone(&transport);
            let inbound = Arc::clone(&inbound);
            AbortOnDrop(tokio::spawn(async move {
                // A dial-only substrate (`listen()` unsupported) → no accept loop;
                // the node can still `dial`. Any other listen error also ends the
                // loop (the transport surfaces it; nothing to serve).
                let Ok(mut incoming) = transport.listen().await else {
                    return;
                };
                // Every connection's own task. Owned by this loop, so dropping
                // the node (which aborts the loop) aborts them all too.
                let mut per_conn = JoinSet::new();
                let pending_accepts = Arc::new(Semaphore::new(MAX_PENDING_INBOUND_ACCEPTS));
                let mut next_id: u64 = 0;
                while let Some(item) = incoming.next().await {
                    while per_conn.try_join_next().is_some() {}
                    // A failed inbound handshake is not fatal to the listener.
                    let Ok(conn) = item else { continue };
                    let peer = conn.peer_identity();
                    let path = conn.path();
                    // The connection gate — quota-refused peers are dropped
                    // before any stream work.
                    let Some(handlers) = factory(peer, path) else {
                        continue;
                    };
                    let Ok(pending) = Arc::clone(&pending_accepts).try_acquire_owned() else {
                        continue;
                    };
                    let id = next_id;
                    next_id += 1;
                    let inbound = Arc::clone(&inbound);
                    // Everything past the gate waits on the peer, so it runs on
                    // the connection's own task: awaited here, one peer that
                    // never opens a stream would stall every later one.
                    per_conn.spawn(async move {
                        // The requester opens the stream + writes first, so the
                        // server ACCEPTS it (see the module docs — opening our own
                        // would deadlock). A substrate that can't accept (the
                        // parked WG inbound conn → `Unsupported`) has nothing to
                        // serve, and a peer that stays silent past the timeout is
                        // not a requester: drop either.
                        let accepted = tokio::time::timeout(
                            INBOUND_STREAM_ACCEPT_TIMEOUT,
                            conn.accept_stream(),
                        )
                        .await;
                        drop(pending);
                        let Ok(Ok(stream)) = accepted else { return };
                        let channel = PeerChannel::over_stream(stream, peer, path);
                        let guard = channel.serve(handlers);
                        let closed = channel.closed();
                        // Keep the conn alive alongside the channel — dropping it
                        // would close the connection + kill the accepted stream —
                        // until the connection ends, then let it go.
                        inbound.lock().unwrap().insert(id, (conn, channel, guard));
                        closed.await;
                        inbound.lock().unwrap().remove(&id);
                    });
                }
            }))
        };
        Self {
            transport,
            inbound,
            accept,
        }
    }

    /// Dial `peer` (traversing NAT via the transport's three-path cascade using
    /// `candidates`) and originate a [`PeerChannel`] over the connection — the
    /// outbound direction. The caller applies the per-pair auth witness to the
    /// channel's [`peer_identity`](PeerChannel::peer_identity) (PT-2/PT-3) before
    /// trusting it.
    pub async fn dial(
        &self,
        peer: EndpointKey,
        candidates: PathCandidates,
    ) -> Result<PeerChannel, TransportError> {
        let conn = self.transport.dial(peer, candidates).await?;
        PeerChannel::open(conn).await
    }

    /// This node's own transport identity (its Ed25519 [`EndpointKey`] — for the
    /// iroh substrate, its `NodeId`, which *is* the actor key, PT-1b).
    pub fn local_identity(&self) -> EndpointKey {
        self.transport.local_identity()
    }

    /// `true` while the `listen()` accept loop is running (the node is up and
    /// accepting inbound peers). `false` once it has exited — e.g. a dial-only
    /// substrate, a closed endpoint, or after the node is dropped.
    pub fn is_active(&self) -> bool {
        self.accept.is_running()
    }

    /// Drop every inbound connection this node is holding, and say how many
    /// there were. The listener stays up, so a peer that dials again is
    /// accepted afresh, through the same per-connection gate as the first time.
    ///
    /// This is what a dropped link looks like from the serving side: every
    /// request in flight on those connections fails as disconnected, and the
    /// peer has to dial again.
    pub fn close_inbound(&self) -> usize {
        let dropped: HashMap<u64, InboundPeer> = std::mem::take(&mut *self.inbound.lock().unwrap());
        dropped.len()
    }

    /// Stop the node and **wait until its listener is gone**: the accept loop
    /// is aborted and joined — so the transport's accept stream it held is
    /// dropped by the time this returns — and every inbound connection is
    /// dropped with it. Dropping the node instead only requests the abort, and
    /// the loop ends a scheduling beat later.
    ///
    /// For a caller handing the machine's one NodeId to another process: the
    /// account runtime's yield to the sync agent releases the engine role only
    /// after this returns, so no instant has two listeners on one identity.
    pub async fn shutdown(mut self) {
        self.accept.abort_and_wait().await;
        self.close_inbound();
    }
}

/// The base `fauna.peer.*` serve set — just `fauna.peer.node_info` (the
/// connectivity check), answered with this node's protocol version + display
/// name. The peer's *cryptographic* identity is the transport-proven
/// `peer_identity()`, never re-sent in the reply.
///
/// Public so a [`PeerNode::start_with`] consumer building a richer serve set
/// (the peer-sync leg's admission + transfer kinds) starts from the same base
/// instead of re-deriving the node-info reply shape.
pub fn base_peer_handlers(display_name: String) -> PeerHandlers {
    PeerHandlers::new().on(KIND_PEER_NODE_INFO, move |_req| {
        let value = node_info_reply(display_name.clone());
        async move { Ok(value) }
    })
}

/// A `fauna.peer.node_info` reply as an L3 `Value` (canonical-CBOR round-trip,
/// the federation channel's `to_value` shape). `Value::Null` on the (unreachable
/// for a plain struct) encode failure.
fn node_info_reply(display_name: String) -> Value {
    let reply = PeerNodeInfoReply {
        protocol_version: PEER_PROTOCOL_VERSION,
        display_name,
        ..Default::default()
    };
    crate::to_value(&reply).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Production code round-trips through `crate::to_value`; these fixtures build
    // raw wire values by hand, so they still reach for the codec directly.
    use async_trait::async_trait;
    use fauna_protocol::{decode_strict, encode_canonical};
    use fauna_transport::{ByteStream, PathKind, PeerConn};
    use tokio::sync::mpsc;

    fn key(seed: u8) -> EndpointKey {
        EndpointKey::from_bytes([seed; 32])
    }

    /// An in-memory [`PeerConn`] holding one pre-made byte stream, handed out
    /// once by `open_stream` (the dialer side of the mem transport below).
    struct MemConn {
        peer: EndpointKey,
        stream: Mutex<Option<ByteStream>>,
    }
    #[async_trait]
    impl PeerConn for MemConn {
        async fn open_stream(&self) -> Result<ByteStream, TransportError> {
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

    /// A dial-only in-memory transport: each `dial` mints a duplex pipe, returns
    /// the near end as a [`MemConn`], and hands the far end to the test over
    /// `server_tx` so the test can host the serving side. `listen()` is
    /// unsupported — this exercises the node's dial path (the serve/accept path
    /// needs a substrate whose peer can *accept* a peer-opened stream, proven over
    /// real iroh in `fauna-iroh`).
    struct MemTransport {
        me: EndpointKey,
        server_tx: mpsc::UnboundedSender<ByteStream>,
    }
    #[async_trait]
    impl PeerTransport for MemTransport {
        async fn dial(
            &self,
            peer: EndpointKey,
            _candidates: PathCandidates,
        ) -> Result<Box<dyn PeerConn>, TransportError> {
            let (near, far) = tokio::io::duplex(64 * 1024);
            self.server_tx
                .send(Box::pin(far))
                .map_err(|_| TransportError::Other("test server gone".into()))?;
            Ok(Box::new(MemConn {
                peer,
                stream: Mutex::new(Some(Box::pin(near))),
            }))
        }
        async fn listen(&self) -> Result<fauna_transport::IncomingConns, TransportError> {
            Err(TransportError::Unsupported)
        }
        fn local_identity(&self) -> EndpointKey {
            self.me
        }
    }

    /// `local_identity()` reflects the transport's identity, and a dial-only
    /// transport (`listen()` unsupported) leaves the accept loop exited →
    /// `is_active()` is `false` (the node still dials, proven separately).
    #[tokio::test]
    async fn local_identity_and_dial_only_is_inactive() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let transport = Arc::new(MemTransport {
            me: key(1),
            server_tx: tx,
        });
        let node = PeerNode::start(transport, "me".into()).await;
        assert_eq!(node.local_identity(), key(1));
        // The accept loop returns immediately on the unsupported `listen()`; give
        // the spawned task a scheduler tick to run, then it must read inactive.
        tokio::task::yield_now().await;
        assert!(
            !node.is_active(),
            "dial-only substrate ⇒ accept loop exits ⇒ inactive"
        );
    }

    /// `PeerNode::dial` originates a real `PeerChannel`: the node dials, a test
    /// server picks up the far end of the duplex and serves `fauna.peer.node_info`,
    /// and the originated request round-trips — proving the outbound path threads
    /// the transport → `PeerChannel::open` → `request` chain.
    #[tokio::test]
    async fn dial_originates_a_working_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ByteStream>();
        let transport = Arc::new(MemTransport {
            me: key(1),
            server_tx: tx,
        });
        let node = PeerNode::start(transport, "me".into()).await;

        // Dial: mints the duplex, sends the far end to `rx`, opens the near end +
        // starts the channel driver (no server needed yet — the duplex buffers).
        let channel = node
            .dial(key(2), PathCandidates::default())
            .await
            .expect("dial");
        assert_eq!(channel.peer_identity(), key(2));

        // Stand up the server on the far end and serve node_info, THEN request.
        let far = rx.recv().await.expect("server end");
        let server = PeerChannel::over_stream(far, key(1), PathKind::Lan);
        let _serve = server.serve(
            PeerHandlers::new().on(KIND_PEER_NODE_INFO, |_req| async move {
                Ok(Value::String("peer-served".into()))
            }),
        );

        let reply = channel
            .request(KIND_PEER_NODE_INFO, Value::Null)
            .await
            .expect("node_info reply");
        assert_eq!(reply, Value::String("peer-served".into()));
    }

    /// The base serve set answers `fauna.peer.node_info` with this node's protocol
    /// version + display name — checked by encoding the reply the node would send
    /// and decoding it back (the wire shape a peer receives).
    #[test]
    fn base_handler_reply_carries_version_and_name() {
        let value = node_info_reply("Alice's box".into());
        let bytes = encode_canonical(&value).expect("encode");
        let reply: PeerNodeInfoReply = decode_strict(&bytes).expect("decode");
        assert_eq!(reply.protocol_version, PEER_PROTOCOL_VERSION);
        assert_eq!(reply.display_name, "Alice's box");
    }

    /// An inbound conn whose `accept_stream()` succeeds once, handing out one
    /// pre-made duplex half — simulating a peer that already opened + wrote to its
    /// stream before the accept loop got to it (the accept side never opens its
    /// own stream, see the module docs).
    struct AcceptableConn {
        peer: EndpointKey,
        stream: Mutex<Option<ByteStream>>,
    }
    #[async_trait]
    impl PeerConn for AcceptableConn {
        async fn open_stream(&self) -> Result<ByteStream, TransportError> {
            Err(TransportError::Unsupported) // accepted conns don't open their own
        }
        async fn accept_stream(&self) -> Result<ByteStream, TransportError> {
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

    /// An inbound conn that can't accept a stream — the parked-WireGuard "nothing
    /// to accept" case (module docs): relies on `PeerConn::accept_stream`'s
    /// default `Unsupported` impl, never overridden.
    struct UnacceptableConn {
        peer: EndpointKey,
    }
    #[async_trait]
    impl PeerConn for UnacceptableConn {
        async fn open_stream(&self) -> Result<ByteStream, TransportError> {
            Err(TransportError::Unsupported)
        }
        fn peer_identity(&self) -> EndpointKey {
            self.peer
        }
        fn path(&self) -> PathKind {
            PathKind::Lan
        }
    }

    /// The canned items a [`ListenTransport`] replays through `listen()`.
    type CannedConns = Vec<Result<Box<dyn PeerConn>, TransportError>>;

    /// A listen-only in-memory transport: `listen()` replays the given items once,
    /// then pends forever — mirroring a real accept loop, which never observes
    /// `None` and so never exits on its own. Proves the accept task (and
    /// `is_active()`) stay alive past the last canned item. `dial` is unused by
    /// these tests.
    struct ListenTransport {
        me: EndpointKey,
        conns: Mutex<Option<CannedConns>>,
    }
    #[async_trait]
    impl PeerTransport for ListenTransport {
        async fn dial(
            &self,
            _peer: EndpointKey,
            _candidates: PathCandidates,
        ) -> Result<Box<dyn PeerConn>, TransportError> {
            Err(TransportError::Unsupported)
        }
        async fn listen(&self) -> Result<fauna_transport::IncomingConns, TransportError> {
            let items = self
                .conns
                .lock()
                .unwrap()
                .take()
                .expect("listen() called once in these tests");
            Ok(Box::pin(
                futures_util::stream::iter(items).chain(futures_util::stream::pending()),
            ))
        }
        fn local_identity(&self) -> EndpointKey {
            self.me
        }
    }

    /// The accept loop's raison d'être: an inbound connection is accepted and
    /// served with `base_handlers` — proving the `KIND_PEER_NODE_INFO` wiring
    /// through a real inbound request (not just the reply-encoding unit test
    /// above) — and the loop, and `is_active()`, stay up afterward (an accept loop
    /// never exits on its own; only `Drop` or a `listen()`-level error ends it).
    #[tokio::test]
    async fn accept_loop_serves_an_inbound_connection_and_reports_active() {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let conn: Box<dyn PeerConn> = Box::new(AcceptableConn {
            peer: key(9),
            stream: Mutex::new(Some(Box::pin(near))),
        });
        let transport = Arc::new(ListenTransport {
            me: key(1),
            conns: Mutex::new(Some(vec![Ok(conn)])),
        });
        let node = PeerNode::start(transport, "server".into()).await;

        let client = PeerChannel::over_stream(Box::pin(far), key(1), PathKind::Lan);
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.request(KIND_PEER_NODE_INFO, Value::Null),
        )
        .await
        .expect("accept loop never served the connection")
        .expect("node_info reply");
        let bytes = encode_canonical(&value).expect("encode reply value");
        let reply: PeerNodeInfoReply = decode_strict(&bytes).expect("decode reply");
        assert_eq!(reply.protocol_version, PEER_PROTOCOL_VERSION);
        assert_eq!(reply.display_name, "server");

        assert!(
            node.is_active(),
            "the accept loop never returns on its own — it must still be running"
        );
    }

    /// A failed inbound handshake (`item` is `Err`, node.rs's `let Ok(conn) = item
    /// else { continue }`) is not fatal to the listener — the loop `continue`s and
    /// still serves a later, good connection.
    #[tokio::test]
    async fn accept_loop_skips_a_failed_handshake_item() {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let good: Box<dyn PeerConn> = Box::new(AcceptableConn {
            peer: key(9),
            stream: Mutex::new(Some(Box::pin(near))),
        });
        let transport = Arc::new(ListenTransport {
            me: key(1),
            conns: Mutex::new(Some(vec![
                Err(TransportError::Io("simulated handshake failure".into())),
                Ok(good),
            ])),
        });
        let _node = PeerNode::start(transport, "server".into()).await;

        let client = PeerChannel::over_stream(Box::pin(far), key(1), PathKind::Lan);
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.request(KIND_PEER_NODE_INFO, Value::Null),
        )
        .await
        .expect("a failed handshake item must not stop the loop from serving the next conn")
        .expect("node_info reply");
        let bytes = encode_canonical(&value).expect("encode reply value");
        let reply: PeerNodeInfoReply = decode_strict(&bytes).expect("decode reply");
        assert_eq!(reply.display_name, "server");
    }

    /// A conn whose `accept_stream()` fails (the parked-WireGuard "nothing to
    /// accept" case, module docs) is dropped, not fatal to the listener — the loop
    /// still serves a later, good connection.
    #[tokio::test]
    async fn accept_loop_skips_a_conn_whose_accept_stream_fails() {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let good: Box<dyn PeerConn> = Box::new(AcceptableConn {
            peer: key(9),
            stream: Mutex::new(Some(Box::pin(near))),
        });
        let unacceptable: Box<dyn PeerConn> = Box::new(UnacceptableConn { peer: key(8) });
        let transport = Arc::new(ListenTransport {
            me: key(1),
            conns: Mutex::new(Some(vec![Ok(unacceptable), Ok(good)])),
        });
        let _node = PeerNode::start(transport, "server".into()).await;

        let client = PeerChannel::over_stream(Box::pin(far), key(1), PathKind::Lan);
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.request(KIND_PEER_NODE_INFO, Value::Null),
        )
        .await
        .expect("an unacceptable conn must not stop the loop from serving the next conn")
        .expect("node_info reply");
        let bytes = encode_canonical(&value).expect("encode reply value");
        let reply: PeerNodeInfoReply = decode_strict(&bytes).expect("decode reply");
        assert_eq!(reply.display_name, "server");
    }

    /// An inbound conn that completed its handshake but never opens a stream —
    /// the silent dialer: `accept_stream()` pends forever.
    struct SilentConn {
        peer: EndpointKey,
    }
    #[async_trait]
    impl PeerConn for SilentConn {
        async fn open_stream(&self) -> Result<ByteStream, TransportError> {
            Err(TransportError::Unsupported)
        }
        async fn accept_stream(&self) -> Result<ByteStream, TransportError> {
            std::future::pending().await
        }
        fn peer_identity(&self) -> EndpointKey {
            self.peer
        }
        fn path(&self) -> PathKind {
            PathKind::Lan
        }
    }

    /// An inbound conn serving one duplex half, plus the client channel over the
    /// other half — the requester a test drives.
    fn served_pair(peer: u8) -> (Box<dyn PeerConn>, PeerChannel) {
        let (near, far) = tokio::io::duplex(64 * 1024);
        let conn: Box<dyn PeerConn> = Box::new(AcceptableConn {
            peer: key(peer),
            stream: Mutex::new(Some(Box::pin(near))),
        });
        let client = PeerChannel::over_stream(Box::pin(far), key(1), PathKind::Lan);
        (conn, client)
    }

    /// A listen-only transport whose inbound connections arrive whenever the
    /// test sends them, so a test can let time pass between two arrivals.
    struct FeedTransport {
        me: EndpointKey,
        feed: Mutex<Option<mpsc::UnboundedReceiver<Box<dyn PeerConn>>>>,
    }
    #[async_trait]
    impl PeerTransport for FeedTransport {
        async fn dial(
            &self,
            _peer: EndpointKey,
            _candidates: PathCandidates,
        ) -> Result<Box<dyn PeerConn>, TransportError> {
            Err(TransportError::Unsupported)
        }
        async fn listen(&self) -> Result<fauna_transport::IncomingConns, TransportError> {
            let rx = self.feed.lock().unwrap().take().expect("listen() once");
            Ok(Box::pin(tokio_stream_from(rx).map(Ok::<_, TransportError>)))
        }
        fn local_identity(&self) -> EndpointKey {
            self.me
        }
    }

    fn tokio_stream_from<T: Send + 'static>(
        rx: mpsc::UnboundedReceiver<T>,
    ) -> impl futures_util::Stream<Item = T> + Send {
        futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|t| (t, rx)) })
    }

    async fn node_info_name(client: &PeerChannel) -> Result<String, ChannelErrorOrTimeout> {
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.request(KIND_PEER_NODE_INFO, Value::Null),
        )
        .await
        .map_err(|_| ChannelErrorOrTimeout::Timeout)?
        .map_err(|_| ChannelErrorOrTimeout::Channel)?;
        let bytes = encode_canonical(&value).expect("encode reply value");
        let reply: PeerNodeInfoReply = decode_strict(&bytes).expect("decode reply");
        Ok(reply.display_name)
    }

    #[derive(Debug, PartialEq)]
    enum ChannelErrorOrTimeout {
        Channel,
        Timeout,
    }

    /// A peer that completes the handshake and never opens a stream
    /// must not hold the node's only accept loop: a second peer arriving right
    /// behind it is served well inside [`INBOUND_STREAM_ACCEPT_TIMEOUT`].
    #[tokio::test]
    async fn a_silent_connection_does_not_stall_the_next_peer() {
        let silent: Box<dyn PeerConn> = Box::new(SilentConn { peer: key(7) });
        let (good, client) = served_pair(9);
        let transport = Arc::new(ListenTransport {
            me: key(1),
            conns: Mutex::new(Some(vec![Ok(silent), Ok(good)])),
        });
        let _node = PeerNode::start(transport, "server".into()).await;

        assert_eq!(
            node_info_name(&client).await,
            Ok("server".into()),
            "a silent connection stalled the accept loop"
        );
    }

    /// An inbound entry goes away once its connection has ended — the
    /// node does not hold every peer that ever connected until `close_inbound`.
    #[tokio::test]
    async fn an_inbound_entry_is_pruned_when_its_peer_disconnects() {
        let (good, client) = served_pair(9);
        let transport = Arc::new(ListenTransport {
            me: key(1),
            conns: Mutex::new(Some(vec![Ok(good)])),
        });
        let node = PeerNode::start(transport, "server".into()).await;
        assert_eq!(node_info_name(&client).await, Ok("server".into()));
        assert_eq!(node.inbound.lock().unwrap().len(), 1, "served peer is held");

        drop(client); // the peer hangs up
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !node.inbound.lock().unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the disconnected peer's inbound entry was never pruned");
    }

    /// Silent connections are bounded two ways: at most
    /// [`MAX_PENDING_INBOUND_ACCEPTS`] wait for a stream at once (a peer
    /// arriving past the cap is dropped, and redials), and each waits at most
    /// [`INBOUND_STREAM_ACCEPT_TIMEOUT`] — after which its slot is free again.
    #[tokio::test(start_paused = true)]
    async fn pending_accepts_are_capped_and_time_out() {
        let (tx, rx) = mpsc::unbounded_channel::<Box<dyn PeerConn>>();
        let transport = Arc::new(FeedTransport {
            me: key(1),
            feed: Mutex::new(Some(rx)),
        });
        let _node = PeerNode::start(transport, "server".into()).await;

        for i in 0..MAX_PENDING_INBOUND_ACCEPTS {
            tx.send(Box::new(SilentConn { peer: key(i as u8) }))
                .unwrap();
        }
        let (refused, refused_client) = served_pair(200);
        tx.send(refused).unwrap();
        assert_eq!(
            node_info_name(&refused_client).await,
            Err(ChannelErrorOrTimeout::Channel),
            "a peer past the pending cap must be dropped, not queued"
        );

        tokio::time::sleep(INBOUND_STREAM_ACCEPT_TIMEOUT * 2).await;
        let (admitted, admitted_client) = served_pair(201);
        tx.send(admitted).unwrap();
        assert_eq!(
            node_info_name(&admitted_client).await,
            Ok("server".into()),
            "timed-out silent connections must give their slots back"
        );
    }
}
