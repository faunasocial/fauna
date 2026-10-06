//! Shared debounced-autosave glue for linux's two drafts legs
//! (`feed::drafts`, `conversations::drafts`) — the manager-backed shape,
//! distinct from the events rail's value-carrying channel (see
//! `views::events::drafts`'s own doc comment for why that one stays
//! separate), and distinct from `conversations::conv_backend`'s MLS-replica
//! autosave (not drafts-shaped; built directly on [`crate::debounce`]).
//!
//! Mirrors `apps/fauna-tui/src/drafts_autosave.rs`'s `DraftsHost` trait +
//! generic loop, GTK-flavored: GTK timers are not futures, so tui's
//! `tokio::select!`-based `run_autosave_loop`/`quiesce` cannot be reused
//! verbatim — this leg's debounce rides
//! [`crate::debounce::arm_generation_debounce`] on the GTK main loop instead,
//! with the seal+upload spawned onto the tokio runtime once a debounce fires.
//!
//! [`DraftsHost`] is the seam: each rail keeps its own manager type and its
//! own `start`/`restore_on_launch`/`flush_now_blocking`/`RAIL`/`slot()`
//! (those genuinely differ — different manager types, rail names, flush call
//! sites) and just calls [`attach_autosave`] with its own observer receiver.
//!
//! **Always `Weak`, even for `conversations`' eternal singleton.** Holding a
//! strong `Arc` across the debounce window would leak `feed`'s swappable
//! manager on every re-auth (`feed::host`'s own doc comment); `Weak::upgrade`
//! against `conversations::host`'s `OnceLock`-backed eternal singleton is
//! unconditionally safe (it degrades to a no-op check that always succeeds),
//! so one uniform shape costs conversations nothing and saves feed from a
//! leak — the same asymmetry tui's own shared module already resolved the
//! same way.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use async_channel::Receiver;
use fauna_client::NestClient;
use fauna_client_drafts::{DraftsSync, autosave_debounce};

use crate::debounce::arm_generation_debounce;

/// A manager that owns compose state which can be snapshotted for the shared
/// drafts-sync plane. One impl per rail's manager type — the identity that
/// stays genuinely per-rail; the debounce glue around it does not.
pub(crate) trait DraftsHost {
    fn drafts_snapshot_bytes(&self) -> Vec<u8>;
}

/// Attach a debounced autosave: an observer tick → GTK-main generation
/// debounce → tokio save. The generation counter coalesces a burst of edits
/// into one save after [`autosave_debounce`] of quiescence;
/// `DraftsSync::save_if_changed` then dedups an unchanged snapshot, so a tick
/// fired by a non-compose manager change costs only a cheap snapshot compare.
///
/// Takes the strong `Arc` only long enough to downgrade it — see the module
/// doc's Lifecycle note on why this holds only a [`Weak`] for the life of the
/// loop, uniformly across both rails.
pub(crate) fn attach_autosave<M: DraftsHost + 'static>(
    manager: Arc<M>,
    rx: Receiver<()>,
    sync: Arc<DraftsSync<Arc<NestClient>>>,
    runtime: tokio::runtime::Handle,
    log_prefix: &'static str,
) {
    let manager = Arc::downgrade(&manager);
    let generation = Rc::new(Cell::new(0u64));
    // Each tick re-arms the debounce. The loop ends once the manager this
    // observer was registered on is dropped (session rebuild replaces the
    // eternal singleton's observer list, or a swappable manager is
    // retired on re-auth) — the last strong ref goes with it, the
    // observer list drops, the sender drops, and `recv` closes.
    crate::async_helper::spawn_wake_loop(rx, move || {
        arm_debounce(&generation, &manager, &sync, &runtime, log_prefix);
        glib::ControlFlow::Continue
    });
}

fn arm_debounce<M: DraftsHost + 'static>(
    generation: &Rc<Cell<u64>>,
    manager: &Weak<M>,
    sync: &Arc<DraftsSync<Arc<NestClient>>>,
    runtime: &tokio::runtime::Handle,
    log_prefix: &'static str,
) {
    let manager = Weak::clone(manager);
    let sync = Arc::clone(sync);
    let runtime = runtime.clone();
    arm_generation_debounce(generation, autosave_debounce(), move || {
        // Re-upgrade at fire time, not at arm time: the session may have
        // retired during the debounce window, and a stale strong ref taken
        // earlier would resurrect a torn-down session's manager.
        let Some(manager) = manager.upgrade() else {
            return;
        };
        // Snapshot on the GTK main thread (the manager's state lives here),
        // then seal + upload off the main thread on the tokio runtime.
        let snapshot = manager.drafts_snapshot_bytes();
        runtime.spawn(async move {
            if let Err(e) = sync.save_if_changed(&snapshot).await {
                tracing::warn!("{log_prefix}: autosave failed: {e}");
            }
        });
    });
}
