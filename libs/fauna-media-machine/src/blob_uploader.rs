//! The bulk-binary blob-upload seam for the Media `upload` gesture.
//!
//! `MediaMachine::upload` seals the picked file in shared Rust
//! (`fauna_core::blob_seal::seal_blob`, the one at-rest shape) then hands the
//! chunks + manifest — and the companion thumbnail blob + sidecar — to a
//! [`MediaBlobUploader`] for the HTTP legs (`POST /api/v1/chunks`,
//! `/api/v1/manifests`, `/api/v1/blob`), the **bulk-binary carve-out** that stays HTTP under the
//! `no-http-ws-rpc-everywhere` directive (`docs/goal/architecture/api-layers.md`
//! § production HTTP carve-out). The WS-RPC control plane (record the manifest
//! member) rides the separate [`MediaNestApi`](crate::nest_api::MediaNestApi)
//! seam.
//!
//! The seam is injected as `Option<Arc<dyn MediaBlobUploader>>` so a platform
//! with no blob-upload coordinator constructs the machine with `None` and
//! `upload` returns a `Transient` "not supported" error rather than failing to
//! compile. Both production arms now exist: the native [`NativeBlobUploader`]
//! (LEG A, over `fauna-nest-http`'s reqwest content API) and the wasm
//! [`WasmBlobUploader`] (LEG B, over `fauna_rpc_wasm::post_multipart_blob`, that
//! API's browser twin) — each built from its per-target client in
//! `build_media_machine`. Neither arm open-codes the nest's strict multipart
//! shape; both call the one helper their target provides.

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
use std::sync::Mutex;

use crate::nest_api::MediaApiError;

/// The single HTTP leg of an upload: push a sealed blob + its DAG-CBOR sidecar to
/// the nest's bulk-binary route and return the nest-assigned content hash (hex).
///
/// `MaybeSendSync` + the dual `async_trait` arm so the one seam serves native
/// (`Send + Sync`) and wasm (the single-threaded SPA, `!Send`) — the identical
/// pattern on [`MediaNestApi`](crate::nest_api::MediaNestApi).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait MediaBlobUploader: fauna_core::MaybeSendSync {
    /// `POST /api/v1/blob` (multipart: `sidecar` `application/cbor` +
    /// `bytes`). `sidecar_cbor` is `UploadSidecar::to_dag_cbor()`; `sealed_bytes`
    /// is the AEAD-sealed primary blob. Returns the content hash (hex) the nest
    /// assigns, which the caller records as the manifest member.
    async fn post_blob(
        &self,
        sidecar_cbor: Vec<u8>,
        sealed_bytes: Vec<u8>,
    ) -> Result<String, MediaApiError>;

    /// `POST /api/v1/chunks` with `X-Content-Hash: hex(store_key)` — store one
    /// **sealed** chunk body under its ciphertext hash, the content-addressed
    /// chunk store's write shape the sync engine's `upload_chunk` uses. The
    /// byte leg of an upload into a **content-keyed** set (served or shared —
    /// `media.md` § Encryption at rest → *Content-keyed sets*): such a set's
    /// files are `ChunkManifest`s over chunks sealed by the engine's own
    /// pipeline (`fauna_core::blob_seal::seal_blob`), never a blob-store
    /// primary, so every manifest reader — the WebDAV MDA, a synced device, a
    /// member — opens them. `body` is exactly what `seal_blob` produced; the
    /// impl verifies the nest filed it under `store_key` (the reply's hash)
    /// and answers `Transient` otherwise.
    async fn post_chunk(&self, store_key: [u8; 32], body: Vec<u8>) -> Result<(), MediaApiError>;

    /// `POST /api/v1/manifests` — store the canonical-encoded `ChunkManifest`
    /// the chunks above belong to; the nest keys it by `blake3(manifest_bytes)`
    /// and refuses a body that is not a canonical manifest. The impl verifies
    /// the reply's hash equals `manifest_hash` and answers `Transient`
    /// otherwise. Call after every chunk is stored, so a manifest never rests
    /// pointing at chunks the nest does not hold.
    async fn post_manifest(
        &self,
        manifest_hash: [u8; 32],
        manifest_bytes: Vec<u8>,
    ) -> Result<(), MediaApiError>;
}

/// The Media upload seam as the shared re-seal's write seam: chunks then the
/// manifest over the byte routes, each put idempotent (the store is
/// content-addressed and the seal deterministic).
pub(crate) struct UploaderSink<'a>(pub(crate) &'a dyn MediaBlobUploader);

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_core::nest_reseal::ChunkStoreSink for UploaderSink<'_> {
    async fn put_chunks(
        &self,
        bodies: Vec<(fauna_core::data::ContentHash, Vec<u8>)>,
        _relative_path: &str,
    ) -> anyhow::Result<usize> {
        let count = bodies.len();
        for (store_key, body) in bodies {
            self.0
                .post_chunk(store_key.digest(), body)
                .await
                .map_err(|e| anyhow::anyhow!("chunk upload: {e}"))?;
        }
        Ok(count)
    }

    async fn put_manifest(
        &self,
        manifest_hash: fauna_core::data::ContentHash,
        manifest_bytes: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.0
            .post_manifest(manifest_hash.digest(), manifest_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("manifest upload: {e}"))
    }
}

/// In-memory fake blob uploader for `MediaMachine::upload` tests: records each
/// `(sidecar_cbor, sealed_bytes)` it receives and returns a canned hash (or a
/// fixtured error), so the whole upload gesture is testable end-to-end with no
/// nest + no HTTP. Mirrors `FakeMediaNestApi`. The chunk-store legs
/// ([`MediaBlobUploader::post_chunk`] / [`post_manifest`](MediaBlobUploader::post_manifest))
/// record what they were handed, keyed exactly as the nest would file it.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug)]
pub struct FakeMediaBlobUploader {
    /// The hash `post_blob` returns when no error is set.
    hash: String,
    /// `Some` makes every leg fail with this error.
    error: Mutex<Option<MediaApiError>>,
    /// Each `(sidecar_cbor, sealed_bytes)` received, in order.
    posts: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    /// Each `(store_key, body)` chunk received, in order.
    chunks: Mutex<Vec<([u8; 32], Vec<u8>)>>,
    /// Each `(manifest_hash, manifest_bytes)` received, in order.
    manifests: Mutex<Vec<([u8; 32], Vec<u8>)>>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl FakeMediaBlobUploader {
    /// A fake returning `hash` from every successful `post_blob`.
    pub fn new(hash: impl Into<String>) -> Self {
        Self {
            hash: hash.into(),
            error: Mutex::new(None),
            posts: Mutex::new(Vec::new()),
            chunks: Mutex::new(Vec::new()),
            manifests: Mutex::new(Vec::new()),
        }
    }

    /// Make every leg fail with `err`.
    pub fn fail(&self, err: MediaApiError) {
        *self.error.lock().unwrap() = Some(err);
    }

    /// All recorded `(sidecar_cbor, sealed_bytes)` posts, in order.
    pub fn posts(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.posts.lock().unwrap().clone()
    }

    /// All recorded `(store_key, body)` chunk posts, in order.
    pub fn chunks(&self) -> Vec<([u8; 32], Vec<u8>)> {
        self.chunks.lock().unwrap().clone()
    }

    /// All recorded `(manifest_hash, manifest_bytes)` posts, in order.
    pub fn manifests(&self) -> Vec<([u8; 32], Vec<u8>)> {
        self.manifests.lock().unwrap().clone()
    }

    fn outcome(&self) -> Result<(), MediaApiError> {
        match self.error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl MediaBlobUploader for FakeMediaBlobUploader {
    async fn post_blob(
        &self,
        sidecar_cbor: Vec<u8>,
        sealed_bytes: Vec<u8>,
    ) -> Result<String, MediaApiError> {
        self.posts
            .lock()
            .unwrap()
            .push((sidecar_cbor, sealed_bytes));
        self.outcome().map(|()| self.hash.clone())
    }

    async fn post_chunk(&self, store_key: [u8; 32], body: Vec<u8>) -> Result<(), MediaApiError> {
        self.chunks.lock().unwrap().push((store_key, body));
        self.outcome()
    }

    async fn post_manifest(
        &self,
        manifest_hash: [u8; 32],
        manifest_bytes: Vec<u8>,
    ) -> Result<(), MediaApiError> {
        self.manifests
            .lock()
            .unwrap()
            .push((manifest_hash, manifest_bytes));
        self.outcome()
    }
}

/// The chunk-store write routes reply `{"hash": "<64 hex>"}` with the key the
/// nest filed the body under; a reply naming any other key means the bytes
/// rest somewhere no manifest points — surface it rather than record a
/// member the reader would 404 on. Shared by both production arms.
#[cfg(feature = "rpc-glue")]
fn verify_stored_under(reply: &[u8], expected: [u8; 32], what: &str) -> Result<(), MediaApiError> {
    let got = parse_blob_hash(reply)?;
    if got.eq_ignore_ascii_case(&hex::encode(expected)) {
        Ok(())
    } else {
        Err(MediaApiError::Transient {
            detail: format!(
                "{what} upload: the nest stored the body under {got}, not the expected {}",
                hex::encode(expected)
            ),
        })
    }
}

/// Parse the `{"hash": "<64 hex chars>"}` reply of `POST /api/v1/blob`
/// (`bins/fauna-nest/src/blob_routes.rs`) into the content hash hex. Shared by
/// the native ([`NativeBlobUploader`]) and wasm ([`WasmBlobUploader`]) arms —
/// both hit the same nest route with the same reply shape.
#[cfg(feature = "rpc-glue")]
fn parse_blob_hash(reply: &[u8]) -> Result<String, MediaApiError> {
    #[derive(serde::Deserialize)]
    struct BlobReply {
        hash: String,
    }
    serde_json::from_slice::<BlobReply>(reply)
        .map(|r| r.hash)
        .map_err(|e| MediaApiError::Transient {
            detail: format!("malformed blob upload reply: {e}"),
        })
}

#[cfg(all(test, feature = "rpc-glue"))]
mod parse_blob_hash_tests {
    use super::{MediaApiError, parse_blob_hash};

    #[test]
    fn parse_blob_hash_extracts_the_hex() {
        let reply = br#"{"hash":"abc123def456"}"#;
        assert_eq!(parse_blob_hash(reply).unwrap(), "abc123def456");
    }

    #[test]
    fn parse_blob_hash_rejects_a_malformed_reply() {
        let reply = br#"{"oops":true}"#;
        assert!(matches!(
            parse_blob_hash(reply),
            Err(MediaApiError::Transient { .. })
        ));
    }
}

// ── Native production impl (LEG A) ──────────────────────────────────────────
//
// The native blob uploader over the bulk-binary HTTP `POST /api/v1/blob`,
// built from the session's `NestClient`. NATIVE-ONLY (`reqwest`/`tokio`); the
// wasm web uploader (LEG B) is a separate fetch coordinator.
#[cfg(all(not(target_arch = "wasm32"), feature = "rpc-glue"))]
mod native {
    use std::sync::Arc;

    use fauna_client::NestClient;
    use fauna_nest_http::{ApiError, BearerSource, NestContentApi, ReqwestNestContentApi};

    use super::{MediaApiError, MediaBlobUploader};

    /// Native [`MediaBlobUploader`] over `POST /api/v1/blob` (the bulk-binary
    /// carve-out). Constructed from the session's [`NestClient`]: it reuses the
    /// client's own `AuthClient` — the **SPKI-pinned** reqwest client
    /// (`security.md` § Cross-connection binding) and the **shared bearer cache**
    /// (one token mint / refresh loop / 401-reactive path) — so the upload path
    /// neither re-pins TLS nor mints a parallel token. All four native apps
    /// (linux/windows/apple/android) get the same uploader by passing their
    /// `NestClient`, retiring the per-app app-code blob POST (priority #2).
    pub struct NativeBlobUploader {
        content: ReqwestNestContentApi<Arc<dyn BearerSource>>,
    }

    impl NativeBlobUploader {
        /// Build over `nest`'s authenticated session (its `AuthClient`'s pinned
        /// http + shared bearer + current nest URL).
        pub fn new(nest: &Arc<NestClient>) -> Self {
            Self {
                content: nest.auth().content_api(),
            }
        }
    }

    #[async_trait::async_trait]
    impl MediaBlobUploader for NativeBlobUploader {
        async fn post_blob(
            &self,
            sidecar_cbor: Vec<u8>,
            sealed_bytes: Vec<u8>,
        ) -> Result<String, MediaApiError> {
            let reply = self
                .content
                .post_multipart_blob("/api/v1/blob", sidecar_cbor, sealed_bytes)
                .await
                .map_err(map_api_err)?;
            super::parse_blob_hash(&reply)
        }

        async fn post_chunk(
            &self,
            store_key: [u8; 32],
            body: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let reply = self
                .content
                .post_bytes_keyed(
                    fauna_nest_http::paths::chunk_store::CHUNKS_UPLOAD,
                    &hex::encode(store_key),
                    body,
                )
                .await
                .map_err(|e| crate::nest_api::map_content_api_err(e, "chunk upload"))?;
            super::verify_stored_under(&reply, store_key, "chunk")
        }

        async fn post_manifest(
            &self,
            manifest_hash: [u8; 32],
            manifest_bytes: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let reply = self
                .content
                .post_bytes(
                    fauna_nest_http::paths::chunk_store::MANIFESTS_UPLOAD,
                    "application/octet-stream",
                    manifest_bytes,
                )
                .await
                .map_err(|e| crate::nest_api::map_content_api_err(e, "manifest upload"))?;
            super::verify_stored_under(&reply, manifest_hash, "manifest")
        }
    }

    /// The **member write door** on native (`ui/media.md` § Cross-nest shared
    /// sets, *Upload*): chunk and manifest POSTs addressed to a cross-nest
    /// set's HOME nest, under the write token the member's own nest mints
    /// (`fauna.folders.write_token.get`), over the pin-or-WebPKI plane the
    /// home's TLS is graduated on against the grant-delivered identity
    /// ([`fauna_client::post_blob::PinnedHomes`], the read door's and the
    /// conversation rail's plane). The session's own bearer never reaches the
    /// home nest. The home's client is resolved at the first POST, since the
    /// graduation is async.
    pub struct NativeMemberUploader {
        home: fauna_client::ForeignHome,
        homes: Arc<fauna_client::post_blob::PinnedHomes>,
        bearer: Arc<dyn BearerSource>,
    }

    impl NativeMemberUploader {
        /// The door over `home` for the set whose channel is `channel_id`,
        /// minting through `own_nest`.
        pub fn new(
            own_nest: Arc<NestClient>,
            home: &fauna_core::folder_keys::ForeignHome,
            channel_id: [u8; 32],
            homes: Arc<fauna_client::post_blob::PinnedHomes>,
        ) -> Self {
            let home_nest_url = home.nest_url.clone();
            let channel = hex::encode(channel_id);
            let bearer =
                fauna_client::write_token_bearer::WriteTokenBearer::from_minter(move || {
                    let own_nest = Arc::clone(&own_nest);
                    let nest_url = home_nest_url.clone();
                    let channel_id = channel.clone();
                    async move {
                        crate::nest_api::ws_rpc::mint_member_write_token(
                            own_nest.as_ref(),
                            nest_url,
                            channel_id,
                        )
                        .await
                        .map_err(|e| match e {
                            crate::nest_api::ws_rpc::WriteMintError::Refused(message) => {
                                ApiError::Status { code: 403, message }
                            }
                            crate::nest_api::ws_rpc::WriteMintError::Transport(detail) => {
                                ApiError::Transport(detail)
                            }
                        })
                    }
                });
            Self {
                home: fauna_client::ForeignHome::new(&home.nest_url, home.nest_actor_id.as_deref()),
                homes,
                bearer: Arc::new(bearer),
            }
        }

        async fn content(
            &self,
        ) -> Result<ReqwestNestContentApi<Arc<dyn BearerSource>>, MediaApiError> {
            use fauna_client::post_blob::ForeignHomeHttp;
            let http = self.homes.client(&self.home).await.map_err(map_api_err)?;
            Ok(ReqwestNestContentApi::new(
                self.home.url.clone(),
                http,
                Arc::clone(&self.bearer),
            ))
        }
    }

    #[async_trait::async_trait]
    impl MediaBlobUploader for NativeMemberUploader {
        async fn post_blob(
            &self,
            _sidecar_cbor: Vec<u8>,
            _sealed_bytes: Vec<u8>,
        ) -> Result<String, MediaApiError> {
            // A folder file is chunks behind a manifest; the single sealed
            // blob is the attachment plane's and never crosses this door.
            Err(MediaApiError::BadRequest {
                detail: "a folder homed on another nest takes chunks, not a single blob"
                    .to_string(),
            })
        }

        async fn post_chunk(
            &self,
            store_key: [u8; 32],
            body: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let reply = self
                .content()
                .await?
                .post_bytes_keyed(
                    fauna_nest_http::paths::chunk_store::CHUNKS_UPLOAD,
                    &hex::encode(store_key),
                    body,
                )
                .await
                .map_err(|e| crate::nest_api::map_content_api_err(e, "chunk upload"))?;
            super::verify_stored_under(&reply, store_key, "chunk")
        }

        async fn post_manifest(
            &self,
            manifest_hash: [u8; 32],
            manifest_bytes: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let reply = self
                .content()
                .await?
                .post_bytes(
                    fauna_nest_http::paths::chunk_store::MANIFESTS_UPLOAD,
                    "application/octet-stream",
                    manifest_bytes,
                )
                .await
                .map_err(|e| crate::nest_api::map_content_api_err(e, "manifest upload"))?;
            super::verify_stored_under(&reply, manifest_hash, "manifest")
        }
    }

    /// Map the HTTP content-API error onto the page's [`MediaApiError`] — see
    /// [`crate::nest_api::map_content_api_err`], the shared taxonomy this and
    /// [`NativeBlobFetcher`](crate::blob_fetcher) both use.
    fn map_api_err(e: ApiError) -> MediaApiError {
        crate::nest_api::map_content_api_err(e, "blob upload")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn api_status_codes_map_to_variants() {
            assert!(matches!(
                map_api_err(ApiError::Status {
                    code: 404,
                    message: "no set".into()
                }),
                MediaApiError::NotFound { detail } if detail == "no set"
            ));
            assert!(matches!(
                map_api_err(ApiError::Status {
                    code: 400,
                    message: "bad".into()
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
pub use native::{NativeBlobUploader, NativeMemberUploader};

// ── Wasm production impl (LEG B) ─────────────────────────────────────────────
//
// The web blob uploader over the same bulk-binary `POST /api/v1/blob`, built
// from the SPA's browser `WsRpcClient`. WASM-ONLY; mirrors native LEG A — both
// delegate the multipart shape to their target's one helper — so
// `MediaMachine::upload` runs the whole seal → POST → record gesture in shared
// Rust on web too (priority #2).
#[cfg(all(target_arch = "wasm32", feature = "rpc-glue"))]
mod wasm {
    use fauna_rpc_wasm::WsRpcClient;

    use super::{MediaApiError, MediaBlobUploader};

    /// Wasm [`MediaBlobUploader`] over `POST /api/v1/blob` (the bulk-binary
    /// carve-out). Constructed from the SPA's [`WsRpcClient`]: it reuses the
    /// client's own single origin ([`WsRpcClient::nest_url`]) + the JS
    /// token-provider bearer ([`WsRpcClient::bearer`]) — the same pattern the
    /// conversation-attachment `WsConversationsRpc::blob_put` already uses — so
    /// the upload path rides the existing authenticated session with no parallel
    /// token. `WsRpcClient` is a cheap `Rc` handle, so it is held by value.
    pub struct WasmBlobUploader {
        nest: WsRpcClient,
    }

    impl WasmBlobUploader {
        /// Build over the SPA's connected `WsRpcClient` (its current nest URL +
        /// JS-provided bearer).
        pub fn new(nest: WsRpcClient) -> Self {
            Self { nest }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl MediaBlobUploader for WasmBlobUploader {
        async fn post_blob(
            &self,
            sidecar_cbor: Vec<u8>,
            sealed_bytes: Vec<u8>,
        ) -> Result<String, MediaApiError> {
            // The multipart shape itself lives in `fauna_rpc_wasm::blob_http`
            // — one owner on wasm, the twin of native's single
            // `fauna_nest_http` helper (LEG A above). This arm keeps only what
            // is page-specific: the `MediaApiError` taxonomy and parsing the
            // nest's `{"hash": ...}` reply.
            let reply = fauna_rpc_wasm::post_multipart_blob(
                &self.nest,
                fauna_rpc_wasm::BLOB_UPLOAD_PATH,
                &sidecar_cbor,
                &sealed_bytes,
            )
            .await
            .map_err(|e| crate::nest_api::map_blob_http_err(e, "blob upload"))?;
            super::parse_blob_hash(reply.as_bytes())
        }

        async fn post_chunk(
            &self,
            store_key: [u8; 32],
            body: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let reply = fauna_rpc_wasm::post_octets_keyed(
                &self.nest,
                fauna_rpc_wasm::CHUNKS_UPLOAD_PATH,
                Some(&hex::encode(store_key)),
                &body,
            )
            .await
            .map_err(|e| crate::nest_api::map_blob_http_err(e, "chunk upload"))?;
            super::verify_stored_under(reply.as_bytes(), store_key, "chunk")
        }

        async fn post_manifest(
            &self,
            manifest_hash: [u8; 32],
            manifest_bytes: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let reply = fauna_rpc_wasm::post_octets_keyed(
                &self.nest,
                fauna_rpc_wasm::MANIFESTS_UPLOAD_PATH,
                None,
                &manifest_bytes,
            )
            .await
            .map_err(|e| crate::nest_api::map_blob_http_err(e, "manifest upload"))?;
            super::verify_stored_under(reply.as_bytes(), manifest_hash, "manifest")
        }
    }

    /// The **member write door** in the browser (`ui/media.md` § Cross-nest
    /// shared sets, *Upload*): chunk and manifest POSTs addressed to a
    /// cross-nest set's HOME nest under the write token the member's own nest
    /// mints per POST (`fauna.folders.write_token.get`) — never the session
    /// bearer. The browser owns TLS, so the grant-delivered home identity
    /// cannot be pinned here (WebPKI only, as the read door).
    pub struct WasmMemberUploader {
        nest: WsRpcClient,
        home_nest_url: String,
        channel_id: String,
    }

    impl WasmMemberUploader {
        /// The door over `home_nest_url` for the set whose channel is
        /// `channel_id`, minting through `nest`.
        pub fn new(nest: WsRpcClient, home_nest_url: &str, channel_id: [u8; 32]) -> Self {
            Self {
                nest,
                home_nest_url: home_nest_url.trim_end_matches('/').to_string(),
                channel_id: hex::encode(channel_id),
            }
        }

        async fn token(&self) -> Result<String, MediaApiError> {
            crate::nest_api::ws_rpc::mint_member_write_token(
                &self.nest,
                self.home_nest_url.clone(),
                self.channel_id.clone(),
            )
            .await
            .map(|(token, _)| token)
            .map_err(crate::nest_api::ws_rpc::WriteMintError::into_media)
        }
    }

    #[async_trait::async_trait(?Send)]
    impl MediaBlobUploader for WasmMemberUploader {
        async fn post_blob(
            &self,
            _sidecar_cbor: Vec<u8>,
            _sealed_bytes: Vec<u8>,
        ) -> Result<String, MediaApiError> {
            Err(MediaApiError::BadRequest {
                detail: "a folder homed on another nest takes chunks, not a single blob"
                    .to_string(),
            })
        }

        async fn post_chunk(
            &self,
            store_key: [u8; 32],
            body: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let token = self.token().await?;
            let reply = fauna_rpc_wasm::post_octets_keyed_with_bearer(
                &self.home_nest_url,
                &token,
                fauna_rpc_wasm::CHUNKS_UPLOAD_PATH,
                Some(&hex::encode(store_key)),
                &body,
            )
            .await
            .map_err(|e| crate::nest_api::map_blob_http_err(e, "chunk upload"))?;
            super::verify_stored_under(reply.as_bytes(), store_key, "chunk")
        }

        async fn post_manifest(
            &self,
            manifest_hash: [u8; 32],
            manifest_bytes: Vec<u8>,
        ) -> Result<(), MediaApiError> {
            let token = self.token().await?;
            let reply = fauna_rpc_wasm::post_octets_keyed_with_bearer(
                &self.home_nest_url,
                &token,
                fauna_rpc_wasm::MANIFESTS_UPLOAD_PATH,
                None,
                &manifest_bytes,
            )
            .await
            .map_err(|e| crate::nest_api::map_blob_http_err(e, "manifest upload"))?;
            super::verify_stored_under(reply.as_bytes(), manifest_hash, "manifest")
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "rpc-glue"))]
pub use wasm::{WasmBlobUploader, WasmMemberUploader};
