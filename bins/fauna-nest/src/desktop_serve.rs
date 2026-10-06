//! Shared cross-OS desktop nest serve-and-restart loop.
//!
//! Both desktop machine-service shells run their nest **in-process** and need the
//! identical sequence: resolve the system data layout, construct a private-mode
//! [`NestConfig`](crate::config::NestConfig), seed the claim code, open the db,
//! load/generate the nest identity, build the backup + token + services state,
//! pin the self-signed TLS *floor* (and keep it renewed/hot-reloaded), then serve
//! — restarting the listener when the admin changes the client-facing
//! `serving_port` (the nest cannot hot-rebind its own `TcpListener`). This module
//! is that one sequence, so the two shells delegate to it rather than each
//! reimplementing it (priority #2/#3):
//!
//! - **Windows** — `apps/fauna-windows/fauna-nest-service` (`run_service_loop`,
//!   the SCM shell), default serving port 443 (the SCM runs as `LocalSystem`, so
//!   it binds the privileged port directly).
//! - **macOS** — `bins/fauna-nest-daemon` (the `social.fauna.nest` `LaunchDaemon`
//!   running as `_fauna`), default serving-port **seed** 3000 (a `LaunchDaemon`
//!   under a non-root service user reaches `:443` via launchd **socket
//!   activation** — a packaging-slice residual; the seed covers the direct-bind /
//!   as-built path).
//!
//! Per-OS concerns the **caller** resolves and passes in: the data dir (Windows
//! `%PROGRAMDATA%\Fauna\nest`; macOS `FAUNA_DATA_DIR` → `/Library/Application
//! Support/Fauna`), the default serving port (443 vs 3000), and the shutdown future (Windows `ctrl_c` under SCM; macOS
//! SIGTERM/SIGINT under launchd). The cross-platform serving-port reconcile
//! *policy* is the shared [`fauna_nest_supervisor`] crate.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fauna_nest_supervisor::{
    SERVING_PORT_POLL, effective_serving_port, serving_port_restart_target,
};

/// Inputs for [`run_serve_loop`]. Every per-OS path/port/key the desktop shells
/// differ on is resolved by the caller and passed here, keeping the loop itself
/// OS-agnostic.
pub struct ServeLoopConfig {
    /// The **external** client-facing bind seed (`0.0.0.0:<port>` on a desktop
    /// box — all interfaces, no distinction by client origin). `start_server`
    /// boot-resolves the admin `serving_port` singleton over this seed's port
    /// (the box is not `FAUNA_FRONTED_BY_ROUTER`). A `port == 0` seed binds an
    /// OS-assigned ephemeral port (used by tests; the production shells pass a
    /// concrete port).
    pub bind: SocketAddr,
    /// The nest data root — `nest.db`, blobs, identity, the `acme`/floor dir, the
    /// `serving-port` flag, `services.json`, sidecar tokens all live under here.
    pub data_dir: PathBuf,
    /// The serving-port **seed** the admin's `serving_port` choice overrides on
    /// restart (Windows 443, macOS 3000) — the `default_port` the shared
    /// reconcile policy resolves the flag against.
    pub default_serving_port: u16,
    /// When `Some(p)`, ask the nest to **also** bind the fixed
    /// `127.0.0.1:p` co-located-IPC loopback listener (the MDA bridge + same-box
    /// app dial it; it never moves when the admin changes the external
    /// `serving_port` — `nest/common.md` § Same-box reach) by setting the
    /// `FAUNA_INTERNAL_LOOPBACK_PORT` IPC env. `None` ⇒ no extra listener (tests
    /// / Docker / dev / bare-metal). Production shells pass
    /// `Some(CANONICAL_INTERNAL_LOOPBACK_PORT)` = `Some(3000)`.
    pub internal_loopback_port: Option<u16>,
}

/// Run the desktop nest serve-and-restart loop until `shutdown` resolves.
///
/// `activated_listener` is the macOS launchd **socket-activation** seam: when
/// `Some`, root launchd has pre-bound the privileged `:443` and handed the
/// listening fd to the non-root `_fauna` daemon (which cannot bind `:443`
/// itself) — the **first** serve iteration uses that listener verbatim as the
/// external listener (`installers/macos.md` § Network-reachable nest). It is
/// consumed once: an admin serving-port change re-enters the loop with the
/// listener already taken, so the restart **direct-binds** the new (now
/// non-privileged, daemon-bindable) port — and the bind-fallback keeps the box
/// reachable on the seed if the new choice is itself unbindable. (The one edge
/// it leaves — an admin moving the port *back* to the launchd-owned `:443` after
/// a change — needs a real launchd to design+test and remains untested;
/// recoverability still holds, the nest stays up on the seed.) `None`
/// is the Windows SCM / direct-bind / test path (the loop binds `cfg.bind`).
///
/// On each (re)start the bound [`SocketAddr`] is sent on `ready` (when `Some`) —
/// production shells pass `None`; tests use it to learn the ephemeral port. The
/// loop returns `Ok(())` on a clean shutdown, and `Err` on a fatal start failure
/// or an unexpected server-task exit (so the OS service layer records a specific
/// fault instead of a silent clean stop).
pub async fn run_serve_loop<S>(
    cfg: ServeLoopConfig,
    activated_listener: Option<std::net::TcpListener>,
    shutdown: S,
    ready: Option<tokio::sync::mpsc::UnboundedSender<SocketAddr>>,
) -> Result<()>
where
    S: Future<Output = ()>,
{
    let ServeLoopConfig {
        bind,
        data_dir,
        default_serving_port,
        internal_loopback_port,
    } = cfg;

    tracing::info!(data_dir = %data_dir.display(), "nest serve loop starting");

    // Same-box reach (nest/common.md § Same-box reach): ask the nest to ALSO bind
    // a fixed 127.0.0.1:<loopback> internal-loopback listener alongside its
    // external serving_port listener. `start_server` reads this IPC env (bucket-2
    // artifact-wiring, the sibling of FAUNA_FRONTED_BY_ROUTER) and adds the
    // additive listener; the fixed loopback never moves when the admin changes
    // the external serving_port, so the co-located bridge + app that dial it are
    // never stranded. `None` ⇒ leave the env untouched (no extra listener).
    // SAFETY: set once here at loop start — before any `start_server` call reads
    // it (below) and before any worker thread reads this var — and the process
    // never mutates the environment again, so there is no concurrent get/set (the
    // edition-2024 `set_var` soundness contract). Matches the Windows service's
    // own one-shot set at startup.
    if let Some(loopback) = internal_loopback_port {
        unsafe {
            std::env::set_var("FAUNA_INTERNAL_LOOPBACK_PORT", loopback.to_string());
        }
    }

    // Ensure the data directory exists.
    std::fs::create_dir_all(&data_dir)?;

    // Paths.
    let db_path = data_dir.join("nest.db");
    let blob_dir = data_dir.join("blobs");
    std::fs::create_dir_all(&blob_dir)?;
    let db_path_str = db_path.to_string_lossy().to_string();

    // Seed the claim code BEFORE start_server, mirroring the standalone
    // `fauna-nest` binary's `main` (`bins/fauna-nest/src/main.rs` — it calls this
    // pre-db, then start_server). `fauna_nest::start_server` only *reconciles* the
    // claim code (deletes a resurrected one once an admin exists); the initial
    // mint is `ensure_claim_code_at`. Without it a fresh desktop nest never writes
    // `<data_dir>/claim-code`, so `setup.status.claimed` derives `true` with NO
    // admin — an unclaimable, unrecoverable box, violating "works out-of-the-box"
    // + `docs/goal/architecture/nest/common.md` § Client-state recoverability.
    // Idempotent (no-op if the file exists) and redacted (prints the code to the
    // log only; reconcile deletes it on claim).
    crate::claim::ensure_claim_code_at(&db_path_str);

    // Build a minimal private-mode NestConfig.
    let nest_config = Arc::new(crate::config::NestConfig {
        nest: crate::config::NestSection {
            mode: crate::config::NodeMode::Private,
            listen: bind.to_string(),
            db_path: db_path_str.clone(),
            blob_dir: Some(blob_dir.to_string_lossy().to_string()),
            // `registration_mode` stays at its `None` default: a private single-user
            // desktop nest seeds no registration posture ⇒ the safe `closed` default.
            // Its owner is admitted by the claim ceremony, which needs no
            // registration; nobody else should be able to self-admit on a box that
            // calls itself single-user.
            ..Default::default()
        },
        bridges: None,
        submission: None,
        // Pin the ACME / TLS-floor directory under the nest's own data dir.
        // `fauna_nest::start_server` writes the always-live self-signed TLS
        // *floor* cert (and keys the encrypted-storage material) at this dir; if
        // it's left `None` the resolver falls back to the default
        // `/var/lib/fauna/acme`, an out-of-data-dir, non-recoverable location —
        // and leaves the supervised MDA with no floor cert under the nest data
        // dir to fetch and serve CalDAV/IMAP over TLS. Keeping it under `data_dir`
        // makes the floor (and storage key) governed by the client's backup /
        // factory-reset of that dir (recoverability invariant). ACME issuance
        // stays off automatically: this is a loopback/LAN nest with no public
        // domain, so the derived ACME gate (`acme::build_acme_config`) never
        // enables it. There is no `enabled` field.
        acme: Some(crate::config::AcmeSection {
            mode: crate::config::AcmeMode::Http01,
            dir: Some(data_dir.join("acme").to_string_lossy().into_owned()),
            directory_url: None,
        }),
        email: None,
        update: Default::default(),
    });

    // Open database.
    let db = Arc::new(crate::db::CacheDb::open(&db_path_str)?);
    tracing::info!("database opened at {}", db_path.display());

    // Reconcile the durable deployment signing key — the nest's SINGLE identity
    // (single-identity unification, `box-recovery.md`). The standalone binary does
    // this in `main.rs`; the embedded desktop nest must too, so it gets a durable
    // `nest_deployment.key` and `start_server` can derive `nest_identity` from the
    // reconciled deployment seed (nest.info/federation/sync all key off it).
    // Idempotent across restarts; a desktop box normally has no
    // `FAUNA_DEPLOYMENT_SEED`, so this just persists the migration-seeded key.
    if let Err(e) = crate::deployment_key::reconcile_deployment_keypair(
        &db,
        &data_dir,
        crate::deployment_key::deployment_seed_from_env(),
    )
    .await
    {
        tracing::error!("deployment-key reconcile failed: {e}");
    }

    // Set up the backup service with the blob dir.
    let backup_service = Some(Arc::new(crate::backup::service::BackupService::new(
        db.clone(),
        None,  // encryption_key
        false, // compression
        blob_dir.clone(),
        Some(db_path.clone()),
    )?));

    // Token store (registration config is rebuilt per (re)start, below — cheap
    // default, and re-callable across serving-port restarts).
    let token_store = Arc::new(crate::token_store::TokenStore::new());

    // Ensure services.json exists (default if missing) and generate sidecar tokens.
    let services_json_path = crate::services::services_json_path(&db_path_str);
    crate::services::ensure_services_json(&services_json_path);
    let sidecar_tokens = crate::sidecar_tokens::generate_sidecar_tokens(&data_dir)?;

    // Build the listener TLS config from the always-live self-signed floor under
    // the nest's acme dir (the same dir the AcmeSection above pins, and the same
    // floor `start_server` writes). Reusing fauna_nest's floor→ServerConfig seam
    // (priority #2) makes this nest serve HTTPS on its routable bind, trusted by
    // clients via TOFU + channel-binding (security.md § Transport trust) — the
    // network-reachable-nest target. `None` domain ⇒ the domainless loopback/IP
    // floor (CN "fauna-nest").
    let acme_dir = data_dir.join("acme");
    let floor_domain: Option<String> = nest_config.nest.domain.clone().filter(|d| !d.is_empty());
    // Test/diagnostic-only plain-HTTP escape, mirroring the standalone fauna-nest
    // binary (main.rs): the tier_3 binary-e2e suite sets FAUNA_INSECURE_DISABLE_TLS
    // process-wide (conftest.py) so every per-platform self-spawned nest — these
    // desktop services included — serves plain HTTP, since the suite reaches nests
    // over http:// from many call sites. NEVER set by any real deployment (the
    // installer/SCM/launchd never emits it). See domains-and-tls-bootstrap.md
    // § Test posture.
    //
    // `test-hooks`-gated so the escape is compiled out of the release artifacts
    // entirely (convention 15 — the compile-time exclusion is the outer security
    // boundary; a runtime env-var gate alone is not enough). This loop is the
    // shared body of BOTH desktop service shells, so one gate reaches two shipped
    // binaries: `fauna-nest-service` (the Windows SCM shell) and
    // `fauna-nest-daemon` (the macOS LaunchDaemon). Neither inherits `fauna-nest`'s
    // features implicitly — each forwards its own `test-hooks` feature onto
    // `fauna-nest/test-hooks` (their Cargo.toml `[features]`), and only the e2e
    // build path names it. The release paths (`release.yml` builds both with no
    // features) therefore carry no escape at all.
    #[cfg(feature = "test-hooks")]
    let force_plain_http = std::env::var("FAUNA_INSECURE_DISABLE_TLS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // The whole plain-HTTP arm — the `warn!` literal included — sits inside the
    // gate, so a release build has no `FAUNA_INSECURE_DISABLE_TLS` string to find.
    // That strings-grep absence is convention 15's own witness, which is why gating
    // just the `env::var` read above would not be enough: the literal would still
    // ship.
    //
    // The floor lookup is a closure rather than the tail of an `if`/`else` so the
    // two flavors can share one copy of it without either flavor growing dead code:
    // with the feature ON it is the `else` arm, with it OFF it is the whole
    // computation. (A labeled block with a gated `break` reads more directly but
    // warns `unused_labels` in the release flavor, where nothing breaks to the
    // label — and `nest-feature-clippy` runs `-D warnings`.)
    let floor_tls = || match crate::self_signed_cert::listener_tls_from_floor(
        &acme_dir,
        floor_domain.as_deref(),
    ) {
        Some((config, resolver)) => (Some(config), Some(resolver)),
        None => {
            tracing::warn!(
                "no TLS floor cert at {} and the bootstrap write failed; the nest \
                 will start WITHOUT TLS (plain HTTP) — remote clients that require \
                 HTTPS cannot reach it",
                acme_dir.display()
            );
            (None, None)
        }
    };

    #[cfg(feature = "test-hooks")]
    let (tls_config, served_cert_resolver) = if force_plain_http {
        tracing::warn!(
            "FAUNA_INSECURE_DISABLE_TLS set — nest serving PLAIN HTTP on its API \
             listener (test/diagnostic only; start_server still writes the self-signed \
             floor for the mail bridge)"
        );
        (None, None)
    } else {
        floor_tls()
    };
    #[cfg(not(feature = "test-hooks"))]
    let (tls_config, served_cert_resolver) = floor_tls();

    // Keep the served self-signed floor fresh and hot-reloaded for the life of the
    // service. Without this a long-lived HTTPS nest's floor would expire (~90 days)
    // and reject even native apps' rustls handshakes. `floor_renew_task`
    // re-synthesizes from the persisted stable key (SPKI unchanged → no client
    // re-prompt); `cert_watcher_task` hot-reloads the refreshed PEM into the
    // resolver backing the live ServerConfig. Spawned ONCE here (not per
    // serving-port restart): the renew/watch tasks back the shared
    // `Arc<rustls::ServerConfig>` that every `start_server` (re)start re-uses, so a
    // restart inherits the already-renewed floor and we never leak a watcher per
    // restart.
    if let Some(resolver) = served_cert_resolver.clone() {
        // spawn-ok(process-lifetime): deliberately hoisted OUT of the serve loop — one cert watcher per process, shared across generations, holds no AppState
        tokio::spawn(crate::acme::cert_watcher_task(
            resolver,
            acme_dir.clone(),
            None, // domainless ⇒ no store_acme_material fan-out; just hot-reload
            None, // no relay sidecar runs beside a desktop-served nest
        ));
        // spawn-ok(process-lifetime): same hoist as the cert watcher above — no AppState, one per process
        tokio::spawn(crate::self_signed_cert::floor_renew_task(
            acme_dir.clone(),
            floor_domain.clone(),
        ));
        tracing::info!("self-signed TLS floor renew + cert-watcher tasks started");
    }

    // The shutdown signal (Windows SCM Stop / foreground Ctrl-C; macOS launchd
    // SIGTERM / SIGINT) — pinned so the shutdown intent survives across
    // serving-port restarts (each restart re-enters the serve loop, but a stop
    // request must still end the service).
    tokio::pin!(shutdown);
    // Poll the `<data_dir>/serving-port` flag for an admin serving-port change.
    let mut serving_port_poll = tokio::time::interval(SERVING_PORT_POLL);
    // The launchd socket-activated listener (macOS `:443`), consumed by the FIRST
    // serve iteration; restart iterations find it `None` and direct-bind (see the
    // fn doc — the consume-once / restart-rebinds-directly model).
    let mut activated_listener = activated_listener;

    // Serve-and-restart loop. The nest cannot hot-rebind its own `TcpListener`, so
    // an admin serving-port change is apply-on-restart: `start_server` boot-resolves
    // the admin `serving_port` over the `bind` seed's port (desktop is not
    // `FAUNA_FRONTED_BY_ROUTER`), and materializes it as the `<data_dir>/serving-port`
    // value flag. When that flag diverges from the port this run was started with,
    // tear down the server task and re-enter `start_server`, which re-resolves and
    // binds the new port. See `nest/common.md` § Serving ports (desktop-direct).
    'serve: loop {
        // Anchor the edge-trigger on the flag value observed at THIS (re)start, not
        // on the port `start_server` actually bound — comparing flag-snapshot vs
        // flag-now (as the MDA supervisor compares `spawned_listen`) makes a
        // transiently divergent flag/DB unable to cause a restart storm.
        let started_with = effective_serving_port(&data_dir, default_serving_port);

        // The socket-activated `:443` listener feeds the FIRST iteration only. A
        // pre-bound `std::net::TcpListener` must be non-blocking before tokio can
        // drive it; convert it here (inside the runtime). A conversion failure is
        // fatal — there is no safe way to recover an fd we cannot serve.
        let external_listener = match activated_listener.take() {
            Some(std_listener) => {
                std_listener.set_nonblocking(true)?;
                Some(tokio::net::TcpListener::from_std(std_listener)?)
            }
            None => None,
        };

        // Start the nest HTTP server. Pass the floor TLS config AND the resolver as
        // the channel-binding SPKI source (`served_cert_spki`) so
        // `fauna.auth.handshake` can bind this self-signed cert to the nest identity
        // — the trust path a remote `test@<ip>` relies on (security.md § Transport
        // trust). All heavy args are `Arc`/`Option<Arc>` (cheap per-restart clone);
        // the registration config is a cheap rebuilt default.
        let (addr, mut server_handle, app_state) = match crate::start_server(
            bind,
            external_listener,
            db.clone(),
            // enforce_tier_quotas = false. This is the owner's own machine: a
            // 100 MB inbox / 5-feed `free`-tier ceiling on your own laptop is
            // nonsense. It does NOT admit strangers — that was the old
            // `require_registration` boolean's other half, now permanently gone:
            // an actor with no `users` row is refused in every mode, and this
            // nest's owner gets their row from the claim ceremony
            // (`claim_core` → `create_user_with_handle`), not from a handshake.
            false,
            backup_service.clone(),
            token_store.clone(),
            tls_config.clone(),
            served_cert_resolver
                .clone()
                .map(|r| r as Arc<dyn crate::acme::ServedCertSpki>),
            crate::routes::RegistrationConfig::default(),
            nest_config.clone(),
            // The keypair sits beside the desktop nest's database like every
            // other data-directory path; whether the bridge is AVAILABLE is
            // then the same derivation every nest runs, and a single-user
            // desktop nest on `localhost` derives nothing. One rule, no
            // per-artifact carve-out (the arg only exists under the
            // off-by-default `bluesky` feature).
            #[cfg(feature = "bluesky")]
            Some(data_dir.join("bluesky_oauth_key.json")),
            None, // push_service
            services_json_path.clone(),
            sidecar_tokens.clone(),
        )
        .await
        {
            Ok(started) => started,
            Err(e) => return Err(e),
        };

        tracing::info!("nest server listening on {addr}");
        // Report the bound address (per (re)start) to any observer — production
        // shells pass `None`; tests learn the ephemeral port from this.
        if let Some(ref tx) = ready {
            let _ = tx.send(addr);
        }

        // Serve until a shutdown signal, an unexpected server-task exit, or an admin
        // serving-port change.
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    tracing::info!("shutting down nest server");
                    server_handle.abort();
                    let _ = server_handle.await;
                    return Ok(());
                }
                join = &mut server_handle => {
                    // The server task should outlive every reconcile; an early exit
                    // (panic / listener death) is a fault, not a restart trigger.
                    // Surface it as an error so the OS service layer records a
                    // specific failure (and can apply its recovery action) instead
                    // of a silent clean stop.
                    return Err(anyhow::anyhow!(
                        "nest server task ended unexpectedly: {join:?}"
                    ));
                }
                _ = serving_port_poll.tick() => {
                    if let Some(new_port) =
                        serving_port_restart_target(started_with, &data_dir, default_serving_port)
                    {
                        tracing::info!(
                            old = started_with,
                            new = new_port,
                            "admin changed serving port — restarting nest to rebind its listener",
                        );
                        // Full generation teardown (not just the server task):
                        // the scoped workers hold the old graph and would
                        // otherwise leak — one per restart — while their
                        // successors run beside them.
                        app_state.teardown_serving_generation(server_handle).await;
                        continue 'serve;
                    }
                }
                _ = app_state.serve_restart.notified() => {
                    // Deployment-seed rotation committed (`box-recovery.md`
                    // § Adoption by the running process): tear down this
                    // serving generation and re-enter `start_server`, which
                    // rebuilds the whole graph — identity, signing key,
                    // workers — from the rotated DB. In-process, so the
                    // desktop shells need no supervisor round-trip.
                    tracing::info!(
                        "serving-generation restart requested (deployment-seed rotation) — \
                         tearing down and re-entering start_server"
                    );
                    app_state.teardown_serving_generation(server_handle).await;
                    continue 'serve;
                }
            }
        }
    }
}
