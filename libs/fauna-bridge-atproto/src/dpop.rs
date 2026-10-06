//! F4 — **DPoP proof validation** (RFC 9449), the possession check the ATProto
//! OAuth spec starts at PAR (`atproto-pds-full.md` § F4 detail, the *Endpoints*
//! bullet's DPoP sentence; § Ecosystem reality item 3, where DPoP is mandatory
//! for **all** client types with server-issued nonces of ≤5 minutes).
//!
//! A DPoP proof is a single-use JWT the client signs with a key it holds, over
//! the request it is about to make. Verifying one turns "this caller sent the
//! right parameters" into "this caller holds this key", and the key's
//! thumbprint (`jkt`) is what a later access token gets bound to — which is
//! what makes a stolen token useless without the key (§ Security posture
//! summary, *Token theft*).
//!
//! # The split, and why the JWT is decomposed exactly once
//!
//! This module owns every **policy** question: which `typ` and `alg` are
//! acceptable, what shape the embedded public key must have, which claims must
//! be present, what `htm`/`htu` must equal, how old an `iat` may be, and that a
//! server-issued nonce is mandatory. It owns none of the **crypto**: it does
//! not verify the signature, and it does not compute a thumbprint. That is the
//! same ruling `getServiceAuth` and the AS signing key already settled — policy
//! must never fork, so it lives in shared Rust; a verifier is an encoding
//! around primitives Go already has, and a second ECDSA implementation of the
//! same ecosystem's conventions is a disagreement that gets silently rejected
//! rather than caught (`atproto-pds-full.md:434`).
//!
//! What makes the split safe is that the compact form is decomposed **here and
//! only here**. [`DpopProof`] hands Go the exact `signing_input` bytes and the
//! exact key coordinates to verify with, so Go never re-splits the token. This
//! is [`crate::oauth_client`]'s "the URL is parsed exactly once, by the
//! component that dials it" applied to a JWT: two splitters is a parser
//! differential, and the two halves would be judging *different headers* — the
//! classic JWT confusion, where the part that decides and the part that
//! verifies do not see the same token.
//!
//! # What this module deliberately does not decide
//!
//! * **Whether the nonce is one this server issued.** A nonce is a keyed MAC
//!   over a clock; holding a secret and reading a clock is exactly the job the
//!   F4 seam already gives Go. This module requires a nonce to be *present*
//!   and refuses with [`OAUTH_ERR_USE_DPOP_NONCE`] when it is not, so the
//!   retryable first-contact case is named by the same error code the caller
//!   will see either way.
//! * **Whether the `jti` has been seen before.** Replay detection needs a
//!   bounded store with a clock — Go's, for the same reason.
//! * **`ath`** is two rules keyed on where the proof arrived, expressed by
//!   [`DpopExpectations::expected_ath`]. At an **authorization-server**
//!   endpoint (`None`) it is *refused* rather than checked: it binds a proof
//!   to an access token, none accompanies the request, so a proof carrying one
//!   was minted for a different request context. At a **resource-server**
//!   request (`Some`) it is *required* and must equal the hash of the access
//!   token the request actually presented (RFC 9449 §4.3) — that equality is
//!   what stops a proof captured alongside one token being replayed alongside
//!   another. The hash itself (`base64url(sha256(token))`) is computed by the
//!   caller: it is crypto, not policy, and this module owns none of the crypto.

use serde::{Deserialize, Serialize};

/// The proof did not establish possession: malformed, wrong claims, bad shape.
/// RFC 9449 §5 — the authorization server answers `400` with this code.
pub const OAUTH_ERR_INVALID_DPOP_PROOF: &str = "invalid_dpop_proof";
/// The proof carried no server-issued nonce, or one this server will not
/// accept. RFC 9449 §8 — a **retryable** signal: the response carries a fresh
/// `DPoP-Nonce` and the client repeats the request with it.
pub const OAUTH_ERR_USE_DPOP_NONCE: &str = "use_dpop_nonce";

/// The only JOSE type a DPoP proof may declare (RFC 9449 §4.2).
const DPOP_TYP: &str = "dpop+jwt";
/// The only signature algorithm this server accepts, and the only one the AS
/// document advertises (`dpop_signing_alg_values_supported`, rendered by
/// [`crate::oauth_metadata`]). Accepting a second here would make that
/// advertisement false in the direction that matters.
const DPOP_ALG: &str = "ES256";

/// A `jti` becomes a key in the replay set, so an anonymous caller must not get
/// to choose an unbounded one. RFC 9449 recommends 96 bits of entropy; every
/// sane encoding of that is far under this.
const JTI_MAX_LEN: usize = 256;
/// A nonce is ours, so anything longer than the ones we mint is not one.
const NONCE_MAX_LEN: usize = 128;
/// The compact form as a whole. A DPoP proof is a handful of small claims plus
/// an embedded P-256 public key; nothing legitimate approaches this.
const PROOF_MAX_LEN: usize = 4096;

/// What the request says, for the proof to be checked against.
///
/// These are facts only the serving process knows (the method it received, the
/// URL it is published at, the time it is now), which is why they are inputs
/// rather than something this module derives. `htu` in particular must be the
/// endpoint URL **this server's own discovery document advertises** — see
/// [`crate::oauth_metadata::oauth_par_endpoint_url`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DpopExpectations {
    /// The HTTP method, uppercase.
    pub htm: String,
    /// The full request URI with no query and no fragment.
    pub htu: String,
    /// Seconds since the Unix epoch, from the serving process's clock.
    pub now_unix: i64,
    /// How far in the past an `iat` may be.
    pub max_age_secs: u32,
    /// How far in the future an `iat` may be — a client's clock is its own,
    /// and a small allowance is the difference between "works" and "works
    /// only on well-synchronised machines".
    pub max_skew_secs: u32,
    /// Which `ath` rule applies (F4 slice 7 — the field that turned the
    /// AS-only refusal into a mode).
    ///
    /// `None` — an **authorization-server** endpoint: no access token
    /// accompanies the request, so a proof carrying `ath` was minted for a
    /// different request context and is refused.
    ///
    /// `Some(hash)` — a **resource-server** request: `ath` is *required* and
    /// must equal `hash`, the caller-computed `base64url(sha256(access
    /// token))` of the token the request actually presented (RFC 9449 §4.3).
    /// The caller computes the hash because it is crypto, not policy; it must
    /// derive it from the presented token through one owner function, never
    /// inline.
    pub expected_ath: Option<String>,
}

/// A proof that passed every policy check, decomposed for verification.
///
/// Holding the pieces rather than the compact string is the point: the caller
/// verifies the signature over [`Self::signing_input`] with
/// [`Self::public_key_x`]/[`Self::public_key_y`], and there is no path by
/// which it could end up verifying a different header than the one judged
/// here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DpopProof {
    /// `<header>.<payload>` — the exact ASCII the signature covers.
    pub signing_input: String,
    /// The raw ES256 signature, `r || s`, 64 bytes.
    pub signature: Vec<u8>,
    /// The embedded public key's affine coordinates, 32 bytes each,
    /// big-endian and zero-padded as JWK requires.
    pub public_key_x: Vec<u8>,
    pub public_key_y: Vec<u8>,
    /// The proof's unique identifier, for the caller's replay set.
    pub jti: String,
    /// The server-issued nonce the proof carried, for the caller to check
    /// against what it would have issued.
    pub nonce: String,
    /// `iat`, carried so a caller can size a replay entry's lifetime from the
    /// proof itself rather than from its own arrival time.
    pub issued_at: i64,
}

/// The decision. `Invalid` carries an RFC 6749 §5.2-style error code and a
/// description; the caller maps the code to an HTTP status through the same
/// mechanical table every other F4 refusal uses, and never interprets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DpopVerdict {
    Valid { proof: DpopProof },
    Invalid { error: String, description: String },
}

impl DpopVerdict {
    fn bad(description: impl Into<String>) -> Self {
        DpopVerdict::Invalid {
            error: OAUTH_ERR_INVALID_DPOP_PROOF.to_string(),
            description: description.into(),
        }
    }

    fn needs_nonce(description: impl Into<String>) -> Self {
        DpopVerdict::Invalid {
            error: OAUTH_ERR_USE_DPOP_NONCE.to_string(),
            description: description.into(),
        }
    }
}

/// The JOSE header of a DPoP proof.
///
/// Unknown members are ignored — JOSE headers grow, and a fetched-document
/// posture applies here the same way it does to client metadata. The private
/// JWK members below are the deliberate exception: they are named so their
/// *presence* is detectable.
#[derive(Deserialize)]
struct ProofHeader {
    #[serde(default)]
    typ: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    jwk: Option<ProofJwk>,
}

#[derive(Deserialize)]
struct ProofJwk {
    #[serde(default)]
    kty: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    /// The EC private scalar. Named so a proof that leaks it is *refused*
    /// rather than silently accepted: a client shipping its private key is a
    /// client whose key must be treated as compromised, and quietly
    /// thumbprinting it (the thumbprint ignores `d`) would let that client
    /// keep authorizing while we hold a secret we were never meant to see.
    #[serde(default)]
    d: Option<serde_json::Value>,
    /// The symmetric key member, for the same reason.
    #[serde(default)]
    k: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct ProofClaims {
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    htm: Option<String>,
    #[serde(default)]
    htu: Option<String>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    nonce: Option<String>,
    /// The access-token hash. Present only when a proof accompanies a token —
    /// which never happens at an authorization-server endpoint.
    #[serde(default)]
    ath: Option<serde_json::Value>,
}

/// Validate a DPoP proof against the request it claims to cover.
///
/// Everything except the signature, the nonce's provenance and the `jti`'s
/// novelty is decided here; see the module docs for why those three are the
/// caller's.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn validate_dpop_proof(compact: String, expect: DpopExpectations) -> DpopVerdict {
    if compact.is_empty() {
        return DpopVerdict::bad("a DPoP proof is required on this endpoint");
    }
    if compact.len() > PROOF_MAX_LEN {
        return DpopVerdict::bad("DPoP proof is larger than any legitimate one");
    }

    // The one decomposition — and it is now literally one, shared with the
    // client assertion this endpoint may also carry (`crate::jws`). Two
    // hand-rolled splitters on the same request path is the parser-differential
    // shape this module's own `htu` rule exists to avoid.
    let jws = match crate::jws::decompose_compact_es256(&compact, "DPoP proof") {
        Ok(jws) => jws,
        Err(why) => return DpopVerdict::bad(why),
    };
    let signing_input = jws.signing_input;

    let header: ProofHeader = match serde_json::from_slice(&jws.header) {
        Ok(h) => h,
        Err(_) => return DpopVerdict::bad("DPoP header is not a JSON object"),
    };

    // `typ` is a media type, and media types are case-insensitive — refusing
    // `DPoP+JWT` would be refusing a conformant client over our own strictness.
    match header.typ.as_deref() {
        Some(t) if t.eq_ignore_ascii_case(DPOP_TYP) => {}
        Some(other) => {
            return DpopVerdict::bad(format!(
                "DPoP proof `typ` must be `{DPOP_TYP}`, got `{other}`"
            ));
        }
        None => return DpopVerdict::bad("DPoP proof header has no `typ`"),
    }

    // `alg` values ARE case-sensitive (JWA), and `none` is refused by name:
    // it is the single most consequential downgrade in JOSE, and a caller
    // trying it deserves to be told that is what was refused.
    match header.alg.as_deref() {
        Some(DPOP_ALG) => {}
        Some("none") => {
            return DpopVerdict::bad(
                "DPoP proof `alg` is `none` — an unsigned proof establishes no possession",
            );
        }
        Some(other) => {
            return DpopVerdict::bad(format!(
                "DPoP proof `alg` must be `{DPOP_ALG}`, got `{other}`"
            ));
        }
        None => return DpopVerdict::bad("DPoP proof header has no `alg`"),
    }

    let Some(jwk) = header.jwk else {
        return DpopVerdict::bad("DPoP proof header has no `jwk` — nothing to verify it against");
    };
    if jwk.d.is_some() || jwk.k.is_some() {
        return DpopVerdict::bad(
            "DPoP proof `jwk` carries private key material — refused, not ignored",
        );
    }
    match jwk.kty.as_deref() {
        Some("EC") => {}
        Some(other) => {
            return DpopVerdict::bad(format!("DPoP proof `jwk.kty` must be `EC`, got `{other}`"));
        }
        None => return DpopVerdict::bad("DPoP proof `jwk` has no `kty`"),
    }
    // ES256 names P-256 specifically. A key on another curve under an
    // `alg: ES256` header is a proof nothing can verify, so it is refused for
    // the same reason the AS signing key refuses a non-P-256 private half.
    match jwk.crv.as_deref() {
        Some("P-256") => {}
        Some(other) => {
            return DpopVerdict::bad(format!(
                "DPoP proof `jwk.crv` must be `P-256` for {DPOP_ALG}, got `{other}`"
            ));
        }
        None => return DpopVerdict::bad("DPoP proof `jwk` has no `crv`"),
    }
    let public_key_x = match decode_coordinate(jwk.x.as_deref(), "x") {
        Ok(v) => v,
        Err(why) => return DpopVerdict::bad(why),
    };
    let public_key_y = match decode_coordinate(jwk.y.as_deref(), "y") {
        Ok(v) => v,
        Err(why) => return DpopVerdict::bad(why),
    };

    let claims: ProofClaims = match serde_json::from_slice(&jws.claims) {
        Ok(c) => c,
        Err(_) => return DpopVerdict::bad("DPoP claims are not a JSON object"),
    };

    // `ath` — two rules, keyed on where the proof arrived (module docs).
    match (&expect.expected_ath, &claims.ath) {
        // An AS endpoint takes no access token, so a proof carrying the hash
        // of one was minted for a different request context.
        (None, Some(_)) => {
            return DpopVerdict::bad(
                "DPoP proof carries `ath` — it was minted to accompany an access token, \
                 and this endpoint takes none",
            );
        }
        (None, None) => {}
        // A resource-server request presents a token, and the proof must be
        // bound to *that* token — a captured proof from one request must not
        // be replayable alongside a different token.
        (Some(_), None) => {
            return DpopVerdict::bad(
                "DPoP proof has no `ath` — a proof accompanying an access token must \
                 carry that token's hash",
            );
        }
        (Some(expected), Some(serde_json::Value::String(got))) if got == expected => {}
        (Some(_), Some(_)) => {
            return DpopVerdict::bad(
                "DPoP proof `ath` does not match the access token this request presented",
            );
        }
    }

    let jti = match claims.jti {
        Some(j) if !j.is_empty() && j.len() <= JTI_MAX_LEN => j,
        Some(_) => {
            return DpopVerdict::bad("DPoP proof `jti` is empty or longer than any legitimate one");
        }
        None => return DpopVerdict::bad("DPoP proof has no `jti`"),
    };

    // HTTP methods are uppercase tokens and the caller passes its own, so an
    // exact comparison is the whole rule.
    match claims.htm.as_deref() {
        Some(m) if m == expect.htm => {}
        Some(other) => {
            return DpopVerdict::bad(format!(
                "DPoP proof `htm` is `{other}`, but this request is `{}`",
                expect.htm
            ));
        }
        None => return DpopVerdict::bad("DPoP proof has no `htm`"),
    }

    // `htu` is compared exactly, and the string it must equal is the one this
    // server's own AS document advertises for this endpoint. That makes the
    // comparison a one-owner property rather than a normalisation contest: a
    // client builds `htu` from the endpoint URL we published, so an exact
    // match is what a conformant client produces. The query/fragment refusal
    // is stated as a substring fact — no URL parser here, for the same reason
    // `oauth_client` has none (a second parse is a differential).
    match claims.htu.as_deref() {
        Some(u) if u == expect.htu => {}
        Some(u) if u.contains('?') || u.contains('#') => {
            return DpopVerdict::bad(
                "DPoP proof `htu` carries a query or fragment — RFC 9449 requires it stripped",
            );
        }
        Some(_) => {
            return DpopVerdict::bad("DPoP proof `htu` is not this endpoint's URL");
        }
        None => return DpopVerdict::bad("DPoP proof has no `htu`"),
    }

    let Some(issued_at) = claims.iat else {
        return DpopVerdict::bad("DPoP proof has no `iat`");
    };
    if issued_at
        > expect
            .now_unix
            .saturating_add(i64::from(expect.max_skew_secs))
    {
        return DpopVerdict::bad("DPoP proof `iat` is in the future");
    }
    if issued_at
        < expect
            .now_unix
            .saturating_sub(i64::from(expect.max_age_secs))
    {
        return DpopVerdict::bad("DPoP proof `iat` is too old");
    }
    // `exp` is optional in a DPoP proof (the `iat` window is the real bound),
    // but a client that states one has made a promise we hold it to.
    if let Some(exp) = claims.exp
        && exp <= expect.now_unix
    {
        return DpopVerdict::bad("DPoP proof has expired");
    }

    // Last, so that a first-contact proof — well-formed in every other way and
    // simply lacking the nonce it has not been given yet — is answered with
    // the retryable code rather than with whatever else was also wrong.
    let nonce = match claims.nonce {
        Some(n) if !n.is_empty() && n.len() <= NONCE_MAX_LEN => n,
        Some(_) => {
            return DpopVerdict::needs_nonce(
                "DPoP proof `nonce` is empty or longer than any this server issues",
            );
        }
        None => {
            return DpopVerdict::needs_nonce(
                "DPoP proof carries no server-issued `nonce` — retry with the one in \
                 this response's DPoP-Nonce header",
            );
        }
    };

    DpopVerdict::Valid {
        proof: DpopProof {
            signing_input,
            signature: jws.signature,
            public_key_x,
            public_key_y,
            jti,
            nonce,
            issued_at,
        },
    }
}

/// Decode one JWK coordinate off this proof's embedded key.
///
/// A thin naming wrapper over [`crate::jws::decode_ec_coordinate`], which owns
/// the width rule (and the reason it is load-bearing) for every EC key F4 reads
/// — this proof's `jwk` and a confidential client's declared key set alike.
fn decode_coordinate(value: Option<&str>, name: &str) -> Result<Vec<u8>, String> {
    crate::jws::decode_ec_coordinate(value, name, "DPoP proof `jwk`")
}

#[cfg(test)]
mod tests {
    use super::*;
    // The shape constants and the base64 engine live in `crate::jws` now (the
    // one decomposition); the tests still build tokens by hand, so they import
    // them directly rather than through a re-export nothing else would use.
    use crate::jws::{ES256_SIG_BYTES, P256_COORD_BYTES};
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;
    const HTU: &str = "https://pds.example.com/oauth/par";

    /// A full-width JWK coordinate. Values are arbitrary — this module never
    /// does curve arithmetic, so only the width matters here; agreement with a
    /// real key is pinned cross-binary against the Go verifier.
    ///
    /// Encoded from bytes rather than written as a literal on purpose: a
    /// hand-typed 43-character base64url string is only a valid 32-byte
    /// encoding if its final character's low bits are zero, which is exactly
    /// the kind of fixture that fails for a reason unrelated to the test.
    fn coord(fill: u8) -> String {
        URL_SAFE_NO_PAD.encode([fill; P256_COORD_BYTES])
    }

    fn expectations() -> DpopExpectations {
        DpopExpectations {
            htm: "POST".to_string(),
            htu: HTU.to_string(),
            now_unix: NOW,
            max_age_secs: 300,
            max_skew_secs: 30,
            expected_ath: None,
        }
    }

    fn b64(value: &serde_json::Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    }

    fn header() -> serde_json::Value {
        json!({
            "typ": "dpop+jwt",
            "alg": "ES256",
            "jwk": {"kty": "EC", "crv": "P-256", "x": coord(0x11), "y": coord(0x22)},
        })
    }

    fn claims() -> serde_json::Value {
        json!({
            "jti": "0123456789abcdef",
            "htm": "POST",
            "htu": HTU,
            "iat": NOW,
            "nonce": "a-server-issued-nonce",
        })
    }

    /// Assemble a compact proof with a signature of the right *width* — the
    /// signature is never checked here, only decoded.
    fn proof_of(header: serde_json::Value, claims: serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            b64(&header),
            b64(&claims),
            URL_SAFE_NO_PAD.encode([7u8; ES256_SIG_BYTES])
        )
    }

    fn good_proof() -> String {
        proof_of(header(), claims())
    }

    fn valid(compact: String) -> DpopProof {
        match validate_dpop_proof(compact, expectations()) {
            DpopVerdict::Valid { proof } => proof,
            other => panic!("expected a valid proof, got {other:?}"),
        }
    }

    fn refusal(compact: String) -> (String, String) {
        match validate_dpop_proof(compact, expectations()) {
            DpopVerdict::Invalid { error, description } => (error, description),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_well_formed_proof_is_decomposed_for_the_caller_to_verify() {
        let proof = valid(good_proof());
        assert_eq!(proof.jti, "0123456789abcdef");
        assert_eq!(proof.nonce, "a-server-issued-nonce");
        assert_eq!(proof.issued_at, NOW);
        assert_eq!(proof.public_key_x.len(), P256_COORD_BYTES);
        assert_eq!(proof.public_key_y.len(), P256_COORD_BYTES);
        assert_eq!(proof.signature.len(), ES256_SIG_BYTES);
    }

    /// The whole point of decomposing here: the bytes handed out for
    /// verification are the ones this module judged, character for character.
    #[test]
    fn the_signing_input_is_the_exact_prefix_of_the_token_that_was_judged() {
        let compact = good_proof();
        let proof = valid(compact.clone());
        assert!(compact.starts_with(&proof.signing_input));
        assert_eq!(
            compact.as_bytes()[proof.signing_input.len()],
            b'.',
            "the signing input must end exactly where the signature segment begins"
        );
        assert_eq!(proof.signing_input.matches('.').count(), 1);
    }

    #[test]
    fn a_proof_that_is_not_three_segments_is_refused() {
        for compact in [
            String::new(),
            "onlyone".to_string(),
            "two.parts".to_string(),
            format!("{}.{}", good_proof(), "extra"),
        ] {
            let (error, _) = refusal(compact.clone());
            assert_eq!(error, OAUTH_ERR_INVALID_DPOP_PROOF, "for {compact:?}");
        }
    }

    #[test]
    fn an_unsigned_proof_is_refused_by_name() {
        let mut h = header();
        h["alg"] = json!("none");
        let (error, description) = refusal(proof_of(h, claims()));
        assert_eq!(error, OAUTH_ERR_INVALID_DPOP_PROOF);
        assert!(description.contains("none"), "{description}");
    }

    #[test]
    fn a_typ_is_matched_case_insensitively_because_media_types_are() {
        let mut h = header();
        h["typ"] = json!("DPoP+JWT");
        valid(proof_of(h, claims()));
    }

    #[test]
    fn another_algorithm_or_curve_is_refused() {
        let mut alg = header();
        alg["alg"] = json!("RS256");
        assert_eq!(
            refusal(proof_of(alg, claims())).0,
            OAUTH_ERR_INVALID_DPOP_PROOF
        );

        let mut crv = header();
        crv["jwk"]["crv"] = json!("P-384");
        assert_eq!(
            refusal(proof_of(crv, claims())).0,
            OAUTH_ERR_INVALID_DPOP_PROOF
        );
    }

    /// A client shipping its private key must be refused rather than quietly
    /// accepted — the thumbprint ignores `d`, so nothing else would notice.
    #[test]
    fn a_jwk_carrying_private_material_is_refused_not_ignored() {
        for member in ["d", "k"] {
            let mut h = header();
            h["jwk"][member] = json!("c29tZS1wcml2YXRlLXNjYWxhcg");
            let (error, description) = refusal(proof_of(h, claims()));
            assert_eq!(error, OAUTH_ERR_INVALID_DPOP_PROOF, "for `{member}`");
            assert!(description.contains("private"), "{description}");
        }
    }

    /// A minimally-encoded coordinate thumbprints differently everywhere else
    /// in the ecosystem, so it cannot be quietly widened to 32 bytes here.
    #[test]
    fn a_short_coordinate_is_refused_rather_than_left_padded() {
        let mut h = header();
        h["jwk"]["x"] = json!(URL_SAFE_NO_PAD.encode([1u8; P256_COORD_BYTES - 1]));
        let (error, description) = refusal(proof_of(h, claims()));
        assert_eq!(error, OAUTH_ERR_INVALID_DPOP_PROOF);
        assert!(description.contains("zero-padded"), "{description}");
    }

    #[test]
    fn the_proof_must_cover_this_request_method_and_url() {
        let mut method = claims();
        method["htm"] = json!("GET");
        assert_eq!(
            refusal(proof_of(header(), method)).0,
            OAUTH_ERR_INVALID_DPOP_PROOF
        );

        let mut url = claims();
        url["htu"] = json!("https://pds.example.com/oauth/token");
        assert_eq!(
            refusal(proof_of(header(), url)).0,
            OAUTH_ERR_INVALID_DPOP_PROOF
        );
    }

    #[test]
    fn an_htu_that_kept_its_query_says_so() {
        let mut c = claims();
        c["htu"] = json!(format!("{HTU}?client_id=x"));
        let (_, description) = refusal(proof_of(header(), c));
        assert!(description.contains("query"), "{description}");
    }

    #[test]
    fn an_iat_outside_the_window_is_refused_in_both_directions() {
        let mut old = claims();
        old["iat"] = json!(NOW - 301);
        assert!(refusal(proof_of(header(), old)).1.contains("too old"));

        let mut future = claims();
        future["iat"] = json!(NOW + 31);
        assert!(refusal(proof_of(header(), future)).1.contains("future"));

        // …and the allowances themselves are honoured, not merely declared.
        let mut edge = claims();
        edge["iat"] = json!(NOW + 30);
        valid(proof_of(header(), edge));
    }

    #[test]
    fn a_stated_exp_is_honoured() {
        let mut c = claims();
        c["exp"] = json!(NOW - 1);
        assert!(refusal(proof_of(header(), c)).1.contains("expired"));
    }

    /// The retryable case, and the one every client hits on first contact.
    #[test]
    fn a_proof_with_no_nonce_asks_for_one_rather_than_refusing_flatly() {
        let mut c = claims();
        c.as_object_mut().unwrap().remove("nonce");
        let (error, _) = refusal(proof_of(header(), c));
        assert_eq!(error, OAUTH_ERR_USE_DPOP_NONCE);
    }

    /// The nonce check is deliberately last: a first-contact proof is
    /// well-formed apart from the nonce, so it must reach that code — but a
    /// proof that is *also* malformed is told about the malformation, which is
    /// the fault the client author can actually fix.
    #[test]
    fn a_malformed_nonceless_proof_is_told_about_the_malformation() {
        let mut c = claims();
        c.as_object_mut().unwrap().remove("nonce");
        c["htm"] = json!("GET");
        assert_eq!(
            refusal(proof_of(header(), c)).0,
            OAUTH_ERR_INVALID_DPOP_PROOF
        );
    }

    /// A proof minted to accompany an access token is not a proof for an
    /// AS endpoint (`expected_ath: None`), even though every other claim
    /// would pass.
    #[test]
    fn a_proof_bound_to_an_access_token_is_refused_at_this_endpoint() {
        let mut c = claims();
        c["ath"] = json!("fUHyO2r2Z3DZ53EsNrWBb0xWXoaNy59IiKCAqksmQEo");
        let (error, description) = refusal(proof_of(header(), c));
        assert_eq!(error, OAUTH_ERR_INVALID_DPOP_PROOF);
        assert!(description.contains("ath"), "{description}");
    }

    /// The resource-server mode (F4 slice 7): with `expected_ath` supplied,
    /// the matching hash passes, a missing `ath` is refused, and a mismatched
    /// one is refused — the binding that stops a captured proof being replayed
    /// alongside a *different* token.
    #[test]
    fn a_resource_server_proof_must_carry_the_presented_tokens_hash() {
        const ATH: &str = "fUHyO2r2Z3DZ53EsNrWBb0xWXoaNy59IiKCAqksmQEo";
        let rs_expect = || DpopExpectations {
            expected_ath: Some(ATH.to_string()),
            ..expectations()
        };

        let mut bound = claims();
        bound["ath"] = json!(ATH);
        assert!(
            matches!(
                validate_dpop_proof(proof_of(header(), bound), rs_expect()),
                DpopVerdict::Valid { .. }
            ),
            "a proof carrying the presented token's own hash must pass"
        );

        for (case, c) in [
            ("missing ath", claims()),
            ("mismatched ath", {
                let mut c = claims();
                c["ath"] = json!("bm90LXRoZS1yaWdodC1oYXNo");
                c
            }),
            ("non-string ath", {
                let mut c = claims();
                c["ath"] = json!(["not", "a", "string"]);
                c
            }),
        ] {
            match validate_dpop_proof(proof_of(header(), c), rs_expect()) {
                DpopVerdict::Invalid { error, description } => {
                    assert_eq!(error, OAUTH_ERR_INVALID_DPOP_PROOF, "{case}");
                    assert!(description.contains("ath"), "{case}: {description}");
                }
                other => panic!("{case}: expected a refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_der_shaped_signature_is_refused_by_width() {
        let compact = format!(
            "{}.{}.{}",
            b64(&header()),
            b64(&claims()),
            URL_SAFE_NO_PAD.encode([0x30u8; 70])
        );
        let (_, description) = refusal(compact);
        assert!(description.contains("DER"), "{description}");
    }

    #[test]
    fn an_unbounded_jti_or_nonce_is_refused_before_it_becomes_a_map_key() {
        let mut long_jti = claims();
        long_jti["jti"] = json!("j".repeat(JTI_MAX_LEN + 1));
        assert_eq!(
            refusal(proof_of(header(), long_jti)).0,
            OAUTH_ERR_INVALID_DPOP_PROOF
        );

        let mut long_nonce = claims();
        long_nonce["nonce"] = json!("n".repeat(NONCE_MAX_LEN + 1));
        assert_eq!(
            refusal(proof_of(header(), long_nonce)).0,
            OAUTH_ERR_USE_DPOP_NONCE
        );
    }

    #[test]
    fn an_oversized_proof_is_refused_before_it_is_decoded() {
        let (_, description) = refusal("a".repeat(PROOF_MAX_LEN + 1));
        assert!(description.contains("larger"), "{description}");
    }

    /// Unknown members grow; JOSE headers and claim sets both do. Ignoring
    /// them is the fetched-document posture, and the private-key members above
    /// are the deliberate exception to it.
    #[test]
    fn unknown_members_are_ignored() {
        let mut h = header();
        h["kid"] = json!("some-key-id");
        h["crit"] = json!([]);
        let mut c = claims();
        c["future_claim"] = json!({"nested": true});
        valid(proof_of(h, c));
    }
}
