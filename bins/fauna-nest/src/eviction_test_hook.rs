//! Test-only HTTP endpoint that moves one account's eviction clock forward and
//! runs the eviction worker once.
//!
//! Gated on `test-hooks` **alone** (the `pending_actions_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the eviction ladder's two windows are hard-coded at fourteen
//! days each (`admin.md` § Cutting a user off — eviction and suspension), and
//! the worker ticks once a minute from the timestamps stored on the user's row.
//! So the ladder's *effect* — full access through the warning window, cut off
//! through the suspension window, deleted after it — is unreachable from a
//! tier_3 without waiting out four weeks. This hook is the same "fast-forward
//! the clock, then run the real production path" seam as the pending-actions
//! hook: it moves the named account's `eviction_{warned,suspend,delete}_at`
//! back by `secs`, then calls the SAME [`crate::eviction::run_eviction_tick`]
//! the background task calls, so what a test observes afterwards is the
//! production ladder, not a shortcut around it. Scoped to one account so it is
//! safe on a nest other tests share: no other row's clock moves.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_admin_users_cut_off.py`.

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
struct AdvanceRequest {
    /// The account whose eviction clock moves, 64 hex chars.
    actor_id: String,
    /// How far forward to move it, in seconds (≥ 0).
    secs: i64,
}

/// `POST /api/v1/test/eviction/advance` — move one account's eviction clock
/// forward by `secs`, then run the eviction worker once. Returns
/// `{"shifted": <bool>, "suspended": <n>, "deleted": <n>, "held": <n>}`;
/// `shifted` is false when the account has no eviction standing.
async fn handle_advance(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AdvanceRequest>,
) -> impl IntoResponse {
    let Ok(actor_id) = fauna_core::hex32::decode(&req.actor_id) else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    if req.secs < 0 {
        return ApiError::bad_request("secs must not be negative").into_response();
    }
    let shifted = match state
        .db
        .test_shift_eviction_clock(&actor_id, req.secs)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return ApiError::internal(format!("shift eviction clock: {e:#}")).into_response();
        }
    };
    match crate::eviction::run_eviction_tick(&state).await {
        Ok(tick) => Json(json!({
            "shifted": shifted,
            "suspended": tick.suspended,
            "deleted": tick.deleted,
            "held": tick.held,
        }))
        .into_response(),
        Err(e) => ApiError::internal(format!("run_eviction_tick: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/eviction/advance` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/eviction/advance", post(handle_advance))
}
