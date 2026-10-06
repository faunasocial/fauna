//! Test-only HTTP endpoints forcing a bridge provider's `status()` into the
//! two degraded shapes `fauna_client_bridges::link_block` recognizes
//! (`bridges.md` § Errors & edge cases → *A bridge that cannot be linked
//! right now*), so a tier_3 test can reach `bridge-link-blocked-reason`
//! end-to-end without a naturally-failing provider: every registered
//! provider's `status()` only errors on a genuine DB fault, and every
//! declared `BridgeLinkMode` today has `platform: None`, so a client's
//! `applicable_modes` count never falls to zero on its own
//! (`AppState::bridge_status_override`'s doc comment).
//!
//! Gated on `test-hooks` **alone** (like `link_preview_test_hook` /
//! `outbound_clock_test_hook`) so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build.
//!
//! Consumer: `tests/e2e-unified/tests/test_bridges.py`. Production never
//! compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::bridge_management::BridgeStatusOverride;
use crate::routes::AppState;

#[derive(Deserialize)]
struct OverrideBody {
    /// Force `provider.status()` to return `Err(..)` with this message — the
    /// nest surfaces it as `BridgeStatus.error` verbatim
    /// (`bridges_ui_handlers::list_handler`'s override branch).
    #[serde(default)]
    error: Option<String>,
    /// Force an `Ok` status with `linked: false, link_modes: None` and no
    /// error — declared, but nothing applies.
    #[serde(default)]
    no_applicable_modes: bool,
}

/// `POST /api/v1/test/bridges/:id/status-override` — see [`OverrideBody`].
/// Returns `{"ok": true, "id": <id>}`.
async fn install_override(
    State(state): State<Arc<AppState>>,
    Path(bridge_id): Path<String>,
    Json(body): Json<OverrideBody>,
) -> impl IntoResponse {
    let over = match (body.error, body.no_applicable_modes) {
        (Some(msg), _) => BridgeStatusOverride::Error(msg),
        (None, true) => BridgeStatusOverride::NoApplicableModes,
        (None, false) => {
            return ApiError::bad_request("provide `error` or `no_applicable_modes: true`")
                .into_response();
        }
    };
    state
        .bridge_status_override
        .lock()
        .expect("bridge_status_override mutex poisoned")
        .insert(bridge_id.clone(), over);
    Json(json!({ "ok": true, "id": bridge_id })).into_response()
}

/// `POST /api/v1/test/bridges/:id/clear` — drop the override for one bridge
/// (test isolation). Returns `{"ok": true}`.
async fn clear_override(
    State(state): State<Arc<AppState>>,
    Path(bridge_id): Path<String>,
) -> impl IntoResponse {
    state
        .bridge_status_override
        .lock()
        .expect("bridge_status_override mutex poisoned")
        .remove(&bridge_id);
    Json(json!({ "ok": true })).into_response()
}

/// Mount the `/api/v1/test/bridges/:id/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/v1/test/bridges/{id}/status-override",
            post(install_override),
        )
        .route("/api/v1/test/bridges/{id}/clear", post(clear_override))
}
