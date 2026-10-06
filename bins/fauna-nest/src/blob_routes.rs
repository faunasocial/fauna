//! HTTP handlers for blob upload/download.
//!
//! Two URL shapes share the path `/api/v1/blob/{id}`:
//!
//! - **Hex `blake3` digest:** `POST /api/v1/blob` (auth) returns
//!   `{"hash": "<64 hex chars>"}`; `GET /api/v1/blob/<hex>` serves the bytes
//!   plus mode-aware MIME / `?thumb=1` lookup via the blob-metadata table.
//!   Every app uploads and downloads through this shape; permanent
//!   (`api-layers.md` § Remaining HTTP).
//!
//! - **CBOR-DAG-everywhere (multibase base32 `Cid`):**
//!   `PUT /api/v1/blob/<cid_b32>` (auth) and `GET /api/v1/blob/<cid_b32>`,
//!   both `application/octet-stream`. The server verifies
//!   `blake3(body) == cid.digest()` before storing or returning bytes — the
//!   canonical byte-source surface per the CBOR-DAG-everywhere Layer 3 design
//!   (tracked internally) § HTTP residue. No metadata lookup, no thumbnail; pure bytes.
//!
//! `download_blob` dispatches by sniffing the path parameter: multibase
//! base32 (`b…`) routes through the CID branch; everything else falls back
//! to the hex branch. PUT goes straight to the CID branch — the legacy
//! upload uses `POST /api/v1/blob` (no path param), no overlap.
//!
//! Both shapes write through the same `BlobStoreBackend` (sharded by
//! `blake3` digest under `<data_dir>/blobs/`), so a blob PUT under its
//! CID is later fetchable by hex hash and vice versa.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use fauna_cbor::Cid;
use fauna_media::sidecar::UploadSidecar;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::{AppState, parse_32_bytes as parse_hash};
use crate::storage::{BlobIngestItem, BlobIngestVerdict};

/// Hard cap on inline blob upload size (multipart total body or octet-stream
/// body). Larger uploads use the `/api/v1/chunks/*` path. Mirrors the
/// effective pre-multipart ceiling and the spec § Wire shape — multipart.
///
/// Enforced twice, once per body shape: the multipart POST checks it while
/// draining parts (below), and the octet-stream `PUT /api/v1/blob/{cid}` gets
/// it as a `DefaultBodyLimit` layer at the route (`crate::build_router`) —
/// without that layer the PUT silently kept axum's 2 MB `Bytes` default, which
/// is why this is `pub`.
///
/// The number is `fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT`, which
/// every app's attachment reader shares: no sealed blob larger than this door
/// accepts is ever fetched or opened.
pub const BLOB_BODY_LIMIT: usize = fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT;

/// POST /api/v1/blob -- Upload a blob, returns its content hash.
///
/// Accepts a single body shape: **`multipart/form-data`** with exactly two
/// parts named `sidecar` (DAG-CBOR `UploadSidecar`) and `bytes` (sealed bytes),
/// in any order. Missing either part, extra parts, or a sidecar that fails
/// strict dag-cbor decode all return 400 with a structured `{"error": "..."}`
/// body.
///
/// Any non-multipart content-type → 400. The legacy `application/octet-stream`
/// shape retired with the blob strict flip (see
/// `encryption-at-rest.md` § Don't do these) — every app now seals +
/// sidecars every blob upload in both storage modes. Total body cap is
/// `BLOB_BODY_LIMIT` (10 MiB); oversize → 413.
///
/// Ingest is dispatched through `state.storage().ingest_blob(...)` so the
/// mode-specific pipeline lives in the `Storage` impl. This handler owns
/// only the network-layer concerns: content-type dispatch, multipart parse,
/// blob store + metadata writes, and metric emission.
///
/// **Auth is [`crate::auth::BulkWriteAuth`] (since 2026-09-09), not a
/// session-only bearer.** A session bearer still satisfies it — every app's
/// own-nest upload is unchanged — and a `Write`-scoped bulk-byte token is
/// accepted too, which is what lets a **foreign conversation member** POST a
/// sealed attachment DIRECT to the room's home nest under the token
/// `fauna.federation.conversation.write_token.mint` issued (the room's
/// attachment bytes rest on its home nest beside the record that pins them —
/// `conversation-rooms.md` § The home nest → *Attachment bytes*). One door,
/// two credentials: the sidecar classification, the strict per-class seal
/// check and the `blob_metadata` row are identical for both, so the GC's
/// step-2g pin sees the same trace whoever uploaded. A bulk token already
/// reached this store byte-for-byte through `PUT /api/v1/blob/{cid}` and the
/// chunk routes (the purpose is spent at the mint, never at a byte route —
/// `BulkByteMintPurpose`), so admitting it here widens nothing at the byte
/// layer.
pub async fn upload_blob(
    State(state): State<Arc<AppState>>,
    bearer: crate::auth::BulkWriteAuth,
    request: Request,
) -> axum::response::Response {
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    // Pick which body parser to run based on Content-Type.
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Only `multipart/form-data` (sidecar + bytes) is accepted. The legacy
    // `application/octet-stream` shape retired alongside the blob strict flip
    // (see `encryption-at-rest.md` § Don't do
    // these) — every app now seals + sidecars every upload in both modes.
    // Any non-multipart Content-Type → 400.
    if !content_type.starts_with("multipart/form-data") {
        return ApiError::bad_request(
                "blob upload requires Content-Type: multipart/form-data with 'sidecar' and 'bytes' parts",
            )
            .into_response();
    }
    let (bytes, sidecar): (Bytes, UploadSidecar) = match parse_multipart_upload(request).await {
        Ok((b, s)) => (b, s),
        Err(e) => return e.into_response(),
    };

    let storage = state.storage();
    let outcome = match storage
        .ingest_blob(&BlobIngestItem {
            uploader: bearer.0.0,
            body: bytes.as_ref(),
            sidecar: Some(sidecar),
        })
        .await
    {
        Ok(o) => o,
        Err(e) => {
            // Distinguish a strict-verifier rejection (400, `verdict=rejected`
            // with the snake_case reason) from a genuine internal error
            // (`verdict=error`), mirroring the post + channel ingest handlers.
            let reason_label = match e.kind {
                crate::storage::StorageUnavailableKind::IngestRejected(r) => r.as_snake_case(),
                _ => "error",
            };
            let verdict = match e.kind {
                crate::storage::StorageUnavailableKind::IngestRejected(_) => "rejected",
                _ => "error",
            };
            tracing::warn!(
                target: "nest_metrics",
                metric = "nest_blob_ingest_total",
                verdict,
                reason = reason_label,
                "blob ingest rejected: {e}"
            );
            return e.into_api_error().into_response();
        }
    };

    match outcome.verdict {
        BlobIngestVerdict::Accepted => {
            tracing::debug!(
                target: "nest_metrics",
                metric = "nest_blob_ingest_total",
                verdict = "accepted",
                "blob ingest"
            );
        }
    }

    let hash_bytes: [u8; 32] = *blake3::hash(outcome.stored_bytes.as_ref()).as_bytes();
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    if let Err(e) = store.put(&hash, outcome.stored_bytes.as_ref()).await {
        tracing::error!(
            target: "nest_metrics",
            metric = "nest_blob_ingest_total",
            verdict = "error",
            "blob upload error: {e}"
        );
        return ApiError::internal("storage error").into_response();
    }

    // The nest never renders a thumbnail — it cannot read the bytes. The
    // uploader renders it client-side (`fauna_media::process`) and uploads it as
    // its own sealed blob in a separate POST /api/v1/blob; only the declarative
    // routing hash rides in the sidecar.
    let thumbnail_hash = outcome.thumbnail_hash;

    // The metadata row is the blob's only durable trace until a reference row
    // lands (GC keeps a row-less blob forever; the `__index` purge keep-check
    // can't see it). Fail the request rather than ACK an upload with no
    // durable trace — the content-addressed retry is idempotent.
    if let Err(e) = state
        .db
        .put_blob_metadata(
            &hash_bytes,
            outcome.stored_bytes.len() as i64,
            &outcome.mime,
            outcome.has_c2pa,
            thumbnail_hash.as_ref(),
        )
        .await
    {
        tracing::error!("failed to record blob metadata: {e}");
        return ApiError::internal("storage error").into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({ "hash": hex::encode(hash_bytes) })),
    )
        .into_response()
}

/// Parse a `multipart/form-data` body into `(bytes, sidecar)`. Enforces the
/// "exactly two parts named `sidecar` and `bytes`" rule, returns 400 on
/// shape failure or sidecar decode failure, and 413 on oversize total body.
async fn parse_multipart_upload(request: Request) -> Result<(Bytes, UploadSidecar), ApiError> {
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|e| ApiError::bad_request(format!("multipart parse error: {e}")))?;

    let mut sidecar: Option<UploadSidecar> = None;
    let mut bytes: Option<Bytes> = None;
    let mut total: usize = 0;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("multipart parse error: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::bad_request(format!("multipart read field '{name}': {e}")))?;
        total = total.saturating_add(data.len());
        if total > BLOB_BODY_LIMIT {
            return Err(ApiError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                message: format!(
                    "upload exceeds {BLOB_BODY_LIMIT}-byte inline-blob cap; use /api/v1/chunks for larger uploads"
                ),
            });
        }
        match name.as_str() {
            "sidecar" => {
                if sidecar.is_some() {
                    return Err(ApiError::bad_request("duplicate 'sidecar' part"));
                }
                let s = UploadSidecar::from_dag_cbor(data.as_ref()).map_err(|e| {
                    ApiError::bad_request(format!("sidecar CBOR decode error: {e}"))
                })?;
                sidecar = Some(s);
            }
            "bytes" => {
                if bytes.is_some() {
                    return Err(ApiError::bad_request("duplicate 'bytes' part"));
                }
                bytes = Some(data);
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "unexpected multipart part: '{other}' (expected 'sidecar' and 'bytes' only)"
                )));
            }
        }
    }

    let sidecar = sidecar.ok_or_else(|| ApiError::bad_request("missing 'sidecar' part"))?;
    let bytes = bytes.ok_or_else(|| ApiError::bad_request("missing 'bytes' part"))?;
    Ok((bytes, sidecar))
}

/// GET /api/v1/blob/{id} -- Download a blob.
///
/// `{id}` is either a multibase base32 CID (`b…` prefix) or a 64-char hex
/// blake3 digest. Multibase CIDs go through the CBOR-DAG-everywhere branch
/// (`get_blob_by_cid_inner`): octet-stream + `ETag: "<cid_b32>"` +
/// server-side `blake3(bytes) == cid.digest()` verification, no thumbnail
/// support. Hex IDs use the legacy branch (mode-aware MIME via
/// `blob_metadata`, `?thumb=1` for thumbnails). Both share the same
/// underlying blob store.
///
/// **Byte ranges — hex branch only.** A single `Range: bytes=…` is answered
/// 206 with exactly those bytes read through
/// [`crate::blob_store::BlobStoreBackend::get_range`] (a seek into a large
/// video never reads the whole blob), 416 when it lies past the end, and the
/// whole body when it is malformed or multi-range; every 200 advertises
/// `Accept-Ranges: bytes` (parser: [`crate::http_range`]). The CID branch
/// ignores `Range` on purpose: it verifies `blake3(bytes) == cid.digest()` over
/// the whole body, and a range of unverified bytes defeats what it is for. A
/// sealed blob's range is served like any other — ciphertext ranges are
/// harmless and useless, and no client asks for one (`render-model.md` § D6c →
/// *Inline playback*, answer 2).
///
/// **Legal-takedown withhold.** Both branches address the same digest in the
/// same store, so the flag is consulted once, here, on the resolved digest
/// before either branch runs — a hex-only gate would be bypassed by asking for
/// the same bytes under their `b…` CID. The check precedes the `?thumb=1`
/// lookup deliberately: a withheld original must not serve a thumbnail *of*
/// itself. The thumbnail blob is also withheld when asked for directly, since
/// the post names it in its own right (`MediaItem::thumbnail` is one of
/// `Post::blob_refs`) — and a thumbnail the post does NOT name is pinned by
/// nothing and has already been swept. See [`crate::moderation_withhold`] for
/// the predicate.
pub async fn download_blob(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // CBOR-DAG-everywhere CID path: any multibase-prefixed identifier
    // that decodes as a valid CID routes to the CID branch. Plain
    // unprefixed hex falls through to the legacy metadata-aware GET.
    // `Cid::from_base32` accepts both dag-cbor (0x71) and raw (0x55)
    // codecs after Layer 3 Task 3.7.
    let (digest, cid_branch) = match blob_id_digest(&id) {
        Some(d) => d,
        None => return ApiError::bad_request("invalid hash hex").into_response(),
    };
    if let Some(resp) = legal_takedown_gate(&state.db, &digest).await {
        return resp;
    }

    if cid_branch {
        tracing::info!(cid = %id, "blob get (cid)");
        return get_blob_by_cid_inner(&state, &id).await;
    }

    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    let hash_bytes = digest;

    let want_thumb = params.get("thumb").map(|v| v == "1").unwrap_or(false);

    // If thumbnail requested, look up the thumbnail hash and serve that instead
    if want_thumb
        && let Ok(Some(meta)) = state.db.get_blob_metadata(&hash_bytes).await
        && let Some(thumb_hash_bytes) = &meta.thumbnail_hash
        && let Ok(thumb_arr) = <[u8; 32]>::try_from(thumb_hash_bytes.as_slice())
    {
        let thumb_hash = fauna_core::data::ContentHash::from_digest_raw(thumb_arr);
        if let Ok(Some(_)) = store.len(&thumb_hash).await {
            return serve_hex_blob(store.as_ref(), &thumb_hash, &headers, "image/jpeg", None).await;
        }
    }
    // Fall through to serve original if no thumbnail

    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    // The stored content-type is user-influenced (encrypted-mode ingest keeps
    // the uploader's sidecar `mime` verbatim), so it is sanitized through the
    // shared inert-type allowlist in `blob_response_builder`.
    let (content_type, c2pa_header) = match state.db.get_blob_metadata(&hash_bytes).await {
        Ok(Some(meta)) => (meta.content_type, meta.has_c2pa),
        _ => (String::new(), None),
    };
    serve_hex_blob(store.as_ref(), &hash, &headers, &content_type, c2pa_header).await
}

/// Serve one hex-addressed blob: 206/416 for a `Range` the request carries
/// ([`crate::http_range`]), else the whole body with 200. Every answer carries
/// the inert-type allowlist + `nosniff` + `inline` (+ `x-c2pa`) of
/// [`blob_response_builder`]. Reached only from [`download_blob`], after its
/// legal-takedown gate.
async fn serve_hex_blob(
    store: &dyn crate::blob_store::BlobStoreBackend,
    hash: &fauna_core::data::ContentHash,
    headers: &HeaderMap,
    content_type: &str,
    has_c2pa: Option<bool>,
) -> Response {
    let storage_error = |e: anyhow::Error| {
        tracing::error!("blob download error: {e}");
        ApiError::internal("storage error").into_response()
    };

    if crate::http_range::has_range(headers) {
        let total = match store.len(hash).await {
            Ok(Some(n)) => n,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(e) => return storage_error(e),
        };
        match crate::http_range::requested(headers, total) {
            crate::http_range::ByteRange::Satisfiable { start, end } => {
                return match store.get_range(hash, start, end).await {
                    Ok(Some(bytes)) => crate::http_range::partial_content(
                        blob_response_builder(content_type, has_c2pa),
                        start,
                        end,
                        total,
                    )
                    .body(axum::body::Body::from(bytes))
                    .expect("sanitized headers")
                    .into_response(),
                    Ok(None) => StatusCode::NOT_FOUND.into_response(),
                    Err(e) => storage_error(e),
                };
            }
            crate::http_range::ByteRange::Unsatisfiable => {
                return crate::http_range::not_satisfiable(total);
            }
            crate::http_range::ByteRange::Ignore => {}
        }
    }

    match store.get(hash).await {
        Ok(Some(data)) => build_blob_response(data, content_type, has_c2pa),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => storage_error(e),
    }
}

/// The blob-store digest a `{id}` path segment addresses, plus whether it was
/// spelled as a CID — a multibase base32 `Cid` (`b…`, either codec) or a plain
/// 64-char hex blake3. Resolved ONCE, in one place, because the two spellings
/// name the same bytes in the same store: parsing the id twice would let the
/// takedown gate and the branch choice disagree, which is precisely the bypass
/// this shape exists to make unrepresentable.
fn blob_id_digest(id: &str) -> Option<([u8; 32], bool)> {
    match Cid::from_base32(id) {
        Ok(cid) => Some((cid.digest(), true)),
        Err(_) => parse_hash(id).map(|h| (h, false)),
    }
}

/// `Some(451)` when a legal takedown withholds this blob from the serve door.
///
/// The takedown withholds the naming record's body at the read primitive every
/// per-record serve path flows through; its attachments live outside that body
/// as content-addressed blobs, which every recipient can already name. This is
/// where that gap closes (`moderation.md` § Legal takedown → *The blob-serve
/// door*; predicate + rebuild in [`crate::moderation_withhold`]).
///
/// **451 with no body**, the same posture the federation HTTP twin takes for a
/// withheld post (`routes.rs::federation_post_get`): the withholding is
/// disclosed rather than hidden behind a 404 — a compelled removal is never
/// silent — but the tombstone reference is not spelled here. A blob has no
/// tombstone render of its own; the reference belongs to the record, whose own
/// serve path already carries it to the client that will paint it.
///
/// **Fails OPEN on a storage error, deliberately.** The alternative is a nest
/// whose media all 404s when SQLite hiccups, and this table is derived: it is
/// rebuilt on the next takedown and on every complete sweep, so a transient
/// read fault is a bounded window on a rare flag, not a broken box.
///
/// `pub(crate)` because the blob store has **more than one door**: the video
/// segment route and the file-sync plane's chunk and manifest routes serve the
/// same digests out of the same store, so they call this rather than growing a
/// second hand-copy of the posture. Enrolling the next one is mechanical —
/// `backup::service`'s `every_blob_store_serve_path_is_partitioned` fails on a
/// caller nobody has classified (`moderation.md` § Legal takedown → *The
/// blob-serve door*).
pub(crate) async fn legal_takedown_gate(
    db: &crate::db::CacheDb,
    digest: &[u8; 32],
) -> Option<axum::response::Response> {
    if !is_legally_withheld(db, digest).await {
        return None;
    }
    tracing::info!(
        blob = hex::encode(digest),
        "blob get withheld — legal takedown"
    );
    Some(StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response())
}

/// The withhold predicate itself, for the readers that are **not routes**.
///
/// [`legal_takedown_gate`] answers a request; the manifest→chunks file walk
/// (`web_content::file_bytes::read_file_by_manifest`) is not a request handler
/// and must fold the same verdict into its own error type, so the posture —
/// including its deliberate fail-OPEN on a storage error, for the reason the
/// gate's own doc gives — lives here once and both read it.
pub(crate) async fn is_legally_withheld(db: &crate::db::CacheDb, digest: &[u8; 32]) -> bool {
    match db.blob_is_legally_withheld(digest).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                blob = hex::encode(digest),
                error = %e,
                "legal-takedown withhold lookup failed — serving the blob"
            );
            false
        }
    }
}

/// Build the response for a hex-hash blob download with content-type
/// sanitization.
///
/// `GET /api/v1/blob/<hex>` is unauthenticated + navigable, and the stored
/// content-type is **user-influenced** — encrypted-mode ingest stores the
/// uploader's sidecar `mime` verbatim (`storage::sealed::SealedStorage::ingest_blob`).
/// If we reflected `text/html` / `image/svg+xml`, a victim who merely navigates to
/// the blob URL would execute attacker-chosen bytes as script on the SPA origin and
/// read `localStorage.fauna_secret` (the raw master key). So the stored type is
/// mapped through the same inert-media allowlist the media proxy uses
/// (`media_proxy_routes::safe_content_type`, `image/svg+xml` excluded) and served
/// with `X-Content-Type-Options: nosniff` so a relabelled octet-stream cannot be
/// sniffed back into html. No bearer is added on purpose: blobs render as
/// `<img src>` / `<video>` subresources across all 7 apps, which cannot carry
/// one — the allowlist + `nosniff` (not a credential) is what closes the
/// navigable-XSS vector, exactly as for the bluesky media proxy.
fn build_blob_response(
    body: impl Into<axum::body::Body>,
    stored_content_type: &str,
    has_c2pa: Option<bool>,
) -> Response {
    blob_response_builder(stored_content_type, has_c2pa)
        .status(StatusCode::OK)
        .header(header::ACCEPT_RANGES, "bytes")
        .body(body.into())
        .unwrap()
        .into_response()
}

/// The content headers every hex-branch answer carries, 200 and 206 alike —
/// the hardening above, with no status and no body.
fn blob_response_builder(
    stored_content_type: &str,
    has_c2pa: Option<bool>,
) -> axum::http::response::Builder {
    let content_type = crate::media_proxy_routes::safe_content_type(stored_content_type);
    let mut builder = axum::http::Response::builder()
        .header("content-type", content_type)
        .header("x-content-type-options", "nosniff")
        .header("content-disposition", "inline");
    if let Some(has_c2pa) = has_c2pa {
        builder = builder.header("x-c2pa", if has_c2pa { "true" } else { "false" });
    }
    builder
}

// ── CBOR-DAG-everywhere blob endpoint (Layer 3) ──────────────────────────
//
// `PUT|GET /api/v1/blob/{cid_b32}` — canonical byte-source surface for
// CID-addressed content. PUT verifies `blake3(body) == cid.digest()` before
// storing (400 on mismatch). GET verifies the same before returning (500 on
// disk corruption). Idempotent under matching CIDs; 404 for unknown CIDs.
// Bytes are written to the existing `BlobStoreBackend` keyed by the 32-byte
// blake3 digest (`cid.digest()`), so the same blob is fetchable under
// its hex hash too.
//
// `Cid::from_base32` accepts both dag-cbor (`0x71`) and raw (`0x55`)
// codec CIDs; the 32-byte digest extraction
// goes through `as_bytes()[4..]`.

/// PUT /api/v1/blob/{cid_b32} -- Upload a blob keyed by its CID.
///
/// Verifies `blake3(body) == cid.digest()` before storing. Idempotent: a
/// repeat PUT with the same CID + body returns 200 with `{"status":
/// "exists"}`. Mismatched body → 400 + `{"error": "cid_mismatch", …}`.
/// Malformed CID in URL → 400 + `{"error": "bad_cid", …}`.
///
/// **Records `blob_metadata` before the ACK** — the row is what puts these
/// bytes inside GC's reach at all (`backup-restore.md` § GC, step 5 and its
/// sweep's-premise note). See the comment at the write below.
///
/// **Auth is [`crate::auth::BulkWriteAuth`], not a session-only bearer.** This is
/// the byte half of the `__index` rail (`content-index.md` § Where the index is
/// built). It once served two writers; since the 2026-08-10 carrier ruling the
/// MDA's build half is retired (its `IndexSegment` mint purpose is refused and
/// `fauna.bridges.index_record` is gone), so the rail's only writer is the
/// user's client — but `BulkWriteAuth` stays the right extractor: bulk-byte
/// tokens remain the write credential for the other live purposes, and a
/// session bearer still satisfies it for the client leg. The chunk route is not
/// an alternative: that one stores `encode_blob(body)` and decodes symmetrically
/// on its own GET, while this pair is verbatim in both directions.
pub async fn put_blob_by_cid(
    State(state): State<Arc<AppState>>,
    _bearer: crate::auth::BulkWriteAuth,
    Path(cid_b32): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    tracing::info!(cid = %cid_b32, bytes = body.len(), "blob put");

    let cid = match Cid::from_base32(&cid_b32) {
        Ok(cid) => cid,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "bad_cid", "raw": cid_b32})),
            )
                .into_response();
        }
    };

    if !cid.matches(body.as_ref()) {
        let computed = Cid::of_dag_cbor(body.as_ref());
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "cid_mismatch",
                "url_cid": cid_b32,
                "body_cid": computed.to_base32(),
            })),
        )
            .into_response();
    }

    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "backup_not_configured"})),
            )
                .into_response();
        }
    };
    let store = backup_svc.local_blob_store();
    let content_hash = fauna_core::data::ContentHash::from_digest_raw(cid.digest());

    let already_existed = match store.exists(&content_hash).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(cid = %cid_b32, "blob put exists() error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };

    if let Err(e) = store.put(&content_hash, body.as_ref()).await {
        tracing::error!(cid = %cid_b32, "blob put store error: {e}");
        return ApiError::internal("storage error").into_response();
    }

    // The metadata row is what makes these bytes REACHABLE BY GC: the delete
    // phase enumerates `blob_metadata` and nothing else
    // (`backup/gc.rs::do_gc_delete_phase` -> `db::blobs::list_all_blob_hashes`),
    // so bytes stored without it are neither charged nor collectable --
    // permanently, for any principal that can reach this route. The
    // unmetered posture this route rests on
    // (`webdav-server.md` Bulk-byte plane) is "charged at record time, and
    // anything never recorded is collected", and only the second half bounds an
    // upload that never gets recorded. Written on the idempotent repeat too, so
    // a blob PUT before this fix gains its row on the next write; the insert is
    // INSERT OR IGNORE, which keeps the original `created_at` anchoring the
    // creation-grace window.
    //
    // Fail the request rather than ACK an upload with no durable trace -- the
    // multipart POST twin above takes the same line, and a content-addressed
    // retry is idempotent. The upload->record gap is covered by that same grace
    // window, and an index segment whose `fauna.index.record` never lands is
    // re-created by the builder (`content-index.md` Where the index is built).
    if let Err(e) = state
        .db
        .put_blob_metadata(&cid.digest(), body.len() as i64, "chunk", None, None)
        .await
    {
        tracing::error!(cid = %cid_b32, "blob put metadata error: {e}");
        return ApiError::internal("storage error").into_response();
    }

    let status = if already_existed { "exists" } else { "created" };
    (
        StatusCode::OK,
        Json(json!({"status": status, "cid": cid_b32})),
    )
        .into_response()
}

/// Internal: serve a blob by CID with on-read verification and ETag.
///
/// Returns 404 if the blob isn't stored. Returns 500 +
/// `{"error": "disk_corruption", …}` if the stored bytes don't hash to
/// the CID — indicates on-disk tampering, never a request bug.
///
/// **Ignores `Range` on purpose** and advertises no `Accept-Ranges`: the
/// verification runs over the whole body, and a range of unverified bytes
/// would defeat what this route is for. Ranged reads are the hex branch's
/// (`download_blob`).
async fn get_blob_by_cid_inner(state: &Arc<AppState>, cid_b32: &str) -> axum::response::Response {
    // Path already validated by the caller via `Cid::from_base32`.
    let cid = Cid::from_base32(cid_b32).expect("caller verified base32 CID");

    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "backup_not_configured"})),
            )
                .into_response();
        }
    };
    let store = backup_svc.local_blob_store();
    let content_hash = fauna_core::data::ContentHash::from_digest_raw(cid.digest());

    let data = match store.get(&content_hash).await {
        Ok(Some(data)) => data,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "not_found", "cid": cid_b32})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(cid = %cid_b32, "blob get store error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };

    if !cid.matches(&data) {
        let computed = Cid::of_dag_cbor(&data);
        tracing::error!(
            cid = %cid_b32,
            computed = %computed.to_base32(),
            "blob get: on-disk bytes do not match CID — corruption or tampering",
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": "disk_corruption",
                "expected": cid_b32,
                "got": computed.to_base32(),
            })),
        )
            .into_response();
    }

    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("etag", format!("\"{cid_b32}\""))
        .body(axum::body::Body::from(data))
        .unwrap()
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(resp: &Response, name: &str) -> String {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    }

    // A stored, user-influenced content-type that a browser would execute
    // (the encrypted-mode uploader controls the sidecar `mime`) must be relabelled
    // to an inert type AND carry `nosniff`, so a navigated unauthenticated blob can
    // never run as script on the SPA origin.
    #[test]
    fn blob_response_relabels_script_capable_types_and_sets_nosniff() {
        for dangerous in [
            "text/html",
            "image/svg+xml",
            "application/javascript",
            "text/javascript",
            "application/xhtml+xml",
            "text/xml",
            "application/xml",
            "text/html; charset=utf-8",
        ] {
            let resp = build_blob_response(Vec::<u8>::new(), dangerous, None);
            assert_eq!(
                header(&resp, "content-type"),
                "application/octet-stream",
                "{dangerous} must be relabelled to octet-stream"
            );
            assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
            assert_eq!(header(&resp, "content-disposition"), "inline");
        }
    }

    // Inert raster/AV types the SPA renders as `<img>`/`<video>` subresources keep
    // their declared type (so rendering still works) and also carry `nosniff`.
    #[test]
    fn blob_response_keeps_inert_media_types() {
        for safe in [
            "image/png",
            "image/jpeg",
            "image/webp",
            "video/mp4",
            "audio/mpeg",
        ] {
            let resp = build_blob_response(Vec::<u8>::new(), safe, None);
            assert_eq!(header(&resp, "content-type"), safe);
            assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
        }
    }

    // Empty / non-MIME stored values fall to octet-stream (no `contains('/')`
    // bypass), still with `nosniff`.
    #[test]
    fn blob_response_empty_or_garbage_type_is_octet_stream() {
        for raw in ["", "notamime", "x"] {
            let resp = build_blob_response(Vec::<u8>::new(), raw, None);
            assert_eq!(header(&resp, "content-type"), "application/octet-stream");
            assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
        }
    }

    // The C2PA advisory header still rides along when present.
    #[test]
    fn blob_response_preserves_c2pa_header() {
        let resp = build_blob_response(Vec::<u8>::new(), "image/jpeg", Some(true));
        assert_eq!(header(&resp, "x-c2pa"), "true");
        let resp = build_blob_response(Vec::<u8>::new(), "image/jpeg", Some(false));
        assert_eq!(header(&resp, "x-c2pa"), "false");
        let resp = build_blob_response(Vec::<u8>::new(), "image/jpeg", None);
        assert_eq!(header(&resp, "x-c2pa"), "");
    }

    // ── Legal-takedown withhold at the door ──────────────────────────────

    /// The two spellings of a blob id address ONE digest, so one gate covers
    /// both branches. This is the whole reason the takedown check resolves the
    /// id before branching: gating only the hex branch would leave "ask again
    /// for the same bytes in base32" as a one-line bypass of a legally
    /// compelled withhold.
    #[test]
    fn a_hex_id_and_its_cid_resolve_to_the_same_digest() {
        let hash = fauna_core::data::ContentHash::of_raw(b"the withheld attachment");
        let hex_id = hex::encode(hash.digest());
        let cid_id = hash.to_base32();
        assert_ne!(
            hex_id, cid_id,
            "the two spellings really are different text"
        );
        assert_eq!(blob_id_digest(&hex_id), Some((hash.digest(), false)));
        assert_eq!(
            blob_id_digest(&cid_id),
            Some((hash.digest(), true)),
            "the CID branch must resolve to the same digest the hex branch does — \
             else the gate below guards one door and not the other"
        );
        assert_eq!(blob_id_digest("not-a-blob-id"), None);
    }

    /// The gate itself: withheld ⇒ 451 (disclosed, never a 404 that would hide
    /// the withholding), otherwise the request proceeds. Driven through the
    /// production predicate table, so a mutant that stops writing it reds here.
    #[tokio::test]
    async fn the_door_answers_451_for_a_withheld_blob_and_passes_every_other() {
        let db = crate::db::CacheDb::open_in_memory().unwrap();
        let withheld = fauna_core::data::ContentHash::of_raw(b"illegal attachment");
        let ordinary = fauna_core::data::ContentHash::of_raw(b"someone else's photo");
        db.replace_blob_legal_withhold(&[withheld.digest()])
            .await
            .unwrap();

        let resp = legal_takedown_gate(&db, &withheld.digest())
            .await
            .expect("a withheld blob must not reach the store read");
        assert_eq!(resp.status(), StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS);
        assert!(
            legal_takedown_gate(&db, &ordinary.digest()).await.is_none(),
            "an unwithheld blob must pass the gate untouched"
        );

        // Restore is tombstone-not-delete: clearing the set re-serves the very
        // same bytes, with nothing having been deleted from the store.
        db.replace_blob_legal_withhold(&[]).await.unwrap();
        assert!(
            legal_takedown_gate(&db, &withheld.digest()).await.is_none(),
            "a restored record's attachment must serve again"
        );
    }

    // ── Byte ranges on the hex branch (render-model.md § D6c → Inline playback,
    //    answer 2) ────────────────────────────────────────────────────────────

    /// A held `video/mp4` blob of 64 bytes plus its 16-byte thumbnail, on a
    /// state with a real disk-backed blob store.
    async fn ranged_fixture() -> (
        Arc<AppState>,
        tempfile::TempDir,
        fauna_core::data::ContentHash,
        Vec<u8>,
        Vec<u8>,
    ) {
        let (state, tmp) = crate::test_support::fixture_state_with_backup().await;
        let store = state.backup_service.as_ref().unwrap().local_blob_store();
        let video: Vec<u8> = (0u8..64).collect();
        let thumb: Vec<u8> = (100u8..116).collect();
        let hash = fauna_core::data::ContentHash::of_raw(&video);
        let thumb_hash = fauna_core::data::ContentHash::of_raw(&thumb);
        store.put(&hash, &video).await.unwrap();
        store.put(&thumb_hash, &thumb).await.unwrap();
        state
            .db
            .put_blob_metadata(
                &hash.digest(),
                video.len() as i64,
                "video/mp4",
                None,
                Some(&thumb_hash.digest()),
            )
            .await
            .unwrap();
        (state, tmp, hash, video, thumb)
    }

    async fn get_blob(
        state: &Arc<AppState>,
        path: &str,
        range: Option<&str>,
    ) -> (Response, Vec<u8>) {
        use tower_service::Service;
        let mut router = axum::Router::new()
            .route("/api/v1/blob/{id}", axum::routing::get(download_blob))
            .with_state(state.clone());
        let mut req = axum::http::Request::builder().uri(path);
        if let Some(r) = range {
            req = req.header(axum::http::header::RANGE, r);
        }
        let resp = router
            .call(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (
            Response::from_parts(parts, axum::body::Body::empty()),
            bytes,
        )
    }

    #[tokio::test]
    async fn a_range_on_the_hex_branch_answers_206_with_exactly_those_bytes() {
        let (state, _tmp, hash, video, _) = ranged_fixture().await;
        let path = format!("/api/v1/blob/{}", hex::encode(hash.digest()));
        let (resp, body) = get_blob(&state, &path, Some("bytes=0-3")).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header(&resp, "content-range"), "bytes 0-3/64");
        assert_eq!(header(&resp, "accept-ranges"), "bytes");
        assert_eq!(body, &video[0..=3]);
        // The 200 path's hardening rides on the 206 unchanged.
        assert_eq!(header(&resp, "content-type"), "video/mp4");
        assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
        assert_eq!(header(&resp, "content-disposition"), "inline");

        let (resp, body) = get_blob(&state, &path, Some("bytes=-4")).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header(&resp, "content-range"), "bytes 60-63/64");
        assert_eq!(body, &video[60..]);
    }

    #[tokio::test]
    async fn a_range_past_the_end_answers_416_naming_the_total() {
        let (state, _tmp, hash, _, _) = ranged_fixture().await;
        let path = format!("/api/v1/blob/{}", hex::encode(hash.digest()));
        let (resp, body) = get_blob(&state, &path, Some("bytes=64-")).await;
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header(&resp, "content-range"), "bytes */64");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn no_range_answers_200_whole_body_advertising_accept_ranges() {
        let (state, _tmp, hash, video, _) = ranged_fixture().await;
        let path = format!("/api/v1/blob/{}", hex::encode(hash.digest()));
        let (resp, body) = get_blob(&state, &path, None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header(&resp, "accept-ranges"), "bytes");
        assert_eq!(body, video);

        // A multi-range (or malformed) header is ignored, never refused.
        let (resp, body) = get_blob(&state, &path, Some("bytes=0-1,5-6")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body, video);
    }

    /// The CID branch verifies `blake3(bytes) == cid.digest()` over the whole
    /// body; a range of unverified bytes would defeat it, so it ignores `Range`.
    #[tokio::test]
    async fn the_cid_branch_ignores_range_and_serves_the_verified_whole() {
        let (state, _tmp, hash, video, _) = ranged_fixture().await;
        let path = format!("/api/v1/blob/{}", hash.to_base32());
        let (resp, body) = get_blob(&state, &path, Some("bytes=0-3")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(header(&resp, "accept-ranges"), "");
        assert_eq!(body, video);
    }

    #[tokio::test]
    async fn a_range_with_thumb_ranges_the_thumbnail() {
        let (state, _tmp, hash, _, thumb) = ranged_fixture().await;
        let path = format!("/api/v1/blob/{}?thumb=1", hex::encode(hash.digest()));
        let (resp, body) = get_blob(&state, &path, Some("bytes=2-5")).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(header(&resp, "content-range"), "bytes 2-5/16");
        assert_eq!(header(&resp, "content-type"), "image/jpeg");
        assert_eq!(body, &thumb[2..=5]);
    }

    #[tokio::test]
    async fn a_withheld_blob_stays_451_under_range() {
        let (state, _tmp, hash, _, _) = ranged_fixture().await;
        state
            .db
            .replace_blob_legal_withhold(&[hash.digest()])
            .await
            .unwrap();
        let path = format!("/api/v1/blob/{}", hex::encode(hash.digest()));
        let (resp, body) = get_blob(&state, &path, Some("bytes=0-3")).await;
        assert_eq!(resp.status(), StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS);
        assert!(body.is_empty());
    }
}
