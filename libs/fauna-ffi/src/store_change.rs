//! UniFFI face of the **store-change notice** — the one shared watch
//! (`fauna_client_account_runtime::store_change`), crossed once for the
//! non-Rust-native apps (`docs/goal/architecture/account-runtime.md`
//! § Multi-instance concurrency → *A runtime's own pump is a source of the
//! notice too*, part 4). tui and linux await the same watch natively
//! (`session::account_store_watch`, `store_surfaces::watch`); this module is
//! that consumer for android, windows, macOS and iOS.
//!
//! What crosses the boundary is one payload-free call,
//! [`FfiStoreChangeListener::store_changed`]: "the account store may have
//! changed". It is a **level, not an event** — the app re-runs the OPEN
//! store-backed surface's own load, paints only what differs, and never
//! discards an edit in progress; those halves are the app's. A gesture's own
//! write never fires it (part 2): the surface that made the gesture repaints
//! from the gesture's own answer.
//!
//! **Registered once per process, not once per runtime.** The listener slot
//! outlives a sign-out or an account switch, so an app registers at start and
//! never again; the relay task is what follows the runtime — started where
//! the store is installed (`crate::account_runtime`'s store edge), ended by
//! the watch itself once that runtime is gone. A notice with no listener
//! registered is dropped: the next visit's own load reads fresh state anyway.

use std::sync::{Arc, Mutex};

use fauna_client_account_runtime::store_change::StoreChangeWatch;
use fauna_sync_engine::account_runtime::AccountStoreHandle;

/// The app's one store-change handler. Called on a runtime thread, never the
/// UI thread — hop before touching UI state.
#[uniffi::export(with_foreign)]
pub trait FfiStoreChangeListener: Send + Sync {
    /// The account store may have changed: re-run the open store-backed
    /// surface's own load.
    fn store_changed(&self);
}

/// This process's listener (module docs: one per process).
static LISTENER: Mutex<Option<Arc<dyn FfiStoreChangeListener>>> = Mutex::new(None);

/// Register the app's store-change handler, replacing any earlier one;
/// `None` clears it. Call once at app start — the registration survives
/// sign-out and account switch.
#[uniffi::export]
pub fn set_store_change_listener(listener: Option<Arc<dyn FfiStoreChangeListener>>) {
    *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = listener;
}

/// Deliver one notice to the registered listener, if any. The slot's lock is
/// released before the foreign call, so a handler may re-register from
/// inside it.
fn notify() {
    let listener = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(listener) = listener {
        listener.store_changed();
    }
}

/// Relay `store`'s watch to the listener until its runtime is gone — sign-out's
/// deterministic shutdown makes the floor's read err — so the handle clone
/// held here never keeps alive a runtime the app has let go of.
pub(crate) async fn relay(store: AccountStoreHandle) {
    let mut watch = StoreChangeWatch::new(store).await;
    while watch.changed().await {
        notify();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// The tests that touch this process's one listener slot take turns.
    pub(crate) static SLOT_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A listener that counts its calls.
    #[derive(Default)]
    pub(crate) struct Counting(pub(crate) AtomicUsize);

    impl FfiStoreChangeListener for Counting {
        fn store_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_notice_reaches_the_registered_listener_and_no_other() {
        let _slot = SLOT_TESTS.lock().await;
        set_store_change_listener(None);
        notify(); // nobody registered: dropped, not a panic

        let first = Arc::new(Counting::default());
        set_store_change_listener(Some(first.clone()));
        notify();
        assert_eq!(first.0.load(Ordering::SeqCst), 1);

        let second = Arc::new(Counting::default());
        set_store_change_listener(Some(second.clone()));
        notify();
        assert_eq!(
            first.0.load(Ordering::SeqCst),
            1,
            "a replaced listener hears nothing more"
        );
        assert_eq!(second.0.load(Ordering::SeqCst), 1);

        set_store_change_listener(None);
        notify();
        assert_eq!(
            second.0.load(Ordering::SeqCst),
            1,
            "a cleared listener hears nothing more"
        );
    }
}
