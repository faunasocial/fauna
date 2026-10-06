//! GTK observer for `SearchManager` snapshot changes — the exact analogue of
//! `crate::feed::observer`.
//!
//! `SearchManager::notify()` fires `SearchSnapshotObserver::on_changed()`
//! synchronously on whatever thread the mutator runs on (a tokio worker for
//! the async `run_query`/`load_more` methods, or the GTK main thread for sync
//! `cancel`). GTK widget mutation must happen on the main thread, so the
//! observer forwards each notification through an `async-channel` whose
//! receiver runs in `glib::MainContext::default().spawn_local`.

use std::sync::Arc;

use async_channel::Sender;
use fauna_client_search::SearchSnapshotObserver;

use crate::search::host::LinuxSearchManager;

pub struct GtkSearchObserver {
    tx: Sender<()>,
}

impl SearchSnapshotObserver for GtkSearchObserver {
    fn on_changed(&self) {
        // try_send so the mutator path never blocks; a full channel is a wake
        // already owed (`async_helper::snapshot_wake_channel`).
        let _ = self.tx.try_send(());
    }
}

/// Register a `GtkSearchObserver` against `manager`, returning the receiver
/// end of its coalescing wake channel. Consume `rx` with
/// [`crate::async_helper::spawn_wake_loop`] so widget updates execute on the
/// GTK main thread, one per coalesced wake, yielding between them.
pub fn attach(manager: &Arc<LinuxSearchManager>) -> async_channel::Receiver<()> {
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer = Arc::new(GtkSearchObserver { tx });
    manager.add_observer(observer);
    rx
}
