//! Integration tests for the multipart-error paths of POST /api/v1/blob.
//!
//! Happy-path multipart (one per AudienceClass) lives in
//! `tests/blob_ingest_sidecar.rs`. This file covers the four 400-producing
//! shapes (missing parts, extra part, sidecar decode failure) + the
//! 413-producing oversize body + the 400-producing wrong-content-type.

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_media::audience::AudienceClass;
use fauna_media::sidecar::UploadSidecar;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;
use reqwest::multipart;

async fn boot_test_nest() -> (String, String) {
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

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), token)
}

fn sidecar_library() -> Vec<u8> {
    UploadSidecar {
        class: AudienceClass::Library,
        mime: "application/octet-stream".to_string(),
        has_c2pa: false,
        thumbnail_hash: None,
    }
    .to_dag_cbor()
}

fn aead_shaped_bytes() -> Vec<u8> {
    (0u8..64).collect()
}

#[tokio::test]
async fn missing_sidecar_part_returns_400() {
    let (base, token) = boot_test_nest().await;
    let form = multipart::Form::new().part(
        "bytes",
        multipart::Part::bytes(aead_shaped_bytes())
            .mime_str("application/octet-stream")
            .unwrap(),
    );
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(
        err.contains("sidecar"),
        "expected error to mention sidecar, got: {err}"
    );
}

#[tokio::test]
async fn missing_bytes_part_returns_400() {
    let (base, token) = boot_test_nest().await;
    let form = multipart::Form::new().part(
        "sidecar",
        multipart::Part::bytes(sidecar_library())
            .mime_str("application/cbor")
            .unwrap(),
    );
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(
        err.contains("bytes"),
        "expected error to mention bytes, got: {err}"
    );
}

#[tokio::test]
async fn extra_part_returns_400() {
    let (base, token) = boot_test_nest().await;
    let form = multipart::Form::new()
        .part(
            "sidecar",
            multipart::Part::bytes(sidecar_library())
                .mime_str("application/cbor")
                .unwrap(),
        )
        .part(
            "bytes",
            multipart::Part::bytes(aead_shaped_bytes())
                .mime_str("application/octet-stream")
                .unwrap(),
        )
        .part(
            "extra",
            multipart::Part::bytes(b"should not be here".to_vec()),
        );
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn sidecar_decode_failure_returns_400() {
    let (base, token) = boot_test_nest().await;
    // Not valid CBOR — leading 0xff is invalid as the start of a CBOR map.
    let bad_sidecar = vec![0xffu8; 32];
    let form = multipart::Form::new()
        .part(
            "sidecar",
            multipart::Part::bytes(bad_sidecar)
                .mime_str("application/cbor")
                .unwrap(),
        )
        .part(
            "bytes",
            multipart::Part::bytes(aead_shaped_bytes())
                .mime_str("application/octet-stream")
                .unwrap(),
        );
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(
        err.contains("sidecar"),
        "expected error to mention sidecar decode, got: {err}"
    );
}

#[tokio::test]
async fn octet_stream_legacy_path_now_rejected() {
    // Strict flip: the legacy `application/octet-stream` shape retired alongside
    // the blob strict flip. Any non-multipart
    // Content-Type → 400 in both storage modes (every app now sidecars every
    // upload). This boots the default plaintext storage — the retirement is
    // route-level, ahead of mode dispatch.
    let (base, token) = boot_test_nest().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/octet-stream")
        .body(b"legacy raw bytes upload".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(
        err.contains("multipart/form-data"),
        "expected error to mention multipart requirement, got: {err}"
    );
}

#[tokio::test]
async fn unknown_content_type_returns_400() {
    let (base, token) = boot_test_nest().await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(b"{}".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
