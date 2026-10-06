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
        content_key_version: None,
        remote_mtime: 0,
        version_num: 1,
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
