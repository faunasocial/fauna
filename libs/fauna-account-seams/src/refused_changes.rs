//! The **refused inbound scheduling changes** over the account store
//! (`inbound-scheduling-authority.md` § *Surfacing*; the kind
//! `fauna.state.refused-scheduling-changes`) — one implementation for every
//! app that hosts the account runtime (priority #2), web included.
//!
//! - **Out** — [`AccountRefusedChangeLog`] is the manager's
//!   `backend::RefusedChangeLog`: both inbound rails' sinks record into the
//!   manager's inbox, which forwards here once [`register`] has run at the
//!   account-store-ready edge and hands over whatever it held before. The
//!   seam only queues; a writer task applies each write through the store's
//!   typed door (`AccountStoreHandle::write_refused_scheduling_changes`), in
//!   order. A write the door refuses is logged and dropped — the calendar was
//!   already left untouched, so a lost notice is a log line, never a retry of
//!   the apply.
//! - **Read and dismiss** — [`load_open`] and [`dismiss`] are what an Events
//!   surface calls. [`load_open`] takes the handle as the host holds it, which
//!   may not be up yet: a surface opened before the account store is
//!   assembled reads an empty list and fills in on its next load, never an
//!   error (the seat's *lent late* ruling, `p2p.md` § Offline share
//!   initiation → *The seat's record is lent late*).
//!
//! The writer ends with what it serves: when the manager drops the seam (an
//! identity change retires it) or the runtime is gone.

use std::sync::Arc;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_account_plane::refused_change_rows::RefusedChangeWrite;
use fauna_conversations::manager::ConversationsManager;
use fauna_core::data::{RefusedSchedulingChange, RefusedSchedulingChanges};
use tokio::sync::mpsc;

use crate::spawner::TaskSpawner;

/// The manager's `backend::RefusedChangeLog` over the account store — a queue
/// into [`register`]'s writer task.
pub struct AccountRefusedChangeLog {
    writes: mpsc::UnboundedSender<RefusedChangeWrite>,
}

impl fauna_conversations::backend::RefusedChangeLog for AccountRefusedChangeLog {
    fn record(&self, row: RefusedSchedulingChange) {
        // A closed queue means the writer is gone with its runtime.
        let _ = self.writes.send(RefusedChangeWrite::Record(Box::new(row)));
    }

    fn adopt(&self, held: RefusedSchedulingChanges) {
        let _ = self.writes.send(RefusedChangeWrite::Merge(Box::new(held)));
    }
}

/// Register the refused-change log on `manager` over `store`, its writer
/// task started on `spawner`. Called by `conversation_seams::wire`; a later
/// registration replaces this one (the replaced seam's queue closes and its
/// writer ends).
pub fn register(
    manager: &Arc<ConversationsManager>,
    store: AccountStoreHandle,
    spawner: &impl TaskSpawner,
) {
    let (writes, queue) = mpsc::unbounded_channel();
    manager
        .refused_changes()
        .register(Some(Arc::new(AccountRefusedChangeLog { writes })));
    spawner.spawn(Box::pin(write_refusals(queue, store)));
}

async fn write_refusals(
    mut queue: mpsc::UnboundedReceiver<RefusedChangeWrite>,
    store: AccountStoreHandle,
) {
    while let Some(write) = queue.recv().await {
        if let Err(e) = store.write_refused_scheduling_changes(write).await {
            tracing::warn!(
                error = %e,
                "refused scheduling change not recorded on the account plane"
            );
        }
    }
}

/// The refused-change rows an Events surface renders — open only, most recent
/// attempt first (`RefusedSchedulingChanges::open`). `store` is the host's
/// handle as it holds it: `None` (the account store not assembled yet) reads
/// empty, and so does a read the store answers with an error, logged — the
/// surface's next load fills it in.
pub async fn load_open(store: Option<&AccountStoreHandle>) -> Vec<RefusedSchedulingChange> {
    let Some(store) = store else {
        return Vec::new();
    };
    match store.refused_scheduling_changes().await {
        Ok(list) => list.open(),
        Err(e) => {
            tracing::warn!(error = %e, "refused scheduling changes not loaded");
            Vec::new()
        }
    }
}

/// Dismiss the row `key` names — the owner's *I have seen this*, the only
/// gesture a refused-change row carries — answering whether it was open.
///
/// # Errors
/// The account store is not up yet (`store` is `None`), or its door refused
/// the write.
pub async fn dismiss(store: Option<&AccountStoreHandle>, key: &str) -> anyhow::Result<bool> {
    let store = store.ok_or_else(|| anyhow::anyhow!("the account store is not assembled yet"))?;
    store
        .write_refused_scheduling_changes(RefusedChangeWrite::Dismiss(key.to_string()))
        .await
}
