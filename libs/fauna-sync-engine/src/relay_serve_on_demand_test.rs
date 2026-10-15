//! The engine's one serve path over an **on-demand replica** at tier_1
//! (`file-sync.md` § Relay serving → *Which seats hold*, the on-demand rows;
//! `p2p-shared-set-build.md` § *Phone peers — design*, decision 1): a replica
//! that owns its tree keeps a body in its kept root or its cache root, never at
//! the engine's root alone, so [`SyncEngine::serve_chunk`] reads through the
//! replica's [`crate::share_body::BodySource`], and every road by which such a
//! replica gains a body — the owned tree's open, the provider's fetch — writes
//! the store-key index the serve resolves through.
//!
//! A placeholder answers none before and after: the seat never hydrates on an
//! asker's behalf. Every served chunk below is checked after the store's chunks
//! are cleared, so a serve that fetched from the nest would fail.
//!
//! Compiled without `p2p-share` too: relay serving is not an excisable feature.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use wiremock::MockServer;

use crate::db::{SyncDb, SyncState};
use crate::download_file_bytes_test::test_sync_engine_over_db;
use crate::engine::SyncEngine;
use crate::provider_face::owned_tree::OwnedTree;
use crate::seal::SealedBlob;
use crate::share_body::OnDemandBodySource;
use crate::test_support::{BlobStore, MockNest};

const OWNER_KEY: [u8; 32] = [0x61; 32];

fn bytes_of(seed: u32) -> Vec<u8> {
    (0..60_000u32)
        .map(|i| (i.wrapping_mul(seed) % 251) as u8)
        .collect()
}

fn owner() -> Option<BackupKey> {
    Some(BackupKey::from_bytes(OWNER_KEY))
}

/// Another seat writes `rel` and records it: the replica below holds only its
/// row until it hydrates.
async fn recorded_elsewhere(
    server: &MockServer,
    store: &BlobStore,
    rel: &str,
    bytes: &[u8],
) -> SealedBlob {
    let watch = tempfile::tempdir().unwrap();
    let writer = test_sync_engine_over_db(
        &server.uri(),
        watch.path().to_path_buf(),
        owner(),
        SyncDb::open_in_memory().unwrap(),
    );
    let full = watch.path().join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, bytes).unwrap();
    writer.upload_file(rel).await.expect("upload_file");
    let sealed = crate::seal::seal_blob(
        bytes,
        Some((
            BackupKey::from_bytes(OWNER_KEY).convergent_chunk_root(),
            None,
        )),
    )
    .unwrap();
    assert_eq!(store.expect_last_manifest_hash(), sealed.manifest_hash);
    sealed
}

/// An on-demand replica that owns its tree: its engine's root is the kept
/// root, its state DB a file (the body source reads it on a second
/// connection, as the share plane's does), and its serve reads through the
/// replica's body source.
struct Replica {
    _dir: tempfile::TempDir,
    tree: OwnedTree,
    engine: SyncEngine,
}

impl Replica {
    fn new(server: &MockServer) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept");
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let db_path = dir.path().join("fsid-test.db");
        let engine = test_sync_engine_over_db(
            &server.uri(),
            kept.clone(),
            owner(),
            SyncDb::open(&db_path).unwrap(),
        );
        engine.set_serve_body(Arc::new(OnDemandBodySource::new(
            kept.clone(),
            cache.clone(),
            SyncDb::open(&db_path).unwrap(),
        )));
        Self {
            _dir: dir,
            tree: OwnedTree::new(kept, cache),
            engine,
        }
    }

    /// The row the replica's populate records for a file another seat wrote:
    /// a placeholder carrying the recorded head.
    fn record_placeholder_row(&self, rel: &str, bytes: &[u8], sealed: &SealedBlob) {
        let hash = ContentHash::of_raw(bytes);
        self.engine
            .db()
            .upsert_entry(
                rel,
                None,
                Some(hash),
                Some(sealed.manifest_hash),
                SyncState::Placeholder,
                0,
                1,
                bytes.len() as i64,
                1,
                None,
            )
            .unwrap();
        self.engine.stamp_own_head_for_test(rel);
    }
}

async fn assert_every_key_serves(engine: &SyncEngine, sealed: &SealedBlob) {
    assert!(!sealed.chunks.is_empty());
    for (store_key, body) in &sealed.chunks {
        let got = engine
            .serve_chunk(&store_key.digest())
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("store key {} serves", hex::encode(store_key.digest())));
        assert_eq!(&got, body, "the served body is the stored chunk");
    }
}

async fn assert_no_key_serves(engine: &SyncEngine, sealed: &SealedBlob) {
    for (store_key, _) in &sealed.chunks {
        assert_eq!(engine.serve_chunk(&store_key.digest()).await.unwrap(), None);
    }
}

/// The owned tree's open hydrates into the cache root — a body the engine's
/// own root never holds. It serves once opened, a placeholder answers none
/// before, and the body freed again answers none after.
#[tokio::test]
async fn a_body_opened_into_the_cache_root_serves_and_a_placeholder_answers_none() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let bytes = bytes_of(17);
    let sealed = recorded_elsewhere(&server, &store, "docs/a.bin", &bytes).await;

    let replica = Replica::new(&server);
    replica.record_placeholder_row("docs/a.bin", &bytes, &sealed);
    assert_no_key_serves(&replica.engine, &sealed).await;

    let body = replica
        .tree
        .open_for_read(&replica.engine, "docs/a.bin")
        .await
        .expect("open hydrates");
    assert!(body.starts_with(replica.tree.cache_root()));

    // Every served chunk now comes from this replica's cache root.
    store.chunks.lock().unwrap().clear();
    assert_every_key_serves(&replica.engine, &sealed).await;

    replica
        .tree
        .record_placeholder(&replica.engine, "docs/a.bin")
        .unwrap();
    assert_no_key_serves(&replica.engine, &sealed).await;
}

/// The provider's fetch (`fetchContents`, the File Provider's and cfapi's
/// hydrate) writes the index too: the body it hands the OS serves once the
/// row says it is held.
#[tokio::test]
async fn a_body_the_provider_fetched_serves() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let bytes = bytes_of(23);
    let sealed = recorded_elsewhere(&server, &store, "b.bin", &bytes).await;

    let replica = Replica::new(&server);
    replica.record_placeholder_row("b.bin", &bytes, &sealed);
    let dest = replica.tree.kept_root().join("b.bin");
    crate::provider_face::serve_fetch_to_path(&replica.engine, "b.bin", &dest)
        .await
        .expect("fetch");

    store.chunks.lock().unwrap().clear();
    assert_every_key_serves(&replica.engine, &sealed).await;
}

/// A body the replica itself wrote stands in its kept root and serves through
/// the same source.
#[tokio::test]
async fn a_kept_root_write_serves() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let replica = Replica::new(&server);
    let bytes = bytes_of(29);
    let full = replica.tree.kept_root().join("mine.bin");
    std::fs::write(&full, &bytes).unwrap();
    replica
        .engine
        .upload_file("mine.bin")
        .await
        .expect("upload");
    let sealed = crate::seal::seal_blob(
        &bytes,
        Some((
            BackupKey::from_bytes(OWNER_KEY).convergent_chunk_root(),
            None,
        )),
    )
    .unwrap();
    assert_every_key_serves(&replica.engine, &sealed).await;
}
