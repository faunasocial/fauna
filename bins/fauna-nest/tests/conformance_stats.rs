//! Integration round-trip for the authenticated stats surface —
//! `fauna.stats.get`. A behavior-preserving transport migration of the
//! (now-deleted) bearer-authed HTTP route `GET /api/v1/stats`. The
//! handler reuses the same `backup::stats::{compute_global_stats,
//! compute_folder_stats}` the HTTP twin called — these tests exercise the
//! WS-RPC layer: request decode (the `folder` param replacing the twin's
//! query string), the discriminated reply (`global` / `folder`), the f64→i64
//! `dedup_ratio_micro` scaling, the `not_found` mapping, replay metadata, and
//! the `User | Admin` allowlist.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/stats.rs`.
//! Slice: tracked internally (Track B17 of the WS-RPC-everywhere
//! migration).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
    stats_handlers,
};
use fauna_protocol::{
    decode_strict as decode,
    stats::{StatsGetReply, StatsGetRequest},
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    stats_handlers::register_stats_handlers(&mut b);
    (b.build(), state)
}

// ── global ──────────────────────────────────────────────────────

#[tokio::test]
async fn global_returns_global_shape() {
    let (router, state) = router_and_state().await;
    // The global branch is Admin-only (ST-1): nest-wide totals are a deployment
    // metric. Seed `[11u8; 32]` as an admin so the empty-repo shape resolves.
    let admin = [11u8; 32];
    state.db.add_admin_actor(&admin[..]).await.unwrap();
    let reply: StatsGetReply = decode(
        &dispatch(
            &router,
            state,
            admin,
            "fauna.stats.get",
            encode(&StatsGetRequest {
                folder: None,
                ..Default::default()
            }),
        )
        .await
        .expect("global stats ok"),
    )
    .unwrap();

    match reply {
        StatsGetReply::Global {
            total_size_bytes,
            total_blobs,
            total_snapshots,
            total_folders,
            dedup_ratio_micro,
            blob_types,
        } => {
            assert_eq!(total_size_bytes, 0);
            assert_eq!(total_blobs, 0);
            assert_eq!(total_snapshots, 0);
            assert_eq!(total_folders, 0);
            // Empty repo: dedup_ratio 1.0 → 1e6.
            assert_eq!(dedup_ratio_micro, 1_000_000);
            assert_eq!(blob_types.chunk, 0);
            assert_eq!(blob_types.manifest, 0);
        }
        StatsGetReply::Folder { .. } | StatsGetReply::Unknown => {
            panic!("expected Global variant for folder = None")
        }
    }
}

// ── folder ────────────────────────────────────────────────────

#[tokio::test]
async fn folder_returns_folder_shape() {
    let (router, state) = router_and_state().await;
    let actor = [22u8; 32];
    state.db.create_folder("photos", &actor).await.unwrap();

    let reply: StatsGetReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.stats.get",
            // As an app sends it: the funnel stamps the hash and takes the
            // plaintext off, so the hash alone must select the per-folder arm
            // (never the admin-only nest-wide one).
            encode(&fauna_protocol::folders::addressed(StatsGetRequest {
                folder: Some("photos".into()),
                ..Default::default()
            })),
        )
        .await
        .expect("folder stats ok"),
    )
    .unwrap();

    match reply {
        StatsGetReply::Folder {
            folder,
            snapshot_count,
            latest_snapshot,
            total_files,
            raw_size_bytes,
            stored_size_bytes,
            dedup_ratio_micro,
            storage_backend,
        } => {
            assert_eq!(folder, "photos");
            assert_eq!(snapshot_count, 0);
            assert!(latest_snapshot.is_none());
            assert_eq!(total_files, 0);
            assert_eq!(raw_size_bytes, 0);
            assert_eq!(stored_size_bytes, 0);
            // No stored bytes: dedup_ratio 1.0 → 1e6.
            assert_eq!(dedup_ratio_micro, 1_000_000);
            // No S3 destinations → local.
            assert_eq!(storage_backend, "local");
        }
        StatsGetReply::Global { .. } | StatsGetReply::Unknown => {
            panic!("expected Folder variant for a named folder")
        }
    }
}

#[tokio::test]
async fn folder_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [33u8; 32],
        "fauna.stats.get",
        encode(&StatsGetRequest {
            folder: Some("does-not-exist".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("missing folder → not_found");
    assert_eq!(err.code, "fauna.stats.not_found");
}

// ── ST-1: cross-user authorization ──────────────────────────────
//
// Before ST-1, `fauna.stats.get`'s folder branch resolved the set via the
// name-only `get_folder` (no owner predicate), so any authenticated `User`
// could read another user's aggregate backup stats — the N1 IDOR class, one
// surface over. The folder branch is now owner-scoped (a non-owner gets
// `not_found`, which also hides the set's existence); the global branch is
// Admin-only (nest-wide totals are a deployment metric).

#[tokio::test]
async fn folder_rejects_non_owner() {
    let (router, state) = router_and_state().await;
    let owner = [22u8; 32];
    let attacker = [99u8; 32];
    // Owner's set. `attacker` is a distinct (unregistered → User) actor that
    // passes the kind allowlist; only the new owner scope can stop the read.
    state.db.create_folder("photos", &owner).await.unwrap();

    let err = dispatch(
        &router,
        state,
        attacker,
        "fauna.stats.get",
        encode(&StatsGetRequest {
            folder: Some("photos".into()),
            ..Default::default()
        }),
    )
    .await
    .expect_err("cross-user folder stats must be denied");
    // Folded into `not_found` (existence oracle closed), per the folder-name
    // kind idiom (`filesync_handlers` prune/check), not `permission_denied`.
    assert_eq!(err.code, "fauna.stats.not_found");
}

#[tokio::test]
async fn global_rejects_non_admin() {
    let (router, state) = router_and_state().await;
    // A regular (unregistered → CallerClass::User) actor — passes the kind
    // allowlist (User | Admin) but not the in-handler Admin gate on the global
    // branch.
    let err = dispatch(
        &router,
        state,
        [44u8; 32],
        "fauna.stats.get",
        encode(&StatsGetRequest {
            folder: None,
            ..Default::default()
        }),
    )
    .await
    .expect_err("nest-wide stats must be denied to a non-admin");
    assert_eq!(err.code, "fauna.stats.permission_denied");
}

// ── malformed payload ───────────────────────────────────────────

#[tokio::test]
async fn rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.stats.get",
        Bytes::from_static(&[0xff, 0xff, 0xff]),
    )
    .await
    .expect_err("malformed payload rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata ─────────────────────────────────────────────

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_and_state().await;
    let m = router
        .kind_meta("fauna.stats.get")
        .expect("kind registered");
    assert!(!m.forbid_replay, "stats.get does not forbid replay");
    assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
}

// ── allowlist ───────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_user_admin_permitted_bridges_denied() {
    for class in [CallerClass::User, CallerClass::Admin] {
        assert!(
            is_permitted(class, "fauna.stats.get"),
            "fauna.stats.get permitted for {class:?}"
        );
    }
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(
            !is_permitted(class, "fauna.stats.get"),
            "fauna.stats.get denied for {class:?}"
        );
    }
}
