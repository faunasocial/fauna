use std::sync::Arc;

use fauna_core::data::{ContentHash, Post, PostBody, Timestamp, VideoSegment};
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::token_store::TokenStore;

fn build_test_state() -> (Arc<fauna_nest::routes::AppState>, Arc<TokenStore>) {
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

    (state, token_store)
}

#[tokio::test]
async fn upload_and_retrieve_video_segment() {
    let (state, token_store) = build_test_state();
    let db = state.db.clone();

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
    let segment_data = b"\x47\x40\x11\x10\x00\x42\xf0\x25video segment payload for test";

    // Upload segment
    let resp = client
        .post(format!("http://{addr}/api/v1/video/segments"))
        .header("authorization", format!("Bearer {token}"))
        .body(segment_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = resp.json().await.unwrap();
    let hash_hex = body["hash"].as_str().unwrap().to_string();
    let size = body["size"].as_u64().unwrap();
    assert_eq!(size, segment_data.len() as u64);
    assert_eq!(hash_hex.len(), 64); // 32 bytes hex-encoded

    // Download segment (public, no auth needed)
    let resp = client
        .get(format!("http://{addr}/api/v1/video/segments/{hash_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "video/mp2t"
    );
    let got = resp.bytes().await.unwrap();
    assert_eq!(got.as_ref(), segment_data);
}

#[tokio::test]
async fn retrieve_nonexistent_segment_returns_404() {
    let (state, _token_store) = build_test_state();

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let client = reqwest::Client::new();
    let fake_hash = "aa".repeat(32);
    let resp = client
        .get(format!("http://{addr}/api/v1/video/segments/{fake_hash}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// A video post on a live nest, with its author's token and a spawned server —
/// the shared setup behind the manifest tests (the green serve pin and the four
/// withholding witnesses below).
struct VideoPostFixture {
    state: Arc<fauna_nest::routes::AppState>,
    token_store: Arc<TokenStore>,
    author_token: String,
    addr: std::net::SocketAddr,
    client: reqwest::Client,
    post_id: [u8; 32],
    post_id_hex: String,
}

impl VideoPostFixture {
    fn master_url(&self) -> String {
        let (addr, id) = (self.addr, &self.post_id_hex);
        format!("http://{addr}/api/v1/video/{id}/master.m3u8")
    }

    fn variant_url(&self) -> String {
        let (addr, id) = (self.addr, &self.post_id_hex);
        format!("http://{addr}/api/v1/video/{id}/720.m3u8")
    }

    /// A second registered actor who is neither the author nor an admin.
    async fn stranger_token(&self) -> String {
        let kp = ActorKeypair::generate();
        self.state
            .db
            .create_user(&kp.actor_id().0, "free", "stranger")
            .await
            .unwrap();
        self.token_store.insert(kp.actor_id(), 3600).await
    }

    async fn get(&self, url: &str, token: Option<&str>) -> reqwest::Response {
        let mut req = self.client.get(url);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        req.send().await.unwrap()
    }
}

async fn video_post_fixture() -> VideoPostFixture {
    let (state, token_store) = build_test_state();
    let db = state.db.clone();

    // Create a registered user + token for auth (needed for segment upload)
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let token = token_store.insert(kp.actor_id(), 3600).await;

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let client = reqwest::Client::new();

    // Upload two fake segments (different resolutions)
    let seg720_data = b"fake 720p segment data";
    let resp = client
        .post(format!("http://{addr}/api/v1/video/segments"))
        .header("authorization", format!("Bearer {token}"))
        .body(seg720_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body720: serde_json::Value = resp.json().await.unwrap();
    let hash720_hex = body720["hash"].as_str().unwrap().to_string();

    let seg360_data = b"fake 360p segment data";
    let resp = client
        .post(format!("http://{addr}/api/v1/video/segments"))
        .header("authorization", format!("Bearer {token}"))
        .body(seg360_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body360: serde_json::Value = resp.json().await.unwrap();
    let hash360_hex = body360["hash"].as_str().unwrap().to_string();

    // Build content hashes from the uploaded segment hashes
    let hash720_bytes: [u8; 32] = hex::decode(&hash720_hex).unwrap().try_into().unwrap();
    let hash360_bytes: [u8; 32] = hex::decode(&hash360_hex).unwrap().try_into().unwrap();

    // Create a video post with segments at two resolutions
    let post = Post {
        author: kp.actor_id(),
        created_at: Timestamp(1_000_000),
        body: PostBody::Video {
            manifest: ContentHash::from_digest_raw([0u8; 32]),
            segments: vec![
                VideoSegment {
                    hash: ContentHash::from_digest_raw(hash720_bytes),
                    resolution: 720,
                    codec: "h264".into(),
                    bitrate: 2500,
                    byte_size: seg720_data.len() as u64,
                },
                VideoSegment {
                    hash: ContentHash::from_digest_raw(hash360_bytes),
                    resolution: 360,
                    codec: "h264".into(),
                    bitrate: 800,
                    byte_size: seg360_data.len() as u64,
                },
            ],
            thumbnail: ContentHash::from_digest_raw([0u8; 32]),
            duration_ms: 10_000,
            aspect_ratio: (16, 9),
            anchors: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    let post_bytes = fauna_core::encoding::sign_and_pack(&kp, &post).unwrap();
    let post_id = fauna_core::encoding::compute_post_id(&post).unwrap();
    let post_id_bytes: [u8; 32] = {
        let b = post_id.as_bytes();
        let mut d = [0u8; 32];
        d.copy_from_slice(&b[4..]);
        d
    };
    db.put_post(&post_id_bytes, &post_bytes, None)
        .await
        .unwrap();

    VideoPostFixture {
        state,
        token_store,
        author_token: token,
        addr,
        client,
        post_id: post_id_bytes,
        post_id_hex: hex::encode(post_id_bytes),
    }
}

#[tokio::test]
async fn serve_master_manifest_for_video_post() {
    let f = video_post_fixture().await;

    // GET the master manifest
    let resp = f.get(&f.master_url(), None).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/vnd.apple.mpegurl"
    );
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("#EXTM3U"), "should start with #EXTM3U");
    assert!(body.contains("720.m3u8"), "should contain 720.m3u8");
    assert!(body.contains("360.m3u8"), "should contain 360.m3u8");
}

// ── the two post-read gates, on both manifest doors ────────────────────
//
// `moderation.md` § Legal takedown → *Posts* binds every path that serves post
// body content into the every-viewer withholding rule, and the blob-door ruling
// (§ Legal takedown, 2026-09-09) states the body "is blanked at the read
// primitive every per-record serve path flows through". Until these witnesses
// the manifest handlers were the counterexample: they read through the
// deliberately flag-blind `load_post_body` and answered 200 with the whole
// playlist — every variant, every segment duration, an absolute URL per segment
// — for a post that was taken down or quarantined.
//
// Four witnesses, two doors × two gates. Each is independently load-bearing:
// reverting the takedown arm reddens the two takedown witnesses only, reverting
// the quarantine arm reddens the two quarantine ones, and reverting
// `serve_variant_manifest` alone (the "fixed master.m3u8, left {variant}.m3u8
// serving" mistake) reddens the two variant ones — three distinct signatures, so
// no witness is covered by another.

/// A taken-down post's master playlist is withheld from **every** viewer,
/// including its author — the same rule `get_post_core` applies to the body, and
/// the same 451-no-body answer `GET /api/v1/posts/{id}` and the blob door give.
#[tokio::test]
async fn a_taken_down_video_posts_master_manifest_is_withheld_from_every_viewer() {
    let f = video_post_fixture().await;
    f.state
        .db
        .set_post_legal_takedown(&f.post_id, Some("court-order-1"))
        .await
        .unwrap();

    let anon = f.get(&f.master_url(), None).await;
    assert_eq!(
        anon.status(),
        451,
        "an anonymous caller gets 451 Unavailable For Legal Reasons"
    );
    assert!(
        anon.text().await.unwrap().is_empty(),
        "no body — a playlist has no tombstone render of its own"
    );

    let author = f.get(&f.master_url(), Some(&f.author_token)).await;
    assert_eq!(
        author.status(),
        451,
        "and the author too — a takedown withholds from every viewer"
    );
}

/// The variant playlist is a **separate handler** with its own copy of the
/// load-decode sequence, so it needs its own witness: a fix applied to
/// `master.m3u8` alone would leave `{variant}.m3u8` serving.
#[tokio::test]
async fn a_taken_down_video_posts_variant_manifest_is_withheld_from_every_viewer() {
    let f = video_post_fixture().await;
    f.state
        .db
        .set_post_legal_takedown(&f.post_id, Some("court-order-1"))
        .await
        .unwrap();

    let anon = f.get(&f.variant_url(), None).await;
    assert_eq!(anon.status(), 451);
    assert!(anon.text().await.unwrap().is_empty());

    let author = f.get(&f.variant_url(), Some(&f.author_token)).await;
    assert_eq!(author.status(), 451);
}

/// Quarantine is the *other* gate and it points the other way: author/admin
/// only, everyone else 404. The author's 200 is half the assertion — it proves
/// the route is gated rather than simply broken.
#[tokio::test]
async fn a_quarantined_video_posts_master_manifest_is_author_only() {
    let f = video_post_fixture().await;
    f.state
        .db
        .set_post_quarantined(&f.post_id, true)
        .await
        .unwrap();

    assert_eq!(
        f.get(&f.master_url(), None).await.status(),
        404,
        "quarantine hides existence from an anonymous caller"
    );
    let stranger = f.stranger_token().await;
    assert_eq!(
        f.get(&f.master_url(), Some(&stranger)).await.status(),
        404,
        "and from a signed-in stranger"
    );
    assert_eq!(
        f.get(&f.master_url(), Some(&f.author_token)).await.status(),
        200,
        "the author still sees their own quarantined post's playlist"
    );
}

/// The variant door's quarantine arm — the second half of the separate-handler
/// point above.
#[tokio::test]
async fn a_quarantined_video_posts_variant_manifest_is_author_only() {
    let f = video_post_fixture().await;
    f.state
        .db
        .set_post_quarantined(&f.post_id, true)
        .await
        .unwrap();

    assert_eq!(f.get(&f.variant_url(), None).await.status(), 404);
    let stranger = f.stranger_token().await;
    assert_eq!(f.get(&f.variant_url(), Some(&stranger)).await.status(), 404);
    assert_eq!(
        f.get(&f.variant_url(), Some(&f.author_token))
            .await
            .status(),
        200,
        "the author still sees their own quarantined post's variant playlist"
    );
}

/// **The blob store's SECOND door honours the legal-takedown withhold** —
/// security review.
///
/// `moderation.md` § Legal takedown → *The blob-serve door* withholds a taken-
/// down record's blobs from the store, and a video post's manifest, thumbnail
/// and every segment hash are in that set (`Post::blob_refs`). The withhold is
/// a property of the bytes, not of one route to them — so a takedown that only
/// shut `/api/v1/blob/<hex>` was bypassed by a path-prefix substitution on the
/// very hex digest the requester already holds.
///
/// This asserts the same digest through both doors: 451 from each, no body,
/// and 200 again from each once the flag is lifted. The restore half is not
/// decoration — tombstone-not-delete means the GC pin stays flag-blind, so the
/// bytes must still be on the box, and a gate that deleted or evicted them
/// would pass the withhold assertion and fail here.
#[tokio::test]
async fn a_withheld_video_segment_is_451_at_the_segment_door_too() {
    let (state, token_store) = build_test_state();
    let db = state.db.clone();

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
    let segment_data = b"\x47\x40\x11\x10taken-down video segment payload";
    let resp = client
        .post(format!("http://{addr}/api/v1/video/segments"))
        .header("authorization", format!("Bearer {token}"))
        .body(segment_data.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let hash_hex = body["hash"].as_str().unwrap().to_string();
    let digest: [u8; 32] = hex::decode(&hash_hex).unwrap().try_into().unwrap();

    let segment_url = format!("http://{addr}/api/v1/video/segments/{hash_hex}");
    let blob_url = format!("http://{addr}/api/v1/blob/{hash_hex}");

    // Before the takedown both doors serve it.
    assert_eq!(client.get(&segment_url).send().await.unwrap().status(), 200);
    assert_eq!(client.get(&blob_url).send().await.unwrap().status(), 200);

    // The takedown withholds the digest — the state the flag handler and every
    // complete GC sweep rebuild wholesale.
    db.replace_blob_legal_withhold(&[digest]).await.unwrap();

    let resp = client.get(&segment_url).send().await.unwrap();
    assert_eq!(
        resp.status(),
        451,
        "the segment door must answer 451 for a withheld digest — a takedown \
         that only shuts /api/v1/blob is bypassed by asking the same hash at \
         /api/v1/video/segments"
    );
    assert!(
        resp.bytes().await.unwrap().is_empty(),
        "451 carries no body: the compelled bytes must not ride the refusal"
    );
    assert_eq!(
        client.get(&blob_url).send().await.unwrap().status(),
        451,
        "and the door the rule was written for still answers 451"
    );

    // Restore re-serves the very same bytes through both doors: the GC pin is
    // deliberately flag-blind, so nothing was deleted.
    db.replace_blob_legal_withhold(&[]).await.unwrap();
    let resp = client.get(&segment_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.bytes().await.unwrap().as_ref(),
        segment_data,
        "restore=true re-serves the same segment, byte for byte"
    );
    assert_eq!(client.get(&blob_url).send().await.unwrap().status(), 200);
}

/// **The withhold precedes the cache-on-fetch arm**, which is what stops the
/// second door from *re-importing* compelled bytes.
///
/// A segment absent locally is fetched from a delivery-source peer, BLAKE3-
/// verified and `store.put` back before serving. So a gate placed after the
/// local read would still pull a taken-down segment onto a box that no longer
/// had it — the takedown running backwards.
///
/// The peer here is a dead address: if the gate fires first, nothing dials it
/// and the answer is 451. If the gate did not fire, the handler would walk the
/// source list and answer 404 after failing to reach it — so the assertion
/// distinguishes the two orderings rather than merely observing a refusal.
#[tokio::test]
async fn a_withheld_segment_is_never_re_imported_from_a_peer() {
    let (state, _token_store) = build_test_state();
    let db = state.db.clone();

    // A digest this box does not hold, that IS withheld, and that has a
    // delivery source recorded for it.
    let digest = [0x5au8; 32];
    let hash_hex = hex::encode(digest);
    db.replace_blob_legal_withhold(&[digest]).await.unwrap();
    db.record_delivery_receipt(&digest, "http://127.0.0.1:1", 1, 0)
        .await
        .unwrap();

    let app = fauna_nest::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/api/v1/video/segments/{hash_hex}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        451,
        "a withheld segment answers 451 without consulting a peer at all — a \
         404 here would mean the gate ran AFTER the cache-on-fetch arm, i.e. \
         the handler tried to pull the compelled bytes back first"
    );
}

// ── both segment writers are doors into the blob sweep's world ─────
//
// Every blob-store write owes the sweep its `blob_metadata` row, because that
// table is the sweep's world (`backup-restore.md` § 9, the sweep's premise). The segment upload and the cache-on-fetch arm wrote
// none, so an upload no post ever named — open to every authenticated user —
// rested on disk permanently, outside quota and outside GC. These pins drive
// each door on a real router and grade it with a real zero-grace sweep.

/// One real GC run with ZERO grace over `state`'s blob store: every blob the
/// sweep's oracle does not reference is deleted, however new.
async fn zero_grace_gc(state: &Arc<fauna_nest::routes::AppState>) {
    let backup_svc = state.backup_service.as_ref().expect("backup service");
    fauna_nest::backup::gc::garbage_collect(
        &state.db,
        &backup_svc.local_blob_store(),
        fauna_nest::backup::gc::PostBodySource {
            segments: &state.post_segments,
        },
        0,
        backup_svc.encryption_key(),
        false,
    )
    .await
    .expect("gc sweep");
}

/// A nest on a real socket, with a registered user's bearer.
async fn serving_nest() -> (
    Arc<fauna_nest::routes::AppState>,
    std::net::SocketAddr,
    String,
) {
    let (state, token_store) = build_test_state();
    let kp = ActorKeypair::generate();
    state
        .db
        .create_user(&kp.actor_id().0, "free", "test")
        .await
        .unwrap();
    let token = token_store.insert(kp.actor_id(), 3600).await;
    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (state, addr, token)
}

/// POST `bytes` to `addr`'s segment door and return the 32-byte hash it ACKed.
async fn upload(addr: std::net::SocketAddr, token: &str, bytes: &[u8]) -> [u8; 32] {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/video/segments"))
        .header("authorization", format!("Bearer {token}"))
        .body(bytes.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the upload must land");
    let body: serde_json::Value = resp.json().await.unwrap();
    hex::decode(body["hash"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

/// **An uploaded segment no post names is collected** — the upload writes its
/// `blob_metadata` row, carrying the body's own length, before its 201.
#[tokio::test]
async fn an_unreferenced_video_segment_is_collected() {
    let (state, addr, token) = serving_nest().await;
    let bytes = b"\x47\x40\x11\x10 a segment no post will ever name";
    let hash = upload(addr, &token, bytes).await;

    let meta = state.db.get_blob_metadata(&hash).await.unwrap();
    assert_eq!(
        meta.as_ref().map(|m| m.size_bytes),
        Some(bytes.len() as i64),
        "the upload must record the segment's blob_metadata row, with its own length, \
         before the ACK"
    );
    let store = state.backup_service.as_ref().unwrap().local_blob_store();
    let content_hash = ContentHash::from_digest_raw(hash);
    assert!(
        store.exists(&content_hash).await.unwrap(),
        "the bytes must be on disk before the sweep, or this pin proves nothing"
    );

    zero_grace_gc(&state).await;
    assert!(
        !store.exists(&content_hash).await.unwrap(),
        "an unreferenced segment survived a zero-grace sweep — the upload stores bytes \
         that neither quota nor GC can reach"
    );
}

/// **A live video post's segments survive the sweep** — the other half of why
/// the upload's row is safe: the sweep's record walk pins every segment a
/// stored video post names (`Post::blob_refs`).
#[tokio::test]
async fn a_live_video_posts_segments_survive_the_sweep() {
    let f = video_post_fixture().await;
    zero_grace_gc(&f.state).await;
    for data in [
        &b"fake 720p segment data"[..],
        &b"fake 360p segment data"[..],
    ] {
        let hash_hex = hex::encode(blake3::hash(data).as_bytes());
        let resp = f
            .get(
                &format!("http://{}/api/v1/video/segments/{hash_hex}", f.addr),
                None,
            )
            .await;
        assert_eq!(
            resp.status(),
            200,
            "a segment the live post names must survive a zero-grace sweep"
        );
    }
}

/// **A segment cached from a peer is collectable once nothing here names it.**
/// Cache-on-fetch stores a missing segment it pulled from a delivery source;
/// the copy carries its `blob_metadata` row, so the sweep reclaims it — a cache
/// entry is re-fetchable, so reclaiming loses nothing — where before it rested
/// on disk for good.
#[tokio::test]
async fn a_cached_segment_is_collectable_once_nothing_names_it() {
    let (_peer, peer_addr, peer_token) = serving_nest().await;
    let bytes = b"\x47\x40\x11\x10 a segment the peer holds";
    let hash = upload(peer_addr, &peer_token, bytes).await;

    let (local, local_addr, _) = serving_nest().await;
    local
        .db
        .record_delivery_receipt(&hash, &format!("http://{peer_addr}"), 1, 0)
        .await
        .unwrap();
    let resp = reqwest::Client::new()
        .get(format!(
            "http://{local_addr}/api/v1/video/segments/{}",
            hex::encode(hash)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "cache-on-fetch serves the peer's segment"
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), &bytes[..]);

    let store = local.backup_service.as_ref().unwrap().local_blob_store();
    let content_hash = ContentHash::from_digest_raw(hash);
    assert!(
        store.exists(&content_hash).await.unwrap(),
        "precondition: the fetched segment was cached here"
    );
    assert_eq!(
        local
            .db
            .get_blob_metadata(&hash)
            .await
            .unwrap()
            .map(|m| m.size_bytes),
        Some(bytes.len() as i64),
        "the cached copy must carry its blob_metadata row, with its own length"
    );

    zero_grace_gc(&local).await;
    assert!(
        !store.exists(&content_hash).await.unwrap(),
        "a cached segment nothing on this box names must reclaim"
    );
}
