//! ATProto OAuth: client construction, handle resolution, scopes.

use std::sync::Arc;
use std::time::Duration;

use atrium_identity::did::{CommonDidResolver, CommonDidResolverConfig, DEFAULT_PLC_DIRECTORY_URL};
use atrium_identity::handle::{WellKnownHandleResolver, WellKnownHandleResolverConfig};
use atrium_oauth::{
    AtprotoClientMetadata, AuthMethod, GrantType, KnownScope, OAuthClient, OAuthClientConfig,
    OAuthResolverConfig, Scope,
};
use atrium_xrpc::HttpClient;
use jose_jwk::Jwk;

use crate::dial_guard::{DialPolicy, GlobalOnlyResolver, dial_refusal, redirect_policy};
use crate::keypair::Es256Keypair;
use crate::store::{SqliteSessionStore, SqliteStateStore, StorageBackend};

// Re-exports for consumers (e.g. fauna-nest auth routes)
pub use atrium_api::agent::{Agent, SessionManager};
pub use atrium_oauth::{AuthorizeOptions, CallbackParams, OAuthClientMetadata, OAuthSession};
pub use atrium_xrpc::http;

/// A canned in-process HTTP responder used **only by tests** to drive the
/// OAuth `authorize()` / `callback()` flows against a fake atproto far end
/// without network access. It maps a request (matched on method + URI) to a
/// canned response; see [`build_oauth_client_with_responder`]. Production
/// [`ReqwestHttpClient`]s never set one (the `responder` field is `None`), so
/// this is inert off the test path.
pub type FakeHttpResponder =
    Arc<dyn Fn(&http::Request<Vec<u8>>) -> http::Response<Vec<u8>> + Send + Sync>;

/// The concrete OAuth session type returned by [`BlueskyOAuthClient`].
pub type BlueskyOAuthSession =
    OAuthSession<ReqwestHttpClient, BlueskyDidResolver, BlueskyHandleResolver, SqliteSessionStore>;

/// An authenticated Bluesky agent backed by an OAuth session.
pub type BlueskyAgent = Agent<BlueskyOAuthSession>;

// ---------------------------------------------------------------------------
// Reqwest-based HttpClient (atrium-oauth default-client feature is off)
// ---------------------------------------------------------------------------

/// The longest one request to an atproto far end may take, from the dial to
/// the last body byte. The far end is the PDS a linked account's DID names —
/// a server any local user may run — so a far end that never answers is cut
/// off here rather than stalling every account's poll and send behind it.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest the dial alone may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest response body read from an atproto far end — far above any
/// real XRPC reply (a page of a timeline, of notifications or of a
/// conversation's messages is tens of kilobytes), so only a far end streaming
/// without end meets it.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Minimal [`HttpClient`] implementation backed by [`reqwest::Client`].
///
/// This is equivalent to atrium-oauth's `DefaultHttpClient` but available
/// without the `default-client` feature flag — and bounded where that one is
/// not: every request under [`REQUEST_TIMEOUT`] / [`CONNECT_TIMEOUT`], every
/// body read under [`MAX_RESPONSE_BYTES`], and every dial — the first and each
/// redirect hop — under the [`crate::dial_guard`]: https only, every address
/// globally routable and pinned for the connection.
#[derive(Clone)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
    /// The targets [`client`](Self::client) may dial — [`DialPolicy::PublicOnly`]
    /// outside tests.
    policy: DialPolicy,
    /// The response-body ceiling — [`MAX_RESPONSE_BYTES`] outside tests.
    max_body: usize,
    /// Test seam: when `Some`, [`send_http`](HttpClient::send_http) serves
    /// every request from this canned responder instead of the network. Always
    /// `None` in production (set only by [`with_responder`](Self::with_responder)).
    responder: Option<FakeHttpResponder>,
    /// Test seam: when `Some`, [`send_http`](HttpClient::send_http) sends every
    /// request to this origin instead of the one in its URI, carrying the
    /// original authority in [`TEST_ORIGINAL_HOST_HEADER`]. Always `None` in
    /// production (set only by [`with_origin_override`](Self::with_origin_override),
    /// which exists only under `test-helpers`).
    origin_override: Option<String>,
}

/// The header a [`ReqwestHttpClient::with_origin_override`] request carries its
/// original authority (`host[:port]`) in, so one loopback fake can answer for
/// every atproto host the flow resolves (handle, PLC directory, PDS,
/// authorization server).
pub const TEST_ORIGINAL_HOST_HEADER: &str = "x-fauna-test-original-host";

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`reqwest::Client`] under the given timeouts and dial policy: names
/// resolved by the guard's resolver (every address permitted, the connection
/// pinned to them), every redirect hop judged before it is followed, and no
/// proxy — a proxy would resolve the target itself, past the guard.
fn bounded_client(
    timeout: Duration,
    connect_timeout: Duration,
    policy: DialPolicy,
) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .dns_resolver(GlobalOnlyResolver::new(policy))
        .redirect(redirect_policy(policy))
        .no_proxy()
        .build()
        // The same failure `reqwest::Client::new()` panics on: the TLS
        // backend did not initialize.
        .expect("the atproto HTTP client builds")
}

impl ReqwestHttpClient {
    pub fn new() -> Self {
        Self::with_limits(
            REQUEST_TIMEOUT,
            CONNECT_TIMEOUT,
            MAX_RESPONSE_BYTES,
            DialPolicy::PublicOnly,
        )
    }

    /// [`Self::new`] under other bounds and policy — what lets a test reach
    /// the bounds in milliseconds and bytes rather than seconds and megabytes,
    /// and a loopback far end under the test policy.
    fn with_limits(
        timeout: Duration,
        connect_timeout: Duration,
        max_body: usize,
        policy: DialPolicy,
    ) -> Self {
        Self {
            client: bounded_client(timeout, connect_timeout, policy),
            policy,
            max_body,
            responder: None,
            origin_override: None,
        }
    }

    /// Build a client that serves every request from an in-process canned
    /// [`FakeHttpResponder`] rather than the network — a **test-only** seam for
    /// exercising the atproto OAuth flow hermetically. See
    /// [`build_oauth_client_with_responder`].
    pub fn with_responder(responder: FakeHttpResponder) -> Self {
        Self {
            responder: Some(responder),
            ..Self::new()
        }
    }

    /// Build a client that sends every request to `origin` (a plain-HTTP
    /// loopback fake, e.g. `http://127.0.0.1:4711`) in place of the origin its
    /// URI names, keeping the path and query and carrying the original
    /// authority in [`TEST_ORIGINAL_HOST_HEADER`]. The **out-of-process** twin
    /// of [`with_responder`](Self::with_responder): it lets a *running* nest's
    /// OAuth client reach an e2e harness's fake atproto far end. Compiled only
    /// under `test-helpers`, which no production build enables; the dial runs
    /// under [`DialPolicy::PublicOrLoopback`], so the loopback fake is the one
    /// non-public target it reaches.
    #[cfg(feature = "test-helpers")]
    pub fn with_origin_override(origin: impl Into<String>) -> Self {
        Self {
            origin_override: Some(origin.into().trim_end_matches('/').to_string()),
            ..Self::with_limits(
                REQUEST_TIMEOUT,
                CONNECT_TIMEOUT,
                MAX_RESPONSE_BYTES,
                DialPolicy::PublicOrLoopback,
            )
        }
    }
}

impl HttpClient for ReqwestHttpClient {
    async fn send_http(
        &self,
        request: atrium_xrpc::http::Request<Vec<u8>>,
    ) -> core::result::Result<
        atrium_xrpc::http::Response<Vec<u8>>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        if let Some(responder) = &self.responder {
            return Ok(responder(&request));
        }
        let request = match &self.origin_override {
            Some(origin) => redirect_to_origin(request, origin)?,
            None => request,
        };
        let request: reqwest::Request = request.try_into()?;
        // An address-literal host never reaches the guard's resolver, so the
        // URL text is judged here; a name's addresses are judged at the dial.
        if let Some(reason) = dial_refusal(request.url(), self.policy) {
            return Err(reason.into());
        }
        let response = self.client.execute(request).await?;
        let mut builder = atrium_xrpc::http::Response::builder().status(response.status());
        for (k, v) in response.headers() {
            builder = builder.header(k, v);
        }
        let body = fauna_nest_http::capped::read_capped(response, self.max_body).await?;
        builder.body(body).map_err(Into::into)
    }
}

/// Rewrite `request` to `origin` (see [`ReqwestHttpClient::with_origin_override`]).
fn redirect_to_origin(
    mut request: atrium_xrpc::http::Request<Vec<u8>>,
    origin: &str,
) -> core::result::Result<
    atrium_xrpc::http::Request<Vec<u8>>,
    Box<dyn std::error::Error + Send + Sync + 'static>,
> {
    let authority = request
        .uri()
        .authority()
        .map(|a| a.as_str().to_string())
        .unwrap_or_default();
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    *request.uri_mut() = format!("{origin}{path_and_query}").parse()?;
    request
        .headers_mut()
        .insert(TEST_ORIGINAL_HOST_HEADER, authority.parse()?);
    Ok(request)
}

// ---------------------------------------------------------------------------
// Concrete type aliases
// ---------------------------------------------------------------------------

/// The concrete DID resolver type used by the Bluesky OAuth client.
pub type BlueskyDidResolver = CommonDidResolver<ReqwestHttpClient>;

/// The concrete handle resolver type used by the Bluesky OAuth client.
pub type BlueskyHandleResolver = WellKnownHandleResolver<ReqwestHttpClient>;

/// The concrete OAuth client type for Bluesky integration.
pub type BlueskyOAuthClient = OAuthClient<
    SqliteStateStore,
    SqliteSessionStore,
    BlueskyDidResolver,
    BlueskyHandleResolver,
    ReqwestHttpClient,
>;

// ---------------------------------------------------------------------------
// Config & construction
// ---------------------------------------------------------------------------

/// Configuration needed to build a [`BlueskyOAuthClient`].
pub struct BlueskyOAuthConfig {
    /// The public base URL of this node (e.g. `https://node.example.com`).
    pub public_url: String,
    /// The ES256 keypair used for `private_key_jwt` authentication.
    pub keypair: Es256Keypair,
    /// The storage backend for OAuth state and sessions.
    pub backend: Arc<dyn StorageBackend>,
}

/// Build [`AtprotoClientMetadata`] suitable for `OAuthClientConfig`.
///
/// This produces metadata for a confidential (non-localhost) client that
/// uses `private_key_jwt` authentication with ES256.
pub fn build_client_metadata(public_url: &str) -> AtprotoClientMetadata {
    AtprotoClientMetadata {
        client_id: format!("{public_url}/.well-known/atproto-oauth-client"),
        client_uri: Some(public_url.to_string()),
        redirect_uris: vec![format!("{public_url}/api/v1/bluesky/auth/callback")],
        token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
        grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
        scopes: bluesky_scopes(),
        jwks_uri: None,
        token_endpoint_auth_signing_alg: Some("ES256".into()),
    }
}

/// The standard set of scopes requested for Bluesky OAuth.
pub fn bluesky_scopes() -> Vec<Scope> {
    vec![
        Scope::Known(KnownScope::Atproto),
        Scope::Known(KnownScope::TransitionChatBsky),
    ]
}

/// Construct a fully-configured [`BlueskyOAuthClient`].
pub fn build_oauth_client(config: BlueskyOAuthConfig) -> anyhow::Result<BlueskyOAuthClient> {
    build_oauth_client_inner(config, ReqwestHttpClient::new())
}

/// Like [`build_oauth_client`], but every HTTP request the client makes
/// (handle/DID resolution, OAuth metadata fetches, PAR) is served in-process by
/// `responder`. **Test-only** — drives `authorize()` / `callback()` against a
/// fake atproto far end without network access. Production code uses
/// [`build_oauth_client`].
pub fn build_oauth_client_with_responder(
    config: BlueskyOAuthConfig,
    responder: FakeHttpResponder,
) -> anyhow::Result<BlueskyOAuthClient> {
    build_oauth_client_inner(config, ReqwestHttpClient::with_responder(responder))
}

/// Like [`build_oauth_client`], but every HTTP request the client makes is
/// sent to the loopback `origin` a test harness serves (see
/// [`ReqwestHttpClient::with_origin_override`]). **Test-only**, compiled only
/// under `test-helpers`: a running nest reaches it through its own
/// `test-hooks` seam.
#[cfg(feature = "test-helpers")]
pub fn build_oauth_client_with_origin_override(
    config: BlueskyOAuthConfig,
    origin: &str,
) -> anyhow::Result<BlueskyOAuthClient> {
    build_oauth_client_inner(config, ReqwestHttpClient::with_origin_override(origin))
}

fn build_oauth_client_inner(
    config: BlueskyOAuthConfig,
    http_client: ReqwestHttpClient,
) -> anyhow::Result<BlueskyOAuthClient> {
    let http_arc = Arc::new(http_client.clone());

    // Convert our Es256Keypair's serde_json::Value into a jose_jwk::Jwk.
    // The keypair stores the full private JWK; we need to add a `kid`.
    let mut jwk_value = config.keypair.private_jwk().clone();
    if let Some(obj) = jwk_value.as_object_mut() {
        // Add a deterministic kid if not already present.
        obj.entry("kid".to_string())
            .or_insert_with(|| serde_json::Value::String("fauna-bluesky-key".into()));
    }
    let jwk: Jwk = serde_json::from_value(jwk_value)?;

    let state_store = SqliteStateStore::new(Arc::clone(&config.backend));
    let session_store = SqliteSessionStore::new(config.backend);

    let client_metadata = build_client_metadata(&config.public_url);

    let did_resolver = CommonDidResolver::new(CommonDidResolverConfig {
        plc_directory_url: DEFAULT_PLC_DIRECTORY_URL.to_string(),
        http_client: Arc::clone(&http_arc),
    });

    let handle_resolver = WellKnownHandleResolver::new(WellKnownHandleResolverConfig {
        http_client: Arc::clone(&http_arc),
    });

    let oauth_client = OAuthClient::new(OAuthClientConfig {
        client_metadata,
        keys: Some(vec![jwk]),
        state_store,
        session_store,
        resolver: OAuthResolverConfig {
            did_resolver,
            handle_resolver,
            authorization_server_metadata: Default::default(),
            protected_resource_metadata: Default::default(),
        },
        http_client,
    })?;

    Ok(oauth_client)
}

/// Restore an authenticated agent for a previously-linked Bluesky account.
///
/// The `did` is the Bluesky account DID stored in `bluesky_accounts`. The OAuth
/// client handles token refresh transparently.
pub async fn restore_agent(
    oauth_client: &BlueskyOAuthClient,
    did: &str,
) -> anyhow::Result<BlueskyAgent> {
    let did_parsed: atrium_api::types::string::Did = did
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid DID: {did}"))?;
    let session = oauth_client
        .restore(&did_parsed)
        .await
        .map_err(|e| anyhow::anyhow!("failed to restore session for {did}: {e}"))?;
    Ok(Agent::new(session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::fixtures::MemoryBackend;

    /// A loopback far end that accepts one connection and hands it to
    /// `serve`, on a thread of its own. Returns its `http://` origin.
    fn far_end(serve: impl FnOnce(std::net::TcpStream) + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                serve(stream);
            }
        });
        origin
    }

    fn get(origin: &str) -> http::Request<Vec<u8>> {
        http::Request::builder()
            .uri(format!("{origin}/xrpc/app.bsky.actor.getProfile"))
            .body(Vec::new())
            .unwrap()
    }

    /// A far end that accepts the dial and never answers is cut off at the
    /// request timeout — the atproto client never waits on it for good.
    #[tokio::test]
    async fn a_far_end_that_never_answers_is_cut_off_at_the_timeout() {
        let origin = far_end(|stream| {
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        });
        let client = ReqwestHttpClient::with_limits(
            Duration::from_millis(300),
            CONNECT_TIMEOUT,
            1024,
            DialPolicy::PublicOrLoopback,
        );
        let outcome =
            tokio::time::timeout(Duration::from_secs(10), client.send_http(get(&origin))).await;
        let result = outcome.expect("the client's own timeout fires first");
        assert!(result.is_err(), "a silent far end is an error");
    }

    /// A far end that streams a body without end is refused once the body
    /// crosses the ceiling, never read whole.
    #[tokio::test]
    async fn a_body_past_the_ceiling_is_refused() {
        let origin = far_end(|mut stream| {
            use std::io::{Read, Write};
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n",
            );
            let chunk = [b'a'; 1024];
            for _ in 0..64 {
                if stream.write_all(b"400\r\n").is_err()
                    || stream.write_all(&chunk).is_err()
                    || stream.write_all(b"\r\n").is_err()
                {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        });
        let client = ReqwestHttpClient::with_limits(
            REQUEST_TIMEOUT,
            CONNECT_TIMEOUT,
            8 * 1024,
            DialPolicy::PublicOrLoopback,
        );
        let result = client.send_http(get(&origin)).await;
        let err = result.expect_err("a body past the ceiling is refused");
        assert!(err.to_string().contains("size cap"), "{err}");
    }

    /// A loopback listener that records whether anything ever connected.
    fn watched_listener() -> (u16, Arc<std::sync::atomic::AtomicBool>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let touched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&touched);
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        (port, touched)
    }

    /// The production client refuses a loopback, private or link-local
    /// target before any byte is sent — an address literal and a name that
    /// resolves inward alike (`network-exposure.md` § Rulings F9).
    #[tokio::test]
    async fn the_production_client_refuses_an_internal_target_before_dialing() {
        use crate::dial_guard::DIAL_REFUSED;
        let (port, touched) = watched_listener();
        let client = ReqwestHttpClient::new();
        for origin in [
            format!("https://127.0.0.1:{port}"),
            format!("https://localhost:{port}"),
            format!("http://127.0.0.1:{port}"),
            format!("https://[::1]:{port}"),
            "https://10.0.0.5".to_string(),
            "https://192.168.1.1".to_string(),
            "https://169.254.169.254".to_string(),
            "https://[fe80::1]".to_string(),
        ] {
            let outcome =
                tokio::time::timeout(Duration::from_secs(5), client.send_http(get(&origin))).await;
            let err = outcome
                .unwrap_or_else(|_| panic!("{origin}: refused at once, never dialed"))
                .expect_err("an internal target is refused");
            assert!(
                format!("{err:?}").contains(DIAL_REFUSED),
                "{origin}: the refusal names the guard: {err:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !touched.load(std::sync::atomic::Ordering::SeqCst),
            "no connection ever reached the loopback listener"
        );
    }

    /// A far end that redirects to an internal address (or to cleartext) is
    /// refused at the redirect: the hop is judged under the first dial's rule.
    #[tokio::test]
    async fn a_redirect_to_an_internal_address_is_refused_at_the_redirect() {
        use crate::dial_guard::DIAL_REFUSED;
        for location in [
            "https://169.254.169.254/latest/meta-data/",
            "https://10.0.0.5/",
            "https://[fd00::1]/",
            "http://example.com/",
        ] {
            let reply =
                format!("HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\n\r\n");
            let origin = far_end(move |mut stream| {
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(reply.as_bytes());
            });
            let client = ReqwestHttpClient::with_limits(
                Duration::from_secs(5),
                Duration::from_secs(2),
                1024,
                DialPolicy::PublicOrLoopback,
            );
            let err = client
                .send_http(get(&origin))
                .await
                .expect_err("the redirect is refused");
            assert!(
                format!("{err:?}").contains(DIAL_REFUSED),
                "{location}: the refusal names the guard: {err:?}"
            );
        }
    }

    #[test]
    fn client_metadata_has_correct_urls() {
        let meta = build_client_metadata("https://node.example.com");
        assert_eq!(
            meta.client_id,
            "https://node.example.com/.well-known/atproto-oauth-client"
        );
        assert_eq!(
            meta.redirect_uris,
            vec!["https://node.example.com/api/v1/bluesky/auth/callback"]
        );
        assert_eq!(meta.token_endpoint_auth_method, AuthMethod::PrivateKeyJwt);
        assert_eq!(
            meta.token_endpoint_auth_signing_alg,
            Some("ES256".to_string())
        );
    }

    /// The origin override moves only the origin: path and query survive, the
    /// original authority rides the header the fake dispatches on, and the
    /// scheme becomes the fake's plain HTTP.
    #[test]
    fn origin_override_keeps_path_and_query_and_carries_the_authority() {
        let req = http::Request::builder()
            .method("GET")
            .uri("https://pds.alice.test/xrpc/app.bsky.feed.getTimeline?limit=50")
            .body(Vec::new())
            .unwrap();
        let out = redirect_to_origin(req, "http://127.0.0.1:4711").unwrap();
        assert_eq!(
            out.uri().to_string(),
            "http://127.0.0.1:4711/xrpc/app.bsky.feed.getTimeline?limit=50"
        );
        assert_eq!(
            out.headers()[TEST_ORIGINAL_HOST_HEADER].to_str().unwrap(),
            "pds.alice.test"
        );
    }

    #[test]
    fn build_oauth_client_succeeds() {
        let keypair = crate::keypair::Es256Keypair::generate().unwrap();
        let backend = Arc::new(MemoryBackend::new());
        let config = BlueskyOAuthConfig {
            public_url: "https://test.example.com".to_string(),
            keypair,
            backend,
        };
        let client = build_oauth_client(config);
        assert!(
            client.is_ok(),
            "build_oauth_client failed: {}",
            client.err().unwrap()
        );
    }
}
