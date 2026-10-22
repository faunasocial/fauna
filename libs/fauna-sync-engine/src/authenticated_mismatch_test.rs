//! Bytes that authenticated under a root this device holds and still miss the
//! record are skipped, never held below (`docs/goal/behavior/file-sync.md`
//! § *A failed change must not strand the device*, TRANSIENT IS THE DEFAULT).
//!
//! A writer co-member reaches each case with bytes it seals itself, and the
//! nest serves them identically on every pull — so an unmarked failure here
//! capped the pass, kept the anchor below the row, and held every member's
//! catch-up of that folder below it for good. Each test
//! applies the hostile row, then an honest one, and asserts the anchor passes
//! both and the honest change lands.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use wiremock::MockServer;

use crate::catchup_failed_report_test::{create_change_with_manifest, test_sync_engine};
use crate::engine::SyncEngine;
use crate::test_support::{BlobStore, MockNest};

const FOLDER: &str = "__test";
const HONEST: &str = "honest.txt";
const HONEST_BODY: &[u8] = b"an honest peer's change after the hostile one";
const HOSTILE: &str = "hostile.bin";

/// The chunk root the engine under test opens with (its owner key, the
/// writer's too: a co-member holds the set's root).
fn set_root() -> [u8; 32] {
    BackupKey::from_bytes([0x61u8; 32]).convergent_chunk_root()
}

/// Seal `(claimed plaintext hash, body that is really there)` pairs under the
/// set's root, the way a writer holding it would, and seed the manifest
/// recording `file_hash` over them. Returns the manifest hash.
fn seed_hostile(
    store: &BlobStore,
    chunks: &[(ContentHash, Vec<u8>)],
    file_hash: ContentHash,
) -> ContentHash {
    let root = set_root();
    let mut stored = Vec::new();
    for (claimed, really) in chunks {
        let framed = fauna_core::chunk_seal::FramedChunk::arbitrary_for_fixtures(
            *claimed,
            fauna_core::compress::compress_chunk(really),
        );
        let (key, body) = fauna_core::chunk_seal::seal_framed_chunk(&framed, &root).unwrap();
        store.seed_chunk(key, body);
        stored.push(key);
    }
    let sizes: Vec<u64> = chunks.iter().map(|(_, b)| b.len() as u64).collect();
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash,
        total_size: sizes.iter().sum(),
        chunk_hashes: chunks.iter().map(|(h, _)| *h).collect(),
        chunk_sizes: sizes,
        stored_hashes: Some(stored),
        sealed_hashes: None,
        min_reader: None,
    };
    let bytes =
        fauna_core::encoding::canonical_encode(&manifest.wire_form(Some(&root)).unwrap()).unwrap();
    let hash = ContentHash::of_raw(&bytes);
    store.seed_manifest(hash, bytes);
    hash
}

/// A peer's honest change to [`HONEST`] on the mock nest, and a fresh device.
async fn scenario(server: &MockServer) -> (SyncEngine, tempfile::TempDir, BlobStore, ContentHash) {
    let store = MockNest::new().mount(server).await;
    let peer_watch = tempfile::tempdir().unwrap();
    let peer = test_sync_engine(
        &server.uri(),
        peer_watch.path().to_path_buf(),
        [0xAAu8; 32],
        FOLDER,
    );
    std::fs::write(peer_watch.path().join(HONEST), HONEST_BODY).unwrap();
    peer.upload_file(HONEST).await.expect("peer upload_file");
    let honest = store.expect_last_manifest_hash();
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), [0u8; 32], FOLDER);
    (engine, watch, store, honest)
}

/// Apply `[hostile row 7, honest row 8]` and assert the anchor passed both,
/// the honest change landed, and the hostile one was recorded, not written.
async fn assert_skipped_and_passed(
    engine: &SyncEngine,
    watch: &tempfile::TempDir,
    hostile: ContentHash,
    honest: ContentHash,
) {
    let batch = engine
        .apply_remote_changes(
            &[
                create_change_with_manifest(7, HOSTILE, hostile),
                create_change_with_manifest(8, HONEST, honest),
            ],
            6,
        )
        .await
        .expect("apply_remote_changes");

    assert!(!batch.deferred, "the hostile row must not cap the pass");
    assert_eq!(
        engine.db().get_anchor().unwrap(),
        8,
        "the anchor passes the hostile row and the honest one after it"
    );
    assert_eq!(
        std::fs::read(watch.path().join(HONEST)).unwrap(),
        HONEST_BODY
    );
    assert!(!watch.path().join(HOSTILE).exists());
    let skipped: Vec<String> = engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .into_iter()
        .filter(|(_, path, kind, _, _)| path == HOSTILE && kind == "catchup_failed")
        .map(|(_, _, _, details, _)| details.unwrap_or_default())
        .collect();
    assert_eq!(
        skipped.len(),
        1,
        "the skip is recorded, never silent: {skipped:?}"
    );
}

/// A chunk sealed under the set's root over a plaintext other than its
/// manifest chunk hash — the shape of the retired daemon's
/// `a_writer_row_with_unopenable_chunks_is_skipped_and_the_next_row_applies`.
#[tokio::test]
async fn a_writer_chunk_that_authenticates_but_misses_its_hash_is_skipped() {
    let server = MockServer::start().await;
    let (engine, watch, store, honest) = scenario(&server).await;
    let claimed = ContentHash::of_raw(b"what the manifest says");
    let hostile = seed_hostile(
        &store,
        &[(claimed, b"what is really there".to_vec())],
        claimed,
    );

    assert_skipped_and_passed(&engine, &watch, hostile, honest).await;
}

/// The streaming path (a file at or past `STREAMING_THRESHOLD`): every chunk
/// opens and addresses its own hash, but they do not reassemble to the
/// recorded file hash. The in-memory path has always marked this; the
/// streaming one must too.
#[tokio::test]
async fn a_streamed_file_whose_chunks_miss_its_file_hash_is_skipped() {
    let server = MockServer::start().await;
    let (engine, watch, store, honest) = scenario(&server).await;
    let chunk = fauna_core::chunker::MAX_CHUNK as u64;
    let count = fauna_core::chunker_stream::STREAMING_THRESHOLD.div_ceil(chunk);
    let chunks: Vec<(ContentHash, Vec<u8>)> = (0..count)
        .map(|i| {
            let body = vec![i as u8; chunk as usize];
            (ContentHash::of_raw(&body), body)
        })
        .collect();
    let hostile = seed_hostile(&store, &chunks, ContentHash::of_raw(b"not these chunks"));

    assert_skipped_and_passed(&engine, &watch, hostile, honest).await;
}
