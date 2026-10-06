//! WS-RPC handlers for the user-facing `fauna.notifications.*` surface —
//! the list / mark-read / count plane end-user clients invoke from the
//! notifications inbox + unread badge. Part of the WS-RPC-everywhere
//! migration (tracked internally).
//!
//! Caller-class enforcement lives in `bridge_method_allowlist::is_permitted`
//! (User-only arms for these kinds); the same gate the `fauna.posts.*`,
//! `fauna.feed.*`, and `fauna.conversations.*` user-facing kinds use,
//! partitioned by `CallerClass`.
//!
//! The business logic is **not** duplicated here — each handler decodes its
//! wire request, then calls the same `CacheDb` methods the HTTP twins also
//! call (`list_notifications` / `mark_notifications_read` /
//! `count_unread_notifications`), and maps the result onto the
//! `fauna.notifications.*` reply / `RpcError` shapes. The connection
//! `actor_id` replaces the HTTP `{actor_id}` path param + bearer-match
//! (exactly the posts/feed treatment). The HTTP twins (`notif_routes.rs`) +
//! the `paths::notifications` constants were deleted in T4 — these kinds are
//! the sole surface now.

use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::{
    RpcError, decode_strict as decode, encode_canonical,
    notifications::{
        CODE_NOTIFICATION_RETAINED, NotifClearReply, NotifClearRequest, NotifCountReply,
        NotifCountRequest, NotifDismissReply, NotifDismissRequest, NotifItem, NotifListReply,
        NotifListRequest, NotifMarkReadReply, NotifMarkReadRequest,
    },
};

use crate::db::notifications::DismissOutcome;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────

use crate::rpc_errors::malformed;

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns("notifications", err)
}

fn encode_reply<T: serde::Serialize>(reply: &T) -> Result<Bytes, RpcError> {
    encode_canonical(reply)
        .map(|v| Bytes::from(v.to_vec()))
        .map_err(|e| internal(format!("encode reply: {e}")))
}

/// `fauna.notifications.retained` — a dismiss of the caller's own
/// security notice inside its window (§ Retention). A within-family refusal:
/// `RpcError::action()` reads it as `Rejected` (no retry will change it
/// before the window ends).
fn retained() -> RpcError {
    RpcError::new(CODE_NOTIFICATION_RETAINED, "error.notifications.retained")
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.notifications.list ───────────────────────────────────

fn notifications_list_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.notifications.list").await?;
            let req: NotifListRequest = decode(&payload).map_err(malformed)?;

            // Match the HTTP twin's default + clamp (default 25, 1..=100).
            let limit = req.limit.unwrap_or(25).clamp(1, 100);

            let rows = state
                .db
                .list_notifications(&actor_id, req.cursor, limit)
                .await
                .map_err(internal)?;

            // The next-page cursor is the id of the last (oldest) row in
            // this page — exactly the HTTP twin's `next_cursor`.
            let cursor = rows.last().map(|r| r.id);
            let notifications: Vec<NotifItem> = rows
                .into_iter()
                .map(|r| NotifItem {
                    id: r.id,
                    notif_type: r.notif_type,
                    source: r.source,
                    sender_id: r.sender_id.as_deref().map(hex::encode),
                    content_id: r.content_id.as_deref().map(hex::encode),
                    subject_uri: r.subject_uri,
                    summary: r.summary,
                    body: r.body,
                    is_read: r.is_read,
                    created_at: r.created_at,
                    extra: std::collections::BTreeMap::new(),
                })
                .collect();

            encode_reply(&NotifListReply {
                notifications,
                cursor,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.notifications.mark_read ──────────────────────────────

fn notifications_mark_read_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.notifications.mark_read").await?;
            let req: NotifMarkReadRequest = decode(&payload).map_err(malformed)?;

            // Default to "now" when `up_to` is omitted — the HTTP twin's
            // behavior (mark everything up to the current time).
            let up_to = req
                .up_to
                .unwrap_or_else(|| fauna_core::data::Timestamp::now_or_zero().as_i64());

            let count = state
                .db
                .mark_notifications_read(&actor_id, up_to)
                .await
                .map_err(internal)?;

            encode_reply(&NotifMarkReadReply {
                marked_read: count as i64,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.notifications.count ──────────────────────────────────

fn notifications_count_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.notifications.count").await?;
            // Decode for the `extra` forward-compat envelope (the count
            // request carries no params besides the connection actor).
            let _req: NotifCountRequest = decode(&payload).map_err(malformed)?;

            let count = state
                .db
                .count_unread_notifications(&actor_id)
                .await
                .map_err(internal)?;

            encode_reply(&NotifCountReply {
                count,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.notifications.dismiss ────────────────────────────────
//
// The two user-initiated deletes `behavior/notifications.md` § Retention
// rules (rule 2). Nothing on the nest sweeps a notification row by age or
// count — it is the user's own record — so these, and a knock doorbell going
// with its knock, are the only deletes short of the account going.

fn notifications_dismiss_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.notifications.dismiss").await?;
            let req: NotifDismissRequest = decode(&payload).map_err(malformed)?;

            // Scoped to the connection actor inside the DELETE itself: an id
            // that is another actor's row deletes nothing and reads as
            // "not found", never as someone else's delete. A security notice
            // inside its window is refused, not reported as not-found
            // (§ Retention, the security-notice window).
            let now = fauna_core::data::Timestamp::now_or_zero().as_i64();
            let dismissed = match state
                .db
                .dismiss_notification(&actor_id, req.id, now)
                .await
                .map_err(internal)?
            {
                DismissOutcome::Dismissed => true,
                DismissOutcome::NotFound => false,
                DismissOutcome::Retained => return Err(retained()),
            };

            encode_reply(&NotifDismissReply {
                dismissed,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── fauna.notifications.clear ──────────────────────────────────

fn notifications_clear_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.notifications.clear").await?;
            let req: NotifClearRequest = decode(&payload).map_err(malformed)?;

            // Default to "now" when `up_to` is omitted — `mark_read`'s shape.
            // Retained security notices are skipped, not counted.
            let now = fauna_core::data::Timestamp::now_or_zero().as_i64();
            let up_to = req.up_to.unwrap_or(now);

            let cleared = state
                .db
                .clear_notifications(&actor_id, up_to, now)
                .await
                .map_err(internal)?;

            encode_reply(&NotifClearReply {
                cleared: cleared as i64,
                extra: std::collections::BTreeMap::new(),
            })
        })
    })
}

// ── Registration entry point ───────────────────────────────────

pub fn register_notifications_handlers(b: &mut RpcRouterBuilder) {
    // Per-kind replay semantics + rationale: see
    // `KindRegistry::register_notifications_kinds`. All five are
    // `forbid_replay = false` @5 s — list/count are pure reads, mark_read is
    // an idempotent upsert (no score-increment hazard like posts.interact),
    // dismiss/clear are idempotent deletes (a replay deletes nothing new).
    b.add(
        "fauna.notifications.dismiss",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: notifications_dismiss_handler(),
        },
    );
    b.add(
        "fauna.notifications.clear",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: notifications_clear_handler(),
        },
    );
    b.add(
        "fauna.notifications.list",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: notifications_list_handler(),
        },
    );
    b.add(
        "fauna.notifications.mark_read",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: notifications_mark_read_handler(),
        },
    );
    b.add(
        "fauna.notifications.count",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: notifications_count_handler(),
        },
    );
}
