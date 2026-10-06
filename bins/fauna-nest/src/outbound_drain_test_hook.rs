//! Test-only HTTP endpoints that make one outbound drain cycle deterministic:
//! a `run_now` **poke** plus the queue **snapshot** that is its positive
//! completion observable.
//!
//! Gated on `test-hooks` **alone**, so both are present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build that
//! `build_node()` produces. Production never compiles this module.
//!
//! # Why the poke rides the production nudge
//!
//! The Go MTA's outbound worker drains on a `PollInterval` (30 s default) and
//! is short-circuited by a coalescing `Trigger()`, which nest already reaches
//! in production via the `fauna.bridges.outbound_ready` push
//! ([`crate::bridge_routing_handlers::notify_bridges_outbound_ready`] →
//! `wsrpc.OutboundReadyHandler` → `OutboundWorker.Trigger`). This hook calls
//! that **same production helper** rather than opening a test-only listener
//! inside the bridge: no new wire kind, no new bridge surface to compile-gate
//! (`e2e-conventions.md` § convention 15), and the poke exercises the real
//! nudge path instead of bypassing it.
//!
//! Nest emits that nudge in production only from the interactive
//! `fauna.email.send` path — the *background* enqueue paths (DSNs, bounces,
//! forwards, TLSRPT) deliberately rely on the poll backstop, and a row that
//! becomes due purely because **time passed** (the retry curve) has no enqueue
//! event to nudge from at all. That second case is exactly what a test
//! fast-forwarding [`crate::routes::AppState::outbound_now`] creates, and why
//! reaching the next attempt otherwise costs a full `PollInterval` of
//! wall-clock sleep — the brittleness `e2e-conventions.md` § convention 14
//! exists to remove.
//!
//! # The observable
//!
//! `GET /api/v1/test/outbound/queue` projects the production
//! `outbound_mail_queue` rows. `attempt_count` is the causal barrier a
//! negative assert anchors to: `mark_outbound_failed_handler` runs the
//! delay-warning enqueue **before** `mark_outbound_attempt` bumps the count
//! (`bridge_routing_handlers.rs`, the `FailedDecision::Retry` arm), so once a
//! row's `attempt_count` has advanced, that attempt's warn decision has
//! already run to completion — "no second warning was emitted" becomes a
//! statement about state rather than about elapsed time.
//!
//! Consumer: `tests/e2e-unified/tests/test_mail_bridge_mta.py::
//! test_outbound_delay_warning_emitted_once`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// `POST /api/v1/test/outbound/poke_bridge` — emit the production
/// `fauna.bridges.outbound_ready` nudge to every approved MTA-role bridge, so
/// the outbound worker re-polls now instead of on its next `PollInterval`.
///
/// Returns `{"nudged": <n>}`, the number of approved MTA bridges the push was
/// addressed to. `0` means no MTA bridge is enrolled — the poke cannot have
/// done anything, and a caller that treats the poke as a barrier should fail
/// loudly rather than fall back to waiting out the poll. Delivery itself stays
/// best-effort (a disconnected MTA's push is dropped, exactly as in
/// production); the `fetch_outbound_due` poll remains the correctness backstop.
async fn handle_poke_bridge(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let nudged = crate::bridge_routing_handlers::notify_bridges_outbound_ready(&state).await;
    Json(json!({ "nudged": nudged })).into_response()
}

/// `GET /api/v1/test/outbound/queue` — project every `outbound_mail_queue` row
/// (any status), ordered by id.
///
/// Deliberately omits `raw_message`: the queue holds user mail, the projection
/// is for assertions about scheduling state, and message bodies have no place
/// in a test-endpoint response. Everything a caller needs to identify a row
/// (recipient, sender, message-id) and to reason about the retry curve
/// (`attempt_count`, `next_attempt_at`, `delay_warned_at`, `status`) is here.
async fn handle_queue(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.db.fetch_all_outbound_for_test().await {
        Ok(rows) => {
            let rows: Vec<_> = rows
                .into_iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "original_msgid": r.original_msgid,
                        "original_sender": r.original_sender,
                        "recipient": r.recipient,
                        "attempt_count": r.attempt_count,
                        "next_attempt_at": r.next_attempt_at,
                        "delay_warned_at": r.delay_warned_at,
                        "status": r.status.as_str(),
                        "last_error": r.last_error,
                        "created_at": r.created_at,
                    })
                })
                .collect();
            Json(json!({ "rows": rows })).into_response()
        }
        Err(e) => ApiError::internal(format!("fetch_all_outbound_for_test: {e}")).into_response(),
    }
}

/// Mount the outbound drain poke + queue-snapshot routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/outbound/poke_bridge",
            post(handle_poke_bridge),
        )
        .route("/api/v1/test/outbound/queue", get(handle_queue))
}
