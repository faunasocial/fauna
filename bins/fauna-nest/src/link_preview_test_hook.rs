//! Test-only HTTP endpoints scripting the D4 link-preview fetch seam.
//!
//! Gated on `test-hooks` **alone** (like `mta_sts_test_hook` /
//! `outbound_clock_test_hook`) so it is present in the standard
//! `cargo build -p fauna-nest --features test-hooks` e2e build. It lets a tier_3
//! test install a fixture page / image for a url into
//! [`crate::routes::AppState::link_preview_override`]; the
//! `fauna.linkpreview.resolve` handler's `OverrideFetcher` then serves that
//! fixture *instead of* the real outbound fetch — so the served-OG success path
//! (and the og:image blob store) runs **without** loosening production SSRF (a
//! url the test does *not* install still hits the real SSRF-guarded fetcher, so
//! the private-IP/oversized rejection paths stay genuine).
//!
//! Consumer: `tests/e2e-unified/tests/api/test_link_preview.py`. Production
//! never compiles this module.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::AppState;

/// A minimal valid 1×1 PNG (the standard 67-byte transparent pixel). Served for
/// `image: true` fixtures so a test can install an og:image url without shipping
/// raw image bytes through JSON; it sniffs as `image/png` in `process_media`.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[derive(Deserialize)]
struct FixtureBody {
    /// The url the resolve handler will request — keyed exactly as posted.
    url: String,
    /// HTML body to serve (content-type `text/html`). For a page fixture.
    #[serde(default)]
    html: Option<String>,
    /// If `true`, serve [`TINY_PNG`] (content-type `image/png`) at `url` — point
    /// a page's `og:image` here to drive the image-blob-store path.
    #[serde(default)]
    image: bool,
}

/// `POST /api/v1/test/linkpreview/fixture` — install a scripted fetch result for
/// `url`. Returns `{"ok": true, "url": <url>}`.
async fn install_fixture(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FixtureBody>,
) -> impl IntoResponse {
    let (bytes, content_type): (Bytes, String) = if body.image {
        (Bytes::from_static(TINY_PNG), "image/png".to_string())
    } else {
        match body.html {
            Some(html) => (Bytes::from(html.into_bytes()), "text/html".to_string()),
            None => {
                return ApiError::bad_request("provide `html` or `image: true`").into_response();
            }
        }
    };
    state
        .link_preview_override
        .lock()
        .expect("link_preview_override mutex poisoned")
        .insert(body.url.clone(), (bytes, content_type));
    Json(json!({ "ok": true, "url": body.url })).into_response()
}

/// `POST /api/v1/test/linkpreview/clear` — drop all installed fixtures (test
/// isolation). Returns `{"ok": true}`.
async fn clear_fixtures(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state
        .link_preview_override
        .lock()
        .expect("link_preview_override mutex poisoned")
        .clear();
    Json(json!({ "ok": true })).into_response()
}

/// Mount the `/api/v1/test/linkpreview/*` routes.
pub fn routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/v1/test/linkpreview/fixture", post(install_fixture))
        .route("/api/v1/test/linkpreview/clear", post(clear_fixtures))
}
