//! tier_1: what the nest's reserved `/app` answers under the admin's web-app
//! origin choice (`docs/goal/behavior/web-content-hosting.md` § Same-origin
//! security model → *The nest-served `/app/` and the central origin*).
//! Exercises the **real `mount_spa`** — the exact helper `build_router` mounts —
//! with a fixed serving decision standing in for the live `AppState` probe.
//!
//! - `bundled` serves the SPA this nest ships.
//! - `central` answers a **302** (never 301/308 — a browser caches a permanent
//!   redirect and the admin could not undo the choice) to the central origin
//!   with the same path and query and `nest=<domain>` appended, and the
//!   invariant-#5 security headers ride the redirect.
//! - A domainless box serves bundled whatever the choice
//!   (`WebAppOriginServing::BundledDomainless`).
//! - `/app` stays reserved with no SPA shipped: bundled answers 404, never
//!   anything else's content (invariant 3); central still redirects.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fauna_protocol::web_app_origin::WebAppOriginServing;
use tower::ServiceExt; // oneshot

const INDEX: &str = "<!doctype html><title>fauna</title>";

fn router(serving: WebAppOriginServing, with_spa: bool) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("index.html"), INDEX).unwrap();
    let dir = tmp.path().to_string_lossy().to_string();
    let base = axum::Router::new().fallback(|| async { (StatusCode::OK, "fallback content") });
    let router = fauna_nest::mount_spa(
        base,
        with_spa.then_some(dir.as_str()),
        fauna_nest::web_app_origin::fixed_probe(serving),
    );
    (router, tmp)
}

async fn get(router: &axum::Router, uri: &str) -> axum::response::Response {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.clone().oneshot(req).await.unwrap()
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

fn central() -> WebAppOriginServing {
    WebAppOriginServing::Central {
        nest_domain: "nest.example".into(),
    }
}

fn assert_security_headers(resp: &axum::response::Response) {
    let h = resp.headers();
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
    assert!(
        h.get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
}

#[tokio::test]
async fn bundled_serves_the_shipped_spa() {
    let (r, _tmp) = router(WebAppOriginServing::Bundled, true);
    let resp = get(&r, "/app/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("location").is_none());
    assert_security_headers(&resp);
    assert_eq!(body_text(resp).await, INDEX);
}

#[tokio::test]
async fn central_answers_a_temporary_redirect_with_path_query_and_nest() {
    let (r, _tmp) = router(central(), true);
    for (uri, want) in [
        ("/app/", "https://app.fauna.social/app/?nest=nest.example"),
        ("/app", "https://app.fauna.social/app?nest=nest.example"),
        (
            "/app/feed/deep?tab=2&nest=spoof.example",
            "https://app.fauna.social/app/feed/deep?tab=2&nest=nest.example",
        ),
    ] {
        let resp = get(&r, uri).await;
        assert_eq!(resp.status(), StatusCode::FOUND, "{uri}: must be 302");
        assert_eq!(
            resp.headers().get("location").unwrap().to_str().unwrap(),
            want,
            "{uri}"
        );
        assert_eq!(
            resp.headers().get("cache-control").unwrap(),
            "no-store",
            "{uri}: the redirect must not be cached — the admin can undo the choice"
        );
        assert_security_headers(&resp);
    }
}

#[tokio::test]
async fn a_domainless_box_serves_bundled_under_central() {
    let (r, _tmp) = router(WebAppOriginServing::BundledDomainless, true);
    let resp = get(&r, "/app/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("location").is_none());
    assert_eq!(body_text(resp).await, INDEX);
}

#[tokio::test]
async fn app_stays_reserved_without_a_shipped_spa() {
    let (r, _tmp) = router(WebAppOriginServing::Bundled, false);
    let resp = get(&r, "/app/").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_ne!(body_text(resp).await, "fallback content");

    let (r, _tmp) = router(central(), false);
    let resp = get(&r, "/app/x").await;
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "https://app.fauna.social/app/x?nest=nest.example"
    );
}

#[tokio::test]
async fn the_choice_never_reaches_paths_outside_app() {
    let (r, _tmp) = router(central(), true);
    let resp = get(&r, "/application").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("location").is_none());
    assert_eq!(body_text(resp).await, "fallback content");
}
