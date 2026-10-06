//! The engine's ONE serve path at tier_1 — [`SyncEngine::serve_chunk`]
//! (`file-sync.md` § Relay serving → *The seat serves from the file, through
//! one serve core*): a store key resolves through the folder's durable
//! store-key index to a plaintext range of a body this seat holds, which is
//! read, checked, sealed under the folder's own seal root, and served only when
//! the result is the key asked for.
//!
//! Every folder kind serves — owner-sealed, content-keyed, metadata-only (whose
//! upload skips the byte plane but must not skip the index) and public — and so
//! does a body this seat downloaded rather than wrote. An edited body, a
//! placeholder and a key the index does not name each answer `None`. No nest
//! read on this path: every serve below would fail its assertion if it fetched,
//! because the expected bytes are computed locally and the metadata-only
//! store holds no chunk at all.
//!
//! Compiled without `p2p-share` too: relay serving is not an excisable feature.

use fauna_core::crypto::BackupKey;
use fauna_core::folder_keys::FolderContentKeys;
use wiremock::MockServer;

use crate::download_file_bytes_test::build_test_engine;
use crate::engine::SyncEngine;
use crate::test_support::{BlobStore, MockNest};

/// Past the 8 MiB single-chunk threshold, varied so FastCDC cuts real
/// boundaries — the range arithmetic needs more than one chunk.
fn multi_chunk_bytes() -> Vec<u8> {
    (0..12 * 1024 * 1024u64)
        .map(|i| ((i.wrapping_mul(31) ^ (i / 251)) % 251) as u8)
        .collect()
}

fn small_bytes(seed: u32) -> Vec<u8> {
    (0..40_000u32)
        .map(|i| (i.wrapping_mul(seed) % 251) as u8)
        .collect()
}

const OWNER_KEY: [u8; 32] = [0x51; 32];
const CONTENT_KEY: [u8; 32] = [7; 32];

/// The four folder kinds a seat serves, by how the upload seals.
#[derive(Clone, Copy, Debug)]
enum Kind {
    OwnerSealed,
    ContentKeyed,
    MetadataOnly,
    Public,
}

impl Kind {
    /// The `(root, generation)` the engine's upload seals under for this kind
    /// — the same rule `upload_seal_root` applies.
    fn seal_root(self) -> Option<([u8; 32], Option<u64>)> {
        match self {
            Kind::OwnerSealed | Kind::MetadataOnly => Some((
                BackupKey::from_bytes(OWNER_KEY).convergent_chunk_root(),
                None,
            )),
            Kind::ContentKeyed => Some((CONTENT_KEY, Some(1))),
            Kind::Public => None,
        }
    }
}

async fn engine_of(kind: Kind, server: &MockServer, watch: &std::path::Path) -> SyncEngine {
    let owner = || Some(BackupKey::from_bytes(OWNER_KEY));
    let e = |backup, group, keys| {
        build_test_engine(
            &server.uri(),
            watch.to_path_buf(),
            None,
            backup,
            group,
            keys,
        )
    };
    match kind {
        Kind::OwnerSealed => e(owner(), None, None),
        Kind::MetadataOnly => e(owner(), None, None).with_metadata_only_residency(true),
        Kind::ContentKeyed => e(
            None,
            Some(vec![0x42u8; 24]),
            Some(FolderContentKeys::genesis(CONTENT_KEY, 1_000)),
        ),
        Kind::Public => e(None, None, None).with_public_audience(true),
    }
}

/// Write `bytes` at `rel` under `watch` and upload it through the engine's real
/// seal pipeline; returns the locally computed artifacts it must reproduce.
async fn upload(
    engine: &SyncEngine,
    store: &BlobStore,
    watch: &std::path::Path,
    rel: &str,
    bytes: &[u8],
    kind: Kind,
) -> crate::seal::SealedBlob {
    let full = watch.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, bytes).unwrap();
    engine.upload_file(rel).await.expect("upload_file");
    let expected = crate::seal::seal_blob(bytes, kind.seal_root()).unwrap();
    assert_eq!(
        store.expect_last_manifest_hash(),
        expected.manifest_hash,
        "the fixture's seal root must be the one the {kind:?} upload used"
    );
    expected
}

async fn assert_every_key_serves(engine: &SyncEngine, sealed: &crate::seal::SealedBlob) {
    assert!(!sealed.chunks.is_empty());
    for (store_key, body) in &sealed.chunks {
        let got = engine
            .serve_chunk(&store_key.digest())
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("store key {} serves", hex::encode(store_key.digest())));
        assert_eq!(
            &got, body,
            "the served body is the stored chunk, byte for byte"
        );
    }
}

/// Owner-sealed — the arm the share leg's core refused — across a multi-chunk
/// body, so every range read lands at its own offset.
#[tokio::test]
async fn an_owner_sealed_upload_serves_every_store_key() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::OwnerSealed, &server, watch.path()).await;

    let bytes = multi_chunk_bytes();
    let sealed = upload(
        &engine,
        &store,
        watch.path(),
        "big.bin",
        &bytes,
        Kind::OwnerSealed,
    )
    .await;
    assert!(sealed.chunks.len() >= 2, "fixture must be multi-chunk");
    assert_every_key_serves(&engine, &sealed).await;
}

#[tokio::test]
async fn a_content_keyed_upload_serves_every_store_key() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::ContentKeyed, &server, watch.path()).await;

    let sealed = upload(
        &engine,
        &store,
        watch.path(),
        "shared/a.bin",
        &small_bytes(17),
        Kind::ContentKeyed,
    )
    .await;
    assert_every_key_serves(&engine, &sealed).await;
}

/// The metadata-only upload posts no chunk — and the relay exists exactly for
/// this folder, so its skip must not skip the index.
#[tokio::test]
async fn a_metadata_only_upload_serves_though_the_store_holds_no_chunk() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::MetadataOnly, &server, watch.path()).await;

    let sealed = upload(
        &engine,
        &store,
        watch.path(),
        "m.bin",
        &small_bytes(23),
        Kind::MetadataOnly,
    )
    .await;
    assert!(
        store.chunks.lock().unwrap().is_empty(),
        "metadata-only: no chunk rests on the nest"
    );
    assert_every_key_serves(&engine, &sealed).await;
}

/// A public folder rests unsealed: its store key is the plaintext hash and the
/// body the framed plaintext — served through the same core, no root.
#[tokio::test]
async fn a_public_upload_serves_its_unsealed_chunks() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::Public, &server, watch.path()).await;

    let sealed = upload(
        &engine,
        &store,
        watch.path(),
        "pub.txt",
        &small_bytes(29),
        Kind::Public,
    )
    .await;
    assert_every_key_serves(&engine, &sealed).await;
}

/// A body this seat downloaded serves too: the apply writes the index.
#[tokio::test]
async fn a_downloaded_file_serves_every_store_key() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let up_watch = tempfile::tempdir().unwrap();
    let uploader = engine_of(Kind::OwnerSealed, &server, up_watch.path()).await;
    let bytes = small_bytes(31);
    let sealed = upload(
        &uploader,
        &store,
        up_watch.path(),
        "d/data.bin",
        &bytes,
        Kind::OwnerSealed,
    )
    .await;

    let dl_watch = tempfile::tempdir().unwrap();
    let downloader = engine_of(Kind::OwnerSealed, &server, dl_watch.path()).await;
    downloader
        .download_and_write_file(
            "d/data.bin",
            sealed.manifest_hash,
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
        .await
        .expect("download succeeds");
    // The served bytes must come from this seat's disk, not the nest.
    store.chunks.lock().unwrap().clear();
    assert_every_key_serves(&downloader, &sealed).await;
}

/// A body edited since it was indexed answers none — never the new bytes
/// under the old key, never an error.
#[tokio::test]
async fn an_edited_file_answers_none() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::OwnerSealed, &server, watch.path()).await;
    let sealed = upload(
        &engine,
        &store,
        watch.path(),
        "e.bin",
        &small_bytes(37),
        Kind::OwnerSealed,
    )
    .await;

    std::fs::write(watch.path().join("e.bin"), small_bytes(41)).unwrap();
    for (store_key, _) in &sealed.chunks {
        assert_eq!(engine.serve_chunk(&store_key.digest()).await.unwrap(), None);
    }
}

/// A placeholder answers none even while a file still stands at its path: the
/// seat never hydrates — nor reads a cloud-only stub — on an asker's behalf.
#[tokio::test]
async fn a_placeholder_answers_none() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::OwnerSealed, &server, watch.path()).await;
    let sealed = upload(
        &engine,
        &store,
        watch.path(),
        "p.bin",
        &small_bytes(43),
        Kind::OwnerSealed,
    )
    .await;

    engine.mark_placeholder("p.bin").unwrap();
    for (store_key, _) in &sealed.chunks {
        assert_eq!(engine.serve_chunk(&store_key.digest()).await.unwrap(), None);
    }
}

#[tokio::test]
async fn an_unknown_key_answers_none() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = engine_of(Kind::OwnerSealed, &server, watch.path()).await;
    upload(
        &engine,
        &store,
        watch.path(),
        "u.bin",
        &small_bytes(47),
        Kind::OwnerSealed,
    )
    .await;

    assert_eq!(engine.serve_chunk(&[0xAB; 32]).await.unwrap(), None);
}
