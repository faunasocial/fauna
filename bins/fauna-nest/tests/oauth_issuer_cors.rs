//! The OAuth issuer plane's CORS posture — **open to every origin, with no
//! credentials** — over the real router (`authorization-server.md` § The issuer →
//! *Cross-origin access*), and the credentialed app allowlist staying exactly as
//! closed as it was for every other route.
//!
//! Why this is a router test and not a handler test: the rule is about the
//! LAYERING — the issuer router carries its own CORS layer and is merged after
//! the credentialed one — so a regression would be a second
//! `Access-Control-Allow-Origin` on the same response, or the plane sliding back
//! under the allowlist. Only the assembled router can show either.
//!
//! Tier 1 (`testing.md` § The four-tier taxonomy): in-process, no listener.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use tower::ServiceExt; // oneshot

/// A website on an origin this nest has never heard of.
const FOREIGN_ORIGIN: &str = "https://third-party.example";

/// The one origin the credentialed allowlist admits on a fresh nest.
const APP_ORIGIN: &str = fauna_nest::node_policy_core::DEFAULT_CORS_ORIGIN;

fn state_with_domain() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    // A claimed domain, so the issuer surfaces answer rather than `503` — the
    // CORS headers attach either way, but the token endpoint's `DPoP-Nonce`
    // is minted only once there is an issuer to mint for.
    state
        .identity_domain
        .store(Some(Arc::new("nest.example".to_string())));
    state
}

async fn send(state: &Arc<AppState>, req: Request<Body>) -> axum::response::Response {
    fauna_nest::build_router(state.clone())
        .oneshot(req)
        .await
        .unwrap()
}

fn header_values<'a>(resp: &'a axum::response::Response, name: &str) -> Vec<&'a str> {
    resp.headers()
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect()
}

fn header_lower(resp: &axum::response::Response, name: &str) -> String {
    header_values(resp, name).join(",").to_ascii_lowercase()
}

fn preflight(path: &str, origin: &str, method: &str, request_headers: &str) -> Request<Body> {
    Request::builder()
        .method("OPTIONS")
        .uri(path)
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, method)
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, request_headers)
        .body(Body::empty())
        .unwrap()
}

/// The headline: a foreign origin's preflight to the token endpoint is admitted,
/// with the `DPoP` request header allowed and no credentials offered.
#[tokio::test]
async fn a_foreign_origins_preflight_to_the_token_endpoint_admits_dpop() {
    let state = state_with_domain();
    let resp = send(
        &state,
        preflight("/oauth/token", FOREIGN_ORIGIN, "POST", "dpop, content-type"),
    )
    .await;

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a preflight is answered, never 405'd"
    );
    assert_eq!(
        header_values(&resp, "access-control-allow-origin"),
        vec!["*"],
        "open to every origin, exactly once"
    );
    let allowed = header_lower(&resp, "access-control-allow-headers");
    assert!(
        allowed.contains("dpop"),
        "DPoP must be an allowed request header, got {allowed:?}"
    );
    assert!(
        allowed.contains("content-type"),
        "the form post needs Content-Type, got {allowed:?}"
    );
    assert!(
        header_lower(&resp, "access-control-allow-methods").contains("post"),
        "the token endpoint is POSTed to"
    );
    assert!(
        resp.headers()
            .get("access-control-allow-credentials")
            .is_none(),
        "the plane is credential-less: nothing on it is authenticated by cookie or bearer"
    );
}

/// A token-endpoint response exposes `DPoP-Nonce` and `WWW-Authenticate` to the
/// foreign page — the nonce is what a browser client dials its principal session
/// with, and the challenge is how it learns it needs one.
#[tokio::test]
async fn a_token_endpoint_reply_exposes_the_nonce_to_a_foreign_origin() {
    let state = state_with_domain();
    let resp = send(
        &state,
        Request::builder()
            .method("POST")
            .uri("/oauth/token")
            .header(header::ORIGIN, FOREIGN_ORIGIN)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("grant_type=authorization_code"))
            .unwrap(),
    )
    .await;

    // No DPoP proof was sent, so this is a refusal — and a refusal is exactly
    // the reply that carries the nonce a client retries with.
    assert!(
        resp.status().is_client_error(),
        "a proof-less token request is refused, got {}",
        resp.status()
    );
    assert!(
        resp.headers().get("dpop-nonce").is_some(),
        "every token-endpoint reply carries a DPoP-Nonce (§ Architectural rules #2)"
    );
    assert_eq!(
        header_values(&resp, "access-control-allow-origin"),
        vec!["*"]
    );
    let exposed = header_lower(&resp, "access-control-expose-headers");
    assert!(
        exposed.contains("dpop-nonce"),
        "DPoP-Nonce must be exposed, got {exposed:?}"
    );
    assert!(
        exposed.contains("www-authenticate"),
        "WWW-Authenticate must be exposed, got {exposed:?}"
    );
}

/// The read surfaces a browser client reaches FIRST — both discovery documents
/// and the JWKS — answer a foreign origin too; a plane open only at PAR and the
/// token endpoint would still be unreachable from a browser.
#[tokio::test]
async fn the_discovery_documents_and_jwks_answer_a_foreign_origin() {
    let state = state_with_domain();
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/openid-configuration",
        "/oauth/jwks",
    ] {
        let resp = send(
            &state,
            Request::builder()
                .uri(path)
                .header(header::ORIGIN, FOREIGN_ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            header_values(&resp, "access-control-allow-origin"),
            vec!["*"],
            "{path} must be readable from a foreign origin"
        );
    }
    // And PAR, the first request-taking endpoint of the browser start.
    let resp = send(
        &state,
        preflight("/oauth/par", FOREIGN_ORIGIN, "POST", "dpop, content-type"),
    )
    .await;
    assert_eq!(
        header_values(&resp, "access-control-allow-origin"),
        vec!["*"]
    );
}

/// The credentialed app routes are untouched: the foreign origin gets nothing
/// there, and the nest's own app origin is still echoed back with credentials.
#[tokio::test]
async fn the_credentialed_app_routes_stay_closed_to_the_foreign_origin() {
    let state = state_with_domain();

    let resp = send(
        &state,
        preflight("/api/v1/health", FOREIGN_ORIGIN, "GET", "authorization"),
    )
    .await;
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "an app route must not admit a foreign origin: {:?}",
        header_values(&resp, "access-control-allow-origin")
    );
    let resp = send(
        &state,
        Request::builder()
            .uri("/api/v1/health")
            .header(header::ORIGIN, FOREIGN_ORIGIN)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "a plain app GET from a foreign origin carries no allow-origin"
    );

    let resp = send(
        &state,
        preflight("/api/v1/health", APP_ORIGIN, "GET", "authorization"),
    )
    .await;
    assert_eq!(
        header_values(&resp, "access-control-allow-origin"),
        vec![APP_ORIGIN],
        "the nest's own app origin is still echoed back"
    );
    assert_eq!(
        header_values(&resp, "access-control-allow-credentials"),
        vec!["true"],
        "and still credentialed"
    );
}

/// The issuer plane answers the nest's own app origin with the open header too —
/// one `Access-Control-Allow-Origin` per response, never the credentialed
/// layer's echo stacked on top of `*` (the layering the merge order protects).
#[tokio::test]
async fn the_issuer_plane_never_stacks_two_allow_origin_headers() {
    let state = state_with_domain();
    let resp = send(
        &state,
        Request::builder()
            .uri("/.well-known/oauth-authorization-server")
            .header(header::ORIGIN, APP_ORIGIN)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        header_values(&resp, "access-control-allow-origin"),
        vec!["*"],
        "exactly one value, and it is the open one"
    );
    assert!(
        resp.headers()
            .get("access-control-allow-credentials")
            .is_none(),
        "the credentialed layer must not reach the issuer plane"
    );
}
