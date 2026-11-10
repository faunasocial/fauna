//! FaunaBridge Windows Service entry point.
//!
//! Modes:
//!   --service     Run under Windows SCM (default when invoked by SCM)
//!   --foreground  Run in foreground for debugging (Ctrl-C to stop)
//!   --data-dir    Override data directory
//!
//! The service serves no IPC of its own: it supervises the Go `fauna-mail-bridge`
//! MDA child and nothing else (`installers/windows.md` § Services).

mod device;
mod mda_supervisor;
mod service;

use clap::Parser;

#[derive(Parser)]
#[command(name = "fauna-bridge-svc", about = "FaunaBridge Windows Service")]
struct Args {
    /// Run as a Windows Service (SCM mode).
    #[arg(long)]
    service: bool,

    /// Run in foreground for debugging.
    #[arg(long)]
    foreground: bool,

    /// Override data directory (default: %PROGRAMDATA%\Fauna\bridge).
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,
}

/// The bridge data dir this service owns: `--data-dir` when given, else
/// `%PROGRAMDATA%\Fauna\bridge` (the dir [`mda_supervisor::resolve`] hands the
/// MDA). SCM mode passes no `--data-dir`, so the default is resolved here.
fn bridge_data_dir(override_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    override_dir
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| mda_supervisor::resolve_dirs(&fauna_ipc::device::programdata_base()).0)
}

/// Install the shared logging stack ([`fauna_log::init`]) rooted at the bridge
/// dir: the ring, the size-capped file `<bridge dir>/logs/fauna.log.<date>` and
/// stderr, gated by `RUST_LOG` (default `info`). The file is essential here, as
/// it is for `fauna-nest-service`: under the SCM, which DISCARDS stdout and
/// stderr, it is this service's only persistent log, and it also holds the Go
/// MDA child's output, which `fauna_mda_supervisor` pipes into tracing
/// (`observability.md` § Persistence & privacy). Hold the guard for the whole
/// process: dropping it flushes and stops the non-blocking file writer.
fn init_logging(bridge_dir: &std::path::Path) -> Option<fauna_log::LogGuard> {
    fauna_log::init(bridge_dir)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let _log_guard = init_logging(&bridge_data_dir(args.data_dir.as_deref()));

    if args.service {
        // SCM mode
        tracing::info!("starting FaunaBridge in service mode");
        #[cfg(windows)]
        {
            service::run_as_service()?;
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("--service mode is only supported on Windows");
        }
    } else if args.foreground || !args.service {
        // Foreground mode (also the default)
        tracing::info!("starting FaunaBridge in foreground mode");
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(service::run_service_loop(args.data_dir.as_deref()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The service's only persistent log is its bounded `fauna_log` file under
    /// the bridge dir it owns — the SCM discards stdout and stderr — and both
    /// its own lines and the MDA child's captured output (which
    /// `fauna_mda_supervisor` re-emits under the `fauna_mail_bridge` target)
    /// reach it. (The one test in this binary that installs the
    /// process-global subscriber.)
    #[test]
    fn init_logging_writes_the_bounded_file_under_the_bridge_dir() {
        let dir = std::env::temp_dir().join(format!(
            "fauna-bridge-service-log-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = init_logging(&bridge_data_dir(Some(&dir)));
        assert!(guard.is_some(), "the file-backed subscriber must install");
        tracing::info!("bridge-service-log-marker");
        tracing::info!(target: "fauna_mail_bridge", "mda-child-log-marker");
        drop(guard); // flushes the non-blocking file writer

        let logs = dir.join("logs");
        let text: String = std::fs::read_dir(&logs)
            .unwrap_or_else(|e| panic!("no log dir at {logs:?}: {e}"))
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        for marker in ["bridge-service-log-marker", "mda-child-log-marker"] {
            assert!(
                text.contains(marker),
                "{marker} must land in <bridge dir>/logs/fauna.log.*: {text:?}"
            );
        }
    }
}
