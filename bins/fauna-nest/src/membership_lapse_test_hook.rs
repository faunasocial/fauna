//! Test-only HTTP endpoint for driving the membership-lapse sweep on demand.
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (e2e conventions,
//! `testing.md` point 15: the automation surface is compiled out of release
//! artifacts).
//!
//! **Why this exists.** [`crate::membership_lapse`] reconciles lapsed
//! memberships hourly, with its first tick at boot. A membership whose paid
//! window has already ended when a test pays for it therefore stays at its
//! paid quota tier for up to an hour, and the only other door that reconciles
//! — the refund ingress — is a refund, not a window running out. So any test
//! asserting that a lapse downgrades (or never clobbers a manual tier) would
//! otherwise sleep on a wall clock, which `testing.md` convention 14 forbids.
//! This poke is the sanctioned alternative: run the sweep's exact reconcile
//! once, synchronously, then assert *state*, never timing.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// `POST /api/v1/test/membership_lapse/run-now` — run exactly the reconcile
/// one periodic tick runs (`reconcile_lapsed_memberships(None)`, every buyer)
/// and return how many members it downgraded: `{"ok": true, "downgraded": N}`.
/// `downgraded == 0` is a meaningful negative signal for a test, not an error.
async fn handle_run_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db.reconcile_lapsed_memberships(None).await {
        Ok(n) => Json(json!({ "ok": true, "downgraded": n })).into_response(),
        Err(e) => ApiError::internal(format!("membership lapse run_now: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/membership_lapse/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/membership_lapse/run-now",
        post(handle_run_now),
    )
}
