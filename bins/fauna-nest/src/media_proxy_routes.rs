//! Shared media privacy proxy for all bridge protocols.
//!
//! Streams remote media through the nest to prevent IP leaks.
//! Used by ActivityPub, Nostr, and (optionally) Bluesky bridges.
//! Route: `GET /api/v1/media/proxy?url=<encoded>`
//!
//! Because the URL is caller-supplied, this is a textbook SSRF sink: it must
//! never reach loopback / private / link-local / cloud-metadata addresses, must
//! not follow redirects into them, and must not be re-resolvable to an internal
//! address after the safety check (DNS rebinding). All of that lives in the
//! shared [`crate::ssrf`] guard; this handler just wires it. The route also
//! requires a credential (authenticated actors only) — a security-review
//! finding (tracked internally): a valid bearer, or a **playback ticket**
//! (`exp` + `sig` query parameters, [`crate::media_ticket`]) for the one remote
//! url it signs, because a `<video src>` cannot carry a bearer
//! (`docs/goal/architecture/render-model.md` § D6c → *Inline playback*,
//! answer 4).
//!
//! The upstream body is **streamed** through, never buffered: a player fetches
//! a 99 MB video in byte ranges, so the client's `Range` is forwarded upstream
//! and the upstream's `206` / `Content-Range` / `Content-Length` /
//! `Accept-Ranges` relayed (answer 2), under [`MAX_PROXY_SIZE`] (answer 3).

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::routes::AppState;

/// The most one proxied response may carry, for every media type: 100 MiB.
/// Mastodon's default video upload limit is 99 MB, so a smaller cap refused
/// what the commonest peer accepts. A resource bound on this nest's egress per
/// request, not a preference — a Rust constant, never a knob
/// (render-model.md § D6c → *Inline playback*, answer 3).
pub const MAX_PROXY_SIZE: u64 = 100 * 1024 * 1024;

/// How long the dial may take.
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long any one wait for upstream bytes may take — the head, then each
/// chunk. Not a whole-body deadline: a 100 MiB stream over a slow link must not
/// be cut at 30 s.
const PROXY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Content types we will serve back with their **declared** type intact.
/// Everything else is relabelled `application/octet-stream`. The point is that
/// some media-proxy routes are reachable without a credential and *navigable*
/// (the bluesky twin still takes no bearer — render-model.md § D6c retires the
/// `<img src>` premise that excused it, and requiring one is a follow-on), so if
/// the upstream served `text/html` or `image/svg+xml` and we reflected it, a victim who
/// merely navigated to the proxy URL would get attacker-influenced bytes
/// executed as script on the SPA origin (a security-review finding, tracked
/// internally).
///
/// `image/svg+xml` is deliberately ABSENT: SVG is the one image type that can
/// carry `<script>`, so it must never render inline on our origin. Only inert
/// raster / AV / HLS-playlist types pass through; with `nosniff` the browser
/// also cannot sniff a relabelled octet-stream back into html/svg.
const SAFE_MEDIA_TYPES: &[&str] = &[
    // Raster images — cannot execute script.
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/apng",
    "image/bmp",
    "image/x-icon",
    "image/heic",
    "image/heif",
    // Video.
    "video/mp4",
    "video/webm",
    "video/ogg",
    "video/quicktime",
    "video/mp2t",
    // HLS playlists (bluesky video) — text manifests fetched by the player, not navigated.
    "application/vnd.apple.mpegurl",
    "application/x-mpegurl",
    // Audio.
    "audio/mpeg",
    "audio/mp4",
    "audio/ogg",
    "audio/wav",
    "audio/webm",
    "audio/aac",
    "audio/flac",
];

/// Map an upstream (or stored) content-type to a safe value: the declared type
/// iff it is on the [`SAFE_MEDIA_TYPES`] allowlist (case-insensitive, parameters
/// stripped), else `application/octet-stream`. Never echoes an attacker-influenced
/// type a browser would execute (`text/html`, `image/svg+xml`,
/// `application/javascript`, …).
///
/// `pub(crate)` because the hex-hash blob download
/// (`blob_routes::download_blob`) reuses the same allowlist: that route is
/// unauthenticated, navigable, and reflects a user-influenced stored
/// content-type, so it shares this route's exact exposure and must
/// share the fix.
pub(crate) fn safe_content_type(raw: &str) -> &'static str {
    let base = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    SAFE_MEDIA_TYPES
        .iter()
        .copied()
        .find(|t| *t == base.as_str())
        .unwrap_or("application/octet-stream")
}

/// Shared SSRF-safe + content-type-safe media fetch behind **every** bridge
/// media-proxy route (the shared `media/proxy`, the bluesky `bluesky/media`
/// twin). The caller enforces its own policy first — `media/proxy` requires a
/// bearer; `bluesky/media` applies a CDN host allowlist — then both funnel the
/// dangerous part (dialing a caller-influenced remote URL and returning its
/// bytes on our origin) through this one place exactly once: SSRF-guarded
/// client (https-only, every resolved address global, redirects disabled,
/// resolved IP pinned against rebinding), [`MAX_PROXY_SIZE`] on the declared
/// total and on the streamed bytes, a fixed content-type allowlist, and
/// `nosniff` + `Content-Disposition: inline` on the way back out.
///
/// `range` is the client's `Range` header, forwarded upstream verbatim; the
/// upstream's `206` and its `Content-Range` come back as-is, and an upstream
/// that ignores the range answers a `200` the player copes with.
pub(crate) async fn proxy_remote_media(url_str: &str, range: Option<HeaderValue>) -> Response {
    let (client, url) = match dial(url_str).await {
        Ok(c) => c,
        Err(reason) => {
            tracing::warn!(url = %url_str, %reason, "media proxy: URL rejected");
            return (StatusCode::BAD_REQUEST, reason.to_string()).into_response();
        }
    };

    let mut request = client.get(url.clone());
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    let upstream = match request.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "media proxy: fetch failed");
            return (StatusCode::BAD_GATEWAY, "upstream fetch failed").into_response();
        }
    };

    if !upstream.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            format!("upstream returned {}", upstream.status()),
        )
            .into_response();
    }
    let partial = upstream.status() == StatusCode::PARTIAL_CONTENT;
    let upstream_header = |name| upstream.headers().get(name).cloned();
    let content_range = upstream_header(header::CONTENT_RANGE).filter(|_| partial);
    let accept_ranges = upstream_header(header::ACCEPT_RANGES);

    // The cap on the DECLARED total, before a body byte is read: a 206's total
    // is the resource's, after the `/` of its `Content-Range`; a 200's is its
    // `Content-Length` (which `CappedBody::new` refuses on too). An undeclared
    // total leaves the streamed cap below as the guard.
    let declared_total = match &content_range {
        Some(range) => range_total(range),
        None => upstream.content_length(),
    };
    if declared_total.is_some_and(|total| total > MAX_PROXY_SIZE) {
        return too_large();
    }

    let content_type = safe_content_type(
        upstream
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );
    let content_length = upstream.content_length();

    // The cap holds against an absent or lying `Content-Length` too: the
    // stream ends in an error at the first chunk past it, so the client sees a
    // cut transfer, never a whole body over the cap.
    let Ok(body) = crate::ssrf::CappedBody::new(upstream, MAX_PROXY_SIZE) else {
        return too_large();
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // Never let the browser sniff a relabelled octet-stream back into html/svg.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Render inline (so `<img src>` keeps working) — the allowlist, not a forced
    // download, is what keeps a navigated URL from executing.
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    if let Some(len) = content_length {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    if let Some(range) = content_range {
        headers.insert(header::CONTENT_RANGE, range);
    }
    if let Some(accept) = accept_ranges {
        headers.insert(header::ACCEPT_RANGES, accept);
    }
    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    (status, headers, Body::from_stream(stream_capped(body))).into_response()
}

fn too_large() -> Response {
    (StatusCode::BAD_GATEWAY, "upstream content too large").into_response()
}

/// The complete length after the `/` of a `Content-Range: bytes a-b/total`;
/// `None` for an unknown (`*`) or unreadable total.
fn range_total(content_range: &HeaderValue) -> Option<u64> {
    let (_, total) = content_range.to_str().ok()?.rsplit_once('/')?;
    total.trim().parse().ok()
}

/// `body`'s chunks as a stream, ending in an error the moment the cap is
/// crossed or the upstream fails — one transport chunk in memory at a time.
fn stream_capped(
    body: crate::ssrf::CappedBody,
) -> impl futures_util::Stream<Item = std::io::Result<bytes::Bytes>> {
    futures_util::stream::unfold(Some(body), |body| async move {
        let mut body = body?;
        match body.next_chunk().await {
            Ok(Some(chunk)) => Some((Ok(chunk), Some(body))),
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e)), None)),
        }
    })
}

/// The SSRF-guarded streaming client for `url_str`. No `User-Agent`: a proxied
/// media fetch stays unattributed, so an origin cannot tell which nest's user
/// is viewing. (ActivityPub is the deliberate exception — it must identify
/// itself; see `ssrf_safe_https_client`.)
async fn dial(url_str: &str) -> Result<(reqwest::Client, url::Url), crate::ssrf::SsrfError> {
    #[cfg(test)]
    if let Some(dialed) = tests::loopback_dial(url_str) {
        return Ok(dialed);
    }
    crate::ssrf::ssrf_safe_https_streaming_client(
        url_str,
        PROXY_CONNECT_TIMEOUT,
        PROXY_READ_TIMEOUT,
        None,
    )
    .await
}

async fn proxy_media(
    // Require a credential: only registered actors may drive an outbound fetch
    // through the nest. (The SSRF guard is the real defense; this keeps the
    // sink off the unauthenticated surface entirely.)
    _credential: ProxyCredential,
    Query(query): Query<ProxyQuery>,
    request_headers: HeaderMap,
) -> Response {
    proxy_remote_media(&query.url, request_headers.get(header::RANGE).cloned()).await
}

#[derive(Deserialize)]
struct ProxyQuery {
    url: String,
    /// The playback ticket's expiry (unix seconds), when the request carries one.
    exp: Option<i64>,
    /// The playback ticket's base64url signature, beside `exp`.
    sig: Option<String>,
}

/// The proxy's credential — either one admits the request:
///
/// - a valid **bearer**, as before (images; tui's fetches), or
/// - a live **playback ticket** for exactly the requested `url`
///   ([`crate::media_ticket::verify`]) — what a `<video src>` can carry.
///
/// A forged, expired or absent credential is the same `401` a missing bearer
/// gets, so the unauthenticated surface is exactly as wide as before.
struct ProxyCredential;

impl FromRequestParts<Arc<AppState>> for ProxyCredential {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if crate::auth::BearerAuth::from_request_parts(parts, state)
            .await
            .is_ok()
        {
            return Ok(Self);
        }
        let Ok(Query(ProxyQuery {
            url,
            exp: Some(exp),
            sig: Some(sig),
        })) = Query::<ProxyQuery>::from_request_parts(parts, state).await
        else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Some(secret) = crate::media_ticket::load_secret(state).await else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let now = fauna_core::data::Timestamp::now_secs_or_zero();
        if crate::media_ticket::verify(&secret, &url, exp, &sig, now) {
            Ok(Self)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/media/proxy", get(proxy_media))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use fauna_core::identity::ActorId;
    use tower_service::Service;

    fn build_state() -> Arc<AppState> {
        crate::test_support::fixture_state()
    }

    // ── A scripted upstream behind the real handler ─────────────────────────
    //
    // The SSRF guard dials only https on global addresses, so a test upstream
    // on loopback is reached through one test-only seam: inside a
    // [`with_upstream`] scope, `https://upstream.test/<path>` dials the scripted
    // server's `http://127.0.0.1:<port>/<path>` with the same timeouts and no
    // redirects. Every other url — and every url outside the scope — takes the
    // real guarded dial.

    const UPSTREAM_HOST: &str = "https://upstream.test";

    tokio::task_local! {
        static UPSTREAM: String;
    }

    pub(super) fn loopback_dial(url_str: &str) -> Option<(reqwest::Client, url::Url)> {
        let base = UPSTREAM.try_with(Clone::clone).ok()?;
        let rest = url_str.strip_prefix(UPSTREAM_HOST)?;
        let url = url::Url::parse(&format!("{base}{rest}")).ok()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(PROXY_CONNECT_TIMEOUT)
            .read_timeout(PROXY_READ_TIMEOUT)
            .build()
            .ok()?;
        Some((client, url))
    }

    /// One scripted response: the head, then `chunks` — each chunk at index
    /// `gate` or later is written only after a [`Upstream::release`]. The
    /// request head it was sent arrives on `request`.
    struct Upstream {
        base: String,
        request: tokio::sync::oneshot::Receiver<String>,
        release: tokio::sync::mpsc::UnboundedSender<()>,
        /// How many chunks have been written so far.
        written: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Upstream {
        async fn serve(head: String, chunks: Vec<Vec<u8>>, gate: usize) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let (request_tx, request) = tokio::sync::oneshot::channel();
            let (release, mut released) = tokio::sync::mpsc::unbounded_channel::<()>();
            let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counter = written.clone();
            // spawn-ok(test): a scripted in-test HTTP upstream.
            tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut byte = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut byte).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&byte[..n]);
                }
                let _ = request_tx.send(String::from_utf8_lossy(&buf).into_owned());
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for (i, chunk) in chunks.iter().enumerate() {
                    if i >= gate && released.recv().await.is_none() {
                        return;
                    }
                    if sock.write_all(chunk).await.is_err() {
                        return;
                    }
                    let _ = sock.flush().await;
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
            Self {
                base,
                request,
                release,
                written,
            }
        }

        fn release(&self) {
            self.release.send(()).unwrap();
        }

        fn written(&self) -> usize {
            self.written.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn head(status: &str, extra: &str) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\n{extra}Connection: close\r\n\r\n")
    }

    /// `GET path` through the real router inside the upstream's scope.
    async fn get_via(
        state: &Arc<AppState>,
        upstream: &Upstream,
        path: &str,
        headers: &[(&str, String)],
    ) -> Response {
        let mut router = routes().with_state(state.clone());
        let mut req = Request::builder().method(Method::GET).uri(path);
        for (name, value) in headers {
            req = req.header(*name, value);
        }
        let req = req.body(Body::empty()).unwrap();
        UPSTREAM
            .scope(upstream.base.clone(), async move {
                router.call(req).await.unwrap()
            })
            .await
    }

    async fn body_of(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("the body streams to its end")
            .to_vec()
    }

    fn proxy_path(url: &str) -> String {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", url)
            .finish();
        format!("/api/v1/media/proxy?{q}")
    }

    fn video_url() -> String {
        format!("{UPSTREAM_HOST}/v.mp4")
    }

    async fn bearer(state: &Arc<AppState>) -> (&'static str, String) {
        let token = state
            .auth
            .token_store
            .insert(ActorId([0x21u8; 32]), 3600)
            .await;
        ("Authorization", format!("Bearer {token}"))
    }

    /// A playback ticket minted over WS-RPC fetches the proxied bytes with no
    /// bearer at all — what a `<video src>` does.
    #[tokio::test]
    async fn a_minted_ticket_fetches_without_a_bearer() {
        let state = crate::media_ticket::tests::booted_state().await;
        let upstream = Upstream::serve(head("200 OK", ""), vec![b"video bytes".to_vec()], 1).await;
        let ticketed = crate::media_ticket::tests::mint(&state, &proxy_path(&video_url()))
            .await
            .expect("minted");
        let resp = get_via(&state, &upstream, &ticketed, &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_of(resp).await, b"video bytes");
    }

    /// A ticket that has expired is the same 401 a missing bearer gets.
    #[tokio::test]
    async fn an_expired_ticket_is_refused() {
        let state = crate::media_ticket::tests::booted_state().await;
        let upstream = Upstream::serve(head("200 OK", ""), vec![b"x".to_vec()], 1).await;
        let secret = crate::media_ticket::load_secret(&state).await.unwrap();
        let exp = fauna_core::data::Timestamp::now_secs_or_zero() - 1;
        let sig = crate::media_ticket::sign(&secret, &video_url(), exp);
        let path = format!("{}&exp={exp}&sig={sig}", proxy_path(&video_url()));
        let resp = get_via(&state, &upstream, &path, &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// A ticket signed over another url does not open this one.
    #[tokio::test]
    async fn a_ticket_for_another_url_is_refused() {
        let state = crate::media_ticket::tests::booted_state().await;
        let upstream = Upstream::serve(head("200 OK", ""), vec![b"x".to_vec()], 1).await;
        let other = crate::media_ticket::tests::mint(
            &state,
            &proxy_path(&format!("{UPSTREAM_HOST}/other.mp4")),
        )
        .await
        .expect("minted");
        let (_, query) = other.split_once('?').unwrap();
        let ticket = query.split_once('&').unwrap().1; // `exp=…&sig=…`
        let path = format!("{}&{ticket}", proxy_path(&video_url()));
        let resp = get_via(&state, &upstream, &path, &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// The bearer is unchanged: images and tui's fetches carry no ticket.
    #[tokio::test]
    async fn a_bearer_without_a_ticket_still_fetches() {
        let state = crate::media_ticket::tests::booted_state().await;
        let upstream = Upstream::serve(head("200 OK", ""), vec![b"image".to_vec()], 1).await;
        let auth = bearer(&state).await;
        let resp = get_via(&state, &upstream, &proxy_path(&video_url()), &[auth]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_of(resp).await, b"image");
    }

    /// The client's `Range` reaches the upstream verbatim and the upstream's
    /// `206` comes back with its `Content-Range`, `Content-Length` and
    /// `Accept-Ranges` — the read a player seeks with.
    #[tokio::test]
    async fn a_range_request_is_relayed_as_206() {
        let state = crate::media_ticket::tests::booted_state().await;
        let mut upstream = Upstream::serve(
            head(
                "206 Partial Content",
                "Content-Range: bytes 0-9/100\r\nContent-Length: 10\r\nAccept-Ranges: bytes\r\n",
            ),
            vec![b"0123456789".to_vec()],
            1,
        )
        .await;
        let auth = bearer(&state).await;
        let resp = get_via(
            &state,
            &upstream,
            &proxy_path(&video_url()),
            &[auth, ("Range", "bytes=0-9".into())],
        )
        .await;
        let sent = (&mut upstream.request).await.unwrap().to_ascii_lowercase();
        assert!(
            sent.contains("\r\nrange: bytes=0-9\r\n"),
            "the Range header reached the upstream verbatim: {sent}"
        );
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_RANGE], "bytes 0-9/100");
        assert_eq!(h[header::CONTENT_LENGTH], "10");
        assert_eq!(h[header::ACCEPT_RANGES], "bytes");
        assert_eq!(body_of(resp).await, b"0123456789");
    }

    /// A declared length one byte over the cap is refused before any body is
    /// read: the upstream's body is held back for the whole exchange, and the
    /// 502 arrives anyway.
    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_refused_before_the_body() {
        let state = crate::media_ticket::tests::booted_state().await;
        let upstream = Upstream::serve(
            head(
                "200 OK",
                &format!("Content-Length: {}\r\n", MAX_PROXY_SIZE + 1),
            ),
            vec![vec![b'x'; 16]],
            0,
        )
        .await;
        let auth = bearer(&state).await;
        let resp = get_via(&state, &upstream, &proxy_path(&video_url()), &[auth]).await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(body_of(resp).await, b"upstream content too large");
        assert_eq!(upstream.written(), 0, "no body byte was ever needed");
    }

    /// A 206 whose `Content-Range` total exceeds the cap is refused too — the
    /// range itself is small, the resource it belongs to is not.
    #[tokio::test]
    async fn a_range_total_over_the_cap_is_refused() {
        let state = crate::media_ticket::tests::booted_state().await;
        let upstream = Upstream::serve(
            head(
                "206 Partial Content",
                &format!(
                    "Content-Range: bytes 0-9/{}\r\nContent-Length: 10\r\n",
                    MAX_PROXY_SIZE + 1
                ),
            ),
            vec![b"0123456789".to_vec()],
            0,
        )
        .await;
        let auth = bearer(&state).await;
        let resp = get_via(
            &state,
            &upstream,
            &proxy_path(&video_url()),
            &[auth, ("Range", "bytes=0-9".into())],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    /// Streaming, not buffering: the response head is in hand while the
    /// upstream still holds its third MiB, and the whole body arrives once it
    /// is let go.
    #[tokio::test]
    async fn the_body_streams_rather_than_buffers() {
        let state = crate::media_ticket::tests::booted_state().await;
        let mib = 1024 * 1024;
        let upstream = Upstream::serve(
            head("200 OK", ""),
            vec![vec![b'a'; mib], vec![b'b'; mib], vec![b'c'; mib]],
            2,
        )
        .await;
        let auth = bearer(&state).await;
        let resp = tokio::time::timeout(
            Duration::from_secs(10),
            get_via(&state, &upstream, &proxy_path(&video_url()), &[auth]),
        )
        .await
        .expect("the head arrived while the upstream held its last chunk");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(upstream.written() < 3, "the third chunk had not been sent");
        upstream.release();
        let body = body_of(resp).await;
        assert_eq!(body.len(), 3 * mib);
        assert!(body[2 * mib..].iter().all(|b| *b == b'c'));
    }

    /// Inert raster / AV / HLS types pass through with their declared
    /// type (case-insensitive, parameters stripped) so real media still renders.
    #[test]
    fn safe_content_type_allows_inert_media() {
        assert_eq!(safe_content_type("image/jpeg"), "image/jpeg");
        assert_eq!(safe_content_type("image/png; charset=binary"), "image/png");
        assert_eq!(safe_content_type("VIDEO/MP4"), "video/mp4");
        assert_eq!(
            safe_content_type("application/vnd.apple.mpegurl"),
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(safe_content_type("  audio/mpeg  "), "audio/mpeg");
    }

    /// The load-bearing assertion: every script-capable or unknown
    /// type is relabelled `application/octet-stream`, so a victim who *navigates*
    /// to the proxy URL can never get attacker-influenced bytes executed as
    /// script on our origin. `image/svg+xml` is the critical one (SVG carries
    /// `<script>`); `text/html` / `application/javascript` are the obvious ones.
    #[test]
    fn safe_content_type_relabels_script_capable() {
        for raw in [
            "image/svg+xml",
            "image/svg+xml; charset=utf-8",
            "text/html",
            "text/html; charset=utf-8",
            "application/xhtml+xml",
            "application/javascript",
            "text/javascript",
            "application/xml",
            "",
            "not-a-type",
        ] {
            assert_eq!(
                safe_content_type(raw),
                "application/octet-stream",
                "{raw} must be relabelled, never reflected"
            );
        }
    }

    /// F2: the media proxy must reject unauthenticated callers — it was
    /// registered with no auth extractor, making the SSRF sink anonymous.
    #[tokio::test]
    async fn proxy_requires_auth() {
        let state = build_state();
        let mut router = routes().with_state(state);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/media/proxy?url=https://example.com/x.png")
            .body(Body::empty())
            .unwrap();
        let resp = router.call(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unauthenticated media-proxy request must be rejected"
        );
    }

    /// F2: an authenticated caller still cannot reach cloud IMDS / internal IPs;
    /// the SSRF guard rejects the URL with 400 before any fetch.
    #[tokio::test]
    async fn proxy_rejects_imds_for_authed_caller() {
        let state = build_state();
        // A registered actor: the bearer door asks the actor's standing, and an
        // actor with no `users` row has none.
        state
            .db
            .create_user(&[0x11u8; 32], "free", "proxy-user")
            .await
            .unwrap();
        let token = state
            .auth
            .token_store
            .insert(ActorId([0x11u8; 32]), 3600)
            .await;
        let mut router = routes().with_state(state);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/media/proxy?url=https://169.254.169.254/latest/meta-data/")
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = router.call(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "authed caller must not reach link-local IMDS via the proxy"
        );
    }
}
