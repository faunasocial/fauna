//! Test-only HTTP endpoint that makes every queued pending action due and runs
//! the executor once.
//!
//! Gated on `test-hooks` **alone** (the `outbound_clock_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: every pending action carries a deliberate cool-off delay
//! (`ActionType::delay_secs` — 6 h for `handle.change`, 14 days for
//! `account.delete`), and the executor ticks once a minute. That makes the
//! *effect* of a pending action — not the queueing, which is already covered —
//! unreachable from a tier_3 without waiting out real hours. This hook is the
//! same "fast-forward the clock, then run the real production path" seam as the
//! outbound-clock hook: it moves `execute_after` back, then calls the SAME
//! [`crate::pending_actions::execute_ready_actions`] the background tick calls,
//! so what a test observes afterwards is the production apply path, not a
//! shortcut around it.
//!
//! Consumer: `tests/e2e-unified/tests/test_atproto_firehose_post.py` (the
//! ATProto rename hook needs a real, applied handle change).

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// `POST /api/v1/test/pending_actions/run_due` — set `execute_after = 0` on
/// every still-pending action, then run the executor once. Returns
/// `{"made_due": <n>, "executed": <n>}`.
async fn handle_run_due(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pending = match state.db.list_all_pending_actions().await {
        Ok(a) => a,
        Err(e) => {
            return ApiError::internal(format!("list pending actions: {e:#}")).into_response();
        }
    };
    let mut made_due = 0usize;
    for action in &pending {
        if action.status != "pending" {
            continue;
        }
        if let Err(e) = state.db.test_set_execute_after(action.id, 0).await {
            return ApiError::internal(format!("set execute_after: {e:#}")).into_response();
        }
        made_due += 1;
    }
    match crate::pending_actions::execute_ready_actions(&state).await {
        Ok(executed) => Json(json!({ "made_due": made_due, "executed": executed })).into_response(),
        Err(e) => ApiError::internal(format!("execute_ready_actions: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/pending_actions/run_due` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/pending_actions/run_due", post(handle_run_due))
}
