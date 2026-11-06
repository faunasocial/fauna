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
    // The identity this loop saves for. Read at attach time — every caller
    // attaches from the post-auth hook, after the switch's
    // `reset_actor_scoped_state` has bumped it — and checked at every fire.
    let armed_for = crate::actor_scope::current_generation();
    // Each tick re-arms the debounce. The loop ends once the manager this
    // observer was registered on is dropped (session rebuild replaces the
    // eternal singleton's observer list, or a swappable manager is
    // retired on re-auth) — the last strong ref goes with it, the
    // observer list drops, the sender drops, and `recv` closes.
    crate::async_helper::spawn_wake_loop(rx, move || {
        arm_debounce(
            &generation,
            armed_for,
            &manager,
            &sync,
            &runtime,
            log_prefix,
        );
        glib::ControlFlow::Continue
    });
}

fn arm_debounce<M: DraftsHost + 'static>(
    generation: &Rc<Cell<u64>>,
    armed_for: u64,
    manager: &Weak<M>,
    sync: &Arc<DraftsSync<Arc<NestClient>>>,
    runtime: &tokio::runtime::Handle,
    log_prefix: &'static str,
) {
    let manager = Weak::clone(manager);
    let sync = Arc::clone(sync);
    let runtime = runtime.clone();
    arm_generation_debounce(generation, autosave_debounce(), move || {
        // Snapshot on the GTK main thread (the manager's state lives here),
        // then seal + upload off the main thread on the tokio runtime.
        let now = crate::actor_scope::current_generation();
        let Some(snapshot) = snapshot_still_owed(&manager, armed_for, now) else {
            return;
        };
        runtime.spawn(async move {
            if let Err(e) = sync.save_if_changed(&snapshot).await {
                tracing::warn!("{log_prefix}: autosave failed: {e}");
            }
        });
    });
}

/// The snapshot a fired debounce owes its save, or `None` when it owes none.
///
/// Two retirements, both read at FIRE time, never at arm time:
///
/// * **The manager is gone** — `feed`'s swappable manager retired on re-auth.
///   A strong ref taken at arm time would resurrect it.
/// * **The identity changed since the loop was attached** (`armed_for` is
///   [`crate::actor_scope::current_generation`] at attach, `now` at fire).
///   `conversations`' manager is an eternal singleton, so the `Weak` above
///   never dies for it: a debounce the outgoing account armed in the last
///   [`autosave_debounce`] before a switch (any `notify()` arms one — a roster
///   poke, a probe landing, opening the composer) would otherwise fire after
///   the switch's `clear_for_identity_change` and snapshot the WIPED manager —
///   or, later still, the incoming account's restored draft — and save it
///   through the outgoing account's `DraftsSync`. The leave-flush has just set
///   that sync's baseline to the real draft, so the empty snapshot reads as
///   changed and overwrites the outgoing account's draft on its plane
///   (`conversation-drafts.md` § Persistence: a restore the outgoing account
///   started fills nothing after an identity change — its saves retire with
///   it, `account-scoping.md` § The scoping taxonomy). tui never meets this:
///   it builds a fresh manager per sign-in, so its `Weak` does the job.
fn snapshot_still_owed<M: DraftsHost>(
    manager: &Weak<M>,
    armed_for: u64,
    now: u64,
) -> Option<Vec<u8>> {
    if now != armed_for {
        return None;
    }
    Some(manager.upgrade()?.drafts_snapshot_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host(Vec<u8>);
    impl DraftsHost for Host {
        fn drafts_snapshot_bytes(&self) -> Vec<u8> {
            self.0.clone()
        }
    }

    /// **A debounce the outgoing identity armed saves nothing after the
    /// switch** — the conversations manager outlives it, so only the
    /// generation can retire it. Mutation: drop the generation check in
    /// [`snapshot_still_owed`] and this reds (the wiped manager's empty
    /// snapshot would be saved over the outgoing account's draft).
    #[test]
    fn a_debounce_armed_before_an_identity_change_owes_no_save() {
        let eternal = Arc::new(Host(Vec::new()));
        let weak = Arc::downgrade(&eternal);
        assert_eq!(snapshot_still_owed(&weak, 3, 4), None);
    }

    /// The common path is unchanged: same identity, live manager → its snapshot.
    #[test]
    fn a_debounce_of_the_current_identity_saves_its_snapshot() {
        let host = Arc::new(Host(b"draft".to_vec()));
        assert_eq!(
            snapshot_still_owed(&Arc::downgrade(&host), 4, 4),
            Some(b"draft".to_vec())
        );
    }

    /// A retired (swappable) manager still owes nothing, as before.
    #[test]
    fn a_debounce_whose_manager_retired_owes_no_save() {
        let weak = Arc::downgrade(&Arc::new(Host(b"draft".to_vec())));
        assert_eq!(snapshot_still_owed(&weak, 4, 4), None);
    }
}
