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
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::always_resident::{EngineCommand, answer_engine_command};
use crate::backfill_thumbnail_test::{seed_file, test_engine};
use crate::db::{OwedRestore, SyncState};
use crate::engine::{StaleHydratedRow, SyncEngine, listed_head};
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
        content_stamp: None,
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

/// A resident root at `V2`, with the versions a restore chooses between.
struct Root {
    /// Keeps the byte plane answering for as long as the root stands.
    _server: MockServer,
    engine: SyncEngine,
    store: BlobStore,
    watch: tempfile::TempDir,
    m1: ContentHash,
    m2: ContentHash,
}

impl Root {
    async fn at_v2() -> Self {
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
        Self {
            _server: server,
            engine,
            store,
            watch,
            m1,
            m2,
        }
    }

    /// A resident root that serves the folder and holds neither a file nor a row at
    /// `REL`, with `V1` and `V2` on the byte plane.
    async fn with_no_file() -> Self {
        let root = Self::at_v2().await;
        std::fs::remove_file(root.watch.path().join(REL)).unwrap();
        root.engine.db().remove_entry(REL).unwrap();
        root
    }

    fn on_disk(&self) -> bool {
        self.watch.path().join(REL).exists()
    }

    fn disk(&self) -> Vec<u8> {
        std::fs::read(self.watch.path().join(REL)).unwrap()
    }

    fn row(&self) -> crate::db::SyncEntry {
        self.engine.db().get_entry(REL).unwrap().expect("the row")
    }
}

/// Every manifest POST so far, a re-post of a known manifest included.
fn manifest_posts(store: &BlobStore) -> usize {
    store.posted_manifests.lock().unwrap().len()
}

fn now() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// The intent the restore verb writes ahead of recording `manifest`.
fn owe(engine: &SyncEngine, manifest: ContentHash, owed_at: i64) {
    engine
        .db()
        .put_owed_restore(&OwedRestore {
            path: REL.to_string(),
            manifest_hash: manifest,
            size_bytes: V1.len() as i64,
            content_key_version: None,
            signed_as: Some(SIGNER),
            owed_at,
        })
        .unwrap();
}

fn owed(engine: &SyncEngine) -> Vec<OwedRestore> {
    engine.db().owed_restores().unwrap()
}

/// This device's own row as the pull lists it. A restore's record carries neither
/// stamp, `derived_through` nor `is_resolution`.
fn own_row(seq: i64, manifest: ContentHash) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{REL}"),
        manifest_hash: Some(hex::encode(manifest.digest())),
        change_type: "modify".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(REL.to_string()),
        device_id: Some(hex::encode([0u8; 32])),
        ..Default::default()
    }
}

/// A row this root's engine wrote: every engine record carries the anchor it derived
/// from, which is what tells it from a row another door recorded under this device's id.
fn engine_row(seq: i64, manifest: ContentHash) -> SyncChange {
    SyncChange {
        derived_through: Some(seq - 1),
        ..own_row(seq, manifest)
    }
}

/// The root's own history reaches it: `V1` at seq 5, `V2` at seq 6. The ledger now
/// holds the version a restore picks, and the merge base is the copy on the disk.
async fn echo_history(engine: &SyncEngine, m1: ContentHash, m2: ContentHash) {
    engine
        .apply_remote_changes(&[engine_row(5, m1), engine_row(6, m2)], 4)
        .await
        .expect("the history's echoes");
    assert!(
        engine
            .causal()
            .holds_content_at_or_below(REL, 6, &ContentHash::of_raw(V1)),
        "the premise: the ledger holds the version the restore picks"
    );
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
    owe(&engine, m1, now());

    let replaced = apply(&engine, restored_to(&engine, m1, V1.len()))
        .await
        .expect("the resident root answers the restore");
    assert!(replaced, "the copy now stands at the restored head");
    assert!(owed(&engine).is_empty(), "the hand-off settles the intent");

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
    owe(&engine, m1, now());

    let replaced = apply(&engine, restored_to(&engine, m1, V1.len()))
        .await
        .expect("a refusal is an answer, not an error");
    assert!(!replaced, "the copy carrying the edit is left as it is");
    assert!(
        owed(&engine).is_empty(),
        "the kept edit settles the intent too: it uploads as a conflict with the restore"
    );

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

/// **The window the intent exists for:** a restore's record landed and nothing ever
/// applied it. Its own echo reaches a root whose ledger holds the restored bytes at a
/// lower seq — a reissue of held content, later than local, over an unmoved base — and
/// that shape is the declined own tail the self-echo re-asserts, by a FORCED upload of
/// the replaced copy: the restore would be recorded away on every device. While the
/// intent is owed the echo only accounts, and the drain applies.
#[tokio::test]
async fn an_owed_restore_is_applied_and_its_own_echo_does_not_undo_it() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let (m1, m2) = (root.m1, root.m2);
    echo_history(engine, m1, m2).await;
    owe(engine, m1, now());
    let posted = manifest_posts(&root.store);

    engine
        .apply_remote_changes(&[own_row(7, m1)], 6)
        .await
        .expect("the restore's echo");

    assert_eq!(
        manifest_posts(&root.store),
        posted,
        "the replaced copy is not re-asserted over the restore"
    );
    assert_eq!(
        engine.causal().frontier(REL),
        Some(7),
        "the echo is still accounted"
    );
    assert_eq!(root.disk(), V2, "the echo leaves the apply to the drain");
    assert_eq!(
        owed(engine)[0].signed_as,
        Some(SIGNER),
        "the verb's intent stands: its echo writes no other"
    );

    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .expect("the drain");
    assert_eq!(drain.applied, 1);

    assert_eq!(root.disk(), V1, "the restored version is on the disk");
    let entry = root.row();
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(entry.manifest_hash, Some(m1));
    assert_eq!(entry.head_signed_as, Some(SIGNER));
    assert!(owed(engine).is_empty(), "the intent died in the replace");

    engine.upload_file(REL).await.expect("the uploader's pass");
    assert_eq!(
        manifest_posts(&root.store),
        posted,
        "nothing was uploaded — the restore is metadata-only"
    );
}

/// **The two doors that write no intent.** Media's restore and the review list's *use
/// the other version* record from the app under this device's id and hand this root
/// nothing, so their echo is the first it hears of them, and a ledger holding the
/// restored bytes reads it as the declined own tail above. The row carries no anchor,
/// which no engine's record of a new head does, so the echo owes the restore itself and
/// the drain applies it, the verb's own road.
#[tokio::test]
async fn another_doors_restore_is_owed_by_its_echo_and_not_recorded_over() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let (m1, m2) = (root.m1, root.m2);
    echo_history(engine, m1, m2).await;
    let posted = manifest_posts(&root.store);

    engine
        .apply_remote_changes(&[own_row(7, m1)], 6)
        .await
        .expect("the door's echo");

    assert_eq!(
        manifest_posts(&root.store),
        posted,
        "the replaced copy is not re-asserted over the restore"
    );
    assert_eq!(root.disk(), V2, "the echo leaves the apply to the drain");
    let intents = owed(engine);
    assert_eq!(intents.len(), 1, "the echo owes the restore");
    assert_eq!(intents[0].manifest_hash, m1);
    assert_eq!(intents[0].size_bytes, V1.len() as i64);

    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .expect("the drain");
    assert_eq!(drain.applied, 1);

    assert_eq!(root.disk(), V1, "the restored version is on the disk");
    let entry = root.row();
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(entry.manifest_hash, Some(m1));
    assert!(
        entry.head_signed_as.is_some(),
        "the head is signed as the identity the door recorded as"
    );
    assert!(owed(engine).is_empty(), "the intent died in the replace");

    engine.upload_file(REL).await.expect("the uploader's pass");
    assert_eq!(
        manifest_posts(&root.store),
        posted,
        "nothing was uploaded — the restore is metadata-only"
    );
}

/// The ledger need not hold the restored bytes for the file to be owed: without the hold
/// the echo reads as an own edit, not a declined tail, and it left the replaced copy on
/// the disk for good, on the very device the user restored from.
#[tokio::test]
async fn another_doors_restore_of_bytes_the_ledger_never_held_reaches_the_disk() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let m1 = root.m1;

    engine
        .apply_remote_changes(&[own_row(7, m1)], 6)
        .await
        .expect("the door's echo");
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .expect("the drain");

    assert_eq!(drain.applied, 1);
    assert_eq!(root.disk(), V1, "the restored version is on the disk");
    assert_eq!(root.row().manifest_hash, Some(m1));
}

/// A later change moved past the door's restore: the echo owes it all the same, and the
/// drain, which writes only the nest's current head, leaves the disk alone. This is what
/// the refused own-echo heuristic lacked (`file-versions.md` § Restore, ordering 1).
#[tokio::test]
async fn a_doors_restore_a_later_change_moved_past_writes_nothing() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let m1 = root.m1;

    engine
        .apply_remote_changes(&[own_row(7, m1)], 6)
        .await
        .expect("the door's echo");
    let third = ContentHash::of_raw(b"a third manifest");
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(third)))
        .await
        .unwrap();

    assert_eq!((drain.applied, drain.kept), (0, 1), "inside the grace");
    assert_eq!(root.disk(), V2);
    assert_eq!(root.row().manifest_hash, Some(root.m2));
}

/// An unsynced edit survives another door's restore as it survives the verb's, and
/// uploads as a conflict with it.
#[tokio::test]
async fn a_doors_restore_keeps_an_unsynced_edit() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let m1 = root.m1;
    let edit: &[u8] = b"edited on this device, not yet uploaded";
    std::fs::write(root.watch.path().join(REL), edit).unwrap();

    engine
        .apply_remote_changes(&[own_row(7, m1)], 6)
        .await
        .expect("the door's echo");
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .unwrap();

    assert_eq!((drain.applied, drain.dropped), (0, 1));
    assert!(owed(engine).is_empty(), "the kept edit settles the intent");
    assert_eq!(root.disk(), edit, "the edit survives");
    assert_eq!(root.row().manifest_hash, Some(root.m2));
}

/// An engine's own older upload, its echo arriving after the file moved on: the row
/// carries its anchor, so it owes nothing. Reading this shape as a restore is what would
/// write an old version over a newer one.
#[tokio::test]
async fn an_engines_late_echo_owes_nothing() {
    let root = Root::at_v2().await;
    let engine = &root.engine;

    engine
        .apply_remote_changes(&[engine_row(5, root.m1)], 4)
        .await
        .expect("the late echo");

    assert!(owed(engine).is_empty(), "an engine's row owes no restore");
    assert_eq!(root.disk(), V2);
}

/// The thumbnail backfill is the one engine record with no anchor, and it re-records the
/// head its row already stands at, so there is nothing to restore: an edit made since
/// keeps uploading as an edit.
#[tokio::test]
async fn a_thumbnail_re_record_of_the_rows_own_head_owes_nothing() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let edit: &[u8] = b"edited after the re-record";
    std::fs::write(root.watch.path().join(REL), edit).unwrap();

    engine
        .apply_remote_changes(&[own_row(7, root.m2)], 6)
        .await
        .expect("the re-record's echo");

    assert!(owed(engine).is_empty(), "the row stands at this head");
    assert_eq!(root.disk(), edit);
}

/// The record never landed: the head is not the restored version. A young intent may
/// still be in flight and is kept; past the grace it is dropped. The disk stays.
#[tokio::test]
async fn an_intent_whose_record_never_landed_is_kept_through_the_grace_then_dropped() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let (m1, m2) = (root.m1, root.m2);

    owe(engine, m1, now());
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m2)))
        .await
        .unwrap();
    assert_eq!(
        (drain.kept, drain.dropped),
        (1, 0),
        "the record may be in flight"
    );
    assert_eq!(owed(engine).len(), 1);

    owe(engine, m1, now() - 11 * 60);
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m2)))
        .await
        .unwrap();
    assert_eq!((drain.kept, drain.dropped), (0, 1));
    assert!(owed(engine).is_empty());
    assert_eq!(root.disk(), V2);
}

/// Another change has moved past the restore: it arrives by the pull like any other,
/// so the intent is dropped without touching the disk.
#[tokio::test]
async fn an_intent_the_head_has_moved_past_is_dropped() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    owe(engine, root.m1, now() - 11 * 60);

    let third = ContentHash::of_raw(b"a third manifest");
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(third)))
        .await
        .unwrap();

    assert_eq!(drain.dropped, 1);
    assert!(owed(engine).is_empty());
    assert_eq!(root.disk(), V2);
    assert_eq!(root.row().manifest_hash, Some(root.m2));
}

/// A delete past the restore: tombstones are not listed, so the head still reads as the
/// restored version, but the pull that applied the delete left a tombstone row, or no
/// row at all, and neither is a copy to write over.
#[tokio::test]
async fn a_delete_past_the_restore_leaves_nothing_to_write_over() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let m1 = root.m1;
    std::fs::remove_file(root.watch.path().join(REL)).unwrap();
    engine.db().delete_entry(REL).unwrap();

    owe(engine, m1, now());
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .unwrap();
    assert_eq!((drain.applied, drain.dropped), (0, 1), "a tombstone row");
    assert!(owed(engine).is_empty());

    engine.db().remove_entry(REL).unwrap();
    owe(engine, m1, now());
    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .unwrap();
    assert_eq!((drain.applied, drain.dropped), (0, 1), "no row");
    assert!(owed(engine).is_empty());

    assert!(
        !root.watch.path().join(REL).exists(),
        "the deleted file is not written back"
    );
}

/// A delete past the restore in one pull: the same two rows, read in seq order, are the
/// other ruling's case. The path's batch-latest change decides, so the door's row writes
/// nothing and the tombstone finds nothing to remove.
#[tokio::test]
async fn a_delete_past_a_doors_record_in_one_batch_writes_nothing() {
    let root = Root::with_no_file().await;
    let delete = SyncChange {
        change_type: "delete".to_string(),
        manifest_hash: None,
        ..own_row(8, root.m1)
    };

    root.engine
        .apply_remote_changes(&[own_row(7, root.m1), delete], 6)
        .await
        .expect("the batch");

    assert!(!root.on_disk(), "the delete is the path's latest change");
}

/// **A Media upload of a new path** (`file-versions.md` § Restore, ordering 1): the record
/// is this device's own and names a path this root holds neither a file nor a row for, so
/// no echo arm had anything to work on and the anchor moved past it. The file appeared on
/// every other device and never on the one that uploaded it.
#[tokio::test]
async fn a_doors_upload_of_a_new_path_reaches_the_disk() {
    let root = Root::with_no_file().await;
    let posted = manifest_posts(&root.store);
    let upload = SyncChange {
        change_type: "create".to_string(),
        ..own_row(7, root.m1)
    };

    root.engine
        .apply_remote_changes(&[upload], 6)
        .await
        .expect("the door's echo");

    assert!(root.on_disk(), "the uploaded file is on this device's disk");
    assert_eq!(root.disk(), V1);
    let entry = root.row();
    assert_eq!(entry.state, SyncState::Synced);
    assert_eq!(entry.manifest_hash, Some(root.m1));
    assert!(
        owed(&root.engine).is_empty(),
        "nothing is left to the drain"
    );

    root.engine
        .upload_file(REL)
        .await
        .expect("the uploader's pass");
    assert_eq!(
        manifest_posts(&root.store),
        posted,
        "the file came from the record, so nothing is recorded back"
    );
}

/// **A restore past a delete made here:** it brings the file back, as it does on every
/// other device. The version restored is often the very head the delete left on the
/// tombstone row, so a row at this manifest says nothing while it is a tombstone.
#[tokio::test]
async fn a_doors_restore_of_a_file_deleted_here_brings_it_back() {
    for restored_head in [false, true] {
        let root = Root::at_v2().await;
        let manifest = if restored_head { root.m2 } else { root.m1 };
        let expected = if restored_head { V2 } else { V1 };
        std::fs::remove_file(root.watch.path().join(REL)).unwrap();
        root.engine.db().delete_entry(REL).unwrap();

        root.engine
            .apply_remote_changes(&[own_row(7, manifest)], 6)
            .await
            .expect("the door's echo");

        assert!(root.on_disk(), "the restored file is back");
        assert_eq!(root.disk(), expected);
        let entry = root.row();
        assert_eq!(entry.state, SyncState::Synced, "the row is live again");
        assert_eq!(entry.manifest_hash, Some(manifest));
    }
}

/// Several door records of one absent path in one pull: only the last is the path's
/// state.
#[tokio::test]
async fn the_last_of_a_doors_records_is_the_one_written() {
    let root = Root::with_no_file().await;

    root.engine
        .apply_remote_changes(&[own_row(7, root.m1), own_row(8, root.m2)], 6)
        .await
        .expect("the door's echoes");

    assert!(root.on_disk());
    assert_eq!(root.disk(), V2);
    assert_eq!(root.row().manifest_hash, Some(root.m2));
}

/// An engine's row over a missing file is no door's: the engine removed the file itself,
/// and its tombstone is on the way. Writing here would bring a deleted file back.
#[tokio::test]
async fn an_engines_echo_over_a_missing_file_writes_nothing() {
    let root = Root::with_no_file().await;

    root.engine
        .apply_remote_changes(&[engine_row(7, root.m1)], 6)
        .await
        .expect("the late echo");

    assert!(!root.on_disk());
}

/// The thumbnail backfill is the one engine record with no anchor. It names the head the
/// live row already stands at, so a missing file there is a delete the engine has yet to
/// record, not a file this device is owed.
#[tokio::test]
async fn a_thumbnail_re_record_over_a_file_just_deleted_writes_nothing() {
    let root = Root::at_v2().await;
    std::fs::remove_file(root.watch.path().join(REL)).unwrap();

    root.engine
        .apply_remote_changes(&[own_row(7, root.m2)], 6)
        .await
        .expect("the re-record's echo");

    assert!(!root.on_disk());
}

/// The door's record over a missing file is the only delivery the file gets, so a fetch
/// that fails must not lose it: the anchor holds below the row, as for a peer's.
#[tokio::test]
async fn a_failed_fetch_of_a_doors_record_holds_the_anchor() {
    let root = Root::with_no_file().await;
    root._server.reset().await;

    let batch = root
        .engine
        .apply_remote_changes(&[own_row(7, root.m1)], 6)
        .await
        .expect("the pass itself stands");

    assert!(batch.deferred, "the pull retries the row");
    assert!(!root.on_disk());
}

/// The head is the newest listed version of the path. The listing runs oldest→newest
/// and a by-name scope may interleave other paths.
#[test]
fn the_listed_head_is_the_newest_version_of_the_path() {
    use fauna_protocol::files::{FileVersionInfo, FilesVersionsListReply};

    let version = |path: &str, manifest: &ContentHash| FileVersionInfo {
        path_hash: fauna_core::sync::path_hash(path).to_vec().into(),
        manifest_hash: manifest.digest().to_vec().into(),
        ..Default::default()
    };
    let listing = |versions| FilesVersionsListReply {
        versions,
        signer_certs: Vec::new(),
        extra: Default::default(),
    };
    let (older, newer, other) = (
        ContentHash::of_raw(V1),
        ContentHash::of_raw(V2),
        ContentHash::of_raw(b"another path's version"),
    );

    let reply = listing(vec![
        version(REL, &older),
        version(REL, &newer),
        version("another.txt", &other),
    ]);
    assert_eq!(listed_head(&reply, REL).unwrap(), Some(newer));
    assert_eq!(
        listed_head(&reply, "unlisted.txt").unwrap(),
        None,
        "nothing listed for the path"
    );

    let mut malformed = version(REL, &newer);
    malformed.manifest_hash = vec![0u8; 3].into();
    assert!(
        listed_head(&listing(vec![malformed]), REL).is_err(),
        "a malformed head is unreadable, never a head"
    );
}

/// A crash between the row move and the signer stamp: the row already stands at the
/// restored head, short of the restore's signer. The drain only stamps it.
#[tokio::test]
async fn a_row_already_at_the_restored_head_takes_only_the_stamp() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let m1 = root.m1;
    std::fs::write(root.watch.path().join(REL), V1).unwrap();
    let content = ContentHash::of_raw(V1);
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
            V1.len() as i64,
            1,
            None,
        )
        .unwrap();
    engine
        .commit_recorded_head(REL, m1, V1.len() as i64, None)
        .expect("commit recorded head");
    assert_ne!(
        root.row().head_signed_as,
        Some(SIGNER),
        "the premise: the crash left the row short of the restore's signer"
    );
    owe(engine, m1, now());
    let posted = manifest_posts(&root.store);

    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .unwrap();

    assert_eq!(drain.applied, 1);
    assert_eq!(root.row().head_signed_as, Some(SIGNER));
    assert!(owed(engine).is_empty());
    assert_eq!(root.disk(), V1);
    assert_eq!(manifest_posts(&root.store), posted);
}

/// A copy carrying an unsynced edit refuses the replace: nothing is written, and the
/// intent dies there — the edit uploads as a conflict with the restore.
#[tokio::test]
async fn a_kept_edit_refuses_the_drain_and_drops_the_intent() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    let m1 = root.m1;
    let edit: &[u8] = b"edited on this device, not yet uploaded";
    std::fs::write(root.watch.path().join(REL), edit).unwrap();
    owe(engine, m1, now());

    let drain = engine
        .drain_owed_restores_with(async |_: &str| Ok(Some(m1)))
        .await
        .unwrap();

    assert_eq!((drain.applied, drain.dropped), (0, 1));
    assert_eq!(root.disk(), edit);
    assert_eq!(root.row().manifest_hash, Some(root.m2));
    assert!(owed(engine).is_empty());
}

/// A head that cannot be read says nothing: the intent waits for the next tick.
#[tokio::test]
async fn an_unreadable_head_keeps_the_intent() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    owe(engine, root.m1, now() - 11 * 60);

    let drain = engine
        .drain_owed_restores_with(async |_: &str| Err(anyhow::anyhow!("the nest is away")))
        .await
        .unwrap();

    assert_eq!(drain.kept, 1);
    assert_eq!(owed(engine).len(), 1);
}

/// The resident loop drains at start: its command arm is reached only past the start
/// passes, so the answer to a command queued ahead of the loop is the barrier. The
/// foreign-routed drop needs no nest, which keeps the observable the intent itself.
#[tokio::test]
async fn the_resident_loop_drains_what_is_owed_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _progress) = crate::pull_remote_changes_test::test_engine_folderless_with_progress(
        dir.path().to_path_buf(),
    );
    engine.set_foreign_routing("https://home.example".to_string(), "aa".repeat(32));
    owe(&engine, ContentHash::of_raw(V1), now());
    // `SyncEngine` is `!Sync`, and the loop's future is `!Send`: raced in-task, so
    // this `Arc` never crosses a thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = std::sync::Arc::new(engine);

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(1);
    let (reply, answer) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(EngineCommand::ApplyStaleHydrated {
            row: StaleHydratedRow {
                relative_path: "another.txt".to_string(),
                manifest_hash: ContentHash::of_raw(V2),
                size_bytes: 0,
                content_stamp: None,
                remote_mtime: 0,
                version_num: 0,
                signed_as: None,
            },
            reply,
        })
        .await
        .expect("the channel buffers the command before the loop runs");
    let looped = crate::always_resident::run_watch_loop(
        std::sync::Arc::clone(&engine),
        dir.path().to_path_buf(),
        "resident".to_string(),
        std::time::Duration::from_secs(3600),
        None,
        Some(cmd_rx),
    );
    tokio::pin!(looped);

    // Generous ceiling, deadline-style: green pays ~nothing.
    tokio::select! {
        _ = &mut looped => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(std::time::Duration::from_secs(60), answer) => {
            got.expect("the loop reaches its command arm")
                .expect("the arm replies")
                .expect("the arm's answer");
        }
    }

    assert!(
        owed(&engine).is_empty(),
        "the start passes drained the intent"
    );
}

/// A nudge's pull is how another door's echo arrives, and the device that restored is
/// waiting on the file: the nudge arm drains, rather than leaving it to the rescan tick.
/// The start passes are behind a command's answer, so only the nudge can drain what is
/// owed after it.
#[tokio::test]
async fn the_resident_loop_drains_what_is_owed_on_a_nudge() {
    let dir = tempfile::tempdir().unwrap();
    let (engine, _progress) = crate::pull_remote_changes_test::test_engine_folderless_with_progress(
        dir.path().to_path_buf(),
    );
    engine.set_foreign_routing("https://home.example".to_string(), "aa".repeat(32));
    // `SyncEngine` is `!Sync`, and the loop's future is `!Send`: raced in-task, so
    // this `Arc` never crosses a thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = std::sync::Arc::new(engine);

    let (wake_tx, wake_rx) = tokio::sync::mpsc::channel(1);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(1);
    let (reply, answer) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(EngineCommand::ApplyStaleHydrated {
            row: StaleHydratedRow {
                relative_path: "another.txt".to_string(),
                manifest_hash: ContentHash::of_raw(V2),
                size_bytes: 0,
                content_stamp: None,
                remote_mtime: 0,
                version_num: 0,
                signed_as: None,
            },
            reply,
        })
        .await
        .expect("the channel buffers the command before the loop runs");
    let looped = crate::always_resident::run_watch_loop(
        std::sync::Arc::clone(&engine),
        dir.path().to_path_buf(),
        "resident".to_string(),
        std::time::Duration::from_secs(3600),
        Some(wake_rx),
        Some(cmd_rx),
    );
    tokio::pin!(looped);

    // Generous ceilings, deadline-style: green pays ~nothing.
    tokio::select! {
        _ = &mut looped => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(std::time::Duration::from_secs(60), answer) => {
            got.expect("the loop reaches its command arm")
                .expect("the arm replies")
                .expect("the arm's answer");
        }
    }

    owe(&engine, ContentHash::of_raw(V1), now());
    wake_tx.send(()).await.expect("the loop holds the receiver");
    let drained = async {
        while !owed(&engine).is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        _ = &mut looped => panic!("the watch loop must not exit"),
        got = tokio::time::timeout(std::time::Duration::from_secs(60), drained) => {
            got.expect("the nudge drained the intent");
        }
    }
}

/// No restore verb reaches a foreign set, so an intent there drops with a warning and
/// nothing asks the nest.
#[tokio::test]
async fn a_foreign_routed_engine_drops_every_intent() {
    let root = Root::at_v2().await;
    let engine = &root.engine;
    engine.set_foreign_routing("https://home.example".to_string(), "aa".repeat(32));
    owe(engine, root.m1, now());

    let drain = engine.drain_owed_restores().await.expect("the drain");

    assert_eq!(drain.dropped, 1);
    assert!(owed(engine).is_empty());
}
