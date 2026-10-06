//! Shared Windows Service Control Manager (SCM) service-entry-point ceremony.
//!
//! FaunaNest and FaunaBridge each register a Windows Service and run an inner
//! loop until SCM sends Stop; the SCM control-handler + status-transition
//! boilerplate around that loop was ~55 near-identical lines duplicated
//! between the two services. This is the
//! same shared home fauna-ipc's other `#[cfg(windows)]` modules
//! (`pipe_transport`, `win_token`) already establish for cross-binary
//! windows-only duplication.
//!
//! `define_windows_service!` still has to be invoked per-binary — it expands
//! to a named `extern "system"` trampoline bound to a concrete function,
//! which can't be generic — so callers keep a thin `service_main` wrapper
//! that delegates its body here.

use std::sync::mpsc;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

/// Register `service_name` with SCM, run `loop_fut` to completion (or until
/// SCM sends Stop) on a fresh tokio runtime, then report the terminal status.
///
/// `map_exit_code` maps a `loop_fut` failure to the [`ServiceExitCode`] SCM
/// records in the System Event Log — FaunaBridge always reports `Win32(0)`
/// even on error (`|_| ServiceExitCode::Win32(0)`); FaunaNest additionally
/// distinguishes an `AddrInUse` bind failure (`ServiceSpecific(10048)`) from
/// every other failure (`ServiceSpecific(1)`).
pub fn run_scm_service_main(
    service_name: &'static str,
    loop_fut: impl std::future::Future<Output = anyhow::Result<()>>,
    map_exit_code: impl FnOnce(&anyhow::Error) -> ServiceExitCode,
) {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();

    let status_handle =
        service_control_handler::register(service_name, move |control_event| match control_event {
            ServiceControl::Stop => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .expect("failed to register service control handler");

    status_handle
        .set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::default(),
            process_id: None,
        })
        .expect("failed to set service status");

    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let mut exit_code = ServiceExitCode::Win32(0);
    rt.block_on(async {
        tokio::select! {
            result = loop_fut => {
                if let Err(e) = &result {
                    tracing::error!(service = service_name, "service loop error: {e}");
                    exit_code = map_exit_code(e);
                }
            }
            _ = tokio::task::spawn_blocking(move || shutdown_rx.recv()) => {
                tracing::info!("service stop requested");
            }
        }
    });

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code,
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    });
}
