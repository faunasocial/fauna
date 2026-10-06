//! The OIDC layer (TP6) — the ID token's claims and `/oauth/userinfo`.
//!
//! `docs/goal/behavior/authorization-server.md` § OIDC (TP6) owns the design;
//! this module is the nest half of it. It is **additive on the token
//! endpoint**: an `openid` grant is an ordinary grant of this authorization
//! server — the same PAR, the same consent card, the same DPoP-bound pair — and
//! the only new artifacts are the ID token beside the pair and the one endpoint
//! that answers under the pair's access token.
//!
//! # What each OIDC scope releases, and from where
//!
//! * `openid` — `sub`, the hex actor id: stable across handle and domain
//!   changes, the same to every client (`subject_types_supported: ["public"]`).
//! * `profile` — `preferred_username`, the account's handle.
//! * `email` — the account's canonical mailbox (`<handle>@<mail domain>`, the
//!   row mail-enable writes) and `email_verified: true`, **only while that
//!   mailbox exists and is this account's**. An account with no mailbox gets
//!   no `email` claim — omitted, never invented (OIDC Core §5.3.2).
//!
//! [`oidc_claims`] resolves them ONCE per surface, and both the ID token and
//! `/oauth/userinfo` render from the same [`OidcClaims`] value, so the two
//! surfaces cannot release different facts about one grant.
//!
//! # `/oauth/userinfo` is a resource endpoint on the issuer
//!
//! It is the one route here that takes an **access token** — so it runs the
//! resource-server half of DPoP (RFC 9449 §7): the proof must carry `ath`, the
//! hash of the token presented, and the proving key must be the token's own
//! `cnf.jkt`. A `Bearer` presentation is refused: these tokens are DPoP-bound,
//! and honouring one without its proof would make it a bearer credential after
//! all. It verifies the `iss` the token was minted under and requires this
//! issuer's identifier to be a member of its `aud` — the nest is the resource
//! server here, so a token addressed to the PDS alone is refused — and, like
//! the PDS, it does not consult the grant registry per call: the 15-minute
//! access lifetime is the
//! revocation bound (`authorization-server.md` § As built → *The grant registry
//! does NOT qualify as access-token revocation*).
//!
//! The `typ` separation is enforced by construction: the token is verified by
//! [`crate::oauth_as_token::verify_access_token`], which pins `typ: at+jwt`, so
//! an ID token presented here is refused before its claims are read.

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use fauna_bridge_atproto::authz::{SCOPE_EMAIL, SCOPE_OPENID, SCOPE_PROFILE};
use fauna_bridge_atproto::oauth_metadata::oauth_userinfo_endpoint_url;

use crate::oauth_as_error::{
    DPOP_NONCE_HEADER, ERR_USE_DPOP_NONCE, OAuthDeny, oauth_error_response,
};
use crate::oauth_as_token::{
    IdTokenClaims, OidcClaims, access_lifetime_secs, b64, verify_access_token,
};
use crate::routes::AppState;

/// RFC 6750 §3.1's code for a token this endpoint will not honour — every
/// verification failure collapses into it, so the answer is no oracle about
/// which check failed.
const ERR_INVALID_TOKEN: &str = "invalid_token";
/// RFC 6750 §3.1's code for a valid token whose grant lacks `openid`.
const ERR_INSUFFICIENT_SCOPE: &str = "insufficient_scope";

/// Does this grant carry `openid` — is it an OIDC sign-in at all?
#[must_use]
pub(crate) fn grants_openid(scopes: &[String]) -> bool {
    scopes.iter().any(|s| s == SCOPE_OPENID)
}

/// The scope-gated claims for `actor` under `scopes` — see the module docs for
/// what each scope releases.
///
/// # Errors
///
/// Returns an error if a database read fails. A fact the account simply does
/// not have is `None`, never an error.
pub(crate) async fn oidc_claims(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    scopes: &[String],
) -> anyhow::Result<OidcClaims> {
    let mut claims = OidcClaims::default();
    if scopes.iter().any(|s| s == SCOPE_PROFILE) {
        claims.preferred_username = state.db.get_handle(actor).await?;
    }
    if scopes.iter().any(|s| s == SCOPE_EMAIL)
        && let Some(address) = canonical_mailbox(state, actor).await?
    {
        claims.email = Some(address);
        claims.email_verified = Some(true);
    }
    Ok(claims)
}

/// The account's canonical mailbox address, only while it is routable to this
/// account.
///
/// The address itself is [`crate::bridge_routing_handlers::canonical_address_for_actor`]'s
/// — the single owner of which `<localpart>@<domain>` is canonical, shared with
/// the writer that creates the row and the guard that protects it. This adds
/// the one fact that owner does not decide: that the exact alias row EXISTS and
/// names this actor. Mail-enable writes it; an account that never enabled mail,
/// or whose localpart another actor holds, has no mailbox to assert — and
/// `email_verified: true` on an address that does not deliver to the account
/// would be the one claim here that is false.
async fn canonical_mailbox(
    state: &Arc<AppState>,
    actor: &[u8; 32],
) -> anyhow::Result<Option<String>> {
    let Some((domain, localpart)) =
        crate::bridge_routing_handlers::canonical_address_for_actor(state, actor)
            .await
            .map_err(|e| anyhow::anyhow!("canonical address: {e:?}"))?
    else {
        return Ok(None);
    };
    let owner = state.db.lookup_exact_alias(&domain, &localpart).await?;
    Ok((owner == Some(*actor)).then(|| format!("{localpart}@{domain}")))
}

/// The ID token's claim set.
///
/// `exp` is the ACCESS token's lifetime, read from the shared owner, and that
/// is load-bearing rather than convenient: both classes are signed by the
/// issuer key, and the rotation horizon keeps a retired key verifiable for
/// exactly the longest-lived token it signed. An ID token outliving an access
/// token would outlive the horizon, and fail verification at a relying party
/// that re-checks it after a rotation.
#[must_use]
pub(crate) fn id_token_claims(
    issuer: &str,
    actor_id: &[u8],
    client_id: &str,
    nonce: Option<String>,
    identity: OidcClaims,
    now: i64,
) -> IdTokenClaims {
    IdTokenClaims {
        iss: issuer.to_string(),
        sub: hex::encode(actor_id),
        aud: client_id.to_string(),
        iat: now,
        exp: now.saturating_add(access_lifetime_secs()),
        nonce,
        identity,
    }
}

/// A refusal in the resource-server shape (RFC 6750 §3, RFC 9449 §7.1): the
/// status, a `WWW-Authenticate: DPoP` challenge naming the error, and the fresh
/// DPoP nonce every answer from this endpoint carries.
pub(crate) fn challenge(status: StatusCode, error: &str, nonce: &str) -> Response {
    let mut response = status.into_response();
    let headers = response.headers_mut();
    if let Ok(value) =
        axum::http::HeaderValue::from_str(&format!("DPoP algs=\"ES256\", error=\"{error}\""))
    {
        headers.insert(axum::http::header::WWW_AUTHENTICATE, value);
    }
    if let Ok(value) = axum::http::HeaderValue::from_str(nonce) {
        headers.insert(DPOP_NONCE_HEADER, value);
    }
    response
}

/// The access token of a `DPoP`-scheme `Authorization` header, or `None` for
/// every other shape — a missing header, two of them, and a `Bearer`
/// presentation alike.
fn dpop_access_token(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(axum::http::header::AUTHORIZATION).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("DPoP") && !token.is_empty()).then(|| token.to_string())
}

/// Verify an access token presented at `/oauth/userinfo`.
///
/// This endpoint is the nest acting as a resource server, so it requires **this
/// issuer's identifier** among the token's audiences — and a token addressed to
/// the PDS alone (an ATProto-only grant) is refused here, exactly as the PDS
/// refuses one addressed to the nest alone (`authorization-server.md` § The
/// issuer → *The audience is the set of readers*).
fn verify_userinfo_token(
    keys: &[crate::oauth_issuer_key::IssuerPublicKey],
    token: &str,
    issuer: &str,
    now: i64,
) -> Option<crate::oauth_as_token::AccessClaims> {
    verify_access_token(keys, token, issuer, &[issuer], now)
}

/// `GET`/`POST /oauth/userinfo` — OIDC Core §5.3, behind a DPoP-bound access
/// token.
///
/// # The ORDER
///
/// 1. A domainless nest has no issuer, so no token of its can exist: `503`.
/// 2. A fresh DPoP nonce, on every answer from here on — the resource-server
///    plane must supply the nonces it requires (the Go resource server's
///    reasoning, one server over).
/// 3. The token: a `DPoP` scheme, then a signature under this issuer's key set
///    with this issuer's `iss`, this issuer among its `aud`, and `typ: at+jwt`.
/// 4. The proof, strictly after the token — so a caller that learns the
///    retryable `use_dpop_nonce` has already shown a token this issuer signed,
///    and the answer is never an oracle about a token. `ath` binds the proof to
///    THIS token; the proving key must be the token's `cnf.jkt`.
/// 5. The grant must carry `openid`: `403 insufficient_scope` otherwise.
pub async fn oauth_userinfo(
    State(state): State<Arc<AppState>>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let Some(issuer) = crate::oauth_issuer_routes::issuer(&state) else {
        return oauth_error_response(&OAuthDeny::unavailable(
            "this nest has not claimed a domain yet, so it has no issuer identity",
        ));
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    let runtime = &state.oauth_as;
    let nonce = runtime.nonces.mint(now);
    let refuse = |status: StatusCode, error: &str| challenge(status, error, &nonce);

    let Some(token) = dpop_access_token(&headers) else {
        return refuse(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN);
    };
    let keys = match crate::oauth_as_routes::verifying_material(&state, now).await {
        Ok((keys, _)) => keys,
        Err(deny) => {
            return crate::oauth_as_routes::with_nonce(oauth_error_response(&deny), &nonce);
        }
    };
    let Some(claims) = verify_userinfo_token(&keys, &token, &issuer, now) else {
        return refuse(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN);
    };

    // The hash is of the token THIS request presented — the RFC 9449 §4.3
    // binding that stops a proof captured beside one token riding another.
    let ath = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(token.as_bytes());
        b64(&digest)
    };
    let htu = oauth_userinfo_endpoint_url(state.web_serving_domain());
    let proofs = crate::oauth_as_routes::dpop_proofs(&headers);
    let jkt = match crate::oauth_as_gates::resource_dpop_gate(
        runtime,
        &proofs,
        method.as_str(),
        &htu,
        now,
        ath,
    ) {
        Ok(jkt) => jkt,
        Err(deny) if deny.error == ERR_USE_DPOP_NONCE => {
            return refuse(StatusCode::UNAUTHORIZED, ERR_USE_DPOP_NONCE);
        }
        Err(_) => return refuse(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN),
    };
    if jkt != claims.cnf.jkt {
        return refuse(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN);
    }

    let scopes = crate::oauth_as_token::split_scope(&claims.scope);
    if !grants_openid(&scopes) {
        return refuse(StatusCode::FORBIDDEN, ERR_INSUFFICIENT_SCOPE);
    }
    let Some(actor) = claims
        .actor_bytes()
        .and_then(|a| <[u8; 32]>::try_from(a).ok())
    else {
        return refuse(StatusCode::UNAUTHORIZED, ERR_INVALID_TOKEN);
    };
    let identity = match oidc_claims(&state, &actor, &scopes).await {
        Ok(identity) => identity,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "oauth: userinfo claim lookup failed");
            return crate::oauth_as_routes::with_nonce(
                oauth_error_response(&OAuthDeny::server("could not read this account's claims")),
                &nonce,
            );
        }
    };

    let mut body = serde_json::to_value(&identity).unwrap_or_else(|_| serde_json::json!({}));
    body["sub"] = serde_json::Value::String(hex::encode(actor));
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    crate::oauth_as_routes::with_nonce(response, &nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// Only the `DPoP` scheme presents a token here. A `Bearer` presentation of
    /// a DPoP-bound token is refused (RFC 9449 §7.2), and so is an ambiguous
    /// request carrying two credentials.
    #[test]
    fn only_a_single_dpop_scheme_credential_is_read() {
        assert_eq!(
            dpop_access_token(&headers(&["DPoP abc"])).as_deref(),
            Some("abc")
        );
        assert_eq!(
            dpop_access_token(&headers(&["dpop abc"])).as_deref(),
            Some("abc")
        );
        assert!(dpop_access_token(&headers(&["Bearer abc"])).is_none());
        assert!(dpop_access_token(&headers(&["DPoP "])).is_none());
        assert!(dpop_access_token(&headers(&["DPoP a", "DPoP b"])).is_none());
        assert!(dpop_access_token(&headers(&[])).is_none());
    }

    /// The ID token is addressed to the CLIENT and names the ACTOR, and it
    /// lives exactly as long as an access token — see [`id_token_claims`] for
    /// why that equality is the rotation horizon's, not a convenience.
    #[test]
    fn id_token_claims_address_the_client_and_share_the_access_lifetime() {
        let claims = id_token_claims(
            "https://nest.example",
            &[7u8; 32],
            "https://app.example/client-metadata.json",
            Some("n".into()),
            OidcClaims::default(),
            1_000,
        );
        assert_eq!(claims.aud, "https://app.example/client-metadata.json");
        assert_eq!(claims.sub, hex::encode([7u8; 32]));
        assert_eq!(claims.exp - claims.iat, access_lifetime_secs());
        assert_eq!(claims.nonce.as_deref(), Some("n"));
    }

    /// `/oauth/userinfo` is the nest as a resource server: it honours a token
    /// that names this issuer among its readers, and refuses one addressed to
    /// the PDS alone — an ATProto-only grant's token, signature and all.
    #[test]
    fn userinfo_refuses_a_token_addressed_to_the_pds_alone() {
        use crate::oauth_as_token::{OAuthGrant, mint_tokens};
        use crate::oauth_issuer_key::{IssuerPublicKey, IssuerSigner};

        const ISS: &str = "https://nest.example";
        const PDS: &str = "did:web:pds.nest.example";
        const NOW: i64 = 1_800_000_000;

        let minted = fauna_provisioning::oauth_issuer::mint_issuer_key().expect("mint");
        let signer = IssuerSigner {
            kid: minted.kid.clone(),
            secret_scalar: minted.secret_scalar,
        };
        let keys = vec![IssuerPublicKey {
            kid: minted.kid,
            x: minted.x,
            y: minted.y,
            retired_at: None,
        }];
        let access = |scopes: &[&str]| {
            let grant = OAuthGrant {
                client_id: "https://app.example/client-metadata.json".to_string(),
                subject: "did:plc:abcdefghijklmnopqrstuvwx".to_string(),
                actor_id: vec![7u8; 32],
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                dpop_jkt: "thumbprint".to_string(),
                session_deadline: 0,
            };
            mint_tokens(&signer, &[9u8; 32], &grant, ISS, PDS, b"sid", b"sid", NOW)
                .expect("mint")
                .0
        };

        let pds_only = access(&["atproto", "transition:generic"]);
        assert!(
            verify_access_token(&keys, &pds_only, ISS, &[PDS], NOW + 1).is_some(),
            "the token itself is sound — the PDS would honour it"
        );
        assert!(verify_userinfo_token(&keys, &pds_only, ISS, NOW + 1).is_none());

        assert!(verify_userinfo_token(&keys, &access(&["openid"]), ISS, NOW + 1).is_some());
        assert!(
            verify_userinfo_token(&keys, &access(&["atproto", "openid"]), ISS, NOW + 1).is_some(),
            "a grant spanning both families names both readers"
        );
    }

    #[test]
    fn only_a_grant_naming_openid_is_a_sign_in() {
        assert!(grants_openid(&["atproto".into(), "openid".into()]));
        assert!(!grants_openid(&["profile".into(), "email".into()]));
    }
}
