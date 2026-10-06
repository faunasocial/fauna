//! GTK observer for `FeedManager` snapshot changes — the exact analogue of
//! `crate::conversations::observer`.
//!
//! `FeedManager::notify()` fires `FeedSnapshotObserver::on_changed()` synchronously
//! on whatever thread the mutator runs on (a tokio worker for the async
//! select/submit/refresh methods, or the GTK main thread for sync
//! `update_compose`). GTK widget mutation must happen on the main thread, so the
//! observer forwards each notification through a coalescing wake channel whose
//! receiver runs in [`crate::async_helper::spawn_wake_loop`].

use std::sync::Arc;

use async_channel::Sender;
use fauna_feed::FeedSnapshotObserver;

use crate::feed::host::LinuxFeedManager;

pub struct GtkFeedObserver {
    tx: Sender<()>,
}

impl FeedSnapshotObserver for GtkFeedObserver {
    fn on_changed(&self) {
        // try_send so the mutator path never blocks; a full channel is a wake
        // already owed (`async_helper::snapshot_wake_channel`).
        let _ = self.tx.try_send(());
    }
}

/// Register a `GtkFeedObserver` against `manager`, returning the receiver end of
/// its coalescing wake channel. Consume `rx` with
/// [`crate::async_helper::spawn_wake_loop`] so widget updates execute on the GTK
/// main thread, one per coalesced wake, yielding between them.
pub fn attach(manager: &Arc<LinuxFeedManager>) -> async_channel::Receiver<()> {
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer = Arc::new(GtkFeedObserver { tx });
    manager.add_observer(observer);
    rx
}
