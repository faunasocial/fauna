//! HTTP forwarding handlers for fauna-router.
//!
//! Provides:
//! - `ProxyState` — shared Axum state
//! - `forward_to_backend()` — generic HTTP forwarding
//! - `proxy_fallback()` — catch-all handler that routes by actor_id
//!
//! The router serves no `/api/v1/*` route of its own. Its four HTTP twins of
//! deleted nest routes (node info, handle availability, handle lookup and
//! registration) were removed before the public release; apps reach all four
//! over the nest's anonymous WS-RPC kinds. Without a registration route nothing
//! writes the route table, so the router cannot onboard an actor until a
//! roaming-nest WS-RPC frontend is designed (`nest/worker.md` § `fauna-router`).

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use tracing::{debug, warn};

use crate::backends::{BackendPool, BackendState};
use crate::db::ProxyDb;
use crate::routing::{actor_id_from_json_body, actor_id_from_token};

// ── ProxyState ────────────────────────────────────────────────────────────────

/// Shared state injected into every Axum handler via `State<Arc<ProxyState>>`.
///
/// Registration state is deliberately absent: the posture, capacity and handle
/// domain are client-set nest state read live from `pool` (the
/// `/internal/router-status` poll), and the reserved-handle list is the shared
/// `fauna_protocol::handle::RESERVED_HANDLES` constant. The `handle_domain` /
/// `reserved_handles` fields that used to sit here were fed from the router's
/// own TOML — hand-edited copies of values the router never owned.
pub struct ProxyState {
    pub db: Arc<ProxyDb>,
    pub pool: Arc<BackendPool>,
    pub client: reqwest::Client,
    /// Primary domain served by this proxy (e.g. `fauna.social`) — genuine
    /// artifact wiring (which host this frontend answers as).
    pub domain: String,
}

// ── Hop-by-hop headers that must not be forwarded ─────────────────────────────

static HOP_BY_HOP: &[&str] = &[
    "host",
    "connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "proxy-authorization",
    "proxy-authenticate",
    "keep-alive",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name.to_lowercase().as_str())
}

// ── forward_to_backend ────────────────────────────────────────────────────────

/// Forward an HTTP request to a specific backend and return the response.
///
/// Copies all non-hop-by-hop headers from `headers`.  The body is sent as-is.
pub async fn forward_to_backend(
    backend: &BackendState,
    client: &reqwest::Client,
    method: Method,
    path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let base = BackendPool::backend_url(backend);
    let url = match query {
        Some(q) if !q.is_empty() => format!("{base}{path}?{q}"),
        _ => format!("{base}{path}"),
    };

    debug!("forwarding {} {} → {}", method, path, url);

    let mut req_builder = client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        &url,
    );

    // Copy headers, skipping hop-by-hop.
    for (name, value) in headers {
        if !is_hop_by_hop(name.as_str())
            && let Ok(val_str) = value.to_str()
        {
            req_builder = req_builder.header(name.as_str(), val_str);
        }
    }

    req_builder = req_builder.body(body.to_vec());

    let upstream = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("backend request to {} failed: {}", url, e);
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
        }
    };

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut response_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !is_hop_by_hop(name.as_str())
            && let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_str().as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            )
        {
            response_headers.insert(n, v);
        }
    }

    let resp_body = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!("reading upstream body failed: {}", e);
            return (StatusCode::BAD_GATEWAY, "upstream body error").into_response();
        }
    };

    let mut response = Response::new(Body::from(resp_body));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

// ── proxy_fallback ────────────────────────────────────────────────────────────

/// Catch-all handler.  Resolves the target backend from the actor_id embedded
/// in the request and forwards the request.
pub async fn proxy_fallback(
    State(state): State<Arc<ProxyState>>,
    req: axum::http::Request<Body>,
) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_owned();
    let query = uri.query().map(|s| s.to_owned());
    let headers = req.headers().clone();

    // Block admin routes entirely.
    if path.starts_with("/admin") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    // Buffer the full body (max 50 MB).
    const MAX_BODY: usize = 50 * 1024 * 1024;
    let body_bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
        }
    };

    // ── Determine actor_id ────────────────────────────────────────────────────

    let actor_id: Option<[u8; 32]> = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(actor_id_from_token)
        .or_else(|| extract_actor_from_path(&path))
        .or_else(|| actor_id_from_json_body(&body_bytes));

    // ── Route lookup ──────────────────────────────────────────────────────────

    let backend = match actor_id {
        Some(id) => match state.db.lookup_route(&id) {
            Ok(Some((nest_id_bytes, _handle))) => {
                let nest_arr: [u8; 32] = match nest_id_bytes.try_into() {
                    Ok(a) => a,
                    Err(_) => {
                        return (StatusCode::INTERNAL_SERVER_ERROR, "invalid nest_id in db")
                            .into_response();
                    }
                };
                match state.pool.find_by_nest_id(&nest_arr) {
                    Some(b) => b,
                    None => {
                        return (StatusCode::BAD_GATEWAY, "backend not found for nest")
                            .into_response();
                    }
                }
            }
            Ok(None) => {
                return (StatusCode::NOT_FOUND, "actor not found").into_response();
            }
            Err(e) => {
                warn!("db lookup_route error: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
            }
        },
        None => {
            return (StatusCode::UNAUTHORIZED, "cannot identify actor").into_response();
        }
    };

    // ── Forward the request ───────────────────────────────────────────────────

    forward_to_backend(
        &backend,
        &state.client,
        method,
        &path,
        query.as_deref(),
        &headers,
        body_bytes,
    )
    .await
}

// ── extract_actor_from_path ───────────────────────────────────────────────────

/// Try to extract an actor_id from a URL path.
///
/// Looks at segment index 4 (0-based) in paths of the form
/// `/api/v1/{resource}/{actor_id}` and tries to parse it as a 64-char hex
/// string representing 32 bytes.
///
/// Segment indices (split on `/`):
///   `["", "api", "v1", "{resource}", "{actor_id}", ...]`
///     0     1     2       3               4
pub fn extract_actor_from_path(path: &str) -> Option<[u8; 32]> {
    let segments: Vec<&str> = path.split('/').collect();
    // segments[0] is always "" (leading slash)
    // We want segments[4] which is index 4.
    let candidate = segments.get(4)?;
    if candidate.len() == 64 {
        crate::routing::actor_id_from_hex(candidate)
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod health_poll_tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use fauna_nest::db::CacheDb;
    use fauna_nest::routes::{AppState, RegistrationConfig};
    use fauna_nest::rpc_router::RpcRouter;
    use fauna_nest::state::AuthState;
    use fauna_nest::token_store::TokenStore;
    use fauna_protocol::node_policy::RegistrationMode;

    use crate::backends::BackendPool;
    use crate::config::BackendConfig;

    const DOMAIN: &str = "test.fauna.social";

    /// Spin an in-process backend nest serving the anonymous WS-RPC endpoint
    /// with `fauna.account.register` registered, in registration posture `mode`.
    /// Returns the `http://` base (the anon connector swaps the scheme to
    /// `ws://`).
    async fn start_backend_nest_in_mode(mode: RegistrationMode) -> String {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let state = Arc::new(AppState {
            auth: AuthState {
                token_store: Arc::new(TokenStore::new()),
                registration: RegistrationConfig {
                    handle_domain: Some(DOMAIN.to_string()),
                    reserved_handles: vec![],
                },
                ..Default::default()
            },
            // The posture is the `registration_mode` singleton, not the retired
            // `open` + `invite_required` pair. `for_test` seeds `Closed`, so this
            // must be set explicitly or `fauna.account.register` below is refused.
            registration_mode: Arc::new(tokio::sync::RwLock::new((mode, None))),
            rpc_router: Arc::new({
                let mut b = RpcRouter::builder();
                fauna_nest::account_handlers::register_account_handlers(&mut b);
                b.build()
            }),
            ..AppState::for_test(db.clone())
        });
        let app = fauna_nest::build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    /// Build a pool with one backend pointing at `nest_url`, in the state a
    /// freshly-started router is in: nothing polled, nothing known.
    fn unpolled_pool(nest_url: &str) -> BackendPool {
        let port: u16 = nest_url.rsplit(':').next().unwrap().parse().unwrap();
        BackendPool::from_config(&[BackendConfig {
            name: "nest-a".into(),
            nest_id: "aa".repeat(32),
            tunnel_ip: "127.0.0.1".into(),
            port,
            labels: vec![],
        }])
        .unwrap()
    }

    /// The whole contract, over real HTTP against a real nest: the router starts
    /// knowing nothing, polls `/internal/router-status`, and adopts the capacity
    /// and posture **that nest** reports. No value here was ever configured on
    /// the router — which is what makes the client the only surface that can
    /// change it.
    #[tokio::test]
    async fn the_router_learns_an_open_nests_posture_from_the_health_poll() {
        let nest_url = start_backend_nest_in_mode(RegistrationMode::Open).await;
        let pool = unpolled_pool(&nest_url);
        let backend = &pool.backends[0];

        assert!(!backend.healthy.load(Ordering::Relaxed));
        assert_eq!(
            backend.max_users.load(Ordering::Relaxed),
            0,
            "a router that has not polled yet must claim no capacity"
        );
        assert!(!pool.accepts_registrations());

        BackendPool::health_check(backend, &reqwest::Client::new()).await;

        assert!(
            backend.healthy.load(Ordering::Relaxed),
            "the poll should have succeeded"
        );
        assert!(
            backend.max_users.load(Ordering::Relaxed) > 0,
            "the nest's reported ceiling must be adopted, not the router's guess"
        );
        assert!(
            pool.accepts_registrations(),
            "the nest reported an open posture, so the pool must say open"
        );
    }

    /// The same poll against a *closed* nest: `openRegistrations` follows the
    /// nest's client-set posture, so the router's federation-facing answer can
    /// never contradict the admin's actual choice.
    #[tokio::test]
    async fn the_router_learns_a_closed_nests_posture_from_the_health_poll() {
        let nest_url = start_backend_nest_in_mode(RegistrationMode::Closed).await;
        let pool = unpolled_pool(&nest_url);
        let backend = &pool.backends[0];

        BackendPool::health_check(backend, &reqwest::Client::new()).await;

        assert!(
            backend.healthy.load(Ordering::Relaxed),
            "a closed nest is still healthy"
        );
        assert!(
            backend.max_users.load(Ordering::Relaxed) > 0,
            "a closed nest still reports its capacity"
        );
        assert!(
            !pool.accepts_registrations(),
            "the nest reported a closed posture, so the pool must say closed"
        );
    }
}
