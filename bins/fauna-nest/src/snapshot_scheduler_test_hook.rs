//! Test-only HTTP endpoint that runs exactly one snapshot-scheduler pass —
//! the automatic capture `backup-restore.md` § Automatic Snapshots promises.
//!
//! Gated on `test-hooks` **alone** (the `version_prune_test_hook` shape), so it
//! rides the standard `--features test-hooks` e2e build and never compiles into
//! production.
//!
//! Why it exists: the scheduler ticks every
//! [`crate::backup::scheduler::CHECK_INTERVAL`] (60 s), so a tier_3 asserting
//! that a changed-then-quiet folder gets a snapshot **by itself** would have to
//! wait out a wall clock — exactly the brittleness `e2e-conventions.md`
//! convention 14 forbids. This hook is the same "run the real production path
//! on demand" seam as its siblings: it calls the SAME
//! [`SnapshotScheduler::check_all_folders`] the background tick calls, built on
//! the SAME [`CHECK_INTERVAL`]/[`DEFAULT_QUIET_SECS`] `main.rs` spawns the
//! production scheduler with — so a folder's own
//! `nest_place.quiet_secs`/`nest_place.snapshots` choice decides the outcome
//! here exactly as it does in production, and what a test observes afterwards
//! is the production capture, not a shortcut around it.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_snapshot_auto_capture.py`.
//! Production passes happen on the real interval loop spawned from `main.rs`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::backup::scheduler::{CHECK_INTERVAL, DEFAULT_QUIET_SECS, SnapshotScheduler};
use crate::routes::AppState;

/// `POST /api/v1/test/snapshot_scheduler/run-now` — run exactly one
/// `check_all_folders` pass and return how many automatic snapshots it cut.
///
/// Returns `{"ok": true, "created": N}`. `created == 0` is a meaningful
/// negative signal for a test (no folder owed a snapshot), not an error.
///
/// A nest with no blob dir configured has no [`BackupService`] and therefore no
/// scheduler in production either; that is reported as a 500 rather than a
/// silent `created: 0`, so a test cannot read "this nest never snapshots
/// anything" as "this folder was not due".
///
/// [`BackupService`]: crate::backup::service::BackupService
async fn handle_run_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(svc) = state.backup_service.clone() else {
        return ApiError::internal(
            "snapshot scheduler run-now: this nest has no backup service (no blob dir), \
             so it runs no snapshot scheduler at all",
        )
        .into_response();
    };
    let scheduler = SnapshotScheduler::new(svc, CHECK_INTERVAL, DEFAULT_QUIET_SECS);
    match scheduler.check_all_folders().await {
        Ok(created) => Json(json!({ "ok": true, "created": created })).into_response(),
        Err(e) => ApiError::internal(format!("snapshot scheduler run-now: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/snapshot_scheduler/run-now` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/snapshot_scheduler/run-now",
        post(handle_run_now),
    )
}
