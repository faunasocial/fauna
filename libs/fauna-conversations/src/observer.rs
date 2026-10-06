//! Reactivity callback. The manager notifies its observers on every
//! state mutation; observers re-read fresh state via
//! `ConversationsManager::snapshot()`. Mirror of `LaunchObserver` in
//! `fauna-launch-machine`.

#[cfg_attr(feature = "uniffi", uniffi::export(with_foreign))]
pub trait SnapshotObserver: Send + Sync {
    /// Called whenever the manager's observable state changes. The
    /// observer reads fresh snapshots via `ConversationsManager::snapshot()`.
    fn on_changed(&self);
}
