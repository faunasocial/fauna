//! The generation **escrow doors** — R14 (account-data-plane.md § The ratified decisions) build step 4, the nest as v1 escrow
//! holder (`account-data-plane.md` § The generation machinery → *The escrow
//! doors*; wire contract: `fauna_protocol::generation_escrow`).
//!
//! All three doors are **User-class and account-derived**: the wraps served
//! and deleted are the authenticated connection actor's own, full stop — an
//! enrolled device and a recovery-ceremony session both authenticate as the
//! account, which is the entire admission story (fleet membership is sealed
//! plane state the nest cannot read, deliberately). The nest stores opaque
//! ciphertext and signs receipts with its **deployment identity**
//! (`AppState::nest_signing_key`) — the key clients already pin — via the
//! shared, holder-generic contract in `fauna_core::generation`
//! (`sign_escrow_receipt`; verification never assumes the deployment key).

use fauna_protocol::generation_escrow::{
    EscrowDeleteReply, EscrowDeleteRequest, EscrowGetReply, EscrowGetRequest, EscrowPutReply,
    EscrowPutRequest, EscrowWrapRow, KIND_ESCROW_DELETE, KIND_ESCROW_GET, KIND_ESCROW_PUT,
    MAX_ESCROW_WRAP_BYTES,
};
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode};

use crate::rpc_errors::{encode_reply, malformed};
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Error namespace for the `fauna.generation.escrow.*` kinds.
const NS: &str = "generation.escrow";

fn coded(code: &str, detail: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::coded_ns(NS, code, detail)
}

fn internal(e: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, e)
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

fn parse_generation_id(bytes: &[u8]) -> Result<[u8; 32], RpcError> {
    bytes
        .try_into()
        .map_err(|_| coded("invalid_request", "generation_id must be 32 bytes"))
}

// ── fauna.generation.escrow.put ──────────────────────────────────────────────

fn escrow_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_ESCROW_PUT).await?;
            let req: EscrowPutRequest = decode(&payload).map_err(malformed)?;
            let generation_id = parse_generation_id(&req.generation_id)?;
            if req.wrap.is_empty() {
                return Err(coded("invalid_request", "wrap must not be empty"));
            }
            if req.wrap.len() > MAX_ESCROW_WRAP_BYTES {
                return Err(coded(
                    "invalid_request",
                    format!(
                        "wrap is {} bytes; the maximum is {MAX_ESCROW_WRAP_BYTES}",
                        req.wrap.len()
                    ),
                ));
            }
            if req.target_key.is_empty() {
                return Err(coded(
                    "invalid_request",
                    "target_key must name the escrow-target row the wrap was sealed under",
                ));
            }
            // The holder identity signs the receipt; a nest still booting
            // without its deployment key cannot act as an escrow holder, and
            // saying so beats an unverifiable receipt.
            let Some(signing_key) = state.nest_signing_key.clone() else {
                return Err(coded(
                    "holder_unavailable",
                    "this nest has no deployment identity to sign receipts with",
                ));
            };

            let wrap_hash: [u8; 32] = blake3::hash(&req.wrap).into();
            let stamped_at_ms = state
                .db
                .put_generation_escrow_wrap(&actor_id, &generation_id, &wrap_hash, &req.wrap)
                .await
                .map_err(internal)?;

            // Durability first, receipt second — the door's contract
            // ("persists durably, then returns the holder-signed receipt").
            // The stamp is the stored row's first-deposit instant, so an
            // idempotent re-put returns a byte-identical receipt.
            let receipt = fauna_core::generation::sign_escrow_receipt(
                &signing_key,
                generation_id,
                wrap_hash,
                &req.target_key,
                stamped_at_ms,
            );
            let receipt_bytes =
                fauna_core::encoding::canonical_encode(&receipt).map_err(internal)?;
            encode_reply(&EscrowPutReply {
                receipt: ByteBuf::from(receipt_bytes),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.generation.escrow.get ──────────────────────────────────────────────

fn escrow_get_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_ESCROW_GET).await?;
            let req: EscrowGetRequest = decode(&payload).map_err(malformed)?;
            let generation_id = req
                .generation_id
                .as_ref()
                .map(|g| parse_generation_id(g))
                .transpose()?;
            let rows = state
                .db
                .get_generation_escrow_wraps(&actor_id, generation_id.as_ref())
                .await
                .map_err(internal)?;
            encode_reply(&EscrowGetReply {
                wraps: rows
                    .into_iter()
                    .map(|r| EscrowWrapRow {
                        generation_id: ByteBuf::from(r.generation_id.to_vec()),
                        wrap: ByteBuf::from(r.wrap),
                        deposited_at_ms: r.created_at,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.generation.escrow.delete ───────────────────────────────────────────

fn escrow_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_ESCROW_DELETE).await?;
            let req: EscrowDeleteRequest = decode(&payload).map_err(malformed)?;
            let generation_id = parse_generation_id(&req.generation_id)?;
            let deleted = state
                .db
                .delete_generation_escrow_wraps(&actor_id, &generation_id)
                .await
                .map_err(internal)?;
            encode_reply(&EscrowDeleteReply {
                deleted,
                extra: Default::default(),
            })
        })
    })
}

/// Register the three escrow doors.
pub fn register_generation_escrow_handlers(b: &mut RpcRouterBuilder) {
    for (kind, handler) in [
        (KIND_ESCROW_PUT, escrow_put_handler()),
        (KIND_ESCROW_GET, escrow_get_handler()),
        (KIND_ESCROW_DELETE, escrow_delete_handler()),
    ] {
        b.add(
            kind,
            RpcKindMeta {
                // A replayed put is harmless by construction (idempotent per
                // (generation id, wrap hash), byte-identical receipt), as are
                // re-reads and re-deletes.
                forbid_replay: false,
                default_deadline: std::time::Duration::from_secs(5),
                handler,
            },
        );
    }
}
