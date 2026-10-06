//! HTTP handlers for video segment upload/download.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::{AppState, parse_32_bytes as parse_hash};

/// POST /api/v1/video/segments -- Upload a video segment, returns its content hash.
pub async fn upload_segment(
    State(state): State<Arc<AppState>>,
    _bearer: crate::auth::BearerAuth,
    body: Bytes,
) -> impl IntoResponse {
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    let hash_bytes: [u8; 32] = *blake3::hash(&body).as_bytes();
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    if let Err(e) = store.put(&hash, &body).await {
        tracing::error!("video segment upload error: {e}");
        return ApiError::internal("storage error").into_response();
    }

    // The segment's `blob_metadata` row, before the ACK: that table is the blob
    // sweep's world, so bytes stored without one are neither collected nor
    // counted, for good — and this route is open to every authenticated user
    // (`backup-restore.md` § 9, the sweep's premise). Safe
    // to write because the sweep already pins every segment a live video post
    // names (step 2f walks `Post::blob_refs`), so an upload the post never
    // names reclaims past grace while a published one never does. Failed
    // rather than ACKed without a durable trace, as the blob routes do; a
    // content-addressed retry is idempotent.
    if let Err(e) = state
        .db
        .put_blob_metadata(&hash_bytes, body.len() as i64, "video/mp2t", None, None)
        .await
    {
        tracing::error!("video segment metadata error: {e}");
        return ApiError::internal("storage error").into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({ "hash": hex::encode(hash_bytes), "size": body.len() })),
    )
        .into_response()
}

/// GET /api/v1/video/segments/{hash} -- Download a video segment by content hash.
///
/// **Withheld digests answer 451 before anything else runs** — this is the
/// blob store's *second* door, and `moderation.md` § Legal takedown → *The
/// blob-serve door* withholds a taken-down record's blobs from the store, not
/// from one route onto it. A video post's manifest, thumbnail and every
/// segment hash are in that set (`Post::blob_refs`), so without this gate the
/// takedown was bypassed by a path-prefix substitution on the very hex digest
/// the requester already holds: `/api/v1/blob/<hex>` answered 451 and
/// `/api/v1/video/segments/<hex>` answered 200 for the same bytes.
///
/// The gate precedes the store read **and** the cache-on-fetch arm below, and
/// that ordering is the load-bearing half: cache-on-fetch pulls a missing
/// segment from a peer and `store.put`s it back, so a gate placed after it
/// would let the second door re-import compelled bytes onto a box that no
/// longer had them.
///
/// It shares `blob_routes::legal_takedown_gate` rather than restating the
/// posture: one named gate, two callers, so the answer shape cannot drift
/// between the doors. Tombstone-not-delete is untouched — nothing is deleted
/// here, and `restore=true` re-serves the same bytes.
pub async fn download_segment(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
) -> impl IntoResponse {
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    let hash_bytes = match parse_hash(&hash_hex) {
        Some(h) => h,
        None => return ApiError::bad_request("invalid hash hex").into_response(),
    };
    if let Some(withheld) = crate::blob_routes::legal_takedown_gate(&state.db, &hash_bytes).await {
        return withheld;
    }
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    match store.get(&hash).await {
        Ok(Some(data)) => {
            return axum::http::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "video/mp2t")
                .body(axum::body::Body::from(data))
                .unwrap()
                .into_response();
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!("video segment download error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    }

    // Segment not found locally — attempt cache-on-fetch from known source nests.
    let sources = match state.db.get_delivery_sources(&hash_bytes).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to look up delivery sources for {hash_hex}: {e}");
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    for source in &sources {
        let url = format!(
            "{}/api/v1/video/segments/{hash_hex}",
            source.nest_url.trim_end_matches('/')
        );
        tracing::debug!("cache-on-fetch: trying {url}");
        let resp = match state.http_client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cache-on-fetch GET {url} failed: {e}");
                continue;
            }
        };
        if !resp.status().is_success() {
            tracing::debug!("cache-on-fetch: {url} returned {}", resp.status());
            continue;
        }
        let data = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("cache-on-fetch: failed to read body from {url}: {e}");
                continue;
            }
        };
        // Verify BLAKE3 hash before caching.
        let fetched_hash: [u8; 32] = *blake3::hash(&data).as_bytes();
        if fetched_hash != hash_bytes {
            tracing::warn!(
                "cache-on-fetch: hash mismatch from {url} — expected {hash_hex}, got {}",
                hex::encode(fetched_hash)
            );
            continue;
        }
        // Cache locally — with the copy's `blob_metadata` row, so the blob sweep
        // can reclaim a cached segment nothing on this box names any more (the
        // sweep's premise, `backup-restore.md` § 9). A cache entry is
        // re-fetchable from its source, so collecting it loses nothing. A copy
        // whose row cannot be written is dropped again rather than left resting
        // outside the sweep's world; the fetched bytes are served either way.
        match store.put(&hash, &data).await {
            Ok(()) => {
                if let Err(e) = state
                    .db
                    .put_blob_metadata(&hash_bytes, data.len() as i64, "video/mp2t", None, None)
                    .await
                {
                    tracing::error!(
                        "cache-on-fetch: no metadata row for segment {hash_hex}, dropping \
                         the cached copy: {e}"
                    );
                    if let Err(e) = store.delete(&hash).await {
                        tracing::warn!("cache-on-fetch: dropping segment {hash_hex}: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::error!("cache-on-fetch: failed to store segment {hash_hex}: {e}")
            }
        }
        return axum::http::Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "video/mp2t")
            .body(axum::body::Body::from(data))
            .unwrap()
            .into_response();
    }

    StatusCode::NOT_FOUND.into_response()
}

/// Resolve a video post for a manifest handler, **through the same post-read
/// core `GET /api/v1/posts/{id}` and `fauna.posts.get` flow through**.
///
/// The manifest handlers used to call [`crate::segments::post::load_post_body`]
/// directly. That primitive is deliberately flag-blind — `moderation_withhold`
/// reads *flagged* posts through it to compute the blob withhold set
/// (`moderation_withhold.rs`), so teaching it to withhold would silently empty
/// that set — which meant an HLS playlist served every variant, every segment
/// duration and an absolute URL per segment for a post that was taken down or
/// quarantined. The gate does
/// not belong in the primitive; it belongs on the serve path, which is what
/// `get_post_core` is.
///
/// So both handlers route through it and inherit, in order:
///
/// 1. the **legal-takedown** gate, checked before the body is read — answered
///    here as **451, no body**, the same answer `GET /api/v1/posts/{id}` gives
///    (`routes.rs`) and the same one the blob door gives
///    (`moderation.md` § Legal takedown → the blob-serve door). A playlist has
///    no tombstone render of its own; the reference reaches the client through
///    the post's own read, which is what paints it;
/// 2. the **quarantine** gate (`caller_may_read_content`), which needs a
///    caller — hence the `HeaderMap` and the optional bearer, exactly as the
///    post HTTP twin does it. These routes therefore stop being unconditionally
///    public: a quarantined post's playlist is author/admin-only. Nothing in
///    the tree consumes `master.m3u8` today (no app, lib or driver references
///    it), so there is no client leg to break, and the alternative — a serve
///    path that answers what the post read refuses — is the bug being closed.
///
/// One consequence worth naming: `get_post_core` reads the `content` row before
/// the segment body, where `load_post_body` was segment-first. A post whose row
/// is missing but whose segment survives now answers 404 here instead of a
/// playlist — the same answer its own post read already gives, and the
/// withholding direction the public-servability predicate deliberately takes
/// for a post whose index write failed (`moderation.md` § Legal takedown).
async fn load_video_post(
    state: &Arc<AppState>,
    headers: &axum::http::HeaderMap,
    post_id_hex: &str,
) -> Result<fauna_core::data::Post, axum::response::Response> {
    let post_id = match parse_hash(post_id_hex) {
        Some(h) => h,
        None => return Err(ApiError::bad_request("invalid post id hex").into_response()),
    };

    let caller = crate::routes::optional_bearer_auth(headers, state)
        .await
        .map(|id| id.0);

    let post_bytes = match crate::routes::get_post_core(state, caller, post_id).await {
        crate::routes::GetPostOutcome::Found(bytes) => bytes,
        crate::routes::GetPostOutcome::LegalTakedown { .. } => {
            return Err(StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response());
        }
        crate::routes::GetPostOutcome::NotFound => {
            return Err(StatusCode::NOT_FOUND.into_response());
        }
        crate::routes::GetPostOutcome::Error => {
            return Err(ApiError::internal("db error").into_response());
        }
    };

    // Decode embed-as-bytes wire shape (signed posts), fall back to bare Post.
    match crate::db::posts::decode_stored_post(&post_bytes) {
        Some(post) => Ok(post),
        None => {
            tracing::error!("failed to decode post");
            Err(ApiError::internal("decode error").into_response())
        }
    }
}

/// GET /api/v1/video/{post_id}/master.m3u8 -- Serve HLS master playlist for a video post.
pub async fn serve_master_manifest(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(post_id_hex): Path<String>,
) -> impl IntoResponse {
    let post = match load_video_post(&state, &headers, &post_id_hex).await {
        Ok(post) => post,
        Err(response) => return response,
    };

    let segments = match &post.body {
        fauna_core::data::PostBody::Video { segments, .. } => segments,
        _ => return ApiError::bad_request("not a video post").into_response(),
    };

    // Claim-refreshed identity domain, not the `node.domain` boot seed: a
    // provisioned box boots domainless, so the seed would put `localhost` into
    // every absolute segment URL the playlist hands the player.
    let domain = state.handle_domain();
    let scheme = if state.tls_enabled { "https" } else { "http" };
    let base_url = format!("{scheme}://{domain}/api/v1/video/segments");

    let seg_refs: Vec<&fauna_core::data::VideoSegment> = segments.iter().collect();
    let playlist = crate::video::manifest::generate_master_playlist(&seg_refs, &base_url);

    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/vnd.apple.mpegurl")
        .body(axum::body::Body::from(playlist))
        .unwrap()
        .into_response()
}

/// GET /api/v1/video/{post_id}/{variant} -- Serve HLS variant playlist for a video post.
pub async fn serve_variant_manifest(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((post_id_hex, variant)): Path<(String, String)>,
) -> impl IntoResponse {
    let resolution = match variant.trim_end_matches(".m3u8").parse::<u16>() {
        Ok(r) => r,
        Err(_) => return ApiError::bad_request("invalid variant").into_response(),
    };

    let post = match load_video_post(&state, &headers, &post_id_hex).await {
        Ok(post) => post,
        Err(response) => return response,
    };

    let (segments, duration_ms) = match &post.body {
        fauna_core::data::PostBody::Video {
            segments,
            duration_ms,
            ..
        } => (segments, *duration_ms),
        _ => return ApiError::bad_request("not a video post").into_response(),
    };

    let filtered: Vec<fauna_core::data::VideoSegment> = segments
        .iter()
        .filter(|s| s.resolution == resolution)
        .cloned()
        .collect();

    if filtered.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let segment_duration = (duration_ms as f64 / 1000.0) / filtered.len() as f64;

    // Claim-refreshed identity domain, not the `node.domain` boot seed: a
    // provisioned box boots domainless, so the seed would put `localhost` into
    // every absolute segment URL the playlist hands the player.
    let domain = state.handle_domain();
    let scheme = if state.tls_enabled { "https" } else { "http" };
    let base_url = format!("{scheme}://{domain}/api/v1/video/segments");

    let playlist =
        crate::video::manifest::generate_variant_playlist(&filtered, &base_url, segment_duration);

    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/vnd.apple.mpegurl")
        .body(axum::body::Body::from(playlist))
        .unwrap()
        .into_response()
}
