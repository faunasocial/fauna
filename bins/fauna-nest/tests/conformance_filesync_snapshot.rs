//! Integration round-trip for the folder snapshot control surface —
//! `fauna.filesync.snapshot.{create_folder,get,delete,undelete,prune,
//! check,diff}` + the `list` folder fold-in. A behavior-preserving
//! transport migration of the 8 folder snapshot JSON HTTP twins
//! (the since-deleted `snapshot_routes` + `stats_routes` twins). The handlers
//! reuse the same `db::*`/`backup::*` calls the twins used — these tests
//! exercise the WS-RPC layer: request decode, reply encoding, the queued-
//! delete pending-action dance, the hard floor, retention prune, the
//! folder `list` fold-in (`message_kind == None`), and the diff
//! same/different-folder behaviors.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/filesync.rs`.
//! Slice: tracked internally (Track B15 of the WS-RPC-everywhere
//! migration).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks).

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use fauna_nest::{
    db::{CacheDb, FolderUpdate},
    filesync_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    ByteBuf, decode_strict as decode,
    filesync::{
        SnapshotCheckRequest, SnapshotCreateFolderReply, SnapshotCreateFolderRequest,
        SnapshotDeleteReply, SnapshotDeleteRequest, SnapshotDiffReply, SnapshotDiffRequest,
        SnapshotGetReply, SnapshotGetRequest, SnapshotListReply, SnapshotListRequest,
        SnapshotPruneReply, SnapshotPruneRequest, SnapshotPruneSetPolicyReply,
        SnapshotPruneSetPolicyRequest, SnapshotRetentionPolicy, SnapshotStampLabelsRequest,
        SnapshotUndeleteRequest,
    },
};

const ACTOR_A: [u8; 32] = [11u8; 32];

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    filesync_handlers::register_filesync_handlers(&mut b);
    (b.build(), state)
}

#[tokio::test]
async fn create_folder_then_get_round_trips() {
    let (router, state) = router_and_state().await;
    state.db.create_folder("documents", &ACTOR_A).await.unwrap();

    let reply: SnapshotCreateFolderReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.create_folder",
            encode(&SnapshotCreateFolderRequest {
                folder: "documents".into(),
                tags: vec!["nightly".into()],
                device_id: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    assert_eq!(reply.tags, vec!["nightly".to_string()]);

    // get the created snapshot — folder row → message_kind None.
    let got: SnapshotGetReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.get",
            encode(&SnapshotGetRequest {
                snapshot_id: reply.id,
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();
    assert_eq!(got.id, reply.id);
    assert_eq!(got.folder, "documents");
    assert_eq!(got.message_kind, None);
    assert_eq!(got.actor_id, None);
}

#[tokio::test]
async fn get_unknown_snapshot_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.get",
        encode(&SnapshotGetRequest {
            snapshot_id: 9999,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("not found");
    assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
}

#[tokio::test]
async fn list_folder_mode_returns_rows_message_kind_none() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("photos", &ACTOR_A).await.unwrap();
    state.db.insert_snapshot_at(fs, 100).await.unwrap();
    state.db.insert_snapshot_at(fs, 200).await.unwrap();

    let reply: SnapshotListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.list",
            encode(&SnapshotListRequest {
                message_kind: None,
                folder: Some("photos".into()),
                limit: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(reply.rows.len(), 2);
    assert!(reply.rows.iter().all(|r| r.message_kind.is_none()));

    // Owner-implicit mode (no folder) lists only message-kind snapshots —
    // none seeded → empty (proves the two branches are distinct).
    let mk: SnapshotListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.list",
            encode(&SnapshotListRequest {
                message_kind: None,
                folder: None,
                limit: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert_eq!(mk.rows.len(), 0);
}

// ── The § 2 row lifecycle fields ────────────────────────────────────────────
//
// `backup-restore.md` § 2 *Row lifecycle fields*: folder mode deliberately
// returns soft-deleted and deletion-pending rows, and until these four fields
// shipped `SnapshotSummaryRow` carried nothing to tell them apart — so every
// app rendered a recoverable-deleted snapshot exactly like an active one and
// the § 7 windows were invisible from every UI.

/// Seconds since the epoch, as the nest's own `now_epoch_secs` computes it
/// (which is `pub(crate)`, hence the local twin — both now delegate to the
/// same `fauna_core::data::Timestamp::now_secs`).
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

async fn list_folder(
    router: &RpcRouter,
    state: &Arc<AppState>,
    name: &str,
) -> Vec<fauna_protocol::filesync::SnapshotSummaryRow> {
    let reply: SnapshotListReply = decode(
        &dispatch(
            router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.list",
            encode(&SnapshotListRequest {
                message_kind: None,
                folder: Some(name.into()),
                limit: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    reply.rows
}

#[tokio::test]
async fn list_projects_the_three_row_states_distinguishably() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    // Six, so the interactive delete below clears the 3-active hard floor.
    let mut ids = Vec::new();
    for ts in [100, 200, 300, 400, 500, 600] {
        ids.push(state.db.insert_snapshot_at(fs, ts).await.unwrap());
    }

    // One soft-deleted (inside its 30-day `purge_after` window) …
    state.db.soft_delete_snapshot(ids[0]).await.unwrap();
    // … and one deletion-pending, queued through the real wire kind so the
    // pending action carrying the deadline is the one production writes.
    let del: SnapshotDeleteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.delete",
            encode(&SnapshotDeleteRequest {
                snapshot_id: ids[1],
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete queues"),
    )
    .unwrap();

    let rows = list_folder(&router, &state, "docs").await;
    assert_eq!(rows.len(), 6, "all three classes stay listed");
    let by_id = |id: i64| rows.iter().find(|r| r.id == id).expect("row present");

    let soft = by_id(ids[0]);
    assert!(soft.soft_deleted);
    assert!(
        soft.purge_after.is_some_and(|p| p > now_secs()),
        "a soft-deleted row carries the future instant its recovery window closes"
    );
    assert!(!soft.deletion_pending);
    assert_eq!(
        soft.execute_after, None,
        "its action already fired — the remaining window is purge_after, not execute_after"
    );

    let pending = by_id(ids[1]);
    assert!(pending.deletion_pending);
    assert_eq!(
        pending.execute_after,
        Some(del.execute_after),
        "the row's cancel deadline is the pending action's own, not a re-derivation"
    );
    assert!(!pending.soft_deleted);
    assert_eq!(pending.purge_after, None);

    let active = by_id(ids[5]);
    assert!(!active.soft_deleted);
    assert!(!active.deletion_pending);
    assert_eq!(active.purge_after, None);
    assert_eq!(active.execute_after, None);
}

/// The half a `target`-only reader would silently drop.
///
/// An **automatic** prune marks a whole batch through one `SnapshotBulkPrune`
/// action whose `target` is the *folder* and whose snapshot ids live in
/// `payload.snapshot_ids` — so a deadline join that understood only
/// `snapshot.delete`'s single-id `target` would render every automatically
/// pruned row as a pending deletion with no date, which is precisely the state
/// a user needs the date for. This drives the real producer
/// (`backup::prune::schedule_auto_prune`), not a hand-forged action row.
#[tokio::test]
async fn list_projects_the_deadline_of_an_automatically_pruned_row() {
    let (router, state) = router_and_state().await;
    let fs_id = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    for ts in [100, 200, 300, 400, 500, 600] {
        state.db.insert_snapshot_at(fs_id, ts).await.unwrap();
    }
    state
        .db
        .update_folder_for_user(
            "docs",
            &ACTOR_A,
            FolderUpdate {
                retention_policy: Some(Some(r#"{"max_snapshots":4,"max_age_days":0}"#)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let fs = state
        .db
        .get_folder_for_actor("docs", &ACTOR_A)
        .await
        .unwrap()
        .expect("folder");
    let before = now_secs();
    let scheduled = fauna_nest::backup::prune::schedule_auto_prune(&state.db, &fs)
        .await
        .unwrap();
    assert_eq!(
        scheduled, 2,
        "6 snapshots, bound 4 → the 2 oldest scheduled"
    );

    let rows = list_folder(&router, &state, "docs").await;
    let pending: Vec<_> = rows.iter().filter(|r| r.deletion_pending).collect();
    assert_eq!(pending.len(), 2);
    for row in pending {
        let deadline = row.execute_after.expect(
            "an automatically-pruned row carries its cancel deadline — the batch's ids live in \
             the action payload, not in its target",
        );
        // The ratified 7-day SnapshotBulkPrune window, bounded generously on
        // both sides rather than pinned to a wall-clock instant.
        assert!(
            deadline >= before + 7 * 24 * 3600 && deadline <= now_secs() + 7 * 24 * 3600,
            "deadline {deadline} is not the 7-day bulk-prune window"
        );
    }
    assert!(
        rows.iter().all(|r| !r.soft_deleted),
        "Layer 2 only marks — nothing is soft-deleted until the window elapses"
    );
}

#[tokio::test]
async fn list_unknown_folder_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.list",
        encode(&SnapshotListRequest {
            message_kind: None,
            folder: Some("nope".into()),
            limit: 0,
            ..Default::default()
        }),
    )
    .await
    .expect_err("not found");
    assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
}

/// scope half: `name_hash` selects a set **on its own**, with no
/// plaintext `folder` beside it — the shape every app sends once S9 stops
/// sending cleartext names. Pre-fix, `name_hash` was read only inside the
/// `folder.is_some()` arm, so this request silently took the owner-implicit
/// message-kind branch instead of the set the caller addressed.
#[tokio::test]
async fn list_addresses_by_name_hash_alone() {
    let (router, state) = router_and_state().await;
    let photos = state.db.create_folder("photos", &ACTOR_A).await.unwrap();
    let docs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    state.db.insert_snapshot_at(photos, 100).await.unwrap();
    state.db.insert_snapshot_at(photos, 200).await.unwrap();
    state.db.insert_snapshot_at(docs, 300).await.unwrap();

    let reply: SnapshotListReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.list",
            encode(&SnapshotListRequest {
                message_kind: None,
                folder: None,
                name_hash: Some(ByteBuf::from(
                    fauna_core::path_crypto::set_name_hash("photos").to_vec(),
                )),
                limit: 0,
                ..Default::default()
            }),
        )
        .await
        .expect("hash-addressed list ok"),
    )
    .unwrap();
    assert_eq!(
        reply.rows.len(),
        2,
        "a hash-only request lists the addressed set's snapshots, not the \
         owner-implicit message-kind branch (which holds none here)"
    );
}

/// validation half — the part a scope-only fix leaves open. A
/// malformed hash must refuse even when no plaintext `folder` rides beside
/// it; before the fix the parse sat on the branch this request never takes, so
/// a 3-byte "digest" was silently accepted and the reply answered the wrong
/// question.
#[tokio::test]
async fn list_refuses_a_malformed_hash_with_no_plaintext_name() {
    let (router, state) = router_and_state().await;
    state.db.create_folder("photos", &ACTOR_A).await.unwrap();

    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.list",
        encode(&SnapshotListRequest {
            message_kind: None,
            folder: None,
            name_hash: Some(ByteBuf::from(vec![1u8, 2, 3])),
            limit: 0,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a 3-byte hash is not a BLAKE3 digest");
    assert_eq!(err.code, "fauna.filesync.snapshot.invalid_request");
}

#[tokio::test]
async fn delete_queues_pending_action_above_floor() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let mut ids = Vec::new();
    for ts in [100, 200, 300, 400] {
        ids.push(state.db.insert_snapshot_at(fs, ts).await.unwrap());
    }
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 4);

    let reply: SnapshotDeleteReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.delete",
            encode(&SnapshotDeleteRequest {
                snapshot_id: ids[0],
                extra: Default::default(),
            }),
        )
        .await
        .expect("delete ok"),
    )
    .unwrap();
    assert_eq!(reply.status, "pending");
    assert!(reply.pending_action_id > 0);
    // Snapshot is now deletion_pending → active count drops to 3.
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 3);
}

#[tokio::test]
async fn delete_breaches_hard_floor() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let mut id = 0;
    for ts in [100, 200, 300] {
        id = state.db.insert_snapshot_at(fs, ts).await.unwrap();
    }
    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.delete",
        encode(&SnapshotDeleteRequest {
            snapshot_id: id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("hard floor");
    assert_eq!(err.code, "fauna.filesync.snapshot.hard_floor_breach");
}

#[tokio::test]
async fn undelete_rejects_non_soft_deleted() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let id = state.db.insert_snapshot_at(fs, 100).await.unwrap();
    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.undelete",
        encode(&SnapshotUndeleteRequest {
            snapshot_id: id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("not soft-deleted");
    assert_eq!(err.code, "fauna.filesync.snapshot.not_soft_deleted");
}

#[tokio::test]
async fn prune_dry_run_then_actual() {
    // The do-not-cheat control: a set with NO recovering rows still
    // prunes exactly what the policy + hard floor dictate — the fix must not be
    // satisfiable by refusing everything. (This test's previous form pinned the
    // defect: 3 snapshots pruned down to 1, through the floor.)
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let mut ids = Vec::new();
    for ts in [100, 200, 300, 400, 500, 600] {
        ids.push(state.db.insert_snapshot_at(fs, ts).await.unwrap());
    }

    let policy = SnapshotRetentionPolicy {
        keep_last: Some(1),
        ..Default::default()
    };

    // Dry-run previews exactly what the real run will do: keep_last(1) marks 5
    // of 6 prunable, and the § 7 floor hands the newest two candidates back.
    let dry: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: true,
                policy: policy.clone(),
                ..Default::default()
            }),
        )
        .await
        .expect("prune dry-run ok"),
    )
    .unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.pruned, 3);
    assert_eq!(dry.remaining, 3);
    assert_eq!(dry.snapshots.len(), 3);
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 6);

    let real: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: false,
                policy,
                ..Default::default()
            }),
        )
        .await
        .expect("prune ok"),
    )
    .unwrap();
    assert!(!real.dry_run);
    assert_eq!(real.pruned, 3);
    assert!(real.snapshots.is_empty());
    assert_eq!(real.remaining, 3);
    // The floor holds: exactly SNAPSHOT_HARD_FLOOR active snapshots remain.
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 3);

    // § 7 Layer 3: the pruned rows are SOFT-deleted — each still exists with
    // its 30-day recovery window open, undelete-able through the wire kind.
    let rows = state.db.list_snapshots(fs).await.unwrap();
    assert_eq!(rows.len(), 6, "pruned rows still exist (soft-deleted)");
    assert_eq!(rows.iter().filter(|s| s.soft_deleted).count(), 3);
    dispatch(
        &router,
        state.clone(),
        ACTOR_A,
        "fauna.filesync.snapshot.undelete",
        encode(&SnapshotUndeleteRequest {
            snapshot_id: ids[0],
            extra: Default::default(),
        }),
    )
    .await
    .expect("a wire-pruned snapshot is undelete-able");
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 4);
}

#[tokio::test]
async fn prune_spares_a_soft_deleted_snapshot_and_its_undelete_window() {
    // a snapshot inside its 30-day `purge_after` recovery
    // window is NOT in the wire prune's candidate population — the prune must
    // not reach into the recovery windows the automatic path opened — and it
    // is still undelete-able AFTER a prune runs over its set.
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let mut ids = Vec::new();
    for ts in [100, 200, 300, 400, 500, 600, 700] {
        ids.push(state.db.insert_snapshot_at(fs, ts).await.unwrap());
    }
    let recovering = ids[1];
    state.db.soft_delete_snapshot(recovering).await.unwrap();
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 6);

    let policy = SnapshotRetentionPolicy {
        keep_last: Some(1),
        ..Default::default()
    };
    let dry: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: true,
                policy: policy.clone(),
                ..Default::default()
            }),
        )
        .await
        .expect("dry-run ok"),
    )
    .unwrap();
    assert!(
        !dry.snapshots.iter().any(|s| s.id == recovering),
        "soft-deleted snapshot {recovering} must not be a prune candidate — \
         its 30-day undelete window is open"
    );
    // The policy binds over the ACTIVE population: 6 active, keep_last(1)
    // marks 5, floor hands back 2 → 3. Counting the dead row would shift this.
    assert_eq!(dry.pruned, 3);

    let real: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: false,
                policy,
                ..Default::default()
            }),
        )
        .await
        .expect("prune ok"),
    )
    .unwrap();
    assert_eq!(real.pruned, 3);

    // The finding's own failure mode: the recovering snapshot must still be
    // undelete-able after the prune. Pre-fix, `delete_snapshots` hard-deleted
    // it and this dispatch returns not_found forever.
    dispatch(
        &router,
        state.clone(),
        ACTOR_A,
        "fauna.filesync.snapshot.undelete",
        encode(&SnapshotUndeleteRequest {
            snapshot_id: recovering,
            extra: Default::default(),
        }),
    )
    .await
    .expect("the soft-deleted snapshot survived the prune and is undelete-able");
}

#[tokio::test]
async fn prune_spares_a_deletion_pending_snapshot() {
    // a snapshot inside its 7-day cancellable window is not a prune
    // candidate — hard-deleting it would foreclose the cancel the window
    // promises.
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let mut ids = Vec::new();
    for ts in [100, 200, 300, 400, 500, 600, 700] {
        ids.push(state.db.insert_snapshot_at(fs, ts).await.unwrap());
    }
    let pending = ids[1];
    state
        .db
        .mark_snapshot_deletion_pending(pending)
        .await
        .unwrap();

    let dry: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: true,
                policy: SnapshotRetentionPolicy {
                    keep_last: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .await
        .expect("dry-run ok"),
    )
    .unwrap();
    assert!(
        !dry.snapshots.iter().any(|s| s.id == pending),
        "deletion-pending snapshot {pending} must not be a prune candidate — \
         its 7-day cancel window is open"
    );

    decode::<SnapshotPruneReply>(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: false,
                policy: SnapshotRetentionPolicy {
                    keep_last: Some(1),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .await
        .expect("prune ok"),
    )
    .unwrap();

    // The pending row survived, window intact: still present, still pending.
    let row = state
        .db
        .list_snapshots(fs)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == pending)
        .expect("the deletion-pending snapshot survived the prune");
    assert!(row.deletion_pending, "the cancel window is still open");
    assert!(!row.soft_deleted, "the prune did not touch its state");
}

#[tokio::test]
async fn prune_never_breaches_the_hard_floor() {
    // § 7 Layer 1 on the wire path: the union engine has no floor, so the
    // handler clamps — an aggressive policy on a 5-snapshot set prunes 2, not
    // 4, and the dry-run preview says the same.
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    for ts in [100, 200, 300, 400, 500] {
        state.db.insert_snapshot_at(fs, ts).await.unwrap();
    }
    let policy = SnapshotRetentionPolicy {
        keep_last: Some(1),
        ..Default::default()
    };
    let dry: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: true,
                policy: policy.clone(),
                ..Default::default()
            }),
        )
        .await
        .expect("dry-run ok"),
    )
    .unwrap();
    assert_eq!(dry.pruned, 2, "the preview honours the floor");

    let real: SnapshotPruneReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune",
            encode(&SnapshotPruneRequest {
                folder: "docs".into(),
                dry_run: false,
                policy,
                ..Default::default()
            }),
        )
        .await
        .expect("prune ok"),
    )
    .unwrap();
    assert_eq!(real.pruned, 2);
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 3);
}

// ── fauna.filesync.snapshot.prune_set_policy (§ 8) ──────────────────────────
//
// The kind exists because the explicit `prune` structurally cannot serve the
// page's button: it takes a client-supplied `keep_*` union policy, and § 8
// forbids re-mapping the shipped 2-field bounds onto that vocabulary — which is
// why each app had invented its own policy and the same button pruned five
// different ways, never applying what the wizard recorded.

async fn set_retention(state: &Arc<AppState>, name: &str, json: &str) {
    state
        .db
        .update_folder_for_user(
            name,
            &ACTOR_A,
            FolderUpdate {
                retention_policy: Some(Some(json)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

async fn prune_set_policy(
    router: &RpcRouter,
    state: &Arc<AppState>,
    name: &str,
    dry_run: bool,
) -> SnapshotPruneSetPolicyReply {
    decode(
        &dispatch(
            router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.prune_set_policy",
            encode(&SnapshotPruneSetPolicyRequest {
                folder: name.into(),
                dry_run,
                ..Default::default()
            }),
        )
        .await
        .expect("prune_set_policy ok"),
    )
    .unwrap()
}

#[tokio::test]
async fn prune_set_policy_previews_then_applies_the_sets_own_bounds() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    for ts in [100, 200, 300, 400, 500, 600] {
        state.db.insert_snapshot_at(fs, ts).await.unwrap();
    }
    // The bounds a user typed into the wizard — never sent on the wire.
    set_retention(&state, "docs", r#"{"max_snapshots":4,"max_age_days":0}"#).await;

    let dry = prune_set_policy(&router, &state, "docs", true).await;
    assert!(dry.dry_run);
    assert_eq!(dry.policy_state, "applied");
    assert_eq!(dry.pruned, 2);
    assert_eq!(dry.remaining, 4);
    assert_eq!(dry.snapshots.len(), 2, "the preview names its candidates");
    assert_eq!(
        state.db.count_active_snapshots(fs).await.unwrap(),
        6,
        "a preview deletes nothing"
    );

    let real = prune_set_policy(&router, &state, "docs", false).await;
    assert!(!real.dry_run);
    assert_eq!(real.policy_state, "applied");
    assert_eq!(
        (real.pruned, real.remaining),
        (dry.pruned, dry.remaining),
        "the preview predicted the run exactly"
    );
    assert!(real.snapshots.is_empty());
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 4);

    // § 7 Layer 3: soft-deleted, not purged — each pruned row keeps its
    // 30-day undelete window, and Layer 2 is deliberately not interposed.
    let rows = state.db.list_snapshots(fs).await.unwrap();
    assert_eq!(rows.len(), 6, "pruned rows still exist");
    assert_eq!(rows.iter().filter(|s| s.soft_deleted).count(), 2);
    assert!(
        rows.iter().all(|s| !s.deletion_pending),
        "a previewed synchronous prune opens no cancel window"
    );
}

#[tokio::test]
async fn prune_set_policy_reports_not_set_and_prunes_nothing() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    for ts in [100, 200, 300, 400, 500, 600] {
        state.db.insert_snapshot_at(fs, ts).await.unwrap();
    }

    // No column at all — the overwhelmingly common healthy state.
    let reply = prune_set_policy(&router, &state, "docs", false).await;
    assert_eq!(reply.policy_state, "not_set");
    assert_eq!(reply.pruned, 0);
    assert_eq!(
        reply.remaining, 6,
        "`remaining` answers what the set holds regardless of whether a policy exists"
    );
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 6);

    // A policy that binds nothing is not a policy — both bounds zero is how a
    // UI submits two empty boxes, and must not read as "keep zero snapshots".
    set_retention(&state, "docs", r#"{"max_snapshots":0,"max_age_days":0}"#).await;
    let reply = prune_set_policy(&router, &state, "docs", false).await;
    assert_eq!(reply.policy_state, "not_set");
    assert_eq!(reply.pruned, 0);
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 6);
}

/// A live at-rest shape, not a hypothetical: `folders.create`/`update` store the
/// `retention_policy` JSON unvalidated, so an incompatible `keep_*` JSON can rest
/// in this very column, and such rows are deliberately left rather than migrated (`backup-restore.md` § 8 → *The
/// off-shape at-rest residual*), so a nest that guessed would prune against a
/// number nobody chose.
#[tokio::test]
async fn prune_set_policy_refuses_a_drifted_policy_loudly() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    for ts in [100, 200, 300, 400, 500, 600] {
        state.db.insert_snapshot_at(fs, ts).await.unwrap();
    }
    set_retention(
        &state,
        "docs",
        r#"{"keep_last":1,"keep_daily":7,"keep_weekly":4}"#,
    )
    .await;

    let reply = prune_set_policy(&router, &state, "docs", false).await;
    assert_eq!(
        reply.policy_state, "unparseable",
        "the drifted shape is reported as itself, never silently read as not_set"
    );
    assert_eq!(reply.pruned, 0);
    assert_eq!(reply.remaining, 6);
    assert_eq!(
        state.db.count_active_snapshots(fs).await.unwrap(),
        6,
        "keep everything — a `keep_last: 1` read through the bounds engine would have destroyed 5"
    );
}

#[tokio::test]
async fn prune_set_policy_never_breaches_the_hard_floor() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    for ts in [100, 200, 300, 400, 500] {
        state.db.insert_snapshot_at(fs, ts).await.unwrap();
    }
    // A bound far below SNAPSHOT_HARD_FLOOR (3).
    set_retention(&state, "docs", r#"{"max_snapshots":1,"max_age_days":0}"#).await;

    let reply = prune_set_policy(&router, &state, "docs", false).await;
    assert_eq!(reply.policy_state, "applied");
    assert_eq!(reply.pruned, 2, "5 → the floor of 3, not to the bound of 1");
    assert_eq!(reply.remaining, 3);
    assert_eq!(state.db.count_active_snapshots(fs).await.unwrap(), 3);
}

/// A tag is a retention shield (§ 8 algorithm step 1 + the arming note): the
/// 2-field bounds have no tag vocabulary at all, so omitting the protection
/// would make this kind strictly more destructive than the explicit `prune`
/// request it replaces on the button.
#[tokio::test]
async fn prune_set_policy_spares_a_tagged_snapshot() {
    let (router, state) = router_and_state().await;
    let fs = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    // The oldest row is tagged — the one a count bound would retire first.
    // Created through the wire kind, so the tag rests the way production writes it.
    let tagged: SnapshotCreateFolderReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.create_folder",
            encode(&SnapshotCreateFolderRequest {
                folder: "docs".into(),
                tags: vec!["keep-forever".into()],
                device_id: None,
                ..Default::default()
            }),
        )
        .await
        .expect("create ok"),
    )
    .unwrap();
    // Everything else is newer, so only the tagged row is over the bound.
    let now = now_secs();
    for offset in 1..=5 {
        state.db.insert_snapshot_at(fs, now + offset).await.unwrap();
    }
    set_retention(&state, "docs", r#"{"max_snapshots":4,"max_age_days":0}"#).await;

    let reply = prune_set_policy(&router, &state, "docs", true).await;
    assert_eq!(reply.policy_state, "applied");
    assert!(
        !reply.snapshots.iter().any(|s| s.id == tagged.id),
        "the tagged snapshot is never a candidate, even over the count bound"
    );
}

#[tokio::test]
async fn prune_set_policy_unknown_folder_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state.clone(),
        ACTOR_A,
        "fauna.filesync.snapshot.prune_set_policy",
        encode(&SnapshotPruneSetPolicyRequest {
            folder: "nope".into(),
            ..Default::default()
        }),
    )
    .await
    .expect_err("unknown set refuses");
    assert_eq!(err.code, "fauna.filesync.snapshot.not_found");
}

#[tokio::test]
async fn check_without_backup_service_is_unavailable() {
    let (router, state) = router_and_state().await;
    state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    // for_test AppState has no backup_service.
    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.check",
        encode(&SnapshotCheckRequest {
            folder: "docs".into(),
            verify_content: false,
            ..Default::default()
        }),
    )
    .await
    .expect_err("backup unavailable");
    assert_eq!(err.code, "fauna.filesync.snapshot.backup_unavailable");
}

#[tokio::test]
async fn diff_same_folder_ok_different_invalid() {
    let (router, state) = router_and_state().await;
    let fs1 = state.db.create_folder("docs", &ACTOR_A).await.unwrap();
    let a = state.db.insert_snapshot_at(fs1, 100).await.unwrap();
    let b = state.db.insert_snapshot_at(fs1, 200).await.unwrap();

    let diff: SnapshotDiffReply = decode(
        &dispatch(
            &router,
            state.clone(),
            ACTOR_A,
            "fauna.filesync.snapshot.diff",
            encode(&SnapshotDiffRequest {
                a,
                b,
                extra: Default::default(),
            }),
        )
        .await
        .expect("diff ok"),
    )
    .unwrap();
    assert_eq!(diff.snapshot_a, a);
    assert_eq!(diff.snapshot_b, b);
    // Both seeded empty → no file changes.
    assert_eq!(diff.summary.added_count, 0);
    assert_eq!(diff.summary.removed_count, 0);

    // Snapshots from different folders → invalid_request.
    let fs2 = state.db.create_folder("photos", &ACTOR_A).await.unwrap();
    let c = state.db.insert_snapshot_at(fs2, 300).await.unwrap();
    let err = dispatch(
        &router,
        state,
        ACTOR_A,
        "fauna.filesync.snapshot.diff",
        encode(&SnapshotDiffRequest {
            a,
            b: c,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("different folders");
    assert_eq!(err.code, "fauna.filesync.snapshot.invalid_request");
}

// ── The per-reader label projection on the snapshot planes (S5e) ────────────
//
// `authorize_snapshot` admits a Q5 `AdminDiscovery`
// reader to a group-bound set's `get`/`diff` and then *discarded* the grant, so
// both kinds shipped `path_hash` + `path_sealed` — and `diff` also shipped the
// set-name pair S5c-1 exists to withhold — to exactly the reader S5d withheld
// them from on `MediaItem`. The ruled property is
// `encryption-at-rest.md` § Carve-outs: `path_hash` "is projected on the wire
// only to a label's audience". These pin it on both kinds.

/// Bind a group-bound shared set and put `member`s on its derived roster.
async fn bind_shared_set(
    state: &Arc<AppState>,
    name: &str,
    owner: &[u8; 32],
    group_id: &[u8],
    members: &[[u8; 32]],
) -> i64 {
    let id = state.db.create_folder(name, owner).await.unwrap();
    state
        .db
        .set_folder_mls_group(name, owner, Some(group_id))
        .await
        .unwrap();
    let channel_id = fauna_mls::types::ChannelId::from_group_id(group_id).0;
    state
        .db
        .register_actor_channel(owner, &channel_id)
        .await
        .unwrap();
    for m in members {
        state
            .db
            .register_actor_channel(m, &channel_id)
            .await
            .unwrap();
    }
    id
}

/// Record one file into `sync_changes` and stamp its `path_sealed` — the shape a
/// keyed writer's `SyncEngine::seal_recorded_path` funnel produces. Stamped by
/// raw SQL because a real seal needs client-side key material no nest holds.
async fn seed_sealed_file(
    db: &CacheDb,
    actor: &[u8; 32],
    folder_id: i64,
    path: &str,
    sealed: &[u8],
) {
    let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
    let manifest: [u8; 32] = *blake3::hash(format!("manifest:{path}").as_bytes()).as_bytes();
    let device: [u8; 32] = [0xd1u8; 32];
    db.record_sync_change(
        actor,
        &path_hash,
        Some(&manifest),
        10,
        "create",
        Some(folder_id),
        Some(&device),
        Some(path),
    )
    .await
    .unwrap();
    let conn = db.conn().await;
    conn.execute(
        "UPDATE sync_changes SET path_sealed = ?1 WHERE folder_id = ?2 AND path = ?3",
        rusqlite::params![sealed, folder_id, path],
    )
    .unwrap();
}

/// Stamp the set's `name_sealed` as the engine's bind/serve catch-up pass would,
/// so the set-name pair has both halves to project.
async fn stamp_set_name_seal(state: &Arc<AppState>, name: &str, owner: &[u8; 32], sealed: &[u8]) {
    state
        .db
        .update_folder_for_user(
            name,
            owner,
            fauna_nest::db::FolderUpdate {
                name_sealed: Some(sealed),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

/// Move a snapshot's `created_at` back, so a second `create_snapshot_v2` in the
/// same wall-clock second does not dedup onto it.
///
/// `snapshots` carries `UNIQUE(folder_id, created_at)` at **seconds**
/// granularity — two creates in one second are deliberately folded to one row
/// (the auto-scheduler/manual-create race; `create_snapshot_v2`'s dedup arm). A
/// two-snapshot test must therefore separate them explicitly. Backdating is the
/// latency-independent way to do that: a `sleep(1s)` would make the test's
/// correctness depend on wall-clock timing, which `testing.md` § point 14 bans.
async fn backdate_snapshot(db: &CacheDb, snapshot_id: i64) {
    let conn = db.conn().await;
    conn.execute(
        "UPDATE snapshots SET created_at = created_at - 60 WHERE id = ?1",
        rusqlite::params![snapshot_id],
    )
    .unwrap();
}

async fn snapshot_get(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: [u8; 32],
    snapshot_id: i64,
) -> SnapshotGetReply {
    decode(
        &dispatch(
            router,
            state.clone(),
            who,
            "fauna.filesync.snapshot.get",
            encode(&SnapshotGetRequest {
                snapshot_id,
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap()
}

async fn snapshot_diff(
    router: &RpcRouter,
    state: &Arc<AppState>,
    who: [u8; 32],
    a: i64,
    b: i64,
) -> SnapshotDiffReply {
    decode(
        &dispatch(
            router,
            state.clone(),
            who,
            "fauna.filesync.snapshot.diff",
            encode(&SnapshotDiffRequest {
                a,
                b,
                extra: Default::default(),
            }),
        )
        .await
        .expect("diff ok"),
    )
    .unwrap()
}

/// The audience arms of `snapshot.get` — the owner and a roster member both
/// receive the full `path_sealed` + `path_hash` pair. Withholding from these
/// readers would be a regression, not a fix, so this twin guards the gate's
/// other direction.
#[tokio::test]
async fn snapshot_get_ships_the_path_pair_to_the_owner_and_a_roster_member() {
    let (router, state) = router_and_state().await;
    let owner = [0xc1u8; 32];
    let member = [0xc2u8; 32];
    let group_id = vec![0x6au8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    let sealed = vec![0xEEu8; 48];
    seed_sealed_file(&state.db, &owner, fs, "s1.jpg", &sealed).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;

    for (who, label) in [(owner, "owner"), (member, "roster member")] {
        let got = snapshot_get(&router, &state, who, snap).await;
        let f = got
            .files
            .first()
            .unwrap_or_else(|| panic!("{label} sees the file"));
        assert_eq!(
            f.path_sealed.as_deref().map(|b| b.to_vec()),
            Some(sealed.clone()),
            "{label} receives the path seal verbatim"
        );
        assert_eq!(
            f.path_hash.as_deref().map(|b| b.to_vec()),
            Some(blake3::hash(b"s1.jpg").as_bytes().to_vec()),
            "{label} receives the salt that opens it"
        );
    }
}

/// The non-audience arm of `snapshot.get`. A Q5 admin
/// still *reads* the snapshot (the discovery grant is unchanged) but receives
/// neither label half: no seal they cannot open, and no salt to dictionary the
/// path back with.
#[tokio::test]
async fn snapshot_get_withholds_the_path_pair_from_a_q5_admin() {
    let (router, state) = router_and_state().await;
    let owner = [0xc3u8; 32];
    let admin = [0xa1u8; 32];
    let group_id = vec![0x6bu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    seed_sealed_file(&state.db, &owner, fs, "s1.jpg", &[0xEEu8; 48]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;
    state.db.add_admin_actor(&admin).await.unwrap();

    let got = snapshot_get(&router, &state, admin, snap).await;
    let f = got
        .files
        .first()
        .expect("the Q5 discovery grant still lists the file — no permission changes here");
    assert_eq!(
        f.path_sealed, None,
        "an admin who cannot open the seal is not sent it"
    );
    assert_eq!(
        f.path_hash, None,
        "and is not sent the unkeyed salt that would recover the path by dictionary"
    );
}

/// Stamp a set's `name_sealed`/`name_hash` the way a keyed client's bind pass
/// would, so a `snapshot.get` assertion on the set-name pair measures the
/// projection rather than an empty column. Raw SQL for the same reason
/// [`seed_sealed_file`] uses it: a real seal needs key material no nest holds.
async fn seed_sealed_set_name(state: &Arc<AppState>, folder_id: i64, name: &str, sealed: &[u8]) {
    let name_hash = fauna_core::path_crypto::set_name_hash(name);
    let conn = state.db.conn().await;
    conn.execute(
        "UPDATE folders SET name_sealed = ?1, name_hash = ?2 WHERE id = ?3",
        rusqlite::params![sealed, &name_hash[..], folder_id],
    )
    .unwrap();
}

/// The audience arm of `snapshot.get` for **`snapshots.tags`** — path-sealing
/// S6-d. The owner and a roster member both receive the sealed tag display copy
/// *and* the set-name pair that salts it.
///
/// The set-name pair is asserted here and not only in the negative twin because
/// it is `tags_sealed`'s **salt**: a reader handed the seal without it holds a
/// blob nothing can open once the plaintext `folder` scrubs, which is the
/// carry-the-salt trap S2b hit on `fauna.media.list`.
///
/// **A roster member is deliberately in the audience.** Unlike
/// `include_paths`/`exclude_paths` — which the nest withholds from a member even
/// in plaintext, and which S6-c therefore sealed owner-only — the plaintext
/// `tags` ships to a member on this very kind today. Sealing under an owner-only
/// root would take tags away from a reader who has them, so this arm is the pin
/// that stops a future "harden it like include/exclude" edit.
#[tokio::test]
async fn snapshot_get_ships_the_sealed_tags_and_their_salt_to_the_audience() {
    let (router, state) = router_and_state().await;
    let owner = [0xd1u8; 32];
    let member = [0xd2u8; 32];
    let group_id = vec![0x7au8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    let name_sealed = vec![0xA1u8; 40];
    seed_sealed_set_name(&state, fs, "shared", &name_sealed).await;
    seed_sealed_file(&state.db, &owner, fs, "s1.jpg", &[0xEEu8; 48]).await;
    let tags_sealed = vec![0xB2u8; 56];
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], Some(&tags_sealed), None)
        .await
        .unwrap()
        .id;

    for (who, label) in [(owner, "owner"), (member, "roster member")] {
        let got = snapshot_get(&router, &state, who, snap).await;
        assert_eq!(
            got.tags_sealed.as_deref().map(|b| b.to_vec()),
            Some(tags_sealed.clone()),
            "{label} receives the sealed tag display copy verbatim"
        );
        assert_eq!(
            got.folder_hash.as_deref().map(|b| b.to_vec()),
            Some(fauna_core::path_crypto::set_name_hash("shared").to_vec()),
            "{label} receives the salt the tag seal opens under"
        );
        assert_eq!(
            got.folder_sealed.as_deref().map(|b| b.to_vec()),
            Some(name_sealed.clone()),
            "{label} receives the set-name seal this reply carries from S6-d on"
        );
    }
}

/// The non-audience arm for the tag plane — path-sealing S6-d, the same shape
/// established for the path pair. A Q5 admin still reads the
/// snapshot, but receives neither the tag seal, nor the set-name seal, nor the
/// digest that salts both.
///
/// The `tags` **plaintext** is deliberately NOT asserted absent: this is the
/// expand phase, the plaintext column still rests, and taking it away is the S9
/// flip's job, not this projection's. What the projection owes is that no
/// *sealed* half and no *salt* crosses to a reader who holds no key.
#[tokio::test]
async fn snapshot_get_withholds_the_sealed_tags_and_their_salt_from_a_q5_admin() {
    let (router, state) = router_and_state().await;
    let owner = [0xd3u8; 32];
    let admin = [0xa2u8; 32];
    let group_id = vec![0x7bu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    seed_sealed_set_name(&state, fs, "shared", &[0xA1u8; 40]).await;
    seed_sealed_file(&state.db, &owner, fs, "s1.jpg", &[0xEEu8; 48]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], Some(&[0xB2u8; 56]), None)
        .await
        .unwrap()
        .id;
    state.db.add_admin_actor(&admin).await.unwrap();

    let got = snapshot_get(&router, &state, admin, snap).await;
    // Non-vacuity: the Q5 discovery grant genuinely reaches this snapshot, so the
    // absences below measure the projection rather than a failed read (S6-c
    // lesson 4 — a negative pin on a filtered surface must prove its subject
    // arrived at the code under test).
    assert_eq!(got.id, snap, "the Q5 discovery grant still reads the row");
    assert!(
        !got.files.is_empty(),
        "and still lists its files — no permission changes here"
    );
    assert_eq!(
        got.tags_sealed, None,
        "an admin who cannot open the tag seal is not sent it"
    );
    assert_eq!(
        got.folder_sealed, None,
        "nor the set-name seal they equally cannot open"
    );
    assert_eq!(
        got.folder_hash, None,
        "nor the unkeyed digest that salts both and would dictionary the set name back"
    );
}

// ── fauna.filesync.snapshot.stamp_labels (S8 D3) ────────────────────────────

/// The stamp's write half, driven by a roster MEMBER first: the kind
/// authorizes with `SnapshotAccess::Read` + `is_label_audience()` precisely
/// so any audience client — not only the owner — can converge a missing or
/// wrong-root seal (the window re-seal relies on it). Then the owner
/// OVERWRITES: the second stamp replaces the first, which is what makes the
/// wrong-root-axis re-seal possible at all.
///
/// ⚠ Both blobs here are filler bytes, so the overwrite rides the
/// *unparseable-at-rest* arm of the predicate. The two arms that
/// carry the finding's meaning — a generation-rooted seal is frozen, an
/// owner-rooted one is not — have their own tests below.
#[tokio::test]
async fn stamp_labels_stamps_for_any_audience_member_and_overwrites() {
    let (router, state) = router_and_state().await;
    let owner = [0xe1u8; 32];
    let member = [0xe2u8; 32];
    let group_id = vec![0x8au8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    // Tagged but UNSEALED — the S8 backfill row shape.
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;

    let first = vec![0xC1u8; 56];
    dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(first.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("a roster member is the audience and stamps");
    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(first),
        "the member's stamp rests and projects to the audience"
    );

    let second = vec![0xC2u8; 56];
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(second.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("overwrite is allowed — the window re-seal needs it");
    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(second),
        "the re-stamp replaced the resting seal"
    );
}

/// A `SealedLabel` envelope with the given key generation — `None` = the
/// owner root, `Some` = a roster (M2 generation) root. The ciphertext is
/// filler: the nest holds no key and never opens it, and the stamp predicate
/// reads only the envelope header.
fn envelope(generation: Option<u64>) -> Vec<u8> {
    fauna_core::path_crypto::SealedLabel {
        v: fauna_core::path_crypto::SEALED_LABEL_V1,
        generation,
        nonce: Some([7u8; 12]),
        ct: ByteBuf::from(vec![0xEEu8; 24]),
    }
    .to_bytes()
    .unwrap()
}

/// the write half: a seal that **already names a key
/// generation** is roster-rooted — its whole audience can open it, so there is
/// nothing left to converge and the stamp freezes. Before this fix the write
/// was a bare `UPDATE`, so any label-audience member could replace the owner's
/// snapshot labels arbitrarily and repeatedly; after the S9 plaintext scrub the
/// planted rendering would be the only copy of the labels left.
#[tokio::test]
async fn stamp_labels_refuses_to_replace_a_generation_rooted_seal() {
    let (router, state) = router_and_state().await;
    let owner = [0xe5u8; 32];
    let member = [0xe6u8; 32];
    let group_id = vec![0x8du8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;

    // The owner's honest first stamp, roster-rooted.
    let honest = envelope(Some(3));
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(honest.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("the first stamp fills an empty column");

    // A roster member — a genuine label-audience reader, so the audience gate
    // is NOT what refuses here — tries to replace it.
    let planted = envelope(Some(3));
    let err = dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(planted),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a generation-rooted seal is frozen — no member may replace it");
    assert_eq!(err.code, "fauna.filesync.snapshot.invalid_request");

    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(honest),
        "the owner's seal is the one that rests — nothing was planted over it"
    );
}

/// the owner arm: the freeze predicate reads the resting
/// envelope's `gen` header, which is UNAUTHENTICATED — a member can plant
/// `gen: Some(n)` over garbage ciphertext holding no keys at all. Before this
/// arm the plant froze out everyone, the owner included, permanently
/// (snapshots are immutable; deleting the snapshot was the only escape). Now
/// the OWNER's honest stamp always wins, while a member still cannot replace
/// a generation-rooted seal — the point, held by the middle leg.
/// Reverting the owner arm alone reds this test at its own "owner arm
/// licenses the re-stamp" assertion, not as a compile error — the named
/// mutation of the verify contract.
#[tokio::test]
async fn stamp_labels_owner_overwrites_a_member_plant() {
    let (router, state) = router_and_state().await;
    let owner = [0xf1u8; 32];
    let member = [0xf2u8; 32];
    let group_id = vec![0x91u8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    // An unsealed snapshot — the keyless-writer shape arms.
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;

    // The member's keyless plant: a parseable envelope naming a generation,
    // over ciphertext no key produced. A first stamp onto an empty column —
    // the audience gate and the freeze both admit it; planting is not what
    // the owner arm removes.
    let plant = envelope(Some(9));
    dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(plant),
            ..Default::default()
        }),
    )
    .await
    .expect("a first stamp is licensed for any audience member");

    // The member cannot entrench further — the freeze still binds
    // members, which is the do-not-cheat control on the owner arm.
    let err = dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(envelope(Some(10))),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a member still cannot replace a generation-rooted seal");
    assert_eq!(err.code, "fauna.filesync.snapshot.invalid_request");

    // …but the OWNER re-stamps the truth over the plant unconditionally.
    let truth = envelope(Some(4));
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(truth.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("the owner arm licenses the re-stamp — this used to freeze forever");

    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(truth),
        "the owner's seal rests — the plant is gone"
    );
}

/// The other side of the same predicate, and the reason it is not simply
/// "first write wins": the window residue — an owner-root `gen: None`
/// seal resting on a *bound* set's snapshot, which no roster member can open —
/// is still re-stampable by any audience client, exactly as S8 D3 designed.
#[tokio::test]
async fn stamp_labels_still_allows_the_owner_root_to_roster_root_axis_upgrade() {
    let (router, state) = router_and_state().await;
    let owner = [0xe7u8; 32];
    let member = [0xe8u8; 32];
    let group_id = vec![0x8eu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;

    // The window shape: sealed under the OWNER root on a bound set.
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(envelope(None)),
            ..Default::default()
        }),
    )
    .await
    .expect("the first stamp fills an empty column");

    let converged = envelope(Some(1));
    dispatch(
        &router,
        state.clone(),
        member,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(converged.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("any audience client converges a wrong-root seal — the whole point of D3");

    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(converged),
        "the roster-rooted seal replaced the unopenable owner-rooted one"
    );
}

/// The storage half: the write is a compare-and-swap, so a stamp
/// that decided against bytes which have since moved is refused rather than
/// clobbering whatever landed in between. This is the leg the handler's
/// read-then-write window depends on, and the only one a single-threaded test
/// can pin deterministically — the handler tests above exercise the predicate,
/// this one exercises the swap.
#[tokio::test]
async fn set_snapshot_tags_sealed_refuses_a_stale_expectation() {
    let (_router, state) = router_and_state().await;
    let owner = [0xeau8; 32];
    let group_id = vec![0x90u8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;

    let first = envelope(None);
    assert!(
        state
            .db
            .set_snapshot_tags_sealed(snap, &first, None)
            .await
            .unwrap(),
        "expecting an empty column and finding one swaps"
    );

    // A second writer that still believes the column is empty.
    assert!(
        !state
            .db
            .set_snapshot_tags_sealed(snap, &envelope(Some(9)), None)
            .await
            .unwrap(),
        "a stale expectation must NOT swap"
    );
    // …and one that read the current value does.
    assert!(
        state
            .db
            .set_snapshot_tags_sealed(snap, &envelope(Some(9)), Some(&first))
            .await
            .unwrap(),
        "the current value is a live expectation"
    );

    let got = snapshot_get(&_router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(envelope(Some(9))),
        "only the live-expectation write landed"
    );
}

/// The predicate reads the RESTING bytes only. An older nest must never
/// format-validate a newer client's envelope revision — that would be a
/// bidirectional-compatibility break — so bytes it cannot parse are accepted
/// as a *stamp payload*, and bytes it cannot parse *at rest* stay replaceable
/// (they open for nobody, so freezing them would strand the row).
#[tokio::test]
async fn stamp_labels_never_format_validates_the_incoming_envelope() {
    let (router, state) = router_and_state().await;
    let owner = [0xe9u8; 32];
    let group_id = vec![0x8fu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;

    // Not a v1 envelope at all — stands in for a future revision this nest
    // has never heard of.
    let unknown = vec![0xC7u8; 56];
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(unknown.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("an unrecognised envelope shape is stored opaquely, never rejected");

    // And it is not frozen: an envelope that parses for nobody is exactly the
    // case the axis re-stamp exists to repair.
    let repaired = envelope(Some(2));
    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(repaired.clone()),
            ..Default::default()
        }),
    )
    .await
    .expect("unparseable at rest ⇒ still replaceable");

    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(
        got.tags_sealed.as_deref().map(|b| b.to_vec()),
        Some(repaired)
    );
}

/// The non-audience arm: a Q5 admin — who CAN read the snapshot under the
/// discovery grant — is refused the stamp, and nothing rests. A planted seal
/// would render as roster-visible tags the roster never wrote.
#[tokio::test]
async fn stamp_labels_refuses_a_q5_admin_and_plants_nothing() {
    let (router, state) = router_and_state().await;
    let owner = [0xe3u8; 32];
    let admin = [0xa3u8; 32];
    let group_id = vec![0x8bu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &["manual".to_string()], None, None)
        .await
        .unwrap()
        .id;
    state.db.add_admin_actor(&admin).await.unwrap();

    // Non-vacuity: the admin's discovery grant genuinely reaches this snapshot
    // (S6-c lesson 4) — the refusal below measures the audience gate, not a
    // failed read.
    let read = snapshot_get(&router, &state, admin, snap).await;
    assert_eq!(read.id, snap);

    let err = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(vec![0xC3u8; 56]),
            ..Default::default()
        }),
    )
    .await
    .expect_err("a non-audience reader cannot plant a seal");
    assert_eq!(err.code, "fauna.filesync.snapshot.permission_denied");

    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(got.tags_sealed, None, "nothing rested");
}

/// Stamp-only means the seal can never CONJURE tags: a tag-less snapshot
/// refuses the stamp outright, so a sealed blob cannot smuggle roster-visible
/// tags onto a snapshot whose creator wrote none.
#[tokio::test]
async fn stamp_labels_refuses_a_tagless_snapshot() {
    let (router, state) = router_and_state().await;
    let owner = [0xe4u8; 32];
    let group_id = vec![0x8cu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    let snap = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;

    let err = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.filesync.snapshot.stamp_labels",
        encode(&SnapshotStampLabelsRequest {
            snapshot_id: snap,
            tags_sealed: ByteBuf::from(vec![0xC4u8; 56]),
            ..Default::default()
        }),
    )
    .await
    .expect_err("no tags ⇒ nothing to seal ⇒ refused");
    assert_eq!(err.code, "fauna.filesync.snapshot.invalid_request");

    let got = snapshot_get(&router, &state, owner, snap).await;
    assert_eq!(got.tags_sealed, None, "nothing rested");
}

/// The audience arms of `snapshot.diff` — both label pairs ride to the owner and
/// to a roster member: the per-entry path pair *and* the top-level set-name pair.
#[tokio::test]
async fn snapshot_diff_ships_both_label_pairs_to_the_audience() {
    let (router, state) = router_and_state().await;
    let owner = [0xc4u8; 32];
    let member = [0xc5u8; 32];
    let group_id = vec![0x6cu8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[member]).await;
    let name_sealed = vec![0xEDu8; 48];
    stamp_set_name_seal(&state, "shared", &owner, &name_sealed).await;

    // Snapshot A is empty; snapshot B holds one sealed file → one `added` entry.
    let a = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;
    backdate_snapshot(&state.db, a).await;
    let path_sealed = vec![0xEEu8; 48];
    seed_sealed_file(&state.db, &owner, fs, "s1.jpg", &path_sealed).await;
    let b = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;

    for (who, label) in [(owner, "owner"), (member, "roster member")] {
        let diff = snapshot_diff(&router, &state, who, a, b).await;
        let e = diff
            .added
            .first()
            .unwrap_or_else(|| panic!("{label} sees the added entry"));
        assert_eq!(
            e.path_sealed.as_deref().map(|b| b.to_vec()),
            Some(path_sealed.clone()),
            "{label} receives the entry's path seal"
        );
        assert_eq!(
            e.path_hash.as_deref().map(|b| b.to_vec()),
            Some(blake3::hash(b"s1.jpg").as_bytes().to_vec()),
            "{label} receives the salt that opens it"
        );
        assert_eq!(
            diff.folder_sealed.as_deref().map(|b| b.to_vec()),
            Some(name_sealed.clone()),
            "{label} receives the set-name seal"
        );
        assert_eq!(
            diff.folder_hash.as_deref().map(|b| b.to_vec()),
            Some(fauna_core::path_crypto::set_name_hash("shared").to_vec()),
            "{label} receives the set-name salt"
        );
    }
}

/// The non-audience arm of `snapshot.diff` — finding leg (b), the leg
/// that also reopened the already-closed set-name axis. A Q5 admin gets
/// **neither** pair: not the per-entry path pair, and not the top-level
/// set-name pair.
#[tokio::test]
async fn snapshot_diff_withholds_both_label_pairs_from_a_q5_admin() {
    let (router, state) = router_and_state().await;
    let owner = [0xc6u8; 32];
    let admin = [0xa2u8; 32];
    let group_id = vec![0x6du8; 24];
    let fs = bind_shared_set(&state, "shared", &owner, &group_id, &[]).await;
    stamp_set_name_seal(&state, "shared", &owner, &[0xEDu8; 48]).await;

    let a = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;
    backdate_snapshot(&state.db, a).await;
    seed_sealed_file(&state.db, &owner, fs, "s1.jpg", &[0xEEu8; 48]).await;
    let b = state
        .db
        .create_snapshot_v2(fs, None, &[], None, None)
        .await
        .unwrap()
        .id;
    state.db.add_admin_actor(&admin).await.unwrap();

    let diff = snapshot_diff(&router, &state, admin, a, b).await;
    let e = diff
        .added
        .first()
        .expect("the Q5 discovery grant still diffs — no permission changes here");
    assert_eq!(e.path_sealed, None, "no path seal to a non-audience reader");
    assert_eq!(
        e.path_hash, None,
        "and no path salt — this is the leg that defeated S5d's MediaItem gate"
    );
    assert_eq!(
        diff.folder_sealed, None,
        "no set-name seal either — diff was a second producer of the pair S5c-1 withholds"
    );
    assert_eq!(
        diff.folder_hash, None,
        "and no set-name salt, which is the set-name axis reopened by this plane"
    );
}
