//! WebSocket client for connecting to external Nostr relays — and the SSRF
//! seat every such dial passes through.
//!
//! A relay URL is **caller-supplied** on every path that reaches
//! [`RelayClient::connect`]: the user's publish relay list, a pasted
//! `bunker://…?relay=` parameter, a follow's relay hints, a paired nest's
//! serving box. Dialing it raw would let any authenticated user of a shared
//! nest point the box at its own loopback, its private network or cloud
//! metadata (`169.254.169.254`) — the confused-deputy SSRF the nest's
//! `ssrf.rs` guard closes for every other caller-supplied URL. So
//! `connect` **requires** a [`RelayDialPolicy`], resolves the target through
//! the shared [`fauna_core::resolve::resolve_permitted_addrs`] under that
//! policy, and connects the TCP stream to the verified addresses itself before
//! the WebSocket handshake — a refusal happens before any socket is opened, and
//! the handshake can never re-resolve to an address the check did not see.
//! The rule, and the ruling that a private-network relay on a user's own list
//! is refused too, is `nest/network-exposure.md` § Rulings F7.

use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, Result};
use fauna_core::resolve::{AddrGuardError, is_global_ip, resolve_permitted_addrs};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::nip01::{ClientMessage, RelayMessage};
use crate::nip11::MAX_WS_MESSAGE_BYTES;
use crate::types::{Event, Filter};

/// Where an outbound relay dial may land — the policy [`RelayClient::connect`]
/// enforces over every address the relay's host resolves to.
///
/// Production is [`Self::PublicOnly`]: the same `is_global_ip` refusal the
/// nest's SSRF guard applies to every other caller-supplied URL — never
/// loopback, private, link-local, ULA, CGNAT or cloud-metadata, and no
/// documented private-network allowance for a relay a user lists (F7).
/// [`Self::PublicOrLoopback`] exists for test harnesses whose peer relays sit
/// on `127.0.0.1` (an in-process paired nest, a loopback mock relay); it never
/// widens past loopback, so a private range or IMDS is refused under either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayDialPolicy {
    /// Globally-routable addresses only — the production posture.
    PublicOnly,
    /// Globally-routable addresses plus loopback — test harnesses only.
    PublicOrLoopback,
}

impl RelayDialPolicy {
    /// May a relay dial land on `ip` under this policy?
    pub fn permits(self, ip: IpAddr) -> bool {
        is_global_ip(ip) || (self == Self::PublicOrLoopback && ip.is_loopback())
    }
}

/// Why a relay URL was refused before any socket was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RelayDialError {
    #[error("invalid relay URL")]
    InvalidUrl,
    #[error("relay URL must be ws:// or wss://")]
    BadScheme,
    #[error("relay URL has no host")]
    NoHost,
    #[error("could not resolve relay host")]
    Resolve,
    #[error("relay address is not permitted (loopback, private, link-local or cloud-metadata)")]
    NotPermitted,
}

/// A relay URL resolved and verified under a [`RelayDialPolicy`]: the parsed
/// URL and the addresses the dial may connect to — and pins.
pub struct RelayTarget {
    pub url: url::Url,
    pub addrs: Vec<SocketAddr>,
}

/// Parse `url`, resolve its host and verify **every** address under `policy`,
/// with no socket opened. Public so a caller that *stores* a relay URL now and
/// dials it later can refuse what is determinably undialable up front; the
/// dial itself always re-runs it.
pub async fn resolve_relay_target(
    url: &str,
    policy: RelayDialPolicy,
) -> Result<RelayTarget, RelayDialError> {
    let parsed = url::Url::parse(url).map_err(|_| RelayDialError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "ws" | "wss") {
        return Err(RelayDialError::BadScheme);
    }
    let host = parsed.host_str().ok_or(RelayDialError::NoHost)?;
    let port = parsed
        .port_or_known_default()
        .ok_or(RelayDialError::InvalidUrl)?;
    let addrs = resolve_permitted_addrs(host, port, |ip| policy.permits(ip))
        .await
        .map_err(|e| match e {
            AddrGuardError::Resolve => RelayDialError::Resolve,
            AddrGuardError::NotPermitted => RelayDialError::NotPermitted,
        })?;
    Ok(RelayTarget { url: parsed, addrs })
}

/// The most one [`RelayClient::connect`] may take, end to end — resolve, TCP
/// connect and the WebSocket/TLS handshake together. A relay URL is
/// caller-supplied, so a relay that accepts the TCP connection and then never
/// answers is one any user of a shared nest can list; without a bound the dial
/// never returns and whoever awaits it — the nest's single Nostr sync worker —
/// stalls with it. The same 10 s [`RelayClient::publish`] waits for an `OK`.
/// A Rust constant, not a knob (`nest/network-exposure.md` § Rulings F8).
pub const RELAY_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The most one [`RelayClient::send`] may wait for the relay to take a frame
/// off the socket. A relay that completes the handshake and then
/// stops reading fills the socket buffers within a few dozen frames, after
/// which an unbounded write awaits the peer for ever — and every write the
/// nest makes (`subscribe`, `close_subscription`, `publish`) goes through
/// `send`. The same 10 s as [`RELAY_CONNECT_TIMEOUT`]: an honest relay drains
/// a frame of at most [`MAX_WS_MESSAGE_BYTES`] far sooner. A Rust constant,
/// not a knob (`nest/network-exposure.md` § Rulings F8).
pub const RELAY_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A WebSocket client for a single external Nostr relay.
pub struct RelayClient {
    url: String,
    ws_tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tungstenite::Message,
    >,
    ws_rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
}

impl RelayClient {
    /// Connect to a Nostr relay WebSocket endpoint. Every relay dial goes
    /// through here, and the relay is one the user chose (or a hostile one on
    /// their list), so two guards sit in front of the handshake: the target is
    /// resolved and verified under `policy` and the TCP stream connected to
    /// those addresses only — a refused relay costs no socket — and
    /// inbound messages and frames are capped at [`MAX_WS_MESSAGE_BYTES`], since
    /// tungstenite's 64 MiB / 16 MiB default would let the relay pin that much
    /// memory per connection. The whole dial is bounded by
    /// [`RELAY_CONNECT_TIMEOUT`], so a relay that accepts and never answers
    /// costs its caller that long and no longer.
    pub async fn connect(url: &str, policy: RelayDialPolicy) -> Result<Self> {
        tokio::time::timeout(RELAY_CONNECT_TIMEOUT, Self::connect_unbounded(url, policy))
            .await
            .map_err(|_| anyhow::anyhow!("connect to relay: timed out"))?
    }

    async fn connect_unbounded(url: &str, policy: RelayDialPolicy) -> Result<Self> {
        let target = resolve_relay_target(url, policy).await?;
        let tcp = tokio::net::TcpStream::connect(&target.addrs[..])
            .await
            .context("connect to relay")?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_WS_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_WS_MESSAGE_BYTES));
        // The handshake rides the stream we pinned; TLS (for `wss`) is
        // negotiated on it with the URL's host as the server name.
        let (ws_stream, _response) = tokio_tungstenite::client_async_tls_with_config(
            target.url.as_str(),
            tcp,
            Some(config),
            None,
        )
        .await
        .context("connect to relay")?;
        let (ws_tx, ws_rx) = ws_stream.split();
        Ok(Self {
            url: url.to_string(),
            ws_tx,
            ws_rx,
        })
    }

    /// The relay URL this client is connected to.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Send a raw client message to the relay, bounded by
    /// [`RELAY_SEND_TIMEOUT`] so a relay that stops reading costs its caller
    /// that long and no longer. A timed-out send may leave a frame
    /// half-written, so the caller drops the connection on any error.
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        let json = msg.to_json();
        tokio::time::timeout(
            RELAY_SEND_TIMEOUT,
            self.ws_tx.send(tungstenite::Message::Text(json.into())),
        )
        .await
        .map_err(|_| anyhow::anyhow!("send to relay: timed out"))?
        .context("send to relay")?;
        Ok(())
    }

    /// Receive the next relay message. Returns None if the connection is closed.
    pub async fn recv(&mut self) -> Result<Option<RelayMessage>> {
        loop {
            match self.ws_rx.next().await {
                Some(Ok(tungstenite::Message::Text(text))) => {
                    match RelayMessage::from_json(&text) {
                        Ok(msg) => return Ok(Some(msg)),
                        Err(e) => {
                            tracing::debug!("relay {}: unparseable message: {e}", self.url);
                            continue;
                        }
                    }
                }
                Some(Ok(tungstenite::Message::Close(_))) | None => return Ok(None),
                Some(Ok(_)) => continue, // ping/pong/binary
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }

    /// Subscribe to events matching the given filters.
    pub async fn subscribe(&mut self, sub_id: &str, filters: Vec<Filter>) -> Result<()> {
        let msg = ClientMessage::Req {
            subscription_id: sub_id.to_string(),
            filters,
        };
        self.send(&msg).await
    }

    /// Publish an event to the relay. Returns the OK response status.
    pub async fn publish(&mut self, event: Event) -> Result<bool> {
        let msg = ClientMessage::Event(event);
        self.send(&msg).await?;

        // Wait for OK response (with timeout)
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(10), self.recv()).await;
        match timeout {
            Ok(Ok(Some(RelayMessage::Ok { accepted, .. }))) => Ok(accepted),
            Ok(Ok(Some(_))) => Ok(false), // unexpected message type
            Ok(Ok(None)) => Err(anyhow::anyhow!("connection closed")),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow::anyhow!("publish timeout")),
        }
    }

    /// Close a subscription.
    pub async fn close_subscription(&mut self, sub_id: &str) -> Result<()> {
        let msg = ClientMessage::Close(sub_id.to_string());
        self.send(&msg).await
    }
}

/// Test-side helpers for the dial guard's pins — a loopback listener that
/// reports whether anything ever connected. Shared with the nest's per-source
/// pins (`fauna-nest`'s `test-helpers` dev-dependency re-declaration).
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support {
    use std::time::Duration;

    /// A loopback TCP listener plus the `ws://` URL that names it.
    pub struct ProbeListener {
        listener: tokio::net::TcpListener,
        pub url: String,
    }

    impl ProbeListener {
        pub async fn bind() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            Self { listener, url }
        }

        /// Did any TCP connection arrive within `window`? A refused dial must
        /// leave this `false` — the guard's observable is "no socket opened",
        /// not an error string.
        pub async fn saw_a_connection_within(&self, window: Duration) -> bool {
            tokio::time::timeout(window, self.listener.accept())
                .await
                .is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::ProbeListener;
    use super::*;
    use std::time::Duration;

    /// Serve one WebSocket connection on loopback that sends `payload` as a
    /// single text message; return the `ws://` URL to dial.
    async fn one_shot_relay(payload: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _ = ws.send(tungstenite::Message::Text(payload.into())).await;
            // Hold the socket open so the client sees the message, not a close.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        format!("ws://{addr}")
    }

    /// An `["EOSE", "<sub id>"]` message whose total length is exactly `len`.
    fn eose_of_len(len: usize) -> String {
        let overhead = r#"["EOSE",""]"#.len();
        format!(r#"["EOSE","{}"]"#, "a".repeat(len - overhead))
    }

    fn dial_error(e: &anyhow::Error) -> Option<RelayDialError> {
        e.downcast_ref::<RelayDialError>().copied()
    }

    /// A relay the user chose — or a hostile one on their list — must
    /// not make the nest buffer a message past the shared transport cap:
    /// tungstenite's 64 MiB default is refused, not held in memory.
    #[tokio::test]
    async fn relay_message_over_the_cap_is_refused() {
        let url = one_shot_relay(eose_of_len(MAX_WS_MESSAGE_BYTES + 1)).await;
        let mut client = RelayClient::connect(&url, RelayDialPolicy::PublicOrLoopback)
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), client.recv())
            .await
            .expect("recv must not hang");
        assert!(
            got.is_err(),
            "over-cap relay message must be refused, got a message: {}",
            matches!(got, Ok(Some(_)))
        );
    }

    /// The cap never clips an honest relay: a message at exactly the cap parses.
    #[tokio::test]
    async fn relay_message_at_the_cap_is_accepted() {
        let url = one_shot_relay(eose_of_len(MAX_WS_MESSAGE_BYTES)).await;
        let mut client = RelayClient::connect(&url, RelayDialPolicy::PublicOrLoopback)
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), client.recv())
            .await
            .expect("recv must not hang")
            .expect("at-cap message is accepted");
        assert!(matches!(got, Some(RelayMessage::Eose(_))), "got {got:?}");
    }

    /// Under the production policy a loopback relay is refused **before any
    /// TCP connect**: the listener behind the URL never sees a
    /// connection, and the error is the guard's own, not a handshake failure.
    #[tokio::test]
    async fn a_loopback_relay_is_refused_before_any_tcp_connect() {
        let probe = ProbeListener::bind().await;
        let err = RelayClient::connect(&probe.url, RelayDialPolicy::PublicOnly)
            .await
            .err()
            .expect("loopback must be refused under PublicOnly");
        assert_eq!(
            dial_error(&err),
            Some(RelayDialError::NotPermitted),
            "{err:#}"
        );
        assert!(
            !probe
                .saw_a_connection_within(Duration::from_millis(300))
                .await,
            "the guard must refuse before opening a socket"
        );
    }

    /// The test-only loopback allowance is exactly loopback: a private range,
    /// a ULA and cloud metadata are refused under **either** policy, with no
    /// resolver consulted (they are literals) and no socket opened.
    #[tokio::test]
    async fn private_and_metadata_relays_are_refused_under_both_policies() {
        for url in [
            "ws://10.0.0.7/",
            "wss://192.168.1.10:7777/",
            "ws://169.254.169.254/latest/meta-data/",
            "ws://100.64.0.1/",
            "ws://[fd00::1]/",
            "ws://[fe80::1]/",
        ] {
            for policy in [
                RelayDialPolicy::PublicOnly,
                RelayDialPolicy::PublicOrLoopback,
            ] {
                let got = resolve_relay_target(url, policy).await.err();
                assert_eq!(
                    got,
                    Some(RelayDialError::NotPermitted),
                    "{url} under {policy:?}"
                );
            }
        }
        // Encoded-decimal literal: the URL parser normalizes it to 127.0.0.1,
        // which only the loopback allowance may pass.
        assert_eq!(
            resolve_relay_target("ws://2130706433/", RelayDialPolicy::PublicOnly)
                .await
                .err(),
            Some(RelayDialError::NotPermitted)
        );
        let t = resolve_relay_target("ws://2130706433/", RelayDialPolicy::PublicOrLoopback)
            .await
            .unwrap();
        assert_eq!(t.addrs, vec!["127.0.0.1:80".parse::<SocketAddr>().unwrap()]);
    }

    /// The text-only half refuses what no resolver is needed to judge, and
    /// refuses nothing a resolver would be needed to judge.
    #[tokio::test]
    async fn the_url_shape_is_judged_before_any_lookup() {
        assert_eq!(
            resolve_relay_target("not a url", RelayDialPolicy::PublicOnly)
                .await
                .err(),
            Some(RelayDialError::InvalidUrl)
        );
        assert_eq!(
            resolve_relay_target("https://relay.example.com/", RelayDialPolicy::PublicOnly)
                .await
                .err(),
            Some(RelayDialError::BadScheme)
        );
        // A public literal needs no DNS and is dialable under both.
        let t = resolve_relay_target("wss://8.8.8.8/", RelayDialPolicy::PublicOnly)
            .await
            .unwrap();
        assert_eq!(t.addrs, vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap()]);
        // `localhost` is a name; the resolver answers loopback, which the
        // production policy refuses and the test allowance admits.
        assert_eq!(
            resolve_relay_target("ws://localhost:1/", RelayDialPolicy::PublicOnly)
                .await
                .err(),
            Some(RelayDialError::NotPermitted)
        );
        assert!(
            resolve_relay_target("ws://localhost:1/", RelayDialPolicy::PublicOrLoopback)
                .await
                .is_ok()
        );
    }

    /// A relay that accepts the TCP connection and then never answers the
    /// upgrade must not hold the dialer: `connect` returns an error
    /// within [`RELAY_CONNECT_TIMEOUT`]. The probe listener never calls
    /// `accept`, so the kernel completes the TCP handshake from its backlog and
    /// the WebSocket upgrade is met with silence — the tarpit shape that
    /// stalled the nest's Nostr sync worker.
    #[tokio::test]
    async fn a_silent_relay_is_given_up_on_within_the_connect_bound() {
        let probe = ProbeListener::bind().await;
        let started = std::time::Instant::now();
        let got = tokio::time::timeout(
            RELAY_CONNECT_TIMEOUT + Duration::from_secs(5),
            RelayClient::connect(&probe.url, RelayDialPolicy::PublicOrLoopback),
        )
        .await
        .expect("connect to a silent relay must not outlive its bound");
        assert!(got.is_err(), "a silent relay must not yield a client");
        assert!(
            started.elapsed() >= RELAY_CONNECT_TIMEOUT - Duration::from_millis(500),
            "the bound is the constant, not an earlier failure: {:?}",
            started.elapsed()
        );
    }

    /// The finding's effect is **blind**: a relay URL that answers
    /// the upgrade with a plain HTTP reply fails the handshake, and nothing of
    /// that reply's body reaches the dialer's error — so even before the guard,
    /// an internal endpoint's response could not be read through a relay dial.
    /// Pinned under the loopback allowance so the dial actually reaches the
    /// listener.
    #[tokio::test]
    async fn a_non_websocket_reply_body_never_reaches_the_dialer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req).await;
            let body = "a metadata body that must stay unseen";
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        });
        let err = RelayClient::connect(&format!("ws://{addr}/"), RelayDialPolicy::PublicOrLoopback)
            .await
            .err()
            .expect("a non-101 reply fails the handshake");
        let text = format!("{err:#} {err:?}");
        assert!(
            !text.contains("a metadata body that must stay unseen"),
            "the reply body must never surface through the dial error: {text}"
        );
    }

    /// A loopback relay with a 4 KiB receive buffer that completes the
    /// WebSocket handshake and then never reads another byte; return the
    /// `ws://` URL to dial.
    async fn deaf_relay() -> String {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(1).unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let _ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            std::future::pending::<()>().await;
        });
        format!("ws://{addr}")
    }

    /// A relay that stops reading must not hold the sender: once
    /// the socket buffers fill, `send` — and so `subscribe`,
    /// `close_subscription` and `publish`'s write — returns an error within
    /// [`RELAY_SEND_TIMEOUT`] instead of awaiting the peer for ever. Each `REQ`
    /// is about 33 KB, so the buffers fill within a few dozen sends; the outer
    /// bound is what a hang trips.
    #[tokio::test]
    async fn a_relay_that_stops_reading_is_given_up_on_within_the_send_bound() {
        let url = deaf_relay().await;
        let mut client = RelayClient::connect(&url, RelayDialPolicy::PublicOrLoopback)
            .await
            .unwrap();
        let authors: Vec<String> = (0..500).map(|i| format!("{i:064x}")).collect();
        for _ in 0..2000 {
            let filter = Filter {
                authors: Some(authors.clone()),
                ..Default::default()
            };
            let sent = tokio::time::timeout(
                RELAY_SEND_TIMEOUT + Duration::from_secs(5),
                client.subscribe("deaf", vec![filter]),
            )
            .await
            .expect("a send to a relay that stopped reading must not outlive its bound");
            if sent.is_err() {
                return;
            }
        }
        panic!("the relay's buffers never filled — the fixture is not deaf");
    }
}
