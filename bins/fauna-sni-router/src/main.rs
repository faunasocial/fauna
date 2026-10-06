//! `fauna-sni-router` — an L4 SNI-passthrough router.
//!
//! Listens on one address (`:443` in the single-box deployment) and forwards
//! each TLS connection, **without terminating TLS**, to a backend chosen by the
//! ClientHello's SNI host_name. Unmatched SNI (including no-SNI) goes to the
//! default backend.
//!
//! Why it exists: on a single-IP box, nest (`<domain>`) and the mail-bridge MDA
//! CalDAV (`mail.<domain>`) both want `:443`. A *terminating* proxy would have
//! to decrypt to route, becoming a process that sees all plaintext. This router
//! terminates nothing — nest and the MDA each terminate their own TLS, so no
//! single process ever sees the union of their plaintext. See
//! `docs/goal/behavior/caldav-server.md` § Network exposure.
//!
//! The SNI is a routing *hint*, not a trust decision: the backend still does
//! full TLS + auth, so a mis-route just fails that backend's handshake.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

mod sni;

/// TLS records cap at 16 KiB of payload; the ClientHello fits in the first one.
const MAX_CLIENT_HELLO: usize = 16 * 1024 + 5;
/// How long to wait for the ClientHello before giving up on a connection.
const PEEK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(name = "fauna-sni-router", version)]
struct Cli {
    /// Address to listen on, e.g. `0.0.0.0:443`.
    #[arg(long, default_value = "0.0.0.0:443")]
    listen: String,

    /// Backend for unmatched / no-SNI connections, e.g. `127.0.0.1:3000` (nest).
    #[arg(long)]
    default: String,

    /// SNI route as `host=addr`, repeatable. Exact (case-insensitive) host
    /// match, e.g. `--route mail.example.com=127.0.0.1:8444`. A `<label>.*` host
    /// is a domain-agnostic leading-label route, e.g. `--route mail.*=127.0.0.1:8444`
    /// matches `mail.<any-domain>` — used by the deploy so `mail.<domain>` reaches
    /// the MDA CalDAV listener without the container knowing the domain at boot (it
    /// is learned at claim). Exact routes win over label routes.
    #[arg(long = "route", value_name = "HOST=ADDR")]
    routes: Vec<String>,

    /// Backend address (matching `--default` or a `--route` target) to which a
    /// PROXY-protocol-v2 header conveying the real client address is prepended,
    /// repeatable. The backend MUST understand PROXY v2: nest does on its TLS
    /// accept path, and the MDA CalDAV listener (`:8444`) now peels it below
    /// its TLS terminator (`internal/proxyproto`), so both are listed in the
    /// deploy run-script. Sending a header to a backend that does NOT parse it
    /// would corrupt that backend's TLS handshake. Without this the backend
    /// sees the router's loopback address as the peer, which defeats per-source
    /// rate limiting and nest's loopback-only `request_enrollment` trust gate.
    #[arg(long = "send-proxy-to", value_name = "ADDR")]
    send_proxy_to: Vec<String>,

    /// Maximum concurrent spliced connections — an FD / memory backstop against
    /// a connection flood (each connection holds two sockets + a task; security
    /// review 2026-06-01 § D2). When the cap is hit the new connection is shed
    /// (closed) rather than blocking the accept loop, so existing connections
    /// keep flowing. A spliced connection whose peer vanishes without a clean
    /// close is bounded by the TCP keepalive this router arms on every accepted
    /// socket (`fauna_conn_limit::arm_dead_peer_detection`) — NOT by the
    /// backend's timeouts, which only cover a connection's pre-upgrade phase.
    /// That mistaken assumption is what took example.com's `:443` down on
    /// 2026-07-31; see the `fauna-conn-limit` crate docs.
    #[arg(long = "max-connections", default_value_t = 4096)]
    max_connections: usize,

    /// Maximum concurrent spliced connections from any single source IP — the
    /// per-source layer on top of `--max-connections`, so one abusive source
    /// can't fill the whole pool. The router is the front-most hop, so it observes the real
    /// client IP as the TCP peer directly (no PROXY header to resolve).
    /// A loopback peer is counted against the shared crate's hard-coded
    /// `LOOPBACK_MAX_CONNS` (1024) instead of this cap — bounded, not exempt. Same
    /// shed-on-cap behaviour as `--max-connections`.
    #[arg(long = "max-connections-per-ip", default_value_t = fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP)]
    max_connections_per_ip: usize,
}

struct Router {
    /// Exact (case-insensitive) SNI host → backend.
    routes: HashMap<String, String>,
    /// Leading-DNS-label → backend, for domain-agnostic routing. A `--route`
    /// whose host is `<label>.*` (e.g. `mail.*`) lands here keyed by `<label>`
    /// and matches ANY SNI whose first label is `<label>` (`mail.example.com`,
    /// `mail.example.org`, …). This is what lets the deploy route `mail.<domain>`
    /// → the MDA CalDAV listener and `relay.<domain>` → the relay **without
    /// knowing the domain at container start** — a domainless-booted box learns
    /// its domain only at claim, so the boot-time run-script can't name it
    /// (domains-and-tls-bootstrap.md § Claim). Exact `routes` win over label
    /// routes; both fall through to `default`.
    label_routes: HashMap<String, String>,
    default: String,
    /// Backends that should receive a PROXY-v2 header (see `--send-proxy-to`).
    send_proxy_to: HashSet<String>,
    /// Secret appended as a PROXY-v2 router-auth TLV so nest can prove the
    /// header came from this router and not a co-resident bridge UID forging one
    /// to spoof a source IP (`security.md` § Co-resident process trust
    /// boundary). `None` when unset (`FAUNA_ROUTER_PROXY_SECRET` empty), in which
    /// case a plain header is sent (the non-router / unprovisioned posture; nest
    /// then trusts any loopback peer). The MDA CalDAV backend ignores the
    /// TLV (it skips unknown TLVs), so emitting it on every header is safe.
    proxy_auth: Option<Vec<u8>>,
}

impl Router {
    fn backend_for(&self, sni: Option<&str>) -> &str {
        let Some(host) = sni else {
            return &self.default;
        };
        // Case-insensitive (routes are stored lowercased — the documented
        // contract; the old raw lookup was a latent case bug).
        let host = host.to_ascii_lowercase();
        if let Some(addr) = self.routes.get(&host) {
            return addr;
        }
        // Fall back to a leading-label match (`mail.*` / `relay.*`) so the route
        // is domain-agnostic (a domainless box's real domain isn't known at boot).
        if let Some((label, _rest)) = host.split_once('.')
            && let Some(addr) = self.label_routes.get(label)
        {
            return addr;
        }
        &self.default
    }

    fn should_send_proxy(&self, backend: &str) -> bool {
        self.send_proxy_to.contains(backend)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let mut routes = HashMap::new();
    let mut label_routes = HashMap::new();
    for r in &cli.routes {
        let (host, addr) = r
            .split_once('=')
            .with_context(|| format!("--route must be HOST=ADDR, got {r:?}"))?;
        if host.is_empty() || addr.is_empty() {
            bail!("--route HOST and ADDR must both be non-empty, got {r:?}");
        }
        let host = host.to_ascii_lowercase();
        // A `<label>.*` host is a domain-agnostic leading-label route (e.g.
        // `mail.*` → the MDA), matching any SNI whose first DNS label is `<label>`.
        if let Some(label) = host.strip_suffix(".*") {
            if label.is_empty() || label.contains('.') {
                bail!(
                    "--route label wildcard must be a single leading label `<label>.*`, got {r:?}"
                );
            }
            label_routes.insert(label.to_string(), addr.to_string());
        } else {
            routes.insert(host, addr.to_string());
        }
    }
    let send_proxy_to: HashSet<String> = cli.send_proxy_to.iter().cloned().collect();
    // The router-auth secret nest expects in a PROXY-v2 header. Supplied
    // as hex via the `FAUNA_ROUTER_PROXY_SECRET` IPC env (the run-script reads
    // `/data/keys/router/proxy-secret` as root and env-passes it — via env, not
    // argv, so it stays out of the world-readable `/proc/<pid>/cmdline`). Empty
    // ⇒ no auth TLV (the non-router / unprovisioned posture, trust-any-loopback); malformed ⇒ hard error (a router-fronted
    // box must not silently send unauthenticated headers).
    let proxy_auth = match std::env::var("FAUNA_ROUTER_PROXY_SECRET") {
        Ok(hex) if !hex.trim().is_empty() => {
            let bytes =
                decode_hex(hex.trim()).context("FAUNA_ROUTER_PROXY_SECRET must be valid hex")?;
            Some(bytes)
        }
        _ => None,
    };
    let router = Arc::new(Router {
        routes,
        label_routes,
        default: cli.default.clone(),
        send_proxy_to,
        proxy_auth,
    });
    let conn_limit = Arc::new(Semaphore::new(cli.max_connections));
    let per_ip_limit = fauna_conn_limit::PerIpConnLimit::new(cli.max_connections_per_ip);
    // Shedding is the symptom an admin needs to see; logging it per-rejection
    // would flood at connection-attempt rate, so both caps report through a
    // rate-limited counter carrying the batch count.
    let global_shed = fauna_conn_limit::ShedCounter::new();
    let per_ip_shed = fauna_conn_limit::ShedCounter::new();

    let listener = TcpListener::bind(&cli.listen)
        .await
        .with_context(|| format!("binding {}", cli.listen))?;
    info!(
        listen = %cli.listen,
        default = %router.default,
        routes = ?router.routes,
        label_routes = ?router.label_routes,
        send_proxy_to = ?router.send_proxy_to,
        proxy_auth = router.proxy_auth.is_some(),
        max_connections = cli.max_connections,
        max_connections_per_ip = cli.max_connections_per_ip,
        "fauna-sni-router listening"
    );

    loop {
        let (client, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "accept failed");
                continue;
            }
        };
        // Shed (close) the new connection when the global concurrency cap is hit
        // rather than blocking the accept loop, so existing splices keep flowing.
        let permit = match Arc::clone(&conn_limit).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                if let Some(shed) = global_shed.record() {
                    // `(sample: peer)`, not an attribution: `global_shed` is one
                    // counter for the whole process, so `peer` is one example of
                    // the batch, never the cause of all of it
                    // (transport-connection.md § Abuse posture).
                    warn!(
                        %peer,
                        max = cli.max_connections,
                        shed_since_last_line = shed,
                        "connection cap reached; shedding (sample: peer)"
                    );
                }
                continue; // `client` dropped here → connection closed
            }
        };
        // Per-source-IP cap: shed if `peer` already holds its ceiling of
        // concurrent connections, so one source can't monopolise the global
        // pool. The peer is the real client (the router is the front-most hop);
        // a loopback peer is counted against its own hard-coded ceiling
        // (`fauna_conn_limit::LOOPBACK_MAX_CONNS`) rather than exempted. The
        // permit decrements when this connection's task ends.
        let ip_permit = match per_ip_limit.try_acquire(peer.ip()) {
            Some(p) => p,
            None => {
                if let Some(shed) = per_ip_shed.record() {
                    // `(sample: peer)` — `per_ip_shed` is ONE counter shared
                    // across every source IP this cap meters (the ceiling is
                    // per-key, the counter is not), so `shed` sums sheds across
                    // all sources, not just this `peer`
                    // (transport-connection.md § Abuse posture).
                    warn!(
                        %peer,
                        max = per_ip_limit.ceiling_for(peer.ip()),
                        loopback = peer.ip().is_loopback(),
                        shed_since_last_line = shed,
                        "per-IP connection cap reached; shedding (sample: peer)"
                    );
                }
                continue; // `client` + `permit` dropped here → connection closed
            }
        };
        // Bound this permit's lifetime: without dead-peer detection a client
        // that disappears without a clean TCP close leaves an `ESTABLISHED`
        // socket forever, permanently burning one of its IP's slots — repeat
        // ~256 times and the source is locked out for good (the 2026-07-31
        // example.com `:443` outage). Non-fatal if it fails: the connection just
        // falls back to the OS default idle.
        if let Err(e) = fauna_conn_limit::arm_dead_peer_detection(&client) {
            warn!(%peer, error = %e, "could not arm TCP keepalive; connection is leak-prone");
        }
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            let _permit = permit; // released when this connection's task ends
            let _ip_permit = ip_permit; // ditto — releases the per-IP slot
            if let Err(e) = handle(client, peer, router).await {
                debug!(%peer, error = %e, "connection closed with error");
            }
        });
    }
}

async fn handle(mut client: TcpStream, peer: SocketAddr, router: Arc<Router>) -> Result<()> {
    // Buffer the ClientHello (and possibly a bit more) without consuming it from
    // the logical stream — we replay every buffered byte to the backend.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    let sni = loop {
        let read = tokio::time::timeout(PEEK_TIMEOUT, client.read(&mut tmp))
            .await
            .context("timed out reading ClientHello")?
            .context("reading ClientHello")?;
        if read == 0 {
            break sni::parse_sni(&buf); // EOF — parse what we have
        }
        buf.extend_from_slice(&tmp[..read]);
        // Once we hold the whole first record, the SNI (if any) is parseable.
        if let Some(n) = sni::first_record_len(&buf)
            && buf.len() >= n
        {
            break sni::parse_sni(&buf);
        }
        if buf.len() >= MAX_CLIENT_HELLO {
            break sni::parse_sni(&buf); // oversized — route on best effort
        }
    };

    let backend_addr = router.backend_for(sni.as_deref()).to_string();
    let send_proxy = router.should_send_proxy(&backend_addr);
    debug!(sni = ?sni, backend = %backend_addr, send_proxy, "routing");

    let mut backend = TcpStream::connect(&backend_addr)
        .await
        .with_context(|| format!("connecting backend {backend_addr}"))?;
    // For PROXY-aware backends (nest), prepend a PROXY-v2 header conveying the
    // real client address *before* the buffered ClientHello, so the backend
    // records the internet client — not this router's loopback address — as the
    // connection source. `dst` is the address the client originally hit (our
    // accepting socket); the backend ignores it but the wire format requires it.
    if send_proxy {
        let dst = client.local_addr().unwrap_or(peer);
        // Attach the router-auth TLV when a secret is configured, so nest
        // can distinguish this router from a co-resident bridge forging a header.
        let header =
            fauna_proxy_protocol::encode_v2_authed(peer, dst, router.proxy_auth.as_deref());
        backend
            .write_all(&header)
            .await
            .context("writing PROXY-v2 header to backend")?;
    }
    // Replay the bytes we peeked, then splice the rest both ways.
    backend
        .write_all(&buf)
        .await
        .context("replaying ClientHello to backend")?;
    copy_bidirectional(&mut client, &mut backend)
        .await
        .context("splicing")?;
    Ok(())
}

/// Decode a hex string to bytes; `None` on any non-hex char or odd length.
/// Inline (no `hex` dep) to honour this binary's deliberately-minimal,
/// dependency-free splicer ethos — the only input is the small artifact-set
/// router-auth secret.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Router, decode_hex};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn decode_hex_roundtrips_and_rejects_bad_input() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(decode_hex("DEADBEEF"), Some(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(decode_hex(""), Some(vec![]));
        assert_eq!(decode_hex("abc"), None, "odd length");
        assert_eq!(decode_hex("zz"), None, "non-hex");
    }

    #[test]
    fn backend_for_exact_then_label_then_default() {
        let mut routes = HashMap::new();
        routes.insert("apex.example.com".to_string(), "127.0.0.1:9000".to_string());
        let mut label_routes = HashMap::new();
        label_routes.insert("mail".to_string(), "127.0.0.1:8444".to_string());
        label_routes.insert("relay".to_string(), "127.0.0.1:8445".to_string());
        let r = Router {
            routes,
            label_routes,
            default: "127.0.0.1:3000".to_string(),
            send_proxy_to: HashSet::new(),
            proxy_auth: None,
        };
        // Label routes are domain-agnostic (the domainless fix): mail.<any>/relay.<any>.
        assert_eq!(r.backend_for(Some("mail.example.com")), "127.0.0.1:8444");
        assert_eq!(r.backend_for(Some("mail.other.org")), "127.0.0.1:8444");
        assert_eq!(
            r.backend_for(Some("MAIL.Example.COM")),
            "127.0.0.1:8444",
            "case-insensitive"
        );
        assert_eq!(r.backend_for(Some("relay.example.com")), "127.0.0.1:8445");
        // Exact route wins over the default.
        assert_eq!(r.backend_for(Some("apex.example.com")), "127.0.0.1:9000");
        // A non-mail/relay subdomain, the apex, and no-SNI all fall to nest.
        assert_eq!(r.backend_for(Some("alice.example.com")), "127.0.0.1:3000");
        assert_eq!(r.backend_for(Some("example.com")), "127.0.0.1:3000");
        assert_eq!(r.backend_for(None), "127.0.0.1:3000");
    }
}
