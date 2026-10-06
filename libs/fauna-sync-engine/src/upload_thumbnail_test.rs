//! SyncEngine thumbnail producer: [`SyncEngine::upload_file`] generates, seals,
//! and uploads a thumbnail for a synced image, so `fauna.media.list` can surface
//! it.
//!
//! The thumbnail rides as a standalone blob: a >300px image yields a JPEG
//! thumbnail (`fauna_media::pipeline::seal_thumbnail_only`), sealed under the
//! owner `BackupKey` (`Audience::Library`), POSTed to `/api/v1/blob`, and its
//! hash recorded on the change. These tests assert the blob POST happens for a
//! real image and does NOT for a small image / non-image / non-owner-sealed set.
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
/// seals chunks + the thumbnail under the owner key (the owner-encrypted set the
/// producer targets); `None` is a plaintext set (no thumbnail in v1).
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
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    engine
        .upload_file("photo.png")
        .await
        .expect("upload_file succeeds");

    assert_eq!(
        nest.blob_posts(),
        1,
        "a >300px image must POST exactly one thumbnail blob"
    );
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
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    engine.upload_file("tiny.png").await.expect("upload_file");

    assert_eq!(
        nest.blob_posts(),
        0,
        "a ≤300px image yields no thumbnail, so no blob POST"
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
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    engine.upload_file("notes.txt").await.expect("upload_file");

    assert_eq!(nest.blob_posts(), 0, "a non-image file yields no thumbnail");
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

    assert_eq!(
        nest.blob_posts(),
        0,
        "no owner BackupKey ⇒ nothing posted (no thumbnail either)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Streaming path (>= STREAMING_THRESHOLD): the thumbnail is generated by
// reading the file incrementally from disk (`seal_thumbnail_only_from_path`),
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
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    engine
        .upload_file_streaming_inner("photo.png", &full_path, file_size, false)
        .await
        .expect("streaming upload succeeds");

    assert_eq!(
        nest.blob_posts(),
        1,
        "a >300px image on the streaming path must POST exactly one thumbnail blob"
    );
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
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    engine
        .upload_file_streaming_inner("tiny.png", &full_path, file_size, false)
        .await
        .expect("streaming upload");

    assert_eq!(
        nest.blob_posts(),
        0,
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

    assert_eq!(
        nest.blob_posts(),
        0,
        "no owner BackupKey ⇒ nothing posted on the streaming path"
    );
}
