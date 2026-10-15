//! Test-only HTTP endpoint that runs the MTA-STS advance's clock half once, at a
//! clock shifted forward.
//!
//! Gated on `test-hooks` **alone** (the `mail_domain_gc_test_hook` shape), so it
//! rides the standard `--features test-hooks` e2e build and never compiles into
//! production.
//!
//! Why it exists: every mail domain is stored `testing` at add and only the
//! nest's seven-day advance moves it to `enforce` (`mail-multidomain.md` § The
//! advance), so no tier can make two domains differ in posture without moving the
//! clock. The hook calls the SAME [`crate::mta_sts_advance::advance_clock_half`]
//! the hourly pass calls — the DB statement, the log and the bridges' config push
//! — with `now` moved `shift_days` ahead. It skips the trust half
//! ([`crate::mta_sts_advance::mail_name_trusted`]) because a harness nest never
//! serves a trusted mail certificate; it forges no trust.
//!
//! Distinct from `mta_sts_test_hook`, which scripts outbound MTA-STS lookups.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_primary_domain_rename_lifecycle.py`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::routes::AppState;

#[derive(Deserialize)]
struct AdvanceBody {
    /// How many days past the real clock the clock half runs at.
    shift_days: u32,
}

/// `POST /api/v1/test/mail-domains/mta-sts/advance` — run the clock half once at
/// `now + shift_days`. Returns `{"advanced": [<domain>, …]}`, the names moved to
/// `enforce`.
async fn handle_advance(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AdvanceBody>,
) -> impl IntoResponse {
    let now_ms = crate::db::now_epoch_millis() + i64::from(body.shift_days) * 86_400_000;
    let advanced = crate::mta_sts_advance::advance_clock_half(&state, now_ms).await;
    Json(json!({ "advanced": advanced })).into_response()
}

/// Mount the `/api/v1/test/mail-domains/mta-sts/advance` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/mail-domains/mta-sts/advance",
        post(handle_advance),
    )
}
