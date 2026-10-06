//! Pre-identity one-time admin-claim WS-RPC handler — `fauna.auth.claim_admin`.
//! The sole transport for admin claim (the `POST /api/v1/claim-admin` HTTP twin
//! was removed in S4d); the ceremony lives in the
//! shared `claim_core`. This kind runs **only** on the
//! anonymous WS connection (`GET /api/v1/ws`, no bearer) — enforced by
//! `pre_identity_allowlist` + the dispatcher gate in `routes::dispatch_request`.
//! Part of the WS-RPC-everywhere migration (tracked internally). Mirrors
//! `auth_handlers` / `account_handlers`.
//!
//! The connection's bearer-actor is irrelevant here (there is none) — the
//! handler authenticates the claiming actor from the **request payload** via the
//! signed message, exactly as the HTTP twin did. The dispatcher's `actor_id`
//! argument is therefore ignored.
//!
//! Error codes: `fauna.auth.{invalid_request,signature_failed,invalid_claim_code,
//! already_claimed,superseded}` and `fauna.account.handle_taken` (the A3 code —
//! same concept, uniformity #3); malformed payloads and server faults reuse the
//! `fauna.protocol.*` infra codes.

use std::time::Duration;

use fauna_core::secret::SecretString;
use fauna_protocol::claim::{ClaimAdminReply, ClaimAdminRequest};
use fauna_protocol::{RpcError, Value, decode_strict as decode};

use crate::claim_core::{self, ClaimError};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

/// Map the transport-agnostic `ClaimError` to an `RpcError`. (The HTTP status
/// each variant once mapped to in the removed `claim::post_claim_admin` twin is
/// noted on `ClaimError` for lineage.)
fn claim_error_to_rpc(e: ClaimError) -> RpcError {
    match e {
        ClaimError::InvalidRequest(reason) => {
            let mut r = RpcError::new("fauna.auth.invalid_request", "error.auth.invalid_request");
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        // All signature/key failures collapse to the auth signature code (the
        // HTTP twin returned 403 for every one of them).
        ClaimError::SignatureFailed(reason) => {
            let mut r = RpcError::new("fauna.auth.signature_failed", "error.auth.signature_failed");
            r.details = Some(Box::new(Value::String(reason.to_string())));
            r
        }
        ClaimError::InvalidClaimCode => RpcError::new(
            "fauna.auth.invalid_claim_code",
            "error.auth.invalid_claim_code",
        ),
        ClaimError::AlreadyClaimed => {
            RpcError::new("fauna.auth.already_claimed", "error.auth.already_claimed")
        }
        // Present-but-unreadable claim-code file: a misprovisioned box, NOT a
        // genuine claim. Distinct terminal code so the client shows a truthful,
        // dedicated message instead of the misleading already_claimed.
        ClaimError::ClaimCodeUnreadable(msg) => {
            let mut r = RpcError::new(
                "fauna.auth.claim_code_unreadable",
                "error.auth.claim_code_unreadable",
            );
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
        // Reuse the A3 account code — the same "handle already taken" concept.
        ClaimError::HandleTaken => {
            RpcError::new("fauna.account.handle_taken", "error.account.handle_taken")
        }
        // The auth ceremonies' own refusal, naming the successor.
        ClaimError::Superseded { new_actor_id } => RpcError::superseded(&new_actor_id),
        ClaimError::Internal(msg) => {
            let mut r = RpcError::new("fauna.protocol.internal", "error.protocol.internal");
            r.details = Some(Box::new(Value::String(msg)));
            r
        }
    }
}

// ── fauna.auth.claim_admin (≡ POST /api/v1/claim-admin) ─────────────────────

fn claim_admin_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: ClaimAdminRequest = decode(&payload).map_err(malformed)?;
            let outcome = claim_core::claim_admin_core(
                &state,
                &req.actor_id,
                req.timestamp,
                &req.signature,
                &req.claim_code,
                &req.handle,
                req.mail_domain.as_deref(),
            )
            .await
            .map_err(claim_error_to_rpc)?;
            // Hand the deployment signing seed to the claiming admin's client so it
            // can custody the nest's identity off-box and re-install it after total
            // box loss (box-recovery.md § Mechanism — claim-an-existing box). Gated
            // to the just-authenticated claiming admin, who now holds full nest-admin
            // authority — no new trust boundary. 64-char hex of the raw 32-byte seed
            // (`SigningKey::to_bytes`), matching the request's hex `actor_id`.
            let deployment_seed = state
                .nest_signing_key
                .as_ref()
                .map(|sk| SecretString::from(hex::encode(sk.to_bytes())));
            encode_reply(&ClaimAdminReply {
                token: outcome.token,
                expires_at: outcome.expires_at,
                domain: outcome.domain,
                handle: outcome.handle,
                deployment_seed,
                extra: Default::default(),
            })
        })
    })
}

/// Register the pre-identity one-time admin-claim kind. `forbid_replay = false`
/// @5 s — it mints a bearer like `fauna.auth.handshake`; a one-time replay is
/// safe (the claim-code file is gone after success → `already_claimed`), and the
/// per-connection idempotency cache replays the first reply on a recovered
/// connection. See `KindRegistry::register_claim_admin_kind` for the
/// client-side metadata twin.
pub fn register_claim_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.auth.claim_admin",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: claim_admin_handler(),
        },
    );
}
