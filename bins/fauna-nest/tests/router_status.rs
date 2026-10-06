//! Tests: GET /internal/router-status endpoint.

use std::sync::Arc;

use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let state = Arc::new(fauna_nest::routes::AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (format!("http://{addr}"), state)
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
