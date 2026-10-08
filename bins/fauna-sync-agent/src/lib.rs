//! `fauna-sync-agent` — the cross-platform per-user sync+backup agent library.
//!
//! Generalized from the windows-only sync service this crate replaced
//! (`sync-agent.md`): the platform-neutral core lives here as a `lib`, and the
//! one `fauna-sync-agent` binary — every platform's, the shipped windows
//! `fauna-sync-agent.exe` included — is a thin `main()` calling
//! [`run_main`]. The cfapi host + windows pipe transport stay behind
//! `cfg(windows)`; the unix-socket transport is served under `cfg(unix)`.
//!
//! [`run_main`] flags:
//!   --foreground     (accepted; always runs in foreground — kept for dev compat)
//!   --data-dir       Override data directory
//!   --pipe-name      Override named pipe path
//!   --cleanup-roots  Unregister every persistent cfapi sync-root registration
//!                    and exit; invoked by the MSI uninstall CA (see [`run_main`])

mod account_host;
mod bearer;
mod bridge;
#[cfg(windows)]
mod cfapi_host;
mod config;
mod content_keys;
mod credentials;
mod custodian;
mod engine_driver;
#[cfg(target_os = "linux")]
mod fuse_host;
mod path_map;
mod peer_files;
mod pin_reaction;
mod pipe_server;
mod push_arm;
mod renewal;
mod service;
mod state;
/// Install-scoped TLS trust (`security.md` § Pin custody across processes).
/// `pub` so the agent's own pin-custody integration test can drive the same
/// startup call the agent makes; nothing outside the crate consumes it.
pub mod trust;
mod versions;

#[cfg(test)]
mod producer_integration;

#[cfg(all(test, windows))]
mod pipe_transport_integration;

#[cfg(all(test, windows))]
mod cfapi_live_integration;

// The linux twin: the agent's own on-demand host over a REAL FUSE mount. Opt-in
// (`fuse-live`) because it needs `/dev/fuse` and the distro's `fusermount3`, which
// a sandboxed build box may not offer (see Cargo.toml `[features]`).
#[cfg(all(test, target_os = "linux", feature = "fuse-live"))]
mod fuse_live_integration;

// Opt-in tier_3 harness: the Version-history + Restore path against a REAL
// in-process nest. Gated on the `tier3-nest` feature so the default test loop
// never builds `fauna-nest` (see Cargo.toml `[features]`).
#[cfg(all(test, windows, feature = "tier3-nest"))]
mod versions_tier3;

// Opt-in tier_3 harness: the cfapi **byte-plane** half of Restore — versions
// produced via the real upload path against a REAL in-process nest chunk store,
// then re-hydrated from the re-pointed manifest (`file-sync.md` § Restore, the
// on-demand byte round-trip). Same `tier3-nest` gate as `versions_tier3` (pulls
// `fauna-nest`); see that module's docs.
#[cfg(all(test, windows, feature = "tier3-nest"))]
mod restore_byteplane_tier3;

use clap::Parser;

#[derive(Parser)]
#[command(name = "fauna-sync-agent", about = "FaunaSync per-user sync agent")]
struct Args {
    /// Run in foreground (accepted for dev/CI compat; agent always runs in foreground).
    #[arg(long)]
    foreground: bool,

    /// Override data directory (default: the per-OS production data root —
    /// see `SyncPaths::base_dir`).
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,

    /// Override named pipe path (default: per-user \\.\pipe\fauna-sync.<SID>).
    #[arg(long)]
    pipe_name: Option<String>,

    /// Unregister every persistent Fauna cfapi sync-root registration (filter +
    /// shell) on this machine and exit — never starts the agent. The MSI's
    /// `CleanSyncRoots` uninstall custom action runs this so uninstalling Fauna
    /// never leaves Explorer rendering a ghost "Fauna – <set>" cloud location
    /// (`docs/goal/behavior/file-sync.md` § Per-file sync-status display).
    #[arg(long)]
    cleanup_roots: bool,
}

/// Entry point for every platform shell (the `fauna-sync-agent` binary and the
/// windows `fauna-sync-agent.exe` shim). Parses args, installs the shared logging
/// stack, and runs the agent loop to shutdown.
pub fn run_main() -> anyhow::Result<()> {
    // A mount's crash guard: the agent re-executes itself in this mode at every
    // linux on-demand mount (`fuse_host::CrashGuard`). Before argument parsing
    // and logging — the guard opens nothing it does not need.
    #[cfg(target_os = "linux")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(fuse_host::GUARD_ARG)) {
        return fuse_host::run_guard_from_env();
    }
    let args = Args::parse();

    // Install the shared logging stack: the in-memory ring + a daily-rolling file
    // `<data_dir>/logs/fauna.log.<date>` + stderr, all gated by `RUST_LOG` (default
    // `info`). The rolling FILE is essential here: the shipped agent is a Windows
    // GUI-subsystem binary launched per-user at logon (no console → stderr is
    // discarded), so without an on-disk log a startup/provision failure is invisible
    // — the same reason `fauna-nest-service` (under the SCM, which likewise discards
    // stderr) uses `fauna_log::init`. Resolve the data root exactly as `run_agent`
    // does. The guard must stay alive for the whole process: dropping it on `main`
    // return flushes + stops the non-blocking file writer.
    let data_dir = config::SyncPaths::new(args.data_dir.clone()).base_dir();
    let _log_guard = fauna_log::init(&data_dir);

    if args.cleanup_roots {
        #[cfg(windows)]
        {
            let removed = cfapi_host::unregister_all_shell_sync_roots();
            tracing::info!(
                removed,
                "uninstall: cleaned up persistent sync-root registrations"
            );
        }
        #[cfg(not(windows))]
        {
            tracing::warn!("--cleanup-roots is a no-op off Windows");
        }
        return Ok(());
    }

    // Resolve the pipe name: --pipe-name wins; otherwise derive the per-user
    // name from the calling process's token SID (Windows) or the fixed dev
    // fallback (off-Windows) so each user's agent binds a distinct pipe.
    let pipe_name: String = match args.pipe_name {
        Some(p) => p,
        None => fauna_ipc::sync::current_user_pipe_name()
            .map_err(|e| anyhow::anyhow!("could not resolve per-user pipe name: {e}"))?,
    };

    tracing::info!("starting FaunaSync per-user sync agent");
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(service::run_agent(&pipe_name, args.data_dir.as_deref()))?;

    Ok(())
}
