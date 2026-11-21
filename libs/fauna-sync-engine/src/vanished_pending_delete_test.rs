//! A published pending row is a row with evidence — the engine's half of the
//! ruling (`docs/goal/behavior/delete-propagation.md` § *A wholesale-vanished
//! folder is infrastructure failure*, the paragraph **A published pending row
//! is a row with evidence**).
//!
//! A `LocallyModified` row with a recorded manifest is an edit of a path the
//! nest holds: its file was a real file on this disk, so its absence from a
//! scan that could look is the person's delete. It joins the delete universe
//! (`Synced` ∪ seen `Placeholder` ∪ `LocallyDeleted` ∪ published
//! `LocallyModified`), counts on both sides of the mass-delete floor, and a
//! missing one goes through `handle_delete` like a `Synced` row — record
//! first, tombstone on the nest's ack, never flipped to `LocallyDeleted`. A
//! never-published pending row stays outside the universe: the reconcile pass
//! settles it with nothing recorded (`settle_vanished_pending`).
//!
//! Engine shapes as in [`crate::offline_placeholder_delete_test`]: a
//! **folderless** engine has nothing to record, so its deletes land at once;
//! an engine over an unreachable nest fails every record; the record-counting
//! story runs over a real `NestClient` with only the socket mocked.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_core::identity::ActorKeypair;
use fauna_ws_substrate::{Supervisor, run_supervisor};

use crate::db::SyncState;
use crate::engine::SyncEngine;
use crate::offline_placeholder_delete_test::serve_records;
use crate::pull_remote_changes_test::{
    seed_tracked_file, test_engine, test_engine_folderless_with_progress,
    test_engine_with_nest_client,
};

fn state(engine: &SyncEngine, rel: &str) -> Option<SyncState> {
    engine.db().get_entry(rel).unwrap().map(|e| e.state)
}

fn folderless(dir: &std::path::Path) -> SyncEngine {
    test_engine_folderless_with_progress(dir.to_path_buf()).0
}

/// A file the nest holds, edited here and not yet uploaded: its row keeps the
/// recorded manifest and is `LocallyModified`.
fn seed_published_pending(engine: &SyncEngine, watch: &std::path::Path, rel: &str) {
    seed_tracked_file(engine, watch, rel, b"the version on the nest");
    std::fs::write(watch.join(rel), b"an edit made here, not yet uploaded").unwrap();
    engine
        .db()
        .update_state(rel, SyncState::LocallyModified)
        .unwrap();
    let row = engine.db().get_entry(rel).unwrap().unwrap();
    assert!(row.manifest_hash.is_some(), "precondition: published");
}

/// A file created here that never reached the nest: no recorded manifest.
fn seed_never_published_pending(engine: &SyncEngine, watch: &std::path::Path, rel: &str) {
    let full = watch.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, b"new here").unwrap();
    let hash = fauna_core::chunker_stream::content_hash_streaming(&full).unwrap();
    engine
        .db()
        .upsert_entry(
            rel,
            Some(hash),
            None,
            None,
            SyncState::LocallyModified,
            1_700_000_000,
            0,
            8,
            1,
            None,
        )
        .unwrap();
}

/// A real `NestClient` over a mocked socket that answers the change record
/// while `up` — the counter is the nest's record of deletes.
struct MockNest {
    engine: SyncEngine,
    up: Arc<AtomicBool>,
    records: Arc<AtomicI64>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl MockNest {
    fn start(dir: &std::path::Path, up: bool) -> Self {
        let auth = Arc::new(AuthClient::new(
            "http://127.0.0.1:0".into(),
            ActorKeypair::from_secret([9u8; 32]),
        ));
        let client = NestClient::with_auth(Arc::clone(&auth));
        let (slot, state_tx) = client.supervisor_channels_for_test();
        let (adapter, server) = mpsc_pair();
        let up = Arc::new(AtomicBool::new(up));
        let records = Arc::new(AtomicI64::new(0));
        let server_task = tokio::spawn(serve_records(server, up.clone(), records.clone()));
        let channel = Arc::new(MockClientChannel::new(
            connect_queue([Ok(adapter)]),
            Arc::clone(&auth),
            PushBroker::new(16),
        ));
        let supervisor = tokio::spawn(async move {
            let _ = run_supervisor(Supervisor {
                channel,
                dispatcher_slot: slot,
                connection_state_tx: state_tx,
                initial_backoff: Duration::from_millis(10),
                max_backoff: Duration::from_millis(50),
            })
            .await;
        });
        let engine = test_engine_with_nest_client(dir.to_path_buf(), "docs", client);
        Self {
            engine,
            up,
            records,
            tasks: vec![server_task, supervisor],
        }
    }

    async fn reconcile(&self) -> crate::engine::ReconcileStats {
        tokio::time::timeout(Duration::from_secs(10), self.engine.reconcile())
            .await
            .expect("reconcile must not hang")
            .unwrap()
    }
}

impl Drop for MockNest {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// (a) A published pending file removed while nothing watched, beside a
/// present `Synced` file: one delete is recorded on the nest, the row is
/// tombstoned, and no upload of it is pending any more.
#[tokio::test]
async fn a_published_pending_file_gone_records_one_delete() {
    let dir = tempfile::tempdir().unwrap();
    let nest = MockNest::start(dir.path(), true);
    seed_tracked_file(&nest.engine, dir.path(), "kept.txt", b"still here");
    seed_published_pending(&nest.engine, dir.path(), "edited.txt");
    std::fs::remove_file(dir.path().join("edited.txt")).unwrap();

    let stats = nest.reconcile().await;
    assert_eq!(stats.deletes_held, 0, "one of two is not totality");
    assert_eq!(stats.deleted_files, 1, "the delete propagates");
    assert_eq!(nest.records.load(Ordering::SeqCst), 1, "exactly one record");
    assert_eq!(state(&nest.engine, "edited.txt"), Some(SyncState::Deleted));
    assert_eq!(state(&nest.engine, "kept.txt"), Some(SyncState::Synced));
    assert!(
        nest.engine
            .db()
            .list_by_state(SyncState::LocallyModified)
            .unwrap()
            .is_empty(),
        "upload_pending has nothing left to retry"
    );
}

/// (f) The record fails (nest unreachable): the row stays `LocallyModified` —
/// never `LocallyDeleted`, the disk holds the evidence — and the next pass,
/// with the nest back, records it and only then tombstones.
#[tokio::test]
async fn a_failed_record_leaves_the_row_pending_and_the_next_pass_lands_it() {
    let dir = tempfile::tempdir().unwrap();
    let nest = MockNest::start(dir.path(), false);
    seed_tracked_file(&nest.engine, dir.path(), "kept.txt", b"still here");
    seed_published_pending(&nest.engine, dir.path(), "edited.txt");
    std::fs::remove_file(dir.path().join("edited.txt")).unwrap();

    let stats = nest.reconcile().await;
    assert_eq!(stats.deleted_files, 0, "the record failed");
    assert_eq!(
        state(&nest.engine, "edited.txt"),
        Some(SyncState::LocallyModified),
        "no flip: the row stays pending with no file"
    );

    nest.up.store(true, Ordering::SeqCst);
    let stats = nest.reconcile().await;
    assert_eq!(stats.deleted_files, 1, "the owed delete lands");
    assert_eq!(nest.records.load(Ordering::SeqCst), 1, "exactly one record");
    assert_eq!(state(&nest.engine, "edited.txt"), Some(SyncState::Deleted));
}

/// (b) Every row with evidence gone at once, one of them published pending:
/// the floor holds, its count includes the pending row, nothing is recorded,
/// and the row stays `LocallyModified`. The folder coming back finds the edit
/// still pending; a second vanish is applied by the user's confirm.
#[tokio::test]
async fn a_wholesale_vanish_holds_the_published_pending_row_too() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "a.txt", b"alpha");
    seed_published_pending(&engine, dir.path(), "sub/edited.txt");
    let edited = std::fs::read(dir.path().join("sub/edited.txt")).unwrap();
    std::fs::remove_file(dir.path().join("a.txt")).unwrap();
    std::fs::remove_file(dir.path().join("sub/edited.txt")).unwrap();

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deletes_held, 2, "the pending row counts in the hold");
    assert_eq!(stats.deleted_files, 0, "a hold records nothing");
    assert_eq!(
        state(&engine, "sub/edited.txt"),
        Some(SyncState::LocallyModified)
    );
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Synced));

    // The folder comes back: the edit is still pending, the upload goes on.
    std::fs::write(dir.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(dir.path().join("sub/edited.txt"), &edited).unwrap();
    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deletes_held, 0);
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(
        state(&engine, "sub/edited.txt"),
        Some(SyncState::LocallyModified),
        "the edit waits for its upload"
    );

    // Gone again, and this time the user confirms.
    std::fs::remove_file(dir.path().join("a.txt")).unwrap();
    std::fs::remove_file(dir.path().join("sub/edited.txt")).unwrap();
    let applied = engine.apply_held_deletes().await.unwrap();
    assert!(applied.floor_was_active);
    assert_eq!(
        applied.applied, 2,
        "the confirm applies the pending row too"
    );
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Deleted));
    assert_eq!(state(&engine, "sub/edited.txt"), Some(SyncState::Deleted));
}

/// (c) Every `Synced` file gone but a published pending file PRESENT: the
/// pending file is evidence the folder is there, so nothing holds and the
/// deletes propagate per file.
#[tokio::test]
async fn a_present_published_pending_file_keeps_the_floor_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "a.txt", b"alpha");
    seed_tracked_file(&engine, dir.path(), "b.txt", b"beta");
    seed_published_pending(&engine, dir.path(), "edited.txt");
    std::fs::remove_file(dir.path().join("a.txt")).unwrap();
    std::fs::remove_file(dir.path().join("b.txt")).unwrap();

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deletes_held, 0, "two of three is not totality");
    assert_eq!(stats.deleted_files, 2, "the deletes propagate per file");
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Deleted));
    assert_eq!(state(&engine, "b.txt"), Some(SyncState::Deleted));
    assert_eq!(
        state(&engine, "edited.txt"),
        Some(SyncState::LocallyModified)
    );
}

/// (d) Under an unreadable prefix the published pending row is withheld and
/// counted, like a `Synced` row.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_prefix_withholds_a_published_pending_row() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    seed_published_pending(&engine, dir.path(), "sub/edited.txt");
    let sub = dir.path().join("sub");

    let stats = crate::mass_delete_floor_test::while_unreadable(&sub, || engine.reconcile())
        .await
        .unwrap();
    assert_eq!(stats.deleted_files, 0, "unreadable is not absent");
    assert_eq!(stats.deletes_skipped_unreadable, 1);
    assert_eq!(
        state(&engine, "sub/edited.txt"),
        Some(SyncState::LocallyModified)
    );
}

/// (e) A never-published pending row gone is settled — removed, nothing
/// recorded — and never counted by the floor: two `Synced` rows gone beside
/// it hold as two, not three.
#[tokio::test]
async fn a_never_published_pending_row_stays_outside_the_universe() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    seed_never_published_pending(&engine, dir.path(), "new.txt");
    std::fs::remove_file(dir.path().join("new.txt")).unwrap();

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.deletes_held, 0);
    assert_eq!(stats.deleted_files, 0, "nothing is recorded for it");
    assert_eq!(state(&engine, "new.txt"), None, "its row is settled");

    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "a.txt", b"alpha");
    seed_tracked_file(&engine, dir.path(), "b.txt", b"beta");
    seed_never_published_pending(&engine, dir.path(), "new.txt");
    for rel in ["a.txt", "b.txt", "new.txt"] {
        std::fs::remove_file(dir.path().join(rel)).unwrap();
    }
    let stats = engine.reconcile().await.unwrap();
    assert_eq!(
        stats.deletes_held, 2,
        "the never-published row is not counted"
    );
    assert_eq!(
        state(&engine, "new.txt"),
        Some(SyncState::LocallyModified),
        "under a hold nothing is settled"
    );
}

/// (g) The re-stat: a published pending row missing from the scan whose file
/// is present by the time the derivation looks (the scan raced an atomic
/// save) is not deleted, and leaves the pass's universe.
#[tokio::test]
async fn a_pending_row_present_at_the_re_stat_is_not_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    seed_published_pending(&engine, dir.path(), "edited.txt");
    // A scan that did not see `edited.txt` — taken mid-save.
    let scan = crate::watcher::DirectoryScan {
        files: crate::watcher::full_scan_filtered(dir.path(), &Default::default())
            .unwrap()
            .files
            .into_iter()
            .filter(|f| f.relative_path != "edited.txt")
            .collect(),
        unreadable_prefixes: Vec::new(),
    };
    let rows = engine.rows_with_evidence().unwrap();
    assert_eq!(rows.len(), 2, "the published pending row is evidence");
    let derived = engine.missing_from_scan(&scan, rows);
    assert!(derived.missing.is_empty(), "present at the re-stat");
    assert!(derived.withheld_unreadable.is_empty());
    assert_eq!(
        derived.observable(2),
        1,
        "the raced row is out of this pass's universe"
    );
}

/// (g′) The re-stat answering neither present nor absent withholds the row
/// like an unreadable prefix.
#[cfg(unix)]
#[tokio::test]
async fn a_pending_row_the_re_stat_cannot_read_is_withheld() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    seed_published_pending(&engine, dir.path(), "sub/edited.txt");
    std::fs::remove_file(dir.path().join("sub/edited.txt")).unwrap();
    // Scanned while readable: `sub` enumerated, the file not in it.
    let scan = crate::watcher::full_scan_filtered(dir.path(), &Default::default()).unwrap();
    let rows = engine.rows_with_evidence().unwrap();
    let sub = dir.path().join("sub");
    let derived = crate::mass_delete_floor_test::while_unreadable(&sub, || async {
        engine.missing_from_scan(&scan, rows)
    })
    .await;
    assert!(derived.missing.is_empty(), "unknown is not absent");
    assert_eq!(
        derived.withheld_unreadable,
        vec!["sub/edited.txt".to_string()]
    );
}

/// A path whose published pending row was tombstoned by the delete, re-created
/// later with the very bytes of the edit, is taken up again as a pending
/// upload — the tombstone leaves nothing a re-create trips on.
#[tokio::test]
async fn a_re_created_path_after_the_delete_is_taken_up_again() {
    let dir = tempfile::tempdir().unwrap();
    let engine = folderless(dir.path());
    seed_tracked_file(&engine, dir.path(), "kept.txt", b"here");
    seed_published_pending(&engine, dir.path(), "edited.txt");
    let edited = std::fs::read(dir.path().join("edited.txt")).unwrap();
    std::fs::remove_file(dir.path().join("edited.txt")).unwrap();
    engine.reconcile().await.unwrap();
    assert_eq!(state(&engine, "edited.txt"), Some(SyncState::Deleted));

    std::fs::write(dir.path().join("edited.txt"), &edited).unwrap();
    engine.reconcile().await.unwrap();
    assert_eq!(
        state(&engine, "edited.txt"),
        Some(SyncState::LocallyModified),
        "the re-created file waits for its upload"
    );
}
