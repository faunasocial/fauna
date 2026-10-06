//! Test-only HTTP endpoint that takes one actor's rendered web site dark the
//! way a failed render does.
//!
//! Gated on `test-hooks` **alone** (the `pending_actions_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: a site goes dark only when a render hits a storage fault at
//! a revoking door (`web-content-hosting.md` § Routing, render, serving → *A
//! revoke is durable*), and a tier_3 has no honest way to make a running
//! nest's storage fail. The in-process pins break the render for real
//! (`tests/conformance_web.rs::break_the_render`); what a journey needs is
//! only the state that fault leaves behind, so the app's status line has
//! something to read. This hook calls the SAME writer the fail-closed door
//! calls — [`crate::db::CacheDb::clear_web_rendered_owing_restore`], the clear
//! and the owed-restore row in one transaction — so the state a test observes
//! is the production state, not a hand-inserted row. It deliberately does not
//! wake the restore retry: the journey then restores the site through a real
//! door's render, with no wall-clock wait.
//!
//! Consumer: `tests/e2e-unified/tests/test_web_authoring.py`.

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
struct BlankSite {
    /// The site owner's 32-byte actor id, hex.
    actor_id: String,
}

/// `POST /api/v1/test/web/blank-site` — clear the actor's rendered pages and
/// leave their restore owed. Returns `{"ok": true}`.
async fn handle_blank_site(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BlankSite>,
) -> impl IntoResponse {
    let Some(actor) = hex::decode(&req.actor_id)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    else {
        return ApiError::bad_request("actor_id must be 32 bytes of hex").into_response();
    };
    match state.db.clear_web_rendered_owing_restore(&actor).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => ApiError::internal(format!("blank site: {e:#}")).into_response(),
    }
}

/// Mount the `/api/v1/test/web/blank-site` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/web/blank-site", post(handle_blank_site))
}
