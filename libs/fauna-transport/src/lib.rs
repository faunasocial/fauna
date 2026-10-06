//! The **P2P-transport seam** — a substrate-agnostic trait that abstracts L1/L2
//! (dial / listen / reliable bidirectional byte stream + NAT traversal + path
//! selection), behind which iroh-QUIC (`libs/fauna-iroh`, the sole impl since
//! the bespoke WireGuard stack was deleted 2026-08-23) sits as an
//! interchangeable, optional, self-hosted, capability-negotiated
//! implementation.
//!
//! Design authority tracked internally (§3 the
//! client-side trait; §2 endpoint-pair generality). Goal-doc authorities:
//! `docs/goal/architecture/transport.md` (the L3/L2/L1 layering — L3
//! [`fauna-protocol`/Y.1] rides over *any* byte stream this seam yields),
//! `docs/goal/behavior/p2p.md` (the "no daemon, no privileged helper" +
//! always-available-fallback invariants), `docs/goal/architecture/federation.md`
//! (nest↔nest auth = mutual nest-key).
//!
//! ## What this seam owns, and what it does not
//!
//! This trait abstracts **L1/L2 only**: establishing a NAT-traversed connection
//! to a peer identified by an Ed25519 key, and yielding a reliable ordered byte
//! stream over it. It deliberately owns *neither*:
//!
//! - **L3 framing** — `fauna-protocol` / Y.1 envelopes (and chunk transfer) ride
//!   *over* [`ByteStream`] unchanged. The seam yields bytes; the caller frames
//!   them.
//! - **Authentication** — the seam is **symmetric** (the local endpoint is "an
//!   Ed25519 *actor* key **or** a *nest* key"; see [`PeerTransport::local_identity`]).
//!   It only proves the remote controls the [`EndpointKey`] it dialed/accepted
//!   (during the impl's own handshake) and surfaces that as
//!   [`PeerConn::peer_identity`]. The *per-pair* auth witness — actor-key for
//!   client↔client, **mutual nest-key** for nest↔nest, mixed for client↔nest
//!   (design §2(a)) — is applied by the **caller**, one layer above this seam.
//! - **The always-available fallback** — when no path establishes (or the user's
//!   routing policy denies relay), the caller routes over the existing
//!   nest-mediated sync path (WS-RPC + chunk transfer). This trait *yields a peer
//!   connection or fails*; the fallback lives one layer up.
//!
//! Every impl runs **in-process** in the client (no daemon — the `p2p.md`
//! invariant): iroh is quinn. It is not a separate supervised client
//! process.

use std::fmt;
use std::net::SocketAddr;
use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(feature = "test-helpers")]
pub mod testing;

/// An Ed25519 public key identifying a transport endpoint.
///
/// The seam dials and listens by this identity. It is **symmetric**: the key may
/// be a client device's **actor** key *or* a **nest** key (design §2 — one
/// uniform seam carries client↔client, client↔nest, *and* nest↔nest pairs). The
/// per-pair auth witness is applied by the caller above the seam, never here.
///
/// Mapping the Ed25519 identity onto the substrate's own addressing is the
/// impl's concern, not the seam's. For the iroh impl it *is* the iroh `NodeId`
/// directly.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointKey([u8; 32]);

impl EndpointKey {
    /// Wrap raw Ed25519 public-key bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw 32-byte Ed25519 public key.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for EndpointKey {
    /// Short hex prefix only — never log a full key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "EndpointKey({:02x}{:02x}{:02x}{:02x}…)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

/// Which path an established connection runs over.
///
/// Surfaced for connection-health tracking and the user's Allow / Warn / Deny
/// routing policy (`p2p.md` § Routing). The three variants map cleanly onto an
/// iroh path event.
///
/// `#[non_exhaustive]`: an impl may
/// surface a path kind this enum predates, and `PathKind` matches
/// drive the security-relevant Allow/Warn/Deny policy — a deliberately-added
/// `_ =>` arm in a downstream policy match beats one bolted on under a compile
/// break when the new variant lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathKind {
    /// Same-subnet LAN-direct path — no NAT traversal.
    Lan,
    /// WAN-direct path established via STUN + UDP hole punch.
    WanDirect,
    /// Relayed through the nest (the always-present fallback path).
    Relay,
}

impl PathKind {
    /// Human-readable label for logging / metrics.
    pub fn label(&self) -> &'static str {
        match self {
            PathKind::Lan => "lan",
            PathKind::WanDirect => "wan_direct",
            PathKind::Relay => "relay",
        }
    }

    /// `true` for a direct path (LAN or WAN), `false` for relay.
    pub fn is_direct(&self) -> bool {
        matches!(self, PathKind::Lan | PathKind::WanDirect)
    }
}

/// Whether this device's nest path can carry a pump's bytes this pass — the
/// one fact [`bytes_may_ride`] needs beside the connection's path. The pump
/// already holds it (its own nest walk answered, or did not) and passes it
/// in; the gate never re-derives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestPath {
    /// The nest answered this pass: it carries the bytes a relayed
    /// connection leaves.
    Reachable,
    /// The nest path is unavailable to this device (or not known to be
    /// available): a connection that already stands carries bytes on any
    /// path, since the alternative is no transfer at all.
    Unavailable,
}

/// The one spelling of `p2p.md` § The relay ruling 4 — *bytes ride direct
/// paths only*: may a pump move bytes over a connection on `path` now?
/// Yes on a direct path (`lan`/`wan_direct`), or when the nest path is
/// unavailable; no over a relayed path while the nest can carry them — the
/// pump then walks its rows as ever and leaves the bytes to the nest path,
/// which is never a failed transfer.
///
/// Callers read `path` from the live connection immediately before each
/// byte pull ([`PeerConn::path`]): a relayed connection can turn direct
/// while the substrate keeps punching. This is not a path choice — the
/// substrate still selects the path inside [`PeerTransport::dial`].
pub fn bytes_may_ride(path: PathKind, nest: NestPath) -> bool {
    path.is_direct() || nest == NestPath::Unavailable
}

/// The candidate addresses the caller knows for a peer, fed to
/// [`PeerTransport::dial`] to drive the three-path cascade
/// (LAN-direct → WAN hole-punch → relay).
///
/// Substrate-agnostic by construction: it carries only network addresses + relay
/// availability, never iroh-specific material (the impl resolves
/// the [`EndpointKey`] to whatever substrate handle it needs).
#[derive(Debug, Clone, Default)]
pub struct PathCandidates {
    /// LAN endpoints advertised by the peer (e.g. `192.168.1.100:41641`).
    pub lan_endpoints: Vec<SocketAddr>,
    /// The peer's STUN-discovered public endpoint, for hole punching.
    pub wan_endpoint: Option<SocketAddr>,
    /// Whether a relay path is available as a last resort.
    pub relay_available: bool,
}

/// PT-4 candidate filter: is `addr` a safe peer-advertised dial candidate?
///
/// **Shared by every [`PeerTransport`] impl** (each feeds peer-advertised
/// [`PathCandidates`] through it before dialing) — so the security-relevant
/// address hygiene is defined *once* here, never re-derived per substrate
/// (priority #4: resolve drift, don't replicate a security filter on a second
/// surface).
///
/// `PathCandidates` arrive via peer-advertised signaling and are
/// attacker-influenceable under the hostile-signer model
/// (`docs/goal/architecture/federation.md`). This rejects loopback, unspecified,
/// multicast/broadcast, and link-local (IPv4 `169.254.0.0/16` — which includes
/// the `169.254.169.254` cloud IMDS — and IPv6 `fe80::/10`), so a poisoned
/// candidate can never redirect a hole-punch at the local host or an internal
/// metadata service. RFC-1918 / ULA private ranges are **kept**: those are
/// legitimate LAN candidates (`docs/goal/behavior/p2p.md` § LAN detection).
/// IPv4-mapped IPv6 candidates are unwrapped and re-checked as IPv4, so a
/// `::ffff:127.0.0.1` cannot smuggle a loopback past the IPv4 rules.
///
/// The caller drops (does not error on) an unsafe candidate — the cascade still
/// falls back to the peer's trusted, owner-bound registry base endpoint, so a
/// poisoned candidate is at worst a no-op, never a DoS of the legitimate dial.
pub fn is_safe_candidate(addr: &SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(v4) => is_safe_v4(&v4),
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_safe_v4(&v4),
            None => {
                // fe80::/10 link-local: top 10 bits are 1111111010.
                let link_local = (v6.segments()[0] & 0xffc0) == 0xfe80;
                !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || link_local)
            }
        },
    }
}

/// IPv4 arm of [`is_safe_candidate`]. RFC-1918 (`is_private`) is intentionally
/// NOT rejected — those are valid LAN candidates.
fn is_safe_v4(v4: &std::net::Ipv4Addr) -> bool {
    !(v4.is_loopback()
        || v4.is_unspecified()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_multicast())
}

/// A reliable, ordered, bidirectional byte stream over a peer connection.
///
/// This is what Y.1 envelopes / chunk transfer ride. Any `AsyncRead + AsyncWrite`
/// satisfies it: for iroh a QUIC bidi stream (its send + recv halves joined).
pub trait DuplexStream: AsyncRead + AsyncWrite + Send {}
impl<T: AsyncRead + AsyncWrite + Send + ?Sized> DuplexStream for T {}

/// A boxed [`DuplexStream`] — the concrete return of [`PeerConn::open_stream`].
pub type ByteStream = Pin<Box<dyn DuplexStream>>;

/// A stream of inbound peer connections yielded by [`PeerTransport::listen`].
pub type IncomingConns =
    Pin<Box<dyn Stream<Item = Result<Box<dyn PeerConn>, TransportError>> + Send>>;

/// Errors a transport impl can surface across the seam.
///
/// `#[non_exhaustive]`: a second
/// (iroh) impl may add an error kind this enum predates; forcing downstream
/// `match`es to carry a wildcard arm now keeps that additive evolution from
/// breaking callers' compiles later.
///
/// **PT-5 (security contract):** impls MUST NOT embed key material, PII, or
/// internal addresses in the [`Io`](TransportError::Io) / [`Other`](TransportError::Other)
/// strings — these are logged.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The three-path cascade was exhausted — no LAN, WAN, or relay path reached
    /// the peer.
    #[error("no path to peer: all cascade paths exhausted")]
    NoPath,
    /// A relay path existed but the user's routing policy denied it.
    #[error("relay path denied by routing policy")]
    RelayDenied,
    /// This impl does not support the requested operation on this platform
    /// (e.g. a browser sandbox forbids the raw UDP socket QUIC needs, so the
    /// web app is relay-only).
    #[error("operation not supported by this transport")]
    Unsupported,
    /// An I/O error from the underlying substrate.
    #[error("transport i/o error: {0}")]
    Io(String),
    /// Any other impl-specific failure.
    #[error("{0}")]
    Other(String),
}

/// An established connection to a single peer.
///
/// Many streams may be opened over one connection (HTTP/2- or QUIC-multiplexing
/// style). The connection holds the verified peer identity
/// and the path it runs over.
#[async_trait]
pub trait PeerConn: Send + Sync {
    /// Open a reliable, ordered, bidirectional byte stream over this connection —
    /// the **initiator** side. The opener writes first (so a request/reply's
    /// *requester* opens the stream).
    async fn open_stream(&self) -> Result<ByteStream, TransportError>;

    /// Accept a peer-*opened* bidirectional byte stream — the **accepter** side,
    /// the counterpart to [`open_stream`](Self::open_stream). A request/reply's
    /// *server* accepts (to read the request the requester opened + wrote), so a
    /// serve loop over a `listen()`-accepted [`PeerConn`] calls this, not
    /// `open_stream` (opening its own stream would deadlock: nothing writes
    /// first).
    ///
    /// Default: [`Unsupported`](TransportError::Unsupported) — an impl whose
    /// inbound conn hands out a single already-accepted stream via `open_stream`
    /// inherits it. The iroh-QUIC impl overrides it with `accept_bi`. A serve
    /// loop treats `Unsupported` as "nothing to accept" and does not serve over
    /// such a connection.
    async fn accept_stream(&self) -> Result<ByteStream, TransportError> {
        Err(TransportError::Unsupported)
    }

    /// The **verified** remote identity (Ed25519).
    ///
    /// The transport proves the remote controls this key during its own
    /// handshake; the caller then applies the pair-type auth witness to it
    /// (design §2(a)) — this method does not itself authorise the peer for any
    /// purpose.
    fn peer_identity(&self) -> EndpointKey;

    /// Which path this connection runs over (LAN / WAN-direct / relay).
    fn path(&self) -> PathKind;
}

/// A P2P transport substrate.
///
/// The iroh-QUIC stack (`libs/fauna-iroh`) is the sole impl; the seam stays
/// substrate-agnostic so a second one is droppable. A consumer holds one behind
/// dynamic dispatch (`Arc<dyn PeerTransport>`), selected by capability
/// negotiation, with transparent fallback to the nest-mediated path when no impl
/// or no path is available.
///
/// **Symmetric.** A *nest* dials another nest over this same trait
/// (`private-nest↔private-nest`, design §2), so the local-endpoint identity is
/// "an Ed25519 actor *or* nest key" and the auth witness the caller applies to
/// [`PeerConn::peer_identity`] is pair-type-dependent.
#[async_trait]
pub trait PeerTransport: Send + Sync {
    /// Establish a connection to `peer`, traversing NAT via the three-path
    /// cascade (LAN-direct → WAN hole-punch → relay) using `candidates`.
    ///
    /// Returns a [`PeerConn`] on success or [`TransportError::NoPath`] when the
    /// cascade is exhausted — the caller then routes over the always-available
    /// nest-mediated fallback (which is *not* this seam's concern).
    async fn dial(
        &self,
        peer: EndpointKey,
        candidates: PathCandidates,
    ) -> Result<Box<dyn PeerConn>, TransportError>;

    /// Accept inbound peer connections. Each impl drives its own
    /// handshake/auth-witness and yields a [`PeerConn`] per accepted peer.
    async fn listen(&self) -> Result<IncomingConns, TransportError>;

    /// This local endpoint's identity — an actor key (client) or a nest key
    /// (nest-as-endpoint mode).
    fn local_identity(&self) -> EndpointKey;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_key_roundtrips_and_debug_is_short() {
        let mut raw = [0u8; 32];
        raw[0] = 0xde;
        raw[1] = 0xad;
        raw[2] = 0xbe;
        raw[3] = 0xef;
        let key = EndpointKey::from_bytes(raw);
        assert_eq!(key.as_bytes(), &raw);
        assert_eq!(format!("{key:?}"), "EndpointKey(deadbeef…)");
    }

    #[test]
    fn endpoint_key_eq_and_hash_by_value() {
        use std::collections::HashSet;
        let a = EndpointKey::from_bytes([1u8; 32]);
        let b = EndpointKey::from_bytes([1u8; 32]);
        let c = EndpointKey::from_bytes([2u8; 32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    #[test]
    fn path_kind_labels_and_directness() {
        assert_eq!(PathKind::Lan.label(), "lan");
        assert_eq!(PathKind::WanDirect.label(), "wan_direct");
        assert_eq!(PathKind::Relay.label(), "relay");
        assert!(PathKind::Lan.is_direct());
        assert!(PathKind::WanDirect.is_direct());
        assert!(!PathKind::Relay.is_direct());
    }

    /// Ruling 4's truth table: a direct path always carries bytes, a relayed
    /// one only while the nest path is unavailable.
    #[test]
    fn bytes_ride_direct_paths_or_when_the_nest_path_is_unavailable() {
        for path in [PathKind::Lan, PathKind::WanDirect] {
            assert!(bytes_may_ride(path, NestPath::Reachable));
            assert!(bytes_may_ride(path, NestPath::Unavailable));
        }
        assert!(!bytes_may_ride(PathKind::Relay, NestPath::Reachable));
        assert!(bytes_may_ride(PathKind::Relay, NestPath::Unavailable));
    }

    #[test]
    fn path_candidates_default_is_empty_relay_off() {
        let c = PathCandidates::default();
        assert!(c.lan_endpoints.is_empty());
        assert!(c.wan_endpoint.is_none());
        assert!(!c.relay_available);
    }

    #[test]
    fn transport_error_display() {
        assert_eq!(
            TransportError::NoPath.to_string(),
            "no path to peer: all cascade paths exhausted"
        );
        assert_eq!(
            TransportError::Io("eof".into()).to_string(),
            "transport i/o error: eof"
        );
    }

    fn sock(s: &str) -> SocketAddr {
        s.parse().expect("valid socket addr")
    }

    /// PT-4: peer-advertised candidates pointing at loopback / link-local (incl.
    /// the 169.254.169.254 cloud IMDS) / unspecified / multicast / broadcast are
    /// rejected, while public and RFC-1918 LAN candidates are kept — so a poisoned
    /// signaling candidate can never redirect a hole-punch at the local host or an
    /// internal metadata service, but a legitimate LAN peer still connects. Shared
    /// by the iroh transport impl.
    #[test]
    fn pt4_rejects_loopback_linklocal_internal_keeps_lan_and_public() {
        // Rejected.
        for bad in [
            "127.0.0.1:51820",             // IPv4 loopback
            "169.254.169.254:80",          // cloud IMDS (link-local)
            "169.254.1.2:51820",           // IPv4 link-local
            "0.0.0.0:51820",               // IPv4 unspecified
            "255.255.255.255:51820",       // IPv4 broadcast
            "224.0.0.1:51820",             // IPv4 multicast
            "[::1]:51820",                 // IPv6 loopback
            "[fe80::1]:51820",             // IPv6 link-local
            "[::]:51820",                  // IPv6 unspecified
            "[ff02::1]:51820",             // IPv6 multicast
            "[::ffff:127.0.0.1]:51820",    // IPv4-mapped loopback (must not smuggle past v4 rules)
            "[::ffff:169.254.169.254]:80", // IPv4-mapped IMDS
        ] {
            assert!(
                !is_safe_candidate(&sock(bad)),
                "candidate {bad} must be rejected"
            );
        }

        // Kept: public + RFC-1918 LAN + IPv6 global/ULA (legitimate LAN candidates).
        for ok in [
            "1.2.3.4:51820",                // public IPv4
            "10.0.0.5:51820",               // RFC-1918
            "172.16.3.4:51820",             // RFC-1918
            "192.168.1.100:51820",          // RFC-1918
            "[2001:db8::1]:51820",          // IPv6 documentation/global-scope
            "[fc00::1]:51820",              // IPv6 ULA (private LAN)
            "[::ffff:192.168.1.100]:51820", // IPv4-mapped RFC-1918
        ] {
            assert!(is_safe_candidate(&sock(ok)), "candidate {ok} must be kept");
        }
    }

    /// A trivial in-memory impl proves the trait is object-safe (`Arc<dyn …>`),
    /// that a tokio duplex satisfies [`ByteStream`], and that the whole surface
    /// composes — the seam holds before any substrate impl exists.
    #[tokio::test]
    async fn trait_is_object_safe_and_composes() {
        use std::sync::Arc;

        struct MemConn {
            peer: EndpointKey,
        }
        #[async_trait]
        impl PeerConn for MemConn {
            async fn open_stream(&self) -> Result<ByteStream, TransportError> {
                // A loopback duplex pipe is AsyncRead + AsyncWrite → a ByteStream.
                // Keep the read half alive by draining it on a task, so the write
                // half doesn't see a BrokenPipe.
                let (client, mut server) = tokio::io::duplex(64);
                tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 64];
                    while server.read(&mut buf).await.unwrap_or(0) > 0 {}
                });
                Ok(Box::pin(client))
            }
            fn peer_identity(&self) -> EndpointKey {
                self.peer
            }
            fn path(&self) -> PathKind {
                PathKind::Lan
            }
        }

        struct MemTransport {
            me: EndpointKey,
        }
        #[async_trait]
        impl PeerTransport for MemTransport {
            async fn dial(
                &self,
                peer: EndpointKey,
                _candidates: PathCandidates,
            ) -> Result<Box<dyn PeerConn>, TransportError> {
                Ok(Box::new(MemConn { peer }))
            }
            async fn listen(&self) -> Result<IncomingConns, TransportError> {
                Err(TransportError::Unsupported)
            }
            fn local_identity(&self) -> EndpointKey {
                self.me
            }
        }

        let transport: Arc<dyn PeerTransport> = Arc::new(MemTransport {
            me: EndpointKey::from_bytes([7u8; 32]),
        });
        assert_eq!(
            transport.local_identity(),
            EndpointKey::from_bytes([7u8; 32])
        );

        let peer = EndpointKey::from_bytes([9u8; 32]);
        let conn = transport
            .dial(peer, PathCandidates::default())
            .await
            .expect("dial");
        assert_eq!(conn.peer_identity(), peer);
        assert_eq!(conn.path(), PathKind::Lan);

        // open_stream yields something we can write to (round-trips through the
        // boxed-trait-object stream).
        use tokio::io::AsyncWriteExt;
        let mut stream = conn.open_stream().await.expect("open_stream");
        stream.write_all(b"hi").await.expect("write");

        assert!(matches!(
            transport.listen().await,
            Err(TransportError::Unsupported)
        ));
    }
}
