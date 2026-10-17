//! Test-only HTTP endpoint for driving the Nostr sync worker's inbound
//! follow sweep on demand.
//!
//! Gated on the `test-hooks` Cargo feature so this module — and the route it
//! registers — never compile into the production binary (`testing.md`
//! point 15: the automation surface is compiled out of release artifacts).
//!
//! **Why this exists.** [`super::sync_worker::NostrSyncWorker`] refreshes its
//! subscriptions to followed authors on a 60 s tick, with no wake for a new
//! follow. A journey asserting that a followed author's note reached the feed
//! would otherwise sleep on that cadence (`testing.md` convention 14). This
//! poke nudges one sweep — the same `REQ` per relay the tick sends — and
//! returns at once; the worker drains what the relays answer, and the test
//! polls for the state it expects, re-poking as it goes (a note the relay
//! answers after the drain's short window is collected on the next wake).
//! The `bluesky/feed/poll-now` route is the precedent.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::routes::AppState;

/// `POST /api/v1/test/nostr/inbound/poll-now` — nudge one inbound follow
/// sweep. Returns `{"ok": true}`; a wake already pending coalesces with this
/// one (the sweep reads current state, not the nudge).
async fn handle_poll_now(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let _ = state.nostr.inbound_wake_tx.try_send(());
    Json(json!({ "ok": true })).into_response()
}

/// Mount the `/api/v1/test/nostr/inbound/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/nostr/inbound/poll-now", post(handle_poll_now))
}
