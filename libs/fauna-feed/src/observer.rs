//! Reactivity callback for the feed snapshot. The [`FeedManager`] notifies
//! its observers on every state mutation; observers re-read fresh state via
//! [`FeedManager::snapshot`]. Direct mirror of
//! `fauna_conversations::observer::SnapshotObserver` (the established
//! stateful-snapshot template the Conversations page uses) so every app's
//! feed view subscribes the same way it subscribes to conversations.
//!
//! Named `FeedSnapshotObserver` (not the bare `SnapshotObserver` the
//! conversations mirror uses) because both traits are re-exported through the
//! single `fauna-ffi` UniFFI module, and the C# bindgen flattens every imported
//! module's types into one `using` scope — two `SnapshotObserver`s there are an
//! unqualified CS0104 ambiguity (Swift modules / Kotlin packages qualify, so
//! they don't hit it). The feed trait is the newer/unconsumed one, so it takes
//! the disambiguating name.
//!
//! [`FeedManager`]: crate::FeedManager
//! [`FeedManager::snapshot`]: crate::FeedManager::snapshot

/// Called whenever the manager's observable state changes. The observer reads
/// fresh snapshots via [`FeedManager::snapshot`](crate::FeedManager::snapshot).
/// `with_foreign` so a GTK/Swift/Kotlin observer object can implement it across
/// the FFI boundary.
#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait FeedSnapshotObserver: Send + Sync {
    fn on_changed(&self);
}
