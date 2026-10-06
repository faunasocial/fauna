//! SHA-256 fingerprint of a cert's `SubjectPublicKeyInfo` — the shared helper
//! the TLS channel-binding leg (`docs/goal/architecture/security.md`
//! § Transport trust, Axis 1) is built on.
//!
//! Both ends of the binding compute this fingerprint and they MUST agree:
//! - the **nest** signs the SPKI of the cert *it* serves
//!   (`bins/fauna-nest/src/acme.rs::ServedCertSpki`), and
//! - the **client** recomputes it from the cert *it* received during the rustls
//!   handshake (`fauna-anon-client`'s capturing verifier).
//!
//! Keeping the one function here — next to [`crate::auth::CertBinding`], the
//! wire type that carries the value — is why the two never diverge (priority
//! #4). It is feature-gated (`tls-spki`) so only the native consumers that parse
//! X.509 (nest + the native client crates) pull in `x509-parser`/`sha2`; the
//! wasm build, which has no rustls handshake to capture a cert from, omits it.
//!
//! Fingerprints the **SPKI**, never the whole cert, so a benign re-issuance with
//! the same key (90-day self-signed expiry, ACME self-heal) keeps the same
//! fingerprint and the identity pin survives the rotation.

use sha2::{Digest, Sha256};

/// SHA-256 of the `SubjectPublicKeyInfo` of a DER-encoded X.509 certificate.
/// Returns `None` if the DER does not parse as an X.509 cert.
pub fn spki_sha256_of_cert_der(cert_der: &[u8]) -> Option<[u8; 32]> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der).ok()?;
    // `raw` is the exact DER of the SubjectPublicKeyInfo as it appeared in the
    // cert — hashing the on-the-wire bytes keeps client and nest in agreement.
    let spki_der = cert.tbs_certificate.subject_pki.raw;
    Some(Sha256::digest(spki_der).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_cert_bytes() {
        assert!(spki_sha256_of_cert_der(b"not a certificate").is_none());
    }
}
