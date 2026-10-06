//! The **revealed** remote-image byte source — the one place this client fetches
//! a url it did not choose.
//!
//! A `RenderBlock::RemoteImage` is a third-party `![alt](https://…)` in someone
//! else's body. Loading it tells that third party the reader opened the post, so
//! the fetch is gated on `revealed` — the manager-owned, per-post, in-memory
//! opt-in of `render-model.md` § D3, flipped only by the user's own
//! `load-remote-content-button`. **Nothing here consults that flag**: the gate
//! lives at the call sites that build the url list, so this module can only ever
//! be handed urls the user already consented to (`crate::document`).
//!
//! Deliberately **not** the same path as the other two image sinks, which both
//! address this user's *own* nest by content hash over the authenticated bulk
//! plane: `post-image` (`feed::Op::FetchImage`) and the link-preview og:image
//! (an own-nest blob the nest fetched — render-model.md § D4). Those are
//! `GET /api/v1/blob/<hash>`; this is an arbitrary host. Same *cache* shape
//! ([`ImageCache`]) and the same rasterizer, different transport.
//!
//! The request itself is **not** here: it is `fauna_client::remote_image`, shared
//! with the Linux app, which makes the identical fetch and differs only in what it
//! does with the bytes. This module is the tui-side glue around it — which urls are
//! due (the cache), and turning bytes into half-block art.

use crate::image_cache::ImageCache;
use crate::thumbnail::Thumbnail;

/// Which of `urls` still need fetching, marking each in flight.
///
/// The cache is the idempotence, not the call site: a url already `Loading`,
/// `Ready` or `Failed` is filtered out, so a burst of observer ticks costs one
/// fetch — and [`begin`](fauna_core::load_cache::LoadCache::begin) marking as
/// it answers also dedupes *within* one tick, which matters here because the
/// same image url can appear in several posts.
pub fn kick(cache: &mut ImageCache, urls: Vec<String>) -> Option<Vec<String>> {
    let mut urls = urls;
    urls.retain(|url| cache.begin(url));
    (!urls.is_empty()).then_some(urls)
}

/// Fetch and rasterize every url concurrently, keyed back by url.
///
/// A refusal of any kind — unreachable host, non-2xx, oversized body,
/// undecodable bytes — degrades that entry to `None`, which paints the
/// placeholder. It is never a page banner: one broken image in someone else's
/// post is not an error the reader must dismiss (the shared per-item degrade
/// that Media's thumbnails and `post-image` already follow).
pub async fn fetch_all(urls: Vec<String>, cols: u32) -> Vec<(String, Option<Thumbnail>)> {
    // One client for the whole batch — a feed refresh can reveal several images at
    // once, and a client per image would pay for a fresh connection pool each time.
    let Some(client) = fauna_client::remote_image::client() else {
        // No TLS backend, no fetches — every url degrades to its placeholder.
        return urls.into_iter().map(|url| (url, None)).collect();
    };
    futures_util::future::join_all(urls.into_iter().map(|url| {
        let client = client.clone();
        async move {
            let art = fauna_client::remote_image::fetch_with(&client, &url)
                .await
                .and_then(|bytes| crate::thumbnail::rasterize(&bytes, cols));
            (url, art)
        }
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kick_requests_each_uncached_url_once_and_dedupes_within_a_tick() {
        let mut cache = ImageCache::new();
        let a = "https://a.example/1.png".to_string();
        let b = "https://b.example/2.png".to_string();

        // The same url twice in one pass (two posts embedding one image) is one
        // request — the in-flight mark lands before the duplicate is examined.
        assert_eq!(
            kick(&mut cache, vec![a.clone(), b.clone(), a.clone()]),
            Some(vec![a.clone(), b.clone()])
        );
        // A repeat tick has nothing to do while they are in flight…
        assert_eq!(kick(&mut cache, vec![a.clone(), b.clone()]), None);
        // …nor once they settle, as art or as a failure.
        cache.set(a.clone(), None);
        assert_eq!(kick(&mut cache, vec![a, b]), None);
    }

    /// The degrade is `fauna_client::remote_image`'s own test; this pins that tui
    /// keeps it end-to-end — an unfetchable url must reach the cache as `None` (a
    /// placeholder), never as a panic or a missing entry.
    #[tokio::test]
    async fn an_unreachable_host_degrades_to_a_placeholder_rather_than_an_error() {
        // `.invalid` is reserved and never resolvable (RFC 6761 § 6.4), so this
        // asserts the degrade without touching the network.
        let out = fetch_all(
            vec!["https://invalid.invalid/nope.png".to_string()],
            crate::thumbnail::POST_IMAGE_COLS,
        )
        .await;
        assert_eq!(out.len(), 1);
        assert!(out[0].1.is_none(), "a failed fetch is None, never a panic");
    }
}
