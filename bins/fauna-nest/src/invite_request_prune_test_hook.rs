//! Test-only HTTP endpoint that ages one denied join request and runs the
//! denied-request retention sweep once, now.
//!
//! Gated on `test-hooks` **alone** (the `pending_actions_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: a refused join request is cleared ninety days after the
//! refusal, after which the person can ask again with nobody doing anything
//! (`admin.md` § 2. Users, the *denied row is not permanent* paragraph). The
//! sweep runs every few days and keys on the stored `decided_at`, so its
//! *effect* is unreachable from a tier_3 without waiting out three months.
//! The hook moves the named actor's denied row's `decided_at` back by `secs`,
//! then calls the SAME [`crate::db::admin::run_invite_request_retention_sweep`]
//! the background sweeper calls, at the production retention — so what a test
//! observes is the production prune, not a shortcut around it. Scoped to one
//! actor so no other row ages.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_registration_posture.py`.

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
struct PruneRequest {
    /// The actor whose denied request ages, 64 hex chars.
    actor_id: String,
    /// How far back to move its `decided_at`, in seconds (≥ 0).
    secs: i64,
}

/// `POST /api/v1/test/invite_requests/prune_denied` — age one actor's denied
/// request by `secs`, then run the retention sweep once. Returns
/// `{"aged": <bool>, "pruned": <n>}`; `aged` is false when the actor has no
/// denied request.
async fn handle_prune(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PruneRequest>,
) -> impl IntoResponse {
    let Ok(actor_id) = fauna_core::hex32::decode(&req.actor_id) else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    if req.secs < 0 {
        return ApiError::bad_request("secs must not be negative").into_response();
    }
    let aged = match state
        .db
        .test_age_denied_invite_request(&actor_id, req.secs)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            return ApiError::internal(format!("age denied request: {e:#}")).into_response();
        }
    };
    match crate::db::admin::run_invite_request_retention_sweep(
        &state.db,
        crate::db::admin::DEFAULT_INVITE_REQUEST_RETENTION,
    )
    .await
    {
        Ok(pruned) => Json(json!({ "aged": aged, "pruned": pruned })).into_response(),
        Err(e) => ApiError::internal(format!("retention sweep: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/invite_requests/prune_denied` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/invite_requests/prune_denied",
        post(handle_prune),
    )
}
