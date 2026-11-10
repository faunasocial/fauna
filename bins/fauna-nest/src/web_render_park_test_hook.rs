//! Test-only HTTP endpoint that parks one actor's next web render in flight,
//! and lets it go.
//!
//! Gated on `test-hooks` **alone** (the `web_render_fault_test_hook` shape), so
//! it rides the standard `--features test-hooks` e2e build and never compiles
//! into production.
//!
//! Why it exists: *A reader sees a whole site or none* (`web-content-hosting.md`
//! § Routing, render, serving) is a promise about what a visitor is served
//! WHILE a render runs, and an ordinary site renders in milliseconds — so
//! fetching pages "during" a re-render from outside races the render and
//! proves nothing (convention 14). This door holds a real render at its first
//! body write, past its claim, its withdrawal-deadline race and its listing
//! ([`crate::web_content::service::WebContentService::arm_render_park`]), so a
//! test reads the site at a known point inside the render and then releases
//! it. The optional `deadline_ms` states which side of the withdrawal deadline
//! the render is on — zero puts a revoking render past it before its first
//! page, as the Rust pins do through `with_withdrawal_deadline` — instead of
//! waiting out the production constant.
//!
//! Consumer: `tests/e2e-unified/tests/api/test_web_render_atomicity.py`.

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
#[serde(rename_all = "snake_case")]
enum Action {
    /// Arm a park: the actor's next render waits at its first body write.
    Arm,
    /// Report whether a render waits at the park.
    Status,
    /// Release the parked render (if any) and disarm.
    Release,
}

#[derive(Deserialize)]
struct RenderPark {
    /// The site owner's 32-byte actor id, hex.
    actor_id: String,
    action: Action,
    /// `arm` only: the withdrawal deadline, in milliseconds, for renders that
    /// start while the park is armed. Absent = the production constant.
    #[serde(default)]
    deadline_ms: Option<u64>,
}

/// `POST /api/v1/test/web/render-park` — arm, read or release the actor's
/// render park. Returns `{"ok": true, "parked": bool}`.
async fn handle_render_park(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenderPark>,
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
    match req.action {
        Action::Arm => {
            service.arm_render_park(
                &actor,
                req.deadline_ms.map(std::time::Duration::from_millis),
            );
            Json(json!({ "ok": true, "parked": false })).into_response()
        }
        Action::Status => {
            Json(json!({ "ok": true, "parked": service.render_parked(&actor) })).into_response()
        }
        Action::Release => {
            let armed = service.release_render_park(&actor);
            Json(json!({ "ok": armed, "parked": false })).into_response()
        }
    }
}

/// Mount the `/api/v1/test/web/render-park` route.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new().route("/api/v1/test/web/render-park", post(handle_render_park))
}
