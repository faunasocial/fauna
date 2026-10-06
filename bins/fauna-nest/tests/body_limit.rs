use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

#[tokio::test]
async fn oversized_body_rejected() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let state = Arc::new(fauna_nest::routes::AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let client = reqwest::Client::new();

    // An 11 MB upload should be rejected (limit is 10 MB). Posted to the blob
    // upload route as its `multipart/form-data` shape (the only accepted shape
    // post strict-flip) — the multipart parser enforces the same
    // `BLOB_BODY_LIMIT` cap on the accumulated body.
    let big_bytes = vec![0u8; 11 * 1024 * 1024];
    let sidecar_bytes = fauna_media::sidecar::UploadSidecar {
        class: fauna_media::audience::AudienceClass::PublicPost,
        mime: "application/octet-stream".to_string(),
        has_c2pa: false,
        thumbnail_hash: None,
    }
    .to_dag_cbor();
    let form = reqwest::multipart::Form::new()
        .part(
            "sidecar",
            reqwest::multipart::Part::bytes(sidecar_bytes)
                .mime_str("application/cbor")
                .unwrap(),
        )
        .part(
            "bytes",
            reqwest::multipart::Part::bytes(big_bytes)
                .mime_str("application/octet-stream")
                .unwrap(),
        );
    let resp = client
        .post(format!("http://{addr}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);
}
