//! tier_3: held-for-friends backup — a holding nest hosts a friend's encrypted
//! backup against the OWNER-ADMINISTERED substrate (no new authorization
//! primitive).
//!
//! Proof obligations (`docs/goal/behavior/backup-destinations.md` § State & data shape →
//! *Held-for-friends enrollment*; `docs/goal/behavior/admin.md` § 2 Users;
//! `docs/goal/architecture/encryption-at-rest.md` row 119):
//! - (#3) a HANDLE-LESS guest admitted at the storage-only `backup` tier owns
//!   its OWN reserved custody copy, provisioned by the nest on the guest's
//!   first custody record through the normal `User`-class authz path
//!   (`fauna.sync.changes.record`) — not an admin-only operation, and never a
//!   client create (`reserved-folders.md` § Destination capability);
//! - (a) the guest's backup is recorded under the guest's own identity (its
//!   own per-actor reserved set + custody);
//! - (b) the holder stores opaque chunks with NO decrypt path — it never holds
//!   the guest's `BackupKey`, so the whole custody/GC lifecycle runs
//!   KEYLESS (`encryption_key = None`), never interpreting chunk content;
//! - (d) the holder "stops hosting" via the EXISTING eviction finalize
//!   (`pending_actions::finalize_user_deletion`, the step the
//!   `admin.delete_user` pending action runs) → the guest's reserved sets
//!   + `backup_custody` drop → the next GC reclaims their chunks.
//!
//! Criterion (c) "upload past `max_storage_bytes` rejected" is proven by the
//! second test (`held_for_friends_guest_over_cap_rejected`) — the per-actor
//! storage-quota enforcement (mirrors the
//! inbox-quota reject; uniform across all tiers, proven here at the `backup`
//! tier).

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::ContentHash;
use fauna_nest::backup::gc::garbage_collect;
use fauna_nest::backup::service::BackupService;
use fauna_nest::blob_store::{BlobStoreBackend, DiskBlobStore};
use fauna_nest::db::CacheDb;
use fauna_nest::folder_handlers::register_folders_handlers;
use fauna_nest::pending_actions::finalize_user_deletion;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::sync_handlers::register_sync_handlers;
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

mod common;
use common::{put_chunk, put_manifest_over};

/// The blob GC's record walk (`gc.rs` step 2f) reads live post bodies from a
/// `__post` segment store; these tests seed no posts, so an empty throwaway
/// one is the honest source.
fn post_segments(tmp: &tempfile::TempDir) -> fauna_segment_store::SegmentManager {
    fauna_segment_store::SegmentManager::new(tmp.path().join("post-segments"), "post")
}

/// Drive the production `fauna.sync.changes.record` handler as `actor`,
/// returning the raw handler result so a caller can assert success OR a typed
/// rejection (criterion (c) — over-`max_storage_bytes` reject). `size_bytes` is
/// the client-asserted logical size charged against the actor's storage quota.
async fn try_record(
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest_hash: ContentHash,
    size_bytes: u64,
) -> Result<SyncChangeRecordReply, RpcError> {
    let req = SyncChangeRecordRequest {
        nest_url: None,
        channel_id: None,
        folder: folder.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: Some(hex::encode(manifest_hash.digest())),
        size_bytes: size_bytes as i64,
        change_type: "create".to_string(),
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let meta = state
        .rpc_router
        .kind_meta("fauna.sync.changes.record")
        .expect("changes.record kind registered");
    let reply_bytes = (meta.handler)(state.clone(), actor, payload).await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

/// Record a backup manifest and assert the handler accepts it (the common case).
async fn record(
    state: &Arc<AppState>,
    actor: [u8; 32],
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest_hash: ContentHash,
    size_bytes: u64,
) {
    let reply = try_record(
        state,
        actor,
        device_hex,
        folder,
        path,
        manifest_hash,
        size_bytes,
    )
    .await
    .expect("record handler ok");
    assert_eq!(reply.seq, 0, "custody-copy record returns seq 0");
}

async fn exists(store: &DiskBlobStore, h: &ContentHash) -> bool {
    store.exists(h).await.unwrap()
}

#[tokio::test]
async fn held_for_friends_guest_backup_lifecycle() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());
    let dyn_store: Arc<dyn BlobStoreBackend> = store.clone();

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            register_folders_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: Some(Arc::new(
                BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None)
                    .unwrap(),
            )),
            ..AppState::for_test(db.clone())
        })
    };

    // The holding nest's admin admitted the data owner at the storage-only
    // `backup` tier through the existing admission lifecycle. The guest's row is
    // HANDLE-LESS (`create_user` leaves `handle = ''`) — no MX / inbox / feeds.
    let guest: [u8; 32] = [0x42; 32];
    db.create_user(&guest, "backup", "friend-of-the-admin")
        .await
        .expect("admit guest at backup tier");
    let backup_tier = db
        .get_user(&guest)
        .await
        .unwrap()
        .expect("guest user row exists");
    assert_eq!(backup_tier.tier, "backup", "guest admitted at backup tier");

    // The coordinator's write-capable device on the holding nest.
    let device: [u8; 32] = [0x07; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&guest, &device, "backup-device", None, "write")
        .await
        .unwrap();

    // ── (a) The guest records its backup (sealed chunks + manifest mirror). ──
    let (c1, l1) = put_chunk(&store, &db, b"opaque-ciphertext-segment-1").await;
    let m1 = put_manifest_over(&store, &db, c1, l1).await;
    record(
        &state,
        guest,
        &device_hex,
        "__mail",
        "4242.../seg-1.dat",
        m1,
        l1,
    )
    .await;

    // ── (#3) The handle-less guest (a non-admin `User`-class actor) now owns
    //    its OWN reserved custody copy: the first record provisioned it through
    //    the normal authz path. ─────────────────────────────────────────────────
    let fs = db
        .get_folder_for_actor("__mail", &guest)
        .await
        .unwrap()
        .expect("the first custody record provisions the guest's set");
    assert!(fs.custody_copy, "the guest's set is a custody copy");

    let (c2, l2) = put_chunk(&store, &db, b"opaque-ciphertext-segment-2-distinct").await;
    let m2 = put_manifest_over(&store, &db, c2, l2).await;
    record(
        &state,
        guest,
        &device_hex,
        "__mail",
        "4242.../seg-2.dat",
        m2,
        l2,
    )
    .await;

    let (cm, lm) = put_chunk(&store, &db, b"opaque-manifest-mirror").await;
    let mm = put_manifest_over(&store, &db, cm, lm).await;
    record(
        &state,
        guest,
        &device_hex,
        "__mail",
        "4242.../manifest.mail",
        mm,
        lm,
    )
    .await;

    let all_blobs = [c1, m1, c2, m2, cm, mm];

    // The backup never enters the device-sync feed (custody-only).
    assert!(
        db.get_sync_changes_for_folder(fs.id, 0, None)
            .await
            .unwrap()
            .is_empty(),
        "custody-copy records stay out of the device-sync feed",
    );

    // ── (b) The holder stores opaque chunks with NO decrypt path: the entire
    //    custody/GC lifecycle runs KEYLESS (the holder never holds the guest's
    //    BackupKey), and GC must preserve every live backup blob. ─────────────
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
    assert_eq!(r.deleted_blobs, 0, "GC must not delete a live backup blob");
    for h in all_blobs {
        assert!(exists(&store, &h).await, "live backup blob survives GC");
    }

    // ── (d) The holder "stops hosting" via the EXISTING eviction finalize (the
    //    step the `admin.delete_user` pending action runs). The guest's reserved
    //    sets + custody drop, so the next GC reclaims all their chunks. ────────
    finalize_user_deletion(&state, &guest)
        .await
        .expect("eviction finalize");

    assert!(
        db.get_user(&guest).await.unwrap().is_none(),
        "the evicted guest's user row is gone",
    );
    assert!(
        db.get_folder_for_actor("__mail", &guest)
            .await
            .unwrap()
            .is_none(),
        "the evicted guest's reserved backup set is reclaimed",
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
        r.deleted_blobs,
        all_blobs.len() as u64,
        "eviction drops custody → GC reclaims every one of the guest's chunks + manifests",
    );
    for h in all_blobs {
        assert!(
            !exists(&store, &h).await,
            "the evicted guest's backup blob is reclaimed",
        );
    }
}

/// (c) "upload past `max_storage_bytes` rejected" — the abuse bound that stops a
/// held-for-friends guest from filling the holder's disk. The holder's admin
/// offers a fixed slice of space (the `backup` tier's admin-editable
/// `max_storage_bytes`, `docs/goal/behavior/admin.md` § 2 Users); a guest record
/// that would push the guest's `users.storage_bytes_used` past that cap is
/// rejected with the typed storage-quota error, and a rejected record charges
/// nothing. Enforcement is general all-tiers (mirror the inbox-quota reject) —
/// proven here at the `backup` tier (`docs/goal/behavior/backup-destinations.md` § Held-for-friends
/// enrollment; `docs/goal/architecture/encryption-at-rest.md` row 119).
#[tokio::test]
async fn held_for_friends_guest_over_cap_rejected() {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskBlobStore::new(tmp.path()).unwrap());

    let state = {
        let rpc_router = Arc::new({
            let mut b = RpcRouter::builder();
            register_sync_handlers(&mut b);
            register_folders_handlers(&mut b);
            b.build()
        });
        Arc::new(AppState {
            rpc_router,
            backup_service: Some(Arc::new(
                BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None)
                    .unwrap(),
            )),
            ..AppState::for_test(db.clone())
        })
    };

    // Stage the two records' bytes up front so the tier cap below can be sized
    // to the DESTINATION-DERIVED charge — on a reserved destination set the
    // charge is what this nest holds under the manifest (manifest blob + its
    // chunks, as metered at upload), never the writer's declared size.
    let (c1, l1) = put_chunk(&store, &db, b"opaque-ciphertext-segment-1").await;
    let m1 = put_manifest_over(&store, &db, c1, l1).await;
    let m1_len = db
        .get_blob_metadata(&m1.digest())
        .await
        .unwrap()
        .unwrap()
        .size_bytes;
    let charge1 = l1 as i64 + m1_len;
    let (c2, l2) = put_chunk(&store, &db, b"opaque-ciphertext-segment-2-distinct").await;
    let m2 = put_manifest_over(&store, &db, c2, l2).await;

    // The holder's admin offers the friend a SMALL slice of space: tighten the
    // `backup` tier's `max_storage_bytes` so exactly one record fits
    // (admin-editable; the seed default is 50 GiB). The tier *is* the quota —
    // no per-user override.
    let mut backup_tier = db
        .get_tier("backup")
        .await
        .unwrap()
        .expect("backup tier seeded");
    backup_tier.max_storage_bytes = charge1;
    db.update_tier(&backup_tier).await.unwrap();

    // Admit the guest at the (now small-capped) backup tier; it owns its own
    // backup set + a write-capable device (the admission substrate proven above).
    let guest: [u8; 32] = [0x42; 32];
    db.create_user(&guest, "backup", "friend-of-the-admin")
        .await
        .expect("admit guest at backup tier");

    let device: [u8; 32] = [0x07; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&guest, &device, "backup-device", None, "write")
        .await
        .unwrap();

    // ── An under-cap record (charge1 ≤ cap) is accepted and CHARGED the
    //    destination-derived amount — the declared 60 is ignored. ─────────────
    record(&state, guest, &device_hex, "__mail", "p/seg-1.dat", m1, 60).await;
    assert_eq!(
        db.get_user(&guest)
            .await
            .unwrap()
            .expect("guest row")
            .storage_bytes_used,
        charge1,
        "an accepted backup record charges what the destination holds",
    );

    // ── An over-cap record (a second segment past the one-record cap) is
    //    REJECTED with the typed storage-quota error, and the rejected record
    //    charges NOTHING. ────────────────────────────────────────────────────
    let err = try_record(&state, guest, &device_hex, "__mail", "p/seg-2.dat", m2, 60)
        .await
        .expect_err("recording past max_storage_bytes must be rejected");
    assert_eq!(
        err.code, "fauna.sync.storage_quota_exceeded",
        "an over-cap record returns the typed storage-quota error",
    );
    assert_eq!(
        db.get_user(&guest)
            .await
            .unwrap()
            .expect("guest row")
            .storage_bytes_used,
        charge1,
        "a rejected record leaves the quota unchanged (no partial charge)",
    );
}
