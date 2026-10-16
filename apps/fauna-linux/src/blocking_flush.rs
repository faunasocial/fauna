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

/// The leave-flush body (`reserved-folders.md` § The leave-flush promise): the
/// engagement-cue tail and all three drafts rails, in one place, so every
/// door — the window-close/quit handler and every session teardown (sign-out,
/// account switch, the e2e reset/actor switch) — flushes the same set. The
/// windows app's `App.FlushDraftRailsAsync()` is the same shape.
///
/// A session teardown calls this FIRST, before
/// `conversations::manager().clear_for_identity_change()` and
/// `actor_scope::reset_actor_scoped_state`: after either clear the snapshot it
/// reads is empty (or the feed manager gone), and `save_if_changed` would
/// upload the emptied draft over the user's real one. A rail with nothing new
/// since its last save costs no round trip (`DraftsSync::save_if_changed`).
pub fn flush_leave_door(bounded: bool) {
    crate::feed::host::flush_cues_on_close(bounded);
    crate::conversations::drafts::flush_now_blocking(bounded);
    crate::feed::drafts::flush_now_blocking(bounded);
    crate::views::events::drafts::flush_now_blocking(bounded);
}
