//! Shared debounced-autosave glue for the tokio-native drafts legs
//! (`conversations::drafts`, `feed::drafts`) — the manager-backed shape,
//! distinct from the events rail's value-carrying channel (see
//! `events::drafts`'s own doc comment for why that one stays separate).
//!
//! Both legs wire an identical `Weak<Manager>` + unbounded-tick-channel +
//! [`tokio::select!`] debounce loop over their own manager type and their own
//! foreign observer trait (`fauna_conversations::observer::SnapshotObserver`
//! vs `fauna_feed::FeedSnapshotObserver`) — the near-duplicate-fns sweep
//! caught `quiesce`, the autosave loop body, and the launch-restore body as
//! byte-for-byte identical modulo the manager type. [`DraftsHost`] is the
//! seam: each rail keeps its own observer struct and `add_observer` call (the
//! part that genuinely differs), then hands the resulting receiver + `Weak`
//! manager to [`run_autosave_loop`].

use std::sync::{Arc, Weak};

use fauna_client::NestClient;
use fauna_client_drafts::{DraftsSync, autosave_debounce};
use tokio::sync::mpsc::UnboundedReceiver;

/// A manager that owns compose state which can be snapshotted for the shared
/// drafts-sync plane and restored from it. One impl per rail's manager type —
/// the identity that stays genuinely per-rail; the glue around it does not.
pub(crate) trait DraftsHost {
    fn drafts_snapshot_bytes(&self) -> Vec<u8>;
    /// Which identity the manager serves right now
    /// (`ConversationsManager::identity_epoch`). Read before the launch load
    /// starts and handed back to [`Self::restore_drafts_at`], so a load the
    /// outgoing account started fills nothing the incoming one sees
    /// (`account-scoping.md` § The scoping taxonomy).
    fn identity_epoch(&self) -> u64;
    /// Async because the conversations rail's restore owes the recipient picker
    /// a probe (`ConversationsManager::restore_drafts_at`); the feed rail has no
    /// picker and its body is still synchronous.
    fn restore_drafts_at(&self, epoch: u64, bytes: Vec<u8>) -> impl Future<Output = ()> + Send;
}

/// Spawn the launch load: fetch + unseal this actor's drafts and hand them to
/// the manager. `Ok(None)` is first run (keep the empty store); a
/// transport/seal error is logged and left non-fatal — an unreachable nest
/// must not blank the composer.
pub(crate) fn restore_on_launch<M: DraftsHost + Send + Sync + 'static>(
    manager: Arc<M>,
    sync: Arc<DraftsSync<Arc<NestClient>>>,
    log_prefix: &'static str,
) {
    tokio::spawn(restore_when_loaded(
        manager,
        async move { sync.load().await },
        log_prefix,
    ));
}

/// The body of [`restore_on_launch`], over any load. The identity epoch is read
/// **here, synchronously, before the load is polled** — reading it after the
/// load returns would take the incoming account's epoch and let the outgoing
/// account's reply through.
pub(crate) fn restore_when_loaded<M, E>(
    manager: Arc<M>,
    load: impl Future<Output = Result<Option<Vec<u8>>, E>> + Send + 'static,
    log_prefix: &'static str,
) -> impl Future<Output = ()> + Send + 'static
where
    M: DraftsHost + Send + Sync + 'static,
    E: std::fmt::Display + Send,
{
    let epoch = manager.identity_epoch();
    async move {
        match load.await {
            Ok(Some(bytes)) => {
                tracing::info!("{log_prefix}: restored {} bytes", bytes.len());
                manager.restore_drafts_at(epoch, bytes).await;
            }
            Ok(None) => tracing::debug!("{log_prefix}: none persisted yet (first run)"),
            Err(e) => tracing::warn!("{log_prefix}: load failed: {e}"),
        }
    }
}

/// Force an immediate save of `manager`'s current snapshot, bypassing the
/// debounce entirely — the leave-door flush (`reserved-folders.md` § The
/// leave-flush promise, row 481), awaited directly in `main.rs` right before
/// the process exits (tui runs natively inside the tokio runtime for its
/// whole life, so no thread-spinning is needed the way linux's GTK main
/// thread requires).
pub(crate) async fn flush_now<M: DraftsHost>(
    manager: &M,
    sync: &DraftsSync<Arc<NestClient>>,
    log_prefix: &'static str,
) {
    let snapshot = manager.drafts_snapshot_bytes();
    if let Err(e) = sync.save_if_changed(&snapshot).await {
        tracing::warn!("{log_prefix}: leave-flush failed: {e}");
    }
}

/// The debounced autosave loop body: wait for a tick, quiesce, snapshot,
/// save. Ends when the channel closes (the observer — and with it the
/// sender — was dropped along with the manager) or the manager itself is
/// gone (checked without holding a strong ref across the upload, so a
/// torn-down session's manager is never kept alive by an in-flight save).
pub(crate) async fn run_autosave_loop<M: DraftsHost>(
    mut rx: UnboundedReceiver<()>,
    manager: Weak<M>,
    sync: Arc<DraftsSync<Arc<NestClient>>>,
    log_prefix: &'static str,
) {
    while rx.recv().await.is_some() {
        if !quiesce(&mut rx).await {
            return;
        }
        let Some(snapshot) = manager.upgrade().map(|m| m.drafts_snapshot_bytes()) else {
            return;
        };
        if let Err(e) = sync.save_if_changed(&snapshot).await {
            tracing::warn!("{log_prefix}: autosave failed: {e}");
        }
    }
}

/// Wait for [`autosave_debounce`] of quiescence, re-arming on every further
/// tick of the same edit burst. Returns `false` if the channel closed while
/// waiting (the caller must then retire). `pub(crate)` so each rail's own
/// tests can drive a real burst through it directly, the same door
/// [`run_autosave_loop`] uses internally.
pub(crate) async fn quiesce(rx: &mut UnboundedReceiver<()>) -> bool {
    loop {
        tokio::select! {
            // `sleep` and `UnboundedReceiver::recv` are both cancel-safe, which
            // is what makes re-arming here lossless.
            _ = tokio::time::sleep(autosave_debounce()) => return true,
            tick = rx.recv() => {
                if tick.is_none() {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use tokio::sync::mpsc::unbounded_channel;

    use super::*;

    /// Ceiling on any single await in these tests — see
    /// `conversations::drafts`'s identical constant for why this is
    /// load-bearing, not belt-and-braces.
    const TEST_AWAIT_CEILING: Duration = Duration::from_secs(60);

    async fn bounded<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
        match tokio::time::timeout(TEST_AWAIT_CEILING, fut).await {
            Ok(v) => v,
            Err(_) => panic!("timed out waiting for {what} — the autosave trigger is not firing"),
        }
    }

    struct CountingHost {
        snapshot_calls: AtomicU32,
    }

    impl DraftsHost for CountingHost {
        fn drafts_snapshot_bytes(&self) -> Vec<u8> {
            self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
            b"snapshot".to_vec()
        }
        fn identity_epoch(&self) -> u64 {
            0
        }
        async fn restore_drafts_at(&self, _epoch: u64, _bytes: Vec<u8>) {}
    }

    fn offline_sync() -> Arc<DraftsSync<Arc<NestClient>>> {
        Arc::new(DraftsSync::new(
            NestClient::new(
                "http://127.0.0.1:1".to_string(),
                fauna_core::identity::ActorKeypair::generate(),
            ),
            &fauna_core::identity::ActorKeypair::generate(),
            "conversations",
        ))
    }

    /// A burst of ticks coalesces into ONE snapshot call. Doesn't wait for the
    /// loop to retire: the save that follows the snapshot is a real (offline,
    /// failing) network attempt, and joining the task would make the
    /// assertion depend on how long that attempt takes to error out — the
    /// snapshot call itself is synchronous and happens before it, so a couple
    /// of `yield_now`s are enough to observe it without touching real I/O
    /// timing.
    #[tokio::test(start_paused = true)]
    async fn a_burst_of_ticks_coalesces_into_one_snapshot() {
        let host = Arc::new(CountingHost {
            snapshot_calls: AtomicU32::new(0),
        });
        let (tx, rx) = unbounded_channel();
        let weak = Arc::downgrade(&host);
        let sync = offline_sync();
        let task = tokio::spawn(run_autosave_loop(rx, weak, sync, "test"));

        tx.send(()).unwrap();
        tx.send(()).unwrap();
        tx.send(()).unwrap();
        // Let the spawned task run up to the point it parks on the debounce
        // timer inside `quiesce` before advancing the virtual clock —
        // otherwise `advance` can race a task that hasn't registered its
        // timer yet.
        tokio::task::yield_now().await;
        tokio::time::advance(autosave_debounce() + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            host.snapshot_calls.load(Ordering::SeqCst),
            1,
            "three ticks in one burst must coalesce into exactly one snapshot"
        );
        task.abort();
    }

    /// The loop must retire when the manager is gone, not resurrect it.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_manager_retires_the_loop_without_a_snapshot() {
        let host = Arc::new(CountingHost {
            snapshot_calls: AtomicU32::new(0),
        });
        let weak = Arc::downgrade(&host);
        drop(host);
        let (tx, rx) = unbounded_channel();
        let sync = offline_sync();
        let task = tokio::spawn(run_autosave_loop(rx, weak, sync, "test"));

        tx.send(()).unwrap();
        tokio::time::advance(autosave_debounce() + Duration::from_millis(1)).await;
        bounded("the loop to retire on a gone manager", task)
            .await
            .unwrap();
    }

    /// The channel closing mid-wait is the retirement path, and `quiesce` must
    /// report it rather than falling through to a save — moved here from the
    /// feed leg's test module, which tested this
    /// generic contract with no manager involved at all.
    #[tokio::test(start_paused = true)]
    async fn a_closed_channel_retires_the_debounce() {
        let (tx, mut rx) = unbounded_channel::<()>();
        tx.send(()).unwrap();
        assert!(bounded("the queued tick", rx.recv()).await.is_some());
        drop(tx);
        assert!(
            !bounded("the closed channel", quiesce(&mut rx)).await,
            "a closed channel must end the task, not fall through to a save",
        );
    }
}
