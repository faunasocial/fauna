//! Test-only HTTP endpoint that makes one actor's web renders fail, and stop
//! failing.
//!
//! Gated on `test-hooks` **alone** (the `web_blank_site_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: the restore retry (`web-content-hosting.md` § Routing,
//! render, serving → *A blanked site is owed its restore*) runs only after a
//! render has failed, and its warning is the signal an admin reads on the
//! Logs page. `/api/v1/test/web/blank-site` leaves the dark state behind but
//! deliberately wakes nothing, and a running nest's storage cannot be made to
//! fail from outside. This door sets a per-actor fault the render checks
//! first, then re-renders the site through the fail-closed path
//! ([`crate::web_content::service::WebContentService::set_render_fault`]), so
//! the clear, the owed-restore row and the retry's wake are the production
//! writer's. Every later render of that site — the retry's included — fails
//! until the fault is lifted.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_web_restore_retry_log.py`.

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
struct RenderFault {
    /// The site owner's 32-byte actor id, hex.
    actor_id: String,
    /// `true` fails the actor's renders and takes the site dark; `false`
    /// lifts the fault and renders nothing.
    fail: bool,
}

/// `POST /api/v1/test/web/render-fault` — set or lift the actor's render
/// fault. Returns `{"ok": true}`.
async fn handle_render_fault(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenderFault>,
) -> impl IntoResponse {
    let Some(actor) = hex::decode(&req.actor_id)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    else {
        return ApiError::bad_request("actor_id must be 32 bytes of hex").into_response();
    };
    let Some(service) = &state.web_content_service else {
        return ApiError::internal("this nest renders no web sites").into_response();
    };
    match service.set_render_fault(&actor, req.fail).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => ApiError::internal(format!("render fault: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/web/render-fault` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/web/render-fault", post(handle_render_fault))
}
