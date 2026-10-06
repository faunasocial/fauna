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
    let http_app = fauna_acme_http01::http01_router(challenge_state.clone(), cfg.apex.clone());
    // spawn-ok(process-lifetime): the port-80 listener lives as long as the
    // process; it holds only the challenge state.
    tokio::spawn(async move {
        if let Err(e) = axum::serve(http_listener, http_app).await {
            tracing::error!("port-80 listener error: {e}");
        }
    });

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
    serve_tls(listener, tls_config, router).await;
    Ok(())
}

/// The TLS accept loop: per-IP connection cap, dead-peer detection, rustls
/// handshake, then hyper (h1 + h2) over the door router. A compact sibling
/// of the nest's `serve_tls`.
async fn serve_tls(
    listener: tokio::net::TcpListener,
    tls_config: Arc<rustls::ServerConfig>,
    router: axum::Router,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);
    let limit = fauna_conn_limit::PerIpConnLimit::new(fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP);
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
        let acceptor = acceptor.clone();
        let router = router.clone();
        // spawn-ok(connection-lifetime): the task owns the connection permit
        // and ends with the connection.
        tokio::spawn(async move {
            let _permit = permit;
            let tls = match acceptor.accept(tcp).await {
                Ok(t) => t,
                Err(_) => return, // handshake failures are routine noise
            };
            let svc = router.layer(Extension(PeerIp(peer.ip())));
            let svc = hyper_util::service::TowerToHyperService::new(svc);
            let io = hyper_util::rt::TokioIo::new(tls);
            let builder =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
            if let Err(e) = builder.serve_connection(io, svc).await {
                tracing::debug!("connection ended: {e}");
            }
        });
    }
}
