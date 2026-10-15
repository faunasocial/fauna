//! HTTP client for a fauna node's **byte-plane** sync APIs — content-addressed
//! chunks, manifests, blobs, framed segments, snapshots, and folder metadata
//! (the HTTP residue per `docs/goal/architecture/api-layers.md` § File Sync).
//!
//! The device-sync **control plane** (register, change record/list) no longer
//! lives here: it rides the bearer WS-RPC connection via the shared
//! `fauna_client_sync::SyncClient` (`fauna.sync.{register,changes.{list,record}}`).
//! `SyncEngine` holds both — this `SyncClient` for bytes, the `NestClient` for
//! the control plane — and routes each call to the right one.

use std::time::Duration;

use anyhow::{Context, Result};
use fauna_core::data::ContentHash;
use fauna_nest_http::NestContentApi as _;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Bandwidth limiter
// ---------------------------------------------------------------------------

/// Simple token-bucket bandwidth limiter.
///
/// Tracks when the next transfer is allowed to start based on prior
/// consumption.  Concurrent callers serialize on the inner mutex, which
/// effectively shapes their aggregate bandwidth.
pub struct BandwidthLimiter {
    bytes_per_sec: u64,
    next_allowed: tokio::sync::Mutex<tokio::time::Instant>,
}

impl BandwidthLimiter {
    pub fn new(kbps: u32) -> Self {
        Self {
            bytes_per_sec: kbps as u64 * 1024,
            next_allowed: tokio::sync::Mutex::new(tokio::time::Instant::now()),
        }
    }

    /// Reserve `bytes` worth of bandwidth, sleeping until the slot is available.
    ///
    /// The lock is released before sleeping so concurrent callers can
    /// compute their own start times without waiting for prior sleeps.
    pub async fn wait(&self, bytes: usize) {
        let sleep_until = {
            let mut next = self.next_allowed.lock().await;
            let sleep_target = *next;
            let duration = Duration::from_secs_f64(bytes as f64 / self.bytes_per_sec as f64);
            *next = sleep_target.max(tokio::time::Instant::now()) + duration;
            sleep_target
        }; // lock released here
        tokio::time::sleep_until(sleep_until).await;
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// HTTP client for the fauna node sync API.
pub struct SyncClient {
    auth: std::sync::Arc<fauna_client::AuthClient>,
    tunnel_base_url: std::sync::RwLock<Option<String>>,
    device_id_hex: String,
    upload_limiter: Option<BandwidthLimiter>,
    download_limiter: Option<BandwidthLimiter>,
    /// The folder this client reads for, sent as the chunk GET's folder hint
    /// (`paths::chunk_store::FOLDER_HINT_PARAM`) so a store miss can be
    /// relayed from a holding seat — the only nest-mediated content path a
    /// metadata-only folder has (`file-sync.md` § Content residency). `None`
    /// for a folder-less client (snapshot restore, custodian pulls): the GET
    /// is then the plain hit-or-404 read.
    folder_hint: Option<String>,
    /// The folder's row, sent beside the name as the hint's ref form
    /// (`paths::chunk_store::FOLDER_REF_HINT_PARAM`) — the form the nest
    /// prefers, since a name finds the reader's own same-named set first.
    /// `None` until the engine's binding names it ([`Self::set_folder_ref`]).
    folder_ref: Option<fauna_core::folder_keys::FolderRef>,
}

/// Response wrapper for the chunk-existence check endpoint.
#[derive(Deserialize)]
struct CheckChunksResponse {
    missing: Vec<String>,
}

/// A segment's `.meta` sidecar could not be fetched: this nest no longer holds
/// that segment (the sidecar door answers 404 for it).
///
/// Typed for the same reason `fauna_account_store::segments::SegmentIncompatible`
/// is: the only honest rendering is a version/availability statement, and a
/// caller that must choose between "bulk-adopt" and "fall back to the feed
/// walk" needs to recognize the case rather than pattern-match a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentMetaUnavailable {
    pub kind: String,
    pub segment_id: u32,
}

impl std::fmt::Display for SegmentMetaUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no `.meta` sidecar served for {} segment {} — this nest no longer \
             holds that segment",
            self.kind, self.segment_id
        )
    }
}

impl std::error::Error for SegmentMetaUnavailable {}

/// The over-bound refusal lives with the store that counts it
/// ([`fauna_account_store::segments::SegmentBodyTooLarge`]); re-exported so the
/// byte plane's callers keep one path.
pub use fauna_account_store::segments::SegmentBodyTooLarge;

/// One segment half's body, streamed under its byte bound
/// ([`fauna_nest_http::capped::CappedBody`], the over-bound refusal typed as
/// [`SegmentBodyTooLarge`]).
///
/// A segment has no size ceiling (`message-segment-store.md` § Segment size),
/// so a half is never collected on a path that keeps it: the adoption arm
/// (`crate::bootstrap_source`) writes each chunk to the store's staging file as
/// it arrives, holding one transport chunk at a time.
pub struct SegmentBody<'a> {
    body: fauna_nest_http::capped::CappedBody,
    kind: String,
    segment_id: u32,
    /// `".dat"` or `".meta"`.
    half: &'static str,
    max_bytes: u64,
    limiter: Option<&'a BandwidthLimiter>,
}

impl SegmentBody<'_> {
    fn too_large(&self) -> anyhow::Error {
        SegmentBodyTooLarge {
            kind: self.kind.clone(),
            segment_id: self.segment_id,
            half: self.half,
            max_bytes: self.max_bytes,
        }
        .into()
    }

    /// The next chunk, or `None` at the end of the body. A chunk that would
    /// carry the body past its bound is refused, not returned.
    pub async fn next_chunk(&mut self) -> Result<Option<bytes::Bytes>> {
        use fauna_nest_http::capped::CappedReadError;
        match self.body.next_chunk().await {
            Ok(Some(chunk)) => {
                if let Some(limiter) = self.limiter {
                    limiter.wait(chunk.len()).await;
                }
                Ok(Some(chunk))
            }
            Ok(None) => Ok(None),
            Err(CappedReadError::TooLarge) => Err(self.too_large()),
            Err(e) => {
                Err(anyhow::Error::new(e).context(format!("read segment {} body", self.half)))
            }
        }
    }

    /// The whole body in memory — for the backup pass alone, which seals the
    /// pair from memory today (`crate::segment_backup::SourceBinding`). Never
    /// for adoption.
    pub async fn collect(mut self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }
}

/// Response wrapper for `POST /api/v1/blob` — the stored blob's hex content hash.
#[derive(Deserialize)]
struct BlobUploadResponse {
    hash: String,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl SyncClient {
    /// Create a new sync client backed by the given `AuthClient`.
    pub fn new(auth: std::sync::Arc<fauna_client::AuthClient>, device_id: &[u8; 32]) -> Self {
        Self {
            auth,
            tunnel_base_url: std::sync::RwLock::new(None),
            device_id_hex: hex::encode(device_id),
            upload_limiter: None,
            download_limiter: None,
            folder_hint: None,
            folder_ref: None,
        }
    }

    /// Name the folder this client reads for — every subsequent
    /// [`Self::download_chunk`] carries it as the relay hint. `SyncEngine::new`
    /// sets it from the engine's folder.
    pub fn set_folder_hint(&mut self, folder: Option<String>) {
        self.folder_hint = folder;
    }

    /// Name the folder's **row** too — the hint's ref form, which the nest
    /// prefers over the name (a name finds the reader's own same-named set
    /// first; `file-sync.md` § Relay serving, step (4)). `SyncEngine::with_folder_ref`
    /// sets it from the engine's binding, where the ref is known; a client
    /// with no ref sends the name forms alone.
    pub fn set_folder_ref(&mut self, folder_ref: Option<fauna_core::folder_keys::FolderRef>) {
        self.folder_ref = folder_ref;
    }

    /// The folder hint in force (see [`Self::set_folder_hint`]).
    pub fn folder_hint(&self) -> Option<&str> {
        self.folder_hint.as_deref()
    }

    /// The folder ref in force (see [`Self::set_folder_ref`]).
    pub fn folder_ref(&self) -> Option<fauna_core::folder_keys::FolderRef> {
        self.folder_ref
    }

    /// Get a bearer token from AuthClient.
    async fn token(&self) -> anyhow::Result<String> {
        self.auth
            .ensure_auth()
            .await
            .map_err(|e| anyhow::anyhow!("auth: {e}"))
    }

    /// Attach the bearer, send, and — on `401` — tell the bearer
    /// ([`fauna_client::AuthClient::clear_token`] → `BearerSource::notify_401`)
    /// and retry once under the re-minted one: the shape
    /// `fauna_nest_http::content` sends with. Every byte-plane call goes
    /// through here, so a token the nest stops honouring before its own
    /// expiry (a restart forgets the nest's in-memory write tokens) costs one
    /// retry, not every call until the TTL runs out. A second `401` is
    /// returned as is — one retry, never a loop — and any other status,
    /// a `404` miss included, never touches the bearer.
    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> Result<reqwest::Response> {
        let token = self.token().await?;
        let retry = req.try_clone();
        let resp = req
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| what.to_string())?;
        if resp.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(resp);
        }
        self.auth.clear_token().await;
        let Some(retry) = retry else {
            return Ok(resp);
        };
        let token = self.token().await?;
        retry
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| what.to_string())
    }

    /// Access the underlying AuthClient.
    pub fn auth(&self) -> &std::sync::Arc<fauna_client::AuthClient> {
        &self.auth
    }

    /// Set the tunnel base URL (e.g. "http://10.0.0.1:3000").
    /// When set, API calls prefer this URL, falling back to the public URL.
    pub fn set_tunnel_url(&self, url: Option<String>) {
        *self.tunnel_base_url.write().unwrap() = url;
    }

    /// Returns the effective base URL — tunnel if set, public otherwise.
    #[allow(dead_code)]
    fn effective_url(&self) -> String {
        self.tunnel_base_url
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.auth.nest_url().to_string())
    }

    /// Set bandwidth limits (in KB/s).  `None` = unlimited.
    pub fn set_bandwidth_limits(&mut self, upload_kbps: Option<u32>, download_kbps: Option<u32>) {
        self.upload_limiter = upload_kbps.map(BandwidthLimiter::new);
        self.download_limiter = download_kbps.map(BandwidthLimiter::new);
    }

    /// Return our device id hex for self-echo detection.
    pub fn device_id_hex(&self) -> &str {
        &self.device_id_hex
    }

    /// Return our actor id hex — the OTHER half of self-echo detection.
    ///
    /// A device id identifies a device only *within* an actor (`sync_devices` is
    /// keyed `(actor_id, device_id)`), so it cannot answer "did I write this row?"
    /// on its own for a set with more than one member. See
    /// [`super::engine::SyncEngine::row_is_own`].
    pub fn actor_id_hex(&self) -> String {
        self.auth.actor_id_hex()
    }

    /// Upload a single chunk by its content hash.
    pub async fn upload_chunk(&self, hash: &ContentHash, data: &[u8]) -> Result<()> {
        if let Some(ref limiter) = self.upload_limiter {
            limiter.wait(data.len()).await;
        }
        let url = format!("{}/api/v1/chunks", self.auth.nest_url());
        let hash_hex = hex::encode(hash.digest());
        let req = self
            .auth
            .http()
            .post(&url)
            .header("X-Content-Hash", &hash_hex)
            .body(data.to_vec());
        self.send(req, "POST /api/v1/chunks")
            .await?
            .error_for_status()
            .context("upload_chunk: unexpected status")?;
        Ok(())
    }

    /// Download a chunk by its content hash.
    ///
    /// Carries the folder hint when one is set ([`Self::set_folder_hint`]):
    /// on a nest store miss the hinted GET is relayed from a seat holding the
    /// bytes instead of answering `404` — additive, ignored on a hit, and
    /// actor-scoped on the nest (the bearer below is what scopes it).
    pub async fn download_chunk(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let hash_hex = hex::encode(hash.digest());
        let url = format!(
            "{}{}",
            self.auth.nest_url(),
            fauna_nest_http::paths::chunk_store::chunk_by_hash(&hash_hex)
        );
        tracing::debug!(url = %url, "downloading chunk");
        let mut req = self.auth.http().get(&url);
        if let Some(folder) = &self.folder_hint {
            // Every form the engine holds (`FolderHint`): the hash is the
            // address a sealed set is found by (its name rests blank on the
            // nest); the plaintext rides until the wire contraction retires
            // it; the ref, when the binding named it, is the form the nest
            // prefers — a name finds this actor's own same-named set first.
            req = req.query(
                &fauna_core::file_download::FolderHint::new(folder.as_str())
                    .with_ref(self.folder_ref)
                    .query_pairs(),
            );
        }
        let resp = self.send(req, "GET /api/v1/chunks/{hash}").await?;
        tracing::debug!(status = %resp.status(), "chunk download response");
        let resp = resp
            .error_for_status()
            .context("download_chunk: unexpected status")?;
        let bytes = resp.bytes().await.context("download_chunk: read body")?;
        if let Some(ref limiter) = self.download_limiter {
            limiter.wait(bytes.len()).await;
        }
        Ok(bytes.to_vec())
    }

    /// Answer the nest's relay ask `request_id` (`file-sync.md` § Relay
    /// serving, step (3)): `POST` the stored chunk, or `DELETE` when this seat
    /// holds none, so the relay's window refills at once. `Ok(false)` when the
    /// nest no longer has the ask pending — another seat answered first, or
    /// the reader went away — which is an ordinary outcome, not a failure.
    pub async fn answer_relay_ask(&self, request_id: u64, chunk: Option<&[u8]>) -> Result<bool> {
        let url = format!(
            "{}{}",
            self.auth.nest_url(),
            fauna_nest_http::paths::chunk_store::chunk_relay_answer(request_id)
        );
        let req = match chunk {
            Some(data) => {
                if let Some(ref limiter) = self.upload_limiter {
                    limiter.wait(data.len()).await;
                }
                self.auth.http().post(&url).body(data.to_vec())
            }
            None => self.auth.http().delete(&url),
        };
        let resp = self
            .send(req, "answer /api/v1/chunks/relay/{request_id}")
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        resp.error_for_status()
            .context("answer_relay_ask: unexpected status")?;
        Ok(true)
    }

    /// Download a framed segment file by (kind, actor_id, segment_id).
    ///
    /// `resume_from` is the byte offset to start from; 0 means a fresh
    /// fetch. When non-zero, sends `Range: bytes=<resume_from>-` so the
    /// nest's segment route returns 206 Partial Content. The response
    /// body is the framed file (header + payload + footer + trailer);
    /// the caller chunks it with FastCDC via `SyncEngine::upload_bytes`.
    ///
    /// Plan 5 T7: used by the custodian pull's [`SourceBinding`] arm
    /// (`crate::segment_backup`) to fetch source segments.
    ///
    /// **`max_bytes` bounds the body** — refused as [`SegmentBodyTooLarge`]
    /// while it streams, never after buffering it (`u64::MAX` = unbounded).
    /// A segment has no legal size ceiling (it rolls only on its month
    /// bucket), so the bound is the caller's: the listing's declared size, a
    /// custody's remaining budget.
    pub async fn get_segment_bytes(
        &self,
        kind: &str,
        actor_hex: &str,
        segment_id: u32,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        self.segment_body(kind, actor_hex, segment_id, resume_from, max_bytes)
            .await?
            .collect()
            .await
    }

    /// [`Self::get_segment_bytes`] as a stream — the form a caller that keeps
    /// the half uses, writing each chunk out as it arrives
    /// ([`SegmentBody::next_chunk`]) so the half is never in memory whole.
    pub async fn segment_body(
        &self,
        kind: &str,
        actor_hex: &str,
        segment_id: u32,
        resume_from: u64,
        max_bytes: u64,
    ) -> Result<SegmentBody<'_>> {
        let url = format!(
            "{}/api/v1/segments/{}/{}/{}",
            self.auth.nest_url(),
            kind,
            actor_hex,
            segment_id
        );
        let mut req = self.auth.http().get(&url);
        if resume_from > 0 {
            req = req.header("Range", format!("bytes={resume_from}-"));
        }
        let what = format!("GET /api/v1/segments/{kind}/{actor_hex}/{segment_id}");
        let resp = self
            .send(req, &what)
            .await?
            .error_for_status()
            .context("get_segment_bytes: unexpected status")?;
        self.segment_half(resp, kind, segment_id, ".dat", max_bytes)
    }

    /// Download a segment's `.meta` sidecar — the other half of the pair an
    /// **adopting** replica needs (`account-data-plane.md` § the bootstrap
    /// contract), and, since 2026-08-29, the half the **custodian pull**
    /// ships opaque beside the `.dat` so the backup corpus is reopenable
    /// (`crate::segment_backup::SourceBinding::segment_pair`). A backup pass
    /// still never reads `record_order`.
    ///
    /// No `resume_from`: the sidecar is a small dag-cbor blob beside its
    /// `.dat` (`message-segment-store.md` § Segment file format), so a
    /// resumable fetch would be machinery with nothing to resume. The `.dat`
    /// keeps its Range path. Small is not bounded, though — the sidecar grows
    /// with the segment's record count — so `max_bytes` bounds it exactly as
    /// it bounds [`Self::get_segment_bytes`].
    ///
    /// A segment this nest no longer holds answers 404, which surfaces here as
    /// [`SegmentMetaUnavailable`] rather than a generic status error — a
    /// stated shape a caller can recognize (§ I2: a peer that cannot serve the
    /// sidecar degrades on a stated shape), and a reason to fall back to the
    /// feed walk.
    pub async fn get_segment_meta_bytes(
        &self,
        kind: &str,
        actor_hex: &str,
        segment_id: u32,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        self.segment_meta_body(kind, actor_hex, segment_id, max_bytes)
            .await?
            .collect()
            .await
    }

    /// [`Self::get_segment_meta_bytes`] as a stream — see
    /// [`Self::segment_body`].
    pub async fn segment_meta_body(
        &self,
        kind: &str,
        actor_hex: &str,
        segment_id: u32,
        max_bytes: u64,
    ) -> Result<SegmentBody<'_>> {
        let url = format!(
            "{}/api/v1/segments/{}/{}/{}/meta",
            self.auth.nest_url(),
            kind,
            actor_hex,
            segment_id
        );
        let what = format!("GET /api/v1/segments/{kind}/{actor_hex}/{segment_id}/meta");
        let resp = self.send(self.auth.http().get(&url), &what).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(SegmentMetaUnavailable {
                kind: kind.to_string(),
                segment_id,
            }
            .into());
        }
        let resp = resp
            .error_for_status()
            .context("get_segment_meta_bytes: unexpected status")?;
        self.segment_half(resp, kind, segment_id, ".meta", max_bytes)
    }

    /// Wrap one half's response in its bound — a truthful `Content-Length`
    /// over the bound is refused here, before any body byte moves.
    fn segment_half(
        &self,
        resp: reqwest::Response,
        kind: &str,
        segment_id: u32,
        half: &'static str,
        max_bytes: u64,
    ) -> Result<SegmentBody<'_>> {
        let too_large = || -> anyhow::Error {
            SegmentBodyTooLarge {
                kind: kind.to_string(),
                segment_id,
                half,
                max_bytes,
            }
            .into()
        };
        let body =
            fauna_nest_http::capped::CappedBody::new(resp, max_bytes).map_err(|_| too_large())?;
        Ok(SegmentBody {
            body,
            kind: kind.to_string(),
            segment_id,
            half,
            max_bytes,
            limiter: self.download_limiter.as_ref(),
        })
    }

    /// Check which of the given chunk hashes are missing on the node.
    pub async fn check_chunks(&self, hashes: &[ContentHash]) -> Result<Vec<ContentHash>> {
        let url = format!("{}/api/v1/chunks/check", self.auth.nest_url());
        let hex_list: Vec<String> = hashes.iter().map(|h| hex::encode(h.digest())).collect();
        let body = serde_json::json!({ "hashes": hex_list });
        let req = self.auth.http().post(&url).json(&body);
        let resp: CheckChunksResponse = self
            .send(req, "POST /api/v1/chunks/check")
            .await?
            .error_for_status()
            .context("check_chunks: unexpected status")?
            .json()
            .await
            .context("check_chunks: parse response")?;

        let missing = resp
            .missing
            .iter()
            .map(|h| {
                let bytes = hex::decode(h).context("check_chunks: decode hex hash")?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("check_chunks: hash not 32 bytes"))?;
                Ok(ContentHash::from_digest_raw(arr))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(missing)
    }

    /// Upload a manifest (serialized bytes).
    pub async fn upload_manifest(&self, manifest_bytes: &[u8]) -> Result<()> {
        let url = format!("{}/api/v1/manifests", self.auth.nest_url());
        let req = self.auth.http().post(&url).body(manifest_bytes.to_vec());
        self.send(req, "POST /api/v1/manifests")
            .await?
            .error_for_status()
            .context("upload_manifest: unexpected status")?;
        Ok(())
    }

    /// Download a manifest by its content hash.
    pub async fn download_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let hash_hex = hex::encode(hash.digest());
        let url = format!("{}/api/v1/manifests/{}", self.auth.nest_url(), hash_hex);
        let bytes = self
            .send(self.auth.http().get(&url), "GET /api/v1/manifests/{hash}")
            .await?
            .error_for_status()
            .context("download_manifest: unexpected status")?
            .bytes()
            .await
            .context("download_manifest: read body")?;
        Ok(bytes.to_vec())
    }

    /// Upload an opaque blob.
    pub async fn upload_blob(&self, data: &[u8]) -> Result<()> {
        let url = format!("{}/api/v1/blob", self.auth.nest_url());
        let req = self.auth.http().post(&url).body(data.to_vec());
        self.send(req, "POST /api/v1/blob")
            .await?
            .error_for_status()
            .context("upload_blob: unexpected status")?;
        Ok(())
    }

    /// Upload a sealed blob as `multipart/form-data` (`sidecar` + `bytes`
    /// parts), returning the nest-assigned hex content hash.
    ///
    /// Mirrors the Media-library producer's `post_multipart_blob`
    /// (`libs/fauna-nest-http/src/content.rs`): a sealed thumbnail rides as a
    /// standalone blob carrying its `UploadSidecar`, so the encrypted-mode
    /// verifier sees the right class. Used by the SyncEngine thumbnail producer
    /// — the returned hash is recorded on the folder member and a client
    /// fetches it direct-by-hash (`GET /api/v1/blob/<hash>`).
    pub async fn upload_blob_multipart(&self, sidecar_cbor: &[u8], bytes: &[u8]) -> Result<String> {
        if let Some(ref limiter) = self.upload_limiter {
            limiter.wait(bytes.len()).await;
        }
        let body = self
            .auth
            .content_api()
            .post_multipart_blob("/api/v1/blob", sidecar_cbor.to_vec(), bytes.to_vec())
            .await
            .context("upload_blob_multipart")?;
        let resp: BlobUploadResponse =
            serde_json::from_slice(&body).context("upload_blob_multipart: parse response")?;
        Ok(resp.hash)
    }

    /// [`download_blob`](Self::download_blob) with a **typed miss**: `Ok(None)`
    /// when the nest holds no blob under this hash, `Err` for every other
    /// failure.
    ///
    /// The post-succession thumbnail move's fetch: a thumbnail the nest no
    /// longer holds is nothing to move, while a transport failure must stay an
    /// error. Collapsing the two would be the dangerous reading: a 500 or a
    /// dropped connection would be scored "gone", and the thumbnail pointer
    /// would drop rather than be retried.
    pub async fn download_blob_opt(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let hash_hex = hex::encode(hash.digest());
        let url = format!("{}/api/v1/blob/{}", self.auth.nest_url(), hash_hex);
        let resp = self
            .send(self.auth.http().get(&url), "GET /api/v1/blob/{hash}")
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = resp
            .error_for_status()
            .context("download_blob_opt: unexpected status")?
            .bytes()
            .await
            .context("download_blob_opt: read body")?;
        Ok(Some(bytes.to_vec()))
    }

    /// Download a blob by its content hash.
    pub async fn download_blob(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let hash_hex = hex::encode(hash.digest());
        let url = format!("{}/api/v1/blob/{}", self.auth.nest_url(), hash_hex);
        let bytes = self
            .send(self.auth.http().get(&url), "GET /api/v1/blob/{hash}")
            .await?
            .error_for_status()
            .context("download_blob: unexpected status")?
            .bytes()
            .await
            .context("download_blob: read body")?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use fauna_nest_http::StaticBearer;
    use std::sync::Arc;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a `SyncClient` whose `AuthClient` points at `server.uri()` and
    /// always returns a fixed bearer (no `/auth/token` round trip needed).
    fn test_client(server_uri: &str) -> SyncClient {
        let kp = ActorKeypair::generate();
        let http = reqwest::Client::new();
        let bearer: Arc<dyn fauna_nest_http::BearerSource> =
            Arc::new(StaticBearer("test.bearer".to_string()));
        let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
            server_uri.to_string(),
            kp,
            bearer,
            http,
        ));
        let device_id = [0u8; 32];
        SyncClient::new(auth, &device_id)
    }

    /// A folder-scoped client names its folder on every chunk GET (the relay
    /// hint, phase 5); a folder-less one sends the bare path. Matched on the
    /// query string itself, so a hint that rides as a header, a path segment
    /// or not at all fails here.
    #[tokio::test]
    async fn download_chunk_carries_the_folder_hint_iff_one_is_set() {
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        let data = b"chunk bytes".to_vec();
        let hash = ContentHash::of_raw(&data);
        let route = fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest()));
        Mock::given(method("GET"))
            .and(path(route.clone()))
            .and(query_param("folder", "Photos & more"))
            .and(query_param(
                "folder_hash",
                hex::encode(fauna_core::path_crypto::set_name_hash("Photos & more")),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;

        let mut hinted = test_client(&server.uri());
        hinted.set_folder_hint(Some("Photos & more".to_string()));
        assert_eq!(hinted.download_chunk(&hash).await.unwrap(), data);

        // Only the hinted mock is mounted, so a bare GET falls through to 404.
        let bare = test_client(&server.uri());
        assert!(bare.folder_hint().is_none());
        bare.download_chunk(&hash)
            .await
            .expect_err("a client with no folder hint must not send one");
    }

    /// A client whose binding named the folder's row sends the ref form
    /// beside the two name forms — the form the nest prefers, so a member
    /// owning a same-named set still reads the shared one (`file-sync.md`
    /// § Relay serving, step (4)). Matched on the query, with the name-only
    /// mock absent, so a ref that rides anywhere else, or not at all, fails.
    #[tokio::test]
    async fn download_chunk_carries_the_folder_ref_when_the_binding_named_it() {
        use wiremock::matchers::query_param;

        let server = MockServer::start().await;
        let data = b"chunk bytes by ref".to_vec();
        let hash = ContentHash::of_raw(&data);
        let route = fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest()));
        Mock::given(method("GET"))
            .and(path(route.clone()))
            .and(query_param("folder", "photos"))
            .and(query_param(
                "folder_hash",
                hex::encode(fauna_core::path_crypto::set_name_hash("photos")),
            ))
            .and(query_param("folder_ref", "local:42"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;

        let mut by_name = test_client(&server.uri());
        by_name.set_folder_hint(Some("photos".to_string()));
        assert!(by_name.folder_ref().is_none());
        by_name
            .download_chunk(&hash)
            .await
            .expect_err("a client with no ref sends the name forms alone");

        let mut by_ref = test_client(&server.uri());
        by_ref.set_folder_hint(Some("photos".to_string()));
        by_ref.set_folder_ref(Some(fauna_core::folder_keys::FolderRef::Local(42)));
        assert_eq!(by_ref.download_chunk(&hash).await.unwrap(), data);
    }

    /// A bearer that answers `stale` until told of a `401`, then `fresh` —
    /// counting the tellings, so a test can pin "re-minted exactly once"
    /// (the fixture's `StaticBearer` no-ops `notify_401`).
    #[derive(Default)]
    struct CountingBearer {
        notified: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl fauna_nest_http::BearerSource for CountingBearer {
        async fn bearer(&self) -> Result<String, fauna_nest_http::ApiError> {
            let told = self.notified.load(std::sync::atomic::Ordering::SeqCst);
            Ok(if told == 0 { "stale" } else { "fresh" }.to_string())
        }
        async fn notify_401(&self) {
            self.notified
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn counting_client(server_uri: &str) -> (SyncClient, Arc<CountingBearer>) {
        let counter = Arc::new(CountingBearer::default());
        let bearer: Arc<dyn fauna_nest_http::BearerSource> = counter.clone();
        let auth = Arc::new(fauna_client::AuthClient::with_bearer_source(
            server_uri.to_string(),
            ActorKeypair::generate(),
            bearer,
            reqwest::Client::new(),
        ));
        (SyncClient::new(auth, &[0u8; 32]), counter)
    }

    fn told(counter: &CountingBearer) -> usize {
        counter.notified.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A byte-plane `401` (a token the home nest no longer honours — its
    /// in-memory token store forgot it across a restart) is fed back to the
    /// bearer once and the call retried once under the re-minted token: the
    /// read and the upload (whose body must ride the retry too) both land.
    #[tokio::test]
    async fn a_byte_plane_401_re_mints_once_and_retries_once() {
        let server = MockServer::start().await;
        let data = b"chunk bytes".to_vec();
        let hash = ContentHash::of_raw(&data);
        let route = fauna_nest_http::paths::chunk_store::chunk_by_hash(&hex::encode(hash.digest()));
        Mock::given(header("authorization", "Bearer stale"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(route))
            .and(header("authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chunks"))
            .and(header("authorization", "Bearer fresh"))
            .and(wiremock::matchers::body_bytes(data.clone()))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let (client, counter) = counting_client(&server.uri());
        assert_eq!(client.download_chunk(&hash).await.unwrap(), data);
        assert_eq!(told(&counter), 1, "the bearer is asked to re-mint once");

        let (client, counter) = counting_client(&server.uri());
        client.upload_chunk(&hash, &data).await.unwrap();
        assert_eq!(told(&counter), 1);
    }

    /// A `404` is an ordinary miss — the relay answer's "no longer pending"
    /// included — and never touches the bearer.
    #[tokio::test]
    async fn a_byte_plane_404_never_re_mints() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path(
                fauna_nest_http::paths::chunk_store::chunk_relay_answer(7),
            ))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let (client, counter) = counting_client(&server.uri());
        assert!(!client.answer_relay_ask(7, None).await.unwrap());
        let hash = ContentHash::of_raw(b"absent");
        client.download_chunk(&hash).await.expect_err("a miss");
        assert_eq!(told(&counter), 0);
    }

    /// A `401` that survives the re-mint fails the call — one retry, no loop.
    #[tokio::test]
    async fn a_second_401_fails_without_a_loop() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;

        let (client, counter) = counting_client(&server.uri());
        let hash = ContentHash::of_raw(b"never served");
        client.download_chunk(&hash).await.expect_err("still 401");
        assert_eq!(told(&counter), 1);
        server.verify().await;
    }

    #[tokio::test]
    async fn get_segment_bytes_no_range_returns_full_body() {
        let server = MockServer::start().await;
        let body: Vec<u8> = (0u8..=255).collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/segments/mail/abcd/0"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
        let client = test_client(&server.uri());
        let got = client
            .get_segment_bytes("mail", "abcd", 0, 0, u64::MAX)
            .await
            .expect("get_segment_bytes should succeed");
        assert_eq!(got, body);
    }

    #[tokio::test]
    async fn get_segment_bytes_with_range_sends_range_header() {
        let server = MockServer::start().await;
        // Only requests carrying `Range: bytes=128-` match — a missing
        // or wrong header falls through to a 404, which would surface
        // as a transport error from `get_segment_bytes`.
        Mock::given(method("GET"))
            .and(path("/api/v1/segments/mail/abcd/7"))
            .and(header("Range", "bytes=128-"))
            .respond_with(ResponseTemplate::new(206).set_body_bytes(vec![0xAAu8; 64]))
            .mount(&server)
            .await;
        let client = test_client(&server.uri());
        let got = client
            .get_segment_bytes("mail", "abcd", 7, 128, u64::MAX)
            .await
            .expect("get_segment_bytes with Range should succeed");
        assert_eq!(got, vec![0xAAu8; 64]);
    }

    #[tokio::test]
    async fn get_segment_bytes_propagates_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/segments/mail/abcd/0"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = test_client(&server.uri());
        let err = client
            .get_segment_bytes("mail", "abcd", 0, 0, u64::MAX)
            .await
            .expect_err("non-owner should fail");
        let s = format!("{err:#}");
        assert!(s.contains("403") || s.contains("Forbidden"), "got: {s}");
    }

    /// A declared length over the bound is refused before the body is read,
    /// typed so a caller can tell "too large" from a transport fault — and the
    /// same body under a bound that fits is served whole.
    #[tokio::test]
    async fn get_segment_bytes_refuses_a_declared_body_over_its_bound() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/segments/post/abcd/3"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 1000]))
            .mount(&server)
            .await;
        let client = test_client(&server.uri());
        let err = client
            .get_segment_bytes("post", "abcd", 3, 0, 999)
            .await
            .expect_err("one byte over the bound");
        let too_large = err
            .downcast_ref::<SegmentBodyTooLarge>()
            .expect("typed refusal");
        assert_eq!(too_large.half, ".dat");
        assert_eq!(too_large.max_bytes, 999);
        assert_eq!(
            client
                .get_segment_bytes("post", "abcd", 3, 0, 1000)
                .await
                .unwrap()
                .len(),
            1000
        );
    }

    /// The pin the unbounded read failed: a responder that sends
    /// no `Content-Length` and never stops. `resp.bytes()` would buffer until
    /// the process died; the bounded read stops one chunk past the cap. Both
    /// halves are pinned, since the sidecar is attacker-sized too. Mutate:
    /// restore `resp.bytes()` in either getter and its half times out here.
    #[tokio::test]
    async fn segment_halves_stop_an_endless_unlengthed_body_at_the_bound() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut req = [0u8; 4096];
                    let _ = sock.read(&mut req).await;
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                                Connection: close\r\n\r\n";
                    if sock.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let chunk = vec![0x5Au8; 64 * 1024];
                    while sock.write_all(&chunk).await.is_ok() {}
                });
            }
        });
        let client = test_client(&format!("http://{addr}"));
        let bound = 256 * 1024;

        let dat = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client.get_segment_bytes("post", "abcd", 1, 0, bound),
        )
        .await
        .expect("the .dat read stopped at its bound instead of buffering forever")
        .expect_err("an endless body is over any bound");
        assert!(
            dat.downcast_ref::<SegmentBodyTooLarge>().is_some(),
            "got: {dat:#}"
        );

        let meta = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            client.get_segment_meta_bytes("post", "abcd", 1, bound),
        )
        .await
        .expect("the .meta read stopped at its bound instead of buffering forever")
        .expect_err("an endless body is over any bound");
        assert!(
            meta.downcast_ref::<SegmentBodyTooLarge>().is_some(),
            "got: {meta:#}"
        );
    }
}
