use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

#[tokio::test]
async fn blob_upload_download_roundtrip() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let token_store = Arc::new(TokenStore::new());

    let state = Arc::new(fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    // Create a registered user + token for auth
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let token = token_store.insert(kp.actor_id(), 3600).await;

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let client = reqwest::Client::new();
    let data = b"test blob data for upload";

    // Upload via the multipart shape (the only accepted shape post strict-flip).
    // This is plaintext-mode storage, which ignores the sidecar — any valid
    // sidecar works; we send a PublicPost one carrying the served MIME.
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
        .post(format!("http://{addr}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let hash_hex = body["hash"].as_str().unwrap().to_string();

    // Download (public, no auth needed)
    let resp = client
        .get(format!("http://{addr}/api/v1/blob/{hash_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), data);

    // Not found
    let fake_hash = "aa".repeat(32);
    let resp = client
        .get(format!("http://{addr}/api/v1/blob/{fake_hash}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// No-false-ACK contract, blob arm (the fix; pinned so the arm
/// can't silently revert to warn-and-ACK): when the metadata row — the blob's
/// only durable trace until a reference row lands — cannot be written,
/// `POST /api/v1/blob` must fail the request rather than ACK. The store is
/// content-addressed, so the client's retry re-puts idempotently.
#[tokio::test]
async fn blob_upload_fails_when_the_metadata_write_fails() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let token_store = Arc::new(TokenStore::new());

    let state = Arc::new(fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let token = token_store.insert(kp.actor_id(), 3600).await;

    // Fault injection: rename the table out from under `put_blob_metadata`, the
    // closest in-process stand-in for a mid-request sqlite write error.
    db.execute_batch("ALTER TABLE blob_metadata RENAME TO blob_metadata_fault;")
        .await
        .unwrap();

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

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
            reqwest::multipart::Part::bytes(b"blob fault-injection body".to_vec())
                .mime_str("application/octet-stream")
                .unwrap(),
        );
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "a failed metadata write must not ACK the blob upload (got {})",
        resp.status()
    );
}
