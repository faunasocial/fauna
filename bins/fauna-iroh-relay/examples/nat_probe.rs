//! NAT path probe — does a pair of devices behind two NATs ever leave the
//! relayed path for a direct one, in our build as shipped? (`behavior/p2p.md`
//! § NAT hole punching.)
//!
//! Every role builds exactly what ships: the relay is [`spawn_relay`] — with
//! `--discovery`, serving address discovery on [`DISCOVERY_PORT`] beside the
//! relay protocol, as the production relay does (`behavior/p2p.md` § The relay →
//! *Address discovery*); without it, the relay protocol only, the shape that
//! shipped before — and each endpoint is the
//! production peer-leg builder (`IrohTransport::builder(secret).relay_url(..)`,
//! bound on `0.0.0.0:0`, the body of `fauna_iroh::peer_leg_transport`) with one
//! test-only difference: it trusts the probe relay's self-signed cert. The
//! dialer hands `dial` no direct candidate and `relay_available: true` — what
//! `compose_facts` publishes today (empty `public_addrs`) leaves it nothing else.
//!
//! Roles (one process each, so a testbed can put them on separate networks):
//!
//! ```text
//! nat_probe relay  <bind-ip> <https-port> <out-dir> [--discovery] <member-seed>...
//! nat_probe listen <seed> <relay-url> <relay-root.der>
//! nat_probe dial   <seed> <relay-url> <relay-root.der> <peer-seed> <secs>
//! nat_probe local  <secs>      # all three in one process, one host, no NAT
//! ```
//!
//! Output is line-oriented for a driver to grade: `PATH t=<secs> kind=<lan|
//! wan_direct|relay>` on every change (and at connect), `FINAL kind=<..>
//! bytes=<n>` at the end of a dial. `lan` and `wan_direct` are both a direct
//! path; only the address class differs (`PeerConn::path`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use fauna_iroh::IrohTransport;
use fauna_iroh_relay::{Admission, DISCOVERY_PORT, RelayServerOptions, spawn_relay};
use fauna_transport::{EndpointKey, PathCandidates, PathKind, PeerConn, PeerTransport};
use futures_util::StreamExt;
use iroh::tls::CaTlsConfig;
use iroh::{RelayUrl, SecretKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The probe's chunk: 16 KiB every 20 ms ≈ 800 KiB/s, a steady sync-shaped stream.
const CHUNK: usize = 16 * 1024;
const CHUNK_EVERY: Duration = Duration::from_millis(20);

/// A fixed member set — the probe relay admits exactly the two probe endpoints
/// (the production rule asks the nest; there is no nest here).
#[derive(Debug)]
struct Members(Vec<[u8; 32]>);

impl Admission for Members {
    fn admits<'a>(
        &'a self,
        endpoint_key: [u8; 32],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { self.0.contains(&endpoint_key) })
    }
}

fn secret(seed: u8) -> [u8; 32] {
    let mut sk = [0u8; 32];
    sk[0] = seed;
    sk
}

fn public(seed: u8) -> [u8; 32] {
    *SecretKey::from_bytes(&secret(seed)).public().as_bytes()
}

fn label(kind: PathKind) -> &'static str {
    kind.label()
}

/// Self-signed relay cert for `ip`, as a rustls server config + the root DER.
fn relay_tls(ip: IpAddr) -> Result<(rustls::ServerConfig, CertificateDer<'static>)> {
    let cert = rcgen::generate_simple_self_signed(vec![ip.to_string()])?;
    let der = cert.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![der.clone()], key)?;
    Ok((config, der))
}

async fn start_relay(
    ip: IpAddr,
    https_port: u16,
    discovery: bool,
    members: Vec<[u8; 32]>,
) -> Result<(
    iroh_relay::server::Server,
    RelayUrl,
    CertificateDer<'static>,
)> {
    let (tls, root) = relay_tls(ip)?;
    let opts = RelayServerOptions {
        https_bind: SocketAddr::new(ip, https_port),
        http_bind: SocketAddr::new(ip, 0),
        key_cache_capacity: Some(1024),
        quic_bind: discovery.then(|| SocketAddr::new(ip, DISCOVERY_PORT)),
    };
    let server = spawn_relay(opts, tls, Arc::new(Members(members))).await?;
    let https = server
        .https_addr()
        .ok_or_else(|| anyhow!("relay has no https addr"))?;
    let url: RelayUrl = format!("https://{https}").parse()?;
    Ok((server, url, root))
}

/// The production peer-leg endpoint, trusting the probe relay's root.
async fn endpoint(
    seed: u8,
    relay: RelayUrl,
    root: CertificateDer<'static>,
) -> Result<IrohTransport> {
    IrohTransport::builder(secret(seed))
        .relay_url(relay)
        .ca_tls_config(CaTlsConfig::custom_roots(vec![root]))
        .build()
        .await
        .map_err(|e| anyhow!("endpoint build: {e}"))
}

/// Accept connections and drain every stream, reporting the path as it changes.
async fn listen(transport: IrohTransport) -> Result<()> {
    let mut incoming = transport
        .listen()
        .await
        .map_err(|e| anyhow!("listen: {e}"))?;
    println!("LISTENING");
    while let Some(conn) = incoming.next().await {
        let conn: Arc<dyn PeerConn> = Arc::from(conn.map_err(|e| anyhow!("accept: {e}"))?);
        tokio::spawn(async move {
            let started = Instant::now();
            let watch = tokio::spawn(watch_path("listener", conn.clone(), started));
            let mut total = 0u64;
            if let Ok(mut stream) = conn.accept_stream().await {
                let mut buf = vec![0u8; CHUNK];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    total += n as u64;
                }
            }
            watch.abort();
            println!("LISTENER_DONE kind={} bytes={total}", label(conn.path()));
        });
    }
    Ok(())
}

/// Print `PATH` once at start and on every change, sampling every 250 ms.
async fn watch_path(who: &'static str, conn: Arc<dyn PeerConn>, started: Instant) {
    let mut last = None;
    loop {
        let kind = conn.path();
        if last != Some(kind) {
            println!(
                "PATH who={who} t={:.1} kind={}",
                started.elapsed().as_secs_f64(),
                label(kind)
            );
            last = Some(kind);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Dial `peer` with no direct candidate, stream for `secs`, report the path.
async fn dial(transport: &IrohTransport, peer: [u8; 32], secs: u64) -> Result<PathKind> {
    let candidates = PathCandidates {
        lan_endpoints: vec![],
        wan_endpoint: None,
        relay_available: true,
    };
    let started = Instant::now();
    let conn = tokio::time::timeout(
        Duration::from_secs(30),
        transport.dial(EndpointKey::from_bytes(peer), candidates),
    )
    .await
    .context("dial timed out")?
    .map_err(|e| anyhow!("dial: {e}"))?;
    let conn: Arc<dyn PeerConn> = Arc::from(conn);
    println!("CONNECTED t={:.1}", started.elapsed().as_secs_f64());
    let watch = tokio::spawn(watch_path("dialer", conn.clone(), started));

    let mut stream = conn
        .open_stream()
        .await
        .map_err(|e| anyhow!("open_stream: {e}"))?;
    let chunk = vec![0x5au8; CHUNK];
    let mut sent = 0u64;
    let deadline = started + Duration::from_secs(secs);
    while Instant::now() < deadline {
        stream.write_all(&chunk).await.context("write")?;
        sent += CHUNK as u64;
        tokio::time::sleep(CHUNK_EVERY).await;
    }
    stream.flush().await.context("flush")?;
    stream.shutdown().await.ok();
    let kind = conn.path();
    watch.abort();
    println!("FINAL kind={} bytes={sent}", label(kind));
    Ok(kind)
}

fn arg<T: std::str::FromStr>(args: &[String], i: usize, what: &str) -> Result<T> {
    args.get(i)
        .ok_or_else(|| anyhow!("missing <{what}>"))?
        .parse()
        .map_err(|_| anyhow!("bad <{what}>"))
}

fn read_root(path: &str) -> Result<CertificateDer<'static>> {
    Ok(CertificateDer::from(
        std::fs::read(path).with_context(|| format!("read {path}"))?,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("relay") => {
            let ip: IpAddr = arg(&args, 2, "bind-ip")?;
            let port: u16 = arg(&args, 3, "https-port")?;
            let out: String = arg(&args, 4, "out-dir")?;
            let discovery = args.get(5).is_some_and(|a| a == "--discovery");
            let members = args[5 + usize::from(discovery)..]
                .iter()
                .map(|s| {
                    s.parse::<u8>()
                        .map(public)
                        .map_err(|_| anyhow!("bad seed {s}"))
                })
                .collect::<Result<Vec<_>>>()?;
            let (mut server, url, root) = start_relay(ip, port, discovery, members).await?;
            std::fs::write(Path::new(&out).join("relay-root.der"), root.as_ref())?;
            println!("RELAY_URL {url} discovery={discovery}");
            server
                .join()
                .await
                .map_err(|e| anyhow!("relay: {e}"))?
                .map_err(|e| anyhow!("relay: {e}"))?;
        }
        Some("listen") => {
            let seed: u8 = arg(&args, 2, "seed")?;
            let url: RelayUrl = arg(&args, 3, "relay-url")?;
            let root = read_root(&arg::<String>(&args, 4, "relay-root.der")?)?;
            listen(endpoint(seed, url, root).await?).await?;
        }
        Some("dial") => {
            let seed: u8 = arg(&args, 2, "seed")?;
            let url: RelayUrl = arg(&args, 3, "relay-url")?;
            let root = read_root(&arg::<String>(&args, 4, "relay-root.der")?)?;
            let peer: u8 = arg(&args, 5, "peer-seed")?;
            let secs: u64 = arg(&args, 6, "secs")?;
            dial(&endpoint(seed, url, root).await?, public(peer), secs).await?;
        }
        Some("local") => {
            // One host, no NAT: the observer's sanity check — over the host's own
            // interfaces a direct path is makeable, so the probe must see it turn.
            let secs: u64 = arg(&args, 2, "secs")?;
            let (server, url, root) = start_relay(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
                false,
                vec![public(0xa1), public(0xb2)],
            )
            .await?;
            let listener = endpoint(0xa1, url.clone(), root.clone()).await?;
            tokio::spawn(listen(listener));
            let dialer = endpoint(0xb2, url, root).await?;
            dial(&dialer, public(0xa1), secs).await?;
            server.shutdown().await.ok();
        }
        _ => bail!("usage: nat_probe relay|listen|dial|local … (see the module doc)"),
    }
    Ok(())
}
