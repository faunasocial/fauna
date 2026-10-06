//! The two gates every authorization-server endpoint runs a caller through:
//! the DPoP proof (RFC 9449) and the `private_key_jwt` client assertion
//! (RFC 7523).
//!
//! Ported from the bridge's `dpopGate` / `clientAssertionGate` (TP5 S2). As
//! there, the *policy* is the shared crate's — [`fauna_bridge_atproto::dpop`]
//! and [`fauna_bridge_atproto::client_assertion`] decide everything about a
//! token except the three things a pure module cannot do: check a signature,
//! recognise a nonce this server minted, and remember an identifier already
//! spent. Those three are here, and nothing else is.
//!
//! # The ORDER is the contract
//!
//! Both gates run the same sequence, and each step is where it is for a reason:
//!
//! 1. **Policy** — everything decidable from the token alone. For DPoP that
//!    includes "is a nonce present at all", so a first-contact proof gets the
//!    retryable `use_dpop_nonce` from here rather than from a lookup.
//! 2. **Nonce** (DPoP only), *before* the signature. It is the cheap check, it
//!    is the one a correct client fails on its very first request, and
//!    answering it early costs an attacker nothing: nonces are public and every
//!    response hands one out anyway.
//! 3. **Signature.** Nothing before this point has a side effect, so nothing
//!    unverified has been written down.
//! 4. **Replay**, strictly AFTER the signature. Recording an identifier from an
//!    unverified token would let anyone poison the set with an honest caller's
//!    future identifiers and lock it out — the check would become the attack.
//!
//! # Why the gates take values rather than a request
//!
//! Everything below is a function of headers and form fields the caller already
//! parsed. Handing over the parsed values keeps the gates testable without an
//! HTTP stack, and — for `htu` — makes it structurally impossible to derive the
//! expected URL from the request itself. That last one is a security property:
//! `htu` is compared for equality against what our discovery document
//! advertises, so a URL built from the request's own `Host` header would let a
//! caller reaching this nest under any other name satisfy the check against a
//! URL we never published. The caller passes the URL from the shared builder
//! ([`fauna_bridge_atproto::oauth_metadata`]) or it passes nothing usable.

use fauna_bridge_atproto::client_assertion::{
    AssertionExpectations, AuthenticatedClient, ClientAssertionVerdict, validate_client_assertion,
};
use fauna_bridge_atproto::dpop::{DpopExpectations, DpopVerdict, validate_dpop_proof};
use fauna_bridge_atproto::oauth_client::ResolvedClient;

use crate::oauth_as_error::{ERR_INVALID_CLIENT, ERR_INVALID_DPOP_PROOF, OAuthDeny};
use crate::oauth_as_state::{DPOP_PROOF_MAX_AGE_SECS, OAuthAsRuntime};

/// How far ahead of us a client's clock may be. A proof is minted milliseconds
/// before it is sent, so anything beyond a small allowance is a clock that is
/// simply wrong, not latency.
pub const DPOP_PROOF_MAX_SKEW_SECS: u32 = 30;

// ── The DPoP gate ────────────────────────────────────────────────────────────

/// Run a request's DPoP proof through policy, nonce, signature and replay, and
/// return the RFC-7638 thumbprint of the key it proved possession of.
///
/// `proofs` is **every** `DPoP` header value on the request, not the first one:
/// two proof headers is a request that means two different things to two
/// different readers, and refusing that is the first thing this does.
///
/// `htu` must come from the shared endpoint-URL builder — see the module docs
/// for why it may never be derived from the request.
///
/// The `ath` rule is fixed at `None` here: an authorization-server endpoint
/// takes no access token, so a proof carrying an access-token hash was minted
/// for a different request context and is refused. The one nest route that
/// DOES take an access token — `/oauth/userinfo` — uses
/// [`resource_dpop_gate`] instead.
pub fn dpop_gate(
    runtime: &OAuthAsRuntime,
    proofs: &[String],
    htm: &str,
    htu: &str,
    now: i64,
) -> Result<String, OAuthDeny> {
    gate(runtime, proofs, htm, htu, now, None)
}

/// [`dpop_gate`] for a request presenting an access token (RFC 9449 §7):
/// `ath` is REQUIRED and must equal `access_token_hash`, the caller-computed
/// `base64url(sha256(token))` of the token this request actually presented.
/// Same order, same nonce minter, same replay set — only the `ath` rule
/// differs, which is the whole difference between the two planes.
pub fn resource_dpop_gate(
    runtime: &OAuthAsRuntime,
    proofs: &[String],
    htm: &str,
    htu: &str,
    now: i64,
    access_token_hash: String,
) -> Result<String, OAuthDeny> {
    gate(runtime, proofs, htm, htu, now, Some(access_token_hash))
}

fn gate(
    runtime: &OAuthAsRuntime,
    proofs: &[String],
    htm: &str,
    htu: &str,
    now: i64,
    expected_ath: Option<String>,
) -> Result<String, OAuthDeny> {
    if proofs.len() != 1 {
        let description = if proofs.len() > 1 {
            "more than one DPoP header was sent"
        } else {
            "a DPoP proof is required on this endpoint"
        };
        return Err(OAuthDeny::new(ERR_INVALID_DPOP_PROOF, description));
    }

    // 1. Policy.
    let proof = match validate_dpop_proof(
        proofs[0].clone(),
        DpopExpectations {
            htm: htm.to_string(),
            htu: htu.to_string(),
            expected_ath,
            now_unix: now,
            // One window for both: a proof is exactly as fresh as the nonce it
            // must carry, so there is no second freshness rule to keep in step.
            max_age_secs: DPOP_PROOF_MAX_AGE_SECS as u32,
            max_skew_secs: DPOP_PROOF_MAX_SKEW_SECS,
        },
    ) {
        DpopVerdict::Valid { proof } => proof,
        DpopVerdict::Invalid { error, description } => {
            return Err(OAuthDeny { error, description });
        }
    };

    // 2. Nonce, before the signature.
    if !runtime.nonces.accepts(&proof.nonce, now) {
        return Err(OAuthDeny::new(
            crate::oauth_as_error::ERR_USE_DPOP_NONCE,
            "the DPoP nonce is not one this server issued recently — retry with the one in this response's DPoP-Nonce header",
        ));
    }

    // 3. Signature.
    let Some(verifying_key) = verify_es256(
        &proof.public_key_x,
        &proof.public_key_y,
        &proof.signature,
        &proof.signing_input,
    ) else {
        return Err(OAuthDeny::new(
            ERR_INVALID_DPOP_PROOF,
            "the DPoP proof's signature does not verify under the key it carries",
        ));
    };

    // The SAME thumbprint function that names this nest's own issuer key `kid`.
    // One owner of "what is this key called" on this box, so a `cnf.jkt` a token
    // carries and a `kid` a JWKS publishes can never be computed two ways.
    //
    // Derived BEFORE the replay record because it is also the record's *scope*:
    // an anonymous caller's only identity here is the key it just proved
    // possession of, so that is what its `jti` namespace must be. This does not
    // disturb the ordering that IS a security property — the record still
    // happens strictly after the verification above, which is what stops a
    // forged proof burning an honest caller's identifier.
    let jkt = fauna_provisioning::oauth_issuer::rfc7638_p256_thumbprint(&verifying_key);

    // 4. Replay.
    if runtime.dpop_replays.record(
        &jkt,
        &proof.jti,
        now.saturating_add(DPOP_PROOF_MAX_AGE_SECS),
        now,
    ) {
        return Err(OAuthDeny::new(
            ERR_INVALID_DPOP_PROOF,
            "this DPoP proof has already been used",
        ));
    }
    Ok(jkt)
}

// ── The client-assertion gate ────────────────────────────────────────────────

/// Authenticate the client behind a request, if this client authenticates at
/// all.
///
/// `assertion_type` and `assertion` are the two form parameters exactly as
/// received — empty when absent, so "sent nothing" stays representable and the
/// closed-world check in the policy module can rule on it. Both directions
/// refuse: a confidential client that sent nothing, and a public client that
/// sent something.
///
/// `issuer` must be this server's issuer identifier — the same string its
/// discovery document advertises — because that is what a conformant client
/// puts in `aud`.
///
/// There is no nonce step here: an assertion's freshness is its own `exp`,
/// bounded by the ceiling the shared module applies, and its anti-replay is the
/// `jti`. A server nonce would mean a second round trip for something the
/// client's signature already binds.
pub fn client_assertion_gate(
    runtime: &OAuthAsRuntime,
    assertion_type: &str,
    assertion: &str,
    client: &ResolvedClient,
    issuer: &str,
    now: i64,
) -> Result<(), OAuthDeny> {
    if issuer.is_empty() {
        // Closed world: with no identity to be the audience of, no
        // authentication decision can be made — so none is assumed.
        return Err(OAuthDeny::server(
            "this nest cannot authenticate confidential clients until it has claimed a domain",
        ));
    }

    // 1. Policy.
    let authenticated: AuthenticatedClient = match validate_client_assertion(
        assertion_type.to_string(),
        assertion.to_string(),
        client.clone(),
        AssertionExpectations {
            audience: issuer.to_string(),
            now_unix: now,
        },
    ) {
        ClientAssertionVerdict::NotRequired => return Ok(()),
        ClientAssertionVerdict::Invalid { error, description } => {
            return Err(OAuthDeny { error, description });
        }
        ClientAssertionVerdict::Authenticated { client } => client,
    };

    // 2. Signature. The key was SELECTED by the policy module — this file never
    // chooses which of a client's keys to trust.
    if verify_es256(
        &authenticated.public_key_x,
        &authenticated.public_key_y,
        &authenticated.signature,
        &authenticated.signing_input,
    )
    .is_none()
    {
        return Err(OAuthDeny::new(
            ERR_INVALID_CLIENT,
            "the client assertion's signature does not verify under the key its metadata declares",
        ));
    }

    // 3. Replay. The entry lives until the module's capped expiry, not until a
    // store-wide constant: an assertion is remembered for exactly as long as
    // replaying it could achieve anything.
    //
    // Scoped by `client_id` — literally RFC 7523 §3's "unique for the issuer",
    // so a client using a per-issuer counter is conformant and a bare key would
    // let any other client spend its next identifier.
    if runtime.assertion_replays.record(
        &client.client_id,
        &authenticated.jti,
        authenticated.replay_until_unix,
        now,
    ) {
        return Err(OAuthDeny::new(
            ERR_INVALID_CLIENT,
            "this client assertion has already been used",
        ));
    }
    Ok(())
}

// ── ES256 ────────────────────────────────────────────────────────────────────

/// Verify an ES256 signature over `signing_input` under the P-256 point
/// `(x, y)`, returning the verifying key on success.
///
/// The point is **validated** rather than assumed: `from_encoded_point` refuses
/// anything not on the curve. For the DPoP path that is load-bearing — a bad
/// point must never become a `cnf.jkt` binding — and for the assertion path it
/// is defence in depth against a malformed declared key set.
///
/// The key is returned rather than a bool so the one caller that needs a
/// thumbprint derives it from the *verified* point, with no second decoding
/// step that could disagree about which key was checked.
fn verify_es256(
    x: &[u8],
    y: &[u8],
    signature: &[u8],
    signing_input: &str,
) -> Option<p256::ecdsa::VerifyingKey> {
    use p256::ecdsa::signature::Verifier as _;

    const COORD_BYTES: usize = 32;
    if x.len() != COORD_BYTES || y.len() != COORD_BYTES || signature.len() != 2 * COORD_BYTES {
        return None;
    }
    let point = p256::EncodedPoint::from_affine_coordinates(x.into(), y.into(), false);
    let key = p256::ecdsa::VerifyingKey::from_encoded_point(&point).ok()?;
    let signature = p256::ecdsa::Signature::from_slice(signature).ok()?;
    key.verify(signing_input.as_bytes(), &signature).ok()?;
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    use p256::ecdsa::signature::Signer as _;

    fn runtime(now: i64) -> OAuthAsRuntime {
        struct NoFetch;
        #[async_trait::async_trait]
        impl crate::oauth_as_client::ClientMetadataFetcher for NoFetch {
            async fn fetch(
                &self,
                _url: &str,
            ) -> Result<String, crate::oauth_as_client::MetadataFetchError> {
                panic!("the gates never fetch")
            }
        }
        OAuthAsRuntime::new(now, std::sync::Arc::new(NoFetch))
    }

    /// A P-256 key, its JWK coordinates, and a signer over them.
    struct Key {
        signing: p256::ecdsa::SigningKey,
        x: String,
        y: String,
    }

    fn key() -> Key {
        let signing = p256::ecdsa::SigningKey::random(&mut rand::thread_rng());
        let (x, y) = fauna_provisioning::oauth_issuer::public_coordinates(signing.verifying_key())
            .expect("coordinates");
        Key { signing, x, y }
    }

    /// A compact DPoP proof, signed for real.
    fn proof(key: &Key, htm: &str, htu: &str, nonce: &str, jti: &str, iat: i64) -> String {
        let header = serde_json::json!({
            "typ": "dpop+jwt",
            "alg": "ES256",
            "jwk": { "kty": "EC", "crv": "P-256", "x": key.x, "y": key.y },
        });
        let claims = serde_json::json!({
            "jti": jti, "htm": htm, "htu": htu, "iat": iat, "nonce": nonce,
        });
        let signing_input = format!(
            "{}.{}",
            B64.encode(serde_json::to_vec(&header).unwrap()),
            B64.encode(serde_json::to_vec(&claims).unwrap())
        );
        let signature: p256::ecdsa::Signature = key.signing.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
    }

    const HTU: &str = "https://nest.example/oauth/par";

    /// A proof carrying an `ath` claim — the resource-server shape.
    fn proof_with_ath(
        key: &Key,
        htm: &str,
        htu: &str,
        nonce: &str,
        jti: &str,
        iat: i64,
        ath: &str,
    ) -> String {
        let header = serde_json::json!({
            "typ": "dpop+jwt",
            "alg": "ES256",
            "jwk": { "kty": "EC", "crv": "P-256", "x": key.x, "y": key.y },
        });
        let claims = serde_json::json!({
            "jti": jti, "htm": htm, "htu": htu, "iat": iat, "nonce": nonce, "ath": ath,
        });
        let signing_input = format!(
            "{}.{}",
            B64.encode(serde_json::to_vec(&header).unwrap()),
            B64.encode(serde_json::to_vec(&claims).unwrap())
        );
        let signature: p256::ecdsa::Signature = key.signing.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
    }

    /// The two planes differ in exactly the `ath` rule (TP6's
    /// `/oauth/userinfo` is the nest's first resource-plane route): the
    /// resource gate REQUIRES the hash of the presented token, and refuses a
    /// proof that omits it or names another token's; the AS gate refuses any
    /// proof that carries one.
    #[test]
    fn the_resource_gate_requires_the_presented_tokens_hash_and_the_as_gate_refuses_one() {
        const USERINFO: &str = "https://nest.example/oauth/userinfo";
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let good = "hash-of-the-presented-token";

        let nonce = rt.nonces.mint(now);
        let bare = proof(&k, "GET", USERINFO, &nonce, "r-1", now);
        assert!(resource_dpop_gate(&rt, &[bare], "GET", USERINFO, now, good.into()).is_err());

        let other = proof_with_ath(&k, "GET", USERINFO, &nonce, "r-2", now, "another-token");
        assert!(resource_dpop_gate(&rt, &[other], "GET", USERINFO, now, good.into()).is_err());

        let bound = proof_with_ath(&k, "GET", USERINFO, &nonce, "r-3", now, good);
        assert!(resource_dpop_gate(&rt, &[bound], "GET", USERINFO, now, good.into()).is_ok());

        let as_plane = proof_with_ath(&k, "POST", HTU, &nonce, "r-4", now, good);
        assert!(dpop_gate(&rt, &[as_plane], "POST", HTU, now).is_err());
    }

    /// The happy path, and the thumbprint it returns is the one this nest would
    /// publish for that key — the property `cnf.jkt` binding rests on.
    #[test]
    fn a_good_proof_returns_the_key_thumbprint() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let nonce = rt.nonces.mint(now);
        let jkt = dpop_gate(
            &rt,
            &[proof(&k, "POST", HTU, &nonce, "jti-1", now)],
            "POST",
            HTU,
            now,
        )
        .expect("valid proof");
        assert_eq!(
            jkt,
            fauna_provisioning::oauth_issuer::rfc7638_p256_thumbprint(k.signing.verifying_key())
        );
    }

    /// A proof is single-use. The second presentation is refused even though
    /// everything about it still validates — that is the replay set doing its
    /// job, not the clock.
    #[test]
    fn the_same_proof_twice_is_a_replay() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let nonce = rt.nonces.mint(now);
        let p = proof(&k, "POST", HTU, &nonce, "jti-1", now);
        dpop_gate(&rt, std::slice::from_ref(&p), "POST", HTU, now).expect("first use");
        let deny = dpop_gate(&rt, &[p], "POST", HTU, now).expect_err("second use");
        assert_eq!(deny.error, ERR_INVALID_DPOP_PROOF);
        assert!(deny.description.contains("already been used"));
    }

    /// The ordering that IS a security property: a proof whose signature does
    /// not verify must not burn its `jti`, or anyone could lock an honest
    /// client out of its own future identifiers by sending forgeries.
    #[test]
    fn a_failed_proof_does_not_burn_its_identifier() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let nonce = rt.nonces.mint(now);
        let good = proof(&k, "POST", HTU, &nonce, "jti-shared", now);

        // Same claims, signature clobbered.
        let mut parts: Vec<&str> = good.split('.').collect();
        let forged_sig = B64.encode([7u8; 64]);
        parts[2] = &forged_sig;
        let forged = parts.join(".");

        let deny = dpop_gate(&rt, &[forged], "POST", HTU, now).expect_err("forgery refused");
        assert_eq!(deny.error, ERR_INVALID_DPOP_PROOF);

        // The honest client's identifier is still spendable.
        dpop_gate(&rt, &[good], "POST", HTU, now).expect("the honest proof still works");
    }

    /// Two callers may legitimately choose the same `jti` — nothing makes one
    /// globally unique — so the replay scope is the key that presented it.
    /// Without that, any caller could spend an honest one's next identifier.
    #[test]
    fn one_callers_jti_cannot_burn_anothers() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let (a, b) = (key(), key());
        let nonce = rt.nonces.mint(now);
        dpop_gate(
            &rt,
            &[proof(&a, "POST", HTU, &nonce, "counter-1", now)],
            "POST",
            HTU,
            now,
        )
        .expect("first caller");
        dpop_gate(
            &rt,
            &[proof(&b, "POST", HTU, &nonce, "counter-1", now)],
            "POST",
            HTU,
            now,
        )
        .expect("second caller with the same jti");
    }

    /// A nonce this server never minted is the retryable refusal — the response
    /// carrying it also carries a usable nonce, so a first-contact client
    /// recovers from the very answer that told it so.
    #[test]
    fn an_unminted_nonce_is_retryable_not_fatal() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let deny = dpop_gate(
            &rt,
            &[proof(
                &k,
                "POST",
                HTU,
                "not-a-nonce-we-minted",
                "jti-1",
                now,
            )],
            "POST",
            HTU,
            now,
        )
        .expect_err("refused");
        assert_eq!(deny.error, crate::oauth_as_error::ERR_USE_DPOP_NONCE);
    }

    /// `htu` is checked against the URL the caller supplies from the shared
    /// builder, so a proof minted for another endpoint does not transfer.
    #[test]
    fn a_proof_for_another_endpoint_does_not_transfer() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let nonce = rt.nonces.mint(now);
        let p = proof(
            &k,
            "POST",
            "https://nest.example/oauth/token",
            &nonce,
            "j",
            now,
        );
        let deny = dpop_gate(&rt, &[p], "POST", HTU, now).expect_err("refused");
        assert_eq!(deny.error, ERR_INVALID_DPOP_PROOF);
    }

    /// Two `DPoP` headers is a request that means two different things to two
    /// different readers; none of them is the one we act on.
    #[test]
    fn two_proof_headers_are_refused_before_anything_is_parsed() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let k = key();
        let nonce = rt.nonces.mint(now);
        let p = proof(&k, "POST", HTU, &nonce, "j", now);
        let deny = dpop_gate(&rt, &[p.clone(), p], "POST", HTU, now).expect_err("refused");
        assert!(deny.description.contains("more than one"));

        let deny = dpop_gate(&rt, &[], "POST", HTU, now).expect_err("refused");
        assert!(deny.description.contains("required"));
    }

    /// A public client authenticates by not authenticating — and the gate says
    /// so without touching the replay set.
    #[test]
    fn a_public_client_needs_no_assertion() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let client = ResolvedClient {
            client_id: "https://client.example/metadata.json".into(),
            client_name: None,
            client_uri: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            redirect_uris: vec!["https://client.example/cb".into()],
            declared_scopes: vec!["atproto".into()],
            confidential: false,
            jwks: vec![],
            jwks_uri: None,
            loopback: false,
            fauna_manifest: None,
        };
        client_assertion_gate(&rt, "", "", &client, "https://nest.example", now)
            .expect("no authentication required");
    }

    /// A domainless nest has no identity to be the audience of, so it refuses
    /// to make an authentication decision rather than assuming one.
    #[test]
    fn a_domainless_nest_authenticates_nobody() {
        let now = 1_700_000_000;
        let rt = runtime(now);
        let client = ResolvedClient {
            client_id: "https://client.example/metadata.json".into(),
            client_name: None,
            client_uri: None,
            logo_uri: None,
            tos_uri: None,
            policy_uri: None,
            redirect_uris: vec!["https://client.example/cb".into()],
            declared_scopes: vec!["atproto".into()],
            confidential: false,
            jwks: vec![],
            jwks_uri: None,
            loopback: false,
            fauna_manifest: None,
        };
        let deny = client_assertion_gate(&rt, "", "", &client, "", now).expect_err("refused");
        assert_eq!(deny.error, crate::oauth_as_error::ERR_SERVER);
    }

    /// A point that is not on the curve is refused rather than thumbprinted —
    /// otherwise a caller could bind a token to a "key" that names no key.
    #[test]
    fn a_point_off_the_curve_never_verifies() {
        assert!(verify_es256(&[9u8; 32], &[9u8; 32], &[1u8; 64], "anything").is_none());
        // Wrong widths, too: a 31-byte coordinate is not a P-256 coordinate.
        assert!(verify_es256(&[0u8; 31], &[0u8; 32], &[1u8; 64], "anything").is_none());
        assert!(verify_es256(&[0u8; 32], &[0u8; 32], &[1u8; 63], "anything").is_none());
    }
}
