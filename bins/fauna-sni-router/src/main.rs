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
//!
//! **Its one link to nest is the sidecar log plane** (`apps/observability.md`
//! § The sidecar log plane): with `--nest-url` set, the router dials nest's
//! `/internal/router/ws`, runs the `fauna.sidecar.hello` token handshake as the
//! `router` scope, and reports its catalogue ([`log_catalogue`]) for the
//! process's life. Routing never waits on that channel. Its credential rule is
//! the sidecar channel's (`transport.md` § the internal sidecar channel → the
//! credential's lifetime): nest refusing the token ENDS the process (exit 78)
//! so s6 restarts it against nest's freshly minted token file — which drops
//! every live `:443` splice once per nest restart, accepted because a nest
//! restart already severs everything routed to nest.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use fauna_conn_limit::ShedCounter;
use fauna_sidecar_client::{CredentialRefused, SidecarDialError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tracing::{debug, error, info, warn};

mod log_catalogue;
mod sni;

/// TLS records cap at 16 KiB of payload; the ClientHello fits in the first one.
const MAX_CLIENT_HELLO: usize = 16 * 1024 + 5;
/// How long to wait for the ClientHello before giving up on a connection.
const PEEK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a SIGTERM'd router keeps its live splices before exiting anyway. The
/// nest's graceful shutdown (WS 1001 to every app, its drain bounded by its 7 s
/// `GRACEFUL_SHUTDOWN_TIMEOUT`, then the final flush) leaves through these
/// splices, so the grace outlasts that budget and still fits the container's
/// 15 s `stop_grace_period` (`transport-connection.md` § Graceful shutdown).
const DRAIN_GRACE: Duration = Duration::from_secs(10);

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
    /// That mistaken assumption is what took the live nest's `:443` down on
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

    /// nest's loopback HTTP(S) base, e.g. `https://127.0.0.1:3000` — artifact-set
    /// IPC wiring (the s6 run-script). When set, the router holds the sidecar
    /// log-plane channel to nest's `/internal/router/ws`, authenticating with the
    /// boot-minted token the run-script passes as `FAUNA_SIDECAR_TOKEN`. Unset
    /// (a bare test run), the router routes with no plane.
    #[arg(long = "nest-url")]
    nest_url: Option<String>,
}

/// The router's internal sidecar route on nest, and the scope its hello declares
/// (`SidecarScope::Router` on the nest side).
const ROUTER_WS_ROUTE: &str = "/internal/router/ws";
const ROUTER_SCOPE: &str = "router";

/// One backend's admin-facing name and its rate-limited connect-failure counter
/// (see [`log_catalogue::backend_unreachable`]).
struct BackendFailures {
    name: String,
    counter: ShedCounter,
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
    /// Backend address → its name and connect-failure counter
    /// ([`backend_failure_counters`]).
    backend_failures: HashMap<String, BackendFailures>,
}

/// Name every backend by how it is routed — never by its address: the default
/// route is `nest` (the deployment's `--default`), a label route is its leading
/// label (`mail`, `relay`, `pds`), an exact route is `exact-route`. The first
/// name given to an address wins, the default first.
fn backend_failure_counters(
    default: &str,
    routes: &HashMap<String, String>,
    label_routes: &HashMap<String, String>,
) -> HashMap<String, BackendFailures> {
    let mut named: Vec<(&str, String)> = vec![(default, "nest".to_string())];
    let mut labels: Vec<_> = label_routes.iter().collect();
    labels.sort();
    named.extend(labels.into_iter().map(|(l, a)| (a.as_str(), l.clone())));
    let mut exact: Vec<_> = routes.values().collect();
    exact.sort();
    named.extend(
        exact
            .into_iter()
            .map(|a| (a.as_str(), "exact-route".to_string())),
    );
    let mut out = HashMap::new();
    for (addr, name) in named {
        out.entry(addr.to_string())
            .or_insert_with(|| BackendFailures {
                name,
                counter: ShedCounter::new(),
            });
    }
    out
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

    /// Count one failed connect to `backend`; at the counter's rate-limited
    /// moments, warn on stderr and report the batch on the log plane.
    fn record_backend_failure(&self, backend: &str) {
        let Some(failures) = self.backend_failures.get(backend) else {
            return;
        };
        if let Some(count) = failures.counter.record() {
            warn!(
                backend = %failures.name,
                failed_since_last_line = count,
                "backend unreachable"
            );
            log_catalogue::backend_unreachable(&failures.name, count);
        }
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
    let backend_failures = backend_failure_counters(&cli.default, &routes, &label_routes);
    let router = Arc::new(Router {
        routes,
        label_routes,
        default: cli.default.clone(),
        send_proxy_to,
        proxy_auth,
        backend_failures,
    });

    let listener = TcpListener::bind(&cli.listen)
        .await
        .with_context(|| format!("binding {}", cli.listen))?;
    if let Ok(addr) = listener.local_addr() {
        log_catalogue::serving(addr.port());
    }
    info!(
        listen = %cli.listen,
        default = %router.default,
        routes = ?router.routes,
        label_routes = ?router.label_routes,
        send_proxy_to = ?router.send_proxy_to,
        proxy_auth = router.proxy_auth.is_some(),
        max_connections = cli.max_connections,
        max_connections_per_ip = cli.max_connections_per_ip,
        nest_url = ?cli.nest_url,
        "fauna-sni-router listening"
    );

    // Created here, not in the accept loop, so the drain below can wait for
    // every live splice to hand back its permit.
    let conn_limit = Arc::new(Semaphore::new(cli.max_connections));
    // Flipped at SIGTERM so every live splice learns the router is draining.
    let (draining_tx, draining) = watch::channel(false);
    let accept = accept_loop(
        listener,
        router,
        Arc::clone(&conn_limit),
        cli.max_connections,
        cli.max_connections_per_ip,
        draining,
    );
    // The run-script always sets the variable; an empty or stale value (the
    // router started before nest minted this boot's tokens) is refused at the
    // hello and recovered by the restart below, like every sidecar's.
    let token = std::env::var("FAUNA_SIDECAR_TOKEN").unwrap_or_default();
    let nest_channel = async move {
        match cli.nest_url {
            Some(nest_url) => nest_channel_loop(nest_url, token).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        never = accept => match never {},
        refused = nest_channel => {
            // Nest re-minted its sidecar tokens (it restarted): ours can never
            // be honoured again and nothing in this process can re-read the
            // root-owned token file. Exit so s6 restarts us against it — every
            // live splice ends here (the accepted cost, module docs).
            error!(error = %refused, "ending the router for a fresh sidecar token");
            std::process::exit(fauna_sidecar_client::EXIT_CREDENTIAL_REFUSED);
        }
        () = shutdown_signal() => {}
    }
    // The accept loop is dropped with the select: the listener is closed and
    // nothing new is accepted. Live splices are their own tasks and keep flowing.
    let _ = draining_tx.send(true);
    drain(&conn_limit, cli.max_connections).await;
    Ok(())
}

/// SIGTERM (or SIGINT in a foreground run). A splice is cut only when the
/// process exits, so the router does not exit on the signal itself — see
/// [`drain`].
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = sigterm.recv() => info!("received SIGTERM; draining live splices"),
            _ = tokio::signal::ctrl_c() => info!("received SIGINT; draining live splices"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("received Ctrl-C; draining live splices");
    }
}

/// Wait until every live splice has ended — each holds one `conn_limit` permit
/// until its task finishes — or [`DRAIN_GRACE`] has passed. While draining, a
/// splice ends once its backend has closed and its last bytes reached the
/// client, so a backend shutting down gracefully (the nest's WS 1001 drain) gets
/// its goodbye through before the router exits, instead of every client seeing
/// a bare TCP drop (`transport-connection.md` § Graceful shutdown) — and a
/// client still holding its own half open does not hold the router for the
/// whole grace, which the container's stop budget cannot afford after the
/// nest's own drain (`installers/docker.md` § s6-overlay Services).
async fn drain(conn_limit: &Semaphore, max_connections: usize) {
    let all = u32::try_from(max_connections).unwrap_or(u32::MAX);
    match tokio::time::timeout(DRAIN_GRACE, conn_limit.acquire_many(all)).await {
        Ok(_) => info!("every splice drained; exiting"),
        Err(_) => warn!(
            live = max_connections.saturating_sub(conn_limit.available_permits()),
            grace = ?DRAIN_GRACE,
            "drain grace elapsed with splices still live; exiting"
        ),
    }
}

/// Hold the router's log-plane channel to nest for the life of the process
/// (shared policy: [`fauna_sidecar_client::serve_while_reaching_nest`]). Returns
/// only when nest refuses the router's credential.
async fn nest_channel_loop(nest_url: String, token: String) -> CredentialRefused {
    fauna_sidecar_client::serve_while_reaching_nest(
        || serve_nest_channel(&nest_url, &token),
        |_e: &SidecarDialError, _delay| log_catalogue::nest_unreachable(),
    )
    .await
}

/// One life of the channel: dial + handshake, then flush the plane's queue on
/// [`log_catalogue::FLUSH_INTERVAL`] until the channel closes. `Ok(())` ⇒ it
/// closed and the caller re-dials; `Err` ⇒ the dial itself failed.
async fn serve_nest_channel(nest_url: &str, token: &str) -> Result<(), SidecarDialError> {
    let dial = fauna_sidecar_client::dial_sidecar(
        nest_url,
        ROUTER_WS_ROUTE,
        token,
        ROUTER_SCOPE,
        BTreeMap::new(),
    )
    .await?;
    let dispatcher = dial.dispatcher;
    let Some(mut inbound) = dispatcher.inbound_requests() else {
        dial.driver.abort();
        return Ok(());
    };
    info!("router channel to nest established; log plane is live");
    // The first tick completes at once: what queued while the channel was down
    // (`serving`, `nest_unreachable`) goes out as soon as it is up.
    let mut flush = tokio::time::interval(log_catalogue::FLUSH_INTERVAL);
    loop {
        tokio::select! {
            _ = flush.tick() => fauna_sidecar_client::log_plane::flush(&dispatcher).await,
            request = inbound.recv() => match request {
                // Nest originates nothing on this channel; anything it sends
                // goes unanswered and times out on its side.
                Some(request) => debug!(kind = %request.kind, "router: kind not served on this channel"),
                None => break,
            },
        }
    }
    dial.driver.abort();
    Ok(())
}

/// Accept and splice for ever.
async fn accept_loop(
    listener: TcpListener,
    router: Arc<Router>,
    conn_limit: Arc<Semaphore>,
    max_connections: usize,
    max_connections_per_ip: usize,
    draining: watch::Receiver<bool>,
) -> Infallible {
    let per_ip_limit = fauna_conn_limit::PerIpConnLimit::new(max_connections_per_ip);
    // Shedding is the symptom an admin needs to see; logging it per-rejection
    // would flood at connection-attempt rate, so both caps report through a
    // rate-limited counter carrying the batch count — on stderr and, with the
    // same count, on the log plane.
    let global_shed = ShedCounter::new();
    let per_ip_shed = ShedCounter::new();

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
                        max = max_connections,
                        shed_since_last_line = shed,
                        "connection cap reached; shedding (sample: peer)"
                    );
                    log_catalogue::connection_cap_shed(shed);
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
                    log_catalogue::per_ip_cap_shed(shed);
                }
                continue; // `client` + `permit` dropped here → connection closed
            }
        };
        // Bound this permit's lifetime: without dead-peer detection a client
        // that disappears without a clean TCP close leaves an `ESTABLISHED`
        // socket forever, permanently burning one of its IP's slots — repeat
        // ~256 times and the source is locked out for good (the 2026-07-31
        // the live nest `:443` outage). Non-fatal if it fails: the connection just
        // falls back to the OS default idle.
        if let Err(e) = fauna_conn_limit::arm_dead_peer_detection(&client) {
            warn!(%peer, error = %e, "could not arm TCP keepalive; connection is leak-prone");
        }
        let router = Arc::clone(&router);
        let draining = draining.clone();
        tokio::spawn(async move {
            let _permit = permit; // released when this connection's task ends
            let _ip_permit = ip_permit; // ditto — releases the per-IP slot
            if let Err(e) = handle(client, peer, router, draining).await {
                debug!(%peer, error = %e, "connection closed with error");
            }
        });
    }
}

async fn handle(
    mut client: TcpStream,
    peer: SocketAddr,
    router: Arc<Router>,
    draining: watch::Receiver<bool>,
) -> Result<()> {
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

    let mut backend = match TcpStream::connect(&backend_addr).await {
        Ok(backend) => backend,
        Err(e) => {
            router.record_backend_failure(&backend_addr);
            return Err(e).with_context(|| format!("connecting backend {backend_addr}"));
        }
    };
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
    splice(&mut client, &mut backend, draining)
        .await
        .context("splicing")
}

/// Copy both ways until both directions end, each direction's EOF shutting down
/// the other side's write — except that once the router is draining, the splice
/// ends as soon as the backend's direction has: the backend has said everything
/// it will say, and only the client's open half would keep the task alive.
async fn splice(
    client: &mut TcpStream,
    backend: &mut TcpStream,
    mut draining: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let (mut client_rd, mut client_wr) = client.split();
    let (mut backend_rd, mut backend_wr) = backend.split();
    let upstream = async {
        tokio::io::copy(&mut client_rd, &mut backend_wr).await?;
        backend_wr.shutdown().await
    };
    let downstream = async {
        tokio::io::copy(&mut backend_rd, &mut client_wr).await?;
        client_wr.shutdown().await
    };
    tokio::pin!(upstream, downstream);
    let (mut up_done, mut down_done) = (false, false);
    while !(up_done && down_done) {
        tokio::select! {
            r = &mut upstream, if !up_done => { r?; up_done = true; }
            r = &mut downstream, if !down_done => { r?; down_done = true; }
            _ = draining.wait_for(|d| *d), if down_done => return Ok(()),
        }
    }
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
    use super::{Router, backend_failure_counters, decode_hex};
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
            backend_failures: HashMap::new(),
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

    /// A backend's admin-facing name comes from how it is routed, never its
    /// address — the log plane's interpolation rule (`log_catalogue` docs).
    #[test]
    fn backends_are_named_by_route_never_by_address() {
        let mut routes = HashMap::new();
        routes.insert("apex.example.com".to_string(), "127.0.0.1:9000".to_string());
        let mut label_routes = HashMap::new();
        label_routes.insert("mail".to_string(), "127.0.0.1:8444".to_string());
        label_routes.insert("pds".to_string(), "127.0.0.1:8447".to_string());
        // A label route sharing nest's address keeps nest's name.
        label_routes.insert("alias".to_string(), "127.0.0.1:3000".to_string());
        let named = backend_failure_counters("127.0.0.1:3000", &routes, &label_routes);
        let name = |addr: &str| named[addr].name.clone();
        assert_eq!(name("127.0.0.1:3000"), "nest");
        assert_eq!(name("127.0.0.1:8444"), "mail");
        assert_eq!(name("127.0.0.1:8447"), "pds");
        assert_eq!(name("127.0.0.1:9000"), "exact-route");
        assert_eq!(named.len(), 4);
    }
}
