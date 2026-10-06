//! The shared ACME order-finalization tail: generate a cert keypair, build a
//! CSR, finalize the order, and poll for the issued certificate.
//!
//! Byte-identical between `fauna-acme-http01`'s HTTP-01 loop and
//! `fauna-client-dns::acme_order`'s DNS-01 loop once each driver's own
//! challenge type has been satisfied and the order is `Ready` — both crates'
//! own doc comments already named the mirror before this crate existed.
//! Everything *before* this point (account load/create, per-authorization
//! challenge presentation, order-ready poll) stays in each driver: it differs
//! by challenge type, and the order-ready poll has already diverged
//! (DNS-01's `Invalid` arm re-fetches authorizations for a diagnostic a real
//! incident added — `tls-certificates.md` § C).

use std::time::Duration;

/// Generating the CSR, or a CA-side step (finalize, certificate fetch, or the
/// certificate not appearing within the poll budget) failed.
#[derive(Debug, thiserror::Error)]
pub enum FinalizeError {
    /// Generating the keypair or serializing the CSR failed.
    #[error("certificate signing request: {0}")]
    Csr(String),
    /// Finalizing the order or fetching the issued certificate failed, or the
    /// certificate did not appear within the poll budget.
    #[error("ACME CA: {0}")]
    Ca(String),
}

/// Generate a fresh keypair, build a CSR for `domains`, finalize `order`, then
/// poll for the issued certificate (10 attempts, 2s apart — the budget both
/// prior copies used).
///
/// Returns `(cert_chain_pem, cert_key_pem)`. Caller's job: persist the pair
/// (to disk, or sealed to a nest) however its own flow requires.
pub async fn finalize_order_and_fetch_certificate(
    order: &mut instant_acme::Order,
    domains: &[String],
) -> Result<(String, String), FinalizeError> {
    let cert_key = rcgen::KeyPair::generate()
        .map_err(|e| FinalizeError::Csr(format!("generate cert key pair: {e}")))?;
    let mut params = rcgen::CertificateParams::new(domains.to_vec())
        .map_err(|e| FinalizeError::Csr(format!("create CSR params: {e}")))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let csr = params
        .serialize_request(&cert_key)
        .map_err(|e| FinalizeError::Csr(format!("serialize CSR: {e}")))?;

    // `finalize_csr`, not `finalize`: since instant-acme 0.8 the bare `finalize`
    // generates its own keypair and CSR internally, which would strand us without
    // the private key we must persist beside the chain. `finalize_csr` keeps the
    // rcgen keypair above ours — the shape both drivers have always had.
    order
        .finalize_csr(csr.der())
        .await
        .map_err(|e| FinalizeError::Ca(format!("finalize order: {e}")))?;

    let mut cert_retries = 10u8;
    let cert_chain_pem = loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match order
            .certificate()
            .await
            .map_err(|e| FinalizeError::Ca(format!("get certificate: {e}")))?
        {
            Some(cert) => break cert,
            None => {
                cert_retries = cert_retries.checked_sub(1).ok_or_else(|| {
                    FinalizeError::Ca("certificate not available after 10 polls".to_string())
                })?;
            }
        }
    };

    Ok((cert_chain_pem, cert_key.serialize_pem()))
}

#[cfg(test)]
mod tests {
    /// The finalize tail takes its SAN set as `&[String]`, so an **IP** SAN and a
    /// **DNS** SAN reach it spelled identically — and RFC 8738 requires the CSR to
    /// carry an `iPAddress` GeneralName, not a `dNSName` holding digits, or the CA
    /// rejects the finalize. `rcgen::CertificateParams::new` makes that call for us
    /// by parsing each string, so the IP bridge cert (`tls-certificates.md` § B-IP)
    /// needs no separate CSR path — this pins the classification we rely on, since
    /// it is invisible at the call site and a silent flip to `DnsName` would fail
    /// only against a live CA.
    #[test]
    fn an_ip_literal_becomes_an_ip_san_and_a_hostname_a_dns_san() {
        let params = rcgen::CertificateParams::new(vec![
            "203.0.113.7".to_string(),
            "2001:db8::1".to_string(),
            "nest.example.com".to_string(),
        ])
        .expect("params from a mixed IP/DNS SAN set");

        let sans = params.subject_alt_names;
        assert_eq!(sans.len(), 3, "one SAN per input string: {sans:?}");
        assert!(
            matches!(sans[0], rcgen::SanType::IpAddress(ip) if ip == "203.0.113.7".parse::<std::net::IpAddr>().unwrap()),
            "an IPv4 literal must become an iPAddress GeneralName, got {:?}",
            sans[0]
        );
        assert!(
            matches!(sans[1], rcgen::SanType::IpAddress(ip) if ip == "2001:db8::1".parse::<std::net::IpAddr>().unwrap()),
            "an IPv6 literal must become an iPAddress GeneralName, got {:?}",
            sans[1]
        );
        assert!(
            matches!(&sans[2], rcgen::SanType::DnsName(n) if n.as_str() == "nest.example.com"),
            "a hostname must stay a dNSName, got {:?}",
            sans[2]
        );
    }
}
