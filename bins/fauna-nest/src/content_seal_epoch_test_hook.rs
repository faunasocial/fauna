//! Test-only HTTP endpoint driving the content-sealing-epochs mail write
//! gate (`AppState::epoch_sealing_enabled`).
//!
//! Gated on `test-hooks` **alone**, so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build. It let a
//! tier_3 prove the epoch-sealing ingest path (B4, content-sealing-epochs
//! design § 6) before the 2026-07-19 write flip; the override is force-on
//! only, so with `MAIL_EPOCH_SEALING_WRITE_DEFAULT = true` it is
//! redundant-but-harmless and kept so pre-flip e2e drives stay valid —
//! mirrors `outbound_clock_test_hook.rs`'s shape exactly.
//!
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use base64::Engine as _;
use fauna_mls::wrapped_blob::SealedRecordBytes;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct EpochSealingBody {
    enabled: bool,
}

/// `POST /api/v1/test/content/epoch_sealing` — force
/// [`AppState::epoch_sealing_enabled`] to `enabled` (`false` restores the
/// production constant). Returns `{"enabled": bool}`.
async fn handle_epoch_sealing(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EpochSealingBody>,
) -> impl IntoResponse {
    state
        .epoch_sealing_test_override
        .store(body.enabled, Ordering::Relaxed);
    Json(json!({ "enabled": state.epoch_sealing_enabled() })).into_response()
}

#[derive(Deserialize)]
struct InjectEpochSealedMailBody {
    actor_id: String,
    /// The exact epoch index to seal under — `actor_id` must have already
    /// published a schedule row for this epoch (real B3 machinery: a real
    /// `provision_recipient_mls_pubkey` call carrying `epoch_keys`), so a
    /// test-author typo (an unpublished epoch) fails loudly rather than
    /// silently sealing under the wrong key.
    epoch: u64,
    raw_rfc5322_b64: String,
    /// The record's stored ingest instant — caller-chosen so a B5 tier_3 can
    /// plant "post-window" content without waiting real wall-clock weeks.
    /// Should fall inside `epoch`'s window (`mail_sealing_epoch_of(timestamp)
    /// == epoch`) or the opener's own epoch-recovery reads a different epoch
    /// than what this hook actually sealed under.
    timestamp: i64,
}

/// `POST /api/v1/test/content/inject_epoch_sealed_mail` — seal
/// `raw_rfc5322_b64` to `actor_id`'s **published epoch-`epoch` key**
/// (`CacheDb::get_actor_epoch_seal_key`, B3a) via the same
/// [`crate::bridge_routing_handlers::seal_recipient_blob`] production seal
/// call `seal_and_persist_local` uses, and persist it with `timestamp` as the
/// record's stored ingest instant — through the real
/// [`crate::bridge_routing_handlers::persist_decoded_inbound_mail`] core.
///
/// This is the content-sealing-epochs analogue of `inject_raw_mail`
/// (`content_seal_test_hook.rs`): production sealing always seals to
/// whichever epoch is wall-clock-current, so there is no live way to plant
/// content sealed under an ARBITRARY chosen epoch without waiting real weeks
/// for that epoch to become current. This hook reuses the real per-epoch
/// public key + the real seal primitive — only the epoch **selection** and
/// the stored **timestamp** are test-chosen — so a B5 tier_3 can prove the
/// § 4/§ 8 windowed-holder bound (in-window opens, post-window stays dark)
/// deterministically.
///
/// Returns `{"ok": true, "epoch": <the epoch actually sealed under>,
/// "message_id_hex": "..."}`. `400` if `actor_id` has no published schedule
/// row at or before `epoch`, or if the newest such row is OLDER than `epoch`
/// (the test must publish the exact epoch first — a silent fallback to an
/// earlier epoch would seal the test fixture under the wrong key).
async fn handle_inject_epoch_sealed_mail(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InjectEpochSealedMailBody>,
) -> impl IntoResponse {
    let actor_id = match fauna_core::hex32::decode(&body.actor_id) {
        Ok(a) => a,
        Err(_) => return ApiError::bad_request("invalid actor_id hex").into_response(),
    };
    let raw_body = match base64::engine::general_purpose::STANDARD.decode(&body.raw_rfc5322_b64) {
        Ok(b) => b,
        Err(_) => return ApiError::bad_request("invalid raw_rfc5322_b64").into_response(),
    };
    if raw_body.is_empty() {
        return ApiError::bad_request("raw_rfc5322_b64 must decode to a non-empty body")
            .into_response();
    }

    let (found_epoch, seal_key) = match state
        .db
        .get_actor_epoch_seal_key(&actor_id, body.epoch)
        .await
    {
        Ok(Some(v)) => v,
        Ok(None) => {
            return ApiError::bad_request(
                "actor has no published epoch-schedule row at or before that epoch",
            )
            .into_response();
        }
        Err(e) => {
            return ApiError::internal(format!("get_actor_epoch_seal_key: {e:?}")).into_response();
        }
    };
    if found_epoch != body.epoch {
        return ApiError::bad_request(format!(
            "actor's newest published epoch <= {} is {found_epoch}, not the exact epoch \
             requested — publish epoch {} first",
            body.epoch, body.epoch
        ))
        .into_response();
    }

    // A placeholder index hint — this hook's whole purpose is the BODY's
    // epoch-key seal; the hint only needs to satisfy the same typed-halves
    // contract persist_decoded_inbound_mail requires.
    let index_hint = fauna_mail::tokenizer::tokenize("test-inject-epoch-sealed").canonical_bytes;
    let pubkey = &seal_key.mls_pubkey;
    let mlkem_ek = Some(seal_key.mlkem_ek.as_slice());
    let sealed_body = match crate::bridge_routing_handlers::seal_recipient_blob(
        &raw_body,
        pubkey,
        mlkem_ek,
        "test-inject-epoch-body",
    ) {
        Ok(b) => b,
        Err(e) => return ApiError::internal(format!("seal body: {e:?}")).into_response(),
    };
    let sealed_hint = match crate::bridge_routing_handlers::seal_recipient_blob(
        &index_hint,
        pubkey,
        mlkem_ek,
        "test-inject-epoch-hint",
    ) {
        Ok(b) => b,
        Err(e) => return ApiError::internal(format!("seal index hint: {e:?}")).into_response(),
    };
    let body_typed = match SealedRecordBytes::verify(sealed_body) {
        Ok(t) => t,
        Err(e) => return ApiError::internal(format!("verify sealed body: {e:?}")).into_response(),
    };
    let hint_typed = match SealedRecordBytes::verify(sealed_hint) {
        Ok(t) => t,
        Err(e) => return ApiError::internal(format!("verify sealed hint: {e:?}")).into_response(),
    };

    // Minted from the plaintext, as every real producer does.
    let dedup = fauna_mail::mail_dedup_keys_from_slice(&raw_body);
    let req = fauna_protocol::bridge_routing::IngestInboundMailRequest {
        actor_id: actor_id.to_vec(),
        dedup_key: dedup.dedup_key,
        envelope_key: dedup.envelope_key,
        public_metadata: fauna_protocol::bridge_routing::PublicMailMetadata {
            timestamp: body.timestamp,
            ciphertext_size: body_typed.len() as u32,
            sender_domain: "epoch-inject.test.invalid".to_string(),
        },
        ..Default::default()
    };

    match crate::bridge_routing_handlers::persist_decoded_inbound_mail(
        state,
        req,
        body_typed,
        hint_typed,
        false,
        fauna_core::data::MailIngress::System,
    )
    .await
    {
        Ok(message_id) => Json(json!({
            "ok": true,
            "epoch": found_epoch,
            "message_id_hex": hex::encode(message_id),
        }))
        .into_response(),
        Err(e) => {
            ApiError::internal(format!("inject_epoch_sealed_mail failed: {e:?}")).into_response()
        }
    }
}

/// Mount the `/api/v1/test/content/{epoch_sealing,inject_epoch_sealed_mail}` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/content/epoch_sealing",
            post(handle_epoch_sealing),
        )
        .route(
            "/api/v1/test/content/inject_epoch_sealed_mail",
            post(handle_inject_epoch_sealed_mail),
        )
}
