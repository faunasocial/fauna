//! tier_3: custodian-authoritative GC-safety for cross-location backups.
//!
//! Proof obligation (`docs/goal/behavior/backup-destinations.md` § Destination-removal / supersede
//! handshake; `docs/goal/architecture/message-segment-store.md` § Cross-location
//! backup protocol → *GC-safety — custodian-authoritative custody*;
//! `docs/goal/behavior/backup-restore.md` § Known gap: segment-backup blobs vs
//! GC): a destination nest's GC must **never** delete a live backup blob, and
//! must **reclaim** a backup's chunks once superseded / its path deleted / its
//! destination removed.
//!
//! This drives the **real** `fauna.sync.changes.record` handler against a
//! a custody-copy folder (so the custodian-routing branch + the device /
//! permission gates participate) and the **real** `garbage_collect`, exactly as
//! a destination nest runs them. The "uploaded" chunk + `manifest.<kind>`
//! blobs are staged directly into the blob store + `blob_metadata` (the state
//! `SyncEngine::upload_bytes`' chunk+seal pipeline produces), then recorded
//! through the handler — the test drives the record path directly, no
//! coordinator required (step 8).

mod common;
use common::{put_chunk, put_manifest_over};

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::ContentHash;
use fauna_nest::backup::gc::{BACKUP_CUSTODY_GRACE_SECS, garbage_collect};
use fauna_nest::backup::service::BackupService;
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::sync_handlers::register_sync_handlers;
use fauna_protocol::folders::FolderCreateRequest;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// The blob GC's record walk (`gc.rs` step 2f) reads live post bodies from a
/// `__post` segment store; these tests seed no posts, so an empty throwaway
/// one is the honest source.
fn post_segments(tmp: &tempfile::TempDir) -> fauna_segment_store::SegmentManager {
    fauna_segment_store::SegmentManager::new(tmp.path().join("post-segments"), "post")
}

/// A `BackupService` over the same root the test's own `DiskBlobStore` uses,
/// so the custody-charge derivation (`record_change_core`, reserved sets)
/// reads exactly the bytes the test staged.
fn svc_over(db: &Arc<CacheDb>, root: &std::path::Path) -> Option<Arc<BackupService>> {
    Some(Arc::new(
        BackupService::new(db.clone(), None, false, root.to_path_buf(), None).unwrap(),
    ))
}

/// Drive the production `fauna.sync.changes.record` handler. `manifest_hash =
/// None` records a delete (compacted-out path → tombstone).
async fn record(
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest_hash: Option<ContentHash>,
    change_type: &str,
) {
    common::seed_dispatch_actor(&state.db, &actor).await;
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
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let meta = state
        .rpc_router
        .kind_meta("fauna.sync.changes.record")
        .expect("changes.record kind registered");
    let reply_bytes = (meta.handler)(state.clone(), actor, payload)
        .await
        .expect("record handler ok");
    let reply: SyncChangeRecordReply = decode(&reply_bytes).expect("decode reply");
    // A backup destination is write-only custody, never pulled back by a
    // device, so no monotonic sequence is assigned.
    assert_eq!(reply.seq, 0, "custody-copy record returns seq 0");
}

async fn exists(store: &DiskBlobStore, h: &ContentHash) -> bool {
    store.exists(h).await.unwrap()
}

#[tokio::test]
async fn backup_custody_gc_safety_round_trip() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: svc_over(&db, tmp.path()),
            ..AppState::for_test(db.clone())
        })
    };

    // The owner's reserved custody-copy set on the destination nest + the
    // coordinator's write-capable device, as lazy provisioning establishes.
    let actor: [u8; 32] = [0x42; 32];
    let device: [u8; 32] = [0x07; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "backup-device", None, "write")
        .await
        .unwrap();
    db.create_folder_with_options(
        "__mail",
        &actor,
        fauna_nest::db::FolderOptions {
            custody_copy: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let fs = db
        .get_folder_for_actor("__mail", &actor)
        .await
        .unwrap()
        .unwrap();

    // ── Scenario 1 — upload N=2 segments + the manifest mirror, record all,
    //    GC, assert nothing reclaimed. ──────────────────────────────────────
    let (c_seg1, l1) = put_chunk(&store, &db, b"segment-1-chunk").await;
    let m_seg1 = put_manifest_over(&store, &db, c_seg1, l1).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4242.../seg-00000001.dat",
        Some(m_seg1),
        "create",
    )
    .await;

    let (c_seg2, l2) = put_chunk(&store, &db, b"segment-2-chunk-distinct").await;
    let m_seg2 = put_manifest_over(&store, &db, c_seg2, l2).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4242.../seg-00000002.dat",
        Some(m_seg2),
        "create",
    )
    .await;

    let (c_mir, lm) = put_chunk(&store, &db, b"manifest-mirror-bytes").await;
    let m_mir = put_manifest_over(&store, &db, c_mir, lm).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4242.../manifest.mail",
        Some(m_mir),
        "create",
    )
    .await;

    // Device-pull exclusion is automatic: backups never enter `sync_changes`.
    assert!(
        db.get_sync_changes_for_folder(fs.id, 0, None)
            .await
            .unwrap()
            .is_empty(),
        "custody-copy records must not enter the device-sync feed",
    );
    assert!(
        db.sync_change_manifest_hashes(i64::MAX)
            .await
            .unwrap()
            .is_empty(),
        "no backup manifest leaks into sync_changes",
    );

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
        "GC must not delete any live backup blob"
    );
    for h in [c_seg1, m_seg1, c_seg2, m_seg2, c_mir, m_mir] {
        assert!(exists(&store, &h).await, "live backup blob survives GC");
    }

    // ── Scenario 2 — supersede seg-1 (new manifest, same path) + compact-out
    //    seg-2 (delete), GC, assert ONLY the now-orphaned chunks reclaim. ────
    let (c_seg1b, l1b) = put_chunk(&store, &db, b"segment-1-chunk-GROWN").await;
    let m_seg1b = put_manifest_over(&store, &db, c_seg1b, l1b).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4242.../seg-00000001.dat",
        Some(m_seg1b),
        "modify",
    )
    .await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4242.../seg-00000002.dat",
        None,
        "delete",
    )
    .await;

    // The custody grace window (T = `BACKUP_CUSTODY_GRACE_SECS`) changed this
    // step's contract, and it is the point of the mitigation: on a **reserved
    // destination** set a supersede/tombstone no longer reclaims immediately —
    // the displaced generation is RETAINED and stays pinned, because a custody
    // writer's supersede power is delete power and the writer is now the owner's
    // source nest (`message-segment-store.md` § Cross-location backup protocol).
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
        "inside T, a superseded/tombstoned generation's chunks must NOT reclaim — \
         that window is what makes a rogue source nest's wipe recoverable",
    );
    for retained in [c_seg1, m_seg1, c_seg2, m_seg2] {
        assert!(
            exists(&store, &retained).await,
            "the displaced generation's blobs survive inside the grace window"
        );
    }
    for live in [c_seg1b, m_seg1b, c_mir, m_mir] {
        assert!(
            exists(&store, &live).await,
            "still-live backup blob survives"
        );
    }

    // ── Scenario 2b — age the retained generations past T; now (and only now)
    //    their exclusive chunks reclaim, and the live ones still do not. ────
    let aged = db
        .backdate_backup_custody_generations_for_test(BACKUP_CUSTODY_GRACE_SECS + 60)
        .await
        .unwrap();
    assert_eq!(
        aged, 2,
        "two generations retained: superseded seg-1 + seg-2"
    );

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
        r.expired_custody_generations, 2,
        "both generations aged out of the window"
    );
    assert_eq!(
        r.deleted_blobs, 4,
        "past T the superseded seg-1 (old manifest+chunk) + compacted-out seg-2 \
         (manifest+chunk) finally reclaim",
    );
    for gone in [c_seg1, m_seg1, c_seg2, m_seg2] {
        assert!(
            !exists(&store, &gone).await,
            "orphaned backup blob reclaimed past T"
        );
    }
    for live in [c_seg1b, m_seg1b, c_mir, m_mir] {
        assert!(
            exists(&store, &live).await,
            "still-live backup blob survives the expiry sweep"
        );
    }

    // ── Scenario 3 — destination removal drops the reserved set's custody,
    //    GC, assert everything reclaims. ───────────────────────────────────
    db.drop_backup_custody_for_folder(fs.id).await.unwrap();
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
        r.deleted_blobs, 4,
        "all remaining backup blobs reclaim on removal"
    );
    for gone in [c_seg1b, m_seg1b, c_mir, m_mir] {
        assert!(
            !exists(&store, &gone).await,
            "removed backup blob reclaimed"
        );
    }
}

/// The grace window's own end-to-end proof, driven through the **real**
/// `fauna.sync.changes.record` handler and the **real** `garbage_collect`: a
/// rogue source nest overwrites a segment with junk, and the owner recovers.
///
/// This is the scenario the whole slice exists for
/// (`message-segment-store.md` § Cross-location backup protocol threat table:
/// destination write power = supersede-delete power).
#[tokio::test]
async fn a_rogue_supersede_is_recoverable_within_the_grace_window() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: svc_over(&db, tmp.path()),
            ..AppState::for_test(db.clone())
        })
    };

    let actor: [u8; 32] = [0x42; 32];
    let device: [u8; 32] = [0x07; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "backup-device", None, "write")
        .await
        .unwrap();
    db.create_folder_with_options(
        "__mail",
        &actor,
        fauna_nest::db::FolderOptions {
            custody_copy: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    const PATH: &str = "4242.../seg-00000001.dat";
    let path_hash = fauna_core::sync::path_hash(PATH);

    // The good backup.
    let (c_good, l_good) = put_chunk(&store, &db, b"the-real-mail-segment").await;
    let m_good = put_manifest_over(&store, &db, c_good, l_good).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        PATH,
        Some(m_good),
        "create",
    )
    .await;

    // The rogue source nest supersedes it with junk — with no grace window this
    // is a permanent, silent delete of the user's backup.
    let (c_junk, l_junk) = put_chunk(&store, &db, b"junk").await;
    let m_junk = put_manifest_over(&store, &db, c_junk, l_junk).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        PATH,
        Some(m_junk),
        "modify",
    )
    .await;

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
    assert_eq!(r.deleted_blobs, 0, "the good generation is not swept");
    assert!(
        exists(&store, &c_good).await && exists(&store, &m_good).await,
        "the good backup's bytes are still on the destination inside T"
    );

    // The owner's client (its OWN authed connection to the destination, never
    // via the source) sees the retained generation and restores it.
    let gens = db
        .list_backup_custody_generations(&actor, None, 0)
        .await
        .unwrap();
    assert_eq!(gens.len(), 1, "exactly the displaced good generation");
    assert_eq!(gens[0].manifest_hash, m_good.digest().to_vec());

    let restored = db
        .restore_backup_custody_generation(&actor, "__mail", &path_hash, &m_good.digest())
        .await
        .unwrap();
    assert_eq!(
        restored,
        fauna_nest::db::GenerationRestore::Restored,
        "the good generation is promoted back to live"
    );

    // And now the JUNK is the displaced generation — so once it ages out, the
    // good backup is what survives the sweep.
    db.backdate_backup_custody_generations_for_test(BACKUP_CUSTODY_GRACE_SECS + 60)
        .await
        .unwrap();
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
    assert_eq!(r.expired_custody_generations, 1);
    assert_eq!(r.deleted_blobs, 2, "the junk generation reclaims past T");
    assert!(
        exists(&store, &c_good).await && exists(&store, &m_good).await,
        "the RESTORED backup survives — the recovery is complete and durable"
    );
    assert!(
        !exists(&store, &c_junk).await && !exists(&store, &m_junk).await,
        "and the rogue write's bytes are gone"
    );
}

/// A custody-referenced manifest that fails to decode makes reachability
/// incomputable — the sweep must fail closed and delete NOTHING that run
/// (`docs/goal/behavior/backup-restore.md` § 9): a live chunk enumerable only
/// through the failed manifest must survive, and so must every unrelated
/// orphan (over-deletion is the data-loss direction; the disk guard, not the
/// sweep, backstops disk-fill). Drives the real `changes.record` handler +
/// the real `garbage_collect`, exactly like the round-trip above.
#[tokio::test]
async fn backup_custody_undecodable_manifest_fails_closed() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: svc_over(&db, tmp.path()),
            ..AppState::for_test(db.clone())
        })
    };

    let actor: [u8; 32] = [0x43; 32];
    let device: [u8; 32] = [0x08; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "backup-device", None, "write")
        .await
        .unwrap();
    db.create_folder_with_options(
        "__mail",
        &actor,
        fauna_nest::db::FolderOptions {
            custody_copy: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // A healthy segment: chunk + decodable manifest, recorded into custody.
    let (c_ok, l_ok) = put_chunk(&store, &db, b"healthy-segment-chunk").await;
    let m_ok = put_manifest_over(&store, &db, c_ok, l_ok).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4343.../seg-00000001.dat",
        Some(m_ok),
        "create",
    )
    .await;

    // A manifest that decoded fine when it was RECORDED, then bit-rotted at
    // rest: custody references it, but the bytes at its address are no longer
    // the ones written there (they hash to something else and do not decode).
    // Its chunk is enumerable only through it. The record-time charge
    // derivation saw the healthy bytes — corruption after acceptance is
    // exactly the case GC's fail-close exists for, and the reason the
    // record-time refusal does not retire it. (Hash-INTACT undecodable bytes
    // are the never-a-manifest case, which deliberately does NOT fail-close —
    // see `gc_sweep_survives_a_recorded_reference_that_was_never_a_manifest`.)
    let (c_stranded, l_stranded) = put_chunk(&store, &db, b"chunk-behind-corrupt-manifest").await;
    let corrupt_hash = put_manifest_over(&store, &db, c_stranded, l_stranded).await;
    record(
        &state,
        actor,
        &device_hex,
        "__mail",
        "4343.../seg-00000002.dat",
        Some(corrupt_hash),
        "create",
    )
    .await;
    // The bit-rot itself: replace the stored manifest bytes in place.
    store.delete(&corrupt_hash).await.unwrap();
    store
        .put(&corrupt_hash, b"\x01corrupt-manifest-bytes")
        .await
        .unwrap();

    // A true orphan that WOULD reclaim on a healthy run.
    let orphan = ContentHash::from_digest_raw([0xEEu8; 32]);
    store.put(&orphan, b"unreferenced orphan").await.unwrap();
    db.put_blob_metadata(&orphan.digest(), 19, "chunk", None, None)
        .await
        .unwrap();

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
        r.manifest_decode_failures, 1,
        "the corrupt manifest counted"
    );
    assert_eq!(
        r.deleted_blobs, 0,
        "fail-closed: no blob may be deleted while a manifest is undecodable"
    );
    for h in [c_ok, m_ok, c_stranded, corrupt_hash, orphan] {
        assert!(exists(&store, &h).await, "every blob survives the run");
    }
}

/// Attempt `fauna.sync.changes.record`, returning the handler's raw result.
/// The record is signed as a production writer signs it — by the recorder's
/// identity key under the set's stored nonce ([`common::SET_NONCE`]); an
/// unsigned record is refused `signature_required`.
async fn try_record_sync(
    state: &Arc<AppState>,
    kp: &fauna_core::identity::ActorKeypair,
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest_hash: Option<ContentHash>,
) -> Result<Bytes, fauna_protocol::RpcError> {
    let actor = kp.actor_id().0;
    common::seed_dispatch_actor(&state.db, &actor).await;
    let req = SyncChangeRecordRequest {
        nest_url: None,
        channel_id: None,
        folder: folder.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: manifest_hash.map(|h| hex::encode(h.digest())),
        size_bytes: 0,
        change_type: "create".to_string(),
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
    let meta = state
        .rpc_router
        .kind_meta("fauna.sync.changes.record")
        .expect("changes.record kind registered");
    (meta.handler)(state.clone(), actor, payload).await
}

/// An ordinary set born under the client-minted set nonce the fixture records
/// sign under ([`common::SET_NONCE`]).
fn nonce_set() -> fauna_nest::db::FolderOptions {
    fauna_nest::db::FolderOptions {
        set_nonce: Some(common::SET_NONCE.to_vec()),
        ..Default::default()
    }
}

/// GC reachability must derive from the **bytes**, not from a folder name.
///
/// `fold_direct_blob_refs` classifies a manifest hash referenced only from
/// reserved (`__`) sets as the stored blob itself — "never a decodable
/// `ChunkManifest`" (`backup-restore.md` § 9) — and so pins it *without walking
/// its chunks*. That was a **trusted convention**, and the trust was misplaced:
/// every reserved set the nest mints (`get_or_create_reserved_folder`) omits
/// `mode` and so takes the schema default `'sync'`, which is exactly the mode
/// whose records land in `sync_changes`. A chunked manifest recorded against
/// one therefore had its live chunks skipped by the walk and swept — silently
/// (it decodes fine, so `manifest_decode_failures` stays 0 and the fail-closed
/// sweep never fires) and unrecoverably off-box.
///
/// The write paths now refuse to create such a row, so this test stages it the
/// way it exists in the wild: **directly at the DB layer, as a pre-enforcement
/// row**. That is the case that matters — a live nest whose backup coordinator
/// hit the `__mail` mode-collision has these rows *already*, and only a
/// content-derived classification saves their chunks. Enforcement stops new
/// ones; this is what protects the ones already written.
#[tokio::test]
async fn gc_walks_a_chunked_manifest_already_recorded_under_a_reserved_set_name() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let actor: [u8; 32] = [0x51; 32];
    let device: [u8; 32] = [0x09; 32];

    // The nest's OWN reserved rail mints `__mail` — no client forgery involved.
    db.get_or_create_reserved_folder(&actor, "mail")
        .await
        .unwrap();
    let fs = db
        .get_folder_for_actor("__mail", &actor)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !fs.custody_copy,
        "premise: a nest-minted reserved set is a rail, not a custody copy — \
         the class whose records land in `sync_changes`, not custody"
    );

    // A live, chunked file, recorded against that reserved set before the guard
    // existed (the coordinator mode-collision leaves exactly this row).
    let (chunk, len) = put_chunk(&store, &db, b"a live file chunk under a reserved set").await;
    let manifest = put_manifest_over(&store, &db, chunk, len).await;
    db.record_sync_change_metered(
        &actor,
        // owner path: recorder == metered owner, no member cap
        &actor,
        None,
        &[0xAB; 32],
        Some(&manifest.digest()),
        len as i64,
        "create",
        fs.id,
        &device,
        Some("notes/live.txt"),
        None,
        None,
        None,
        None,
        None,
        i64::MAX,
    )
    .await
    .unwrap();

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
        r.manifest_decode_failures, 0,
        "the manifest decodes fine — the hazard is silent; the fail-closed sweep \
         never fires, which is why it cannot be the thing that saves the chunk"
    );
    assert!(
        exists(&store, &manifest).await,
        "the manifest itself is pinned either way"
    );
    assert!(
        exists(&store, &chunk).await,
        "DATA LOSS: the live file chunk was GC-deleted. Its manifest was classified \
         direct-blob on the strength of the `__` name alone, so the walk never \
         enumerated its chunks"
    );
    assert_eq!(
        r.direct_blob_refs, 0,
        "the reference must be RE-classified manifest-class by the content probe — \
         counting it direct-blob is precisely the bug"
    );
}

/// A genuine direct-blob rail reference (a client-sealed `__drafts`
/// blob — bytes that do NOT decode as a `ChunkManifest`) must still be
/// classified direct-blob and pinned decode-free. The content probe must not
/// re-introduce the decode-failure noise the classification was built to kill:
/// counting a rail blob as a `manifest_decode_failure` would fail-close the
/// sweep on every cycle and disable reclamation forever.
#[tokio::test]
async fn gc_still_classifies_a_real_rail_blob_as_direct_blob() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let actor: [u8; 32] = [0x52; 32];
    let device: [u8; 32] = [0x0A; 32];

    db.get_or_create_reserved_folder(&actor, "config")
        .await
        .unwrap();
    let fs = db
        .get_folder_for_actor("__config", &actor)
        .await
        .unwrap()
        .unwrap();

    // A sealed rail blob: version byte + ciphertext. Not a ChunkManifest — the
    // `0x01` parses as a complete CBOR integer, leaving the rest as trailing
    // data, so canonical decoding fails. That failure is EXPECTED here.
    let sealed = [&[0x01u8][..], &[0x9Au8; 64][..]].concat();
    let (blob, _len) = put_chunk(&store, &db, &sealed).await;
    db.record_sync_change_metered(
        &actor,
        // owner path: recorder == metered owner, no member cap
        &actor,
        None,
        &[0xCD; 32],
        Some(&blob.digest()),
        sealed.len() as i64,
        "create",
        fs.id,
        &device,
        Some("config"),
        None,
        None,
        None,
        None,
        None,
        i64::MAX,
    )
    .await
    .unwrap();

    let orphan = ContentHash::of_raw(b"an unreferenced orphan");
    store.put(&orphan, b"an unreferenced orphan").await.unwrap();
    db.put_blob_metadata(&orphan.digest(), 22, "chunk", None, None)
        .await
        .unwrap();

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
        r.manifest_decode_failures, 0,
        "a rail blob failing to decode is the EXPECTED case, never a decode failure — \
         counting it would fail-close the sweep every cycle"
    );
    assert_eq!(r.direct_blob_refs, 1, "still classified direct-blob");
    assert!(exists(&store, &blob).await, "the rail blob stays pinned");
    assert!(
        !exists(&store, &orphan).await,
        "…and the sweep still runs: a genuine orphan is reclaimed"
    );
}

/// A blob store whose `get` fails with a transient I/O error for one hash
/// (EMFILE/EIO/EACCES — the store is up, this read did not land). Every other
/// operation delegates to the real `DiskBlobStore`.
struct ErrOnGetStore {
    inner: Arc<DiskBlobStore>,
    fails: ContentHash,
}

#[async_trait::async_trait]
impl BlobStoreBackend for ErrOnGetStore {
    async fn get(&self, hash: &ContentHash) -> anyhow::Result<Option<Vec<u8>>> {
        if hash.digest() == self.fails.digest() {
            anyhow::bail!("simulated transient blob-store read error (EMFILE)");
        }
        self.inner.get(hash).await
    }
    async fn put(&self, hash: &ContentHash, data: &[u8]) -> anyhow::Result<()> {
        self.inner.put(hash, data).await
    }
    async fn exists(&self, hash: &ContentHash) -> anyhow::Result<bool> {
        self.inner.exists(hash).await
    }
    async fn exists_batch(&self, hashes: &[ContentHash]) -> anyhow::Result<Vec<bool>> {
        self.inner.exists_batch(hashes).await
    }
    async fn delete(&self, hash: &ContentHash) -> anyhow::Result<()> {
        self.inner.delete(hash).await
    }
    async fn usage_bytes(&self) -> anyhow::Result<u64> {
        self.inner.usage_bytes().await
    }
    async fn list_all_hashes(&self) -> anyhow::Result<Vec<ContentHash>> {
        self.inner.list_all_hashes().await
    }
}

/// The content probe is the ONLY thing protecting the chunked-manifest-under-a-
/// reserved-name rows a live nest already carries (the write-path guards only
/// stop NEW ones). So a probe that cannot read the blob must not answer the
/// destructive question: a transient store READ error is not "absent" — the
/// blob is there and may well be a manifest whose chunks are live. Fail-close
/// this run (count it, so both consumers — the GC sweep and the `__index` boot
/// purge — bail) rather than sweep against a provably incomplete reference set.
/// The manifest-walk arm has always treated the identical `Err` this way; the
/// probe folding it into "not a manifest" is the data-loss bug.
#[tokio::test]
async fn gc_fails_closed_when_the_probe_cannot_read_a_reserved_set_reference() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

    let actor: [u8; 32] = [0x53; 32];
    let device: [u8; 32] = [0x0B; 32];

    db.get_or_create_reserved_folder(&actor, "mail")
        .await
        .unwrap();
    let fs = db
        .get_folder_for_actor("__mail", &actor)
        .await
        .unwrap()
        .unwrap();

    // Exactly the row class a live nest already has: a genuine chunked manifest
    // recorded against a reserved set.
    let (chunk, len) = put_chunk(&store, &db, b"a live file chunk the probe cannot reach").await;
    let manifest = put_manifest_over(&store, &db, chunk, len).await;
    db.record_sync_change_metered(
        &actor,
        // owner path: recorder == metered owner, no member cap
        &actor,
        None,
        &[0xEF; 32],
        Some(&manifest.digest()),
        len as i64,
        "create",
        fs.id,
        &device,
        Some("notes/unreadable.txt"),
        None,
        None,
        None,
        None,
        None,
        i64::MAX,
    )
    .await
    .unwrap();

    // The probe's read of the manifest fails — the one read that decides whether
    // this reference's chunks get walked.
    let dyn_store: Arc<dyn BlobStoreBackend> = Arc::new(ErrOnGetStore {
        inner: store.clone(),
        fails: manifest,
    });

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

    assert!(
        exists(&store, &chunk).await,
        "DATA LOSS: one EMFILE during a GC cycle swept a live file's chunk. The \
         probe folded the read error into `_ => false` (= not a manifest), so the \
         walk never enumerated it, and the failure went uncounted so the \
         fail-closed sweep never fired"
    );
    assert!(
        r.manifest_decode_failures >= 1,
        "a probe READ ERROR must be counted: it is the signal both destructive \
         consumers fail-close on. Uncounted, the § 9 gate never fires and the \
         `__index` boot purge deletes against an incomplete reachable set"
    );
    assert_eq!(
        r.direct_blob_refs, 0,
        "an unreadable reference must NOT be classified direct-blob — that answer \
         is what skips the chunk walk"
    );
}

/// The `__` namespace belongs to the nest: `fauna.folders.create` refuses every
/// reserved name (`reserved-folders.md` § The management surface refuses the
/// namespace — whole). A client-created reserved set's records would land in
/// `sync_changes` under a reserved name (the § 9 direct-blob hazard above), and
/// the custody copy a backup destination needs is provisioned by the nest itself
/// on the first custody record — never created by a client.
#[tokio::test]
async fn folders_create_guards_the_reserved_namespace() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db.clone()));
    let router = {
        let mut b = RpcRouter::builder();
        fauna_nest::folder_handlers::register_folders_handlers(&mut b);
        b.build()
    };
    let create = |state: Arc<AppState>, actor: [u8; 32], name: &str| {
        let req = FolderCreateRequest {
            name: name.to_string(),
            ..Default::default()
        };
        let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
        let meta = router
            .kind_meta("fauna.folders.create")
            .expect("create kind registered");
        (meta.handler)(state, actor, payload)
    };

    let actor: [u8; 32] = [0x77; 32];
    common::seed_dispatch_actor(&db, &actor).await;

    for name in ["__attack", "__mail", "__conv/deadbeef"] {
        let err = create(state.clone(), actor, name)
            .await
            .expect_err("a reserved name must be rejected");
        assert_eq!(err.code, "fauna.folders.invalid_request", "{name}");
        assert!(
            !err.code.ends_with(".conflict"),
            "must not be a conflict code — `is_already_exists` swallows exactly those"
        );
        assert!(
            db.get_folder_for_actor(name, &actor)
                .await
                .unwrap()
                .is_none(),
            "no row may be created for the reserved name {name}"
        );
    }
}

/// `changes.record` is the belt to `create`'s braces: even for a reserved set
/// that already exists (a nest-minted rail), a device-sync record is
/// refused, because it would land in `sync_changes` under a reserved name.
#[tokio::test]
async fn changes_record_refuses_a_reserved_folder() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: svc_over(&db, tmp.path()),
            ..AppState::for_test(db.clone())
        })
    };

    let kp = common::signing_actor(0x53);
    let actor: [u8; 32] = kp.actor_id().0;
    let device: [u8; 32] = [0x0B; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "device", None, "write")
        .await
        .unwrap();
    db.get_or_create_reserved_folder(&actor, "mail")
        .await
        .unwrap();

    let (chunk, len) = put_chunk(&store, &db, b"chunk the client wants to hide").await;
    let manifest = put_manifest_over(&store, &db, chunk, len).await;

    let err = try_record_sync(
        &state,
        &kp,
        &device_hex,
        "__mail",
        "notes/live.txt",
        Some(manifest),
    )
    .await
    .expect_err("a device-sync record against a reserved set must be rejected");
    assert_eq!(err.code, "fauna.sync.invalid_request", "{err:?}");
}

/// A known issue
/// (`backup-restore.md` § 9): any authenticated user can upload arbitrary
/// bytes through `/api/v1/chunks` (the route verifies the storage key against
/// the bytes, so the blob is a hash-INTACT preimage of its content address)
/// and record that hash as the `manifest_hash` of a file in their OWN ordinary
/// set. The reference is manifest-class and undecodable — but the bytes are
/// intact under their content address, and a content address is immutable, so
/// they provably NEVER decoded as a manifest at any point: no chunk was ever
/// enumerable through this reference, and there is nothing for the fail-close
/// to protect. Sweeping must proceed for the rest of the box; only a
/// hash-MISMATCH (bytes that are not the preimage of their key — genuine
/// at-rest corruption) may fail-close the run.
///
/// No attacker is even required: the android engine records
/// `manifest.fileHash` instead of the manifest-blob key, and for a
/// single-chunk file that IS the chunk's content address — a present,
/// hash-intact, undecodable "manifest" reference, from a stock client.
#[tokio::test]
async fn gc_sweep_survives_a_recorded_reference_that_was_never_a_manifest() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();
    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: svc_over(&db, tmp.path()),
            ..AppState::for_test(db.clone())
        })
    };

    // The poisoning actor: an ordinary sync set + junk bytes staged exactly as
    // `/api/v1/chunks` stores them (content-addressed on the bytes).
    let kp = common::signing_actor(0x66);
    let actor: [u8; 32] = kp.actor_id().0;
    let device: [u8; 32] = [0x0C; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "device", None, "write")
        .await
        .unwrap();
    db.create_folder_with_options("docs", &actor, nonce_set())
        .await
        .unwrap();
    let (junk, _len) = put_chunk(&store, &db, b"junk bytes that never were a manifest").await;
    try_record_sync(&state, &kp, &device_hex, "docs", "junk.bin", Some(junk))
        .await
        .expect("the record itself is accepted today (log-and-accept posture)");

    // A DIFFERENT actor's healthy live file — must stay untouched either way.
    let victim_kp = common::signing_actor(0x67);
    let victim: [u8; 32] = victim_kp.actor_id().0;
    let vdevice: [u8; 32] = [0x0D; 32];
    let vdevice_hex = hex::encode(vdevice);
    db.register_sync_device(&victim, &vdevice, "device", None, "write")
        .await
        .unwrap();
    db.create_folder_with_options("photos", &victim, nonce_set())
        .await
        .unwrap();
    let (v_chunk, v_len) = put_chunk(&store, &db, b"the victim's live photo chunk").await;
    let v_manifest = put_manifest_over(&store, &db, v_chunk, v_len).await;
    try_record_sync(
        &state,
        &victim_kp,
        &vdevice_hex,
        "photos",
        "photo.jpg",
        Some(v_manifest),
    )
    .await
    .expect("victim record ok");

    // …and a true orphan of the victim's (a superseded old blob): the thing
    // reclamation exists for. One actor's junk reference must not stop it.
    let orphan = ContentHash::of_raw(b"the victim's reclaimable orphan");
    store
        .put(&orphan, b"the victim's reclaimable orphan")
        .await
        .unwrap();
    db.put_blob_metadata(&orphan.digest(), 31, "chunk", None, None)
        .await
        .unwrap();

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
        r.manifest_decode_failures, 0,
        "a hash-intact never-a-manifest reference is not corruption and must \
         not count toward the fail-close gate — counting it hands any single \
         client permanent, nest-wide, cross-actor disable of blob reclamation"
    );
    assert!(
        !exists(&store, &orphan).await,
        "DoS: the sweep never ran — one client's junk reference fail-closed \
         reclamation for every actor on the box"
    );
    assert!(
        exists(&store, &junk).await,
        "the junk blob itself stays pinned — it is referenced, and over-pinning \
         is the safe direction"
    );
    assert!(
        exists(&store, &v_chunk).await && exists(&store, &v_manifest).await,
        "the victim's live file survives"
    );
}
