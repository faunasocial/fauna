//! Test-only HTTP endpoint for driving the custody-hosting pump on demand.
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (e2e conventions,
//! `testing.md` point 15: the automation surface is compiled out of release
//! artifacts).
//!
//! **Why this exists.** [`CustodyHostingWorker`] ticks every
//! [`crate::custody_hosting_worker::PULL_INTERVAL`] — 15 minutes — with the
//! first tick at boot. A hosting row deposited *after* boot therefore gets no
//! pull for up to a quarter hour, so any test asserting that the custodian
//! nest pulled (or that a receipt reached the owner) would otherwise sleep on
//! a wall clock — exactly the brittleness `testing.md` convention 14 forbids.
//! This poke is the sanctioned alternative: run one sweep synchronously, then
//! assert *state*, never timing.
//!
//! The poke alone does not stop the periodic loop: its first tick fires at
//! boot and the next every 15 minutes, so on a nest that lives across tests
//! (a module-scoped fixture) a scheduled pass can land mid-journey and make a
//! "nothing confirmed yet" precondition a race. `POST .../hold` suspends the
//! periodic loop's passes on this nest until released; `run-now` still runs.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::custody_hosting_worker::CustodyHostingWorker;
use crate::routes::AppState;

/// `POST /api/v1/test/custody_hosting/run-now` — run exactly one
/// custody-hosting sweep and return its pass report.
///
/// Returns `{"ok": true, "rows": N, "pulled": N, "recorded": N,
/// "adopted_segments": N, "stopped": N, "refused_url": N, "failed": N,
/// "receipts_deposited": N, "receipts_failed": N}`.
/// `rows == 0` is a meaningful negative signal for a test, not an error.
///
/// The sweep runs on the blocking pool driven by this runtime's handle,
/// exactly as [`CustodyHostingWorker::spawn`] does and for the same reasons
/// (blocking-shaped, `!Send` store futures).
async fn handle_run_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sweep = tokio::task::spawn_blocking(move || {
        let worker = CustodyHostingWorker::new(state, std::time::Duration::MAX);
        tokio::runtime::Handle::current().block_on(worker.run_once())
    })
    .await;

    match sweep {
        Ok(Ok(report)) => Json(json!({
            "ok": true,
            "rows": report.rows,
            "stopped": report.stopped,
            "refused_url": report.refused_url,
            "pulled": report.pulled,
            "recorded": report.recorded,
            "adopted_segments": report.adopted_segments,
            "failed": report.failed,
            "receipts_deposited": report.receipts_deposited,
            "receipts_failed": report.receipts_failed,
        }))
        .into_response(),
        Ok(Err(e)) => ApiError::internal(format!("custody hosting run_now: {e:#}")).into_response(),
        Err(e) => ApiError::internal(format!("custody hosting run_now join: {e}")).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct HoldBody {
    held: bool,
}

/// `POST /api/v1/test/custody_hosting/hold` `{"held": bool}` — hold (or
/// release) the periodic loop: while held every scheduled tick skips its
/// pass. Returns `{"ok": true, "held": bool}`.
async fn handle_hold(
    State(state): State<Arc<AppState>>,
    Json(body): Json<HoldBody>,
) -> impl IntoResponse {
    state
        .custody_hosting_periodic_held
        .store(body.held, std::sync::atomic::Ordering::SeqCst);
    Json(json!({ "ok": true, "held": body.held }))
}

/// Mount the `/api/v1/test/custody_hosting/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/custody_hosting/run-now", post(handle_run_now))
        .route("/api/v1/test/custody_hosting/hold", post(handle_hold))
}
