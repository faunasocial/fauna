//! Per-class envelope-shape verifier integration tests for the (sole) sealed
//! blob ingest path. One test per AudienceClass.

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_media::audience::AudienceClass;
use fauna_media::sidecar::UploadSidecar;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::storage::{SealedStorage, Storage};
use fauna_nest::token_store::TokenStore;
use reqwest::multipart;

async fn boot_test_nest() -> (String, String) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let token_store = Arc::new(TokenStore::new());

    // `for_test` already installs `SealedStorage` — the one `Storage` impl
    // (`docs/goal/architecture/nest/storage-modes.md`) — so no swap is needed.
    let state = fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    };
    let state = Arc::new(state);

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

fn sidecar(class: AudienceClass, mime: &str, has_c2pa: bool) -> Vec<u8> {
    UploadSidecar {
        class,
        mime: mime.to_string(),
        has_c2pa,
        thumbnail_hash: None,
    }
    .to_dag_cbor()
}

fn aead_shaped_bytes(seed: u8) -> Vec<u8> {
    let mut v = vec![seed; 64];
    v[0] = seed.wrapping_add(0x10); // no plaintext magic prefix
    v
}

async fn post_blob(base: &str, token: &str, sc: Vec<u8>, bs: Vec<u8>) -> reqwest::Response {
    let form = multipart::Form::new()
        .part(
            "sidecar",
            multipart::Part::bytes(sc)
                .mime_str("application/cbor")
                .unwrap(),
        )
        .part(
            "bytes",
            multipart::Part::bytes(bs)
                .mime_str("application/octet-stream")
                .unwrap(),
        );
    reqwest::Client::new()
        .post(format!("{base}/api/v1/blob"))
        .header("authorization", format!("Bearer {token}"))
        .multipart(form)
        .send()
        .await
        .unwrap()
}

// ── Library: AEAD-shaped bytes + octet-stream mime + has_c2pa=false → 201

#[tokio::test]
async fn library_aead_bytes_accepted() {
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(AudienceClass::Library, "application/octet-stream", false);
    let bs = aead_shaped_bytes(0x42);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 201);
}

#[tokio::test]
async fn library_with_image_mime_in_sidecar_rejected() {
    // mime_class_mismatch: Library MUST have mime=application/octet-stream.
    // Strict flip: reject with 400, don't store.
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(AudienceClass::Library, "image/png", false);
    let bs = aead_shaped_bytes(0x43);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "ingest rejected: blob_mime_class_mismatch");
}

#[tokio::test]
async fn library_with_png_magic_in_bytes_rejected() {
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(AudienceClass::Library, "application/octet-stream", false);
    let mut bs = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bs.extend_from_slice(&[0u8; 64]);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"],
        "ingest rejected: blob_plaintext_magic_prefix"
    );
}

// ── Conversation: AEAD-shaped → 201

#[tokio::test]
async fn conversation_aead_bytes_accepted() {
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(
        AudienceClass::Conversation,
        "application/octet-stream",
        false,
    );
    let bs = aead_shaped_bytes(0x44);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 201);
}

// ── GroupRestrictedPost: AEAD-shaped → 201

#[tokio::test]
async fn group_restricted_post_aead_bytes_accepted() {
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(
        AudienceClass::GroupRestrictedPost,
        "application/octet-stream",
        false,
    );
    let bs = aead_shaped_bytes(0x45);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 201);
}

// ── PeriodRestrictedPost: AEAD-shaped → 201

#[tokio::test]
async fn period_restricted_post_aead_bytes_accepted() {
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(
        AudienceClass::PeriodRestrictedPost,
        "application/octet-stream",
        false,
    );
    let bs = aead_shaped_bytes(0x46);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 201);
}

// ── PublicPost: plaintext bytes are FINE; MIME must be non-empty.

#[tokio::test]
async fn public_post_plaintext_bytes_accepted() {
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(AudienceClass::PublicPost, "image/png", false);
    let mut bs = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bs.extend_from_slice(&[0u8; 32]);
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 201);
}

#[tokio::test]
async fn public_post_empty_mime_rejected() {
    // mime_empty_public_post: empty MIME on a public-post upload.
    let (base, token) = boot_test_nest().await;
    let sc = sidecar(AudienceClass::PublicPost, "", false);
    let bs = b"any bytes".to_vec();
    let resp = post_blob(&base, &token, sc, bs).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["error"],
        "ingest rejected: blob_mime_empty_public_post"
    );
}

// ── Sidecar present populates BlobIngestOutcome metadata (unit test on
//    SealedStorage directly — the integration test is in blob_api.rs's
//    download-with-thumbnail flow if/when that lands).

#[tokio::test]
async fn sealed_storage_blob_ingest_unit_populates_outcome_from_sidecar() {
    use fauna_nest::storage::BlobIngestItem;
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let s = SealedStorage::new(db, std::path::PathBuf::from("/tmp/test-acme-sidecar"));
    let bs = aead_shaped_bytes(0x47);
    let outcome = s
        .ingest_blob(&BlobIngestItem {
            uploader: [1u8; 32],
            body: &bs,
            sidecar: Some(UploadSidecar {
                class: AudienceClass::PublicPost,
                mime: "image/jpeg".to_string(),
                has_c2pa: true,
                thumbnail_hash: Some([2u8; 32]),
            }),
        })
        .await
        .unwrap();
    assert_eq!(outcome.mime, "image/jpeg");
    assert_eq!(outcome.has_c2pa, Some(true));
    // The nest never renders the thumbnail server-side — only the
    // declarative sidecar hash travels through. The companion sealed
    // thumbnail blob is uploaded by the client separately.
    assert_eq!(outcome.thumbnail_hash, Some([2u8; 32]));
}
