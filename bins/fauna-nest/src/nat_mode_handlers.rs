//! Pre-identity NAT-mode commit WS-RPC handler — `fauna.setup.nat_mode`. The
//! sole transport for the NAT-axis set; the ceremony lives in the shared
//! `nat_mode_core`. A thin adapter mapping `ModeCommitError` → `RpcError` and
//! `NatModeOutcome` → reply; there is no write-once `mode_conflict` (the NAT
//! mode is mutable).
//!
//! This kind runs on the **anonymous** WS connection (`GET /api/v1/ws`, no
//! bearer) — enforced by `pre_identity_allowlist` + the dispatcher gate. The
//! signing actor (who must be the committed admin) is authenticated from the
//! request payload's signature, not the connection; the dispatcher's `actor_id`
//! is ignored. The admin's authed client signs the same payload for the
//! post-onboarding admin-panel toggle (design § 5.2).
//!
//! Error codes: `fauna.setup.{invalid_request,signature_failed,not_claimed,
//! forbidden}`; malformed payloads and server faults reuse the
//! `fauna.protocol.*` infra codes.

use std::time::Duration;

use fauna_protocol::decode_strict as decode;
use fauna_protocol::nat_mode::{NatModeReply, NatModeRequest};

use crate::mode_commit::mode_commit_error_to_rpc;
use crate::nat_mode_core;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers ────────────────────────────────────────────────────────────────

use crate::rpc_errors::{encode_reply, malformed};

// ── fauna.setup.nat_mode ─────────────────────────────────────────────────────

fn nat_mode_handler() -> RpcHandler {
    Box::new(|state, _actor, payload| {
        Box::pin(async move {
            let req: NatModeRequest = decode(&payload).map_err(malformed)?;
            let outcome = nat_mode_core::commit_nat_mode_core(
                &state,
                &req.mode,
                &req.actor_id,
                req.timestamp,
                &req.signature,
                &req.nest_id,
            )
            .await
            .map_err(mode_commit_error_to_rpc)?;
            encode_reply(&NatModeReply {
                mode: outcome.mode.as_str().to_string(),
                extra: Default::default(),
            })
        })
    })
}

/// Register the pre-identity NAT-mode commit kind. `forbid_replay = false` @5 s
/// — the set is mutable (a replayed or repeated set is idempotent), and the
/// per-connection idempotency cache replays the first reply on a recovered
/// connection. See `KindRegistry::register_nat_mode_kind` for the client-side
/// metadata twin.
pub fn register_nat_mode_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.setup.nat_mode",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: nat_mode_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode_commit::ModeCommitError;

    /// The shared `ModeCommitError` → `fauna.setup.*` code mapping is the only
    /// transport-specific seam in the WS path (the commit ceremony lives in
    /// `nat_mode_core`). Lock every variant's code.
    #[test]
    fn nat_mode_error_codes_are_stable() {
        assert_eq!(
            mode_commit_error_to_rpc(ModeCommitError::InvalidRequest("x")).code,
            "fauna.setup.invalid_request"
        );
        assert_eq!(
            mode_commit_error_to_rpc(ModeCommitError::SignatureFailed("x")).code,
            "fauna.setup.signature_failed"
        );
        assert_eq!(
            mode_commit_error_to_rpc(ModeCommitError::NotClaimed).code,
            "fauna.setup.not_claimed"
        );
        assert_eq!(
            mode_commit_error_to_rpc(ModeCommitError::NotAdmin).code,
            "fauna.setup.forbidden"
        );
        assert_eq!(
            mode_commit_error_to_rpc(ModeCommitError::Internal("x")).code,
            "fauna.protocol.internal"
        );
    }
}
