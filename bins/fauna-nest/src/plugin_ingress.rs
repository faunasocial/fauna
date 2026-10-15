//! Nest-terminated **ingress** for hosted plugins — the identity-aware proxy
//! that feeds a plugin's `ingress.handle` (`docs/goal/architecture/security.md`
//! § Co-resident process trust boundary → *Hosted third-party code*;
//! `third-party.md` § The manifest, the `ingress` member, and § The runner
//! contract).
//!
//! Two doors, one proxy. **The path half:** the public router's fallback asks
//! [`proxy_if_routed`] first. A request is proxied when its `Host` is the
//! nest's own (never a user's custom domain or `<handle>` subdomain, so a
//! plugin prefix never shadows a user's published site) and its path falls
//! under a prefix an installed plugin declared. Because this runs in the
//! FALLBACK, a path the nest routes is never reached here at all;
//! [`NEST_RESERVED_INGRESS_PREFIXES`] additionally refuses, at install, a
//! prefix whose subtree the nest serves, so a plugin can never answer for an
//! unrouted path under one either. **The whole-name half:** [`whole_name_gate`]
//! sits AHEAD of every route. A request whose `Host` is `<label>.<apex>` for
//! a label an installed plugin claimed is the plugin's on every path — the
//! nest answers nothing on a claimed name (no API, no `/app/`, no web
//! content), so the name is a whole origin holding no Fauna secret. The SNI
//! router needs no change: its default backend is the nest, and the three
//! labels it routes elsewhere (`mail`, `relay`, `pds`) are reserved labels no
//! document may claim (`fauna_protocol::kind_manifest::parse_ingress`). The
//! name's certificate comes from the per-name web cert reconcile
//! (`web_content::cert`), never from the plugin.
//!
//! What the plugin receives: the method, the full path, the query, the
//! headers minus `authorization` and the hop-by-hop set, a body capped at
//! [`INGRESS_BODY_LIMIT`], and `actor` — the actor the nest authenticated
//! from the request's bearer through the one bearer validator, or none.
//! `actor` is a record field the nest fills, never derived from a header, so
//! no inbound header can claim an identity. A presented bearer that does not
//! validate answers 401 from the nest without reaching the plugin.
//!
//! What the client receives: the plugin's status, body and headers, minus the
//! hop-by-hop set and `set-cookie`, with `X-Content-Type-Options: nosniff`
//! forced. On the nest's own host `Content-Security-Policy: sandbox` is
//! forced too: the plugin answers on the origin where the web app keeps the
//! user's secrets, so whatever it serves renders in an opaque origin and can
//! never script that origin — a hosted plugin is headless (`third-party.md`
//! § The runner contract, TP2). On a claimed whole name no sandbox is forced:
//! the name is its own origin, where the nest serves nothing — the posture a
//! user's `<handle>.<apex>` site already has (`web-content-hosting.md` §
//! Same-origin security model). `set-cookie` stays dropped on both: a plugin
//! is headless, and a `Domain=<apex>` cookie tossed onto the site stays
//! impossible. A fault, or a plugin that is not running, answers 502.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, header};
use axum::middleware::Next;
use fauna_plugin_host::IngressRequest;

use crate::routes::AppState;

/// The path prefixes no plugin may declare: every subtree `build_router`
/// serves (pinned against the router's route literals by
/// `every_routed_path_is_reserved`). Matched as string prefixes in both
/// directions: a declared path under one refuses, and so does one a reserved
/// prefix lies under — and every route literal, `/nostr` and `/app` included,
/// lies under one of these.
pub const NEST_RESERVED_INGRESS_PREFIXES: &[&str] = &[
    "/api/",
    "/oauth/",
    "/.well-known/",
    "/xrpc/",
    "/app/",
    "/share/",
    "/ap/",
    "/nostr/",
    "/nodeinfo/",
    "/caldav/",
    "/carddav/",
    "/list/",
    "/internal/",
    "/health/",
    "/version/",
];

/// The largest request body the proxy hands a plugin — the public router's
/// own body limit.
pub const INGRESS_BODY_LIMIT: usize = 10 * 1024 * 1024;

/// The reserved prefix `path` collides with, if any.
#[must_use]
pub fn reserved_prefix_clash(path: &str) -> Option<&'static str> {
    NEST_RESERVED_INGRESS_PREFIXES
        .iter()
        .copied()
        .find(|r| path.starts_with(r) || r.starts_with(path))
}

/// The hop-by-hop headers (RFC 9110 § 7.6.1) a proxy never forwards, in
/// either direction.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The forwardable headers of `headers`: neither hop-by-hop, nor named by
/// `Connection`, nor in `drop`.
fn forwardable<'a>(
    headers: &'a HeaderMap,
    drop: &'a [HeaderName],
) -> impl Iterator<Item = (&'a HeaderName, &'a HeaderValue)> {
    let named: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .collect();
    headers.iter().filter(move |(name, _)| {
        let n = name.as_str();
        !HOP_BY_HOP.contains(&n) && !named.iter().any(|c| c == n) && !drop.contains(name)
    })
}

/// Is `host` (a `Host` header value) the nest's own? With no resolver (a
/// nest without web hosting) every host is. With one: the nest's apex
/// domain, or — on a domainless box — any host that is not a registered
/// custom domain. A `<handle>` subdomain, a custom domain, and the reserved
/// `mail.`/`app.`/`mta-sts.` hosts never are.
async fn is_nest_host(state: &AppState, host: &str) -> bool {
    match &state.host_resolver {
        None => true,
        Some(resolver) => resolver.is_nest_host(host).await,
    }
}

/// Proxy `req` to the plugin whose declared prefix its path falls under, or
/// hand it back when none does (or the host is not the nest's own).
pub async fn proxy_if_routed(
    state: &Arc<AppState>,
    req: Request<Body>,
) -> Result<Response<Body>, Request<Body>> {
    let Some(principal_id) = state.plugins.route(req.uri().path()) else {
        return Err(req);
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !is_nest_host(state, host).await {
        return Err(req);
    }
    Ok(proxy(state, &principal_id, req, Origin::NestHost).await)
}

/// The one DNS label under `apex` that `host` (a `Host` header value) names
/// — `matrix` for `Matrix.Nest.Example:443` under `nest.example` — or `None`
/// when `host` is the apex itself, not under it, more than one label deep,
/// or when the box has no apex. Normalized as the host resolver normalizes:
/// port stripped, lowercased, trailing dot trimmed.
#[must_use]
pub fn plugin_host_label(host: &str, apex: &str) -> Option<String> {
    if apex.is_empty() {
        return None;
    }
    let host = host.split(':').next().unwrap_or(host);
    let host = fauna_core::web::normalize_dns_name(host);
    let label = host.strip_suffix(&format!(".{apex}"))?;
    (!label.is_empty() && !label.contains('.')).then(|| label.to_string())
}

/// The whole-name door, layered ahead of every route in `build_router`: a
/// request whose `Host` is a name an installed plugin claimed is that
/// plugin's whatever its path; everything else passes to the router.
pub async fn whole_name_gate(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let claimed = state.host_resolver.as_ref().and_then(|resolver| {
        let host = req.headers().get(header::HOST)?.to_str().ok()?;
        let label = plugin_host_label(host, &resolver.nest_domain())?;
        state.plugins.route_host(&label)
    });
    match claimed {
        Some(principal_id) => proxy(&state, &principal_id, req, Origin::OwnName).await,
        None => next.run(req).await,
    }
}

/// Which origin a proxied reply lands on, which decides the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The nest's own host (the path half): `Content-Security-Policy:
    /// sandbox` is forced.
    NestHost,
    /// A whole name the plugin claimed: its own origin, no forced sandbox.
    OwnName,
}

fn status_only(status: StatusCode) -> Response<Body> {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = status;
    r
}

async fn proxy(
    state: &Arc<AppState>,
    principal_id: &[u8],
    req: Request<Body>,
    origin: Origin,
) -> Response<Body> {
    let (parts, body) = req.into_parts();
    // The identity assertion: the bearer through the one validator, or none.
    let actor = match crate::auth::bearer_token_from_headers(&parts.headers) {
        None => None,
        Some(token) => match crate::auth::validate_bearer(state, token).await {
            Some(actor) => Some(actor.0),
            None => return status_only(StatusCode::UNAUTHORIZED),
        },
    };
    let Ok(body) = axum::body::to_bytes(body, INGRESS_BODY_LIMIT).await else {
        return status_only(StatusCode::PAYLOAD_TOO_LARGE);
    };
    let headers = forwardable(&parts.headers, &[header::AUTHORIZATION])
        .filter_map(|(n, v)| Some((n.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    let request = IngressRequest {
        method: parts.method.as_str().to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(str::to_string),
        headers,
        body: body.to_vec(),
        actor,
    };
    let answer = match state.plugins.ingress(principal_id, request).await {
        Ok(a) => a,
        Err(why) => {
            tracing::info!(
                principal = %hex::encode(principal_id),
                "plugin ingress answered 502: {why}"
            );
            return status_only(StatusCode::BAD_GATEWAY);
        }
    };
    let Ok(status) = StatusCode::from_u16(answer.status) else {
        return status_only(StatusCode::BAD_GATEWAY);
    };
    let mut from_plugin = HeaderMap::new();
    for (name, value) in &answer.headers {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            from_plugin.append(n, v);
        }
    }
    let mut response = Response::new(Body::from(answer.body));
    *response.status_mut() = status;
    let out = response.headers_mut();
    let dropped: &[HeaderName] = match origin {
        Origin::NestHost => &[
            header::SET_COOKIE,
            header::CONTENT_LENGTH,
            header::CONTENT_SECURITY_POLICY,
            header::X_CONTENT_TYPE_OPTIONS,
        ],
        Origin::OwnName => &[
            header::SET_COOKIE,
            header::CONTENT_LENGTH,
            header::X_CONTENT_TYPE_OPTIONS,
        ],
    };
    for (n, v) in forwardable(&from_plugin, dropped) {
        out.append(n.clone(), v.clone());
    }
    if origin == Origin::NestHost {
        out.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox"),
        );
    }
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Every path literal any `src/` file registers with `.route(…)` — the
    /// routers `build_router` assembles — lies under a reserved prefix (or is
    /// the root, which no ingress path can be), so no plugin can declare a
    /// prefix the nest serves. A new route family fails here until its
    /// prefix joins [`NEST_RESERVED_INGRESS_PREFIXES`].
    #[test]
    fn every_routed_path_is_reserved() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        // Built from halves so this file's own source never matches it.
        let needle = format!("{}{}", ".route", "(\"");
        let mut unreserved = Vec::new();
        let mut seen = 0;
        for path in files {
            let text = std::fs::read_to_string(&path).expect("source is utf-8");
            let squashed: String = text.split_whitespace().collect();
            for (at, _) in squashed.match_indices(needle.as_str()) {
                let rest = &squashed[at + needle.len()..];
                let literal = &rest[..rest.find('"').expect("a closed literal")];
                seen += 1;
                if literal != "/" && reserved_prefix_clash(literal).is_none() {
                    unreserved.push(format!("{}: {literal}", path.display()));
                }
            }
        }
        assert!(seen > 50, "the walk found the router's routes ({seen})");
        assert!(
            unreserved.is_empty(),
            "routed paths a plugin could claim: {unreserved:#?}"
        );
        // The SPA mount and the issuer plane are registered without a literal
        // `.route` (a `nest_service`, constants); pinned by name.
        for served in ["/app", "/app/index.html", "/oauth/par", "/oauth/token"] {
            assert!(reserved_prefix_clash(served).is_some(), "{served}");
        }
    }

    /// The whole-name door keys on exactly one label under the live apex,
    /// normalized as the host resolver normalizes; the apex itself, a deeper
    /// name, a foreign name and a domainless box all pass to the router.
    #[test]
    fn a_whole_name_is_one_label_under_the_apex() {
        let label = |host: &str| plugin_host_label(host, "nest.example");
        assert_eq!(label("matrix.nest.example").as_deref(), Some("matrix"));
        assert_eq!(label("Matrix.Nest.Example:443").as_deref(), Some("matrix"));
        assert_eq!(label("matrix.nest.example.").as_deref(), Some("matrix"));
        assert_eq!(label("nest.example"), None);
        assert_eq!(label(".nest.example"), None);
        assert_eq!(label("a.matrix.nest.example"), None);
        assert_eq!(label("matrix.other.example"), None);
        assert_eq!(label("matrixnest.example"), None);
        assert_eq!(label(""), None);
        assert_eq!(plugin_host_label("matrix.nest.example", ""), None);
    }

    #[test]
    fn a_prefix_clashes_in_both_directions() {
        assert_eq!(reserved_prefix_clash("/api/"), Some("/api/"));
        assert_eq!(reserved_prefix_clash("/api/v2/"), Some("/api/"));
        assert_eq!(reserved_prefix_clash("/nostr/"), Some("/nostr/"));
        assert_eq!(reserved_prefix_clash("/nostr"), Some("/nostr/"));
        assert_eq!(reserved_prefix_clash("/nostrich/"), None);
        assert_eq!(reserved_prefix_clash("/application/"), None);
        assert_eq!(reserved_prefix_clash("/matrix/"), None);
        assert_eq!(reserved_prefix_clash("/_matrix/"), None);
    }

    #[test]
    fn hop_by_hop_and_connection_named_headers_never_cross() {
        let mut h = HeaderMap::new();
        h.insert(header::CONNECTION, HeaderValue::from_static("x-secret"));
        h.insert("x-secret", HeaderValue::from_static("1"));
        h.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t"));
        h.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        let kept: Vec<&str> = forwardable(&h, &[header::AUTHORIZATION])
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(kept, ["accept"]);
    }
}
