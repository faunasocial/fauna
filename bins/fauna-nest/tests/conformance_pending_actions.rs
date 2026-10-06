//! Integration round-trip for the authenticated pending-actions surface —
//! `fauna.pending_actions.{list,get,cancel,approve}`. A behavior-preserving
//! transport migration of the bearer-authed HTTP routes
//! (`pending_action_routes::{list,get,cancel,approve}_pending_action(s)`). The
//! handlers reuse the same `CacheDb` pending-action methods the HTTP twins call
//! — these tests exercise the WS-RPC layer: request decode, the reused `CacheDb`
//! reaching a real in-memory store, reply encoding, the actor scoping keyed on
//! the connection actor (replacing the HTTP path-param + bearer match), the
//! error-code mapping, replay metadata, and the `User | Admin` / Admin-only
//! allowlist.
//!
//! Pending actions are seeded directly via the public `CacheDb`
//! `create_pending_action`; an admin approver is seeded via `add_admin_actor`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/pending_actions.rs`.
//! Slice: tracked internally (Track B20 of the WS-RPC-everywhere
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
    pending_action_handlers,
    pending_actions::ActionType,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    decode_strict as decode,
    pending_actions::{
        PendingActionApproveReply, PendingActionApproveRequest, PendingActionCancelReply,
        PendingActionCancelRequest, PendingActionGetReply, PendingActionGetRequest,
        PendingActionsListReply, PendingActionsListRequest,
    },
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    pending_action_handlers::register_pending_actions_handlers(&mut b);
    (b.build(), state)
}

// ── fauna.pending_actions.list ──────────────────────────────────

#[tokio::test]
async fn list_returns_actors_actions() {
    let (router, state) = router_and_state().await;
    let actor = [11u8; 32];
    let id1 = state
        .db
        .create_pending_action(&ActionType::AccountDelete, &actor, None, None, None)
        .await
        .unwrap();
    let id2 = state
        .db
        .create_pending_action(
            &ActionType::HandleChange,
            &actor,
            Some("alice"),
            Some(r#"{"new_handle":"bob"}"#),
            None,
        )
        .await
        .unwrap();

    let reply: PendingActionsListReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.pending_actions.list",
            encode(&PendingActionsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();

    assert_eq!(reply.actions.len(), 2);
    // Both ids present (order is created_at DESC; same-second rows are not
    // strictly ordered, so assert membership + look up by id).
    let ids: Vec<i64> = reply.actions.iter().map(|a| a.id).collect();
    assert!(ids.contains(&id1) && ids.contains(&id2));
    let handle_change = reply.actions.iter().find(|a| a.id == id2).unwrap();
    assert_eq!(handle_change.action_type, "handle.change");
    assert_eq!(handle_change.target.as_deref(), Some("alice"));
    assert_eq!(handle_change.status, "pending");
    assert!(handle_change.approvals.is_empty());
}

#[tokio::test]
async fn list_scopes_to_connection_actor() {
    let (router, state) = router_and_state().await;
    let a = [11u8; 32];
    let b = [99u8; 32];
    state
        .db
        .create_pending_action(&ActionType::AccountDelete, &a, None, None, None)
        .await
        .unwrap();

    // A different connection actor sees none of A's actions.
    let reply: PendingActionsListReply = decode(
        &dispatch(
            &router,
            state,
            b,
            "fauna.pending_actions.list",
            encode(&PendingActionsListRequest {
                extra: Default::default(),
            }),
        )
        .await
        .expect("list ok"),
    )
    .unwrap();
    assert!(reply.actions.is_empty());
}

// ── fauna.pending_actions.get ───────────────────────────────────

#[tokio::test]
async fn get_returns_detail_with_payload_and_ip() {
    let (router, state) = router_and_state().await;
    let actor = [21u8; 32];
    let id = state
        .db
        .create_pending_action(
            &ActionType::HandleChange,
            &actor,
            Some("alice"),
            Some(r#"{"new_handle":"bob"}"#),
            Some("203.0.113.7"),
        )
        .await
        .unwrap();

    let reply: PendingActionGetReply = decode(
        &dispatch(
            &router,
            state,
            actor,
            "fauna.pending_actions.get",
            encode(&PendingActionGetRequest {
                id,
                extra: Default::default(),
            }),
        )
        .await
        .expect("get ok"),
    )
    .unwrap();

    assert_eq!(reply.id, id);
    assert_eq!(reply.action_type, "handle.change");
    assert_eq!(reply.payload.as_deref(), Some(r#"{"new_handle":"bob"}"#));
    assert_eq!(reply.ip_address.as_deref(), Some("203.0.113.7"));
    assert_eq!(reply.status, "pending");
    assert!(reply.executed_at.is_none());
    assert!(reply.approvals.is_empty());
}

#[tokio::test]
async fn get_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.pending_actions.get",
        encode(&PendingActionGetRequest {
            id: 9999,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("missing id → not_found");
    assert_eq!(err.code, "fauna.pending_actions.not_found");
}

#[tokio::test]
async fn get_forbidden_for_other_actor() {
    let (router, state) = router_and_state().await;
    let a = [11u8; 32];
    let b = [99u8; 32];
    let id = state
        .db
        .create_pending_action(&ActionType::AccountDelete, &a, None, None, None)
        .await
        .unwrap();

    // B is permitted at the allowlist layer (User), but the row is A's → the
    // twin's 403 ownership check maps to permission_denied.
    let err = dispatch(
        &router,
        state,
        b,
        "fauna.pending_actions.get",
        encode(&PendingActionGetRequest {
            id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("other actor's row → permission_denied");
    assert_eq!(err.code, "fauna.pending_actions.permission_denied");
}

// ── fauna.pending_actions.cancel ────────────────────────────────

#[tokio::test]
async fn cancel_succeeds_for_creator() {
    let (router, state) = router_and_state().await;
    let actor = [31u8; 32];
    let id = state
        .db
        .create_pending_action(&ActionType::AccountDelete, &actor, None, None, None)
        .await
        .unwrap();

    let reply: PendingActionCancelReply = decode(
        &dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.pending_actions.cancel",
            encode(&PendingActionCancelRequest {
                id,
                extra: Default::default(),
            }),
        )
        .await
        .expect("cancel ok"),
    )
    .unwrap();
    assert!(reply.ok);

    // The cancel actually took: the row is now `cancelled`.
    let row = state.db.get_pending_action(id).await.unwrap().unwrap();
    assert_eq!(row.status, "cancelled");
}

#[tokio::test]
async fn cancel_not_authorized_for_other_actor() {
    let (router, state) = router_and_state().await;
    let a = [31u8; 32];
    let b = [99u8; 32];
    let id = state
        .db
        .create_pending_action(&ActionType::AccountDelete, &a, None, None, None)
        .await
        .unwrap();

    // User action → only the creator may cancel; the DB authz bail maps to
    // permission_denied (the twin's 403).
    let err = dispatch(
        &router,
        state,
        b,
        "fauna.pending_actions.cancel",
        encode(&PendingActionCancelRequest {
            id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("non-creator cancel rejected");
    assert_eq!(err.code, "fauna.pending_actions.permission_denied");
}

#[tokio::test]
async fn cancel_not_found() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.pending_actions.cancel",
        encode(&PendingActionCancelRequest {
            id: 9999,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("missing id → not_found");
    assert_eq!(err.code, "fauna.pending_actions.not_found");
}

#[tokio::test]
async fn cancel_already_cancelled_is_invalid_request() {
    let (router, state) = router_and_state().await;
    let actor = [41u8; 32];
    let id = state
        .db
        .create_pending_action(&ActionType::AccountDelete, &actor, None, None, None)
        .await
        .unwrap();

    // First cancel succeeds.
    dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.pending_actions.cancel",
        encode(&PendingActionCancelRequest {
            id,
            extra: Default::default(),
        }),
    )
    .await
    .expect("first cancel ok");

    // Second cancel → "not cancellable" (status != pending) → invalid_request
    // (the twin's 400, distinct from approve's "not in pending" → 403).
    let err = dispatch(
        &router,
        state,
        actor,
        "fauna.pending_actions.cancel",
        encode(&PendingActionCancelRequest {
            id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("re-cancel rejected");
    assert_eq!(err.code, "fauna.pending_actions.invalid_request");
}

// ── fauna.pending_actions.approve (Admin-only) ──────────────────

#[tokio::test]
async fn approve_succeeds_for_admin_approver() {
    let (router, state) = router_and_state().await;
    let creator = [51u8; 32];
    let approver = [52u8; 32];
    state.db.add_admin_actor(&approver).await.unwrap();
    let target_hex = hex::encode([60u8; 32]);
    let id = state
        .db
        .create_pending_action(
            &ActionType::AdminDeleteUser,
            &creator,
            Some(target_hex.as_str()),
            None,
            None,
        )
        .await
        .unwrap();

    let reply: PendingActionApproveReply = decode(
        &dispatch(
            &router,
            state.clone(),
            approver,
            "fauna.pending_actions.approve",
            encode(&PendingActionApproveRequest {
                id,
                extra: Default::default(),
            }),
        )
        .await
        .expect("approve ok"),
    )
    .unwrap();
    assert!(reply.ok);

    // The approval landed in the row's approvals array.
    let row = state.db.get_pending_action(id).await.unwrap().unwrap();
    let approvals: Vec<String> = serde_json::from_str(&row.approvals).unwrap();
    assert!(approvals.contains(&hex::encode(approver)));
}

#[tokio::test]
async fn approve_self_approval_denied() {
    let (router, state) = router_and_state().await;
    let admin = [61u8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let target_hex = hex::encode([70u8; 32]);
    let id = state
        .db
        .create_pending_action(
            &ActionType::AdminDeleteUser,
            &admin,
            Some(target_hex.as_str()),
            None,
            None,
        )
        .await
        .unwrap();

    // The admin created the action and tries to approve it → self-approval bail
    // → permission_denied (the twin's 403).
    let err = dispatch(
        &router,
        state,
        admin,
        "fauna.pending_actions.approve",
        encode(&PendingActionApproveRequest {
            id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("self-approval rejected");
    assert_eq!(err.code, "fauna.pending_actions.permission_denied");
}

#[tokio::test]
async fn approve_denied_for_non_admin_caller() {
    let (router, state) = router_and_state().await;
    let creator = [71u8; 32];
    let user = [72u8; 32]; // not an admin → CallerClass::User
    let target_hex = hex::encode([80u8; 32]);
    let id = state
        .db
        .create_pending_action(
            &ActionType::AdminDeleteUser,
            &creator,
            Some(target_hex.as_str()),
            None,
            None,
        )
        .await
        .unwrap();

    // The allowlist gate rejects a non-admin caller before any DB work
    // (mirrors the twin's `AdminBearerAuth`).
    let err = dispatch(
        &router,
        state,
        user,
        "fauna.pending_actions.approve",
        encode(&PendingActionApproveRequest {
            id,
            extra: Default::default(),
        }),
    )
    .await
    .expect_err("non-admin approve rejected");
    assert_eq!(err.code, "fauna.pending_actions.permission_denied");
}

// ── malformed payload ───────────────────────────────────────────

#[tokio::test]
async fn list_rejects_malformed_payload() {
    let (router, state) = router_and_state().await;
    let err = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.pending_actions.list",
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
    for kind in [
        "fauna.pending_actions.list",
        "fauna.pending_actions.get",
        "fauna.pending_actions.cancel",
        "fauna.pending_actions.approve",
    ] {
        let m = router.kind_meta(kind).expect("kind registered");
        assert!(!m.forbid_replay, "{kind} does not forbid replay");
        assert_eq!(m.default_deadline, std::time::Duration::from_secs(5));
    }
}

// ── allowlist ───────────────────────────────────────────────────

#[tokio::test]
async fn allowlist_user_admin_for_reads_admin_only_for_approve() {
    for kind in [
        "fauna.pending_actions.list",
        "fauna.pending_actions.get",
        "fauna.pending_actions.cancel",
    ] {
        for class in [CallerClass::User, CallerClass::Admin] {
            assert!(is_permitted(class, kind), "{kind} permitted for {class:?}");
        }
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
    assert!(is_permitted(
        CallerClass::Admin,
        "fauna.pending_actions.approve"
    ));
    assert!(!is_permitted(
        CallerClass::User,
        "fauna.pending_actions.approve"
    ));
}
