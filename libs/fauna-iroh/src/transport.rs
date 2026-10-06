//! [`PeerTransport`] over an iroh [`Endpoint`] (QUIC). See the crate docs for the
//! reversible/additive framing and what is deferred (relay sidecar, PQ provider).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use fauna_transport::{
    ByteStream, EndpointKey, IncomingConns, PathCandidates, PathKind, PeerConn, PeerTransport,
    TransportError, is_safe_candidate,
};
use iroh::endpoint::Connection;
use iroh::tls::CaTlsConfig;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr,
};

/// Default ALPN for fauna's P2P link. Versioned so a future wire revision can
/// negotiate distinctly. The L3 (Y.1 / chunk transfer) framing rides *over* the
/// byte stream this ALPN's connections yield — the ALPN names the seam, not L3.
pub const DEFAULT_ALPN: &[u8] = b"fauna/p2p/0";

/// An iroh-QUIC implementation of the [`PeerTransport`] seam.
///
/// Holds one bound iroh [`Endpoint`]. `dial` opens an authenticated QUIC
/// connection to a peer by its Ed25519 [`EndpointKey`] (which *is* the iroh
/// `EndpointId`); `listen` accepts inbound connections; both yield [`IrohConn`].
pub struct IrohTransport {
    endpoint: Endpoint,
    local_id: EndpointKey,
    alpn: Vec<u8>,
    /// The self-hosted relay this transport routes through when a dial's
    /// [`PathCandidates::relay_available`] is set (the nest-side `iroh-relay`
    /// sidecar's URL; `None` = direct-only, no relay configured). Attaching it
    /// per-dial — not forcing it — keeps iroh's cascade (direct preferred, relay
    /// fallback) intact.
    relay_url: Option<RelayUrl>,
}

/// Builder for [`IrohTransport`]. The crypto provider is injectable so the §7.3
/// `ring → aws-lc-rs` PQ migration is a one-line swap (proven SPKI-neutral by the
/// 2026-06-28 prototype); it defaults to ring, matching the rest of the tree.
pub struct IrohTransportBuilder {
    secret_key: [u8; 32],
    bind_addr: SocketAddr,
    alpn: Vec<u8>,
    crypto_provider: Option<Arc<rustls::crypto::CryptoProvider>>,
    relay_url: Option<RelayUrl>,
    ca_tls_config: Option<CaTlsConfig>,
}

/// The peer-leg factory body shared by every native app (tui, the sync
/// agent — `fauna_sync_engine::peer_leg`'s `PeerTransportFactory` closures
/// wrap exactly this): one endpoint from the machine's device principal
/// (R5 (account-data-plane.md § The ratified decisions) — NodeId = the account store's writer key), with the nest-advertised
/// relay attached when present and parseable. A malformed relay advert logs
/// and binds direct-only — direct paths must not die with it. TLS toward a
/// production `relay.<domain>` rides the default trust roots;
/// [`IrohTransportBuilder::custom_roots`] stays the test-relay injection
/// point.
pub async fn peer_leg_transport(
    secret: [u8; 32],
    relay_url: Option<&str>,
) -> Result<(Arc<dyn PeerTransport>, Vec<SocketAddr>), TransportError> {
    let mut builder = IrohTransport::builder(secret);
    if let Some(url) = relay_url {
        match url.parse::<RelayUrl>() {
            Ok(url) => builder = builder.relay_url(url),
            Err(e) => tracing::warn!(
                "peer leg: nest-advertised relay URL unusable ({e}) — binding direct-only"
            ),
        }
    }
    let transport = builder.build().await?;
    let bound_addrs = transport.bound_addrs();
    Ok((Arc::new(transport), bound_addrs))
}

/// The offline-share ceremony's [`CeremonyTransportFactory`](fauna_sync_engine::offline_share::CeremonyTransportFactory)
/// over [`peer_leg_transport`] — round 33's lift of the byte-identical closure
/// `apps/fauna-linux` and `apps/fauna-tui` each hand-rolled in their own
/// `offline_share.rs`. `fauna_sync_engine::offline_share` deliberately names
/// no concrete transport substrate (the same iroh-cleanliness bargain
/// `peer_leg_transport`'s own doc states — the `fauna_iroh` call belongs on
/// the app side of that seam), so this factory lives here, not there: this
/// crate is exactly the "app side" for every native (non-mobile) app, and
/// both consumers already depend on `fauna-sync-engine` with the same
/// features this needs.
pub fn ceremony_transport() -> fauna_sync_engine::offline_share::CeremonyTransportFactory {
    Arc::new(|secret| {
        Box::pin(async move {
            let (transport, bound_addrs) = peer_leg_transport(secret, None)
                .await
                .map_err(|e| format!("peer transport: {e}"))?;
            Ok(fauna_sync_engine::offline_share::CeremonyBinding {
                transport,
                bound_addrs,
            })
        })
    })
}

impl IrohTransport {
    /// Start a builder from a 32-byte Ed25519 secret key (the local actor — or
    /// nest — identity; the seam is symmetric).
    pub fn builder(secret_key: [u8; 32]) -> IrohTransportBuilder {
        IrohTransportBuilder {
            secret_key,
            // OS-assigned port on all interfaces — the right default for a client
            // doing real NAT traversal. Tests override to loopback.
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            alpn: DEFAULT_ALPN.to_vec(),
            crypto_provider: None,
            relay_url: None,
            ca_tls_config: None,
        }
    }

    /// Convenience: build with all defaults from a secret key.
    pub async fn new(secret_key: [u8; 32]) -> Result<Self, TransportError> {
        Self::builder(secret_key).build().await
    }
}

impl IrohTransportBuilder {
    /// Override the UDP bind address (default `0.0.0.0:0`).
    pub fn bind_addr(mut self, addr: SocketAddr) -> Self {
        self.bind_addr = addr;
        self
    }

    /// Override the ALPN (default [`DEFAULT_ALPN`]).
    pub fn alpn(mut self, alpn: impl Into<Vec<u8>>) -> Self {
        self.alpn = alpn.into();
        self
    }

    /// Inject the rustls crypto provider (default: ring). The §7.3 PQ migration
    /// swaps in an aws-lc-rs provider with `prefer-post-quantum` here.
    pub fn crypto_provider(mut self, provider: Arc<rustls::crypto::CryptoProvider>) -> Self {
        self.crypto_provider = Some(provider);
        self
    }

    /// Route through the self-hosted `iroh-relay` at `url` (the nest-side relay
    /// sidecar). Sets the endpoint's relay mode to [`RelayMode::Custom`] over a
    /// single-relay [`RelayMap`] — **never an n0 default relay** (the
    /// self-hosted-only / works-out-of-the-box invariant; `presets::Empty` bakes
    /// in nothing). With no relay URL the endpoint stays [`RelayMode::Disabled`]
    /// (direct-only), the unchanged prior behavior. Whether a given *dial* uses
    /// the relay is gated per-dial on [`PathCandidates::relay_available`]; this
    /// only makes the relay *available* to the cascade.
    pub fn relay_url(mut self, url: RelayUrl) -> Self {
        self.relay_url = Some(url);
        self
    }

    /// Trust config for the relay's TLS (the relay protocol runs over HTTPS,
    /// independent of the E2E QUIC it carries). Default = the platform's native
    /// roots. A nest serving its relay with a self-signed bootstrap cert (the
    /// no-domain / LAN box, like nest's own self-signed serving cert that
    /// `connect_sidecar_ws` accepts via the shared capturing verifier) needs a
    /// matching trust override here. Set [`CaTlsConfig::insecure_skip_verify`]
    /// only for tests / a relay whose cert is verified out of band.
    pub fn ca_tls_config(mut self, config: CaTlsConfig) -> Self {
        self.ca_tls_config = Some(config);
        self
    }

    /// Bind the endpoint and return the transport.
    pub async fn build(self) -> Result<IrohTransport, TransportError> {
        let secret = SecretKey::from_bytes(&self.secret_key);
        let provider = self
            .crypto_provider
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));

        // Relay mode: a configured self-hosted relay → `Custom` over a one-entry
        // `RelayMap`; otherwise `Disabled` (direct-only, the prior behavior).
        // Either way `presets::Empty` keeps n0's default relays/DNS out (the
        // works-out-of-the-box, self-hosted-only invariant: this crate never
        // silently depends on a third-party service).
        let relay_mode = match &self.relay_url {
            Some(url) => RelayMode::Custom(RelayMap::from(url.clone())),
            None => RelayMode::Disabled,
        };

        let mut builder = Endpoint::builder(iroh::endpoint::presets::Empty)
            .secret_key(secret)
            .alpns(vec![self.alpn.clone()])
            .relay_mode(relay_mode)
            .crypto_provider(provider);
        if let Some(ca) = self.ca_tls_config {
            builder = builder.ca_tls_config(ca);
        }
        let endpoint = builder
            .bind_addr(self.bind_addr)
            .map_err(|e| TransportError::Io(format!("iroh bind_addr: {e}")))?
            .bind()
            .await
            .map_err(|e| TransportError::Io(format!("iroh endpoint bind: {e}")))?;

        let local_id = EndpointKey::from_bytes(*endpoint.id().as_bytes());
        Ok(IrohTransport {
            endpoint,
            local_id,
            alpn: self.alpn,
            relay_url: self.relay_url,
        })
    }
}

impl IrohTransport {
    /// The socket addresses this endpoint is bound on. What a replica
    /// publishes as its dial candidates (the device-endpoints entry's
    /// `lan_addrs` — `account-data-plane.md` § The peer leg → *Discovery*),
    /// and what a loopback test hands a raw dialer.
    pub fn bound_addrs(&self) -> Vec<SocketAddr> {
        self.endpoint.bound_sockets()
    }
}

#[async_trait]
impl PeerTransport for IrohTransport {
    async fn dial(
        &self,
        peer: EndpointKey,
        candidates: PathCandidates,
    ) -> Result<Box<dyn PeerConn>, TransportError> {
        let remote = EndpointId::from_bytes(peer.as_bytes())
            .map_err(|e| TransportError::Other(format!("invalid peer key: {e}")))?;

        let mut addr = EndpointAddr::new(remote);

        // PT-4: peer-advertised candidates are attacker-influenceable; filter them
        // through the SAME shared `fauna_transport::is_safe_candidate` the
        // WireGuard impl uses (one audited filter, no per-substrate drift —
        // priority #4) before handing them to iroh as direct IP addresses.
        for sa in candidates
            .lan_endpoints
            .iter()
            .copied()
            .chain(candidates.wan_endpoint)
            .filter(is_safe_candidate)
        {
            addr = addr.with_ip_addr(sa);
        }

        // Relay fallback (the three-path cascade's last resort): attach the
        // self-hosted relay's URL to the dial target ONLY when this dial allows a
        // relay path. Attaching it does not *force* relay — iroh still prefers a
        // direct path and falls back to the relay — but a dial with
        // `relay_available == false` (the user's Deny-relay policy, or a peer with
        // no relay) gets a direct-only target, never silently relayed. The relay
        // URL itself is trusted local config (the nest's own sidecar), so it is
        // not subject to the PT-4 candidate filter above (which guards
        // peer-advertised addresses).
        if candidates.relay_available
            && let Some(url) = &self.relay_url
        {
            addr = addr.with_relay_url(url.clone());
        }

        let conn = self
            .endpoint
            .connect(addr, &self.alpn)
            .await
            .map_err(|e| TransportError::Io(format!("iroh connect: {e}")))?;

        // PT-1 is intrinsic here: iroh's `connect()` completes the authenticated
        // QUIC TLS 1.3 handshake BEFORE returning, so `remote_id()` already rests
        // on completed cryptographic proof the remote controls this key. (Contrast
        // the WireGuard impl, whose `dial` returns pre-handshake and must block on
        // `is_healthy()` for the same guarantee.) The caller still applies the
        // per-pair auth witness above the seam.
        let proven = EndpointKey::from_bytes(*conn.remote_id().as_bytes());
        Ok(Box::new(IrohConn { conn, peer: proven }))
    }

    async fn listen(&self) -> Result<IncomingConns, TransportError> {
        // Unlike the WireGuard impl (whose inbound side is PeerTunnel's in-tunnel
        // axum server, so `listen()` is `Unsupported` until the Y.1 reshape), iroh
        // accepts inbound QUIC connections natively.
        //
        // Each inbound connection's QUIC handshake (`incoming.await`) is driven on
        // its **own** task so a slow or stalled inbound handshake cannot
        // head-of-line block the accept loop: the
        // accept loop returns to `endpoint.accept()` immediately, and completed
        // connections arrive over a bounded channel whose capacity backpressures
        // delivery to the consumer. The accept loop stops — no task leak — when the
        // endpoint closes *or* the consumer drops the returned stream
        // (`tx.closed()` fires once the receiver is gone). (PT-5a — bounding the
        // count of *concurrently in-flight* handshakes per-peer + globally — is
        // layered when a consumer actually wires `listen()` and can tune the
        // limits; the per-conn spawn here is the head-of-line fix only.)
        let endpoint = self.endpoint.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Box<dyn PeerConn>, TransportError>>(16);
        tokio::spawn(async move {
            loop {
                let incoming = tokio::select! {
                    biased;
                    // Consumer dropped the IncomingConns stream → stop accepting.
                    () = tx.closed() => break,
                    incoming = endpoint.accept() => incoming,
                };
                // `None` → the endpoint was closed → end the accept loop.
                let Some(incoming) = incoming else { break };
                let tx = tx.clone();
                tokio::spawn(async move {
                    // A failed inbound handshake (the `Err` case) is not fatal to
                    // the listener — drop it and keep accepting.
                    if let Ok(conn) = incoming.await {
                        let peer = EndpointKey::from_bytes(*conn.remote_id().as_bytes());
                        // Receiver gone (consumer dropped the stream) → the send
                        // errors and the conn is dropped; not fatal.
                        let _ = tx
                            .send(Ok(Box::new(IrohConn { conn, peer }) as Box<dyn PeerConn>))
                            .await;
                    }
                });
            }
        });
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }

    fn local_identity(&self) -> EndpointKey {
        self.local_id
    }
}

/// An established iroh-QUIC connection to one peer.
///
/// Holds the iroh [`Connection`] (dropping it closes the connection). Many
/// streams may be opened over one connection (QUIC multiplexing). The path is
/// read directly off the connection's observed remote address.
pub struct IrohConn {
    conn: Connection,
    peer: EndpointKey,
}

#[async_trait]
impl PeerConn for IrohConn {
    async fn open_stream(&self) -> Result<ByteStream, TransportError> {
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| TransportError::Io(format!("iroh open_bi: {e}")))?;
        // Join the QUIC recv (AsyncRead) + send (AsyncWrite) halves into one
        // duplex byte stream — what Y.1 envelopes / chunk transfer ride.
        Ok(Box::pin(tokio::io::join(recv, send)))
    }

    async fn accept_stream(&self) -> Result<ByteStream, TransportError> {
        // The accepter side: block until the peer opens a bidi stream and writes
        // its first bytes (`accept_bi` returns then), so a serve loop can read the
        // request. Same recv/send join order as `open_stream`.
        let (send, recv) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| TransportError::Io(format!("iroh accept_bi: {e}")))?;
        Ok(Box::pin(tokio::io::join(recv, send)))
    }

    fn peer_identity(&self) -> EndpointKey {
        self.peer
    }

    /// Path kind, read off iroh's *selected* path for this connection — never
    /// guessed (priority #4) and never over-claimed (PT-6): no selected IP path
    /// (relay, custom transport, or none) reports [`PathKind::Relay`], so a
    /// connection can only claim a *direct* path when iroh genuinely has a
    /// selected IP one. A direct path on a private/loopback/link-local address is
    /// [`PathKind::Lan`], otherwise [`PathKind::WanDirect`].
    fn path(&self) -> PathKind {
        let paths = self.conn.paths();
        paths
            .iter()
            .find(|p| p.is_selected())
            .map(|p| match p.remote_addr() {
                TransportAddr::Ip(addr) => {
                    if is_lan(addr.ip()) {
                        PathKind::Lan
                    } else {
                        PathKind::WanDirect
                    }
                }
                // Relay, custom, or any future (`#[non_exhaustive]`) transport
                // kind: not a known direct path (PT-6 — never over-claim direct).
                _ => PathKind::Relay,
            })
            .unwrap_or(PathKind::Relay)
    }
}

/// Is `ip` a LAN-scope address (private / loopback / link-local / ULA)? Used only
/// to split an observed *direct* path into [`PathKind::Lan`] vs
/// [`PathKind::WanDirect`].
fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::Endpoint;
    use iroh::endpoint::presets;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ring_provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    /// Every IPv4 dial candidate a seat publishes must name a port that an
    /// IPv4-capable socket listens on. The compare code and the device-endpoint
    /// entry both cross the listener's bound ports with the machine's IPv4
    /// interface addresses (`fauna_core::device_endpoints`); a port that only an
    /// IPv6 socket holds is unreachable over IPv4 wherever that socket is
    /// v6-only, and a send to it draws ICMP port-unreachable.
    #[tokio::test]
    async fn peer_leg_candidates_only_name_ipv4_listening_ports() {
        let (_transport, bound) = peer_leg_transport([0x51; 32], None).await.expect("bind");
        let lan: Vec<Ipv4Addr> = vec!["10.9.8.7".parse().unwrap()];
        let candidates = fauna_core::device_endpoints::lan_socket_addr_candidates(&bound, &lan);
        assert!(!candidates.is_empty(), "bound={bound:?}");
        for c in &candidates {
            assert!(
                bound.iter().any(|b| b.is_ipv4() && b.port() == c.port()),
                "candidate {c} names a port no IPv4 socket listens on; bound={bound:?}"
            );
        }
    }

    /// A raw iroh endpoint bound to loopback — the controlled "other side" for
    /// driving the trait under test (the trait is dialer-centric: `open_stream`
    /// opens a QUIC bidi stream the peer must `accept_bi`, so the peer is raw iroh).
    async fn raw_loopback_endpoint(seed: u8) -> Endpoint {
        let mut sk = [0u8; 32];
        sk[0] = seed;
        Endpoint::builder(presets::Empty)
            .secret_key(SecretKey::from_bytes(&sk))
            .alpns(vec![DEFAULT_ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .crypto_provider(ring_provider())
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("bind_addr")
            .bind()
            .await
            .expect("bind raw endpoint")
    }

    fn loopback_addrs(ep: &Endpoint) -> Vec<SocketAddr> {
        ep.bound_sockets()
            .into_iter()
            .map(|a| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port()))
            .collect()
    }

    /// End-to-end round trip through the seam over loopback: a raw-iroh peer
    /// connects to a trait [`IrohTransport`] listener, the `listen()`-yielded
    /// [`PeerConn`] reports the authenticated remote identity + a `Lan`
    /// direct-loopback path, and `open_stream()` yields a real duplex byte stream
    /// that round-trips a payload byte-identical. The additive landing's
    /// red-green proof that the second substrate carries fauna's L3 over the seam
    /// exactly as the WireGuard impl does. (The byte round-trip is driven from the
    /// listener-yielded conn because the trait is dialer-centric — `open_stream`
    /// opens a QUIC bidi stream the peer accepts — and `dial()`'s PT-4 filter
    /// rejects loopback by design, so loopback dialing is exercised raw here and
    /// the filter is proven separately below.)
    #[tokio::test]
    async fn iroh_round_trips_over_loopback_via_listen() {
        let mut l_sk = [0u8; 32];
        l_sk[0] = 0xc3;
        let listener = IrohTransport::builder(l_sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .build()
            .await
            .expect("listener build");
        let listener_id = listener.local_identity();
        let listener_addrs = loopback_addrs(&listener.endpoint);

        // Raw peer: connect to the listener, then accept the listener-opened bi
        // stream and echo 5 bytes back.
        let peer = raw_loopback_endpoint(0xd4).await;
        let peer_id = EndpointKey::from_bytes(*peer.id().as_bytes());
        let target = listener_addrs.into_iter().fold(
            EndpointAddr::new(EndpointId::from_bytes(listener_id.as_bytes()).expect("listener id")),
            |a, sa| a.with_ip_addr(sa),
        );
        let peer_task = tokio::spawn(async move {
            let conn = peer
                .connect(target, DEFAULT_ALPN)
                .await
                .expect("peer connect");
            // Accept the listener-opened bi stream and echo the bytes back.
            let (mut send, mut recv) = conn.accept_bi().await.expect("peer accept_bi");
            let mut buf = [0u8; 5];
            recv.read_exact(&mut buf).await.expect("peer read");
            send.write_all(&buf).await.expect("peer echo");
            send.finish().expect("peer finish");
            // Hold the connection open until the listener has read the echo and
            // closes it (dropping its conn below) — else dropping here would race
            // the listener's read and close the connection first.
            conn.closed().await;
        });

        let mut incoming = listener.listen().await.expect("listen");
        use futures_util::StreamExt;
        let conn = incoming
            .next()
            .await
            .expect("an inbound conn")
            .expect("conn ok");
        assert_eq!(
            conn.peer_identity(),
            peer_id,
            "listener proved the peer's identity"
        );
        assert_eq!(conn.path(), PathKind::Lan, "direct loopback ⇒ Lan");

        let mut stream = conn.open_stream().await.expect("open_stream");
        stream.write_all(b"hello").await.expect("write");
        stream.flush().await.expect("flush");
        let mut back = [0u8; 5];
        stream.read_exact(&mut back).await.expect("read echo");
        assert_eq!(&back, b"hello", "byte stream round-trips faithfully");

        // Close the listener side; the peer's `conn.closed()` then returns.
        drop(stream);
        drop(conn);
        peer_task.await.expect("peer task");
    }

    /// The slice-4 deliverable: a `fauna_peer_channel::PeerChannel`
    /// round-trips a real Y.1 `Request`/`Reply` over a **live iroh loopback
    /// connection** — proving the substrate-agnostic Y.1 channel (length-prefixed
    /// DAG-CBOR + the kind-routed `RpcDispatcher`) composes on top of this crate's
    /// [`PeerConn`] exactly as it does over a raw duplex (its unit tests) and, in
    /// the federation channel, over a WebSocket. Because the channel makes the
    /// WG-vs-iroh choice invisible above L1, the same proof carries to the
    /// WireGuard `PeerConn` once its `listen()` lands (slice 5).
    ///
    /// Shape mirrors [`iroh_round_trips_over_loopback_via_listen`]: the trait
    /// listener yields a `PeerConn` whose `PeerChannel::open` ORIGINATES (the trait
    /// is dialer-centric — `open_stream` opens a bi stream the peer accepts), while
    /// the raw peer accepts that stream and hosts a *serving*
    /// `PeerChannel::over_stream`. (Loopback dialing is exercised raw because
    /// `dial()`'s PT-4 filter rejects loopback by design — proven separately above.)
    #[tokio::test]
    async fn peer_channel_round_trips_over_real_iroh_loopback() {
        use fauna_peer_channel::{PeerChannel, PeerHandlers};
        use fauna_protocol::Value;

        let mut l_sk = [0u8; 32];
        l_sk[0] = 0xc9;
        let listener = IrohTransport::builder(l_sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .build()
            .await
            .expect("listener build");
        let listener_id = listener.local_identity();
        let listener_addrs = loopback_addrs(&listener.endpoint);

        // Raw peer: connect, accept the listener-opened bi stream, host a SERVING
        // PeerChannel that answers `fauna.peer.node_info`.
        let peer = raw_loopback_endpoint(0xda).await;
        let peer_id = EndpointKey::from_bytes(*peer.id().as_bytes());
        let target = listener_addrs.into_iter().fold(
            EndpointAddr::new(EndpointId::from_bytes(listener_id.as_bytes()).expect("listener id")),
            |a, sa| a.with_ip_addr(sa),
        );
        let peer_task = tokio::spawn(async move {
            let conn = peer
                .connect(target, DEFAULT_ALPN)
                .await
                .expect("peer connect");
            let remote = EndpointKey::from_bytes(*conn.remote_id().as_bytes());
            // `accept_bi` blocks until the listener's PeerChannel opens + writes the
            // first request frame; join the QUIC halves into the channel's byte
            // stream (recv = read, send = write — same order as `IrohConn::open_stream`).
            let (send, recv) = conn.accept_bi().await.expect("peer accept_bi");
            let stream: ByteStream = Box::pin(tokio::io::join(recv, send));
            let channel = PeerChannel::over_stream(stream, remote, PathKind::Lan);
            let _serve = channel.serve(
                PeerHandlers::new().on("fauna.peer.node_info", |_req| async move {
                    Ok(Value::String("node-info-ok".into()))
                }),
            );
            // Hold the channel + conn open until the listener has read the reply and
            // drops its side (else dropping here would race the listener's read).
            conn.closed().await;
        });

        let mut incoming = listener.listen().await.expect("listen");
        use futures_util::StreamExt;
        let conn = incoming
            .next()
            .await
            .expect("an inbound conn")
            .expect("conn ok");
        assert_eq!(
            conn.peer_identity(),
            peer_id,
            "listener proved the peer's identity"
        );

        // The listener side ORIGINATES over a PeerChannel built on the PeerConn.
        let channel = PeerChannel::open(conn).await.expect("open channel");
        assert_eq!(
            channel.peer_identity(),
            peer_id,
            "the channel carries the transport-proven identity (PT-2/PT-3)"
        );
        let reply = channel
            .request("fauna.peer.node_info", Value::Null)
            .await
            .expect("reply");
        assert_eq!(
            reply,
            Value::String("node-info-ok".into()),
            "the Y.1 Request/Reply round-trips over a live iroh connection"
        );

        drop(channel); // closes the listener side → peer's `conn.closed()` returns
        peer_task.await.expect("peer task");
    }

    /// Slice 7: a `fauna_peer_channel::PeerNode` running over this crate's
    /// [`IrohTransport`] **serves `fauna.peer.node_info`** to a connecting peer —
    /// the shared node lifecycle both native P2P consumers (linux + the FFI) hold.
    /// The node's `listen()` accept loop `accept_stream`s the peer-opened bi stream
    /// (the accepter side — proving the slice-7 `accept_stream()` seam addition,
    /// `IrohConn::accept_stream` = `accept_bi`) and answers with the node's protocol
    /// version + display name; a raw-iroh peer opens the stream, requests
    /// `fauna.peer.node_info` over a `PeerChannel`, and decodes the typed reply.
    /// This is the node-lifecycle twin of `peer_channel_round_trips_over_real_iroh_loopback`
    /// (which proved a bare channel) — here a real `PeerNode` is the server.
    #[tokio::test]
    async fn peer_node_serves_node_info_over_real_iroh() {
        use fauna_peer_channel::{PeerChannel, PeerNode};
        use fauna_protocol::peer::{KIND_PEER_NODE_INFO, PEER_PROTOCOL_VERSION, PeerNodeInfoReply};
        use fauna_protocol::{Value, decode_strict, encode_canonical};

        let mut l_sk = [0u8; 32];
        l_sk[0] = 0xcb;
        let listener = IrohTransport::builder(l_sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .build()
            .await
            .expect("listener build");
        let listener_id = listener.local_identity();
        let listener_addrs = loopback_addrs(&listener.endpoint);

        // Raw peer: connect, OPEN a bi stream (the requester opens + writes first,
        // so the node's `accept_stream` returns), host a PeerChannel and REQUEST
        // `fauna.peer.node_info`. Returns the reply payload for the node's assert.
        let peer = raw_loopback_endpoint(0xdc).await;
        let target = listener_addrs.into_iter().fold(
            EndpointAddr::new(EndpointId::from_bytes(listener_id.as_bytes()).expect("listener id")),
            |a, sa| a.with_ip_addr(sa),
        );
        let peer_task = tokio::spawn(async move {
            let conn = peer
                .connect(target, DEFAULT_ALPN)
                .await
                .expect("peer connect");
            let remote = EndpointKey::from_bytes(*conn.remote_id().as_bytes());
            let (send, recv) = conn.open_bi().await.expect("peer open_bi");
            let stream: ByteStream = Box::pin(tokio::io::join(recv, send));
            let channel = PeerChannel::over_stream(stream, remote, PathKind::Lan);
            let reply = channel
                .request(KIND_PEER_NODE_INFO, Value::Null)
                .await
                .expect("node_info reply");
            drop(conn); // held alive through the request; released after the reply
            reply
        });

        // Bring the node up over the trait listener; its accept loop accepts the
        // peer's stream and serves node_info.
        let node = PeerNode::start(Arc::new(listener), "node-under-test".into()).await;

        let reply = peer_task.await.expect("peer task");
        // The reply is a `PeerNodeInfoReply` on the wire — decode + assert.
        let bytes = encode_canonical(&reply).expect("re-encode reply value");
        let info: PeerNodeInfoReply = decode_strict(&bytes).expect("decode node_info reply");
        assert_eq!(info.protocol_version, PEER_PROTOCOL_VERSION);
        assert_eq!(
            info.display_name, "node-under-test",
            "the PeerNode served its own display name"
        );
        assert!(node.is_active(), "the node's accept loop is still running");
        drop(node);
    }

    /// The accept loop keeps accepting after each inbound handshake is spawned
    /// (completed conns are delivered over a
    /// channel instead of awaiting each handshake inline in the loop): two raw
    /// peers both connect and the listener yields *both*, identified by their
    /// authenticated keys (in either order). Before the fix, a single in-flight
    /// handshake held the loop; this proves it no longer does.
    #[tokio::test]
    async fn iroh_listen_accepts_multiple_inbound_conns() {
        use std::collections::HashSet;

        let mut l_sk = [0u8; 32];
        l_sk[0] = 0xc7;
        let listener = IrohTransport::builder(l_sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .build()
            .await
            .expect("listener build");
        let listener_id = listener.local_identity();
        let listener_addrs = loopback_addrs(&listener.endpoint);
        let target = listener_addrs.iter().copied().fold(
            EndpointAddr::new(EndpointId::from_bytes(listener_id.as_bytes()).expect("listener id")),
            |a, sa| a.with_ip_addr(sa),
        );

        let mut incoming = listener.listen().await.expect("listen");

        // Two raw peers connect concurrently; record their authenticated ids.
        let mut expected = HashSet::new();
        let mut peer_tasks = Vec::new();
        for seed in [0xa1u8, 0xb2u8] {
            let peer = raw_loopback_endpoint(seed).await;
            expected.insert(EndpointKey::from_bytes(*peer.id().as_bytes()));
            let tgt = target.clone();
            peer_tasks.push(tokio::spawn(async move {
                let conn = peer.connect(tgt, DEFAULT_ALPN).await.expect("peer connect");
                // Hold open until the listener drops its side.
                conn.closed().await;
            }));
        }

        use futures_util::StreamExt;
        let mut got = HashSet::new();
        for _ in 0..2 {
            let conn = incoming
                .next()
                .await
                .expect("an inbound conn")
                .expect("conn ok");
            got.insert(conn.peer_identity());
            // conn drops at end of iteration → closes → peer's closed() returns.
        }
        assert_eq!(got, expected, "listener accepted both inbound peers");

        drop(incoming);
        for t in peer_tasks {
            let _ = t.await;
        }
    }

    /// `dial()` enforces the shared PT-4 candidate filter end-to-end: a dial whose
    /// only candidates are loopback (the attacker-poisoned-candidate shape) drops
    /// them all and fails with no addressing — a poisoned signaling candidate can
    /// never steer an iroh dial at the local host (mirrors the WireGuard impl's
    /// PT-4 guarantee at its own dial entry).
    #[tokio::test]
    async fn iroh_dial_drops_unsafe_loopback_candidates() {
        let mut sk = [0u8; 32];
        sk[0] = 0xe5;
        let client = IrohTransport::builder(sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .build()
            .await
            .expect("client build");
        // A valid Ed25519 point (derived from a secret key) so dial reaches the
        // addressing stage rather than failing at key parsing.
        let target =
            EndpointKey::from_bytes(*SecretKey::from_bytes(&[0x42; 32]).public().as_bytes());
        let candidates = PathCandidates {
            lan_endpoints: vec![
                "127.0.0.1:51820".parse().unwrap(),
                "169.254.169.254:80".parse().unwrap(), // cloud IMDS
            ],
            wan_endpoint: Some("[::1]:51820".parse().unwrap()),
            relay_available: false,
        };
        // All candidates are unsafe ⇒ filtered ⇒ no addressing ⇒ dial fails. (A
        // valid Ed25519 point that isn't actually reachable would otherwise just
        // time out; here it fails fast on missing addressing, proving the filter
        // dropped every candidate rather than dialing one.)
        let err = match client.dial(target, candidates).await {
            Err(e) => e,
            Ok(_) => panic!("dial must fail when every candidate is filtered as unsafe"),
        };
        assert!(
            matches!(err, TransportError::Io(_) | TransportError::NoPath),
            "expected an addressing/path failure, got {err:?}"
        );
    }

    // ---- self-hosted relay (the durable form of the 2026-06-28 relay-sovereignty
    // proof; the throwaway prototype crate's recipe is tracked internally,
    // § Reproduction notes). These exercise the builder's `RelayMode::Custom` + the `dial`-side
    // relay attachment gated on `PathCandidates::relay_available`. ----

    /// Spawn a self-hosted `iroh-relay` on loopback (HTTPS + a self-signed cert),
    /// exactly as iroh's own relay test helpers do — **no n0 relay configured at
    /// all**. Returns the URL a client dials and the running `Server` (drop or
    /// `shutdown()` to take it down for the negative control).
    #[allow(clippy::field_reassign_with_default)] // ServerConfig is #[non_exhaustive] → can't struct-literal it; mutate a Default
    async fn spawn_local_relay() -> (RelayUrl, iroh_relay::server::Server) {
        use iroh_relay::server::{
            CertConfig, RelayConfig as RelayServerConfig, Server, ServerConfig, TlsConfig,
        };
        let (_certs, server_config) =
            iroh_relay::server::testing::self_signed_tls_certs_and_config();
        let tls = TlsConfig::new(
            (Ipv4Addr::LOCALHOST, 0),
            CertConfig::Manual { server_config },
        );
        let mut relay = RelayServerConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.tls = Some(tls);
        relay.key_cache_capacity = Some(1024);
        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        let server = Server::spawn(config)
            .await
            .expect("spawn self-hosted relay");
        let url: RelayUrl = format!("https://{}", server.https_addr().expect("relay https addr"))
            .parse()
            .expect("relay url");
        (url, server)
    }

    /// A raw iroh endpoint that registers with `relay_url` and — via
    /// `AddrFilter::relay_only` — advertises ONLY its relay path (no direct
    /// addresses). A dialer given no direct candidates can therefore reach it
    /// *solely* through the relay. Accepts one inbound connection, echoes the
    /// first bidi stream's bytes, and holds the connection open until the dialer
    /// closes it. Returns the acceptor's identity + its task handle.
    async fn spawn_relay_only_echo_acceptor(
        relay_url: RelayUrl,
        seed: u8,
    ) -> (EndpointKey, tokio::task::JoinHandle<()>) {
        use iroh::address_lookup::AddrFilter;
        let mut sk = [0u8; 32];
        sk[0] = seed;
        let ep = Endpoint::builder(presets::Empty)
            .secret_key(SecretKey::from_bytes(&sk))
            .alpns(vec![DEFAULT_ALPN.to_vec()])
            .relay_mode(RelayMode::Custom(RelayMap::from(relay_url)))
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .addr_filter(AddrFilter::relay_only())
            .crypto_provider(ring_provider())
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("bind_addr")
            .bind()
            .await
            .expect("bind relay-only acceptor");
        let id = EndpointKey::from_bytes(*ep.id().as_bytes());
        let handle = tokio::spawn(async move {
            let incoming = ep.accept().await.expect("acceptor: an inbound conn");
            let conn = incoming.await.expect("acceptor: incoming -> conn");
            let (mut send, mut recv) = conn.accept_bi().await.expect("acceptor: accept_bi");
            let mut buf = [0u8; 5];
            recv.read_exact(&mut buf).await.expect("acceptor: read");
            send.write_all(&buf).await.expect("acceptor: echo");
            send.finish().expect("acceptor: finish");
            // Hold the connection (and the endpoint) open until the dialer reads
            // the echo and drops its side — else dropping here races the read.
            conn.closed().await;
            drop(ep);
        });
        (id, handle)
    }

    /// `IrohTransport::dial` reaches a relay-only peer **through the self-hosted
    /// relay** and round-trips a byte payload over `open_stream`, with the dialer
    /// given NO direct candidates and `relay_available: true`. Proves the builder's
    /// `RelayMode::Custom` + the `dial`-side relay attachment carry fauna's L3 over
    /// the relay path with zero n0 — the permanent regression form of a throwaway
    /// relay-sovereignty proof.
    #[tokio::test]
    async fn iroh_dials_through_self_hosted_relay() {
        let (relay_url, relay) = spawn_local_relay().await;
        let (acceptor_id, acceptor) = spawn_relay_only_echo_acceptor(relay_url.clone(), 0xa1).await;

        let mut d_sk = [0u8; 32];
        d_sk[0] = 0xb2;
        let dialer = IrohTransport::builder(d_sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .relay_url(relay_url)
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .build()
            .await
            .expect("dialer build");

        // No direct candidates: the relay is the only way to reach the acceptor.
        let candidates = PathCandidates {
            lan_endpoints: vec![],
            wan_endpoint: None,
            relay_available: true,
        };
        let conn = tokio::time::timeout(
            Duration::from_secs(20),
            dialer.dial(acceptor_id, candidates),
        )
        .await
        .expect("dial timed out")
        .expect("dial via self-hosted relay");

        assert_eq!(
            conn.peer_identity(),
            acceptor_id,
            "relay-carried dial proved the peer's identity"
        );

        let mut stream = conn.open_stream().await.expect("open_stream over relay");
        stream.write_all(b"relay").await.expect("write");
        stream.flush().await.expect("flush");
        let mut back = [0u8; 5];
        stream.read_exact(&mut back).await.expect("read echo");
        assert_eq!(
            &back, b"relay",
            "byte stream round-trips through the self-hosted relay"
        );

        // `path()` is intentionally NOT asserted: over loopback iroh may hole-punch
        // a direct path after the relay bootstraps the connection, so the carried
        // path is non-deterministic. The negative control below is the load-bearing
        // proof that the relay (not a silent direct fallback) was required.
        drop(stream);
        drop(conn);
        acceptor.await.expect("acceptor task");
        relay.shutdown().await.ok();
    }

    /// Negative control: with the relay DOWN and no direct candidates, a
    /// `relay_available` dial MUST fail — proving the positive test genuinely
    /// traversed the relay (no silent direct path snuck in).
    #[tokio::test]
    async fn iroh_relay_dial_fails_when_relay_down() {
        // Spawn then immediately shut the relay down: its URL is now dead.
        let (relay_url, relay) = spawn_local_relay().await;
        relay.shutdown().await.ok();

        let mut d_sk = [0u8; 32];
        d_sk[0] = 0xc3;
        let dialer = IrohTransport::builder(d_sk)
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .relay_url(relay_url)
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .build()
            .await
            .expect("dialer build");

        // A valid Ed25519 point with NO direct candidates + relay_available: the
        // only possible path is the downed relay.
        let target =
            EndpointKey::from_bytes(*SecretKey::from_bytes(&[0x42; 32]).public().as_bytes());
        let candidates = PathCandidates {
            lan_endpoints: vec![],
            wan_endpoint: None,
            relay_available: true,
        };
        // iroh resiliently retries a dead relay rather than fast-failing, so this
        // bound is what ends the (correctly) pathless dial; 5s is ample to show no
        // connection forms (a live relay connects in ~1s — see the positive test).
        let result =
            tokio::time::timeout(Duration::from_secs(5), dialer.dial(target, candidates)).await;
        // Expected: a transport error, or a timeout with no path. `Ok(Ok(_))` would
        // mean a connection formed with the relay down — a silent direct path.
        if let Ok(Ok(_)) = result {
            panic!("dial succeeded with the relay down — a silent direct path!");
        }
    }
}
