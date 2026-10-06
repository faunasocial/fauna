//! Push-subscription-management WS-RPC handlers (bearer connection) — part
//! of the WS-RPC-everywhere migration (tracked internally). A
//! behavior-preserving transport migration of the three push-management HTTP routes
//! (`push_routes::{get_vapid_key, subscribe, unsubscribe}`).
//!
//! Four kinds:
//!
//! - `fauna.push.vapid_key` — `PushService::vapid_public_key`.
//! - `fauna.push.subscribe` — `CacheDb::upsert_push_subscription`.
//! - `fauna.push.unsubscribe` — `CacheDb::delete_push_subscription`.
//! - `fauna.push.presence` (ruled 2026-09-26, no HTTP twin) —
//!   `WsState::announce_device`: tags the calling connection with the push
//!   device it serves, the input to the per-device dispatch decision
//!   (`apps/common.md` § Registration → *Every connection announces*).
//!
//! The handlers reuse the same `CacheDb` methods + `PushService` the twins call
//! (no shared core — one call plus reply shaping, like `stats_handlers`).
//!
//! Gate `User | Admin`. `subscribe` / `unsubscribe` are actor-scoped on the
//! connection `actor_id` (each device subscription belongs to the connection
//! actor); `vapid_key` reads the server's public key (the HTTP twin was no-auth,
//! but push subscription is post-login so it migrates onto the authenticated
//! connection — no pre-identity caller). Push *delivery* to Web Push / APNs is
//! server-to-third-party HTTP and stays as is.

use std::time::Duration;

use fauna_protocol::push::{
    PresenceReply, PresenceRequest, SubscribeReply, SubscribeRequest, UnsubscribeReply,
    UnsubscribeRequest, VapidKeyReply, VapidKeyRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for every `fauna.push.*` code.
const NS: &str = "push";

// ── Helpers (mirroring `stats_handlers`, scoped to `push`) ───────────────────

use crate::rpc_errors::{encode_reply, malformed};

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn invalid_request(reason: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::invalid_request_ns(NS, reason)
}

/// The twin's "push service not configured" (`get_vapid_key` returned 404).
fn unavailable() -> RpcError {
    crate::rpc_errors::unavailable_ns(NS, "push service not configured")
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.push.vapid_key (≡ GET /api/v1/push/vapid-key) ──────────────────────

fn vapid_key_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.push.vapid_key").await?;
            let _req: VapidKeyRequest = decode(&payload).map_err(malformed)?;

            let svc = state.push_service.as_ref().ok_or_else(unavailable)?;
            encode_reply(&VapidKeyReply {
                public_key: svc.vapid_public_key().to_string(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.push.subscribe (≡ POST /api/v1/push/subscribe) ─────────────────────

fn subscribe_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.push.subscribe").await?;
            let req: SubscribeRequest = decode(&payload).map_err(malformed)?;

            if req.device_id.is_empty() {
                return Err(invalid_request("device_id is required"));
            }
            if req.endpoint.is_empty() {
                return Err(invalid_request("endpoint is required"));
            }
            let transport = req.transport.as_deref().unwrap_or("web-push");
            match transport {
                // Delivered over the device's own live connection: nothing is
                // dialled, so the row's shape is all there is to check.
                "ws-device" => crate::push::validate_ws_device_subscription(
                    &req.device_id,
                    &req.endpoint,
                    req.key_p256dh.is_some() || req.key_auth.is_some(),
                )
                .map_err(invalid_request)?,
                // The endpoint is a destination the nest will later dial on
                // this caller's say-so: refuse here whatever can never be
                // dialled (`push.rs` § The dial policy; the dial re-checks with
                // DNS).
                "web-push" | "apns" => {
                    crate::push::validate_subscription_endpoint(transport, &req.endpoint)
                        .map_err(invalid_request)?
                }
                _ => {
                    return Err(invalid_request(
                        "transport must be \"web-push\", \"apns\" or \"ws-device\"",
                    ));
                }
            }

            state
                .db
                .upsert_push_subscription(
                    &actor_id,
                    &req.device_id,
                    transport,
                    &req.endpoint,
                    req.key_p256dh.as_deref(),
                    req.key_auth.as_deref(),
                )
                .await
                .map_err(internal)?;
            encode_reply(&SubscribeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.push.unsubscribe (≡ DELETE /api/v1/push/subscribe/{device_id}) ─────

fn unsubscribe_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.push.unsubscribe").await?;
            let req: UnsubscribeRequest = decode(&payload).map_err(malformed)?;

            state
                .db
                .delete_push_subscription(&actor_id, &req.device_id)
                .await
                .map_err(internal)?;
            encode_reply(&UnsubscribeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.push.presence (ruled 2026-09-26) ───────────────────────────────────

fn presence_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, "fauna.push.presence").await?;
            let req: PresenceRequest = decode(&payload).map_err(malformed)?;

            if req.device_id.is_empty() {
                return Err(invalid_request("device_id is required"));
            }
            // The tag lives on the connection the request rode in on, which
            // only the dispatch core knows: the per-actor plane always sets it.
            let conn_id = crate::dispatch_core::current_caller()
                .map(|c| c.conn_id)
                .ok_or_else(|| internal("presence called outside a client connection"))?;
            // A socket already gone has nothing left to tag; its presence ended
            // with it, which is the answer the dispatch needs anyway.
            let _ = state.ws.announce_device(&actor_id, conn_id, &req.device_id);
            encode_reply(&PresenceReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the push-management surface on the **bearer** router. Per-kind
/// replay semantics + rationale: see `KindRegistry::register_push_kinds`. All
/// four are `forbid_replay = false` @5 s (a read + three idempotent writes).
pub fn register_push_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        ("fauna.push.vapid_key", vapid_key_handler()),
        ("fauna.push.subscribe", subscribe_handler()),
        ("fauna.push.unsubscribe", unsubscribe_handler()),
        ("fauna.push.presence", presence_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                forbid_replay: false,
                default_deadline: Duration::from_secs(5),
                handler,
            },
        );
    }
}
