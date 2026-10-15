//! Test-only HTTP endpoint that runs one at-risk renewal-push tick, at a clock
//! shifted forward.
//!
//! Gated on `test-hooks` **alone** (the `mail_domain_gc_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists (`tls-certificates.md` § C → *No cadence knob — and one
//! compile-gated tick seam for mitigation 4*): the nudge loop ticks once at
//! spawn — before any admin device could subscribe — then sleeps a day, so the
//! reminder itself is unreachable from a test run. The hook calls the SAME
//! [`crate::cert_nudge::tick_once`] the loop calls, with the same push
//! operation ([`crate::cert_nudge::admin_nudge_push`]) and `now` moved
//! `shift_days` ahead. The de-dup is honoured, never bypassed: `shift_days` is
//! the one lever, so a second call inside the window witnesses the de-dup and a
//! call a day on witnesses the re-nudge.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_cert_nudge_reminder.py`.

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
    /// How many days past the real clock the tick runs at (`0` when omitted).
    #[serde(default)]
    shift_days: u32,
}

/// `POST /api/v1/test/tls/cert-nudge/tick` — run one whole nudge tick at
/// `now + shift_days`. Returns `{"apex", "at_risk", "nudged", "dialled"}`,
/// what the tick decided. Refuses (409) where the loop would not have been
/// spawned or would have returned at once — ACME HTTP-01 on, no served-cert
/// resolver, no push service — so a mis-set fixture fails loud.
async fn handle_tick(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TickBody>,
) -> impl IntoResponse {
    // The spawn gate's ACME half, derived the way `main.rs` derives it — the
    // one source of truth for "this nest runs ACME" (HTTP-01 self-renews, so
    // no nudge loop runs there).
    let node_mode = *state.node_mode.read().await;
    let (_, acme_enabled) = crate::acme::build_acme_config(&state.config, node_mode, None);
    if acme_enabled {
        return ApiError::conflict("cert-nudge: ACME HTTP-01 is on — no nudge loop runs")
            .into_response();
    }
    let Some(resolver) = state.served_cert_spki.clone() else {
        return ApiError::conflict("cert-nudge: no served-cert resolver — no nudge loop runs")
            .into_response();
    };
    let Some(push) = state.push_service.clone() else {
        return ApiError::conflict("cert-nudge: no push service — no nudge loop runs")
            .into_response();
    };
    let push_op = crate::cert_nudge::admin_nudge_push(push, Arc::clone(&state.ws));
    let now = crate::acme_http01::now_unix() as i64 + i64::from(body.shift_days) * 86_400;
    let outcome =
        crate::cert_nudge::tick_once(&state, resolver.as_ref(), &state.acme_dir, &push_op, now)
            .await;
    Json(json!({
        "apex": outcome.apex,
        "at_risk": outcome.at_risk,
        "nudged": outcome.nudged,
        "dialled": outcome.dialled,
    }))
    .into_response()
}

/// Mount the `/api/v1/test/tls/cert-nudge/tick` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/tls/cert-nudge/tick", post(handle_tick))
}
