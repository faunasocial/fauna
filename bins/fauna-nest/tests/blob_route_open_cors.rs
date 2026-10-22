//! The post-blob route's CORS posture — **open to every origin, no credentials,
//! `x-c2pa` exposed** — over the real router (`transport-trust.md` § Transport
//! trust → *Mechanism for the federation-granted row* → *The post arm* → *The
//! browser's leg*): a web app on nest A's origin reads a post homed on nest B
//! straight from B's `GET /api/v1/blob/{hash}`, an origin B's credentialed
//! allowlist cannot know.
//!
//! A router test, not a handler test, for `oauth_issuer_cors.rs`'s reason: the
//! rule is about the LAYERING — the route sits in the `public_bytes` group,
//! merged after the credentialed layer — so a regression is the route sliding
//! back under the allowlist or a second `Access-Control-Allow-Origin` stacked on
//! one response. The CORS answer attaches whatever the handler's status (this
//! test nest has no blob store), so the hash below needs no stored bytes.
//!
//! Tier 1 (`testing.md` § The four-tier taxonomy): in-process, no listener.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, header};
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use tower::ServiceExt; // oneshot

/// The SPA of a reader on another nest — an origin this home has never heard of.
const FOREIGN_ORIGIN: &str = "https://app.reader-nest.example";

/// The one origin the credentialed allowlist admits on a fresh nest.
const APP_ORIGIN: &str = fauna_nest::node_policy_core::DEFAULT_CORS_ORIGIN;

/// A well-formed blob hash (plain hex, the legacy metadata-aware arm).
fn blob_path() -> String {
    format!("/api/v1/blob/{}", "0".repeat(64))
}

async fn send(req: Request<Body>) -> axum::response::Response {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    fauna_nest::build_router(state).oneshot(req).await.unwrap()
}

fn header_values<'a>(resp: &'a axum::response::Response, name: &str) -> Vec<&'a str> {
    resp.headers()
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect()
}

fn read(method: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(blob_path())
        .header(header::ORIGIN, origin)
        .body(Body::empty())
        .unwrap()
}

fn assert_open(resp: &axum::response::Response, what: &str) {
    assert_eq!(
        header_values(resp, "access-control-allow-origin"),
        vec!["*"],
        "{what}: the blob route is open to every origin, exactly once"
    );
    assert!(
        resp.headers()
            .get("access-control-allow-credentials")
            .is_none(),
        "{what}: the open layer is credential-less — a content hash is the capability"
    );
}

/// The headline: a foreign origin's GET is admitted and can read `x-c2pa`, the
/// header the browser's C2PA pre-filter keys on.
#[tokio::test]
async fn a_foreign_origins_blob_get_is_open_and_exposes_x_c2pa() {
    let resp = send(read("GET", FOREIGN_ORIGIN)).await;
    assert_open(&resp, "GET");
    let exposed = header_values(&resp, "access-control-expose-headers")
        .join(",")
        .to_ascii_lowercase();
    assert!(
        exposed.contains("x-c2pa"),
        "x-c2pa must be exposed cross-origin, got {exposed:?}"
    );
}

/// The HEAD the C2PA pre-filter sends (`head_has_c2pa`) answers the same way.
#[tokio::test]
async fn a_foreign_origins_blob_head_is_open() {
    assert_open(&send(read("HEAD", FOREIGN_ORIGIN)).await, "HEAD");
}

/// A bearer-carrying read preflights: the open layer admits `Authorization`
/// (the own-nest read's script-held bearer) without offering credentials.
#[tokio::test]
async fn a_bearer_carrying_preflight_is_admitted() {
    let resp = send(
        Request::builder()
            .method("OPTIONS")
            .uri(blob_path())
            .header(header::ORIGIN, FOREIGN_ORIGIN)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_open(&resp, "preflight");
    let allowed = header_values(&resp, "access-control-allow-headers")
        .join(",")
        .to_ascii_lowercase();
    assert!(
        allowed.contains("authorization"),
        "Authorization must be an allowed request header, got {allowed:?}"
    );
}

/// The credentialed answer is gone for this path: the nest's own app origin gets
/// the open answer too, never its origin echoed back with credentials.
#[tokio::test]
async fn the_own_app_origin_gets_the_open_answer_not_the_credentialed_one() {
    assert_open(&send(read("GET", APP_ORIGIN)).await, "own-origin GET");
}
