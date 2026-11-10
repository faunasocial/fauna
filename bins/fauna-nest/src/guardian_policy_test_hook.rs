//! Test-only HTTP endpoints that put a ward's guardian policy in two states the
//! wire cannot reach: a knob value this build cannot name, and a transfer
//! proposal past its 7-day lapse.
//!
//! Gated on `test-hooks` **alone** (the `guardian_content_document_test_hook`
//! shape), so it rides the standard `--features test-hooks` e2e build and never
//! compiles into production.
//!
//! Why it exists:
//!
//! - **An unrecognized knob value.** Within a major version a client may be
//!   older than its nest, so a newer nest may store a knob value the client
//!   cannot name; the app must render it as the strictest option, so a save
//!   never weakens the policy (`family-safety.md` § Implementation status
//!   today, the fail-closed knob rule). The only write path,
//!   `fauna.family.policy.update`, validates every knob, so the state is
//!   unreachable from a tier_3 through the wire. `POST
//!   /api/v1/test/family/policy_knob` stores a raw string into one of the
//!   closed-enum knob columns ([`crate::db::family::TEST_SETTABLE_POLICY_KNOBS`])
//!   and does nothing else.
//! - **A lapsed transfer.** A pending hand-over lapses after 7 days
//!   (`family-safety.md` § Graduation & transfer → *Expiry*), lazily against
//!   the wall clock. `POST /api/v1/test/family/transfer_age` moves the ward's
//!   pending proposal back by N seconds; every read then applies the
//!   production cutoff unchanged.
//!
//! What a test observes afterwards is the production `fauna.family.status`
//! read and the apps' render, unchanged.
//!
//! Consumers: `tests/e2e-unified/tests/test_family_policy_fail_closed.py`
//! (the knob) and `tests/e2e-unified/tests/test_family_transfer_lapse.py` (the
//! lapse).

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::db::family::TEST_SETTABLE_POLICY_KNOBS;
use crate::routes::AppState;

#[derive(Deserialize)]
struct KnobRequest {
    /// The supervised account, 64 hex chars.
    actor_id: String,
    /// The knob column: one of `TEST_SETTABLE_POLICY_KNOBS`.
    knob: String,
    /// The raw value to store, unvalidated.
    value: String,
}

#[derive(Deserialize)]
struct TransferAgeRequest {
    /// The supervised account whose pending proposal to age, 64 hex chars.
    actor_id: String,
    /// How far into the past to move the proposal's `created_at`.
    seconds: i64,
}

/// `POST /api/v1/test/family/policy_knob` — store a raw string into one knob
/// of a ward's policy row. Returns `{"written": <n>}`: 1, or 0 when the ward
/// has no policy row.
async fn handle_knob(
    State(state): State<Arc<AppState>>,
    Json(req): Json<KnobRequest>,
) -> impl IntoResponse {
    let Ok(actor_id) = fauna_core::hex32::decode(&req.actor_id) else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    if !TEST_SETTABLE_POLICY_KNOBS.contains(&req.knob.as_str()) {
        return ApiError::bad_request(format!(
            "knob must be one of {TEST_SETTABLE_POLICY_KNOBS:?}"
        ))
        .into_response();
    }
    match state
        .db
        .test_set_guardian_policy_knob(&actor_id, &req.knob, &req.value)
        .await
    {
        Ok(written) => Json(json!({ "written": written })).into_response(),
        Err(e) => ApiError::internal(format!("set policy knob: {e:#}")).into_response(),
    }
}

/// `POST /api/v1/test/family/transfer_age` — move a ward's pending transfer
/// proposal `seconds` into the past. Returns `{"written": <n>}`: 1, or 0 when
/// nothing is pending.
async fn handle_transfer_age(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TransferAgeRequest>,
) -> impl IntoResponse {
    let Ok(actor_id) = fauna_core::hex32::decode(&req.actor_id) else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    if req.seconds < 0 {
        return ApiError::bad_request("seconds must not be negative").into_response();
    }
    match state
        .db
        .test_age_pending_transfer(&actor_id, req.seconds)
        .await
    {
        Ok(written) => Json(json!({ "written": written })).into_response(),
        Err(e) => ApiError::internal(format!("age pending transfer: {e:#}")).into_response(),
    }
}

/// Mount the two `/api/v1/test/family/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/family/policy_knob", post(handle_knob))
        .route(
            "/api/v1/test/family/transfer_age",
            post(handle_transfer_age),
        )
}
