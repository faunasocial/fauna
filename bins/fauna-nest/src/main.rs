use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use fauna_nest::db::CacheDb;
use tracing_subscriber::EnvFilter;

const ACCEPTED_CONTACT_TTL_SECS: i64 = 30 * 24 * 3600; // 30 days
const PENDING_KNOCK_TTL_SECS: i64 = 90 * 24 * 3600; // 90 days
const EXPIRY_INTERVAL_SECS: u64 = 3600; // 1 hour

/// Default listen address for the standalone `fauna-nest` binary when neither
/// `--bind` nor a config-file `listen` value is given. A nest listens on **all
/// interfaces** (`0.0.0.0`) with **no distinction by client origin** —
/// `localhost`, LAN, and WAN all reach it the same way
/// (`docs/goal/architecture/installers/windows.md` § Network-reachable nest).
/// The port is the **canonical internal nest port `3000`** (the `FAUNA_PORT`
/// default that Docker fronts with the `:443` SNI router, and that the Windows
/// SCM service / VPS front with `:443` directly —
/// `docs/goal/architecture/installers/docker.md`). `:443` is privileged on
/// Linux, so the unprivileged dev binary keeps `3000` (works-out-of-the-box);
/// production hides it behind the uniform client-facing `:443`. Mirrors
/// `config/default.toml`'s `listen = "0.0.0.0:3000"`.
const DEFAULT_LISTEN: &str = "0.0.0.0:3000";

/// [`DEFAULT_LISTEN`] parsed into a [`SocketAddr`]. Infallible — the constant is
/// a valid literal, asserted by the `default_listen_addr_*` unit tests.
fn default_listen_addr() -> SocketAddr {
    DEFAULT_LISTEN
        .parse()
        .expect("DEFAULT_LISTEN is a valid SocketAddr literal")
}

/// The `db_path` the nest should use when the `FAUNA_DATA_DIR` bucket-2 IPC env is
/// set, or `None` to keep the config/CLI value.
///
/// `FAUNA_DATA_DIR` is **artifact-wiring, never a human-edited knob** (a
/// product invariant — the IPC sibling of `FAUNA_FRONTED_BY_ROUTER` /
/// `FAUNA_INTERNAL_LOOPBACK_PORT`): the deployment artifact points the binary at a
/// system data-root the binary cannot derive itself. On the macOS machine-service
/// re-shape the `_fauna` `LaunchDaemon` sets it to `/Library/Application
/// Support/Fauna` (`installers/macos.md` § File Layout) so the nest writes server
/// state to a system path it doesn't own a home dir under. Because the nest derives
/// `data_dir = db_path.parent()` for blobs, the identity key, sidecar tokens,
/// factory-reset, and the deployment key, relocating `db_path` relocates the whole
/// on-disk data layout; `acme.dir` is pinned separately by the artifact's config
/// (bucket-2 too — the same split the Windows nest-service uses, pinning
/// `acme.dir = data_dir/acme` in its `NestConfig`). Precedence is applied at the
/// call site: an explicit `--db` flag still wins over this env, which wins over the
/// config-file `db_path`. A whitespace-only / empty value is treated as unset.
fn data_dir_db_path_override(env_data_dir: Option<&str>) -> Option<String> {
    fauna_deployment_flags::parse_dir_override(env_data_dir)
        .map(|dir| dir.join("nest.db").to_string_lossy().into_owned())
}

/// The boot **seed** for the CORS allow-list: `--cors-origin` when the operator's
/// deployment artifact passed any, else whatever the config file carries.
///
/// This is only a seed either way — a present `nest_cors_origins` DB row wins over
/// it for good, because the live surface is the app-set
/// `fauna.admin.set_cors_origins` state (`node_policy_core::resolve_cors_origins`;
/// `provisioning/registry.md` § Health-poll CORS). What this function exists to
/// settle is the precedence *between the two artifact-wiring inputs*, which follows
/// the file's standing rule ("CLI args take precedence over config file values",
/// the same rule `--db` and `--bind` follow just below the config load): a flag that
/// was given replaces the config's list wholesale; a flag that was not leaves it
/// alone, so the Docker artifact — which seeds `[nest].cors_origins` into
/// `/data/nest.toml` from `FAUNA_CORS_ORIGINS` and passes no flag — is untouched.
///
/// It has to be applied explicitly, in the `Arc::get_mut` window with the other
/// artifact-set seeds, because the flag was previously wired ONLY into the
/// no-config-file branch of the config build — so every invocation that passed
/// `--config` (which is every deployment, and the whole e2e harness) dropped it in
/// silence and kept the `DEFAULT_CORS_ORIGIN`-only posture.
fn resolve_cors_origins_seed(cli_origins: Vec<String>, config_origins: Vec<String>) -> Vec<String> {
    if cli_origins.is_empty() {
        config_origins
    } else {
        cli_origins
    }
}

/// The nest server for Fauna — open-source software for social communication.
///
/// When invoked without a subcommand, runs the nest server (equivalent to
/// `fauna-nest serve ...`; the container entrypoint runs this form). All
/// `--flag` options work both with and without the `serve` subcommand.
#[derive(Parser)]
#[command(name = "fauna-nest")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// When no subcommand is given, these serve args are used directly.
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the nest server (default when no subcommand given)
    Serve(Box<ServeArgs>),
    /// Restore database from a backup blob. Exits after restore.
    Restore {
        /// Backup blob hash (64 hex characters / 32 bytes)
        #[arg(long)]
        from: String,
        /// Path to the SQLite database file to restore into
        #[arg(long, default_value = "fauna.db")]
        db: String,
        /// Directory for blob storage
        #[arg(long, default_value = "blobs")]
        blob_dir: String,
        /// Use logical (NDJSON/tar) restore instead of hot copy
        #[arg(long)]
        logical: bool,
        /// Comma-separated table names for selective logical restore
        #[arg(long)]
        tables: Option<String>,
    },
}

#[derive(Parser, Clone, Debug)]
struct ServeArgs {
    /// Address to bind the HTTP server to (overrides config file)
    #[arg(long)]
    bind: Option<SocketAddr>,

    /// Path to the SQLite database file (overrides config file)
    #[arg(long)]
    db: Option<String>,

    /// Dev-only override: path to a VAPID EC private key PEM file, used for
    /// this boot only. Omit this flag for the ordinary path — the nest
    /// generates and persists its own VAPID keypair on first boot.
    #[arg(long)]
    vapid_pem: Option<String>,

    /// Directory to store ACME certificates (artifact wiring). The ACME domain is
    /// `[nest].domain` and the claim; the CA and the contact are constants —
    /// there is no flag for any of them.
    #[arg(long)]
    acme_dir: Option<String>,

    /// Allowed CORS origin (can be specified multiple times)
    #[arg(long = "cors-origin")]
    cors_origins: Vec<String>,

    /// Directory for blob/backup storage
    #[arg(long)]
    blob_dir: Option<String>,

    /// Directory containing static web SPA files (served at /app)
    #[arg(long)]
    static_dir: Option<String>,

    // The registration posture (open / invite-required / closed) and the free-tier
    // ceiling are NOT CLI flags: they are an admin choice, so they are set from the
    // client (`fauna.admin.set_registration_mode`) and persisted in nest state, with
    // `[nest] registration_mode` as the pre-claim seed only. `principles.md`
    // § One configuration surface; owner: `public-mode.md` § Registration Modes.
    /// Handle domain for self-service registration
    #[arg(long)]
    handle_domain: Option<String>,

    // There is deliberately NO `--reserved-handle` flag. The reserved-handle
    // list is a correctness constant (`fauna_protocol::handle::RESERVED_HANDLES`),
    // not a policy anyone chooses — and the flag's plumbing
    // (`registration.reserved_handles = args.reserved_handles`) silently
    // replaced the entire default list with the flag's empty default on every
    // boot that didn't pass it, i.e. every real deployment. Deleted 2026-07-17.
    // `--bluesky-public-url` was REMOVED 2026-09-02 — the same
    // class as `--push-relay-url` and the VAPID keypair, and it failed the same
    // test. It named this nest's OWN public URL, which is its identity domain
    // and nobody's choice, so a flag was the banned operator tier of
    // `principles.md` § One configuration surface. It was also the only writer
    // of the OAuth client, and no shipped launch line ever passed it — so the
    // Bluesky bridge was dark on every real deployment. The URL is now derived
    // from the claimed identity domain (`bluesky::oauth_public_url`).
    /// Path to TOML configuration file
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Default to `info` when RUST_LOG is unset so operational startup/status
    // events are visible out of the box — matching the rest of the server fleet
    // (fauna-router, fauna-sync, fauna-sni-router). Without this default the
    // filter is ERROR-only, which is why startup status historically had to be
    // `eprintln!`'d to stay visible; now it all flows through tracing.
    // Multi-layer registry: the nest's existing fmt output (console /
    // journald) PLUS `fauna_log::RingLayer`, the in-memory ring that backs the
    // admin Logs view (`fauna.admin.logs`, observability.md § Surfaces). The
    // shared `EnvFilter` gates both layers (default `info`), so the ring shows
    // exactly what the admin configured. NOT `fauna_log::init` — that is the
    // *client's* full ring+rolling-file+stderr stack; the nest keeps its own fmt
    // config and just adds the ring layer (observability.md § Surfaces).
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fauna_log::RingLayer)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Serve(args)) => cmd_serve(*args).await,
        Some(Commands::Restore {
            from,
            db,
            blob_dir,
            logical,
            tables,
        }) => cmd_restore(&from, &db, &blob_dir, logical, tables.as_deref()).await,
        None => {
            // No subcommand: run serve with the flattened args (the container's form)
            cmd_serve(cli.serve).await
        }
    }
}

/// Restore a database from a backup blob stored in the blob store.
///
/// Two modes:
/// - Hot copy (default): decode the blob and write it directly as the database file.
/// - Logical (`--logical`): unpack the tar.zst dump to a sibling directory.
async fn cmd_restore(
    hash_hex: &str,
    db_path: &str,
    blob_dir: &str,
    logical: bool,
    tables: Option<&str>,
) -> Result<()> {
    use fauna_core::data::ContentHash;
    use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};

    let hash_arr = fauna_core::hex32::decode(hash_hex)
        .map_err(|e| anyhow::anyhow!("invalid hex hash: {e}"))?;

    // Open blob store and fetch the backup blob.
    let store = DiskBlobStore::new(std::path::Path::new(blob_dir))?;
    let hash = ContentHash::from_digest_raw(hash_arr);
    let encoded = store
        .get(&hash)
        .await?
        .ok_or_else(|| anyhow::anyhow!("backup blob not found: {hash_hex}"))?;

    // Decode: decompress (+ decrypt if key provided — no encryption key in CLI for now),
    // bounded by the snapshot's recorded size.
    let decoded = fauna_nest::backup::decode_snapshot_blob(
        std::path::Path::new(blob_dir),
        &hash_arr,
        &encoded,
        None,
    )?;

    if logical {
        // Unpack tar.zst dump to a sibling directory.
        let output_dir = std::path::Path::new(db_path).with_extension("restored");
        std::fs::create_dir_all(&output_dir)?;

        let cursor = std::io::Cursor::new(&decoded);
        let decoder = zstd::stream::Decoder::new(cursor)?;
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&output_dir)?;

        println!("Logical dump extracted to: {}", output_dir.display());
        if let Some(table_filter) = tables {
            println!("Requested tables: {table_filter}");
            println!("Selective restore not yet implemented — full dump extracted.");
        }
    } else {
        // Hot copy: replace the database file with the decoded blob.
        let db_file = std::path::Path::new(db_path);
        if db_file.exists() {
            // Back up current DB before overwriting.
            let backup_path = db_file.with_extension("db.pre-restore");
            std::fs::rename(db_file, &backup_path)?;
            println!("Existing database backed up to: {}", backup_path.display());
        }
        std::fs::write(db_file, &decoded)?;
        println!("Database restored from backup {hash_hex}");
        println!("Database written to: {db_path}");
    }

    println!("Restore complete. Restart the server.");
    Ok(())
}

/// Build the listener TLS config from on-disk cert material — **DB-independent**,
/// so the normal boot and the degraded "needs-update" boot
/// (`fauna_nest::degraded_serve`, version-compatibility.md § 2.2) serve the same
/// cert through one code path. Writes a self-signed bootstrap cert when a domain
/// is configured and none exists, loads (or pends, under ACME) the resolver, and
/// wires the self-signed floor key. Returns `(tls_config, cert_resolver)`; the
/// resolver is `None` only when there is no cert and no path to one (plain-HTTP
/// dev/e2e nest).
#[cfg_attr(not(feature = "test-hooks"), allow(unused_variables))]
fn prepare_listener_tls(
    acme_config: &fauna_nest::acme::AcmeConfig,
    acme_enabled: bool,
    node_domain: Option<&str>,
    force_plain_http: bool,
) -> (
    Option<Arc<rustls::ServerConfig>>,
    Option<Arc<fauna_nest::acme::MultiDomainCertResolver>>,
) {
    // Test/diagnostic-only insecure escape (`FAUNA_INSECURE_DISABLE_TLS`): the
    // **nest's own API listener** serves plain HTTP (returns no TLS config) so the
    // tier_3 binary-e2e suite — which reaches nests over plain `http://` from
    // dozens of call sites across all seven apps — need not flip onto https in
    // one cross-platform sweep. The always-live self-signed floor the bridges need
    // is still written, just not here: `start_server` writes it on EVERY nest
    // entry path (`self_signed_cert::ensure_floor_present`), so a plain-HTTP nest's
    // in-process mail bridges still fetch+serve it for CalDAV/IMAP, regardless of
    // the nest API's mode (caldav-imap-any-locator). tier_3's job is wire/logic
    // over locally-built binaries (the nest TLS floor is covered by the unit tests
    // here + the tier_4 Docker suite), so the suite opts out via this env knob (set
    // process-wide in `tests/.../conftest.py`). `test-hooks`-gated below so a
    // release build has no such knob at all — `force_plain_http` is always `false`
    // when the feature is off, so this arm is unreachable there regardless, but
    // gating it too keeps the literal itself out of the release binary
    // (convention 15's strings-grep witness). See domains-and-tls-bootstrap.md
    // § Test posture.
    #[cfg(feature = "test-hooks")]
    if force_plain_http {
        tracing::warn!(
            "FAUNA_INSECURE_DISABLE_TLS set — nest API serving PLAIN HTTP \
             (start_server still writes the self-signed floor for the mail bridges; \
             test/diagnostic only, never use in a real deployment)"
        );
        return (None, None);
    }

    let floor_domain = node_domain.filter(|d| !d.is_empty());

    // Serve TLS from the always-live self-signed floor — or any real ACME/admin
    // cert already on disk. Floor synthesis (write-if-absent), cert load, and the
    // per-SNI floor-key wiring all live in the shared
    // `fauna_nest::self_signed_cert::listener_tls_from_floor` seam the Windows
    // `fauna-nest-service` reuses (priority #2); see its doc for the
    // always-live-floor rationale (tls-certificates.md § A;
    // domains-and-tls-bootstrap.md). On the healthy path this returns the floor
    // (or real) cert resolver, so a domainless/IP nest serves HTTPS from boot and
    // an ACME deploy serves the floor *first* — never the certless pending
    // resolver — until HTTP-01 self-heals it to a trusted cert.
    if let Some((config, resolver)) =
        fauna_nest::self_signed_cert::listener_tls_from_floor(&acme_config.acme_dir, floor_domain)
    {
        return (Some(config), Some(resolver));
    }

    // Fallback — reached only if the floor write above *failed* (e.g. a disk
    // error) and no cert is on disk. Under ACME + a configured domain, bind TLS
    // now with a pending (certless) resolver so the cert-watcher can swap in the
    // first issued cert with no restart (HTTPS handshakes fail cleanly until then;
    // the HTTP-01 challenge runs on the separate HTTP listener, unaffected).
    // Otherwise keep the legacy no-TLS bind so a plain-HTTP dev/e2e nest or a
    // misconfigured box stays reachable over HTTP for diagnosis.
    if acme_enabled && !acme_config.domain.is_empty() {
        let resolver = Arc::new(fauna_nest::acme::MultiDomainCertResolver::pending());
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(resolver.clone()),
        );
        tracing::info!(
            "No TLS cert at {} yet; binding TLS with a pending resolver — \
             HTTPS handshakes fail until ACME obtains the first cert (no restart needed)",
            acme_config.acme_dir.display()
        );
        // Wire the stable floor key onto the pending resolver too — the same
        // per-SNI floor fallback `listener_tls_from_floor` applies on the
        // cert-on-disk path (tls-certificates.md § A).
        match fauna_nest::self_signed_cert::load_or_create_floor_key(&acme_config.acme_dir) {
            Ok(key) => resolver.set_floor_key(key.serialize_pem()),
            Err(e) => tracing::warn!(
                "could not load/create self-signed floor key at {}: {e}; per-SNI \
                 floor fallback disabled (a custom domain with no valid cert serves \
                 the apex default)",
                acme_config.acme_dir.display()
            ),
        }
        (Some(config), Some(resolver))
    } else {
        if acme_enabled {
            tracing::warn!(
                "ACME enabled but no domain configured and no TLS cert at {}; \
                 starting without TLS",
                acme_config.acme_dir.display()
            );
        }
        (None, None)
    }
}

async fn cmd_serve(args: ServeArgs) -> Result<()> {
    let cli_bind = args.bind;
    let cli_db = args.db;

    let mut registration = fauna_nest::routes::RegistrationConfig::default();
    if let Some(ref domain) = args.handle_domain {
        registration.handle_domain = Some(domain.clone());
    }
    // `reserved_handles` stays the default constant — see the Cli struct note on
    // the deleted `--reserved-handle` flag, whose plumbing here used to wipe it.

    let config_path = args.config;

    // Load TOML config if provided, otherwise use a minimal default
    let mut nest_config = if let Some(ref path) = config_path {
        Arc::new(fauna_nest::config::NestConfig::from_file(path)?)
    } else {
        let default_bind = cli_bind.unwrap_or_else(default_listen_addr);
        let default_db = cli_db.clone().unwrap_or_else(|| "./nest.db".to_string());
        Arc::new(fauna_nest::config::NestConfig {
            nest: fauna_nest::config::NestSection {
                mode: fauna_nest::config::NodeMode::Public,
                listen: default_bind.to_string(),
                db_path: default_db,
                blob_dir: args.blob_dir.clone(),
                static_dir: args.static_dir.clone(),
                cors_origins: args.cors_origins.clone(),
                // `registration_mode` stays `None` ⇒ the safe `closed` seed. A nest
                // booted with no config file admits nobody until its admin picks a
                // posture from their client.
                ..Default::default()
            },
            bridges: None,
            submission: None,
            acme: None,
            email: None,
            update: Default::default(),
        })
    };

    // Seed the **pre-claim** NAT axis from `FAUNA_MODE` (the installer's
    // topology posture — there is no user before claim, so this is
    // topology the artifact wires, never a user choice). This REPLACES the
    // entrypoint's former `sed mode=public→private` bake, so the persistent
    // `/data/nest.toml` carries no NAT *policy* — the client-set `nest_nat_mode`
    // DB row is authoritative and overrides this seed the moment onboarding
    // writes one (`nat_mode_core::resolve_node_mode`). Re-applied every boot
    // (the env is stable in the compose file), so a pre-claim restart keeps the
    // seeded posture without baking it. `nest_config` is freshly built and not
    // yet shared, so `get_mut` always succeeds here.
    if let Some(env_mode) = std::env::var("FAUNA_MODE")
        .ok()
        .and_then(|s| fauna_nest::config::NodeMode::from_wire_str(&s))
        && let Some(cfg) = Arc::get_mut(&mut nest_config)
        && cfg.nest.mode != env_mode
    {
        tracing::info!(
            "FAUNA_MODE={} seeds the pre-claim NAT axis (the nest_nat_mode DB row overrides it post-onboarding)",
            env_mode.as_str()
        );
        cfg.nest.mode = env_mode;
    }

    // FAUNA_DATA_DIR (bucket-2 IPC, artifact-set — see `data_dir_db_path_override`)
    // relocates the nest's whole on-disk data layout to a system data-root when no
    // explicit `--db` flag is given. Applied here, in the same `Arc::get_mut`
    // window as the FAUNA_MODE seed, so it rewrites `nest_config.nest.db_path`
    // itself — keeping every derivation consistent, including `sidecar_data_dir`
    // (which reads `nest_config.nest.db_path` directly, not the resolved local
    // `db_path` below). An explicit `--db` still wins (resolved at the `db_path`
    // line below); this only moves the config-file value.
    if cli_db.is_none()
        && let Some(relocated) =
            data_dir_db_path_override(std::env::var("FAUNA_DATA_DIR").ok().as_deref())
        && let Some(cfg) = Arc::get_mut(&mut nest_config)
        && cfg.nest.db_path != relocated
    {
        tracing::info!(
            data_dir = %relocated,
            "FAUNA_DATA_DIR relocates the nest data layout to the artifact-set system data-root"
        );
        cfg.nest.db_path = relocated;
    }

    // `--cors-origin` (bucket-1 artifact wiring — `registry.md` § Health-poll CORS
    // calls it the boot seed, never the choice surface) overlays the config file's
    // own seed, in the same `Arc::get_mut` window as the two above and for the same
    // reason: the value has to reach `nest_config` itself, since that is what
    // `resolve_cors_origins` reads at boot. Before this it was applied only when NO
    // config file was given, so a nest started `--config … --cors-origin …` — every
    // deployment shape there is — silently kept the default-origin-only posture and
    // the flag did nothing at all.
    let cors_seed = resolve_cors_origins_seed(
        args.cors_origins.clone(),
        nest_config.nest.cors_origins.clone(),
    );
    if let Some(cfg) = Arc::get_mut(&mut nest_config)
        && cfg.nest.cors_origins != cors_seed
    {
        tracing::info!(
            origins = ?cors_seed,
            "--cors-origin seeds the CORS allow-list (the nest_cors_origins row overrides it once an admin sets one)"
        );
        cfg.nest.cors_origins = cors_seed;
    }

    // `--static-dir` (bucket-1 artifact wiring: where this artifact's SPA build
    // sits) overlays the config file's value, in the same window and for the
    // same reason as `--cors-origin` above: applied only when NO config file was
    // given, a nest started `--config … --static-dir …` silently served no
    // `/app/` and no share viewer page. The image writes the key into its
    // `nest.toml` instead (`docker/nest-toml-overlay.sh ensure-static-dir`).
    if let Some(dir) = args.static_dir.clone()
        && let Some(cfg) = Arc::get_mut(&mut nest_config)
        && cfg.nest.static_dir.as_deref() != Some(dir.as_str())
    {
        tracing::info!(static_dir = %dir, "--static-dir sets the SPA build this nest serves");
        cfg.nest.static_dir = Some(dir);
    }
    let nest_config = nest_config;

    // CLI args take precedence over config file values.
    // If neither is provided, fall back to whatever the config has (which the
    // FAUNA_DATA_DIR block above may have relocated to the system data-root).
    let db_path = cli_db.unwrap_or_else(|| nest_config.nest.db_path.clone());
    let bind: SocketAddr = cli_bind.unwrap_or_else(|| {
        nest_config
            .nest
            .listen
            .parse()
            .unwrap_or_else(|_| default_listen_addr())
    });

    // Provisional ACME config from the `config.nest.mode` *seed* (`FAUNA_MODE`):
    // the NAT axis is client-set, but the DB row that carries the client's
    // choice isn't readable until the DB is open below. This seed-based result
    // feeds only the degraded "needs-update" listener in the db-open error arm;
    // the authoritative `(acme_config, acme_enabled, is_private)` are re-derived
    // from the *resolved* NAT mode right after the DB opens (search
    // `resolved_node_mode`).
    let (acme_config, acme_enabled) = fauna_nest::acme::build_acme_config(
        &nest_config,
        nest_config.nest.mode,
        args.acme_dir.as_deref(),
    );

    // Test/diagnostic-only: serve plain HTTP instead of the always-live
    // self-signed floor. Never set by any deployment path (domains-and-tls-
    // bootstrap.md § Test posture) — only the tier_3 binary-e2e harness sets it.
    // `test-hooks`-gated so the escape is compiled out of the release binary
    // entirely (convention 15 — the compile-time exclusion is the outer
    // security boundary, a runtime env-var gate alone is not enough); the
    // e2e harness always builds this binary with `test-hooks` on by default
    // (`tests/common/nest.py::build_node`), so tier_3 is unaffected.
    #[cfg(feature = "test-hooks")]
    let force_plain_http = std::env::var("FAUNA_INSECURE_DISABLE_TLS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    #[cfg(not(feature = "test-hooks"))]
    let force_plain_http = false;

    // Generate or load nest identity keypair
    let data_dir = std::path::Path::new(&db_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));

    // Factory reset (restart-wipe): if `fauna.admin.factory_reset` staged a
    // marker before the supervisor restarted us, wipe deployment state NOW —
    // before opening the DB — preserving the host identity + ACME cert. The
    // claim code staged in the marker is installed so the fresh nest is
    // re-claimable with it. No marker → no-op. See `factory_reset.rs`.
    let factory_reset_blob_dir = args
        .blob_dir
        .clone()
        .unwrap_or_else(|| data_dir.join("blobs").to_string_lossy().into_owned());
    match fauna_nest::factory_reset::maybe_run_factory_reset(&db_path, &factory_reset_blob_dir) {
        Ok(true) => tracing::warn!("Factory reset completed; booting fresh / unclaimed"),
        Ok(false) => {}
        Err(e) => tracing::error!("Factory reset wipe failed: {e}"),
    }

    // The nest identity is the deployment signing key (single-identity
    // unification — `box-recovery.md`); it is reconciled into the DB below and
    // `start_server` derives `nest_identity` from it. No separate
    // `nest_identity.key` is loaded here anymore.

    // Ensure claim code exists (generate if missing)
    fauna_nest::claim::ensure_claim_code_at(&db_path);

    // Generate per-session sidecar tokens and write them to the data dir.
    //
    // A sidecar's run-script `cat`s its token file ONCE, at start, and the image
    // starts the relay sidecar unconditionally — possibly before this line has
    // run. That race converges by itself: a sidecar holding an empty or stale
    // token is refused at its hello and ends for a supervised restart, which
    // re-reads the file (`transport.md` § Future directions → the sidecar
    // channel's credential lifetime).
    let sidecar_data_dir = std::path::Path::new(&nest_config.nest.db_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("/data"));
    let sidecar_tokens = fauna_nest::sidecar_tokens::generate_sidecar_tokens(sidecar_data_dir)?;
    tracing::info!("Generated {} sidecar tokens", sidecar_tokens.len());

    // Ensure services.json exists (create default if missing).
    // Use the config's db_path to find the data directory — it reflects
    // the actual storage location (especially under --config).
    let services_json_path = fauna_nest::services::services_json_path(&nest_config.nest.db_path);
    fauna_nest::services::ensure_services_json(&services_json_path);

    // Open the DB — but if it carries a breaking schema change this binary
    // predates (`SchemaIncompatible`, version-compatibility.md § 2.2), do NOT
    // crash-loop (off-box brick — `nest/common.md` § Client-state recoverability)
    // and do NOT migrate it destructively (`CacheDb::open` already declined to
    // run migrations). Instead boot the degraded "needs-update" mode: a TLS/WS
    // listener that answers `fauna.nest.outdated` to every client. This skips all
    // the DB-dependent setup below (none of which can run against a DB this
    // binary cannot operate) and serves until the admin deploys a newer image.
    let db = match CacheDb::open(&db_path) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            match e.downcast::<fauna_nest::db::migrations::SchemaIncompatible>() {
                Ok(incompatible) => {
                    let (tls_config, _resolver) = prepare_listener_tls(
                        &acme_config,
                        acme_enabled,
                        nest_config.nest.domain.as_deref(),
                        force_plain_http,
                    );
                    return fauna_nest::degraded_serve::serve_incompatible(
                        bind,
                        tls_config,
                        incompatible,
                    )
                    .await;
                }
                // Any other open failure is a genuine error — propagate it.
                Err(other) => return Err(other),
            }
        }
    };

    // Resolve the client-set NAT axis now that the DB is open (after the
    // factory-reset wipe + migrations above). The `nest_nat_mode` row wins; an
    // absent row falls back to the `config.nest.mode` seed (`FAUNA_MODE`). This
    // is the authoritative value for every pre-`AppState` boot decision below —
    // the ACME gate, the HTTP-01 challenge listener, and STUN — so a box a
    // client set to private (public seed) runs no ACME/STUN, and one set to
    // public (private seed) does. `start_server` resolves the same row to
    // populate the live `AppState.node_mode` for the runtime readers.
    // (`2026-06-15-nest-nat-mode-client-set-design.md` §§ 3,6.)
    let resolved_node_mode = fauna_nest::nat_mode_core::resolve_node_mode(&db, &nest_config).await;
    // Re-derive the ACME config + private-axis gate against the resolved mode
    // (the seed-based build above fed only the degraded db-open-failed listener).
    // `build_acme_config` is the single source of truth for "run ACME?".
    let (acme_config, acme_enabled) = fauna_nest::acme::build_acme_config(
        &nest_config,
        resolved_node_mode,
        args.acme_dir.as_deref(),
    );
    // § Plaintext-mode behavior / § Don't do these
    // (deployment-home-with-public-relay.md): a private NAT-axis nest runs NO
    // ACME client; `build_acme_config` force-disabled it above, and `is_private`
    // gates the HTTP-01 challenge listener below on the same axis.
    let is_private = resolved_node_mode == fauna_nest::config::NodeMode::Private;
    if is_private && !acme_config.domain.is_empty() && acme_config.domain != "localhost" {
        tracing::info!(
            "A domain is configured but this nest is in private NAT mode; \
             not running an ACME client (private nests use a self-signed or \
             client-published-DNS-01 LAN cert shipped over sync)"
        );
    }

    // Restore the durable deployment signing key — the nest's SINGLE identity
    // (single-identity unification, `box-recovery.md`): the channel-binding
    // `nest_actor_id` a client TOFU-pins / a public domain publishes as DNS
    // `self=`, AND the identity `start_server` derives `nest_identity` from for
    // nest.info/federation/backup/pairing/sync. After a factory-reset wipe the
    // migration above seeded a fresh random `nest_keypair`; reconcile it from the
    // preserved `nest_deployment.key` so the re-claimed nest re-presents the SAME
    // identity and pinned clients reconnect (security.md § Transport trust;
    // common.md § Client-state recoverability). First-ever boot adopts the
    // existing DB key into the file unchanged.
    //
    // `FAUNA_DEPLOYMENT_SEED` (bucket-2 IPC, artifact-set — cloud-init env / installer
    // input) supplies a caller-chosen deployment seed at provision: on a FRESH box it
    // installs the admin's saved identity so the rebuilt box re-presents the same
    // `nest_actor_id` after **total box loss** (box-recovery.md § Mechanism). It is
    // inert once the box has an established on-disk identity.
    let deployment_seed_override = fauna_nest::deployment_key::deployment_seed_from_env();
    if let Err(e) = fauna_nest::deployment_key::reconcile_deployment_keypair(
        &db,
        data_dir,
        deployment_seed_override,
    )
    .await
    {
        tracing::error!("deployment-key reconcile failed: {e}");
    }

    // After a factory reset: seat the DKIM keys the wipe carried, now that the
    // seed they are sealed under is back (`factory_reset.rs`, *Carried*).
    match db.receive_carried_dkim_keys(data_dir).await {
        Ok(0) => {}
        Ok(n) => tracing::info!("seated {n} DKIM signing key(s) carried across a factory reset"),
        Err(e) => tracing::error!("could not seat the carried DKIM keys: {e:#}"),
    }

    // The claim banner (code + the fauna://claim URI carrying this nest's
    // identity) prints on every unclaimed boot, here — after the reconcile
    // above, the earliest point the identity is final. `ensure_claim_code_at`
    // (pre-DB, earlier) only mints the file.
    match db.get_nest_keypair().await {
        Ok(Some((_, public_key))) => match <[u8; 32]>::try_from(public_key.as_slice()) {
            Ok(nest_actor_id) => fauna_nest::claim::print_claim_banner(&db_path, &nest_actor_id),
            Err(_) => tracing::error!("nest keypair public key is not 32 bytes; no claim banner"),
        },
        Ok(None) => tracing::error!("no nest keypair after reconcile; no claim banner"),
        Err(e) => tracing::error!("nest keypair read failed: {e}; no claim banner"),
    }

    #[cfg(feature = "bluesky")]
    {
        fauna_nest::bluesky::init_db(&db).await?;
        tracing::info!("Bluesky bridge tables initialized");
    }

    #[cfg(feature = "nostr")]
    {
        fauna_nest::nostr::init_db(&db).await?;
        tracing::info!("Nostr bridge tables initialized");
    }

    #[cfg(feature = "activitypub")]
    {
        fauna_nest::activitypub::init_db(&db).await?;
        tracing::info!("ActivityPub bridge tables initialized");
    }

    // Where the Bluesky OAuth keypair lives — beside the database, artifact-set
    // like every other data-directory path. The client itself is NOT built here:
    // its `client_id` is derived from the identity domain, which is learned at
    // CLAIM, after boot (`state::BlueskyState`).
    #[cfg(feature = "bluesky")]
    let bluesky_keypair_path = std::path::Path::new(&db_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("bluesky_oauth_key.json");

    let blob_dir = args.blob_dir;
    let backup_service = match blob_dir.as_ref() {
        Some(dir) => Some(Arc::new(fauna_nest::backup::service::BackupService::new(
            db.clone(),
            None,  // encryption_key
            false, // compression
            std::path::PathBuf::from(dir),
            Some(std::path::PathBuf::from(&db_path)),
        )?)),
        None => None,
    };

    // Build the listener TLS config from on-disk cert material. Factored into
    // `prepare_listener_tls` (DB-independent) so the degraded "needs-update" boot
    // (below, on an incompatible-schema DB) serves the same cert via the same
    // logic — version-compatibility.md § 2.2.
    let (tls_config, cert_resolver) = prepare_listener_tls(
        &acme_config,
        acme_enabled,
        nest_config.nest.domain.as_deref(),
        force_plain_http,
    );

    // Start the HTTP-01 ACME challenge listener if this nest runs ACME.
    // `acme_enabled` already encodes "public NAT axis AND a real orderable
    // domain (non-empty, not `localhost`)" (`acme::build_acme_config`, the single
    // source of truth for "should this nest run ACME"). Here we additionally
    // require `!force_plain_http` — the test/diagnostic plain-HTTP escape
    // (`FAUNA_INSECURE_DISABLE_TLS`) that EVERY tier_3 e2e nest sets — so a
    // plain-HTTP test nest never binds the machine-wide `0.0.0.0:8080` challenge
    // port — and HTTP-01 mode. A private nest, a domainless/`localhost` box, or a
    // plain-HTTP test nest therefore runs no ACME
    // (deployment-home-with-public-relay.md § Don't do these).
    // Spawn the HTTP-01 challenge listener + cert-lifecycle task whenever the nest
    // is ACME-CAPABLE — public NAT axis, not the plain-HTTP escape, HTTP-01 mode —
    // WITHOUT requiring a domain at boot. A domainless-booted box learns its domain
    // at claim; the lifecycle loop reads the apex per-iteration (`handle_domain()`)
    // and issues then, woken by the claim (`acme_retry_notify`) — no restart, so
    // retiring `FAUNA_DOMAIN` (domainless boot) doesn't regress trusted-TLS
    // issuance. This drops the domain clause `acme_enabled` carries; that clause is
    // kept for the `prepare_listener_tls` resolver-choice (a domainless box still
    // serves the self-signed floor until the loop overwrites the cert file, which
    // the cert watcher hot-reloads). `resolved_node_mode != Private` is the axis
    // half of `acme_enabled`.
    let acme_http01_mode = resolved_node_mode != fauna_nest::config::NodeMode::Private
        && !force_plain_http
        && nest_config
            .acme
            .as_ref()
            .map(|a| a.mode == fauna_nest::config::AcmeMode::Http01)
            .unwrap_or(false);

    // When HTTP-01 ACME is enabled, start the challenge listener now (it needs
    // no AppState) but defer the cert-lifecycle task until after `start_server`
    // hands us the AppState: the lifecycle task derives its desired SAN set from
    // active mail domains in nest state, so the mail hostnames (`mail.<domain>`)
    // get covered, not just the apex. Stash the runtime here, spawn below.
    let acme_http01_runtime: Option<(
        fauna_nest::acme_http01::Http01Config,
        std::sync::Arc<fauna_nest::acme_http01::ChallengeState>,
    )> = if acme_http01_mode {
        let challenge_state = std::sync::Arc::new(fauna_nest::acme_http01::ChallengeState::new());

        let http01_config = fauna_nest::acme_http01::Http01Config::new(
            acme_config.domain.clone(),
            acme_config.acme_dir.clone(),
            8080,
            acme_config.directory_url.clone(),
        );

        // Start the HTTP listener for ACME challenges (port 8080, mapped to 80 by Docker)
        match fauna_nest::acme_http01::start_http01_listener(
            challenge_state.clone(),
            acme_config.domain.clone(),
            http01_config.http_port,
        )
        .await
        {
            Ok(_handle) => {
                tracing::info!(
                    "HTTP-01 ACME challenge listener started on port {}",
                    http01_config.http_port
                );
            }
            Err(e) => {
                tracing::error!("Failed to start HTTP-01 listener: {e:#}");
            }
        }

        Some((http01_config, challenge_state))
    } else {
        None
    };

    let scheme = if tls_config.is_some() {
        "https"
    } else {
        "http"
    };

    let token_store = Arc::new(fauna_nest::token_store::TokenStore::new());

    // Load APNs configuration from env vars (returns Ok(None) if not configured).
    let apns_config = match fauna_nest::push::ApnsConfig::from_env() {
        Ok(Some(cfg)) => {
            tracing::info!("APNs configuration loaded from environment");
            Some(cfg)
        }
        Ok(None) => {
            tracing::debug!("APNs not configured (APNS_KEY_PATH not set)");
            None
        }
        Err(e) => {
            tracing::error!("Failed to load APNs configuration: {e}");
            None
        }
    };

    // VAPID keypair: `--vapid-pem` is a dev-only override (a chosen key never
    // persists to the DB — it is provision-time input, not this nest's
    // identity); the ordinary path is nest-self-generated-and-persisted on
    // first boot, so web push works out of the box with no CLI flag
    // (`docs/goal/architecture/apps/common.md` § Push Notifications).
    let vapid_pem: Option<Vec<u8>> = match &args.vapid_pem {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => {
                tracing::info!("VAPID keypair: using dev-only override from {path}");
                Some(bytes)
            }
            Err(e) => {
                tracing::error!("Failed to read VAPID PEM file {path}: {e}");
                None
            }
        },
        None => match fauna_nest::push::ensure_vapid_pem(&db).await {
            Ok(pem) => Some(pem),
            Err(e) => {
                tracing::error!("Failed to load/generate the VAPID keypair: {e}");
                None
            }
        },
    };
    let push_service: Option<Arc<fauna_nest::push::PushService>> = vapid_pem.and_then(|pem| {
        match fauna_nest::push::PushService::new(db.clone(), pem, apns_config) {
            Ok(svc) => Some(Arc::new(svc)),
            Err(e) => {
                tracing::error!("Failed to initialize PushService: {e}");
                None
            }
        }
    });

    // The serve-and-restart loop (`box-recovery.md` § Deployment-seed
    // rotation → *Adoption by the running process*): one iteration = one
    // serving generation. A committed rotation fires `serve_restart`; the
    // generation is torn down (server task, WS connections, every scoped
    // worker) and `start_server` is re-entered, rebuilding the whole graph
    // from the rotated DB — in-process, so no supervisor is required.
    // (`_app_state` keeps the final generation's graph alive through the
    // graceful drain below; the drain itself needs only `handle` + `ws_state`.)
    let (handle, _app_state, ws_state) = 'serve: loop {
        let (addr, handle, app_state) = fauna_nest::start_server(
            bind,
            // The standalone `fauna-nest` binary binds its own `--bind` listener; the
            // pre-bound (launchd socket-activation) path is the macOS machine daemon.
            None,
            db.clone(),
            // The standalone server is multi-tenant: tier quotas apply. (The embedded
            // desktop nest passes `false` — a tier ceiling on the owner's own machine
            // is nonsense.) Artifact-set, not a human knob.
            true,
            backup_service.clone(),
            token_store.clone(),
            tls_config.clone(),
            cert_resolver
                .clone()
                .map(|r| r as std::sync::Arc<dyn fauna_nest::acme::ServedCertSpki>),
            registration.clone(),
            nest_config.clone(),
            #[cfg(feature = "bluesky")]
            Some(bluesky_keypair_path.clone()),
            push_service.clone(),
            services_json_path.clone(),
            sidecar_tokens.clone(),
        )
        .await?;

        // Keep a handle to the WS connection registry for the graceful-shutdown path
        // below — `app_state` itself is moved into the eviction task further down.
        let ws_state = std::sync::Arc::clone(&app_state.ws);

        // Host-OS maintenance readiness writer (installers/vps.md § Host OS
        // Maintenance): on an onboarded VPS, publish the live connection count into
        // the `/data/maintenance` bind mount so the host `fauna-reboot-coordinator`
        // reboots for kernel/security updates only while the nest is idle. None-gated
        // — a no-op on any nest without the mount (dev / desktop / bare-metal).
        {
            let ws_state = std::sync::Arc::clone(&ws_state);
            let data_dir = data_dir.to_path_buf();
            app_state.spawn_scoped(fauna_nest::host_maintenance::readiness_writer_task(
                ws_state, data_dir,
            ));
        }

        // The WireGuard tunnel start and the embedded STUN server were removed
        // 2026-08-23 with the whole WireGuard stack (user-directed;
        // `docs/goal/behavior/p2p.md` — iroh-QUIC is the only substrate). STUN
        // existed to discover a public endpoint for WG hole-punching; iroh does
        // its own path discovery through its relay, so nothing replaces it here.

        // Start TLS cert file watcher for hot-reload.
        // Phase D1.6 / D3.2: also pass the AppState + domain so that
        // store_acme_material is called whenever the cert is renewed, always using
        // the current storage impl (handles the encrypted-mode swap after startup).
        // Captured before `cert_resolver` is moved into the watcher below — the
        // web-content per-domain cert loop installs custom-domain certs into the
        // same resolver (it needs its own handle; the watcher only touches the
        // default cert).
        let web_cert_resolver = cert_resolver.clone();
        // Third handle on the same resolver: the ACME lifecycle task installs the
        // § B-IP IP bridge cert into it (`set_ip_cert`), which no watcher covers.
        let ip_bridge_resolver = cert_resolver.clone();

        if let Some(resolver) = cert_resolver.clone() {
            let acme_dir = acme_config.acme_dir.clone();
            let storage_hook = if acme_config.domain.is_empty() {
                None
            } else {
                Some((
                    std::sync::Arc::clone(&app_state),
                    acme_config.domain.clone(),
                ))
            };
            app_state.spawn_scoped(fauna_nest::acme::cert_watcher_task(
                resolver,
                acme_dir,
                storage_hook,
                Some(app_state.relay_cert_changed.clone()),
            ));
            tracing::info!("TLS certificate watcher started");

            // Keep the self-signed floor fresh on **every** nest (the boot bootstrap
            // wrote it unconditionally above — tls-certificates.md § A "always live").
            // Without renewal it would silently expire on a long-lived nest and
            // eventually fail even native apps' rustls handshakes. The task
            // re-synthesizes from the persisted stable key (SPKI unchanged → no MUA
            // re-prompt / TLSA churn); the cert-watcher just spawned hot-reloads it.
            // It backs off without clobbering if a real CA cert is ever on disk
            // (`floor_due_for_renewal` returns false for a CA-issued cert — an admin
            // self-signed re-provision is a real cert too), so it is safe to spawn on
            // a domainless, localhost/LAN, *or* public ACME nest alike.
            let floor_domain = nest_config.nest.domain.as_deref().filter(|d| !d.is_empty());
            let floor_label = floor_domain.unwrap_or("no domain (IP/identity-only nest)");
            app_state.spawn_scoped(fauna_nest::self_signed_cert::floor_renew_task(
                acme_config.acme_dir.clone(),
                floor_domain.map(str::to_string),
            ));
            tracing::info!("Self-signed TLS floor renew task started for {floor_label}");

            // At-risk renewal push (tls-certificates.md § C.4, mitigation 4). Only a
            // client-driven DNS-01 deployment (ACME HTTP-01 off) can't renew its own
            // cert, so the nudge stays gated on `!acme_enabled` AND a **real** public
            // domain — a localhost/IP/.local box or a domainless nest is happy on the
            // floor, never nag it (same predicate the mail auto-domain net uses) — and
            // a configured PushService (checked inside the task).
            if !acme_enabled
                && let Some(domain) = floor_domain
                && fauna_provisioning::probe::resolve_handle_domain(domain).is_public_dns_name
            {
                app_state.spawn_scoped(fauna_nest::cert_nudge::cert_at_risk_nudge_task(
                    app_state.clone(),
                    domain.to_string(),
                    acme_config.acme_dir.clone(),
                ));
                tracing::info!("At-risk TLS renewal push task started for {domain}");
            }
        }

        // Spawn the ACME cert-lifecycle task now that AppState (and its DB) exists.
        // It derives the desired SAN set (apex + `mail.<domain>` for each active mail
        // domain) from nest state and re-issues whenever the on-disk cert stops
        // covering that set or nears expiry. The cert-watcher above then seals + fans
        // the renewed chain out to the approved bridges (store_acme_material).
        if let Some((http01_config, challenge_state)) = acme_http01_runtime.clone() {
            // Web-content per-domain cert lifecycle: for each `active` custom web
            // domain AND each opted-in `<handle>.<node-domain>` subdomain (Slice 3),
            // issue an HTTP-01 cert into `acme_dir/<domain>/` and install it into the
            // SNI resolver; renew at <30 days; drop deregistered/opted-out names.
            // Shares the apex LE account
            // (account_dir = acme_dir) and the apex challenge listener
            // (challenge_state, keyed by token), but stays OUT of the apex SAN set
            // and the bridge seal-and-fan-out. Runs only when TLS is on (a resolver
            // exists) AND HTTP-01 ACME is enabled (account + challenge listener up).
            if let Some(resolver) = web_cert_resolver {
                app_state.spawn_scoped(fauna_nest::web_content::cert::web_cert_lifecycle_task(
                    app_state.db.clone(),
                    resolver,
                    challenge_state.clone(),
                    fauna_nest::web_content::cert::WebCertConfig {
                        acme_dir: http01_config.acme_dir.clone(),
                        directory_url: http01_config.directory_url.clone(),
                    },
                    // Opted-in subdomain certs are `<handle>.<apex>`; the loop
                    // forms them from `list_subdomain_enabled` × handles, reading
                    // the apex per-tick from the router's live `HostResolver`
                    // (seeded at boot from the resolved identity domain, swapped by
                    // `apply_primary_identity` on a post-boot claim) — so issuance
                    // follows the claim with no restart, exactly when routing does.
                    app_state
                        .host_resolver
                        .clone()
                        .expect("start_server always builds the HostResolver"),
                ));
            }

            app_state.spawn_scoped(fauna_nest::acme_http01::cert_lifecycle_task(
                http01_config,
                challenge_state,
                app_state.clone(),
                // The § B-IP bridge cert is installed straight into the resolver
                // by the lifecycle task: the cert watcher only watches the
                // *domain* cert's filenames, so nothing else would ever pick up
                // `ip-fullchain.pem`. `None` on a plain-HTTP nest — no listener
                // to serve a bridge on, so nothing is ordered.
                ip_bridge_resolver,
            ));
        }

        // Web-content custom-domain lifecycle: poll pending `web_domains` every
        // 5 min and advance `pending → verified → active` once the
        // `_fauna-verify.<domain>` TXT token resolves (via the shared public-
        // recursive DnsVerifier — the same resolver the DNS-management verify
        // surface uses), then reconcile the router's live `HostResolver`
        // custom-domain map against the `active` set — so a domain verified
        // post-boot starts routing in that same pass, and a deregistered one
        // stops, with no restart. Per-domain ACME issuance for `active` domains
        // is a separate lifecycle loop (web-content Slice 3); unlike this task
        // it only runs when TLS + HTTP-01 ACME are both on, which is why
        // routing cannot ride it.
        app_state.spawn_scoped(fauna_nest::web_content::domain::domain_lifecycle_task(
            app_state.db.clone(),
            app_state.dns_verifier.clone(),
            app_state
                .host_resolver
                .clone()
                .expect("start_server always builds the HostResolver"),
        ));

        tracing::info!("Fauna Node listening on {scheme}://{addr}");
        tracing::info!("Admin API available at {scheme}://{addr}/admin/api");
        // The *resolved* posture (client-set DB row wins over the `[nest]` seed), not a
        // CLI flag's echo. An unregistered actor is refused in every mode.
        {
            let (mode, cap) = *app_state.registration_mode.read().await;
            tracing::info!("Registration mode: {}", mode.as_wire_str());
            if let Some(cap) = cap {
                tracing::info!("Free-tier ceiling: {cap} users");
            }
            if mode != fauna_protocol::node_policy::RegistrationMode::Closed
                && app_state.handle_domain_if_set().is_none()
            {
                tracing::warn!(
                    "registration is open but the nest has no handle domain — \
                 self-service registration will have no domain to bind handles to"
                );
            }
        }

        // The private-side workers run on every nest and act only while the
        // resolved NAT mode is private; each pass takes its targets from the
        // users' own pairing rows (`private-mode.md` § Implementation status
        // today). Outbox: forward each author's posts to the nests their
        // `post_forward` rows name. Sync: namespace sync + mail relay per row.
        {
            let state2 = app_state.clone();
            app_state.spawn_scoped(async move {
                fauna_nest::outbox::run_outbox_worker(state2).await;
            });
            let state3 = app_state.clone();
            app_state.spawn_scoped(async move {
                fauna_nest::nest_sync_worker::run_sync_worker(state3).await;
            });
            tracing::info!("Outbox and namespace sync + mail relay workers started");
        }

        // Token store GC: clean up expired bearer tokens every 10 minutes
        {
            let token_store = token_store.clone();
            // Bulk-byte tokens (the WebDAV/byte-route `mint_bulk_byte_token` grants)
            // live in a store DISJOINT from the session TokenStore and were previously
            // never swept — the in-memory map grew unbounded until nest restart. Sweep it on the
            // same 10-min tick as the session tokens (both GC `expires_at`-expired rows).
            let bulk_byte_tokens = app_state.auth.bulk_byte_tokens.clone();
            // The challenge-nonce store has the same shape and worse exposure:
            // `ChallengeStore::gc` existed from the start but was **never
            // scheduled**, while its writer (`fauna.auth.challenge`) is pre-identity
            // and deliberately unthrottled — so any anonymous source could grow the
            // map until restart. An entry is otherwise removed only by a
            // *successful* consume, and an abandoned sign-in never consumes. Swept
            // on the same tick.
            // The seed-escrow and replacement-veto ceremonies' nonce stores
            // (identity-succession slice 2) are disjoint from the sign-in one
            // (one pool per ceremony) and swept the same way.
            let challenge_store = app_state.auth.challenge_store.clone();
            let escrow_challenge_store = app_state.auth.escrow_challenge_store.clone();
            let veto_challenge_store = app_state.auth.veto_challenge_store.clone();
            let age_nonce_store = app_state.auth.age_nonce_store.clone();
            // The same tick also lands due seed-initiated RecoveryKey replacements
            // (`identity-succession.md:37`): a 30-day window checked every 10 min
            // costs nothing and keeps the landing latency negligible next to the
            // window itself.
            let sweep_state = app_state.clone();
            app_state.spawn_scoped(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(600));
                loop {
                    interval.tick().await;
                    let removed = token_store.gc().await;
                    if removed > 0 {
                        tracing::info!("token GC: removed {removed} expired tokens");
                    }
                    let bulk_removed = bulk_byte_tokens.gc().await;
                    if bulk_removed > 0 {
                        tracing::info!("bulk-byte token GC: removed {bulk_removed} expired tokens");
                    }
                    let challenge_removed = challenge_store.gc().await
                        + escrow_challenge_store.gc().await
                        + veto_challenge_store.gc().await
                        + age_nonce_store.gc().await;
                    if challenge_removed > 0 {
                        tracing::info!("challenge GC: removed {challenge_removed} expired nonces");
                    }
                    let now = fauna_core::data::Timestamp::now_secs_or_zero();
                    match fauna_nest::recovery_handlers::land_due_replacements(&sweep_state, now)
                        .await
                    {
                        Ok((landed, cancelled)) if landed + cancelled > 0 => {
                            tracing::info!(
                                landed,
                                cancelled,
                                "recovery replacement sweep: pending windows resolved"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!("recovery replacement sweep failed: {e}");
                        }
                    }
                }
            });
        }

        // Rate-limiter bucket eviction (F4): the `bridge_rate_limit::Limiter`
        // DashMaps (federation / bridge / anonymous / claim / invite) otherwise grow
        // unboundedly — a flood of distinct keys (Sybil `nest_id`s + source IPs,
        // rotating credential strings) each mints a bucket that lingers past its
        // window. Sweep stale buckets every 60s (≈ one window) so the maps stay
        // bounded to recently-active sources.
        {
            let sweep_state = app_state.clone();
            app_state.spawn_scoped(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let evicted = sweep_state.federation_rate_limit.sweep()
                        + sweep_state.bridge_rate_limit.sweep()
                        + sweep_state.anonymous_rate_limit.sweep()
                        + sweep_state.claim_rate_limit.sweep()
                        + sweep_state.global_claim_rate_limit.sweep()
                        + sweep_state.invite_verify_rate_limit.sweep();
                    if evicted > 0 {
                        tracing::debug!("rate-limiter sweep: evicted {evicted} stale buckets");
                    }
                }
            });
        }

        // Background expiry task: periodically clean up stale contacts and knocks
        {
            let db = db.clone();
            let expiry_state = app_state.clone();
            app_state.spawn_scoped(async move {
                // `FAUNA_EXPIRY_INTERVAL_SECS` overrides the 1 h default — a test-only
                // tick accelerator (the e2e harness sets it to a few seconds so the
                // scheduled DKIM rotation-mint fires within a test). Harmless to
                // shorten: the contact/knock TTLs are 30/90 days and the soft-delete
                // and export-session GCs are 30 days, so every job but the due-gated
                // DKIM rotation stays a no-op at any cadence (the export orphan
                // reclaim is race-free against a live export at any cadence).
                // Production leaves it unset → 1 h.
                let expiry_interval_secs = std::env::var("FAUNA_EXPIRY_INTERVAL_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|&v| v > 0)
                    .unwrap_or(EXPIRY_INTERVAL_SECS);
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(expiry_interval_secs));
                loop {
                    interval.tick().await;
                    match db.expire_accepted_contacts(ACCEPTED_CONTACT_TTL_SECS).await {
                        Ok(n) if n > 0 => tracing::info!("expired {n} accepted contacts"),
                        Err(e) => tracing::warn!("contact expiry error: {e}"),
                        _ => {}
                    }
                    match db.expire_old_knocks(PENDING_KNOCK_TTL_SECS).await {
                        Ok(n) if n > 0 => tracing::info!("expired {n} old knocks"),
                        Err(e) => tracing::warn!("knock expiry error: {e}"),
                        _ => {}
                    }
                    // 30-day soft-delete GC for removed mail domains
                    // (`mail-multidomain.md` § After 30 days): hard-delete each row
                    // past its recovery window plus its per-domain DKIM/TLS blobs +
                    // disabled aliases.
                    match db.gc_expired_soft_deleted_mail_domains().await {
                        Ok(gone) if !gone.is_empty() => {
                            tracing::info!(
                                "GC'd {} expired soft-deleted mail domain(s): {gone:?}",
                                gone.len()
                            )
                        }
                        Err(e) => tracing::warn!("mail-domain GC error: {e}"),
                        _ => {}
                    }
                    // The MTA-STS advance (`mail-multidomain.md` § The advance): a
                    // domain past its 7-day `testing` window on a trusted mail name
                    // is stored `enforce`. The interval's first tick fires at once,
                    // so this is the boot pass too.
                    fauna_nest::mta_sts_advance::advance_mta_sts_modes(&expiry_state).await;
                    // Mailbox-export expiry (`mail-export.md` § Expiry, § Reclaim):
                    // unlink each expired session's blob and then delete its row,
                    // then collect any blob no row names. The per-actor entry
                    // points only hide expired rows; this is what removes them.
                    fauna_nest::mail_export_blobs::run_export_expiry_tick(
                        &db,
                        &expiry_state.config.nest.db_path,
                    )
                    .await;
                    // Scheduled DKIM rotation (`mail-multidomain.md` § Rotation), two
                    // composed halves on each tick:
                    //   1. mint — for every due domain with no rotation in flight,
                    //      provision a fresh `<YYYYMM>` key (sealed nest-side to the
                    //      approved MTA), so there is a newer selector to flip to;
                    //   2. auto-flip — activate any newer selector that has aged past
                    //      the 24 h peer-cache warmup, reusing the same flip primitive
                    //      `force_rotate_dkim` uses + pushing `config_changed` so the
                    //      bridge rebuilds live.
                    // Mint before flip so a freshly-due domain starts its warmup this
                    // tick; the flip lands a later tick once the new key has aged.
                    fauna_nest::bridge_routing_handlers::run_scheduled_dkim_rotation_mint(
                        &expiry_state,
                    )
                    .await;
                    fauna_nest::bridge_routing_handlers::run_scheduled_dkim_autoflip(&expiry_state)
                        .await;
                }
            });
        }

        // Deliverability blocklist self-check (mail-deliverability.md § Blocklist
        // self-check): a 24h sweep of the deployment's outbound IP against the DNSBL
        // set, default-on. Aligned to ~03:00 UTC + a per-deployment random offset in
        // [0, 60min) so the fleet's deployments don't query the DNSBLs in a
        // synchronized storm. The sweep no-ops when mail isn't enabled.
        {
            let blocklist_state = app_state.clone();
            app_state.spawn_scoped(async move {
                const DAY: u64 = 86_400;
                const TARGET_UTC_SECS: u64 = 3 * 3600; // 03:00 UTC
                let now = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
                let into_day = now % DAY;
                let to_target = if into_day < TARGET_UTC_SECS {
                    TARGET_UTC_SECS - into_day
                } else {
                    DAY - into_day + TARGET_UTC_SECS
                };
                let jitter = rand::random::<u64>() % 3600;
                tokio::time::sleep(std::time::Duration::from_secs(to_target + jitter)).await;
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(DAY));
                loop {
                    interval.tick().await;
                    fauna_nest::bridge_routing_handlers::run_scheduled_blocklist_self_check(
                        &blocklist_state,
                    )
                    .await;
                    // Same daily tick: prune `spam_training_history` rows past the
                    // admin-effective `mail.spam.training_history_retention_days`
                    // (`mail-spam.md` § Training-sample retention). A local DB
                    // delete with no DNS/timing requirement — riding the existing
                    // 03:00-UTC sweep avoids a second alignment/jitter block.
                    fauna_nest::bridge_routing_handlers::run_spam_training_history_gc(
                        &blocklist_state,
                    )
                    .await;
                }
            });
        }

        // Mail compaction worker (Plan 3 T3): 6h scheduled sweep over every
        // actor with mail data, honouring snapshot pins. Spawn before the
        // eviction task moves `app_state`.
        {
            let compaction_worker = fauna_nest::segments::CompactionWorker::new(
                app_state.clone(),
                std::time::Duration::from_secs(6 * 60 * 60), // 6h cadence (spec D7)
            );
            app_state.scope_handle(compaction_worker.spawn());
        }

        // In-process segment-backup coordinator: every enrolled owner's segments are
        // sealed under the `NestBackupKey` they granted and written to every
        // destination they registered — with no client and no agent awake
        // (`backup-restore.md` § Background Tasks; `message-segment-store.md`
        // § Cross-location backup protocol). A no-op on a nest where nobody has
        // enrolled, which is every nest until a client runs the enroll flow. The
        // cadence is the SHARED constant the client arm uses, so the two cannot
        // drift while both exist (the client arm retires at the slice-5 flip).
        {
            let backup_worker = fauna_nest::segment_backup::NestBackupWorker::new(
                app_state.clone(),
                fauna_sync_engine::segment_backup::PERIODIC_INTERVAL,
            );
            app_state.scope_handle(backup_worker.spawn());
        }

        // Custody-hosting pump (the custodian-nest runtime, stage b): every
        // tick, pull every registered custody's covered planes from the
        // owner's nest — what makes a nest-anchored custody hold with no host
        // device running. First tick at boot resumes holds after a restart.
        {
            let custody_worker = fauna_nest::custody_hosting_worker::CustodyHostingWorker::new(
                app_state.clone(),
                fauna_nest::custody_hosting_worker::PULL_INTERVAL,
            );
            app_state.scope_handle(custody_worker.spawn());
        }

        // Eviction background task: advance warning → suspended → deleted
        fauna_nest::eviction::spawn_eviction_task(Arc::clone(&app_state));

        // Trend re-decay sweep: every ~15 min, re-score live `trending` rows against
        // wall-clock and withdraw the fully-decayed ones (`trending.md` § Local
        // velocity). New acts between sweeps are handled by the engagement-event
        // transition hook; this owns only the time-driven decay.
        fauna_nest::trend_sweeper::spawn_trend_sweeper(Arc::clone(&app_state));

        // Membership-lapse reconcile: hourly (and at boot, on the first tick) flip
        // every expired member back to their link's `lapse_tier` — the time-driven
        // half of Pillar 4 Rail C step 3 the webhook-ingress reconcile can't see
        // (monetization.md § Pillar 4). Renewals/refunds reconcile their buyer live.
        fauna_nest::membership_lapse::spawn_membership_lapse_sweeper(Arc::clone(&app_state));

        // ATProto unreferenced-upload sweep: hourly (and at boot, on the first
        // tick) retire `atproto_blobs` rows no record ever named, past the
        // reference window (atproto-pds-full.md § Nest state schema). Rows only —
        // dropping one is what un-pins its bytes for the box-wide GC.
        fauna_nest::atproto_blob_sweeper::spawn_atproto_blob_sweeper(Arc::clone(&app_state));

        // Post-delete re-drive: hourly (and at boot, on the first tick) chase the
        // outward delete legs a first attempt stranded — Bluesky cross-posts whose
        // `deleteRecord` gave up, replicas a disconnected worker missed (`feed.md`
        // § Post deletion → Propagation). The worker's connect replays its half too.
        fauna_nest::post_delete_redrive::spawn_post_delete_redrive_sweeper(Arc::clone(&app_state));

        // Feature-gate usage prune: daily (and at boot, on the first tick) drop the
        // usage buckets that have aged past the largest window any policy can name
        // (dynamic-features.md § Usage accounting). Bounds the table; changes no
        // verdict, since a bucket past the horizon contributes to no window sum.
        fauna_nest::feature_gate::spawn_usage_pruner(Arc::clone(&app_state));

        // The region tier's pull-based refresh (dynamic-features.md § The region
        // tier; region-blocking.md § Publication and signing). Its first tick is the
        // at-boot re-fold of whatever artifact is stored, so it is deliberately not
        // skipped. On a deployment that declares no region — every deployment while
        // the curated registry is empty — a tick reads one row and returns without
        // touching the network.
        fauna_nest::region_tier::spawn_region_refresh(Arc::clone(&app_state));

        // Identity-succession pull: hourly (and at boot, on the first tick) ask the
        // home nest of every remote identity this nest holds residue about whether it
        // has been superseded. The backstop for the push leg — a nest that was down
        // when a peer pushed, or whose address the peer never recorded, would
        // otherwise keep accepting a stolen key's content forever
        // (`identity-succession.md:81` § Propagation).
        fauna_nest::succession_pull::spawn_succession_pull(Arc::clone(&app_state));

        // Pending actions executor: process ready actions every 60 seconds.
        // Needs the full AppState — finalizing a deletion revokes the actor's tokens
        // and closes its live WebSockets, not just its DB rows.
        fauna_nest::pending_actions::start_executor(Arc::clone(&app_state));

        // Snapshot + GC schedulers: only start if backup is configured
        if let Some(ref svc) = backup_service {
            // Cadence + nest-wide quiet period live on the scheduler module, so
            // the `test-hooks` run-now route builds the SAME scheduler this
            // does rather than a second, drifting copy of the numbers.
            let scheduler = Arc::new(fauna_nest::backup::scheduler::SnapshotScheduler::new(
                svc.clone(),
                fauna_nest::backup::scheduler::CHECK_INTERVAL,
                fauna_nest::backup::scheduler::DEFAULT_QUIET_SECS,
            ));
            app_state.scope_handle(scheduler.spawn());

            let gc_scheduler = Arc::new(fauna_nest::backup::gc_scheduler::GcScheduler::new(
                svc.clone(),
                app_state.post_segments.clone(),
                std::time::Duration::from_secs(21600), // every 6 hours
                1800,                                  // 30 min grace period
            ));
            app_state.scope_handle(gc_scheduler.spawn());
        }

        // Wait for a shutdown signal, then flush the database before exiting.
        let shutdown = async {
            #[cfg(unix)]
            {
                fauna_nest::unix_signal::shutdown_signal().await;
            }
            #[cfg(windows)]
            {
                // Ctrl+Break as well as Ctrl+C: a parent that started this nest in
                // its own process group (where Ctrl+C is disabled) can only reach
                // it with a Ctrl+Break, which is the one console event targetable
                // at a single group — Windows' nearest thing to a SIGTERM.
                let mut ctrl_break =
                    tokio::signal::windows::ctrl_break().expect("failed to listen for Ctrl+Break");
                tokio::select! {
                    r = tokio::signal::ctrl_c() => {
                        r.expect("failed to listen for Ctrl+C");
                        tracing::info!("Received Ctrl+C, shutting down gracefully");
                    }
                    _ = ctrl_break.recv() => {
                        tracing::info!("Received Ctrl+Break, shutting down gracefully");
                    }
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                tokio::signal::ctrl_c()
                    .await
                    .expect("failed to listen for Ctrl+C");
                tracing::info!("Received Ctrl+C, shutting down gracefully");
            }
        };

        let restart = app_state.serve_restart.clone();
        tokio::select! {
            _ = shutdown => break 'serve (handle, app_state, ws_state),
            _ = restart.notified() => {
                tracing::info!(
                    "serving-generation restart requested (deployment-seed rotation) — \
                     tearing down and re-entering start_server"
                );
                app_state.teardown_serving_generation(handle).await;
                continue 'serve;
            }
        }
    };

    // Graceful shutdown (transport.md § Graceful shutdown): stop accepting new
    // connections, then tell every connected client we're going away with WS
    // 1001 so it reconnects promptly with backoff — never 1000, which would
    // stop its reconnect loop on every redeploy — then drain and flush.
    // Lifted lib-side so
    // `bins/fauna-nest/tests/` (which links the lib, never this bin) can
    // drive the exact sequence and mutation-test it.
    fauna_nest::graceful_shutdown(handle, &ws_state, &db).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FAUNA_DATA_DIR` unset (or empty / whitespace-only) ⇒ no relocation — the
    /// config/CLI `db_path` stands (back-compat for every non-system-data-dir
    /// deployment: Docker, dev, bare-metal, the as-built per-user macOS install).
    #[test]
    fn data_dir_override_none_when_unset_or_blank() {
        assert_eq!(data_dir_db_path_override(None), None);
        assert_eq!(data_dir_db_path_override(Some("")), None);
        assert_eq!(data_dir_db_path_override(Some("   ")), None);
    }

    /// A set `FAUNA_DATA_DIR` relocates `db_path` to `<dir>/nest.db` — the system
    /// data-root the macOS `_fauna` LaunchDaemon points the nest at. The whole data
    /// layout (blobs, identity key, sidecar tokens, factory-reset) then derives from
    /// `db_path.parent()` = the data-root.
    #[test]
    fn data_dir_override_relocates_db_path_to_node_db() {
        assert_eq!(
            data_dir_db_path_override(Some("/Library/Application Support/Fauna")).as_deref(),
            Some("/Library/Application Support/Fauna/nest.db")
        );
        // Trailing whitespace tolerated (the value is trimmed before joining).
        assert_eq!(
            data_dir_db_path_override(Some("/srv/fauna\n")).as_deref(),
            Some("/srv/fauna/nest.db")
        );
    }

    #[test]
    fn cli_no_args_parses() {
        // No args = default serve, should not error
        let cli = Cli::try_parse_from(["fauna-nest"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.serve.bind, None);
    }

    #[test]
    fn cli_serve_subcommand() {
        let cli = Cli::try_parse_from([
            "fauna-nest",
            "serve",
            "--bind",
            "0.0.0.0:8080",
            "--db",
            "/data/nest.db",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Serve(boxed_args)) => {
                let args = *boxed_args;
                assert_eq!(
                    args.bind,
                    Some("0.0.0.0:8080".parse::<SocketAddr>().unwrap())
                );
                assert_eq!(args.db, Some("/data/nest.db".to_string()));
            }
            _ => panic!("expected Serve subcommand"),
        }
    }

    #[test]
    fn cli_serve_flags_without_subcommand() {
        // Flags without a subcommand (the container entrypoint's form)
        let cli = Cli::try_parse_from([
            "fauna-nest",
            "--bind",
            "0.0.0.0:8080",
            "--db",
            "/data/nest.db",
        ])
        .unwrap();
        assert!(cli.command.is_none());
        assert_eq!(
            cli.serve.bind,
            Some("0.0.0.0:8080".parse::<SocketAddr>().unwrap())
        );
        assert_eq!(cli.serve.db, Some("/data/nest.db".to_string()));
    }

    /// The registration posture is nest state set from a client, never a flag
    /// (`principles.md` § One configuration surface). If someone re-adds one of
    /// these, this test fails and points them at `fauna.admin.set_registration_mode`.
    #[test]
    fn registration_posture_has_no_cli_flags() {
        for flag in [
            "--registration-open",
            "--registration-invite-required",
            "--max-free-users",
            "--require-registration",
            "--no-require-registration",
        ] {
            assert!(
                Cli::try_parse_from(["fauna-nest", flag]).is_err(),
                "{flag} must not exist: the registration posture is client-set nest \
                 state (fauna.admin.set_registration_mode), not a CLI flag"
            );
        }
    }

    /// The worker flags do not ship: nothing in production is a worker, so a
    /// `--worker-key` / `--worker-allow-ip` flag would authorize a peer that does
    /// not exist and be the documented way to attach one. The authorization
    /// surface arrives with the worker itself as in-app enrollment
    /// (`nest/worker.md` § What Is the Worker?).
    #[test]
    fn worker_flags_do_not_exist() {
        let key = "00".repeat(32);
        for args in [
            ["fauna-nest", "--worker-key", key.as_str()],
            ["fauna-nest", "--worker-allow-ip", "10.0.0.1"],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "{} must not exist: worker authorization is in-app enrollment, not a CLI flag",
                args[1]
            );
        }
    }

    /// `--cors-origin` must reach the running nest even when a `--config` file is
    /// loaded. Until this pin, the flag was wired ONLY into the no-config-file
    /// branch of the config build, so every deployment that passes `--config` —
    /// which is all of them, the e2e harness included — dropped it silently and
    /// the box kept the `DEFAULT_CORS_ORIGIN`-only posture. `registry.md`
    /// § Health-poll CORS calls this flag the artifact's boot seed, so a seed
    /// that evaporates whenever a config file exists is the flag not working at
    /// all. Precedence follows the file's standing rule ("CLI args take
    /// precedence over config file values") — and an *unset* flag must leave the
    /// config's own seed alone, which is what the Docker artifact relies on
    /// (`docker/entrypoint.sh` seeds `cors_origins` into `/data/nest.toml`).
    #[test]
    fn cors_origin_flag_wins_over_config_seed_but_only_when_given() {
        let cfg = vec!["https://app.fauna.social".to_string()];

        // No flag ⇒ the config seed stands (the Docker artifact's path).
        assert_eq!(resolve_cors_origins_seed(vec![], cfg.clone()), cfg);

        // Flag given ⇒ it replaces the config seed wholesale.
        let cli = vec![
            "http://127.0.0.1:45123".to_string(),
            "https://fauna.social".to_string(),
        ];
        assert_eq!(resolve_cors_origins_seed(cli.clone(), cfg.clone()), cli);

        // Neither ⇒ empty, which `origin_allowed` collapses to the built-in
        // default-origin-only posture. The fresh-nest default is untouched.
        assert!(resolve_cors_origins_seed(vec![], vec![]).is_empty());
    }

    #[test]
    fn cli_cors_origins_multiple() {
        let cli = Cli::try_parse_from([
            "fauna-nest",
            "--cors-origin",
            "http://localhost:5173",
            "--cors-origin",
            "https://fauna.social",
        ])
        .unwrap();
        assert!(cli.command.is_none());
        assert_eq!(
            cli.serve.cors_origins,
            vec!["http://localhost:5173", "https://fauna.social"]
        );
    }

    #[test]
    fn reserved_handle_flag_is_retired() {
        // The reserved list is a constant, never a knob; re-adding the flag
        // re-opens the wipe bug this pin documents (see the Cli struct note).
        assert!(Cli::try_parse_from(["fauna-nest", "--reserved-handle", "admin"]).is_err());
    }

    /// The CA and the account contact are constants, and the ACME domain comes from
    /// the claim (`[nest].domain`) — none of these is a flag
    /// (`tls-certificates.md` § ACME settings — constants, not choices).
    #[test]
    fn acme_contact_staging_and_domain_flags_are_retired() {
        for args in [
            ["fauna-nest", "--acme-email", "a@b"],
            ["fauna-nest", "--acme-domain", "nest.example.com"],
            ["fauna-nest", "--acme-staging", "true"],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "{} must not exist: the CA and contact are constants",
                args[1]
            );
        }
        assert!(Cli::try_parse_from(["fauna-nest", "--acme-staging"]).is_err());
    }

    fn test_acme_config(
        acme_dir: std::path::PathBuf,
        domain: &str,
    ) -> fauna_nest::acme::AcmeConfig {
        fauna_nest::acme::AcmeConfig {
            domain: domain.to_string(),
            acme_dir,
            directory_url: None,
        }
    }

    /// The standalone-binary default bind (`--bind`/config `listen` both unset)
    /// must listen on **all interfaces** (`0.0.0.0`), not loopback — a nest is
    /// reached identically from `localhost`, the LAN, and the WAN, with no
    /// distinction by client origin
    /// (`docs/goal/architecture/installers/windows.md` § Network-reachable nest;
    /// `docs/goal/architecture/nest/common.md` § CLI flags). Guards against a
    /// regression back to the `127.0.0.1` loopback-only default.
    #[test]
    fn default_listen_addr_is_all_interfaces() {
        let addr = default_listen_addr();
        assert!(
            addr.ip().is_unspecified(),
            "the standalone default must bind all interfaces (0.0.0.0), got {addr}"
        );
        // The unprivileged dev binary keeps the canonical internal nest port
        // `3000` (`:443` is privileged on Linux; production fronts it with the
        // `:443` SNI router / Windows SCM). Mirrors `config/default.toml`.
        assert_eq!(
            addr.port(),
            3000,
            "the standalone default keeps the canonical internal port 3000, got {addr}"
        );
    }

    /// A **domainless** nest (no configured domain, ACME off — the default) must
    /// still come up on TLS: `prepare_listener_tls` writes the always-live
    /// self-signed floor and returns a real resolver, so a keypair-identified
    /// nest serves HTTPS from boot before any domain is added from a client
    /// (tls-certificates.md § A; domains-and-tls-bootstrap.md). Before this track
    /// the gate required a configured domain, so a domainless nest fell through to
    /// the plain-HTTP `(None, None)` branch.
    #[test]
    fn prepare_listener_tls_serves_floor_when_domainless() {
        let dir = tempfile::tempdir().expect("tempdir");
        let acme_config = test_acme_config(dir.path().to_path_buf(), "");
        let (tls_config, resolver) = prepare_listener_tls(&acme_config, false, None, false);
        assert!(
            tls_config.is_some(),
            "a domainless nest must serve TLS on the always-live floor"
        );
        assert!(resolver.is_some(), "the floor resolver must be wired");
        assert!(
            dir.path().join(fauna_nest::acme::CERT_FILENAME).exists()
                && dir.path().join(fauna_nest::acme::KEY_FILENAME).exists(),
            "the domainless floor cert + key were written to the acme dir"
        );
    }

    /// With ACME enabled + a domain, the unconditional floor is written *first*,
    /// so `prepare_listener_tls` returns a real resolver serving the floor cert
    /// immediately — never the certless *pending* resolver. This is what lets a
    /// non-completable ACME order (LAN box on a real hostname, no public :80) stop
    /// stranding the listener: the floor is already up and ACME self-heals it.
    #[test]
    fn prepare_listener_tls_serves_floor_first_under_acme() {
        let dir = tempfile::tempdir().expect("tempdir");
        let acme_config = test_acme_config(dir.path().to_path_buf(), "example.com");
        let (tls_config, resolver) =
            prepare_listener_tls(&acme_config, true, Some("example.com"), false);
        assert!(
            tls_config.is_some(),
            "the floor serves TLS immediately even under ACME"
        );
        assert!(resolver.is_some());
        // A floor cert on disk proves the bootstrap ran (the pending-resolver
        // branch writes none) — i.e. we took the real-cert path, not the pending one.
        assert!(
            dir.path().join(fauna_nest::acme::CERT_FILENAME).exists(),
            "the unconditional floor bootstrap wrote a cert under ACME"
        );
    }

    /// The test/diagnostic-only `force_plain_http` escape
    /// (`FAUNA_INSECURE_DISABLE_TLS`) serves plain HTTP on the **nest's own API
    /// listener** — `(None, None)` — so the tier_3 binary-e2e suite stays on plain
    /// HTTP without a cross-platform https sweep. It writes no floor cert *here*:
    /// the always-live floor the in-process mail bridges fetch+serve (over their
    /// own CalDAV/IMAP TLS, regardless of the nest API's mode —
    /// caldav-imap-any-locator) is written universally by `start_server`
    /// (`self_signed_cert::ensure_floor_present`), so under this escape
    /// `prepare_listener_tls` is a pure no-op. Never set by any real deployment
    /// (domains-and-tls-bootstrap.md § Test posture). `test-hooks`-gated because
    /// the escape it exercises is now compiled out of a release build.
    #[cfg(feature = "test-hooks")]
    #[test]
    fn prepare_listener_tls_force_plain_http_skips_nest_tls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let acme_config = test_acme_config(dir.path().to_path_buf(), "");
        let (tls_config, resolver) = prepare_listener_tls(&acme_config, false, None, true);
        assert!(
            tls_config.is_none(),
            "plain-http escape serves no TLS on the nest's own listener"
        );
        assert!(resolver.is_none());
        assert!(
            !dir.path().join(fauna_nest::acme::CERT_FILENAME).exists(),
            "prepare_listener_tls writes no floor in plain-HTTP mode \
             (start_server owns the universal floor write)"
        );
    }
}
