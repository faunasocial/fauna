//! Test-only HTTP endpoint minting an EXPIRED web-paywall capability-URL
//! token (`monetization.md` § Pillar 2; `web-content-hosting.md` § Sealed
//! static files).
//!
//! Gated on `test-hooks` **alone** (like `tlsa_test_hook` /
//! `link_preview_test_hook`) so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build. The
//! production `fauna.web.paywall.mint_token` RPC always stamps
//! `now + WEB_PAYWALL_TOKEN_TTL_SECS` (a hard-coded 10-minute TTL — no
//! operator/knob exists to shorten it), so there is no production path to an
//! *expired* token without a tier_3 test waiting out the real TTL. This hook
//! signs one with the real web-serve holder key via
//! `WebPaywallToken::mint_with_expiry`, already past its `expires`.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_web_paywall_folder.py`.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct ExpiredTokenRequest {
    /// Hex-encoded 32-byte owner actor id (the paywalled path's creator).
    owner: String,
    /// The rendered/synced path the token is scoped to.
    path: String,
}

/// `POST /api/v1/test/web-paywall/expired-token` — mint a token for
/// `(owner, path)` that is already expired. Returns `{"token": <base64url>}`.
async fn mint_expired_token(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExpiredTokenRequest>,
) -> impl IntoResponse {
    let Some(holder) = &state.web_serve_holder else {
        return ApiError::service_unavailable(
            "web-paywall serving is not active on this nest (no web-serve holder)",
        )
        .into_response();
    };
    let Ok(owner) = fauna_core::hex32::decode(&body.owner) else {
        return ApiError::bad_request("invalid owner: expected 32-byte hex").into_response();
    };
    let keypair = holder.actor_keypair();
    // 1 = 1970-01-01T00:00:01Z — always in the past.
    match crate::web_content::token::WebPaywallToken::mint_with_expiry(
        &keypair, owner, &body.path, 1,
    ) {
        Ok(token) => Json(json!({ "token": token })).into_response(),
        Err(e) => ApiError::internal(e.to_string()).into_response(),
    }
}

/// Mount the `/api/v1/test/web-paywall/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/web-paywall/expired-token",
        post(mint_expired_token),
    )
}
