//! Test-only HTTP endpoint that runs the removed-mail-domain GC once, at a
//! clock shifted forward.
//!
//! Gated on `test-hooks` **alone** (the `spam_history_gc_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: a removed domain stays restorable for thirty days, and only
//! then does the background-expiry tick (`main.rs`) hard-delete it with its
//! DKIM key, cert blobs and aliases (`mail-multidomain.md` § After 30 days). That
//! effect is unreachable from a tier_3 without waiting out the window. The hook
//! calls the SAME [`crate::db::CacheDb::gc_expired_soft_deleted_mail_domains_at`]
//! the tick calls, with `now` moved `shift_days` ahead, so what a test observes
//! afterwards over the wire is the production sweep, not a shortcut around it.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py`.

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
struct RunBody {
    /// How many days past the real clock the sweep runs at.
    shift_days: u32,
}

/// `POST /api/v1/test/mail-domains/gc/run` — run the GC once at
/// `now + shift_days`. Returns `{"gone": [<domain>, …]}`, the names it removed.
async fn handle_run(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RunBody>,
) -> impl IntoResponse {
    let now_ms = crate::db::now_epoch_millis() + i64::from(body.shift_days) * 86_400_000;
    match state
        .db
        .gc_expired_soft_deleted_mail_domains_at(now_ms)
        .await
    {
        Ok(gone) => Json(json!({ "gone": gone })).into_response(),
        Err(e) => ApiError::internal(format!("mail-domain gc: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/mail-domains/gc/run` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/mail-domains/gc/run", post(handle_run))
}
