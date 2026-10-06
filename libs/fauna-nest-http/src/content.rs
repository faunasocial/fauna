//! The bearer-authed REST surface to a fauna nest: the [`NestContentApi`]
//! trait + the production [`ReqwestNestContentApi`] impl.
//!
//! [`ReqwestNestContentApi`] is generic over a [`BearerSource`](crate::bearer::BearerSource)
//! and is the single chokepoint that owns:
//!
//! - attaching the bearer (via the source — `LaunchMachine`-driven or
//!   keypair-signed);
//! - the **401-reactive token refresh**: on a `401` it calls the source's
//!   `notify_401()` (an immediate refresh — the reactive path a TTL
//!   pre-expiry timer can't cover) and retries the request **once** with the
//!   fresh bearer. A second `401`, a refresh that fails, or a non-cloneable
//!   streaming body surfaces the *first* response — never an infinite retry;
//! - extracting the nest's structured `{"error": ...}` body on a non-2xx
//!   into [`ApiError::Status`].
//!
//! Path arguments are nest-relative (`paths::blob::UPLOAD`, `paths::blob::by_hash(h)`);
//! the impl prepends the base URL it was constructed with. Methods return the
//! raw response body as [`Bytes`] on success — the caller decodes (JSON,
//! DAG-CBOR, raw blob bytes, …).
//!
//! Lifted (and generalized over `BearerSource`) from
//! `apps/fauna-linux/src/nest_content_api/{mod,reqwest_impl}.rs`. **Cross-nest
//! traffic does NOT belong here** — key-package fetches / welcome / inbox
//! posts to a *remote* nest the user isn't authenticated to are unauthed or
//! signed-body, and that remote wouldn't accept this bearer; those keep using
//! a bare `reqwest::Client`. So do WebSocket connects (the bearer rides in the
//! upgrade, not an `Authorization` header) and unrelated services.
//!
//! Adding a method? Mirror it in [`ReqwestNestContentApi`]'s impl and in
//! [`crate::fake::FakeNestContentApi`], cover the wire contract in
//! `tests/content_round_trip.rs`, and add the path constant to [`crate::paths`].

use async_trait::async_trait;
use bytes::Bytes;

use crate::bearer::BearerSource;
use crate::error::ApiError;

/// Nest error contract: every nest handler renders failures as
/// `{"error": "<msg>"}` (`fauna_nest::api_error::ApiError`). Parsed with
/// serde, not ad-hoc string matching; falls back to the raw body for
/// off-contract upstreams (reverse-proxy / CDN HTML, empty body, …).
#[derive(serde::Deserialize)]
pub(crate) struct NestErrorBody {
    pub(crate) error: String,
}

/// The bearer-authed REST surface to a fauna nest. Path arguments are
/// nest-relative (see [`crate::paths`]); the impl prepends its base URL.
///
/// On success: the raw response body as [`Bytes`] (caller decodes). On
/// failure: [`ApiError::Status { code, message }`] for a non-2xx (the nest's
/// `{"error": ...}` text, or the raw body when off-contract), or
/// [`ApiError::Transport`] for everything else (no bearer, connection/TLS
/// failure, timeout, body-read error).
#[async_trait]
pub trait NestContentApi: Send + Sync {
    /// `GET {base}{path}` with the session bearer.
    async fn get(&self, path: &str) -> Result<Bytes, ApiError>;

    /// `GET {base}{path}` with query parameters and the session bearer.
    /// (Some callers — the bridge daemon's calendar sync, search — pass
    /// dynamic param lists; appending to the path string also works but
    /// loses percent-encoding.)
    async fn get_with_query(&self, path: &str, params: &[(&str, &str)]) -> Result<Bytes, ApiError>;

    /// `POST {base}{path}` with a JSON body and the session bearer.
    async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<Bytes, ApiError>;

    /// `POST {base}{path}` with a raw byte body, the given `Content-Type`
    /// (almost always `"application/octet-stream"`; calendar import uses
    /// `"text/calendar"`), and the session bearer.
    async fn post_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ApiError>;

    /// `POST {base}{path}` with a raw `application/octet-stream` body, the
    /// session bearer, **and `X-Content-Hash: {content_hash_hex}`** — the
    /// content-addressed chunk store's write shape
    /// ([`crate::paths::chunk_store::CHUNKS_UPLOAD`]): the header names the
    /// store key the nest must file the body under, and the nest verifies it
    /// against the body (`blake3(body)` for a sealed chunk) before storing —
    /// the F9 check every sync writer passes. The sync engine's own
    /// `upload_chunk` sends exactly this; the Media page's content-keyed upload
    /// reaches the same route through here.
    async fn post_bytes_keyed(
        &self,
        path: &str,
        content_hash_hex: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ApiError>;

    /// `POST {base}{path}` as `multipart/form-data` with exactly two parts —
    /// `sidecar` (DAG-CBOR `UploadSidecar`, `Content-Type: application/cbor`)
    /// and `bytes` (sealed primary bytes, `Content-Type:
    /// application/octet-stream`) — and the session bearer. This is the
    /// encrypted-mode blob-ingest wire shape (`POST /api/v1/blob`); the nest
    /// parses it in `bins/fauna-nest/src/blob_routes.rs::parse_multipart_upload`
    /// (spec tracked internally).
    ///
    /// Note: a multipart body is a stream, so the [`crate::ReqwestNestContentApi`]
    /// 401-retry can't `try_clone` it — a stale bearer surfaces the first
    /// response rather than retrying (the documented streaming-body behavior).
    /// The proactive TTL refresh keeps the bearer fresh on the common path.
    async fn post_multipart_blob(
        &self,
        path: &str,
        sidecar_cbor: Vec<u8>,
        sealed_bytes: Vec<u8>,
    ) -> Result<Bytes, ApiError>;

    /// `PUT {base}{path}` with a raw byte body, the given `Content-Type`
    /// (almost always `"application/octet-stream"`), and the session bearer.
    ///
    /// The PUT twin of [`post_bytes`](Self::post_bytes), for the routes that are
    /// idempotent under their own path — today `PUT /api/v1/blob/{cid}`
    /// ([`crate::paths::blob::by_cid`]), where the path *is* the content
    /// address, so re-sending the same bytes is a no-op rather than a second
    /// blob. Buffered (not streamed), so the 401-retry can `try_clone` it.
    async fn put_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ApiError>;

    /// `PUT {base}{path}` with a JSON body and the session bearer.
    async fn put_json(&self, path: &str, body: &serde_json::Value) -> Result<Bytes, ApiError>;

    /// `PATCH {base}{path}` with a JSON body and the session bearer.
    async fn patch_json(&self, path: &str, body: &serde_json::Value) -> Result<Bytes, ApiError>;

    /// `DELETE {base}{path}` with the session bearer.
    async fn delete(&self, path: &str) -> Result<Bytes, ApiError>;

    /// `HEAD {base}{path}` with the session bearer, reporting whether the
    /// response's `x-c2pa` header reads `"true"` — the C2PA provenance hint a
    /// blob response carries (`ui/media.md` § C2PA provenance;
    /// `bins/fauna-nest/src/blob_routes.rs::build_blob_response`). A HEAD
    /// avoids downloading the blob bytes just to read one header — the same
    /// reachable-badge check android's `checkBlobC2pa` makes over its own HTTP
    /// client, shared here so any direct-Rust client (tui, linux) gets it for
    /// free instead of hand-rolling it per app (priority #2).
    async fn head_has_c2pa(&self, path: &str) -> Result<bool, ApiError>;
}

/// Production [`NestContentApi`] over `reqwest::Client` + a [`BearerSource`].
/// The single chokepoint for the bearer attach + the 401-reactive refresh +
/// the structured-error extraction (see the module docs).
pub struct ReqwestNestContentApi<B: BearerSource> {
    http: reqwest::Client,
    /// Base URL of the nest, e.g. `https://nest.example` (no trailing slash).
    /// Paths passed to the trait methods are appended verbatim.
    base_url: String,
    bearer: B,
}

impl<B: BearerSource> ReqwestNestContentApi<B> {
    pub fn new(base_url: impl Into<String>, http: reqwest::Client, bearer: B) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            bearer,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Attach the bearer, send, and — on `401` — fire the source's
    /// `notify_401()` and retry once with the fresh bearer.
    ///
    /// A second `401`, a `notify_401` that doesn't recover (source can't
    /// produce a new bearer), or a non-cloneable streaming body all surface
    /// the *first* response — never an infinite retry. The common path (no
    /// 401 — the TTL loop / cache keeps the bearer fresh) pays only one
    /// `try_clone`, cheap for the buffered bodies the content API uses.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, ApiError> {
        let token = self.bearer.bearer().await?;
        let retry = req.try_clone();
        let resp = req
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?;
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        // Stale / server-revoked bearer. React, then one retry with a fresh one.
        self.bearer.notify_401().await;
        let (Some(retry), Ok(token)) = (retry, self.bearer.bearer().await) else {
            return Ok(resp);
        };
        retry
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))
    }

    /// Drain a finished response into [`Bytes`] (2xx) or an
    /// [`ApiError::Status`] carrying the nest's error message.
    async fn into_body(resp: reqwest::Response) -> Result<Bytes, ApiError> {
        let status = resp.status();
        if status.is_success() {
            return resp
                .bytes()
                .await
                .map_err(|e| ApiError::Transport(e.to_string()));
        }
        let code = status.as_u16();
        Err(ApiError::Status {
            code,
            message: error_body_message(resp).await,
        })
    }
}

/// How much of a non-2xx body is ever read. The body is a diagnostic — it goes
/// to a log line, and for our own nest into a transport sentence — and the
/// responder can be a nest someone else runs (a conversation's home nest is
/// chosen by whoever created the room), so it does not get to choose how much
/// this client allocates. Generous for any real `{"error": …}` reply.
pub const MAX_ERROR_BODY_BYTES: usize = 4096;

/// A non-2xx response's message: the nest's `{"error": …}` when it parses, the
/// lossy text otherwise — read at most [`MAX_ERROR_BODY_BYTES`], streamed so an
/// absent or dishonest `Content-Length` cannot push past the cap
/// ([`crate::capped::read_prefix`], truncating rather than failing: the status
/// code must still reach the caller). A body cut mid-JSON simply
/// fails the parse and falls through to the text.
async fn error_body_message(resp: reqwest::Response) -> String {
    let body = crate::capped::read_prefix(resp, MAX_ERROR_BODY_BYTES).await;
    serde_json::from_slice::<NestErrorBody>(&body)
        .map(|b| b.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned())
}

#[async_trait]
impl<B: BearerSource> NestContentApi for ReqwestNestContentApi<B> {
    async fn get(&self, path: &str) -> Result<Bytes, ApiError> {
        let resp = self.send(self.http.get(self.url(path))).await?;
        Self::into_body(resp).await
    }

    async fn get_with_query(&self, path: &str, params: &[(&str, &str)]) -> Result<Bytes, ApiError> {
        let resp = self
            .send(self.http.get(self.url(path)).query(params))
            .await?;
        Self::into_body(resp).await
    }

    async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<Bytes, ApiError> {
        let resp = self.send(self.http.post(self.url(path)).json(body)).await?;
        Self::into_body(resp).await
    }

    async fn post_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        let resp = self
            .send(
                self.http
                    .post(self.url(path))
                    .header("content-type", content_type.to_string())
                    .body(body),
            )
            .await?;
        Self::into_body(resp).await
    }

    async fn post_bytes_keyed(
        &self,
        path: &str,
        content_hash_hex: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        let resp = self
            .send(
                self.http
                    .post(self.url(path))
                    .header("content-type", "application/octet-stream")
                    .header("X-Content-Hash", content_hash_hex.to_string())
                    .body(body),
            )
            .await?;
        Self::into_body(resp).await
    }

    async fn post_multipart_blob(
        &self,
        path: &str,
        sidecar_cbor: Vec<u8>,
        sealed_bytes: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        let form = reqwest::multipart::Form::new()
            .part(
                "sidecar",
                reqwest::multipart::Part::bytes(sidecar_cbor)
                    .mime_str("application/cbor")
                    .expect("application/cbor is a valid MIME"),
            )
            .part(
                "bytes",
                reqwest::multipart::Part::bytes(sealed_bytes)
                    .mime_str("application/octet-stream")
                    .expect("application/octet-stream is a valid MIME"),
            );
        let resp = self
            .send(self.http.post(self.url(path)).multipart(form))
            .await?;
        Self::into_body(resp).await
    }

    async fn put_bytes(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ApiError> {
        let resp = self
            .send(
                self.http
                    .put(self.url(path))
                    .header("content-type", content_type.to_string())
                    .body(body),
            )
            .await?;
        Self::into_body(resp).await
    }

    async fn put_json(&self, path: &str, body: &serde_json::Value) -> Result<Bytes, ApiError> {
        let resp = self.send(self.http.put(self.url(path)).json(body)).await?;
        Self::into_body(resp).await
    }

    async fn patch_json(&self, path: &str, body: &serde_json::Value) -> Result<Bytes, ApiError> {
        let resp = self
            .send(self.http.patch(self.url(path)).json(body))
            .await?;
        Self::into_body(resp).await
    }

    async fn delete(&self, path: &str) -> Result<Bytes, ApiError> {
        let resp = self.send(self.http.delete(self.url(path))).await?;
        Self::into_body(resp).await
    }

    async fn head_has_c2pa(&self, path: &str) -> Result<bool, ApiError> {
        let resp = self.send(self.http.head(self.url(path))).await?;
        let status = resp.status();
        if !status.is_success() {
            let code = status.as_u16();
            return Err(ApiError::Status {
                code,
                message: error_body_message(resp).await,
            });
        }
        Ok(resp
            .headers()
            .get("x-c2pa")
            .is_some_and(|v| v.as_bytes() == b"true"))
    }
}
