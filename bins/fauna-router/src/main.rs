use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tracing::{info, warn};

pub mod backends;
mod config;
pub mod db;
pub mod proxy_handler;
pub mod routing;
pub mod ws_proxy;

use backends::BackendPool;
use config::ProxyConfig;
use db::ProxyDb;
use proxy_handler::{ProxyState, proxy_fallback};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "fauna-router")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Subcommand)]
enum Commands {
    Serve(ServeArgs),
}

#[derive(Parser, Clone, Debug)]
struct ServeArgs {
    #[arg(long, default_value = "proxy.toml")]
    config: String,
    /// Override the bind address from config (e.g. `0.0.0.0:8080`).
    #[arg(long)]
    bind: Option<String>,
    /// Override the SQLite DB path from config.
    #[arg(long)]
    db: Option<String>,
    /// Override the primary domain from config.
    #[arg(long)]
    domain: Option<String>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let args = match cli.command {
        Some(Commands::Serve(a)) => a,
        None => cli.serve,
    };

    // Build a multi-thread Tokio runtime and run the async server.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?
        .block_on(run(args))
}

// ── Async server ──────────────────────────────────────────────────────────────

async fn run(args: ServeArgs) -> anyhow::Result<()> {
    // ── Logging ───────────────────────────────────────────────────────────────

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // ── Config ────────────────────────────────────────────────────────────────

    info!("loading config from {}", args.config);
    let mut cfg = ProxyConfig::from_file(&args.config)
        .with_context(|| format!("failed to load config '{}'", args.config))?;

    // CLI flags override config values.
    if let Some(bind) = args.bind {
        cfg.proxy.listen = bind;
    }
    if let Some(db_path) = args.db {
        cfg.proxy.db_path = db_path;
    }
    if let Some(domain) = args.domain {
        cfg.proxy.domain = domain;
    }

    info!(
        domain = %cfg.proxy.domain,
        listen = %cfg.proxy.listen,
        db_path = %cfg.proxy.db_path,
        backends = cfg.backend.len(),
        "proxy config loaded"
    );

    // ── Database ──────────────────────────────────────────────────────────────

    info!("opening database at {}", cfg.proxy.db_path);
    let db = Arc::new(
        ProxyDb::open(&cfg.proxy.db_path)
            .with_context(|| format!("failed to open database '{}'", cfg.proxy.db_path))?,
    );

    // ── Backend pool ──────────────────────────────────────────────────────────

    let pool = Arc::new(
        BackendPool::from_config(&cfg.backend)
            .context("failed to build backend pool from config")?,
    );

    if pool.backends.is_empty() {
        warn!("no backends configured — only proxy-handled routes will work");
    } else {
        info!("{} backend(s) configured", pool.backends.len());
    }

    // ── HTTP client ───────────────────────────────────────────────────────────

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.proxy.request_timeout_secs))
        .build()
        .context("failed to build reqwest client")?;

    // ── ProxyState ────────────────────────────────────────────────────────────
    // No registration state here: the posture, capacity and handle domain are
    // client-set nest state learned from the poll (`pool`), and the reserved
    // list is the shared `fauna_protocol::handle::RESERVED_HANDLES` constant.

    let state = Arc::new(ProxyState {
        db: Arc::clone(&db),
        pool: Arc::clone(&pool),
        client: client.clone(),
        domain: cfg.proxy.domain.clone(),
    });

    // ── Health check loop ─────────────────────────────────────────────────────

    let health_interval = cfg.proxy.health_check_interval_secs;
    {
        let pool_hc = Arc::clone(&pool);
        let client_hc = client.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(health_interval));
            // The first tick fires immediately — use that for the initial check.
            loop {
                interval.tick().await;
                for backend in &pool_hc.backends {
                    let b = Arc::clone(backend);
                    let c = client_hc.clone();
                    tokio::spawn(async move {
                        BackendPool::health_check(&b, &c).await;
                        let status = if b.healthy.load(std::sync::atomic::Ordering::Relaxed) {
                            "healthy"
                        } else {
                            "unhealthy"
                        };
                        tracing::debug!(backend = %b.name, status, "health check");
                    });
                }
            }
        });
    }

    // ── Axum router ───────────────────────────────────────────────────────────

    // No `/api/v1/*` route of its own (`proxy_handler.rs` module doc): every
    // request is forwarded to the backend that hosts its actor.
    let router = Router::new().fallback(proxy_fallback).with_state(state);

    // ── TCP listener ──────────────────────────────────────────────────────────

    let bind_addr = &cfg.proxy.listen;
    info!("binding to {}", bind_addr);
    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind to '{}'", bind_addr))?;

    info!("fauna-router listening on {}", bind_addr);
    axum::serve(listener, router)
        .await
        .context("axum server error")?;

    Ok(())
}
