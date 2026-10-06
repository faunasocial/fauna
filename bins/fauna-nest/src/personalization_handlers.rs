//! WS-RPC handlers for the sealed personalization-model plane —
//! `fauna.personalization.model.{fetch,put,delete}`
//! (`docs/goal/behavior/topic-factors.md` § Wire & registry; payload types in
//! `libs/fauna-protocol/src/personalization.rs`; at-rest home
//! `db/personalization.rs`).
//!
//! All three kinds are **User-class** (`bridge_method_allowlist::is_permitted`,
//! the `fauna.feed.factors.*` treatment). Owner-scoping is by construction:
//! every DB accessor keys on the connection's authenticated `actor_id`, so a
//! caller can never read, overwrite, or delete another actor's rows.
//!
//! The blob is sealed client-side under the BackupKey and **nest-opaque from
//! birth** — these handlers validate only the *envelope* (factor namespace,
//! blob size, per-actor factor cap) and never look inside the bytes.
//! `sample_count` is ADVISORY (an adopt-if-larger cross-device reconcile
//! hint) — stored and echoed verbatim, never validated against the blob.

use std::time::Duration;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_protocol::personalization::{
    KIND_MODEL_DELETE, KIND_MODEL_FETCH, KIND_MODEL_PUT, PersonalizationModelDeleteReply,
    PersonalizationModelDeleteRequest, PersonalizationModelFetchReply,
    PersonalizationModelFetchRequest, PersonalizationModelPutReply, PersonalizationModelPutRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

/// Hard cap on a *sealed* personalization-model blob accepted at put —
/// 512 KiB (`docs/goal/behavior/topic-factors.md` § At rest — seal + home;
/// the client-side self-cap on the *plaintext* model is
/// `fauna_text_model::TOPIC_MODEL_MAX_BYTES` = 256 KiB, so a compliant client
/// never comes near this). Hard-coded Rust — no human chooses it.
pub const PERSONALIZATION_MODEL_MAX_PUT_BYTES: usize = 524_288;

/// Max trained factors per actor — owned by the wire crate so the client's
/// registry-add guard and this create-cap share one definition
/// (`fauna_protocol::personalization::TRAINED_FACTORS_MAX`); re-exported here
/// for the conformance tests and the put handler.
pub use fauna_protocol::personalization::TRAINED_FACTORS_MAX;

/// Error namespace for every `fauna.personalization.*` code.
const NS: &str = "personalization";

// ── Helpers (mirroring `engagement_handlers`, scoped to `personalization`) ──

use crate::rpc_errors::malformed;

fn invalid_params(reason: &str) -> RpcError {
    crate::rpc_errors::invalid_params_ns(NS, reason)
}

fn internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal_ns(NS, err)
}

fn encode_reply<T: serde::Serialize>(reply: &T) -> Result<Bytes, RpcError> {
    encode_canonical(reply)
        .map(|v| Bytes::from(v.to_vec()))
        .map_err(|e| internal(format!("encode reply: {e}")))
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

/// The sealed personalization-model wire accepts the `topic:` trained-model
/// namespace and the `cues:` engagement-cue rollup namespace
/// (`fauna_core::scoring::is_personalization_model_factor` — envelope-only; the
/// sealed blob stays opaque). Additive per `engagement-cues.md` § Seal + home
/// ("the nest-side put validation extends its accepted prefix set from `topic:`
/// to `topic: | cues:`"); future sealed model kinds added *beside* these widen it.
fn require_personalization_model_factor(factor: &str) -> Result<(), RpcError> {
    if !fauna_core::scoring::is_personalization_model_factor(factor) {
        return Err(invalid_params(&format!(
            "factor {factor:?} is not an accepted personalization-model key \
             (v1 accepts the topic: and cues: namespaces)"
        )));
    }
    Ok(())
}

// ── fauna.personalization.model.fetch ───────────────────────────────────────

fn model_fetch_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_MODEL_FETCH).await?;
            let req: PersonalizationModelFetchRequest = decode(&payload).map_err(malformed)?;
            require_personalization_model_factor(&req.factor)?;

            let row = state
                .db
                .get_personalization_model(&actor_id, &req.factor)
                .await
                .map_err(|e| internal(format!("get_personalization_model: {e}")))?;

            // Absent row ⇒ the documented empty shape (the client starts
            // from a fresh empty model).
            let (sealed_blob, sample_count, updated_at) = match row {
                // The stored count always fits (put stores `i64::from(u32)`);
                // clamp defensively rather than error on a hand-edited row.
                Some((blob, count, at)) => (
                    Some(ByteBuf::from(blob)),
                    u32::try_from(count.max(0)).unwrap_or(u32::MAX),
                    at,
                ),
                None => (None, 0, 0),
            };
            encode_reply(&PersonalizationModelFetchReply {
                sealed_blob,
                sample_count,
                updated_at,
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.personalization.model.put ─────────────────────────────────────────

fn model_put_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_MODEL_PUT).await?;
            let req: PersonalizationModelPutRequest = decode(&payload).map_err(malformed)?;
            require_personalization_model_factor(&req.factor)?;
            if req.sealed_blob.is_empty() {
                return Err(invalid_params(
                    "sealed_blob is empty (delete the factor instead of putting an empty blob)",
                ));
            }
            if req.sealed_blob.len() > PERSONALIZATION_MODEL_MAX_PUT_BYTES {
                return Err(invalid_params(&format!(
                    "sealed_blob is {} bytes; the cap is {PERSONALIZATION_MODEL_MAX_PUT_BYTES} \
                     (512 KiB — topic-factors.md § At rest)",
                    req.sealed_blob.len()
                )));
            }

            let stored = state
                .db
                .put_personalization_model(
                    &actor_id,
                    &req.factor,
                    &req.sealed_blob,
                    i64::from(req.sample_count),
                    TRAINED_FACTORS_MAX as i64,
                )
                .await
                .map_err(|e| internal(format!("put_personalization_model: {e}")))?;
            if !stored {
                return Err(invalid_params(&format!(
                    "trained-factor cap reached ({TRAINED_FACTORS_MAX} per actor — \
                     topic-factors.md § At rest); delete a factor to create a new one"
                )));
            }
            encode_reply(&PersonalizationModelPutReply {
                status: "ok".into(),
                extra: Default::default(),
            })
        })
    })
}

// ── fauna.personalization.model.delete ──────────────────────────────────────

fn model_delete_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_MODEL_DELETE).await?;
            let req: PersonalizationModelDeleteRequest = decode(&payload).map_err(malformed)?;
            require_personalization_model_factor(&req.factor)?;

            // Idempotent: deleting an absent row succeeds, `deleted: false`.
            let deleted = state
                .db
                .delete_personalization_model(&actor_id, &req.factor)
                .await
                .map_err(|e| internal(format!("delete_personalization_model: {e}")))?;
            encode_reply(&PersonalizationModelDeleteReply {
                status: "ok".into(),
                deleted,
                extra: Default::default(),
            })
        })
    })
}

// ── Registration entry point ────────────────────────────────────────────────

/// Register the personalization surface on the bearer router. All three
/// kinds are `forbid_replay = false` @5 s — a pure read plus two idempotent
/// owner-keyed writes. See `KindRegistry::register_personalization_kinds`.
pub fn register_personalization_handlers(b: &mut RpcRouterBuilder) {
    let meta = |handler| RpcKindMeta {
        forbid_replay: false,
        default_deadline: Duration::from_secs(5),
        handler,
    };
    b.add(KIND_MODEL_FETCH, meta(model_fetch_handler()));
    b.add(KIND_MODEL_PUT, meta(model_put_handler()));
    b.add(KIND_MODEL_DELETE, meta(model_delete_handler()));
}
