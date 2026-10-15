//! Test-only HTTP endpoint: make `fauna.bridges.mail_health` answer with a chosen
//! `state` token.
//!
//! Gated on `test-hooks` **alone** (the `outbound_clock_test_hook` shape), so it
//! is present in the standard e2e nest build. The wire `state` is an open string
//! enum — an app that meets a token it does not know renders the generic "needs
//! attention" label, so a newer nest never breaks an older app
//! (`docs/goal/behavior/mail-deliverability.md` § The mail health readout on
//! `admin-mail`). No real nest can send such a token to a build of the same
//! version, so the journey that witnesses that promise stands one in with this
//! hook.
//!
//! Consumer: `tests/e2e-unified/tests/test_admin_mail_health.py::
//! test_a_state_newer_than_the_app_reads_needs_attention`. Production never
//! compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::routes::AppState;

#[derive(Deserialize)]
struct StateBody {
    /// The token to answer with; absent or `null` restores the fold's own answer.
    #[serde(default)]
    state: Option<String>,
}

/// `POST /api/v1/test/mail_health/state` — set (or clear) the override of the
/// reply's `state`. Returns `{"state": <token or null>}`.
async fn handle_state(
    State(app): State<Arc<AppState>>,
    Json(body): Json<StateBody>,
) -> impl IntoResponse {
    *app.mail_health_state_override
        .lock()
        .expect("mail health state override mutex") = body.state.clone();
    Json(json!({ "state": body.state }))
}

/// Mount the `/api/v1/test/mail_health/state` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/mail_health/state", post(handle_state))
}
