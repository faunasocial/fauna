//! Test-only HTTP endpoint that lands every due seed-alone RecoveryKey
//! replacement with an injected `now`.
//!
//! Gated on `test-hooks` **alone** (the `pending_actions_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: a seed-alone kit replacement ("I lost my kit") parks for
//! `RECOVERY_REPLACE_GRACE_SECS` (30 days) and lands only through
//! [`crate::recovery_handlers::land_due_replacements`], whose production caller
//! is the wall-clock periodic sweep in `main.rs`. That makes "the new kit takes
//! over after the waiting period" (`identity-succession.md` § The RecoveryKey →
//! *Replacement*) unreachable from a tier_3 without waiting out a month. This
//! hook is the same "fast-forward the clock, then run the real production path"
//! seam as the pending-actions hook, except that nothing is rewritten in the
//! database: `land_due_replacements` already takes its `now` as a parameter, so
//! the hook only supplies a later one and the landing path runs unchanged.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::recovery_handlers::land_due_replacements;
use crate::routes::AppState;
use fauna_core::recovery::RECOVERY_REPLACE_GRACE_SECS;

#[derive(Deserialize)]
struct LandDueBody {
    /// Seconds to add to the wall clock before landing. Absent → one second
    /// past the grace window, so every pending replacement is due.
    advance_secs: Option<i64>,
}

/// `POST /api/v1/test/recovery/land_due_replacements` — run the landing sweep
/// once at `now = wall_now + advance_secs` (default: the grace window + 1 s).
/// Optional body `{"advance_secs": <n>}`. Returns
/// `{"landed": <n>, "cancelled": <n>}`.
async fn handle_land_due(State(state): State<Arc<AppState>>, body: Bytes) -> impl IntoResponse {
    let advance = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<LandDueBody>(&body) {
            Ok(b) => b.advance_secs,
            Err(e) => return ApiError::bad_request(format!("body: {e}")).into_response(),
        }
    };
    let advance = advance.unwrap_or(RECOVERY_REPLACE_GRACE_SECS as i64 + 1);
    let now = fauna_core::data::Timestamp::now_secs_or_zero() + advance;
    match land_due_replacements(&state, now).await {
        Ok((landed, cancelled)) => {
            Json(json!({ "landed": landed, "cancelled": cancelled })).into_response()
        }
        Err(e) => ApiError::internal(format!("land_due_replacements: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/recovery/land_due_replacements` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/recovery/land_due_replacements",
        post(handle_land_due),
    )
}
