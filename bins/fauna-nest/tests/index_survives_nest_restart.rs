//! tier_3: **a nest boot leaves `__index` content intact.**
//!
//! Proof obligation (`docs/goal/behavior/content-index.md` § Before Plan 5b
//! writes its first byte → constraint 1, and § Don't do these → *Don't let the
//! boot-time `__index` purge survive Plan 5b*): from the moment a
//! capability-position builder syncs real sealed segments into the `__index`
//! reserved folder, **no boot path may delete them**. The content-blind boot
//! purge that shredded every `__index` blob on every boot
//! (`db/index_purge.rs`, correct while the only `__index` bytes in existence
//! were the retired nest-side writer's unsealed residue) is deleted as rollout
//! slice S2, in the same landing window as the first real writer (S3).
//!
//! This is the standing regression pin for that removal, and it is deliberately
//! written against the **whole boot**, not against the one function that was
//! deleted: it drives the real shared serve loop
//! (`fauna_nest::desktop_serve::run_serve_loop` — the same construction +
//! `start_server` path the Windows service and the macOS daemon run) over a
//! data dir that already holds `__index` content at rest, then re-opens the
//! nest's own `nest.db` + blob store afterwards. Any *future* boot reconcile
//! that tombstones or sweeps `__index` reddens this test too, which a test
//! calling only `purge_unsealed_index_folders` could never do.
//!
//! Red before S2 (the boot purge tombstones both paths and deletes both blobs),
//! green after.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fauna_core::data::ContentHash;
use fauna_index::{ContentKind, INDEX_FOLDER, mailcal_manifest_path, segment_path};
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::db::CacheDb;
use fauna_nest::desktop_serve::{ServeLoopConfig, run_serve_loop};

/// Stage one `__index` blob exactly as the sync rail records it: bytes into the
/// content-addressed store, a `sync_changes` row carrying the virtual path.
/// Returns the blob hash.
async fn stage_index_blob(
    db: &Arc<CacheDb>,
    store: &Arc<dyn BlobStoreBackend>,
    actor: &[u8; 32],
    fs_id: i64,
    path: &str,
    bytes: &[u8],
) -> [u8; 32] {
    let hash: [u8; 32] = *blake3::hash(bytes).as_bytes();
    store
        .put(&ContentHash::from_digest_raw(hash), bytes)
        .await
        .expect("put blob");
    let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
    db.record_sync_change(
        actor,
        &path_hash,
        Some(&hash),
        bytes.len() as i64,
        "created",
        Some(fs_id),
        Some(&[0x77; 32]),
        Some(path),
    )
    .await
    .expect("record sync change");
    hash
}

/// The latest `change_type` the journal holds for a path — `"deleted"` is what
/// a tombstoning reconcile leaves behind.
async fn latest_change_type(db: &Arc<CacheDb>, fs_id: i64, path: &str) -> String {
    let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
    let conn = db.conn().await;
    conn.query_row(
        "SELECT change_type FROM sync_changes
         WHERE folder_id = ?1 AND path_hash = ?2
         ORDER BY seq DESC LIMIT 1",
        rusqlite::params![fs_id, path_hash.to_vec()],
        |row| row.get::<_, String>(0),
    )
    .expect("a journal row for the staged path")
}

#[tokio::test]
async fn sealed_index_segments_survive_a_nest_restart() {
    // Plain HTTP so the loop needs no TLS floor material — the same escape the
    // sibling serve-loop tests and the tier_3 binary suite use.
    // SAFETY: set once at the top of this single-threaded test before the loop
    // (which reads it) is spawned; nothing else mutates the env concurrently.
    unsafe {
        std::env::set_var("FAUNA_INSECURE_DISABLE_TLS", "1");
    }

    let data_dir = tempfile::tempdir().expect("tempdir");
    let db_path = data_dir.path().join("nest.db");
    let blob_dir = data_dir.path().join("blobs");
    std::fs::create_dir_all(&blob_dir).expect("blob dir");

    // ---- Arrange: `__index` content at rest, as a builder's sync leaves it.
    let actor = [0xAA; 32];
    let seg_path = segment_path(ContentKind::Mail, 0);
    let manifest_path = mailcal_manifest_path();
    let (seg_hash, manifest_hash, fs_id) = {
        let db = Arc::new(CacheDb::open(&db_path).expect("open nest.db"));
        let store: Arc<dyn BlobStoreBackend> =
            Arc::new(DiskBlobStore::new(&blob_dir).expect("blob store"));
        let fs_id = db
            .create_folder(INDEX_FOLDER, &actor)
            .await
            .expect("create __index folder");

        // Sealed bytes: opaque to the nest by construction (that is the whole
        // posture — `content-index.md` § Encryption posture). Their *content*
        // is irrelevant here; what matters is that nothing on the boot path
        // inspects or deletes them.
        let seg = stage_index_blob(
            &db,
            &store,
            &actor,
            fs_id,
            &seg_path,
            b"sealed-segment-bytes",
        )
        .await;
        let man = stage_index_blob(
            &db,
            &store,
            &actor,
            fs_id,
            &manifest_path,
            b"sealed-mailcal-manifest",
        )
        .await;
        (seg, man, fs_id)
    };

    // ---- Act: a real nest boot over that data dir, then a clean shutdown.
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddr>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let cfg = ServeLoopConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        data_dir: data_dir.path().to_path_buf(),
        default_serving_port: 3000,
        // No co-located internal loopback listener (keeps the test off the
        // fixed 127.0.0.1:3000 IPC port).
        internal_loopback_port: None,
    };
    let loop_handle = tokio::spawn(async move {
        run_serve_loop(
            cfg,
            None,
            async move {
                let _ = shutdown_rx.await;
            },
            Some(ready_tx),
        )
        .await
    });

    // The loop reports its bound address once the nest is serving — i.e. once
    // every boot reconcile ahead of `axum::serve` has run.
    tokio::time::timeout(Duration::from_secs(60), ready_rx.recv())
        .await
        .expect("nest booted and bound a listener")
        .expect("ready channel delivered the bound addr");

    shutdown_tx.send(()).expect("send shutdown");
    tokio::time::timeout(Duration::from_secs(30), loop_handle)
        .await
        .expect("serve loop returned after shutdown")
        .expect("serve loop task did not panic")
        .expect("serve loop returned Ok(())");

    // ---- Assert: both the journal rows and the blobs are exactly as staged.
    let db = Arc::new(CacheDb::open(&db_path).expect("re-open nest.db"));
    let store: Arc<dyn BlobStoreBackend> =
        Arc::new(DiskBlobStore::new(&blob_dir).expect("re-open blob store"));

    assert_eq!(
        latest_change_type(&db, fs_id, &seg_path).await,
        "created",
        "the boot tombstoned a live `__index` segment path — a boot path is \
         shredding user index data (content-index.md § Before Plan 5b)"
    );
    assert_eq!(
        latest_change_type(&db, fs_id, &manifest_path).await,
        "created",
        "the boot tombstoned the `__index` mail/calendar manifest path"
    );
    assert!(
        store
            .exists(&ContentHash::from_digest_raw(seg_hash))
            .await
            .expect("probe segment blob"),
        "the boot deleted a sealed `__index` segment blob — the data a builder \
         synced is gone and the user cannot recreate it without a full re-index"
    );
    assert!(
        store
            .exists(&ContentHash::from_digest_raw(manifest_hash))
            .await
            .expect("probe manifest blob"),
        "the boot deleted the sealed `__index` manifest blob"
    );
}
