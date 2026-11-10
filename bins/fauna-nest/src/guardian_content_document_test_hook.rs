//! Test-only HTTP endpoint that stores raw bytes as a ward's guardian rule
//! document.
//!
//! Gated on `test-hooks` **alone** (the `share_link_age_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: a guardian's rule document this build cannot read — a newer
//! grammar version, or bytes that do not decode — must render fail-closed on
//! the ward's device, never read as absent (`nest-rules.md` § Implementation
//! status today; `family-client-enforcement.md` § Content policy). The only
//! write path, `fauna.family.policy.update`, refuses such a document (a nest
//! never stores rules no device on its build could apply), so the state is
//! unreachable from a tier_3 through the wire. The hook overwrites the ward's
//! stored `content_document` with the given bytes and does nothing else; what a
//! test then observes is the production `fauna.family.status` read and the
//! ward's render, unchanged. Scoped to one ward, whose policy row must already
//! exist (the test saves a real document first).
//!
//! Consumer: `tests/e2e-unified/tests/test_nest_rules.py`.

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
struct DocumentRequest {
    /// The supervised account, 64 hex chars.
    actor_id: String,
    /// The bytes to store as its guardian rule document, hex-encoded.
    document_hex: String,
}

/// `POST /api/v1/test/family/content_document` — store raw bytes as one
/// ward's guardian rule document. Returns `{"written": <n>}`: 1, or 0 when the
/// ward has no policy row.
async fn handle_set(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DocumentRequest>,
) -> impl IntoResponse {
    let Ok(actor_id) = fauna_core::hex32::decode(&req.actor_id) else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    let Ok(bytes) = hex::decode(&req.document_hex) else {
        return ApiError::bad_request("document_hex must be hex").into_response();
    };
    match state
        .db
        .test_set_guardian_content_document(&actor_id, &bytes)
        .await
    {
        Ok(written) => Json(json!({ "written": written })).into_response(),
        Err(e) => ApiError::internal(format!("set content document: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/family/content_document` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/family/content_document", post(handle_set))
}
