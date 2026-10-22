//! fauna-cors-proxy — stateless CORS forwarder for provisioning provider APIs.
//!
//! Forwards POST /<provider>/<...path> to the provider's API endpoint,
//! preserving the request body and the user's Authorization header.
//!
//! Does NOT parse or log request/response bodies. The only state is the
//! static provider base-URL map.

use axum::{
    Router,
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
};
use std::{net::SocketAddr, time::Duration};

/// The provider-key → upstream-base table the forwarder resolves against.
/// Injected as router state so tests can point it at a local stub upstream.
type Bases = &'static [(&'static str, &'static str)];

const BASE_URLS: Bases = &[
    ("hetzner", "https://api.hetzner.cloud"),
    ("cloudflare", "https://api.cloudflare.com"),
    ("porkbun", "https://api.porkbun.com"),
    ("ovh", "https://api.ovh.com"),
    ("digitalocean", "https://api.digitalocean.com"),
    ("vultr", "https://api.vultr.com"),
    ("linode", "https://api.linode.com"),
    ("gandi", "https://api.gandi.net"),
    ("namecheap", "https://api.namecheap.com"),
    // Let's Encrypt ACME directories — the browser (wasm) DNS-01 cert order
    // (`fauna-client-dns::acme_pure`) can't reach these cross-origin (no CORS), so
    // it routes every ACME request (and the absolute follow-up URLs the CA
    // returns) through here. Requests are JWS-signed end-to-end, so the proxy
    // stays credential-blind — same invariant as the DNS-provider routes
    // (`tls-certificates.md` § C). The proxy preserves all response headers, so
    // the `Replay-Nonce` / `Location` headers ACME relies on pass through intact.
    ("acme-le", "https://acme-v02.api.letsencrypt.org"),
    (
        "acme-le-staging",
        "https://acme-staging-v02.api.letsencrypt.org",
    ),
    // The nest image's registry — the browser setup wizard's anonymous read of
    // which update channels have a build (`onboarding-provisioning.md` § 5 →
    // *What exists decides the default*). Pull tokens are anonymous and the
    // client re-points the bearer-challenge realm (`https://ghcr.io/token`) at
    // this same route, so the proxy stays credential-blind; it relays the
    // `Authorization` header, the token query and the `Docker-Content-Digest`
    // reply header unchanged.
    ("ghcr", "https://ghcr.io"),
];

/// Build the forwarder router over a given provider→base table.
///
/// Split out of `main` so tests can drive the real routing/forwarding path
/// against a stub upstream instead of a live provider API.
fn app(bases: Bases) -> Router {
    Router::new()
        .route("/{provider}/{*path}", any(forward))
        .route("/healthz", any(|| async { "ok" }))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(ProxyState {
            bases,
            client: upstream_client(),
        })
}

/// Per-request ceiling on an upstream call. Hard-coded: no human chooses it.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// The one client every forwarded request shares (one connection pool, one TLS
/// setup). It relays exactly ONE request: redirects are never chased here — a
/// `3xx` goes back to the caller as-is, and the caller re-points the
/// `Location` at the proxy itself (the ACME comment above). Chasing it from the
/// proxy would aim a provider-controlled `Location` at the proxy's own network
/// position, loopback behind the front door included.
fn upstream_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(UPSTREAM_TIMEOUT)
        .build()
        .expect("static reqwest client config is valid")
}

/// Router state: the provider table plus the shared upstream client.
#[derive(Clone)]
struct ProxyState {
    bases: Bases,
    client: reqwest::Client,
}

/// Resolve an incoming `/{provider}/{path}?{query}` request to its upstream URL.
///
/// Pure — the whole URL-construction contract lives here so it can be pinned
/// by unit tests without a network hop.
///
/// The query string is part of the request, not decoration: Namecheap passes
/// *every* parameter that way (`?ApiUser=…&Command=…`), and Cloudflare/Vultr
/// carry pagination and filters there. Forwarding the path alone would send a
/// materially different request upstream than the client made.
fn upstream_url(bases: Bases, provider: &str, path: &str, query: Option<&str>) -> Option<String> {
    let base = bases
        .iter()
        .find_map(|(k, v)| if *k == provider { Some(*v) } else { None })?;
    Some(match query {
        Some(q) if !q.is_empty() => format!("{base}/{path}?{q}"),
        _ => format!("{base}/{path}"),
    })
}

/// Bind address from artifact-set wiring (`PORT`, `BIND_ADDR` — the systemd
/// unit / container sets them; there is no human-facing config surface).
/// Defaults preserve the container deployment (`0.0.0.0:8080`); behind the
/// fauna.social front door the unit sets `BIND_ADDR=127.0.0.1` so the proxy
/// is loopback-only and the door is the sole process on 80/443
/// (front-door.md § Security architecture).
fn bind_addr(port: Option<&str>, bind: Option<&str>) -> SocketAddr {
    let port: u16 = port.and_then(|s| s.parse().ok()).unwrap_or(8080);
    let ip: std::net::IpAddr = bind
        .and_then(|s| s.parse().ok())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    SocketAddr::from((ip, port))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let app = app(BASE_URLS);

    let addr = bind_addr(
        std::env::var("PORT").ok().as_deref(),
        std::env::var("BIND_ADDR").ok().as_deref(),
    );
    tracing::info!(?addr, "fauna-cors-proxy listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn forward(
    State(ProxyState { bases, client }): State<ProxyState>,
    Path((provider, path)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let url =
        upstream_url(bases, &provider, &path, query.as_deref()).ok_or(StatusCode::NOT_FOUND)?;

    let mut req = client.request(method, &url).body(body);
    for (k, v) in headers.iter() {
        // Forward all headers except Host (reqwest sets it from URL).
        if k == axum::http::header::HOST {
            continue;
        }
        req = req.header(k.as_str(), v);
    }
    let resp = req.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let status = resp.status();
    let rheaders = resp.headers().clone();
    let rbody = resp.bytes().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    let mut builder = Response::builder().status(status);
    for (k, v) in rheaders.iter() {
        builder = builder.header(k.as_str(), v);
    }
    builder
        .body(axum::body::Body::from(rbody))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUB: Bases = &[("stub", "https://api.example.test/v1")];

    /// Serve `router` on an ephemeral loopback port; returns its address.
    async fn serve(router: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    }

    /// The real `app()` over a one-entry table pointing `stub` at `upstream`.
    async fn proxy_to(upstream: SocketAddr) -> SocketAddr {
        let base: &'static str = Box::leak(format!("http://{upstream}").into_boxed_str());
        let bases: Bases = Box::leak(Box::new([("stub", base)]));
        serve(app(bases)).await
    }

    fn no_redirect_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    /// The forwarder relays ONE request. A `302` from the upstream must come
    /// back as that `302` (with its `Location`), never as the body of the page
    /// it points at — chasing it would fetch from the proxy's own network
    /// position, loopback behind the front door included.
    #[tokio::test]
    async fn upstream_redirect_is_relayed_not_followed() {
        let target =
            serve(Router::new().route("/final", any(|| async { "REDIRECTED BODY" }))).await;
        let location = format!("http://{target}/final");
        let upstream = serve(Router::new().route(
            "/hop",
            any(move || {
                let location = location.clone();
                async move {
                    Response::builder()
                        .status(StatusCode::FOUND)
                        .header("location", location)
                        .body(axum::body::Body::empty())
                        .unwrap()
                }
            }),
        ))
        .await;
        let proxy = proxy_to(upstream).await;

        let resp = no_redirect_client()
            .get(format!("http://{proxy}/stub/hop"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::FOUND);
        assert_eq!(
            resp.headers().get("location").unwrap().to_str().unwrap(),
            format!("http://{target}/final"),
        );
        assert!(resp.text().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_provider_is_404() {
        let upstream = serve(Router::new()).await;
        let proxy = proxy_to(upstream).await;
        let resp = reqwest::get(format!("http://{proxy}/nope/some/path"))
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }

    /// `Host` is the caller's view of the proxy; the upstream must see its own.
    #[tokio::test]
    async fn host_header_is_not_forwarded() {
        let upstream = serve(Router::new().route(
            "/host",
            any(|headers: HeaderMap| async move {
                headers
                    .get(axum::http::header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            }),
        ))
        .await;
        let proxy = proxy_to(upstream).await;
        let seen = reqwest::get(format!("http://{proxy}/stub/host"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(seen, upstream.to_string());
    }

    #[test]
    fn unknown_provider_has_no_upstream() {
        assert_eq!(upstream_url(STUB, "nope", "some/path", None), None);
    }

    /// The wizard's registry read: the OCI paths and the token realm both ride
    /// the `ghcr` route, query intact.
    #[test]
    fn the_ghcr_route_forwards_registry_and_token_paths() {
        assert_eq!(
            upstream_url(
                BASE_URLS,
                "ghcr",
                "v2/faunasocial/nest/manifests/latest",
                None
            )
            .as_deref(),
            Some("https://ghcr.io/v2/faunasocial/nest/manifests/latest"),
        );
        assert_eq!(
            upstream_url(
                BASE_URLS,
                "ghcr",
                "token",
                Some("scope=repository:faunasocial/nest:pull&service=ghcr.io")
            )
            .as_deref(),
            Some("https://ghcr.io/token?scope=repository:faunasocial/nest:pull&service=ghcr.io"),
        );
    }

    #[test]
    fn known_provider_appends_the_path_to_its_base() {
        assert_eq!(
            upstream_url(STUB, "stub", "zones/list", None).as_deref(),
            Some("https://api.example.test/v1/zones/list"),
        );
    }

    /// Namecheap's entire API is query-string driven
    /// (`?ApiUser=…&ApiKey=…&ClientIp=…&Command=namecheap.domains.check`), and
    /// Cloudflare/Vultr carry pagination + filters the same way. A forwarder
    /// that drops the query silently turns every such call into a different
    /// request than the client made — so query preservation is part of the
    /// upstream-URL contract, not an incidental detail.
    #[test]
    fn query_string_is_preserved_on_the_upstream_url() {
        assert_eq!(
            upstream_url(
                STUB,
                "stub",
                "xml.response",
                Some("ApiUser=u&Command=namecheap.domains.check"),
            )
            .as_deref(),
            Some(
                "https://api.example.test/v1/xml.response\
                 ?ApiUser=u&Command=namecheap.domains.check"
            ),
        );
    }

    #[test]
    fn absent_query_adds_no_question_mark() {
        assert_eq!(
            upstream_url(STUB, "stub", "zones/list", None).as_deref(),
            Some("https://api.example.test/v1/zones/list"),
        );
    }

    /// An empty `?` carries no parameters; appending a bare `?` would change
    /// the request for no reason, so it is dropped like an absent query.
    #[test]
    fn empty_query_adds_no_question_mark() {
        assert_eq!(
            upstream_url(STUB, "stub", "zones/list", Some("")).as_deref(),
            Some("https://api.example.test/v1/zones/list"),
        );
    }

    #[test]
    fn bind_addr_defaults_preserve_the_container_shape() {
        assert_eq!(bind_addr(None, None).to_string(), "0.0.0.0:8080");
        assert_eq!(bind_addr(Some("9000"), None).to_string(), "0.0.0.0:9000");
    }

    #[test]
    fn bind_addr_honors_the_loopback_wiring_and_ignores_garbage() {
        assert_eq!(
            bind_addr(Some("8402"), Some("127.0.0.1")).to_string(),
            "127.0.0.1:8402"
        );
        // Unparseable artifact values fall back rather than panic the unit.
        assert_eq!(
            bind_addr(Some("x"), Some("nonsense")).to_string(),
            "0.0.0.0:8080"
        );
    }
}
