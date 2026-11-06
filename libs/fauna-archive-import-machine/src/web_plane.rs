//! The browser [`ArchivePlane`] — the web twin of [`crate::rpc_glue::NativePlane`]
//! (`archive-import.md` § The wizard and its machine; the SPA's archive-import
//! chunk and its core chunk both build it, so the page's run and the
//! `Archive` rail's reads go through one plane).
//!
//! Every leg rides the SPA's own session, the role the content API and the
//! sync engine's chunk store play natively:
//!
//! - reads: [`fauna_core::file_download::WasmPublicChunkFetcher`], its folder
//!   reads under the session bearer (the relay hint, `file-sync.md` § Relay
//!   serving), as the Media page's download leg;
//! - `put_sealed`: `POST /api/v1/chunks` per chunk, then the manifest, through
//!   `fauna_rpc_wasm::post_octets_keyed` (the Backups recovery plane's shape).
//!   No dedup check: the browser has no `POST /chunks/check` leg, and the store
//!   is content-addressed under a deterministic seal, so a re-put is idempotent;
//! - the three blob posts: `fauna_media::pipeline::process_and_seal{,_with_mime}`
//!   and `UploadPayload::into_multipart_parts`, POSTed thumbnail first and
//!   best-effort, the primary's echoed hash checked against the local blake3
//!   (`security.md` § sign-over-cid) — `fauna_client::media_upload`'s order and
//!   check, over `fauna_rpc_wasm::post_multipart_blob`.
//!
//! The identity seed is handed in by the page (the tab's identity secret): the
//! browser connection authenticates by token and holds no keypair of its own.

use std::rc::Rc;

use fauna_core::blob_seal::SealedBlob;
use fauna_core::data::ContentHash;
use fauna_core::file_download::{BlobFetcher, FolderHint, SessionBearer, WasmPublicChunkFetcher};
use fauna_media::audience::{Audience, RestrictedPostAudience};
use fauna_media::pipeline::{MultipartBlob, process_and_seal, process_and_seal_with_mime};
use fauna_media::sidecar::UploadSidecar;
use fauna_rpc_wasm::WsRpcClient;
use zeroize::Zeroizing;

use crate::nest::{MediaSeal, UploadedMedia};
use crate::rpc_glue::ArchivePlane;

/// The browser plane over the SPA's connected `WsRpcClient` (a cheap `Rc`
/// handle, held by value).
pub struct WebPlane {
    nest: WsRpcClient,
    seed: Zeroizing<[u8; 32]>,
    fetcher: WasmPublicChunkFetcher,
}

impl WebPlane {
    /// `seed` is the owner's identity secret — the root the archive folder's
    /// chunks seal under, and what [`ArchivePlane::identity_seed`] answers.
    pub fn new(nest: WsRpcClient, seed: Zeroizing<[u8; 32]>) -> Self {
        let fetcher = WasmPublicChunkFetcher::new(nest.nest_url().trim_end_matches('/'))
            .with_session_bearer(Rc::new(Bearer(nest.clone())));
        Self {
            nest,
            seed,
            fetcher,
        }
    }

    /// POST one blob's multipart parts and answer its hex content hash: the
    /// client's own `blake3`, which the nest's echo must equal.
    async fn post_parts(
        &self,
        primary: MultipartBlob,
        thumbnail: Option<MultipartBlob>,
    ) -> Result<String, String> {
        if let Some(thumb) = thumbnail
            && let Err(e) = fauna_rpc_wasm::post_multipart_blob(
                &self.nest,
                fauna_rpc_wasm::BLOB_UPLOAD_PATH,
                &thumb.sidecar_cbor,
                &thumb.bytes,
            )
            .await
        {
            tracing::warn!("blob upload: thumbnail failed (non-fatal): {e}");
        }
        let local = primary.content_hash_hex();
        let reply = fauna_rpc_wasm::post_multipart_blob(
            &self.nest,
            fauna_rpc_wasm::BLOB_UPLOAD_PATH,
            &primary.sidecar_cbor,
            &primary.bytes,
        )
        .await
        .map_err(|e| format!("blob upload: {e}"))?;
        let echoed = echoed_hash(&reply).ok_or("blob upload response missing hash")?;
        if !echoed.eq_ignore_ascii_case(&local) {
            return Err(format!(
                "blob upload hash mismatch: nest echoed {echoed}, local blake3 is {local}"
            ));
        }
        Ok(local)
    }

    /// POST one chunk-store body and check the nest filed it under `expected`.
    async fn put_keyed(
        &self,
        path: &str,
        key: Option<&ContentHash>,
        body: &[u8],
        expected: &ContentHash,
        what: &str,
    ) -> anyhow::Result<()> {
        let key_hex = key.map(|k| hex::encode(k.digest()));
        let reply = fauna_rpc_wasm::post_octets_keyed(&self.nest, path, key_hex.as_deref(), body)
            .await
            .map_err(|e| anyhow::anyhow!("{what} upload: {e}"))?;
        let filed = echoed_hash(&reply)
            .ok_or_else(|| anyhow::anyhow!("{what} upload: unreadable reply"))?;
        let want = hex::encode(expected.digest());
        anyhow::ensure!(
            filed.eq_ignore_ascii_case(&want),
            "{what} upload: the nest filed the body under {filed}, not {want}"
        );
        Ok(())
    }
}

/// The `hash` of the nest's `{"hash": "<hex>"}` upload reply.
fn echoed_hash(reply: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Reply {
        hash: String,
    }
    serde_json::from_str::<Reply>(reply).ok().map(|r| r.hash)
}

fn uploaded(hash_hex: &str, media_type: String, size: usize) -> Result<UploadedMedia, String> {
    Ok(UploadedMedia {
        blob_hash: fauna_core::hex32::decode(hash_hex).map_err(|e| format!("blob hash: {e}"))?,
        media_type,
        size_bytes: size as u64,
    })
}

/// The session bearer for the fetcher's hinted folder reads.
struct Bearer(WsRpcClient);

#[async_trait::async_trait(?Send)]
impl SessionBearer for Bearer {
    async fn bearer(&self) -> anyhow::Result<String> {
        self.0
            .bearer(false)
            .await
            .map_err(|e| anyhow::anyhow!("session bearer: {e}"))
    }
}

#[async_trait::async_trait(?Send)]
impl BlobFetcher for WebPlane {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        self.fetcher.fetch_manifest(hash).await
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        self.fetcher.fetch_chunks(store_keys, relative_path).await
    }

    async fn fetch_folder_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
        folder: &FolderHint,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        self.fetcher
            .fetch_folder_chunks(store_keys, relative_path, folder)
            .await
    }
}

#[async_trait::async_trait(?Send)]
impl ArchivePlane for WebPlane {
    fn identity_seed(&self) -> Option<Zeroizing<[u8; 32]>> {
        Some(self.seed.clone())
    }

    async fn put_sealed(&self, sealed: &SealedBlob) -> anyhow::Result<()> {
        for (key, body) in &sealed.chunks {
            self.put_keyed(
                fauna_rpc_wasm::CHUNKS_UPLOAD_PATH,
                Some(key),
                body,
                key,
                "chunk",
            )
            .await?;
        }
        self.put_keyed(
            fauna_rpc_wasm::MANIFESTS_UPLOAD_PATH,
            None,
            &sealed.manifest_bytes,
            &sealed.manifest_hash,
            "manifest",
        )
        .await
    }

    async fn post_public_media(&self, _name: &str, bytes: &[u8]) -> Result<UploadedMedia, String> {
        // The public seal derives no key; the blob precedes its post, so a zero
        // id is the honest value (`fauna_client::media_upload`'s public payload).
        let audience = Audience::PublicPost {
            post_id: ContentHash::from_digest_raw([0u8; 32]),
        };
        let upload = process_and_seal(bytes, &audience);
        let media_type = upload.primary_sidecar.mime.clone();
        let (primary, thumbnail) = upload.into_multipart_parts();
        let hash = self.post_parts(primary, thumbnail).await?;
        uploaded(&hash, media_type, bytes.len())
    }

    async fn post_period_sealed_media(
        &self,
        _name: &str,
        bytes: &[u8],
        seal: &MediaSeal,
    ) -> Result<UploadedMedia, String> {
        let audience = Audience::RestrictedPost {
            post_id: ContentHash::from_digest_raw(seal.seal_id),
            audience: RestrictedPostAudience::Period {
                tier: seal.tier.clone(),
                period_epoch: seal.period_version,
                period_key: seal.period_key.clone(),
            },
        };
        let (upload, media_type) = process_and_seal_with_mime(bytes, &audience);
        let (primary, thumbnail) = upload.into_multipart_parts();
        let hash = self.post_parts(primary, thumbnail).await?;
        uploaded(&hash, media_type, bytes.len())
    }

    async fn post_gated_body(&self, sealed: Vec<u8>) -> Result<String, String> {
        let reply = fauna_rpc_wasm::post_multipart_blob(
            &self.nest,
            fauna_rpc_wasm::BLOB_UPLOAD_PATH,
            &UploadSidecar::gated_post().to_dag_cbor(),
            &sealed,
        )
        .await
        .map_err(|e| format!("gated blob upload: {e}"))?;
        echoed_hash(&reply).ok_or_else(|| "gated blob upload response missing hash".to_string())
    }
}
