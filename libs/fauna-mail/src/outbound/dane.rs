//! DANE/TLSA outbound verification primitives (RFC 7672).
//!
//! When delivering mail to an MX host we look up `_25._tcp.<mx>.` TLSA
//! records. If any DNSSEC-secure records are published they pin the cert
//! chain the MX is expected to present — even a compromised CA root can't
//! let an attacker's cert through, because it won't match the published
//! TLSA hash and the handshake refuses.
//!
//! ⚠ **A hash match is the whole proof only for DANE-EE (3).** For DANE-TA
//! (2) the record names a trust ANCHOR, and association data is a hash of a
//! *public* certificate — an anchor being the most public certificate there
//! is. So a usage-2 record is satisfied only when the presented leaf actually
//! **descends** from the matched cert and carries the connected host's name
//! (RFC 7672 §3.1.1, RFC 6698 §2.1.1); see [`anchor_authenticates_leaf`].
//! Accepting the bare hash match instead lets an on-path attacker present
//! `[their own self-signed leaf, a verbatim copy of the victim's anchor]`
//! with no CA compromise and no private key but their own.
//!
//! **Where this runs (the split, smtp-server.md § Architectural
//! rules — there is no separate § DANE heading, as that doc's own T2.1b
//! status bullet notes).** The Go MTA
//! bridge owns the outbound TLS handshake but cannot do DNSSEC validation
//! in the Go stdlib, so the work divides:
//!   - The DNSSEC-validating TLSA *fetch* ([`lookup_tlsa`] / [`TlsaResolver`])
//!     runs **nest-side** behind the `outbound-net` feature (hickory's
//!     `dnssec-ring`), reached over `fauna.bridges.fetch_tlsa`.
//!   - The pure cert-chain *decision* ([`dane_chain_matches`]) is exported
//!     over UniFFI (`fauna_ffi::dane_chain_matches`) and called **Go-side**
//!     inside the TLS `VerifyPeerCertificate` callback. The whole decision
//!     lives here rather than half here and half in Go: the usage-2 path
//!     validation has to run against *the cert the record matched*, so a
//!     split would force the Go side to re-derive that match and duplicate
//!     this file's association logic in another language.
//!
//! Lifted from the retired `fauna-bridge-smtp::dane` (deleted at
//! the I4/I5 cutover) so the permanent Go-bridge → nest outbound path
//! doesn't couple to that retired crate. The in-nest `DaneVerifier` rustls
//! `ServerCertVerifier` is NOT lifted — Go does the pinning.
//!
//! TLSA record fields per RFC 6698:
//!   - usage: 0 PKIX-TA / 1 PKIX-EE / 2 DANE-TA / 3 DANE-EE
//!   - selector: 0 Full cert / 1 SubjectPublicKeyInfo
//!   - matching: 0 Exact / 1 SHA-256 / 2 SHA-512

use sha2::{Digest, Sha256, Sha512};

/// One TLSA record. Fields match the wire format (u8 each for usage,
/// selector, matching; raw bytes for the certificate-association data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsaRecord {
    pub usage: u8,
    pub selector: u8,
    pub matching: u8,
    pub data: Vec<u8>,
}

impl TlsaRecord {
    /// Returns true if this record's usage applies to SMTP-DANE per RFC
    /// 7672 §3.1. We support DANE-TA (2) and DANE-EE (3); PKIX-TA (0) /
    /// PKIX-EE (1) require PKIX validation in addition and are not
    /// implemented here — a host publishing only those is treated as
    /// having no usable DANE records (callers fall back to the MTA-STS /
    /// opportunistic posture rather than failing the delivery).
    pub fn is_smtp_dane(&self) -> bool {
        matches!(self.usage, 2 | 3)
    }
}

/// Render TLSA fields as the RFC 7672 §3 presentation form:
/// `"<usage> <selector> <matching> <hex-lower(data)>"`. The one shared home
/// for this shape — nest's `dns_verifier::format_tlsa_observed` renders its
/// resolved `hickory` `TLSA` RR through this same function specifically so
/// its output lines up byte-for-byte with [`tlsa_policy_string`]'s; the two
/// used to hand-copy this format independently; a copy that drifted would
/// silently break every DANE observed-vs-expected comparison.
pub fn tlsa_presentation_string(usage: u8, selector: u8, matching: u8, data: &[u8]) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(data.len() * 2);
    for byte in data {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{usage} {selector} {matching} {hex}")
}

/// Format a TLSA record as one RFC 8460 §4.4 `policy-string` entry
/// (RFC 7672 §3 presentation form, see [`tlsa_presentation_string`]). The
/// TLSRPT outbound reporter buckets DANE attempts under these strings (see
/// [`crate::outbound::tlsrpt`]).
pub fn tlsa_policy_string(rec: &TlsaRecord) -> String {
    tlsa_presentation_string(rec.usage, rec.selector, rec.matching, &rec.data)
}

/// Vec form of [`tlsa_policy_string`] for the slice the TLSRPT recorder
/// consumes (`policy_for_attempt`'s `tlsa_policy_strings` argument).
pub fn tlsa_policy_strings(records: &[TlsaRecord]) -> Vec<String> {
    records.iter().map(tlsa_policy_string).collect()
}

/// Returns true iff the presented chain satisfies at least one TLSA record
/// in `records`. `cert_chain_der` is DER-encoded, leaf-first: index 0 is the
/// end-entity cert, the rest are intermediates — the order Go's
/// `tls.ConnectionState.PeerCertificates` / `VerifyPeerCertificate`'s
/// `rawCerts` hands us. `mx_host` is the MX hostname the connection was made
/// to, i.e. the TLSA base domain.
///
/// Pure (no network, no DNS — it does read the wall clock, to judge
/// certificate validity periods) so the Go mail bridge can reuse it over
/// UniFFI to pin the presented chain against the records nest fetched. Walks
/// the cartesian product (record × candidate cert) — small in practice (a
/// TLSA RR-set is rarely more than 2-3 records, a chain 1-3 certs):
///
///   - **DANE-EE (3)**: the leaf's own association must match, and nothing
///     else is checked. RFC 7672 §3.1.1 deliberately waives name and
///     expiry here: the record names *this exact key*, so possession of its
///     private key is the whole proof.
///   - **DANE-TA (2)**: a chain cert's association must match **and** the
///     leaf must actually descend from that cert — see
///     [`anchor_authenticates_leaf`]. A hash match alone proves nothing: a
///     trust anchor is a *public* certificate, so an attacker who merely
///     appends a verbatim copy of it to their own self-signed leaf would
///     satisfy a match-only test while holding none of the keys that matter.
///   - **PKIX-TA/EE (0/1)**: skipped (see [`TlsaRecord::is_smtp_dane`]).
///
/// An empty chain never matches; a record whose selector/matching is
/// out of RFC 6698 range never matches.
pub fn dane_chain_matches(
    records: &[TlsaRecord],
    cert_chain_der: &[Vec<u8>],
    mx_host: &str,
) -> bool {
    let Some((end_entity, intermediates)) = cert_chain_der.split_first() else {
        return false;
    };
    for record in records {
        match record.usage {
            // DANE-EE: the leaf, and only the leaf.
            3 => {
                if record_matches_cert(record, end_entity) {
                    return true;
                }
            }
            // DANE-TA: a matching cert is a candidate ANCHOR, not a verdict.
            2 => {
                for candidate in std::iter::once(end_entity).chain(intermediates.iter()) {
                    if record_matches_cert(record, candidate)
                        && anchor_authenticates_leaf(candidate, end_entity, intermediates, mx_host)
                    {
                        return true;
                    }
                }
            }
            _ => continue,
        }
    }
    false
}

/// The DANE-TA half of [`dane_chain_matches`]: does `anchor_der` — a chain
/// cert whose association matched a usage-2 record — actually authenticate
/// `leaf_der` for `mx_host`?
///
/// RFC 7672 §3.1.1 and RFC 6698 §2.1.1 both require the end-entity
/// certificate to pass PKIX path validation **to the matched anchor**, plus
/// the usual identity check against the connected host. Without that, usage 2
/// authenticates nobody: the association data is a hash of a *public*
/// certificate, and the most public certificate in existence is a trust
/// anchor — so an on-path attacker needs no CA compromise and no private key
/// but their own. They present `[their self-signed leaf, a verbatim copy of
/// the victim's published anchor]`, a match-any-chain-cert test finds the
/// anchor among the intermediates, and the handshake completes on a session
/// the attacker terminates and can read.
///
/// Two arms:
///
///   - **The anchor IS the leaf.** Nothing is left to chain: the peer
///     presented exactly the pinned certificate, so it holds that
///     certificate's private key — the identical proof DANE-EE accepts under
///     usage 3. Accepted, and still name-checked, which usage 3 waives; so
///     this arm is strictly stricter than the usage-3 one beside it.
///   - **Otherwise**, real PKIX path validation from the leaf to that single
///     trust anchor, over the presented intermediates, with the anchor's own
///     validity, the signatures along the path, and the server-auth key usage
///     all enforced by `rustls-webpki` rather than by anything hand-rolled
///     here. The anchor is the ONLY root offered, so a path to any other
///     anchor — a public CA included — does not help an attacker.
fn anchor_authenticates_leaf(
    anchor_der: &[u8],
    leaf_der: &[u8],
    intermediates: &[Vec<u8>],
    mx_host: &str,
) -> bool {
    use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

    let leaf = CertificateDer::from(leaf_der.to_vec());
    let Ok(end_entity) = webpki::EndEntityCert::try_from(&leaf) else {
        return false;
    };

    // The identity check, which applies to both arms. RFC 7672 §3.1.1 keeps
    // it for usage 2 (unlike usage 3) precisely because the anchor delegates
    // to whatever names it chooses to sign for.
    let Ok(server_name) = ServerName::try_from(mx_host) else {
        return false;
    };
    if end_entity
        .verify_is_valid_for_subject_name(&server_name)
        .is_err()
    {
        return false;
    }

    if anchor_der == leaf_der {
        return true;
    }

    let anchor_cert = CertificateDer::from(anchor_der.to_vec());
    let Ok(anchor) = webpki::anchor_from_trusted_cert(&anchor_cert) else {
        return false;
    };
    // The matched anchor is excluded from the intermediate pool it would
    // otherwise sit in twice; everything else the peer sent stays available
    // as a path link.
    let path_links: Vec<CertificateDer<'_>> = intermediates
        .iter()
        .filter(|der| der.as_slice() != anchor_der)
        .map(|der| CertificateDer::from(der.to_vec()))
        .collect();

    end_entity
        .verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &[anchor.to_owned()],
            &path_links,
            UnixTime::now(),
            webpki::KeyUsage::server_auth(),
            None,
            None,
        )
        .is_ok()
}

/// Checks a single TLSA record against a single DER-encoded cert.
fn record_matches_cert(record: &TlsaRecord, cert_der: &[u8]) -> bool {
    // Selector 0 = Full cert; selector 1 = SubjectPublicKeyInfo.
    let association_bytes: Vec<u8> = match record.selector {
        0 => cert_der.to_vec(),
        1 => match extract_spki(cert_der) {
            Some(spki) => spki,
            None => return false,
        },
        _ => return false,
    };
    // Matching 0 = exact; 1 = SHA-256; 2 = SHA-512.
    let computed: Vec<u8> = match record.matching {
        0 => association_bytes,
        1 => Sha256::digest(&association_bytes).to_vec(),
        2 => Sha512::digest(&association_bytes).to_vec(),
        _ => return false,
    };
    computed == record.data
}

/// Extracts the SubjectPublicKeyInfo bytes from a DER-encoded X.509 cert.
/// Returns None if the cert can't be parsed — the caller treats that as
/// "no match" (not a hard error, since other TLSA records or the broader
/// chain may still satisfy verification).
fn extract_spki(cert_der: &[u8]) -> Option<Vec<u8>> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert_der).ok()?;
    Some(parsed.tbs_certificate.subject_pki.raw.to_vec())
}

// ── DNSSEC-validating TLSA resolver (nest-side, native-only) ──────────

/// TLSA lookup behind a `dyn`-compatible boundary so nest can hold an
/// `Arc<dyn TlsaResolver>` and tests can substitute a mock. Mirrors
/// [`crate::outbound::mta_sts::MtaStsFetcher`]. A lookup returns the
/// DNSSEC-secure TLSA records for `_25._tcp.<mx_host>` (empty when none are
/// published or none prove secure).
#[async_trait::async_trait]
pub trait TlsaResolver: Send + Sync {
    async fn lookup(&self, mx_host: &str) -> anyhow::Result<Vec<TlsaRecord>>;
}

/// Resolver that always returns no records. Used by test fixtures and any
/// caller that doesn't model DANE so the `TlsaResolver` boundary is always
/// satisfiable (mirrors [`crate::outbound::mta_sts::NullMtaStsFetcher`]).
pub struct NullTlsaResolver;

#[async_trait::async_trait]
impl TlsaResolver for NullTlsaResolver {
    async fn lookup(&self, _mx_host: &str) -> anyhow::Result<Vec<TlsaRecord>> {
        Ok(Vec::new())
    }
}

/// Looks up TLSA records for `mx_host` at `_25._tcp.<mx_host>` per RFC 7672
/// §2.2 with mandatory DNSSEC validation (RFC 7672 §2.2.1: "the security of
/// DANE is dependent on a chain of trust to the root zone via DNSSEC").
/// Only records with `Proof::Secure` are returned.
///
/// Outcomes:
/// - Secure-proven TLSA records → returned to the caller.
/// - NXDOMAIN / NoData → empty list, no DANE pinning, fall back to CA.
/// - Records present but DNSSEC proof is Insecure / Bogus / Indeterminate
///   → empty list + warn-log. Caller falls back to CA validation.
///   (Can't trust the records; an attacker may have spoofed them.)
/// - Resolver/network error → propagated. Caller logs + falls back.
///
/// Native-only (`outbound-net` feature): `hickory-resolver` does not
/// cross-compile to wasm32 and must not enter the client FFI surface.
#[cfg(feature = "outbound-net")]
pub async fn lookup_tlsa(mx_host: &str) -> anyhow::Result<Vec<TlsaRecord>> {
    use hickory_resolver::TokioResolver;
    use hickory_resolver::config::ResolverOpts;
    use hickory_resolver::proto::dnssec::Proof;
    use hickory_resolver::proto::rr::RData;

    let mut opts = ResolverOpts::default();
    opts.validate = true; // Require DNSSEC validation for the chain.
    let resolver = TokioResolver::builder_tokio()
        .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?
        .with_options(opts)
        .build()
        .map_err(|e| anyhow::anyhow!("build resolver: {e}"))?;

    let query = format!("_25._tcp.{}", mx_host.trim_end_matches('.'));
    let rrset = match resolver.tlsa_lookup(query).await {
        Ok(rrset) => rrset,
        Err(e) => {
            // NXDOMAIN / NoData → no DANE records published, not an error. hickory
            // 0.26 collapsed `ResolveErrorKind::Proto(ProtoErrorKind::NoRecordsFound)`
            // into `NetError::is_no_records_found()`.
            if e.is_no_records_found() {
                return Ok(Vec::new());
            }
            return Err(anyhow::anyhow!("TLSA lookup failed: {e}"));
        }
    };

    let mut records = Vec::new();
    let mut had_unsecure = false;
    // hickory 0.26: `tlsa_lookup` returns a generic `Lookup` whose records carry their
    // per-record DNSSEC `proof` (populated because `opts.validate = true`). A record is
    // trustworthy only when its proof is `Secure`; anything else (e.g. an `Insecure`
    // unsigned delegation) is treated as no DANE per RFC 7672 §2.2.2. A hard
    // validation failure surfaces as a lookup error, handled above.
    for record in rrset.answers() {
        if record.proof == Proof::Secure {
            if let RData::TLSA(tlsa) = &record.data {
                records.push(TlsaRecord {
                    usage: u8::from(tlsa.cert_usage),
                    selector: u8::from(tlsa.selector),
                    matching: u8::from(tlsa.matching),
                    data: tlsa.cert_data.to_vec(),
                });
            }
        } else {
            had_unsecure = true;
        }
    }

    if records.is_empty() && had_unsecure {
        // Records present but DNSSEC didn't prove them secure. RFC 7672
        // §2.2.2: treat as if no records — caller falls back to CA. Log
        // loudly because this could be an attacker spoofing TLSA.
        tracing::warn!(
            mx = %mx_host,
            "TLSA records present but DNSSEC proof not Secure — discarding (potential DNS spoof?)",
        );
    }

    Ok(records)
}

/// Live `TlsaResolver` over the DNSSEC-validating [`lookup_tlsa`]. The
/// production outbound path holds an `Arc<dyn TlsaResolver>`; this is the
/// network-touching impl (mirrors
/// [`crate::outbound::mta_sts::LiveMtaStsFetcher`]).
#[cfg(feature = "outbound-net")]
pub struct LiveTlsaResolver;

#[cfg(feature = "outbound-net")]
#[async_trait::async_trait]
impl TlsaResolver for LiveTlsaResolver {
    async fn lookup(&self, mx_host: &str) -> anyhow::Result<Vec<TlsaRecord>> {
        lookup_tlsa(mx_host).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One DER-cert chain as the raw `Vec<Vec<u8>>` the Go side passes
    /// (leaf-first); `bytes` is the synthetic leaf.
    fn chain(certs: &[&[u8]]) -> Vec<Vec<u8>> {
        certs.iter().map(|c| c.to_vec()).collect()
    }

    /// The MX host the connection was made to — the TLSA base domain, which
    /// DANE-TA (but not DANE-EE) name-checks the leaf against.
    const MX: &str = "mx.example.com";

    #[test]
    fn tlsa_policy_string_uses_rfc7672_presentation_form() {
        let rec = TlsaRecord {
            usage: 3,
            selector: 1,
            matching: 1,
            data: vec![0xde, 0xad, 0xbe, 0xef],
        };
        assert_eq!(tlsa_policy_string(&rec), "3 1 1 deadbeef");
    }

    #[test]
    fn tlsa_policy_strings_renders_each_record() {
        let recs = vec![
            TlsaRecord {
                usage: 3,
                selector: 1,
                matching: 1,
                data: vec![0xab, 0xcd],
            },
            TlsaRecord {
                usage: 2,
                selector: 0,
                matching: 2,
                data: vec![0x00, 0xff],
            },
        ];
        let strings = tlsa_policy_strings(&recs);
        assert_eq!(
            strings,
            vec!["3 1 1 abcd".to_string(), "2 0 2 00ff".to_string()]
        );
    }

    #[test]
    fn tlsa_policy_strings_empty_for_empty_input() {
        let recs: Vec<TlsaRecord> = Vec::new();
        assert!(tlsa_policy_strings(&recs).is_empty());
    }

    #[test]
    fn dane_ee_full_exact_matches() {
        let leaf = b"\x30\x82\x01\x00 fake leaf cert bytes";
        let record = TlsaRecord {
            usage: 3,    // DANE-EE
            selector: 0, // Full cert
            matching: 0, // Exact
            data: leaf.to_vec(),
        };
        assert!(dane_chain_matches(&[record], &chain(&[leaf]), MX));
    }

    #[test]
    fn dane_ee_full_sha256_matches() {
        let leaf = b"\x30\x82\x01\x00 another fake leaf";
        let record = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 1, // SHA-256
            data: Sha256::digest(leaf).to_vec(),
        };
        assert!(dane_chain_matches(&[record], &chain(&[leaf]), MX));
    }

    #[test]
    fn dane_ee_full_sha512_matches() {
        let leaf = b"\x30\x82\x01\x00 sha512 fake leaf";
        let record = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 2, // SHA-512
            data: Sha512::digest(leaf).to_vec(),
        };
        assert!(dane_chain_matches(&[record], &chain(&[leaf]), MX));
    }

    #[test]
    fn dane_ee_with_unrelated_intermediate_still_matches_leaf() {
        let leaf = b"leaf";
        let inter = b"intermediate";
        let record = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 0,
            data: leaf.to_vec(),
        };
        assert!(dane_chain_matches(&[record], &chain(&[leaf, inter]), MX));
    }

    #[test]
    fn dane_ee_does_not_match_intermediate_only() {
        let leaf = b"leaf";
        let inter = b"intermediate";
        let record = TlsaRecord {
            usage: 3, // DANE-EE matches leaf ONLY
            selector: 0,
            matching: 0,
            data: inter.to_vec(),
        };
        assert!(!dane_chain_matches(&[record], &chain(&[leaf, inter]), MX));
    }

    // ── DANE-TA (usage 2): the anchor has to actually anchor something ──
    //
    // These need REAL certificates, because the whole question is whether the
    // leaf descends from the matched cert — and the attack is a chain whose
    // bytes are individually genuine (the anchor is a verbatim copy of the
    // victim's) while the relationship between them is not. Synthetic byte
    // strings cannot express that difference.

    /// A minted CA and a leaf it really signed, plus the attacker's own
    /// self-signed leaf for the same name. DER, in the order Go presents.
    struct Certs {
        ca_der: Vec<u8>,
        leaf_der: Vec<u8>,
        attacker_leaf_der: Vec<u8>,
    }

    fn mint_certs(leaf_name: &str) -> Certs {
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .key_usages
            .push(rcgen::KeyUsagePurpose::KeyCertSign);
        ca_params.key_usages.push(rcgen::KeyUsagePurpose::CrlSign);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "fauna test anchor");
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let mut leaf_params = rcgen::CertificateParams::new(vec![leaf_name.to_string()]).unwrap();
        leaf_params
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        // Built from the params we just minted, not re-parsed out of
        // `ca_cert.der()`: `Issuer::from_ca_cert_der` lives behind rcgen's
        // `x509-parser` feature, which nothing in the workspace turns on, so
        // that form compiles only where some other crate happens to unify the
        // feature in. It carries the same subject DN and the same key, which
        // is all the path validation below reads.
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

        // The attacker's leaf: same name, their own key, signed by nobody but
        // themselves. Perfectly well-formed — it just descends from nothing.
        let mut evil_params = rcgen::CertificateParams::new(vec![leaf_name.to_string()]).unwrap();
        evil_params
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        let evil_key = rcgen::KeyPair::generate().unwrap();
        let evil_cert = evil_params.self_signed(&evil_key).unwrap();

        Certs {
            ca_der: ca_cert.der().to_vec(),
            leaf_der: leaf_cert.der().to_vec(),
            attacker_leaf_der: evil_cert.der().to_vec(),
        }
    }

    fn ta_record(cert_der: &[u8]) -> TlsaRecord {
        TlsaRecord {
            usage: 2,
            selector: 0,
            matching: 1, // SHA-256 of the full cert — the common `2 0 1`.
            data: Sha256::digest(cert_der).to_vec(),
        }
    }

    /// The honest case the usage-2 arm exists to serve: the peer presents a
    /// leaf its published anchor really signed.
    #[test]
    fn dane_ta_accepts_a_leaf_that_really_chains_to_the_pinned_anchor() {
        let c = mint_certs(MX);
        assert!(dane_chain_matches(
            &[ta_record(&c.ca_der)],
            &chain(&[&c.leaf_der, &c.ca_der]),
            MX,
        ));
    }

    /// **The attack this arm exists to refuse, and the pin that discriminates.**
    /// A TLSA association is a hash of a *public* certificate, and a trust
    /// anchor is the most public certificate there is — so an on-path attacker
    /// needs no CA compromise and no private key but their own: they present
    /// their own self-signed leaf with a verbatim copy of the victim's genuine
    /// anchor appended. Every byte in that chain is authentic; only the
    /// relationship is forged.
    ///
    /// A match-any-chain-cert matcher accepts it on the spot, which is what
    /// this pin was red-verified against.
    #[test]
    fn dane_ta_refuses_an_attacker_leaf_beside_a_copy_of_the_genuine_anchor() {
        let c = mint_certs(MX);
        assert!(
            !dane_chain_matches(
                &[ta_record(&c.ca_der)],
                &chain(&[&c.attacker_leaf_der, &c.ca_der]),
                MX,
            ),
            "a leaf that does not descend from the pinned anchor must be \
             refused, however genuine the anchor bytes beside it are"
        );
    }

    /// The identity half of RFC 7672 §3.1.1: an anchor delegates to whatever
    /// names it signs for, so a genuinely-issued cert for one host does not
    /// authenticate a connection to another.
    #[test]
    fn dane_ta_refuses_a_real_chain_presented_for_the_wrong_host() {
        let c = mint_certs("mx.elsewhere.example");
        assert!(!dane_chain_matches(
            &[ta_record(&c.ca_der)],
            &chain(&[&c.leaf_der, &c.ca_der]),
            MX,
        ));
    }

    /// The anchor may be the leaf itself. Nothing is left to chain — the peer
    /// presented exactly the pinned certificate, so it holds that
    /// certificate's private key, which is the same proof usage 3 accepts.
    /// Still name-checked, which usage 3 waives.
    #[test]
    fn dane_ta_accepts_the_pinned_cert_when_it_is_the_leaf_itself() {
        let c = mint_certs(MX);
        assert!(dane_chain_matches(
            &[ta_record(&c.attacker_leaf_der)],
            &chain(&[&c.attacker_leaf_der]),
            MX,
        ));
        // …and only for the name it carries.
        assert!(!dane_chain_matches(
            &[ta_record(&c.attacker_leaf_der)],
            &chain(&[&c.attacker_leaf_der]),
            "mx.elsewhere.example",
        ));
    }

    /// The negative control that keeps the tightening honest: DANE-EE (3) is
    /// unchanged. RFC 7672 §3.1.1 waives name and validity checks there on
    /// purpose — the record names *this exact key*, so holding its private
    /// key is the entire proof, and a self-signed cert with no name anyone
    /// recognises is the ordinary DANE-EE deployment.
    #[test]
    fn dane_ee_still_ignores_the_host_name() {
        let c = mint_certs("mx.elsewhere.example");
        let record = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 1,
            data: Sha256::digest(&c.leaf_der).to_vec(),
        };
        assert!(dane_chain_matches(
            &[record],
            &chain(&[&c.leaf_der, &c.ca_der]),
            MX,
        ));
    }

    #[test]
    fn pkix_class_records_skipped_in_smtp_dane_path() {
        let leaf = b"leaf";
        // Usage 0 (PKIX-TA) and 1 (PKIX-EE) are NOT supported by our
        // SMTP-DANE path (would need PKIX validation in addition).
        let records = vec![
            TlsaRecord {
                usage: 0,
                selector: 0,
                matching: 0,
                data: leaf.to_vec(),
            },
            TlsaRecord {
                usage: 1,
                selector: 0,
                matching: 0,
                data: leaf.to_vec(),
            },
        ];
        assert!(!dane_chain_matches(&records, &chain(&[leaf]), MX));
    }

    #[test]
    fn no_match_when_data_differs() {
        let leaf = b"real leaf";
        let record = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 1, // SHA-256
            data: Sha256::digest(b"different cert").to_vec(),
        };
        assert!(!dane_chain_matches(&[record], &chain(&[leaf]), MX));
    }

    #[test]
    fn first_match_short_circuits() {
        // Multiple records; one matches. Must return true.
        let leaf = b"leaf";
        let records = vec![
            TlsaRecord {
                usage: 3,
                selector: 0,
                matching: 0,
                data: b"won't match".to_vec(),
            },
            TlsaRecord {
                usage: 3,
                selector: 0,
                matching: 0,
                data: leaf.to_vec(),
            },
        ];
        assert!(dane_chain_matches(&records, &chain(&[leaf]), MX));
    }

    #[test]
    fn empty_chain_never_matches() {
        let record = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 0,
            data: b"anything".to_vec(),
        };
        assert!(!dane_chain_matches(&[record], &[], MX));
    }

    #[test]
    fn out_of_range_selector_or_matching_never_matches() {
        let leaf = b"leaf";
        let bad_selector = TlsaRecord {
            usage: 3,
            selector: 9,
            matching: 0,
            data: leaf.to_vec(),
        };
        let bad_matching = TlsaRecord {
            usage: 3,
            selector: 0,
            matching: 9,
            data: leaf.to_vec(),
        };
        assert!(!dane_chain_matches(&[bad_selector], &chain(&[leaf]), MX));
        assert!(!dane_chain_matches(&[bad_matching], &chain(&[leaf]), MX));
    }
}
