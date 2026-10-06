//! Host-header dispatch to the door's four public vhosts, static serving
//! with the hardened header set, the burrow 404, and the SPA fallback.
//! Topology + header policy: front-door.md § Public-vhost topology /
//! § Security architecture.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use tower_http::services::ServeFile;

use crate::{DoorState, PeerIp, proxy};

/// CSP for the zero-JS static site: Astro inlines `<style>` blocks, hence
/// `'unsafe-inline'` for styles only; everything else is locked down.
pub const SITE_CSP: &str = "default-src 'none'; img-src 'self' data:; \
     style-src 'self' 'unsafe-inline'; font-src 'self'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

/// CSP for the SPA: wasm needs `'wasm-unsafe-eval'`; `connect-src https: wss:`
/// is deliberate — the app connects to whichever nest the user enrolls with,
/// which is by definition not a fixed origin.
pub const APP_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
     connect-src 'self' https: wss:; img-src 'self' data: blob:; \
     media-src 'self' blob:; style-src 'self' 'unsafe-inline'; \
     worker-src 'self' blob:; base-uri 'none'; frame-ancestors 'none'";

/// Build the door's HTTPS router: every request funnels through the
/// Host-header dispatch.
pub fn door_router(state: Arc<DoorState>) -> Router {
    Router::new().fallback(dispatch).with_state(state)
}

/// Port-stripped `Host` of the request (empty when absent/invalid).
fn request_host(req: &Request) -> String {
    req.headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| fauna_core::web::split_host_port(h).0.to_ascii_lowercase())
        .unwrap_or_default()
}

async fn dispatch(State(state): State<Arc<DoorState>>, req: Request) -> Response {
    let host = request_host(&req);
    let cfg = &state.cfg;
    if host == cfg.apex {
        let path = req.uri().path().to_owned();
        let mut resp = serve_static(&state.cfg.site_root, req, NotFound::BurrowPage).await;
        finish_static(&mut resp, &path, SITE_CSP);
        resp
    } else if host == cfg.www {
        // Permanent (308) redirect to the same path+query on the apex.
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str().to_owned())
            .unwrap_or_else(|| "/".to_owned());
        let mut resp =
            Redirect::permanent(&format!("https://{}{}", cfg.apex, path_and_query)).into_response();
        hsts(&mut resp);
        resp
    } else if host == cfg.app {
        let path = req.uri().path().to_owned();
        let mut resp = serve_static(&state.cfg.app_root, req, NotFound::SpaFallback).await;
        finish_static(&mut resp, &path, APP_CSP);
        resp
    } else if host == cfg.proxy {
        let peer = req
            .extensions()
            .get::<PeerIp>()
            .map(|p| p.0)
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        if !state.limiter.allow(peer, Instant::now()) {
            let mut resp = (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return resp;
        }
        let mut resp = proxy::proxy_pass(&state, req).await;
        hsts(&mut resp);
        resp
    } else {
        // A name we do not serve (possible only via IP-literal or a stray
        // SNI-less client — our cert covers exactly the four vhosts).
        StatusCode::MISDIRECTED_REQUEST.into_response()
    }
}

/// What a static miss turns into.
enum NotFound {
    /// The site's burrow `404.html`, status 404.
    BurrowPage,
    /// The SPA's `index.html`, status 200 — client-side routing — unless the
    /// last path segment looks like an asset (contains a dot), which stays a
    /// plain 404 so a broken content-hashed reference is a loud miss.
    SpaFallback,
}

/// What a request path names inside a docroot.
///
/// The docroots are written by the deploy key, and its forced command confines
/// WHERE that key writes, not WHAT: the one option word it accepts is the one
/// `rsync -az` sends, whose `-l` keeps symlinks and whose `-D` keeps devices
/// and specials. So a release can carry a link pointing anywhere the door can
/// read — its own `StateDirectory`, which holds the issued certificate keys and
/// the ACME account — or a FIFO, which parks whoever opens it. `front-door.md`
/// § The box promises a leaked deploy key "can replace content, never touch
/// certificates, units, or the system, and **read nothing**"; the door is the
/// other reader of what that key writes, so containment is the door's job too.
enum Resolved {
    /// A canonical regular file inside the canonical docroot.
    File(PathBuf),
    /// A directory named without a trailing slash: ask again with one, so the
    /// page's relative links resolve against the directory.
    TrailingSlash,
    /// Nothing servable — absent, outside the docroot, or not a regular file.
    Miss,
}

/// Canonicalize `path` and accept it only as a regular file inside the
/// already-canonical `root`.
///
/// Canonicalizing FIRST is what makes this a boundary: every symlink is
/// followed before the decision, so a link is judged by where it lands rather
/// than by its own name, and `is_file()` on the resolved path then rejects a
/// FIFO, a socket, a device and a directory alike. The caller opens the path
/// this returns and no other — the path that is checked is the path that is
/// opened.
async fn contained_file(root: &Path, path: &Path) -> Option<PathBuf> {
    let real = tokio::fs::canonicalize(path).await.ok()?;
    if !real.starts_with(root) {
        return None;
    }
    tokio::fs::metadata(&real)
        .await
        .ok()?
        .is_file()
        .then_some(real)
}

/// The relative path a URL path names under a docroot, or `None` if it names
/// anything else.
///
/// Each segment is percent-decoded on its own and must be an ordinary name:
/// `.`, `..` and a decoded separator (`%2f`, and `%5c` where a backslash
/// separates) are refused rather than normalized, so no spelling of a traversal
/// survives into the join.
fn docroot_relative(url_path: &str) -> Option<PathBuf> {
    let mut rel = PathBuf::new();
    for raw in url_path.split('/') {
        if raw.is_empty() {
            continue;
        }
        let seg = fauna_core::web::percent_decode(raw, false);
        if seg == "." || seg == ".." || seg.contains(['/', '\\', '\0']) {
            return None;
        }
        rel.push(seg);
    }
    Some(rel)
}

/// Resolve a request path against a docroot.
///
/// The root is canonicalized per request because it IS a symlink — `current`,
/// which the deploy flips onto a new release underneath a running door
/// (front-door.md § The box) — so a root resolved once at startup would pin the
/// containment test to the release that happened to be live at boot.
async fn resolve(root: &Path, url_path: &str) -> Resolved {
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return Resolved::Miss;
    };
    let Some(rel) = docroot_relative(url_path) else {
        return Resolved::Miss;
    };
    let Ok(real) = tokio::fs::canonicalize(root.join(rel)).await else {
        return Resolved::Miss;
    };
    if !real.starts_with(&root) {
        return Resolved::Miss;
    }
    let Ok(meta) = tokio::fs::metadata(&real).await else {
        return Resolved::Miss;
    };
    if meta.is_file() {
        return Resolved::File(real);
    }
    if !meta.is_dir() {
        // A FIFO, socket or device: a miss, never an open.
        return Resolved::Miss;
    }
    if !url_path.ends_with('/') {
        return Resolved::TrailingSlash;
    }
    // A directory is servable only through its `index.html` — the site is
    // built with directory pages (`who/index.html`), so this is the ordinary
    // path for every page but the root.
    match contained_file(&root, &real.join("index.html")).await {
        Some(file) => Resolved::File(file),
        None => Resolved::Miss,
    }
}

async fn serve_static(root: &Path, req: Request, miss: NotFound) -> Response {
    let path = req.uri().path().to_owned();
    let file = match resolve(root, &path).await {
        Resolved::File(file) => file,
        Resolved::TrailingSlash => {
            // 307 + the query, which is what this vhost has always answered
            // here; deliberately not a permanent redirect, so a page that
            // later becomes a file is not cached into a loop.
            let query = req.uri().query().map_or(String::new(), |q| format!("?{q}"));
            return Redirect::temporary(&format!("{path}/{query}")).into_response();
        }
        Resolved::Miss => return static_miss(root, &path, miss).await,
    };
    // Serving the RESOLVED path, never the request's: `ServeFile` opens the
    // one file `resolve` cleared and does not re-interpret the URL.
    match ServeFile::new(&file).try_call(req).await {
        Ok(r) => r.map(Body::new),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// What a static miss turns into (§ vhost table).
async fn static_miss(root: &Path, path: &str, miss: NotFound) -> Response {
    match miss {
        NotFound::BurrowPage => serve_file(root, "404.html", StatusCode::NOT_FOUND).await,
        NotFound::SpaFallback => {
            let last = path.rsplit('/').next().unwrap_or("");
            if last.contains('.') {
                // asset-shaped miss stays a plain 404
                StatusCode::NOT_FOUND.into_response()
            } else {
                serve_file(root, "index.html", StatusCode::OK).await
            }
        }
    }
}

/// Serve one named file from `root` with a fixed status (the 404/fallback
/// cold path — per-request reads are fine here). Contained on the same terms
/// as every other read: these two names are as pushable as any other content,
/// and a linked `404.html` or SPA `index.html` would otherwise hand out
/// whatever it points at on every miss.
async fn serve_file(root: &Path, name: &str, status: StatusCode) -> Response {
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return status.into_response();
    };
    let Some(path) = contained_file(&root, &root.join(name)).await else {
        return status.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut resp = Response::new(Body::from(bytes));
            *resp.status_mut() = status;
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            resp
        }
        Err(_) => status.into_response(),
    }
}

/// Security + caching headers for a static vhost response.
fn finish_static(resp: &mut Response, path: &str, csp: &'static str) {
    let headers = resp.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        axum::http::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    if resp.status().is_success() {
        let cache = if path.contains("/_astro/") || path.starts_with("/assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=300"
        };
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    }
    hsts(resp);
}

/// HSTS on every 443 response.
fn hsts(resp: &mut Response) {
    resp.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DoorConfig;
    use axum::body::to_bytes;
    use axum::http::Request;
    use std::net::IpAddr;
    use tower_service::Service;

    /// A DoorConfig over temp docroots and a caller-chosen upstream.
    fn test_cfg(
        site_root: &std::path::Path,
        app_root: &std::path::Path,
        upstream: &str,
    ) -> DoorConfig {
        DoorConfig {
            apex: "door.test".into(),
            www: "www.door.test".into(),
            app: "app.door.test".into(),
            proxy: "proxy.door.test".into(),
            site_root: site_root.into(),
            app_root: app_root.into(),
            proxy_upstream: upstream.into(),
            state_dir: std::env::temp_dir(),
            http_bind: ([127, 0, 0, 1], 0).into(),
            https_bind: ([127, 0, 0, 1], 0).into(),
            contact_email: String::new(),
            acme_directory_url: None,
            acme_staging: true,
        }
    }

    /// Marker of a stand-in for the door's own TLS key: a file OUTSIDE both
    /// docroots, which is where a link pushed into a release would aim
    /// (front-door.md § The box — the door may read its own `StateDirectory`,
    /// so nothing below the door stops the read).
    const KEY_MARKER: &str = "stand-in door private key";

    struct Fixture {
        app: Router,
        site: tempfile::TempDir,
        app_dir: tempfile::TempDir,
        outside: tempfile::TempDir,
    }

    impl Fixture {
        fn site(&self) -> &std::path::Path {
            self.site.path()
        }
        fn app_root(&self) -> &std::path::Path {
            self.app_dir.path()
        }
        /// The stand-in key, outside both docroots.
        fn key(&self) -> std::path::PathBuf {
            self.outside.path().join("privkey.pem")
        }
    }

    fn fixture_with_upstream(upstream: &str) -> Fixture {
        let site = tempfile::tempdir().expect("site tempdir");
        let app_dir = tempfile::tempdir().expect("app tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::write(
            outside.path().join("privkey.pem"),
            format!("-----BEGIN PRIVATE KEY-----\n{KEY_MARKER}\n"),
        )
        .unwrap();
        std::fs::write(site.path().join("index.html"), "<h1>burrow home</h1>").unwrap();
        // The site is built with directory pages (`who/index.html`), so a
        // directory request is the ordinary case, not an edge one.
        std::fs::create_dir_all(site.path().join("who")).unwrap();
        std::fs::write(site.path().join("who/index.html"), "<h1>who we are</h1>").unwrap();
        std::fs::create_dir_all(site.path().join("_astro")).unwrap();
        std::fs::write(site.path().join("_astro/site.css"), "body{}").unwrap();
        std::fs::write(site.path().join("404.html"), "<h1>lost in the burrow</h1>").unwrap();
        std::fs::write(app_dir.path().join("index.html"), "<div id=app></div>").unwrap();
        std::fs::create_dir_all(app_dir.path().join("assets")).unwrap();
        std::fs::write(app_dir.path().join("assets/app.js"), "//js").unwrap();
        let cfg = test_cfg(site.path(), app_dir.path(), upstream);
        let state = Arc::new(DoorState::new(Arc::new(cfg)).expect("state"));
        Fixture {
            app: door_router(state),
            site,
            app_dir,
            outside,
        }
    }

    fn fixture() -> Fixture {
        fixture_with_upstream("http://127.0.0.1:9")
    }

    fn get(host: &str, path: &str) -> Request<Body> {
        get_from(
            host,
            path,
            IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 9)),
        )
    }

    fn get_from(host: &str, path: &str, peer: IpAddr) -> Request<Body> {
        Request::builder()
            .uri(path)
            .header("host", host)
            .extension(PeerIp(peer))
            .body(Body::empty())
            .unwrap()
    }

    async fn body_string(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn apex_serves_the_site_with_hardened_headers() {
        let mut fx = fixture();
        let resp = fx.app.call(get("door.test", "/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS.as_str()], "nosniff");
        assert_eq!(h["x-frame-options"], "DENY");
        assert_eq!(h[header::CONTENT_SECURITY_POLICY.as_str()], SITE_CSP);
        assert!(h.contains_key(header::STRICT_TRANSPORT_SECURITY.as_str()));
        assert!(h.contains_key("permissions-policy"));
        assert_eq!(h[header::CACHE_CONTROL.as_str()], "public, max-age=300");
        assert!(body_string(resp).await.contains("burrow home"));
    }

    #[tokio::test]
    async fn apex_hashed_assets_cache_immutably() {
        let mut fx = fixture();
        let resp = fx
            .app
            .call(get("door.test", "/_astro/site.css"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()[header::CACHE_CONTROL.as_str()],
            "public, max-age=31536000, immutable"
        );
    }

    #[tokio::test]
    async fn apex_miss_serves_the_burrow_404_with_status_404() {
        let mut fx = fixture();
        let resp = fx
            .app
            .call(get("door.test", "/no/such/page"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()[header::CONTENT_SECURITY_POLICY.as_str()],
            SITE_CSP,
            "the 404 page carries the same hardened headers"
        );
        assert!(body_string(resp).await.contains("lost in the burrow"));
    }

    #[tokio::test]
    async fn www_redirects_308_to_the_apex_preserving_path_and_query() {
        let mut fx = fixture();
        let resp = fx
            .app
            .call(get("www.door.test", "/what?tab=cost"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            resp.headers()[header::LOCATION.as_str()],
            "https://door.test/what?tab=cost"
        );
        assert!(
            resp.headers()
                .contains_key(header::STRICT_TRANSPORT_SECURITY.as_str())
        );
    }

    #[tokio::test]
    async fn app_serves_spa_and_falls_back_to_index_for_client_routes() {
        let mut fx = fixture();
        let direct = fx.app.call(get("app.door.test", "/")).await.unwrap();
        assert_eq!(direct.status(), StatusCode::OK);
        assert_eq!(
            direct.headers()[header::CONTENT_SECURITY_POLICY.as_str()],
            APP_CSP
        );

        let route = fx
            .app
            .call(get("app.door.test", "/conversations/inbox"))
            .await
            .unwrap();
        assert_eq!(route.status(), StatusCode::OK, "client route → index.html");
        assert!(body_string(route).await.contains("id=app"));
    }

    #[tokio::test]
    async fn app_asset_shaped_miss_stays_a_plain_404() {
        let mut fx = fixture();
        let resp = fx
            .app
            .call(get("app.door.test", "/assets/gone.19af.js"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_string(resp).await;
        assert!(
            !body.contains("id=app"),
            "a broken content-hashed reference must be a loud miss, not the SPA shell"
        );
    }

    #[tokio::test]
    async fn apex_serves_a_directory_page_through_its_index() {
        let mut fx = fixture();
        let resp = fx.app.call(get("door.test", "/who/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers()[header::CONTENT_TYPE.as_str()]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "the index's own type, not the directory's"
        );
        assert!(body_string(resp).await.contains("who we are"));
    }

    #[tokio::test]
    async fn apex_sends_a_directory_named_without_a_slash_to_one() {
        let mut fx = fixture();
        let resp = fx.app.call(get("door.test", "/who?tab=a")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            resp.headers()[header::LOCATION.as_str()],
            "/who/?tab=a",
            "relative links on the page resolve against the directory"
        );
    }

    #[tokio::test]
    async fn an_encoded_path_names_the_same_file_the_plain_one_does() {
        let mut fx = fixture();
        let resp = fx
            .app
            .call(get("door.test", "/_astro/site%2Ecss"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(body_string(resp).await.contains("body{}"));
    }

    #[tokio::test]
    async fn an_encoded_separator_is_refused_rather_than_normalized() {
        let mut fx = fixture();
        for path in [
            "/..%2f..%2fprivkey.pem",
            "/%2e%2e/%2e%2e/privkey.pem",
            "/.%2e/index.html",
            // Lands back inside the docroot, so containment never sees it:
            // only the segment refusal answers this one.
            "/who/%2e%2e/who/",
            "/who/../who/",
        ] {
            let resp = fx.app.call(get("door.test", path)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    /// The docroots are written by the deploy key, whose forced command
    /// confines WHERE it writes and not WHAT: `rsync -az` keeps symlinks
    /// (`-l`) and specials (`-D`), so a release can carry a link out of the
    /// tree or a FIFO. `front-door.md` § The box promises a leaked deploy key
    /// "can replace content ... and **read nothing**" — the door is the other
    /// reader of what that key writes, so containment is its job too.
    #[cfg(unix)]
    mod docroot_containment {
        use super::*;
        use std::os::unix::fs::symlink;

        const FIFO_SENTINEL: &str = "parked request sentinel";

        /// `mkfifo(1)` rather than a `libc`/`nix` dev-dep: the door's decision
        /// record puts a hard **zero new dependencies** on this crate
        /// (front-door.md § The decision).
        fn mkfifo(path: &std::path::Path) {
            let st = std::process::Command::new("mkfifo")
                .arg(path)
                .status()
                .expect("run mkfifo(1)");
            assert!(st.success(), "mkfifo {}", path.display());
        }

        #[tokio::test]
        async fn a_pushed_file_link_out_of_the_docroot_serves_nothing() {
            let mut fx = fixture();
            symlink(fx.key(), fx.site().join("leak.txt")).unwrap();
            let resp = fx.app.call(get("door.test", "/leak.txt")).await.unwrap();
            let status = resp.status();
            let body = body_string(resp).await;
            assert!(!body.contains(KEY_MARKER), "the door served its own key");
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(body.contains("lost in the burrow"));
        }

        #[tokio::test]
        async fn a_pushed_directory_link_out_of_the_docroot_serves_nothing() {
            let mut fx = fixture();
            symlink(fx.outside.path(), fx.site().join("state")).unwrap();
            for path in ["/state/privkey.pem", "/state/"] {
                let resp = fx.app.call(get("door.test", path)).await.unwrap();
                let status = resp.status();
                let body = body_string(resp).await;
                assert!(!body.contains(KEY_MARKER), "{path} served the key");
                assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            }
        }

        #[tokio::test]
        async fn an_encoded_name_reaches_no_link_a_plain_one_cannot() {
            let mut fx = fixture();
            symlink(fx.key(), fx.site().join("leak.txt")).unwrap();
            let resp = fx.app.call(get("door.test", "/leak%2Etxt")).await.unwrap();
            let status = resp.status();
            let body = body_string(resp).await;
            assert!(
                !body.contains(KEY_MARKER),
                "the path that is checked must be the path that is opened"
            );
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn a_linked_burrow_404_is_not_served() {
            let mut fx = fixture();
            std::fs::remove_file(fx.site().join("404.html")).unwrap();
            symlink(fx.key(), fx.site().join("404.html")).unwrap();
            let resp = fx
                .app
                .call(get("door.test", "/no/such/page"))
                .await
                .unwrap();
            let status = resp.status();
            let body = body_string(resp).await;
            assert!(!body.contains(KEY_MARKER), "the 404 path served the key");
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn a_linked_spa_index_is_not_served() {
            let mut fx = fixture();
            std::fs::remove_file(fx.app_root().join("index.html")).unwrap();
            symlink(fx.key(), fx.app_root().join("index.html")).unwrap();
            for path in ["/", "/conversations/inbox"] {
                let resp = fx.app.call(get("app.door.test", path)).await.unwrap();
                let body = body_string(resp).await;
                assert!(!body.contains(KEY_MARKER), "{path} served the key");
            }
        }

        /// A FIFO parks whoever opens it. The witness carries no clock
        /// (e2e-conventions § 14): a writer thread blocks in `open(2)` until
        /// someone opens the read end, so if the door opens the FIFO the door
        /// IS that reader and the sentinel reaches the response. The test then
        /// becomes the reader itself so the writer is never left parked.
        #[tokio::test]
        async fn a_pushed_fifo_is_never_opened() {
            let mut fx = fixture();
            let fifo = fx.site().join("park.txt");
            mkfifo(&fifo);
            let writer_path = fifo.clone();
            let writer = std::thread::spawn(move || {
                use std::io::Write;
                if let Ok(mut f) = std::fs::File::create(&writer_path) {
                    let _ = f.write_all(FIFO_SENTINEL.as_bytes());
                }
            });

            let resp = fx.app.call(get("door.test", "/park.txt")).await.unwrap();
            let status = resp.status();
            let body = body_string(resp).await;
            assert!(
                !body.contains(FIFO_SENTINEL),
                "the door opened a pushed FIFO"
            );
            assert_eq!(status, StatusCode::NOT_FOUND);

            drop(std::fs::File::open(&fifo).expect("open the read end"));
            writer.join().expect("fifo writer");
        }

        /// The same shape pushed as the burrow `404.html`: the fallback reads
        /// go through the same containment as every other open, so a FIFO
        /// there cannot park every miss the site serves.
        #[tokio::test]
        async fn a_fifo_pushed_as_the_burrow_404_is_never_opened() {
            let mut fx = fixture();
            let fifo = fx.site().join("404.html");
            std::fs::remove_file(&fifo).unwrap();
            mkfifo(&fifo);
            let writer_path = fifo.clone();
            let writer = std::thread::spawn(move || {
                use std::io::Write;
                if let Ok(mut f) = std::fs::File::create(&writer_path) {
                    let _ = f.write_all(FIFO_SENTINEL.as_bytes());
                }
            });

            let resp = fx
                .app
                .call(get("door.test", "/no/such/page"))
                .await
                .unwrap();
            let status = resp.status();
            let body = body_string(resp).await;
            assert!(
                !body.contains(FIFO_SENTINEL),
                "the 404 path opened a pushed FIFO"
            );
            assert_eq!(status, StatusCode::NOT_FOUND);

            drop(std::fs::File::open(&fifo).expect("open the read end"));
            writer.join().expect("fifo writer");
        }

        /// Containment is the rule, not link-phobia: a link that lands inside
        /// the docroot is ordinary content and stays served.
        #[tokio::test]
        async fn a_link_that_stays_inside_the_docroot_is_still_served() {
            let mut fx = fixture();
            symlink(fx.site().join("who"), fx.site().join("mirror")).unwrap();
            let resp = fx.app.call(get("door.test", "/mirror/")).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(body_string(resp).await.contains("who we are"));
        }
    }

    #[tokio::test]
    async fn unknown_host_is_misdirected() {
        let mut fx = fixture();
        let resp = fx.app.call(get("evil.example", "/")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::MISDIRECTED_REQUEST);
        let no_host = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = fx.app.call(no_host).await.unwrap();
        assert_eq!(resp.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn proxy_vhost_forwards_path_and_query_and_strips_hop_by_hop() {
        // Stub upstream that echoes what it received.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stub = Router::new().fallback(|req: axum::extract::Request| async move {
            let echo = format!(
                "{}|{}|conn={}",
                req.uri().path(),
                req.uri().query().unwrap_or(""),
                req.headers().contains_key("connection"),
            );
            ([("x-upstream-echo", echo)], "upstream ok")
        });
        // spawn-ok(test-lifetime): dies with the test process.
        tokio::spawn(async move {
            axum::serve(listener, stub).await.ok();
        });

        let mut fx = fixture_with_upstream(&format!("http://{addr}"));
        let mut req = get(
            "proxy.door.test",
            "/namecheap/xml.response?Command=check&ClientIp=",
        );
        req.headers_mut()
            .insert("connection", HeaderValue::from_static("close"));
        let resp = fx.app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let echo = resp.headers()["x-upstream-echo"].to_str().unwrap();
        assert_eq!(
            echo, "/namecheap/xml.response|Command=check&ClientIp=|conn=false",
            "query string preserved verbatim; hop-by-hop stripped"
        );
        assert!(body_string(resp).await.contains("upstream ok"));
    }

    #[tokio::test]
    async fn proxy_upstream_down_is_a_502() {
        // Reserve a port, then close it — nothing listens there.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut fx = fixture_with_upstream(&format!("http://{addr}"));
        let resp = fx.app.call(get("proxy.door.test", "/x")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn proxy_vhost_rate_limits_per_peer() {
        let mut fx = fixture(); // upstream down — but 502s still consume budget
        let hammer = IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 77));
        let mut saw_429 = false;
        for _ in 0..40 {
            let resp = fx
                .app
                .call(get_from("proxy.door.test", "/x", hammer))
                .await
                .unwrap();
            if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                assert_eq!(resp.headers()[header::RETRY_AFTER.as_str()], "1");
                saw_429 = true;
                break;
            }
        }
        assert!(
            saw_429,
            "40 immediate requests must exhaust the 30-token burst"
        );
        // An unrelated peer is not limited.
        let other = fx
            .app
            .call(get_from(
                "proxy.door.test",
                "/x",
                IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 78)),
            ))
            .await
            .unwrap();
        assert_ne!(other.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
