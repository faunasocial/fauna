//! SyncEngine thumbnail **download-backfill** (Seam B): a file whose
//! `sync_changes` row was recorded without a thumbnail (a build without the
//! thumbnailer, or a failed thumbnail upload) carries `thumbnail_hash = None`. Downloading it from a peer
//! hands us the plaintext, so [`SyncEngine::download_and_write_file`] generates
//! the missing thumbnail, seals it under the owner `BackupKey`, uploads it as a
//! standalone blob, and re-records the (live) file with its hash — additively,
//! no data loss, so `fauna.media.list` surfaces it on the next list.
//!
//! Two halves are tested:
//!   1. [`SyncEngine::thumbnail_backfill_targets`] — the pure per-path gate that
//!      picks only paths whose **batch-latest** change is a live create/modify
//!      still lacking a thumbnail (so a same-batch delete can never be
//!      resurrected by the backfill re-record — the NEXT Seam B guard 1).
//!   2. The download-time effect: a real round-trip where downloading a
//!      thumbnail-less image POSTs exactly one backfill thumbnail blob, and one
//!      that already has a thumbnail (or is not an image) POSTs none.
//!
//! Like the sibling `upload_thumbnail_test.rs`, the WS-RPC record path is
//! unconnected (the re-record fails-and-logs), so these assert the byte-plane
//! thumbnail blob POST — the `thumbnail_hash` field carried by `changes_record`
//! is covered by `fauna-client-sync`'s own test.
//!
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), mirroring
//! `download_file_bytes_test.rs`.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::SyncDb;
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// ─────────────────────────────────────────────────────────────────────
// thumbnail_backfill_targets — the pure per-path gate (no I/O)
// ─────────────────────────────────────────────────────────────────────

/// A `SyncChange` fixture: `path`, `seq`, whether it is live (a manifest) or a
/// delete, and whether it already carries a thumbnail.
fn change(
    path: &str,
    seq: i64,
    change_type: &str,
    has_manifest: bool,
    has_thumb: bool,
) -> SyncChange {
    SyncChange {
        seq,
        path_hash: "00".repeat(32),
        manifest_hash: has_manifest.then(|| "ab".repeat(32)),
        size_bytes: 4096,
        change_type: change_type.to_string(),
        created_at: seq,
        path: Some(path.to_string()),
        device_id: Some("11".repeat(32)),
        content_key_version: None,
        thumbnail_hash: has_thumb.then(|| "cd".repeat(32)),
        ..Default::default()
    }
}

#[test]
fn backfill_targets_includes_a_live_change_without_a_thumbnail() {
    let changes = vec![change("photo.png", 10, "create", true, false)];
    let targets = SyncEngine::thumbnail_backfill_targets(&changes);
    let t = targets
        .get("photo.png")
        .expect("a live no-thumbnail change is a target");
    assert_eq!(t.seq, 10);
    assert_eq!(t.size_bytes, 4096);
}

#[test]
fn backfill_targets_excludes_a_change_that_already_has_a_thumbnail() {
    let changes = vec![change("photo.png", 10, "create", true, true)];
    assert!(
        SyncEngine::thumbnail_backfill_targets(&changes).is_empty(),
        "a change that already carries a thumbnail is not a backfill target"
    );
}

#[test]
fn backfill_targets_excludes_a_path_whose_batch_latest_is_a_delete() {
    // A catching-up device (pull-since-0) pulls a file that was created then
    // deleted before it caught up. The batch-latest state is the delete, so the
    // backfill must NOT re-record the create (which would resurrect the file).
    let changes = vec![
        change("gone.png", 10, "create", true, false),
        change("gone.png", 15, "delete", false, false),
    ];
    assert!(
        SyncEngine::thumbnail_backfill_targets(&changes).is_empty(),
        "a path whose latest change is a delete must not be a backfill target"
    );
}

#[test]
fn backfill_targets_picks_the_latest_of_several_live_changes() {
    let changes = vec![
        change("photo.png", 10, "create", true, false),
        change("photo.png", 20, "modify", true, false),
    ];
    let t = SyncEngine::thumbnail_backfill_targets(&changes)
        .remove("photo.png")
        .expect("a live path is a target");
    assert_eq!(
        t.seq, 20,
        "the backfill re-records against the batch-latest change"
    );
}

#[test]
fn backfill_targets_excludes_a_delete_only_history() {
    let changes = vec![change("gone.png", 15, "delete", false, false)];
    assert!(SyncEngine::thumbnail_backfill_targets(&changes).is_empty());
}

// ─────────────────────────────────────────────────────────────────────
// Download-time effect — a real chunk/manifest round-trip
// ─────────────────────────────────────────────────────────────────────

fn test_sync_client(server_uri: &str) -> SyncClient {
    let kp = ActorKeypair::generate();
    let http = reqwest::Client::new();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        kp,
        bearer,
        http,
    ));
    let device_id = [0u8; 32];
    SyncClient::new(auth, &device_id)
}

pub(crate) fn test_engine(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    backup_key: Option<BackupKey>,
) -> SyncEngine {
    let db = SyncDb::open_in_memory().unwrap();
    let client = test_sync_client(server_uri);
    let device_id = [0u8; 32];
    let format_registry = FormatRegistry::new();
    let ignore = IgnoreMatcher::default();
    let concurrency = Arc::new(AdaptiveConcurrency::fixed(4));
    let transfer_pool = TransferPool::new(concurrency, None);
    // Unconnected WS-RPC client (mirrors `upload_thumbnail_test`): the backfill
    // re-record over it fails-and-logs (a 30s wait-for-connected deadline), so
    // these tests assert the byte-plane thumbnail blob POST — the control-plane
    // record carrying `thumbnail_hash` is covered by `fauna-client-sync`.
    let nest_client =
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate());

    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()),
        device_id,
        None, // mls
        None, // epoch_secret
        backup_key.map(Into::into),
        None, // mls_group_id
        None, // content_keys
        fauna_core::format::ConflictPolicy::default(),
        format_registry,
        ignore,
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

// A real PNG larger than the 300×300 thumbnail threshold.
use fauna_media::test_fixtures::build_png;

/// Upload `bytes` as `rel` through the real pipeline (populating the mock store)
/// and return its manifest hash + on-nest size. The upload's own Seam-A
/// thumbnail POST (if any) is captured by the counter; callers snapshot the
/// counter *after* this to isolate the later backfill POST.
pub(crate) async fn seed_file(
    engine: &SyncEngine,
    store: &crate::test_support::BlobStore,
    watch: &std::path::Path,
    rel: &str,
    bytes: &[u8],
) -> ContentHash {
    let full = watch.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, bytes).unwrap();
    engine.upload_file(rel).await.expect("seed upload");
    store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("manifest posted")
}

/// The dehydration gate's proven-head half, download-apply side: a file written
/// by `download_and_write_file` (the fold applying an incoming change / initial
/// hydration) is provably the nest's content, so freeing its bytes is lossless
/// and `is_dehydration_safe` must allow it. The apply path must stamp
/// `recorded_content_hash`; before that wiring the row's is NULL and the gate
/// fails closed. (Mirror of the record-success / hydrate-on-open proofs in
/// `pull_remote_changes_test.rs`.)
#[tokio::test]
async fn a_downloaded_file_is_dehydration_safe() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);

    // Uploader populates the store with a plain file's chunks + manifest.
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let body: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let manifest_hash = seed_file(&uploader, &store, up_watch.path(), "data.bin", &body).await;

    // A peer applies the incoming change locally (backfill = None: no thumbnail).
    let dl_watch = tempfile::tempdir().unwrap();
    let downloader = test_engine(&server.uri(), dl_watch.path().to_path_buf(), Some(key));
    downloader
        .download_and_write_file(
            "data.bin",
            manifest_hash,
            None,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            // leg-4 ruling: no listing context in this fixture — conservative claim
            false,
        )
        .await
        .expect("download succeeds");

    assert!(
        downloader.is_dehydration_safe("data.bin"),
        "a downloaded file is provably the nest's content — freeing it must be \
         allowed; download_and_write_file must stamp recorded_content_hash"
    );
}

#[tokio::test]
async fn download_backfills_a_missing_thumbnail_for_an_image_recorded_without_one() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);

    // Uploader populates the store with an owner-encrypted image's chunks +
    // manifest (Seam A also POSTs a thumbnail here — snapshot after).
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let png = build_png(800, 600);
    let manifest_hash = seed_file(&uploader, &store, up_watch.path(), "photo.png", &png).await;
    let size = std::fs::metadata(up_watch.path().join("photo.png"))
        .unwrap()
        .len() as i64;
    let baseline = store.blob_posts();

    // A peer device downloads the thumbnail-less change (thumbnail_hash = None ⇒
    // backfill = Some(size)); it must POST exactly one backfill thumbnail blob.
    let dl_watch = tempfile::tempdir().unwrap();
    let downloader = test_engine(&server.uri(), dl_watch.path().to_path_buf(), Some(key));
    downloader
        .download_and_write_file(
            "photo.png",
            manifest_hash,
            None,
            Some("11".repeat(32)),
            0,
            Some(size),
            None,
            None,
            None,
            None,
            // leg-4 ruling: no listing context in this fixture — conservative claim
            false,
        )
        .await
        .expect("download succeeds");

    assert_eq!(
        store.blob_posts() - baseline,
        1,
        "downloading a thumbnail-less image must POST exactly one backfill thumbnail blob"
    );
}

#[tokio::test]
async fn download_does_not_backfill_when_the_change_already_has_a_thumbnail() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);

    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let png = build_png(800, 600);
    let manifest_hash = seed_file(&uploader, &store, up_watch.path(), "photo.png", &png).await;
    let baseline = store.blob_posts();

    // backfill = None (the change already carries a thumbnail): no extra POST.
    let dl_watch = tempfile::tempdir().unwrap();
    let downloader = test_engine(&server.uri(), dl_watch.path().to_path_buf(), Some(key));
    downloader
        .download_and_write_file(
            "photo.png",
            manifest_hash,
            None,
            Some("11".repeat(32)),
            0,
            None,
            None,
            None,
            None,
            None,
            // leg-4 ruling: no listing context in this fixture — conservative claim
            false,
        )
        .await
        .expect("download succeeds");

    assert_eq!(
        store.blob_posts(),
        baseline,
        "a change that already has a thumbnail triggers no backfill POST"
    );
}

#[tokio::test]
async fn download_does_not_backfill_a_non_image_even_when_eligible() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);

    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let manifest_hash = seed_file(
        &uploader,
        &store,
        up_watch.path(),
        "notes.txt",
        b"plainly not an image",
    )
    .await;
    let size = std::fs::metadata(up_watch.path().join("notes.txt"))
        .unwrap()
        .len() as i64;
    let baseline = store.blob_posts();

    // Eligible (backfill = Some) but not an image ⇒ maybe_upload_thumbnail
    // declines, so no blob POST.
    let dl_watch = tempfile::tempdir().unwrap();
    let downloader = test_engine(&server.uri(), dl_watch.path().to_path_buf(), Some(key));
    downloader
        .download_and_write_file(
            "notes.txt",
            manifest_hash,
            None,
            Some("11".repeat(32)),
            0,
            Some(size),
            None,
            None,
            None,
            None,
            // leg-4 ruling: no listing context in this fixture — conservative claim
            false,
        )
        .await
        .expect("download succeeds");

    assert_eq!(
        store.blob_posts(),
        baseline,
        "a non-image file yields no backfill thumbnail even when eligible"
    );
}

/// The nest-pull door refuses a remotely-authored
/// row whose path traverses a **symlinked intermediate directory** out of the
/// watch dir. The lexical `is_safe_relative_path` accepts `linkdir/escape.txt`
/// (no `..`); only resolving the path against the filesystem catches that
/// `linkdir` is a symlink pointing outside the sync root. Before the
/// `resolved_target_within_root` fix the write landed in the symlink's target;
/// the door must now skip the row (refuse) and write nothing outside the root.
///
/// Unix-only: creating the symlink needs `std::os::unix`, and on Windows an
/// unprivileged symlink isn't a given (same reason as
/// `watcher::tests::event_rel_path_resolves_a_symlinked_root`).
#[cfg(unix)]
#[tokio::test]
async fn download_refuses_a_symlinked_intermediate_that_escapes_the_watch_dir() {
    use std::os::unix::fs::symlink;

    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x22; 32]);

    // Seed the payload under an ordinary path (content-addressed, so the path
    // the downloader later requests is irrelevant to the fetch).
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = test_engine(
        &server.uri(),
        up_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let body = b"attacker-chosen bytes at an attacker-chosen path".to_vec();
    let manifest_hash = seed_file(&uploader, &store, up_watch.path(), "seed.bin", &body).await;

    // The victim's tree: a legitimate user-created symlink `linkdir` pointing
    // OUTSIDE the sync root (the precondition — a `~/Sync/notes ->
    // ~/Documents/notes` shape). The attacker knows its name and sends a row
    // at `linkdir/escape.txt`.
    let dl_watch = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), dl_watch.path().join("linkdir")).expect("plant the escaping symlink");

    let downloader = test_engine(&server.uri(), dl_watch.path().to_path_buf(), Some(key));
    let result = downloader
        .download_and_write_file(
            "linkdir/escape.txt",
            manifest_hash,
            None,
            None,
            0,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .await;

    assert!(
        result.is_err(),
        "the door must refuse a path that escapes the watch dir through a \
         symlinked intermediate directory"
    );
    assert!(
        !outside.path().join("escape.txt").exists(),
        "no bytes may land outside the sync root"
    );
}
