//! Test-only HTTP endpoint driving the production outbound clock seam.
//!
//! Gated on `test-hooks` **alone**, so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build that
//! `build_node()` produces. It lets a tier_3 test fast-forward the
//! outbound queue's wall-clock ([`crate::routes::AppState::outbound_now`])
//! so the 4 h delay-warning / 5 d give-up boundaries
//! (`docs/goal/behavior/smtp-server.md` § Retry schedule) are reachable
//! without the test waiting hours.
//!
//! Consumer: `tests/e2e-unified/tests/test_mail_bridge_mta.py::
//! test_outbound_delay_warning_emitted_once`. Production never compiles
//! this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct ClockBody {
    /// Freeze the outbound clock at this absolute epoch second.
    #[serde(default)]
    set: Option<i64>,
    /// Freeze the outbound clock at `outbound_now() + advance` — i.e.
    /// fast-forward from the current effective time (real clock if the
    /// override is still unset, else the already-frozen value).
    #[serde(default)]
    advance: Option<i64>,
}

/// `POST /api/v1/test/outbound/clock` — set or advance the outbound clock
/// override (`0`/unset ⇒ real wall-clock). Returns `{"clock": <epoch>}`.
async fn handle_clock(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ClockBody>,
) -> impl IntoResponse {
    let new = match (body.set, body.advance) {
        (Some(s), None) => s,
        (None, Some(d)) => state.outbound_now() + d,
        (Some(s), Some(d)) => s + d,
        (None, None) => {
            return ApiError::bad_request("must provide `set` or `advance`").into_response();
        }
    };
    state.outbound_clock_override.store(new, Ordering::Relaxed);
    Json(json!({ "clock": new })).into_response()
}

/// Mount the `/api/v1/test/outbound/clock` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/outbound/clock", post(handle_clock))
}
