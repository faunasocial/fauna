//! Test-only HTTP endpoint that runs the version-retention evaluation sweep
//! immediately — the leg of `file-versions.md` § Retention (3)'s pipeline the
//! GC cycle triggers in production.
//!
//! Gated on `test-hooks` **alone** (the `pending_actions_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the evaluator runs on the GC scheduler's 6-hour interval
//! (`GcScheduler` in `main.rs`), so a tier_3 could never observe a scheduled
//! `VersionBulkPrune` without waiting out real hours. This hook is the same
//! "fast-forward the clock, then run the real production path" seam as its
//! siblings: it calls the SAME per-folder
//! [`crate::backup::version_prune::schedule_version_auto_prune`] the GC cycle
//! calls (`gc_scheduler.rs::prune_all_folders`), with the same
//! one-set's-failure-never-stops-the-sweep isolation — so what a test observes
//! afterwards is the production evaluation, not a shortcut around it. The
//! *execution* leg needs no hook of its own: a `VersionBulkPrune` is an
//! ordinary pending action, which the existing
//! `POST /api/v1/test/pending_actions/run_due` makes due and executes.
//!
//! Consumer: `tests/e2e-unified/tests/test_version_prune_recovery.py` (the
//! recovery browse needs a really soft-pruned version to recover).

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// `POST /api/v1/test/version_prune/evaluate` — run the version-retention
/// evaluation over every folder, exactly as the GC cycle does. Returns
/// `{"scheduled": <n>}`, the number of `VersionBulkPrune` actions created.
async fn handle_evaluate(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let folders = match state.db.list_folders().await {
        Ok(f) => f,
        Err(e) => {
            return ApiError::internal(format!("list folders: {e:#}")).into_response();
        }
    };
    let mut scheduled = 0usize;
    for fs in &folders {
        match crate::backup::version_prune::schedule_version_auto_prune(&state.db, fs).await {
            Ok(n) => scheduled += n,
            // The production sweep's isolation rule: skip the set, keep going.
            Err(e) => {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&fs.name),
                    error = %e,
                    "version-prune test hook: scheduling failed for one set"
                );
            }
        }
    }
    Json(json!({ "scheduled": scheduled })).into_response()
}

/// Mount the `/api/v1/test/version_prune/evaluate` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/version_prune/evaluate", post(handle_evaluate))
}
