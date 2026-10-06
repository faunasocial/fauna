//! A `Placeholder` row on a **resident** root is a pending download, never a
//! delete (`docs/goal/behavior/on-demand-files.md` § Linux FUSE binding, the
//! flips rule). An on-demand→always flip — the linux FUSE root unmounting, or a
//! windows cfapi root unregistering — leaves the folder's cloud-only files as
//! `Placeholder` rows with no bytes on the disk, and the on-demand fold already
//! moved the change anchor past them, so the resident pull never re-delivers
//! them. [`SyncEngine::materialize_placeholder_rows`] is the pass that fetches
//! them; the resident watch loop runs it at start and on every rescan tick.

use fauna_core::crypto::BackupKey;
use wiremock::MockServer;

use crate::db::SyncState;
use crate::download_file_bytes_test::build_test_engine;
use crate::engine::SyncEngine;
use crate::test_support::{BlobStore, MockNest};

fn state(engine: &SyncEngine, rel: &str) -> Option<SyncState> {
    engine.db().get_entry(rel).unwrap().map(|e| e.state)
}

/// Upload `body` at `rel`, then leave the row as an on-demand root leaves a
/// cloud-only file: `Placeholder` at the nest's head, never seen on this disk,
/// no bytes here. (This module's nest client is unconnected, so the upload's
/// record never lands; the head is the manifest the byte plane stored.)
async fn cloud_only(
    engine: &SyncEngine,
    store: &BlobStore,
    dir: &std::path::Path,
    rel: &str,
    body: &[u8],
) {
    let full = dir.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, body).unwrap();
    engine.upload_file(rel).await.expect("upload_file");
    std::fs::remove_file(&full).unwrap();
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(store.expect_last_manifest_hash()),
            SyncState::Placeholder,
            0,
            1_700_000_000,
            body.len() as i64,
            1,
            None,
        )
        .unwrap();
    engine.stamp_own_head_for_test(rel);
    // The directory an on-demand root never materialized is not on the disk
    // either.
    if let Some(parent) = std::path::Path::new(rel).parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::remove_dir_all(dir.join(parent.components().next().unwrap()));
    }
}

fn engine_on(server: &MockServer, dir: &std::path::Path) -> SyncEngine {
    build_test_engine(
        &server.uri(),
        dir.to_path_buf(),
        None,
        Some(BackupKey::from_bytes([0x61u8; 32])),
        None,
        None,
    )
}

#[tokio::test]
async fn a_flip_to_always_materializes_every_placeholder_and_decides_no_delete() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(&server, watch.path());

    let top: Vec<u8> = (0..90_000u32).map(|i| (i % 251) as u8).collect();
    cloud_only(&engine, &store, watch.path(), "top.bin", &top).await;
    cloud_only(
        &engine,
        &store,
        watch.path(),
        "deep/er/notes.txt",
        b"nested body",
    )
    .await;
    assert!(!watch.path().join("deep").exists());

    // The resident start's local half first, exactly as the loop runs it: a
    // never-seen `Placeholder` row missing from the scan is evidence of nothing.
    engine.reconcile().await.expect("reconcile");
    assert_eq!(state(&engine, "top.bin"), Some(SyncState::Placeholder));
    assert_eq!(
        state(&engine, "deep/er/notes.txt"),
        Some(SyncState::Placeholder)
    );

    let fetched = engine
        .materialize_placeholder_rows()
        .await
        .expect("materialize");
    assert_eq!(fetched, 2, "both cloud-only rows are pending downloads");
    assert_eq!(std::fs::read(watch.path().join("top.bin")).unwrap(), top);
    assert_eq!(
        std::fs::read(watch.path().join("deep/er/notes.txt")).unwrap(),
        b"nested body"
    );
    assert_eq!(state(&engine, "top.bin"), Some(SyncState::Synced));
    assert_eq!(state(&engine, "deep/er/notes.txt"), Some(SyncState::Synced));
    assert!(
        engine.is_dehydration_safe("top.bin"),
        "a materialized row carries the recorded proof, like any download"
    );

    // Idempotent: nothing is left pending, and a reconcile after it finds the
    // files where the rows say they are.
    assert_eq!(engine.materialize_placeholder_rows().await.unwrap(), 0);
    engine.reconcile().await.expect("reconcile");
    assert_eq!(state(&engine, "top.bin"), Some(SyncState::Synced));
}

/// A dehydrate cut short between its row flip and its unlink leaves a
/// `Placeholder` row over the very bytes it recorded. On a resident root those
/// bytes are the file: the row goes back to `Synced`, no download.
#[tokio::test]
async fn a_placeholder_row_over_its_own_recorded_bytes_is_synced_again() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(&server, watch.path());

    let full = watch.path().join("kept.txt");
    std::fs::write(&full, b"kept bytes").unwrap();
    engine.upload_file("kept.txt").await.expect("upload_file");
    engine
        .db()
        .stamp_recorded_content_from_local("kept.txt", crate::db::ProofOrigin::OwnRecord)
        .unwrap();
    engine
        .db()
        .update_state("kept.txt", SyncState::Placeholder)
        .unwrap();

    assert_eq!(engine.materialize_placeholder_rows().await.unwrap(), 0);
    assert_eq!(state(&engine, "kept.txt"), Some(SyncState::Synced));
    assert_eq!(std::fs::read(&full).unwrap(), b"kept bytes");
}

/// Different bytes under a `Placeholder` row are the user's, never
/// overwritten by the pass: the row is left for reconcile, which reads them
/// as a local edit.
#[tokio::test]
async fn different_bytes_under_a_placeholder_row_are_never_overwritten() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(&server, watch.path());

    cloud_only(&engine, &store, watch.path(), "mine.txt", b"nest bytes").await;
    std::fs::write(watch.path().join("mine.txt"), b"the user's own bytes").unwrap();

    assert_eq!(engine.materialize_placeholder_rows().await.unwrap(), 0);
    assert_eq!(
        std::fs::read(watch.path().join("mine.txt")).unwrap(),
        b"the user's own bytes"
    );
    assert_eq!(state(&engine, "mine.txt"), Some(SyncState::Placeholder));
}

/// An on-demand engine's placeholders are meant to stay rows: the pass is a
/// resident root's, and under the off-disk posture it fetches nothing.
#[tokio::test]
async fn the_pass_fetches_nothing_under_the_off_disk_posture() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_on(&server, watch.path());

    cloud_only(&engine, &store, watch.path(), "cloud.bin", b"cloud bytes").await;
    engine.set_placeholders_off_disk();

    assert_eq!(engine.materialize_placeholder_rows().await.unwrap(), 0);
    assert!(!watch.path().join("cloud.bin").exists());
    assert_eq!(state(&engine, "cloud.bin"), Some(SyncState::Placeholder));
}
