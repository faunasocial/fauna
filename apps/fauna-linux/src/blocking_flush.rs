//! Bounded blocking flush of an async future from a context that holds no
//! tokio runtime handle — the GTK main thread inside `connect_close_request`,
//! which is exactly where every leave-door flush (engagement cues and the
//! three drafts rails) has to run.
//!
//! `feed::host::flush_cues_on_close` and the three drafts rails' own
//! `flush_now_blocking` all delegate here so a new call site does not
//! reinvent the same worker-thread + scratch-runtime + bounded-channel
//! ritual: spin a fresh current-thread tokio runtime on a worker thread, run
//! `fut` under `soft_timeout`, and — when `bounded` — block the caller until
//! the worker finishes or `bound` elapses. `bounded=true` is for the paths
//! where the process (or the client runtime) is about to go away and the
//! flush must land before it does; `bounded=false` is fire-and-forget for a
//! path where the process stays alive (hide-to-tray), where the ordinary
//! debounce would finish the job anyway.

use std::time::Duration;

pub fn run_bounded<F>(fut: F, soft_timeout: Duration, bound: Duration, bounded: bool)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            let _ = rt.block_on(async { tokio::time::timeout(soft_timeout, fut).await });
        }
        let _ = tx.send(());
    });
    if bounded {
        let _ = rx.recv_timeout(bound);
    }
}
