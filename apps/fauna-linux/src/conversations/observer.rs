//! GTK observer for `ConversationsManager` snapshot changes.
//!
//! `ConversationsManager::notify()` fires `SnapshotObserver::on_changed()`
//! synchronously on whatever thread the mutator runs on (often a tokio
//! worker for backend send/resolve, or the GTK main thread for click
//! handlers). GTK widget mutation must happen on the main thread, so the
//! observer forwards each notification through a coalescing wake channel
//! whose receiver runs in [`crate::async_helper::spawn_wake_loop`].
//!
//! Mirrors the `OnboardingMachine` ↔ `GtkObserver` pattern in
//! `views/onboarding/machine_glue.rs` (chosen because `glib::MainContext::
//! channel` was removed in glib 0.20).

use std::sync::Arc;

use async_channel::Sender;
use fauna_conversations::{ConversationsManager, SnapshotObserver};

pub struct GtkConversationsObserver {
    tx: Sender<()>,
}

impl SnapshotObserver for GtkConversationsObserver {
    fn on_changed(&self) {
        // try_send so the mutator path never blocks; a full channel is a wake
        // already owed (`async_helper::snapshot_wake_channel`).
        let _ = self.tx.try_send(());
    }
}

/// Register a `GtkConversationsObserver` against `manager`, returning the
/// receiver end of its coalescing wake channel. Consume `rx` with
/// [`crate::async_helper::spawn_wake_loop`] so widget updates execute on the
/// GTK main thread, one per coalesced wake, yielding between them.
pub fn attach(manager: &Arc<ConversationsManager>) -> async_channel::Receiver<()> {
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer = Arc::new(GtkConversationsObserver { tx });
    manager.add_observer(observer);
    rx
}
