//! Integration tests for the pending_actions DB methods.

use std::sync::Arc;

use fauna_nest::db::CacheDb;
use fauna_nest::pending_actions::{ActionType, execute_ready_actions as run_executor};
use fauna_nest::routes::AppState;

/// Create an AccountDelete pending action and verify:
/// - The delay is 14 days.
/// - Listing for the actor returns the created row.
#[tokio::test]
async fn create_and_list_pending_action() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor: [u8; 32] = [1u8; 32];

    let fourteen_days = 14 * 24 * 3600i64;
    assert_eq!(ActionType::AccountDelete.delay_secs(), fourteen_days);

    let id = db
        .create_pending_action(
            &ActionType::AccountDelete,
            &actor,
            None,
            None,
            Some("127.0.0.1"),
        )
        .await
        .unwrap();

    assert!(id > 0, "row id should be positive");

    let rows = db.list_pending_actions_for_actor(&actor).await.unwrap();
    assert_eq!(rows.len(), 1);

    let row = &rows[0];
    assert_eq!(row.id, id);
    assert_eq!(row.action_type, "account.delete");
    assert_eq!(row.status, "pending");
    assert_eq!(row.ip_address.as_deref(), Some("127.0.0.1"));
    assert!(row.chain_hash.is_some(), "chain_hash should be set");
    assert!(
        row.execute_after >= row.created_at + fourteen_days,
        "execute_after should be at least 14 days after created_at"
    );
}

/// Create a pending action then cancel it. Verify status and cancelled_by are set.
#[tokio::test]
async fn cancel_pending_action() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor: [u8; 32] = [2u8; 32];

    let id = db
        .create_pending_action(
            &ActionType::HandleChange,
            &actor,
            Some("new-handle"),
            None,
            None,
        )
        .await
        .unwrap();

    db.cancel_pending_action(id, &actor).await.unwrap();

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.cancelled_by.as_deref(), Some(actor.as_slice()));
    assert!(row.cancelled_at.is_some());
}

/// Actor B must not be able to cancel Actor A's HandleChange (user-only action).
#[tokio::test]
async fn cannot_cancel_other_users_action() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor_a: [u8; 32] = [3u8; 32];
    let actor_b: [u8; 32] = [4u8; 32];

    let id = db
        .create_pending_action(&ActionType::HandleChange, &actor_a, None, None, None)
        .await
        .unwrap();

    let result = db.cancel_pending_action(id, &actor_b).await;
    assert!(
        result.is_err(),
        "actor_b should not be authorized to cancel actor_a's HandleChange"
    );

    // Action should still be pending
    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "pending");
}

/// AdminRemove requires quorum=2. Verify:
/// - requires_quorum is stored as 2.
/// - An approver can add their approval.
/// - Self-approval is blocked.
/// - Duplicate approval is silently ignored.
#[tokio::test]
async fn quorum_action_tracks_approvals() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor: [u8; 32] = [5u8; 32];
    let approver_a: [u8; 32] = [6u8; 32];
    let approver_b: [u8; 32] = [7u8; 32];

    assert_eq!(ActionType::AdminRemove.requires_quorum(), 2);

    let id = db
        .create_pending_action(
            &ActionType::AdminRemove,
            &actor,
            Some("target-admin"),
            None,
            None,
        )
        .await
        .unwrap();

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.requires_quorum, 2);
    assert_eq!(row.approvals, "[]");

    // Self-approval must be blocked
    let self_approve = db.approve_pending_action(id, &actor).await;
    assert!(self_approve.is_err(), "self-approval should be rejected");

    // Approver A adds approval
    db.approve_pending_action(id, &approver_a).await.unwrap();

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    let approvals: Vec<String> = serde_json::from_str(&row.approvals).unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0], hex::encode(approver_a));

    // Duplicate approval by approver_a — should be silently ignored
    db.approve_pending_action(id, &approver_a).await.unwrap();
    let row = db.get_pending_action(id).await.unwrap().unwrap();
    let approvals: Vec<String> = serde_json::from_str(&row.approvals).unwrap();
    assert_eq!(
        approvals.len(),
        1,
        "duplicate approval should not add a second entry"
    );

    // Approver B adds approval
    db.approve_pending_action(id, &approver_b).await.unwrap();
    let row = db.get_pending_action(id).await.unwrap().unwrap();
    let approvals: Vec<String> = serde_json::from_str(&row.approvals).unwrap();
    assert_eq!(
        approvals.len(),
        2,
        "both distinct approvers should be recorded"
    );
}

/// Create a pending action, move execute_after to the past, then verify it appears
/// in `list_ready_pending_actions`.
#[tokio::test]
async fn execute_ready_actions() {
    let db = CacheDb::open_in_memory().unwrap();

    let actor: [u8; 32] = [8u8; 32];

    let id = db
        .create_pending_action(
            &ActionType::SnapshotDelete,
            &actor,
            Some("snap-123"),
            None,
            None,
        )
        .await
        .unwrap();

    // Initially not ready (execute_after is in the future)
    let ready = db.list_ready_pending_actions().await.unwrap();
    assert!(
        ready.iter().all(|r| r.id != id),
        "action should not be ready yet"
    );

    // Move execute_after to the past
    db.test_set_execute_after(id, 1).await.unwrap();

    let ready = db.list_ready_pending_actions().await.unwrap();
    assert!(
        ready.iter().any(|r| r.id == id),
        "action should appear in ready list after execute_after is in the past"
    );

    // Claim it, as the executor does, then mark it executed: it drops from
    // the ready list at the claim and stays out.
    db.claim_pending_action(id)
        .await
        .unwrap()
        .expect("claimable");
    assert!(db.mark_pending_action_executed(id).await.unwrap());
    let ready = db.list_ready_pending_actions().await.unwrap();
    assert!(
        ready.iter().all(|r| r.id != id),
        "executed action should no longer appear in ready list"
    );

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "executed");
    assert!(row.executed_at.is_some());
}

/// Create a HandleChange action, move execute_after to the past, call
/// execute_ready_actions, and verify the status becomes "executed".
#[tokio::test]
async fn executor_runs_ready_non_quorum_action() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let actor: [u8; 32] = [9u8; 32];

    // HandleChange requires a "new_handle" in the payload
    let payload = r#"{"new_handle": "new-name"}"#;

    let id = db
        .create_pending_action(
            &ActionType::HandleChange,
            &actor,
            Some("new-name"),
            Some(payload),
            None,
        )
        .await
        .unwrap();

    // Move execute_after to the past so it's immediately ready
    db.test_set_execute_after(id, 1).await.unwrap();

    // Register the actor so set_handle doesn't fail on a missing user; if the
    // user doesn't exist the UPDATE is a no-op, which is fine — we just check
    // the action status transitions correctly.
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    let executed = run_executor(&state).await.unwrap();
    assert_eq!(executed, 1, "one action should have been executed");

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "executed", "action status should be 'executed'");
    assert!(row.executed_at.is_some(), "executed_at should be set");
}

/// Create an AdminRemove (quorum=2) with execute_after in the past and zero
/// approvals; call execute_ready_actions and verify the action is expired.
#[tokio::test]
async fn executor_expires_quorum_action_without_enough_approvals() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let actor: [u8; 32] = [10u8; 32];
    let target_hex = hex::encode([11u8; 32]);

    assert_eq!(ActionType::AdminRemove.requires_quorum(), 2);

    let id = db
        .create_pending_action(
            &ActionType::AdminRemove,
            &actor,
            Some(&target_hex),
            None,
            None,
        )
        .await
        .unwrap();

    // Move execute_after to the past — no approvals have been added
    db.test_set_execute_after(id, 1).await.unwrap();

    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    let executed = run_executor(&state).await.unwrap();
    assert_eq!(
        executed, 0,
        "quorum-lacking action should not count as executed"
    );

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "expired", "action status should be 'expired'");
}

/// The executor's authority-stripping arms must tear down *live* authority
/// (bearer + open sockets), not only mutate the DB — transport.md
/// § Revocation teardown. Pre-fix, `admin.remove` and `admin.change_role`
/// performed neither half: the demoted admin's
/// socket stayed open receiving Push events until it happened to die. Pins the *callers* — the direct-
/// `disconnect_actor` conformance pins structurally cannot catch a missing
/// caller.
#[tokio::test]
async fn executor_authority_strips_tear_down_live_connections() {
    use fauna_nest::pending_actions::execute_action;

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));

    // One case per newly wired arm: (action type, target, payload).
    let cases: [(&ActionType, [u8; 32], Option<&str>); 2] = [
        (&ActionType::AdminRemove, [21u8; 32], None),
        (
            &ActionType::AdminChangeRole,
            [22u8; 32],
            Some(r#"{"role": "admin"}"#),
        ),
    ];
    // A standing peer superadmin keeps every roster mutation below floor-legal
    // — this test's subject is the authority teardown, not the superadmin
    // floor (which has its own pins).
    db.add_admin_actor(&[19u8; 32][..]).await.unwrap();
    for (action_type, target, payload) in cases {
        db.create_user(&target, "free", "t").await.unwrap();
        if matches!(
            action_type,
            ActionType::AdminRemove | ActionType::AdminChangeRole
        ) {
            db.add_admin_actor(&target).await.unwrap();
        }
        let token = state
            .auth
            .token_store
            .insert(fauna_core::identity::ActorId(target), 3600)
            .await;
        let (conn, _rx) = state.ws.subscribe(target);

        let target_hex = hex::encode(target);
        let id = db
            .create_pending_action(action_type, &[20u8; 32], Some(&target_hex), payload, None)
            .await
            .unwrap();
        let row = db.get_pending_action(id).await.unwrap().unwrap();
        execute_action(&state, &row)
            .await
            .unwrap_or_else(|e| panic!("{:?} execute failed: {e}", action_type));

        assert!(
            conn.is_revoked(),
            "{action_type:?}: the target's live WS must be flagged for the 4401 close"
        );
        assert!(
            state.auth.token_store.validate(&token).await.is_none(),
            "{action_type:?}: the target's bearer must be revoked"
        );
    }
}

/// The § 7 Layer-2 → Layer-3 hop for an automatic retention prune
/// (`backup-restore.md` § 7 Deletion Safety; armed 2026-08-01 per § 8 RULING
/// consequence (iii)). Its executor arm was a `tracing::warn!` stub —
/// *"snapshot_bulk_prune not yet implemented"* — for as long as the action type
/// has existed, so a scheduled prune would have expired into a no-op.
///
/// Two assertions, and the second is the load-bearing one: the terminal state is
/// **soft**-deleted with a `purge_after` recovery window, NOT a row deletion. If
/// this arm ever reaches `delete_snapshots`, an automatic policy becomes
/// immediate irreversible data loss.
#[tokio::test]
async fn snapshot_bulk_prune_soft_deletes_its_targets_and_never_hard_deletes() {
    use fauna_nest::pending_actions::execute_action;

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    let actor: [u8; 32] = [42u8; 32];

    let fs_id = db.create_folder("prune-exec", &actor).await.unwrap();
    let mut ids = Vec::new();
    for ts in [1000, 2000, 3000, 4000] {
        ids.push(db.insert_snapshot_at(fs_id, ts).await.unwrap());
    }

    assert_eq!(
        ActionType::SnapshotBulkPrune.delay_secs(),
        7 * 24 * 3600,
        "the ratified bulk-prune window is 7 days"
    );

    let payload = serde_json::json!({ "snapshot_ids": [ids[0], ids[1]] }).to_string();
    let id = db
        .create_pending_action(
            &ActionType::SnapshotBulkPrune,
            &actor,
            Some(&fs_id.to_string()),
            Some(&payload),
            None,
        )
        .await
        .unwrap();
    let row = db.get_pending_action(id).await.unwrap().unwrap();
    execute_action(&state, &row).await.unwrap();

    let snaps = db.list_snapshots(fs_id).await.unwrap();
    assert_eq!(snaps.len(), 4, "no row is deleted by the executor");
    for s in &snaps {
        let targeted = s.id == ids[0] || s.id == ids[1];
        assert_eq!(s.soft_deleted, targeted, "snapshot {} soft-delete", s.id);
        if targeted {
            assert!(
                s.purge_after.is_some_and(|p| p > 0),
                "a soft-deleted snapshot carries its 30-day recovery deadline"
            );
        }
    }
}

/// Cancelling a snapshot deletion must un-mark its targets — for BOTH producers
/// of a `deletion_pending` mark (`snapshot.delete`, one id in `target`;
/// `snapshot.bulk_prune`, a batch in `payload.snapshot_ids`).
///
/// Pre-existing defect, found while arming auto-prune (2026-08-01):
/// `cancel_pending_action` flipped only the `pending_actions` row and nothing
/// else ever clears the flag (`soft_delete_snapshot` / `undelete_snapshot` both
/// act on a snapshot already past this stage). A cancelled delete therefore left
/// its snapshot permanently outside `count_active_snapshots` — silently excluded
/// from its own folder's § 7 Layer-1 hard floor, and from the auto-pruner's
/// candidate population, on the strength of a deletion the user called off.
/// Arming multiplies the number of marks, which is why it is fixed here.
#[tokio::test]
async fn cancelling_a_snapshot_deletion_releases_its_targets() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let actor: [u8; 32] = [43u8; 32];

    let fs_id = db.create_folder("cancel-release", &actor).await.unwrap();
    let mut ids = Vec::new();
    for ts in [1000, 2000, 3000, 4000, 5000] {
        ids.push(db.insert_snapshot_at(fs_id, ts).await.unwrap());
    }
    let active = || db.count_active_snapshots(fs_id);
    assert_eq!(active().await.unwrap(), 5);

    // Producer 1 — the single-snapshot delete: id in `target`.
    let single = db
        .create_pending_action(
            &ActionType::SnapshotDelete,
            &actor,
            Some(&ids[0].to_string()),
            None,
            None,
        )
        .await
        .unwrap();
    db.mark_snapshot_deletion_pending(ids[0]).await.unwrap();

    // Producer 2 — the auto-prune batch: ids in `payload.snapshot_ids`.
    let batch_payload = serde_json::json!({ "snapshot_ids": [ids[1], ids[2]] }).to_string();
    let batch = db
        .create_pending_action(
            &ActionType::SnapshotBulkPrune,
            &actor,
            Some(&fs_id.to_string()),
            Some(&batch_payload),
            None,
        )
        .await
        .unwrap();
    for id in [ids[1], ids[2]] {
        db.mark_snapshot_deletion_pending(id).await.unwrap();
    }
    assert_eq!(
        active().await.unwrap(),
        2,
        "three snapshots are in flight, so the hard floor sees only two"
    );

    db.cancel_pending_action(single, &actor).await.unwrap();
    assert_eq!(active().await.unwrap(), 3, "the single target came back");

    db.cancel_pending_action(batch, &actor).await.unwrap();
    assert_eq!(
        active().await.unwrap(),
        5,
        "every cancelled target is active again — a cancel restores the floor"
    );
    assert!(
        db.list_snapshots(fs_id)
            .await
            .unwrap()
            .iter()
            .all(|s| !s.deletion_pending && !s.soft_deleted),
        "and no snapshot is left in a half-deleted state"
    );
}

/// Seed `n` distinct posts authored by `author`, returning their post ids.
async fn seed_posts(db: &CacheDb, author: [u8; 32], n: usize) -> Vec<[u8; 32]> {
    use fauna_core::data::*;
    use fauna_core::encoding::canonical_encode;
    use fauna_core::identity::ActorId;

    let mut ids = Vec::new();
    for i in 0..n {
        let post = Post {
            author: ActorId(author),
            created_at: Timestamp(1000 + i as u64),
            body: PostBody::Text {
                content: format!("post {i}"),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let data = canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&data).as_bytes();
        db.put_post(&post_id, &data, None).await.unwrap();
        ids.push(post_id);
    }
    ids
}

/// Account deletion must actually RETRACT the actor's posts, not leave them
/// standing (`account-data-plane.md` § Nest-side
/// requirements item 1 — posts are `Policy::Retain` precisely because they
/// "need the existing federation-aware per-post retraction, not a raw purge",
/// and until this leg existed nothing ever performed that retraction).
///
/// Pre-fix, `finalize_user_deletion` purged the ~140 `Policy::Purge` tables and
/// dropped the `users` row while every post the actor ever wrote stayed live
/// and servable — the deleted account's content outliving the account.
///
/// The retraction must run **before** the purge sweep, which is why this is
/// wired at the `execute_action` arm rather than inside `finalize_user_deletion`:
/// `nostr_accounts` (holding the author's encrypted nsec, the only key that can
/// sign the kind-5 retraction) and `ap_post_map` (the ActivityPub `Delete`-push
/// witness) are both `Policy::Purge`, so a retraction attempted afterwards can
/// never reach the federated copies.
#[tokio::test]
async fn account_deletion_retracts_the_actors_posts() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));

    let actor: [u8; 32] = [42u8; 32];
    let ids = seed_posts(&db, actor, 3).await;

    assert_eq!(
        db.list_posts_by_author(&actor).await.unwrap().len(),
        3,
        "precondition: the actor has three live posts"
    );

    let id = db
        .create_pending_action(&ActionType::AccountDelete, &actor, None, None, None)
        .await
        .unwrap();
    db.test_set_execute_after(id, 1).await.unwrap();

    let executed = run_executor(&state).await.unwrap();
    assert_eq!(executed, 1, "the account deletion should have executed");

    assert_eq!(
        db.list_posts_by_author(&actor).await.unwrap(),
        Vec::<Vec<u8>>::new(),
        "every post the deleted actor authored must be retracted, not left standing"
    );
    for post_id in &ids {
        assert!(
            db.get_post_author(post_id).await.unwrap().is_none(),
            "post {} must be gone from the serving projection",
            hex::encode(post_id)
        );
    }
}

/// The retraction sits **after** `finalize_user_deletion`'s fail-safes, so a
/// deletion that is *refused* never destroys the user's content.
///
/// The admin fail-safe refuses rather than stripping the role, and the action
/// then retries every tick until the actor is demoted — which is precisely why
/// retraction must not run before it. Retracting first would mean each refused
/// tick had already destroyed posts (irreversibly, and federated) for an
/// account that still exists and may never be deleted at all.
#[tokio::test]
async fn a_refused_deletion_retracts_nothing() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));

    let actor: [u8; 32] = [43u8; 32];
    seed_posts(&db, actor, 2).await;
    // Hold the admin role — the fail-safe that refuses the finalize.
    db.add_admin_actor(&actor[..]).await.unwrap();

    let err = fauna_nest::pending_actions::finalize_user_deletion(&state, &actor)
        .await
        .expect_err("an admin's deletion must be refused");
    assert!(err.to_string().contains("admin role"), "{err}");

    assert_eq!(
        db.list_posts_by_author(&actor).await.unwrap().len(),
        2,
        "a refused deletion must leave every post standing — retraction runs \
         after the fail-safes, never before them"
    );
}

/// N individually-legal, quorum-approved removals scheduled
/// while the roster was full must not empty it. The door's last-superadmin
/// refusal is a schedule-time check on a 24 h-delayed action, so every removal
/// here was legal when scheduled — the floor has to bind at the point of
/// effect. A zero-superadmin roster is an off-box brick: every `fauna.admin.*`
/// kind (factory reset included) resolves `CallerClass::Admin` for nobody, and
/// the claim gate reads `admin_count = 0` with the single-use code file long
/// consumed — `admin.md:121` says this state is unrepresentable.
#[tokio::test]
async fn a_full_roster_of_approved_removals_cannot_empty_the_superadmin_roster() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let admins: [[u8; 32]; 3] = [[10u8; 32], [11u8; 32], [12u8; 32]];
    for a in &admins {
        // `role` defaults to 'superadmin' — the only role the add door mints.
        db.add_admin_actor(&a[..]).await.unwrap();
    }
    assert_eq!(db.admin_count_by_role("superadmin").await.unwrap(), 3);

    // Each removal is created by a peer and approved by the two non-creators —
    // the exact shape the schedule + approve doors produce (creator
    // self-approval is refused; `AdminRemove` quorum is 2).
    let mut ids = Vec::new();
    for (i, target) in admins.iter().enumerate() {
        let creator = admins[(i + 1) % 3];
        let id = db
            .create_pending_action(
                &ActionType::AdminRemove,
                &creator,
                Some(&hex::encode(target)),
                None,
                None,
            )
            .await
            .unwrap();
        for approver in admins.iter().filter(|a| **a != creator) {
            db.approve_pending_action(id, &approver[..]).await.unwrap();
        }
        db.test_set_execute_after(id, 1).await.unwrap();
        ids.push(id);
    }

    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    // Two passes: one drains the ready backlog the way the background loop
    // does; the second proves the refusal is stable, not merely deferred.
    run_executor(&state).await.unwrap();
    run_executor(&state).await.unwrap();

    let remaining = db.admin_count_by_role("superadmin").await.unwrap();
    assert!(
        remaining >= 1,
        "the executor emptied the superadmin roster ({remaining} left): the box \
         is now administrable by nobody and re-claimable by nobody — the \
         off-box brick admin.md:121 declares unrepresentable"
    );

    // Two removals legally execute (3 → 2 → 1); the third must PARK — status
    // stays 'pending' (retryable once the roster can afford it, the
    // `admin.add` arm's posture), never 'executed' and never 'expired'
    // (its approvals were sufficient).
    let mut statuses = Vec::new();
    for id in &ids {
        statuses.push(db.get_pending_action(*id).await.unwrap().unwrap().status);
    }
    assert_eq!(
        statuses.iter().filter(|s| s.as_str() == "executed").count(),
        2,
        "exactly two of three removals should execute: {statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| s.as_str() == "pending").count(),
        1,
        "the floor-refused removal must stay pending — not silently marked \
         executed, not expired: {statuses:?}"
    );
}

/// The floor's counterpart: a removal whose target is no longer an admin
/// (already removed, or never was) must COMPLETE, not park — an eternally
/// retrying no-op action would warn every tick forever. Guards the executor's
/// refusal mapping against over-eager bailing.
#[tokio::test]
async fn removing_a_target_that_is_not_an_admin_completes_instead_of_parking() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let a: [u8; 32] = [20u8; 32];
    let b: [u8; 32] = [21u8; 32];
    db.add_admin_actor(&a[..]).await.unwrap();
    db.add_admin_actor(&b[..]).await.unwrap();

    // Target was never an admin.
    let stranger: [u8; 32] = [99u8; 32];
    let id = db
        .create_pending_action(
            &ActionType::AdminRemove,
            &a,
            Some(&hex::encode(stranger)),
            None,
            None,
        )
        .await
        .unwrap();
    db.approve_pending_action(id, &b[..]).await.unwrap();
    db.approve_pending_action(id, &stranger[..]).await.unwrap();
    db.test_set_execute_after(id, 1).await.unwrap();

    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    run_executor(&state).await.unwrap();

    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(
        row.status, "executed",
        "an already-satisfied removal completes idempotently"
    );
    assert_eq!(db.admin_count_by_role("superadmin").await.unwrap(), 2);
}

/// The same floor binds the `admin.change_role` arm: demoting the last
/// superadmin empties the tier exactly as removing them would. No door
/// schedules `AdminChangeRole` today, but the executor arm exists and a
/// persisted action can outlive the upgrade that adds a door — the
/// `admin.add` arm's own recorded rationale for re-checking at execution.
#[tokio::test]
async fn a_role_change_cannot_demote_the_last_superadmin() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let target: [u8; 32] = [30u8; 32];
    db.add_admin_actor(&target[..]).await.unwrap();

    let creator: [u8; 32] = [31u8; 32];
    let approver: [u8; 32] = [32u8; 32];
    let id = db
        .create_pending_action(
            &ActionType::AdminChangeRole,
            &creator,
            Some(&hex::encode(target)),
            Some(r#"{"role":"moderator"}"#),
            None,
        )
        .await
        .unwrap();
    db.approve_pending_action(id, &approver[..]).await.unwrap();
    db.test_set_execute_after(id, 1).await.unwrap();

    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    run_executor(&state).await.unwrap();

    assert_eq!(
        db.get_admin_role(&target).await.unwrap().as_deref(),
        Some("superadmin"),
        "demoting the last superadmin must be refused at the writer"
    );
    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(
        row.status, "pending",
        "the refused demotion parks (retryable once another superadmin exists)"
    );
}

/// `admin.add`'s execution-time `Admin ⊇ User` check refuses a target that was
/// SUCCEEDED during the action's 24 h window (`admin.md` § Admin continuity and
/// succession). The retired key keeps a handle-less `users` row, so a
/// `get_user`-only check would grant the role to a key that can never log in —
/// dead weight in `admin_count` and the removal quorum. The action parks
/// (retryable, the arm's posture for an unregistered target), never grants.
#[tokio::test]
async fn an_admin_add_whose_target_was_succeeded_in_the_window_does_not_grant() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let admin: [u8; 32] = [50u8; 32];
    db.add_admin_actor(&admin[..]).await.unwrap();
    let (retired, successor): ([u8; 32], [u8; 32]) = ([51u8; 32], [52u8; 32]);
    db.create_user(&retired, "free", "").await.unwrap();

    let id = db
        .create_pending_action(
            &ActionType::AdminAdd,
            &admin,
            Some(&hex::encode(retired)),
            None,
            None,
        )
        .await
        .unwrap();
    // Meet the grant's quorum, so the only thing that can stop it is the
    // supersession refusal (an unapproved row would expire instead).
    let approver: [u8; 32] = [55u8; 32];
    db.add_admin_actor(&approver[..]).await.unwrap();
    db.approve_pending_action(id, &approver[..]).await.unwrap();
    // The recovery ceremony runs inside the window.
    db.record_succession(&retired, &successor, b"s", 1)
        .await
        .unwrap()
        .unwrap();
    db.test_set_execute_after(id, 1).await.unwrap();

    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    run_executor(&state).await.unwrap();

    assert!(
        !db.is_admin(&retired[..]).await.unwrap(),
        "a retired key must not be granted the admin role at execution"
    );
    assert!(
        !db.is_admin(&successor[..]).await.unwrap(),
        "nor does the grant silently re-target the successor"
    );
    let row = db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "pending", "the refused grant parks");
}

/// Deleting an account whose LOCAL PREDECESSOR holds an admin row is refused
/// (`admin.md` § Admin continuity and succession: the last-superadmin floor and
/// `Admin ⊇ User` at both ends of the role's life). The deletion's predecessor
/// walk purges `admin_actor_ids` for every predecessor (`Policy::Purge`) and
/// `delete_user` drops their `users` rows — so without this refusal the
/// predecessor's admin row would leave without `admin.remove`'s quorum or the
/// superadmin floor. Here the predecessor is the nest's ONLY superadmin: the
/// deletion would leave a claimed box administrable by nobody.
#[tokio::test]
async fn deleting_an_account_whose_predecessor_holds_the_admin_role_is_refused() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(Arc::clone(&db)));
    let (retired, successor): ([u8; 32], [u8; 32]) = ([53u8; 32], [54u8; 32]);
    db.create_user(&retired, "free", "").await.unwrap();
    db.record_succession(&retired, &successor, b"s", 1)
        .await
        .unwrap()
        .unwrap();
    // A row granted to the retired key through a pre-fix grant path.
    db.add_admin_actor(&retired[..]).await.unwrap();
    assert!(!db.is_admin(&successor[..]).await.unwrap());

    let err = fauna_nest::pending_actions::finalize_user_deletion(&state, &successor)
        .await
        .expect_err("a predecessor's admin row must refuse the deletion");
    assert!(err.to_string().contains("admin role"), "{err}");
    assert!(
        db.is_admin(&retired[..]).await.unwrap(),
        "the predecessor's admin row leaves only through the floor-checked path"
    );
    assert!(
        db.get_user(&successor).await.unwrap().is_some(),
        "the refused deletion leaves the account standing"
    );
}
