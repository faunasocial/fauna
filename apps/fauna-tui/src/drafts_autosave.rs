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
use fauna_client_drafts::{DraftsSync, LEAVE_FLUSH_BUDGET, LaunchRestore, autosave_debounce};
use fauna_protocol::RpcRequester;
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
    /// The leave door's fill ([`LeaveFlush`]; `DraftsSync::save_on_leave`):
    /// the launch restore's FILL of `bytes` at `epoch`, synchronously and
    /// without the conversations rail's recipient probe (the session is
    /// leaving), then the snapshot to save.
    fn fill_drafts_for_leave(&self, epoch: u64, bytes: &[u8]) -> Vec<u8>;
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

/// The body of [`restore_on_launch`], over any load: the shared
/// [`fauna_client_drafts::restore_when_loaded`] (which reads the identity epoch
/// before the load is polled) over this host, logged under `log_prefix`.
pub(crate) fn restore_when_loaded<M, E>(
    manager: Arc<M>,
    load: impl Future<Output = Result<Option<Vec<u8>>, E>> + Send + 'static,
    log_prefix: &'static str,
) -> impl Future<Output = ()> + Send + 'static
where
    M: DraftsHost + Send + Sync + 'static,
    E: std::fmt::Display + Send,
{
    let host = Arc::clone(&manager);
    let restore = fauna_client_drafts::restore_when_loaded(
        move || manager.identity_epoch(),
        load,
        move |epoch, bytes| async move { host.restore_drafts_at(epoch, bytes).await },
    );
    async move {
        match restore.await {
            LaunchRestore::Restored(len) => tracing::info!("{log_prefix}: restored {len} bytes"),
            LaunchRestore::FirstRun => {
                tracing::debug!("{log_prefix}: none persisted yet (first run)")
            }
            LaunchRestore::Failed(e) => tracing::warn!("{log_prefix}: load failed: {e}"),
        }
    }
}

/// The leave-door flush of every drafts rail (`reserved-folders.md` § The
/// leave-flush promise): each rail's current compose, saved now, bypassing the
/// debounce. One body for both of tui's leave doors — the quit
/// (`main.rs`, awaited right before the process exits) and the session
/// teardown (`session::sign_out`, awaited on the stop task before the client
/// disconnects, which covers sign-out, account switch and the e2e reset alike).
/// A door taken before the launch load has lifted a rail's save gate still
/// saves: the rail reads once and fills from what rests before saving
/// (`DraftsSync::save_on_leave`).
///
/// Split into a synchronous [`Self::capture`] and an async [`Self::run`]
/// because the sign-out tears the rails down in the same synchronous call that
/// starts it: the snapshots must be read while the managers still hold the
/// user's last edit, and the saves ride the stop task afterwards. Each rail's
/// [`DraftsSync`] holds its own `Arc` of the session client, so the saves keep
/// a transport after the session has left `App`.
pub(crate) struct LeaveFlush {
    saves: Vec<RailSave>,
}

/// The leave fill a rail hands its [`RailSave`]: given the draft the door's
/// read found resting on the nest, the snapshot to save
/// (`DraftsSync::save_on_leave`).
type LeaveFill = Box<dyn FnOnce(Vec<u8>) -> Vec<u8> + Send>;

/// One rail's half of a [`LeaveFlush`]: the snapshot taken at capture, the
/// fill that applies a resting draft to it, and the sync that saves it.
/// Generic over the transport only so the tests can run it over an in-memory
/// nest; the app's rails all ride the session client.
struct RailSave<R: RpcRequester = Arc<NestClient>> {
    log_prefix: &'static str,
    sync: Arc<DraftsSync<R>>,
    snapshot: Vec<u8>,
    fill: LeaveFill,
}

impl<R: RpcRequester> RailSave<R> {
    /// A manager-backed rail's half: its snapshot now, and a fill that applies
    /// a resting draft to the same manager at the identity epoch read now, so
    /// a door that straddles an identity change fills nothing.
    fn of_manager<M: DraftsHost + Send + Sync + 'static>(
        log_prefix: &'static str,
        sync: &Arc<DraftsSync<R>>,
        manager: &Arc<M>,
    ) -> Self {
        let epoch = manager.identity_epoch();
        let host = Arc::clone(manager);
        Self {
            log_prefix,
            sync: Arc::clone(sync),
            snapshot: manager.drafts_snapshot_bytes(),
            fill: Box::new(move |resting| host.fill_drafts_for_leave(epoch, &resting)),
        }
    }

    /// The events rail's half: the form as the door found it, filled by the
    /// rail's own rule (`EventDrafts::filled_snapshot`).
    fn of_event_draft(
        sync: &Arc<DraftsSync<R>>,
        draft: fauna_client_caldav::drafts::EventDrafts,
    ) -> Self {
        Self {
            log_prefix: "event drafts",
            sync: Arc::clone(sync),
            snapshot: draft.snapshot_bytes(),
            fill: Box::new(move |resting| draft.filled_snapshot(&resting)),
        }
    }

    /// Open a gate the launch load has not lifted yet with one read, fill,
    /// and save — the whole of it under [`LEAVE_FLUSH_BUDGET`].
    async fn run(self) {
        let save = self.sync.save_on_leave(self.snapshot, self.fill);
        match tokio::time::timeout(LEAVE_FLUSH_BUDGET, save).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!("{}: leave-flush failed: {e}", self.log_prefix),
            Err(_) => tracing::warn!("{}: leave-flush timed out", self.log_prefix),
        }
    }
}

impl LeaveFlush {
    /// Snapshot every rail this session wired, or `None` when it wired none
    /// (pre-auth, or a session whose identity secret could not seal).
    pub(crate) fn capture(app: &crate::app::App) -> Option<Self> {
        let mut saves = Vec::new();
        if let (Some(manager), Some(sync)) =
            (&app.conversations.manager, &app.conversations.drafts_sync)
        {
            saves.push(RailSave::of_manager("drafts", sync, manager));
        }
        if let (Some(manager), Some(sync)) = (&app.feed.manager, &app.feed.drafts_sync) {
            saves.push(RailSave::of_manager("feed drafts", sync, manager));
        }
        if let Some(sync) = &app.events.drafts_sync {
            saves.push(RailSave::of_event_draft(sync, app.events.event_draft()));
        }
        (!saves.is_empty()).then_some(Self { saves })
    }

    /// Save every captured rail, concurrently, each under its own
    /// [`LEAVE_FLUSH_BUDGET`] — one rail's timeout never costs another its
    /// flush. A rail whose save gate the launch load has not lifted yet first
    /// reads once (`DraftsSync::save_on_leave`); an unchanged snapshot is a
    /// byte-compare, no upload.
    pub(crate) async fn run(self) {
        futures_util::future::join_all(self.saves.into_iter().map(RailSave::run)).await;
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
        fn fill_drafts_for_leave(&self, _epoch: u64, _bytes: &[u8]) -> Vec<u8> {
            self.drafts_snapshot_bytes()
        }
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

    /// The feed rail's leave save over an in-memory nest: `rest` is what
    /// another device left resting, `typed` the composer as this session's
    /// door finds it. Returns the nest, the leaving manager and the put count
    /// before the door.
    async fn feed_leave(
        rest: &str,
        typed: &str,
    ) -> (
        Arc<fauna_client_drafts::testing::MemoryDraftsNest>,
        Arc<crate::feed::CliFeedManager>,
        u32,
    ) {
        use fauna_client_drafts::testing::{MemoryDraftsNest, SharedDraftsNest};
        let nest = Arc::new(MemoryDraftsNest::default());
        let posts_sync = |nest: &Arc<MemoryDraftsNest>| {
            Arc::new(DraftsSync::new(
                SharedDraftsNest(nest.clone()),
                &fauna_core::identity::ActorKeypair::from_secret([5u8; 32]),
                fauna_protocol::drafts::RAIL_POSTS,
            ))
        };
        let feed_manager = || {
            Arc::new(crate::feed::CliFeedManager::new(
                crate::app::tests::test_session().client,
                [7u8; 32],
            ))
        };

        let other_device = feed_manager();
        other_device.update_compose(rest.to_string(), String::new(), None);
        let other_sync = posts_sync(&nest);
        other_sync.load().await.unwrap();
        assert!(
            other_sync
                .save_if_changed(&other_device.drafts_snapshot_bytes())
                .await
                .unwrap()
        );
        let puts_before = *nest.puts.lock().unwrap();

        // This session's launch load never landed: its gate is shut.
        let leaving = feed_manager();
        if !typed.is_empty() {
            leaving.update_compose(typed.to_string(), String::new(), None);
        }
        RailSave::of_manager("feed drafts", &posts_sync(&nest), &leaving)
            .run()
            .await;
        (nest, leaving, puts_before)
    }

    /// Read back what rests on the posts rail, as a fresh manager restores it.
    async fn resting_feed_text(
        nest: &Arc<fauna_client_drafts::testing::MemoryDraftsNest>,
    ) -> String {
        let reader = DraftsSync::new(
            fauna_client_drafts::testing::SharedDraftsNest(nest.clone()),
            &fauna_core::identity::ActorKeypair::from_secret([5u8; 32]),
            fauna_protocol::drafts::RAIL_POSTS,
        );
        let bytes = reader.load().await.unwrap().expect("a draft rests");
        let manager =
            crate::feed::CliFeedManager::new(crate::app::tests::test_session().client, [7u8; 32]);
        manager.restore_drafts(bytes);
        manager.snapshot().compose.text
    }

    /// A leave door taken before the launch load lifted the gate, over an
    /// untouched composer: the resting draft fills it and nothing is written
    /// over the draft another device left.
    #[tokio::test]
    async fn a_leave_door_over_a_shut_gate_never_overwrites_a_resting_draft() {
        let (nest, leaving, puts_before) = feed_leave("from the other device", "").await;
        assert_eq!(*nest.puts.lock().unwrap(), puts_before, "nothing written");
        assert_eq!(resting_feed_text(&nest).await, "from the other device");
        assert_eq!(leaving.snapshot().compose.text, "from the other device");
    }

    /// A leave door taken before the launch load lifted the gate, over a
    /// typed edit: one read, then the user's text is saved — before this, the
    /// shut gate refused the save and the edit was lost.
    #[tokio::test]
    async fn a_leave_door_over_a_shut_gate_saves_a_typed_edit() {
        let (nest, _leaving, _) =
            feed_leave("from the other device", "typed before the load").await;
        assert_eq!(resting_feed_text(&nest).await, "typed before the load");
    }

    /// The events rail's door: a typed form over a shut gate is saved.
    #[tokio::test]
    async fn an_event_leave_door_over_a_shut_gate_saves_a_typed_form() {
        use fauna_client_caldav::drafts::EventDrafts;
        use fauna_client_drafts::testing::{MemoryDraftsNest, SharedDraftsNest};
        let nest = Arc::new(MemoryDraftsNest::default());
        let events_sync = || {
            Arc::new(DraftsSync::new(
                SharedDraftsNest(nest.clone()),
                &fauna_core::identity::ActorKeypair::from_secret([5u8; 32]),
                fauna_protocol::drafts::RAIL_EVENTS,
            ))
        };
        let typed = EventDrafts {
            summary: "standup".into(),
            ..EventDrafts::default()
        };
        RailSave::of_event_draft(&events_sync(), typed.clone())
            .run()
            .await;
        let reader = events_sync();
        let rest = reader.load().await.unwrap().expect("a draft rests");
        assert_eq!(EventDrafts::restore_from_bytes(&rest).unwrap(), typed);
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
