//! Integration tests for the CBOR-DAG-everywhere blob endpoint
//! (`PUT|GET /api/v1/blob/{cid_b32}`), per Layer 3 Task 3.5.
//!
//! Sibling to `blob_api.rs` which covers the legacy hex-keyed
//! `POST /api/v1/blob` + `GET /api/v1/blob/{hash}` shape.

use std::sync::Arc;

use fauna_cbor::Cid;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

/// Spin up a real nest instance with bearer auth, return
/// `(addr, token, _tempdir_keepalive)`. The caller must keep the tempdir
/// binding alive for the duration of the test — dropping it would unlink
/// the on-disk blob store the spawned server still holds.
async fn spawn_nest() -> (std::net::SocketAddr, String, tempfile::TempDir) {
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

    (addr, token, dir)
}

/// Build a tiny canonical dag-cbor block for fixture content (`{"hello":
/// "world"}` encoded canonically), so the test exercises real wire bytes
/// rather than random noise.
fn fixture_dag_cbor() -> Vec<u8> {
    let mut map = std::collections::BTreeMap::new();
    map.insert("hello".to_string(), "world".to_string());
    fauna_cbor::encode_canonical(&map).expect("canonical encode")
}

#[tokio::test]
async fn put_then_get_round_trips_bytes() {
    let (addr, token, _dir) = spawn_nest().await;
    let client = reqwest::Client::new();
    let bytes = fixture_dag_cbor();
    let cid = Cid::of_dag_cbor(&bytes);
    let cid_b32 = cid.to_base32();

    let resp = client
        .put(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .header("authorization", format!("Bearer {token}"))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "PUT body: {:?}", resp.text().await);

    let resp = client
        .get(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(etag, format!("\"{cid_b32}\""));
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), bytes.as_slice());
}

#[tokio::test]
async fn put_wrong_cid_in_url_rejected_with_400() {
    let (addr, token, _dir) = spawn_nest().await;
    let client = reqwest::Client::new();

    let body_a = fixture_dag_cbor();
    let body_b = b"completely different bytes".to_vec();
    let cid_b = Cid::of_dag_cbor(&body_b);
    let cid_b_str = cid_b.to_base32();

    // PUT body_a at URL of cid_b → mismatch.
    let resp = client
        .put(format!("http://{addr}/api/v1/blob/{cid_b_str}"))
        .header("authorization", format!("Bearer {token}"))
        .body(body_a.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "cid_mismatch");
    assert_eq!(body["url_cid"], cid_b_str);
    assert_eq!(body["body_cid"], Cid::of_dag_cbor(&body_a).to_base32());
}

#[tokio::test]
async fn put_then_put_same_cid_idempotent() {
    let (addr, token, _dir) = spawn_nest().await;
    let client = reqwest::Client::new();
    let bytes = fixture_dag_cbor();
    let cid_b32 = Cid::of_dag_cbor(&bytes).to_base32();

    let resp = client
        .put(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .header("authorization", format!("Bearer {token}"))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "created");

    let resp = client
        .put(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .header("authorization", format!("Bearer {token}"))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "exists");
}

#[tokio::test]
async fn get_unknown_cid_returns_404() {
    let (addr, _token, _dir) = spawn_nest().await;
    let client = reqwest::Client::new();
    // Valid CID shape, never PUT.
    let cid_b32 = Cid::of_dag_cbor(b"never-uploaded").to_base32();

    let resp = client
        .get(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
    assert_eq!(body["cid"], cid_b32);
}

#[tokio::test]
async fn get_returns_octet_stream_content_type() {
    let (addr, token, _dir) = spawn_nest().await;
    let client = reqwest::Client::new();
    let bytes = fixture_dag_cbor();
    let cid_b32 = Cid::of_dag_cbor(&bytes).to_base32();

    client
        .put(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .header("authorization", format!("Bearer {token}"))
        .body(bytes)
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!("http://{addr}/api/v1/blob/{cid_b32}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "application/octet-stream");
}

#[tokio::test]
async fn put_with_malformed_url_cid_returns_400() {
    let (addr, token, _dir) = spawn_nest().await;
    let client = reqwest::Client::new();

    // `not-a-valid-base32` starts with `n`, not a multibase prefix → parse fails.
    let resp = client
        .put(format!("http://{addr}/api/v1/blob/not-a-valid-base32"))
        .header("authorization", format!("Bearer {token}"))
        .body(b"any bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "bad_cid");
    assert_eq!(body["raw"], "not-a-valid-base32");
}
