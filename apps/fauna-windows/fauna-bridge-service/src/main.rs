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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

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
