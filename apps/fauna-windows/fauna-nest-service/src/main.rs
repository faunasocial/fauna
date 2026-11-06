//! FaunaNest Windows Service entry point.
//!
//! Modes:
//!   --service     Run under Windows SCM (default when invoked by SCM)
//!   --foreground  Run in foreground for debugging (Ctrl-C to stop)
//!   --port        Override HTTP listen port (default: 7450)
//!   --data-dir    Override data directory (default: %PROGRAMDATA%\Fauna\nest)

mod config;
mod service;

use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser)]
#[command(name = "fauna-nest-svc", about = "FaunaNest Windows Service")]
struct Args {
    /// Run as a Windows Service (SCM mode).
    #[arg(long)]
    service: bool,

    /// Run in foreground for debugging.
    #[arg(long)]
    foreground: bool,

    /// Override HTTP listen port (default: 7450).
    ///
    /// Foreground/debug only, and NOT a user or admin knob (principles.md §
    /// One configuration surface): SCM service mode ignores every flag and
    /// binds the fixed serving port, and the user-chosen port lives in nest
    /// state / `device.toml`. The flag exists as a TEST SEAM — the
    /// `--foreground` e2e nest (`tests/e2e-unified/helpers/windows_caldav_nest.py`)
    /// passes a free ephemeral loopback port so parallel runs never collide —
    /// so it stays; 7450 is only the foreground fallback.
    #[arg(long, default_value_t = 7450)]
    port: u16,

    /// Override data directory (default: %PROGRAMDATA%\Fauna\nest).
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,
}

fn main() -> anyhow::Result<()> {
    // The commit this binary was compiled from, handed to the embedded nest
    // before it serves health (`fauna_nest::build_identity` owns why).
    fauna_nest::build_identity::stamp(fauna_build_commit::commit!());
    let args = Args::parse();

    // Install the shared logging stack: the in-memory ring (backs the admin Logs
    // view / `fauna.admin.logs`) + a daily-rolling file `<data_dir>/logs/fauna.log.<date>`
    // + stderr, all gated by `RUST_LOG` (default `info`). The rolling FILE is
    // essential here, and is why this binary uses `fauna_log::init` while the
    // standalone `fauna-nest` deliberately does not: the standalone nest's stdout
    // is captured by journald / Docker, but THIS binary runs under the Windows SCM
    // which DISCARDS stdout/stderr — so without an on-disk log a startup/bind
    // failure is completely invisible (a silent service that "runs" but serves
    // nothing). The guard must stay alive for the whole process so the non-blocking
    // file writer keeps running; it drops (flush + stop) when `main` returns.
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(config::default_data_dir);
    let _log_guard = fauna_log::init(&data_dir);

    if args.service {
        // SCM mode
        tracing::info!("starting FaunaNest in service mode");
        #[cfg(windows)]
        {
            service::run_as_service()?;
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("--service mode is only supported on Windows");
        }
    } else {
        // Foreground mode (default when --service is not set)
        tracing::info!("starting FaunaNest in foreground mode");
        let bind = SocketAddr::from(([127, 0, 0, 1], args.port));
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(service::run_service_loop(bind, args.data_dir.as_deref()))?;
    }

    Ok(())
}
