//! **The self-echo reads the local body** (`file-sync.md` § Relay serving): a
//! seat's own change comes back in its pull, and the echo compares the
//! manifest's whole-file hash with the local file first. Equal → the local
//! body *is* the echoed content and no chunk is fetched, in every residency;
//! a body that has since moved on is fetched as before.
//!
//! The metadata-only case is the one that lost state: the nest holds no chunk
//! of an own write there, so an echo that always fetched failed before its
//! causal accounting ran (`conflicts.md` clause 5 — the frontier never covered
//! the seat's own row). The mock nest 404s every chunk it was never sent, which
//! is exactly that nest; its chunk-GET log is the "nothing was fetched"
//! observable.
//!
//! **The echo whose bytes no seat serves** (`conflicts.md` clause 5, refinement
//! (iv)): a superseded own write in a metadata-only folder — local has moved
//! on, the nest never held a chunk — is accounted from its manifest alone, the
//! base left where it was; an own covering tail that would re-adopt by log
//! order but cannot be fetched re-asserts local instead of failing silently.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::backfill_thumbnail_test::{seed_file, test_engine};
use crate::test_support::MockNest;

const REL: &str = "doc.txt";
/// `test_engine` runs as the all-zero device, so a row carrying it is an echo.
const OWN_DEVICE: [u8; 32] = [0u8; 32];

fn own_echo(seq: i64, manifest_hash: ContentHash, size: usize) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{REL}"),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: size as i64,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(REL.to_string()),
        device_id: Some(hex::encode(OWN_DEVICE)),
        ..Default::default()
    }
}

fn merge_base(engine: &crate::engine::SyncEngine) -> Option<Vec<u8>> {
    std::fs::read(engine.base_dir_for_test().join(REL)).ok()
}

#[tokio::test]
async fn a_metadata_only_own_echo_accounts_its_row_from_the_local_body() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    )
    .with_metadata_only_residency(true);

    let body: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let manifest = seed_file(&engine, &store, watch.path(), REL, &body).await;
    assert!(
        store.chunks.lock().unwrap().is_empty(),
        "the premise: a metadata-only record sends the manifest and no chunk"
    );

    engine
        .apply_remote_changes(&[own_echo(7, manifest, body.len())], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(
        engine.causal().frontier(REL),
        Some(7),
        "the echo's causal accounting ran — the frontier covers the seat's own row"
    );
    assert_eq!(
        merge_base(&engine).as_deref(),
        Some(&body[..]),
        "the merge base is the file"
    );
    assert!(
        store.fetched_chunks().is_empty(),
        "an echo whose local body still matches fetches no chunk: {:?}",
        store.fetched_chunks()
    );
}

/// "In every residency": a full folder's matching echo saves the download too.
#[tokio::test]
async fn a_full_folders_matching_own_echo_fetches_no_chunk() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    );

    let body = b"a full folder's own write".to_vec();
    let manifest = seed_file(&engine, &store, watch.path(), REL, &body).await;
    assert!(
        !store.chunks.lock().unwrap().is_empty(),
        "the nest holds it"
    );

    engine
        .apply_remote_changes(&[own_echo(3, manifest, body.len())], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(engine.causal().frontier(REL), Some(3));
    assert_eq!(merge_base(&engine).as_deref(), Some(&body[..]));
    assert!(store.fetched_chunks().is_empty());
}

/// A body that has moved on since the record is fetched, as before: the local
/// file cannot stand in for bytes it no longer holds.
#[tokio::test]
async fn an_own_echo_whose_local_body_moved_on_is_fetched() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    );

    let published = b"what was published".to_vec();
    let manifest = seed_file(&engine, &store, watch.path(), REL, &published).await;
    std::fs::write(watch.path().join(REL), b"edited since").unwrap();

    engine
        .apply_remote_changes(&[own_echo(4, manifest, published.len())], 0)
        .await
        .expect("apply_remote_changes");

    assert!(
        !store.fetched_chunks().is_empty(),
        "the published bytes came from the byte plane"
    );
    assert_eq!(engine.causal().frontier(REL), Some(4));
    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        b"edited since",
        "the local edit is untouched"
    );
}

/// Refinement (iv), the accounting half: the echo of a superseded own write in
/// a metadata-only folder is accounted from its manifest — frontier,
/// edit-frontier and manifest memory — with the local edit untouched and the
/// base where it was, although every chunk GET 404s.
#[tokio::test]
async fn a_metadata_only_echo_whose_bytes_no_seat_serves_is_accounted_from_its_manifest() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    )
    .with_metadata_only_residency(true);

    let published = b"what was published, then superseded locally".to_vec();
    let manifest = seed_file(&engine, &store, watch.path(), REL, &published).await;
    assert!(
        store.chunks.lock().unwrap().is_empty(),
        "the premise: a metadata-only record sends the manifest and no chunk"
    );
    std::fs::write(watch.path().join(REL), b"edited since").unwrap();
    let base_before = merge_base(&engine);

    engine
        .apply_remote_changes(&[own_echo(4, manifest, published.len())], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(
        engine.causal().frontier(REL),
        Some(4),
        "the frontier covers the seat's own row although its bytes rest nowhere"
    );
    assert_eq!(
        engine.causal().frontiers(REL).effective_edit_frontier(),
        Some(4),
        "an own EDIT echo advances the edit-frontier, the manifest's hash the witness"
    );
    assert!(
        engine
            .causal()
            .manifest_remembered_at_or_below(REL, 4, &hex::encode(manifest.digest())),
        "the manifest is remembered at the row's seq"
    );
    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        b"edited since",
        "the local edit is untouched"
    );
    assert_eq!(merge_base(&engine), base_before, "the base does not move");
}

/// Refinement (iv), the re-adopt half: an own covering resolution echo that
/// lands after local moved to an earlier-seq resolution would re-adopt by log
/// order; its bytes rest on no seat, so the seat re-asserts its current bytes
/// as a fresh log-tail row — a second manifest reaches the nest.
#[tokio::test]
async fn an_unfetchable_own_covering_tail_re_asserts_local() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    )
    .with_metadata_only_residency(true);

    // This seat's own resolution: published (manifest only), then superseded
    // locally by the earlier-seq resolution it adopted from a peer.
    let ours = b"our resolution, published then replaced".to_vec();
    let ours_manifest = seed_file(&engine, &store, watch.path(), REL, &ours).await;
    let theirs = b"the peer's resolution, adopted at seq 2".to_vec();
    std::fs::write(watch.path().join(REL), &theirs).unwrap();
    engine.causal().hold(REL, 2, &theirs);
    engine.causal().advance_frontier(REL, 2);
    let base = engine.base_dir_for_test().join(REL);
    std::fs::create_dir_all(base.parent().unwrap()).unwrap();
    std::fs::write(&base, &theirs).unwrap();
    let manifests_before = store.manifests.lock().unwrap().len();

    let mut echo = own_echo(5, ours_manifest, ours.len());
    echo.is_resolution = Some(true);
    echo.derived_through = Some(2);
    engine
        .apply_remote_changes(&[echo], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(engine.causal().frontier(REL), Some(5));
    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        theirs,
        "nothing was written over local — there were no bytes to adopt"
    );
    let manifests_after = store.manifests.lock().unwrap().len();
    assert_eq!(
        manifests_after,
        manifests_before + 1,
        "the re-assert published local as a fresh log-tail row"
    );
    assert_ne!(
        store.expect_last_manifest_hash(),
        ours_manifest,
        "the fresh row carries local's bytes, not the lost ones"
    );
}
