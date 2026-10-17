//! A `Placeholder` row over the bytes it names: the stale cache a restore leaves when
//! another program holds the file and refuses the restore's free
//! (`docs/goal/behavior/file-versions.md` § Restore, ordering 2). Those bytes are the version
//! the restore replaced, already recorded, and the row's `local_hash` names them. So the
//! uploader and reconcile read them as no edit, and the row keeps describing the restored
//! version, which is what the free that later lets them go reads to complete the restore. A
//! real edit of the cache is other bytes, and it uploads (`on-demand-files.md` § *Sync
//! direction*).
//!
//! The agent's half (the restore keeping the identity, the live free through Explorer's
//! unpin) is pinned in `fauna-sync-agent` (`pipe_server.rs` tests and
//! `cfapi_live_integration.rs`).

use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::data::ContentHash;

use crate::db::SyncState;
use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::test_engine_with_keys;

const REL: &str = "restored.txt";
/// The version the restore replaced, still on the disk.
const REPLACED: &[u8] = b"the version the restore replaced, still on the disk\n";
/// The restored version's size: the row's, not the disk's.
const RESTORED_SIZE: i64 = 12;

/// An engine that holds its owner key, so an upload it does attempt gets as far as its
/// `Uploading` write before the unreachable nest fails it.
fn engine(dir: &std::path::Path) -> SyncEngine {
    test_engine_with_keys(
        dir.to_path_buf(),
        Some(OwnerSealKey::Client(BackupKey::from_bytes([11u8; 32]))),
        None,
        None,
    )
}

/// The row a restore whose free was refused leaves: `Placeholder` at the restored head and
/// its size, naming the bytes of the version it replaced, which are still on the disk.
fn stale_cache(dir: &std::path::Path) -> SyncEngine {
    let engine = engine(dir);
    std::fs::write(dir.join(REL), REPLACED).unwrap();
    engine
        .db()
        .upsert_entry(
            REL,
            Some(ContentHash::of_raw(REPLACED)),
            None,
            Some(ContentHash::from_digest_raw([0x6c; 32])),
            SyncState::Placeholder,
            0,
            1_700_000_000,
            RESTORED_SIZE,
            1,
            None,
        )
        .unwrap();
    engine
}

fn row(engine: &SyncEngine) -> crate::db::SyncEntry {
    engine.db().get_entry(REL).unwrap().unwrap()
}

/// **The uploader takes the stale cache for no edit.** A watcher event on the file (Explorer's
/// *Free up space* unpin is one) reaches `upload_file`. Until 2026-10-10 that sealed and
/// uploaded the replaced version and recorded it over the restore, on the nest and on every
/// device.
#[tokio::test]
async fn the_uploader_takes_a_stale_cache_for_no_edit() {
    let dir = tempfile::tempdir().unwrap();
    let engine = stale_cache(dir.path());

    let outcome = engine
        .upload_file(REL)
        .await
        .expect("a stale cache is declined, not attempted: the nest is unreachable here");
    assert!(!outcome.recorded, "nothing was recorded");

    let row = row(&engine);
    assert_eq!(
        (row.state, row.size_bytes, row.local_hash),
        (
            SyncState::Placeholder,
            RESTORED_SIZE,
            Some(ContentHash::of_raw(REPLACED))
        ),
        "the row still names the restored version over the stale cache"
    );
}

/// **Reconcile takes the stale cache for no edit, and leaves the row's size alone.** The size
/// is the restored version's. A row rewritten to the stale cache's size agrees with the
/// disk, so the free that later lets the cache go would keep the replaced version's size and
/// every open of the file would be refused.
#[tokio::test]
async fn reconcile_takes_a_stale_cache_for_no_edit_and_keeps_the_restored_size() {
    let dir = tempfile::tempdir().unwrap();
    let engine = stale_cache(dir.path());

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.modified_files, 0, "the stale cache is no local edit");

    let row = row(&engine);
    assert_eq!(
        (row.state, row.size_bytes),
        (SyncState::Placeholder, RESTORED_SIZE),
        "the row still describes the restored version"
    );
}

/// An edit of the stale cache: bytes the row does not name.
const EDITED: &[u8] = b"the user typed this over the stale copy\n";

/// **A real edit of the stale cache uploads from the watcher's event** (`on-demand-files.md`
/// § *Sync direction*: a change to a tracked file must be uploaded) — a conflict with the
/// restore, as ordering 2 says. Its bytes are not the ones the row names.
#[tokio::test]
async fn the_uploader_takes_an_edit_of_a_stale_cache() {
    let dir = tempfile::tempdir().unwrap();
    let engine = stale_cache(dir.path());
    std::fs::write(dir.path().join(REL), EDITED).unwrap();

    // The nest is unreachable, so the upload fails after its `Uploading` write: the write
    // naming the edit's bytes is the proof the uploader took it.
    let _ = engine.upload_file(REL).await;
    let row = row(&engine);
    assert_ne!(
        row.state,
        SyncState::Placeholder,
        "the uploader took the edit"
    );
    assert_eq!(row.local_hash, Some(ContentHash::of_raw(EDITED)));
}

/// **…and from the converge**: reconcile reads the edit as one.
#[tokio::test]
async fn reconcile_takes_an_edit_of_a_stale_cache_for_an_edit() {
    let dir = tempfile::tempdir().unwrap();
    let engine = stale_cache(dir.path());
    std::fs::write(dir.path().join(REL), EDITED).unwrap();

    let stats = engine.reconcile().await.unwrap();
    assert_eq!(stats.modified_files, 1, "an edit is an edit");
    assert_eq!(row(&engine).state, SyncState::LocallyModified);
}
