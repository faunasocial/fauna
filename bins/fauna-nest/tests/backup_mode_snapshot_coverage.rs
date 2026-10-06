//! tier_3: an ordinary (non-reserved) backup-type folder records to the
//! `sync_changes` head feed like every other folder — the folders re-model
//! phase 3 head unification (`docs/goal/behavior/file-sync.md` § Membership
//! → *Target state — head unification*;
//! `docs/goal/behavior/folders.md` § Target re-model, *one write plane*).
//!
//! Proof obligations:
//! 1. `fauna.sync.changes.record` on an ordinary Backup folder assigns a
//!    **real, ascending seq** (pre-phase-3 it routed to the latest-per-path
//!    `backup_custody` projection and always replied `seq: 0`).
//! 2. Snapshots capture the folder's files from the **head plane** and stay
//!    non-empty (the 2026-07-17 silent-feature-death finding, re-pinned on the
//!    unified plane).
//! 3. Version history is the sync-set semantics now: a superseded version's
//!    blobs survive GC until the client marks them via
//!    `fauna.sync.changes.supersede` — the verb that REFUSED Backup folders
//!    pre-phase-3 — and a snapshot pin still protects a marked version.
//!
//! The `backup_custody` plane is untouched for reserved `__*` destination sets
//! (pinned by the segment-backup conformance tests, not here).
//!
//! Drives the **real** `fauna.sync.changes.record`,
//! `fauna.sync.changes.supersede`, `fauna.filesync.snapshot.create_folder`,
//! and `fauna.filesync.snapshot.get` handlers plus the **real**
//! `garbage_collect`, with staged chunk + manifest blobs (the state the upload
//! pipeline produces).

mod common;
use common::{put_chunk, put_manifest_over};

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::ContentHash;
use fauna_nest::backup::gc::garbage_collect;
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::db::CacheDb;
use fauna_nest::filesync_handlers::register_filesync_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::sync_handlers::register_sync_handlers;
use fauna_protocol::filesync::{
    SnapshotCreateFolderReply, SnapshotCreateFolderRequest, SnapshotGetReply, SnapshotGetRequest,
};
use fauna_protocol::sync::{
    SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesSupersedeReply,
    SyncChangesSupersedeRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// The blob GC's record walk (`gc.rs` step 2f) reads live post bodies from a
/// `__post` segment store; these tests seed no posts, so an empty throwaway
/// one is the honest source.
fn post_segments(tmp: &tempfile::TempDir) -> fauna_segment_store::SegmentManager {
    fauna_segment_store::SegmentManager::new(tmp.path().join("post-segments"), "post")
}

async fn dispatch(state: &Arc<AppState>, actor: [u8; 32], kind: &str, payload: Bytes) -> Bytes {
    common::seed_dispatch_actor(&state.db, &actor).await;
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state.clone(), actor, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind} handler ok, got {e:?}"))
}

/// Record as a production writer does: signed by the recorder's identity key
/// under the set's stored nonce ([`common::SET_NONCE`], which the set is
/// created under) — an unsigned record is refused `signature_required`.
async fn record(
    state: &Arc<AppState>,
    kp: &fauna_core::identity::ActorKeypair,
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest_hash: Option<ContentHash>,
    change_type: &str,
) -> i64 {
    let req = SyncChangeRecordRequest {
        nest_url: None,
        channel_id: None,
        folder: folder.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: manifest_hash.map(|h| hex::encode(h.digest())),
        size_bytes: 0,
        change_type: change_type.to_string(),
        content_key_version: None,
        thumbnail_hash: None,
        // S9 flip: a sealless record refuses; the nest stores this opaquely.
        path_sealed: Some(fauna_protocol::ByteBuf::from(
            b"e2e-synthetic-seal".to_vec(),
        )),
        ..Default::default()
    };
    let req = common::signed_record(req, kp);
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let reply: SyncChangeRecordReply =
        decode(&dispatch(state, kp.actor_id().0, "fauna.sync.changes.record", payload).await)
            .unwrap();
    reply.seq
}

async fn supersede(
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_hex: &str,
    folder: &str,
    path: &str,
    head: ContentHash,
) -> u64 {
    let req = SyncChangesSupersedeRequest {
        folder: folder.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: hex::encode(head.digest()),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let reply: SyncChangesSupersedeReply =
        decode(&dispatch(state, actor, "fauna.sync.changes.supersede", payload).await).unwrap();
    reply.superseded
}

async fn exists(store: &DiskBlobStore, h: &ContentHash) -> bool {
    store.exists(h).await.unwrap()
}

#[tokio::test]
async fn backup_folder_records_ride_the_head_feed_and_snapshots_pin_versions() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            register_filesync_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            ..AppState::for_test(db.clone())
        })
    };

    // A wizard-created ordinary folder + the recording client's
    // write-capable device — the Photo-Library shape.
    let kp = common::signing_actor(0x51);
    let actor: [u8; 32] = kp.actor_id().0;
    let device: [u8; 32] = [0x09; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "phone", None, "write")
        .await
        .unwrap();
    db.create_folder_with_options(
        "photo-library",
        &actor,
        fauna_nest::db::FolderOptions {
            set_nonce: Some(common::SET_NONCE.to_vec()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Path A: recorded, then SNAPSHOTTED (its v1 earns a pin). The reply seq
    // is REAL — head-unified Backup folders are in the device-sync feed.
    let (c_a1, la1) = put_chunk(&store, &db, b"photo-A-version-1").await;
    let m_a1 = put_manifest_over(&store, &db, c_a1, la1).await;
    let seq_a1 = record(
        &state,
        &kp,
        &device_hex,
        "photo-library",
        "2024/IMG_0001.heic",
        Some(m_a1),
        "create",
    )
    .await;
    assert!(
        seq_a1 >= 1,
        "head-unified: a Backup-folder record is assigned a real seq, got {seq_a1}"
    );

    // The production snapshot-create handler captures from the head plane.
    let create_reply: SnapshotCreateFolderReply = decode(
        &dispatch(
            &state,
            actor,
            "fauna.filesync.snapshot.create_folder",
            Bytes::from(
                encode_canonical(&SnapshotCreateFolderRequest {
                    folder: "photo-library".into(),
                    tags: vec![],
                    device_id: None,
                    ..Default::default()
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    assert_eq!(
        create_reply.file_count, 1,
        "the snapshot must capture the Backup folder's head, not be empty"
    );

    // snapshot.get lists the file with its manifest.
    let get_reply: SnapshotGetReply = decode(
        &dispatch(
            &state,
            actor,
            "fauna.filesync.snapshot.get",
            Bytes::from(
                encode_canonical(&SnapshotGetRequest {
                    snapshot_id: create_reply.id,
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
            ),
        )
        .await,
    )
    .unwrap();
    assert_eq!(get_reply.files.len(), 1);
    // S9 flip: the required wire field carries the scrub sentinel; the entry
    // is addressed by its hash companion.
    assert_eq!(get_reply.files[0].path, "");
    assert_eq!(
        get_reply.files[0].path_hash.as_deref().map(|b| b.to_vec()),
        Some(fauna_core::sync::path_hash("2024/IMG_0001.heic").to_vec())
    );
    assert_eq!(
        get_reply.files[0].manifest_hash.as_ref(),
        m_a1.digest().as_slice(),
        "the snapshot pins the exact recorded manifest"
    );

    // Path B: recorded AFTER the snapshot (its v1 earns no pin) — the
    // non-vacuity control for the pin assertion below.
    let (c_b1, lb1) = put_chunk(&store, &db, b"photo-B-version-1").await;
    let m_b1 = put_manifest_over(&store, &db, c_b1, lb1).await;
    let seq_b1 = record(
        &state,
        &kp,
        &device_hex,
        "photo-library",
        "2024/IMG_0002.heic",
        Some(m_b1),
        "create",
    )
    .await;
    assert!(seq_b1 > seq_a1, "seqs ascend on the one head feed");

    // Edit BOTH paths. On the head plane an edit is an append — the prior
    // version becomes history, not an automatic reclaim (the file-versions
    // semantics every other folder already has).
    let (c_a2, la2) = put_chunk(&store, &db, b"photo-A-version-2-edited").await;
    let m_a2 = put_manifest_over(&store, &db, c_a2, la2).await;
    record(
        &state,
        &kp,
        &device_hex,
        "photo-library",
        "2024/IMG_0001.heic",
        Some(m_a2),
        "modify",
    )
    .await;
    let (c_b2, lb2) = put_chunk(&store, &db, b"photo-B-version-2-edited").await;
    let m_b2 = put_manifest_over(&store, &db, c_b2, lb2).await;
    record(
        &state,
        &kp,
        &device_hex,
        "photo-library",
        "2024/IMG_0002.heic",
        Some(m_b2),
        "modify",
    )
    .await;

    // Un-superseded history is a live reference: GC reclaims nothing yet.
    let r = garbage_collect(
        &db,
        &dyn_store,
        fauna_nest::backup::gc::PostBodySource {
            segments: &post_segments(&tmp),
        },
        0,
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        r.deleted_blobs, 0,
        "head-plane version history stays referenced until the client supersedes it"
    );
    for live in [c_a1, m_a1, c_b1, m_b1, c_a2, m_a2, c_b2, m_b2] {
        assert!(
            exists(&store, &live).await,
            "all versions live pre-supersede"
        );
    }

    // The client verifies its v2 copies and marks the old versions — through
    // the production supersede verb, which refused Backup folders before the
    // head unification.
    let marked_a = supersede(
        &state,
        actor,
        &device_hex,
        "photo-library",
        "2024/IMG_0001.heic",
        m_a2,
    )
    .await;
    assert_eq!(marked_a, 1, "A's v1 row marks superseded");
    let marked_b = supersede(
        &state,
        actor,
        &device_hex,
        "photo-library",
        "2024/IMG_0002.heic",
        m_b2,
    )
    .await;
    assert_eq!(marked_b, 1, "B's v1 row marks superseded");

    let r2 = garbage_collect(
        &db,
        &dyn_store,
        fauna_nest::backup::gc::PostBodySource {
            segments: &post_segments(&tmp),
        },
        0,
        None,
        false,
    )
    .await
    .unwrap();

    // A-v1 is pinned by the snapshot: point-in-time recovery survives the
    // supersede. B-v1 was never snapshotted: it reclaims — proving the
    // snapshot pin (not some blanket keep-everything) is what saved A-v1.
    for live in [c_a1, m_a1, c_a2, m_a2, c_b2, m_b2] {
        assert!(
            exists(&store, &live).await,
            "snapshot-pinned v1 + both live v2 blobs survive GC"
        );
    }
    for gone in [c_b1, m_b1] {
        assert!(
            !exists(&store, &gone).await,
            "an unsnapshotted superseded version reclaims (non-vacuity control)"
        );
    }
    assert_eq!(
        r2.deleted_blobs, 2,
        "exactly the unpinned superseded version's manifest + chunk reclaim"
    );
}
