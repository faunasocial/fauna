//! The loopback reverse-proxy pass for `proxy.fauna.social` → the CORS
//! proxy unit. Buffered forwarding with hop-by-hop filtering, mirroring
//! `fauna-router`'s `forward_to_backend`; the CORS proxy's own forwarding
//! semantics (query preservation etc.) are owned by
//! `registry.md` § CORS proxy and pass through here UNMODIFIED.

use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::DoorState;

/// Provider API calls are small JSON/XML; anything bigger is not a
/// legitimate proxy-pass payload.
pub const MAX_PROXY_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Hop-by-hop headers that must not be forwarded in either direction
/// (RFC 9110 § 7.6.1) — plus `host`, which reqwest derives from the
/// upstream URL.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

pub async fn proxy_pass(state: &DoorState, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match to_bytes(body, MAX_PROXY_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };

    let path = parts.uri.path();
    let url = match parts.uri.query() {
        Some(q) if !q.is_empty() => format!("{}{}?{}", state.cfg.proxy_upstream, path, q),
        _ => format!("{}{}", state.cfg.proxy_upstream, path),
    };

    let method = match reqwest::Method::from_bytes(parts.method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };
    let mut builder = state.http.request(method, &url);
    for (name, value) in &parts.headers {
        if !is_hop_by_hop(name.as_str())
            && let Ok(v) = value.to_str()
        {
            builder = builder.header(name.as_str(), v);
        }
    }
    let upstream = match builder.body(bytes.to_vec()).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("proxy upstream {url} failed: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !is_hop_by_hop(name.as_str())
            && let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_str().as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            )
        {
            headers.insert(n, v);
        }
    }
    let body = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("proxy upstream body read failed: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}
