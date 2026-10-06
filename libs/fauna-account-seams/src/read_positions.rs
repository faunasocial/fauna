//! The **read-position** seam over the account store — the fauna-native
//! rail's synced read state (`docs/goal/behavior/conversation-read-state.md`
//! § The read-marker record), one implementation for every app that hosts
//! the account runtime (priority #2), beside [`crate::group_reception`] for
//! the same boundary reason: `fauna-conversations` declares the seam and must
//! never learn about the account runtime.
//!
//! Both directions live here:
//!
//! - **Out** — [`AccountReadPositions`] is the manager's
//!   `backend::ReadPositions`. The manager calls it synchronously from its one
//!   read chokepoint, so it only queues: a writer task drains the queue,
//!   coalesces a burst to its highest position per channel, and raises each
//!   marker through the store's monotone door
//!   (`AccountStoreHandle::raise_read_marker`).
//! - **In** — a watcher task reads every marker this store holds and hands
//!   the manager the positions: once at registration, then again whenever
//!   they may have moved — whenever the one store-change watch fires
//!   ([`crate::store_change`]): a run of this runtime's own pump that changed
//!   the store (its walk is where a sibling's raise arrives), or the store's
//!   cross-connection change counter moving (a co-located process that holds
//!   the pump — the always-on agent natively, another tab on web — committed
//!   instead). The plane has no per-kind change feed; re-reading one small
//!   kind on the notice is this kind's answer (`account-sync-plane.md` owns a
//!   general feed, if one is ever built). The manager hears only positions
//!   that changed.
//!
//! Both tasks end with what they serve: the writer when the manager drops
//! the seam (an identity change retires it), the watcher when the manager
//! refuses its registration, the manager is gone, or the runtime is.
//!
//! **Web registers this seam once it hosts the runtime** (`account-client-lifecycle.md`
//! § The client-side lifecycle → *The trigger fired*, ruling (4)); until
//! then its native threads stay on the launch floor
//! (`conversation-read-state.md` § web).

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_conversations::manager::ConversationsManager;
use tokio::sync::mpsc;

use crate::spawner::TaskSpawner;
use crate::store_change::StoreChangeWatch;

/// `fauna_conversations::backend::ReadPositions` over this account's store.
pub struct AccountReadPositions {
    raises: mpsc::UnboundedSender<(String, u64)>,
}

impl fauna_conversations::backend::ReadPositions for AccountReadPositions {
    fn raise(&self, channel_id_hex: &str, through: u64) {
        // A closed queue means the writer is gone with its runtime: the read
        // holds in memory, which is the seam's declared degradation.
        let _ = self.raises.send((channel_id_hex.to_string(), through));
    }
}

/// Register the read-position seam on `manager` over `store`, and start its
/// two tasks (module docs) on `spawner`. Call it at the account-store-ready
/// edge ([`crate::conversation_seams::wire`] does).
///
/// The spawner is explicit because not every registering edge runs inside a
/// runtime: a UniFFI host may complete the (session, store) pair from a
/// synchronous foreign call, and hands in the runtime its store runs on;
/// web hands in the browser's executor.
pub fn register(
    manager: &Arc<ConversationsManager>,
    store: AccountStoreHandle,
    spawner: &impl TaskSpawner,
) {
    let (raises, queue) = mpsc::unbounded_channel();
    let generation = manager.set_read_positions(Arc::new(AccountReadPositions { raises }));
    spawner.spawn(Box::pin(write_raises(queue, store.clone())));
    spawner.spawn(Box::pin(deliver_positions(
        Arc::downgrade(manager),
        generation,
        store,
    )));
}

/// The outward half: drain the queue, highest position per channel, one
/// store raise each.
async fn write_raises(
    mut queue: mpsc::UnboundedReceiver<(String, u64)>,
    store: AccountStoreHandle,
) {
    while let Some(first) = queue.recv().await {
        let mut burst: HashMap<String, u64> = HashMap::new();
        let mut take = |(channel, through): (String, u64)| {
            let held = burst.entry(channel).or_default();
            *held = (*held).max(through);
        };
        take(first);
        while let Ok(next) = queue.try_recv() {
            take(next);
        }
        for (channel, through) in burst {
            if let Err(e) = store.raise_read_marker(&channel, through).await {
                // The thread is read in memory; its next read raises again.
                tracing::warn!(
                    error = %e,
                    "the account store could not raise a conversation read marker — \
                     the read holds on this device until the thread is read again"
                );
            }
        }
    }
}

/// The inward half: deliver the store's positions at registration and
/// whenever they may have moved.
async fn deliver_positions(
    manager: Weak<ConversationsManager>,
    generation: u64,
    store: AccountStoreHandle,
) {
    let mut delivered: Option<Vec<(String, u64)>> = None;
    // Seeded before the first read, so nothing between the two is missed.
    let mut watch = StoreChangeWatch::new(store.clone()).await;
    while manager.strong_count() > 0 {
        let Ok(mut positions) = store.read_markers().await else {
            return; // the runtime is gone: nothing left to deliver from
        };
        positions.sort();
        if delivered.as_ref() != Some(&positions) {
            let Some(manager) = manager.upgrade() else {
                return;
            };
            if !manager.apply_read_positions(generation, positions.clone()) {
                return; // retired — an identity change, or a newer registration
            }
            delivered = Some(positions);
        }
        if !watch.changed().await {
            return; // the runtime is gone
        }
    }
}
