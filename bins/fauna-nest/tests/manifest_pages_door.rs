//! The paged-manifest doors (`file-sync.md` § Paged manifests → *Transport
//! and order*): `POST /api/v1/manifest-pages` accepts a canonical page and
//! nothing else, and `POST /api/v1/manifests` accepts a paged root only once
//! its pages are at rest and hold what the root names.

use std::sync::Arc;

use fauna_core::chunk::ChunkManifest;
use fauna_core::data::ContentHash;
use fauna_core::identity::ActorKeypair;
use fauna_core::manifest_paging::{MANIFEST_PAGE_MAX, ManifestPage, PagingRule};
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

async fn start() -> (std::net::SocketAddr, String, tempfile::TempDir) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let state = Arc::new(fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(TokenStore::new()),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db)
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
    (addr, token, dir)
}

async fn post(addr: std::net::SocketAddr, token: &str, path: &str, body: Vec<u8>) -> (u16, String) {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

/// A plaintext file of a dozen chunks, written paged under the small rule:
/// `(root bytes, page bytes)`.
fn paged_file() -> (Vec<u8>, Vec<Vec<u8>>) {
    let content = vec![0x42u8; 192];
    let pieces: Vec<&[u8]> = content.chunks(16).collect();
    let manifest = ChunkManifest {
        file_hash: ContentHash::of_raw(&content),
        total_size: content.len() as u64,
        chunk_hashes: pieces.iter().map(|p| ContentHash::of_raw(p)).collect(),
        chunk_sizes: pieces.iter().map(|p| p.len() as u64).collect(),
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let (root, pages) = manifest
        .into_wire_parts_with(&PagingRule::with_thresholds(4, 2, 3, 1), None)
        .unwrap();
    assert!(pages.len() >= 3);
    (root, pages)
}

/// The page door accepts a canonical page — fetched back, like a root, from
/// `GET /api/v1/manifests/{hash}` — and refuses junk, a page over the entry
/// bound and a manifest body.
#[tokio::test]
async fn the_page_door_accepts_a_page_and_refuses_everything_else() {
    let (addr, token, _dir) = start().await;
    let (_, pages) = paged_file();

    let (status, body) = post(addr, &token, "/api/v1/manifest-pages", pages[0].clone()).await;
    assert_eq!(status, 201, "a canonical page is stored: {body}");
    let hash: serde_json::Value = serde_json::from_str(&body).unwrap();
    let hash = hash["hash"].as_str().unwrap();
    assert_eq!(hash, hex::encode(ContentHash::of_raw(&pages[0]).digest()));
    let got = reqwest::get(format!("http://{addr}/api/v1/manifests/{hash}"))
        .await
        .unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(got.bytes().await.unwrap().as_ref(), pages[0].as_slice());

    let h = ContentHash::of_raw(b"x");
    let oversize = ManifestPage {
        chunk_hashes: vec![h; MANIFEST_PAGE_MAX + 1],
        chunk_sizes: vec![1; MANIFEST_PAGE_MAX + 1],
        stored_hashes: None,
        sealed_hashes: None,
    };
    let single = ChunkManifest {
        file_hash: h,
        total_size: 1,
        chunk_hashes: vec![h],
        chunk_sizes: vec![1],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
        pages: None,
    };
    let (root, _) = paged_file();
    for (label, body) in [
        ("junk", b"not a page at all".to_vec()),
        ("an empty body", Vec::new()),
        (
            "a page over the entry bound",
            fauna_core::encoding::canonical_encode(&oversize).unwrap(),
        ),
        (
            "a ChunkManifest body",
            fauna_core::encoding::canonical_encode(&single).unwrap(),
        ),
        ("a paged root", root),
    ] {
        let (status, _) = post(addr, &token, "/api/v1/manifest-pages", body).await;
        assert_eq!(status, 400, "{label} is refused at the page door");
    }
}

/// The root door: a root goes up after its pages. Ahead of them it is
/// refused (garbage collection would otherwise meet an unresolved page and
/// fail closed for every account), as is a root carrying chunk entries of its
/// own; with its pages at rest it is stored.
#[tokio::test]
async fn the_root_door_takes_a_root_only_after_its_pages() {
    let (addr, token, _dir) = start().await;
    let (root, pages) = paged_file();

    let (status, body) = post(addr, &token, "/api/v1/manifests", root.clone()).await;
    assert_eq!(status, 400, "a root ahead of its pages is refused: {body}");

    let mut mixed: ChunkManifest = fauna_core::encoding::canonical_decode(&root).unwrap();
    mixed.chunk_hashes = vec![ContentHash::of_raw(b"x")];
    mixed.chunk_sizes = vec![1];
    let (status, _) = post(
        addr,
        &token,
        "/api/v1/manifests",
        fauna_core::encoding::canonical_encode(&mixed).unwrap(),
    )
    .await;
    assert_eq!(
        status, 400,
        "a root with chunk entries of its own is refused"
    );

    for page in &pages {
        let (status, body) = post(addr, &token, "/api/v1/manifest-pages", page.clone()).await;
        assert_eq!(status, 201, "{body}");
    }
    let (status, body) = post(addr, &token, "/api/v1/manifests", root.clone()).await;
    assert_eq!(
        status, 201,
        "a root whose pages are at rest is stored: {body}"
    );
}
