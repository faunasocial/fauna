//! HTTP handlers for chunk and manifest upload/download.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::routes::{AppState, parse_32_bytes as parse_hash};

#[derive(Deserialize)]
pub struct CheckChunksRequest {
    pub hashes: Vec<String>,
}

/// Resolve and VERIFY the content-addressed storage key (the chunk's *plaintext*
/// BLAKE3 — what a manifest references) for an uploaded body.
///
/// The chunk store is a single global content-addressed store shared across all
/// users (dedup via `/api/v1/chunks/check`), so storing arbitrary bytes under a
/// caller-asserted hash would let one user pre-seed a hash a victim's manifest
/// references (poisoning / dedup confusion). We therefore never
/// trust `X-Content-Hash` without proof:
///
/// * **header absent** ⇒ the body *is* the plaintext, so the key is
///   `blake3(body)` by construction — already content-addressed, nothing to forge.
/// * **header present** ⇒ the header names the store key, and one of two
///   proofs must hold (both preimage-hard to forge for a chosen victim hash):
///   **(a)** `blake3(body) == header` — the key addresses the stored bytes
///   themselves. This is the live path for every SEALED upload: a sealed
///   chunk rests under its ciphertext hash (`fauna_core::chunk_seal`; the sync
///   engine, the sync daemon, the Go byte plane and the mail body staging all
///   send it), so it is not a compatibility arm. **(b)**
///   `blake3(decompress(body)) == header` — a framed plaintext chunk (the
///   public-audience shape) rests under its plaintext hash.
///
/// Returns the verified hash, or a `400` response on a malformed header /
/// hash mismatch.
fn resolve_verified_chunk_hash(
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Result<[u8; 32], axum::response::Response> {
    let Some(hdr) = headers.get("X-Content-Hash") else {
        return Ok(*blake3::hash(body).as_bytes());
    };
    let claimed: [u8; 32] = match hdr
        .to_str()
        .ok()
        .and_then(|s| fauna_core::hex32::decode(s).ok())
    {
        Some(arr) => arr,
        None => return Err(ApiError::bad_request("invalid X-Content-Hash header").into_response()),
    };

    // (a) The key addresses the body itself — every sealed (ciphertext) upload.
    if *blake3::hash(body).as_bytes() == claimed {
        return Ok(claimed);
    }
    // (b) Framed plaintext: the body unframes (bomb-bounded) to the keyed plaintext.
    if let Ok(plaintext) = fauna_core::compress::unframe_strict_bounded(
        body,
        fauna_core::compress::MAX_DECOMPRESSED_CHUNK,
    ) && *blake3::hash(&plaintext).as_bytes() == claimed
    {
        return Ok(claimed);
    }

    tracing::warn!(
        claimed = hex::encode(claimed),
        "chunk upload: X-Content-Hash matches neither the body nor its decompression"
    );
    Err(ApiError::bad_request("X-Content-Hash does not match chunk content").into_response())
}

/// POST /api/v1/chunks -- Upload a chunk, returns its content hash.
///
/// If the client sends an `X-Content-Hash` header, that hash is the storage key
/// (the manifest references the plaintext hash, so the nest must store under it,
/// not the hash of the possibly-compressed body). The header is verified against
/// the body — see [`resolve_verified_chunk_hash`].
pub async fn upload_chunk(
    State(state): State<Arc<AppState>>,
    _auth: crate::auth::BulkWriteAuth,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    // Determine + verify the content-addressed storage key for the body.
    let hash_bytes: [u8; 32] = match resolve_verified_chunk_hash(&headers, &body) {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    let to_store = match crate::backup::encode_blob(
        &body,
        backup_svc.encryption_key(),
        backup_svc.compression(),
    ) {
        Ok(data) => data,
        Err(e) => {
            tracing::error!("chunk encode error: {e}");
            return ApiError::internal("encryption error").into_response();
        }
    };

    if let Err(e) = store.put(&hash, &to_store).await {
        tracing::error!("chunk upload error: {e}");
        return ApiError::internal("storage error").into_response();
    }
    tracing::info!(
        hash = hex::encode(hash_bytes),
        size = body.len(),
        "chunk stored"
    );

    // The metadata row is the blob's only durable trace until a reference row
    // lands: without it GC keeps the blob forever (missing-metadata-keeps) and
    // the `__index` purge's keep-check can't see it. Fail the request rather
    // than ACK an upload with no durable trace — the store is content-addressed,
    // so the client's retry re-puts idempotently and re-attempts the row.
    if let Err(e) = state
        .db
        .put_blob_metadata(&hash_bytes, body.len() as i64, "chunk", None, None)
        .await
    {
        tracing::error!("failed to record chunk metadata: {e}");
        return ApiError::internal("storage error").into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({ "hash": hex::encode(hash_bytes) })),
    )
        .into_response()
}

/// Query half of `GET /api/v1/chunks/{hash}` — the optional folder hint
/// (`fauna_nest_http::paths::chunk_store::FOLDER_HINT_PARAM`).
#[derive(Deserialize, Default)]
pub struct ChunkDownloadQuery {
    /// The folder the requester is reading. Consulted **only on a store
    /// miss**, where it names which seats may hold the bytes and whether the
    /// relay may cache them (the folder's content residency).
    #[serde(default)]
    pub folder: Option<String>,
    /// The same folder's address, hex of its `set_name_hash`
    /// (`fauna_nest_http::paths::chunk_store::FOLDER_HASH_HINT_PARAM`). When
    /// present it is the hint — a sealed set's name rests blank on the nest,
    /// so the plaintext above cannot find it (`path-sealing.md` § the set-name
    /// plane).
    #[serde(default)]
    pub folder_hash: Option<String>,
}

impl ChunkDownloadQuery {
    /// Whether the request hints a folder at all — either form arms the relay.
    fn hints_a_folder(&self) -> bool {
        self.folder.is_some() || self.folder_hash.is_some()
    }
}

/// GET /api/v1/chunks/:hash -- Download a chunk by content hash.
///
/// Two arms. The **hit** arm is the public content-addressed read every
/// consumer rides (no bearer; integrity is the address, confidentiality the
/// sealing). The **miss** arm is `404` — unless the request carries a folder
/// hint, in which case it becomes the phase-5 **relay read path**
/// (`file-sync.md` § Content residency, gate 3): the bytes are pulled from a
/// connection that announced the folder for relay serving (`file-sync.md`
/// § Relay serving —
/// the owner's or a roster member's, answering on
/// [`answer_relay_chunk`]), and served to this
/// requester; for a metadata-only folder they are served **without** being
/// written to the blob store, for a full one they are cached as the resolver
/// always did. The hinted arm is actor-scoped — it needs a bearer that names
/// its caller (`401` without one): a session bearer, whose folder is resolved
/// owner-or-member through the same `folder_authz::resolve_readable_folder`
/// every folder read uses; or the byte-plane token a member on another nest
/// was minted (`crate::auth::relay_reader`), whose folder is resolved through
/// the cross-nest roster alone, at every request
/// (`folder_authz::resolve_foreign_readable_folder`). Either way a hint cannot
/// make the nest probe a stranger's seats for a hash (`404`,
/// indistinguishable from a plain miss).
///
/// **Withheld digests answer 451 before either arm runs.** This is the blob
/// store's *third* door, and `moderation.md` § Legal takedown → *The
/// blob-serve door* withholds a taken-down record's blobs from the store, not
/// from one route onto it. A taken-down post's photo, thumbnail and gated
/// body, and a video's manifest and every segment, are all in that set
/// (`Post::blob_refs`), so without this gate the takedown was bypassed by a
/// path-prefix substitution on the very hex digest the requester already
/// holds: `/api/v1/blob/<hex>` and `/api/v1/video/segments/<hex>` answered 451
/// while this route — unauthenticated, open CORS — answered 200.
///
/// The gate precedes the store read **and** the relay arm, and that ordering
/// is the load-bearing half: the relay pulls a missing chunk from a seat
/// that announced the folder and, on a full-residency folder, `store.put`s it back,
/// so a gate placed after it would let this door re-import compelled bytes
/// onto a box that no longer had them.
///
/// It shares `blob_routes::legal_takedown_gate` rather than restating the
/// posture, so the answer shape cannot drift between the doors. **A
/// consequence worth naming here:** this is the file-sync data plane and the
/// store is content-addressed, so a legitimate sync of a chunk whose bytes
/// dedup to a withheld digest is answered 451 too — the withhold is keyed on
/// the bytes, which is the rule, not an edge of it.
pub async fn download_chunk(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ChunkDownloadQuery>,
    headers: axum::http::HeaderMap,
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
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    // Before the store read and before the relay arm — see this fn's doc.
    if let Some(resp) = crate::blob_routes::legal_takedown_gate(&state.db, &hash_bytes).await {
        return resp;
    }

    tracing::debug!(hash = %hash_hex, "chunk download request");
    match store.get(&hash).await {
        Ok(Some(data)) => {
            // Decode the nest's at-rest `encode_blob` framing (symmetric with
            // `upload_chunk`'s encode and with `download_manifest`), returning the
            // exact bytes the client uploaded. Because `decode_blob ∘ encode_blob`
            // is the identity, this is byte-identical to what the client sent — the
            // client then applies its own decompression/decryption (it owns the
            // chunk's encoding, e.g. the engine's `compress_chunk` prefix). Reading
            // the stored bytes *verbatim* would leak the nest's self-describing
            // prefix into the response and corrupt the client's decode.
            let decoded = match crate::backup::decode_blob(&data, backup_svc.encryption_key()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("chunk decode error: {e}");
                    return ApiError::internal("decode error").into_response();
                }
            };
            (
                StatusCode::OK,
                [("content-type", "application/octet-stream")],
                decoded,
            )
                .into_response()
        }
        Ok(None) if query.hints_a_folder() => {
            relay_chunk_for_folder(&state, &hash, &query, &headers).await
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("chunk download error: {e}");
            ApiError::internal("storage error").into_response()
        }
    }
}

/// The store-miss relay arm of [`download_chunk`] (see its doc for the
/// contract). Resolves the bearer → actor, the hint → a folder row the actor
/// may read, the row's residency → the cache policy, then asks the resolver
/// for the bytes from the connections that announced the row.
async fn relay_chunk_for_folder(
    state: &Arc<AppState>,
    hash: &fauna_core::data::ContentHash,
    hint: &ChunkDownloadQuery,
    headers: &axum::http::HeaderMap,
) -> axum::response::Response {
    use crate::chunk_relay::RelayCache;

    let Some(token) = crate::auth::bearer_token_from_headers(headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(reader) = crate::auth::relay_reader(state, token).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // The hash hint wins; a malformed one names no folder (the same `404` as
    // an unreadable one), never a fall back to the plaintext.
    let name_hash: Option<[u8; 32]> = match hint.folder_hash.as_deref() {
        None => None,
        Some(hex_hash) => match parse_hash(hex_hash) {
            Some(h) => Some(h),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let name = hint.folder.as_deref().unwrap_or("");
    // Which roster names the caller follows how it authenticated, and the two
    // never mix: a session reads through the owner-or-member resolver, a
    // cross-nest member's token through the cross-nest roster alone — read
    // here, per request, so a removal ends that member's reads whatever life
    // its token has left.
    let resolved = match &reader {
        crate::auth::RelayReader::Session(actor) => {
            crate::folder_authz::resolve_readable_folder(
                &state.db,
                name,
                name_hash.as_ref(),
                &actor.0,
            )
            .await
        }
        crate::auth::RelayReader::ForeignMember(actor) => {
            crate::folder_authz::resolve_foreign_readable_folder(
                &state.db,
                name,
                name_hash.as_ref(),
                &actor.0,
            )
            .await
        }
    };
    let row = match resolved {
        Ok(Some(row)) => row,
        Ok(None) => {
            tracing::debug!("chunk relay: hint names no folder the caller may read");
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(e) => {
            tracing::error!("chunk relay: folder lookup error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    let owner: [u8; 32] = match row.actor_id.as_slice().try_into() {
        Ok(o) => o,
        Err(_) => return ApiError::internal("folder owner id is not 32 bytes").into_response(),
    };
    // Fail-closed to FULL, like every other residency read (`folder_handlers::
    // residency_of`): only an explicit, parsed `metadata_only` withholds the
    // cache write. An unparseable value must never stop bytes resting.
    let mut cache = if row.nest_content_residency.as_deref() == Some("metadata_only") {
        RelayCache::Transient
    } else {
        RelayCache::Store
    };
    // **the hinted folder's residency is not the CHUNK's
    // residency** unless the answer says so. This route authorizes on a folder
    // and fetches on a hash, so a caller who may read a `full` folder `F` of
    // this owner could hint `F` for a hash belonging to the owner's
    // `metadata_only` folder `P`, and the `Store` arm would rest `P`'s chunk on
    // the nest permanently — with the unauthenticated hit arm then serving it
    // to anyone holding the hash, while the owner's UI still reports `P` as
    // metadata-only. Content addressing makes such a hash easy to come by (a
    // file shared in `F`, later removed but kept in `P`, has the same hash).
    //
    // So an owner who holds a metadata-only folder anywhere gets
    // `StoreIfAttributed`: the bytes rest only when the answer is attributed
    // to the hinted folder (`chunk_relay::SeatAnswer`). An announced
    // seat's answer is attributed by construction — the ask names the folder
    // and the seat answers from that folder's state alone — so the
    // re-hydration cache holds through it; an unattributed answer falls to *do
    // not rest*, never to `Store` (`principles.md` § The user always controls
    // their data). Serving is untouched either way. `unwrap_or(true)` is the
    // same direction: a read that failed cannot rule the chunk out, so it
    // takes the attribution-gated arm rather than resting bytes on an
    // unanswered question. The cost of being wrong here is a re-relay; the
    // cost the other way is permanent.
    if cache == RelayCache::Store
        && state
            .db
            .actor_has_metadata_only_folder(&owner)
            .await
            .unwrap_or(true)
    {
        tracing::debug!(
            folder_id = row.id,
            "chunk relay: owner holds a metadata-only folder — caching only if the \
             answer is attributed to this folder"
        );
        cache = RelayCache::StoreIfAttributed;
    }
    let mut announced = match announced_seats_for(state, &row, &owner).await {
        Ok(seats) => seats,
        Err(e) => {
            tracing::error!("chunk relay: announced-seat lookup error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    };
    match foreign_seats_for(state, &row).await {
        Ok(seats) => announced.extend(seats),
        Err(e) => {
            tracing::error!("chunk relay: foreign-seat lookup error: {e}");
            return ApiError::internal("storage error").into_response();
        }
    }
    match state
        .sync
        .chunk_resolver
        .relay_for_folder(hash, row.id, announced, cache)
        .await
    {
        Ok(Some(bytes)) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            bytes,
        )
            .into_response(),
        // No live holder, the holder had no such chunk, or it answered with
        // bytes that are not the address's preimage — all read as absent.
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("chunk relay error: {e}");
            ApiError::internal("storage error").into_response()
        }
    }
}

/// The connections that announced `row` (`fauna.sync.serve.announce`), as the
/// relay's announced candidates (`file-sync.md` § Relay serving).
///
/// The announce was admitted against the row when it was made; a member may
/// have left the roster since, and the connection keeps its announce for its
/// whole life. So each non-owner actor is re-checked here, on the same gate the
/// announce used — `can_read_folder`'s owner and member arms, never the admin
/// discovery grant — before it is asked anything: a store key is not sent to an
/// actor who can no longer read the folder.
async fn announced_seats_for(
    state: &Arc<AppState>,
    row: &crate::db::FolderRow,
    owner: &[u8; 32],
) -> anyhow::Result<Vec<crate::chunk_relay::AnnouncedSeat>> {
    use crate::folder_authz::FolderReadGrant;

    let conns = state.ws.announced_for_folder(row.id);
    let folder_ref = fauna_core::folder_keys::FolderRef::Local(row.id).to_wire();
    let mut verdicts: std::collections::HashMap<[u8; 32], bool> = Default::default();
    let mut seats = Vec::with_capacity(conns.len());
    for conn in conns {
        let actor = conn.actor_id;
        let holds = if actor == *owner {
            true
        } else if let Some(v) = verdicts.get(&actor) {
            *v
        } else {
            let v = matches!(
                crate::folder_authz::can_read_folder(&state.db, row, &actor).await?,
                Some(FolderReadGrant::Owner | FolderReadGrant::Member)
            );
            verdicts.insert(actor, v);
            v
        };
        if holds {
            seats.push(crate::chunk_relay::AnnouncedSeat {
                actor,
                via: crate::chunk_relay::SeatVia::Connection(conn),
                folder_ref: folder_ref.clone(),
            });
        }
    }
    Ok(seats)
}

/// The foreign seats leased for `row` (`fauna.federation.folder.serve.announce`),
/// as the relay's third candidate kind (`file-sync.md` § Relay serving → *A
/// member on another nest*, step (3)).
///
/// Each is re-checked on the write gate it was leased under — the member still
/// bound to the leasing nest, still holding `writer` — and a seat that fails it
/// is dropped from the table here, so a removal or a demotion ends it at the
/// next ask. A passing seat is asked through its member's nest; the answer it
/// gives comes back on the answer route under its own actor, the one actor
/// whose answer the ask takes.
async fn foreign_seats_for(
    state: &Arc<AppState>,
    row: &crate::db::FolderRow,
) -> anyhow::Result<Vec<crate::chunk_relay::AnnouncedSeat>> {
    let table = state.sync.chunk_resolver.foreign_seats();
    let mut seats = Vec::new();
    for seat in table.live_for_folder(row.id) {
        if !crate::federation_handlers::foreign_seat_still_admitted(state, &seat).await? {
            table.drop_seat(&seat);
            continue;
        }
        let actor = seat.member;
        let folder_ref = fauna_core::folder_keys::FolderRef::Foreign(seat.channel_id).to_wire();
        let state = Arc::clone(state);
        seats.push(crate::chunk_relay::AnnouncedSeat {
            actor,
            via: crate::chunk_relay::SeatVia::Forwarded(Box::new(move |request_id, store_key| {
                Box::pin(async move {
                    crate::federation_pool::originate_folder_chunk_wanted(
                        &state.federation_pool,
                        &state,
                        &seat,
                        request_id,
                        store_key,
                    )
                    .await
                })
            })),
            folder_ref,
        });
    }
    Ok(seats)
}

/// `POST /api/v1/chunks/relay/{request_id}` — an announced seat's answer to a
/// `fauna.sync.chunk.wanted` ask: the stored chunk as the body
/// (`file-sync.md` § Relay serving, step (3);
/// `fauna_nest_http::paths::chunk_store::chunk_relay_answer`).
///
/// Same auth and body limit as `POST /api/v1/chunks` (`BulkWriteAuth`, which
/// takes a session bearer or a bulk write token and yields the **actor**), and
/// the actor is the whole of the check: the answer is taken only for a pending
/// ask that went to this actor, compared by actor id — no session is required,
/// which is what lets a member's write token answer for its seat. `204` when
/// taken; `404` for an unknown, expired, answered or other-actor request id,
/// touching nothing. The bytes are checked against the store key by the walk
/// that asked, before they are served or rested.
pub async fn answer_relay_chunk(
    State(state): State<Arc<AppState>>,
    auth: crate::auth::BulkWriteAuth,
    Path(request_id): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    take_relay_answer(&state, &auth, &request_id, Some(body.to_vec())).await
}

/// `DELETE /api/v1/chunks/relay/{request_id}` — the asked seat holds no such
/// chunk, so the relay's window refills at once instead of at the fetch
/// deadline. Same auth and refusals as [`answer_relay_chunk`].
pub async fn decline_relay_chunk(
    State(state): State<Arc<AppState>>,
    auth: crate::auth::BulkWriteAuth,
    Path(request_id): Path<String>,
) -> impl IntoResponse {
    take_relay_answer(&state, &auth, &request_id, None).await
}

async fn take_relay_answer(
    state: &Arc<AppState>,
    auth: &crate::auth::BulkWriteAuth,
    request_id: &str,
    data: Option<Vec<u8>>,
) -> axum::response::Response {
    use crate::chunk_relay::AnnouncedAnswer;

    // An id that does not parse names no ask — the same `404` as one that
    // names nobody's.
    let Ok(request_id) = request_id.parse::<u64>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match state
        .sync
        .chunk_resolver
        .answer_announced(request_id, &auth.0.0, data)
        .await
    {
        AnnouncedAnswer::Taken => StatusCode::NO_CONTENT.into_response(),
        AnnouncedAnswer::Refused => StatusCode::NOT_FOUND.into_response(),
    }
}

/// POST /api/v1/chunks/check -- Check which chunks exist.
pub async fn check_chunks(
    State(state): State<Arc<AppState>>,
    _auth: crate::auth::BulkWriteAuth,
    Json(req): Json<CheckChunksRequest>,
) -> impl IntoResponse {
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    let mut hashes = Vec::with_capacity(req.hashes.len());
    let mut hex_strings = Vec::with_capacity(req.hashes.len());
    for h in &req.hashes {
        match parse_hash(h) {
            Some(bytes) => {
                hashes.push(fauna_core::data::ContentHash::from_digest_raw(bytes));
                hex_strings.push(h.clone());
            }
            None => return ApiError::bad_request("invalid hash hex").into_response(),
        }
    }

    match store.exists_batch(&hashes).await {
        Ok(exists) => {
            let missing: Vec<&str> = hex_strings
                .iter()
                .zip(exists.iter())
                .filter(|(_, e)| !**e)
                .map(|(h, _)| h.as_str())
                .collect();
            Json(json!({ "missing": missing })).into_response()
        }
        Err(e) => {
            tracing::error!("check chunks error: {e}");
            ApiError::internal("storage error").into_response()
        }
    }
}

/// POST /api/v1/manifests -- Upload a manifest, returns its content hash.
pub async fn upload_manifest(
    State(state): State<Arc<AppState>>,
    _auth: crate::auth::BulkWriteAuth,
    body: Bytes,
) -> impl IntoResponse {
    let backup_svc = match &state.backup_service {
        Some(svc) => svc,
        None => return (StatusCode::SERVICE_UNAVAILABLE, "backup not configured").into_response(),
    };
    let store = backup_svc.local_blob_store();

    // The door: a manifest route body MUST be a canonical `ChunkManifest`.
    // Without this the route hashes ARBITRARY bytes and stamps them
    // `content_type = "manifest"`, so the type is a client-assertable lie and
    // GC's reachability walk inherits an undecodable "manifest" it can only
    // fail-close on. Validating here makes *"a manifest-typed blob really is a
    // manifest"* true by construction.
    //
    // Every legitimate writer already sends exactly this, so nothing is turned
    // away — traced before adding the guard: shared Rust's `serialize_manifest`
    // (the `SyncEngine` + the backup coordinator's `upload_bytes` via
    // `nest_client::upload_manifest`; apple `SyncEngine.swift` and android
    // `ChunkedSyncEngine.kt` through the FFI; the Go WebDAV bridge through
    // `faunaFfi.SerializeManifest`), plus the `fauna-sync` daemon, which
    // `canonical_decode`s the very bytes it pushes. Web uploads no manifests.
    //
    // The body is the PLAINTEXT manifest (`encode_blob` seals it below), so this
    // is a plain `canonical_decode` — not GC's `decode_manifest`, which reverses
    // `encode_blob` first.
    //
    // NOTE: this is necessary but NOT sufficient to stop a client from poisoning
    // GC's fail-close gate — a client can still point `changes.record` at bytes
    // it uploaded via `/api/v1/chunks`, and GC classifies by reference class, not
    // by `content_type`. Closing that needs record-time validation; see
    // `backup-restore.md` § 9.
    if fauna_core::encoding::canonical_decode::<fauna_core::chunk::ChunkManifest>(&body).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "body is not a canonical ChunkManifest",
        )
            .into_response();
    }

    // Hash is always computed on the plaintext.
    let hash_bytes: [u8; 32] = *blake3::hash(&body).as_bytes();
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    let to_store = match crate::backup::encode_blob(
        &body,
        backup_svc.encryption_key(),
        backup_svc.compression(),
    ) {
        Ok(data) => data,
        Err(e) => {
            tracing::error!("manifest encode error: {e}");
            return ApiError::internal("encryption error").into_response();
        }
    };

    if let Err(e) = store.put(&hash, &to_store).await {
        tracing::error!("manifest upload error: {e}");
        return ApiError::internal("storage error").into_response();
    }

    // Same no-false-ACK contract as `upload_chunk` above: the metadata row is
    // the manifest's only durable trace until a reference row lands, so a
    // failed write must fail the request, not ACK a blob GC can only
    // over-retain and the `__index` purge's keep-check can't see.
    if let Err(e) = state
        .db
        .put_blob_metadata(&hash_bytes, body.len() as i64, "manifest", None, None)
        .await
    {
        tracing::error!("failed to record manifest metadata: {e}");
        return ApiError::internal("storage error").into_response();
    }

    (
        StatusCode::CREATED,
        Json(json!({ "hash": hex::encode(hash_bytes) })),
    )
        .into_response()
}

/// GET /api/v1/manifests/:hash -- Download a manifest by content hash.
///
/// **Withheld digests answer 451 before the store read** — the blob store's
/// *fourth* door, gated for the reason [`download_chunk`] spells out. A video
/// post's manifest hash is in the withheld set in its own right
/// (`Post::blob_refs`), so a takedown that shut the other three doors was
/// bypassed by asking for the manifest here.
pub async fn download_manifest(
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
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);

    if let Some(resp) = crate::blob_routes::legal_takedown_gate(&state.db, &hash_bytes).await {
        return resp;
    }

    match store.get(&hash).await {
        Ok(Some(data)) => {
            let decoded = match crate::backup::decode_blob(&data, backup_svc.encryption_key()) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("manifest decode error: {e}");
                    return ApiError::internal("decode error").into_response();
                }
            };
            (
                StatusCode::OK,
                [("content-type", "application/octet-stream")],
                decoded,
            )
                .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("manifest download error: {e}");
            ApiError::internal("storage error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use fauna_core::compress::compress_chunk;

    fn headers_with_hash(hash: &[u8; 32]) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("X-Content-Hash", hex::encode(hash).parse().unwrap());
        h
    }

    /// F9: a compressed upload whose X-Content-Hash matches the plaintext is
    /// accepted and keyed by the plaintext hash.
    #[test]
    fn verified_compressed_chunk_accepted() {
        let plaintext = b"the quick brown fox ".repeat(300);
        let plaintext_hash = *blake3::hash(&plaintext).as_bytes();
        let body = compress_chunk(&plaintext); // what the client uploads

        let got = resolve_verified_chunk_hash(&headers_with_hash(&plaintext_hash), &body)
            .expect("honest compressed chunk accepted");
        assert_eq!(got, plaintext_hash, "stored under the plaintext hash");
    }

    /// F9 core: a body that does NOT hash (after decompression) to the claimed
    /// X-Content-Hash is rejected — the poisoning primitive is closed.
    #[test]
    fn forged_hash_rejected() {
        let real = compress_chunk(b"real chunk bytes");
        let victim_hash =
            *blake3::hash(b"a totally different chunk a victim references").as_bytes();
        // Attacker uploads `real` bytes but claims the victim's hash.
        assert!(
            resolve_verified_chunk_hash(&headers_with_hash(&victim_hash), &real).is_err(),
            "storing bytes under a non-matching hash must be rejected"
        );
    }

    /// Robustness: a RAW plaintext body (compression disabled) sent WITH a
    /// header — including one whose first byte coincides with a `0x00`/`0x01`
    /// framing marker — is accepted via the raw-hash path, not mis-stripped.
    #[test]
    fn raw_plaintext_with_framing_first_byte_accepted() {
        for first in [0x00u8, 0x01u8, 0x42u8] {
            let mut plaintext = vec![first];
            plaintext.extend_from_slice(b" raw chunk, no compression layer");
            let claimed = *blake3::hash(&plaintext).as_bytes();
            let got = resolve_verified_chunk_hash(&headers_with_hash(&claimed), &plaintext)
                .unwrap_or_else(|_| panic!("raw body starting 0x{first:02x} must be accepted"));
            assert_eq!(got, claimed);
        }
    }

    /// No header ⇒ keyed by blake3(body) (content-addressed by construction).
    #[test]
    fn no_header_keys_by_body_hash() {
        let body = b"uncompressed chunk";
        let got = resolve_verified_chunk_hash(&HeaderMap::new(), body).unwrap();
        assert_eq!(got, *blake3::hash(body).as_bytes());
    }

    /// A malformed hex header is rejected (no silent fallback that would let a
    /// caller dodge verification).
    #[test]
    fn malformed_header_rejected() {
        let mut h = HeaderMap::new();
        h.insert("X-Content-Hash", "not-hex".parse().unwrap());
        assert!(resolve_verified_chunk_hash(&h, b"body").is_err());
    }

    /// A zstd bomb body is rejected by the bounded decompress, not expanded.
    #[test]
    fn bomb_body_rejected() {
        let bomb = compress_chunk(&vec![0u8; 64 * 1024 * 1024]); // 64 MiB of zeros, > 16 MiB cap
        let any_hash = *blake3::hash(b"x").as_bytes();
        assert!(
            resolve_verified_chunk_hash(&headers_with_hash(&any_hash), &bomb).is_err(),
            "a chunk decompressing past the cap must be rejected"
        );
    }

    /// Harness for the no-false-ACK fault-injection pins: a real in-memory
    /// `CacheDb` + a real disk-backed `BackupService`, with the `blob_metadata`
    /// table renamed away so every `put_blob_metadata` write fails — the
    /// closest in-process stand-in for a mid-request sqlite write error.
    async fn metadata_faulted_state() -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().unwrap());
        let blob_dir = tempfile::tempdir().unwrap();
        let blob_path = blob_dir.path().to_path_buf();
        std::mem::forget(blob_dir); // outlive the handler call; never deleted under test
        let backup_svc = Arc::new(
            crate::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
                .unwrap(),
        );
        db.execute_batch("ALTER TABLE blob_metadata RENAME TO blob_metadata_fault;")
            .await
            .unwrap();
        Arc::new(AppState {
            backup_service: Some(backup_svc),
            ..AppState::for_test(db)
        })
    }

    fn test_auth() -> crate::auth::BulkWriteAuth {
        crate::auth::BulkWriteAuth(
            fauna_core::identity::ActorKeypair::from_secret([0x07; 32]).actor_id(),
        )
    }

    /// No-false-ACK contract, manifest arm: when the
    /// metadata row — the blob's only durable trace until a reference row
    /// lands — cannot be written, `POST /api/v1/manifests` must fail the
    /// request rather than ACK. The store is content-addressed, so the
    /// client's retry re-puts idempotently and re-attempts the row.
    #[tokio::test]
    async fn upload_manifest_fails_when_the_metadata_write_fails() {
        let state = metadata_faulted_state().await;
        let manifest = fauna_core::chunker::chunk_file(b"manifest fault-injection body");
        let body = Bytes::from(fauna_core::encoding::canonical_encode(&manifest).unwrap());
        let resp = upload_manifest(State(state), test_auth(), body)
            .await
            .into_response();
        assert!(
            resp.status().is_server_error(),
            "a failed metadata write must not ACK the manifest upload (got {})",
            resp.status()
        );
    }

    /// The same contract on the chunk arm (fixed; pinned so the arm can't silently revert to warn-and-ACK).
    #[tokio::test]
    async fn upload_chunk_fails_when_the_metadata_write_fails() {
        let state = metadata_faulted_state().await;
        let resp = upload_chunk(
            State(state),
            test_auth(),
            HeaderMap::new(),
            Bytes::from_static(b"chunk fault-injection body"),
        )
        .await
        .into_response();
        assert!(
            resp.status().is_server_error(),
            "a failed metadata write must not ACK the chunk upload (got {})",
            resp.status()
        );
    }
}
