//! A generic one-shot generation-debounce timer for the GTK main loop.
//!
//! Lifted out of three independent hand-copies of the exact same ceremony —
//! [`crate::feed::drafts`]'s `arm_debounce`, [`crate::conversations::drafts`]'s
//! `arm_debounce`, and [`crate::conversations::conv_backend`]'s
//! `arm_replica_debounce` — each bumping a generation counter on every tick,
//! arming a [`glib::timeout_add_local_once`] timer, and skipping the fire if a
//! newer tick has since superseded it. What fires (a drafts
//! snapshot-and-save, an MLS-replica snapshot-and-upload) is never this
//! module's business, so it stays a caller-supplied closure: callers keep
//! full control over what they capture — a `Weak` manager for a swappable
//! singleton, an `Arc` for an eternal one — and what they do with it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// (Re)arm the one-shot debounce timer for the current edit/mutation burst.
/// Each call bumps `generation` and schedules an `after` timer; only the
/// timer whose generation is still current when it fires runs `on_fire`, so a
/// later tick supersedes an earlier pending one.
pub(crate) fn arm_generation_debounce(
    generation: &Rc<Cell<u64>>,
    after: Duration,
    on_fire: impl FnOnce() + 'static,
) {
    let g = generation.get().wrapping_add(1);
    generation.set(g);
    let generation = Rc::clone(generation);
    glib::timeout_add_local_once(after, move || {
        if generation.get() != g {
            return; // a newer tick arrived; its timer owns the fire
        }
        on_fire();
    });
}

// No test module: `timeout_add_local_once` acquires glib's process-wide
// DEFAULT `MainContext`, which only one thread may hold at a time — measured
// against this exact suite (`default main context already acquired by
// another thread`, colliding with the concurrently-running `walk::*` tests
// that own a real `gtk::Application`). None of the three call sites this was
// lifted from tested the live timer firing either, for the same reason (see
// `feed::drafts::tests::weak_manager_reports_gone_once_the_strong_ref_is_dropped`
// for the same-level precedent: it tests the `Weak`/`Arc` semantic in
// isolation, never the glib timer itself).
