//! Test-only HTTP endpoints for the two trending mechanisms an api-tier test
//! cannot otherwise reach in a test's lifetime (`docs/goal/behavior/trending.md`
//! § Local velocity, § Import-triggered public-post fetch).
//!
//! * `POST /api/v1/test/trending/sweep` runs the production wall-clock sweep
//!   ([`crate::db::CacheDb::sweep_trend_scores`], the body of
//!   [`crate::trend_sweeper::spawn_trend_sweeper`]'s tick) once, at the real
//!   clock plus `advance_s` seconds. The decay half-life is hours, so without
//!   it the "a post with no new engagement fades out" outcome is days away.
//!   The advance is ONE-SHOT, never a stored clock override: the trend clock
//!   is read inside the engagement transition hook and the federation export
//!   too, and a persistent skew there would change what the rest of the test
//!   observes. A withdrawn row stays withdrawn: the real sweeper scans only
//!   live rows, and no new act recomputes the post.
//! * `POST /api/v1/test/trending/tamper-post-get` makes this nest's
//!   `fauna.federation.post.get` answer serve bytes that do NOT hash to the
//!   requested id (the stored bytes with their last byte flipped) until
//!   cleared, standing this real nest in for a peer that substitutes content.
//!   The seam is cut at the serve point ([`maybe_tamper`], called from
//!   `federation_handlers::post_get_handler`) and nowhere else: nothing
//!   stored changes, so clearing it makes the same nest an honest peer again.
//!
//! Gated on `test-hooks` alone, so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build. Consumer:
//! `tests/e2e-unified/tests/api/test_trending_federation.py`. Production never
//! compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// Whether this nest's `post.get` answer serves substituted bytes. A process
/// static: one nest per process, and the flag is read on the serve path only.
static TAMPER_POST_GET: AtomicBool = AtomicBool::new(false);

/// `post.get` answers served with substituted bytes, and as stored — the
/// test's barrier that a peer's fetch really reached this nest (a fetch it
/// refused shows as a later honest serve: a peer that had accepted the
/// substitute holds the id and never asks again).
static TAMPERED_SERVES: AtomicU64 = AtomicU64::new(0);
static HONEST_SERVES: AtomicU64 = AtomicU64::new(0);

/// The `post.get` serve seam: the stored bytes, or — while the tamper switch is
/// on — the same bytes with their last byte flipped, which no longer hash to
/// the id they were requested under.
pub fn maybe_tamper(mut bytes: Vec<u8>) -> Vec<u8> {
    if TAMPER_POST_GET.load(Ordering::Relaxed)
        && let Some(last) = bytes.last_mut()
    {
        *last ^= 0xff;
        TAMPERED_SERVES.fetch_add(1, Ordering::Relaxed);
    } else {
        HONEST_SERVES.fetch_add(1, Ordering::Relaxed);
    }
    bytes
}

/// The switch and both serve counters, as one JSON reply.
fn tamper_state() -> serde_json::Value {
    json!({
        "on": TAMPER_POST_GET.load(Ordering::Relaxed),
        "tampered_serves": TAMPERED_SERVES.load(Ordering::Relaxed),
        "honest_serves": HONEST_SERVES.load(Ordering::Relaxed),
    })
}

#[derive(Deserialize)]
struct SweepBody {
    /// Seconds past the real clock to run the sweep at.
    advance_s: i64,
}

/// `POST /api/v1/test/trending/sweep` `{"advance_s": N}` — run the trend sweep
/// once at `now + N` s. Returns `{"rescored": <live rows scanned>}`.
async fn handle_sweep(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SweepBody>,
) -> impl IntoResponse {
    if body.advance_s < 0 {
        return ApiError::bad_request("`advance_s` must not be negative").into_response();
    }
    let now_us = fauna_core::data::Timestamp::now().as_i64() + body.advance_s * 1_000_000;
    match state.db.sweep_trend_scores(now_us).await {
        Ok(n) => Json(json!({ "rescored": n })).into_response(),
        Err(e) => ApiError::internal(format!("trend sweep: {e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct TamperBody {
    on: bool,
}

/// `POST /api/v1/test/trending/tamper-post-get` `{"on": bool}` — switch the
/// substituted-bytes `post.get` answer on or off. Returns the state below.
async fn handle_tamper(Json(body): Json<TamperBody>) -> impl IntoResponse {
    TAMPER_POST_GET.store(body.on, Ordering::Relaxed);
    Json(tamper_state()).into_response()
}

/// `GET /api/v1/test/trending/tamper-post-get` —
/// `{"on", "tampered_serves", "honest_serves"}`.
async fn handle_tamper_state() -> impl IntoResponse {
    Json(tamper_state()).into_response()
}

/// Mount the `/api/v1/test/trending/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/trending/sweep", post(handle_sweep))
        .route(
            "/api/v1/test/trending/tamper-post-get",
            post(handle_tamper).get(handle_tamper_state),
        )
}
