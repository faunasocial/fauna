//! A local conflict row arms the verbatim-winner apply only
//! when a propagated winner can exist: when the row's candidate-bearing
//! (chooser) report LANDED on the nest. `conflicts.md` § Skipped catch-up
//! changes reach the review list, the verbatim-winner ruling.
//!
//! The defect this pins: the degrade arms of `auto_resolve_conflict` and
//! `report_declined_delete` recorded a `concurrent_edit` / `delete_declined`
//! row — kinds on `CONFLICT_KINDS_WITH_PROPAGATED_WINNER` — although no
//! chooser report had reached the nest (the local version's upload failed, so
//! the report went candidate-free; or every report rung failed). The next
//! incoming change on the path then skipped conflict detection and was written
//! over the local file: the user's unpublished edit, which existed nowhere but
//! this disk, was gone with no conflict row and no retained loser.
//!
//! Each degrade test drives two incoming changes through the same harness
//! `keep_local_resolution_test.rs` uses (a stateful HTTP blob store plus the
//! [`FakeSyncControl`] WS-RPC seam) and asserts the local bytes survive the
//! second change AND that ordinary detection ran on it (a further conflict
//! report). The chooser test is the other side of the fence: a row whose
//! candidate-bearing report landed still arms the verbatim apply.

use std::sync::Arc;

use fauna_core::data::ContentHash;
use fauna_protocol::sync::SyncChange;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::engine::SyncEngine;
use crate::keep_local_resolution_test::{create_change_with_manifest, test_sync_engine};
use crate::nest_api::FakeSyncControl;
use crate::pull_remote_changes_test::seed_tracked_file;
use crate::test_support::MockNest;

const REL: &str = "contested.bin";
const PEER_V1: &[u8] = b"a peer's first version\x00\x01";
const PEER_V2: &[u8] = b"a peer's second version\x02\x03";
const USER_BODY: &[u8] = b"the user's unpublished edit \xff\xfe";
const PEER: [u8; 32] = [0xAAu8; 32];

/// A peer publishes two versions of [`REL`] to the mock store; returns their
/// manifest hashes.
async fn peer_versions(server: &MockServer) -> (ContentHash, ContentHash) {
    let store = MockNest::new().mount(server).await;
    let peer_watch = tempfile::tempdir().unwrap();
    let peer = test_sync_engine(&server.uri(), peer_watch.path().to_path_buf(), PEER);
    std::fs::write(peer_watch.path().join(REL), PEER_V1).unwrap();
    peer.upload_file(REL).await.expect("peer upload v1");
    let v1 = store.expect_last_manifest_hash();
    std::fs::write(peer_watch.path().join(REL), PEER_V2).unwrap();
    peer.upload_file(REL).await.expect("peer upload v2");
    let v2 = store.expect_last_manifest_hash();
    assert_ne!(v1, v2);
    (v1, v2)
}

/// From now on every chunk upload is refused — the transient failure (or a
/// hostile nest's refusal) that keeps the local version off the nest. Reads
/// still serve, so the peer's versions stay fetchable.
async fn refuse_chunk_uploads(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v1/chunks"))
        .respond_with(ResponseTemplate::new(400))
        .with_priority(1)
        .mount(server)
        .await;
}

fn modify_change(seq: i64, manifest: ContentHash) -> SyncChange {
    SyncChange {
        change_type: "modify".to_string(),
        ..create_change_with_manifest(seq, REL, manifest)
    }
}

fn delete_change(seq: i64) -> SyncChange {
    SyncChange {
        manifest_hash: None,
        change_type: "delete".to_string(),
        ..create_change_with_manifest(seq, REL, ContentHash::of_raw(b""))
    }
}

/// A device holding the user's unpublished edit at [`REL`], untracked.
fn device_with_edit(server: &MockServer) -> (SyncEngine, tempfile::TempDir) {
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), [0u8; 32]);
    std::fs::write(watch.path().join(REL), USER_BODY).unwrap();
    (engine, watch)
}

/// The local version's upload fails: the report goes candidate-free. The next
/// remote change must go through ordinary detection — never be written over
/// the edit that exists only on this disk.
#[tokio::test]
async fn a_failed_local_upload_never_arms_the_verbatim_winner_apply() {
    let server = MockServer::start().await;
    let (v1, v2) = peer_versions(&server).await;
    let (engine, watch) = device_with_edit(&server);
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));
    refuse_chunk_uploads(&server).await;

    engine
        .apply_remote_changes(&[create_change_with_manifest(1, REL, v1)], 0)
        .await
        .expect("first apply");
    let reports = control.conflict_reports();
    assert_eq!(reports.len(), 1, "one candidate-free report");
    assert!(reports[0].candidates.is_empty() && reports[0].resolution.is_none());

    engine
        .apply_remote_changes(&[modify_change(2, v2)], 1)
        .await
        .expect("second apply");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "a candidate-free conflict row has no propagated winner — the \
         next remote change must not overwrite the never-uploaded local edit"
    );
    assert_eq!(
        control.conflict_reports().len(),
        2,
        "ordinary detection must run on the second change (it conflicts again)"
    );
}

/// Same fence for the declined-delete arm: the surviving version's upload
/// fails, so its `delete_declined` report is candidate-free.
#[tokio::test]
async fn a_failed_survivor_upload_never_arms_the_verbatim_winner_apply() {
    let server = MockServer::start().await;
    let (_v1, v2) = peer_versions(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), [0u8; 32]);
    seed_tracked_file(&engine, watch.path(), REL, b"synced v0");
    std::fs::write(watch.path().join(REL), USER_BODY).unwrap();
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));
    refuse_chunk_uploads(&server).await;

    engine
        .apply_remote_changes(&[delete_change(1)], 0)
        .await
        .expect("delete apply");
    let reports = control.conflict_reports();
    assert_eq!(
        reports.len(),
        1,
        "one candidate-free declined-delete report"
    );
    assert_eq!(reports[0].conflict_type, "delete_declined");
    assert!(reports[0].candidates.is_empty());

    engine
        .apply_remote_changes(&[modify_change(2, v2)], 1)
        .await
        .expect("second apply");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "a candidate-free declined-delete row has no propagated winner"
    );
    assert_eq!(
        control.conflict_reports().len(),
        2,
        "ordinary detection must run on the second change (it conflicts again)"
    );
}

/// The local version uploaded, but no report rung landed (the resolved report
/// and the unresolved fallback both failed): no nest row, so no winner will
/// ever be chosen — the next change goes through ordinary detection.
#[tokio::test]
async fn an_unlanded_chooser_report_never_arms_the_verbatim_winner_apply() {
    let server = MockServer::start().await;
    let (v1, v2) = peer_versions(&server).await;
    let (engine, watch) = device_with_edit(&server);
    let control = FakeSyncControl::failing("nest unreachable");
    engine.set_control_api(Arc::new(control.clone()));

    engine
        .apply_remote_changes(&[create_change_with_manifest(1, REL, v1)], 0)
        .await
        .expect("first apply");
    assert_eq!(
        control.conflict_reports().len(),
        2,
        "resolved, then unresolved"
    );

    engine
        .apply_remote_changes(&[modify_change(2, v2)], 1)
        .await
        .expect("second apply");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "a row whose chooser report never landed has no propagated winner"
    );
    assert_eq!(
        control.conflict_reports().len(),
        4,
        "ordinary detection must run on the second change (the ladder again)"
    );
}

/// The other side of the fence: the resolved report is refused but the
/// candidate-bearing unresolved report LANDS — the user can choose a winner on
/// the review list, so the next change on the path is that propagated winner
/// and is taken verbatim, clearing the local row.
#[tokio::test]
async fn a_landed_chooser_report_arms_the_verbatim_winner_apply() {
    let server = MockServer::start().await;
    let (v1, v2) = peer_versions(&server).await;
    let (engine, watch) = device_with_edit(&server);
    let control = FakeSyncControl::failing_first_reports(1);
    engine.set_control_api(Arc::new(control.clone()));

    engine
        .apply_remote_changes(&[create_change_with_manifest(1, REL, v1)], 0)
        .await
        .expect("first apply");
    let reports = control.conflict_reports();
    assert_eq!(
        reports.len(),
        2,
        "resolved (refused), then unresolved (landed)"
    );
    assert!(
        !reports[1].candidates.is_empty(),
        "the chooser carries candidates"
    );
    assert!(
        engine
            .db()
            .has_unresolved_winner_conflict_for_path(REL)
            .unwrap()
    );

    engine
        .apply_remote_changes(&[modify_change(2, v2)], 1)
        .await
        .expect("winner apply");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        PEER_V2,
        "the propagated winner is taken verbatim"
    );
    assert_eq!(
        control.conflict_reports().len(),
        2,
        "no detection, no new report"
    );
    assert!(
        engine.db().list_unresolved_conflicts().unwrap().is_empty(),
        "the winner's landing clears the local row"
    );
}
