//! tier_3 (2026-06-23 module-isolation & client-attack-surface review;
//! `docs/goal/behavior/web-content-hosting.md` § Same-origin security model,
//! invariant #5): the SPA origin (`/app/*`) carries the defense-in-depth security
//! headers that protect the user's raw Ed25519 master secret (held in the SPA's
//! `localStorage` under `fauna_secret`). Exercises the **real `mount_spa`** — the
//! exact helper `build_router` mounts — so a refactor that drops the layer is
//! caught here.
//!
//! Two HTTP-header-only protections live in the nest (a `<meta>` CSP cannot carry
//! `frame-ancestors`, so it MUST be an HTTP header): the clickjacking lock
//! (`X-Frame-Options: DENY` + CSP `frame-ancestors 'none'`, invariant #5) and the
//! transport hygiene (`X-Content-Type-Options: nosniff`, `Referrer-Policy:
//! no-referrer`). The *resource* CSP (`script-src`/`style-src`/…) is emitted by
//! SvelteKit's `kit.csp` as a hashed `<meta>` in the built SPA, not here — that
//! half is verified by the web build + `--client web` e2e, not this test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // oneshot

/// A `Router` mounting a SPA `static_dir` (a temp dir holding a minimal
/// `index.html`) via the real `mount_spa`. Returns the router plus the tempdir
/// guard (kept alive for the duration of the requests).
fn spa_router() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("index.html"),
        "<!doctype html><title>fauna</title>",
    )
    .unwrap();
    let router = fauna_nest::mount_spa(
        axum::Router::new(),
        Some(&tmp.path().to_string_lossy()),
        fauna_nest::web_app_origin::fixed_probe(
            fauna_protocol::web_app_origin::WebAppOriginServing::Bundled,
        ),
    );
    (router, tmp)
}

/// GET `uri` over the router; returns the response.
async fn get(router: &axum::Router, uri: &str) -> axum::response::Response {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.clone().oneshot(req).await.unwrap()
}

/// The SPA index document carries all four security headers.
#[tokio::test]
async fn spa_index_carries_security_headers() {
    let (router, _tmp) = spa_router();
    let resp = get(&router, "/app/index.html").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers();

    assert_eq!(
        h.get("x-frame-options").map(|v| v.to_str().unwrap()),
        Some("DENY"),
        "SPA must deny framing (invariant #5 clickjacking lock)"
    );
    assert_eq!(
        h.get("x-content-type-options").map(|v| v.to_str().unwrap()),
        Some("nosniff"),
    );
    assert_eq!(
        h.get("referrer-policy").map(|v| v.to_str().unwrap()),
        Some("no-referrer"),
    );
    let csp = h
        .get("content-security-policy")
        .expect("SPA must carry a CSP header for frame-ancestors")
        .to_str()
        .unwrap();
    assert!(
        csp.contains("frame-ancestors 'none'"),
        "the HTTP-header CSP must lock frame-ancestors (a <meta> CSP cannot); got {csp:?}"
    );
}

/// A client-side-routed deep link (`/app/feed/…`) falls through `ServeDir` to the
/// index fallback; the headers must ride that fallback too (the SPA shell is
/// served for every in-app route).
#[tokio::test]
async fn spa_deep_link_fallback_carries_security_headers() {
    let (router, _tmp) = spa_router();
    let resp = get(&router, "/app/feed/some/deep/route").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-frame-options")
            .map(|v| v.to_str().unwrap()),
        Some("DENY"),
    );
}

/// Scope guard: the security headers are attached to the `/app` service, NOT
/// applied globally. A path outside `/app` (here a 404 on the SPA-only router)
/// must carry neither `X-Frame-Options` nor the SPA `frame-ancestors` CSP — the
/// apex/subdomain web-content fallback serves admin-/user-authored HTML on
/// isolated origins (invariant #1) and must not inherit the SPA framing lock.
#[tokio::test]
async fn headers_are_scoped_to_app_not_global() {
    let (router, _tmp) = spa_router();
    let resp = get(&router, "/somewhere-else").await;
    let h = resp.headers();
    assert!(
        h.get("x-frame-options").is_none(),
        "the SPA framing lock must stay scoped to /app, not applied globally"
    );
    assert!(
        h.get("content-security-policy").is_none(),
        "the SPA CSP must stay scoped to /app, not applied globally"
    );
}
