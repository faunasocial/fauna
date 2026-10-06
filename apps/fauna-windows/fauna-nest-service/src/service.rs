//! Windows SCM (Service Control Manager) integration for FaunaNest.
//!
//! Registers as a Windows Service named "FaunaNest", handles Stop/Interrogate
//! control events, and dispatches to the main service loop which starts the
//! nest HTTP server.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Result;
// The cross-OS desktop nest serve-and-restart loop — the nest construction +
// TLS-floor + claim-seeding + serve/restart sequence shared with the macOS
// `fauna-nest-daemon` LaunchDaemon (priority #2/#3). This Windows SCM shell
// resolves the per-OS data dir + `device.toml`-derived worker key + the 443
// default port, then delegates to it. (The serving-port reconcile *policy* —
// `effective_serving_port` / `serving_port_restart_target` / `SERVING_PORT_POLL`
// — lives one level down in `fauna-nest-supervisor`, which the shared loop
// drives.)
use fauna_nest::desktop_serve::{ServeLoopConfig, run_serve_loop};
use fauna_protocol::node_policy::{CANONICAL_INTERNAL_LOOPBACK_PORT, DEFAULT_SERVING_PORT};

use crate::config;

// Only referenced by the Windows SCM `run_as_service` path below.
#[cfg(windows)]
const SERVICE_NAME: &str = "FaunaNest";

/// If `err` is a "port already in use" bind failure, return a clear, actionable
/// message naming the conflicting address — else `None`.
///
/// The network-reachable nest binds `0.0.0.0:443` (installers/windows.md
/// § Network-reachable nest). Under SCM there is no console, so a raw bind
/// failure would surface only as the generic MSI 1920 / an opaque crash. This
/// turns the `AddrInUse` io error into a diagnosis a headless admin can act on
/// (logged via tracing — visible by re-running `fauna-nest-svc.exe --foreground`
/// — and the caller also sets a distinctive `WSAEADDRINUSE` service exit code so
/// SCM records the failure in the System Event Log).
pub fn bind_in_use_message(err: &anyhow::Error, bind: SocketAddr) -> Option<String> {
    let io = err.downcast_ref::<std::io::Error>()?;
    if io.kind() != std::io::ErrorKind::AddrInUse {
        return None;
    }
    Some(format!(
        "FaunaNest cannot start: address {bind} is already in use — another \
         process is listening on port {}. Free that port (stop the conflicting \
         server) and restart the FaunaNest service.",
        bind.port()
    ))
}

/// Windows SCM / foreground entry into the shared desktop nest serve loop.
///
/// Resolves the Windows-specific bits — the `%PROGRAMDATA%\Fauna\nest` data dir,
/// the `device.toml`-derived worker key, the `:443` default serving port, and a
/// Ctrl-C/SCM-Stop shutdown — then delegates the actual nest construction +
/// serve-and-restart to the cross-OS [`fauna_nest::desktop_serve::run_serve_loop`]
/// (shared with the macOS LaunchDaemon; priority #2/#3). On a bind failure it logs
/// the actionable AddrInUse diagnostic before propagating (under SCM stdout is
/// void, so without this the only signal is the generic MSI 1920 / an opaque
/// crash — `service_main` then maps AddrInUse to a distinctive WSAEADDRINUSE exit
/// code SCM records in the System Event Log).
pub async fn run_service_loop(bind_addr: SocketAddr, data_dir: Option<&Path>) -> Result<()> {
    let data_dir = data_dir
        .map(PathBuf::from)
        .unwrap_or_else(config::default_data_dir);
    tracing::info!(data_dir = %data_dir.display(), "nest service loop starting");

    // Ensure the data directory (and therefore its parent `%PROGRAMDATA%\Fauna`,
    // where device.toml lives) exists before writing device.toml below.
    std::fs::create_dir_all(&data_dir)?;

    // Load or create the shared device config. This WRITES/refreshes device.toml so
    // the co-located bridge dials the fixed internal-loopback port
    // (nest_port = CANONICAL_INTERNAL_LOOPBACK_PORT). device.toml lives at the
    // PARENT of the nest data dir (`%PROGRAMDATA%\Fauna\device.toml`), shared with
    // the bridge.
    let device_config_path = data_dir.parent().unwrap_or(&data_dir).join("device.toml");
    config::load_or_init_device_config(&device_config_path)?;

    // Resolve the EXTERNAL serving bind seed. A 0 port (e.g. `--port 0`) falls back
    // to the serving-port default (DEFAULT_SERVING_PORT = 443); device.toml's
    // nest_port is deliberately NOT used here — it is the FIXED internal-loopback
    // dial port for co-located clients, a distinct concept from the movable
    // external serving port (the shared loop binds that via internal_loopback_port).
    let bind = if bind_addr.port() != 0 {
        bind_addr
    } else {
        SocketAddr::from(([127, 0, 0, 1], DEFAULT_SERVING_PORT))
    };

    let cfg = ServeLoopConfig {
        bind,
        data_dir,
        // The SCM runs as LocalSystem, so it binds the privileged :443 directly.
        default_serving_port: DEFAULT_SERVING_PORT,
        // Bind the fixed 127.0.0.1:CANONICAL_INTERNAL_LOOPBACK_PORT co-located-IPC
        // listener alongside the external one (nest/common.md § Same-box reach), so
        // the co-located bridge + app dial a port that never moves on a serving-port
        // change.
        internal_loopback_port: Some(CANONICAL_INTERNAL_LOOPBACK_PORT),
    };

    // The shutdown signal: SCM Stop / foreground Ctrl-C, surfaced as SIGINT.
    let result = run_serve_loop(
        cfg,
        // No launchd socket activation on Windows — the SCM runs as LocalSystem
        // and binds the privileged `:443` directly (`default_serving_port` above).
        None,
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
        None,
    )
    .await;

    if let Err(ref e) = result {
        // Make a "port already in use" failure diagnosable (see fn docs).
        if let Some(msg) = bind_in_use_message(e, bind) {
            tracing::error!("{msg}");
        }
    }
    result
}

/// Run as a Windows Service via SCM dispatcher.
///
/// The registration/status-transition ceremony is shared with FaunaBridge —
/// see [`fauna_ipc::scm_service::run_scm_service_main`]; only the bind
/// address, the loop future, and the AddrInUse exit-code mapping are
/// FaunaNest-specific.
#[cfg(windows)]
pub fn run_as_service() -> Result<()> {
    use windows_service::{define_windows_service, service::ServiceExitCode, service_dispatcher};

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<std::ffi::OsString>) {
        // SCM mode uses hardcoded defaults — CLI flags (--port, --data-dir) are not
        // available here; device.toml supplies any port override (run_service_loop).
        // Bind a routable interface on :443 so remote clients reach this nest
        // (network-reachable nest, installers/windows.md § Network-reachable nest);
        // a desktop box has no SNI router, so the main listener binds :443 directly.
        let bind = SocketAddr::from(([0, 0, 0, 0], 443));
        fauna_ipc::scm_service::run_scm_service_main(
            SERVICE_NAME,
            run_service_loop(bind, None),
            |e| {
                // Report a distinctive exit code so SCM records a specific failure in
                // the System Event Log (not a generic crash) and failure-recovery can
                // act. WSAEADDRINUSE (10048) for a :443 clash — the actionable message
                // text is logged in run_service_loop (visible via
                // `fauna-nest-svc.exe --foreground`).
                if e.downcast_ref::<std::io::Error>().map(std::io::Error::kind)
                    == Some(std::io::ErrorKind::AddrInUse)
                {
                    ServiceExitCode::ServiceSpecific(10048)
                } else {
                    ServiceExitCode::ServiceSpecific(1)
                }
            },
        );
    }

    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| anyhow::anyhow!("failed to start service dispatcher: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 443))
    }

    // The serving-port flag-read + restart-decision tests moved with the logic
    // into `libs/fauna-nest-supervisor` (parameterized on the default port). Only
    // the Windows-specific `bind_in_use_message` SCM-diagnostic tests stay here.

    #[test]
    fn bind_in_use_message_names_the_port_for_addrinuse() {
        let io = std::io::Error::new(std::io::ErrorKind::AddrInUse, "address in use");
        let err = anyhow::Error::from(io);
        let msg = bind_in_use_message(&err, addr()).expect("AddrInUse yields a message");
        assert!(msg.contains("443"), "message names the conflicting port");
        assert!(
            msg.to_lowercase().contains("already in use"),
            "message states the port is in use"
        );
    }

    #[test]
    fn bind_in_use_message_none_for_other_errors() {
        // A non-AddrInUse io error → None (don't mis-attribute unrelated failures).
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err = anyhow::Error::from(io);
        assert!(bind_in_use_message(&err, addr()).is_none());
        // A non-io anyhow error → None.
        let other = anyhow::anyhow!("some other startup failure");
        assert!(bind_in_use_message(&other, addr()).is_none());
    }
}
