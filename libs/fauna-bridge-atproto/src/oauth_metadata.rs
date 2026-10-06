//! The two OAuth discovery documents a deployment publishes, plus the paths
//! they name (`atproto-pds-full.md` § F4 detail, § Ecosystem reality item 3;
//! `authorization-server.md` § The issuer).
//!
//! # Why the bodies are rendered here and cross to their servers as strings
//!
//! Every field name in these documents is **wire vocabulary**: a client reads
//! `require_pushed_authorization_requests` and decides whether to send a PAR
//! request. Handing a server a struct to serialize would put those names back
//! on its side, where they can drift from the policy that produced them — the
//! drift `appview_service_did()` was introduced to prevent (§ F3 detail, shape
//! (1)). So the documents are built and rendered to JSON *here*, beside
//! [`authz`], and each server's job is to write the bytes with a content type.
//!
//! [`authz`]: crate::authz
//!
//! # Two hosts, one issuer (TP5)
//!
//! The authorization server is the **nest's**, on the apex domain
//! (`https://<apex>`); the resource server is the PDS bridge, on `pds.<apex>`.
//! So the two documents hang off different hosts:
//!
//! * `/.well-known/oauth-protected-resource` — served by the PDS bridge
//!   ([`oauth_protected_resource_document`]); its `resource` is the PDS and its
//!   `authorization_servers` is the nest's issuer
//!   ([`protected_resource_authorization_servers`]).
//! * `/.well-known/oauth-authorization-server` and `openid-configuration` —
//!   served by the nest ([`oauth_authorization_server_document`]), every
//!   endpoint URL hanging off the issuer.
//!
//! The bridge-hosted AS this module first described (F4 slices 2–8d, all on
//! the PDS host) retired in the same change that re-pointed the protected-
//! resource document at the nest: the re-point, the resource server's teaching
//! and that retirement are ONE change (`authorization-server.md` § The issuer →
//! *The re-point, the teaching, and the bridge AS's retirement are ONE
//! change*). Every endpoint the AS document advertises has a nest route
//! (`bins/fauna-nest/src/oauth_as_routes.rs`).

use serde::Serialize;

use crate::authz::{GRANTABLE_SCOPES, OIDC_SCOPES};
use crate::fauna_scope::FaunaScopeArm;

// ── The AS endpoint paths ────────────────────────────────────────────────────
//
// One spelling per path, consumed by the AS document below AND by the nest's
// route table, so a route can never live at a path the document does not name.

/// Pushed authorization requests (RFC 9126) — mandatory in ATProto OAuth.
pub const PATH_PAR: &str = "/oauth/par";
/// The consent page (D3 rung 2 — the binding-code ceremony).
pub const PATH_AUTHORIZE: &str = "/oauth/authorize";
/// The consent page's own long-poll endpoint — not a spec endpoint, and never
/// advertised in any document: the page's inline script is its only caller, so
/// the browser learns it from the page and nothing else. It lives beside
/// [`PATH_AUTHORIZE`] because it is the same surface's second half, and here in
/// shared Rust for the same one-spelling reason every other path is.
pub const PATH_AUTHORIZE_POLL: &str = "/oauth/authorize/poll";
/// Token exchange and refresh.
pub const PATH_TOKEN: &str = "/oauth/token";
/// Token revocation (RFC 7009).
pub const PATH_REVOKE: &str = "/oauth/revoke";
/// The public JWKS the ecosystem verifies our tokens against.
pub const PATH_JWKS: &str = "/oauth/jwks";
/// OIDC's UserInfo endpoint (TP6) — the claims an `openid` grant's access
/// token may read, behind that token and its DPoP proof.
pub const PATH_USERINFO: &str = "/oauth/userinfo";
/// The principal session's WS-RPC upgrade — the nest as a resource server for
/// the Fauna scope family (`transport-connection.md` § Connection lifecycle →
/// *The principal session*). Not an AS endpoint and advertised in no document;
/// it lives here because its DPoP `htu` is built beside the others, from one
/// spelling the route table shares.
pub const PATH_PRINCIPAL_WS: &str = "/api/v1/principal/ws";
/// The folder deposit door for a remote principal (`file-sync.md`
/// § Third-party deposit ingress; HTTP residue per `api-layers.md`): the
/// route pattern; [`folder_deposit_htu`] spells one folder's URL.
pub const PATH_FOLDER_DEPOSIT: &str = "/api/v1/folders/{id}/deposit";
/// The record door's walk for a remote principal (`third-party-kinds.md`
/// § The record doors; HTTP residue per `api-layers.md`): one `ext.*` kind's
/// rows, a page per cursor. [`records_htu`] spells one kind's URL.
pub const PATH_RECORDS: &str = "/api/v1/records/{kind}";
/// The record door's one-item form: `{key}` is the row's 32-byte blinded
/// `item_key` in hex — the nest never holds a logical key.
pub const PATH_RECORD: &str = "/api/v1/records/{kind}/{key}";
/// The events door for a remote principal (`transport.md` § Push events →
/// *Third-party event doors*; HTTP residue per `api-layers.md`): the
/// `fauna.events.poll` long-poll, `GET` with `?cursor=` and `?wait=`.
pub const PATH_EVENTS: &str = "/api/v1/events";
/// The typed-code consent start — RFC 8628's device authorization endpoint
/// (`authorization-server.md` § Consent).
pub const PATH_DEVICE_AUTHORIZATION: &str = "/oauth/device_authorization";
/// The quiet-push consent start — CIBA's backchannel authentication endpoint,
/// poll mode only (`authorization-server.md` § Consent).
pub const PATH_BC_AUTHORIZE: &str = "/oauth/bc-authorize";

/// RFC 8628 §3.4's grant type — how a typed-code start redeems at
/// `/oauth/token`. One spelling, shared by the document's
/// `grant_types_supported` and the token endpoint's dispatch.
pub const GRANT_TYPE_DEVICE_CODE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// CIBA Core §10.1's grant type — how a quiet-push start redeems at
/// `/oauth/token`.
pub const GRANT_TYPE_CIBA: &str = "urn:openid:params:grant-type:ciba";
/// The same-device handoff's grant type (`authorization-server.md` § Consent →
/// *How the same-device handoff is built*) — how a device app polls with the
/// `request_uri` its pushed request was given, once the user's own Fauna app
/// has opened it. Fauna's own URN: no registered grant polls a PAR handle.
pub const GRANT_TYPE_HANDOFF: &str = "urn:fauna:params:grant-type:handoff";

/// The absolute URL pushed authorization requests are served at.
///
/// One builder because a DPoP proof's `htu` claim must equal the endpoint's URL
/// (RFC 9449 §4.3), and the URL a conformant client puts there is the one it
/// read out of *our* AS document — so the string the proof is checked against
/// and the string the document advertises must come from one builder. Written
/// as `endpoint(PATH_PAR)` is written, against the same [`issuer`], because a
/// second spelling of "this endpoint's URL" is a mismatch that would refuse
/// every proof from every correctly-implemented client, with nothing visibly
/// wrong on either side.
pub fn oauth_par_endpoint_url(apex_domain: String) -> String {
    format!("{}{PATH_PAR}", issuer(&apex_domain))
}

/// The absolute URL the token endpoint is served at.
///
/// One builder for the same reason as [`oauth_par_endpoint_url`], one endpoint
/// over: a DPoP proof's `htu` must equal the URL the client read out of our AS
/// document, so the string the token endpoint checks proofs against and the
/// string the document advertises come from one builder.
pub fn oauth_token_endpoint_url(apex_domain: String) -> String {
    format!("{}{PATH_TOKEN}", issuer(&apex_domain))
}

/// The absolute URL the revocation endpoint is served at.
///
/// One builder for the same reason as [`oauth_par_endpoint_url`]: revocation
/// carries a DPoP proof (§ F4 detail — possession is what stands in for client
/// authentication at a public client), so its `htu` is checked against the URL
/// the client read out of our AS document, from this one builder.
pub fn oauth_revoke_endpoint_url(apex_domain: String) -> String {
    format!("{}{PATH_REVOKE}", issuer(&apex_domain))
}

/// The absolute URL the UserInfo endpoint is served at.
///
/// One builder for the same reason as [`oauth_par_endpoint_url`]: a UserInfo
/// request carries a DPoP proof whose `htu` is checked against the URL the
/// client read out of our discovery document, from this one builder.
pub fn oauth_userinfo_endpoint_url(apex_domain: String) -> String {
    format!("{}{PATH_USERINFO}", issuer(&apex_domain))
}

/// The principal session upgrade's URL in its `https` form — the `htu` its
/// DPoP proof must name, whatever scheme (`wss`) the socket was dialled on
/// (`transport-connection.md` § *The principal session*).
pub fn principal_ws_htu(apex_domain: &str) -> String {
    format!("{}{PATH_PRINCIPAL_WS}", issuer(apex_domain))
}

/// One folder's deposit door URL in its `https` form — the `htu` its DPoP
/// proof must name.
pub fn folder_deposit_htu(apex_domain: &str, folder_id: i64) -> String {
    format!(
        "{}{}",
        issuer(apex_domain),
        PATH_FOLDER_DEPOSIT.replace("{id}", &folder_id.to_string())
    )
}

/// The record door's URL in its `https` form — the `htu` its DPoP proof must
/// name: the kind's walk, or with `item_key_hex` one item of it.
pub fn records_htu(apex_domain: &str, kind: &str, item_key_hex: Option<&str>) -> String {
    let path = match item_key_hex {
        Some(key) => PATH_RECORD.replace("{kind}", kind).replace("{key}", key),
        None => PATH_RECORDS.replace("{kind}", kind),
    };
    format!("{}{path}", issuer(apex_domain))
}

/// The events door's URL in its `https` form — the `htu` its DPoP proof must
/// name.
pub fn events_htu(apex_domain: &str) -> String {
    format!("{}{PATH_EVENTS}", issuer(apex_domain))
}

/// Every scope the **authorization server** advertises — the ATProto family's
/// [`GRANTABLE_SCOPES`], then the fixed OIDC family ([`OIDC_SCOPES`]), then
/// the Fauna family's built arms ([`FaunaScopeArm::ALL`]).
///
/// The protected-resource document keeps [`GRANTABLE_SCOPES`] alone: it
/// describes the PDS, which honours neither an OIDC nor a Fauna-family scope.
#[must_use]
pub fn advertised_scopes() -> Vec<String> {
    GRANTABLE_SCOPES
        .iter()
        .chain(OIDC_SCOPES)
        .copied()
        .chain(
            FaunaScopeArm::ALL
                .iter()
                .flat_map(|arm| arm.advertised().iter().copied()),
        )
        .map(str::to_string)
        .collect()
}

/// The absolute URL the typed-code start is served at — the `htu` its DPoP
/// proof is checked against, from the builder the document advertises it with.
pub fn oauth_device_authorization_endpoint_url(apex_domain: String) -> String {
    format!("{}{PATH_DEVICE_AUTHORIZATION}", issuer(&apex_domain))
}

/// The absolute URL the quiet-push start is served at — the `htu` its DPoP
/// proof is checked against, from the builder the document advertises it with.
pub fn oauth_bc_authorize_endpoint_url(apex_domain: String) -> String {
    format!("{}{PATH_BC_AUTHORIZE}", issuer(&apex_domain))
}

/// The issuer identifier for a host — its origin, scheme included, with no path
/// and no trailing slash. The nest's issuer is this over its apex domain.
///
/// This exact string is compared for equality by clients — it is the `iss` of
/// every access token, the `issuer` of the AS document, the entry in the
/// protected resource's `authorization_servers`, and (per RFC 8414) the origin
/// the `.well-known` URL was fetched from. One builder, so those cannot
/// disagree.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn oauth_issuer(host: String) -> String {
    issuer(&host)
}

/// The origin of a host, for callers inside this crate.
fn issuer(host: &str) -> String {
    format!("https://{host}")
}

/// The PDS host of a deployment, from its apex domain: `pds.<apex>`.
///
/// The dedicated subdomain the PDS serves on — the same hostname the nest
/// returns as `pds_endpoint` and the SNI router routes there
/// (`atproto-pds-full.md` § Wire & process topology).
///
/// It lives here because [`pds_service_did`] below is derived from it and the
/// nest and the bridge must agree on that DID byte for byte; the bridge reads
/// both through [`atproto_pds_host`] and [`atproto_pds_service_did`] rather than
/// spelling them itself.
#[must_use]
pub fn pds_host(apex_domain: &str) -> String {
    format!("pds.{apex_domain}")
}

/// [`pds_host`], exported for the PDS bridge.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn atproto_pds_host(apex_domain: String) -> String {
    pds_host(&apex_domain)
}

/// [`pds_service_did`], exported for the PDS bridge: the `aud` its resource
/// server requires of every access token, and the one the nest mints with.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn atproto_pds_service_did(apex_domain: String) -> String {
    pds_service_did(&apex_domain)
}

/// The PDS **service DID** — `did:web:pds.<apex>` — the name the PDS goes by in
/// the `aud` of an OAuth access token this ecosystem mints.
///
/// RFC 9068 makes `aud` the resource server, and the PDS bridge is the resource
/// server for every ATProto-family scope — so an access token whose grant holds
/// one names this among its audiences (`authorization-server.md` § The issuer →
/// *The audience is the set of readers*; `crate::authz::access_token_audience`
/// decides it). A token that does not name it is refused by the PDS, which is
/// why the two sides may not each spell it.
///
/// The per-user DID is a different thing entirely: it is the account's real
/// `did:plc`, resolved nest-side and carried as `sub`.
#[must_use]
pub fn pds_service_did(apex_domain: &str) -> String {
    format!("did:web:{}", pds_host(apex_domain))
}

// ── /.well-known/oauth-protected-resource (RFC 9728) ─────────────────────────

#[derive(Serialize)]
struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
    scopes_supported: Vec<String>,
    bearer_methods_supported: Vec<String>,
}

/// Render `/.well-known/oauth-protected-resource` for the PDS of the deployment
/// whose apex domain is `apex_domain`.
///
/// Its whole job is the redirection in § F4 detail: *this resource server's
/// authorization server is that one* — the nest's issuer, on a different host
/// from the PDS it describes, so a client has to follow it to find the AS.
///
/// `bearer_methods_supported` is **`DPoP` only**, never `header`: DPoP is
/// mandatory for all client types in ATProto (§ Ecosystem reality item 3), so
/// advertising bearer would invite a presentation this server refuses.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn oauth_protected_resource_document(apex_domain: String) -> String {
    let doc = ProtectedResourceMetadata {
        resource: issuer(&pds_host(&apex_domain)),
        authorization_servers: protected_resource_authorization_servers(&apex_domain),
        scopes_supported: GRANTABLE_SCOPES.iter().map(|s| s.to_string()).collect(),
        bearer_methods_supported: vec!["DPoP".to_string()],
    };
    render(&doc)
}

/// The authorization server(s) the PDS sends clients to, which is the one
/// member of the protected-resource document that decides *whose* tokens it
/// accepts: the nest's issuer, `https://<apex>`.
///
/// Its own owner because the resource server's acceptance rests on the same
/// fact: the PDS pins the issuer and verifies against the key set the nest
/// feeds it (`fauna.bridges.atproto.fetch_issuer_jwks`), so this list may name
/// no issuer that feed does not carry (`authorization-server.md` § The issuer →
/// *The teaching is one WS-RPC feed carrying both halves*). The feed's issuer
/// and this list are the same `oauth_issuer(apex)`, pinned equal nest-side.
#[must_use]
pub fn protected_resource_authorization_servers(apex_domain: &str) -> Vec<String> {
    vec![issuer(apex_domain)]
}

// ── /.well-known/oauth-authorization-server (RFC 8414 + ATProto) ─────────────

/// The AS document's members.
///
/// ⚠ `revocation_endpoint` was **withheld through slices 2–7** and is re-added
/// here (F4 slice 8a) **in the same change that mounts the route** — the rule
/// the withholding existed to state: what this document advertises must be
/// mounted, or a client's sign-out 404s. Slice 7 could not keep the member
/// because mounting the document without the endpoint is the exact
/// document-lie the mounting gate prevented, one endpoint over.
///
/// ⚠ `revocation_endpoint_auth_methods_supported` is **required, not
/// decorative**: RFC 8414 §2 makes the member's default `client_secret_basic`,
/// a credential class this AS does not have at all, so *omitting* it advertises
/// an authentication method every request using it would be refused for. It
/// mirrors `token_endpoint_auth_methods_supported` because revocation holds a
/// client to exactly the same identity requirement the token endpoint does.
#[derive(Serialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    pushed_authorization_request_endpoint: String,
    revocation_endpoint: String,
    userinfo_endpoint: String,
    device_authorization_endpoint: String,
    backchannel_authentication_endpoint: String,
    /// CIBA Discovery §4 — REQUIRED once the endpoint is advertised. Poll
    /// only: ping and push would have this nest call out to a client-named
    /// URL, an outbound surface nothing here needs.
    backchannel_token_delivery_modes_supported: Vec<String>,
    jwks_uri: String,
    scopes_supported: Vec<String>,
    response_types_supported: Vec<String>,
    grant_types_supported: Vec<String>,
    code_challenge_methods_supported: Vec<String>,
    token_endpoint_auth_methods_supported: Vec<String>,
    token_endpoint_auth_signing_alg_values_supported: Vec<String>,
    revocation_endpoint_auth_methods_supported: Vec<String>,
    dpop_signing_alg_values_supported: Vec<String>,
    require_pushed_authorization_requests: bool,
    client_id_metadata_document_supported: bool,
    authorization_response_iss_parameter_supported: bool,
    subject_types_supported: Vec<String>,
    id_token_signing_alg_values_supported: Vec<String>,
}

/// Render the authorization server's discovery document for the deployment
/// whose apex domain is `apex_domain`: the issuer is `https://<apex>` and every
/// endpoint hangs off it. The nest serves it at both
/// `/.well-known/oauth-authorization-server` (RFC 8414) and
/// `/.well-known/openid-configuration` (OIDC Discovery) — one document, so the
/// two cannot drift.
///
/// Every member is either a spec hard requirement from § Ecosystem reality
/// item 3 or a direct consequence of one:
///
/// * `require_pushed_authorization_requests: true` — PAR is mandatory, so the
///   document must say so rather than merely offering the endpoint.
/// * `code_challenge_methods_supported: ["S256"]` — S256 only. `plain` is a
///   downgrade a client would take if offered, and PKCE S256 is mandatory.
/// * `dpop_signing_alg_values_supported: ["ES256"]` — DPoP mandatory for all
///   client types.
/// * `client_id_metadata_document_supported: true` — client-id-as-URL
///   resolution, the ATProto client-registration model.
/// * `token_endpoint_auth_methods_supported: ["none", "private_key_jwt"]` —
///   public clients authenticate not at all (they are PKCE- and DPoP-bound
///   instead) and confidential clients by signed assertion. There is no shared
///   secret to carry a `client_secret_*` method, and inventing one would be a
///   credential class this design does not have.
/// * `revocation_endpoint_auth_methods_supported` — the same two, for the
///   reason on the struct: RFC 8414's default for this member is a method we do
///   not implement, so silence would be a false advertisement.
///
/// `scopes_supported` is [`advertised_scopes`] — the ATProto family's
/// [`GRANTABLE_SCOPES`] (see there for why the list is owned beside the matrix
/// rather than written out here) plus the fixed OIDC family.
///
/// The OIDC members (TP6, `authorization-server.md` § OIDC): `userinfo_endpoint`
/// joined **in the change that mounted the route**, the rule every endpoint
/// member here obeys; `id_token_signing_alg_values_supported: ["ES256"]` because
/// the ID token is signed under the same issuer key as the access token (the
/// two classes separated by `typ`); `subject_types_supported: ["public"]` —
/// `sub` is the actor id, the same to every client.
pub fn oauth_authorization_server_document(apex_domain: String) -> String {
    let iss = issuer(&apex_domain);
    let endpoint = |path: &str| format!("{iss}{path}");
    let doc = AuthorizationServerMetadata {
        authorization_endpoint: endpoint(PATH_AUTHORIZE),
        token_endpoint: endpoint(PATH_TOKEN),
        pushed_authorization_request_endpoint: endpoint(PATH_PAR),
        revocation_endpoint: endpoint(PATH_REVOKE),
        userinfo_endpoint: endpoint(PATH_USERINFO),
        device_authorization_endpoint: endpoint(PATH_DEVICE_AUTHORIZATION),
        backchannel_authentication_endpoint: endpoint(PATH_BC_AUTHORIZE),
        backchannel_token_delivery_modes_supported: vec!["poll".to_string()],
        jwks_uri: endpoint(PATH_JWKS),
        issuer: iss,
        scopes_supported: advertised_scopes(),
        response_types_supported: vec!["code".to_string()],
        grant_types_supported: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
            GRANT_TYPE_DEVICE_CODE.to_string(),
            GRANT_TYPE_CIBA.to_string(),
            GRANT_TYPE_HANDOFF.to_string(),
        ],
        code_challenge_methods_supported: vec!["S256".to_string()],
        token_endpoint_auth_methods_supported: vec![
            "none".to_string(),
            "private_key_jwt".to_string(),
        ],
        token_endpoint_auth_signing_alg_values_supported: vec!["ES256".to_string()],
        revocation_endpoint_auth_methods_supported: vec![
            "none".to_string(),
            "private_key_jwt".to_string(),
        ],
        dpop_signing_alg_values_supported: vec!["ES256".to_string()],
        require_pushed_authorization_requests: true,
        client_id_metadata_document_supported: true,
        authorization_response_iss_parameter_supported: true,
        subject_types_supported: vec!["public".to_string()],
        id_token_signing_alg_values_supported: vec!["ES256".to_string()],
    };
    render(&doc)
}

/// Serialize a metadata document.
///
/// Infallible in practice — every field is a `String`, `bool` or `Vec<String>`,
/// none of which `serde_json` can fail on — so the signature stays a plain
/// `String` rather than making every caller handle an error that cannot occur.
/// The fallback is deliberately an empty JSON object rather than a panic: this
/// runs inside a request path, and a nonsense document is a better failure than
/// a downed bridge.
fn render<T: Serialize>(doc: &T) -> String {
    serde_json::to_string(doc).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{AuthzInput, PLANE_OAUTH};
    use serde_json::Value;

    const APEX: &str = "example.com";

    fn as_json(raw: &str) -> Value {
        serde_json::from_str(raw).expect("a rendered document must be valid JSON")
    }

    /// The `htu` a DPoP proof must carry is the endpoint URL a client reads out
    /// of this very document, so the two spellings are pinned equal. A drift
    /// here refuses every proof from every correctly-implemented client, and
    /// neither side looks wrong on its own.
    #[test]
    fn the_par_endpoint_url_is_the_one_the_document_advertises() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            doc["pushed_authorization_request_endpoint"],
            Value::String(oauth_par_endpoint_url(APEX.to_string()))
        );
    }

    /// The token twin of the pin above (F4 slice 7): the `htu` a token-request
    /// DPoP proof must carry is the `token_endpoint` the client read out of
    /// this document.
    #[test]
    fn the_token_endpoint_url_is_the_one_the_document_advertises() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            doc["token_endpoint"],
            Value::String(oauth_token_endpoint_url(APEX.to_string()))
        );
    }

    /// The revocation twin of the two pins above (F4 slice 8a). It replaces the
    /// negative pin slices 3–7 carried (`the_revocation_endpoint_is_not_
    /// advertised_while_unmounted`), which said the member must be absent until
    /// the route existed; the route exists, so the property to hold is now the
    /// positive one — and `/oauth/revoke` takes a DPoP proof, so this URL is
    /// also the `htu` the endpoint checks against.
    #[test]
    fn the_revocation_endpoint_url_is_the_one_the_document_advertises() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            doc["revocation_endpoint"],
            Value::String(oauth_revoke_endpoint_url(APEX.to_string()))
        );
    }

    /// The two consent starts' twins of the pins above (TP9): each endpoint
    /// takes a DPoP proof whose `htu` must equal the URL advertised here.
    #[test]
    fn the_consent_start_urls_are_the_ones_the_document_advertises() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            doc["device_authorization_endpoint"],
            Value::String(oauth_device_authorization_endpoint_url(APEX.to_string()))
        );
        assert_eq!(
            doc["backchannel_authentication_endpoint"],
            Value::String(oauth_bc_authorize_endpoint_url(APEX.to_string()))
        );
    }

    /// RFC 8414 §2 defaults `revocation_endpoint_auth_methods_supported` to
    /// `client_secret_basic` when the member is absent — a credential class
    /// this AS does not implement. Omitting it is therefore not "saying
    /// nothing", it is advertising a method every request using it would be
    /// refused for, so the member is pinned present and equal to the token
    /// endpoint's list: revocation holds a client to the same identity
    /// requirement the token endpoint does.
    #[test]
    fn revocation_advertises_the_same_auth_methods_as_the_token_endpoint() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            doc["revocation_endpoint_auth_methods_supported"],
            doc["token_endpoint_auth_methods_supported"],
            "the two endpoints authenticate clients identically, so their \
             advertised method lists must not be able to drift"
        );
        assert!(
            !doc["revocation_endpoint_auth_methods_supported"].is_null(),
            "absent means client_secret_basic per RFC 8414 — a method this AS \
             refuses; the member must be rendered explicitly"
        );
    }

    /// **The slice's headline property: the documents cannot advertise a
    /// capability the matrix denies.**
    ///
    /// Every scope in `scopes_supported` is walked through the real
    /// [`crate::authz::authorize`] until some `lxm` allows — so this fails the
    /// day a scope is advertised that D8 grants nothing under, which is exactly
    /// how a discovery document starts lying. It asserts *that* something is
    /// granted, never *what*: pinning the per-scope grant here would duplicate
    /// the matrix's own tests and turn a widening into two edits.
    ///
    /// The walk itself lives in [`crate::authz::scope_grants_something`], which
    /// `crate::oauth_par` also runs each **requested** scope through — the
    /// advertised set and the accepted set are decided by one function, so they
    /// cannot drift into disagreeing about what this server can honour.
    ///
    /// Walks the RENDERED `scopes_supported` of both documents, not a list
    /// this test chooses, so a family added to either document is walked the
    /// day it is added (the OIDC family answers by membership —
    /// `scope_grants_something`'s own docs).
    #[test]
    fn every_advertised_scope_grants_something() {
        let docs = [
            as_json(&oauth_authorization_server_document(APEX.to_string())),
            as_json(&oauth_protected_resource_document(APEX.to_string())),
        ];
        for doc in &docs {
            let scopes = doc["scopes_supported"]
                .as_array()
                .expect("scopes_supported");
            assert!(!scopes.is_empty());
            for scope in scopes {
                let scope = scope.as_str().expect("a scope is a string");
                // The advertised strings that are not grantable as written:
                // the `records` arm's plane and verb, whose qualifier is the
                // client's own kind (`authorization-server.md` § Scope grammar
                // → *The third arm*: "Discovery advertises the plane and
                // verb"), which PAR refuses bare; and the two folder arms',
                // whose qualifier is the user's (§ Scope grammar →
                // *The folder plane's qualifier is the user's*): requestable
                // bare as a card-qualified request, still never granted as
                // written — the ceremony qualifies it before anything records.
                if scope == crate::fauna_scope::SCOPE_RECORDS_RW {
                    assert_eq!(crate::fauna_scope::arm_of(scope), None);
                    assert_eq!(crate::fauna_scope::user_qualified_bare_arm(scope), None);
                    continue;
                }
                if scope == crate::fauna_scope::SCOPE_FOLDER_DEPOSIT
                    || scope == crate::fauna_scope::SCOPE_FOLDER_READ
                {
                    assert_eq!(crate::fauna_scope::arm_of(scope), None);
                    assert!(crate::fauna_scope::user_qualified_bare_arm(scope).is_some());
                    continue;
                }
                assert!(
                    crate::authz::scope_grants_something(scope),
                    "`{scope}` is advertised in scopes_supported but D8 grants \
                     nothing under it — a client would request it, the consent \
                     screen would show it, and every call would still deny"
                );
            }
        }
    }

    /// The OIDC family is the ISSUER's (TP6): the authorization server
    /// advertises it, and the PDS's protected-resource document — which
    /// describes a resource server that honours none of it — does not.
    #[test]
    fn the_oidc_family_is_advertised_by_the_issuer_and_not_by_the_pds() {
        let as_doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        let pr_doc = as_json(&oauth_protected_resource_document(APEX.to_string()));
        for scope in OIDC_SCOPES {
            assert!(
                as_doc["scopes_supported"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::String(scope.to_string())),
                "the issuer must advertise `{scope}`"
            );
            assert!(
                !pr_doc["scopes_supported"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::String(scope.to_string())),
                "the PDS honours no `{scope}` and must not advertise it"
            );
        }
    }

    /// The Fauna family is the nest's: the issuer advertises each built arm,
    /// and the PDS's protected-resource document advertises none.
    #[test]
    fn the_fauna_arms_are_advertised_by_the_issuer_and_not_by_the_pds() {
        let as_doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        let pr_doc = as_json(&oauth_protected_resource_document(APEX.to_string()));
        for advertised in FaunaScopeArm::ALL.iter().flat_map(|arm| arm.advertised()) {
            let scope = Value::String(advertised.to_string());
            assert!(
                as_doc["scopes_supported"]
                    .as_array()
                    .unwrap()
                    .contains(&scope)
            );
            assert!(
                !pr_doc["scopes_supported"]
                    .as_array()
                    .unwrap()
                    .contains(&scope)
            );
        }
    }

    /// The UserInfo twin of the endpoint pins above (TP6): the `htu` a
    /// UserInfo DPoP proof must carry is the `userinfo_endpoint` this document
    /// advertises. It replaces the negative pin that held the member absent
    /// while the route was unmounted — the route and the member land together.
    #[test]
    fn the_userinfo_endpoint_url_is_the_one_the_document_advertises() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            doc["userinfo_endpoint"],
            Value::String(oauth_userinfo_endpoint_url(APEX.to_string()))
        );
        assert_eq!(
            doc["id_token_signing_alg_values_supported"],
            serde_json::json!(["ES256"])
        );
        assert_eq!(
            doc["subject_types_supported"],
            serde_json::json!(["public"])
        );
    }

    /// **Permission sets are served but not advertised, and this pins the
    /// silence as a decision** (`atproto-pds-full.md:335`, resolved by the PS-b
    /// build 2026-08-03).
    ///
    /// The temptation is precise and someone will feel it: `include:` scopes
    /// are now fully served at PAR, so the discovery document looks incomplete,
    /// and the neighbouring families are advertised by wildcard exemplar —
    /// `include:*` is the obvious symmetric entry. It is also the one thing
    /// this list may never contain. An exemplar has to be a *real grantable
    /// value*, and `include:*` cannot be: the grammar takes an NSID, `*` fails
    /// NSID validation before any resolution happens, and no wildcard could be
    /// granted in principle because what a set permits is decided by its
    /// publisher's document. So the honest options are silence or a signal that
    /// is not `scopes_supported`, and which one a real client needs is an
    /// **open question left to F5's reference-client run** — not something to
    /// settle by making the document lie.
    #[test]
    fn permission_sets_are_served_but_never_advertised() {
        for tempting in ["include:", "include:*", "include:com.example.appPerms"] {
            assert!(
                !GRANTABLE_SCOPES.contains(&tempting),
                "`{tempting}` cannot be an advertised exemplar — it is not a \
                 grantable value, and what a set permits is its publisher's \
                 document to say. If F5 shows clients need a discovery signal, \
                 it is a NEW member, never an entry in this list"
            );
        }
        assert!(
            !crate::authz::scope_grants_something("include:com.example.appPerms"),
            "a bare include: grants nothing on its own — it is expanded at PAR \
             and only its MEMBERS reach the matrix, which is exactly why it \
             cannot be advertised here"
        );
    }

    /// The negative half, and the one that actually bites: the scopes the
    /// matrix parses but grants nothing under must stay OUT of the advertised
    /// set. Each is a live temptation — they are real grammar, and a session
    /// completing the list from the spec would add them.
    #[test]
    fn scopes_the_matrix_grants_nothing_under_are_not_advertised() {
        for empty in [
            "transition:email",
            "account:email",
            "account:status",
            "identity:handle",
            "identity:*",
        ] {
            assert!(
                !GRANTABLE_SCOPES.contains(&empty),
                "`{empty}` parses but grants nothing on this PDS, so \
                 advertising it would promise a capability that does not exist"
            );
        }
    }

    /// A collection-narrowed `repo:` scope is a *deferred* gap rather than an
    /// empty one, and must not be advertised while it denies — advertising a
    /// narrowing this server answers by refusing everything is worse than not
    /// offering it.
    #[test]
    fn a_collection_narrowed_repo_scope_is_not_advertised_while_it_denies() {
        assert!(
            !crate::authz::authorize(AuthzInput {
                plane: PLANE_OAUTH.to_string(),
                scopes: vec!["repo:app.bsky.feed.post".to_string()],
                lxm: "com.atproto.repo.createRecord".to_string(),
                ..probe_input()
            })
            .is_allow(),
            "the collection-narrowed gap closed — advertise it now, and delete \
             this test's premise along with the exclusion note on GRANTABLE_SCOPES"
        );
        assert!(!GRANTABLE_SCOPES.contains(&"repo:app.bsky.feed.post"));
    }

    /// The base scope must leave an `atproto`-only grant able to ask who it is
    /// (finding 57 — before F4 slice 2 this denied, including `getSession`),
    /// and must not have quietly become a second `transition:generic`.
    #[test]
    fn the_base_scope_grants_identity_and_nothing_more() {
        let under_base = |lxm: &str| {
            crate::authz::authorize(AuthzInput {
                plane: PLANE_OAUTH.to_string(),
                scopes: vec![crate::authz::SCOPE_ATPROTO_BASE.to_string()],
                lxm: lxm.to_string(),
                ..probe_input()
            })
            .is_allow()
        };

        assert!(
            under_base("com.atproto.server.getSession"),
            "a spec-compliant `atproto`-only grant must be able to ask who it \
             authenticated as"
        );
        assert!(under_base("com.atproto.server.describeServer"));

        for denied in [
            "com.atproto.repo.getRecord",
            "com.atproto.repo.createRecord",
            "com.atproto.repo.uploadBlob",
            "app.bsky.feed.getTimeline",
            "chat.bsky.convo.listConvos",
            "com.atproto.server.getServiceAuth",
            "app.bsky.actor.getPreferences",
            // The session lifecycle verbs — settled by
            // splitting the class. See the dedicated pin below for why this is
            // a plane boundary and not merely a narrower base scope.
            "com.atproto.server.createSession",
            "com.atproto.server.refreshSession",
            "com.atproto.server.deleteSession",
        ] {
            assert!(
                !under_base(denied),
                "the base scope must not cover `{denied}` — a granular scope \
                 exists to say so explicitly, and folding it in would make the \
                 consent screen understate what it granted"
            );
        }
    }

    /// **Session lifecycle is a PLANE boundary, not a scope width**.
    ///
    /// `createSession`/`refreshSession`/`deleteSession` are the app-credential
    /// plane's own lifecycle (D3 rung 1); the OAuth plane's is `/oauth/token`
    /// and `/oauth/revoke`. So the answer is not "the base scope should be
    /// narrower" — it is that **no granular scope reaches them at all**, which
    /// is what walking every advertised scope asserts here rather than just
    /// the base one.
    ///
    /// **The transition scopes are the deliberate exception, and they stay
    /// one.** `transition:generic` and `transition:chat.bsky` are *defined* by
    /// the spec as "what an app password grants" — that definitional identity
    /// is why [`crate::authz`]'s `implicit_grant` is shared between them and
    /// the app-credential plane in the first place. Carving lifecycle out of
    /// them would make our `transition:` narrower than the ecosystem's, in the
    /// one scope family whose entire purpose is compatibility, to close a path
    /// that is unreachable anyway. Compatibility wins here; the narrowing
    /// instinct does not.
    ///
    /// Nothing reaches this today by *either* arm: all three routes register
    /// `Auth: Public` and verify the app plane's refresh token handler-side,
    /// so they never consult the matrix (`internal/atprotopds/server.go`
    /// registration → `internal/xrpc/xrpc.go`'s `route.Auth != Public` gate →
    /// `atprotopds/authz.go`'s `caller == nil` early return). The pin exists
    /// because a route registration is exactly what this matrix judges: the
    /// day one of them is re-registered as authenticated, this must already
    /// have been decided.
    ///
    /// ⚠ **This pin was VACUOUS as first written**,
    /// and how it was vacuous is the durable half. It swept
    /// `GRANTABLE_SCOPES` — `rpc:*?aud=*` included — over a [`probe_input`]
    /// that leaves `aud` at `None`. The `rpc:` arm refuses an absent `aud`
    /// before it looks at anything else, so the entry that could actually have
    /// reached lifecycle was answered by a condition this pin never mentions,
    /// and the class split it was written to assert did no work at all (that
    /// arm is class-blind — see `authz::no_granular_scope_reaches`). The sweep
    /// therefore runs **with an audience present**, which is the only shape in
    /// which `rpc:*?aud=*` is a live scope, and keeps the audience-less shape
    /// beside it so a future `aud`-independent regression is caught too.
    #[test]
    fn no_granular_oauth_scope_reaches_the_session_lifecycle_verbs() {
        for scope in GRANTABLE_SCOPES
            .iter()
            .filter(|s| !s.starts_with("transition:"))
        {
            for lxm in [
                "com.atproto.server.createSession",
                "com.atproto.server.refreshSession",
                "com.atproto.server.deleteSession",
            ] {
                // Both audience shapes. `None` is the resting state of every
                // non-`Proxyable` route; `Some` is what `ProxyFallback` and
                // `AuthorizeServiceAuth` supply — and is the one that makes an
                // `rpc:` scope evaluable at all.
                for aud in [None, Some("did:web:audience.example".to_string())] {
                    let verdict = crate::authz::authorize(AuthzInput {
                        plane: PLANE_OAUTH.to_string(),
                        scopes: vec![(*scope).to_string()],
                        lxm: lxm.to_string(),
                        aud: aud.clone(),
                        ..probe_input()
                    });
                    assert!(
                        !verdict.is_allow(),
                        "`{scope}` grants `{lxm}` (aud={aud:?}) — session lifecycle \
                         belongs to the app-credential plane, and the OAuth plane \
                         has /oauth/token and /oauth/revoke for the same job"
                    );
                }
            }
        }
        // …and the exception is asserted too, so it stays a decision rather
        // than becoming a gap nobody notices.
        assert!(
            crate::authz::authorize(AuthzInput {
                plane: PLANE_OAUTH.to_string(),
                scopes: vec!["transition:generic".to_string()],
                lxm: "com.atproto.server.refreshSession".to_string(),
                ..probe_input()
            })
            .is_allow(),
            "`transition:generic` is defined as the app-password grant — if it \
             stops covering session lifecycle, that is a deliberate divergence \
             from the ecosystem and needs its own ruling, not a silent drift"
        );
    }

    /// The base scope is additive with the granular ones rather than a
    /// fallback: a realistic modern grant carries both, and each is asked
    /// independently.
    #[test]
    fn the_base_scope_composes_with_granular_scopes() {
        let verdict = crate::authz::authorize(AuthzInput {
            plane: PLANE_OAUTH.to_string(),
            scopes: vec![
                crate::authz::SCOPE_ATPROTO_BASE.to_string(),
                "repo:*".to_string(),
            ],
            lxm: "com.atproto.repo.createRecord".to_string(),
            ..probe_input()
        });
        assert!(verdict.is_allow(), "{verdict:?}");
    }

    /// The re-point, stated as the deployment shape: the resource is the PDS on
    /// `pds.<apex>`, and the authorization server it sends clients to is the
    /// nest's issuer on the apex — two hosts, not one.
    #[test]
    fn the_protected_resource_document_sends_clients_to_the_nests_issuer() {
        let doc = as_json(&oauth_protected_resource_document(APEX.to_string()));
        assert_eq!(doc["resource"], "https://pds.example.com");
        assert_eq!(
            doc["authorization_servers"],
            serde_json::json!(["https://example.com"])
        );
        assert_eq!(doc["bearer_methods_supported"], serde_json::json!(["DPoP"]));
    }

    /// The rendered member and the pin are the SAME list, not two that happen
    /// to agree — which is the whole reason
    /// [`protected_resource_authorization_servers`] exists as its own owner.
    #[test]
    fn the_advertised_authorization_servers_are_the_pin() {
        let doc = as_json(&oauth_protected_resource_document(APEX.to_string()));
        assert_eq!(
            doc["authorization_servers"],
            serde_json::json!(protected_resource_authorization_servers(APEX))
        );
    }

    /// The PDS sends clients to exactly the issuer the AS document names — the
    /// identifier a client pins and every access token carries. Two spellings
    /// here is a client that follows the redirection and then refuses the
    /// server it arrives at.
    #[test]
    fn the_resource_servers_authorization_server_is_the_as_documents_issuer() {
        let as_doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(
            protected_resource_authorization_servers(APEX),
            vec![as_doc["issuer"].as_str().unwrap().to_string()]
        );
    }

    /// The spec hard requirements from § Ecosystem reality item 3, asserted as
    /// the *document's* claims — a client reads exactly these members to decide
    /// how to talk to us, so a wrong value here is a wrong protocol, not a
    /// cosmetic slip.
    #[test]
    fn the_as_document_states_every_hard_requirement() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));

        assert_eq!(doc["issuer"], "https://example.com");
        assert_eq!(doc["require_pushed_authorization_requests"], true);
        assert_eq!(
            doc["pushed_authorization_request_endpoint"],
            "https://example.com/oauth/par"
        );
        assert_eq!(
            doc["code_challenge_methods_supported"],
            serde_json::json!(["S256"]),
            "S256 only — offering `plain` is a downgrade a client would take"
        );
        assert_eq!(
            doc["dpop_signing_alg_values_supported"],
            serde_json::json!(["ES256"])
        );
        assert_eq!(doc["client_id_metadata_document_supported"], true);
        assert_eq!(doc["response_types_supported"], serde_json::json!(["code"]));
        assert_eq!(
            doc["grant_types_supported"],
            serde_json::json!([
                "authorization_code",
                "refresh_token",
                "urn:ietf:params:oauth:grant-type:device_code",
                "urn:openid:params:grant-type:ciba",
                "urn:fauna:params:grant-type:handoff"
            ]),
            "exactly the four grants /oauth/token serves"
        );
        assert_eq!(
            doc["backchannel_token_delivery_modes_supported"],
            serde_json::json!(["poll"]),
            "poll only — ping and push would dial a client-named URL"
        );
        assert_eq!(
            doc["token_endpoint_auth_methods_supported"],
            serde_json::json!(["none", "private_key_jwt"]),
            "no shared secret exists to carry a client_secret_* method"
        );
    }

    /// `jwks_uri` is the route the nest mounts, by construction — this is the
    /// pair whose drift would leave a client unable to verify our tokens with
    /// no error anywhere on our side.
    #[test]
    fn the_jwks_uri_is_the_mounted_path() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        assert_eq!(doc["jwks_uri"], format!("https://{APEX}{PATH_JWKS}"));
    }

    /// The issuer is compared for equality by clients, so the shape is the
    /// assertion: an origin, no trailing slash, no path.
    #[test]
    fn the_issuer_is_a_bare_origin() {
        let doc = as_json(&oauth_authorization_server_document(APEX.to_string()));
        let iss = doc["issuer"].as_str().unwrap();
        assert!(!iss.ends_with('/'), "issuer must not end in a slash: {iss}");
        assert_eq!(iss, format!("https://{APEX}"));
    }

    /// An [`AuthzInput`] with the fields these tests do not vary: the account
    /// enabled, on an ordinary authed route.
    fn probe_input() -> AuthzInput {
        AuthzInput {
            plane: PLANE_OAUTH.to_string(),
            scopes: Vec::new(),
            lxm: String::new(),
            endpoint_class: crate::authz::ENDPOINT_CLASS_AUTHED.to_string(),
            aud: None,
            external_apps_enabled: true,
        }
    }
}
