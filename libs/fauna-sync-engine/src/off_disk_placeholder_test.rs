//! The off-disk placeholder posture — the engine's half of the linux FUSE root's
//! dehydrate rule (`docs/goal/behavior/on-demand-files.md` § Linux FUSE binding:
//! *Dehydrate ("free up space") removes the underlying bytes — and that removal
//! is NEVER a delete*).
//!
//! On that root a placeholder is a row, never a file on the disk, so the cfapi
//! evidence rule (a **seen** placeholder missing from a scan is a delete —
//! `offline_placeholder_delete_test.rs`) must not apply: no `Placeholder` row is
//! evidence, the watcher rail drops a `Placeholder` row's remove, the engine
//! never writes the seen mark, and the dehydrate flips the row BEFORE the caller
//! unlinks — a crash in between is repaired toward the bytes.
//!
//! The agent's half (the FUSE invalidator, the loop's verbs, the live mount) is
//! pinned in `fauna-sync-agent` (`bridge.rs` tests + `fuse_live_integration.rs`).

use fauna_core::data::ContentHash;

use crate::always_resident::LocalWriteHost;
use crate::db::SyncState;
use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::{seed_tracked_file, test_engine_folderless_with_progress};

/// A folderless engine under the off-disk posture: its `handle_delete` has
/// nothing to record and tombstones at once, so any delete the engine wrongly
/// decides shows as a `Deleted` row.
fn off_disk(dir: &std::path::Path) -> SyncEngine {
    let engine = test_engine_folderless_with_progress(dir.to_path_buf()).0;
    engine.set_placeholders_off_disk();
    engine
}

/// A folded cloud-only row, as the fold leaves it.
fn seed_placeholder(engine: &SyncEngine, rel: &str) {
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(ContentHash::of_raw(rel.as_bytes())),
            SyncState::Placeholder,
            0,
            1_700_000_000,
            5,
            1,
            None,
        )
        .unwrap();
}

/// A hydrated file whose recorded head provably reassembles to its bytes — the
/// state after a hydrate-on-open or a recorded upload, and the only one the
/// dehydrate gate lets go.
fn seed_recorded_file(engine: &SyncEngine, dir: &std::path::Path, rel: &str, body: &[u8]) {
    seed_tracked_file(engine, dir, rel, body);
    engine
        .db()
        .stamp_recorded_content_from_local(rel, crate::db::ProofOrigin::Fetched)
        .unwrap();
}

fn state(engine: &SyncEngine, rel: &str) -> Option<SyncState> {
    engine.db().get_entry(rel).unwrap().map(|e| e.state)
}

fn seen(engine: &SyncEngine, rel: &str) -> bool {
    engine.db().get_entry(rel).unwrap().unwrap().seen_on_disk
}

/// **No `Placeholder` row is evidence under the posture, marked or not.** A mark
/// that slipped in (an inherited state DB, a stray writer) would make the cfapi
/// rule record this file as deleted — `offline_placeholder_delete_test`'s
/// `a_seen_placeholder_absent_from_the_scan_is_deleted` is that rule. Here the
/// placeholder was never on the disk, so its absence is evidence of nothing.
#[tokio::test]
async fn a_marked_placeholder_absent_from_the_scan_is_no_delete_off_disk() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_placeholder(&engine, "cloud.txt");
    engine.db().mark_seen(&["cloud.txt"]).unwrap();
    // A hydrated sibling keeps the mass-delete floor out of the picture.
    seed_recorded_file(&engine, dir.path(), "local.txt", b"local bytes");

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(
        stats.deleted_files, 0,
        "a row-only placeholder is never a delete"
    );
    assert_eq!(stats.deletes_held, 0);
    assert_eq!(state(&engine, "cloud.txt"), Some(SyncState::Placeholder));
}

/// **The watcher rail drops a `Placeholder` row's remove** (obligation 4): under
/// the posture that remove is the provider's own unlink — a dehydrate whose
/// suppression token was spent. The inherent `handle_delete` is untouched: it is
/// what the mount's `unlink` of a placeholder calls, the ONE way a placeholder
/// is deleted there.
#[tokio::test]
async fn the_watcher_rail_drops_a_placeholder_remove_but_the_unlink_still_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_placeholder(&engine, "a.txt");

    LocalWriteHost::handle_delete(&engine, "a.txt")
        .await
        .unwrap();
    assert_eq!(
        state(&engine, "a.txt"),
        Some(SyncState::Placeholder),
        "the watcher's remove of an off-disk placeholder is dropped"
    );

    SyncEngine::handle_delete(&engine, "a.txt").await.unwrap();
    assert_eq!(
        state(&engine, "a.txt"),
        Some(SyncState::Deleted),
        "the user's unlink through the mount deletes it"
    );
}

/// Without the posture the watcher rail is the cfapi one: a `Placeholder` row's
/// remove is the user deleting a placeholder on the disk.
#[tokio::test]
async fn without_the_posture_the_watcher_rail_deletes_a_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine_folderless_with_progress(dir.path().to_path_buf()).0;
    seed_placeholder(&engine, "a.txt");

    LocalWriteHost::handle_delete(&engine, "a.txt")
        .await
        .unwrap();
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Deleted));
}

/// **The off-disk dehydrate's row half**: lossless → the row flips to
/// `Placeholder` unseen, the removal suppression is armed, and the file is still
/// on the disk — the caller unlinks second. The manifest anchor and the recorded
/// identity stay, so the next open fetches the same content.
#[tokio::test]
async fn dehydrate_off_disk_flips_the_row_unseen_and_arms_the_suppression() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_recorded_file(&engine, dir.path(), "a.txt", b"recorded bytes");
    let before = engine.db().get_entry("a.txt").unwrap().unwrap();

    assert!(engine.dehydrate_off_disk("a.txt").unwrap());

    let after = engine.db().get_entry("a.txt").unwrap().unwrap();
    assert_eq!(after.state, SyncState::Placeholder);
    assert!(!after.seen_on_disk, "the root never writes the seen mark");
    assert_eq!(
        after.manifest_hash, before.manifest_hash,
        "the anchor stays"
    );
    assert_eq!(after.recorded_content_hash, before.recorded_content_hash);
    assert!(
        engine.was_recent_removal("a.txt"),
        "the unlink's echo is suppressed"
    );
    assert!(
        dir.path().join("a.txt").exists(),
        "the row goes first; the caller unlinks"
    );
}

/// The gate is fail-closed: a file whose content the engine has not recorded
/// (an upload whose record never landed — no `recorded_content_hash`) is not
/// freed, and nothing is written.
#[tokio::test]
async fn dehydrate_off_disk_refuses_an_unrecorded_file() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_tracked_file(&engine, dir.path(), "a.txt", b"never recorded");

    assert!(!engine.dehydrate_off_disk("a.txt").unwrap());
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Synced));
    assert!(!engine.was_recent_removal("a.txt"));
}

/// Under the posture `mark_placeholder` — the cfapi in-place free's record —
/// leaves the row unseen: nothing of the file is on this disk.
#[tokio::test]
async fn mark_placeholder_writes_no_seen_mark_off_disk() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_recorded_file(&engine, dir.path(), "a.txt", b"bytes");

    engine.mark_placeholder("a.txt").unwrap();
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Placeholder));
    assert!(!seen(&engine, "a.txt"));
}

/// **Crash point 1 — row flipped, unlink not done.** The next sweep finds a
/// `Placeholder` row over bytes that are the recorded head, and repairs it back
/// to `Synced`: the bytes are kept, nothing is uploaded or deleted, and the user
/// may free it again.
#[tokio::test]
async fn a_dehydrate_cut_short_before_the_unlink_is_repaired_to_synced() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_recorded_file(&engine, dir.path(), "a.txt", b"recorded bytes");
    assert!(engine.dehydrate_off_disk("a.txt").unwrap());
    // ...and the agent died here, before the unlink.

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::Synced));
    assert_eq!(stats.deleted_files, 0);
    assert_eq!(
        stats.modified_files, 0,
        "the recorded head is not a local edit"
    );
    assert!(
        engine.is_dehydration_safe("a.txt"),
        "the repaired row can be freed again"
    );
}

/// The same crash with the bytes changed since (an edit landing between the flip
/// and the crash, or over the leftover file): an ordinary local edit, uploaded
/// like any other — never a placeholder row claiming the bytes are elsewhere.
#[tokio::test]
async fn a_placeholder_row_over_other_bytes_is_a_local_edit() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_recorded_file(&engine, dir.path(), "a.txt", b"recorded bytes");
    assert!(engine.dehydrate_off_disk("a.txt").unwrap());
    std::fs::write(dir.path().join("a.txt"), b"an edit nobody recorded").unwrap();

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(state(&engine, "a.txt"), Some(SyncState::LocallyModified));
    assert_eq!(stats.modified_files, 1);
    assert_eq!(stats.deleted_files, 0);
}

/// **Crash point 2 — why the reverse order is never taken.** Bytes gone under a
/// row still `Synced` is a user's delete to the sweep; that is exactly what
/// `dehydrate_off_disk` running first (and `free_local_bytes` unlinking only
/// after it answers) makes unreachable. The completed dehydrate — row flipped,
/// then unlinked — is no delete.
#[tokio::test]
async fn only_the_row_first_order_survives_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_recorded_file(&engine, dir.path(), "freed.txt", b"freed properly");
    seed_recorded_file(&engine, dir.path(), "reversed.txt", b"unlinked first");
    seed_recorded_file(&engine, dir.path(), "kept.txt", b"keeps the floor quiet");
    seed_recorded_file(&engine, dir.path(), "kept2.txt", b"and so does this");

    assert!(engine.dehydrate_off_disk("freed.txt").unwrap());
    std::fs::remove_file(dir.path().join("freed.txt")).unwrap();
    std::fs::remove_file(dir.path().join("reversed.txt")).unwrap();

    engine.reconcile().await.unwrap();
    assert_eq!(state(&engine, "freed.txt"), Some(SyncState::Placeholder));
    assert!(!seen(&engine, "freed.txt"));
    assert_eq!(
        state(&engine, "reversed.txt"),
        Some(SyncState::Deleted),
        "the unlink-first order would have been a delete"
    );
}

/// The pin column has a setter and a reader: a pin survives later state writes
/// (`upsert_entry` omits it), and the pinned placeholders are listed for the
/// eager hydration.
#[tokio::test]
async fn a_pin_lives_in_the_row() {
    let dir = tempfile::tempdir().unwrap();
    let engine = off_disk(dir.path());
    seed_placeholder(&engine, "b.txt");
    seed_placeholder(&engine, "a.txt");
    seed_recorded_file(&engine, dir.path(), "local.txt", b"bytes");

    assert!(engine.db().set_pinned("a.txt", true).unwrap());
    assert!(engine.db().set_pinned("local.txt", true).unwrap());
    assert!(
        !engine.db().set_pinned("nope.txt", true).unwrap(),
        "no row, no pin"
    );
    assert_eq!(
        engine.db().pinned_placeholder_paths().unwrap(),
        vec!["a.txt".to_string()],
        "only a pinned file whose bytes are elsewhere is owed a hydration"
    );

    seed_placeholder(&engine, "a.txt"); // a later fold rewrites the row
    assert!(engine.db().get_entry("a.txt").unwrap().unwrap().pinned);
    assert!(engine.db().set_pinned("a.txt", false).unwrap());
    assert!(engine.db().pinned_placeholder_paths().unwrap().is_empty());
}
