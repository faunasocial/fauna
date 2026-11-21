//! Test-only HTTP endpoint: script the outcome of the deliverability
//! diagnostics' outbound-STARTTLS probe to a known receiver's port 25.
//!
//! Gated on `test-hooks` **alone** (the `mail_health_state_test_hook` shape), so
//! it is present in the standard e2e nest build, whose prober is the
//! `ScriptedStarttlsProber` reading the cell this route sets. A fresh cloud box
//! whose provider blocks outbound port 25 is the case it stands in: the probe's
//! connection never opens, the diagnostics row fails, and the mail health
//! readout reads `outbound_blocked` (`docs/goal/behavior/mail-deliverability.md`
//! § Symptom diagnostics, § The mail health readout on `admin-mail`). The
//! network itself is never touched.
//!
//! Consumer: `tests/e2e-unified/tests/test_admin_mail_health.py::
//! test_a_blocked_outbound_port_reads_outgoing_mail_cannot_leave_naming_why`.
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::mail_deliverability::StarttlsProbeOutcome;
use crate::routes::AppState;

#[derive(Deserialize)]
struct ProbeBody {
    /// `"ok"`, `"unreachable"` or `"failed"`; absent or `null` restores the
    /// unscripted answer (the Null prober's "not available in this build").
    #[serde(default)]
    outcome: Option<String>,
    /// The reason the scripted outcome carries; a fixed one when absent.
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/v1/test/mail/starttls-probe` — set (or clear) the scripted probe
/// outcome. Returns `{"outcome": <token or null>}`; an unknown token is a 400.
async fn handle_probe(
    State(app): State<Arc<AppState>>,
    Json(body): Json<ProbeBody>,
) -> impl IntoResponse {
    let reason = body
        .reason
        .clone()
        .unwrap_or_else(|| "scripted by the test hook".into());
    let scripted = match body.outcome.as_deref() {
        None => None,
        Some("ok") => Some(StarttlsProbeOutcome::Ok),
        Some("unreachable") => Some(StarttlsProbeOutcome::Unreachable(reason)),
        Some("failed") => Some(StarttlsProbeOutcome::Failed(reason)),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("unknown outcome {other:?}") })),
            );
        }
    };
    *app.starttls_probe_script
        .lock()
        .expect("starttls probe script mutex") = scripted;
    (StatusCode::OK, Json(json!({ "outcome": body.outcome })))
}

/// Mount the `/api/v1/test/mail/starttls-probe` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/mail/starttls-probe", post(handle_probe))
}
