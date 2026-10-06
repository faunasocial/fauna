//! The bulk-binary blob-download seam for the Media `media-thumbnail` render.
//!
//! `MediaMachine::fetch_thumbnail` fetches a thumbnail blob by content hash then
//! decrypts it in shared Rust (`fauna_core::crypto::decrypt_backup_chunk`). The
//! fetch is the one platform-variant leg — `GET /api/v1/blob/<hash>` **direct-by-
//! hash** (the bulk-binary carve-out that stays HTTP under the
//! `no-http-ws-rpc-everywhere` directive). It is the inverse of the
//! [`MediaBlobUploader`](crate::blob_uploader::MediaBlobUploader) POST: a
//! Media-library / device-synced thumbnail has **no** `/api/v1/blob` *primary*
//! with `blob_metadata` (the file is stored as encrypted chunks), so the legacy
//! `?thumb=1` routing can't resolve it — the client holds the thumbnail's *own*
//! hash (`MediaItem.thumbnail_hash`) and fetches it directly (`media.md`
//! § Implementation status (a); `bins/fauna-nest/src/blob_routes.rs` by-hash GET).
//!
//! The seam is injected as `Option<Arc<dyn MediaBlobFetcher>>` so a platform with
//! no blob-fetch coordinator constructs the machine with `None` and
//! `fetch_thumbnail` returns a `Transient` "not supported" error rather than
//! failing to compile. Both production arms now exist: the native
//! [`NativeBlobFetcher`] (over `fauna-nest-http`'s reqwest content API) and the
//! wasm [`WasmBlobFetcher`] (the render twin of the upload LEG B — a `gloo-net`
//! fetch GET of the public by-hash route) — each built from its per-target client
//! in `build_media_machine`.

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
use std::sync::Mutex;

use crate::nest_api::MediaApiError;

/// The single HTTP leg of a thumbnail render: download the stored blob bytes by
/// content hash. The bytes are returned **as stored** — for an owner-`Library`
/// thumbnail that is the AEAD-sealed ciphertext, which the caller
/// (`MediaMachine::fetch_thumbnail`) decrypts under the owner `BackupKey`.
///
/// `MaybeSendSync` + the dual `async_trait` arm so the one seam serves native
/// (`Send + Sync`) and wasm (the single-threaded SPA, `!Send`) — the identical
/// pattern on [`MediaBlobUploader`](crate::blob_uploader::MediaBlobUploader).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait MediaBlobFetcher: fauna_core::MaybeSendSync {
    /// `GET /api/v1/blob/<hash>` (direct-by-hash). `hash` is the hex content hash
    /// of the thumbnail blob (a `MediaItem.thumbnail_hash`). Returns the raw
    /// stored bytes (still sealed for a Library thumbnail).
    async fn fetch_blob(&self, hash: String) -> Result<Vec<u8>, MediaApiError>;
}

/// In-memory fake blob fetcher for `MediaMachine::fetch_thumbnail` tests: returns
/// canned bytes (or a fixtured error) and records each requested hash, so the
/// whole fetch+decrypt gesture is testable with no nest + no HTTP. Mirrors
/// `FakeMediaBlobUploader`.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug)]
pub struct FakeMediaBlobFetcher {
    /// The bytes `fetch_blob` returns when no error is set.
    blob: Mutex<Vec<u8>>,
    /// `Some` makes `fetch_blob` fail with this error.
    error: Mutex<Option<MediaApiError>>,
    /// Each requested hash, in order.
    fetches: Mutex<Vec<String>>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl FakeMediaBlobFetcher {
    /// A fake returning `blob` from every successful `fetch_blob`.
    pub fn new(blob: Vec<u8>) -> Self {
        Self {
            blob: Mutex::new(blob),
            error: Mutex::new(None),
            fetches: Mutex::new(Vec::new()),
        }
    }

    /// Make `fetch_blob` fail with `err`.
    pub fn fail(&self, err: MediaApiError) {
        *self.error.lock().unwrap() = Some(err);
    }

    /// All requested hashes, in order.
    pub fn fetches(&self) -> Vec<String> {
        self.fetches.lock().unwrap().clone()
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl MediaBlobFetcher for FakeMediaBlobFetcher {
    async fn fetch_blob(&self, hash: String) -> Result<Vec<u8>, MediaApiError> {
        self.fetches.lock().unwrap().push(hash);
        match self.error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(self.blob.lock().unwrap().clone()),
        }
    }
}

// ── Native production impl ───────────────────────────────────────────────────
//
// The native blob fetcher over the bulk-binary HTTP `GET /api/v1/blob/<hash>`,
// built from the session's `NestClient`. NATIVE-ONLY (`reqwest`/`tokio`); the
// wasm web fetcher (the render twin of the upload LEG B) is the separate fetch
// coordinator below.
#[cfg(all(not(target_arch = "wasm32"), feature = "rpc-glue"))]
mod native {
    use std::sync::Arc;

    use fauna_client::NestClient;
    use fauna_nest_http::{ApiError, BearerSource, NestContentApi, ReqwestNestContentApi, paths};

    use super::{MediaApiError, MediaBlobFetcher};

    /// Native [`MediaBlobFetcher`] over `GET /api/v1/blob/<hash>` (the bulk-binary
    /// carve-out). Constructed from the session's [`NestClient`] exactly like
    /// [`NativeBlobUploader`](crate::blob_uploader::NativeBlobUploader): it reuses
    /// the client's own `AuthClient` — the **SPKI-pinned** reqwest client
    /// (`security.md` § Cross-connection binding) and the shared bearer cache — so
    /// the fetch neither re-pins TLS nor mints a parallel token.
    ///
    /// The by-hash GET is a **public** route (`paths::blob::by_hash` is served
    /// with no auth gate). We deliberately route it through the session's pinned
    /// `ReqwestNestContentApi` rather than a parallel bare client: it is the user's
    /// *own* session nest, so the attached bearer is simply ignored by the public
    /// route, and reusing the one pinned + tested content client avoids a second
    /// reqwest client (and a TLS re-pin) for image loads. Integrity does not rest
    /// on the bearer regardless — [`fetch_thumbnail`](crate::MediaMachine::fetch_thumbnail) verifies the
    /// content address (`blake3(fetched) == thumbnail_hash`) before decrypting, so
    /// a substituted blob is rejected. (The AEAD tag alone proves only that the
    /// bytes were sealed under the owner's key, **not** that they are *this* blob:
    /// the backup-chunk frame carries no AAD, so a different owner-sealed blob
    /// would decrypt cleanly — the content-address check is what catches the swap.)
    pub struct NativeBlobFetcher {
        content: ReqwestNestContentApi<Arc<dyn BearerSource>>,
    }

    impl NativeBlobFetcher {
        /// Build over `nest`'s authenticated session (its `AuthClient`'s pinned
        /// http + shared bearer + current nest URL).
        pub fn new(nest: &Arc<NestClient>) -> Self {
            Self {
                content: nest.auth().content_api(),
            }
        }
    }

    #[async_trait::async_trait]
    impl MediaBlobFetcher for NativeBlobFetcher {
        async fn fetch_blob(&self, hash: String) -> Result<Vec<u8>, MediaApiError> {
            self.content
                .get(&paths::blob::by_hash(&hash))
                .await
                .map(|b| b.to_vec())
                .map_err(map_api_err)
        }
    }

    /// Map the HTTP content-API error onto the page's [`MediaApiError`] — see
    /// [`crate::nest_api::map_content_api_err`], the shared taxonomy this and
    /// [`NativeBlobUploader`](crate::blob_uploader) both use.
    fn map_api_err(e: ApiError) -> MediaApiError {
        crate::nest_api::map_content_api_err(e, "blob fetch")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn api_status_codes_map_to_variants() {
            assert!(matches!(
                map_api_err(ApiError::Status {
                    code: 404,
                    message: "no blob".into()
                }),
                MediaApiError::NotFound { detail } if detail == "no blob"
            ));
            assert!(matches!(
                map_api_err(ApiError::Status {
                    code: 400,
                    message: "bad hash".into()
                }),
                MediaApiError::BadRequest { .. }
            ));
            assert!(matches!(
                map_api_err(ApiError::Status {
                    code: 500,
                    message: "boom".into()
                }),
                MediaApiError::Transient { .. }
            ));
            assert!(matches!(
                map_api_err(ApiError::Transport("offline".into())),
                MediaApiError::Transient { detail } if detail == "offline"
            ));
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "rpc-glue"))]
pub use native::NativeBlobFetcher;

// ── Wasm production impl (the render twin of upload LEG B) ────────────────────
//
// The web blob fetcher over the same bulk-binary `GET /api/v1/blob/<hash>`, built
// from the SPA's browser `WsRpcClient`. WASM-ONLY (`gloo-net` fetch); mirrors the
// native fetcher, so `MediaMachine::fetch_thumbnail` runs the whole fetch →
// verify → decrypt render leg in shared Rust on web too (priority #2).
#[cfg(all(target_arch = "wasm32", feature = "rpc-glue"))]
mod wasm {
    use fauna_rpc_wasm::WsRpcClient;

    use super::{MediaApiError, MediaBlobFetcher};

    /// Wasm [`MediaBlobFetcher`] over `GET /api/v1/blob/<hash>` (the bulk-binary
    /// carve-out). Constructed from the SPA's [`WsRpcClient`] for its current nest
    /// URL ([`WsRpcClient::nest_url`]). The by-hash GET is a **public** route
    /// (`bins/fauna-nest/src/blob_routes.rs` — "unauthenticated + navigable"), so
    /// unlike the upload POST ([`WasmBlobUploader`](crate::blob_uploader::WasmBlobUploader))
    /// it carries **no** bearer — the same pattern the conversation-attachment
    /// `WsConversationsRpc::blob_get` already uses. Integrity does not rest on the
    /// bearer regardless: [`fetch_thumbnail`](crate::MediaMachine::fetch_thumbnail)
    /// verifies the content address (`blake3(fetched) == thumbnail_hash`) before
    /// decrypting, so a substituted blob is rejected.
    /// `WsRpcClient` is a cheap `Rc` handle, so it is held by value.
    pub struct WasmBlobFetcher {
        nest: WsRpcClient,
    }

    impl WasmBlobFetcher {
        /// Build over the SPA's connected `WsRpcClient` (its current nest URL).
        pub fn new(nest: WsRpcClient) -> Self {
            Self { nest }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl MediaBlobFetcher for WasmBlobFetcher {
        async fn fetch_blob(&self, hash: String) -> Result<Vec<u8>, MediaApiError> {
            let url = format!("{}/api/v1/blob/{hash}", self.nest.nest_url());
            let resp = gloo_net::http::Request::get(&url)
                .send()
                .await
                .map_err(|e| MediaApiError::Transient {
                    detail: format!("blob fetch GET: {e}"),
                })?;

            let status = resp.status();
            if !(200..300).contains(&status) {
                let message = resp.text().await.unwrap_or_default();
                return Err(crate::nest_api::map_blob_http_err(
                    fauna_rpc_wasm::BlobHttpError::Status { status, message },
                    "blob fetch",
                ));
            }
            // The stored bytes verbatim (still sealed for a Library thumbnail); the
            // caller content-address-verifies + decrypts them.
            resp.binary().await.map_err(|e| MediaApiError::Transient {
                detail: format!("blob fetch read body: {e}"),
            })
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "rpc-glue"))]
pub use wasm::WasmBlobFetcher;
