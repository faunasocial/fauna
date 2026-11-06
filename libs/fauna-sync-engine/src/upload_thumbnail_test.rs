//! SyncEngine thumbnail producer: [`SyncEngine::upload_file`] renders, seals,
//! and uploads a thumbnail for a synced image, so `fauna.media.list` can surface
//! it.
//!
//! A thumbnail rests as its file rests (`media.md` § Encryption at rest → *A
//! thumbnail rests as its file rests*): a >300px image yields a JPEG thumbnail
//! (`fauna_media::pipeline::render_thumbnail_only`), sealed through the file's
//! own pipeline under the file's root, posted as chunks behind a manifest —
//! never a `/api/v1/blob` blob — and that manifest's hash recorded on the
//! change. These tests assert the second manifest lands and opens under the
//! file's root for a real image, and does NOT for a small image / non-image /
//! keyless set.
//!
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), mirroring
//! `download_file_bytes_test.rs`.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::SyncDb;
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// ─────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────

/// A `SyncClient` whose `AuthClient` points at `server_uri` and always sends a
/// fixed bearer (no `/auth/token` round trip).
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

/// A `SyncEngine` rooted at `watch_dir`, folder `__test`. `backup_key = Some`
/// seals chunks + the thumbnail under the owner's root; `None` is a keyless
/// owner-only engine, which fails closed.
fn test_engine(
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
    // Unconnected WS-RPC client: `record_change` over it fails-and-logs, which
    // is fine — these tests assert the byte-plane thumbnail blob POST, not the
    // control-plane record (covered by `fauna-client-sync`'s `changes_record`
    // test that the `thumbnail_hash` field is carried).
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

// A real PNG larger than the 300×300 thumbnail threshold, so `process_media`
// renders a JPEG thumbnail.
use fauna_media::test_fixtures::build_png;

/// The owner key every keyed engine here seals under.
const KEY: [u8; 32] = [0x11; 32];

/// How many manifests the byte plane has stored.
fn manifests(nest: &crate::test_support::BlobStore) -> usize {
    nest.manifests.lock().unwrap().len()
}

/// Open the LAST posted manifest — the thumbnail's, posted after its file's —
/// through the shared walk under the owner root, and check it is a JPEG.
async fn assert_last_manifest_is_an_owner_sealed_jpeg(
    engine: &SyncEngine,
    nest: &crate::test_support::BlobStore,
) {
    let thumb = nest.expect_last_manifest_hash();
    let owner = fauna_core::file_download::FileDownloadKeys::owner(BackupKey::from_bytes(KEY));
    let opened = fauna_core::file_download::download_file_bytes_by_manifest(
        &engine.blob_fetcher(),
        &owner,
        thumb,
        None,
        "photo.png",
    )
    .await
    .expect("the file's own root opens the thumbnail manifest");
    assert_eq!(&opened[..2], &[0xFF, 0xD8], "a JPEG thumbnail");
    fauna_core::file_download::download_file_bytes_by_manifest(
        &engine.blob_fetcher(),
        &fauna_core::file_download::FileDownloadKeys::default(),
        thumb,
        None,
        "photo.png",
    )
    .await
    .expect_err("it rests sealed, never in the clear");
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn upload_file_posts_thumbnail_for_large_image() {
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("photo.png"), build_png(800, 600)).unwrap();

    let engine = test_engine(
        &server.uri(),
        dir.path().to_path_buf(),
        Some(BackupKey::from_bytes(KEY)),
    );
    engine
        .upload_file("photo.png")
        .await
        .expect("upload_file succeeds");

    assert_eq!(
        nest.blob_posts(),
        0,
        "a thumbnail is never a blob-store blob"
    );
    assert_eq!(
        manifests(&nest),
        2,
        "the file's manifest, then the thumbnail's"
    );
    assert_last_manifest_is_an_owner_sealed_jpeg(&engine, &nest).await;
}

#[tokio::test]
async fn upload_file_no_thumbnail_for_small_image() {
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("tiny.png"), build_png(100, 100)).unwrap();

    let engine = test_engine(
        &server.uri(),
        dir.path().to_path_buf(),
        Some(BackupKey::from_bytes(KEY)),
    );
    engine.upload_file("tiny.png").await.expect("upload_file");

    assert_eq!(nest.blob_posts(), 0);
    assert_eq!(
        manifests(&nest),
        1,
        "a ≤300px image yields no thumbnail, so only the file's manifest"
    );
}

#[tokio::test]
async fn upload_file_no_thumbnail_for_non_image() {
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"plainly not an image").unwrap();

    let engine = test_engine(
        &server.uri(),
        dir.path().to_path_buf(),
        Some(BackupKey::from_bytes(KEY)),
    );
    engine.upload_file("notes.txt").await.expect("upload_file");

    assert_eq!(nest.blob_posts(), 0);
    assert_eq!(manifests(&nest), 1, "a non-image file yields no thumbnail");
}

#[tokio::test]
async fn upload_file_keyless_owner_only_fails_closed_and_posts_nothing() {
    // A keyless owner-only folder engine no longer uploads plaintext AT ALL:
    // the upload fails closed, and in particular no (unsealable) thumbnail is
    // posted either.
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("photo.png"), build_png(800, 600)).unwrap();

    let engine = test_engine(&server.uri(), dir.path().to_path_buf(), None);
    engine
        .upload_file("photo.png")
        .await
        .expect_err("keyless owner-only upload must fail closed, not upload plaintext");

    assert_eq!(nest.blob_posts(), 0);
    assert_eq!(
        manifests(&nest),
        0,
        "no owner BackupKey ⇒ nothing posted (no thumbnail either)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Streaming path (>= STREAMING_THRESHOLD): the thumbnail is generated by
// reading the file incrementally from disk (`render_thumbnail_only_from_path`),
// so the O(MAX_CHUNK) streaming upload never loads the whole file. We drive
// `upload_file_streaming` directly with a small real image (its real size), so
// the test exercises the streaming code path without materializing a 64 MiB
// fixture — the size gate itself (`upload_file` → `upload_file_streaming`) is a
// trivial threshold compare covered elsewhere.
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn upload_file_streaming_posts_thumbnail_for_large_image() {
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let full_path = dir.path().join("photo.png");
    std::fs::write(&full_path, build_png(800, 600)).unwrap();
    let file_size = std::fs::metadata(&full_path).unwrap().len();

    let engine = test_engine(
        &server.uri(),
        dir.path().to_path_buf(),
        Some(BackupKey::from_bytes(KEY)),
    );
    engine
        .upload_file_streaming_inner("photo.png", &full_path, file_size, false)
        .await
        .expect("streaming upload succeeds");

    assert_eq!(
        nest.blob_posts(),
        0,
        "a thumbnail is never a blob-store blob"
    );
    assert_eq!(
        manifests(&nest),
        2,
        "a >300px image on the streaming path posts its thumbnail's manifest too"
    );
    assert_last_manifest_is_an_owner_sealed_jpeg(&engine, &nest).await;
}

#[tokio::test]
async fn upload_file_streaming_no_thumbnail_for_small_image() {
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let full_path = dir.path().join("tiny.png");
    std::fs::write(&full_path, build_png(100, 100)).unwrap();
    let file_size = std::fs::metadata(&full_path).unwrap().len();

    let engine = test_engine(
        &server.uri(),
        dir.path().to_path_buf(),
        Some(BackupKey::from_bytes(KEY)),
    );
    engine
        .upload_file_streaming_inner("tiny.png", &full_path, file_size, false)
        .await
        .expect("streaming upload");

    assert_eq!(nest.blob_posts(), 0);
    assert_eq!(
        manifests(&nest),
        1,
        "a ≤300px image yields no thumbnail on the streaming path either"
    );
}

#[tokio::test]
async fn upload_file_streaming_keyless_owner_only_fails_closed() {
    // The streaming twin of the fail-closed guard: a keyless owner-only
    // engine refuses the streaming upload too, and posts nothing.
    let server = MockServer::start().await;
    let nest = MockNest::new().with_blob_plane().mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let full_path = dir.path().join("photo.png");
    std::fs::write(&full_path, build_png(800, 600)).unwrap();
    let file_size = std::fs::metadata(&full_path).unwrap().len();

    let engine = test_engine(&server.uri(), dir.path().to_path_buf(), None);
    engine
        .upload_file_streaming_inner("photo.png", &full_path, file_size, false)
        .await
        .expect_err("keyless owner-only streaming upload must fail closed");

    assert_eq!(nest.blob_posts(), 0);
    assert_eq!(
        manifests(&nest),
        0,
        "no owner BackupKey ⇒ nothing posted on the streaming path"
    );
}
