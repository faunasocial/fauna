//! Shared shape for a construct-run-drop FFI worker: a dedicated OS thread with
//! its own current-thread tokio runtime, servicing a bounded request channel.
//!
//! [`crate::file_provider_host`] and [`crate::sync_engine_host`] each keep a
//! `!Send` engine (a rusqlite `SyncDb` connection, created/used/dropped on one
//! thread) off the shared async executor this way — see either module's own
//! "why a dedicated worker thread" doc for the full reasoning. This is the one
//! piece that was truly identical between them: build the channel, spawn the
//! named thread, build the runtime, `block_on` the caller's driving future.

use std::future::Future;
use std::thread;

use tokio::runtime::Builder;
use tokio::sync::mpsc;

/// Spawn `run` on a new OS thread named `thread_name`, wired to a
/// `capacity`-bounded request channel whose sender is returned. `run` receives
/// the matching receiver and is driven to completion on a fresh current-thread
/// tokio runtime built on that thread. A runtime-build failure is logged under
/// `host_name` and `run` is never called.
pub(crate) fn spawn_worker_thread<Req, F>(
    thread_name: &'static str,
    host_name: &'static str,
    capacity: usize,
    run: impl FnOnce(mpsc::Receiver<Req>) -> F + Send + 'static,
) -> mpsc::Sender<Req>
where
    Req: Send + 'static,
    F: Future<Output = ()>,
{
    let (requests, request_rx) = mpsc::channel::<Req>(capacity);
    thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let rt = match Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("could not build {host_name} host runtime: {e}");
                    return;
                }
            };
            rt.block_on(run(request_rx));
        })
        .ok();
    requests
}
