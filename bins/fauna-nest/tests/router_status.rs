//! Tests: GET /internal/router-status endpoint — the body over loopback, and
//! the loopback-only gate (`worker.md` § Router Status Endpoint) that refuses
//! every other source and fails closed on a request with no peer address.

use std::net::SocketAddr;
use std::sync::Arc;

use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use tower::ServiceExt; // oneshot

fn test_state() -> Arc<fauna_nest::routes::AppState> {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    Arc::new(fauna_nest::routes::AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    })
}

async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let state = test_state();
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Serve as both production listeners do (`serve_plain` / `serve_tls`):
    // with `ConnectInfo`, so the loopback poll carries its peer address.
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });

    (format!("http://{addr}"), state)
}

/// Drive the real router in-process with an optional peer address.
async fn status_from(peer: Option<SocketAddr>) -> (axum::http::StatusCode, String) {
    let app = fauna_nest::build_router(test_state());
    let mut req = axum::http::Request::builder()
        .uri("/internal/router-status")
        .body(axum::body::Body::empty())
        .unwrap();
    if let Some(peer) = peer {
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn assert_no_status_fields(body: &str) {
    for field in ["nest_id", "version", "current_users", "max_users"] {
        assert!(
            !body.contains(field),
            "a refused poll must carry no `{field}`: {body}"
        );
    }
}

#[tokio::test]
async fn router_status_returns_nest_info() {
    let (url, _state) = start_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{url}/internal/router-status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["nest_id"].is_string());
    assert!(body["healthy"].as_bool().unwrap());
    assert!(body["max_users"].is_number());
    assert!(body["current_users"].is_number());
    assert!(body["version"].is_string());
}

#[tokio::test]
async fn router_status_refuses_a_non_loopback_source() {
    let (status, body) = status_from(Some(SocketAddr::from(([203, 0, 113, 9], 4444)))).await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "body: {body}");
    assert_no_status_fields(&body);
}

#[tokio::test]
async fn router_status_fails_closed_without_a_peer_address() {
    let (status, body) = status_from(None).await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "body: {body}");
    assert_no_status_fields(&body);
}
