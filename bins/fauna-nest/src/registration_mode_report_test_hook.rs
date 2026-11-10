//! Test-only HTTP endpoints rewriting the `registration_mode` that
//! `fauna.setup.status` REPORTS — an unknown wire string, or the field absent —
//! while the nest keeps running its real posture underneath.
//!
//! # Why a hook
//!
//! The ruling under test is the client half of `public-mode.md` § Registration
//! Modes (*the posture read-back is the raw wire string on purpose*): an app that
//! cannot name the reported mode — a newer nest's mode, or none — renders the
//! posture section read-only and never saves a guess over it
//! (`version-compatibility.md` § Dimension 3). No honest path reaches that reply
//! on a test nest: the heartbeat reports `Some(mode.as_wire_str())` from the typed
//! [`fauna_protocol::node_policy::RegistrationMode`], and an unknown stored value
//! fails shut to `closed` before it ever reaches the wire. So this seam is the
//! newer-nest reply, cut at the one field the app reads
//! ([`crate::discovery_core`], the `setup.status` builder) and nowhere else: the
//! live posture cell, the stored policy row and registration itself are
//! untouched, so a test reads the real posture back unchanged once it clears.
//!
//! Gated on `test-hooks` alone, so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build.
//!
//! Consumer: `tests/e2e-unified/tests/test_admin_registration_posture.py`
//! (`test_an_unnamed_registration_mode_is_shown_read_only_and_never_saved_over`).
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::routes::AppState;

/// What `fauna.setup.status` reports in place of the live posture while the
/// override is armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedRegistrationMode {
    /// The `registration_mode` field is left out of the reply.
    Absent,
    /// The field carries this wire string verbatim (unknown to the client, say).
    Wire(String),
}

/// The reply field under an armed override: `None` = no override (report the
/// live posture), else the override's field value.
pub fn reported(over: Option<&ReportedRegistrationMode>, live: Option<String>) -> Option<String> {
    match over {
        None => live,
        Some(ReportedRegistrationMode::Absent) => None,
        Some(ReportedRegistrationMode::Wire(s)) => Some(s.clone()),
    }
}

/// Body of the arm call: `{"mode": "<wire>"}`, or `{"absent": true}`.
#[derive(Debug, Deserialize)]
struct ArmBody {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    absent: bool,
}

/// `POST /api/v1/test/registration-mode/report` — report `mode` (or no field,
/// with `absent`) from `fauna.setup.status` until cleared. Returns
/// `{"ok": true}`; a body naming neither (or both) is a 400.
async fn arm(State(state): State<Arc<AppState>>, Json(body): Json<ArmBody>) -> impl IntoResponse {
    let over = match (body.mode, body.absent) {
        (Some(m), false) => ReportedRegistrationMode::Wire(m),
        (None, true) => ReportedRegistrationMode::Absent,
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "name exactly one of `mode` or `absent`" })),
            )
                .into_response();
        }
    };
    *state
        .registration_mode_report_override
        .lock()
        .expect("registration_mode_report_override mutex poisoned") = Some(over);
    Json(json!({ "ok": true })).into_response()
}

/// `POST /api/v1/test/registration-mode/report/clear` — report the live posture
/// again. Returns `{"ok": true}`.
async fn clear(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    *state
        .registration_mode_report_override
        .lock()
        .expect("registration_mode_report_override mutex poisoned") = None;
    Json(json!({ "ok": true })).into_response()
}

/// Mount the two `/api/v1/test/registration-mode/report` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/registration-mode/report", post(arm))
        .route("/api/v1/test/registration-mode/report/clear", post(clear))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_override_reports_the_live_posture() {
        assert_eq!(reported(None, Some("open".into())), Some("open".into()));
    }

    #[test]
    fn an_armed_override_replaces_the_field_or_drops_it() {
        let wire = ReportedRegistrationMode::Wire("by_referral".into());
        assert_eq!(
            reported(Some(&wire), Some("open".into())),
            Some("by_referral".into())
        );
        assert_eq!(
            reported(Some(&ReportedRegistrationMode::Absent), Some("open".into())),
            None
        );
    }
}
