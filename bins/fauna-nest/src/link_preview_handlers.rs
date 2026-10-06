//! WS-RPC handler for the D4 link-preview surface — `fauna.linkpreview.resolve`
//! (`docs/goal/architecture/render-model.md` § D4).
//!
//! The per-app render manager emits a `RenderBlock::LinkPreview { url, state:
//! Resolving }` for a standalone bare-url paragraph and then calls this
//! authenticated kind to fill it in. The producer is **nest-side** because a
//! client-side fetch leaks every user's IP to every linked site and is
//! CORS-blocked on web (a priority-#1 per-app divergence): so the nest
//! fetches the page behind its SSRF/size/time guards (`link_preview::fetch`),
//! parses the OpenGraph/`<meta>` tags (`link_preview::parse`), stores the
//! og:image as a public content-addressed blob (the client resolves it through
//! the existing `GET /api/v1/blob/<hash>` media path, blocked-by-default per
//! D3), caches the result by url, and returns the wire reply.
//!
//! The business logic (fetch + parse + image-fetch) is **not** duplicated here —
//! it lives in `link_preview::resolve_preview_meta`; this module wires it to
//! `AppState` (cache, per-actor rate limit, blob store) and the wire types.

use std::sync::Arc;
use std::time::Duration;

// Only the `test-hooks` `OverrideFetcher` names `Bytes` directly (the real
// fetcher returns it inside `FetchedResource`), so the import carries the same
// gate its one use site does.
#[cfg(feature = "test-hooks")]
use bytes::Bytes;

use fauna_protocol::{
    RpcError, decode_strict as decode,
    linkpreview::{KIND_LINKPREVIEW_RESOLVE, LinkPreviewResolveReply, LinkPreviewResolveRequest},
};

use crate::link_preview::{self, FetchedImage, MetaResolution, cache::CachedPreview};
use crate::routes::AppState;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};

// ── Helpers (the per-handler-module convention; see `posts_handlers.rs`) ──────

use crate::rpc_errors::{encode_reply, malformed};

/// The per-actor resolve budget for this window is spent — back off. Reuses the
/// generic protocol rate-limit code (no new i18n key), like `invite_handlers`.
fn rate_limited() -> RpcError {
    crate::rpc_errors::rate_limited()
}

/// Reject an over-long request url *before* it becomes a cache key or reaches the
/// fetcher (LP-2). The url is otherwise bounded only by the global WS message cap
/// (F-CL1); [`link_preview::MAX_URL_LEN`] is a far tighter, semantic bound.
fn check_url_len(url: &str) -> Result<(), RpcError> {
    if url.len() > link_preview::MAX_URL_LEN {
        return Err(malformed(format!(
            "url length {} exceeds the {}-byte cap",
            url.len(),
            link_preview::MAX_URL_LEN
        )));
    }
    Ok(())
}

use crate::bridge_method_allowlist::require_permission_default as require_permission;

// ── fauna.linkpreview.resolve ────────────────────────────────────────────────

fn link_preview_resolve_handler() -> RpcHandler {
    Box::new(|state, actor_id, payload| {
        Box::pin(async move {
            require_permission(&state, &actor_id, KIND_LINKPREVIEW_RESOLVE).await?;
            let req: LinkPreviewResolveRequest = decode(&payload).map_err(malformed)?;
            let url = req.url;

            // Reject an implausibly long url before it touches the cache (as a
            // key) or the fetcher (LP-2). Sits ahead of `cache.get` so a rejected
            // url is never inserted.
            check_url_len(&url)?;

            // A by-url cache hit is free — no outbound fetch, so it does not
            // spend the actor's rate-limit budget.
            if let Some(p) = state.link_preview.cache.get(&url) {
                return encode_reply(&LinkPreviewResolveReply::Resolved {
                    title: p.title,
                    description: p.description,
                    image_hash: p.image_hash,
                });
            }

            // Only the fetch path is rate-limited (the resolve surface keys on
            // the caller actor alone, so the bridge limiter's two pubkey axes
            // are both the actor and the credential axis is the kind).
            if !state.link_preview.rate_limiter.check(
                &actor_id,
                &actor_id,
                KIND_LINKPREVIEW_RESOLVE,
            ) {
                return Err(rate_limited());
            }

            match resolve(&state, &url).await {
                MetaResolution::Resolved {
                    title,
                    description,
                    image,
                } => {
                    let image_hash = match image {
                        Some(img) => store_preview_image(&state, img).await,
                        None => None,
                    };
                    let preview = CachedPreview {
                        title,
                        description,
                        image_hash,
                    };
                    state.link_preview.cache.insert(url, preview.clone());
                    encode_reply(&LinkPreviewResolveReply::Resolved {
                        title: preview.title,
                        description: preview.description,
                        image_hash: preview.image_hash,
                    })
                }
                MetaResolution::Failed => encode_reply(&LinkPreviewResolveReply::Failed),
            }
        })
    })
}

/// Resolve `url`'s preview metadata via the production SSRF-safe fetcher. Under
/// `test-hooks`, a scripted fixture in `AppState::link_preview_override` is
/// consulted ahead of the live fetcher **only for urls present in the map** —
/// an unmapped url (e.g. a private-IP literal) still falls through to the real
/// SSRF rejection, so the e2e exercises both paths without loosening production.
async fn resolve(state: &Arc<AppState>, url: &str) -> MetaResolution {
    #[cfg(feature = "test-hooks")]
    {
        let overrides = state
            .link_preview_override
            .lock()
            .expect("link_preview_override mutex poisoned")
            .clone();
        if !overrides.is_empty() {
            let fetcher = OverrideFetcher {
                overrides,
                live: state.link_preview.fetcher.clone(),
            };
            return link_preview::resolve_preview_meta(&fetcher, url).await;
        }
    }
    link_preview::resolve_preview_meta(state.link_preview.fetcher.as_ref(), url).await
}

/// Store a fetched og:image as a **public, content-addressed** blob (never
/// sealed — it is third-party public content) and return its hex BLAKE3 hash, or
/// `None` if there's no blob store wired or the bytes aren't a usable image. A
/// `None` here degrades to a text-only preview rather than failing the resolve.
/// Whether a sniffed og:image MIME is stored as a link-preview blob, or the
/// preview degrades to text-only.
///
/// ⚠ The candidate set here is decided two crates away, by
/// `fauna_media::process`'s `Container::mime()` — when that sniff taxonomy
/// grows a new `image/*` type, this gate widens with it unless the new type is
/// refused here. That implicitness is exactly what silently widened this gate
/// on 2026-08-01, when the HEIC/AVIF brands stopped sniffing as `video/mp4`.
///
/// HEIC/HEIF are refused **deliberately**: web browsers do not render them, and
/// a preview card whose image paints on six apps and breaks on the seventh is a
/// per-app divergence — a refusal degrades to the text-only card everywhere
/// instead. AVIF passes: it renders on every app surface and is already on the
/// media proxy's `SAFE_MEDIA_TYPES`.
fn preview_mime_is_storable(mime: &str) -> bool {
    mime.starts_with("image/") && !matches!(mime, "image/heic" | "image/heif")
}

async fn store_preview_image(state: &Arc<AppState>, image: FetchedImage) -> Option<String> {
    // No blob store (e.g. `--blob-dir` absent) → text-only preview.
    let backup = state.backup_service.as_ref()?;

    // EXIF-strip + MIME-sniff; a refused sniff means the og:image url didn't
    // point at an image every app can render → drop it (text-only preview).
    let processed = fauna_media::process::process_media(&image.bytes);
    if !preview_mime_is_storable(&processed.mime) {
        return None;
    }

    let hash_bytes: [u8; 32] = *blake3::hash(&processed.stripped_bytes).as_bytes();
    let hash = fauna_core::data::ContentHash::from_digest_raw(hash_bytes);
    backup
        .local_blob_store()
        .put(&hash, &processed.stripped_bytes)
        .await
        .ok()?;
    state
        .db
        .put_blob_metadata(
            &hash_bytes,
            processed.stripped_bytes.len() as i64,
            &processed.mime,
            Some(processed.has_c2pa),
            None,
        )
        .await
        .ok()?;
    Some(hex::encode(hash_bytes))
}

/// A `test-hooks`-only fetcher that serves scripted fixtures for mapped urls and
/// delegates every other url to the real SSRF-guarded fetcher.
#[cfg(feature = "test-hooks")]
struct OverrideFetcher {
    overrides: std::collections::HashMap<String, (Bytes, String)>,
    live: Arc<dyn link_preview::fetch::LinkPreviewFetcher>,
}

#[cfg(feature = "test-hooks")]
#[async_trait::async_trait]
impl link_preview::fetch::LinkPreviewFetcher for OverrideFetcher {
    async fn fetch(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<link_preview::fetch::FetchedResource, link_preview::fetch::FetchError> {
        use link_preview::fetch::{FetchError, FetchedResource};
        if let Some((body, content_type)) = self.overrides.get(url) {
            // Honor the same size cap as the real fetcher so the e2e can drive
            // the oversized-→-Failed case through a fixture.
            if body.len() > max_bytes {
                return Err(FetchError::TooLarge);
            }
            let parsed = url::Url::parse(url)
                .map_err(|_| FetchError::Ssrf(crate::ssrf::SsrfError::InvalidUrl))?;
            return Ok(FetchedResource {
                url: parsed,
                content_type: content_type.clone(),
                body: body.clone(),
            });
        }
        self.live.fetch(url, max_bytes).await
    }
}

// ── Registration entry point ─────────────────────────────────────────────────

pub fn register_link_preview_handlers(b: &mut RpcRouterBuilder) {
    // Replay-safe @30s: the resolve is a pure read of third-party metadata
    // whose side effects (content-addressed image store + by-url cache) are
    // idempotent on a byte-identical re-fetch. The 30s deadline matches the
    // protocol-crate kind metadata (`register_linkpreview_kinds`, the bluesky
    // external-fetch precedent) and envelopes a multi-hop redirect fetch.
    b.add(
        KIND_LINKPREVIEW_RESOLVE,
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(30),
            handler: link_preview_resolve_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link_preview::cache::LinkPreviewCache;

    /// The gate decision for a raw og:image body, through the REAL sniff — so
    /// a `fauna_media` MIME-taxonomy change that would re-widen (or re-narrow)
    /// the stored set reds here, not at restore time.
    fn gate_accepts(body: &[u8]) -> bool {
        preview_mime_is_storable(&fauna_media::process::process_media(body).mime)
    }

    fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
        let mut body: Vec<u8> = b"\x00\x00\x00\x18ftyp".to_vec();
        body.extend_from_slice(brand);
        body
    }

    #[test]
    fn preview_image_gate_refuses_heic_accepts_avif() {
        // Positive control: a JPEG is stored.
        assert!(gate_accepts(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpeg is storable");
        // AVIF renders on every app surface and is on `SAFE_MEDIA_TYPES` — a
        // correctly-labelled AVIF og:image is stored (until 2026-08-01 it was
        // stored under `image/heic`, a type no browser renders).
        assert!(gate_accepts(&ftyp(b"avif")), "avif is storable");
        assert!(gate_accepts(&ftyp(b"avis")), "avif sequence is storable");
        // HEIC/HEIF: web cannot render them — refused, the preview degrades to
        // text-only on all 7 apps alike rather than breaking on one.
        assert!(!gate_accepts(&ftyp(b"heic")), "heic is refused");
        assert!(!gate_accepts(&ftyp(b"mif1")), "heif is refused");
        // A video og:image (plain mp4 brand) was never storable and still isn't.
        assert!(!gate_accepts(&ftyp(b"isom")), "video is refused");
        // Unsniffable bytes → octet-stream → refused.
        assert!(!gate_accepts(b"not an image"), "octet-stream is refused");
    }

    #[test]
    fn url_at_cap_is_accepted_over_cap_is_rejected() {
        let at_cap = "a".repeat(link_preview::MAX_URL_LEN);
        assert!(
            check_url_len(&at_cap).is_ok(),
            "exactly at the cap is allowed"
        );

        let over_cap = "a".repeat(link_preview::MAX_URL_LEN + 1);
        let err = check_url_len(&over_cap).expect_err("over the cap must reject");
        assert_eq!(err.code, "fauna.protocol.malformed");
    }

    #[test]
    fn rejected_url_is_not_cached() {
        // Mirror the handler's ordering: the url-length check runs *before* any
        // `cache.get`/`cache.insert`, so a rejected url never becomes a cache
        // key. Drive that order against a real cache and assert it stays empty.
        let cache = LinkPreviewCache::new(Duration::from_secs(3600));
        let over_cap = "https://example.com/".to_string() + &"a".repeat(link_preview::MAX_URL_LEN);

        let outcome = check_url_len(&over_cap).map(|()| {
            // Only reached on accept — the handler would cache here.
            cache.insert(
                over_cap.clone(),
                CachedPreview {
                    title: "t".into(),
                    description: "d".into(),
                    image_hash: None,
                },
            );
        });

        assert!(outcome.is_err(), "over-long url is rejected");
        assert!(cache.is_empty(), "rejected url left no cache entry");
        assert!(cache.get(&over_cap).is_none());
    }
}
