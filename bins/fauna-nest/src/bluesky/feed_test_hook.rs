//! Test-only HTTP endpoint for driving the consume-side Bluesky feed poller on
//! demand.
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (`testing.md`
//! point 15: the automation surface is compiled out of release artifacts).
//!
//! **Why this exists.** [`super::feed_worker::BlueskyFeedWorker`] ticks every
//! five minutes, its first tick at boot — before any e2e journey has linked an
//! account. A test asserting that a followed account's post reached the feed
//! would otherwise sleep on a wall clock (`testing.md` convention 14). This
//! poke runs one pass synchronously; the test then asserts state, never timing.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::bluesky::{db_helpers, feed_worker};
use crate::routes::AppState;

/// `POST /api/v1/test/bluesky/feed/poll-now` — run exactly one poll over every
/// consume-side linked account, the worker's own pass.
///
/// Returns `{"ok": true, "actors": N, "stored": N, "errors": ["<actor>: <why>", …]}`.
/// One account's failure is reported, never fatal — the worker's own rule.
async fn handle_poll_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let actors = {
        let conn = state.db.conn().await;
        match db_helpers::list_consume_side_linked_actors(&conn) {
            Ok(actors) => actors,
            Err(e) => {
                return ApiError::internal(format!("bluesky feed poll-now: {e:#}")).into_response();
            }
        }
    };
    let mut stored = 0u64;
    let mut errors = Vec::new();
    for actor_hex in &actors {
        match feed_worker::poll_bluesky_feeds(&state, actor_hex).await {
            Ok(n) => stored += n,
            Err(e) => errors.push(format!("{actor_hex}: {e:#}")),
        }
    }
    Json(json!({
        "ok": true,
        "actors": actors.len(),
        "stored": stored,
        "errors": errors,
    }))
    .into_response()
}

/// Mount the `/api/v1/test/bluesky/feed/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/bluesky/feed/poll-now", post(handle_poll_now))
}
