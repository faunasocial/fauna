//! Test-only HTTP endpoint standing in for the CA in a primary-domain rename.
//!
//! Gated on `test-hooks` alone (like `domain_expiry_test_hook`), so it is
//! present in the standard `cargo build -p fauna-nest --features test-hooks`
//! e2e build.
//!
//! # Why a hook rather than driving the real issuance
//!
//! A rename leaves `cert_issuance` only when the managed cert covers
//! `mail.<new-primary>` (`mail-primary-domain-rename.md` § Lifecycle), and that
//! takes a CA validating HTTP-01 against a name that resolves to the nest. A
//! standalone test nest listens on `127.0.0.1:<port>` and never spawns the
//! ACME loop at all, so its rename stays in `cert_issuance` for ever and the
//! grace-period controls and the post-flip identity are unreachable. The
//! issuance half is pinned elsewhere: the unit tests in
//! [`crate::acme_http01`] drive `cert_issuance → cert_ready → grace` with a
//! fake issuer, and the tier_4 pebble journey issues a real cert through the
//! nest's own HTTP-01 client. So the seam is cut at the CA: this hook stamps
//! the rename `cert_ready` as the loop would on seeing a covering cert, then
//! runs the loop's own post-cert steps
//! ([`crate::acme_http01::rename_post_cert_steps`] — the anchor flip, the
//! identity reconcile and its `config_changed` push), so everything from the
//! flip onwards is the production code against a real nest binary.
//!
//! Production never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// The fingerprint stamped on a hook-issued rename, so a row that reached
/// `cert_ready` this way is recognisable in the nest's own data.
const TEST_HOOK_FINGERPRINT: &str = "test-hook-no-ca";

/// `POST /api/v1/test/primary-domain-rename/cert-issued` — treat the in-flight
/// rename's `mail.<new>` SAN as issued and advance it through the anchor flip
/// into `grace`. Refuses when no rename is in `cert_issuance`.
async fn handle_cert_issued(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let rename = match state.db.get_active_rename().await {
        Ok(Some(r)) => r,
        Ok(None) => return ApiError::conflict("no rename in flight").into_response(),
        Err(e) => return ApiError::internal(format!("read rename: {e}")).into_response(),
    };
    if rename.parsed_state() != Some(fauna_mail::RenameState::CertIssuance) {
        return ApiError::conflict(format!("rename is in {}, not cert_issuance", rename.state))
            .into_response();
    }
    if let Err(e) = state
        .db
        .mark_rename_cert_ready(
            &rename.rename_id,
            TEST_HOOK_FINGERPRINT,
            crate::db::now_epoch_millis(),
        )
        .await
    {
        return ApiError::internal(format!("mark cert_ready: {e}")).into_response();
    }
    crate::acme_http01::rename_post_cert_steps(&state, Some(rename.rename_id)).await;
    match state.db.get_active_rename().await {
        Ok(Some(r)) => Json(json!({ "ok": true, "state": r.state })).into_response(),
        Ok(None) => ApiError::internal("rename vanished after the flip").into_response(),
        Err(e) => ApiError::internal(format!("re-read rename: {e}")).into_response(),
    }
}

/// Mount the `/api/v1/test/primary-domain-rename/cert-issued` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route(
        "/api/v1/test/primary-domain-rename/cert-issued",
        post(handle_cert_issued),
    )
}
