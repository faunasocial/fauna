//! The **private contact overlay** seam over the account store
//! (`docs/goal/ui/contacts.md` § The private overlay) — one implementation for
//! every app that hosts the account runtime (priority #2), beside
//! [`crate::read_positions`] and in its shape, for the same boundary reason:
//! `fauna-conversations` holds the projection (`ContactsCache`) and the
//! succession verdicts, and must never learn about the account runtime.
//!
//! - **In** — a watcher task reads every overlay this store holds and loads
//!   the manager's projection: once at registration, then whenever they may
//!   have moved — whenever the one store-change watch fires
//!   ([`crate::store_change`]): a run of this runtime's own pump that changed
//!   the store (its walk is where a sibling device's edit arrives), or the
//!   store's cross-connection change counter moving (a co-located process
//!   holding the pump committed instead). The manager re-emits only on a real
//!   change, and runs the succession fold's reconcile on every load.
//! - **Out** — [`AccountContactOverlayFolds`] is the manager's
//!   `backend::ContactOverlayFolds`: the manager asks synchronously, so it
//!   only queues; a writer task folds each pair through the store's typed
//!   door (`AccountStoreHandle::fold_contact_overlay`) and reloads the
//!   projection. [`save`] is the private section's Save: the store's typed
//!   door writes the changed registers, and the projection reloads at once,
//!   so the saving device re-paints without waiting for a pass.
//!
//! Both tasks end with what they serve: the writer when the manager drops the
//! seam (an identity change retires it), the watcher when the manager refuses
//! its registration, the manager is gone, or the runtime is.

use std::collections::BTreeSet;
use std::sync::{Arc, Weak};

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_account_plane::contact_overlay_rows::{OverlayWrite, OverlayWriteOutcome};
use fauna_conversations::manager::ConversationsManager;
use tokio::sync::mpsc;

use crate::spawner::TaskSpawner;
use crate::store_change::{STORE_CHANGE_POLL_INTERVAL, StoreChangeWatch};

/// `fauna_conversations::backend::ContactOverlayFolds` over this account's
/// store.
pub struct AccountContactOverlayFolds {
    folds: mpsc::UnboundedSender<(String, String)>,
}

impl fauna_conversations::backend::ContactOverlayFolds for AccountContactOverlayFolds {
    fn fold(&self, predecessor_hex: &str, successor_hex: &str) {
        // A closed queue means the writer is gone with its runtime; the next
        // projection load asks again.
        let _ = self
            .folds
            .send((predecessor_hex.to_string(), successor_hex.to_string()));
    }
}

/// Register the overlay seam on `manager` over `store`, and start its two
/// tasks (module docs) on `spawner`. Call it at the account-store-ready edge
/// ([`crate::conversation_seams::wire`] does).
pub fn register(
    manager: &Arc<ConversationsManager>,
    store: AccountStoreHandle,
    spawner: &impl TaskSpawner,
) {
    let (folds, queue) = mpsc::unbounded_channel();
    let generation =
        manager.register_contact_overlays(Some(Arc::new(AccountContactOverlayFolds { folds })));
    spawner.spawn(Box::pin(write_folds(
        queue,
        Arc::downgrade(manager),
        generation,
        store.clone(),
    )));
    spawner.spawn(Box::pin(deliver_overlays(
        Arc::downgrade(manager),
        generation,
        store,
    )));
}

/// Save the private section's `write` for the person `actor_id_hex`, then
/// reload `manager`'s projection from the store. A refusal at the writer
/// door while no generation tip resolves is `Err` (the save error, the staged edits kept on screen); a label
/// cap is [`OverlayWriteOutcome::Refused`]. On a replica that has never
/// listed the fleet scope, or that holds the account's rows under a
/// generation it may still be keyed for, the save is refused at the read gate
/// ([`not_ready_reason`]) and nothing is put.
pub async fn save(
    manager: &ConversationsManager,
    store: &AccountStoreHandle,
    actor_id_hex: &str,
    write: OverlayWrite,
) -> anyhow::Result<OverlayWriteOutcome> {
    // Taken before the write: a reload racing an identity change is refused.
    let generation = manager.contact_overlays_generation();
    let outcome = store.write_contact_overlay(actor_id_hex, write).await?;
    if matches!(outcome, OverlayWriteOutcome::Written(_)) {
        manager.apply_contact_overlays(generation, store.contact_overlays().await?);
    }
    Ok(outcome)
}

/// The reason a surface shows when a [`save`] error is the read gate's
/// refusal (`account-client-lifecycle.md` § The client-side lifecycle → *The
/// first listing*, clauses (4) and (5)) — the save can be retried once what
/// it waits for has come, so the surface shows what that is
/// (`common.needs_nest`, or `common.needs_other_device`) rather than the save
/// error. `None` for any other failure.
pub fn not_ready_reason(e: &anyhow::Error) -> Option<&'static str> {
    fauna_account_plane::account_driver::not_ready_reason(e)
}

/// [`not_ready_reason`] as a `LocalizedText` key, for an app that resolves
/// text through its platform's own pipeline (the UniFFI face).
pub fn not_ready_text(e: &anyhow::Error) -> Option<fauna_core::localized::LocalizedText> {
    fauna_account_plane::account_driver::not_ready_reason_key(e)
        .map(fauna_core::localized::LocalizedText::key)
}

/// The outward half: fold each queued pair, then reload the projection once
/// per burst (the reload's reconcile finds the fixed point, or asks again).
///
/// A fold the store refused is retried here, on the store-change floor's
/// cadence, until it lands or the seam is dropped: the projection reloads
/// only when the store may have changed (the watcher), so a refused fold
/// with nothing else moving would otherwise wait for an unrelated change.
async fn write_folds(
    mut queue: mpsc::UnboundedReceiver<(String, String)>,
    manager: Weak<ConversationsManager>,
    generation: u64,
    store: AccountStoreHandle,
) {
    let mut failed: BTreeSet<(String, String)> = BTreeSet::new();
    loop {
        let mut burst = if failed.is_empty() {
            let Some(first) = queue.recv().await else {
                return; // the seam was dropped
            };
            BTreeSet::from([first])
        } else {
            tokio::select! {
                next = queue.recv() => {
                    let Some(next) = next else { return };
                    let mut burst = std::mem::take(&mut failed);
                    burst.insert(next);
                    burst
                }
                () = fauna_sleep::sleep(STORE_CHANGE_POLL_INTERVAL) => {
                    if store.data_version().await.is_err() {
                        return; // the runtime is gone: nothing left to fold into
                    }
                    std::mem::take(&mut failed)
                }
            }
        };
        while let Ok(next) = queue.try_recv() {
            burst.insert(next);
        }
        let mut wrote = false;
        for pair in burst {
            match store.fold_contact_overlay(&pair.0, &pair.1).await {
                Ok(w) => wrote |= w,
                // The item stays under the predecessor until the retry lands.
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "the account store could not fold a contact overlay onto its \
                         verified successor — retried shortly"
                    );
                    failed.insert(pair);
                }
            }
        }
        if !wrote {
            continue;
        }
        let Ok(overlays) = store.contact_overlays().await else {
            return;
        };
        let Some(m) = manager.upgrade() else {
            return;
        };
        m.apply_contact_overlays(generation, overlays);
    }
}

/// The inward half: deliver the store's overlays at registration and
/// whenever they may have moved.
async fn deliver_overlays(
    manager: Weak<ConversationsManager>,
    generation: u64,
    store: AccountStoreHandle,
) {
    // Seeded before the first read, so nothing between the two is missed.
    let mut watch = StoreChangeWatch::new(store.clone()).await;
    loop {
        let Ok(overlays) = store.contact_overlays().await else {
            return; // the runtime is gone: nothing left to deliver from
        };
        let Some(m) = manager.upgrade() else {
            return;
        };
        if !m.apply_contact_overlays(generation, overlays) {
            return; // retired: an identity change, or a newer registration
        }
        drop(m);
        if !watch.changed().await {
            return; // the runtime is gone
        }
    }
}

#[cfg(test)]
mod tests {
    use fauna_account_plane::account_driver::{NotReadyReason, ScopeNotReady};

    use super::*;

    /// The read gate's two reasons reach a key-resolving app as two different
    /// texts, and any other failure as none — never one bare "not ready".
    #[test]
    fn the_read_gates_refusal_names_what_the_save_waits_for() {
        let key = |reason| {
            not_ready_text(&anyhow::Error::new(ScopeNotReady {
                scope: "fleet".into(),
                reason,
            }))
            .map(|text| text.key)
        };
        for reason in [NotReadyReason::NotListed, NotReadyReason::HeldForNest] {
            assert_eq!(key(reason).as_deref(), Some("common.needs_nest"));
        }
        assert_eq!(
            key(NotReadyReason::HeldForSibling).as_deref(),
            Some("common.needs_other_device")
        );
        assert_eq!(not_ready_text(&anyhow::anyhow!("no generation tip")), None);
    }
}
