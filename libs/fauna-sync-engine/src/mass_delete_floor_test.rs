//! The mass-delete floor: reconcile refuses to read a wholesale-vanished folder
//! as user intent (`docs/goal/behavior/file-sync.md` § Files Appear
//! Automatically; no-user-data-loss, `docs/goal/principles.md`).
//!
//! The incident class this pins (2026-07-24, live `e2e-multiseat` set): a bound
//! folder's backing store goes away while its root survives — an unmounted or
//! renamed external volume in production; a GC'd tmp dir under an orphaned
//! agent in the harness — and the next reconcile's scan comes back empty. The
//! pre-floor delete-detection pass recorded a delete for EVERY `Synced` row
//! missing from the scan, erasing the nest copy of the whole set. The
//! discriminating shape is *totality*: every synced row of a multi-folder
//! missing at once. A partial vanish stays ordinary sync semantics, and the
//! hold is DERIVED (re-evaluated per reconcile, nothing stored), so files
//! reappearing resume sync with nothing lost.
//!
//! Uses [`pull_remote_changes_test`]'s harness: the engine's clients point at
//! an unreachable URL, so any attempted `record_change` FAILS — which is what
//! makes `unchanged_files` after restoration an end-to-end witness (a row that
//! had been tombstoned or state-flipped could not come back as `unchanged`).

use crate::progress::ProgressEvent;
use crate::pull_remote_changes_test::{
    seed_tracked_file, seed_tracked_row, test_engine, test_engine_with_progress,
};

/// Drain every `DeletesHeld` count the engine reported outward this pass.
///
/// A `Vec` rather than a single value on purpose: "reported exactly once per
/// reconcile" is itself part of the contract — a surface fed two counts for one
/// pass would render whichever arrived last, and a duplicated non-zero would
/// survive a `== Some(n)` assertion unnoticed.
fn drained_held_counts(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>) -> Vec<u64> {
    let mut held = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ProgressEvent::DeletesHeld { held: n } = event {
            held.push(n);
        }
    }
    held
}

/// The floor itself: every synced file of a multi-folder missing at once —
/// root present, contents gone — records NOTHING, reports the held count, and
/// resumes losslessly when the files come back (the remounted-drive story).
#[tokio::test]
async fn a_wholesale_vanished_folder_holds_instead_of_propagating() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    for (path, body) in [
        ("a.txt", b"alpha".as_slice()),
        ("b.txt", b"beta"),
        ("sub/c.txt", b"gamma"),
    ] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }

    // The dangerous shape: the root still exists (a missing root errors the
    // scan — already safe), but every tracked file is gone at once.
    for path in ["a.txt", "b.txt", "sub/c.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }

    let stats = engine.reconcile().await.expect("reconcile");
    assert_eq!(
        stats.deleted_files, 0,
        "a wholesale vanish must not record deletes"
    );
    assert_eq!(
        stats.deletes_held, 3,
        "the floor reports what it held, for the status surface"
    );

    // The remounted-drive recovery: the files come back, and the next
    // reconcile sees them UNCHANGED — which proves the hold left every row
    // untouched (`Synced`, same hash/mtime metadata): a recorded or
    // tombstoned row would re-enter as new/modified instead.
    for (path, body) in [
        ("a.txt", b"alpha".as_slice()),
        ("b.txt", b"beta"),
        ("sub/c.txt", b"gamma"),
    ] {
        let full = dir.path().join(path);
        std::fs::write(&full, body).unwrap();
    }
    let stats = engine.reconcile().await.expect("reconcile after restore");
    assert_eq!(
        stats.deletes_held, 0,
        "the hold is derived — it clears itself"
    );
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(
        stats.new_files, 0,
        "no row was tombstoned during the hold — nothing re-enters as new"
    );
}

/// A partial vanish is ordinary sync semantics: some-but-not-all missing rows
/// still go through the per-file delete path (which here ATTEMPTS the record
/// against the unreachable nest — `deleted_files` stays 0 because the record
/// fails, but the floor must not have engaged).
#[tokio::test]
async fn a_partial_delete_still_takes_the_per_file_path() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "keep.txt", b"kept");
    seed_tracked_file(&engine, dir.path(), "gone.txt", b"going");

    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();

    let stats = engine.reconcile().await.expect("reconcile");
    assert_eq!(
        stats.deletes_held, 0,
        "one of two missing is not totality — the floor must not engage"
    );
    // The record fails (unreachable nest), so the row survives for retry —
    // the pre-existing record-first contract, unchanged by the floor.
    let entry = engine.db().get_entry("gone.txt").unwrap().unwrap();
    assert_eq!(entry.state, crate::db::SyncState::Synced);
}

/// The deliberate MIN=2 boundary: emptying a single-folder still propagates.
/// One file's absence is far likelier a genuine deletion than an unmount, and
/// the loss magnitude is one file (recoverable via § File Versions) — pinned so
/// the boundary is a decision, not an accident.
#[tokio::test]
async fn emptying_a_single_folder_still_takes_the_per_file_path() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "only.txt", b"alone");

    std::fs::remove_file(dir.path().join("only.txt")).unwrap();

    let stats = engine.reconcile().await.expect("reconcile");
    assert_eq!(
        stats.deletes_held, 0,
        "a 1-folder is below the floor — its delete is treated as intent"
    );
    let entry = engine.db().get_entry("only.txt").unwrap().unwrap();
    assert_eq!(
        entry.state,
        crate::db::SyncState::Synced,
        "record attempted and failed (unreachable nest) — row survives for retry"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The status surface: what the floor reports OUTWARD
//
// `ReconcileStats` is returned to whoever called `reconcile()`, and the
// resident loop's `converge` discards it (`always_resident.rs`) — so before
// this, a held set was visible only in a log line. These pin the outward
// report the sync-agent status projection reads (`file-sync.md` § Files Appear
// Automatically: *"surfacing that count on the sync-agent status projection"*).
// ─────────────────────────────────────────────────────────────────────

/// The hold is reported outward, once, with the count — the engine-side half of
/// the status surface.
#[tokio::test]
async fn a_held_set_reports_its_count_on_the_progress_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) = test_engine_with_progress(dir.path().to_path_buf());
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }
    // Seeding uploads; only the reconcile under test may be observed.
    let _ = drained_held_counts(&mut progress);

    for path in ["a.txt", "b.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }
    engine.reconcile().await.expect("reconcile");

    assert_eq!(
        drained_held_counts(&mut progress),
        vec![2],
        "the floor must report its count outward exactly once — a status surface \
         fed nothing renders nothing, and fed twice renders the last one"
    );
}

/// **The self-clearing half, and the reason this rides a per-pass emission
/// rather than a stored field.** The hold is derived — `file-sync.md` ties the
/// floor's crash-safety to nothing being stored — so the surface must be told
/// the drive came back, not merely stop being told it went away. A report that
/// only ever fires on the held branch would leave "N deletions held" painted
/// forever after a remount.
#[tokio::test]
async fn a_reconcile_that_holds_nothing_reports_zero_so_the_surface_clears() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) = test_engine_with_progress(dir.path().to_path_buf());
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }
    for path in ["a.txt", "b.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }
    engine.reconcile().await.expect("reconcile");
    let _ = drained_held_counts(&mut progress);

    // The drive comes back.
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        std::fs::write(dir.path().join(path), body).unwrap();
    }
    engine.reconcile().await.expect("reconcile after restore");

    assert_eq!(
        drained_held_counts(&mut progress),
        vec![0],
        "a pass that held nothing must SAY so — the surface clears on the report, \
         not on the absence of one"
    );
}

/// A partial vanish never engages the floor, so it reports a plain zero — the
/// negative case, pinned so the emission can never be mistaken for "some
/// deletes happened" (which is `deleted_files`, a different number).
#[tokio::test]
async fn a_partial_vanish_reports_zero_held() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) = test_engine_with_progress(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "keep.txt", b"kept");
    seed_tracked_file(&engine, dir.path(), "gone.txt", b"going");
    let _ = drained_held_counts(&mut progress);

    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
    engine.reconcile().await.expect("reconcile");

    assert_eq!(
        drained_held_counts(&mut progress),
        vec![0],
        "one of two missing is ordinary sync — nothing was HELD"
    );
}

// ─────────────────────────────────────────────────────────────────────
// The verb: `apply_held_deletes` — user-confirmed propagation of a hold
//
// `delete-propagation.md` § the mass-delete floor: *"propagation of a held set
// is an explicit user action, never this pass"*. The pins below are the verb's
// three safety properties (see its doc): re-derive-now, floor-gated, and
// record-first resumable. The folderless fixture makes the tombstone half
// observable (no folder → nothing to record → `handle_delete` completes
// locally); the folder-bound fixture pins the failed-record retry contract.
// ─────────────────────────────────────────────────────────────────────

/// The confirmed path: a held set is driven through `handle_delete` row by
/// row — tombstoned, counted, and the surface told the new (zero) count
/// immediately rather than at the next rescan tick.
#[tokio::test]
async fn apply_held_deletes_applies_a_held_set_and_clears_the_surface() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) =
        crate::pull_remote_changes_test::test_engine_folderless_with_progress(
            dir.path().to_path_buf(),
        );
    for (path, body) in [
        ("a.txt", b"alpha".as_slice()),
        ("b.txt", b"beta"),
        ("sub/c.txt", b"gamma"),
    ] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }
    for path in ["a.txt", "b.txt", "sub/c.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }
    engine.reconcile().await.expect("reconcile");
    assert_eq!(drained_held_counts(&mut progress), vec![3], "held first");

    let outcome = engine.apply_held_deletes().await.expect("apply");
    assert!(outcome.floor_was_active);
    assert_eq!(outcome.applied, 3, "every held row driven through");
    assert_eq!(outcome.remaining_held, 0);
    for path in ["a.txt", "b.txt", "sub/c.txt"] {
        let entry = engine.db().get_entry(path).unwrap().unwrap();
        assert_eq!(
            entry.state,
            crate::db::SyncState::Deleted,
            "{path}: applied means tombstoned, exactly as a per-file delete"
        );
    }
    assert_eq!(
        drained_held_counts(&mut progress),
        vec![0],
        "the surface clears on the apply's own report, not a later rescan"
    );

    // The pass after: nothing held, nothing re-detected — the verb left the
    // same steady state a per-file propagation would have.
    let stats = engine.reconcile().await.expect("reconcile after apply");
    assert_eq!(stats.deletes_held, 0);
    assert_eq!(stats.deleted_files, 0);
}

/// **The data-loss race this verb exists to survive:** the user confirms
/// against a stale "N deletions held" and the drive remounts before the click
/// lands. The verb re-derives NOW and applies nothing — a displayed count is
/// never consumed.
#[tokio::test]
async fn apply_held_deletes_refuses_when_the_files_came_back() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) =
        crate::pull_remote_changes_test::test_engine_folderless_with_progress(
            dir.path().to_path_buf(),
        );
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }
    for path in ["a.txt", "b.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }
    engine.reconcile().await.expect("reconcile");
    assert_eq!(
        drained_held_counts(&mut progress),
        vec![2],
        "hold displayed"
    );

    // The remount, between render and click.
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        std::fs::write(dir.path().join(path), body).unwrap();
    }

    let outcome = engine.apply_held_deletes().await.expect("apply");
    assert!(
        !outcome.floor_was_active,
        "no hold is active NOW — the stale confirm must not delete present files"
    );
    assert_eq!(outcome.applied, 0);
    for path in ["a.txt", "b.txt"] {
        let entry = engine.db().get_entry(path).unwrap().unwrap();
        assert_eq!(
            entry.state,
            crate::db::SyncState::Synced,
            "{path}: untouched"
        );
    }
    assert_eq!(
        drained_held_counts(&mut progress),
        vec![0],
        "the refusal still clears the stale surface"
    );
}

/// A partial vanish is ordinary reconcile territory: the verb is the hold's
/// counterpart, not a general force-delete API — below the floor it refuses
/// and touches nothing (the next reconcile owns the per-file path).
#[tokio::test]
async fn apply_held_deletes_refuses_a_partial_vanish() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) =
        crate::pull_remote_changes_test::test_engine_folderless_with_progress(
            dir.path().to_path_buf(),
        );
    seed_tracked_file(&engine, dir.path(), "keep.txt", b"kept");
    seed_tracked_file(&engine, dir.path(), "gone.txt", b"going");
    let _ = drained_held_counts(&mut progress);

    std::fs::remove_file(dir.path().join("gone.txt")).unwrap();

    let outcome = engine.apply_held_deletes().await.expect("apply");
    assert!(!outcome.floor_was_active);
    assert_eq!(outcome.applied, 0, "one of two missing is not a hold");
    let entry = engine.db().get_entry("gone.txt").unwrap().unwrap();
    assert_eq!(
        entry.state,
        crate::db::SyncState::Synced,
        "the missing row is reconcile's to propagate, not the verb's"
    );
}

/// Record-first, resumable: on the folder-bound fixture every record FAILS
/// (unreachable nest), so an apply completes nothing — and every row survives
/// `Synced`, the floor still holds, and a re-invoke retries. A crash mid-apply
/// has the same shape: nothing stored, nothing stranded.
#[tokio::test]
async fn a_failed_record_keeps_the_rows_for_a_resumed_apply() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }
    for path in ["a.txt", "b.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }

    let outcome = engine.apply_held_deletes().await.expect("apply");
    assert!(outcome.floor_was_active, "the hold was real");
    assert_eq!(outcome.applied, 0, "no record reached the nest");
    assert_eq!(outcome.remaining_held, 2, "both rows still owed");
    for path in ["a.txt", "b.txt"] {
        let entry = engine.db().get_entry(path).unwrap().unwrap();
        assert_eq!(
            entry.state,
            crate::db::SyncState::Synced,
            "{path}: record-first — a failed record never tombstones"
        );
    }
    // Resumable: the derived state is unchanged, so a second apply retries.
    let again = engine.apply_held_deletes().await.expect("second apply");
    assert!(again.floor_was_active, "still held — the verb can resume");
}

/// The resident loop's command arm end-to-end: an `EngineCommand::ApplyHeldDeletes`
/// sent into `run_watch_loop`'s channel reaches the loop's OWN engine and the
/// verdict comes back on the oneshot — the seam the agent's pipe server drives.
///
/// The held set is seeded as **rows over an empty directory**
/// ([`seed_tracked_row`], which owns the full rationale) rather than as files
/// removed just before the loop starts. This is the one test in this file that
/// runs a real watcher, and a watcher is not reliably blind to a removal that
/// predates it: on macOS FSEvents can still deliver one, whereupon the loop's
/// own `handle_delete` — deliberately floor-free, since a streamed per-file
/// removal is genuine per-file evidence — tombstones the rows and leaves the
/// floor nothing to hold. That is a race, not an outcome, so the precondition
/// is established without ever touching the watched directory: the floor's
/// inputs are the `Synced` rows and the scan, and both are exactly what they
/// would be after the vanish.
#[tokio::test]
async fn the_resident_loop_routes_an_apply_command_to_its_engine() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _progress) = crate::pull_remote_changes_test::test_engine_folderless_with_progress(
        dir.path().to_path_buf(),
    );
    for (path, body) in [("a.txt", b"alpha".as_slice()), ("b.txt", b"beta")] {
        seed_tracked_row(&engine, path, body);
    }

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);
    // The loop's future is deliberately not `Send` (its host trait is
    // `?Send`), so it is raced in-task rather than spawned — dropping it at
    // test end is exactly the caller contract (cancellation is the caller's
    // job).
    // `SyncEngine` is `!Sync` (its `SyncDb` wraps a raw `rusqlite::Connection`);
    // paired with the `!Send` note above, this `Arc` never crosses a thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = std::sync::Arc::new(engine);
    let loop_fut = crate::always_resident::run_watch_loop(
        engine,
        dir.path().to_path_buf(),
        "held-set".to_string(),
        // Far beyond the test's life — the command arm, not the tick, must do
        // the work (a tick-driven pass would RECONCILE, which holds).
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    tokio::pin!(loop_fut);

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(crate::always_resident::EngineCommand::ApplyHeldDeletes { reply: reply_tx })
        .await
        .expect("capacity-2 channel buffers the command before the loop runs");
    // Generous ceiling, deadline-style (convention 14): green pays ~nothing;
    // only a loop that lost the command arm ever waits this out.
    let outcome = tokio::select! {
        _ = &mut loop_fut => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx) => got
            .expect("the loop must answer the command well inside the ceiling")
            .expect("reply channel alive")
            .expect("apply succeeds"),
    };
    assert!(outcome.floor_was_active, "the vanished set was held");
    assert_eq!(outcome.applied, 2, "the loop's engine applied its own hold");
}

// ---------------------------------------------------------------------------
// "An unreadable folder is not an empty one" — the engine's half.
//
// The floor asks *"is EVERY synced row missing?"*. That question is only
// meaningful about rows the pass could LOOK at, and before this the scan
// answered `Ok` with a silently empty list for a directory it could not read.
// So a `chmod 000` produced, per blast radius:
//
//   * on a SUBDIRECTORY — a partial vanish, below the floor, propagating a
//     delete per file under it to the nest and to every device of the set;
//   * on the ROOT — a total vanish, which the floor DOES catch, and therefore
//     surfaces as *"your folder emptied — apply N deletions"*: the floor turned
//     the failure into a one-click erase of the whole set.
//
// `watcher::unreadable_scan_tests` pins the scan primitive; these pin what
// reconcile and `apply_held_deletes` do with it.
// ---------------------------------------------------------------------------

/// Put `dir` beyond reach, run `body`, restore the mode whatever happened.
/// Restoring is not hygiene: a mode-0 directory makes the `TempDir` undeletable,
/// so an un-restored one leaks a directory per failing run.
///
/// Asserts its own precondition — root ignores mode bits, and without this check
/// every test below would degrade to "the directory is readable" and pass for
/// the wrong reason.
#[cfg(unix)]
pub(crate) async fn while_unreadable<T, F>(dir: &std::path::Path, body: impl FnOnce() -> F) -> T
where
    F: std::future::Future<Output = T>,
{
    use std::os::unix::fs::PermissionsExt;

    let restore = std::fs::metadata(dir).unwrap().permissions();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let probe = std::fs::read_dir(dir).err().map(|e| e.kind());
    if !matches!(probe, Some(std::io::ErrorKind::PermissionDenied)) {
        std::fs::set_permissions(dir, restore).unwrap();
        panic!(
            "fixture precondition: read_dir on a mode-0 directory must fail with \
             PermissionDenied (are these tests running as root?); got {probe:?}"
        );
    }

    let out = body().await;
    std::fs::set_permissions(dir, restore).unwrap();
    out
}

/// A synced subdirectory becomes unreadable: **no delete is recorded**, the
/// rows keep their `Synced` state, and they come back `unchanged` once the
/// directory reads again — the same restore-losslessly witness the floor's own
/// pin uses, and for the same reason (a tombstoned or state-flipped row could
/// not re-enter as `unchanged`).
///
/// Red before the fix on `deleted_files`: the scan's `PermissionDenied` arm
/// returned `Ok(())`, `sub/c.txt` and `sub/d.txt` were simply absent from the
/// scan, and with `keep.txt` still present the floor saw a *partial* vanish and
/// propagated both.
#[tokio::test]
#[cfg(unix)]
async fn a_row_under_an_unreadable_directory_records_no_delete() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    for (path, body) in [
        ("keep.txt", b"kept".as_slice()),
        ("sub/c.txt", b"gamma"),
        ("sub/d.txt", b"delta"),
    ] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }

    let sub = dir.path().join("sub");
    let stats = while_unreadable(&sub, || engine.reconcile()).await.unwrap();

    assert_eq!(
        stats.deleted_files, 0,
        "a directory that cannot be READ is not a directory whose files were deleted"
    );
    assert_eq!(
        stats.deletes_skipped_unreadable, 2,
        "both hidden rows are REPORTED as withheld — a row appearing in no count at all is \
         exactly how a silent delete-side gap survives"
    );
    assert_eq!(
        stats.deletes_held, 0,
        "this is not a floor hold: the floor means 'we looked and the folder was empty', and \
         nothing here was looked at"
    );
    for path in ["sub/c.txt", "sub/d.txt"] {
        let entry = engine.db().get_entry(path).unwrap().unwrap();
        assert_eq!(
            entry.state,
            crate::db::SyncState::Synced,
            "{path}: untouched, so the next readable pass decides on real evidence"
        );
    }

    // The permissions come back: the files were there all along, so the pass
    // sees them UNCHANGED — proof no row was touched while they were hidden.
    let stats = engine.reconcile().await.expect("reconcile after restore");
    assert_eq!(stats.deletes_skipped_unreadable, 0, "derived — it clears");
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(stats.new_files, 0, "nothing re-enters as new");
    assert_eq!(stats.unchanged_files, 3);
}

/// The sharpest case, and the one the finding's own write-up understated: an
/// unreadable **ROOT**. The floor engages precisely on totality, so a root that
/// scanned as empty made every synced row missing at once — a textbook hold,
/// rendered to the user as *"your folder emptied — apply N deletions"*, one
/// click from erasing the set on the nest and on every device.
///
/// The load-bearing assertion is `deletes_held == 0`: not merely "nothing was
/// recorded" (the floor already gave that) but **nothing was OFFERED**. Before
/// the fix it was 3.
#[tokio::test]
#[cfg(unix)]
async fn an_unreadable_root_is_never_offered_as_a_folder_that_emptied() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) = test_engine_with_progress(dir.path().to_path_buf());
    for (path, body) in [
        ("a.txt", b"alpha".as_slice()),
        ("b.txt", b"beta"),
        ("sub/c.txt", b"gamma"),
    ] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }

    let stats = while_unreadable(dir.path(), || engine.reconcile())
        .await
        .unwrap();

    assert_eq!(
        stats.deletes_held, 0,
        "an unreadable root must NOT read as a wholesale vanish — a hold is an offer to \
         apply N deletions, and there is nothing here for a user to confirm"
    );
    assert_eq!(
        drained_held_counts(&mut progress),
        vec![0],
        "and the surface is told zero, so no 'folder emptied' line can be painted"
    );
    assert_eq!(
        stats.deletes_skipped_unreadable, 3,
        "the fault is reported as what it is: a folder that could not be read"
    );
    assert_eq!(stats.deleted_files, 0);
    for path in ["a.txt", "b.txt", "sub/c.txt"] {
        assert_eq!(
            engine.db().get_entry(path).unwrap().unwrap().state,
            crate::db::SyncState::Synced,
            "{path}: untouched"
        );
    }
}

/// The withholding is reported on the progress channel exactly like the hold
/// is: every pass, zero included. A derived report that is only ever emitted
/// non-zero can never be retracted, so a surface would keep showing a fault the
/// folder recovered from.
#[tokio::test]
#[cfg(unix)]
async fn the_withheld_count_is_reported_every_pass_so_it_can_clear() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, mut progress) = test_engine_with_progress(dir.path().to_path_buf());
    for (path, body) in [("keep.txt", b"kept".as_slice()), ("sub/c.txt", b"gamma")] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }

    let sub = dir.path().join("sub");
    while_unreadable(&sub, || engine.reconcile()).await.unwrap();
    assert_eq!(
        drained_skipped_counts(&mut progress),
        vec![1],
        "reported outward once for the pass, not only into the log"
    );

    engine.reconcile().await.expect("reconcile after restore");
    assert_eq!(
        drained_skipped_counts(&mut progress),
        vec![0],
        "a zero is the only thing that ever retracts it"
    );
}

/// Drain every `DeletesSkippedUnreadable` count reported this pass — the twin
/// of [`drained_held_counts`], and a `Vec` for the same reason: "reported
/// exactly once per pass" is part of the contract.
#[cfg(unix)] // its only caller is unix-gated
fn drained_skipped_counts(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>,
) -> Vec<u64> {
    let mut skipped = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let ProgressEvent::DeletesSkippedUnreadable { skipped: n } = event {
            skipped.push(n);
        }
    }
    skipped
}

/// The second consumer of the same derivation — the **apply** verb. It
/// re-derives through `missing_from_scan` too, so a row hidden under an
/// unreadable directory is not applied even though a user clicked confirm: the
/// click authorizes deleting the files the pass looked at, and nobody looked at
/// these.
///
/// The fixture is the composite case that separates the two rules: `a.txt` and
/// `b.txt` are genuinely gone (the floor holds them — every row the pass could
/// observe is missing), while `sub/c.txt` is merely hidden. The apply must take
/// the first two and leave the third.
#[tokio::test]
#[cfg(unix)]
async fn apply_held_deletes_withholds_rows_under_an_unreadable_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _progress) = crate::pull_remote_changes_test::test_engine_folderless_with_progress(
        dir.path().to_path_buf(),
    );
    for (path, body) in [
        ("a.txt", b"alpha".as_slice()),
        ("b.txt", b"beta"),
        ("sub/c.txt", b"gamma"),
    ] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }
    for path in ["a.txt", "b.txt"] {
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }

    let sub = dir.path().join("sub");
    let outcome = while_unreadable(&sub, || engine.apply_held_deletes())
        .await
        .unwrap();

    assert!(
        outcome.floor_was_active,
        "the rows the pass COULD observe were all missing — that is a genuine hold, and the \
         unreadable prefix must not disarm the floor for the rest of the folder"
    );
    assert_eq!(
        outcome.applied, 2,
        "the two genuinely-missing rows are applied: the fix withholds, it does not freeze"
    );
    assert_eq!(
        engine.db().get_entry("sub/c.txt").unwrap().unwrap().state,
        crate::db::SyncState::Synced,
        "the hidden row is NOT applied — a confirm cannot authorize deleting a file nobody \
         looked at"
    );
}

/// And the case where the *whole* confirm is void: an unreadable root. Before
/// the fix this was the end of the data-loss path — the floor held every row,
/// the app offered the button, and the apply drove all of them through.
#[tokio::test]
#[cfg(unix)]
async fn apply_held_deletes_applies_nothing_under_an_unreadable_root() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _progress) = crate::pull_remote_changes_test::test_engine_folderless_with_progress(
        dir.path().to_path_buf(),
    );
    for (path, body) in [
        ("a.txt", b"alpha".as_slice()),
        ("b.txt", b"beta"),
        ("sub/c.txt", b"gamma"),
    ] {
        seed_tracked_file(&engine, dir.path(), path, body);
    }

    let outcome = while_unreadable(dir.path(), || engine.apply_held_deletes())
        .await
        .unwrap();

    assert!(
        !outcome.floor_was_active,
        "no hold can be active when nothing was observed"
    );
    assert_eq!(
        outcome.applied, 0,
        "every byte the user has is still on disk — the apply must touch nothing"
    );
    for path in ["a.txt", "b.txt", "sub/c.txt"] {
        assert_eq!(
            engine.db().get_entry(path).unwrap().unwrap().state,
            crate::db::SyncState::Synced,
            "{path}: untouched"
        );
    }
}
