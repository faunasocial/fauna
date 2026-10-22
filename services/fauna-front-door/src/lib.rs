//! The fauna.social **front door** — the public web surface of the
//! organization, treated as release infrastructure.
//!
//! One binary serving all public vhosts over HTTPS with hardened headers:
//! the apex site, the `www`→apex redirect, the `app` SPA, and a loopback
//! reverse-proxy pass for `proxy.fauna.social` (the CORS proxy) — plus the
//! shared ACME HTTP-01 flow (`fauna-acme-http01`) on port 80 with a
//! self-signed floor and hot-swapped issuance.
//!
//! Authority: `docs/goal/architecture/front-door.md` (decision record, vhost
//! topology, TLS policy, security architecture, box/deploy contract).

pub mod config;
pub mod proxy;
pub mod ratelimit;
pub mod renew;
pub mod tls;
pub mod vhost;

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Extension;
use fauna_acme_http01::ChallengeState;

pub use config::DoorConfig;

/// Peer address of the TLS connection a request arrived on, injected into
/// request extensions by the accept loop (tests inject it directly). The
/// proxy vhost's rate limiter keys on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIp(pub IpAddr);

/// Everything the vhost dispatcher needs, shared across connections.
pub struct DoorState {
    pub cfg: Arc<DoorConfig>,
    /// Loopback client for the proxy pass (buffered forwarding, mirroring
    /// `fauna-router`'s `forward_to_backend`).
    pub http: reqwest::Client,
    /// Per-peer-IP token bucket for the proxy vhost — the only route that
    /// spends someone else's resources (front-door.md § Security architecture).
    pub limiter: ratelimit::IpRateLimiter,
}

impl DoorState {
    pub fn new(cfg: Arc<DoorConfig>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("build proxy-pass client")?;
        Ok(Self {
            cfg,
            http,
            limiter: ratelimit::IpRateLimiter::new(
                ratelimit::PROXY_RATE_CAPACITY,
                ratelimit::PROXY_RATE_REFILL_PER_SEC,
            ),
        })
    }
}

/// Bring the door up: port 80 (ACME challenges + HTTPS redirect), the
/// renewal task, and the TLS vhost listener on 443. Runs for the process
/// lifetime; returns only if a listener cannot bind.
pub async fn run(cfg: DoorConfig) -> Result<()> {
    let cfg = Arc::new(cfg);
    let challenge_state = Arc::new(ChallengeState::new());

    tokio::fs::create_dir_all(&cfg.state_dir)
        .await
        .context("create state dir")?;
    let resolver = tls::init_resolver(&cfg.state_dir, &cfg.sans())?;

    // Port 80: ACME challenge serving + permanent redirect to HTTPS — the
    // shared router the nest uses.
    let http_listener = tokio::net::TcpListener::bind(cfg.http_bind)
        .await
        .with_context(|| format!("bind {}", cfg.http_bind))?;
    // Redirect only to the names the door serves — its certificate's SANs —
    // never to whatever `Host` a request carries (an open redirect otherwise).
    let served = cfg.sans();
    let http_app = port80_app(
        cfg.apex.clone(),
        served,
        challenge_state.clone(),
        Arc::new(ratelimit::IpRateLimiter::new(
            ratelimit::PORT80_RATE_CAPACITY,
            ratelimit::PORT80_RATE_REFILL_PER_SEC,
        )),
    );
    // spawn-ok(process-lifetime): the port-80 listener lives as long as the
    // process; it holds only the challenge state.
    tokio::spawn(serve_plain(http_listener, http_app, new_conn_limit()));

    // Certificate acquisition + renewal, hot-swapping into the resolver.
    // spawn-ok(process-lifetime): renewal runs for the process lifetime.
    tokio::spawn(renew::renewal_task(
        cfg.clone(),
        challenge_state.clone(),
        resolver.clone(),
    ));

    let state = Arc::new(DoorState::new(cfg.clone())?);
    let router = vhost::door_router(state);
    let tls_config = tls::tls_config(resolver);
    let listener = tokio::net::TcpListener::bind(cfg.https_bind)
        .await
        .with_context(|| format!("bind {}", cfg.https_bind))?;
    tracing::info!(
        "front door serving {} / {} / {} / {} on {}",
        cfg.apex,
        cfg.www,
        cfg.app,
        cfg.proxy,
        cfg.https_bind
    );
    serve_tls(listener, tls_config, router, new_conn_limit()).await;
    Ok(())
}

/// The door's port-80 router: the shared ACME challenge + redirect router,
/// behind the per-peer-IP token bucket. Redirects go only to `served` — the
/// names the door's certificate covers.
fn port80_app(
    apex: String,
    served: Vec<String>,
    challenge_state: Arc<ChallengeState>,
    limiter: Arc<ratelimit::IpRateLimiter>,
) -> axum::Router {
    fauna_acme_http01::http01_router(
        challenge_state,
        apex,
        Arc::new(move |host: &str| served.iter().any(|s| s.eq_ignore_ascii_case(host))),
    )
    .layer(axum::middleware::from_fn_with_state(limiter, govern_port80))
}

/// Per-peer-IP request governor for port 80: past the bucket, a 429.
async fn govern_port80(
    axum::extract::State(limiter): axum::extract::State<Arc<ratelimit::IpRateLimiter>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::IntoResponse;
    let peer = req
        .extensions()
        .get::<PeerIp>()
        .map(|p| p.0)
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    if !limiter.allow(peer, std::time::Instant::now()) {
        let mut resp = (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return resp;
    }
    next.run(req).await
}

/// The per-peer-IP connection cap both door listeners share a flavour of.
fn new_conn_limit() -> Arc<fauna_conn_limit::PerIpConnLimit> {
    fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP)
}

/// The TLS flavour of the door's one accept loop ([`serve_listener`]).
async fn serve_tls(
    listener: tokio::net::TcpListener,
    tls_config: Arc<rustls::ServerConfig>,
    router: axum::Router,
    limit: Arc<fauna_conn_limit::PerIpConnLimit>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);
    serve_listener(
        listener,
        router,
        limit,
        Some(acceptor),
        fauna_conn_limit::HEADER_READ_TIMEOUT,
    )
    .await;
}

/// The plain-HTTP flavour of the door's one accept loop ([`serve_listener`]).
async fn serve_plain(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    limit: Arc<fauna_conn_limit::PerIpConnLimit>,
) {
    serve_listener(
        listener,
        router,
        limit,
        None,
        fauna_conn_limit::HEADER_READ_TIMEOUT,
    )
    .await;
}

/// The door's one accept loop, behind both listeners: per-IP connection cap,
/// dead-peer detection, the TLS handshake when `tls` is `Some` (bounded by
/// `TLS_HANDSHAKE_TIMEOUT`), then hyper (h1 + h2) over the router with a
/// header-read timeout, so the TLS and port-80 listeners cannot diverge in
/// what they defend. A compact sibling of the nest's `serve_admitted`.
async fn serve_listener(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    limit: Arc<fauna_conn_limit::PerIpConnLimit>,
    tls: Option<tokio_rustls::TlsAcceptor>,
    header_timeout: std::time::Duration,
) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("accept error: {e}");
                continue;
            }
        };
        let Some(permit) = limit.try_acquire(peer.ip()) else {
            continue; // over the per-IP cap — shed silently
        };
        let _ = fauna_conn_limit::arm_dead_peer_detection(&tcp);
        let tls = tls.clone();
        let router = router.clone();
        // spawn-ok(connection-lifetime): the task owns the connection permit
        // and ends with the connection.
        tokio::spawn(async move {
            let _permit = permit;
            match tls {
                Some(acceptor) => {
                    let handshake = acceptor.accept(tcp);
                    let stream = match tokio::time::timeout(
                        fauna_conn_limit::TLS_HANDSHAKE_TIMEOUT,
                        handshake,
                    )
                    .await
                    {
                        Ok(Ok(t)) => t,
                        _ => return, // failed or stalled handshakes are routine noise
                    };
                    serve_connection(stream, router, peer.ip(), header_timeout).await;
                }
                None => serve_connection(tcp, router, peer.ip(), header_timeout).await,
            }
        });
    }
}

/// One admitted connection: hyper (h1 + h2) over `router`, the peer's address
/// injected for the governors. `header_read_timeout` needs a registered hyper
/// timer (hyper panics on the first request without one), hence `TokioTimer`.
async fn serve_connection<IO>(
    io: IO,
    router: axum::Router,
    peer: IpAddr,
    header_timeout: std::time::Duration,
) where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let svc = router.layer(Extension(PeerIp(peer)));
    let svc = hyper_util::service::TowerToHyperService::new(svc);
    let io = hyper_util::rt::TokioIo::new(io);
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(header_timeout);
    if let Err(e) = builder.serve_connection(io, svc).await {
        tracing::debug!("connection ended: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tower_service::Service;

    fn router() -> axum::Router {
        port80_app(
            "door.test".into(),
            vec!["door.test".into()],
            Arc::new(ChallengeState::new()),
            Arc::new(ratelimit::IpRateLimiter::new(1000.0, 1000.0)),
        )
    }

    /// A plain listener on a loopback port, `limit` and `header_timeout` as
    /// given, serving the port-80 router.
    async fn spawn_plain(
        limit: Arc<fauna_conn_limit::PerIpConnLimit>,
        header_timeout: Duration,
    ) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        // spawn-ok(test): dies with the test runtime.
        tokio::spawn(serve_listener(
            listener,
            router(),
            limit,
            None,
            header_timeout,
        ));
        addr
    }

    const REQ: &[u8] = b"GET / HTTP/1.1\r\nHost: door.test\r\n\r\n";

    /// Read until the peer closes; true when it closed (rather than the guard
    /// elapsing with the connection still open).
    async fn closed_by_peer(s: &mut TcpStream) -> bool {
        let mut buf = [0u8; 1024];
        tokio::time::timeout(Duration::from_secs(10), async {
            while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {}
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn port80_sheds_connections_past_the_per_ip_cap() {
        let limit = fauna_conn_limit::PerIpConnLimit::with_loopback_ceiling(2, 2);
        let addr = spawn_plain(limit, fauna_conn_limit::HEADER_READ_TIMEOUT).await;
        let _held1 = TcpStream::connect(addr).await.unwrap();
        let _held2 = TcpStream::connect(addr).await.unwrap();
        // Let the loop admit both before the third arrives.
        let mut probe = TcpStream::connect(addr).await.unwrap();
        probe.write_all(REQ).await.unwrap();
        assert!(
            closed_by_peer(&mut probe).await,
            "a third connection from one IP is shed, not served"
        );
    }

    #[tokio::test]
    async fn port80_closes_a_connection_that_never_finishes_its_head() {
        let limit = fauna_conn_limit::PerIpConnLimit::new(8);
        let addr = spawn_plain(limit, Duration::from_millis(150)).await;
        let mut slow = TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\nHost: door.test\r\n")
            .await
            .unwrap();
        assert!(
            closed_by_peer(&mut slow).await,
            "a slow request head is closed at the header-read timeout"
        );
    }

    #[tokio::test]
    async fn port80_governor_answers_429_past_the_peer_budget() {
        let mut app = port80_app(
            "door.test".into(),
            vec!["door.test".into()],
            Arc::new(ChallengeState::new()),
            Arc::new(ratelimit::IpRateLimiter::new(2.0, 0.0)),
        );
        let req = || {
            Request::builder()
                .uri("/")
                .header("host", "door.test")
                .extension(PeerIp(IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 9))))
                .body(Body::empty())
                .unwrap()
        };
        let mut codes = Vec::new();
        for _ in 0..3 {
            codes.push(app.call(req()).await.unwrap().status());
        }
        assert_ne!(codes[0], axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_ne!(codes[1], axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(codes[2], axum::http::StatusCode::TOO_MANY_REQUESTS);
    }
}
