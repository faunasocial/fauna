//! Pending-actions WS-RPC handlers (bearer connection) — part of the
//! WS-RPC-everywhere migration (tracked internally). A behavior-preserving
//! transport migration of the (now-deleted) bearer-authed HTTP routes
//! `/api/v1/pending-actions{,/{id},/{id}/cancel,/{id}/approve}`. The read/manage complement
//! to the pending-action *creators* on the authenticated account surface
//! (`account_handlers`' `account.delete` / `profile.handle.change`).
//!
//! Each handler reuses the same `CacheDb` pending-action method the twin called
//! — no shared core is needed (the logic is one DB call plus reply shaping),
//! mirroring `register_account_user_handlers`. The connection `actor_id`
//! replaces the HTTP `bearer.0.0`:
//!
//! - `fauna.pending_actions.list` — `list_pending_actions_for_actor(actor_id)`.
//! - `fauna.pending_actions.get` — `get_pending_action(id)`, then the twin's
//!   ownership check (`row.actor_id != actor_id → permission_denied`, the twin's
//!   403).
//! - `fauna.pending_actions.cancel` — `cancel_pending_action(id, actor_id)`; the
//!   DB enforces the creator/target/admin authorization matrix and we map its
//!   error strings to `RpcError` codes exactly as the twin mapped them to HTTP
//!   statuses.
//! - `fauna.pending_actions.approve` — `approve_pending_action(id, actor_id)`.
//!   **Admin-only** (the twin used `AdminBearerAuth`) — enforced by the
//!   `is_permitted` arm; the DB blocks self-approval and is idempotent on
//!   duplicate approvals.
//!
//! Caller class: `User | Admin` for list/get/cancel, `Admin` for approve — see
//! `bridge_method_allowlist::is_permitted`. Error codes are scoped to the
//! `fauna.pending_actions.*` namespace; malformed payloads + server faults map
//! to the `fauna.protocol.*` infra codes.

use std::time::Duration;

use fauna_protocol::pending_actions::{
    PendingActionApproveReply, PendingActionApproveRequest, PendingActionCancelReply,
    PendingActionCancelRequest, PendingActionGetReply, PendingActionGetRequest,
    PendingActionSummary, PendingActionsListReply, PendingActionsListRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::pending_actions::{ActionType, PendingActionRow};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.pending_actions.*` code.
const NS: &str = "pending_actions";

// ── Helpers (mirroring `account_handlers`, scoped to `pending_actions`) ──────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn not_found() -> RpcError {
    crate::rpc_errors::not_found_ns(NS, "pending action not found")
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(NS, reason)
}

fn permission_denied(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::permission_denied_ns(NS, reason)
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` / `AdminBearerAuth`
/// extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// Parse the stored JSON-array string of approver-actor hexes; a malformed
/// blob degrades to an empty list (matching the DB's own
/// `serde_json::from_str(...).unwrap_or_default()`).
fn parse_approvals(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

/// The list summary projection (omits payload / executed_at / ip_address).
fn row_to_summary(r: &PendingActionRow) -> PendingActionSummary {
    PendingActionSummary {
        id: r.id,
        action_type: r.action_type.clone(),
        target: r.target.clone(),
        status: r.status.clone(),
        created_at: r.created_at,
        execute_after: r.execute_after,
        requires_quorum: r.requires_quorum,
        approvals: parse_approvals(&r.approvals),
        extra: Default::default(),
    }
}

/// The per-id detail projection (adds payload / executed_at / ip_address).
fn row_to_detail(r: &PendingActionRow) -> PendingActionGetReply {
    PendingActionGetReply {
        id: r.id,
        action_type: r.action_type.clone(),
        target: r.target.clone(),
        payload: r.payload.clone(),
        status: r.status.clone(),
        created_at: r.created_at,
        execute_after: r.execute_after,
        executed_at: r.executed_at,
        requires_quorum: r.requires_quorum,
        approvals: parse_approvals(&r.approvals),
        ip_address: r.ip_address.clone(),
        extra: Default::default(),
    }
}

// ── fauna.pending_actions.list (≡ GET /api/v1/pending-actions) ──────────────

fn list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.pending_actions.list").await?;
            let _req: PendingActionsListRequest = decode(&payload).map_err(malformed)?;

            let rows = state
                .db
                .list_pending_actions_for_actor(&actor_id)
                .await
                .map_err(internal)?;
            let actions = rows.iter().map(row_to_summary).collect();
            encode_reply(&PendingActionsListReply {
                actions,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.pending_actions.get (≡ GET /api/v1/pending-actions/{id}) ──────────

fn get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.pending_actions.get").await?;
            let req: PendingActionGetRequest = decode(&payload).map_err(malformed)?;

            let row = state
                .db
                .get_pending_action(req.id)
                .await
                .map_err(internal)?
                .ok_or_else(not_found)?;

            // The twin's ownership check — a row owned by another actor → 403 —
            // widened to the party `list` also shows the row to: the target of
            // an admin action against their account (2026-09-24).
            let is_target = ActionType::from_str(&row.action_type)
                .is_some_and(|t| t.is_admin_action_against_user())
                && row.target.as_deref() == Some(hex::encode(actor_id).as_str());
            if row.actor_id != actor_id && !is_target {
                return Err(permission_denied("not authorized"));
            }
            encode_reply(&row_to_detail(&row))
        })
    })
}

// ── fauna.pending_actions.cancel (≡ POST …/{id}/cancel) ─────────────────────

/// Map `cancel_pending_action`'s `anyhow` error to an `RpcError`, mirroring the
/// the (now-deleted) twin's HTTP-status mapping for cancel:
/// "not authorized" → 403, "not found" → 404, "not cancellable" / "already
/// cancelled" → 400, else 500. Match order matters (the authz bail does not
/// contain "not found").
fn map_cancel_error(e: anyhow::Error) -> RpcError {
    let msg = e.to_string();
    if msg.contains("not authorized") {
        permission_denied("not authorized to cancel this action")
    } else if msg.contains("not found") {
        not_found()
    } else if msg.contains("not cancellable") || msg.contains("already cancelled") {
        invalid_request(msg)
    } else {
        tracing::error!("cancel_pending_action error: {e}");
        internal(e)
    }
}

fn cancel_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.pending_actions.cancel").await?;
            let req: PendingActionCancelRequest = decode(&payload).map_err(malformed)?;

            state
                .db
                .cancel_pending_action(req.id, &actor_id)
                .await
                .map_err(map_cancel_error)?;
            // The cancel landed; tell everyone the action concerned — creator,
            // target, co-admins — who called it off
            // (`pending_actions::notify_transition`). A failed read-back costs
            // the notice, never the cancel.
            match state.db.get_pending_action(req.id).await {
                Ok(Some(row)) => {
                    crate::pending_actions::notify_transition(
                        &state,
                        &row,
                        crate::pending_actions::Transition::Cancelled { by: &actor_id },
                    )
                    .await;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(id = req.id, "cancel notice read-back failed: {e}"),
            }
            encode_reply(&PendingActionCancelReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.pending_actions.approve (≡ POST …/{id}/approve) ───────────────────

/// Map `approve_pending_action`'s `anyhow` error to an `RpcError`, mirroring the
/// the (now-deleted) twin's HTTP-status mapping for approve:
/// "self-approval" / "not in pending" → 403, "not found" → 404, else 500. (The
/// twin maps approve's "not in pending status" to 403, unlike cancel's "not
/// cancellable" → 400 — the asymmetry is preserved verbatim.)
fn map_approve_error(e: anyhow::Error) -> RpcError {
    let msg = e.to_string();
    if msg.contains("self-approval") || msg.contains("not in pending") {
        permission_denied(msg)
    } else if msg.contains("not found") {
        not_found()
    } else {
        tracing::error!("approve_pending_action error: {e}");
        internal(e)
    }
}

fn approve_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.pending_actions.approve").await?;
            let req: PendingActionApproveRequest = decode(&payload).map_err(malformed)?;

            match state.db.approve_pending_action(req.id, &actor_id).await {
                Ok(()) => encode_reply(&PendingActionApproveReply {
                    ok: true,
                    extra: Default::default(),
                }),
                Err(e) => Err(map_approve_error(e)),
            }
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the authenticated pending-actions surface on the **bearer** router.
/// Per-kind replay semantics + rationale: see
/// `KindRegistry::register_pending_actions_kinds`. All four are
/// `forbid_replay = false` @5 s — two pure reads plus the idempotent
/// cancel/approve mutations.
pub fn register_pending_actions_handlers(b: &mut RpcRouterBuilder) {
    let read = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add("fauna.pending_actions.list", read(list_handler()));
    b.add("fauna.pending_actions.get", read(get_handler()));
    b.add("fauna.pending_actions.cancel", read(cancel_handler()));
    b.add("fauna.pending_actions.approve", read(approve_handler()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use crate::pending_actions::ActionType;
    use crate::routes::AppState;
    use std::sync::Arc;

    /// A cancel through the real handler tells the action's creator, naming
    /// who called it off (`notifications.md` § Security notices → *Pending
    /// actions*).
    #[tokio::test]
    async fn cancelling_rings_the_creator_once() {
        let creator = [0x41u8; 32];
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        db.create_user(&creator, "free", "test").await.unwrap();
        let state = Arc::new(AppState::for_test(Arc::clone(&db)));
        let id = db
            .create_pending_action(&ActionType::AccountDelete, &creator, None, None, None)
            .await
            .unwrap();

        let payload = fauna_protocol::encode_canonical(&PendingActionCancelRequest {
            id,
            extra: Default::default(),
        })
        .unwrap();
        cancel_handler()(Arc::clone(&state), creator, payload)
            .await
            .expect("the creator may cancel");

        let rows: Vec<_> = db
            .list_notifications(&creator, None, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.notif_type == fauna_protocol::notifications::NotifType::SecurityNotice)
            .collect();
        assert_eq!(rows.len(), 1, "one ring per cancel");
        let body = rows[0].body.as_ref().expect("a localized body");
        assert_eq!(body.key, "notifications.row_security_action_cancelled");
        assert_eq!(
            body.args.get("action_type").map(String::as_str),
            Some("account.delete")
        );
    }
}
