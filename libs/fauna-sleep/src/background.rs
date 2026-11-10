//! Where a page machine's fire-and-forget work runs.
//!
//! A synchronous setter that starts background work (onboarding's location
//! change re-listing the VPS offers; Media's `set_filter` entering a foreign
//! browse scope, `ui/media.md` § Cross-nest shared sets) has its result arrive
//! through the machine's observer, so no caller awaits it. Native callers
//! differ in what runtime their thread has: tui runs on its own tokio loop, but
//! a UniFFI shell calls the setter from its UI thread (the Swift main thread, a
//! WinUI or Android UI thread), which has none, and linux calls it from inside
//! a runtime it builds for that one call and drops on return — work spawned
//! there dies with it. A machine therefore never depends on its caller's
//! runtime: it always spawns on a runtime of its own — one per process, built
//! on first use, driven by one thread with the default ~2 MB stack linux and
//! tui run this code on (`docs/goal/architecture/apps/native-async-execution.md`
//! § Implementation status today). On wasm the browser's microtask queue is
//! always there.
//!
//! Lifted here from `fauna-onboarding-machine` (its one home until 2026-11-07)
//! when Media needed the same thing: this crate already owns the cross-target
//! async split and already carries tokio natively, and its wasm leg reaches
//! `spawn_local` through `fauna-core`'s `js` feature — a path-crate edge, no
//! new third-party entry.

use std::future::Future;

/// Run `work` in the background: on the process's own background runtime
/// natively, never the caller's (which may not outlive the call);
/// `spawn_local` on wasm.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_background(work: impl Future<Output = ()> + Send + 'static) {
    drop(own_runtime().spawn(work));
}

/// Run `work` in the background: on the browser's microtask queue.
#[cfg(target_arch = "wasm32")]
pub fn spawn_background(work: impl Future<Output = ()> + 'static) {
    fauna_core::spawn_local(work);
}

/// The process's background runtime: a current-thread runtime that one
/// detached thread drives for the life of the process. Process-lived, so a
/// machine's shared HTTP client never holds a pooled connection whose runtime
/// has gone away.
#[cfg(not(target_arch = "wasm32"))]
fn own_runtime() -> &'static tokio::runtime::Handle {
    static HANDLE: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();
    HANDLE.get_or_init(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build the machines' background runtime");
        let handle = rt.handle().clone();
        std::thread::Builder::new()
            .name("fauna-machine-bg".into())
            .spawn(move || rt.block_on(std::future::pending::<()>()))
            .expect("start the machines' background thread");
        handle
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    /// linux calls a machine from a runtime it builds for one call and drops
    /// on return (`async_helper::run_on_tokio`, the e2e bridge's
    /// `block_on_tokio`): work spawned from inside it must still finish.
    #[test]
    fn work_outlives_a_callers_short_lived_runtime() {
        let (tx, rx) = mpsc::channel();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            super::spawn_background(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let _ = tx.send(());
            });
        });
        drop(rt);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the background work finished after its caller's runtime was dropped");
    }

    /// A caller with no runtime at all — a UniFFI shell's UI thread.
    #[test]
    fn work_runs_from_a_thread_with_no_runtime() {
        let (tx, rx) = mpsc::channel();
        super::spawn_background(async move {
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the background work ran with no caller runtime");
    }
}
