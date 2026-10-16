//! Test-only HTTP endpoint that runs ONE tick of the spam-baseline
//! standing-publish cadence, at a time the test names.
//!
//! Gated on `test-hooks` **alone** (the `spam_history_gc_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: with standing publish on, the nest republishes the shared
//! baseline by itself once every `BASELINE_REPUBLISH_INTERVAL` (24 h;
//! `mail-spam.md` § Cold start → *Standing publish*), so its *effect* is
//! unreachable from a tier_3 without waiting out a day. The hook calls the SAME
//! [`crate::spam_baseline::run_standing_publish_if_due`] the cadence task calls
//! each hour, passing the test's `now_ms` instead of the clock — the shape the
//! nest's own tests use (convention 14) — so a test can tick "an hour later"
//! (not due: nothing runs) and "a day later" (due: an ordinary publish under
//! both floors) without a shortened timer.
//!
//! Consumer: `tests/e2e-unified/tests/test_spam_baseline_standing.py`.

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
struct TickBody {
    /// The epoch millisecond the tick runs at; absent → the clock, as the
    /// cadence task itself passes.
    #[serde(default)]
    now_ms: Option<i64>,
}

/// `POST /api/v1/test/spam/baseline/standing-tick` — one cadence tick.
/// Returns `{"ran": false}` when standing publish is off or not yet due, else
/// `{"ran": true, "published", "deferred", "contributors", "skipped_contributors"}`
/// — the run's own aggregates; the test reads the effect from the admin's
/// `get_spam_baseline_state`.
async fn handle_tick(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TickBody>,
) -> impl IntoResponse {
    let now_ms = body.now_ms.unwrap_or_else(crate::db::now_epoch_millis);
    match crate::spam_baseline::run_standing_publish_if_due(&state, now_ms).await {
        Ok(None) => Json(json!({ "ran": false })).into_response(),
        Ok(Some(o)) => Json(json!({
            "ran": true,
            "published": o.published,
            "deferred": o.deferred,
            "contributors": o.contributors,
            "skipped_contributors": o.skipped_contributors,
        }))
        .into_response(),
        Err(e) => ApiError::internal(format!("run_standing_publish_if_due: {e}")).into_response(),
    }
}

/// Mount the `/api/v1/test/spam/baseline/standing-tick` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/spam/baseline/standing-tick",
        post(handle_tick),
    )
}
