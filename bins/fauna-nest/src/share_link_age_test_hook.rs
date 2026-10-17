//! Test-only HTTP endpoint that moves one author's share-link rows back in time.
//!
//! Gated on `test-hooks` **alone** (the `keypackage_age_test_hook` shape), so it
//! rides the standard `--features test-hooks` e2e build and never compiles into
//! production.
//!
//! Why it exists: an expired link stays in its author's list, marked expired
//! (`share-links.md` § List), and the app marks a row expired by reading its
//! stored `expires_at` against the wall clock. The shortest length a user can
//! choose is a day, so the expiry is unreachable from a tier_3 without waiting
//! it out. The hook moves the named author's `created_at` and `expires_at` back
//! by `secs` and does nothing else; what a test then observes is the production
//! `fauna.share.list` read, unchanged. Scoped to one author so no other row
//! ages.
//!
//! Consumer: `tests/e2e-unified/tests/test_share_links.py`.

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
struct AgeRequest {
    /// The author whose share links age, 64 hex chars.
    actor_id: String,
    /// How far back to move them, in seconds (≥ 0).
    secs: i64,
}

/// `POST /api/v1/test/share_links/age` — move one author's share-link rows
/// back by `secs`. Returns `{"aged": <n>}`, the number of rows moved.
async fn handle_age(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgeRequest>,
) -> impl IntoResponse {
    let Ok(actor_id) = fauna_core::hex32::decode(&req.actor_id) else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    if req.secs < 0 {
        return ApiError::bad_request("secs must not be negative").into_response();
    }
    match state.db.test_age_share_tokens(&actor_id, req.secs).await {
        Ok(aged) => Json(json!({ "aged": aged })).into_response(),
        Err(e) => ApiError::internal(format!("age share links: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/share_links/age` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/share_links/age", post(handle_age))
}
