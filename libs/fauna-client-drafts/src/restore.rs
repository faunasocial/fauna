//! The launch-restore body every tokio-native drafts leg shares: read the
//! manager's identity epoch, await the load, hand the bytes back under that
//! epoch.
//!
//! The ordering is the load-bearing part
//! (`docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy):
//! the epoch is read **synchronously, before the load is polled**. Reading it
//! after the load returns would take the incoming account's epoch and let the
//! outgoing account's late reply fill the incoming account's composer. It is
//! written once here so a leg cannot get it wrong; each leg keeps only its own
//! log wording (this crate carries no logging dependency), by matching the
//! returned [`LaunchRestore`].

use std::future::Future;

/// What a launch restore did, for the leg to log in its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRestore {
    /// A persisted blob of this many bytes was handed to the manager.
    Restored(usize),
    /// Nothing persisted yet (first run); the empty store stays.
    FirstRun,
    /// The load failed (transport or seal). Non-fatal — an unreachable nest must
    /// not blank the composer. Carries the error's display text.
    Failed(String),
}

/// The body of a leg's `restore_on_launch`, over any load. `read_epoch` is the
/// manager's identity epoch (`ConversationsManager::identity_epoch`) and is
/// called **here, synchronously**, so the returned future (`'static`, ready to
/// `spawn`) already carries it; `restore` hands the bytes back under that epoch
/// (`restore_drafts_at`, async because the conversations rail owes a restored
/// recipient picker its rail probe) and the manager refuses a stale one. Closures
/// rather than a trait: the managers live in crates that cannot depend on this
/// one, so a leg's impl of a trait defined here would be an orphan.
pub fn restore_when_loaded<E, R, Fut>(
    read_epoch: impl FnOnce() -> u64,
    load: impl Future<Output = Result<Option<Vec<u8>>, E>> + Send + 'static,
    restore: R,
) -> impl Future<Output = LaunchRestore> + Send + 'static
where
    E: std::fmt::Display + Send,
    R: FnOnce(u64, Vec<u8>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let epoch = read_epoch();
    async move {
        match load.await {
            Ok(Some(bytes)) => {
                let len = bytes.len();
                // Awaited: a dropped restore never fills the store, while the
                // completed load has already lifted `save_if_changed`'s gate —
                // the first autosave would replace the account's blob with this
                // device's edits alone.
                restore(epoch, bytes).await;
                LaunchRestore::Restored(len)
            }
            Ok(None) => LaunchRestore::FirstRun,
            Err(e) => LaunchRestore::Failed(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

    fn recorder(
        seen: &Seen,
    ) -> impl FnOnce(u64, Vec<u8>) -> std::future::Ready<()> + Send + 'static {
        let seen = Arc::clone(seen);
        move |epoch, bytes| {
            seen.lock().unwrap().push((epoch, bytes));
            std::future::ready(())
        }
    }

    /// The epoch is read when the function is CALLED, not when the load
    /// finishes: bump it between the call and the poll and the restore must
    /// still carry the old one. Mutation: read it inside the `async move`, and
    /// this reds.
    #[test]
    fn the_epoch_is_read_before_the_load_is_polled() {
        let epoch = Arc::new(AtomicU64::new(0));
        let seen = Seen::default();
        let read = Arc::clone(&epoch);
        let fut = restore_when_loaded(
            move || read.load(Ordering::SeqCst),
            async { Ok::<_, String>(Some(vec![1, 2, 3])) },
            recorder(&seen),
        );
        epoch.store(7, Ordering::SeqCst);
        assert_eq!(block_on(fut), LaunchRestore::Restored(3));
        assert_eq!(*seen.lock().unwrap(), vec![(0, vec![1, 2, 3])]);
    }

    #[test]
    fn first_run_and_failure_restore_nothing() {
        let seen = Seen::default();
        let none = restore_when_loaded(|| 0, async { Ok::<_, String>(None) }, recorder(&seen));
        assert_eq!(block_on(none), LaunchRestore::FirstRun);
        let err = restore_when_loaded(
            || 0,
            async { Err::<Option<Vec<u8>>, _>("nest unreachable".to_string()) },
            recorder(&seen),
        );
        assert_eq!(
            block_on(err),
            LaunchRestore::Failed("nest unreachable".into())
        );
        assert!(seen.lock().unwrap().is_empty());
    }
}
