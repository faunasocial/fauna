//! Pre-identity in-band invite WS-RPC handlers —
//! `fauna.account.invite_request.{submit,status,cancel}` +
//! `fauna.account.invite_code.verify`. Behavior-preserving transport migrations
//! of the public invite routes — the `/api/v1/invite-requests*` +
//! `/api/v1/invite-code/verify` HTTP twins, retired in S4a2/S4b; the ceremonies
//! live in the shared `invite_core`. These kinds run **only** on the anonymous
//! WS connection
//! (`GET /api/v1/ws`, no bearer) — enforced by `pre_identity_allowlist` + the
//! dispatcher gate in `routes::dispatch_request`. Part of the
//! WS-RPC-everywhere migration (tracked internally). Mirrors `account_handlers` /
//! `claim_handlers`.
//!
//! The connection's bearer-actor is irrelevant (there is none) — submit/cancel
//! authenticate the actor from the **request payload** via the signed message;
//! status/verify are public reads. The dispatcher's `actor_id` argument is
//! ignored.
//!
//! Error codes reuse the A3 `fauna.account.*` family where the concept matches
//! (`invalid_request`, `signature_failed`, `actor_exists`, `handle_taken`) and
//! add `invite_request_exists`, `invite_request_not_found`, `invite_code_invalid`;
//! `submit` also answers the auth ceremonies' `fauna.auth.superseded` for a
//! retired key (`auth_core::successor_of`); malformed payloads and server
//! faults reuse the `fauna.protocol.*` infra codes.

use std::time::Duration;

use fauna_protocol::invite::{
    InviteCodeVerify, InviteCodeVerifyReply, InviteRequestCancel, InviteRequestCancelReply,
    InviteRequestStatus, InviteRequestStatusQuery, InviteRequestSubmit,
};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::db::InviteRequestRow;
use crate::invite_core::{self, InviteError};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

/// Map a `db::InviteRequestRow` to the `InviteRequestStatus` wire type — the
/// WS-RPC counterpart of the HTTP twin's `status_json`.
fn row_to_status(row: &InviteRequestRow) -> InviteRequestStatus {
    InviteRequestStatus {
        id: row.id,
        actor_id: hex::encode(&row.actor_id),
        handle: row.handle.clone(),
        message: row.message.clone(),
        status: row.status.clone(),
        created_at: row.created_at,
        decided_at: row.decided_at,
        decided_by: row.decided_by.as_ref().map(hex::encode),
        denial_reason: row.denial_reason.clone(),
        extra: Default::default(),
    }
}

/// Map the transport-agnostic `InviteError` to an `RpcError`. (The HTTP status
/// mapping in the retired `invite_requests` twins was the historical sibling.)
fn invite_error_to_rpc(e: InviteError) -> RpcError {
    match e {
        InviteError::InvalidRequest(reason) => {
            let mut r = RpcError::new(
                "fauna.account.invalid_request",
                "error.account.invalid_request",
            );
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        InviteError::SignatureFailed(reason) => {
            let mut r = RpcError::new(
                "fauna.account.signature_failed",
                "error.account.signature_failed",
            );
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        InviteError::ActorAlreadyRegistered => {
            RpcError::new("fauna.account.actor_exists", "error.account.actor_exists")
        }
        InviteError::Superseded { new_actor_id } => RpcError::superseded(&new_actor_id),
        InviteError::HandleTaken => {
            RpcError::new("fauna.account.handle_taken", "error.account.handle_taken")
        }
        // The HTTP twin returned the existing row with 409; on the WS surface the
        // caller re-queries via `invite_request.status` to read it.
        InviteError::AlreadyExists(_) => RpcError::new(
            "fauna.account.invite_request_exists",
            "error.account.invite_request_exists",
        ),
        InviteError::InviteRequestNotFound(_) => RpcError::new(
            "fauna.account.invite_request_not_found",
            "error.account.invite_request_not_found",
        ),
        InviteError::InviteCodeInvalid => RpcError::new(
            "fauna.account.invite_code_invalid",
            "error.account.invite_code_invalid",
        ),
        InviteError::AgeAttestationInvalid(reason) => {
            let mut r = RpcError::new(
                "fauna.account.age_attestation_invalid",
                "error.account.age_attestation_invalid",
            );
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        // Global pending-row cap reached — a transient capacity refusal. Reuse
        // the throttle code clients already handle rather than minting a new
        // client-facing error (the caller retries once the backlog clears).
        InviteError::TooManyPending => {
            RpcError::new("fauna.protocol.rate_limited", "error.protocol.rate_limited")
        }
        InviteError::Internal(msg) => {
            let mut r = RpcError::new("fauna.protocol.internal", "error.protocol.internal");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
    }
}

// ── fauna.account.invite_request.submit ─────────────────────────────────────

fn submit_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: InviteRequestSubmit = decode(&payload).map_err(malformed)?;
            let row = invite_core::submit_invite_request_core(
                &state,
                &req.actor_id,
                &req.handle,
                &req.message,
                req.timestamp,
                &req.signature,
                req.age_claim.as_ref(),
            )
            .await
            .map_err(invite_error_to_rpc)?;
            encode_reply(&row_to_status(&row))
        })
    })
}

// ── fauna.account.invite_request.status ─────────────────────────────────────

fn status_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: InviteRequestStatusQuery = decode(&payload).map_err(malformed)?;
            let row = invite_core::get_invite_request_status_core(&state, &req.actor_id)
                .await
                .map_err(invite_error_to_rpc)?;
            encode_reply(&row_to_status(&row))
        })
    })
}

// ── fauna.account.invite_request.cancel ─────────────────────────────────────

fn cancel_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: InviteRequestCancel = decode(&payload).map_err(malformed)?;
            invite_core::cancel_invite_request_core(
                &state,
                &req.actor_id,
                req.timestamp,
                &req.signature,
            )
            .await
            .map_err(invite_error_to_rpc)?;
            encode_reply(&InviteRequestCancelReply {
                ok: true,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.account.invite_code.verify ────────────────────────────────────────

fn verify_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: InviteCodeVerify = decode(&payload).map_err(malformed)?;
            let (invite_id, supervised_by, age_band) =
                invite_core::verify_invite_code_core(&state, &req.code)
                    .await
                    .map_err(invite_error_to_rpc)?;
            encode_reply(&InviteCodeVerifyReply {
                invite_id,
                supervised_by,
                age_band,
                extra: Default::default(),
            })
        })
    })
}

/// Register the pre-identity invite kinds. `submit` is `forbid_replay = false`
/// @30 s (a write, like `fauna.account.register`); `status` / `verify` are pure
/// reads and `cancel` an idempotent delete @5 s. The per-IP rate-limit the
/// retired HTTP twin carried on `verify` is **restored** as a dispatcher gate
/// (1b‴, `routes.rs`) keyed on the anonymous WS peer IP — the "no peer IP yet"
/// deferral is obsolete now that `WithConnectInfo` carries the real client IP on
/// the TLS path (see `crate::anonymous_rate_limit::invite_verify_config`).
/// `submit`'s per-source throttle is **also** restored (dispatcher gate 1b⁗⁗,
/// keyed on the WS peer IP, `crate::anonymous_rate_limit::register_config`'s
/// sibling `invite_request_config`) and backed by a global pending-row cap in
/// `invite_core` (`MAX_PENDING_INVITE_REQUESTS`) — closing the queue-spam
/// hardening item the WS migration deferred (security review § D6). See
/// `KindRegistry::register_invite_kinds` for the client-side metadata twin.
pub fn register_invite_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.account.invite_request.submit",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: submit_handler(),
        },
    );
    b.add(
        "fauna.account.invite_request.status",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: status_handler(),
        },
    );
    b.add(
        "fauna.account.invite_request.cancel",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: cancel_handler(),
        },
    );
    b.add(
        "fauna.account.invite_code.verify",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: verify_handler(),
        },
    );
}
