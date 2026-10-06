//! Media privacy proxy for Bluesky CDN content.
//!
//! A thin wrapper over the shared SSRF-safe + content-type-safe media core
//! ([`crate::media_proxy_routes::proxy_remote_media`]). This route's only
//! additional policy is the bsky CDN **host allowlist** ([`validate_media_url`]):
//! it may proxy only `cdn.bsky.app` / `video.bsky.app`, never an arbitrary URL.
//!
//! Unlike the shared `media/proxy` route it does **not** require a bearer —
//! today. Its original premise, that all 7 apps render these paths as a plain
//! `<img src>` / `<video>` subresource that cannot carry an `Authorization`
//! header, is RETIRED (render-model.md § D6c, ruled 2026-09-28): a bridged
//! post's picture is a `RenderBlock::ProxiedImage` every app fetches from its
//! own nest WITH the session bearer, exactly like `/api/v1/blob/<hash>` (web
//! through its authenticated `fetch` and an object URL). Requiring the bearer
//! here too — the CDN host allowlist stays regardless — is a follow-on once
//! every app's loader carries it. Until then the navigable-XSS vector a
//! credential would close is
//! closed by the shared core's content-type allowlist + `nosniff` +
//! `Content-Disposition: inline`, so attacker-influenced CDN bytes (e.g. an
//! uploaded `image/svg+xml`) can never execute as script on our origin even when
//! the URL is navigated to directly.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use std::sync::Arc;

use crate::routes::AppState;
use fauna_bridge_atproto::media::validate_media_url;

#[derive(Deserialize)]
pub struct MediaProxyQuery {
    url: String,
}

pub async fn proxy_media(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<MediaProxyQuery>,
) -> Response {
    // First line: only the bsky CDN host allowlist may be proxied here.
    if let Err(reason) = validate_media_url(&query.url) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }
    // Then the shared SSRF-safe + content-type-safe fetch (no redirects,
    // global-only, pinned IP, content-type allowlist, nosniff, inline). No
    // `Range`: this twin serves HLS playlists and thumbnails, never a player's
    // byte-range read (render-model.md § D6c → *Inline playback*, answer 2).
    crate::media_proxy_routes::proxy_remote_media(&query.url, None).await
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/bluesky/media", get(proxy_media))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use tower_service::Service;

    fn build_state() -> Arc<AppState> {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        Arc::new(AppState::for_test(db))
    }

    /// The host allowlist is this route's first line and must survive the
    /// dedup onto the shared core — any host outside the bsky CDN is rejected
    /// with 400 before anything is dialed (so the shared SSRF-safe fetch only
    /// ever sees an already-allowlisted bsky host).
    #[tokio::test]
    async fn rejects_non_allowlisted_host() {
        let mut router = routes().with_state(build_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/bluesky/media?url=https://evil.example/x.svg")
            .body(Body::empty())
            .unwrap();
        let resp = router.call(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a non-bsky-CDN host must be rejected before any fetch"
        );
    }

    /// A plain `http://` (non-https) bsky URL is also rejected by the allowlist
    /// (`validate_media_url` requires https), confirming the route never reaches
    /// the shared core with a downgraded scheme.
    #[tokio::test]
    async fn rejects_non_https_scheme() {
        let mut router = routes().with_state(build_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/bluesky/media?url=http://cdn.bsky.app/img/x.jpg")
            .body(Body::empty())
            .unwrap();
        let resp = router.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
