//! The authorization server's token plane — the ES256 access token the
//! ecosystem verifies against `/oauth/jwks`, and the HS256 refresh token that
//! rotates through the nest's own session registry.
//!
//! Ported from the bridge's since-retired `oauth_token.go` (TP5 S2c).
//! Every claim, every pinned header field and every refusal is the Go
//! original's; what changed is who holds the keys and, as a consequence, what
//! two of the claims say.
//!
//! # Two signers, and the split is the AS-key rotation ruling
//!
//! `docs/goal/behavior/authorization-server.md` § As built: an access token is
//! **ES256 under the issuer key** because it is a public artifact any resource
//! server may verify against the JWKS; a refresh token is **HS256 under the
//! session secret** because it is presented back to this server and to nothing
//! else. ⚠ Never unify them onto the issuer key "for symmetry" — that is what
//! makes rotation impossible, because the outgoing key would have to stay
//! verifiable for the refresh lifetime (180 days) instead of for minutes.
//!
//! The HS256 half is [`crate::oauth_session_secret`], which is the nest's own
//! and NOT the bridge's — § The issuer → *Two HS256 secrets, not one*.
//!
//! # `iss` and `aud` follow the artifact's real reader
//!
//! The bridge minted both classes under one identity because one host was
//! authorization server and resource server at once. On the nest they part, and
//! § The issuer ratifies the split:
//!
//! * **Access token** — `iss` is this nest (the AS that minted it), `aud` is the
//!   **set of its readers**: RFC 9068 makes `aud` the resource server, and the
//!   grant's scope families decide which there are — the PDS service DID for an
//!   ATProto-family scope, this nest's issuer identifier for an OIDC or
//!   Fauna-family one ([`Audience`]; § The issuer → *The audience is the set of
//!   readers*).
//! * **Refresh token** — `aud` is this nest, because it comes back to
//!   `/oauth/token` and `/oauth/revoke` and goes nowhere else. That also means
//!   an app-plane token can never verify here even if the two HS256 secrets
//!   were ever merged: a structural interlock beside the plane claim.
//!
//! The PDS resource server accepts what this plane mints because the nest
//! feeds it both halves it pins — the issuer identifier and the public key set
//! (`fauna.bridges.atproto.fetch_issuer_jwks`; § The issuer → *The teaching is
//! one WS-RPC feed carrying both halves*).
//!
//! # The lifetimes are read, never declared here
//!
//! [`fauna_provisioning::oauth_issuer`] owns all three
//! (`ACCESS_TOKEN_LIFETIME_SECS`, `REFRESH_TOKEN_LIFETIME_SECS`,
//! `PUBLIC_SESSION_LIFETIME_SECS`). A copy here would be the drift the horizon
//! ruling exists to prevent, one endpoint over.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use fauna_provisioning::oauth_issuer::{
    ACCESS_TOKEN_LIFETIME_SECS, PUBLIC_SESSION_LIFETIME_SECS, REFRESH_TOKEN_LIFETIME_SECS,
};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::oauth_issuer_key::{IssuerPublicKey, IssuerSigner};

/// The session plane an OAuth grant's tokens belong to.
///
/// ⚠ Kept even though this nest's OAuth refresh tokens are MACed under a secret
/// the app plane's are not, so a cross-plane token already fails its MAC here.
/// An interlock that held only while two secrets stayed unmerged would be a
/// promise about the future rather than a property of a token: the claim is
/// required by name, and a token naming another plane — or none — is refused
/// (the bridge's app-plane verifiers refuse an absent claim the same way).
pub const PLANE_OAUTH: &str = "oauth";

/// The app-credential plane — the bridge's own session tokens. Never accepted
/// here; named so the refusal can be pinned.
pub const PLANE_APP_CREDENTIAL: &str = "app_credential";

/// The JOSE `typ` of an OAuth access token (RFC 9068).
///
/// Pinned on both the mint and the verify. It is inside the signed input, so a
/// token minted as one class cannot verify as the other at a verifier that
/// checks it — the rule-#8 separation
/// `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment
/// infrastructure → *Issuer signing key* relies on for one key signing two JWT
/// classes.
pub const ACCESS_TOKEN_TYP: &str = "at+jwt";

/// The JOSE `typ` of an OIDC ID token — plain `JWT`, as OIDC Core issues it.
///
/// The other half of the rule-#8 separation [`ACCESS_TOKEN_TYP`] describes: the
/// same issuer key signs both classes, and each verifier pins its own `typ`, so
/// an ID token presented as an access token fails the access verifier's header
/// check before its claims are even read.
pub const ID_TOKEN_TYP: &str = "JWT";

/// The `token_type` of the token response. Never `Bearer`: these tokens are
/// DPoP-bound, and a client told `Bearer` would present one without a proof —
/// which the resource server refuses, leaving the client broken for a reason
/// its own code never chose.
pub const TOKEN_TYPE_DPOP: &str = "DPoP";

/// The refresh token's `scope`, which is what makes an access token presented
/// at `/oauth/token` fail rather than rotate.
pub const SCOPE_REFRESH: &str = "com.atproto.refresh";

/// The fixed width of a P-256 scalar in the JOSE encodings — both signature
/// halves and both JWKS coordinates.
const P256_COORD_BYTES: usize = 32;

/// The `jti` / session-family granularity: 16 random bytes.
const JTI_BYTES: usize = 16;

/// Everything about a grant that both token classes are minted from.
///
/// One struct for both the code and the refresh path, which is why it carries
/// no provenance (`sets`, the consent row): the refresh path records no grant
/// and would have nothing to put there. The code path reads provenance off the
/// redeemed code directly.
#[derive(Debug, Clone)]
pub struct OAuthGrant {
    pub client_id: String,
    /// The access token's (and refresh token's) `sub`.
    ///
    /// The account's real DID for a grant carrying any ATProto-family scope —
    /// the PDS resource server reads `sub` as the repo's DID. For an OIDC-only
    /// sign-in, which reaches nothing at the PDS and needs no ATProto identity,
    /// it is the hex actor id: the same subject the ID token names
    /// (`authorization-server.md` § OIDC (TP6)). See [`grant_subject`].
    pub subject: String,
    pub actor_id: Vec<u8>,
    pub scopes: Vec<String>,
    /// The RFC 7638 thumbprint the flow proved possession of; the access
    /// token's `cnf.jkt` and the refresh token's `jkt`.
    pub dpop_jkt: String,
    /// A public client's absolute session deadline in epoch seconds, or `0` for
    /// a confidential client's unlimited session.
    pub session_deadline: i64,
}

/// RFC 9449 §6's confirmation claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cnf {
    pub jkt: String,
}

/// The claim set of an ES256 access token.
///
/// `cnf.jkt` is the binding this whole plane exists to carry: the thumbprint of
/// the key the client proved possession of at PAR, which a resource server
/// re-checks against the DPoP proof accompanying every call. A token without it
/// is a bearer token wearing a DPoP costume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    pub iss: String,
    pub sub: String,
    pub aud: Audience,
    pub scope: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    pub cnf: Cnf,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    /// The hex 32-byte Fauna actor id this token authenticates. Its own claim
    /// because `sub` is the account's real DID, and deriving the actor back out
    /// of a DID string is what once put the write path and the projection loop
    /// in two different repos.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fauna_actor: String,
}

/// An access token's `aud`: the resource servers it may be presented at.
///
/// On the wire **one reader is a string and any other count an array** (RFC
/// 7519 §4.1.3 allows both), so an ATProto-only token is byte for byte what it
/// was when the PDS was the only reader. Either spelling parses.
///
/// A reader asks [`Audience::names_any`] with its own identifier and ignores
/// the other members; nothing compares the set for equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audience(pub Vec<String>);

impl Audience {
    /// Does this token name at least one of `readers`?
    #[must_use]
    pub fn names_any(&self, readers: &[&str]) -> bool {
        self.0.iter().any(|a| readers.contains(&a.as_str()))
    }
}

impl Serialize for Audience {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => serializer.serialize_str(one),
            many => many.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Audience {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            One(String),
            Many(Vec<String>),
        }
        Ok(Self(match Wire::deserialize(deserializer)? {
            Wire::One(one) => vec![one],
            Wire::Many(many) => many,
        }))
    }
}

/// The claim set of an OIDC ID token (TP6, `authorization-server.md` § OIDC).
///
/// `aud` is the **client** — its document URL, the `client_id` — because an ID
/// token is an assertion *to* the relying party, not a credential for a
/// resource server; `sub` is the hex actor id, stable across handle and domain
/// changes. The optional members appear only when their scope was granted and
/// the fact exists: a claim this account does not have is omitted, never
/// invented (OIDC Core §5.3.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdTokenClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    /// The PAR's `nonce`, verbatim — what lets the client tell this token was
    /// minted for its own sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(flatten)]
    pub identity: OidcClaims,
}

/// The scope-gated OIDC claims — the members the ID token and
/// `/oauth/userinfo` BOTH carry, from one value, so the two surfaces cannot
/// release different facts about the same grant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcClaims {
    /// `profile`: the account's handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_username: Option<String>,
    /// `email`: the account's canonical mailbox, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Present exactly when `email` is. Always `true`: the address is this
    /// nest's own canonical mailbox for the account, so the issuer is the
    /// authority that delivers to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_verified: Option<bool>,
}

/// The `sub` a grant's access and refresh tokens carry — see
/// [`OAuthGrant::subject`].
///
/// `None` when the grant carries an ATProto-family scope and the account has no
/// active ATProto identity: that grant cannot be honoured at the PDS, which
/// is the only place its ATProto scopes grant anything.
#[must_use]
pub fn grant_subject(
    scopes: &[String],
    login_did: Option<&str>,
    actor_id: &[u8],
) -> Option<String> {
    let atproto_family = scopes
        .iter()
        .any(|s| fauna_bridge_atproto::authz::is_atproto_family_scope(s));
    if atproto_family {
        login_did
            .filter(|did| !did.is_empty())
            .map(ToString::to_string)
    } else {
        Some(hex::encode(actor_id))
    }
}

/// The claim set of an HS256 refresh token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshClaims {
    pub sub: String,
    pub aud: String,
    pub scope: String,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    pub sid: String,
    /// The ORIGINAL access-token scope, so a rotation re-mints the same set
    /// without re-consulting the consent row.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ascope: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fauna_actor: String,
    /// See [`PLANE_OAUTH`]. Required: an absent claim is refused.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plane: String,
    /// The RFC 7638 thumbprint this grant is DPoP-bound to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub jkt: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    /// The grant's ABSOLUTE session deadline (epoch seconds), carried across
    /// rotations so it cannot be extended by rotating. Absent (`0`) for a
    /// confidential client.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sexp: i64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &i64) -> bool {
    *v == 0
}

impl RefreshClaims {
    /// The 32-byte actor id, or `None` for a token that carries no well-formed
    /// one. Refused rather than defaulted: the actor id decides which account
    /// every nest-facing call acts on, so guessing it is unthinkable.
    #[must_use]
    pub fn actor_bytes(&self) -> Option<Vec<u8>> {
        let b = hex::decode(&self.fauna_actor).ok()?;
        (b.len() == 32).then_some(b)
    }

    /// The session-family id.
    #[must_use]
    pub fn sid_bytes(&self) -> Option<Vec<u8>> {
        decode_id(&self.sid)
    }

    /// This token's own id — the value a rotation presents as spent.
    #[must_use]
    pub fn jti_bytes(&self) -> Option<Vec<u8>> {
        decode_id(&self.jti)
    }

    /// The access-token scope a rotation should re-mint.
    ///
    /// ⚠ **A deliberate divergence from the Go original, and the one place this
    /// port does not copy it.** `Claims.AccessScope` falls back to the app-pass
    /// scope when `ascope` is empty, which is right *there* because one minter
    /// served both planes and an app-plane token legitimately carries none.
    /// Here the plane check has already refused every app-plane token, so an
    /// empty `ascope` can only mean a malformed one — and defaulting it would
    /// re-mint a grant under a scope **nobody consented to**, which is the
    /// exact laundering the plane claim exists to stop. An empty scope set is
    /// useless to its holder, which is the correct outcome for a token that
    /// should not have verified in the first place.
    #[must_use]
    pub fn access_scopes(&self) -> Vec<String> {
        split_scope(&self.ascope)
    }
}

impl AccessClaims {
    /// The 32-byte actor id this token authenticates, if well-formed.
    #[must_use]
    pub fn actor_bytes(&self) -> Option<Vec<u8>> {
        let b = hex::decode(&self.fauna_actor).ok()?;
        (b.len() == 32).then_some(b)
    }

    /// The session-family id, if well-formed.
    #[must_use]
    pub fn sid_bytes(&self) -> Option<Vec<u8>> {
        decode_id(&self.sid)
    }
}

fn decode_id(s: &str) -> Option<Vec<u8>> {
    let b = B64.decode(s).ok()?;
    (!b.is_empty()).then_some(b)
}

/// Space-joined, the one spelling the OAuth `scope` parameter uses and the same
/// one the consent row stores.
#[must_use]
pub fn join_scope(scopes: &[String]) -> String {
    scopes.join(" ")
}

/// The inverse of [`join_scope`]. Empty segments are dropped, so a doubled
/// space cannot produce an empty scope nobody granted.
#[must_use]
pub fn split_scope(scope: &str) -> Vec<String> {
    scope
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Mint 16 random bytes — the `jti` and session-family granularity.
///
/// # Errors
///
/// Returns an error if the platform RNG fails.
pub fn mint_jti() -> anyhow::Result<Vec<u8>> {
    let mut b = vec![0u8; JTI_BYTES];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("mint jti: {e}"))?;
    Ok(b)
}

/// Base64url (no padding), the JOSE encoding everything here uses.
#[must_use]
pub fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

fn json_b64<T: Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(B64.encode(serde_json::to_vec(value)?))
}

/// The JOSE header of an ES256 token this AS mints.
///
/// `kid` is present from the very first token, and that is the AS-key-rotation
/// ruling's day-one half: a verifier that has always been handed a `kid` can be
/// pointed at a key SET the day rotation lands, whereas one that learned to
/// expect a single implicit key could not be, and every token in flight would
/// have to break to teach it.
#[derive(Serialize, Deserialize)]
struct JoseHeader {
    alg: String,
    typ: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    kid: String,
}

/// Sign an ES256 access token with the issuer key.
///
/// # Errors
///
/// Returns an error if the stored scalar is not a valid P-256 signing key, or
/// if the claims do not serialise.
pub fn mint_access_token(signer: &IssuerSigner, claims: &AccessClaims) -> anyhow::Result<String> {
    sign_es256(signer, ACCESS_TOKEN_TYP, claims)
}

/// Sign an ES256 OIDC ID token with the issuer key — the same key as the
/// access token, separated from it by [`ID_TOKEN_TYP`] inside the signed bytes.
///
/// # Errors
///
/// As [`mint_access_token`].
pub fn mint_id_token(signer: &IssuerSigner, claims: &IdTokenClaims) -> anyhow::Result<String> {
    sign_es256(signer, ID_TOKEN_TYP, claims)
}

/// The one ES256 signer every JWT class goes through, so the only thing that
/// can differ between them is the `typ` each caller names — the two token
/// classes here and the events webhook's security event token
/// (`crate::events_webhook`).
pub(crate) fn sign_es256<T: Serialize>(
    signer: &IssuerSigner,
    typ: &str,
    claims: &T,
) -> anyhow::Result<String> {
    use p256::ecdsa::signature::Signer as _;

    let header = JoseHeader {
        alg: "ES256".to_string(),
        typ: typ.to_string(),
        kid: signer.kid.clone(),
    };
    let signing_input = format!("{}.{}", json_b64(&header)?, json_b64(claims)?);
    let key = p256::ecdsa::SigningKey::from_bytes(&signer.secret_scalar.into())
        .map_err(|e| anyhow::anyhow!("the stored issuer scalar is not a signing key: {e}"))?;
    // Fixed-width r‖s, never ASN.1: JOSE's ES256 is the raw concatenation, and
    // a DER signature here is the shape every ecosystem verifier rejects.
    let signature: p256::ecdsa::Signature = key.sign(signing_input.as_bytes());
    Ok(format!("{signing_input}.{}", b64(&signature.to_bytes())))
}

/// Verify an ES256 access token minted by this AS.
///
/// Key selection is **by `kid` over the served set**, never "the current key":
/// that is what makes the verify path already correct on the day a second key
/// exists, rather than something a rotation would have to change while tokens
/// are in flight. An unknown `kid` resolves to no key — never a fallback to the
/// current one.
///
/// `readers` is who is asking: the token must name **at least one** of them in
/// its `aud`, and its other members are ignored. A resource server passes its
/// own identifier alone (`/oauth/userinfo` passes this issuer's); `/oauth/revoke`
/// passes every reader this issuer mints for, because there the issuer is
/// identifying its own artifact rather than being addressed by it.
///
/// Returns `None` for every failure. The distinction is deliberately not
/// reported: both callers collapse every token failure into one answer —
/// `/oauth/revoke`'s uniform `200` and `/oauth/userinfo`'s `401 invalid_token` —
/// so as not to be a validity oracle.
#[must_use]
pub fn verify_access_token(
    keys: &[IssuerPublicKey],
    token: &str,
    issuer: &str,
    readers: &[&str],
    now: i64,
) -> Option<AccessClaims> {
    use p256::ecdsa::signature::Verifier as _;

    let (header_b64, claims_b64, signature_b64) = split3(token)?;
    let header: JoseHeader = serde_json::from_slice(&B64.decode(header_b64).ok()?).ok()?;
    // `alg` is pinned rather than read: an attacker-chosen algorithm is the
    // oldest JWT break there is. `typ` is pinned so a token minted for another
    // purpose under this key cannot be replayed as an access token.
    if header.alg != "ES256" || header.typ != ACCESS_TOKEN_TYP {
        return None;
    }
    let key = keys.iter().find(|k| k.kid == header.kid)?;
    let x = B64.decode(&key.x).ok()?;
    let y = B64.decode(&key.y).ok()?;
    if x.len() != P256_COORD_BYTES || y.len() != P256_COORD_BYTES {
        return None;
    }
    let signature = B64.decode(signature_b64).ok()?;
    if signature.len() != 2 * P256_COORD_BYTES {
        return None;
    }
    let point = p256::EncodedPoint::from_affine_coordinates(
        x.as_slice().into(),
        y.as_slice().into(),
        false,
    );
    let verifying = p256::ecdsa::VerifyingKey::from_encoded_point(&point).ok()?;
    let signature = p256::ecdsa::Signature::from_slice(&signature).ok()?;
    let signing_input = format!("{header_b64}.{claims_b64}");
    verifying
        .verify(signing_input.as_bytes(), &signature)
        .ok()?;

    let claims: AccessClaims = serde_json::from_slice(&B64.decode(claims_b64).ok()?).ok()?;
    if claims.exp <= now || claims.iat > now + 60 {
        return None;
    }
    // `iss` so a token minted by a different authorization server cannot be
    // presented here; `aud` so one minted for a different service by THIS
    // server cannot be either.
    if claims.iss != issuer || !claims.aud.names_any(readers) {
        return None;
    }
    if claims.cnf.jkt.is_empty() {
        return None;
    }
    Some(claims)
}

/// The pinned HS256 header. Comparing the encoded header for equality rejects
/// `alg` confusion (`none`, `RS256`, …) by construction rather than by parsing
/// an attacker's choice and then deciding whether to honour it.
fn hs256_header() -> String {
    B64.encode(br#"{"alg":"HS256","typ":"JWT"}"#)
}

/// Sign an HS256 refresh token under the nest's own OAuth session secret.
///
/// # Errors
///
/// Returns an error if the claims do not serialise.
pub fn mint_refresh_token(secret: &[u8; 32], claims: &RefreshClaims) -> anyhow::Result<String> {
    let signing_input = format!("{}.{}", hs256_header(), json_b64(claims)?);
    Ok(format!(
        "{signing_input}.{}",
        b64(&mac(secret, &signing_input))
    ))
}

fn mac(secret: &[u8; 32], signing_input: &str) -> Vec<u8> {
    let mut m = <Hmac<Sha256>>::new_from_slice(secret).expect("HMAC accepts any key length");
    m.update(signing_input.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Verify a refresh token presented at `/oauth/token` or `/oauth/revoke`.
///
/// Requires the OAuth plane, so an app-password refresh token cannot be
/// redeemed for a grant nobody consented to; requires the refresh scope and a
/// session id, so an access token presented here fails; and enforces the
/// grant's absolute session deadline independently of this token's own `exp`,
/// so rotating cannot extend a session past it.
///
/// Returns `None` for every failure — see [`verify_access_token`] for why the
/// reason is not reported.
#[must_use]
pub fn verify_refresh_token(
    secret: &[u8; 32],
    token: &str,
    audience: &str,
    now: i64,
) -> Option<RefreshClaims> {
    let (header_b64, claims_b64, signature_b64) = split3(token)?;
    if header_b64 != hs256_header() {
        return None;
    }
    let signing_input = format!("{header_b64}.{claims_b64}");
    let presented = B64.decode(signature_b64).ok()?;
    // Constant-time: a byte-by-byte compare here is a timing oracle on a MAC.
    // The workspace's own primitive, not a second one — `fauna_core::secret`
    // is where every such compare in this binary already goes.
    let expected = mac(secret, &signing_input);
    if !fauna_core::secret::constant_time_eq(&expected, &presented) {
        return None;
    }
    let claims: RefreshClaims = serde_json::from_slice(&B64.decode(claims_b64).ok()?).ok()?;
    if claims.exp <= now || claims.iat > now + 60 {
        return None;
    }
    if claims.aud != audience {
        return None;
    }
    if claims.scope != SCOPE_REFRESH || claims.sid.is_empty() {
        return None;
    }
    if claims.plane != PLANE_OAUTH {
        return None;
    }
    if claims.sexp != 0 && claims.sexp <= now {
        return None;
    }
    Some(claims)
}

fn split3(token: &str) -> Option<(&str, &str, &str)> {
    let mut parts = token.split('.');
    let a = parts.next()?;
    let b = parts.next()?;
    let c = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

/// When the refresh token being minted expires: the individual-token cap,
/// clamped by the grant's absolute session deadline where there is one.
///
/// Clamping rather than ignoring is what keeps a public client's last refresh
/// token from outliving the session it belongs to.
#[must_use]
pub fn refresh_expiry(grant: &OAuthGrant, now: i64) -> i64 {
    let own = now.saturating_add(i64::try_from(REFRESH_TOKEN_LIFETIME_SECS).unwrap_or(i64::MAX));
    if grant.session_deadline != 0 && grant.session_deadline < own {
        grant.session_deadline
    } else {
        own
    }
}

/// A public client's absolute session deadline, from the instant of consent.
#[must_use]
pub fn public_session_deadline(now: i64) -> i64 {
    now.saturating_add(i64::try_from(PUBLIC_SESSION_LIFETIME_SECS).unwrap_or(i64::MAX))
}

/// How long an access token this issuer mints stays valid, in seconds — read
/// from the owner, never declared here.
#[must_use]
pub fn access_lifetime_secs() -> i64 {
    i64::try_from(ACCESS_TOKEN_LIFETIME_SECS).unwrap_or(i64::MAX)
}

/// The minted pair for one grant.
///
/// `session_id` is the grant family id: the grant's own id nest-side, the
/// `atproto_sessions.session_id`, and the initial refresh `jti` all at once, so
/// one identifier joins the connected-apps row to the family its rotations
/// walk.
///
/// The access token's `aud` is derived here, from the grant's scopes, by the
/// one shared function that decides it — a caller supplies the two identifiers
/// and cannot supply an audience.
///
/// # Errors
///
/// Returns an error if signing fails, or if the grant's scopes name no reader:
/// a token nothing accepts is not minted.
pub fn mint_tokens(
    signer: &IssuerSigner,
    secret: &[u8; 32],
    grant: &OAuthGrant,
    issuer: &str,
    pds_service_did: &str,
    session_id: &[u8],
    refresh_jti: &[u8],
    now: i64,
) -> anyhow::Result<(String, String)> {
    let audience =
        fauna_bridge_atproto::authz::access_token_audience(&grant.scopes, pds_service_did, issuer);
    anyhow::ensure!(
        !audience.is_empty(),
        "the grant's scopes name no resource server"
    );
    let access_jti = mint_jti()?;
    let access = mint_access_token(
        signer,
        &AccessClaims {
            iss: issuer.to_string(),
            sub: grant.subject.clone(),
            aud: Audience(audience),
            scope: join_scope(&grant.scopes),
            iat: now,
            exp: now.saturating_add(access_lifetime_secs()),
            jti: b64(&access_jti),
            cnf: Cnf {
                jkt: grant.dpop_jkt.clone(),
            },
            sid: b64(session_id),
            client_id: grant.client_id.clone(),
            fauna_actor: hex::encode(&grant.actor_id),
        },
    )?;
    let refresh = mint_refresh_token(
        secret,
        &RefreshClaims {
            sub: grant.subject.clone(),
            // The refresh token comes back HERE, so this nest is its audience —
            // not the resource server the access token names.
            aud: issuer.to_string(),
            scope: SCOPE_REFRESH.to_string(),
            iat: now,
            exp: refresh_expiry(grant, now),
            jti: b64(refresh_jti),
            sid: b64(session_id),
            ascope: join_scope(&grant.scopes),
            fauna_actor: hex::encode(&grant.actor_id),
            plane: PLANE_OAUTH.to_string(),
            jkt: grant.dpop_jkt.clone(),
            client_id: grant.client_id.clone(),
            sexp: grant.session_deadline,
        },
    )?;
    Ok((access, refresh))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISS: &str = "https://nest.example";
    const AUD: &str = "did:web:pds.nest.example";
    const NOW: i64 = 1_800_000_000;

    fn signer() -> (IssuerSigner, Vec<IssuerPublicKey>) {
        let minted = fauna_provisioning::oauth_issuer::mint_issuer_key().expect("mint");
        let signer = IssuerSigner {
            kid: minted.kid.clone(),
            secret_scalar: minted.secret_scalar,
        };
        let public = vec![IssuerPublicKey {
            kid: minted.kid,
            x: minted.x,
            y: minted.y,
            retired_at: None,
        }];
        (signer, public)
    }

    fn grant() -> OAuthGrant {
        OAuthGrant {
            client_id: "https://app.example/client-metadata.json".to_string(),
            subject: "did:plc:abcdefghijklmnopqrstuvwx".to_string(),
            actor_id: vec![7u8; 32],
            scopes: vec!["atproto".to_string(), "transition:generic".to_string()],
            dpop_jkt: "thumbprint-of-the-client-key".to_string(),
            session_deadline: 0,
        }
    }

    fn pair(g: &OAuthGrant) -> (String, String, [u8; 32], IssuerSigner, Vec<IssuerPublicKey>) {
        let (s, public) = signer();
        let secret = [9u8; 32];
        let (access, refresh) =
            mint_tokens(&s, &secret, g, ISS, AUD, b"family-id", b"family-id", NOW).expect("mint");
        (access, refresh, secret, s, public)
    }

    /// The two classes are signed by different keys, and each verifies only
    /// under its own. This is the two-signers ruling as a mechanism: were they
    /// ever unified, the AS key would have to stay verifiable for the refresh
    /// lifetime and rotation would become an operation nobody could complete.
    #[test]
    fn each_token_class_verifies_only_under_its_own_signer() {
        let g = grant();
        let (access, refresh, secret, _s, public) = pair(&g);
        assert!(verify_access_token(&public, &access, ISS, &[AUD], NOW + 1).is_some());
        assert!(verify_refresh_token(&secret, &refresh, ISS, NOW + 1).is_some());
        // An access token is not a refresh token and vice versa, under either
        // verifier: the header alone already separates them.
        assert!(verify_refresh_token(&secret, &access, ISS, NOW + 1).is_none());
        assert!(verify_access_token(&public, &refresh, ISS, &[AUD], NOW + 1).is_none());
    }

    /// `iss` and `aud` follow the artifact's real reader — the ruling in
    /// § The issuer, pinned so a later edit cannot quietly re-unify them.
    #[test]
    fn the_access_token_names_the_resource_server_and_the_refresh_token_names_this_nest() {
        let g = grant();
        let (access, refresh, secret, _s, public) = pair(&g);
        let a = verify_access_token(&public, &access, ISS, &[AUD], NOW + 1).expect("access");
        assert_eq!(a.iss, ISS, "the AS that minted it");
        assert_eq!(
            a.aud,
            Audience(vec![AUD.to_string()]),
            "an ATProto-family grant's one reader: the PDS resource server"
        );
        let r = verify_refresh_token(&secret, &refresh, ISS, NOW + 1).expect("refresh");
        assert_eq!(r.aud, ISS, "presented back here and nowhere else");
    }

    /// A refresh token naming the app plane — or no plane — must not redeem
    /// here. The plane claim is checked even though this nest's secret already
    /// differs from the bridge's: the interlock is a property of the token, not
    /// of two secrets staying unmerged.
    #[test]
    fn an_app_plane_refresh_token_is_refused() {
        let secret = [9u8; 32];
        let mut claims = RefreshClaims {
            sub: "did:plc:x".to_string(),
            aud: ISS.to_string(),
            scope: SCOPE_REFRESH.to_string(),
            iat: NOW,
            exp: NOW + 1000,
            jti: b64(b"jti"),
            sid: b64(b"sid"),
            ascope: String::new(),
            fauna_actor: hex::encode([7u8; 32]),
            plane: PLANE_APP_CREDENTIAL.to_string(),
            jkt: "t".to_string(),
            client_id: "c".to_string(),
            sexp: 0,
        };
        let token = mint_refresh_token(&secret, &claims).expect("mint");
        assert!(verify_refresh_token(&secret, &token, ISS, NOW + 1).is_none());
        // And an ABSENT claim names no plane, so it is refused too.
        claims.plane = String::new();
        let planeless = mint_refresh_token(&secret, &claims).expect("mint");
        assert!(verify_refresh_token(&secret, &planeless, ISS, NOW + 1).is_none());
    }

    /// A public client's session deadline binds independently of the token's
    /// own `exp`, so rotating cannot extend it past 14 days from consent.
    #[test]
    fn the_absolute_session_deadline_outranks_the_tokens_own_expiry() {
        let secret = [9u8; 32];
        let mut g = grant();
        g.session_deadline = NOW + 100;
        let (_s, _p) = signer();
        let claims = RefreshClaims {
            sub: g.subject.clone(),
            aud: ISS.to_string(),
            scope: SCOPE_REFRESH.to_string(),
            iat: NOW,
            // Deliberately far past the deadline: this is the token a buggy
            // mint would produce, and the verify must still refuse it.
            exp: NOW + 1_000_000,
            jti: b64(b"jti"),
            sid: b64(b"sid"),
            ascope: String::new(),
            fauna_actor: hex::encode(&g.actor_id),
            plane: PLANE_OAUTH.to_string(),
            jkt: g.dpop_jkt.clone(),
            client_id: g.client_id.clone(),
            sexp: g.session_deadline,
        };
        let token = mint_refresh_token(&secret, &claims).expect("mint");
        assert!(verify_refresh_token(&secret, &token, ISS, NOW + 50).is_some());
        assert!(verify_refresh_token(&secret, &token, ISS, NOW + 101).is_none());
    }

    /// Clamping, not ignoring: a public client's last refresh token expires
    /// with its session rather than 180 days later.
    #[test]
    fn a_public_clients_refresh_expiry_is_clamped_to_its_session_deadline() {
        let mut g = grant();
        assert_eq!(
            refresh_expiry(&g, NOW),
            NOW + i64::try_from(REFRESH_TOKEN_LIFETIME_SECS).unwrap(),
            "a confidential client's token gets the full individual cap"
        );
        g.session_deadline = public_session_deadline(NOW);
        assert_eq!(refresh_expiry(&g, NOW), g.session_deadline);
    }

    /// A token signed under another secret must not verify, and the compare is
    /// constant-time so failing does not leak how far it matched.
    #[test]
    fn a_refresh_token_from_another_secret_is_refused() {
        let g = grant();
        let (_a, refresh, _secret, _s, _p) = pair(&g);
        assert!(verify_refresh_token(&[1u8; 32], &refresh, ISS, NOW + 1).is_none());
    }

    /// `alg` is pinned, not read. A `none`-algorithm token with the right
    /// claims is the oldest JWT break there is.
    #[test]
    fn an_alg_none_token_is_refused_by_both_verifiers() {
        let g = grant();
        let (_a, _r, secret, _s, public) = pair(&g);
        let header = B64.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let claims = B64.encode(
            serde_json::to_vec(&serde_json::json!({
                "sub": "did:plc:x", "aud": ISS, "scope": SCOPE_REFRESH,
                "iat": NOW, "exp": NOW + 1000, "jti": "a", "sid": "b",
                "plane": PLANE_OAUTH,
            }))
            .unwrap(),
        );
        let forged = format!("{header}.{claims}.");
        assert!(verify_refresh_token(&secret, &forged, ISS, NOW + 1).is_none());
        assert!(verify_access_token(&public, &forged, ISS, &[AUD], NOW + 1).is_none());
    }

    /// An unknown `kid` resolves to NO key — never a fallback to the current
    /// one. That is what makes the verify path already correct on the day a
    /// second key exists.
    #[test]
    fn an_unknown_kid_resolves_to_no_key_rather_than_the_current_one() {
        let g = grant();
        let (access, _r, _secret, _s, mut public) = pair(&g);
        public[0].kid = "some-other-thumbprint".to_string();
        assert!(verify_access_token(&public, &access, ISS, &[AUD], NOW + 1).is_none());
    }

    /// A token minted by a different issuer, or for a different resource
    /// server, is refused even when its signature is ours.
    #[test]
    fn the_issuer_and_audience_are_both_checked() {
        let g = grant();
        let (access, _r, _secret, _s, public) = pair(&g);
        assert!(
            verify_access_token(&public, &access, "https://other.example", &[AUD], NOW + 1)
                .is_none()
        );
        assert!(verify_access_token(&public, &access, ISS, &["did:web:other"], NOW + 1).is_none());
    }

    fn grant_for(scopes: &[&str]) -> OAuthGrant {
        OAuthGrant {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            ..grant()
        }
    }

    /// One reader is a JSON string — so an ATProto-only token is byte for byte
    /// what it was before the reader set — and two are the two-member array.
    #[test]
    fn one_reader_is_a_string_on_the_wire_and_two_are_an_array() {
        let (atproto, ..) = pair(&grant_for(&["atproto", "transition:generic"]));
        assert_eq!(decode_part(&atproto, 1)["aud"], serde_json::json!(AUD));

        let (oidc, ..) = pair(&grant_for(&["openid", "profile"]));
        assert_eq!(decode_part(&oidc, 1)["aud"], serde_json::json!(ISS));

        let (both, ..) = pair(&grant_for(&["atproto", "openid"]));
        assert_eq!(decode_part(&both, 1)["aud"], serde_json::json!([AUD, ISS]));
    }

    /// Either wire spelling parses, and a one-member array is the same set as
    /// the string — a reader must not care which the mint chose.
    #[test]
    fn the_audience_parses_from_a_string_or_an_array() {
        let parse = |v: serde_json::Value| serde_json::from_value::<Audience>(v).expect("parse");
        assert_eq!(parse(serde_json::json!(AUD)), Audience(vec![AUD.into()]));
        assert_eq!(parse(serde_json::json!([AUD])), Audience(vec![AUD.into()]));
        assert_eq!(
            parse(serde_json::json!([AUD, ISS])),
            Audience(vec![AUD.into(), ISS.into()])
        );
        assert!(serde_json::from_value::<Audience>(serde_json::json!(7)).is_err());
        assert!(serde_json::from_value::<Audience>(serde_json::json!([AUD, 7])).is_err());
    }

    /// Each reader requires its OWN identifier to be a member and ignores the
    /// rest: a two-reader token verifies at either, a one-reader token only at
    /// the reader it names.
    #[test]
    fn a_reader_whose_identifier_is_absent_refuses_the_token() {
        let verifies = |scopes: &[&str], reader: &str| {
            let (access, _r, _secret, _s, public) = pair(&grant_for(scopes));
            verify_access_token(&public, &access, ISS, &[reader], NOW + 1).is_some()
        };
        assert!(verifies(&["atproto"], AUD));
        assert!(
            !verifies(&["atproto"], ISS),
            "an ATProto-only token is not addressed to the nest"
        );
        assert!(verifies(&["openid"], ISS));
        assert!(
            !verifies(&["openid"], AUD),
            "an OIDC-only token is not addressed to the PDS"
        );
        assert!(verifies(&["atproto", "openid"], AUD));
        assert!(verifies(&["atproto", "openid"], ISS));
        assert!(!verifies(&["atproto", "openid"], "did:web:other"));
    }

    /// `/oauth/revoke` identifies its own artifact, so it asks with every
    /// reader this issuer mints for: a token stays revocable whatever its
    /// families.
    #[test]
    fn a_verifier_asking_with_several_readers_accepts_a_token_naming_any_of_them() {
        for scopes in [&["atproto"][..], &["openid"], &["atproto", "openid"]] {
            let (access, _r, _secret, _s, public) = pair(&grant_for(scopes));
            assert!(
                verify_access_token(&public, &access, ISS, &[AUD, ISS], NOW + 1).is_some(),
                "{scopes:?}"
            );
        }
        let (access, _r, _secret, _s, public) = pair(&grant_for(&["atproto"]));
        assert!(verify_access_token(&public, &access, ISS, &[], NOW + 1).is_none());
    }

    /// A grant whose scopes name no reader mints nothing — there is no server
    /// such a token could be presented at.
    #[test]
    fn a_grant_naming_no_reader_is_not_minted() {
        let (s, _public) = signer();
        let minted = mint_tokens(
            &s,
            &[9u8; 32],
            &grant_for(&[]),
            ISS,
            AUD,
            b"family-id",
            b"family-id",
            NOW,
        );
        assert!(minted.is_err());
    }

    /// The access token's lifetime is the shared owner's, not a literal here.
    /// Pinned because a copy would be the exact drift the horizon ruling exists
    /// to prevent.
    #[test]
    fn the_access_lifetime_comes_from_the_shared_owner() {
        let g = grant();
        let (access, _r, _secret, _s, public) = pair(&g);
        let a = verify_access_token(&public, &access, ISS, &[AUD], NOW + 1).expect("access");
        assert_eq!(a.exp - a.iat, access_lifetime_secs());
    }

    /// Scope round-trips through the wire spelling, and a doubled space cannot
    /// manufacture an empty scope nobody granted.
    #[test]
    fn scope_round_trips_and_drops_empty_segments() {
        assert_eq!(
            split_scope("atproto  transition:generic"),
            vec!["atproto".to_string(), "transition:generic".to_string()]
        );
        assert_eq!(split_scope(""), Vec::<String>::new());
        let scopes = vec!["a".to_string(), "b".to_string()];
        assert_eq!(split_scope(&join_scope(&scopes)), scopes);
    }

    /// A four-part or two-part token is refused before anything is decoded —
    /// a JWS with an extra segment is a JWE, which this AS never mints.
    #[test]
    fn only_a_three_part_token_is_considered() {
        assert!(split3("a.b").is_none());
        assert!(split3("a.b.c.d").is_none());
        assert!(split3("a.b.c").is_some());
    }

    fn decode_part(token: &str, index: usize) -> serde_json::Value {
        let part = token.split('.').nth(index).expect("three parts");
        serde_json::from_slice(&B64.decode(part).expect("base64url")).expect("json")
    }

    /// **The rule-#8 separation, isolated** (`key-material-hierarchy.md`
    /// § Audience: deployment infrastructure → *Issuer signing key*): the SAME
    /// access-token claims, signed by the same key, verify as an access token
    /// under `typ: at+jwt` and are refused under the ID token's `typ: JWT`. So
    /// the refusal comes from the header inside the signature — not from the
    /// two claim sets happening to differ.
    #[test]
    fn the_same_claims_signed_as_an_id_token_do_not_verify_as_an_access_token() {
        let (s, public) = signer();
        let claims = AccessClaims {
            iss: ISS.to_string(),
            sub: "did:plc:x".to_string(),
            aud: Audience(vec![AUD.to_string()]),
            scope: "openid".to_string(),
            iat: NOW,
            exp: NOW + 100,
            jti: b64(b"jti"),
            cnf: Cnf {
                jkt: "thumbprint".to_string(),
            },
            sid: b64(b"sid"),
            client_id: "c".to_string(),
            fauna_actor: hex::encode([7u8; 32]),
        };
        let as_access = sign_es256(&s, ACCESS_TOKEN_TYP, &claims).expect("sign");
        let as_id = sign_es256(&s, ID_TOKEN_TYP, &claims).expect("sign");
        assert!(verify_access_token(&public, &as_access, ISS, &[AUD], NOW + 1).is_some());
        assert!(
            verify_access_token(&public, &as_id, ISS, &[AUD], NOW + 1).is_none(),
            "a `typ: JWT` token verified as an access token"
        );
    }

    /// An ID token carries `typ: JWT`, the key's `kid`, the client as `aud`,
    /// the nonce verbatim, and only the identity claims it was given — an
    /// absent fact is omitted, never rendered as `null`.
    #[test]
    fn an_id_token_is_typed_addressed_to_the_client_and_omits_what_it_lacks() {
        let (s, _public) = signer();
        let token = mint_id_token(
            &s,
            &IdTokenClaims {
                iss: ISS.to_string(),
                sub: hex::encode([7u8; 32]),
                aud: "https://app.example/client-metadata.json".to_string(),
                iat: NOW,
                exp: NOW + 100,
                nonce: Some("n-0S6".to_string()),
                identity: OidcClaims {
                    preferred_username: Some("alice".to_string()),
                    ..OidcClaims::default()
                },
            },
        )
        .expect("mint");
        let header = decode_part(&token, 0);
        assert_eq!(header["typ"], ID_TOKEN_TYP);
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], s.kid);
        let claims = decode_part(&token, 1);
        assert_eq!(claims["aud"], "https://app.example/client-metadata.json");
        assert_eq!(claims["nonce"], "n-0S6");
        assert_eq!(claims["preferred_username"], "alice");
        let obj = claims.as_object().expect("object");
        assert!(!obj.contains_key("email"), "{claims}");
        assert!(!obj.contains_key("email_verified"), "{claims}");
    }

    /// The access token's subject: the DID when the grant reaches the PDS, the
    /// actor id for an OIDC-only sign-in — which therefore completes for an
    /// account with no ATProto identity, and an ATProto grant for such an
    /// account does not.
    #[test]
    fn the_grant_subject_follows_the_families_the_grant_carries() {
        let actor = [7u8; 32];
        let oidc: Vec<String> = vec!["openid".into(), "profile".into()];
        let mixed: Vec<String> = vec!["atproto".into(), "openid".into()];
        assert_eq!(grant_subject(&oidc, None, &actor), Some(hex::encode(actor)));
        assert_eq!(
            grant_subject(&oidc, Some("did:plc:x"), &actor),
            Some(hex::encode(actor)),
            "an OIDC-only grant names the actor even when a DID exists"
        );
        assert_eq!(
            grant_subject(&mixed, Some("did:plc:x"), &actor),
            Some("did:plc:x".to_string())
        );
        assert_eq!(grant_subject(&mixed, None, &actor), None);
        assert_eq!(grant_subject(&mixed, Some(""), &actor), None);
    }
}
