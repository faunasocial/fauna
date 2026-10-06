use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

fn build_test_state(
    db: Arc<CacheDb>,
    backup_svc: Arc<BackupService>,
) -> (Arc<fauna_nest::routes::AppState>, String) {
    let token_store = Arc::new(TokenStore::new());

    let kp = ActorKeypair::generate();
    // We need to register user synchronously before starting server,
    // but create_user is async. We'll do it in each test instead.

    let state = Arc::new(fauna_nest::routes::AppState {
        backup_service: Some(backup_svc),
        auth: fauna_nest::state::AuthState {
            token_store: token_store.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    // We'll create the token in each test since we need async
    let _ = kp; // suppress warning
    (state, String::new())
}

async fn start_test_server_with_token(
    state: Arc<fauna_nest::routes::AppState>,
) -> (std::net::SocketAddr, String) {
    // Create a registered user + token
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
    (addr, token)
}

#[tokio::test]
async fn chunk_upload_and_check() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db, backup_svc);
    let (addr, token) = start_test_server_with_token(state).await;

    let client = reqwest::Client::new();
    let chunk_data = b"test chunk data for upload";

    // Upload a chunk
    let resp = client
        .post(format!("http://{addr}/api/v1/chunks"))
        .header("authorization", format!("Bearer {token}"))
        .body(chunk_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let hash_hex = body["hash"].as_str().unwrap().to_string();

    // Download the chunk (public, no auth needed)
    let resp = client
        .get(format!("http://{addr}/api/v1/chunks/{hash_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), chunk_data);

    // Check chunks: one present, one missing
    let fake_hash = "bb".repeat(32);
    let resp = client
        .post(format!("http://{addr}/api/v1/chunks/check"))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "hashes": [hash_hex, fake_hash] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let missing = body["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].as_str().unwrap(), fake_hash);
}

#[tokio::test]
async fn manifest_upload_and_fetch() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db, backup_svc);
    let (addr, token) = start_test_server_with_token(state).await;

    let client = reqwest::Client::new();
    // The route accepts canonical `ChunkManifest` bytes only — exactly what every
    // production writer sends (shared Rust's `serialize_manifest`).
    let manifest_data = canonical_manifest_bytes(b"a chunk");

    // Upload manifest
    let resp = client
        .post(format!("http://{addr}/api/v1/manifests"))
        .header("authorization", format!("Bearer {token}"))
        .body(manifest_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let hash_hex = body["hash"].as_str().unwrap().to_string();

    // Fetch manifest (public, no auth needed)
    let resp = client
        .get(format!("http://{addr}/api/v1/manifests/{hash_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), manifest_data.as_slice());
}

/// Canonical dag-cbor bytes of a real `ChunkManifest` over one chunk — what every
/// production writer POSTs to `/api/v1/manifests` (all five funnel through shared
/// Rust's `serialize_manifest` / `canonical_encode`).
fn canonical_manifest_bytes(chunk: &[u8]) -> Vec<u8> {
    let h = fauna_core::data::ContentHash::of_raw(chunk);
    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: h,
        total_size: chunk.len() as u64,
        chunk_hashes: vec![h],
        chunk_sizes: vec![chunk.len() as u64],
        stored_hashes: None,
        sealed_hashes: None,
        min_reader: None,
    };
    fauna_core::encoding::canonical_encode(&manifest).unwrap()
}

/// The manifest door: a body that is not a canonical `ChunkManifest` is refused.
///
/// Without this the route hashes arbitrary bytes and stamps them
/// `content_type = "manifest"` — so any authenticated client could make
/// `content_type` a lie and hand GC's reachability walk an undecodable
/// "manifest" it can only fail-close on (`backup-restore.md` § 9). Every
/// legitimate writer already sends canonical manifest bytes, so nothing real is
/// turned away.
#[tokio::test]
async fn manifest_upload_refuses_a_body_that_is_not_a_chunk_manifest() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db, backup_svc);
    let (addr, token) = start_test_server_with_token(state).await;

    let client = reqwest::Client::new();

    for (label, body) in [
        ("arbitrary bytes", b"manifest bytes content".to_vec()),
        ("empty body", Vec::new()),
        ("20 bytes of garbage", vec![0x9Au8; 20]),
    ] {
        let resp = client
            .post(format!("http://{addr}/api/v1/manifests"))
            .header("authorization", format!("Bearer {token}"))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "{label}: a non-manifest body must be refused at the door, not stored \
             and stamped `content_type=manifest`"
        );
    }

    // …and a genuine manifest is still accepted (the guard must not turn away the
    // traffic every real writer sends).
    let resp = client
        .post(format!("http://{addr}/api/v1/manifests"))
        .header("authorization", format!("Bearer {token}"))
        .body(canonical_manifest_bytes(b"a chunk"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "a canonical ChunkManifest is accepted");
}

/// Regression for the `encode_blob` self-describing fix. The live `BackupService`
/// config is `(None, false)`, so the nest stores blobs uncompressed + unencrypted.
/// A chunk/manifest body whose first byte is `0x00`/`0x01` must round-trip
/// byte-for-byte through `POST`+`GET`. The download handlers decode the nest's
/// at-rest framing, and `decode_blob ∘ encode_blob` is the identity — so the
/// client gets back exactly the bytes it uploaded regardless of leading byte.
///
/// Before the fix, `encode_blob(body, None, false)` stored the body un-prefixed
/// while `download_manifest` decoded it (`decompress_chunk`), mis-stripping a
/// `0x00`/`0x01` leading byte — this manifest case failed red.
///
/// The bug lived in `download_manifest`'s DECODE, so that is what must stay
/// pinned. Since the manifest *door* now refuses a non-manifest body (a canonical
/// `ChunkManifest` starts with a CBOR map header, never `0x00`/`0x01`, so the
/// colliding byte can no longer arrive that way), the colliding bytes are
/// uploaded through the chunk route — where arbitrary bytes are still legal — and
/// read back through the **manifest** route. Both routes read the one
/// content-addressed store, so this drives the identical `download_manifest`
/// decode path with the identical hostile prefix. The guard keeps its teeth.
#[tokio::test]
async fn round_trip_preserves_prefix_leading_bytes() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db, backup_svc);
    let (addr, token) = start_test_server_with_token(state).await;

    let client = reqwest::Client::new();

    // Bodies whose first byte collides with the compression prefix scheme
    // (0x00 = uncompressed, 0x01 = zstd), plus interior collisions.
    let zero_leading = vec![0x00u8, 0xAB, 0xCD, 0x00, 0x01, 0xFF];
    let one_leading = vec![0x01u8, 0x00, 0x10, 0x20];

    for body in [&zero_leading, &one_leading] {
        // Chunk: upload then download must be byte-identical.
        let resp = client
            .post(format!("http://{addr}/api/v1/chunks"))
            .header("authorization", format!("Bearer {token}"))
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
        let hash_hex = resp.json::<serde_json::Value>().await.unwrap()["hash"]
            .as_str()
            .unwrap()
            .to_string();
        let got = client
            .get(format!("http://{addr}/api/v1/chunks/{hash_hex}"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(
            got.as_ref(),
            body.as_slice(),
            "chunk with prefix-colliding leading byte must round-trip verbatim"
        );

        // Same property through the decode-on-download MANIFEST route: the blob is
        // already in the one content-addressed store (uploaded as a chunk above),
        // so fetching that same hash via `/api/v1/manifests/{hash}` exercises
        // `download_manifest`'s decode — the exact code path that had the bug —
        // with the exact hostile leading byte.
        let got = client
            .get(format!("http://{addr}/api/v1/manifests/{hash_hex}"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(
            got.as_ref(),
            body.as_slice(),
            "manifest-route download with a prefix-colliding leading byte must \
             round-trip verbatim"
        );
    }
}

/// **The blob store's THIRD and FOURTH doors honour the legal-takedown
/// withhold**.
///
/// `moderation.md` § Legal takedown → *The blob-serve door* withholds a taken-
/// down record's blobs from the **store**, and the withhold is a property of
/// the bytes rather than of one route to them. `GET /api/v1/chunks/{hash}` and
/// `GET /api/v1/manifests/{hash}` are mounted `public_bytes` — unauthenticated,
/// open CORS, "a ciphertext hash is the capability" — and read the very store
/// `/api/v1/blob/{id}` and `/api/v1/video/segments/{hash}` serve out of. So a
/// takedown that shut only those two was bypassed by a path-prefix
/// substitution on the same hex digest the requester already holds: 451 at two
/// paths, 200 at the other two.
///
/// This asserts the same digests through both of the doors this closes: 451
/// from each, no body, and 200 again once the flag is lifted. The restore half
/// is not decoration — tombstone-not-delete keeps the GC pin flag-blind, so
/// the bytes must still be on the box, and a gate that deleted or evicted them
/// would pass the withhold assertion and fail here.
#[tokio::test]
async fn a_withheld_blob_is_451_at_the_chunk_and_manifest_doors_too() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db.clone(), backup_svc);
    let (addr, token) = start_test_server_with_token(state).await;

    let client = reqwest::Client::new();

    let chunk_data = b"the compelled bytes, served at a third path".to_vec();
    let resp = client
        .post(format!("http://{addr}/api/v1/chunks"))
        .header("authorization", format!("Bearer {token}"))
        .body(chunk_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let chunk_hex = resp.json::<serde_json::Value>().await.unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();

    let manifest_data = canonical_manifest_bytes(&chunk_data);
    let resp = client
        .post(format!("http://{addr}/api/v1/manifests"))
        .header("authorization", format!("Bearer {token}"))
        .body(manifest_data.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let manifest_hex = resp.json::<serde_json::Value>().await.unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();

    let chunk_url = format!("http://{addr}/api/v1/chunks/{chunk_hex}");
    let manifest_url = format!("http://{addr}/api/v1/manifests/{manifest_hex}");

    // Before the takedown both doors serve.
    assert_eq!(client.get(&chunk_url).send().await.unwrap().status(), 200);
    assert_eq!(
        client.get(&manifest_url).send().await.unwrap().status(),
        200
    );

    // The takedown withholds both digests — a video post's manifest and every
    // segment hash are in the withheld set together (`Post::blob_refs`), so
    // this is the shape a real takedown produces, not a contrived one.
    let chunk_digest: [u8; 32] = hex::decode(&chunk_hex).unwrap().try_into().unwrap();
    let manifest_digest: [u8; 32] = hex::decode(&manifest_hex).unwrap().try_into().unwrap();
    db.replace_blob_legal_withhold(&[chunk_digest, manifest_digest])
        .await
        .unwrap();

    let resp = client.get(&chunk_url).send().await.unwrap();
    assert_eq!(
        resp.status(),
        451,
        "the chunk door must answer 451 for a withheld digest — a takedown \
         that shuts /api/v1/blob and /api/v1/video/segments is bypassed by \
         asking the same hash at /api/v1/chunks"
    );
    assert!(
        resp.bytes().await.unwrap().is_empty(),
        "451 carries no body: the compelled bytes must not ride the refusal"
    );

    let resp = client.get(&manifest_url).send().await.unwrap();
    assert_eq!(
        resp.status(),
        451,
        "and the manifest door too — a video post's manifest hash is in the \
         withheld set in its own right"
    );
    assert!(
        resp.bytes().await.unwrap().is_empty(),
        "451 carries no body at the manifest door either"
    );

    // Restore re-serves the very same bytes through both doors: the GC pin is
    // deliberately flag-blind, so nothing was deleted.
    db.replace_blob_legal_withhold(&[]).await.unwrap();
    let resp = client.get(&chunk_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.bytes().await.unwrap().as_ref(),
        chunk_data.as_slice(),
        "restore=true re-serves the same chunk, byte for byte"
    );
    let resp = client.get(&manifest_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.bytes().await.unwrap().as_ref(),
        manifest_data.as_slice(),
        "and the same manifest, byte for byte"
    );
}

/// **The withhold precedes the store-miss relay arm**, which is what stops the
/// third door from *re-importing* compelled bytes.
///
/// A chunk absent locally, asked for with a folder hint, becomes the phase-5
/// relay read: the nest pulls the bytes from a seat that announced the folder
/// and — for a full-residency folder — `store.put`s them back
/// (`chunk_relay`, `RelayCache::Store`). So a gate placed after the local
/// read would let this door pull a taken-down chunk back onto a box that no
/// longer had it: the takedown running backwards behind a
/// bearer.
///
/// Both assertions distinguish the ORDERING rather than merely observing a
/// refusal. A withheld digest this box does not hold answers **404** if the
/// gate runs after the store read, and **401** if the relay arm is entered
/// without a bearer — 451 to both is only reachable by gating first.
#[tokio::test]
async fn a_withheld_chunk_is_never_relayed_from_a_seat() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db.clone(), backup_svc);
    let (addr, _token) = start_test_server_with_token(state).await;

    // A digest this box does not hold, that IS withheld.
    let digest = [0x5au8; 32];
    let hash_hex = hex::encode(digest);
    db.replace_blob_legal_withhold(&[digest]).await.unwrap();

    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/api/v1/chunks/{hash_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        451,
        "a withheld digest answers 451 even when the store does not hold it — \
         a 404 here would mean the gate ran AFTER the store read"
    );

    let resp = client
        .get(format!(
            "http://{addr}/api/v1/chunks/{hash_hex}?folder=anything"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        451,
        "and 451 with a folder hint and no bearer — a 401 here would mean the \
         handler entered the relay arm first, i.e. it was willing to go and \
         fetch the compelled bytes back from a seat"
    );
}

/// A decompression bomb stored through the raw video-segment writer must not
/// expand past the blob store's decode bound when the anonymous chunk door
/// serves it.
///
/// `POST /api/v1/video/segments` stores the body verbatim, so the client's
/// first byte becomes the at-rest prefix: a `0x01` ‖ zstd body is, to
/// `decode_blob`, a compressed blob. A few kilobytes of zstd expand to 64 MiB
/// of zeros; unbounded, the GET materialised all of it (and a 2 MB upload
/// asked for ~64 GB — an OOM kill of the whole nest from one registered user
/// plus one anonymous request). Bounded, the read refuses before allocating
/// past `MAX_DECODED_BLOB`.
#[tokio::test]
async fn a_zstd_bomb_video_segment_is_refused_at_the_chunk_door() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    let (state, _) = build_test_state(db, backup_svc);
    let (addr, token) = start_test_server_with_token(state).await;

    let expanded = 4 * fauna_nest::backup::MAX_DECODED_BLOB;
    let mut bomb = vec![0x01u8];
    bomb.extend(zstd::encode_all(&vec![0u8; expanded][..], 19).unwrap());
    assert!(bomb.len() < 64 * 1024, "the bomb is a few KB on the wire");

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/api/v1/video/segments"))
        .header("authorization", format!("Bearer {token}"))
        .body(bomb.clone())
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "segment upload: {}",
        resp.status()
    );
    let hash_hex = hex::encode(blake3::hash(&bomb).as_bytes());

    let resp = client
        .get(format!("http://{addr}/api/v1/chunks/{hash_hex}"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.bytes().await.unwrap();
    assert!(
        !status.is_success(),
        "the chunk door served a {}-byte expansion of a {}-byte upload",
        body.len(),
        bomb.len()
    );
    assert!(body.len() < 1024, "the refusal carries no expansion");
}
