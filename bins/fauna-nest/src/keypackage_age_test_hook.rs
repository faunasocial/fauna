//! Test-only HTTP endpoint that moves one actor's key packages back in time.
//!
//! Gated on `test-hooks` **alone** (the `invite_request_prune_test_hook`
//! shape), so it rides the standard `--features test-hooks` e2e build and never
//! compiles into production.
//!
//! Why it exists: a starter key expires thirty days after it is published, and
//! an expired one is never handed out and no longer counted
//! (`direct-messages.md` § Key Package Management). Every read keys on the
//! stored `expires_at` against the wall clock, so the expiry is unreachable
//! from a tier_3 without waiting out a month. The hook moves the named actor's
//! `published_at` and `expires_at` back by `secs` and does nothing else; what a
//! test then observes is the production count, fetch and `by_handle` reads,
//! unchanged. Scoped to one actor so no other row ages.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_starter_key_supply.py`.

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
    /// The actor whose key packages age, 64 hex chars.
    actor_id: String,
    /// How far back to move them, in seconds (≥ 0).
    secs: i64,
}

/// `POST /api/v1/test/keypackages/age` — move one actor's key packages back by
/// `secs`. Returns `{"aged": <n>}`, the number of key packages moved.
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
    match state.db.test_age_key_packages(&actor_id, req.secs).await {
        Ok(aged) => Json(json!({ "aged": aged })).into_response(),
        Err(e) => ApiError::internal(format!("age key packages: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/keypackages/age` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/keypackages/age", post(handle_age))
}
