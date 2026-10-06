//! Test-only HTTP endpoint for driving the nest's segment-backup sweep on demand.
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (e2e conventions,
//! `testing.md` point 15: the automation surface is compiled out of release
//! artifacts).
//!
//! **Why this exists.** [`NestBackupWorker`] ticks every
//! [`fauna_sync_engine::segment_backup::PERIODIC_INTERVAL`] — 15 minutes — with
//! the first tick at boot. A destination enrolled *after* boot therefore gets no
//! upload for up to a quarter hour, so any test asserting that
//! `backup-destination-last-upload-time` reflects a **nest-side** upload would
//! otherwise have to sleep on a wall clock. That is exactly the brittleness
//! `testing.md` convention 14 forbids: this poke is the sanctioned alternative —
//! the test runs one sweep synchronously and then asserts *state*, never timing.
//!
//! Consumer: `tests/e2e-unified/tests/test_backups.py::test_nest_side_upload_advances_last_upload_time`.
//! Production sweeps happen on the real interval loop spawned from `main.rs`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;
use crate::segment_backup::NestBackupWorker;

/// `POST /api/v1/test/backup/run-now` — run exactly one segment-backup sweep
/// over every enrolled owner and return how many owners were processed.
///
/// Returns `{"ok": true, "owners_run": N}`. `owners_run` is `0` when no owner
/// has both a granted `NestBackupKey` and a registered destination — a
/// meaningful negative signal for a test, not an error.
///
/// The sweep runs on the blocking pool driven by this runtime's handle, exactly
/// as [`NestBackupWorker::spawn`] does, and for the same two reasons: a pass is
/// blocking-shaped (SQLite reads, segment reads, the seal of every uploaded
/// segment), and it is `!Send` by construction (`SyncEngine`/`SyncDb` hold a
/// `rusqlite::Connection`), so it cannot be awaited directly in a `Send` axum
/// handler future.
async fn handle_run_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sweep = tokio::task::spawn_blocking(move || {
        let worker = NestBackupWorker::new(state, std::time::Duration::MAX);
        tokio::runtime::Handle::current().block_on(worker.run_once())
    })
    .await;

    match sweep {
        Ok(Ok(owners_run)) => Json(json!({ "ok": true, "owners_run": owners_run })).into_response(),
        Ok(Err(e)) => ApiError::internal(format!("backup run_now: {e:#}")).into_response(),
        Err(e) => ApiError::internal(format!("backup run_now join: {e}")).into_response(),
    }
}

/// `POST /api/v1/test/backup/reclaim-generations` — run the destination's
/// retained-generation reclaim once, as if `now_offset_secs` had passed.
///
/// Body `{"now_offset_secs": N}` (absent → 0). Returns
/// `{"ok": true, "reclaimed": N}` — the rows the reclaim dropped.
///
/// **Only the clock is fake.** The grace window `T` is a 30-day Rust constant
/// ([`crate::backup::gc::BACKUP_CUSTODY_GRACE_SECS`]) and the production reclaim
/// runs inside the GC sweep, so a journey that must witness "a version past its
/// recovery window says so" (`nests.md` § Trust facet — generation recovery)
/// would otherwise have to wait out a month (convention 14 — a fake clock,
/// never a sleep). The cutoff is computed exactly as the GC's step 0 computes
/// it, moved by the offset, and the reclaim is the production statement pair —
/// quota credit included — so nothing about what gets reclaimed is injectable.
/// The chunk sweep that follows step 0 in production is deliberately not run:
/// the generation row going away is the whole observable a restore reads.
async fn handle_reclaim_generations(
    State(state): State<Arc<AppState>>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let offset = body
        .as_ref()
        .and_then(|Json(v)| v.get("now_offset_secs"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cutoff = crate::db::now_epoch_secs().saturating_add(offset)
        - crate::backup::gc::BACKUP_CUSTODY_GRACE_SECS;
    match state
        .db
        .reclaim_expired_backup_custody_generations(cutoff)
        .await
    {
        Ok(reclaimed) => Json(json!({ "ok": true, "reclaimed": reclaimed })).into_response(),
        Err(e) => ApiError::internal(format!("backup reclaim-generations: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/backup/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/backup/run-now", post(handle_run_now))
        .route(
            "/api/v1/test/backup/reclaim-generations",
            post(handle_reclaim_generations),
        )
}
