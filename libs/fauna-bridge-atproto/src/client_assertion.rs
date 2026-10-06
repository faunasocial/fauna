//! F4 — **confidential-client authentication** (`private_key_jwt`, RFC 7523
//! §2.2), the last of the three checks the discovery-mounting gate holds
//! (`atproto-pds-full.md` § F4 detail, the *PAR* bullet's *Still deferred*
//! sub-bullet; the *Endpoints* bullet already advertises
//! `token_endpoint_auth_methods_supported: ["none", "private_key_jwt"]`).
//!
//! A confidential client proves it is itself by signing a short-lived JWT with
//! a key its own published metadata declares. That is the whole mechanism:
//! there is no shared secret anywhere in this design, so the client's *document*
//! is its credential store, and the `client_id` URL that document is served at
//! is what makes the identity self-authenticating.
//!
//! # What this closes
//!
//! [`ResolvedClient::confidential`] has been a fact with no consumer since F4
//! slice 3. Until now a client declaring `private_key_jwt` was held to nothing:
//! anyone could push authorization requests in its name. Two directions
//! therefore matter equally, and both are pinned — a valid assertion
//! **authenticates**, and a confidential client that sends **none is refused**.
//!
//! # The split, unchanged from [`crate::dpop`]
//!
//! This module owns every policy question — the assertion type, `alg`, the
//! `iss`/`sub`/`aud` identity triple, the lifetime bounds, and *which declared
//! key* a `kid` selects. It owns no crypto: Go verifies the ES256 signature over
//! the [`signing_input`](AuthenticatedClient::signing_input) this module hands
//! back, and Go owns the `jti` replay set. Same ruling as the AS signing key and
//! `getServiceAuth` (`atproto-pds-full.md:434`): policy must never fork, and a
//! second ECDSA implementation of the same ecosystem's conventions is a
//! disagreement that gets silently rejected rather than caught.
//!
//! The compact form is decomposed exactly once, by [`crate::jws`] — the same
//! splitter the DPoP proof on the very same request uses. Two splitters on one
//! request path would be the JWT-confusion shape twice over.
//!
//! # What this module does not do
//!
//! * **No network.** The client's keys were resolved eagerly and are already in
//!   the [`ResolvedClient`] (see [`crate::oauth_client::attach_client_jwks`] for
//!   why). Authentication is a pure function over data in hand.
//! * **No replay memory.** Bounded stores with clocks are Go's, for the same
//!   reason the DPoP `jti` set is. This module returns the `jti` and the
//!   **capped** expiry the entry should carry.

use serde::{Deserialize, Serialize};

use crate::oauth_client::{ClientJwk, OAUTH_ERR_INVALID_CLIENT, ResolvedClient};

/// The only `client_assertion_type` RFC 7523 defines, and the only one this
/// server accepts. Exact match: it is a fixed URN, not a family.
pub const CLIENT_ASSERTION_TYPE_JWT_BEARER: &str =
    "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// The only signature algorithm, matching what the AS document advertises and
/// what ATProto mandates. `none` is refused by name below — it is the single
/// most consequential downgrade in JOSE.
const ASSERTION_ALG: &str = "ES256";

/// How far into the future an assertion's `exp` may reach.
///
/// **A separate constant from [`crate::dpop`]'s proof window, deliberately** —
/// and this is the one place F4 declines to follow the "one freshness constant,
/// not two" ruling that governs the nonce and the proof `iat`. That ruling
/// applies because those two are *the same rule* stated twice: a proof cannot
/// outlive the nonce it must carry. These are two different rules on two
/// different artifacts — the proof's freshness is bounded by a window **this
/// server** mints, while an assertion's is a claim **the client** makes and this
/// value is the ceiling we impose on it. Sharing a number would mean a future
/// session widening the DPoP window silently widened how long a client assertion
/// stays replayable.
///
/// Five minutes because it is the assertion lifetime RFC 7523 deployments
/// converge on, and because this value is exactly what bounds the replay set:
/// an entry lives until the assertion it names can no longer be used, so an
/// unbounded `exp` would mean an unbounded entry.
pub const ASSERTION_MAX_LIFETIME_SECS: u32 = 300;

/// How far a client's clock may run ahead of ours before `iat`/`nbf` refuse.
/// Same allowance the DPoP path makes, and for the same reason: a client's
/// clock is its own, and no allowance is the difference between "works" and
/// "works only on well-synchronised machines".
pub const ASSERTION_MAX_SKEW_SECS: u32 = 30;

/// An assertion is a handful of small claims. Nothing legitimate approaches
/// this, and the bound exists because the caller is unauthenticated until this
/// function has answered.
const ASSERTION_MAX_LEN: usize = 4096;

/// A `jti` becomes a key in the replay set, so an unauthenticated caller must
/// not get to choose an unbounded one.
const JTI_MAX_LEN: usize = 256;

/// What the serving process knows: who we are, and what time it is.
///
/// `audience` must be **this AS's issuer identifier**, rendered by
/// [`crate::oauth_metadata::oauth_issuer`] — the same builder that prints
/// `issuer` in the AS document a conformant client read it out of. One owner, so
/// the string a correct client puts in `aud` and the string we compare against
/// cannot be spelled differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AssertionExpectations {
    pub audience: String,
    /// Seconds since the Unix epoch, from the serving process's clock.
    pub now_unix: i64,
}

/// An assertion that passed every policy check, decomposed for verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AuthenticatedClient {
    /// `<header>.<payload>` — the exact ASCII the signature covers.
    pub signing_input: String,
    /// The raw ES256 signature, `r || s`, 64 bytes.
    pub signature: Vec<u8>,
    /// The **selected** key's coordinates. Selection happened here, so Go never
    /// chooses which of a client's keys to trust.
    pub public_key_x: Vec<u8>,
    pub public_key_y: Vec<u8>,
    /// The assertion's unique identifier, for the caller's replay set.
    pub jti: String,
    /// When the replay entry may be forgotten: the assertion's own `exp`, capped
    /// at [`ASSERTION_MAX_LIFETIME_SECS`] from now.
    ///
    /// Deriving the entry's life from the artifact rather than from a store-wide
    /// constant is what makes the set's size a function of the cap: an entry is
    /// remembered for exactly as long as replaying it could achieve anything,
    /// and not one second longer.
    pub replay_until_unix: i64,
}

/// The decision. `Invalid` carries an RFC 6749 §5.2 error code and a
/// description; the caller maps the code to an HTTP status through the same
/// mechanical table every other F4 refusal uses, and never interprets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ClientAssertionVerdict {
    /// The client authenticated — verify the signature and record the `jti`.
    Authenticated {
        client: AuthenticatedClient,
    },
    /// No authentication was required and none was offered.
    NotRequired,
    Invalid {
        error: String,
        description: String,
    },
}

impl ClientAssertionVerdict {
    fn bad(description: impl Into<String>) -> Self {
        ClientAssertionVerdict::Invalid {
            error: OAUTH_ERR_INVALID_CLIENT.to_string(),
            description: description.into(),
        }
    }
}

#[derive(Deserialize)]
struct AssertionHeader {
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
struct AssertionClaims {
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    /// RFC 7519 lets `aud` be a string or an array of strings. Both spellings
    /// are accepted because both are conformant; what must not vary is *what it
    /// has to contain*.
    #[serde(default)]
    aud: Option<serde_json::Value>,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    nbf: Option<i64>,
}

/// Authenticate the client behind a request, if this client authenticates.
///
/// `assertion_type` and `assertion` are the two form parameters exactly as
/// received — empty strings when absent, so "sent nothing" is representable and
/// this closed-world check can rule on it rather than the caller guessing.
///
/// # Both directions refuse
///
/// A **confidential** client that sends no assertion is refused: its document
/// says it authenticates, so an unauthenticated request in its name is a
/// request from someone else. A **public** client that sends one is *also*
/// refused, rather than having it ignored — the same posture as the loopback
/// client's unknown query parameters (`atproto-pds-full.md` § F4 detail): a
/// caller that sends credentials to an endpoint which silently discards them
/// believes it authenticated, and that belief is worth refusing.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn validate_client_assertion(
    assertion_type: String,
    assertion: String,
    client: ResolvedClient,
    expect: AssertionExpectations,
) -> ClientAssertionVerdict {
    let offered = !assertion_type.is_empty() || !assertion.is_empty();

    if !client.confidential {
        if offered {
            return ClientAssertionVerdict::bad(
                "this client's metadata declares `token_endpoint_auth_method: none`, \
                 so it does not authenticate — a request carrying a client assertion \
                 was built against a different client's configuration",
            );
        }
        return ClientAssertionVerdict::NotRequired;
    }

    if !offered {
        return ClientAssertionVerdict::bad(
            "this client's metadata declares `token_endpoint_auth_method: \
             private_key_jwt`, so every request must carry a `client_assertion`",
        );
    }
    // Exact match on a fixed URN. A caller naming a type we do not implement is
    // told which one is missing rather than being handed a parse error for a
    // token that was never a JWT.
    if assertion_type != CLIENT_ASSERTION_TYPE_JWT_BEARER {
        return ClientAssertionVerdict::bad(format!(
            "`client_assertion_type` must be `{CLIENT_ASSERTION_TYPE_JWT_BEARER}`"
        ));
    }
    if assertion.is_empty() {
        return ClientAssertionVerdict::bad("`client_assertion` is empty");
    }
    if assertion.len() > ASSERTION_MAX_LEN {
        return ClientAssertionVerdict::bad("client assertion is larger than any legitimate one");
    }

    // The one decomposition, shared with the DPoP proof on this same request.
    let jws = match crate::jws::decompose_compact_es256(&assertion, "client assertion") {
        Ok(jws) => jws,
        Err(why) => return ClientAssertionVerdict::bad(why),
    };

    let header: AssertionHeader = match serde_json::from_slice(&jws.header) {
        Ok(h) => h,
        Err(_) => {
            return ClientAssertionVerdict::bad("client assertion header is not a JSON object");
        }
    };
    match header.alg.as_deref() {
        Some(ASSERTION_ALG) => {}
        Some("none") => {
            return ClientAssertionVerdict::bad(
                "client assertion `alg` is `none` — an unsigned assertion authenticates nobody",
            );
        }
        Some(other) => {
            return ClientAssertionVerdict::bad(format!(
                "client assertion `alg` must be `{ASSERTION_ALG}`, got `{other}`"
            ));
        }
        None => return ClientAssertionVerdict::bad("client assertion header has no `alg`"),
    }

    let claims: AssertionClaims = match serde_json::from_slice(&jws.claims) {
        Ok(c) => c,
        Err(_) => {
            return ClientAssertionVerdict::bad("client assertion claims are not a JSON object");
        }
    };

    // The identity triple. RFC 7523 §3: for client authentication `iss` and
    // `sub` are both the client_id — the client is asserting about itself. A
    // mismatch between them is the shape of an assertion minted for one client
    // being presented as another's.
    match claims.iss.as_deref() {
        Some(iss) if iss == client.client_id => {}
        Some(_) => {
            return ClientAssertionVerdict::bad(
                "client assertion `iss` is not the client_id it authenticates",
            );
        }
        None => return ClientAssertionVerdict::bad("client assertion has no `iss`"),
    }
    match claims.sub.as_deref() {
        Some(sub) if sub == client.client_id => {}
        Some(_) => {
            return ClientAssertionVerdict::bad(
                "client assertion `sub` is not the client_id it authenticates",
            );
        }
        None => return ClientAssertionVerdict::bad("client assertion has no `sub`"),
    }

    // `aud` is what stops an assertion the client minted for *another*
    // authorization server from being replayed at this one — the single most
    // valuable claim in the token. Compared against this server's issuer, the
    // string our own AS document publishes.
    if !audience_contains(claims.aud.as_ref(), &expect.audience) {
        return ClientAssertionVerdict::bad(
            "client assertion `aud` is not this authorization server — an assertion \
             minted for one server must not authenticate at another",
        );
    }

    let jti = match claims.jti {
        Some(j) if !j.is_empty() && j.len() <= JTI_MAX_LEN => j,
        Some(_) => {
            return ClientAssertionVerdict::bad(
                "client assertion `jti` is empty or longer than any legitimate one",
            );
        }
        // RFC 7523 §3 requires `jti`, and without one there is nothing to
        // remember — a single-use credential with no identifier is a reusable
        // one.
        None => return ClientAssertionVerdict::bad("client assertion has no `jti`"),
    };

    // Lifetime. `exp` is required; `iat` and `nbf` are honoured when stated,
    // because a client that made a promise should be held to it.
    let Some(exp) = claims.exp else {
        return ClientAssertionVerdict::bad("client assertion has no `exp`");
    };
    if exp <= expect.now_unix {
        return ClientAssertionVerdict::bad("client assertion has expired");
    }
    if let Some(iat) = claims.iat
        && iat
            > expect
                .now_unix
                .saturating_add(i64::from(ASSERTION_MAX_SKEW_SECS))
    {
        return ClientAssertionVerdict::bad("client assertion `iat` is in the future");
    }
    if let Some(nbf) = claims.nbf
        && nbf
            > expect
                .now_unix
                .saturating_add(i64::from(ASSERTION_MAX_SKEW_SECS))
    {
        return ClientAssertionVerdict::bad("client assertion is not yet valid");
    }
    // The cap is applied rather than enforced: an over-long assertion is not
    // refused (that would break a client whose clock or policy differs from ours
    // for no security gain — the credential is still single-use and still
    // signed), it is simply not honoured past the ceiling. What the ceiling
    // protects is the replay set, whose entry lifetime this becomes.
    let ceiling = expect
        .now_unix
        .saturating_add(i64::from(ASSERTION_MAX_LIFETIME_SECS));
    let replay_until_unix = exp.min(ceiling);

    // Key selection is POLICY, and it happens here so Go never picks which of a
    // client's keys to trust.
    let key = match select_key(&client.jwks, header.kid.as_deref()) {
        Ok(key) => key,
        Err(why) => return ClientAssertionVerdict::bad(why),
    };

    ClientAssertionVerdict::Authenticated {
        client: AuthenticatedClient {
            signing_input: jws.signing_input,
            signature: jws.signature,
            public_key_x: key.x.clone(),
            public_key_y: key.y.clone(),
            jti,
            replay_until_unix,
        },
    }
}

/// Does the `aud` claim name this server?
///
/// Both RFC 7519 spellings are accepted — a bare string, or an array
/// containing one. Neither is more correct, and refusing the array form would
/// refuse conformant clients over our own convenience.
fn audience_contains(aud: Option<&serde_json::Value>, expected: &str) -> bool {
    match aud {
        Some(serde_json::Value::String(s)) => s == expected,
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .any(|i| i.as_str().is_some_and(|s| s == expected)),
        _ => false,
    }
}

/// Choose which declared key verifies this assertion.
///
/// **A `kid` that names no declared key refuses — it never falls back to
/// trying the others.** A fallback would mean an assertion signed by a key we
/// do not hold could still be verified against one we do, and more importantly
/// it would make "which key signed this" unanswerable — the client's own `kid`
/// would become advisory. Refusing is also what makes rotation debuggable: a
/// client whose new key has not propagated gets told exactly that.
///
/// A `kid`-less assertion is accepted **only against a single-key set**. With
/// several keys there is no non-arbitrary choice, and an arbitrary one resolved
/// by document order is one an attacker who can append to the set controls.
fn select_key<'a>(jwks: &'a [ClientJwk], kid: Option<&str>) -> Result<&'a ClientJwk, String> {
    // Defence in depth: a confidential client is refused at resolution unless it
    // has a usable key, so this is unreachable through the intended path — but
    // an empty set must never read as "any key will do".
    if jwks.is_empty() {
        return Err("this client has declared no signing key".to_string());
    }
    match kid {
        Some(kid) => jwks
            .iter()
            .find(|k| k.kid.as_deref() == Some(kid))
            .ok_or_else(|| {
                format!(
                    "client assertion names `kid` `{kid}`, which is not a key this client declares"
                )
            }),
        None if jwks.len() == 1 => Ok(&jwks[0]),
        None => Err(
            "client assertion has no `kid` and this client declares more than one key \
             — key selection must not depend on document order"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_client::{ClientResolution, parse_client_metadata};
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    const CLIENT_ID: &str = "https://app.example.com/client.json";
    const ISSUER: &str = "https://pds.example.com";
    const NOW: i64 = 1_800_000_000;

    fn coord(seed: u8) -> String {
        URL_SAFE_NO_PAD.encode([seed; 32])
    }

    fn jwks_json(kids: &[&str]) -> serde_json::Value {
        json!({
            "keys": kids.iter().enumerate().map(|(i, kid)| json!({
                "kty": "EC",
                "crv": "P-256",
                "kid": kid,
                "x": coord(i as u8 + 1),
                "y": coord(i as u8 + 100),
            })).collect::<Vec<_>>()
        })
    }

    /// A confidential client built the way production builds one: through the
    /// real metadata parse, so the fixture cannot drift from what a document
    /// actually produces.
    fn confidential_client(jwks: serde_json::Value) -> ResolvedClient {
        let body = json!({
            "client_id": CLIENT_ID,
            "client_name": "Example App",
            "redirect_uris": ["https://app.example.com/cb"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "scope": "atproto repo:*",
            "dpop_bound_access_tokens": true,
            "application_type": "web",
            "token_endpoint_auth_method": "private_key_jwt",
            "jwks": jwks,
        })
        .to_string();
        match parse_client_metadata(CLIENT_ID.to_string(), body) {
            ClientResolution::Resolved { client } => client,
            other => panic!("fixture client must resolve, got {other:?}"),
        }
    }

    fn public_client() -> ResolvedClient {
        let body = json!({
            "client_id": CLIENT_ID,
            "redirect_uris": ["https://app.example.com/cb"],
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "scope": "atproto",
            "dpop_bound_access_tokens": true,
            "token_endpoint_auth_method": "none",
        })
        .to_string();
        match parse_client_metadata(CLIENT_ID.to_string(), body) {
            ClientResolution::Resolved { client } => client,
            other => panic!("fixture client must resolve, got {other:?}"),
        }
    }

    fn token(header: serde_json::Value, claims: serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string()),
            URL_SAFE_NO_PAD.encode([9u8; 64])
        )
    }

    fn good_header(kid: &str) -> serde_json::Value {
        json!({"alg": "ES256", "kid": kid})
    }

    fn good_claims() -> serde_json::Value {
        json!({
            "iss": CLIENT_ID,
            "sub": CLIENT_ID,
            "aud": ISSUER,
            "jti": "assertion-1",
            "iat": NOW,
            "exp": NOW + 60,
        })
    }

    fn expectations() -> AssertionExpectations {
        AssertionExpectations {
            audience: ISSUER.to_string(),
            now_unix: NOW,
        }
    }

    fn judge(client: &ResolvedClient, compact: &str) -> ClientAssertionVerdict {
        validate_client_assertion(
            CLIENT_ASSERTION_TYPE_JWT_BEARER.to_string(),
            compact.to_string(),
            client.clone(),
            expectations(),
        )
    }

    fn refusal(v: ClientAssertionVerdict) -> String {
        match v {
            ClientAssertionVerdict::Invalid {
                error, description, ..
            } => {
                assert_eq!(error, OAUTH_ERR_INVALID_CLIENT);
                description
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // ── The headline pair: both directions ───────────────────────────────────

    #[test]
    fn a_confidential_client_authenticates_with_an_assertion_signed_by_a_declared_key() {
        let client = confidential_client(jwks_json(&["key-1", "key-2"]));
        let v = judge(&client, &token(good_header("key-2"), good_claims()));
        let ClientAssertionVerdict::Authenticated { client: auth } = v else {
            panic!("a well-formed assertion must authenticate, got {v:?}");
        };
        // The SELECTED key crossed, not the first one — key choice is policy and
        // it happened here.
        assert_eq!(auth.public_key_x, vec![2u8; 32]);
        assert_eq!(auth.jti, "assertion-1");
        assert_eq!(auth.replay_until_unix, NOW + 60);
        assert_eq!(auth.signature.len(), 64);
        assert_eq!(auth.signing_input.matches('.').count(), 1);
    }

    /// The whole point of the slice: `confidential` stops being a fact with no
    /// consumer.
    #[test]
    fn a_confidential_client_that_sends_no_assertion_is_refused() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let why = refusal(validate_client_assertion(
            String::new(),
            String::new(),
            client,
            expectations(),
        ));
        assert!(why.contains("private_key_jwt"), "{why}");
    }

    /// A public client is not merely un-authenticated — it must not be able to
    /// *appear* authenticated either, and credentials sent to an endpoint that
    /// would discard them are refused rather than ignored.
    #[test]
    fn a_public_client_needs_no_assertion_and_may_not_send_one() {
        let client = public_client();
        assert!(matches!(
            validate_client_assertion(String::new(), String::new(), client.clone(), expectations()),
            ClientAssertionVerdict::NotRequired
        ));
        let why = refusal(judge(&client, &token(good_header("key-1"), good_claims())));
        assert!(why.contains("does not authenticate"), "{why}");
    }

    // ── The claims that carry the security ───────────────────────────────────

    /// `aud` is what stops an assertion minted for another authorization server
    /// being replayed at this one.
    #[test]
    fn an_assertion_minted_for_another_authorization_server_is_refused() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let mut claims = good_claims();
        claims["aud"] = json!("https://other-pds.example.com");
        let why = refusal(judge(&client, &token(good_header("key-1"), claims)));
        assert!(why.contains("not this authorization server"), "{why}");
    }

    /// Both RFC 7519 spellings of `aud` are conformant; refusing the array form
    /// would refuse correct clients over our own convenience.
    #[test]
    fn an_audience_array_containing_this_server_is_accepted() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let mut claims = good_claims();
        claims["aud"] = json!(["https://elsewhere.example", ISSUER]);
        assert!(matches!(
            judge(&client, &token(good_header("key-1"), claims)),
            ClientAssertionVerdict::Authenticated { .. }
        ));
    }

    #[test]
    fn the_identity_triple_must_be_this_client() {
        let client = confidential_client(jwks_json(&["key-1"]));
        for (member, value) in [
            ("iss", "https://other.example/c.json"),
            ("sub", "https://other.example/c.json"),
        ] {
            let mut claims = good_claims();
            claims[member] = json!(value);
            let why = refusal(judge(&client, &token(good_header("key-1"), claims)));
            assert!(
                why.contains(member),
                "want the `{member}` refusal, got {why}"
            );
        }
    }

    #[test]
    fn an_unsigned_or_wrongly_signed_algorithm_is_refused_by_name() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let why = refusal(judge(
            &client,
            &token(json!({"alg": "none", "kid": "key-1"}), good_claims()),
        ));
        assert!(why.contains("none"), "{why}");
        let why = refusal(judge(
            &client,
            &token(json!({"alg": "RS256", "kid": "key-1"}), good_claims()),
        ));
        assert!(why.contains("ES256"), "{why}");
    }

    #[test]
    fn an_expired_or_identifier_less_assertion_is_refused() {
        let client = confidential_client(jwks_json(&["key-1"]));

        let mut expired = good_claims();
        expired["exp"] = json!(NOW - 1);
        assert!(refusal(judge(&client, &token(good_header("key-1"), expired))).contains("expired"));

        let mut no_exp = good_claims();
        no_exp["exp"] = json!(null);
        assert!(refusal(judge(&client, &token(good_header("key-1"), no_exp))).contains("`exp`"));

        // A single-use credential with no identifier is a reusable one.
        let mut no_jti = good_claims();
        no_jti["jti"] = json!(null);
        assert!(refusal(judge(&client, &token(good_header("key-1"), no_jti))).contains("`jti`"));
    }

    /// The cap is what bounds the replay set — an entry is remembered for
    /// exactly as long as replaying it could achieve anything.
    #[test]
    fn a_long_lived_assertion_is_honoured_but_its_replay_entry_is_capped() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let mut claims = good_claims();
        claims["exp"] = json!(NOW + 86_400);
        let v = judge(&client, &token(good_header("key-1"), claims));
        let ClientAssertionVerdict::Authenticated { client: auth } = v else {
            panic!("an over-long assertion is honoured, not refused: {v:?}");
        };
        assert_eq!(
            auth.replay_until_unix,
            NOW + i64::from(ASSERTION_MAX_LIFETIME_SECS),
            "the replay entry must not outlive the ceiling this server imposes"
        );
    }

    /// **The DPoP window and the assertion ceiling are separate constants on
    /// purpose** — they are two different rules, not one rule stated twice.
    #[test]
    fn the_assertion_ceiling_is_not_the_dpop_proof_window() {
        let mut claims = good_claims();
        claims["exp"] = json!(NOW + i64::from(ASSERTION_MAX_LIFETIME_SECS));
        let client = confidential_client(jwks_json(&["key-1"]));
        let v = judge(&client, &token(good_header("key-1"), claims));
        let ClientAssertionVerdict::Authenticated { client: auth } = v else {
            panic!("an assertion living exactly to the ceiling is valid: {v:?}");
        };
        assert_eq!(auth.replay_until_unix, NOW + 300);
    }

    // ── Key selection ────────────────────────────────────────────────────────

    /// A `kid` that names no declared key refuses rather than falling back —
    /// otherwise "which key signed this" becomes unanswerable and the client's
    /// own `kid` becomes advisory.
    #[test]
    fn an_unknown_kid_refuses_rather_than_trying_the_other_keys() {
        let client = confidential_client(jwks_json(&["key-1", "key-2"]));
        let why = refusal(judge(&client, &token(good_header("key-99"), good_claims())));
        assert!(why.contains("key-99"), "{why}");
    }

    #[test]
    fn a_kid_less_assertion_is_accepted_only_against_a_single_key_set() {
        let one = confidential_client(json!({
            "keys": [{"kty": "EC", "crv": "P-256", "x": coord(1), "y": coord(2)}]
        }));
        assert!(matches!(
            judge(&one, &token(json!({"alg": "ES256"}), good_claims())),
            ClientAssertionVerdict::Authenticated { .. }
        ));

        let many = confidential_client(jwks_json(&["key-1", "key-2"]));
        let why = refusal(judge(&many, &token(json!({"alg": "ES256"}), good_claims())));
        assert!(why.contains("document order"), "{why}");
    }

    // ── The assertion type ───────────────────────────────────────────────────

    #[test]
    fn only_the_jwt_bearer_assertion_type_is_accepted() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let why = refusal(validate_client_assertion(
            "urn:example:something-else".to_string(),
            token(good_header("key-1"), good_claims()),
            client,
            expectations(),
        ));
        assert!(why.contains("jwt-bearer"), "{why}");
    }

    /// A caller that sends a type but no token has authenticated with nothing;
    /// the refusal must not be a decode error about an empty string.
    #[test]
    fn an_assertion_type_with_no_assertion_is_refused_as_missing_not_malformed() {
        let client = confidential_client(jwks_json(&["key-1"]));
        let why = refusal(validate_client_assertion(
            CLIENT_ASSERTION_TYPE_JWT_BEARER.to_string(),
            String::new(),
            client,
            expectations(),
        ));
        assert!(why.contains("empty"), "{why}");
    }
}
