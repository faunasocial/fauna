//! Test-only HTTP endpoint for driving the consume-side Bluesky DM worker on
//! demand — the DM twin of [`super::feed_test_hook`].
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (`testing.md`
//! point 15: the automation surface is compiled out of release artifacts).
//!
//! **Why this exists.** [`super::dm_worker::BlueskyDmWorker`] ticks every
//! [`super::dm_worker::DM_POLL_INTERVAL_SECS`] seconds. A journey asserting that
//! a far DM became a bridged room would otherwise wait on that clock
//! (`testing.md` convention 14). This poke runs one pass synchronously; the
//! test then asserts state, never timing.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::bluesky::{db_helpers, dm_leg, dm_worker};
use crate::routes::AppState;

/// `POST /api/v1/test/bluesky/dm/poll-now` — run exactly one of the worker's
/// passes: every consume-side linked account's conversations in, then the
/// leg's outbox out.
///
/// The pass starts from no remembered heads, so it reads every conversation's
/// head page; the deposit is idempotent on the message id, so a page the
/// worker already ingested stores nothing twice.
///
/// Returns `{"ok": true, "actors": N, "stored": N, "sent": N, "errors": ["<actor>: <why>", …]}`.
/// One account's failure is reported, never fatal — the worker's own rule.
async fn handle_poll_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let actors = {
        let conn = state.db.conn().await;
        match db_helpers::list_consume_side_linked_actors(&conn) {
            Ok(actors) => actors,
            Err(e) => {
                return ApiError::internal(format!("bluesky dm poll-now: {e:#}")).into_response();
            }
        }
    };
    let mut heads = dm_worker::Heads::new();
    let mut stored = 0u64;
    let mut errors = Vec::new();
    for actor_hex in &actors {
        match dm_worker::poll_bluesky_dms(&state, actor_hex, &mut heads).await {
            Ok(n) => stored += n,
            Err(e) => errors.push(format!("{actor_hex}: {e:#}")),
        }
    }
    let far = dm_leg::SessionChat { state: &state };
    let sent = match dm_leg::drain_outbox(&state, &far).await {
        Ok(sent) => sent.len(),
        Err(e) => {
            errors.push(format!("outbox drain: {e:#}"));
            0
        }
    };
    Json(json!({
        "ok": true,
        "actors": actors.len(),
        "stored": stored,
        "sent": sent,
        "errors": errors,
    }))
    .into_response()
}

/// Mount the `/api/v1/test/bluesky/dm/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/bluesky/dm/poll-now", post(handle_poll_now))
}
