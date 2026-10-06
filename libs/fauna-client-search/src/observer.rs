//! Reactivity callback for the search snapshot. The [`SearchManager`] notifies
//! its observers on every state mutation; observers re-read fresh state via
//! [`SearchManager::snapshot`]. Direct mirror of `fauna_feed::FeedSnapshotObserver`
//! (itself a mirror of `fauna_conversations::observer::SnapshotObserver`), so
//! every app's search view subscribes exactly the way it already subscribes
//! to the feed and to conversations.
//!
//! Named `SearchSnapshotObserver`, **never** a bare `SnapshotObserver`: the
//! traits are re-exported through the single `fauna-ffi` UniFFI module, and the
//! C# bindgen flattens every imported module's types into one `using` scope, so
//! a second `SnapshotObserver` there is an unqualified **CS0104** ambiguity
//! (Swift modules / Kotlin packages qualify, so they don't hit it). Ratified as
//! part of the snapshot design — `docs/goal/ui/search.md` § State & data shape.
//!
//! [`SearchManager`]: crate::SearchManager
//! [`SearchManager::snapshot`]: crate::SearchManager::snapshot

/// Called whenever the manager's observable state changes. The observer reads
/// fresh snapshots via [`SearchManager::snapshot`](crate::SearchManager::snapshot).
/// `with_foreign` so a GTK/Swift/Kotlin observer object can implement it across
/// the FFI boundary.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait SearchSnapshotObserver: Send + Sync {
    fn on_changed(&self);
}
