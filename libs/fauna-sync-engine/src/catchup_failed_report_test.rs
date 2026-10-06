//! A skipped catch-up change reaches the review list
//! (`docs/goal/behavior/conflicts.md` § Skipped catch-up changes reach the
//! review list).
//!
//! A `catchup_failed` row records a change this device could never apply. It
//! is reported to the nest as an unresolved, candidate-free conflict, re-sent
//! until a reply lands, cured when a later change to that path lands — and it
//! is NOT a conflict a propagated winner exists for, so it must never arm the
//! verbatim-winner apply: with one on record, a later remote change used to
//! overwrite unpublished local edits with no conflict row and no retained loser.

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
use crate::nest_api::FakeSyncControl;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

// Same trimmed harness as `keep_local_resolution_test.rs` (whose helpers are
// private to it).
fn test_sync_engine(
    server_uri: &str,
    watch_dir: std::path::PathBuf,
    device_id: [u8; 32],
) -> SyncEngine {
    let kp = ActorKeypair::generate();
    let http = reqwest::Client::new();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        kp,
        bearer,
        http,
    ));
    let client = SyncClient::new(auth, &device_id);
    let db = SyncDb::open_in_memory().unwrap();
    let nest_client =
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate());
    let transfer_pool = TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None);
    SyncEngine::new(
        watch_dir,
        db,
        client,
        Some("__test".to_string()),
        device_id,
        None,
        None,
        Some(BackupKey::from_bytes([0x61u8; 32]).into()),
        None,
        None,
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        transfer_pool,
        nest_client,
        crate::config::SyncMode::Sync,
    )
}

fn create_change_with_manifest(seq: i64, path: &str, manifest_hash: ContentHash) -> SyncChange {
    SyncChange {
        seq,
        path_hash: hex::encode(fauna_core::sync::path_hash(path)),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: 0,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000, // always older than the local edit
        path: Some(path.to_string()),
        device_id: Some(hex::encode([0xAAu8; 32])),
        ..Default::default()
    }
}

const REL: &str = "skipped.bin";
const PEER_BODY: &[u8] = b"a later peer version of the skipped path\x00\x01";
const USER_BODY: &[u8] = b"the user's unpublished edit \xff\xfe\xfd";

/// A peer's version of [`REL`] sits on the (mock) nest; a fresh device holds
/// an unpublished local edit at the same path.
async fn scenario(server: &MockServer) -> (SyncEngine, tempfile::TempDir, ContentHash) {
    let store = MockNest::new().mount(server).await;
    {
        let peer_watch = tempfile::tempdir().unwrap();
        let peer = test_sync_engine(&server.uri(), peer_watch.path().to_path_buf(), [0xAAu8; 32]);
        std::fs::write(peer_watch.path().join(REL), PEER_BODY).unwrap();
        peer.upload_file(REL).await.expect("peer upload_file");
    }
    let peer_manifest = store
        .last_manifest_hash
        .lock()
        .unwrap()
        .expect("the peer's upload must have POSTed a manifest");
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine(&server.uri(), watch.path().to_path_buf(), [0u8; 32]);
    std::fs::write(watch.path().join(REL), USER_BODY).unwrap();
    (engine, watch, peer_manifest)
}

fn unresolved_of_kind(engine: &SyncEngine, kind: &str) -> usize {
    engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .iter()
        .filter(|(_, p, k, _, _)| p == REL && k == kind)
        .count()
}

/// Slice 1 — the data-loss half. A skip row on the path, an unpublished local
/// edit, then a later remote change: ordinary detection + auto-resolve runs
/// (the newer local edit wins, the peer version is retained nest-side by the
/// resolved report), never a verbatim overwrite of the user's bytes.
#[tokio::test]
async fn a_skip_row_never_arms_the_verbatim_winner_apply() {
    let server = MockServer::start().await;
    let (engine, watch, peer_manifest) = scenario(&server).await;
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));

    engine
        .db()
        .record_conflict(REL, "catchup_failed", Some("path escapes the sync root"))
        .unwrap();

    engine
        .apply_remote_changes(&[create_change_with_manifest(7, REL, peer_manifest)], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        USER_BODY,
        "a skip row is not a propagated winner: the unpublished edit must survive"
    );
    let reports = control.conflict_reports();
    assert!(
        reports
            .iter()
            .any(|r| r.conflict_type == "concurrent_edit" && r.resolution.is_some()),
        "the divergence must go through ordinary detection and be reported \
         auto-resolved (the loser retained nest-side); got {reports:?}"
    );
}

/// Slice 4 — the cure. A reported skip on the path, then a later change lands
/// (here a kept local winner, the head after the resolved report): the local
/// row is resolved and its nest row gets the candidate-free resolve.
#[tokio::test]
async fn a_landed_later_change_cures_the_skip_and_resolves_its_nest_row() {
    let server = MockServer::start().await;
    let (engine, _watch, peer_manifest) = scenario(&server).await;
    let control = FakeSyncControl::accepting();
    engine.set_control_api(Arc::new(control.clone()));

    let skip = engine
        .db()
        .record_skipped_change(REL, false, None, Some("path escapes the sync root"))
        .unwrap()
        .unwrap();
    engine.db().set_conflict_nest_id(skip.id, 77).unwrap();

    engine
        .apply_remote_changes(&[create_change_with_manifest(7, REL, peer_manifest)], 0)
        .await
        .expect("apply_remote_changes");

    assert_eq!(unresolved_of_kind(&engine, "catchup_failed"), 0, "cured");
    let resolves = control.conflict_resolves();
    assert_eq!(resolves.len(), 1, "{resolves:?}");
    assert_eq!(resolves[0].id, 77);
    assert_eq!(
        resolves[0].winning_manifest_hash, None,
        "the candidate-free resolve — the only one a candidate-free conflict admits"
    );
    assert!(
        engine
            .db()
            .list_catchup_failures_owing_nest_resolve()
            .unwrap()
            .is_empty()
    );
}

// ── The two report sites, against key-holding fixtures ──────────────────────

use fauna_core::crypto::OwnerSealKey;
use fauna_core::label_custody;
use fauna_core::path_crypto::LabelRoot;

use crate::pull_remote_changes_test::test_engine_with_keys;

fn held_key() -> BackupKey {
    BackupKey::from_bytes([11u8; 32])
}

fn engine_holding_the_key(watch: &std::path::Path, control: &FakeSyncControl) -> SyncEngine {
    let engine = test_engine_with_keys(
        watch.to_path_buf(),
        Some(OwnerSealKey::Client(held_key())),
        None,
        None,
    );
    engine.set_control_api(Arc::new(control.clone()));
    engine
}

fn sealed_row(seq: i64, path_sealed: Vec<u8>, path_hash: String) -> SyncChange {
    SyncChange {
        seq,
        path_hash,
        manifest_hash: Some(hex::encode([0xABu8; 32])),
        size_bytes: 11,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000,
        path: None,
        path_sealed: Some(path_sealed.into()),
        device_id: Some("peerpeerpeer".to_string()),
        ..Default::default()
    }
}

async fn open_then_apply(engine: &SyncEngine, mut changes: Vec<SyncChange>, since: i64) {
    engine.open_sealed_change_paths(&mut changes);
    engine.apply_remote_changes(&changes, since).await.unwrap();
}

/// The details as a reader holding the set's root opens them, salted by the
/// report's own `path_hash` — what the shared review-list render does.
fn opened_details(report: &fauna_protocol::folders::ConflictReportRequest) -> Option<String> {
    let salt: [u8; 32] = report.path_hash.as_ref()?[..].try_into().ok()?;
    label_custody::render_conflict_details(
        &fauna_core::file_download::FileDownloadKeys::owner(held_key()),
        report.details_sealed.as_ref().map(|b| &b[..]),
        "",
        Some(&salt),
    )
    .text()
    .map(str::to_string)
}

/// The apply-stage refusal knows its plaintext path: it reports through the
/// ordinary funnel — path sealed, empty candidates, no resolution (unsigned),
/// details the content-free reason class — and stamps the reply's id.
#[tokio::test]
async fn the_apply_stage_skip_reports_a_candidate_free_conflict() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let path = "../outside-the-root.txt";
    let row = sealed_row(
        70,
        label_custody::seal_path(&LabelRoot::owner_of(&held_key()), path).unwrap(),
        hex::encode(fauna_core::sync::path_hash(path)),
    );

    open_then_apply(&engine, vec![row.clone()], 69).await;

    let reports = control.conflict_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let r = &reports[0];
    assert_eq!(r.conflict_type, "catchup_failed");
    assert!(r.candidates.is_empty());
    assert_eq!(r.resolution, None);
    assert_eq!(
        r.winner_signature, None,
        "an unresolved report mints no row to sign"
    );
    assert_eq!(r.details.as_deref(), Some("path escapes the sync root"));
    assert_eq!(
        r.path_hash.as_deref().map(Vec::as_slice),
        Some(&fauna_core::sync::path_hash(path)[..])
    );
    assert!(r.path_sealed.is_some(), "the name rides sealed");
    assert_eq!(
        opened_details(r).as_deref(),
        Some("path escapes the sync root")
    );
    assert!(
        engine
            .db()
            .list_unreported_catchup_failures()
            .unwrap()
            .is_empty(),
        "the reply's id is stamped, so nothing is re-sent"
    );

    // The head re-judge meeting the same change again records and reports
    // nothing new (once per path).
    open_then_apply(&engine, vec![SyncChange { seq: 71, ..row }], 70).await;
    assert_eq!(control.conflict_reports().len(), 1);
}

/// The sealed-path refusal has no plaintext: it forwards the change row's own
/// label pair verbatim, `path` empty, details sealed by the row's hash.
#[tokio::test]
async fn the_sealed_path_skip_forwards_the_rows_own_label_pair() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let hash = fauna_core::sync::path_hash("whatever.txt");
    let blob = b"not a cbor envelope".to_vec();

    open_then_apply(
        &engine,
        vec![sealed_row(70, blob.clone(), hex::encode(hash))],
        69,
    )
    .await;

    let reports = control.conflict_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let r = &reports[0];
    assert_eq!(r.conflict_type, "catchup_failed");
    assert_eq!(r.path, "", "no plaintext exists — that IS the failure");
    assert_eq!(
        r.path_sealed.as_deref().map(Vec::as_slice),
        Some(&blob[..]),
        "forwarded verbatim"
    );
    assert_eq!(r.path_hash.as_deref().map(Vec::as_slice), Some(&hash[..]));
    assert!(r.candidates.is_empty());
    assert_eq!(r.resolution, None);
    assert_eq!(
        opened_details(r).as_deref(),
        Some("sealed path unusable: sealed path envelope did not decode")
    );
}

/// A row hash that is not 32 hex bytes cannot salt anything: the report's
/// `path_hash` is the BLAKE3 of its raw string, and the details seal under it.
#[tokio::test]
async fn a_malformed_row_hash_is_reported_under_the_hash_of_its_raw_string() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let blob = label_custody::seal_path(&LabelRoot::owner_of(&held_key()), "a.jpg").unwrap();

    open_then_apply(
        &engine,
        vec![sealed_row(70, blob, "not-hex".to_string())],
        69,
    )
    .await;

    let reports = control.conflict_reports();
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0].path_hash.as_deref().map(Vec::as_slice),
        Some(&blake3::hash(b"not-hex").as_bytes()[..])
    );
    assert!(opened_details(&reports[0]).is_some());
}

/// A seal-less change can be filed nowhere a sealed plane accepts: recorded,
/// never reported, and never listed for a re-send.
#[tokio::test]
async fn a_seal_less_skip_stays_local() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::accepting();
    let engine = engine_holding_the_key(watch.path(), &control);
    let mut row = sealed_row(70, Vec::new(), hex::encode([7u8; 32]));
    row.path_sealed = None;

    open_then_apply(&engine, vec![row], 69).await;
    engine.flush_skip_reports().await;

    assert!(control.conflict_reports().is_empty());
    assert_eq!(
        engine
            .db()
            .list_unresolved_conflicts()
            .unwrap()
            .iter()
            .filter(|(_, _, k, _, _)| k == "catchup_failed")
            .count(),
        1,
        "still recorded, never silent"
    );
}

/// A lost reply leaves the row unreported; the next pass re-sends it exactly
/// once, and a pass after that sends nothing.
#[tokio::test]
async fn a_lost_report_is_re_sent_on_the_next_pass() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::failing("the reply was lost");
    let engine = engine_holding_the_key(watch.path(), &control);
    let hash = fauna_core::sync::path_hash("whatever.txt");

    open_then_apply(
        &engine,
        vec![sealed_row(70, b"bad".to_vec(), hex::encode(hash))],
        69,
    )
    .await;
    assert_eq!(control.conflict_reports().len(), 1, "attempted once");
    assert_eq!(
        engine
            .db()
            .list_unreported_catchup_failures()
            .unwrap()
            .len(),
        1
    );

    control.set_failing(None);
    engine.flush_skip_reports().await;
    assert_eq!(control.conflict_reports().len(), 2, "re-sent");
    assert!(
        engine
            .db()
            .list_unreported_catchup_failures()
            .unwrap()
            .is_empty()
    );

    engine.flush_skip_reports().await;
    assert_eq!(
        control.conflict_reports().len(),
        2,
        "never sent again once landed"
    );
}

/// A failed nest resolve stays owed and the next pass re-sends it.
#[tokio::test]
async fn a_lost_resolve_is_re_sent_on_the_next_pass() {
    let watch = tempfile::tempdir().unwrap();
    let control = FakeSyncControl::failing("the reply was lost");
    let engine = engine_holding_the_key(watch.path(), &control);
    let skip = engine
        .db()
        .record_skipped_change("a.txt", false, None, Some("r"))
        .unwrap()
        .unwrap();
    engine.db().set_conflict_nest_id(skip.id, 9).unwrap();
    engine
        .db()
        .cure_catchup_failures_for_path("a.txt", "")
        .unwrap();

    engine.flush_skip_reports().await;
    assert_eq!(control.conflict_resolves().len(), 1);
    assert_eq!(
        engine
            .db()
            .list_catchup_failures_owing_nest_resolve()
            .unwrap()
            .len(),
        1
    );

    control.set_failing(None);
    engine.flush_skip_reports().await;
    assert_eq!(control.conflict_resolves().len(), 2);
    assert!(
        engine
            .db()
            .list_catchup_failures_owing_nest_resolve()
            .unwrap()
            .is_empty()
    );
}
