//! DKIM signing primitive for outbound mail.
//!
//! Implements `docs/goal/behavior/smtp-server.md` § Outbound delivery (DKIM
//! signing on outbound) and the Phase D portion of the mail-bridge rollout.
//!
//! One consumer: the nest's outbound hand-out
//! (`bins/fauna-nest/src/mail_dkim_key.rs::OutboundSigner`), which opens each
//! mail domain's nest-held key and calls [`sign`] on the message it hands the
//! MTA bridge. The Go bridge holds no DKIM key; it calls
//! `select_signing_domain` (through `libs/fauna-ffi/src/mail.rs`) only to
//! decide whether a message's From: domain is one the deployment signs for.

#[cfg(feature = "multidomain")]
use fauna_core::web::normalize_dns_name;
use mail_auth::common::crypto::{Ed25519Key, RsaKey, Sha256};
use mail_auth::dkim::DkimSigner;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;

/// Signing algorithm carried alongside the raw key material. Matches the
/// nest's stored `mail_dkim_keys.alg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningAlg {
    /// Ed25519. `priv_key` is PKCS8 DER (`Ed25519Key::from_pkcs8_maybe_unchecked_der`).
    Ed25519,
    /// RSA with SHA-256. `priv_key` is PEM-encoded (`PrivateKeyDer::from_pem_slice`).
    RsaSha256,
}

#[cfg(feature = "multidomain")]
impl SigningAlg {
    /// The `k=` tag value for this algorithm's `default._domainkey.<domain>`
    /// TXT record per RFC 6376 §3.6.1 (`k=rsa`) and RFC 8463 §3
    /// (`k=ed25519`). Consumed by [`crate::dns::per_domain::build_dkim_txt_record`]
    /// when assembling the per-domain DKIM public-key record.
    pub fn as_dns_k_value(&self) -> &'static str {
        match self {
            Self::Ed25519 => "ed25519",
            Self::RsaSha256 => "rsa",
        }
    }
}

/// One DKIM key + selector + signing domain. The caller hands a slice of
/// these to [`sign`]; signatures are emitted in slice order (production
/// dual-sign passes `[ed25519, rsa]` so Ed25519 appears above RSA in headers,
/// matching the pre-Phase-D legacy behaviour).
#[derive(Debug, Clone)]
pub struct SigningKey {
    pub alg: SigningAlg,
    pub priv_key: Vec<u8>,
    pub selector: String,
    pub domain: String,
}

/// Header set canonicalised + signed. The first five match the legacy
/// `libs/fauna-bridge-smtp/src/outbound.rs::dkim_sign` set; the six RFC
/// 2369 / RFC 8058 list headers were appended so legitimate-list mail
/// (`mail-mass-mailing.md` § RFC 2369 list headers) carries them *inside*
/// a From-aligned DKIM signature — RFC 8058 §3 requires `List-Unsubscribe` +
/// `List-Unsubscribe-Post` to be signature-covered, and SPF (which the
/// SPF-only client-mail posture relies on) does not cover headers.
///
/// mail-auth over-signs the full set — every name here lands in the `h=`
/// tag whether or not the message carries it, so a header absent at signing
/// time is bound as empty and any downstream insertion of it breaks the
/// signature (the standard DKIM anti-injection posture). This is uniform
/// across submission, auto-reply, and list mail: the six list headers are
/// only populated on list mail, but oversigning them on ordinary mail costs
/// nothing and hardens it against a forged `List-Unsubscribe` phishing header.
const SIGNED_HEADERS: [&str; 11] = [
    "From",
    "To",
    "Subject",
    "Date",
    "Message-ID",
    "List-Id",
    "List-Help",
    "List-Archive",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
    "Precedence",
];

/// DKIM signing errors. Distinguished from generic `anyhow` so the FFI
/// surface and Go consumers can match on the cause.
#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("parse RSA PEM: {0}")]
    RsaPemParse(String),
    #[error("parse RSA key: {0}")]
    RsaKey(String),
    #[error("RSA DKIM key too small: {0} bits (RFC 8301 floor is 2048)")]
    RsaKeyTooSmall(usize),
    #[error("parse Ed25519 key: {0}")]
    Ed25519Key(String),
    #[error("DKIM sign: {0}")]
    DkimSign(String),
    #[error("no signing keys provided")]
    Empty,
}

/// The RSA modulus bit length of a DER-encoded private key, for the RFC 8301
/// §3.2 floor check ahead of signing/sealing. `key_der`'s DER encoding
/// mirrors whichever PEM label the caller supplied (`PrivateKeyDer::from_pem_slice`
/// preserves it rather than normalizing) — PKCS#8 for a `PRIVATE KEY` label,
/// PKCS#1 for a legacy `RSA PRIVATE KEY` label — so this tries PKCS#8 first
/// (the common case: `fauna-provisioning::dkim::generate_rsa_2048` emits
/// PKCS#8) and falls back to PKCS#1. A key that parses for signing but not
/// for this check must fail closed rather than sign/seal unmeasured.
///
/// Two consumers share this: [`sign`]'s `RsaSha256` arm (DKIM) and
/// `crate::outbound::arc::seal`'s `RsaSha256` arm (ARC) — RFC 8617 §4.1.2
/// reuses DKIM's signing algorithms, so the same 2048-bit floor applies to
/// both sites.
pub(crate) fn rsa_modulus_bits(der: &[u8]) -> Result<usize, String> {
    use rsa::pkcs1::DecodeRsaPrivateKey as _;
    use rsa::pkcs8::DecodePrivateKey as _;
    use rsa::traits::PublicKeyParts as _;

    let key = rsa::RsaPrivateKey::from_pkcs8_der(der)
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs1_der(der))
        .map_err(|e| e.to_string())?;
    Ok(key.n().bits())
}

/// DKIM-sign a raw RFC 5322 message with one or more keys. Signatures are
/// prepended to the message in `keys` order (first key's `DKIM-Signature`
/// header appears first; matches the legacy "Ed25519 above RSA" production
/// case when called with `&[ed25519, rsa]`).
///
/// Returns the signed message: `<DKIM-Signature headers...><CRLF><original message>`.
///
/// # Errors
///
/// * [`SignError::Empty`] if `keys` is empty.
/// * [`SignError::RsaPemParse`] / [`SignError::RsaKey`] / [`SignError::Ed25519Key`]
///   if a key's `priv_key` bytes don't parse for the declared `alg`.
/// * [`SignError::RsaKeyTooSmall`] if an RSA key's modulus is below the RFC 8301
///   2048-bit DKIM floor.
/// * [`SignError::DkimSign`] if `mail_auth::DkimSigner::sign` fails (malformed
///   message body / header).
pub fn sign(raw_message: &[u8], keys: &[SigningKey]) -> Result<Vec<u8>, SignError> {
    if keys.is_empty() {
        return Err(SignError::Empty);
    }

    let mut signed = Vec::with_capacity(raw_message.len() + 1024);

    for key in keys {
        match key.alg {
            SigningAlg::Ed25519 => {
                let ed_key = Ed25519Key::from_pkcs8_maybe_unchecked_der(&key.priv_key)
                    .map_err(|e| SignError::Ed25519Key(e.to_string()))?;
                let signer = DkimSigner::from_key(ed_key)
                    .domain(&key.domain)
                    .selector(&key.selector)
                    .headers(SIGNED_HEADERS);
                let signature = signer
                    .sign(raw_message)
                    .map_err(|e| SignError::DkimSign(e.to_string()))?;
                signature.write(&mut signed, true);
            }
            SigningAlg::RsaSha256 => {
                let key_der = PrivateKeyDer::from_pem_slice(&key.priv_key)
                    .map_err(|e| SignError::RsaPemParse(e.to_string()))?;
                let bits = rsa_modulus_bits(key_der.secret_der()).map_err(SignError::RsaKey)?;
                if bits < 2048 {
                    return Err(SignError::RsaKeyTooSmall(bits));
                }
                let pk = RsaKey::<Sha256>::from_key_der(key_der)
                    .map_err(|e| SignError::RsaKey(e.to_string()))?;
                let signer = DkimSigner::from_key(pk)
                    .domain(&key.domain)
                    .selector(&key.selector)
                    .headers(SIGNED_HEADERS);
                let signature = signer
                    .sign(raw_message)
                    .map_err(|e| SignError::DkimSign(e.to_string()))?;
                signature.write(&mut signed, true);
            }
        }
    }

    signed.extend_from_slice(raw_message);
    Ok(signed)
}

/// The From: header domain is neither a local domain nor a subdomain of one,
/// so we have no DKIM key to sign on its behalf. Submission must be rejected
/// with `550 5.7.7 From: domain not local` per
/// `docs/goal/behavior/mail-multidomain.md` § Signing-key selection at
/// outbound time.
#[cfg(feature = "multidomain")]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("From: domain `{from_domain}` is not a local domain or subdomain of one")]
pub struct NotLocalError {
    pub from_domain: String,
}

/// Select the DKIM `d=` signing domain for an outbound message from its From:
/// header domain, per RFC 6376 §3.6 + `docs/goal/behavior/mail-multidomain.md`
/// § Signing-key selection at outbound time.
///
/// * Exact match against a `local_domains` entry wins (`d=<that domain>`).
/// * Otherwise the From: domain must be a subdomain of some local domain; we
///   sign with the **closest parent** local domain's key (longest matching
///   suffix) and set `d=<parent>`. The caller is then responsible for setting
///   `i=<from_address>` per RFC 6376 §3.5 when the returned domain differs from
///   `from_header_domain` (i.e. the From: was a subdomain).
/// * If neither holds, [`NotLocalError`] — the caller rejects submission with
///   `550 5.7.7 From: domain not local`.
///
/// `local_domains` is the caller's current `mail_domains` active-list
/// projection (the `domain_name` column where `removed_at IS NULL`). Matching
/// is ASCII-case-insensitive on the comparison but the returned `&str` is the
/// original `local_domains` entry (so the caller controls the published case).
/// The returned reference borrows from `local_domains`.
#[cfg(feature = "multidomain")]
pub fn select_signing_domain<'a>(
    from_header_domain: &str,
    local_domains: &[&'a str],
) -> Result<&'a str, NotLocalError> {
    let from = normalize_dns_name(from_header_domain);

    // Exact match first.
    if let Some(exact) = local_domains.iter().find(|d| normalize_dns_name(d) == from) {
        return Ok(exact);
    }

    // Closest-parent suffix match: `from` ends with `.<local>`. Among all such
    // parents, prefer the longest (most specific) so `a.sub.example.com`
    // signs under `sub.example.com` rather than `example.com` when both are
    // local.
    local_domains
        .iter()
        .filter(|d| {
            let parent = normalize_dns_name(d);
            from.len() > parent.len() + 1
                && from.ends_with(&parent)
                && from.as_bytes()[from.len() - parent.len() - 1] == b'.'
        })
        // Deliberately a bare dot-strip, not `normalize_dns_name`: this measures
        // the candidate's length, and case-folding cannot change a byte count —
        // normalizing here would allocate a `String` purely to call `.len()` on
        // it. The strip itself IS load-bearing (pinned by
        // `select_prefers_closest_parent_by_dot_stripped_length`).
        .max_by_key(|d| d.trim_end_matches('.').len())
        .copied()
        .ok_or(NotLocalError {
            from_domain: from_header_domain.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty keys → `SignError::Empty`. Guards against an accidental
    /// "sign without configured keys" callsite shipping unsigned mail.
    #[test]
    fn empty_keys_errors() {
        let err = sign(b"hello\r\n", &[]).unwrap_err();
        assert!(matches!(err, SignError::Empty));
    }

    /// Malformed Ed25519 key bytes → `SignError::Ed25519Key`. Guards against
    /// the caller passing through garbage from a corrupted stored key.
    #[test]
    fn malformed_ed25519_key_errors() {
        let err = sign(
            b"From: a@example.test\r\n\r\nbody\r\n",
            &[SigningKey {
                alg: SigningAlg::Ed25519,
                priv_key: vec![0u8; 4],
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SignError::Ed25519Key(_)));
    }

    /// Malformed RSA PEM → `SignError::RsaPemParse`. Same intent.
    #[test]
    fn malformed_rsa_pem_errors() {
        let err = sign(
            b"From: a@example.test\r\n\r\nbody\r\n",
            &[SigningKey {
                alg: SigningAlg::RsaSha256,
                priv_key: b"not a PEM".to_vec(),
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SignError::RsaPemParse(_)));
    }

    /// Generate an RSA private key of the given modulus size, PKCS#8 PEM
    /// encoded exactly as `SigningKey::priv_key` expects for `RsaSha256`
    /// (`sign`'s RSA arm parses via `PrivateKeyDer::from_pem_slice`).
    fn generate_rsa_pem(bits: usize) -> Vec<u8> {
        use rsa::pkcs8::{EncodePrivateKey as _, LineEnding};
        let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, bits).expect("rsa keygen");
        key.to_pkcs8_pem(LineEnding::LF)
            .expect("rsa pkcs8 pem")
            .as_bytes()
            .to_vec()
    }

    /// A sub-2048-bit RSA key must be rejected at the sign site (RFC 8301's
    /// 2048-bit DKIM floor), never silently signed with a weak key.
    #[test]
    fn rsa_key_below_2048_bits_errors() {
        let priv_key = generate_rsa_pem(1024);
        let err = sign(
            b"From: a@example.test\r\n\r\nbody\r\n",
            &[SigningKey {
                alg: SigningAlg::RsaSha256,
                priv_key,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SignError::RsaKeyTooSmall(1024)), "{err:?}");
    }

    /// Companion green case: a conformant 2048-bit RSA key still signs, proving
    /// the floor admits exactly what RFC 8301 requires and nothing less.
    #[test]
    fn rsa_key_at_2048_bits_signs() {
        let priv_key = generate_rsa_pem(2048);
        let signed = sign(
            b"From: a@example.test\r\n\r\nbody\r\n",
            &[SigningKey {
                alg: SigningAlg::RsaSha256,
                priv_key,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        );
        assert!(signed.is_ok(), "{:?}", signed.err());
    }

    /// Pin the DKIM canonicalization to `relaxed/relaxed` (RFC 6376 §3.4.6) on
    /// the `sign` path. `sign` never calls `.header_canonicalization(...)`, so it
    /// rides mail-auth's `DkimSigner` default; a future mail-auth bump that
    /// changes that default (or a stray `.header_canonicalization(Simple)`) would
    /// flip the `c=` tag and break interop with strict external verifiers. The
    /// full sign→verify round-trip lives in `tests/dkim_tests.rs`, but that would
    /// still pass under a self-consistent canonicalization flip — this asserts the
    /// wire tag explicitly. Replaces the regression test deleted with
    /// `fauna-bridge-smtp::dkim_audit.rs`.
    #[test]
    fn sign_uses_relaxed_relaxed_canonicalization() {
        let pkcs8_der = Ed25519Key::generate_pkcs8().expect("generate ed25519 pkcs8");
        let signed = sign(
            b"From: alice@example.test\r\nSubject: hi\r\n\r\nbody\r\n",
            &[SigningKey {
                alg: SigningAlg::Ed25519,
                priv_key: pkcs8_der,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .expect("dkim sign");
        let header = String::from_utf8_lossy(&signed);
        assert!(
            header.contains("c=relaxed/relaxed"),
            "DKIM-Signature must use relaxed/relaxed canonicalization; got:\n{header}"
        );
    }

    /// Extract the lower-cased, colon-split header names from the
    /// `DKIM-Signature` `h=` tag. Whitespace-strips first so folded headers
    /// (FWS inside the `h=` value) don't split tokens, and anchors on `;h=`
    /// so the `bh=` body-hash tag is never mistaken for `h=`.
    fn signed_header_names(signed: &[u8]) -> Vec<String> {
        let s: String = String::from_utf8_lossy(signed)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        // The `h=` tag always sits mid-signature (after `v=1;a=...;c=...`),
        // so it is preceded by `;`. Anchoring on `;h=` also excludes `;bh=`.
        let start = s.find(";h=").expect("DKIM-Signature has an h= tag") + 3;
        let val = &s[start..s[start..].find(';').map(|i| start + i).unwrap_or(s.len())];
        val.split(':')
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// RFC 8058 §3 requires `List-Unsubscribe` + `List-Unsubscribe-Post` to be
    /// covered by the DKIM signature aligned with From, and a legitimate list
    /// signs the rest of the RFC 2369 set + `Precedence` too. When those headers
    /// are present, they must appear in the signed `h=` set.
    #[test]
    fn sign_covers_rfc8058_list_headers_when_present() {
        let pkcs8_der = Ed25519Key::generate_pkcs8().expect("generate ed25519 pkcs8");
        let raw = b"From: news@example.test\r\n\
To: alice@example.com\r\n\
Subject: Issue 12\r\n\
Date: Tue, 01 Jan 2030 00:00:00 +0000\r\n\
Message-ID: <abc@example.test>\r\n\
List-Id: Bob's Weekly <bob-weekly.example.test>\r\n\
List-Help: <https://example.test/list/help>\r\n\
List-Archive: <https://example.test/list/archive>\r\n\
List-Unsubscribe: <mailto:unsubscribe+tok@example.test>, <https://example.test/list/unsubscribe?t=tok>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
Precedence: bulk\r\n\
\r\n\
body\r\n";
        let signed = sign(
            raw,
            &[SigningKey {
                alg: SigningAlg::Ed25519,
                priv_key: pkcs8_der,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .expect("dkim sign");
        let names = signed_header_names(&signed);
        for expected in [
            "list-id",
            "list-help",
            "list-archive",
            "list-unsubscribe",
            "list-unsubscribe-post",
            "precedence",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "DKIM h= must cover {expected}; got {names:?}"
            );
        }
    }

    /// Documents the deliberate over-signing posture: mail-auth binds the
    /// *entire* [`SIGNED_HEADERS`] set into `h=` regardless of which headers the
    /// message actually carries, so even a bare From+Subject message oversigns
    /// the full set (including the six list headers, bound as empty). This is the
    /// standard DKIM anti-injection behaviour — and it is what lets ordinary mail
    /// reject a forged `List-Unsubscribe` inserted downstream — not a regression.
    #[test]
    fn sign_oversigns_full_header_set_even_on_plain_mail() {
        let pkcs8_der = Ed25519Key::generate_pkcs8().expect("generate ed25519 pkcs8");
        let signed = sign(
            b"From: alice@example.test\r\nSubject: hi\r\n\r\nbody\r\n",
            &[SigningKey {
                alg: SigningAlg::Ed25519,
                priv_key: pkcs8_der,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .expect("dkim sign");
        let mut names = signed_header_names(&signed);
        names.sort();
        let mut expected: Vec<String> = SIGNED_HEADERS
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        expected.sort();
        assert_eq!(
            names, expected,
            "h= must over-sign the full SIGNED_HEADERS set"
        );
    }
}

#[cfg(all(test, feature = "multidomain"))]
mod multidomain_tests {
    use super::*;

    #[test]
    fn k_value_matches_rfc_tags() {
        assert_eq!(SigningAlg::Ed25519.as_dns_k_value(), "ed25519");
        assert_eq!(SigningAlg::RsaSha256.as_dns_k_value(), "rsa");
    }

    #[test]
    fn select_exact_match_wins() {
        let locals = ["example.com", "other.org"];
        assert_eq!(
            select_signing_domain("example.com", &locals).unwrap(),
            "example.com"
        );
    }

    #[test]
    fn select_exact_match_is_case_and_dot_insensitive() {
        let locals = ["example.com"];
        assert_eq!(
            select_signing_domain("Example.COM.", &locals).unwrap(),
            "example.com"
        );
    }

    #[test]
    fn select_single_level_subdomain_falls_through() {
        let locals = ["example.com"];
        assert_eq!(
            select_signing_domain("mail.example.com", &locals).unwrap(),
            "example.com"
        );
    }

    #[test]
    fn select_two_level_subdomain_falls_through() {
        let locals = ["example.com"];
        assert_eq!(
            select_signing_domain("a.b.example.com", &locals).unwrap(),
            "example.com"
        );
    }

    #[test]
    fn select_prefers_closest_parent() {
        // Both `example.com` and `sub.example.com` are local; a sub-subdomain
        // signs under the more-specific local domain.
        let locals = ["example.com", "sub.example.com"];
        assert_eq!(
            select_signing_domain("a.sub.example.com", &locals).unwrap(),
            "sub.example.com"
        );
    }

    #[test]
    fn select_exact_match_normalizes_the_local_entry_too() {
        // Normalization is two-sided: a `mail_domains` row stored FQDN-style
        // and mixed-case must still match a bare lowercase From:. The returned
        // `&str` is the ORIGINAL entry — the caller controls the published `d=`
        // case (see this fn's doc), so the assertion is on the stored spelling.
        let locals = ["Example.COM."];
        assert_eq!(
            select_signing_domain("example.com", &locals).unwrap(),
            "Example.COM."
        );
    }

    #[test]
    fn select_parent_match_normalizes_the_local_entry_too() {
        // Same two-sidedness on the closest-parent path, which normalizes each
        // candidate separately from the exact-match scan.
        let locals = ["Example.COM."];
        assert_eq!(
            select_signing_domain("mail.example.com", &locals).unwrap(),
            "Example.COM."
        );
    }

    #[test]
    fn select_prefers_closest_parent_by_dot_stripped_length() {
        // The closest-parent tiebreak measures the DOT-STRIPPED name, not the
        // raw entry. `example.com.....` is 16 raw bytes vs `sub.example.com`'s
        // 15, so a raw-`len()` tiebreak would pick the *less* specific parent;
        // stripped, it is 11 vs 15 and the specific one wins. Deliberately
        // over-dotted so the strip is observable.
        let locals = ["example.com.....", "sub.example.com"];
        assert_eq!(
            select_signing_domain("a.sub.example.com", &locals).unwrap(),
            "sub.example.com"
        );
    }

    #[test]
    fn select_rejects_non_local() {
        let locals = ["example.com"];
        let err = select_signing_domain("other.org", &locals).unwrap_err();
        assert_eq!(err.from_domain, "other.org");
    }

    #[test]
    fn select_rejects_suffix_lookalike_not_subdomain() {
        // `notexample.com` shares the suffix `example.com` textually but is not
        // a subdomain of it (no label boundary), so it must be rejected.
        let locals = ["example.com"];
        assert!(select_signing_domain("notexample.com", &locals).is_err());
    }
}
