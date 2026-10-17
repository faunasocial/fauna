//! Test-only HTTP endpoints for the mail-portability witnesses: set the
//! export policy, and fast-forward one actor's export / import sessions past
//! their thirty days.
//!
//! Gated on `test-hooks` **alone** (the `spam_history_gc_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the per-export size ceiling and the per-person footprint
//! (`mail-export.md` § Quota composition) are read from a policy row whose only
//! product writer is the admin surface `mail-policy-config.md` gates on ui.yaml
//! ratification, so at the shipped 10 GiB / 30 GiB defaults they are
//! unreachable from outside; and the thirty-day expiry of an export (§ Expiry)
//! and of an untouched import (`mailbox-migration.md`) cannot be waited out.
//! `age` moves ONE actor's rows back and, for exports, runs the SAME
//! [`crate::mail_export_blobs::run_export_expiry_tick`] the hourly loop runs;
//! an import's forgetting is the lazy GC every per-actor import entry point
//! already performs, so the test observes it by calling the production RPC.
//!
//! Consumers: `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py`.
//! Policy is nest-wide: use a DEDICATED nest.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::db::mail_policy::ExportPolicyOverrides;
use crate::routes::AppState;

/// `POST /api/v1/test/mail_portability/export_policy` — replace the export
/// policy row. Body `{"max_blob_bytes": n|null, "max_concurrent_per_user": n|null}`.
async fn handle_policy(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportPolicyOverrides>,
) -> impl IntoResponse {
    match state.db.put_export_policy(body).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => ApiError::internal(format!("put export policy: {e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct AgeRequest {
    /// Hex of the 32-byte actor id.
    actor_id: String,
    #[serde(default)]
    exports: bool,
    #[serde(default)]
    imports: bool,
}

/// `POST /api/v1/test/mail_portability/age` — age the actor's sessions, then
/// (exports) run the production expiry tick once. Returns
/// `{"aged_exports": n, "aged_imports": n}`.
async fn handle_age(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgeRequest>,
) -> impl IntoResponse {
    let Some(actor) = hex::decode(req.actor_id.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    else {
        return ApiError::bad_request("actor_id must be 64 hex chars").into_response();
    };
    let (e, i) = match state
        .db
        .test_age_mail_portability_sessions(&actor, req.exports, req.imports)
        .await
    {
        Ok(n) => n,
        Err(e) => return ApiError::internal(format!("age sessions: {e:#}")).into_response(),
    };
    if req.exports {
        crate::mail_export_blobs::run_export_expiry_tick(&state.db, &state.config.nest.db_path)
            .await;
    }
    Json(json!({ "aged_exports": e, "aged_imports": i })).into_response()
}

/// Mount the `/api/v1/test/mail_portability/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/mail_portability/export_policy",
            post(handle_policy),
        )
        .route("/api/v1/test/mail_portability/age", post(handle_age))
}
