//! Test-only HTTP endpoint that runs the spam training-history retention sweep
//! once, now.
//!
//! Gated on `test-hooks` **alone** (the `pending_actions_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the sweep that ends a training entry's undo window
//! (`mail-spam.md` § Training-sample retention) runs once a day at 03:00 UTC
//! (`main.rs`), so its *effect* — the entry is gone and can no longer be undone,
//! while the model keeps what it learned — is unreachable from a tier_3 without
//! waiting out a day and a month. The hook calls the SAME
//! [`crate::bridge_routing_handlers::run_spam_training_history_gc`] the daily
//! loop calls; a test makes an entry old by writing its `created_at`, so what it
//! observes afterwards is the production sweep, not a shortcut around it.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_mail_spam_history_retention.py`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::routes::AppState;

/// `POST /api/v1/test/spam/history-gc/run` — run the retention sweep once.
/// Returns `{"ran": true}`; the sweep's own outcome is read from the table.
async fn handle_run(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    crate::bridge_routing_handlers::run_spam_training_history_gc(&state).await;
    Json(json!({ "ran": true }))
}

/// Mount the `/api/v1/test/spam/history-gc/run` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/spam/history-gc/run", post(handle_run))
}
