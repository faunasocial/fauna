//! The nest's OAuth issuer **surfaces**: `/oauth/jwks` and the two discovery
//! documents (TP5 slice S1, part 2 — the readers of the key plane part 1 built).
//!
//! `docs/goal/behavior/authorization-server.md` § The issuer rules that the
//! authorization server is "a nest-native surface on the nest's own domain" and
//! "is up whenever the nest is up", and
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
//! infrastructure → *Issuer signing key* rules that the public half "is served
//! at `/oauth/jwks` as a key **set** with `kid`s". These routes mount
//! unconditionally, in every nest flavor, for the same reason
//! [`crate::oauth_issuer_key`] carries no cargo feature: the issuer exists so it
//! no longer depends on an optional bridge being enrolled.
//!
//! # The issuer identity, and the domainless nest
//!
//! The issuer **is** the nest's apex domain ([`AppState::web_serving_domain`]),
//! which is empty on a box that has not claimed one. An issuer identity is not
//! something a nest can invent: `iss` is a security-relevant, client-pinned
//! value, and advertising `https://` + a guess (an IP, `localhost`) would mint
//! tokens under an identity that stops being true the moment a domain is
//! claimed — every one of them then failing verification, with nothing to
//! re-point them to.
//!
//! So a domainless nest answers **`503` until a domain is claimed**, exactly as
//! `/.well-known/carddav` answers `503` until there is an MDA host to send a
//! client to (`crate::well_known_dav`). This is the same "the surface is real,
//! its prerequisite is not here yet" shape, and it recovers by itself: a
//! post-boot claim re-points the live apex through
//! `identity_domain_core::apply_primary_identity`, so these routes start
//! answering with no restart — the property `web_serving_domain`'s own contract
//! guarantees.
//!
//! ⚠ The goal docs did not decide this case; it was decided here from that
//! prior art and recorded in `authorization-server.md` § The issuer in the same
//! commit.
//!
//! # What this advertises
//!
//! **Only what the nest actually serves**, which is the rule that governed the
//! whole port: a discovery document naming an endpoint that answers `404` is
//! worse than one that omits it, because a client reading `token_endpoint` and
//! getting a 404 cannot tell a broken deployment from an unfinished one. The
//! document is the shared
//! [`fauna_bridge_atproto::oauth_metadata::oauth_authorization_server_document`]
//! over this nest's apex, and every endpoint it names answers here
//! ([`crate::oauth_as_routes`]).
//!
//! "Serves" means the token is **honoured**, not merely that the route answers
//! (`authorization-server.md` § The issuer → *The staging rule covers both
//! documents, and one pin enforces it*). That holds because the deployment's
//! resource server — the PDS bridge — sends clients here: its protected-resource
//! document names this issuer
//! ([`fauna_bridge_atproto::oauth_metadata::protected_resource_authorization_servers`]),
//! and it verifies against the issuer and key set this nest feeds it
//! (`fauna.bridges.atproto.fetch_issuer_jwks`). The staging gate that withheld
//! the flow until that re-point retired with it, in the same change that took
//! the bridge's own authorization server down.
//!
//! `userinfo_endpoint` joined with OIDC (TP6), in the change that mounted
//! [`crate::oauth_as_oidc::oauth_userinfo`] — the day it answered, like every
//! member before it. The device grant (TP9) is still absent, honestly so.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{
        HeaderName, Method,
        header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE},
    },
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

use crate::api_error::ApiError;
use crate::oauth_as_error::{DPOP_NONCE_HEADER, DPOP_PROOF_HEADER};
use crate::routes::AppState;

/// The issuer's whole HTTP plane as ONE router: the three read surfaces this
/// module serves (the two discovery documents and the JWKS) and the
/// request-taking endpoints of [`crate::oauth_as_routes`], under the plane's one
/// CORS posture ([`cors_layer`]).
///
/// Mounted **unconditionally, in every flavor**: `authorization-server.md` § The
/// issuer rules the AS "is up whenever the nest is up", and the whole point of
/// moving the issuer to nest custody was that it stops depending on an optional
/// bridge being enrolled. A domainless nest answers `503` on every surface (the
/// module docs own why, and the carddav prior art it follows). The
/// request-taking half is mounted on the same terms and for the same reason:
/// this is the deployment's only authorization server — the PDS's
/// protected-resource document names this issuer, and the bridge's own
/// `/oauth/*` retired in the change that re-pointed it (`authorization-server.md`
/// § The issuer → *The re-point, the teaching, and the bridge AS's retirement are
/// ONE change*).
///
/// ⚠ **Merge this into the main router AFTER that router's credentialed
/// `.layer(cors)`** — the way `lib.rs`'s `public_bytes` is — never before. A
/// router carrying its own CORS layer that is then merged *under* a second one
/// answers with two `Access-Control-Allow-Origin` headers (`*` and the echoed
/// app origin), which a browser treats as no header at all, and the plane would
/// be shut to exactly the browser client it was opened for.
pub fn routes(limiter: Arc<crate::oauth_as_rate_limit::EndpointLimiter>) -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(wellknown_oauth_authorization_server),
        )
        .route(
            "/.well-known/openid-configuration",
            get(wellknown_openid_configuration),
        )
        .route("/oauth/jwks", get(oauth_jwks))
        .merge(crate::oauth_as_routes::routes(limiter))
        .layer(cors_layer())
}

/// The issuer plane's CORS posture — **open to every origin, with no
/// credentials** (`authorization-server.md` § The issuer → *Cross-origin
/// access*).
///
/// Three facts make the open posture the correct one rather than a lenient one:
///
/// 1. **A browser client's origin is unknowable ahead of time.** A website that
///    offers "sign in with Fauna" is identified by its client metadata document,
///    resolved at PAR — never by an admin registering its origin. An origin
///    allowlist for the issuer would be a second registration of the same fact,
///    and a knob no admin should hold (`principles.md` § One configuration
///    surface). The nest's credentialed allowlist (`lib.rs`'s `cors`) is for the
///    nest's OWN app origins, and stays exactly as it is for every other route.
/// 2. **Nothing on this plane is authenticated by an ambient credential.** No
///    cookie, no nest bearer: every request is self-authenticating (a DPoP
///    proof, a PKCE verifier, a `private_key_jwt` assertion, an access token in
///    `Authorization`), so a foreign page can do nothing here that `curl` cannot
///    — the same reasoning that opened the content-addressed byte routes. Hence
///    no `Access-Control-Allow-Credentials`, which a wildcard origin forbids
///    anyway.
/// 3. **The plane's own headers must cross the origin boundary.** `DPoP` is a
///    request header a browser may only send after a preflight has allowed it,
///    and `DPoP-Nonce` / `WWW-Authenticate` are response headers a page can
///    only read when exposed — the nonce is what a browser client dials its
///    principal session with (`transport-connection.md` § Connection lifecycle →
///    *The principal session*), and the challenge is how it learns that it
///    needs one.
///
/// GET and POST are the plane's only methods (the discovery reads, the form
/// posts, `userinfo`'s two verbs). The header names go through the one spelling
/// each already has (`oauth_as_error`); `HeaderName::from_bytes` lower-cases
/// them, as HTTP header names are case-insensitive.
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    let header = |name: &str| {
        HeaderName::from_bytes(name.as_bytes())
            .expect("a header name constant is a valid header name")
    };
    tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([CONTENT_TYPE, AUTHORIZATION, header(DPOP_PROOF_HEADER)])
        .expose_headers([header(DPOP_NONCE_HEADER), WWW_AUTHENTICATE])
}

/// The issuer identifier — `https://<apex>` — or `None` on a domainless box.
///
/// Read live, never cached: a claim after boot must take effect without a
/// restart (see the module docs).
///
/// ⚠ Rendered by [`fauna_bridge_atproto::oauth_metadata::oauth_issuer`], never
/// by a `format!` here. That builder's own contract is that this exact string
/// is compared for equality by clients in five places — the `iss` of every
/// token, the `issuer` of this document, the `aud` a client assertion must
/// carry, the entry in the protected resource's `authorization_servers`, and
/// (per RFC 8414) the origin the `.well-known` URL was fetched from. A second
/// spelling anywhere is a mismatch with nothing visibly wrong on either side.
pub(crate) fn issuer(state: &AppState) -> Option<String> {
    apex(state).map(fauna_bridge_atproto::oauth_metadata::oauth_issuer)
}

/// The apex domain itself — the host half of [`issuer`], `None` on a domainless
/// box. The shared document builder takes the apex, not the rendered issuer.
fn apex(state: &AppState) -> Option<String> {
    let domain = state.web_serving_domain();
    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

/// The `503` a domainless nest answers with, on every issuer surface.
pub(crate) fn no_issuer_yet() -> Response {
    ApiError::service_unavailable(
        "this nest has not claimed a domain yet, so it has no issuer identity to advertise"
            .to_string(),
    )
    .into_response()
}

/// `GET /oauth/jwks` — the issuer key **set**, active key first.
///
/// A lookup, never a mint: the active signer is seated at boot, before the nest
/// serves, so a fresh nest serves a usable JWKS — and a nest whose boot step
/// could not seat one answers an error rather than an empty set that a client
/// would cache as "this issuer has no keys". The same read is where a retired
/// key past its horizon stops being served —
/// [`crate::oauth_issuer_key::serve_key_set`] owns both, and says why the
/// horizon is applied lazily here rather than on a timer.
pub async fn oauth_jwks(State(state): State<Arc<AppState>>) -> Response {
    if issuer(&state).is_none() {
        return no_issuer_yet();
    }

    let db = state.db.clone();
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let keys = tokio::task::spawn_blocking(move || {
        crate::oauth_issuer_key::serve_key_set(&db.conn_blocking(), now)
    })
    .await;

    let keys = match keys {
        Ok(Ok(keys)) => keys,
        Ok(Err(e)) => {
            return ApiError::internal(format!("read issuer key set: {e}")).into_response();
        }
        Err(e) => return ApiError::internal(format!("issuer key set task: {e}")).into_response(),
    };

    let jwks: Vec<Value> = keys
        .iter()
        .map(|k| {
            json!({
                "kty": "EC",
                "crv": "P-256",
                "alg": "ES256",
                "use": "sig",
                "kid": k.kid,
                "x": k.x,
                "y": k.y,
            })
        })
        .collect();
    Json(json!({ "keys": jwks })).into_response()
}

/// The discovery document both `.well-known` paths serve, for an apex domain
/// this nest has claimed.
///
/// One builder, two mounts: RFC 8414's `oauth-authorization-server` and OIDC
/// Discovery's `openid-configuration` describe the same authorization server,
/// and serving two documents that could drift is how a client ends up pinning
/// one identity while the other says something else. They differ only when a
/// key is defined by exactly one of the specs — none is yet.
///
/// Rendered by the shared owner, which is where the reasoning for every member
/// lives (`require_pushed_authorization_requests`, S256-only PKCE, the two
/// client-authentication methods and why `revocation_endpoint_auth_methods_supported`
/// is required rather than decorative). It is a string because the owner
/// renders wire vocabulary once and every server writes the bytes.
fn discovery_document(apex: &str) -> String {
    fauna_bridge_atproto::oauth_metadata::oauth_authorization_server_document(apex.to_string())
}

/// Serve [`discovery_document`], or the domainless `503`.
fn discovery_response(state: &AppState) -> Response {
    match apex(state) {
        Some(apex) => (
            [(CONTENT_TYPE, "application/json")],
            discovery_document(&apex),
        )
            .into_response(),
        None => no_issuer_yet(),
    }
}

/// `GET /.well-known/oauth-authorization-server` — RFC 8414 discovery.
pub async fn wellknown_oauth_authorization_server(State(state): State<Arc<AppState>>) -> Response {
    discovery_response(&state)
}

/// `GET /.well-known/openid-configuration` — OIDC Discovery, same document.
pub async fn wellknown_openid_configuration(State(state): State<Arc<AppState>>) -> Response {
    discovery_response(&state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(apex: &str) -> Value {
        serde_json::from_str(&discovery_document(apex)).expect("the document is JSON")
    }

    /// Every advertised URL hangs off the issuer, so a nest that later claims a
    /// different domain advertises the new one everywhere at once — there is no
    /// second place a stale host could survive.
    #[test]
    fn the_document_advertises_only_issuer_rooted_urls() {
        let doc = doc("nest.example");
        assert_eq!(doc["issuer"], "https://nest.example");
        assert_eq!(doc["jwks_uri"], "https://nest.example/oauth/jwks");
    }

    /// The document advertises what the nest SERVES and nothing more: every
    /// endpoint member it names is a route this nest mounts. `userinfo_endpoint`
    /// joined with OIDC and the two consent starts with TP9, each in the change
    /// that mounted it — before then, appearing here would have sent clients to
    /// a `404` they cannot distinguish from a broken nest.
    #[test]
    fn the_document_advertises_no_endpoint_the_nest_does_not_serve() {
        let doc = doc("nest.example");
        let obj = doc.as_object().expect("object");
        let mounted = [
            "pushed_authorization_request_endpoint",
            "authorization_endpoint",
            "token_endpoint",
            "revocation_endpoint",
            "userinfo_endpoint",
            "device_authorization_endpoint",
            "backchannel_authentication_endpoint",
        ];
        for member in obj.keys().filter(|k| k.ends_with("_endpoint")) {
            assert!(
                mounted.contains(&member.as_str()),
                "advertised {member}, which no nest route answers"
            );
        }
    }

    /// The seven endpoint members are the routes this nest mounts, on this
    /// nest's host. Pinned as a set rather than one assertion each, so a later
    /// edit cannot drop one and stay green.
    #[test]
    fn the_seven_endpoints_are_advertised_on_this_nest() {
        let doc = doc("nest.example");
        for (member, path) in [
            ("pushed_authorization_request_endpoint", "/oauth/par"),
            ("authorization_endpoint", "/oauth/authorize"),
            ("token_endpoint", "/oauth/token"),
            ("revocation_endpoint", "/oauth/revoke"),
            ("userinfo_endpoint", "/oauth/userinfo"),
            (
                "device_authorization_endpoint",
                "/oauth/device_authorization",
            ),
            ("backchannel_authentication_endpoint", "/oauth/bc-authorize"),
        ] {
            assert_eq!(
                doc[member],
                format!("https://nest.example{path}"),
                "{member} is missing or does not hang off the issuer"
            );
        }
    }

    /// **Serves means honoured**: the deployment's resource server sends
    /// clients to exactly the issuer this document names, so a client that
    /// completes the ceremony holds a token the PDS accepts
    /// (`authorization-server.md` § The issuer → *The staging rule covers both
    /// documents*). Red-verify by making the pin name the PDS host again.
    #[test]
    fn the_resource_server_sends_clients_to_this_issuer() {
        let apex = "nest.example";
        assert_eq!(
            fauna_bridge_atproto::oauth_metadata::protected_resource_authorization_servers(apex),
            vec![doc(apex)["issuer"].as_str().expect("issuer").to_string()]
        );
    }

    /// The scope list is the shared owner's, not a copy: a client's grant may
    /// never be wider than what this document advertises, so the two lists
    /// being the same list is the property, not their happening to match.
    #[test]
    fn the_advertised_scopes_are_the_shared_owners() {
        assert_eq!(
            doc("nest.example")["scopes_supported"],
            json!(fauna_bridge_atproto::oauth_metadata::advertised_scopes())
        );
    }
}
