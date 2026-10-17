//! Test-only HTTP endpoint that runs the daily deliverability blocklist
//! self-check once, now.
//!
//! Gated on `test-hooks` **alone** (the `spam_history_gc_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the nest checks its own sending address against the DNSBLs
//! once a day unasked, at ~03:00 UTC plus a per-deployment offset
//! (`mail-deliverability.md` § The check; the timer is in `main.rs`), so the
//! *effect* — a history row nobody asked for, beside the nest's own
//! diagnostics run — is unreachable from a tier_3 without waiting for the
//! slot. The hook calls the SAME
//! [`crate::bridge_routing_handlers::run_scheduled_blocklist_self_check`] the
//! daily loop calls — never a shortened timer.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_mail_deliverability.py`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::routes::AppState;

/// `POST /api/v1/test/mail/blocklist-self-check/run` — run the daily
/// self-check once. Returns `{"ran": true}`; its outcome is read from the
/// admin's history reads.
async fn handle_run(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    crate::bridge_routing_handlers::run_scheduled_blocklist_self_check(&state).await;
    Json(json!({ "ran": true }))
}

/// Mount the `/api/v1/test/mail/blocklist-self-check/run` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/mail/blocklist-self-check/run",
        post(handle_run),
    )
}
