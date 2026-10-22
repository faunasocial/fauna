//! Test-only HTTP endpoint that records one nest-placed ACME order's failure,
//! as the CA would have described it.
//!
//! Gated on `test-hooks` **alone** (the `cert_nudge_test_hook` shape), so it
//! rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists (`tls-certificates.md` § Keeping the cert alive → *One
//! compile-gated seam for the failing-order line*): the nest places its own
//! orders only in HTTP-01 mode, which needs a TLS-serving nest on the fixed
//! challenge port — no harness session nest runs it, so the admin's
//! failing-order line is unreachable from an app e2e. The hook writes through
//! the SAME recorder the lifecycle loop's arms call
//! ([`RetryState::record_order_error`] on a failure,
//! [`RetryState::clear_order_error`] on a success, then [`RetryState::save`]),
//! and touches nothing else: the failed-attempt budget is left alone, so the
//! seam cannot delay a real order. Everything downstream — the file read by
//! `fauna.tls.cert_status`, the wire, the shared snapshot and the app's line —
//! is the production path.
//!
//! Consumer: `tests/e2e-unified/tests/test_admin_dns_cert_issuance_error.py`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::acme_http01::{IssuanceOrder, RetryState, now_unix};
use crate::api_error::ApiError;
use crate::routes::AppState;

#[derive(Deserialize)]
struct RecordBody {
    /// The stored order tag (`domain` / `ip_bridge`).
    order: String,
    /// The CA-supplied failure text to keep; `null` clears the order's entry,
    /// as that order's success does.
    detail: Option<String>,
}

/// `POST /api/v1/test/tls/issuance-error` — record `detail` as `order`'s last
/// failure, now, or clear that order's entry (`detail: null`, the success
/// arm's own [`RetryState::clear_order_error`]). Replies `{"order": <tag>,
/// "kept": <bool>}`. Refuses (409) on a nest running ACME, whose lifecycle
/// loop owns the file, so a mis-set fixture fails loud instead of racing it;
/// an unknown order tag is a 400.
async fn handle_record(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecordBody>,
) -> impl IntoResponse {
    let node_mode = *state.node_mode.read().await;
    let (_, acme_enabled) = crate::acme::build_acme_config(&state.config, node_mode, None);
    if acme_enabled {
        return ApiError::conflict(
            "issuance-error: ACME runs here — the lifecycle loop owns the file",
        )
        .into_response();
    }
    let Some(order) = IssuanceOrder::from_tag(&body.order) else {
        return ApiError::bad_request("issuance-error: unknown order tag").into_response();
    };
    let mut retry = RetryState::load(&state.acme_dir);
    match &body.detail {
        Some(detail) => retry.record_order_error(order, detail, now_unix()),
        None => {
            retry.clear_order_error(order);
        }
    }
    retry.save(&state.acme_dir);
    Json(json!({ "order": order.as_str(), "kept": body.detail.is_some() })).into_response()
}

/// Mount the `/api/v1/test/tls/issuance-error` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/tls/issuance-error", post(handle_record))
}
