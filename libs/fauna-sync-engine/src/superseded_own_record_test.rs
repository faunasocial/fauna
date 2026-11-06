//! **A superseded own-record body is replaced, never freed** (`file-sync.md`
//! § Relay serving): on an on-demand root in a metadata-only folder, a seat
//! whose own recorded write another seat has written past follows the head by
//! fetching the new body first and writing it over the old one — so at every
//! instant it holds one whole version of the file.
//!
//! The mock nest's blob plane stands in for the relay: a body it holds is one
//! some holder answers for, a manifest it does not hold is a holder that is
//! away. Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), beside the
//! holder-keeps pins in `pull_remote_changes_test.rs`.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use wiremock::MockServer;

use crate::backfill_thumbnail_test::{seed_file, test_engine};
use crate::db::SyncState;
use crate::engine::{StaleHydratedRow, SyncEngine};
use crate::pull_remote_changes_test::record_own_write;
use crate::test_support::MockNest;

const REL: &str = "doc.txt";
const V1: &[u8] = b"v1 - this seat's own record";

/// What the fold reports for `REL` once the head is `manifest`.
fn head_moved_to(manifest: ContentHash, size: usize) -> StaleHydratedRow {
    StaleHydratedRow {
        relative_path: REL.to_string(),
        manifest_hash: manifest,
        size_bytes: size as i64,
        content_stamp: None,
        remote_mtime: 0,
        version_num: 1,
        signed_as: None,
    }
}

/// Seat A on an off-disk root of a metadata-only folder, holding `V1` as its
/// own record.
fn seat_holding_its_own_record(uri: &str, watch: &std::path::Path, key: BackupKey) -> SyncEngine {
    let seat = test_engine(uri, watch.to_path_buf(), Some(key)).with_metadata_only_residency(true);
    seat.set_placeholders_off_disk();
    record_own_write(&seat, watch, REL, V1);
    seat
}

#[tokio::test]
async fn a_superseded_own_record_is_replaced_by_the_fetched_head() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);

    // Seat B writes past A's version; its body is what a holder answers with.
    let b_watch = tempfile::tempdir().unwrap();
    let seat_b = test_engine(
        &server.uri(),
        b_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let v2: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
    let v2_manifest = seed_file(&seat_b, &store, b_watch.path(), REL, &v2).await;

    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(&server.uri(), a_watch.path(), key);
    assert!(
        !seat_a.dehydrate_off_disk(REL).unwrap(),
        "the holder-keeps gate refuses to free the old body — the premise"
    );

    let replaced = seat_a
        .replace_superseded_own_record(&head_moved_to(v2_manifest, v2.len()))
        .await
        .expect("a holder answered");
    assert!(replaced, "the seat follows the head");

    assert_eq!(
        std::fs::read(a_watch.path().join(REL)).unwrap(),
        v2,
        "the new head's body is on the disk, in place of the old one"
    );
    let entry = seat_a.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(entry.state, SyncState::Synced, "the file stays hydrated");
    assert_eq!(
        entry.manifest_hash,
        Some(v2_manifest),
        "the row is at the head"
    );
    assert!(
        seat_a.is_dehydration_safe(REL),
        "the body is now a fetched one — another holder has it, so it may be freed"
    );
}

#[tokio::test]
async fn with_no_holder_answering_the_seat_keeps_its_old_version_whole() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(
        &server.uri(),
        a_watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    let before = seat_a.db().get_entry(REL).unwrap().expect("the row");

    // A head nobody serves: its writer is away.
    let unreachable = ContentHash::from_digest_raw([0x99; 32]);
    assert!(
        seat_a
            .replace_superseded_own_record(&head_moved_to(unreachable, 10))
            .await
            .is_err(),
        "a fetch that fails is an error the next pull retries"
    );

    assert_eq!(std::fs::read(a_watch.path().join(REL)).unwrap(), V1);
    let after = seat_a.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(after.state, SyncState::Synced);
    assert_eq!(
        after.manifest_hash, before.manifest_hash,
        "the row has not moved"
    );
    assert_eq!(after.recorded_content_hash, before.recorded_content_hash);
}

/// The rule takes only the body the holder-keeps gate holds. A body the gate
/// would free is the ordinary invalidation's, and one edited since its record
/// is a local edit — neither is fetched over.
#[tokio::test]
async fn only_a_kept_own_record_is_replaced() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);
    let b_watch = tempfile::tempdir().unwrap();
    let seat_b = test_engine(
        &server.uri(),
        b_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let v2 = b"v2 - written past it".to_vec();
    let v2_manifest = seed_file(&seat_b, &store, b_watch.path(), REL, &v2).await;
    let moved = head_moved_to(v2_manifest, v2.len());

    // A full folder: the nest holds the old body, so the free is not refused.
    let full_watch = tempfile::tempdir().unwrap();
    let full = seat_holding_its_own_record(&server.uri(), full_watch.path(), key.clone());
    full.install_residency(Some(false));
    assert!(!full.replace_superseded_own_record(&moved).await.unwrap());
    assert_eq!(std::fs::read(full_watch.path().join(REL)).unwrap(), V1);

    // An edit the seat has not recorded yet sits where the own record was.
    let edited_watch = tempfile::tempdir().unwrap();
    let edited = seat_holding_its_own_record(&server.uri(), edited_watch.path(), key);
    std::fs::write(edited_watch.path().join(REL), b"edited since the record").unwrap();
    assert!(!edited.replace_superseded_own_record(&moved).await.unwrap());
    assert_eq!(
        std::fs::read(edited_watch.path().join(REL)).unwrap(),
        b"edited since the record"
    );
}

/// The fold that reports `row` as its one stale hydrated row — what the
/// on-demand pull hands [`SyncEngine::apply_refresh_fold`].
fn fold_reporting(row: StaleHydratedRow) -> crate::engine::PlaceholderFold {
    crate::engine::PlaceholderFold {
        stale_hydrated: vec![row],
        ..Default::default()
    }
}

/// The refresh of a host whose OS keeps the replica (the File Provider) holds
/// a kept own record at its old head while no holder answers for the new one:
/// the row is neither re-pointed nor stripped of its proof, so the OS is never
/// told its copy is superseded before this seat holds the head.
#[tokio::test]
async fn the_refresh_holds_a_kept_own_record_at_its_old_head_while_no_holder_answers() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(
        &server.uri(),
        a_watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    let before = seat_a.db().get_entry(REL).unwrap().expect("the row");

    let unreachable = ContentHash::from_digest_raw([0x99; 32]);
    let changed = seat_a
        .apply_refresh_fold(&fold_reporting(head_moved_to(unreachable, 10)), None)
        .await
        .expect("a held row is no error of the refresh");
    assert!(!changed, "nothing moved, so the enumerator has nothing new");

    let after = seat_a.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(
        after.state,
        SyncState::Synced,
        "the body is still this seat's"
    );
    assert_eq!(
        after.manifest_hash, before.manifest_hash,
        "the row has not moved"
    );
    assert_eq!(
        after.recorded_content_hash, before.recorded_content_hash,
        "the proof stands, so the item's contentVersion is unchanged"
    );
    assert_eq!(std::fs::read(a_watch.path().join(REL)).unwrap(), V1);
}

/// With a holder answering, the refresh follows the head the rule's way: the
/// new body is fetched first and the row moves last, onto a fetched proof.
#[tokio::test]
async fn the_refresh_moves_a_kept_own_record_only_onto_the_fetched_head() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);
    let b_watch = tempfile::tempdir().unwrap();
    let seat_b = test_engine(
        &server.uri(),
        b_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let v2 = b"v2 - another seat wrote past it".to_vec();
    let v2_manifest = seed_file(&seat_b, &store, b_watch.path(), REL, &v2).await;

    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(&server.uri(), a_watch.path(), key);
    let changed = seat_a
        .apply_refresh_fold(&fold_reporting(head_moved_to(v2_manifest, v2.len())), None)
        .await
        .unwrap();
    assert!(changed, "the row moved, so the OS must re-list it");

    let entry = seat_a.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "never a placeholder in between"
    );
    assert_eq!(entry.manifest_hash, Some(v2_manifest));
    assert_eq!(std::fs::read(a_watch.path().join(REL)).unwrap(), v2);
    assert!(
        seat_a.is_dehydration_safe(REL),
        "the new body is a fetched one, freeable like any other"
    );
}

/// An own record the gate does not keep — the folder is full — is re-pointed
/// by the refresh as before: the nest holds the new head.
#[tokio::test]
async fn the_refresh_repoints_an_own_record_of_a_full_folder_as_before() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(
        &server.uri(),
        a_watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    seat_a.install_residency(Some(false));

    let moved = ContentHash::from_digest_raw([0x99; 32]);
    assert!(
        seat_a
            .apply_refresh_fold(&fold_reporting(head_moved_to(moved, 10)), None)
            .await
            .unwrap()
    );
    let entry = seat_a.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(entry.state, SyncState::Placeholder);
    assert_eq!(entry.manifest_hash, Some(moved));
    assert_eq!(entry.recorded_content_hash, None);
}

/// A kept own record edited since its record is held too: the edit is the
/// upload rail's, and freeing the body under it would lose the only copy.
#[tokio::test]
async fn the_refresh_holds_a_kept_own_record_edited_since_its_record() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(
        &server.uri(),
        a_watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    std::fs::write(a_watch.path().join(REL), b"edited since the record").unwrap();
    let before = seat_a.db().get_entry(REL).unwrap().expect("the row");

    let moved = ContentHash::from_digest_raw([0x99; 32]);
    assert!(
        !seat_a
            .apply_refresh_fold(&fold_reporting(head_moved_to(moved, 10)), None)
            .await
            .unwrap()
    );
    let after = seat_a.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(after.state, SyncState::Synced);
    assert_eq!(after.manifest_hash, before.manifest_hash);
    assert_eq!(after.recorded_content_hash, before.recorded_content_hash);
}

/// The File Provider's items say which bodies the gate keeps, so the binding
/// can hold exactly those out of the OS's own eviction: this seat's own
/// record in a metadata-only folder, and no other — not once the folder is
/// full, and never a body fetched from another holder.
#[tokio::test]
async fn the_provider_rows_mark_exactly_the_kept_own_records() {
    use crate::provider_face::ProviderEngine;

    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let key = BackupKey::from_bytes([0x11; 32]);
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(&server.uri(), a_watch.path(), key.clone());

    let kept_of = |rows: Vec<crate::provider_face::ProviderRow>| {
        rows.into_iter()
            .find(|r| r.rel == REL)
            .expect("the row is enumerated")
            .kept
    };
    assert!(
        kept_of(seat_a.provider_rows().await.unwrap()),
        "own record, metadata-only"
    );

    seat_a.install_residency(Some(false));
    assert!(
        !kept_of(seat_a.provider_rows().await.unwrap()),
        "a full folder's nest holds the body"
    );

    // A body this seat fetched: the head another seat wrote, followed by the
    // rule's replace in a metadata-only folder.
    let b_watch = tempfile::tempdir().unwrap();
    let seat_b = test_engine(
        &server.uri(),
        b_watch.path().to_path_buf(),
        Some(key.clone()),
    );
    let v2 = b"v2 - fetched from its holder".to_vec();
    let v2_manifest = seed_file(&seat_b, &store, b_watch.path(), REL, &v2).await;
    let c_watch = tempfile::tempdir().unwrap();
    let seat_c = seat_holding_its_own_record(&server.uri(), c_watch.path(), key);
    assert!(
        seat_c
            .replace_superseded_own_record(&head_moved_to(v2_manifest, v2.len()))
            .await
            .unwrap()
    );
    assert!(
        !kept_of(seat_c.provider_rows().await.unwrap()),
        "a fetched body may be freed"
    );
}

/// A residency flip on a running File Provider host changes which items the
/// gate keeps, and the OS reads an item's keep policy only when told the item
/// changed: the refresh reports exactly the rels whose kept bit the step
/// flipped — this seat's own records, never a body it does not keep either way
/// — and nothing when the step flipped nothing.
#[tokio::test]
async fn a_residency_flip_reports_exactly_the_flipped_kept_rels() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let seat = seat_holding_its_own_record(
        &server.uri(),
        watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    crate::pull_remote_changes_test::record_own_write(&seat, watch.path(), "a.txt", b"own too");
    // A synced body with no record of this seat's: never kept.
    let fetched = b"fetched";
    seat.db()
        .upsert_entry(
            "fetched.txt",
            Some(ContentHash::of_raw(fetched)),
            Some(ContentHash::of_raw(fetched)),
            None,
            SyncState::Synced,
            0,
            0,
            fetched.len() as i64,
            1,
            None,
        )
        .unwrap();

    let ((), to_full) = seat
        .kept_flips_across(async { seat.install_residency(Some(false)) })
        .await;
    assert_eq!(to_full, vec!["a.txt".to_string(), REL.to_string()]);

    let ((), unchanged) = seat
        .kept_flips_across(async { seat.install_residency(Some(false)) })
        .await;
    assert!(unchanged.is_empty(), "no flip, nothing to report");

    let ((), back) = seat
        .kept_flips_across(async { seat.install_residency(Some(true)) })
        .await;
    assert_eq!(back, vec!["a.txt".to_string(), REL.to_string()]);
}

/// The posture read a File Provider host now runs at every refresh keeps the
/// residency it holds when the nest cannot be read — a host whose control
/// plane is down reports no flip and keeps every own record kept.
#[tokio::test]
async fn an_unreadable_posture_read_flips_nothing() {
    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let seat = seat_holding_its_own_record(
        &server.uri(),
        watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );

    let ((), flipped) = seat.kept_flips_across(seat.refresh_sync_mode()).await;
    assert!(flipped.is_empty());
    assert_eq!(seat.kept_own_records().unwrap(), vec![REL.to_string()]);
}

/// The File Provider's `fetchContents` reads the body its staging root already
/// holds before it asks the network: an own record in a metadata-only folder
/// the nest took no byte of, re-opened after the OS let go of its copy, is
/// served from the staged copy with no holder answering.
#[tokio::test]
async fn the_provider_serves_a_recorded_head_its_staging_root_holds() {
    use crate::provider_face::ProviderEngine;

    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(
        &server.uri(),
        a_watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    let head = seat_a
        .db()
        .get_entry(REL)
        .unwrap()
        .expect("the row")
        .recorded_content_hash;

    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("served");
    let served = seat_a
        .fetch_to_path(REL, &dest)
        .await
        .expect("served from the staged copy — no holder holds this manifest");
    assert_eq!(
        Some(served),
        head,
        "the served identity is the recorded head"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), V1);
}

/// A staged copy that is not the recorded head — edited since its record — is
/// never served as that head: the read goes to the network, and with no holder
/// answering it fails with nothing left at the destination.
#[tokio::test]
async fn the_provider_never_serves_a_staged_copy_edited_since_its_record() {
    use crate::provider_face::ProviderEngine;

    let server = MockServer::start().await;
    let _store = MockNest::new().with_blob_plane().mount(&server).await;
    let a_watch = tempfile::tempdir().unwrap();
    let seat_a = seat_holding_its_own_record(
        &server.uri(),
        a_watch.path(),
        BackupKey::from_bytes([0x11; 32]),
    );
    std::fs::write(a_watch.path().join(REL), b"edited since the record").unwrap();

    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("served");
    assert!(seat_a.fetch_to_path(REL, &dest).await.is_err());
    assert!(!dest.exists(), "no partial or wrong body is left behind");
}
