//! The authorization server's request-taking endpoints — `/oauth/par`,
//! `/oauth/authorize` and its poll, `/oauth/token` and `/oauth/revoke`
//! (TP5 S2), and the two polled consent starts `/oauth/device_authorization`
//! and `/oauth/bc-authorize` (TP9). Every endpoint this issuer serves answers
//! here.
//!
//! `docs/goal/behavior/authorization-server.md` § The issuer rules that the AS
//! "is up whenever the nest is up", so these mount **unconditionally, in every
//! nest flavor**, beside [`crate::oauth_issuer_routes`]'s discovery and JWKS.
//! A domainless nest answers `503` here for the same reason it does there: the
//! issuer identity is a client-pinned value, and one this nest invented would
//! stop being true the moment a domain is claimed.
//!
//! # Discovery advertises all of it, and only now
//!
//! A discovery document naming an endpoint is a promise to every client that
//! reads it, so the four members were withheld until all four answered: PAR
//! alone is not a flow, and a client that pushed a request and then found no
//! `authorization_endpoint` would be stranded holding a `request_uri` it can do
//! nothing with. They arrive together with this module's last two endpoints —
//! see [`crate::oauth_issuer_routes`]'s document builder.
//!
//! # Permission sets resolve THROUGH the bridge, never here
//!
//! An `include:<NSID>` scope needs an ATProto resolution plane — TXT lookup,
//! PLC directory, `did:web`, and MST proof verification over a third party's
//! repo — which lives on the PDS bridge (`atprotolex`) and is exactly the
//! ATProto half TP5 leaves where it is. This endpoint reaches that plane over
//! the permission-set request call ([`crate::oauth_as_permission_sets`]): a
//! push naming the set to the connected PDS bridge, answered by the bridge's
//! `deliver_permission_set` with the verified bytes, which the shared expander
//! then turns into the card's scopes. Every failure — no bridge connected, an
//! older bridge, a refusal, the deadline — gives the answer the bridge's own
//! closed world gives: `invalid_scope`, never an unverified expansion.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use fauna_bridge_atproto::authz::describe_scope;
use fauna_bridge_atproto::oauth_client::ResolvedClient;
use fauna_bridge_atproto::oauth_metadata::{
    GRANT_TYPE_CIBA, GRANT_TYPE_DEVICE_CODE, GRANT_TYPE_HANDOFF, PATH_AUTHORIZE,
    PATH_AUTHORIZE_POLL, PATH_BC_AUTHORIZE, PATH_DEVICE_AUTHORIZATION, PATH_PAR, PATH_REVOKE,
    PATH_TOKEN, PATH_USERINFO, oauth_bc_authorize_endpoint_url,
    oauth_device_authorization_endpoint_url, oauth_par_endpoint_url, oauth_revoke_endpoint_url,
    oauth_token_endpoint_url, pds_service_did,
};
use fauna_bridge_atproto::oauth_par::{
    AcceptedParRequest, ParPlan, ParRequest, ParVerdict, StartRequest, finish_par_request,
    plan_par_request, plan_start_request,
};
use fauna_protocol::atproto_pds::ConsentSetInfo;

use crate::bridge_atproto_handlers::{
    ApprovedConsent, CONSENT_REQUEST_TTL_MILLIS, ConsentStart, ConsentState,
};
use crate::oauth_as_ceremony::{
    AUTH_CODE_TTL_SECS, AUTHORIZE_FLOW_SLACK_SECS, AuthorizeFlow, BACKCHANNEL_POLL_INTERVAL_SECS,
    BackchannelFlow, BackchannelLookup, BackchannelPace, BackchannelStart, StoredAuthCode,
    mint_handle,
};
use crate::oauth_as_error::{
    DPOP_NONCE_HEADER, DPOP_PROOF_HEADER, ERR_ACCESS_DENIED, ERR_AUTHORIZATION_PENDING,
    ERR_EXPIRED_TOKEN, ERR_INVALID_BINDING_MESSAGE, ERR_INVALID_GRANT, ERR_INVALID_REQUEST,
    ERR_INVALID_SCOPE, ERR_SLOW_DOWN, ERR_UNSUPPORTED_GRANT_TYPE, OAuthDeny, oauth_error_response,
};
use crate::oauth_as_rate_limit::{
    AUTHORIZATION_BUDGET, AUTHORIZATION_CODE_GRANT_BUDGET, EndpointBudget, OAuthRateLimitLayer,
    POLL_BUDGET, REFRESH_GRANT_BUDGET, TOKEN_ENDPOINT_BUDGET, USERINFO_BUDGET,
};
use crate::oauth_as_state::{PAR_REQUEST_TTL_SECS, ParPeek, StoredParRequest};
use crate::oauth_as_token::{
    OAuthGrant, TOKEN_TYPE_DPOP, access_lifetime_secs, join_scope, mint_jti, mint_tokens,
    public_session_deadline, refresh_expiry, verify_access_token, verify_refresh_token,
};
use crate::oauth_issuer_key::{IssuerPublicKey, IssuerSigner};
use crate::routes::AppState;

/// Caps the posted form. Every legitimate parameter set is well under a
/// kilobyte; the cap is what stops an anonymous endpoint being asked to buffer
/// a caller's choice of body.
pub const PAR_MAX_BODY_BYTES: usize = 16 * 1024;

/// RFC 9126's recommended URN form for a `request_uri`. The reference value
/// after it carries 256 bits of entropy, well past the RFC's 128-bit floor.
/// Spelled once in the shared route grammar, which carries the handle to the
/// Fauna app in the same-device handoff (`fauna://consent/<request_uri>`).
const REQUEST_URI_PREFIX: &str = fauna_core::app_route::PAR_REQUEST_URI_PREFIX;
const REQUEST_URI_ENTROPY: usize = 32;

/// The AS's request-taking routes.
///
/// **One sub-router per endpoint, merged** — not one router carrying several
/// routes — because the budget is a per-endpoint fact and a layer applies to
/// everything in the router it is attached to. Merging keeps each endpoint's
/// bucket name attached to exactly the route that spends it, which is the whole
/// point of the per-route re-decision ([`crate::oauth_as_rate_limit`]).
///
/// The budget is a layer rather than a call inside the handler so it is spent
/// before the handler is entered at all — see that module for why the ordering
/// is the ruling and not a preference.
///
/// ⚠ There is deliberately **no `DefaultBodyLimit` layer.** The body cap belongs
/// after the DPoP gate, not before it, so it is applied by the handler's own
/// [`axum::body::to_bytes`] call — see [`oauth_par`]'s `body` parameter.
pub fn routes(limiter: Arc<crate::oauth_as_rate_limit::EndpointLimiter>) -> Router<Arc<AppState>> {
    Router::new()
        .merge(
            Router::new()
                .route(PATH_PAR, post(oauth_par))
                .layer(OAuthRateLimitLayer::new(
                    Arc::clone(&limiter),
                    PATH_PAR,
                    AUTHORIZATION_BUDGET,
                )),
        )
        .merge(
            Router::new()
                .route(PATH_AUTHORIZE, axum::routing::get(oauth_authorize))
                .layer(OAuthRateLimitLayer::new(
                    Arc::clone(&limiter),
                    PATH_AUTHORIZE,
                    AUTHORIZATION_BUDGET,
                )),
        )
        .merge(
            Router::new()
                .route(PATH_AUTHORIZE_POLL, post(oauth_authorize_poll))
                .layer(OAuthRateLimitLayer::new(
                    Arc::clone(&limiter),
                    PATH_AUTHORIZE_POLL,
                    POLL_BUDGET,
                )),
        )
        .merge(
            Router::new()
                .route(PATH_TOKEN, post(oauth_token))
                .layer(OAuthRateLimitLayer::new(
                    Arc::clone(&limiter),
                    PATH_TOKEN,
                    TOKEN_ENDPOINT_BUDGET,
                )),
        )
        .merge(Router::new().route(PATH_REVOKE, post(oauth_revoke)).layer(
            OAuthRateLimitLayer::new(Arc::clone(&limiter), PATH_REVOKE, AUTHORIZATION_BUDGET),
        ))
        // OIDC Core §5.3.1: UserInfo answers both GET and POST.
        .merge(
            Router::new()
                .route(
                    PATH_USERINFO,
                    axum::routing::get(crate::oauth_as_oidc::oauth_userinfo)
                        .post(crate::oauth_as_oidc::oauth_userinfo),
                )
                .layer(OAuthRateLimitLayer::new(
                    Arc::clone(&limiter),
                    PATH_USERINFO,
                    USERINFO_BUDGET,
                )),
        )
        // The two polled consent starts spend the authorization budget PAR
        // does: each is the first request of a flow that ends in a consent
        // card, an unauthenticated write surface exactly as PAR is.
        .merge(
            Router::new()
                .route(PATH_DEVICE_AUTHORIZATION, post(oauth_device_authorization))
                .layer(OAuthRateLimitLayer::new(
                    Arc::clone(&limiter),
                    PATH_DEVICE_AUTHORIZATION,
                    AUTHORIZATION_BUDGET,
                )),
        )
        .merge(
            Router::new()
                .route(PATH_BC_AUTHORIZE, post(oauth_bc_authorize))
                .layer(OAuthRateLimitLayer::new(
                    limiter,
                    PATH_BC_AUTHORIZE,
                    AUTHORIZATION_BUDGET,
                )),
        )
}

/// `POST /oauth/par` — RFC 9126 pushed authorization requests.
///
/// **The ordering is the contract**, ported from the bridge's own handler:
/// every refusal returns before the store is touched, so a request that will be
/// rejected never occupies a slot in an unauthenticated write surface.
///
/// 1. The closed-world check — a nest with no issuer identity cannot start a
///    flow it could only fail after the user has been redirected.
/// 2. The request budget (the layer above, already spent by the time we are
///    here).
/// 3. A fresh `DPoP-Nonce` on **every** response from here on, success or
///    refusal. RFC 9449 has the server supply nonces this way, and it means a
///    client that is out of step recovers from the very response that told it
///    so — including its first request, which cannot have one.
/// 4. The DPoP gate, **before the body is read** and before client resolution,
///    which dials a host the caller names. Requiring a proof and a live nonce
///    first means an anonymous caller cannot make this nest fetch anything
///    until it has completed a round trip and proved possession of a key.
/// 5. The body, read at last and only up to [`PAR_MAX_BODY_BYTES`], then the
///    form parsed out of it.
/// 6. Client resolution, then client **authentication**, then request
///    validation. Authenticate before you authorize: a confidential client that
///    has not proved it is itself must not have its parameters evaluated at
///    all, or the scope and redirect refusals — diagnostics about someone
///    else's configuration — become a probe.
async fn oauth_par(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    // ⚠ `Body`, not `Bytes`, and that is step 4's ordering being real rather
    // than described. `Bytes` is an extractor: axum would read the whole body
    // before this function is entered at all, so the DPoP gate could not
    // possibly precede it. Taking the unread stream and calling
    // [`axum::body::to_bytes`] below — after the gate, with the cap as its
    // argument — is the Go original's `MaxBytesReader`-after-`dpopGate` shape
    // exactly, and it is what lets an anonymous caller be refused before this
    // nest reads a byte it chose.
    body: axum::body::Body,
) -> Response {
    // The refusal is in the OAuth error shape, not the nest's generic one — and
    // that is the one place this endpoint deliberately differs from its sibling
    // read surfaces, which answer `503` through `ApiError`. Their readers are
    // discovery consumers; this endpoint's reader is an OAuth client, which
    // parses `{error, error_description}` and can act on
    // `temporarily_unavailable` (retry later) where a generic body tells it
    // only that something went wrong. The bridge's own PAR answered exactly
    // this, for exactly this case.
    let Some(issuer) = crate::oauth_issuer_routes::issuer(&state) else {
        return oauth_error_response(&OAuthDeny::unavailable(
            "this nest has not claimed a domain yet, so it is not serving authorization requests",
        ));
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;

    // Every response from here on carries a fresh nonce.
    let nonce = runtime.nonces.mint(now);
    let respond = |response: Response| with_nonce(response, &nonce);

    // The `htu` a conformant client builds comes from the endpoint URL it read
    // out of OUR discovery document, so it is rendered by the same shared
    // builder that document uses — never from this request's own Host header,
    // which a caller reaching this nest under another name could then satisfy.
    let htu = oauth_par_endpoint_url(state.web_serving_domain());
    let proofs: Vec<String> = headers
        .get_all(DPOP_PROOF_HEADER)
        .iter()
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .collect();
    let dpop_jkt = match crate::oauth_as_gates::dpop_gate(runtime, &proofs, "POST", &htu, now) {
        Ok(jkt) => jkt,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };

    // Only now is the body read, and only up to the cap. A body over it fails
    // here — as the refusal a client can act on, carrying the nonce — rather
    // than as a bare `413` from a layer that ran before this endpoint had
    // decided anything about the caller.
    let body = match axum::body::to_bytes(body, PAR_MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return respond(oauth_error_response(&OAuthDeny::new(
                ERR_INVALID_REQUEST,
                "request body is unreadable or larger than any legitimate authorization request",
            )));
        }
    };
    let form = match parse_form(&headers, &body) {
        Some(form) => form,
        None => {
            return respond(oauth_error_response(&OAuthDeny::new(
                ERR_INVALID_REQUEST,
                "request body is not a readable form encoding",
            )));
        }
    };

    // A client pushes parameters; it does not push a `request_uri`. Accepting
    // one would let a caller name the handle its own request is stored under.
    if !first(&form, "request_uri").is_empty() {
        return respond(oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "request_uri is issued by this server, not supplied by the client",
        )));
    }
    let client_id = first(&form, "client_id");
    if client_id.is_empty() {
        return respond(oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "client_id is required",
        )));
    }
    // The key attestations (`third-party.md` § The principal model, rule 2;
    // `third-party-kinds.md` § Principal write authority): parsed before the
    // client is resolved, so a malformed one costs this nest no fetch. Absent
    // is the standard client and is fine; present and not a 32-byte key is the
    // client's error.
    let attested = match parse_attested_keys(&form) {
        Ok(keys) => keys,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };

    let client: ResolvedClient = match crate::oauth_as_client::resolve_client(
        &runtime.clients,
        runtime.fetcher.as_ref(),
        &client_id,
        now,
    )
    .await
    {
        Ok(client) => client,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };

    if let Err(deny) = crate::oauth_as_gates::client_assertion_gate(
        runtime,
        &first(&form, "client_assertion_type"),
        &first(&form, "client_assertion"),
        &client,
        &issuer,
        now,
    ) {
        return respond(oauth_error_response(&deny));
    }

    let accepted = match settle_plan(
        &state,
        plan_par_request(
            ParRequest {
                client_id: client_id.clone(),
                response_type: first(&form, "response_type"),
                redirect_uri: first(&form, "redirect_uri"),
                scope: first(&form, "scope"),
                state: first(&form, "state"),
                code_challenge: first(&form, "code_challenge"),
                code_challenge_method: first(&form, "code_challenge_method"),
                login_hint: optional(&form, "login_hint"),
                nonce: form_nonce(&form),
            },
            client.clone(),
        ),
    )
    .await
    {
        Ok(request) => request,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };

    let request_uri = mint_request_uri();
    runtime.par.put(
        request_uri.clone(),
        StoredParRequest {
            request: accepted,
            client,
            dpop_jkt,
            attested,
            expires: now.saturating_add(PAR_REQUEST_TTL_SECS),
        },
        now,
    );

    // RFC 9126 §2.2: 201 Created, with the request_uri and its lifetime.
    let mut response = (
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "request_uri": request_uri,
            "expires_in": PAR_REQUEST_TTL_SECS,
        })),
    )
        .into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    respond(response)
}

/// The PAR parameter a client attests its capability-grant holder key in: the
/// base64url (unpadded) encoding of an X25519 public key. Namespaced so it can
/// never collide with a parameter a future OAuth extension defines.
pub(crate) const HOLDER_X25519_PARAM: &str = "fauna_holder_x25519";

/// The start parameter a client attests its writer key in: the base64url
/// (unpadded) encoding of the Ed25519 public key it signs its `ext.*` rows
/// with (`third-party-kinds.md` § Principal write authority) — the same
/// carriage, bounds and refusals as [`HOLDER_X25519_PARAM`].
pub(crate) const WRITER_ED25519_PARAM: &str = "fauna_writer_ed25519";

/// Both key attestations a start may carry — the one parser every start
/// (PAR and the two polled starts) calls, so no door can take one and drop
/// the other.
fn parse_attested_keys(
    form: &HashMap<String, Vec<String>>,
) -> Result<crate::db::third_party_principals::AttestedKeys, OAuthDeny> {
    Ok(crate::db::third_party_principals::AttestedKeys {
        holder_x25519: parse_attested_key(
            optional(form, HOLDER_X25519_PARAM),
            HOLDER_X25519_PARAM,
            "X25519",
        )?,
        writer_ed25519: parse_attested_key(
            optional(form, WRITER_ED25519_PARAM),
            WRITER_ED25519_PARAM,
            "Ed25519",
        )?,
    })
}

/// Decode one attested key: absent → `None`; exactly 32 bytes of base64url →
/// the key; anything else → `invalid_request`, naming the parameter so a
/// client developer can tell which of their values was wrong.
fn parse_attested_key(
    raw: Option<String>,
    param: &str,
    curve: &str,
) -> Result<Option<[u8; 32]>, OAuthDeny> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.as_bytes())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .map(Some)
        .ok_or_else(|| {
            OAuthDeny::new(
                ERR_INVALID_REQUEST,
                format!("{param} must be an unpadded base64url {curve} public key (32 bytes)"),
            )
        })
}

/// Turn a consent request's plan into an accepted request or a refusal —
/// shared by PAR and both polled starts, so permission sets expand one way
/// whichever door the request came in by.
///
/// The resolution plane is the PDS bridge's, reached over the permission-set
/// request call — see the module docs. Nothing is stored before
/// `finish_par_request` returns, and any set the bridge could not deliver
/// verified refuses the whole request, naming the set (the client's own
/// string) and never the reason.
async fn settle_plan(
    state: &Arc<AppState>,
    plan: ParPlan,
) -> Result<AcceptedParRequest, OAuthDeny> {
    match plan {
        ParPlan::Accept { request } => Ok(request),
        ParPlan::Deny { error, description } => Err(OAuthDeny { error, description }),
        ParPlan::Resolve { pending } => {
            let records =
                crate::oauth_as_permission_sets::resolve_permission_sets(state, &pending.includes)
                    .await
                    .map_err(|unresolved| {
                        OAuthDeny::new(
                            ERR_INVALID_SCOPE,
                            format!("permission set {:?} could not be resolved", unresolved.nsid),
                        )
                    })?;
            match finish_par_request(pending, records) {
                ParVerdict::Accept { request } => Ok(request),
                ParVerdict::Deny { error, description } => Err(OAuthDeny { error, description }),
            }
        }
    }
}

/// Attach the nonce this request was answered under.
///
/// A header value that will not render is dropped rather than panicking: the
/// mint is base64url over a MAC, so it cannot happen, and a request path
/// answers rather than aborts.
pub(crate) fn with_nonce(mut response: Response, nonce: &str) -> Response {
    if let Ok(value) = axum::http::HeaderValue::from_str(nonce) {
        response.headers_mut().insert(DPOP_NONCE_HEADER, value);
    }
    response
}

/// The posted form, as a multimap.
///
/// A multimap rather than a map because presence and emptiness are different
/// answers for `login_hint`, and because a duplicated parameter must not
/// silently pick a winner the client did not intend — the first value is the
/// one acted on, exactly as the Go original's `PostForm.Get` does.
///
/// `None` when the body is not a form: the content type must say so, since a
/// caller posting JSON to a form endpoint has misunderstood something worth
/// telling it about rather than silently parsing as an empty form.
fn parse_form(headers: &HeaderMap, body: &Bytes) -> Option<HashMap<String, Vec<String>>> {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Parameters after the media type are legal (`; charset=utf-8`).
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media_type != "application/x-www-form-urlencoded" {
        return None;
    }
    let mut form: HashMap<String, Vec<String>> = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(body) {
        form.entry(key.into_owned())
            .or_default()
            .push(value.into_owned());
    }
    Some(form)
}

/// The first value for `key`, or the empty string — the shape every closed-world
/// check in the policy modules expects, where "absent" and "present and empty"
/// are the same request.
fn first(form: &HashMap<String, Vec<String>>, key: &str) -> String {
    form.get(key)
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or_default()
}

/// The first value for `key` as an option — for the one parameter where absent
/// and empty are genuinely different, and both mean "no hint".
fn optional(form: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    form.get(key)
        .and_then(|values| values.first())
        .filter(|value| !value.is_empty())
        .cloned()
}

/// The OIDC `nonce` exactly as sent: absent is `None`, and a PRESENT empty
/// value stays `Some("")` so the shared policy refuses it. Unlike a
/// `login_hint`, an empty nonce is not "no nonce" — a client that sent the
/// parameter meant to bind its sign-in to something, and quietly minting an ID
/// token bound to nothing would let it believe otherwise.
fn form_nonce(form: &HashMap<String, Vec<String>>) -> Option<String> {
    form.get("nonce").and_then(|values| values.first()).cloned()
}

/// Mint a `request_uri`: the RFC's URN prefix over 256 bits of entropy.
fn mint_request_uri() -> String {
    use base64::Engine as _;
    use rand::RngCore as _;

    let mut buf = [0u8; REQUEST_URI_ENTROPY];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    format!(
        "{REQUEST_URI_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
    )
}

// ── The consent page ─────────────────────────────────────────────────────────

/// How long one poll request may be held open before answering "pending" and
/// letting the page re-poll.
///
/// Under every common proxy and browser idle bound, and each re-poll costs the
/// caller a request slot — which is what keeps a held poll from being a free
/// resource hold.
pub const POLL_HOLD_SECS: u64 = 25;

/// The fallback re-ask cadence while holding. The consent wake makes resolution
/// immediate; this bounds the added latency when that best-effort nudge is lost
/// — the page must never *depend* on a wake arriving.
pub const POLL_INTERVAL_SECS: u64 = 5;

/// Caps the poll's posted form. It carries one field.
const POLL_MAX_BODY_BYTES: usize = 4 * 1024;

/// The page is deliberately **dependency-free**: inline styles, one nonce-gated
/// inline script, no external resource of any kind. That is what lets the CSP be
/// `default-src 'none'`, so the consent surface can never be made to load
/// something an attacker named — the same posture that keeps `logo_uri` off the
/// approval card entirely.
const AUTHORIZE_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Approve access</title>
<style>
body{font-family:system-ui,sans-serif;margin:0;background:#f5f5f4;color:#1c1917}
main{max-width:26rem;margin:8vh auto;padding:2rem;background:#fff;border-radius:12px;box-shadow:0 1px 4px rgba(0,0,0,.08)}
h1{font-size:1.15rem;margin:0 0 1rem}
.client{font-weight:600;margin:0}
.client-id{color:#57534e;font-size:.85rem;word-break:break-all;margin:.2rem 0 1rem}
.set{margin:0}
.set-id{color:#57534e;font-size:.85rem;word-break:break-all;margin:.2rem 0 .4rem}
.set-details{color:#57534e;font-size:.9rem;margin:0 0 .4rem}
ul{padding-left:1.2rem;margin:0 0 1.25rem}
li{margin:.3rem 0}
.code{font-size:1.9rem;font-weight:700;letter-spacing:.12em;text-align:center;padding:.6rem 0;background:#f5f5f4;border-radius:8px;margin:0 0 1.25rem;font-variant-numeric:tabular-nums}
#status{color:#57534e;font-size:.9rem;text-align:center;margin:0}
</style>
</head>
<body>
<main>
<h1>Approve this in your Fauna app</h1>
<p class="client">{{#if client_name}}{{client_name}}{{else}}An application{{/if}}</p>
<p class="client-id">{{client_id}}</p>
<p>is asking to:</p>
<ul>{{#each scopes}}<li>{{this}}</li>{{/each}}</ul>
{{#each sets}}<p class="set">Some of that comes from the permission set{{#if title}} &ldquo;{{title}}&rdquo;{{/if}}:</p>
<p class="set-id">{{nsid}}</p>
{{#if details}}<p class="set-details">{{details}}</p>{{/if}}
<ul>{{#each members}}<li>{{this}}</li>{{/each}}</ul>
{{/each}}<p>Open your Fauna app and approve the request there. Before you approve,
check that the code your app shows matches this one:</p>
<div class="code" id="binding-code">{{code}}</div>
<p id="status">Waiting for your approval&hellip;</p>
<span id="flow" data-flow="{{flow_token}}" data-poll="{{poll_path}}" hidden></span>
<script nonce="{{nonce}}">
(function(){
  var el = document.getElementById('flow');
  var status = document.getElementById('status');
  function dead(){ status.textContent = 'This request is no longer active. Close this tab and start again from the application.'; }
  function poll(){
    fetch(el.dataset.poll, {
      method: 'POST',
      headers: {'Content-Type': 'application/x-www-form-urlencoded'},
      body: 'flow=' + encodeURIComponent(el.dataset.flow),
      cache: 'no-store'
    }).then(function(r){
      if (r.status === 429) { setTimeout(poll, 15000); return null; }
      if (!r.ok) { dead(); return null; }
      return r.json();
    }).then(function(j){
      if (!j) return;
      if (j.redirect) { location.replace(j.redirect); return; }
      setTimeout(poll, 1000);
    }).catch(function(){ setTimeout(poll, 3000); });
  }
  poll();
})();
</script>
</main>
</body>
</html>
"#;

/// A terminal failure the flow cannot redirect out of — no live PAR means no
/// trustworthy redirect target, so the page is where the user is told. Fixed
/// strings only.
const AUTHORIZE_ERROR_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Request failed</title>
<style>body{font-family:system-ui,sans-serif;margin:0;background:#f5f5f4;color:#1c1917}
main{max-width:26rem;margin:8vh auto;padding:2rem;background:#fff;border-radius:12px;box-shadow:0 1px 4px rgba(0,0,0,.08)}
h1{font-size:1.15rem;margin:0 0 .75rem}p{margin:0;color:#57534e}</style>
</head>
<body><main><h1>{{title}}</h1><p>{{detail}}</p></main></body>
</html>
"#;

/// One permission set as this page renders it.
///
/// ⚠ `title` and `details` are the **set author's** text — a different attacker
/// from the client, and no more reviewed — which is why they go through the
/// template's escaping exactly like the client name does.
#[derive(serde::Serialize)]
struct AuthorizePageSet {
    nsid: String,
    title: String,
    details: String,
    members: Vec<String>,
}

#[derive(serde::Serialize)]
struct AuthorizePageData {
    client_name: String,
    client_id: String,
    scopes: Vec<String>,
    /// The permission-set provenance behind those scopes. It renders here for
    /// the same reason the scope *wording* is shared with the app card: the two
    /// surfaces sit side by side during the ceremony, so a set the app names and
    /// this page does not is precisely the "is this the same request?" doubt the
    /// binding code exists to remove.
    sets: Vec<AuthorizePageSet>,
    code: String,
    flow_token: String,
    poll_path: String,
    nonce: String,
}

/// Render a template with handlebars' default escaping.
///
/// `{{ }}` HTML-escapes, which is the whole reason this uses a template engine
/// the nest already carries rather than a `format!` and a hand-rolled escaper:
/// every attacker-controlled value on this page — the client's name, its
/// `client_id`, a set's title and details, the scope wording — lands in an HTML
/// *text* node, and a reviewed escaper is the right thing between them and a
/// consent screen. The two values that land in attributes (the flow token and
/// the script nonce) are base64url minted by this nest.
///
/// A render failure answers with the fixed error page rather than a partial one:
/// a half-rendered consent screen is a screen that could be misread.
fn render_page(template: &str, data: &impl serde::Serialize) -> Result<String, ()> {
    let mut hbs = handlebars::Handlebars::new();
    hbs.register_escape_fn(handlebars::html_escape);
    hbs.render_template(template, data).map_err(|e| {
        tracing::error!(error = %e, "oauth: consent page render failed");
    })
}

/// The page's security headers.
///
/// The CSP allows exactly the page's own nonce-gated script and inline styles
/// plus same-origin fetch for the poll — nothing else, from anywhere.
/// `frame-ancestors 'none'` because a framed consent page is a clickjacking
/// primitive; `no-referrer` because the redirect targets are other people's
/// apps.
fn authorize_page_headers(script_nonce: Option<&str>) -> [(axum::http::HeaderName, String); 5] {
    use axum::http::header;
    let script = match script_nonce {
        Some(nonce) => format!("'nonce-{nonce}'"),
        None => "'none'".to_string(),
    };
    [
        (
            header::CONTENT_SECURITY_POLICY,
            format!(
                "default-src 'none'; style-src 'unsafe-inline'; script-src {script}; \
                 connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
            ),
        ),
        (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
        (header::CACHE_CONTROL, "no-store".to_string()),
        (header::REFERRER_POLICY, "no-referrer".to_string()),
        (header::X_FRAME_OPTIONS, "DENY".to_string()),
    ]
}

fn error_page(status: axum::http::StatusCode, title: &str, detail: &str) -> Response {
    let body = render_page(
        AUTHORIZE_ERROR_PAGE,
        &serde_json::json!({ "title": title, "detail": detail }),
    )
    .unwrap_or_else(|()| "<!doctype html><title>Request failed</title>".to_string());
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    for (name, value) in authorize_page_headers(None) {
        if let Ok(value) = axum::http::HeaderValue::from_str(&value) {
            headers.insert(name, value);
        }
    }
    response
}

/// The one wording every terminal "this request is not usable" answer gives.
///
/// Deliberately identical for an unknown handle, an expired one, a spent one and
/// a `client_id` that does not match the pushed request: a caller poking at
/// `request_uri`s must not be able to tell which kind of dead it found, and the
/// user's remedy is the same in all four.
fn unusable_request_page() -> Response {
    error_page(
        axum::http::StatusCode::BAD_REQUEST,
        "Unknown or expired request",
        "This authorization request does not exist or has expired. Start again from the application.",
    )
}

#[derive(serde::Deserialize)]
struct AuthorizeQuery {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    request_uri: String,
}

/// `GET /oauth/authorize` — the consent page.
///
/// **The ordering is `/oauth/par`'s discipline, one endpoint over:** the budget
/// runs before any store is read (so a flood cannot consume other flows' PAR
/// handles for free — a refused request leaves the handle live for its owner's
/// retry), and the single-use PAR consumption runs before the nest is asked to
/// open a consent (so a request that will be refused never creates nest state).
async fn oauth_authorize(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<AuthorizeQuery>,
) -> Response {
    if crate::oauth_issuer_routes::issuer(&state).is_none() {
        return error_page(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Authorization is not available",
            "This nest has not claimed a domain yet, so it is not serving authorization requests.",
        );
    }
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;

    if query.client_id.is_empty() || query.request_uri.is_empty() {
        return error_page(
            axum::http::StatusCode::BAD_REQUEST,
            "Malformed authorization request",
            "The link that brought you here is incomplete. Start again from the application.",
        );
    }
    let Some(stored) = runtime.par.take(&query.request_uri, now) else {
        return unusable_request_page();
    };
    // RFC 9126 §4: the authorization request's `client_id` must identify the
    // same client the pushed request was made for. The PAR is already consumed
    // either way — single use is the store's rule, and a mismatched attempt is
    // precisely the kind of replay it exists to spend.
    if query.client_id != stored.request.client_id {
        return unusable_request_page();
    }

    // Open the pending consent. The nest mints the binding code and stores the
    // row the approval card renders; this surface renders what it is handed and
    // never mints a code of its own. Nothing in the result says whether the
    // `login_hint` resolved, and nothing here may try to infer it — the page
    // text is the same either way.
    let client_name = stored
        .client
        .client_name
        .as_deref()
        .filter(|name| !name.is_empty());
    // The frozen expansion crosses with the request. A set's `ignored` members
    // deliberately do not: that is the resolver's diagnostic record of members
    // which granted nothing, and a card cannot act on it.
    let consent_sets: Vec<ConsentSetInfo> = stored
        .request
        .sets
        .iter()
        .map(|set| ConsentSetInfo {
            nsid: set.nsid.clone(),
            title: set.title.clone(),
            details: set.details.clone(),
            members: set.members.clone(),
            extra: Default::default(),
        })
        .collect();
    let row = match crate::bridge_atproto_handlers::open_consent_request(
        &state,
        ConsentStart::Browser {
            login_hint: stored.request.login_hint.as_deref(),
        },
        &stored.request.client_id,
        client_name,
        &stored.request.scopes,
        &consent_sets,
        &consent_binding(&stored.client, stored.attested),
    )
    .await
    {
        Ok(Some(row)) => row,
        // The browser start always opens a row; a `None` here is the owner
        // breaking its own contract, and answered as the failure it is.
        Ok(None) => {
            tracing::error!("oauth: the browser start opened no consent row");
            return error_page(
                axum::http::StatusCode::BAD_GATEWAY,
                "Could not start the approval",
                "This nest could not open the approval request. Start again from the application.",
            );
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "oauth: open consent request failed");
            return error_page(
                axum::http::StatusCode::BAD_GATEWAY,
                "Could not start the approval",
                "This nest could not open the approval request. Start again from the application.",
            );
        }
    };

    let flow_token = mint_handle();
    let page = AuthorizePageData {
        client_name: client_name.unwrap_or_default().to_string(),
        client_id: stored.request.client_id.clone(),
        // The scope wording has ONE owner, shared with the app's approval card:
        // a set is a grouping, never a second vocabulary.
        scopes: stored
            .request
            .scopes
            .iter()
            .map(|scope| describe_scope(scope.clone()))
            .collect(),
        sets: stored
            .request
            .sets
            .iter()
            .map(|set| AuthorizePageSet {
                nsid: set.nsid.clone(),
                title: set.title.clone().unwrap_or_default(),
                details: set.details.clone().unwrap_or_default(),
                members: set
                    .members
                    .iter()
                    .map(|member| describe_scope(member.clone()))
                    .collect(),
            })
            .collect(),
        code: row.code.clone(),
        flow_token: flow_token.clone(),
        poll_path: PATH_AUTHORIZE_POLL.to_string(),
        nonce: mint_handle(),
    };
    let nonce = page.nonce.clone();
    let Ok(body) = render_page(AUTHORIZE_PAGE, &page) else {
        return error_page(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Could not render the approval page",
            "Start again from the application.",
        );
    };

    // The flow outlives its consent row by a slack window, so the page's next
    // poll is answered with the honest "expired" redirect read off the row
    // rather than the flow vanishing first and answering a generic "gone".
    runtime.flows.put(
        flow_token,
        AuthorizeFlow::new(
            row.consent_id.clone(),
            stored,
            row.expires_at / 1000 + AUTHORIZE_FLOW_SLACK_SECS,
        ),
    );

    let mut response = (axum::http::StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    for (name, value) in authorize_page_headers(Some(&nonce)) {
        if let Ok(value) = axum::http::HeaderValue::from_str(&value) {
            headers.insert(name, value);
        }
    }
    response
}

/// `POST /oauth/authorize/poll` — the page's long-poll.
///
/// The caller's only credential is the flow token; an invalid one is answered
/// immediately with no hold and no consent read, so guessing costs a map lookup
/// inside the poll budget.
///
/// Answer shape: `{"status": "..."}` with `redirect` set once the ceremony has
/// its one answer. The redirect carries the OAuth authorization response —
/// `code`+`state`+`iss` on approval, `error=access_denied`+`state`+`iss` on a
/// decline or expiry — so a declined or expired request fails the page cleanly
/// into the client's own callback rather than hanging.
async fn oauth_authorize_poll(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let Some(issuer) = crate::oauth_issuer_routes::issuer(&state) else {
        return poll_answer(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            None,
        );
    };
    let Ok(body) = axum::body::to_bytes(body, POLL_MAX_BODY_BYTES).await else {
        return poll_answer(axum::http::StatusCode::BAD_REQUEST, "expired", None);
    };
    let Some(form) = parse_form(&headers, &body) else {
        return poll_answer(axum::http::StatusCode::BAD_REQUEST, "expired", None);
    };
    let token = first(&form, "flow");
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    if token.is_empty() || state.oauth_as.flows.get(&token, now).is_none() {
        return poll_answer(axum::http::StatusCode::NOT_FOUND, "expired", None);
    }

    match poll_consent(&state, &token, &issuer).await {
        Ok(Some(redirect)) => poll_answer(axum::http::StatusCode::OK, "resolved", Some(&redirect)),
        Ok(None) => poll_answer(axum::http::StatusCode::OK, "pending", None),
        // A transient failure reading the consent: answer pending and let the
        // page retry. The flow is still live, and failing it here would strand a
        // ceremony the user may be mid-approval on.
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "oauth: consent poll failed");
            poll_answer(axum::http::StatusCode::OK, "pending", None)
        }
    }
}

fn poll_answer(status: axum::http::StatusCode, state: &str, redirect: Option<&str>) -> Response {
    let mut body = serde_json::Map::new();
    body.insert("status".into(), serde_json::Value::String(state.into()));
    if let Some(redirect) = redirect {
        body.insert(
            "redirect".into(),
            serde_json::Value::String(redirect.into()),
        );
    }
    let mut response = (status, Json(serde_json::Value::Object(body))).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Ask for the flow's resolution, holding for up to the poll budget while it is
/// pending. `Ok(None)` is still-pending.
async fn poll_consent(
    state: &Arc<AppState>,
    token: &str,
    issuer: &str,
) -> anyhow::Result<Option<String>> {
    let runtime = &state.oauth_as;
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let Some(snapshot) = runtime.flows.get(token, now) else {
        return Ok(None);
    };
    // A raced release may already have the answer — never re-ask or re-mint.
    if let Some(answer) = snapshot.released {
        return Ok(Some(answer));
    }
    if !runtime.flows.add_waiter(token) {
        // Held-poll cap reached for this flow: answer without holding.
        return fetch_and_maybe_release(state, token, issuer).await;
    }

    // The one path that ever grows the wake store, so it is also where the
    // store is swept — the sweep runs exactly as often as the growth it
    // answers, and never on a nest nobody is asking.
    runtime.consent_wakes.sweep_expired(now);
    let result =
        hold_for_resolution(state, token, issuer, &snapshot.consent_id, snapshot.expires).await;
    runtime.flows.drop_waiter(token);
    result
}

/// The held half of the poll, split out so the waiter slot is always released
/// on every path out — including the `?` an inner read would take.
async fn hold_for_resolution(
    state: &Arc<AppState>,
    token: &str,
    issuer: &str,
    consent_id: &[u8],
    flow_expires: i64,
) -> anyhow::Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(POLL_HOLD_SECS);
    loop {
        // ⚠ SUBSCRIBE BEFORE READING. A resolution landing between the read
        // below and the wait at the bottom must still wake this loop, and the
        // receiver taken here is what remembers it. Reading first would open
        // exactly the gap the wake exists to close, and cost the user a full
        // fallback interval on the common path.
        let mut wake = state
            .oauth_as
            .consent_wakes
            .subscribe(consent_id, flow_expires);

        if let Some(answer) = fetch_and_maybe_release(state, token, issuer).await? {
            return Ok(Some(answer));
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let interval = remaining.min(Duration::from_secs(POLL_INTERVAL_SECS));
        match tokio::time::timeout(interval, wake.changed()).await {
            // Woken, or the fallback tick — either way, re-read.
            Ok(Ok(())) | Err(_) => {}
            // ⚠ The registration is GONE — evicted because the wake store hit
            // its ceiling. `changed()` resolves to an error the instant the
            // sender drops, so swallowing this with the timeout's own result
            // (`let _ = …`, which is what stood here) makes the loop spin as
            // fast as the CPU allows until the deadline, on the one endpoint an
            // anonymous caller can hold open. The eviction is legitimate — the
            // nudge is best-effort — so the honest response is to take the
            // fallback interval this iteration would otherwise have waited.
            Ok(Err(_)) => tokio::time::sleep(interval).await,
        }
    }
}

/// Read the consent once and, on a terminal state, build and memoize the flow's
/// one redirect answer. `Ok(None)` means still pending.
async fn fetch_and_maybe_release(
    state: &Arc<AppState>,
    token: &str,
    issuer: &str,
) -> anyhow::Result<Option<String>> {
    let runtime = &state.oauth_as;
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let Some(snapshot) = runtime.flows.get(token, now) else {
        return Ok(None);
    };
    if let Some(answer) = snapshot.released {
        return Ok(Some(answer));
    }

    let resolution =
        crate::bridge_atproto_handlers::read_consent_state(state, &snapshot.consent_id).await?;
    let redirect_uri = snapshot.redirect_uri;
    let flow_state = snapshot.state;
    let released = match resolution {
        ConsentState::Pending => return Ok(None),
        ConsentState::Approved(approved) => runtime.flows.release(token, || {
            build_approved_redirect(
                runtime,
                &snapshot.client_id,
                &snapshot.code_challenge,
                &snapshot.dpop_jkt,
                snapshot.nonce.clone(),
                snapshot.attested,
                &redirect_uri,
                &flow_state,
                issuer,
                &approved,
            )
        }),
        ConsentState::Denied => runtime.flows.release(token, || {
            error_redirect(
                &redirect_uri,
                &flow_state,
                issuer,
                "the user declined the request",
            )
        }),
        ConsentState::Expired => runtime.flows.release(token, || {
            error_redirect(
                &redirect_uri,
                &flow_state,
                issuer,
                "the request expired before it was approved",
            )
        }),
    };
    Ok(released)
}

/// Mint the single-use authorization code and the redirect that carries it.
///
/// ⚠ The stored grant's scopes are the **resolution's** — the set read back from
/// the row the user was shown — never the flow's own PAR copy. That is what
/// stops a recorded grant ever being wider than the card.
///
/// Runs inside [`AuthorizeFlowStore::release`], under the flow store's lock, so
/// it must not block or await; it does neither. Taking the code store's lock
/// here is the module's declared ordering (flows, then codes).
#[allow(clippy::too_many_arguments)]
fn build_approved_redirect(
    runtime: &crate::oauth_as_state::OAuthAsRuntime,
    client_id: &str,
    code_challenge: &str,
    dpop_jkt: &str,
    nonce: Option<String>,
    attested: crate::db::third_party_principals::AttestedKeys,
    redirect_uri: &str,
    flow_state: &str,
    issuer: &str,
    approved: &ApprovedConsent,
) -> String {
    let Some(subject) = crate::oauth_as_token::grant_subject(
        &approved.scopes,
        approved.login_did.as_deref(),
        &approved.actor_id,
    ) else {
        // Approved, but the account cannot complete the flow — the grant
        // carries ATProto scopes and the account has no ACTIVE ATProto
        // identity. (An OIDC-only sign-in needs none, and never lands here.)
        // Post-consent, so answering honestly is no oracle, and the token
        // exchange could only fail later anyway.
        return error_redirect(
            redirect_uri,
            flow_state,
            issuer,
            "the approving account cannot complete this request",
        );
    };
    let code = mint_handle();
    runtime.auth_codes.put(
        code.clone(),
        StoredAuthCode {
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            scopes: approved.scopes.clone(),
            sets: approved.sets.clone(),
            actor_id: approved.actor_id.to_vec(),
            subject,
            code_challenge: code_challenge.to_string(),
            dpop_jkt: dpop_jkt.to_string(),
            nonce,
            attested,
            expires: fauna_core::data::Timestamp::now_secs_or_zero()
                .saturating_add(AUTH_CODE_TTL_SECS),
        },
    );
    append_query(
        redirect_uri,
        &[("code", &code), ("state", flow_state), ("iss", issuer)],
    )
}

/// The OAuth error half of the authorization response (RFC 6749 §4.1.2.1 plus
/// RFC 9207's `iss`).
///
/// **One error code on purpose:** `access_denied` covers both a decline and an
/// expiry, and the description carries the difference — a second code would be a
/// distinction no client branches on.
fn error_redirect(redirect_uri: &str, flow_state: &str, issuer: &str, description: &str) -> String {
    append_query(
        redirect_uri,
        &[
            ("error", "access_denied"),
            ("error_description", description),
            ("state", flow_state),
            ("iss", issuer),
        ],
    )
}

/// Attach an authorization response to a redirect URI, keeping any query the
/// client declared (RFC 6749 §3.1.2: the redirect URI may carry its own query
/// component, which MUST be retained).
fn append_query(redirect_uri: &str, params: &[(&str, &str)]) -> String {
    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
    let query: String = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{redirect_uri}{separator}{query}")
}

// ── `/oauth/token` and `/oauth/revoke` ───────────────────────────────────────
//
// The last two endpoints of the port (TP5 S2c). Their ordering is `/oauth/par`'s
// one endpoint over, and every refusal below is the Go original's by value.

/// The signing material a mint needs: the active issuer key and the nest's own
/// OAuth session secret.
///
/// One blocking hop for both, because both are DB reads under the deployment
/// seed and two hops would be two chances to be answered from a rotation's
/// two sides. Both are lookups, never mints: the boot step seats the key and
/// the secret before the nest serves, so a door answered by a serving
/// generation that a deployment-seed rotation has not torn down yet refuses
/// rather than sealing either one under the retired seed that generation holds.
async fn signing_material(state: &Arc<AppState>) -> Result<(IssuerSigner, [u8; 32]), OAuthDeny> {
    let Some(signing_key) = state.nest_signing_key.as_ref() else {
        return Err(OAuthDeny::server(
            "this nest holds no deployment signing key",
        ));
    };
    let seed = signing_key.to_bytes();
    let db = state.db.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let conn = db.conn_blocking();
        let signer = crate::oauth_issuer_key::active_signer(&conn, &seed)?;
        let secret = crate::oauth_session_secret::session_secret(&conn, &seed)?;
        anyhow::Ok((signer, secret))
    })
    .await;
    match loaded {
        Ok(Ok(pair)) => Ok(pair),
        Ok(Err(e)) => {
            tracing::error!(error = %format!("{e:#}"), "oauth: signing material unavailable");
            Err(OAuthDeny::server(
                "could not reach this issuer's signing keys",
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, "oauth: signing material task failed");
            Err(OAuthDeny::server(
                "could not reach this issuer's signing keys",
            ))
        }
    }
}

/// The verifying material `/oauth/revoke` needs: the served public key set and
/// the session secret.
///
/// The key set comes through [`crate::oauth_issuer_key::serve_key_set`] — the
/// **same door** `/oauth/jwks` and the admin status kind read, which is what
/// keeps the retirement horizon applied in exactly one place. A revocation
/// presented with a token signed by a key past that horizon therefore stops
/// being recognised at the same instant a resource server stops accepting it.
pub(crate) async fn verifying_material(
    state: &Arc<AppState>,
    now: i64,
) -> Result<(Vec<IssuerPublicKey>, [u8; 32]), OAuthDeny> {
    let Some(signing_key) = state.nest_signing_key.as_ref() else {
        return Err(OAuthDeny::server(
            "this nest holds no deployment signing key",
        ));
    };
    let seed = signing_key.to_bytes();
    let db = state.db.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let conn = db.conn_blocking();
        let keys = crate::oauth_issuer_key::serve_key_set(&conn, now)?;
        let secret = crate::oauth_session_secret::session_secret(&conn, &seed)?;
        anyhow::Ok((keys, secret))
    })
    .await;
    match loaded {
        Ok(Ok(pair)) => Ok(pair),
        Ok(Err(e)) => {
            tracing::error!(error = %format!("{e:#}"), "oauth: verifying material unavailable");
            Err(OAuthDeny::server("could not reach this issuer's keys"))
        }
        Err(e) => {
            tracing::error!(error = %e, "oauth: verifying material task failed");
            Err(OAuthDeny::server("could not reach this issuer's keys"))
        }
    }
}

/// The PDS resource server's identifier — the reader an access token names in
/// `aud` when its grant holds an ATProto-family scope. The other reader is this
/// nest, named by its issuer identifier; which of the two a token carries is
/// decided at the mint from the grant's scopes (`authorization-server.md`
/// § The issuer → *The audience is the set of readers*).
///
/// Built from the shared owner, never spelled here: the bridge looks for this
/// exact string among the token's audiences when it verifies, so the two sides
/// may not each have an opinion about it.
fn pds_reader(state: &AppState) -> String {
    pds_service_did(&state.web_serving_domain())
}

/// `POST /oauth/token` — RFC 6749 §4.1.3 and §6, the two grants.
///
/// **The ordering is `/oauth/par`'s, one endpoint over**, and it is the
/// contract: the closed-world check, the endpoint's budget (the layer), a fresh
/// nonce on every response, the DPoP gate **before the body is read**, then the
/// body, then the grant. Here the gate does one more thing than at PAR — the
/// thumbprint it returns is what every branch below compares against what the
/// flow was bound to, so a stolen code or refresh token is useless without the
/// key its ceremony proved.
///
/// ⚠ It takes the whole [`axum::extract::Request`] rather than extractors, and
/// that is the gate-before-body ordering being real rather than described:
/// `Bytes` is an extractor, so axum would read the entire body before this
/// function were entered at all. Taking the request and splitting it here means
/// the unread stream is still a stream when the DPoP gate runs — the Go
/// original's `MaxBytesReader`-after-`dpopGate` shape exactly. It is also how
/// the peer address is reached: the same `ConnectInfo` extension the rate-limit
/// layer reads, so the refresh grant's own bucket keys on exactly what the
/// endpoint's does.
async fn oauth_token(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> Response {
    let source = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let Some(issuer) = crate::oauth_issuer_routes::issuer(&state) else {
        return oauth_error_response(&OAuthDeny::unavailable(
            "this nest has not claimed a domain yet, so it is not issuing tokens",
        ));
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;
    let nonce = runtime.nonces.mint(now);
    let respond = |response: Response| with_nonce(response, &nonce);

    let htu = oauth_token_endpoint_url(state.web_serving_domain());
    let dpop_jkt = match crate::oauth_as_gates::dpop_gate(
        runtime,
        &dpop_proofs(&headers),
        "POST",
        &htu,
        now,
    ) {
        Ok(jkt) => jkt,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };

    let form = match read_form(body, &headers).await {
        Ok(form) => form,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };

    // The two grants that are not polled each spend their OWN budget, after
    // `grant_type` is parsed and before any lookup — the ordering the
    // `ClassAuth` re-decision fixes. See [`REFRESH_GRANT_BUDGET`] and
    // [`AUTHORIZATION_CODE_GRANT_BUDGET`] for what each bounds that the
    // endpoint's poll-class bucket does not.
    let over_grant_budget = |route: &'static str, budget: EndpointBudget, what: &str| {
        (!state.oauth_limiter.allow(route, source, budget, now)).then(|| {
            let mut response = oauth_error_response(&OAuthDeny::unavailable(format!(
                "too many {what} requests from this address — retry after the interval in Retry-After"
            )));
            *response.status_mut() = axum::http::StatusCode::TOO_MANY_REQUESTS;
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("60"),
            );
            response
        })
    };
    match first(&form, "grant_type").as_str() {
        "authorization_code" => {
            if let Some(refused) = over_grant_budget(
                AUTHORIZATION_CODE_GRANT_ROUTE,
                AUTHORIZATION_CODE_GRANT_BUDGET,
                "authorization code",
            ) {
                return respond(refused);
            }
            respond(authorization_code_grant(&state, &form, &issuer, &dpop_jkt, now).await)
        }
        "refresh_token" => {
            if let Some(refused) =
                over_grant_budget(REFRESH_GRANT_ROUTE, REFRESH_GRANT_BUDGET, "refresh")
            {
                return respond(refused);
            }
            respond(refresh_token_grant(&state, &form, &issuer, &dpop_jkt, now).await)
        }
        GRANT_TYPE_DEVICE_CODE => respond(
            backchannel_grant(
                &state,
                &form,
                BackchannelStart::TypedCode,
                &issuer,
                &dpop_jkt,
                now,
            )
            .await,
        ),
        GRANT_TYPE_CIBA => respond(
            backchannel_grant(
                &state,
                &form,
                BackchannelStart::Push,
                &issuer,
                &dpop_jkt,
                now,
            )
            .await,
        ),
        GRANT_TYPE_HANDOFF => respond(
            backchannel_grant(
                &state,
                &form,
                BackchannelStart::Handoff,
                &issuer,
                &dpop_jkt,
                now,
            )
            .await,
        ),
        "" => respond(oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "grant_type is required",
        ))),
        // RFC 6749 §5.2 has its own code for this, and naming it is what tells
        // a client author the grant is unsupported rather than malformed.
        _ => respond(oauth_error_response(&OAuthDeny::new(
            ERR_UNSUPPORTED_GRANT_TYPE,
            "this authorization server supports authorization_code, refresh_token, the \
             device_code grant, the CIBA grant and the handoff grant",
        ))),
    }
}

/// PKCE — the ONE comparison, for both grants whose request began with PAR
/// (the browser start's code and the same-device handoff's poll). The
/// challenge was validated as S256-and-well-formed at PAR and carried verbatim
/// ever since, so there is deliberately no method to re-read here: a value
/// whose only legal setting was checked once invites a second,
/// differently-written check.
fn pkce_check(form: &HashMap<String, Vec<String>>, code_challenge: &str) -> Result<(), OAuthDeny> {
    let verifier = first(form, "code_verifier");
    if verifier.is_empty() {
        return Err(OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "code_verifier is required",
        ));
    }
    let digest = <sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes());
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    if !fauna_core::secret::constant_time_eq(computed.as_bytes(), code_challenge.as_bytes()) {
        return Err(OAuthDeny::new(
            ERR_INVALID_GRANT,
            "code_verifier does not match the authorization request's code_challenge",
        ));
    }
    Ok(())
}

/// The route keys the two non-polled grants' buckets are kept under. Not
/// mounted paths — each names a *sub*-decision of `/oauth/token`, and the `#`
/// is what makes it unmistakably not one.
const REFRESH_GRANT_ROUTE: &str = "/oauth/token#refresh_token";
const AUTHORIZATION_CODE_GRANT_ROUTE: &str = "/oauth/token#authorization_code";

/// Redeem a released authorization code.
///
/// ⚠ **The code is consumed BEFORE it is validated, and the grant is recorded
/// BEFORE the tokens are answered.** Both are deliberate and neither is an
/// ordering to tidy:
///
/// * `take` deletes on lookup, so a redemption that then fails PKCE or the key
///   binding has still spent the code — a guessed or stolen code is worth
///   exactly one attempt. An honest client's retry failing too is correct: a
///   client that must retry starts a new flow.
/// * A crash between the grant row and the answer costs one failed exchange,
///   while answering first would hand out credentials the connected-apps
///   surface cannot see or revoke — a capability with no audit row, which
///   `principles.md` forbids outright.
async fn authorization_code_grant(
    state: &Arc<AppState>,
    form: &HashMap<String, Vec<String>>,
    issuer: &str,
    dpop_jkt: &str,
    now: i64,
) -> Response {
    let code = first(form, "code");
    if code.is_empty() {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "code is required for the authorization_code grant",
        ));
    }
    let Some(entry) = state.oauth_as.auth_codes.take(&code, now) else {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "authorization code is unknown, expired, or already redeemed",
        ));
    };

    // The client must be the one the code was issued to, and it must
    // authenticate if its document says it is confidential. Resolution runs
    // against the same TTL'd cache PAR filled moments ago, so this is a hit
    // rather than a fetch on any real flow.
    let client_id = first(form, "client_id");
    if client_id.is_empty() || client_id != entry.client_id {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "client_id does not match the client this code was issued to",
        ));
    }
    let client = match resolve_and_authenticate(state, form, &client_id, issuer, now).await {
        Ok(client) => client,
        Err(deny) => return oauth_error_response(&deny),
    };

    // RFC 6749 §4.1.3: the redirect_uri, when the authorization request carried
    // one, must be repeated identically here.
    if first(form, "redirect_uri") != entry.redirect_uri {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "redirect_uri does not match the authorization request",
        ));
    }

    if let Err(deny) = pkce_check(form, &entry.code_challenge) {
        return oauth_error_response(&deny);
    }

    // The key binding, and it is what makes a stolen code useless: the code was
    // released to a flow whose PAR proved possession of one key, so redeeming
    // it requires proving possession of the SAME key. Without this check every
    // other check here is about the request rather than the requester.
    if dpop_jkt != entry.dpop_jkt {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "this request is signed with a different key than the authorization request",
        ));
    }

    record_and_issue(
        state,
        &client,
        ConsentedGrant {
            client_id: &entry.client_id,
            subject: &entry.subject,
            actor_id: &entry.actor_id,
            scopes: &entry.scopes,
            // Straight off the redeemed code: the provenance belongs to the
            // ceremony that produced this code.
            sets: &entry.sets,
            dpop_jkt: &entry.dpop_jkt,
            nonce: entry.nonce.as_deref(),
            attested: entry.attested,
        },
        issuer,
        now,
    )
    .await
}

/// What a consent ceremony settled, as a grant is recorded from it: every
/// field read back from the consent the user answered (through the code it
/// released, or straight off the row for a polled start) — never from the
/// client's own copy of its request.
struct ConsentedGrant<'a> {
    client_id: &'a str,
    /// The tokens' `sub`, as [`crate::oauth_as_token::grant_subject`] resolved
    /// it for this grant's scopes — the DID for an ATProto grant, the actor id
    /// for an OIDC-only one.
    subject: &'a str,
    actor_id: &'a [u8],
    scopes: &'a [String],
    sets: &'a [ConsentSetInfo],
    dpop_jkt: &'a str,
    /// The ceremony's OIDC `nonce`, echoed into the ID token — PAR's only; the
    /// two polled starts carry none.
    nonce: Option<&'a str>,
    /// The keys the client attested at its start (`fauna_holder_x25519`,
    /// `fauna_writer_ed25519`,
    /// `third-party.md` § The principal model) — at PAR for the browser start,
    /// at the start endpoint itself for the two polled starts.
    attested: crate::db::third_party_principals::AttestedKeys,
}

/// Record the grant a consent settled, then answer its tokens — **one owner
/// for every consent-backed grant** (the authorization code and both polled
/// starts), so the record-before-answer ordering and the grant registry's
/// shape cannot differ by the door a ceremony came in by.
///
/// ⚠ The grant is recorded BEFORE the tokens are answered: a crash between the
/// two costs one failed exchange, while answering first would hand out
/// credentials the connected-apps surface cannot see or revoke.
async fn record_and_issue(
    state: &Arc<AppState>,
    client: &ResolvedClient,
    consented: ConsentedGrant<'_>,
    issuer: &str,
    now: i64,
) -> Response {
    // A public client's grant is time-boxed absolutely; a confidential one's
    // session is unlimited while its individual refresh tokens are bounded.
    let grant = OAuthGrant {
        client_id: consented.client_id.to_string(),
        subject: consented.subject.to_string(),
        actor_id: consented.actor_id.to_vec(),
        scopes: consented.scopes.to_vec(),
        dpop_jkt: consented.dpop_jkt.to_string(),
        session_deadline: if client.confidential {
            0
        } else {
            public_session_deadline(now)
        },
    };

    // The grant id is the session family id is the initial refresh `jti` — one
    // identifier joining the connected-apps row, its session, and every
    // rotation.
    let Ok(family_id) = mint_jti() else {
        return oauth_error_response(&OAuthDeny::server("could not issue a grant"));
    };
    let Some(actor) = actor_array(&grant.actor_id) else {
        return oauth_error_response(&OAuthDeny::server("the approved consent names no account"));
    };

    let recorded = crate::bridge_atproto_handlers::record_oauth_grant(
        state,
        &actor,
        &family_id,
        &grant.client_id,
        // The RESOLVED name, never the client's self-asserted string as-is —
        // the same value the consent card showed, so the connected-apps row the
        // user audits later names what they approved.
        client.client_name.as_deref(),
        &grant.scopes,
        // The ceremony's own provenance, not `grant`'s: `OAuthGrant` is shared
        // with the refresh path, which records no grant.
        consented.sets,
        &grant.dpop_jkt,
        refresh_expiry(&grant, now).saturating_mul(1000),
        (grant.session_deadline != 0).then(|| grant.session_deadline.saturating_mul(1000)),
        // THIS authorization server minted it. The mark is what lets a forced
        // session-secret rotation end exactly the grants whose refresh families
        // its re-mint killed: those families are MACed under the nest's own
        // HS256 secret.
        crate::db::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
        // The consent mints (or finds) this document's principal in the same
        // transaction (`third-party.md` § The principal model). Its execution
        // form is derived from how the client authenticates — read off its own
        // document at resolution — never from anything it says about itself.
        // The manifest is the client's own, verified at its resolution — the
        // re-run here only yields the parsed form the row keeps.
        &crate::db::third_party_principals::PrincipalAttestation {
            keys: consented.attested,
            execution_form: crate::db::third_party_principals::ExecutionForm::for_client(
                client.is_remote_form(),
            ),
            manifest: match crate::oauth_as_client::verified_manifest(client) {
                Ok(manifest) => manifest
                    .as_ref()
                    .map(crate::db::third_party_principals::PrincipalManifest::of),
                Err(deny) => return oauth_error_response(&deny),
            },
        },
    )
    .await;
    match recorded {
        Ok(crate::bridge_atproto_handlers::GrantRecorded::Yes) => {}
        Ok(crate::bridge_atproto_handlers::GrantRecorded::ExternalAppsDisabled) => {
            return oauth_error_response(&OAuthDeny::new(
                ERR_INVALID_GRANT,
                "this account is not accepting external application access",
            ));
        }
        // Nothing was recorded, so the code (or the polled handle) is spent and
        // no token exists: the client re-runs the ceremony with a key of its own.
        Ok(crate::bridge_atproto_handlers::GrantRecorded::HolderKeyRefused(refused)) => {
            return oauth_error_response(&OAuthDeny::new(ERR_INVALID_GRANT, refused.to_string()));
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "oauth: record_oauth_grant failed");
            return oauth_error_response(&OAuthDeny::server("could not record the grant"));
        }
    }

    // OIDC (TP6): an `openid` grant's first token reply carries an ID token.
    // Only a consent-backed grant mints one (the code grant and both polled
    // grants, all through here) — OIDC Core §12.2 lets a refresh reply omit
    // it, and the sign-in it asserts happened at the consent, once.
    let id_token = if crate::oauth_as_oidc::grants_openid(&grant.scopes) {
        match crate::oauth_as_oidc::oidc_claims(state, &actor, &grant.scopes).await {
            Ok(identity) => Some(IdTokenRequest {
                identity,
                nonce: consented.nonce.map(str::to_string),
            }),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "oauth: OIDC claim lookup failed");
                return oauth_error_response(&OAuthDeny::server("could not issue tokens"));
            }
        }
    } else {
        None
    };

    issue_tokens(state, &grant, issuer, &family_id, &family_id, id_token, now).await
}

/// What [`issue_tokens`] needs to mint an ID token beside the pair — the
/// scope-gated claims, resolved before the call because resolving them reads
/// the database, and the ceremony's `nonce`.
struct IdTokenRequest {
    identity: crate::oauth_as_token::OidcClaims,
    nonce: Option<String>,
}

/// Rotate an existing grant's token pair.
async fn refresh_token_grant(
    state: &Arc<AppState>,
    form: &HashMap<String, Vec<String>>,
    issuer: &str,
    dpop_jkt: &str,
    now: i64,
) -> Response {
    let presented = first(form, "refresh_token");
    if presented.is_empty() {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "refresh_token is required for the refresh_token grant",
        ));
    }
    let (_, secret) = match signing_material(state).await {
        Ok(pair) => pair,
        Err(deny) => return oauth_error_response(&deny),
    };
    // Plane-checked inside the verifier, so an app-password refresh token
    // presented here is refused and cannot be redeemed for a grant nobody
    // consented to.
    let Some(claims) = verify_refresh_token(&secret, &presented, issuer, now) else {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "refresh token is invalid, expired, or not an OAuth token",
        ));
    };
    // The same key binding the code grant makes: a refresh token carries the
    // thumbprint of the key its grant was bound to (RFC 9449 §5).
    if claims.jkt.is_empty() || claims.jkt != dpop_jkt {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "this request is signed with a different key than the grant was bound to",
        ));
    }
    // A confidential client must still be itself at every rotation, not only at
    // the first exchange — otherwise its credential requirement would apply for
    // one request and never again.
    let client_id = first(form, "client_id");
    if !client_id.is_empty() && client_id != claims.client_id {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "client_id does not match the client this grant was issued to",
        ));
    }
    let client = match resolve_and_authenticate(state, form, &claims.client_id, issuer, now).await {
        Ok(client) => client,
        Err(deny) => return oauth_error_response(&deny),
    };

    let (Some(actor_id), Some(family_id), Some(presented_jti)) =
        (claims.actor_bytes(), claims.sid_bytes(), claims.jti_bytes())
    else {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "refresh token is invalid",
        ));
    };
    let (Some(actor), Ok(new_jti)) = (actor_array(&actor_id), mint_jti()) else {
        return oauth_error_response(&OAuthDeny::server("could not rotate the grant"));
    };

    // Key continuity: a refresh uses the principal without a consent, so the
    // document must still be signed by the key its row pinned — a rotated
    // publisher key is a re-consent, never a silent swap under this grant
    // (`third-party-kinds.md` § The manifest). Refused before the rotation,
    // so the family stays live for the client's next attempt after the user
    // re-consents.
    let pinned = match state
        .db
        .get_third_party_principal_publisher_key(&actor, &claims.client_id)
        .await
    {
        Ok(pinned) => pinned,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "oauth: pinned publisher key read failed");
            return oauth_error_response(&OAuthDeny::server("could not rotate the grant"));
        }
    };
    if let Err(deny) = crate::oauth_as_client::manifest_key_continues(&client, pinned.as_ref()) {
        return oauth_error_response(&deny);
    }

    let grant = OAuthGrant {
        client_id: claims.client_id.clone(),
        subject: claims.sub.clone(),
        actor_id,
        scopes: claims.access_scopes(),
        dpop_jkt: claims.jkt.clone(),
        session_deadline: claims.sexp,
    };

    // Rotation runs through the NEST's own registry — the same rotate-on-use row
    // and the same reuse family-kill the app-credential plane uses. That sharing
    // is the design: a replayed OAuth refresh token must kill its family exactly
    // as a replayed app-password one does.
    let status = match crate::bridge_atproto_handlers::rotate_session(
        state,
        &actor,
        &family_id,
        &presented_jti,
        &new_jti,
        refresh_expiry(&grant, now).saturating_mul(1000),
    )
    .await
    {
        Ok(status) => status,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "oauth: refresh rotation failed");
            return oauth_error_response(&OAuthDeny::server("could not rotate the grant"));
        }
    };
    if status != fauna_protocol::atproto_pds::refresh_status::ROTATED {
        // Every non-rotation answer — reuse detected (the family is now dead),
        // unknown, revoked, expired, kill-switch OFF — is ONE refusal to the
        // client. Distinguishing them would tell a token thief which happened,
        // and the client's remedy is the same in all four: start a new flow.
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            "refresh token is no longer valid",
        ));
    }

    issue_tokens(state, &grant, issuer, &family_id, &new_jti, None, now).await
}

/// Mint the pair and render RFC 6749 §5.1's success response.
async fn issue_tokens(
    state: &Arc<AppState>,
    grant: &OAuthGrant,
    issuer: &str,
    session_id: &[u8],
    refresh_jti: &[u8],
    id_token: Option<IdTokenRequest>,
    now: i64,
) -> Response {
    let (signer, secret) = match signing_material(state).await {
        Ok(pair) => pair,
        Err(deny) => return oauth_error_response(&deny),
    };
    let minted = mint_tokens(
        &signer,
        &secret,
        grant,
        issuer,
        &pds_reader(state),
        session_id,
        refresh_jti,
        now,
    );
    let Ok((access, refresh)) = minted else {
        tracing::error!("oauth: token mint failed");
        return oauth_error_response(&OAuthDeny::server("could not issue tokens"));
    };
    let mut body = serde_json::json!({
        "access_token": access,
        "token_type": TOKEN_TYPE_DPOP,
        "expires_in": access_lifetime_secs(),
        "refresh_token": refresh,
        "scope": join_scope(&grant.scopes),
        "sub": grant.subject,
    });
    if let Some(request) = id_token {
        // Under the SAME signer as the access token — one key, two classes,
        // separated by `typ` (`key-material-hierarchy.md` § Audience:
        // deployment infrastructure → *Issuer signing key*).
        let claims = crate::oauth_as_oidc::id_token_claims(
            issuer,
            &grant.actor_id,
            &grant.client_id,
            request.nonce,
            request.identity,
            now,
        );
        let Ok(token) = crate::oauth_as_token::mint_id_token(&signer, &claims) else {
            tracing::error!("oauth: id token mint failed");
            return oauth_error_response(&OAuthDeny::server("could not issue tokens"));
        };
        body["id_token"] = serde_json::Value::String(token);
    }
    let mut response = Json(body).into_response();
    let h = response.headers_mut();
    h.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    h.insert(
        axum::http::header::PRAGMA,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    response
}

/// `POST /oauth/revoke` — RFC 7009.
///
/// # Revocation destroys the GRANT, never one artifact
///
/// RFC 7009 §2.1 permits either and recommends the cascade; here the cascade is
/// the only honest option, because an access token is a self-contained JWT
/// nothing consults per call — "revoke just this access token" is a request this
/// server could answer `200` to and do nothing about. Either artifact therefore
/// identifies one family and kills it, and because the grant id **is** the
/// session-family id that is [`end_oauth_session`] over that id.
///
/// # Why almost everything answers 200
///
/// RFC 7009 §2.2 already requires `200` for an invalid token, and the reason
/// generalises to every outcome reachable past the DPoP gate: revoked, unknown,
/// expired, minted for the other plane, or bound to a different key. A
/// distinguishable answer would make this a **validity oracle** on token bytes —
/// a thief holding a stolen token but not its DPoP key could still learn the
/// token is live, and learn when the honest client's rotation retires it. The
/// client's remedy is identical in every case.
///
/// The non-`200` answers are all about the **request**: wrong method, a
/// domainless nest, the budget, a failed DPoP gate, a body with no `token`, a
/// confidential client that did not authenticate, and the one genuine failure —
/// the revocation itself erroring, which must not answer `200` because the
/// client would believe it had signed out.
///
/// # Why a DPoP proof is required at all
///
/// RFC 7009 does not ask for one, but without it anyone holding stolen token
/// bytes could sign the honest user's app out — a denial of service reachable
/// with a credential that is otherwise useless. With it, revoking needs the key
/// the grant was bound to, which is what *using* the token needs. `ath` is
/// refused rather than required (the AS-plane rule): the token being revoked
/// travels as a form parameter, not as this request's credential.
/// Takes the whole request for the same reason [`oauth_token`] does — the
/// gate must precede the body read, and an extractor would have read it first.
async fn oauth_revoke(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let headers = parts.headers;
    let Some(issuer) = crate::oauth_issuer_routes::issuer(&state) else {
        return oauth_error_response(&OAuthDeny::unavailable(
            "this nest has not claimed a domain yet, so it is not serving revocations",
        ));
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;
    let nonce = runtime.nonces.mint(now);
    let respond = |response: Response| with_nonce(response, &nonce);

    let htu = oauth_revoke_endpoint_url(state.web_serving_domain());
    let dpop_jkt = match crate::oauth_as_gates::dpop_gate(
        runtime,
        &dpop_proofs(&headers),
        "POST",
        &htu,
        now,
    ) {
        Ok(jkt) => jkt,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };
    let form = match read_form(body, &headers).await {
        Ok(form) => form,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };
    let presented = first(&form, "token");
    if presented.is_empty() {
        return respond(oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "token is required",
        )));
    }

    let (keys, secret) = match verifying_material(&state, now).await {
        Ok(pair) => pair,
        Err(deny) => return respond(oauth_error_response(&deny)),
    };
    // Revocation is this issuer reading its own artifact, not a resource server
    // being addressed by one: an access token it minted must stay revocable
    // whichever readers its grant's families named, so it asks with both.
    let pds = pds_reader(&state);
    let identified = identify_revocable_token(
        &keys,
        &secret,
        &presented,
        &first(&form, "token_type_hint"),
        issuer.as_str(),
        &[pds.as_str(), issuer.as_str()],
        now,
    );
    // Unknown, expired, malformed, from the app plane, or bound to a key other
    // than the one this caller proved. All 200 — the oracle argument above.
    let Some(identified) =
        identified.filter(|t| !t.bound_jkt.is_empty() && t.bound_jkt == dpop_jkt)
    else {
        return respond(revoked());
    };

    // A confidential client must be itself here exactly as at every rotation,
    // or its credential requirement would apply to minting and not to
    // destroying. Resolution failures are refusals about the REQUEST, so unlike
    // the token outcomes above they are reported.
    if let Err(deny) =
        resolve_and_authenticate(&state, &form, &identified.client_id, &issuer, now).await
    {
        return respond(oauth_error_response(&deny));
    }

    let Some(actor) = actor_array(&identified.actor_id) else {
        return respond(revoked());
    };
    if let Err(e) =
        crate::bridge_atproto_handlers::end_oauth_session(&state, &actor, &identified.family_id)
            .await
    {
        // The one case where a live token stays live, so it must not answer
        // 200: the client would believe it had signed out.
        tracing::warn!(error = %format!("{e:#}"), "oauth: revocation failed");
        return respond(oauth_error_response(&OAuthDeny::server(
            "could not revoke the grant",
        )));
    }
    respond(revoked())
}

/// A presented token resolved to the grant it belongs to.
struct RevocableToken {
    actor_id: Vec<u8>,
    family_id: Vec<u8>,
    client_id: String,
    /// The thumbprint the grant was bound to, which the caller compares against
    /// the key actually proved. Returned rather than compared here, so this
    /// function is about identification only.
    bound_jkt: String,
}

/// Resolve a presented token, trying both OAuth token types.
///
/// `token_type_hint` orders the attempts and never limits them (RFC 7009 §2.1:
/// a server that cannot honour the hint must extend its search to the other
/// type), so a client that mislabels its own token still gets it revoked —
/// which is the whole point of the requirement.
///
/// `None` for anything this AS cannot map to one of its own live OAuth grants,
/// including an app-credential-plane token — refused by the plane claim inside
/// the verifier rather than by a check here, so the plane boundary keeps one
/// owner.
fn identify_revocable_token(
    keys: &[IssuerPublicKey],
    secret: &[u8; 32],
    token: &str,
    hint: &str,
    issuer: &str,
    access_readers: &[&str],
    now: i64,
) -> Option<RevocableToken> {
    let as_access = || {
        let claims = verify_access_token(keys, token, issuer, access_readers, now)?;
        Some(RevocableToken {
            actor_id: claims.actor_bytes()?,
            family_id: claims.sid_bytes()?,
            client_id: claims.client_id.clone(),
            bound_jkt: claims.cnf.jkt.clone(),
        })
    };
    let as_refresh = || {
        let claims = verify_refresh_token(secret, token, issuer, now)?;
        Some(RevocableToken {
            actor_id: claims.actor_bytes()?,
            family_id: claims.sid_bytes()?,
            client_id: claims.client_id.clone(),
            bound_jkt: claims.jkt.clone(),
        })
    };
    if hint == "access_token" {
        return as_access().or_else(as_refresh);
    }
    as_refresh().or_else(as_access)
}

/// RFC 7009's success answer: `200` with an empty body.
///
/// Also what every non-revoking token outcome answers, per the oracle argument
/// in [`oauth_revoke`] — ONE writer, so the two cannot be told apart by a header
/// or a body length either.
fn revoked() -> Response {
    let mut response = axum::http::StatusCode::OK.into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Every `DPoP` header value on a request — **all** of them, not the first:
/// two proof headers is a request that means two different things to two
/// different readers, and the gate refuses that outright.
pub(crate) fn dpop_proofs(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(DPOP_PROOF_HEADER)
        .iter()
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .collect()
}

/// Read the posted body — only after the gate, and only up to the cap — and
/// parse the form out of it.
async fn read_form(
    body: axum::body::Body,
    headers: &HeaderMap,
) -> Result<HashMap<String, Vec<String>>, OAuthDeny> {
    let body = axum::body::to_bytes(body, PAR_MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            OAuthDeny::new(
                ERR_INVALID_REQUEST,
                "request body is unreadable or larger than any legitimate token request",
            )
        })?;
    parse_form(headers, &body).ok_or_else(|| {
        OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "request body is not a readable form encoding",
        )
    })
}

/// Resolve a `client_id` and hold it to its own document's authentication
/// method — **authenticate, then authorize**, the slice-5 ruling, which applies
/// at these endpoints for the same reason it applies at PAR: the refusals
/// afterwards describe this client's own flow, and serving them to a caller that
/// has not shown it IS that client makes the endpoint a probe for someone
/// else's exchange.
async fn resolve_and_authenticate(
    state: &Arc<AppState>,
    form: &HashMap<String, Vec<String>>,
    client_id: &str,
    issuer: &str,
    now: i64,
) -> Result<ResolvedClient, OAuthDeny> {
    let runtime = &state.oauth_as;
    let client = crate::oauth_as_client::resolve_client(
        &runtime.clients,
        runtime.fetcher.as_ref(),
        client_id,
        now,
    )
    .await?;
    crate::oauth_as_gates::client_assertion_gate(
        runtime,
        &first(form, "client_assertion_type"),
        &first(form, "client_assertion"),
        &client,
        issuer,
        now,
    )?;
    Ok(client)
}

// ── The polled consent starts (TP9) ──────────────────────────────────────────
//
// `authorization-server.md` § Consent: the typed code (RFC 8628's device
// authorization endpoint) and the quiet push (CIBA, poll mode only). Both are a
// consent START — a request that ends on the one card the browser start
// reaches — and both end with the client polling `/oauth/token` with a handle
// only it holds, bound to the key it proved at the start.

/// What both polled starts have settled before either does its own work.
struct PolledStart {
    issuer: String,
    dpop_jkt: String,
    /// The keys the client attested (`fauna_holder_x25519`,
    /// `fauna_writer_ed25519`), parsed as PAR parses them.
    attested: crate::db::third_party_principals::AttestedKeys,
    client: ResolvedClient,
    request: AcceptedParRequest,
    now: i64,
}

/// The polled starts' shared prologue — **PAR's posture verbatim**, one
/// endpoint over (§ Consent: "The push endpoint inherits PAR's posture
/// verbatim"): the closed-world check, the budget (the layer), a fresh nonce on
/// every response, the DPoP gate before the body is read, the body under the
/// cap, then client resolution, client authentication, and the request held to
/// the client's own document by the same scope decision PAR makes.
///
/// `check_form` runs on the parsed form BEFORE client resolution, so a request
/// a start refuses on its own parameters never makes this nest dial a
/// caller-named host. Every `Err` already carries the nonce.
async fn polled_start_prologue(
    state: &Arc<AppState>,
    request: axum::extract::Request,
    htu: String,
    nonce: &str,
    check_form: impl FnOnce(&HashMap<String, Vec<String>>) -> Result<Option<String>, OAuthDeny>,
) -> Result<PolledStart, Response> {
    let respond = |deny: OAuthDeny| with_nonce(oauth_error_response(&deny), nonce);
    let (parts, body) = request.into_parts();
    let Some(issuer) = crate::oauth_issuer_routes::issuer(state) else {
        return Err(oauth_error_response(&OAuthDeny::unavailable(
            "this nest has not claimed a domain yet, so it is not serving authorization requests",
        )));
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;
    let dpop_jkt =
        crate::oauth_as_gates::dpop_gate(runtime, &dpop_proofs(&parts.headers), "POST", &htu, now)
            .map_err(respond)?;
    let form = read_form(body, &parts.headers).await.map_err(respond)?;
    let client_id = first(&form, "client_id");
    if client_id.is_empty() {
        return Err(respond(OAuthDeny::new(
            ERR_INVALID_REQUEST,
            "client_id is required",
        )));
    }
    let login_hint = check_form(&form).map_err(respond)?;
    // The same key attestations PAR takes (`third-party.md` § The principal
    // model): a device app is exactly the principal that holds keys. Parsed
    // before the client is resolved, so a malformed one costs no fetch.
    let attested = parse_attested_keys(&form).map_err(respond)?;
    let client = resolve_and_authenticate(state, &form, &client_id, &issuer, now)
        .await
        .map_err(respond)?;
    let request = settle_plan(
        state,
        plan_start_request(
            StartRequest {
                client_id,
                scope: first(&form, "scope"),
                login_hint,
            },
            client.clone(),
        ),
    )
    .await
    .map_err(respond)?;
    Ok(PolledStart {
        issuer,
        dpop_jkt,
        attested,
        client,
        request,
        now,
    })
}

/// The consent row's display name for a resolved client — the resolved name,
/// never the client's self-asserted string alone, and none when it is empty.
pub(crate) fn consent_client_name(client: &ResolvedClient) -> Option<&str> {
    client
        .client_name
        .as_deref()
        .filter(|name| !name.is_empty())
}

/// What the card shows beyond the scope list (`third-party-kinds.md` § The
/// record doors): the keys this ceremony attested and the client document's
/// kind manifest, verified at resolution — a client whose manifest did not
/// verify was never resolved, so nothing here can carry one that failed.
pub(crate) fn consent_binding(
    client: &ResolvedClient,
    attested: crate::db::third_party_principals::AttestedKeys,
) -> crate::db::atproto_pds::ConsentBinding {
    crate::db::atproto_pds::ConsentBinding {
        attested,
        fauna_manifest: client.fauna_manifest.clone(),
    }
}

/// The frozen permission-set expansion as the consent row stores it.
pub(crate) fn consent_sets(request: &AcceptedParRequest) -> Vec<ConsentSetInfo> {
    request
        .sets
        .iter()
        .map(|set| ConsentSetInfo {
            nsid: set.nsid.clone(),
            title: set.title.clone(),
            details: set.details.clone(),
            members: set.members.clone(),
            extra: Default::default(),
        })
        .collect()
}

/// A polled start's success answer: `200`, `no-store`, and the nonce.
fn polled_start_answer(body: serde_json::Value, nonce: &str) -> Response {
    let mut response = (axum::http::StatusCode::OK, Json(body)).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    with_nonce(response, nonce)
}

/// The consent window in seconds, as both polled starts advertise it.
fn consent_window_secs() -> i64 {
    CONSENT_REQUEST_TTL_MILLIS / 1000
}

/// `POST /oauth/device_authorization` — the **typed-code** start (RFC 8628
/// §3.1–3.2, the server half).
///
/// Opens a typed-code consent row — unassigned, listed to nobody — whose code
/// is the `user_code` the device displays; the user opens Fauna, types it
/// (`fauna.oauth.consent.lookup_code`), and answers the card. The `device_code`
/// the client polls with is a 256-bit handle bound to the key this request was
/// proved under.
///
/// `verification_uri` is the issuer's own origin — this nest, where the
/// user's Fauna is served. No `verification_uri_complete`: a link carrying the
/// code is RFC 8628 §5.4's remote-phishing aid, and typing the code is the
/// whole of what this start proves.
async fn oauth_device_authorization(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> Response {
    let nonce = state
        .oauth_as
        .nonces
        .mint(fauna_core::data::Timestamp::now_secs_or_zero());
    let htu = oauth_device_authorization_endpoint_url(state.web_serving_domain());
    let started = match polled_start_prologue(&state, request, htu, &nonce, |form| {
        // RFC 8628 defines no hint, and this start resolves its account by the
        // code the user types. A hint would be a parameter the client believes
        // it set and this server ignored.
        if !first(form, "login_hint").is_empty() {
            return Err(OAuthDeny::new(
                ERR_INVALID_REQUEST,
                "the typed-code start takes no login_hint — the user types the code into their own Fauna",
            ));
        }
        Ok(None)
    })
    .await
    {
        Ok(started) => started,
        Err(response) => return response,
    };

    let row = match crate::bridge_atproto_handlers::open_consent_request(
        &state,
        ConsentStart::TypedCode,
        &started.request.client_id,
        consent_client_name(&started.client),
        &started.request.scopes,
        &consent_sets(&started.request),
        &consent_binding(&started.client, started.attested),
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) | Err(_) => {
            return with_nonce(
                oauth_error_response(&OAuthDeny::server("could not open the approval request")),
                &nonce,
            );
        }
    };

    let device_code = mint_handle();
    let expires = started.now.saturating_add(consent_window_secs());
    state.oauth_as.backchannel.put(
        device_code.clone(),
        BackchannelFlow::new(
            BackchannelStart::TypedCode,
            Some(row.consent_id.clone()),
            started.request.client_id.clone(),
            started.dpop_jkt.clone(),
            started.attested,
            expires,
        ),
    );
    polled_start_answer(
        serde_json::json!({
            "device_code": device_code,
            "user_code": row.code,
            "verification_uri": started.issuer,
            "expires_in": consent_window_secs(),
            "interval": BACKCHANNEL_POLL_INTERVAL_SECS,
        }),
        &nonce,
    )
}

/// `POST /oauth/bc-authorize` — the **quiet push** start (CIBA Core §7, poll
/// mode only).
///
/// The account is named by `login_hint`, and **nothing in the answer says
/// whether it resolved** — an unresolved hint and a client the account has
/// blocked both open no row, yet answer with an `auth_req_id` exactly as a
/// real request does and poll `authorization_pending` until it expires. What
/// opens, who is notified and what replaces what are TP9's three rules, decided
/// by the one owner every start opens rows through
/// ([`crate::bridge_atproto_handlers::open_consent_request`]).
///
/// Two CIBA parameters are refused rather than ignored, because a client that
/// sends one believes something happened: `binding_message` (the card shows
/// the nest-minted code, never client-authored text) and `user_code`. The other
/// hint forms (`id_token_hint`, `login_hint_token`) are refused likewise.
async fn oauth_bc_authorize(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
) -> Response {
    let nonce = state
        .oauth_as
        .nonces
        .mint(fauna_core::data::Timestamp::now_secs_or_zero());
    let htu = oauth_bc_authorize_endpoint_url(state.web_serving_domain());
    let started = match polled_start_prologue(&state, request, htu, &nonce, |form| {
        if !first(form, "binding_message").is_empty() {
            return Err(OAuthDeny::new(
                ERR_INVALID_BINDING_MESSAGE,
                "this server does not display client-supplied binding messages — the card shows a \
                 code this nest mints",
            ));
        }
        for unsupported in ["user_code", "id_token_hint", "login_hint_token"] {
            if !first(form, unsupported).is_empty() {
                return Err(OAuthDeny::new(
                    ERR_INVALID_REQUEST,
                    format!("{unsupported} is not supported — name the account with login_hint"),
                ));
            }
        }
        match optional(form, "login_hint") {
            Some(hint) => Ok(Some(hint)),
            None => Err(OAuthDeny::new(
                ERR_INVALID_REQUEST,
                "login_hint is required — it names the account the request is for",
            )),
        }
    })
    .await
    {
        Ok(started) => started,
        Err(response) => return response,
    };
    let Some(login_hint) = started.request.login_hint.clone() else {
        return with_nonce(
            oauth_error_response(&OAuthDeny::new(
                ERR_INVALID_REQUEST,
                "login_hint is required",
            )),
            &nonce,
        );
    };

    let opened = crate::bridge_atproto_handlers::open_consent_request(
        &state,
        ConsentStart::Push {
            login_hint: &login_hint,
            dpop_jkt: &started.dpop_jkt,
            authenticated: started.client.confidential,
        },
        &started.request.client_id,
        consent_client_name(&started.client),
        &started.request.scopes,
        &consent_sets(&started.request),
        &consent_binding(&started.client, started.attested),
    )
    .await;
    let consent_id = match opened {
        Ok(row) => row.map(|row| row.consent_id),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "oauth: open quiet-push consent failed");
            return with_nonce(
                oauth_error_response(&OAuthDeny::server("could not open the approval request")),
                &nonce,
            );
        }
    };

    // ⚠ The answer below is built from constants and a fresh handle only —
    // never from the row — so a request that opened nothing is byte-for-byte
    // shaped like one that did.
    let auth_req_id = mint_handle();
    state.oauth_as.backchannel.put(
        auth_req_id.clone(),
        BackchannelFlow::new(
            BackchannelStart::Push,
            consent_id,
            started.request.client_id.clone(),
            started.dpop_jkt.clone(),
            started.attested,
            started.now.saturating_add(consent_window_secs()),
        ),
    );
    polled_start_answer(
        serde_json::json!({
            "auth_req_id": auth_req_id,
            "expires_in": consent_window_secs(),
            "interval": BACKCHANNEL_POLL_INTERVAL_SECS,
        }),
        &nonce,
    )
}

/// The three polled grants at `/oauth/token` — RFC 8628 §3.4–3.5, CIBA Core
/// §10–11 and the same-device handoff's (`authorization-server.md` § Consent →
/// *How the same-device handoff is built*), one path because all three answer
/// a poll in the same four words.
///
/// The handoff adds two things, in this order: PKCE after the key check (its
/// request began with PAR, and every parameter PAR demanded takes effect), and
/// a handle the user's app has not opened yet — still a live PAR — answering
/// `authorization_pending` under the same client and key checks, unpaced
/// (the window is PAR's own 90 seconds, under the endpoint's budget).
///
/// **The order is the security property.** The handle is looked up (never
/// consumed while pending), the client is held to the one it was issued to and
/// authenticated, and the key binding is checked — all BEFORE the consent is
/// read or the poll is paced, so a caller holding a stolen handle but not its
/// key learns nothing about the ceremony and cannot push the honest client into
/// `slow_down`. Only an approval consumes the handle, and only one poll can
/// consume it: one approval, one set of tokens.
async fn backchannel_grant(
    state: &Arc<AppState>,
    form: &HashMap<String, Vec<String>>,
    start: BackchannelStart,
    issuer: &str,
    dpop_jkt: &str,
    now: i64,
) -> Response {
    let param = match start {
        BackchannelStart::TypedCode => "device_code",
        BackchannelStart::Push => "auth_req_id",
        BackchannelStart::Handoff => "request_uri",
    };
    let handle = first(form, param);
    if handle.is_empty() {
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_REQUEST,
            format!("{param} is required for this grant"),
        ));
    }
    let runtime = &state.oauth_as;
    let flow = match runtime.backchannel.get(&handle, start, now) {
        BackchannelLookup::Live(flow) => flow,
        BackchannelLookup::Expired => {
            return oauth_error_response(&OAuthDeny::new(
                ERR_EXPIRED_TOKEN,
                "the request expired before it was approved — start again",
            ));
        }
        BackchannelLookup::Unknown if start == BackchannelStart::Handoff => {
            // Not opened by the user's app yet — or never pushed, or spent by
            // the browser door. Only the first is worth waiting on.
            return match runtime.par.peek(&handle, now) {
                ParPeek::Live(par) => match hold_poller(
                    state,
                    form,
                    param,
                    PollerBinding {
                        client_id: &par.request.client_id,
                        dpop_jkt: &par.dpop_jkt,
                        code_challenge: Some(&par.request.code_challenge),
                    },
                    issuer,
                    dpop_jkt,
                    now,
                )
                .await
                {
                    Ok(_) => oauth_error_response(&OAuthDeny::new(
                        ERR_AUTHORIZATION_PENDING,
                        "the user has not opened the request in their Fauna app yet",
                    )),
                    Err(deny) => oauth_error_response(&deny),
                },
                ParPeek::Expired => oauth_error_response(&OAuthDeny::new(
                    ERR_EXPIRED_TOKEN,
                    "the request expired before it was opened — start again",
                )),
                ParPeek::Unknown => oauth_error_response(&OAuthDeny::new(
                    ERR_INVALID_GRANT,
                    format!("{param} is unknown, expired, or already redeemed"),
                )),
            };
        }
        BackchannelLookup::Unknown => {
            return oauth_error_response(&OAuthDeny::new(
                ERR_INVALID_GRANT,
                format!("{param} is unknown, expired, or already redeemed"),
            ));
        }
    };

    let client = match hold_poller(
        state,
        form,
        param,
        PollerBinding {
            client_id: &flow.client_id,
            dpop_jkt: &flow.dpop_jkt,
            code_challenge: flow.code_challenge.as_deref(),
        },
        issuer,
        dpop_jkt,
        now,
    )
    .await
    {
        Ok(client) => client,
        Err(deny) => return oauth_error_response(&deny),
    };

    let pending = || match runtime.backchannel.pace(&handle, now) {
        BackchannelPace::InTime => oauth_error_response(&OAuthDeny::new(
            ERR_AUTHORIZATION_PENDING,
            "the user has not answered yet",
        )),
        BackchannelPace::TooSoon => oauth_error_response(&OAuthDeny::new(
            ERR_SLOW_DOWN,
            "polled faster than the interval — the interval is now longer",
        )),
    };
    // A quiet push that opened nothing polls pending until it expires — the
    // answer every request nobody answers gets.
    let Some(consent_id) = flow.consent_id.as_deref() else {
        return pending();
    };
    let resolution =
        match crate::bridge_atproto_handlers::read_consent_state(state, consent_id).await {
            Ok(resolution) => resolution,
            Err(e) => {
                // Transient: the client polls again, and the flow is still live.
                tracing::warn!(error = %format!("{e:#}"), "oauth: polled consent read failed");
                return pending();
            }
        };
    let approved = match resolution {
        ConsentState::Pending => return pending(),
        ConsentState::Denied => {
            runtime.backchannel.take(&handle);
            return oauth_error_response(&OAuthDeny::new(
                ERR_ACCESS_DENIED,
                "the user declined the request",
            ));
        }
        ConsentState::Expired => {
            runtime.backchannel.take(&handle);
            return oauth_error_response(&OAuthDeny::new(
                ERR_EXPIRED_TOKEN,
                "the request expired before it was approved — start again",
            ));
        }
        ConsentState::Approved(approved) => approved,
    };
    if runtime.backchannel.take(&handle).is_none() {
        // A concurrent poll consumed the approval first and holds its tokens.
        return oauth_error_response(&OAuthDeny::new(
            ERR_INVALID_GRANT,
            format!("{param} is unknown, expired, or already redeemed"),
        ));
    }
    let Some(subject) = crate::oauth_as_token::grant_subject(
        &approved.scopes,
        approved.login_did.as_deref(),
        &approved.actor_id,
    ) else {
        // Approved, but the grant carries ATProto scopes and the account holds
        // no ACTIVE ATProto identity to sign in as (an OIDC-only grant needs
        // none). Post-consent, so answering honestly is no oracle.
        return oauth_error_response(&OAuthDeny::new(
            ERR_ACCESS_DENIED,
            "the approving account cannot complete this request",
        ));
    };
    record_and_issue(
        state,
        &client,
        ConsentedGrant {
            client_id: &flow.client_id,
            subject: &subject,
            actor_id: &approved.actor_id,
            scopes: &approved.scopes,
            sets: &approved.sets,
            dpop_jkt: &flow.dpop_jkt,
            nonce: None,
            attested: flow.attested,
        },
        issuer,
        now,
    )
    .await
}

/// What a polled handle was bound to at its start.
struct PollerBinding<'a> {
    client_id: &'a str,
    dpop_jkt: &'a str,
    /// The PAR's S256 challenge — the handoff's alone.
    code_challenge: Option<&'a str>,
}

/// Hold a poller to its handle's binding — **before** anything about the
/// ceremony is read or the poll is paced: the client it was issued to,
/// authenticated; the key it was started under; and, for a start that began
/// with PAR, the `code_verifier` for its challenge.
async fn hold_poller(
    state: &Arc<AppState>,
    form: &HashMap<String, Vec<String>>,
    param: &str,
    binding: PollerBinding<'_>,
    issuer: &str,
    dpop_jkt: &str,
    now: i64,
) -> Result<ResolvedClient, OAuthDeny> {
    let client_id = first(form, "client_id");
    if client_id.is_empty() || client_id != binding.client_id {
        return Err(OAuthDeny::new(
            ERR_INVALID_GRANT,
            format!("client_id does not match the client this {param} was issued to"),
        ));
    }
    let client = resolve_and_authenticate(state, form, &client_id, issuer, now).await?;
    if dpop_jkt != binding.dpop_jkt {
        return Err(OAuthDeny::new(
            ERR_INVALID_GRANT,
            "this request is signed with a different key than the one that started it",
        ));
    }
    if let Some(challenge) = binding.code_challenge {
        pkce_check(form, challenge)?;
    }
    Ok(client)
}

/// The 32-byte actor id a nest call needs, or `None` for anything else. A
/// wrong-width actor is refused rather than padded: it decides which account
/// every call below acts on.
fn actor_array(actor_id: &[u8]) -> Option<[u8; 32]> {
    actor_id.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static(
                "application/x-www-form-urlencoded; charset=utf-8",
            ),
        );
        h
    }

    /// A body that is not a form is refused rather than read as an empty one —
    /// otherwise a client posting JSON would be told `client_id is required`,
    /// which is true and useless.
    #[test]
    fn a_non_form_body_is_not_read_as_an_empty_form() {
        let mut json = HeaderMap::new();
        json.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        assert!(parse_form(&json, &Bytes::from_static(b"{}")).is_none());
        assert!(parse_form(&HeaderMap::new(), &Bytes::from_static(b"a=b")).is_none());
    }

    /// A media type with parameters is still a form.
    #[test]
    fn a_charset_parameter_does_not_make_it_a_different_media_type() {
        let form = parse_form(&form_headers(), &Bytes::from_static(b"client_id=x")).expect("form");
        assert_eq!(first(&form, "client_id"), "x");
    }

    /// `login_hint` is the one parameter where absent and empty differ in the
    /// wire shape and must NOT differ in what the flow does: both mean no hint,
    /// so neither can become a hint of `""`.
    #[test]
    fn an_empty_login_hint_is_no_hint() {
        let form = parse_form(&form_headers(), &Bytes::from_static(b"login_hint=")).expect("form");
        assert_eq!(optional(&form, "login_hint"), None);
        let form = parse_form(&form_headers(), &Bytes::from_static(b"")).expect("form");
        assert_eq!(optional(&form, "login_hint"), None);
        let form =
            parse_form(&form_headers(), &Bytes::from_static(b"login_hint=alice")).expect("form");
        assert_eq!(optional(&form, "login_hint"), Some("alice".to_string()));
    }

    #[test]
    fn an_attested_key_is_thirty_two_bytes_or_absent() {
        use base64::Engine as _;
        let b64 = |raw: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let form = |body: String| parse_form(&form_headers(), &Bytes::from(body)).expect("form");
        assert_eq!(
            parse_attested_keys(&form(String::new())).ok(),
            Some(Default::default())
        );
        let both = parse_attested_keys(&form(format!(
            "{HOLDER_X25519_PARAM}={}&{WRITER_ED25519_PARAM}={}",
            b64(&[9; 32]),
            b64(&[8; 32])
        )))
        .expect("both keys");
        assert_eq!(both.holder_x25519, Some([9; 32]));
        assert_eq!(both.writer_ed25519, Some([8; 32]));
        for param in [HOLDER_X25519_PARAM, WRITER_ED25519_PARAM] {
            for bad in [b64(&[9; 31]), b64(&[9; 33]), "not-base64!".to_string()] {
                let deny = parse_attested_keys(&form(format!("{param}={bad}"))).expect_err(&bad);
                assert_eq!(deny.error, ERR_INVALID_REQUEST);
                assert!(deny.description.contains(param), "{deny:?}");
            }
        }
    }

    /// A duplicated parameter takes its first value rather than concatenating
    /// or picking arbitrarily — the Go original's rule, and the one that makes
    /// a smuggled second `client_id` inert instead of decisive.
    #[test]
    fn a_duplicated_parameter_takes_its_first_value() {
        let form = parse_form(
            &form_headers(),
            &Bytes::from_static(b"client_id=first&client_id=second"),
        )
        .expect("form");
        assert_eq!(first(&form, "client_id"), "first");
    }

    // ── The rotation window ──────────────────────────────────────────────────

    /// `/oauth/token` and `/oauth/revoke` answered by a serving generation the
    /// deployment-seed rotation has not torn down yet mint nothing: their doors
    /// only look the issuer key and the session secret up
    /// (`box-recovery.md` § Deployment-seed rotation → *The bounded hand-off
    /// window*). A mint there would seal both rows under the retired seed, and
    /// the satellite walk would refuse them on every later rotation.
    #[tokio::test]
    async fn the_token_doors_mint_nothing_in_the_rotation_window() {
        use zeroize::Zeroizing;
        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        let public = ed25519_dalek::SigningKey::from_bytes(&a)
            .verifying_key()
            .to_bytes();
        db.set_nest_keypair(&a[..], &public)
            .await
            .expect("seat the deployment keypair");
        let generation = |seed: &[u8; 32]| {
            Arc::new(AppState {
                nest_signing_key: Some(ed25519_dalek::SigningKey::from_bytes(seed)),
                ..AppState::for_test(db.clone())
            })
        };
        let outgoing = generation(&a);

        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        let now = fauna_core::data::Timestamp::now_secs_or_zero();
        let signed = signing_material(&outgoing).await.is_ok();
        let verified = verifying_material(&outgoing, now).await.is_ok();
        for table in ["oauth_issuer_keys", "oauth_session_secret"] {
            let db = db.clone();
            let rows: i64 = tokio::task::spawn_blocking(move || {
                db.conn_blocking()
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .expect("count")
            })
            .await
            .expect("count task");
            assert_eq!(
                rows, 0,
                "a token door inside the rotation window minted {table} under the retired \
                 deployment seed"
            );
        }
        assert!(
            !signed && !verified,
            "nothing is seated, and a read says so rather than filling the gap"
        );

        // The teardown re-enters `start_server`, whose boot step seats both rows
        // under the seed the database holds — the successor's.
        crate::test_support::boot_mint(&db).await;
        let successor = generation(&b);
        assert!(
            signing_material(&successor).await.is_ok()
                && verifying_material(&successor, now).await.is_ok(),
            "the successor generation signs and verifies with what the boot step seated"
        );

        let next = db
            .rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation commits — nothing wedges the satellite walk")
            .expect("and is no rule refusal");
        assert!(
            next.satellites_rekeyed >= 3,
            "the seated rows ride the rotation"
        );
        assert!(
            signing_material(&generation(&c)).await.is_ok(),
            "and still open under the seed after it"
        );
    }

    // ── The consent page and its redirects ───────────────────────────────────

    /// **The escaping assertion, and it is the load-bearing one on this page.**
    /// A client's name and its `client_id` are attacker-authored, and they land
    /// beside the words "is asking to" on a screen whose whole job is telling a
    /// user what they are agreeing to. Markup that survived into it could paint
    /// an entirely different request.
    #[test]
    fn attacker_authored_text_never_renders_as_markup() {
        let data = AuthorizePageData {
            client_name: "<script>alert(1)</script>".into(),
            client_id: "http://localhost?scope=<img src=x onerror=alert(1)>".into(),
            scopes: vec!["\"><b>everything</b>".into()],
            sets: vec![AuthorizePageSet {
                nsid: "com.example.<script>".into(),
                // The SET AUTHOR's text — a different attacker from the client,
                // and no more reviewed.
                title: "</p><h1>Approved!</h1>".into(),
                details: "<iframe src=evil>".into(),
                members: vec!["<b>read everything</b>".into()],
            }],
            code: "ABC-DEF".into(),
            flow_token: "flow-token".into(),
            poll_path: PATH_AUTHORIZE_POLL.into(),
            nonce: "nonce".into(),
        };
        let html = render_page(AUTHORIZE_PAGE, &data).expect("renders");

        for raw in [
            "<script>alert(1)",
            "<img src=x",
            "<b>everything</b>",
            "</p><h1>Approved!</h1>",
            "<iframe src=evil>",
            "<b>read everything</b>",
        ] {
            assert!(
                !html.contains(raw),
                "unescaped attacker markup reached the consent page: {raw}"
            );
        }
        // And the text is still THERE — escaped, not dropped. A consent screen
        // that silently swallowed the identity would be worse than one that
        // showed it awkwardly.
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );
        assert!(html.contains("ABC-DEF"), "the binding code is missing");
    }

    /// The page carries no external resource, so its CSP can forbid every
    /// origin outright. Pinned because a later edit adding a font or an icon
    /// would have to loosen exactly this line, and should have to notice.
    #[test]
    fn the_page_forbids_every_origin_but_its_own_poll() {
        let headers = authorize_page_headers(Some("abc123"));
        let csp = &headers[0].1;
        assert!(csp.contains("default-src 'none'"), "{csp}");
        assert!(csp.contains("script-src 'nonce-abc123'"), "{csp}");
        assert!(csp.contains("connect-src 'self'"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(csp.contains("form-action 'none'"), "{csp}");
        // A page with no script of its own allows none at all.
        let plain = authorize_page_headers(None);
        assert!(plain[0].1.contains("script-src 'none'"), "{}", plain[0].1);
    }

    /// A client may declare a redirect URI that already carries a query, and
    /// RFC 6749 §3.1.2 says it must be retained — so the response is appended
    /// with `&`, never `?`, and never by clobbering.
    #[test]
    fn a_redirect_uri_keeps_the_query_the_client_declared() {
        let appended = append_query("http://127.0.0.1/cb?app=1", &[("code", "xyz")]);
        assert_eq!(appended, "http://127.0.0.1/cb?app=1&code=xyz");
        let fresh = append_query("http://127.0.0.1/cb", &[("code", "xyz")]);
        assert_eq!(fresh, "http://127.0.0.1/cb?code=xyz");
    }

    /// Every parameter is percent-encoded on the way out. A `state` the client
    /// chose is echoed verbatim in meaning, and must not be able to smuggle a
    /// second parameter into its own redirect.
    #[test]
    fn a_response_parameter_cannot_smuggle_another() {
        let appended = append_query(
            "http://127.0.0.1/cb",
            &[("state", "a&code=forged"), ("iss", "https://nest.example")],
        );
        assert!(
            !appended.contains("&code=forged"),
            "a state value forged a parameter: {appended}"
        );
        assert!(appended.contains("state=a%26code%3Dforged"), "{appended}");
    }

    /// A decline and an expiry answer with the SAME error code, and differ only
    /// in the description. A second code would be a distinction no client
    /// branches on — and one more thing for two implementations to disagree
    /// about.
    #[test]
    fn a_decline_and_an_expiry_share_one_error_code() {
        let declined = error_redirect(
            "http://127.0.0.1/cb",
            "csrf",
            "https://nest.example",
            "the user declined the request",
        );
        let expired = error_redirect(
            "http://127.0.0.1/cb",
            "csrf",
            "https://nest.example",
            "the request expired before it was approved",
        );
        for redirect in [&declined, &expired] {
            assert!(redirect.contains("error=access_denied"), "{redirect}");
            assert!(redirect.contains("state=csrf"), "{redirect}");
            assert!(
                redirect.contains("iss=https%3A%2F%2Fnest.example"),
                "RFC 9207's iss must ride every authorization response: {redirect}"
            );
        }
        assert_ne!(declined, expired, "the descriptions must still differ");
    }

    /// The error page renders its fixed strings and carries the same headers as
    /// the consent page — a terminal failure is still a page an attacker would
    /// like to frame.
    #[test]
    fn the_error_page_is_as_locked_down_as_the_consent_page() {
        let response = unusable_request_page();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let csp = response
            .headers()
            .get(axum::http::header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
    }

    /// The minted handle is the RFC's URN form, and two mints never collide.
    #[test]
    fn a_request_uri_is_a_urn_with_full_entropy() {
        let a = mint_request_uri();
        let b = mint_request_uri();
        assert!(a.starts_with(REQUEST_URI_PREFIX), "{a}");
        assert_ne!(a, b);
        // 32 bytes base64url-no-pad is 43 characters.
        assert_eq!(a.len(), REQUEST_URI_PREFIX.len() + 43);
        // The same-device handoff carries exactly this handle to the Fauna app.
        let route = fauna_core::app_route::AppRoute::Consent { request_uri: a };
        assert_eq!(
            fauna_core::app_route::AppRoute::parse(&route.to_uri()),
            Some(route)
        );
    }

    // ── `hold_for_resolution`'s two route-level pins ────────────
    //
    // Both arms below are
    // pinned at the store (`oauth_as_ceremony`'s own tests) but never at the
    // route that actually holds an anonymous caller open. These drive the
    // real endpoint function over a real DB, so a regression in either arm
    // reddens here even though every store-level test stays green.

    use crate::oauth_as_ceremony::CONSENT_WAKE_CAPACITY;
    use fauna_bridge_atproto::oauth_par::AcceptedParRequest;

    /// The route reads the consent through `state.db`, so the fixture is a
    /// real (in-memory) `AppState`, not a bare `OAuthAsRuntime` — nothing here
    /// talks to the network, and `AppState::for_test` wires the runtime's
    /// stores exactly as `start_server` does.
    async fn test_state() -> Arc<AppState> {
        let db = Arc::new(crate::db::CacheDb::open_in_memory().expect("in-memory db"));
        Arc::new(AppState::for_test(db))
    }

    fn stored_par(redirect_uri: &str) -> StoredParRequest {
        StoredParRequest {
            request: AcceptedParRequest {
                client_id: "http://localhost".into(),
                redirect_uri: redirect_uri.into(),
                scopes: vec!["atproto".into()],
                sets: vec![],
                state: "csrf".into(),
                code_challenge: "challenge".into(),
                login_hint: None,
                nonce: None,
            },
            client: ResolvedClient {
                client_id: "http://localhost".into(),
                client_name: None,
                client_uri: None,
                logo_uri: None,
                tos_uri: None,
                policy_uri: None,
                redirect_uris: vec![redirect_uri.into()],
                declared_scopes: vec!["atproto".into()],
                confidential: false,
                jwks: vec![],
                jwks_uri: None,
                loopback: true,
                fauna_manifest: None,
            },
            dpop_jkt: "thumbprint".into(),
            attested: crate::db::third_party_principals::AttestedKeys::default(),
            expires: i64::MAX,
        }
    }

    /// Opens a live, unassigned consent request and a browser flow held on
    /// it — the same two rows `oauth_authorize_page` writes, built directly
    /// so a test can drive `hold_for_resolution` without a full HTTP round
    /// trip.
    async fn open_flow(state: &Arc<AppState>, flow_expires: i64) -> (String, Vec<u8>) {
        let row = crate::bridge_atproto_handlers::open_consent_request(
            state,
            ConsentStart::Browser { login_hint: None },
            "http://localhost",
            None,
            &["atproto".to_string()],
            &[],
            &crate::db::atproto_pds::ConsentBinding::default(),
        )
        .await
        .expect("open consent request")
        .expect("the browser start always opens a row");
        let token = mint_handle();
        state.oauth_as.flows.put(
            token.clone(),
            AuthorizeFlow::new(
                row.consent_id.clone(),
                stored_par("http://127.0.0.1/cb"),
                flow_expires,
            ),
        );
        (token, row.consent_id)
    }

    /// A deadline-poll, never a settle-sleep (`e2e-conventions.md` point
    /// 14): a green run returns on the first pass, and only a genuine
    /// failure spends the budget. Under `start_paused`, the sleep below is
    /// virtual time — it costs nothing real; its only job is giving the
    /// paused clock a timer to auto-advance to when the condition isn't true
    /// yet.
    async fn wait_for(mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("condition never held within the 30s budget");
    }

    /// **The route half of the race the store-side test only proves in
    /// isolation.** `a_wake_between_subscribing_and_waiting_is_still_seen`
    /// (`oauth_as_ceremony`) drives `ConsentWakes` directly — a prior security
    /// review measured that a route-level regression to
    /// read-then-subscribe order (M9) leaves it green, because it never calls
    /// the route at all.
    ///
    /// `subscribe` and the read are two plain, back-to-back synchronous
    /// statements with no `.await` of their own between them, so no amount of
    /// task-scheduling luck (`yield_now`, a short sleep) can land a
    /// resolution strictly between them — there is no gap to schedule into.
    /// This borrows a REAL one instead: `state.db`'s connection lock, which
    /// the read must acquire and `subscribe` never touches at all. Holding it
    /// from the test forces the loop's read to block — after subscribing, if
    /// the order is right (proven by the registration existing while blocked
    /// on a lock this test still holds), or before ever subscribing, if the
    /// order is swapped (in which case the wait below never observes it and
    /// this test fails, correctly). `tokio::sync::Mutex` is documented FIFO,
    /// so once released, the loop's already-queued read is guaranteed to run
    /// — and find the request still pending — before this test's own next
    /// lock request (the resolve two lines down) gets a turn.
    #[tokio::test(start_paused = true)]
    async fn a_resolution_between_the_routes_read_and_its_wait_still_wakes_the_poll() {
        let state = test_state().await;
        let flow_expires = fauna_core::data::Timestamp::now_secs_or_zero() + 3600;
        let (token, consent_id) = open_flow(&state, flow_expires).await;

        // Held across the awaits below on purpose — see the fn doc.
        let guard = state.db.conn().await;

        let held_state = Arc::clone(&state);
        let held_consent_id = consent_id.clone();
        // spawn-ok(test): the held poll this test resolves mid-hold.
        let held = tokio::spawn(async move {
            hold_for_resolution(
                &held_state,
                &token,
                "https://nest.example",
                &held_consent_id,
                flow_expires,
            )
            .await
        });

        // Only reachable if the loop subscribed BEFORE its read blocked on
        // the lock above — under the swapped order this times out instead,
        // failing the test rather than silently passing it.
        wait_for(|| state.oauth_as.consent_wakes.contains(&consent_id)).await;
        drop(guard);

        let actor = [9u8; 32];
        state
            .db
            .resolve_atproto_consent_request(&consent_id, &actor, true)
            .await
            .expect("resolve consent")
            .expect("the row was live");
        state.oauth_as.consent_wakes.wake(&consent_id);

        // A regression to read-then-subscribe order would still eventually
        // answer this poll — just after paying a full POLL_INTERVAL_SECS
        // fallback tick — so the ceiling below, not just the final value, is
        // what actually distinguishes the two orders.
        let outcome = tokio::time::timeout(Duration::from_secs(POLL_INTERVAL_SECS), held)
            .await
            .expect(
                "a resolution landing between the read and the wait must wake the poll \
                 inside one fallback interval, not only after it",
            )
            .expect("the held task did not panic")
            .expect("hold_for_resolution");
        assert!(
            outcome.is_some(),
            "a resolved consent must answer the held poll, not leave it pending"
        );
    }

    /// **M10, corrected.** A single overflow of the wake store is healed by
    /// the loop's own re-subscribe on its very next turn — it costs exactly
    /// one interval and looks identical whether or not the closed-arm fix is
    /// in place. What actually distinguishes the two is repeated eviction:
    /// an attacker who keeps re-flooding the store costs the held poll one
    /// fallback interval PER TURN, never a spin, however many times it
    /// happens — so this evicts the same registration twice in a row and
    /// requires both turns to pay the interval.
    #[tokio::test(start_paused = true)]
    async fn an_evicted_wake_pays_its_fallback_interval_every_turn_not_just_once() {
        let state = test_state().await;
        // Nearer than anything the flood ever inserts below, so the victim is
        // always the eviction target, never one of the flood's own entries.
        let victim_expires = fauna_core::data::Timestamp::now_secs_or_zero() + 3600;
        let (token, consent_id) = open_flow(&state, victim_expires).await;

        // Fill the store to one below its ceiling, so the loop's own first
        // subscribe tips it to capacity with no room left to spare.
        for id in 0..(CONSENT_WAKE_CAPACITY - 1) {
            let _rx = state
                .oauth_as
                .consent_wakes
                .subscribe(&(id as u64).to_le_bytes(), i64::MAX);
        }

        let held_state = Arc::clone(&state);
        let held_consent_id = consent_id.clone();
        // spawn-ok(test): the held poll this test starves of its wake, twice
        // running, then abandons once both turns are measured.
        let held = tokio::spawn(async move {
            hold_for_resolution(
                &held_state,
                &token,
                "https://nest.example",
                &held_consent_id,
                victim_expires,
            )
            .await
        });

        for (flood_id, turn) in (CONSENT_WAKE_CAPACITY as u64..).zip(0..2) {
            // The top of a turn: the loop has (re)subscribed the victim.
            wait_for(|| state.oauth_as.consent_wakes.contains(&consent_id)).await;

            // Evict it — a fresh key, since the store sits at its ceiling and
            // the victim is nearer to expiry than every flood entry here.
            let _rx = state
                .oauth_as
                .consent_wakes
                .subscribe(&flood_id.to_le_bytes(), i64::MAX);
            let evicted_at = tokio::time::Instant::now();

            // The only thing that makes the victim reappear is the loop
            // re-subscribing on its NEXT turn — which the fixed arm reaches
            // only after sleeping a full interval on THIS one.
            wait_for(|| state.oauth_as.consent_wakes.contains(&consent_id)).await;
            let elapsed = tokio::time::Instant::now() - evicted_at;
            assert!(
                elapsed >= Duration::from_secs(POLL_INTERVAL_SECS),
                "turn {turn}: an evicted wake resumed after {elapsed:?}, under one \
                 POLL_INTERVAL_SECS — the loop spun instead of taking its fallback interval"
            );
        }

        held.abort();
    }

    // ── The same-device handoff's poll (`authorization-server.md` § Consent →
    // *How the same-device handoff is built*) ────────────────────────────────

    mod handoff_poll {
        use super::*;

        const HANDLE: &str = "urn:ietf:params:oauth:request_uri:handoff-test-handle";
        const VERIFIER: &str = "a-code-verifier-long-enough-to-be-a-real-one-0123456789";
        const USER: [u8; 32] = [42u8; 32];

        fn challenge_of(verifier: &str) -> String {
            let digest = <sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes());
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
        }

        /// A pushed request as `/oauth/par` stores it, with a real challenge.
        fn push(state: &Arc<AppState>, expires: i64) {
            let mut par = stored_par("http://127.0.0.1/cb");
            par.request.code_challenge = challenge_of(VERIFIER);
            par.expires = expires;
            state.oauth_as.par.put(HANDLE.to_string(), par, 0);
        }

        fn poll_form(verifier: &str) -> HashMap<String, Vec<String>> {
            HashMap::from([
                (
                    "client_id".to_string(),
                    vec!["http://localhost".to_string()],
                ),
                ("request_uri".to_string(), vec![HANDLE.to_string()]),
                ("code_verifier".to_string(), vec![verifier.to_string()]),
            ])
        }

        /// The `error` code a poll answered.
        async fn poll(state: &Arc<AppState>, verifier: &str, dpop_jkt: &str, now: i64) -> String {
            let response = backchannel_grant(
                state,
                &poll_form(verifier),
                BackchannelStart::Handoff,
                "https://nest.example",
                dpop_jkt,
                now,
            )
            .await;
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
            json["error"].as_str().unwrap_or_default().to_string()
        }

        async fn open(state: &Arc<AppState>) -> crate::db::atproto_pds::ConsentRequestRow {
            crate::oauth_consent_handlers::open_handoff(state, USER, HANDLE)
                .await
                .expect("open handoff")
                .expect("a live handle opens")
        }

        async fn seeded() -> Arc<AppState> {
            let state = test_state().await;
            state.db.create_user(&USER, "free", "test").await.unwrap();
            state
        }

        /// Before the user's app opens the handle it is still a live PAR: the
        /// poll waits under the PAR's key and is refused under any other, and
        /// the look-up leaves the handle for the app to spend.
        #[tokio::test]
        async fn a_poll_before_the_open_waits_only_under_the_pars_key() {
            let state = seeded().await;
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            push(&state, now + 90);

            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now).await,
                ERR_AUTHORIZATION_PENDING
            );
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now).await,
                ERR_AUTHORIZATION_PENDING,
                "unpaced — the window is PAR's own"
            );
            assert_eq!(
                poll(&state, VERIFIER, "another-key", now).await,
                ERR_INVALID_GRANT
            );
            assert_eq!(
                poll(
                    &state,
                    "a-wrong-verifier-that-is-also-long-enough-000000",
                    "thumbprint",
                    now
                )
                .await,
                ERR_INVALID_GRANT
            );
            open(&state).await;
        }

        /// A PAR nobody opened in time answers `expired_token` once, then is
        /// unknown.
        #[tokio::test]
        async fn a_handle_that_expired_unopened_answers_expired_once() {
            let state = seeded().await;
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            push(&state, now + 90);
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now + 90).await,
                ERR_EXPIRED_TOKEN
            );
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now + 90).await,
                ERR_INVALID_GRANT
            );
        }

        /// Once opened, the poll is the shared polled path plus PKCE: the
        /// wrong verifier is refused before the ceremony is read, the right one
        /// waits, and a decline answers `access_denied` and spends the handle.
        #[tokio::test]
        async fn an_opened_handoff_polls_under_pkce_and_ends_on_the_users_answer() {
            let state = seeded().await;
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            push(&state, now + 90);
            let row = open(&state).await;

            assert_eq!(
                poll(
                    &state,
                    "a-wrong-verifier-that-is-also-long-enough-000000",
                    "thumbprint",
                    now
                )
                .await,
                ERR_INVALID_GRANT
            );
            assert_eq!(
                poll(&state, VERIFIER, "another-key", now).await,
                ERR_INVALID_GRANT
            );
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now).await,
                ERR_AUTHORIZATION_PENDING
            );

            assert!(
                state
                    .db
                    .resolve_atproto_consent_request(&row.consent_id, &USER, false)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now).await,
                ERR_ACCESS_DENIED
            );
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now).await,
                ERR_INVALID_GRANT,
                "the decline spent the handle"
            );
        }

        /// A handle the browser door spent is unknown to the poll.
        #[tokio::test]
        async fn a_handle_spent_by_the_browser_door_is_unknown() {
            let state = seeded().await;
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            push(&state, now + 90);
            assert!(state.oauth_as.par.take(HANDLE, now).is_some());
            assert_eq!(
                poll(&state, VERIFIER, "thumbprint", now).await,
                ERR_INVALID_GRANT
            );
        }
    }
}
