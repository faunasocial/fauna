//! `fauna.pair.*` WS-RPC handlers (bearer connection).
//!
//! The user-facing per-user-multi-homing ("Linked nests") surface:
//!
//! - `fauna.pair.list` — owner-implicit list of the bearer actor's active
//!   pairings; gate `User | Admin`.
//! - `fauna.pair.add` — the user authorizes one of their own nests to sync
//!   their account (owner-scoped; gated by the admin `pairing` service knob).
//! - `fauna.pair.revoke` — the user unlinks a nest (owner-scoped).
//! - `fauna.pair.forward_retry` / `.forward_discard` — the user's two actions
//!   on their own post-forward queue, whose reading (`forward_queue`) rides
//!   `fauna.pair.list` (`private-mode.md` § Post Forwarding → the queue is
//!   the user's to see; 2026-09-23).
//!
//! Authorization is the user's bearer action (per-user-pairing design,
//! 2026-05-25): there is no admin approval and no peer handshake. The retired
//! `POST /api/v1/pair`(+`/revoke`) endpoints and `fauna.admin.pairings.{list,
//! approve}` kinds are gone. The nest↔nest *sync* surface these pairings feed
//! also moved off HTTP — Spec Y2 slice 5 retired the interim `/api/v1/nest-sync/*`
//! routes in favor of the federation WS channel (`/api/v1/federation/ws`;
//! `private-mode.md` § Pairing Flow).

use std::time::Duration;

use bytes::Bytes;

use fauna_protocol::pair::{
    ForwardQueue, PairAddReply, PairAddRequest, PairForwardDiscardReply, PairForwardDiscardRequest,
    PairForwardRetryReply, PairForwardRetryRequest, PairListReply, PairListRequest,
    PairRevokeReply, PairRevokeRequest, PairingRow, default_self_sync,
};
use fauna_protocol::{ByteBuf, RpcError, Value, decode_strict as decode, encode_canonical};

use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use crate::services::ServiceIntent;

/// Error namespace for every `fauna.pair.*` code.
const NS: &str = "pair";

pub fn register_pair_handlers(b: &mut RpcRouterBuilder) {
    fn meta(handler: RpcHandler) -> RpcKindMeta {
        RpcKindMeta {
            forbid_replay: false,
            // Read-only SELECT / owner-scoped single-row write — idempotent on
            // replay (`store_pairing` is INSERT OR REPLACE; `revoke` a DELETE).
            default_deadline: Duration::from_secs(5),
            handler,
        }
    }
    b.add("fauna.pair.list", meta(list_handler()));
    b.add("fauna.pair.add", meta(add_handler()));
    b.add("fauna.pair.revoke", meta(revoke_handler()));
    b.add("fauna.pair.forward_retry", meta(forward_retry_handler()));
    b.add(
        "fauna.pair.forward_discard",
        meta(forward_discard_handler()),
    );
}

// ── Helpers (mirroring `calendar_handlers`, scoped to `pair`) ────────────────

use crate::rpc_errors::malformed;

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

/// The admin has disabled user-initiated pairing nest-wide (the `pairing`
/// service knob, default-on — `public-mode.md` § Nest Pairing Policy).
fn pairing_disabled() -> RpcError {
    let mut e = RpcError::new(
        format!("fauna.{NS}.pairing_disabled"),
        format!("error.{NS}.pairing_disabled"),
    );
    e.details = Some(Box::new(Value::String(
        "nest pairing is disabled by the admin".to_string(),
    )));
    e
}

/// Whether the admin `pairing` service knob is on. Default-**on**: a missing
/// or unreadable `services.json` does not disable pairing (the knob defaults on,
/// and `ServiceFlags`'s field default reads a pre-flag file back as enabled).
fn pairing_enabled(state: &AppState) -> bool {
    match ServiceIntent::read_from(&state.services_json_path) {
        Ok(intent) => intent.services.pairing,
        Err(_) => true,
    }
}

fn encode_reply<T: serde::Serialize>(reply: &T) -> Result<Bytes, RpcError> {
    encode_canonical(reply)
        .map(|v| Bytes::from(v.to_vec()))
        .map_err(|e| internal(format!("encode reply: {e}")))
}

/// Resolve the connection actor's `CallerClass` and check the kind's allowlist
/// arm — the WS-RPC counterpart of the HTTP `BearerAuth` extractor gate.
use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.pair.list (mirrors the shape of the retired HTTP twin
// `GET /api/v1/pairings/{actor}`, deleted in the WS-RPC-everywhere rip-out) ──

fn list_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.pair.list").await?;
            let _req: PairListRequest = decode(&payload).map_err(malformed)?;

            // Owner-implicit: the connection actor scopes the query (the twin
            // checked `bearer.0.0 == actor_id` on the path param).
            let pairings = state
                .db
                .list_pairings_for_actor(&actor)
                .await
                .map_err(internal)?;
            let rows = pairings
                .iter()
                .map(|p| PairingRow {
                    private_nest_id: ByteBuf::from(p.private_nest_id.clone()),
                    capabilities: p.capabilities.clone(),
                    expires_at: p.expires_at,
                    created_at: p.created_at,
                    label: p.label.clone(),
                    nest_url: p.nest_url.clone(),
                    extra: Default::default(),
                })
                .collect();
            // The caller's own forward queue rides the same reply: this is
            // what the Nests page renders, and a stuck forward must be visible
            // from the app, never only in the worker's log (`private-mode.md`
            // § Post Forwarding). Per author, so one user never sees another's
            // count; a public nest answers zeros (nothing enqueues there).
            let queue = state
                .db
                .outbox_status_for_author(&actor)
                .await
                .map_err(internal)?;
            encode_reply(&PairListReply {
                pairings: rows,
                forward_queue: ForwardQueue {
                    queued: queue.queued.max(0) as u64,
                    stuck: queue.stuck.max(0) as u64,
                    last_error: queue.last_error,
                    extra: Default::default(),
                },
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.pair.forward_retry / fauna.pair.forward_discard ────────────────────
// The user's two actions on their own forward queue (`private-mode.md` § Post
// Forwarding → the queue is the user's to see). Owner-implicit like the
// pairing kinds: the connection actor scopes both writes, so a user can only
// re-arm or drop entries they queued themselves.

fn forward_retry_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.pair.forward_retry").await?;
            let _req: PairForwardRetryRequest = decode(&payload).map_err(malformed)?;
            let rearmed = state
                .db
                .outbox_retry_now_for_author(&actor)
                .await
                .map_err(internal)?;
            let _ = state
                .db
                .audit(Some(&actor), "outbox.retry", None, None)
                .await;
            encode_reply(&PairForwardRetryReply {
                rearmed: rearmed as u64,
                extra: Default::default(),
            })
        })
    })
}

fn forward_discard_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.pair.forward_discard").await?;
            let _req: PairForwardDiscardRequest = decode(&payload).map_err(malformed)?;
            let discarded = state
                .db
                .outbox_discard_for_author(&actor)
                .await
                .map_err(internal)?;
            let _ = state
                .db
                .audit(Some(&actor), "outbox.discard", None, None)
                .await;
            encode_reply(&PairForwardDiscardReply {
                discarded: discarded as u64,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.pair.add ───────────────────────────────────────────────────────────

fn add_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.pair.add").await?;
            let req: PairAddRequest = decode(&payload).map_err(malformed)?;

            // Admin nest-level pairing-policy gate (default-on).
            if !pairing_enabled(&state) {
                return Err(pairing_disabled());
            }

            // Empty capabilities → the canonical full self-sync set (NOT the
            // retired "sync"/"federation" strings — per-user-pairing design § 3).
            let capabilities = if req.capabilities.is_empty() {
                default_self_sync()
            } else {
                req.capabilities
            };

            // Owner-implicit: the connection actor scopes the write, so a user
            // can only pair their own account.
            state
                .db
                .store_pairing(
                    &actor,
                    req.private_nest_id.as_ref(),
                    &capabilities,
                    req.expires_at,
                    req.nest_url.as_deref(),
                    req.label.as_deref(),
                )
                .await
                .map_err(internal)?;
            // The row names a peer URL: pin it, and exempt it if the actor is an admin.
            crate::nest_sync_worker::refresh_pairing_targets(&state).await;
            // A nudge, not the mechanism: the one-action link adds the relay's
            // row — the grant a refused forward waits on — at the same moment,
            // so retry this user's queued forwards now rather than at the end
            // of a backoff of up to 8.5 hours (`private-mode.md` § Post
            // Forwarding). Best-effort: the retry finds the grant regardless.
            let _ = state.db.outbox_retry_now_for_author(&actor).await;
            let _ = state
                .db
                .audit(
                    Some(&actor),
                    "pairing.add",
                    Some(&hex::encode(req.private_nest_id.as_ref())),
                    None,
                )
                .await;
            encode_reply(&PairAddReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.pair.revoke ────────────────────────────────────────────────────────

fn revoke_handler() -> RpcHandler {
    Box::new(|state, actor, payload| {
        Box::pin(async move {
            require_permission(&state, &actor, "fauna.pair.revoke").await?;
            let req: PairRevokeRequest = decode(&payload).map_err(malformed)?;

            state
                .db
                .revoke_pairing(&actor, req.private_nest_id.as_ref())
                .await
                .map_err(internal)?;
            crate::nest_sync_worker::refresh_pairing_targets(&state).await;

            let _ = state
                .db
                .audit(
                    Some(&actor),
                    "pairing.revoke",
                    Some(&hex::encode(req.private_nest_id.as_ref())),
                    None,
                )
                .await;
            encode_reply(&PairRevokeReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}
