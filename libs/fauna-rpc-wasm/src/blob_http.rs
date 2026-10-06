//! `POST /api/v1/blob` multipart — the browser twin of native
//! `fauna_nest_http::NestContentApi::post_multipart_blob`.
//!
//! The nest's blob route is **bulk binary over HTTP**, the one carve-out from
//! the WS-RPC-everywhere directive (`docs/goal/architecture/api-layers.md`
//! § production HTTP carve-out). Its verifier
//! (`bins/fauna-nest/src/blob_routes.rs::upload_blob`) accepts exactly one body
//! shape — `multipart/form-data` with a `sidecar` part (DAG-CBOR
//! `UploadSidecar`) and a `bytes` part (the sealed blob) — and answers **400 to
//! anything else**, so a raw-body POST fails every time.
//!
//! Native has one implementation of that shape which every native uploader
//! shares (`fauna-nest-http`'s content API: conversation attachments, the media
//! `upload` gesture, `fauna_client::media_upload`). This module is its wasm
//! counterpart, so the browser has one too rather than one per caller — the
//! shape is easy to get subtly wrong, and getting it wrong fails only against a
//! real nest, which no mock-backed test reaches.
//!
//! ⚠ The multipart `Content-Type` (with its boundary) is set by the **browser**
//! from the `FormData` body. Never set that header explicitly: a hand-written
//! value carries no boundary and the nest's multipart parser rejects the body.

use wasm_bindgen::JsValue;

use crate::client::WsRpcClient;

/// The nest's bulk-binary upload path. One owner of the literal on wasm; the
/// native twin is `fauna_nest_http::paths::blob::UPLOAD`.
pub const BLOB_UPLOAD_PATH: &str = "/api/v1/blob";

/// `POST /api/v1/chunks` — the content-addressed chunk store's write route
/// (one chunk body under its `X-Content-Hash` store key); the browser spelling
/// of native `fauna_nest_http::paths::chunk_store::CHUNKS_UPLOAD`, which the
/// wasm graph does not link.
pub const CHUNKS_UPLOAD_PATH: &str = "/api/v1/chunks";

/// `POST /api/v1/manifests` — store a canonical `ChunkManifest`; the twin of
/// `fauna_nest_http::paths::chunk_store::MANIFESTS_UPLOAD`.
pub const MANIFESTS_UPLOAD_PATH: &str = "/api/v1/manifests";

/// Why a multipart blob upload did not return a body.
///
/// Deliberately transport-shaped rather than domain-shaped: each caller maps it
/// into its own page-level taxonomy (`MediaApiError`'s NotFound/BadRequest/
/// Transient split, `ConvRpcError::transient`), exactly as the native callers
/// map `fauna-nest-http`'s `ApiError`.
#[derive(Debug, thiserror::Error)]
pub enum BlobHttpError {
    /// The request never produced an HTTP response — bearer fetch, `FormData`
    /// construction, or the `fetch` itself failed. `context` names which.
    #[error("{context}: {detail}")]
    Transport {
        /// Which step failed, for a legible message at the call site.
        context: &'static str,
        /// The underlying error, stringified.
        detail: String,
    },
    /// The nest answered with a non-2xx status. `message` is the response body
    /// (the nest's `ApiError` JSON), empty if it could not be read.
    #[error("HTTP {status}: {message}")]
    Status {
        /// The HTTP status code the nest returned.
        status: u16,
        /// The response body, for the failing call site's message.
        message: String,
    },
}

/// How much of a non-2xx body [`BlobHttpError::Status`] ever carries — the
/// browser twin of native `fauna_nest_http::content::MAX_ERROR_BODY_BYTES`, and
/// the same value; keep the two equal. The responder can be a nest someone else
/// runs (a conversation's home nest), so it does not choose how much text this
/// client carries into its errors and logs.
///
/// Honest about what it bounds: in a browser the platform has already received
/// the response by the time `text()` resolves, so this caps what the client
/// holds and passes on, not the transfer. Native streams and stops reading.
pub const MAX_ERROR_BODY_BYTES: usize = 4096;

/// `text` cut to at most [`MAX_ERROR_BODY_BYTES`], on a char boundary.
fn capped(mut text: String) -> String {
    if text.len() > MAX_ERROR_BODY_BYTES {
        let mut end = MAX_ERROR_BODY_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

impl BlobHttpError {
    /// The HTTP status, when the nest actually answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            BlobHttpError::Status { status, .. } => Some(*status),
            BlobHttpError::Transport { .. } => None,
        }
    }
}

/// POST `sealed_bytes` + its DAG-CBOR `sidecar_cbor` to `path` on the nest the
/// `WsRpcClient` is connected to, as the two-part multipart body the nest
/// requires. Returns the response body (the nest replies
/// `{"hash": "<64 hex>"}`); callers that need the hash parse it, callers that
/// only need the upload to have happened discard it.
///
/// Rides the SPA's existing authenticated session — the client's own single
/// origin ([`WsRpcClient::nest_url`]) and JS-provided bearer
/// ([`WsRpcClient::bearer`]) — so there is no parallel token. This is the role
/// native's `ReqwestNestContentApi` plays, built from the same auth handle.
pub async fn post_multipart_blob(
    nest: &WsRpcClient,
    path: &str,
    sidecar_cbor: &[u8],
    sealed_bytes: &[u8],
) -> Result<String, BlobHttpError> {
    let token = nest
        .bearer(false)
        .await
        .map_err(|e| BlobHttpError::Transport {
            context: "blob upload bearer",
            detail: e.to_string(),
        })?;
    post_multipart_blob_with_bearer(&nest.nest_url(), &token, path, sidecar_cbor, sealed_bytes)
        .await
}

/// POST a raw `application/octet-stream` `body` to `path` on the nest the
/// `WsRpcClient` is connected to, under **`X-Content-Hash: content_hash_hex`**
/// — the content-addressed chunk store's write shape (`POST /api/v1/chunks`
/// keyed by the store key; `POST /api/v1/manifests` with `None`, which is
/// keyed by the nest's own hash of the body). Returns the response body (the
/// nest replies `{"hash": "<64 hex>"}`). The browser twin of native
/// `NestContentApi::post_bytes_keyed`; rides the same session bearer as
/// [`post_multipart_blob`].
pub async fn post_octets_keyed(
    nest: &WsRpcClient,
    path: &str,
    content_hash_hex: Option<&str>,
    body: &[u8],
) -> Result<String, BlobHttpError> {
    let token = nest
        .bearer(false)
        .await
        .map_err(|e| BlobHttpError::Transport {
            context: "chunk upload bearer",
            detail: e.to_string(),
        })?;
    let url = format!("{}{path}", nest.nest_url().trim_end_matches('/'));
    let mut req = gloo_net::http::Request::post(&url)
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/octet-stream");
    if let Some(hash_hex) = content_hash_hex {
        req = req.header("X-Content-Hash", hash_hex);
    }
    let resp = req
        .body(js_sys::Uint8Array::from(body))
        .map_err(|e| BlobHttpError::Transport {
            context: "chunk upload body",
            detail: e.to_string(),
        })?
        .send()
        .await
        .map_err(|e| BlobHttpError::Transport {
            context: "chunk upload POST",
            detail: e.to_string(),
        })?;

    let status = resp.status();
    if !(200..300).contains(&status) {
        let message = capped(resp.text().await.unwrap_or_default());
        return Err(BlobHttpError::Status { status, message });
    }
    resp.text().await.map_err(|e| BlobHttpError::Transport {
        context: "chunk upload read reply",
        detail: e.to_string(),
    })
}

/// [`post_multipart_blob`] against an explicit `base_url` + bearer — the
/// cross-nest arm: a foreign conversation member POSTs a sealed attachment
/// DIRECT to the room's **home** nest under the short-lived write token its own
/// nest relayed (`fauna.conversations.blob.write_token.get`; the nest's
/// `BulkWriteAuth` arm accepts it on the same door a session bearer uses).
/// The session-bearer helper above delegates here.
pub async fn post_multipart_blob_with_bearer(
    base_url: &str,
    bearer: &str,
    path: &str,
    sidecar_cbor: &[u8],
    sealed_bytes: &[u8],
) -> Result<String, BlobHttpError> {
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let token = bearer;

    // `multipart/form-data` with exactly the two parts the nest keys on by name
    // — `sidecar` (DAG-CBOR) + `bytes` (sealed) — matching native
    // `post_multipart_blob`. Only the bearer header is set explicitly; the
    // browser derives the multipart Content-Type + boundary from the FormData.
    let form = build_multipart(sidecar_cbor, sealed_bytes)?;
    let resp = gloo_net::http::Request::post(&url)
        .header("authorization", &format!("Bearer {token}"))
        .body(form)
        .map_err(|e| BlobHttpError::Transport {
            context: "blob upload body",
            detail: e.to_string(),
        })?
        .send()
        .await
        .map_err(|e| BlobHttpError::Transport {
            context: "blob upload POST",
            detail: e.to_string(),
        })?;

    let status = resp.status();
    if !(200..300).contains(&status) {
        let message = capped(resp.text().await.unwrap_or_default());
        return Err(BlobHttpError::Status { status, message });
    }
    resp.text().await.map_err(|e| BlobHttpError::Transport {
        context: "blob upload read reply",
        detail: e.to_string(),
    })
}

/// Build the two-part `multipart/form-data` body — `sidecar`
/// (`application/cbor`) + `bytes` (`application/octet-stream`), the same MIMEs
/// the native reqwest form declares. The nest keys parts by name and ignores
/// the declared MIME; we set it to stay byte-for-byte uniform with native.
fn build_multipart(
    sidecar_cbor: &[u8],
    sealed_bytes: &[u8],
) -> Result<web_sys::FormData, BlobHttpError> {
    let form = web_sys::FormData::new().map_err(js_err)?;
    append_part(&form, "sidecar", sidecar_cbor, "application/cbor")?;
    append_part(&form, "bytes", sealed_bytes, "application/octet-stream")?;
    Ok(form)
}

/// Append one binary part as a typed `Blob` (a `Uint8Array` is a valid
/// `BlobPart`).
fn append_part(
    form: &web_sys::FormData,
    name: &str,
    bytes: &[u8],
    mime: &str,
) -> Result<(), BlobHttpError> {
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array);
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let blob =
        web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(js_err)?;
    form.append_with_blob(name, &blob).map_err(js_err)?;
    Ok(())
}

fn js_err(e: JsValue) -> BlobHttpError {
    BlobHttpError::Transport {
        context: "blob upload multipart",
        detail: format!("{e:?}"),
    }
}
