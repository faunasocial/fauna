//! The one compact-JWS decomposition, shared by every ES256 token this PDS
//! reads from a caller (`atproto-pds-full.md` § F4 detail).
//!
//! F4 reads two ES256 JWTs off a single request: the DPoP proof ([`crate::dpop`])
//! and, from a confidential client, the `private_key_jwt` client assertion
//! ([`crate::client_assertion`]). Their *policies* have almost nothing in
//! common — different headers, different claims, different key sources — but
//! the act of taking a compact token apart is identical, and doing it twice is
//! the parser-differential shape this codebase has now closed three times (the
//! proxy dial in § F3 detail phase 3, the `client_id` URL in *Client metadata
//! resolution*, and the DPoP proof itself in *DPoP at PAR*).
//!
//! So it is done **here, once**. What each caller gets back is the exact
//! `signing_input` slice the signature covers plus decoded segment bytes; what
//! each caller keeps is every judgement about what those bytes must contain.
//!
//! # Why the signature length lives here too
//!
//! "An ES256 signature is 64 raw bytes, `r ‖ s`, not DER" is a fact about the
//! algorithm rather than about either caller, and both callers are ES256-only
//! (ATProto mandates it for DPoP proofs and for `private_key_jwt` alike). A
//! DER-encoded signature is the single most common thing a JOSE implementation
//! gets wrong here, so the refusal names it.
//!
//! # Why `subject` is a parameter
//!
//! Every refusal a caller emits should say *which* token was malformed — a
//! request carrying both a proof and an assertion otherwise produces an error
//! that could mean either. Passing the noun in keeps one owner of the sentence
//! shape while letting the caller name its own subject.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A P-256 coordinate, and half an ES256 signature.
pub(crate) const P256_COORD_BYTES: usize = 32;

/// A raw ES256 signature: `r ‖ s`, both full-width.
pub(crate) const ES256_SIG_BYTES: usize = 2 * P256_COORD_BYTES;

/// A compact JWS taken apart, with the bytes each half decodes to.
#[derive(Debug)]
pub(crate) struct CompactJws {
    /// `<header>.<payload>` — the exact ASCII the signature covers.
    ///
    /// A slice of the original string rather than a re-join of the parts, so it
    /// is byte-for-byte what the client signed even if a segment were to
    /// round-trip differently through a decode/encode pair.
    pub signing_input: String,
    /// The decoded JOSE header. Still bytes: what JSON shape it must have is
    /// the caller's question.
    pub header: Vec<u8>,
    /// The decoded claims set, same contract.
    pub claims: Vec<u8>,
    /// The raw signature, already length-checked at [`ES256_SIG_BYTES`].
    pub signature: Vec<u8>,
}

/// Take an ES256 compact JWS apart, or say why it is not one.
///
/// The error is a description rather than a typed fault for the same reason
/// [`crate::dpop`]'s coordinate decoder returns one: which *error code* a shape
/// failure carries is the caller's decision (a DPoP fault is
/// `invalid_dpop_proof`, an assertion fault is `invalid_client`), and only the
/// sentence is shared.
pub(crate) fn decompose_compact_es256(compact: &str, subject: &str) -> Result<CompactJws, String> {
    let mut parts = compact.split('.');
    let (Some(header_b64), Some(claims_b64), Some(sig_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("{subject} is not a three-part compact JWS"));
    };
    if header_b64.is_empty() || claims_b64.is_empty() || sig_b64.is_empty() {
        return Err(format!("{subject} has an empty segment"));
    }
    let signing_input = compact[..header_b64.len() + 1 + claims_b64.len()].to_string();

    let header = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| format!("{subject} header is not unpadded base64url"))?;
    let claims = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|_| format!("{subject} claims are not unpadded base64url"))?;
    let signature = match URL_SAFE_NO_PAD.decode(sig_b64) {
        Ok(s) if s.len() == ES256_SIG_BYTES => s,
        Ok(s) => {
            return Err(format!(
                "{subject} signature is {} bytes, not the {ES256_SIG_BYTES} an ES256 \
                 signature is (r || s, not DER)",
                s.len()
            ));
        }
        Err(_) => return Err(format!("{subject} signature is not unpadded base64url")),
    };

    Ok(CompactJws {
        signing_input,
        header,
        claims,
        signature,
    })
}

/// Decode one JWK EC coordinate to its full width.
///
/// The fixed width is load-bearing, not pedantry: a coordinate whose leading
/// byte is zero encodes one byte short unless the producer zero-pads, and
/// accepting the short form would mean this server and every other
/// implementation compute **different thumbprints for the same key** — a
/// binding that silently stops matching. It also matters for a client's
/// declared key set, where a short coordinate would produce a `kid` lookup that
/// appears to succeed against a key that is not the one published.
///
/// `context` names the member for the refusal (`"DPoP proof `jwk`"`,
/// `"client JWKS key"`, …) — see [`decompose_compact_es256`] for why callers
/// supply their own noun.
pub(crate) fn decode_ec_coordinate(
    value: Option<&str>,
    name: &str,
    context: &str,
) -> Result<Vec<u8>, String> {
    let Some(raw) = value else {
        return Err(format!("{context} has no `{name}`"));
    };
    match URL_SAFE_NO_PAD.decode(raw) {
        Ok(b) if b.len() == P256_COORD_BYTES => Ok(b),
        Ok(b) => Err(format!(
            "{context} `{name}` is {} bytes, not the {P256_COORD_BYTES} a P-256 \
             coordinate is (it must be zero-padded, not minimally encoded)",
            b.len()
        )),
        Err(_) => Err(format!("{context} `{name}` is not unpadded base64url")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(s: &str) -> String {
        URL_SAFE_NO_PAD.encode(s.as_bytes())
    }

    fn sig() -> String {
        URL_SAFE_NO_PAD.encode([7u8; ES256_SIG_BYTES])
    }

    #[test]
    fn a_well_formed_token_decomposes_to_the_bytes_the_signature_covers() {
        let compact = format!(
            "{}.{}.{}",
            seg(r#"{"alg":"ES256"}"#),
            seg(r#"{"jti":"a"}"#),
            sig()
        );
        let jws = decompose_compact_es256(&compact, "test token").expect("must decompose");
        assert_eq!(jws.header, br#"{"alg":"ES256"}"#);
        assert_eq!(jws.claims, br#"{"jti":"a"}"#);
        assert_eq!(jws.signature.len(), ES256_SIG_BYTES);
        // The signing input is the ORIGINAL substring, not a re-encoding.
        assert!(compact.starts_with(&jws.signing_input));
        assert_eq!(jws.signing_input.matches('.').count(), 1);
    }

    /// A DER signature is the single most common ES256 mistake, so the refusal
    /// names it rather than reporting a generic length problem.
    #[test]
    fn a_der_encoded_signature_is_refused_by_name() {
        let der = URL_SAFE_NO_PAD.encode([0x30u8, 0x44, 0x02, 0x20]);
        let compact = format!("{}.{}.{}", seg("{}"), seg("{}"), der);
        let err = decompose_compact_es256(&compact, "test token").unwrap_err();
        assert!(err.contains("DER"), "{err}");
    }

    #[test]
    fn every_malformed_shape_names_its_subject() {
        for bad in [
            "only.two".to_string(),
            format!("a.b.c.{}", sig()),
            format!(".{}.{}", seg("{}"), sig()),
            format!("!!!.{}.{}", seg("{}"), sig()),
            format!("{}.!!!.{}", seg("{}"), sig()),
            format!("{}.{}.!!!", seg("{}"), seg("{}")),
        ] {
            let err = decompose_compact_es256(&bad, "client assertion").unwrap_err();
            assert!(
                err.starts_with("client assertion"),
                "a refusal must name which token was malformed, got {err:?}"
            );
        }
    }

    /// The short-coordinate refusal is the one that keeps thumbprints agreeing
    /// across implementations — a minimally-encoded coordinate is a different
    /// `jkt` everywhere else in the ecosystem.
    #[test]
    fn a_minimally_encoded_coordinate_is_refused_rather_than_padded() {
        let short = URL_SAFE_NO_PAD.encode([1u8; P256_COORD_BYTES - 1]);
        let err = decode_ec_coordinate(Some(&short), "x", "client JWKS key").unwrap_err();
        assert!(err.contains("zero-padded"), "{err}");

        let full = URL_SAFE_NO_PAD.encode([1u8; P256_COORD_BYTES]);
        assert_eq!(
            decode_ec_coordinate(Some(&full), "x", "client JWKS key").unwrap(),
            vec![1u8; P256_COORD_BYTES]
        );
    }
}
