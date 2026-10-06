//! Nest-side link-preview resolver (`render-model.md` § D4).
//!
//! The producer of a `RenderBlock::LinkPreview` is **nest-side**: a client-side
//! fetch would leak every user's IP to every linked site and is CORS-blocked on
//! web (a priority-#1 per-app divergence). So the nest fetches a posted URL's
//! OpenGraph/`<meta>`, optionally fetches the `og:image`, and returns metadata a
//! client resolves through its existing media path — all behind SSRF guards.
//!
//! Layering (each piece independently testable):
//! - [`fetch`]  — the SSRF-safe, redirect-following, capped outbound fetch seam.
//! - [`parse`]  — pure HTML → [`parse::ParsedMeta`] (OpenGraph + fallbacks).
//! - [`resolve_preview_meta`] — orchestrates fetch + parse + image-fetch into a
//!   [`MetaResolution`]; storage-free and cache-free so it unit-tests with a fake
//!   fetcher (the handler stores the image and caches the result).
//! - [`cache`]  — the URL → resolved-preview TTL cache.

pub mod cache;
pub mod fetch;
pub mod parse;

use std::sync::Arc;
use std::time::Duration;

use fetch::{FetchError, LinkPreviewFetcher};

/// Production URL-cache TTL — a fetched preview is reused across viewers for an
/// hour (`render-model.md` § D4: "caches by URL"), bounding repeat outbound
/// fetches for a popular link.
pub const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// Per-actor rate-limit window for `fauna.linkpreview.resolve`.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Per-actor max resolves per [`RATE_WINDOW`]. Generous enough for a link-heavy
/// feed scroll (each *novel* url costs one; cache hits are free), tight enough
/// that an authed actor can't use the resolver as an outbound request amplifier
/// / scanner against arbitrary third parties.
pub const RATE_MAX_EVENTS: u32 = 60;

/// Hard cap on a `fauna.linkpreview.resolve` request `url` length, in bytes
/// (LP-2). The wire field is otherwise bounded only by the global WS message cap
/// (F-CL1), and the url becomes the by-url cache key — so an over-long url is an
/// over-long key. 8 KiB is far past any real url (browsers/servers commonly cap
/// at ~2–8 KiB); the handler rejects anything longer as `malformed` *before* the
/// cache lookup or any outbound fetch.
pub const MAX_URL_LEN: usize = 8 * 1024;

/// The link-preview resolver's shared state on [`crate::routes::AppState`]: the
/// SSRF-safe outbound fetcher, the by-url TTL cache, and a per-actor rate
/// limiter. Construction is the same for production and `for_test` — the fetcher
/// is the real [`fetch::SsrfSafeFetcher`] in both (the `test-hooks` fixture
/// override rides on a separate `AppState::link_preview_override` map, mirroring
/// `mta_sts_override`, so production SSRF is never loosened).
pub struct LinkPreviewState {
    pub fetcher: Arc<dyn LinkPreviewFetcher>,
    pub cache: Arc<cache::LinkPreviewCache>,
    /// Per-actor sliding-window limiter. Reuses the bridge blob-fetch
    /// [`crate::bridge_rate_limit::Limiter`] shape (the resolve surface keys on
    /// the caller actor alone — see the handler's `check` call).
    pub rate_limiter: Arc<crate::bridge_rate_limit::Limiter>,
}

impl LinkPreviewState {
    pub fn new() -> Self {
        Self {
            fetcher: Arc::new(fetch::SsrfSafeFetcher::default()),
            cache: Arc::new(cache::LinkPreviewCache::new(CACHE_TTL)),
            rate_limiter: Arc::new(crate::bridge_rate_limit::Limiter::with_config(
                crate::bridge_rate_limit::LimiterConfig {
                    window: RATE_WINDOW,
                    max_events: RATE_MAX_EVENTS,
                },
            )),
        }
    }
}

impl Default for LinkPreviewState {
    fn default() -> Self {
        Self::new()
    }
}

/// HTML fetch cap — a `<head>` of OpenGraph tags is small; 512 KiB is generous
/// and bounds the parse + memory cost of a hostile page.
pub const MAX_HTML_BYTES: usize = 512 * 1024;
/// og:image fetch cap.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// An og:image the resolver fetched but has not yet stored. The handler runs it
/// through `fauna_media::process::process_media` + the blob store to mint
/// `image_hash`; the bytes are public third-party content (never sealed).
pub struct FetchedImage {
    pub bytes: bytes::Bytes,
    /// The fetch's declared `Content-Type` (advisory — the blob store re-sniffs).
    pub declared_content_type: String,
}

/// Outcome of resolving a URL's preview metadata, *before* the og:image is stored.
pub enum MetaResolution {
    /// At least one of title / description / image is present.
    Resolved {
        title: String,
        description: String,
        image: Option<FetchedImage>,
    },
    /// Any fetch/parse failure, or a page with no usable metadata at all — a
    /// generic, non-retried failure (`render-model.md` § D4: client falls back to
    /// the plain inline link).
    Failed,
}

/// Fetch a URL's HTML, parse OpenGraph/`<meta>`, and (if present) fetch the
/// og:image bytes. Errors and empty pages collapse to [`MetaResolution::Failed`].
/// Storage- and cache-free: the caller stores the image and caches the result.
pub async fn resolve_preview_meta(fetcher: &dyn LinkPreviewFetcher, url: &str) -> MetaResolution {
    match resolve_inner(fetcher, url).await {
        Ok(resolution) => resolution,
        Err(_) => MetaResolution::Failed,
    }
}

async fn resolve_inner(
    fetcher: &dyn LinkPreviewFetcher,
    url: &str,
) -> Result<MetaResolution, FetchError> {
    let page = fetcher.fetch(url, MAX_HTML_BYTES).await?;

    // `parse_meta` owns the `!Send` `Html` and returns owned data, so nothing
    // unsendable is held across the image-fetch `.await` below.
    let meta = parse::parse_meta(&String::from_utf8_lossy(page.body.as_ref()), &page.url);

    if meta.title.is_empty() && meta.description.is_empty() && meta.image_url.is_none() {
        // Nothing worth a card — render `Failed` (plain link fallback).
        return Ok(MetaResolution::Failed);
    }

    let image = match meta.image_url.as_deref() {
        Some(image_url) => match fetcher.fetch(image_url, MAX_IMAGE_BYTES).await {
            Ok(res) => Some(FetchedImage {
                bytes: res.body,
                declared_content_type: res.content_type,
            }),
            // A failed image fetch is non-fatal — keep the text preview.
            Err(_) => None,
        },
        None => None,
    };

    Ok(MetaResolution::Resolved {
        title: meta.title,
        description: meta.description,
        image,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;

    /// Maps URL → (body, content_type). Unmapped URLs simulate a fetch failure.
    struct FakeFetcher {
        pages: HashMap<String, (Vec<u8>, String)>,
    }

    impl FakeFetcher {
        fn new() -> Self {
            Self {
                pages: HashMap::new(),
            }
        }
        fn with(mut self, url: &str, body: &str, ct: &str) -> Self {
            self.pages
                .insert(url.to_string(), (body.as_bytes().to_vec(), ct.to_string()));
            self
        }
        fn with_bytes(mut self, url: &str, body: &[u8], ct: &str) -> Self {
            self.pages
                .insert(url.to_string(), (body.to_vec(), ct.to_string()));
            self
        }
    }

    #[async_trait]
    impl LinkPreviewFetcher for FakeFetcher {
        async fn fetch(
            &self,
            url: &str,
            _max_bytes: usize,
        ) -> Result<fetch::FetchedResource, FetchError> {
            match self.pages.get(url) {
                Some((body, ct)) => Ok(fetch::FetchedResource {
                    url: url::Url::parse(url).unwrap(),
                    content_type: ct.clone(),
                    body: bytes::Bytes::from(body.clone()),
                }),
                None => Err(FetchError::Network),
            }
        }
    }

    fn assert_resolved(r: MetaResolution) -> (String, String, Option<FetchedImage>) {
        match r {
            MetaResolution::Resolved {
                title,
                description,
                image,
            } => (title, description, image),
            MetaResolution::Failed => panic!("expected Resolved, got Failed"),
        }
    }

    #[tokio::test]
    async fn resolves_og_page_and_fetches_image() {
        let html = r#"<head>
            <meta property="og:title" content="Hello">
            <meta property="og:description" content="World">
            <meta property="og:image" content="https://cdn.example.com/c.png">
        </head>"#;
        let f = FakeFetcher::new()
            .with("https://example.com/p", html, "text/html")
            .with_bytes(
                "https://cdn.example.com/c.png",
                &[0x89, b'P', b'N', b'G'],
                "image/png",
            );
        let (title, desc, image) =
            assert_resolved(resolve_preview_meta(&f, "https://example.com/p").await);
        assert_eq!(title, "Hello");
        assert_eq!(desc, "World");
        let image = image.expect("image fetched");
        assert_eq!(image.bytes.as_ref(), &[0x89, b'P', b'N', b'G']);
        assert_eq!(image.declared_content_type, "image/png");
    }

    #[tokio::test]
    async fn title_only_page_is_resolved_without_image() {
        let f = FakeFetcher::new().with(
            "https://example.com/t",
            "<head><title>Just A Title</title></head>",
            "text/html",
        );
        let (title, desc, image) =
            assert_resolved(resolve_preview_meta(&f, "https://example.com/t").await);
        assert_eq!(title, "Just A Title");
        assert_eq!(desc, "");
        assert!(image.is_none());
    }

    #[tokio::test]
    async fn page_without_metadata_is_failed() {
        let f = FakeFetcher::new().with(
            "https://example.com/empty",
            "<html><body><p>nothing here</p></body></html>",
            "text/html",
        );
        assert!(matches!(
            resolve_preview_meta(&f, "https://example.com/empty").await,
            MetaResolution::Failed
        ));
    }

    #[tokio::test]
    async fn fetch_failure_is_failed() {
        let f = FakeFetcher::new();
        assert!(matches!(
            resolve_preview_meta(&f, "https://unreachable.example/").await,
            MetaResolution::Failed
        ));
    }

    #[tokio::test]
    async fn image_fetch_failure_keeps_text_preview() {
        // og:image points at an URL the fetcher can't serve → image dropped,
        // text preview survives.
        let html = r#"<head>
            <meta property="og:title" content="Has Text">
            <meta property="og:image" content="https://cdn.example.com/missing.png">
        </head>"#;
        let f = FakeFetcher::new().with("https://example.com/p", html, "text/html");
        let (title, _desc, image) =
            assert_resolved(resolve_preview_meta(&f, "https://example.com/p").await);
        assert_eq!(title, "Has Text");
        assert!(image.is_none());
    }
}
