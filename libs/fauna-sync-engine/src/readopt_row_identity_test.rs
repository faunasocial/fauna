//! The author-blind re-adopt arm must leave the row agreeing with the disk.
//!
//! `apply_self_echo`'s supersession arm (`conflicts.md` § Concurrent
//! resolution, clause 5, receiver rule 3: a device's OWN covering resolution
//! whose echo lands after local moved to an earlier-seq resolution re-adopts)
//! rewrites the file to the echoed bytes and refreshes the merge base. It used
//! to stop there: the entry row kept the PREVIOUS content's `local_hash`, so
//! the row said one thing and the disk another.
//!
//! The delete arm reads exactly that pair (`delete-propagation.md` — a
//! tombstone applies only on a settled row whose disk still hashes to its
//! `local_hash`), so the seat that had just converged declined the next
//! genuine delete as "locally modified content", filed an edit-wins
//! delete-vs-edit report, and that report recreated the file on every seat —
//! the deleting one included. Measured live by
//! `test_filesync_seats.py::test_seats_converge[3seat-native+native+native]`
//! (2026-09-22b and 2026-09-24): the seat whose last act was
//! `self-echo: own covering resolution re-adopted` logged
//! `declined a remote delete for locally modified content` two seconds later.
//!
//! The test drives the engine's own paths to the re-adopt (no private state is
//! poked): the seat publishes two versions, echoes the older as an edit, then
//! echoes the newer as its own covering resolution — the arm fires — and a
//! peer's delete follows.

use std::sync::Arc;

use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::format::FormatRegistry;
use fauna_core::identity::ActorKeypair;
use fauna_nest_http::{BearerSource, StaticBearer};
use fauna_protocol::sync::SyncChange;
use wiremock::MockServer;

use crate::adaptive::AdaptiveConcurrency;
use crate::db::{SyncDb, SyncState};
use crate::engine::SyncEngine;
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::test_support::MockNest;
use crate::transfer::TransferPool;

const OUR_DEVICE: [u8; 32] = [0u8; 32];
const REL: &str = "anchor.txt";
/// The permutation this seat folded first (its own order).
const EARLIER: &[u8] = b"same-anchor file\nc: appended\na: appended\nb: appended\n";
/// The log-tail permutation it re-adopts when its own resolution echoes.
const LOG_TAIL: &[u8] = b"same-anchor file\nc: appended\nb: appended\na: appended\n";

fn engine(server_uri: &str, watch_dir: std::path::PathBuf) -> SyncEngine {
    let kp = ActorKeypair::generate();
    let bearer: Arc<dyn BearerSource> = Arc::new(StaticBearer("test.bearer".to_string()));
    let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
        server_uri.to_string(),
        kp,
        bearer,
        reqwest::Client::new(),
    ));
    SyncEngine::new(
        watch_dir,
        SyncDb::open_in_memory().unwrap(),
        SyncClient::new(auth, &OUR_DEVICE),
        Some("__test".to_string()),
        OUR_DEVICE,
        None,
        None,
        Some(BackupKey::from_bytes([0x61u8; 32]).into()),
        None,
        None,
        fauna_core::format::ConflictPolicy::default(),
        FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        fauna_client::NestClient::new(server_uri.to_string(), ActorKeypair::generate()),
        crate::config::SyncMode::Sync,
    )
}

fn own_change(
    seq: i64,
    manifest: ContentHash,
    derived_through: i64,
    is_resolution: bool,
) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{REL}"),
        manifest_hash: Some(hex::encode(manifest.digest())),
        change_type: "modify".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(REL.to_string()),
        device_id: Some(hex::encode(OUR_DEVICE)),
        derived_through: Some(derived_through),
        is_resolution: Some(is_resolution),
        ..Default::default()
    }
}

fn peer_delete(seq: i64) -> SyncChange {
    SyncChange {
        seq,
        path_hash: format!("path-hash-{REL}"),
        change_type: "delete".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(REL.to_string()),
        device_id: Some(hex::encode([0xBBu8; 32])),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_re_adopted_own_resolution_leaves_a_later_delete_applicable() {
    let server = MockServer::start().await;
    let store = MockNest::new().mount(&server).await;
    let watch = tempfile::tempdir().unwrap();
    let full = watch.path().join(REL);
    let engine = engine(&server.uri(), watch.path().to_path_buf());

    // The log-tail bytes are this seat's own publication (its resolution)…
    std::fs::write(&full, LOG_TAIL).unwrap();
    engine
        .upload_file(REL)
        .await
        .expect("upload the log-tail bytes");
    let log_tail_manifest = store.last_manifest_hash.lock().unwrap().unwrap();
    // …but local then sits on the earlier permutation, published too.
    std::fs::write(&full, EARLIER).unwrap();
    engine
        .upload_file(REL)
        .await
        .expect("upload the earlier bytes");
    let earlier_manifest = store.last_manifest_hash.lock().unwrap().unwrap();

    // The earlier permutation's echo: base, frontier, edit-frontier and ledger
    // all land on seq 5.
    engine
        .apply_remote_changes(&[own_change(5, earlier_manifest, 4, false)], 4)
        .await
        .expect("echo the earlier permutation");
    // The own covering resolution echoes LATER in the log (seq 7, covering the
    // edit-frontier at 5): the author-blind supersession re-adopts it.
    engine
        .apply_remote_changes(&[own_change(7, log_tail_manifest, 6, true)], 5)
        .await
        .expect("echo the log-tail resolution");
    assert_eq!(
        std::fs::read(&full).unwrap(),
        LOG_TAIL,
        "precondition: the re-adopt arm fired and local now holds the log tail"
    );

    let entry = engine.db().get_entry(REL).unwrap().expect("the row exists");
    assert_eq!(
        entry.local_hash,
        Some(ContentHash::of_raw(LOG_TAIL)),
        "the re-adopt rewrote the file, so the row's local identity must be the \
         re-adopted bytes — a row naming the previous permutation reads as a local \
         edit to every later judgement that compares disk against row"
    );
    assert_eq!(
        entry.state,
        SyncState::Synced,
        "the re-adopted head is settled"
    );

    // A peer deletes the converged file.
    engine
        .apply_remote_changes(&[peer_delete(8)], 7)
        .await
        .expect("apply the peer's delete");
    assert!(
        !full.exists(),
        "nothing on this seat is unpublished — local IS its own published row — so \
         the delete must apply; declining it files an edit-wins report that \
         recreates the file on every seat (the live 3-seat red)"
    );
}
