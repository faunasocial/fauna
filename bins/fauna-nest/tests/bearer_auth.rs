use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    // Leak the tempdir so it lives long enough
    let dir_path = dir.path().to_path_buf();
    std::mem::forget(dir);
    let backup_svc = Arc::new(BackupService::new(db.clone(), None, false, dir_path, None).unwrap());
    let tokens = Arc::new(TokenStore::new());
    let state = Arc::new(fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
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
async fn app_state_includes_token_store() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let state = Arc::new(fauna_nest::routes::AppState {
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    // Verify token store is accessible via state
    let kp = ActorKeypair::generate();
    let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;
    assert!(state.auth.token_store.validate(&token).await.is_some());
}

// `get_inbox_requires_bearer_token` DELETED 2026-07-12 (Phase-4 session; the test
// was already failing on `origin/main`, not by this change).
//
// It drove `GET /api/v1/inbox/{actor_id}` and asserted 401 / 200 / 403. **Both
// `/api/v1/inbox/{actor_id}` HTTP twins were deleted in the WS-RPC-everywhere rip**
// (`lib.rs`, the note above `build_router`'s route list). The nest's catch-all
// `.fallback(web_content_or_info)` answers an unrouted path with a `200` HTML info
// page — so the "without token -> 401" arm was asserting against the catch-all and
// had been red ever since the route went away, silently (nothing builds the nest's
// integration binaries on the merge path).
//
// This is the standing trap: **a deleted nest route returns 200, not 404** — never
// assert a status against a route you have not confirmed is registered
// (`rg 'route\("/…'`). Bearer-auth on the surviving surface is covered by the
// WS-RPC dispatcher's own auth gate; there is nothing here to re-home.

#[tokio::test]
async fn upload_blob_requires_bearer() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(kp.actor_id(), 3600).await;

    let client = reqwest::Client::new();
    let data = b"blob data for auth test";

    // Without token -> 401
    let resp = client
        .post(format!("{base}/api/v1/blob"))
        .body(data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // With token -> 201. Uploads via the multipart shape (the only accepted
    // shape post strict-flip); default plaintext storage ignores the sidecar.
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
            reqwest::multipart::Part::bytes(data.to_vec())
                .mime_str("application/octet-stream")
                .unwrap(),
        );
    let resp = client
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

// The `post_inbox_still_works_without_bearer` test was deleted with the
// `POST /api/v1/inbox/{actor}` HTTP twin (WS-RPC-everywhere rip): clients now ride
// the authenticated `fauna.inbox.send` kind, so there is no unauthenticated inbox
// POST left to exempt. The GET drain's `get_inbox_requires_bearer_token` above
// still guards the surviving `GET /api/v1/inbox/{actor}` twin.
