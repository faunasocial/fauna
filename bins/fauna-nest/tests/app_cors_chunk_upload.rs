//! A web app on an allowlisted origin other than the nest's own — the central
//! origin (`web-content-hosting.md` § The nest-served `/app/` and the central
//! origin) — uploads a content-keyed chunk cross-origin: `POST /api/v1/chunks`
//! under `X-Content-Hash` (`fauna-rpc-wasm`'s `post_octets_keyed`, the Media
//! upload's byte leg). The browser preflights that header, so the credentialed
//! app CORS layer must allow it, or every such upload fails before the nest
//! sees it.
//!
//! Tier 1 (`testing.md` § The four-tier taxonomy): the assembled router,
//! in-process, no listener.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, header};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use tower::ServiceExt; // oneshot

/// The one origin the credentialed allowlist admits on a fresh nest.
const APP_ORIGIN: &str = fauna_nest::node_policy_core::DEFAULT_CORS_ORIGIN;

#[tokio::test]
async fn an_allowlisted_origins_chunk_upload_preflight_admits_x_content_hash() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/api/v1/chunks")
        .header(header::ORIGIN, APP_ORIGIN)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization, content-type, x-content-hash",
        )
        .body(Body::empty())
        .unwrap();
    let resp = fauna_nest::build_router(state).oneshot(req).await.unwrap();

    let origins: Vec<_> = resp
        .headers()
        .get_all(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    assert_eq!(
        origins,
        [APP_ORIGIN],
        "the allowlisted origin is echoed once"
    );
    let allowed = resp
        .headers()
        .get_all(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(",")
        .to_ascii_lowercase();
    assert!(
        allowed.contains("x-content-hash"),
        "the chunk upload's store-key header must pass the preflight; allowed: {allowed:?}"
    );
}
