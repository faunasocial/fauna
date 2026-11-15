//! A skipped catch-up change reaches the review list on an ON-DEMAND root
//! (`docs/goal/behavior/conflicts.md` § Skipped catch-up changes reach the
//! review list → *The sites on an on-demand root*).
//!
//! An on-demand root receives through the placeholder fold, which reads the
//! set's whole log on every pass and has no apply stage — so each refusal is
//! reported where the root first knows it: an unopenable sealed path and a
//! refused path at the fold, a permanent content refusal at hydration. The
//! report shapes and the once-per-path record are the resident ones.

use std::sync::Arc;

use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::catchup_failed_report_test::{
    create_change_with_manifest, engine_holding_the_key, opened_details, sealed_row,
    test_sync_engine,
};
use crate::db::SyncState;
use crate::engine::SyncEngine;
use crate::nest_api::FakeSyncControl;
use crate::test_support::MockNest;

fn unresolved_skips(engine: &SyncEngine) -> usize {
    engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .iter()
        .filter(|(_, _, k, _, _)| k == "catchup_failed")
        .count()
}

fn skip_reports(control: &FakeSyncControl) -> Vec<fauna_protocol::folders::ConflictReportRequest> {
    control
        .conflict_reports()
        .into_iter()
        .filter(|r| r.conflict_type == "catchup_failed")
        .collect()
}

const BAD_SEAL: &[u8] = b"not a cbor envelope";
const SEALED_NAME: &str = "whatever.txt";

fn unopenable(seq: i64) -> SyncChange {
    sealed_row(
        seq,
        BAD_SEAL.to_vec(),
        hex::encode(fauna_core::sync::path_hash(SEALED_NAME)),
    )
}

// ── (a) the fold ────────────────────────────────────────────────────────────

/// An unopenable sealed path folded twice is reported ONCE, forwarding the
/// row's own label pair, gets no row — and every other head of the same log
/// is placed in the same pass (nothing blocks).
#[tokio::test]
async fn an_unopenable_path_folded_twice_reports_once_and_places_the_rest() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let other = create_change_with_manifest(71, "other.txt", ContentHash::of_raw(b"o"));
    let log = vec![unopenable(70), other];

    engine.fold_and_report(&log).await.unwrap();
    engine.fold_and_report(&log).await.unwrap();

    let reports = skip_reports(&control);
    assert_eq!(reports.len(), 1, "{reports:?}");
    let r = &reports[0];
    assert_eq!(r.path, "", "no plaintext exists — that IS the failure");
    assert_eq!(
        r.path_sealed.as_deref().map(Vec::as_slice),
        Some(BAD_SEAL),
        "forwarded verbatim"
    );
    assert_eq!(
        r.path_hash.as_deref().map(Vec::as_slice),
        Some(&fauna_core::sync::path_hash(SEALED_NAME)[..])
    );
    assert!(r.candidates.is_empty());
    assert_eq!(r.resolution, None);
    assert_eq!(
        opened_details(r).as_deref(),
        Some("sealed path unusable: sealed path envelope did not decode")
    );
    assert_eq!(unresolved_skips(&engine), 1);
    assert_eq!(
        engine.db().get_entry("other.txt").unwrap().map(|e| e.state),
        Some(SyncState::Placeholder),
        "the other head is placed in the same pass"
    );
    assert_eq!(
        engine.db().list_all().unwrap().len(),
        1,
        "no row for the unopenable one"
    );
}

/// A later row of the same path hash that OPENS is the cure: the local row is
/// resolved, its nest row gets the candidate-free resolve, and re-reading the
/// whole log (the refused row still in it, no longer the newest of its hash)
/// records and reports nothing again.
#[tokio::test]
async fn a_later_openable_row_of_the_same_hash_cures_the_skip() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    engine.fold_and_report(&[unopenable(70)]).await.unwrap();
    assert_eq!(skip_reports(&control).len(), 1);

    let opened = create_change_with_manifest(72, SEALED_NAME, ContentHash::of_raw(b"w"));
    let log = vec![unopenable(70), opened];
    engine.fold_and_report(&log).await.unwrap();

    assert_eq!(unresolved_skips(&engine), 0, "cured");
    assert_eq!(control.conflict_resolves().len(), 1);
    assert_eq!(
        engine.db().get_entry(SEALED_NAME).unwrap().map(|e| e.state),
        Some(SyncState::Placeholder)
    );

    engine.fold_and_report(&log).await.unwrap();
    assert_eq!(skip_reports(&control).len(), 1, "never re-recorded");
    assert_eq!(control.conflict_resolves().len(), 1);
    assert_eq!(unresolved_skips(&engine), 0);
}

/// A refused row that is NOT the newest of its hash is never reported: the
/// later row that opened already supersedes it.
#[tokio::test]
async fn a_superseded_unopenable_row_is_not_reported() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let opened = create_change_with_manifest(72, SEALED_NAME, ContentHash::of_raw(b"w"));

    engine
        .fold_and_report(&[unopenable(70), opened])
        .await
        .unwrap();

    assert!(skip_reports(&control).is_empty());
    assert_eq!(unresolved_skips(&engine), 0);
}

/// A path the root may not hold gets no row and no placeholder, and is
/// reported once through the ordinary funnel with the apply stage's reason.
#[tokio::test]
async fn a_refused_path_gets_no_row_and_is_reported_once() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let escape = create_change_with_manifest(70, "../escape.txt", ContentHash::of_raw(b"e"));
    let log = vec![escape];

    let fold = engine.fold_and_report(&log).await.unwrap();
    engine.fold_and_report(&log).await.unwrap();

    assert_eq!(fold.recorded, 0);
    assert!(fold.created.is_empty());
    assert!(engine.db().get_entry("../escape.txt").unwrap().is_none());
    let reports = skip_reports(&control);
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0].details.as_deref(),
        Some(fauna_core::apply_failure::PermanentApplyFailure::PATH_REFUSED.reason)
    );
    assert_eq!(
        reports[0].path_hash.as_deref().map(Vec::as_slice),
        Some(&fauna_core::sync::path_hash("../escape.txt")[..])
    );
}

/// The fold's delete arm, for a path with NO row, cures a skip on record
/// under that path.
#[tokio::test]
async fn a_row_less_folded_delete_cures_a_skip_on_record() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let skip = engine
        .db()
        .record_skipped_change("gone.txt", false, None, Some("r"))
        .unwrap()
        .unwrap();
    engine.db().set_conflict_nest_id(skip.id, 77).unwrap();
    let delete = SyncChange {
        seq: 9,
        path_hash: hex::encode(fauna_core::sync::path_hash("gone.txt")),
        manifest_hash: None,
        change_type: "delete".to_string(),
        created_at: 1_700_000_000_000,
        path: Some("gone.txt".to_string()),
        device_id: Some(hex::encode([0xAAu8; 32])),
        ..Default::default()
    };

    let fold = engine.fold_and_report(&[delete]).await.unwrap();

    assert!(fold.deleted.is_empty(), "no row: nothing for the host");
    assert_eq!(unresolved_skips(&engine), 0, "cured");
    let resolves = control.conflict_resolves();
    assert_eq!(resolves.len(), 1, "{resolves:?}");
    assert_eq!(resolves[0].id, 77);
}

// ── (b) hydration ───────────────────────────────────────────────────────────

const REL: &str = "stuck.bin";

/// A manifest stamped for a reader this binary does not implement — a
/// permanent content refusal (`apply_failure::permanent_reason`).
fn seed_too_new_manifest(store: &crate::test_support::BlobStore) -> ContentHash {
    let manifest = ChunkManifest {
        file_hash: ContentHash::of_raw(b"x"),
        total_size: 0,
        chunk_hashes: Vec::new(),
        chunk_sizes: Vec::new(),
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: Some(99),
    };
    let bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let hash = ContentHash::of_raw(&bytes);
    store.seed_manifest(hash, bytes);
    hash
}

fn seed_placeholder(engine: &SyncEngine, rel: &str, manifest: ContentHash, size: i64) {
    engine
        .db()
        .upsert_entry(
            rel,
            None,
            None,
            Some(manifest),
            SyncState::Placeholder,
            0,
            1,
            size,
            1,
            None,
        )
        .unwrap();
    engine.stamp_own_head_for_test(rel);
}

async fn on_demand_engine(server: &MockServer) -> (SyncEngine, tempfile::TempDir, FakeSyncControl) {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        [0u8; 32],
        "__test",
    );
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));
    (engine, watch, control)
}

/// A hydration failing permanently reports ONCE through the ordinary funnel,
/// keeps the row a `Placeholder`, and is cured when the fold re-points the
/// placeholder at a later head.
#[tokio::test]
async fn a_permanent_hydration_failure_reports_once_keeps_the_placeholder_and_a_repoint_cures_it() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let (engine, watch, control) = on_demand_engine(&server).await;
    let bad = seed_too_new_manifest(&store);
    seed_placeholder(&engine, REL, bad, 0);

    assert!(engine.download_file_bytes(REL).await.is_err());
    assert!(
        engine
            .download_file_to_path(REL, &watch.path().join("dest.bin"))
            .await
            .is_err()
    );

    let reports = skip_reports(&control);
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0].details.as_deref(),
        Some(fauna_core::apply_failure::PermanentApplyFailure::MANIFEST_TOO_NEW.reason)
    );
    assert_eq!(
        reports[0].path_hash.as_deref().map(Vec::as_slice),
        Some(&fauna_core::sync::path_hash(REL)[..])
    );
    assert_eq!(
        engine.db().get_entry(REL).unwrap().map(|e| e.state),
        Some(SyncState::Placeholder),
        "the placeholder stays listed"
    );

    // The fold re-points the placeholder at a later head: cured.
    let moved = create_change_with_manifest(90, REL, ContentHash::of_raw(b"later"));
    engine.fold_and_report(&[moved]).await.unwrap();
    assert_eq!(unresolved_skips(&engine), 0, "cured by the re-point");
    assert_eq!(control.conflict_resolves().len(), 1);
}

/// A later SUCCESSFUL hydration of the path cures a skip on record (a key
/// gained, a reader upgraded).
#[tokio::test]
async fn a_successful_hydration_cures_the_skip() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let (engine, watch, control) = on_demand_engine(&server).await;
    let body = b"the bytes that finally open".to_vec();
    std::fs::write(watch.path().join(REL), &body).unwrap();
    engine.upload_file(REL).await.expect("upload_file");
    let good = store.expect_last_manifest_hash();
    std::fs::remove_file(watch.path().join(REL)).unwrap();
    seed_placeholder(&engine, REL, good, body.len() as i64);
    let skip = engine
        .db()
        .record_skipped_change(REL, false, None, Some("r"))
        .unwrap()
        .unwrap();
    engine.db().set_conflict_nest_id(skip.id, 77).unwrap();

    assert_eq!(engine.download_file_bytes(REL).await.unwrap(), body);

    assert_eq!(unresolved_skips(&engine), 0, "cured");
    let resolves = control.conflict_resolves();
    assert_eq!(resolves.len(), 1, "{resolves:?}");
    assert_eq!(resolves[0].id, 77);
}

/// A transient hydration failure (the manifest not on the nest yet) records
/// nothing.
#[tokio::test]
async fn a_transient_hydration_failure_records_nothing() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let (engine, _watch, control) = on_demand_engine(&server).await;
    seed_placeholder(&engine, REL, ContentHash::of_raw(b"not relayed yet"), 10);

    let err = engine.download_file_bytes(REL).await.unwrap_err();
    assert_eq!(
        fauna_core::apply_failure::permanent_reason(&err),
        None,
        "{err:#}"
    );

    assert_eq!(unresolved_skips(&engine), 0);
    assert!(skip_reports(&control).is_empty());
}
