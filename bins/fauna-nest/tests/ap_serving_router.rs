//! Tests: the ActivityPub discovery surface reaches its handlers through the
//! FULL `build_router` — never the `web_content_or_info` landing page.
//!
//! `activitypub.md` § Architecture lists `/.well-known/nodeinfo`,
//! `/nodeinfo/2.1`, `/.well-known/webfinger` and `/ap/instance` as the shipped
//! serving surface. The handler-level tests in `activitypub/actor_routes.rs`
//! mount `actor_routes::routes()` alone, so they cannot see a route shadowed or
//! dropped by the composition in `build_router`; this file can.

use std::sync::Arc;

use fauna_nest::db::CacheDb;

async fn start_server() -> String {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(fauna_nest::routes::AppState::for_test(db));
    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://{addr}")
}

/// The landing page `info_page` renders — what an unmatched path falls to.
const LANDING_MARKER: &str = "Fauna Nest API";

async fn get_body(url: &str) -> (u16, String, String) {
    let resp = reqwest::Client::new().get(url).send().await.unwrap();
    let status = resp.status().as_u16();
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, ctype, resp.text().await.unwrap())
}

#[tokio::test]
async fn nodeinfo_discovery_is_served_not_the_landing_page() {
    let base = start_server().await;
    let (status, ctype, body) = get_body(&format!("{base}/.well-known/nodeinfo")).await;
    assert!(
        !body.contains(LANDING_MARKER),
        "/.well-known/nodeinfo fell through to the landing page ({status} {ctype})"
    );
    assert_eq!(status, 200);
    assert!(
        ctype.starts_with("application/json"),
        "content-type {ctype}"
    );
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        doc["links"][0]["href"]
            .as_str()
            .is_some_and(|h| h.ends_with("/nodeinfo/2.1")),
        "discovery document lacks the 2.1 link: {body}"
    );
}

#[tokio::test]
async fn ap_paths_never_fall_through_to_the_landing_page() {
    let base = start_server().await;
    for path in [
        "/nodeinfo/2.1",
        "/.well-known/webfinger?resource=acct:nobody@example.invalid",
        "/ap/instance",
    ] {
        let (status, ctype, body) = get_body(&format!("{base}{path}")).await;
        assert!(
            !body.contains(LANDING_MARKER),
            "{path} fell through to the landing page ({status} {ctype})"
        );
    }
}
