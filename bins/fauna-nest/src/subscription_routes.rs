//! Subscription-management surviving HTTP routes.
//!
//! The mutating subscription twins (tier CRUD, subscribe/unsubscribe,
//! request approval/rejection, subscriber management, delegation upload)
//! were ripped out in the WS-RPC-everywhere cutover; their behavior now
//! lives solely on the `fauna.subscriptions.*` WS-RPC kinds defined in
//! `libs/fauna-protocol/src/subscriptions.rs` and implemented by
//! `bins/fauna-nest/src/subscription_handlers.rs`.
//!
//! What remains here are the public, unauthenticated residue routes that
//! external (non-Fauna-client) consumers need: the per-author tier listing
//! (`list_tiers` — kept as the deliberately-public web-paywall / federation
//! read per `monetization.md` § Pillar 1; authenticated clients read the
//! same data over `fauna.subscriptions.{offers,tiers}.list`) and the public
//! federation bootstrap endpoints (`nest_info`, `get_delegation`). The three
//! subscriber key-material byte downloads (`get_key_blob`,
//! `get_epoch_secret`, `get_archival_blob`) were deleted in the
//! WS-RPC-everywhere follow-on once every app read them over the
//! `fauna.subscriptions.*.get` kinds — of which only `key_blob.get` remains:
//! the tier-MLS pair (`epoch_secret.get`, `archival_blob.get`) was retired
//! with its writer-less plane by the compat-remnant sweep (2026-09-27,
//! `ui/feed.md` § Encryption at rest, room ruling 8).
//! The nest wraps no period key itself: every tier's key is client-minted
//! (`subscription_handlers.rs`), so no key-rotation helper lives here.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// GET /api/v1/subscriptions/tiers/{author_id}
pub async fn list_tiers(
    State(state): State<Arc<AppState>>,
    Path(author_id_hex): Path<String>,
) -> impl IntoResponse {
    let author_id: [u8; 32] = match fauna_core::hex32::decode(&author_id_hex) {
        Ok(id) => id,
        Err(_) => return ApiError::bad_request("invalid author_id hex").into_response(),
    };

    match state.db.list_subscription_tiers(&author_id).await {
        Ok(tiers) => {
            // The unauthenticated twin of `fauna.subscriptions.offers.list`,
            // so it applies the same exclusion: a per-post pay-to-unlock tier
            // never appears in a generic tier browse (`monetization.md:128`).
            // Filtering the authenticated surface but not this one would
            // leave the enumeration wide open to any browser.
            let items: Vec<serde_json::Value> = tiers
                .into_iter()
                // …and a hidden tier (monetization.md § The unifying model),
                // for the same enumeration reason.
                .filter(|t| t.unlocks_post.is_none() && !t.hidden)
                .map(|t| {
                    json!({
                        "name": t.name,
                        "rank": t.rank,
                        "description": t.description,
                        "price_hint": t.price_hint,
                        "payment_url": t.payment_url,
                        "auto_approve": t.auto_approve,
                        "created_at": t.created_at,
                    })
                })
                .collect();
            Json(items).into_response()
        }
        Err(e) => {
            tracing::error!("list_tiers error: {e}");
            ApiError::internal("storage error").into_response()
        }
    }
}

// ── Nest Info & Delegation ──────────────────────────────────────────────

/// GET /api/v1/nest/info — public, returns nest public key
pub async fn nest_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match &state.nest_signing_key {
        Some(key) => {
            let public = key.verifying_key().to_bytes();
            Json(json!({ "public_key": hex::encode(public) })).into_response()
        }
        None => ApiError::internal("nest keypair not initialized").into_response(),
    }
}

/// GET /api/v1/subscriptions/delegate/{author_id} — public, returns stored delegation payload (hex)
pub async fn get_delegation(
    State(state): State<Arc<AppState>>,
    Path(author_id_hex): Path<String>,
) -> impl IntoResponse {
    let author_id: [u8; 32] = match fauna_core::hex32::decode(&author_id_hex).ok() {
        Some(id) => id,
        None => return ApiError::bad_request("invalid author_id").into_response(),
    };
    match state.db.get_device_authorization(&author_id).await {
        Ok(Some(payload)) => Json(json!({ "payload": hex::encode(&payload) })).into_response(),
        Ok(None) => ApiError::not_found("no delegation found").into_response(),
        Err(e) => {
            tracing::error!("get delegation: {e}");
            ApiError::internal("storage error").into_response()
        }
    }
}
