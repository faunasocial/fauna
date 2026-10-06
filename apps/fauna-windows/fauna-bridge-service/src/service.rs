//! Windows SCM (Service Control Manager) integration for FaunaBridge.
//!
//! Registers as a Windows Service named "FaunaBridge", handles Stop/Interrogate
//! control events, and dispatches to the main service loop.

#[cfg(windows)]
const SERVICE_NAME: &str = "FaunaBridge";

/// Main service loop — loads device config, supervises the Go MDA child.
pub async fn run_service_loop(data_dir: Option<&std::path::Path>) -> anyhow::Result<()> {
    tracing::info!(?data_dir, "bridge service loop starting");

    // Load device config (shared across all Fauna services)
    let device_config = if let Some(dir) = data_dir {
        crate::device::load_device_config_from(&dir.join("device.toml"))
    } else {
        crate::device::load_device_config()
    };

    if let Some(ref dc) = device_config {
        tracing::info!(
            nest_url = %dc.nest_base_url(),
            "device config available; bridge can connect to nest"
        );
    } else {
        tracing::info!("no device config; bridge will start when device registers");
    }

    // `nest_endpoint` is the local loopback nest the MDA dials over WS-RPC (the
    // MDA's client upgrades the scheme — `http://` → `ws://`, `https://` →
    // `wss://` — and skips TLS verification for the loopback self-signed floor,
    // so `https://127.0.0.1:443` works).
    let nest_endpoint = device_config
        .as_ref()
        .map(|d| d.nest_base_url())
        .unwrap_or_else(|| "https://127.0.0.1:443".to_string());

    // Supervise the Go `fauna-mail-bridge` MDA: spawn it as a monitored child when
    // the admin has enabled mail and/or CalDAV (read from the nest's enable flag
    // files), restart it on exit, and stop it when both are off. First Windows
    // service to spawn a child; see `crate::mda_supervisor`.
    let mda_supervisor = crate::mda_supervisor::resolve(nest_endpoint);

    // Run until shutdown: Ctrl-C in foreground; in service mode the SCM Stop drops
    // this whole future. The MDA supervisor loop runs concurrently — when this
    // future is dropped or returns, its child is reaped via `kill_on_drop`.
    tokio::select! {
        r = tokio::signal::ctrl_c() => {
            r?;
            tracing::info!("ctrl-c received, shutting down");
        }
        _ = mda_supervisor.run() => {
            tracing::warn!("MDA supervisor loop returned unexpectedly");
        }
    }

    Ok(())
}

/// Run as a Windows Service via SCM dispatcher.
///
/// The registration/status-transition ceremony is shared with FaunaNest —
/// see [`fauna_ipc::scm_service::run_scm_service_main`]; FaunaBridge always
/// reports `Win32(0)` even on a loop error (unlike FaunaNest, which
/// distinguishes an AddrInUse bind failure).
#[cfg(windows)]
pub fn run_as_service() -> anyhow::Result<()> {
    use windows_service::{define_windows_service, service::ServiceExitCode, service_dispatcher};

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<std::ffi::OsString>) {
        fauna_ipc::scm_service::run_scm_service_main(SERVICE_NAME, run_service_loop(None), |_| {
            ServiceExitCode::Win32(0)
        });
    }

    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| anyhow::anyhow!("failed to start service dispatcher: {e}"))?;

    Ok(())
}
