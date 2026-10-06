//! Ack honesty for the local write verbs: [`SyncEngine::handle_delete`]'s
//! record-first ordering and [`SyncEngine::upload_file`]'s recorded-head-proof
//! skip.
//!
//! Both pin the same invariant from two directions — *a platform surface may be
//! acked only when the change record provably reached the nest, and a failed
//! record must stay retryable*:
//!
//! - **delete**: the row must SURVIVE a failed delete record (record-FIRST,
//!   tombstone-on-success). Tombstoning first erased the only local evidence
//!   that a delete was owed — the File Provider acked the OS, reconcile's
//!   delete-detection (`Synced` rows only) skipped the `Deleted` row, and
//!   `purge_tombstones` removed it: the delete silently never propagated —
//!   the file lived on on the nest and every other device while the deleting
//!   device showed it gone (found in review, 2026-07-19).
//! - **ingest skip**: a `Synced` row is skipped only when
//!   `recorded_content_hash` proves the record landed for exactly these bytes —
//!   then the skip reports `recorded: true` so a retried write whose ack was
//!   lost converges to an ack. A proof-less `Synced` row (the residue of a
//!   failed record — the upload path flips state to `Synced` *before*
//!   recording) re-drives the upload + record instead of skipping forever with
//!   the record never landing.
//!
//! Uses [`pull_remote_changes_test`]'s harness shape: an engine whose clients
//! point at an unreachable URL, so every `record_change` / chunk upload FAILS —
//! exactly the record-failure arm these tests pin. The success arms run over a
//! real nest in `bins/fauna-nest/tests/conformance_file_provider_client.rs`.

use crate::db::SyncState;
use crate::pull_remote_changes_test::{seed_tracked_file, test_engine};

/// A delete whose nest record fails must NOT tombstone the row: the surviving
/// row is what the File Provider retry and reconcile's delete-detection re-drive.
#[tokio::test]
async fn an_unrecorded_delete_keeps_the_row_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "doc.txt", b"hello");

    let outcome = engine
        .handle_delete("doc.txt")
        .await
        .expect("handle_delete");
    assert!(
        !outcome.recorded,
        "the nest is unreachable — the delete record cannot have landed"
    );

    let entry = engine
        .db()
        .get_entry("doc.txt")
        .unwrap()
        .expect("the row must survive a failed delete record");
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "the row keeps its pre-delete state so reconcile's Synced-scan re-detects it"
    );
}

/// Deleting an untracked path is vacuously complete — the retry of a delete
/// whose record already landed (row gone, ack lost) must converge to an ack,
/// not spin forever.
#[tokio::test]
async fn a_delete_of_an_untracked_path_acks_vacuously() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());

    let outcome = engine
        .handle_delete("gone.txt")
        .await
        .expect("handle_delete");
    assert!(
        outcome.recorded,
        "nothing tracked ⇒ nothing owed to the nest"
    );
}

/// An **already-tombstoned** row must not record a second delete on the nest.
///
/// `delete_entry` TOMBSTONES (`state = Deleted`) rather than removing the row,
/// and `get_entry` returns tombstones — so `handle_delete`'s `is_none()` guard
/// never fires for a path this device already deleted. Every spurious delete
/// event for such a path therefore re-recorded the delete on the nest.
///
/// That is not a cosmetic duplicate. A tombstoned row means the delete
/// **provably reached the nest** (record-FIRST: a failed record returns before
/// tombstoning — `an_unrecorded_delete_keeps_the_row_for_retry` above), so
/// there is nothing left owed. Re-recording it re-broadcasts a delete for a
/// path another device may have since RECREATED, erasing the new file — the
/// no-user-data-loss hazard in `docs/goal/principles.md`.
///
/// Observed live 2026-07-24 (macOS multiseat seat, run `macosfix1`): applying
/// 24 remote deletes made the engine re-record all 24 back to the nest.
#[tokio::test]
async fn an_already_tombstoned_delete_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());
    seed_tracked_file(&engine, dir.path(), "doc.txt", b"hello");

    // Tombstone the row the way a landed delete does, and remove the file the
    // way `handle_delete` / an applied remote delete does.
    engine.db().delete_entry("doc.txt").unwrap();
    std::fs::remove_file(dir.path().join("doc.txt")).unwrap();
    assert_eq!(
        engine.db().get_entry("doc.txt").unwrap().unwrap().state,
        SyncState::Deleted,
        "precondition: the row is a tombstone, not absent",
    );

    // The nest is unreachable, so ANY attempt to record would fail and report
    // `recorded: false`. A true ack can only come from the no-work path.
    let outcome = engine
        .handle_delete("doc.txt")
        .await
        .expect("handle_delete");
    assert!(
        outcome.recorded,
        "an already-tombstoned path owes the nest nothing — it must ack without \
         touching the network, not re-record the delete"
    );
}

/// A `Synced` row whose `recorded_content_hash` proves the record landed for
/// exactly the on-disk bytes skips WITHOUT touching the network and reports
/// `recorded: true` — the lost-ack retry converges.
#[tokio::test]
async fn a_proven_synced_skip_acks_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());

    let body = b"proven bytes";
    let full = dir.path().join("proven.txt");
    std::fs::write(&full, body).unwrap();
    let hash = fauna_core::data::ContentHash::of_raw(body);
    let mtime = 1_700_000_000i64;
    engine
        .db()
        .upsert_entry(
            "proven.txt",
            Some(hash),
            Some(hash),
            Some(hash), // manifest_hash — the recorded head
            SyncState::Synced,
            mtime,
            mtime,
            body.len() as i64,
            1,
            None,
        )
        .unwrap();
    // The record landed for these bytes: stamp the proof the skip gate reads.
    engine
        .db()
        .stamp_recorded_content_from_local("proven.txt", crate::db::ProofOrigin::OwnRecord)
        .unwrap();

    // The nest is unreachable, so an Ok can only come from the skip path —
    // and it must report recorded, not the old always-false skip.
    let outcome = engine.upload_file("proven.txt").await.expect("upload_file");
    assert!(
        outcome.recorded,
        "an honest skip of an already-recorded head must ack (lost-ack retry convergence)"
    );
}

/// A `Synced` row WITHOUT the recorded-head proof is the residue of a failed
/// record: it must NOT skip — the retry re-drives the upload + record (which
/// here fails loudly against the unreachable nest, proving the network path
/// was taken).
#[tokio::test]
async fn an_unproven_synced_row_is_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let engine = test_engine(dir.path().to_path_buf());

    let body = b"unproven bytes";
    let full = dir.path().join("unproven.txt");
    std::fs::write(&full, body).unwrap();
    let hash = fauna_core::data::ContentHash::of_raw(body);
    let mtime = 1_700_000_000i64;
    engine
        .db()
        .upsert_entry(
            "unproven.txt",
            Some(hash),
            Some(hash),
            Some(hash),
            SyncState::Synced,
            mtime,
            mtime,
            body.len() as i64,
            1,
            None,
        )
        .unwrap();
    // No recorded_content_hash stamp — the record never landed.

    let result = engine.upload_file("unproven.txt").await;
    assert!(
        result.is_err(),
        "a proof-less Synced row must re-drive the upload (which fails against the \
         unreachable nest) instead of skipping: got {result:?}"
    );
}
