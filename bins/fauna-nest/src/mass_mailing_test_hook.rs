//! Test-only HTTP endpoint to set the mass-mailing policy overrides.
//!
//! Gated on `test-hooks` **alone**, so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build that
//! `build_node()` produces. It lets a tier_3 test lower the deployment-wide
//! list-recipient ceilings (`docs/goal/behavior/mail-mass-mailing.md`
//! § Per-list rate accounting) to small numbers so the per-day-cap tempfail
//! (`452 4.7.0`, `fauna.bridges.list_daily_cap_exceeded`) is reachable without
//! sending 50 000 messages. The admin write RPC + UI for this policy land with
//! the flat `admin-mail` page (deferred); until then this hook is the only way
//! to exercise the per-day branch of [`crate::db::CacheDb::try_consume_list_quota`]
//! end-to-end through the `send_list_message` handler.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_mail_lists_send.py::
//! test_list_per_day_cap_tempfails`. Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::db::mail_policy::MassMailingPolicyOverrides;
use crate::routes::AppState;

/// `POST /api/v1/test/mass-mailing/policy` — overwrite the
/// `mail_mass_mailing_policy` overrides row. The JSON body deserializes into
/// [`MassMailingPolicyOverrides`] (every field optional, `#[serde(default)]`),
/// so a partial body sets only the named ceilings and leaves the rest at the
/// catalog default. Returns `{"ok": true}`.
async fn handle_set_policy(
    State(state): State<Arc<AppState>>,
    Json(overrides): Json<MassMailingPolicyOverrides>,
) -> impl IntoResponse {
    match state.db.put_mass_mailing_policy(overrides).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => ApiError::internal(format!("put_mass_mailing_policy: {e}")).into_response(),
    }
}

/// Mount the `/api/v1/test/mass-mailing/policy` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/mass-mailing/policy", post(handle_set_policy))
}
