//! **A restore's local apply on an always-resident root** (`file-versions.md`
//! § Restore, ordering 2's kept-copy branch): every file on such a root is a
//! copy this device keeps, so the restored version is written in over the copy
//! and only then does the row move — `Synced` at the restored head, signed as
//! the identity the restore recorded as, nothing uploaded. A copy carrying an
//! unsynced local edit is left as it is, and the edit uploads.
//!
//! Driven through the resident loop's own command arm
//! ([`answer_engine_command`]), the door the agent's restore verb reaches.
//! Lives under `src/` (gated `#[cfg(test)]` in `lib.rs`), beside
//! `superseded_own_record_test.rs`, whose replace-in-place road this shares.

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use wiremock::MockServer;

use crate::always_resident::{EngineCommand, answer_engine_command};
use crate::backfill_thumbnail_test::{seed_file, test_engine};
use crate::db::SyncState;
use crate::engine::{StaleHydratedRow, SyncEngine};
use crate::test_support::BlobStore;

const REL: &str = "notes.txt";
const V1: &[u8] = b"v1 - the version the user restores";
const V2: &[u8] = b"v2 - the version the restore replaces, longer than the first";
/// The identity the restore recorded as.
const SIGNER: [u8; 32] = [0x5A; 32];

/// The head a restore of `manifest` moved `REL` to, as the agent hands it over.
fn restored_to(engine: &SyncEngine, manifest: ContentHash, size: usize) -> StaleHydratedRow {
    let entry = engine.db().get_entry(REL).unwrap().expect("the row");
    StaleHydratedRow {
        relative_path: REL.to_string(),
        manifest_hash: manifest,
        size_bytes: size as i64,
        content_key_version: None,
        remote_mtime: 1_700_000_000,
        version_num: entry.version_num,
        signed_as: Some(SIGNER),
    }
}

/// Send `row` to the resident root's command arm and return its answer.
async fn apply(engine: &SyncEngine, row: StaleHydratedRow) -> anyhow::Result<bool> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    answer_engine_command(engine, EngineCommand::ApplyStaleHydrated { row, reply }).await;
    answer.await.expect("the arm replies")
}

fn manifests_posted(store: &BlobStore) -> usize {
    store.manifests.lock().unwrap().len()
}

/// A resident root holding `V2`, recorded after `V1`: the state a restore of
/// `V1` starts from. Both versions' bodies are put on the byte plane by a
/// seeding seat (its record never lands here — the test client has no control
/// plane), and the root's row is committed at `V2`'s head the way a landed
/// record commits it. Returns `V1`'s and `V2`'s manifests.
async fn root_at_v2(
    engine: &SyncEngine,
    store: &BlobStore,
    server_uri: &str,
    watch: &std::path::Path,
) -> (ContentHash, ContentHash) {
    let seed_watch = tempfile::tempdir().unwrap();
    let seeder = test_engine(
        server_uri,
        seed_watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    let m1 = seed_file(&seeder, store, seed_watch.path(), REL, V1).await;
    let m2 = seed_file(&seeder, store, seed_watch.path(), REL, V2).await;

    std::fs::write(watch.join(REL), V2).unwrap();
    let content = ContentHash::of_raw(V2);
    engine
        .db()
        .upsert_entry(
            REL,
            Some(content),
            Some(content),
            None,
            SyncState::Synced,
            0,
            0,
            V2.len() as i64,
            1,
            None,
        )
        .unwrap();
    engine
        .commit_recorded_head(REL, m2, V2.len() as i64, None)
        .expect("commit recorded head");
    let entry = engine.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(entry.state, SyncState::Synced, "the premise: a synced copy");
    assert_eq!(
        entry.manifest_hash,
        Some(m2),
        "the premise: the row is at V2"
    );
    assert_eq!(
        entry.recorded_content_hash,
        Some(content),
        "the premise: the row proves V2 recorded"
    );
    (m1, m2)
}

#[tokio::test]
async fn a_restore_writes_the_restored_version_over_the_copy() {
    let server = MockServer::start().await;
    let store = crate::test_support::MockNest::new()
        .with_blob_plane()
        .mount(&server)
        .await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    let (m1, _) = root_at_v2(&engine, &store, &server.uri(), watch.path()).await;
    let posted = manifests_posted(&store);

    let replaced = apply(&engine, restored_to(&engine, m1, V1.len()))
        .await
        .expect("the resident root answers the restore");
    assert!(replaced, "the copy now stands at the restored head");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        V1,
        "the restored version is on the disk"
    );
    let entry = engine.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(entry.state, SyncState::Synced, "the file stays on the disk");
    assert_eq!(
        entry.manifest_hash,
        Some(m1),
        "the row is at the restored head"
    );
    assert_eq!(
        entry.recorded_content_hash,
        Some(ContentHash::of_raw(V1)),
        "the row proves the bytes on the disk are the restored head's"
    );
    assert_eq!(
        entry.head_signed_as,
        Some(SIGNER),
        "the head is signed as the identity the restore recorded as"
    );

    // The restored bytes are no edit: nothing is recorded over the restore.
    engine.upload_file(REL).await.expect("the uploader's pass");
    assert_eq!(
        manifests_posted(&store),
        posted,
        "nothing was uploaded — the restore is metadata-only"
    );

    // A second restore of the same version finds the copy already there.
    assert!(
        apply(&engine, restored_to(&engine, m1, V1.len()))
            .await
            .expect("the resident root answers again"),
        "a copy already at the restored head stands there"
    );
    assert_eq!(std::fs::read(watch.path().join(REL)).unwrap(), V1);
}

#[tokio::test]
async fn a_copy_carrying_an_unsynced_edit_keeps_it_and_uploads_it() {
    let server = MockServer::start().await;
    let store = crate::test_support::MockNest::new()
        .with_blob_plane()
        .mount(&server)
        .await;
    let watch = tempfile::tempdir().unwrap();
    let engine = test_engine(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(BackupKey::from_bytes([0x11; 32])),
    );
    let (m1, m2) = root_at_v2(&engine, &store, &server.uri(), watch.path()).await;
    // An edit the uploader has not recorded yet.
    let edit: &[u8] = b"edited on this device, not yet uploaded";
    std::fs::write(watch.path().join(REL), edit).unwrap();

    let replaced = apply(&engine, restored_to(&engine, m1, V1.len()))
        .await
        .expect("a refusal is an answer, not an error");
    assert!(!replaced, "the copy carrying the edit is left as it is");

    assert_eq!(
        std::fs::read(watch.path().join(REL)).unwrap(),
        edit,
        "the edit survives"
    );
    let entry = engine.db().get_entry(REL).unwrap().expect("the row");
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(
        entry.manifest_hash,
        Some(m2),
        "the row stays where it was, so the edit reads as an edit"
    );

    let posted = manifests_posted(&store);
    engine.upload_file(REL).await.expect("the edit uploads");
    assert_eq!(
        manifests_posted(&store),
        posted + 1,
        "the edit is uploaded, a conflict with the restore for its readers to judge"
    );
    assert_eq!(
        engine
            .db()
            .get_entry(REL)
            .unwrap()
            .expect("the row")
            .local_hash,
        Some(ContentHash::of_raw(edit))
    );
}
