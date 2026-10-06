//! tier_3: per-actor storage-quota accounting + enforcement for the GENERAL
//! (non-backup) case — a normal `sync`-mode folder on a `free` / `personal`
//! tier. The held-for-friends sibling (`segment_backup_held_for_friends.rs`)
//! proves the `backup`-tier custody path; this proves the same one code path is
//! UNIFORM across tiers (priorities #1/#4; `docs/goal/behavior/admin.md` § 2
//! Users — "the tier *is* the quota", no per-user override) and that
//! supersede / folder-delete return headroom.
//!
//! Asserts, driving the production `fauna.sync.changes.record` WS-RPC handler:
//!  - a record under the tier `max_storage_bytes` increments
//!    `users.storage_bytes_used` (the column stops being dead);
//!  - a record that would exceed the cap is rejected with the typed
//!    `fauna.sync.storage_quota_exceeded` error, charging nothing;
//!  - retained accounting (`file-versions.md` § Retention (4)): a shrink
//!    charges its own size and returns NO headroom (the retained version keeps
//!    charging); headroom comes back when the owner supersede releases the
//!    retained row, and only then does a previously-rejected record fit;
//!  - deleting the folder reclaims all of its accounted bytes.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::folder_handlers::register_folders_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::sync_handlers::register_sync_handlers;
use fauna_protocol::folders::{FolderCreateReply, FolderCreateRequest};
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

/// Build an `AppState` wired with the sync + folder handlers over a fresh
/// in-memory DB.
fn test_state(db: Arc<CacheDb>) -> Arc<AppState> {
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_sync_handlers(&mut b);
        register_folders_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        ..AppState::for_test(db)
    })
}

/// Drive `fauna.sync.changes.record` as `actor`, returning the raw handler
/// result so a caller can assert success OR a typed quota rejection. The
/// record is signed as a production writer signs it — by the recorder's
/// identity key under the set's stored nonce ([`common::SET_NONCE`], which the
/// flow creates the set under); an unsigned record is refused
/// `signature_required` before the quota is ever consulted.
async fn try_record(
    state: &Arc<AppState>,
    actor: &ActorKeypair,
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest: [u8; 32],
    size_bytes: u64,
) -> Result<SyncChangeRecordReply, RpcError> {
    let req = SyncChangeRecordRequest {
        path_sealed: Some(fauna_protocol::ByteBuf::from(
            b"e2e-synthetic-seal".to_vec(),
        )),
        nest_url: None,
        channel_id: None,
        folder: folder.to_string(),
        device_id: device_hex.to_string(),
        path: path.to_string(),
        manifest_hash: Some(hex::encode(manifest)),
        size_bytes: size_bytes as i64,
        change_type: "create".to_string(),
        content_key_version: None,
        thumbnail_hash: None,
        ..Default::default()
    };
    let req = common::signed_record(req, actor);
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let meta = state
        .rpc_router
        .kind_meta("fauna.sync.changes.record")
        .expect("changes.record kind registered");
    let reply_bytes = (meta.handler)(state.clone(), actor.actor_id().0, payload).await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

/// Record and assert the handler accepts it, returning the assigned seq.
async fn record(
    state: &Arc<AppState>,
    actor: &ActorKeypair,
    device_hex: &str,
    folder: &str,
    path: &str,
    manifest: [u8; 32],
    size_bytes: u64,
) -> i64 {
    let reply = try_record(state, actor, device_hex, folder, path, manifest, size_bytes)
        .await
        .expect("record handler ok");
    assert!(reply.seq > 0, "an ordinary record assigns a monotonic seq");
    reply.seq
}

async fn storage_used(db: &CacheDb, actor: &[u8; 32]) -> i64 {
    db.get_user(actor)
        .await
        .unwrap()
        .expect("user row")
        .storage_bytes_used
}

/// The full accounting + enforcement + reclaim flow for one tier, capped small.
async fn run_quota_flow(tier: &str) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = test_state(db.clone());

    // The tier *is* the quota: tighten this tier's max_storage_bytes to 100 B.
    let mut row = db
        .get_tier(tier)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{tier} tier seeded"));
    row.max_storage_bytes = 100;
    db.update_tier(&row).await.unwrap();

    let kp = common::signing_actor(0x11);
    let actor: [u8; 32] = kp.actor_id().0;
    db.create_user(&actor, tier, "a-user").await.unwrap();

    // A normal device-sync (ordinary) folder + a write-capable device.
    let create = FolderCreateRequest {
        name: "docs".to_string(),
        retention_policy: None,
        set_nonce: common::set_nonce_field(),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&create).unwrap().to_vec());
    let meta = state.rpc_router.kind_meta("fauna.folders.create").unwrap();
    let reply = (meta.handler)(state.clone(), actor, payload)
        .await
        .expect("create sync set");
    let _: FolderCreateReply = decode(&reply).expect("decode create reply");

    let device: [u8; 32] = [0x22; 32];
    let device_hex = hex::encode(device);
    db.register_sync_device(&actor, &device, "dev", None, "write")
        .await
        .unwrap();

    // (1) Under-cap record (60 ≤ 100) is accepted and charged.
    record(&state, &kp, &device_hex, "docs", "a.txt", [0xA1; 32], 60).await;
    assert_eq!(
        storage_used(&db, &actor).await,
        60,
        "[{tier}] an accepted record charges storage_bytes_used (column no longer dead)",
    );

    // (2) Over-cap record (60 + 60 > 100) is rejected; nothing charged.
    let err = try_record(&state, &kp, &device_hex, "docs", "b.txt", [0xB1; 32], 60)
        .await
        .expect_err("over-cap record rejected");
    assert_eq!(
        err.code, "fauna.sync.storage_quota_exceeded",
        "[{tier}] over-cap record returns the typed storage-quota error",
    );
    assert_eq!(
        storage_used(&db, &actor).await,
        60,
        "[{tier}] a rejected record charges nothing",
    );

    // (3) Modify a.txt to a smaller manifest (60 → 10): retained accounting
    //     (`file-versions.md` § Retention (4)) — the former head stays
    //     listable as a retained version and KEEPS charging, so the shrink
    //     charges its own 10 B and returns no headroom (60 + 10 = 70).
    record(&state, &kp, &device_hex, "docs", "a.txt", [0xA2; 32], 10).await;
    assert_eq!(
        storage_used(&db, &actor).await,
        70,
        "[{tier}] a shrink charges its own size; the retained version keeps charging",
    );

    // (4) Headroom comes back only when a version actually leaves the charged
    //     population — the owner's M2 supersede releases a.txt's retained
    //     60 B row (70 → 10), and only then does the previously-rejected
    //     record fit (10 + 60 = 70 ≤ 100).
    let err = try_record(&state, &kp, &device_hex, "docs", "b.txt", [0xB1; 32], 60)
        .await
        .expect_err("still over-cap while the retained version charges");
    assert_eq!(
        err.code, "fauna.sync.storage_quota_exceeded",
        "[{tier}] retained bytes hold the quota until released",
    );
    let a_hash = fauna_core::sync::path_hash("a.txt");
    let outcome = db
        .supersede_sync_changes_for_path(
            db.list_folders()
                .await
                .unwrap()
                .into_iter()
                .find(|f| f.name == "docs")
                .expect("docs folder listed")
                .id,
            &a_hash,
            &[0xA2; 32],
        )
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            fauna_nest::db::sync_storage::SupersedeOutcome::Marked(1)
        ),
        "[{tier}] the supersede releases exactly the retained row, got {outcome:?}",
    );
    assert_eq!(
        storage_used(&db, &actor).await,
        10,
        "[{tier}] the release credits the retained bytes back",
    );
    record(&state, &kp, &device_hex, "docs", "b.txt", [0xB1; 32], 60).await;
    assert_eq!(
        storage_used(&db, &actor).await,
        70,
        "[{tier}] a record that now fits within released headroom is accepted",
    );

    // (5) Deleting the folder reclaims all of its accounted bytes (the
    //     production `delete_folder_for_user` path the delete handler runs).
    let deleted = db.delete_folder_for_user("docs", &actor).await.unwrap();
    assert!(deleted, "[{tier}] folder deleted");
    assert_eq!(
        storage_used(&db, &actor).await,
        0,
        "[{tier}] deleting the folder reclaims all its accounted storage",
    );
}

#[tokio::test]
async fn storage_quota_uniform_on_free_tier() {
    run_quota_flow("free").await;
}

#[tokio::test]
async fn storage_quota_uniform_on_personal_tier() {
    run_quota_flow("personal").await;
}
