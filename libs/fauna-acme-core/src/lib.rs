//! The shared ACME order-finalization tail: generate a cert keypair, build a
//! CSR, finalize the order, and poll for the issued certificate.
//!
//! Byte-identical between `fauna-acme-http01`'s HTTP-01 loop and
//! `fauna-client-dns::acme_order`'s DNS-01 loop once each driver's own
//! challenge type has been satisfied and the order is `Ready` — both crates'
//! own doc comments already named the mirror before this crate existed.
//! Everything *before* this point (account load/create, per-authorization
//! challenge presentation, order-ready poll) stays in each driver: it differs
//! by challenge type. The order-ready poll's `Invalid` arm shares only its
//! diagnostic here ([`describe_invalid_order`]): the HTTP-01 driver uses it;
//! DNS-01's arm still hand-rolls an identifier-only variant of the same
//! re-fetch (a real incident added it — `tls-certificates.md` § C) and can
//! adopt the builder to also name the challenge error.

use std::time::Duration;

/// One failed challenge's CA-reported problem, owned so the message builder is
/// pure (no `instant_acme` borrow) and unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChallengeFailure {
    /// The challenge type, e.g. `http-01`.
    pub challenge_type: String,
    /// The ACME problem type, e.g. `urn:ietf:params:acme:error:connection`.
    pub problem_type: Option<String>,
    /// The CA's human-readable detail, e.g. "Timeout during connect (likely
    /// firewall problem)".
    pub detail: Option<String>,
}

/// One authorization of an invalid order, with the problems on its challenges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationFailure {
    /// The identifier (hostname or IP) the authorization covers.
    pub identifier: String,
    /// The authorization status, e.g. `Invalid`.
    pub status: String,
    /// Every challenge of this authorization that carries an error.
    pub challenges: Vec<ChallengeFailure>,
}

/// Build the message for an order that went `invalid`. The order-level error is
/// frequently empty — the reason lives on the failed authorization's challenge —
/// so the message names each non-valid authorization's identifier and every
/// challenge problem (type + detail) the CA reported.
pub fn format_invalid_order(
    order_error: Option<&str>,
    authorizations: &[AuthorizationFailure],
) -> String {
    let mut out = format!(
        "ACME order became invalid (order-level error: {})",
        order_error.unwrap_or("none")
    );
    if authorizations.is_empty() {
        out.push_str("; no authorization detail available");
        return out;
    }
    for auth in authorizations {
        out.push_str(&format!(
            "; authorization for {} is {}",
            auth.identifier, auth.status
        ));
        for c in &auth.challenges {
            out.push_str(&format!(
                " [{} challenge: {} — {}]",
                c.challenge_type,
                c.problem_type.as_deref().unwrap_or("unknown problem type"),
                c.detail.as_deref().unwrap_or("no detail"),
            ));
        }
    }
    out
}

/// Re-walk `order`'s authorizations and build the [`format_invalid_order`]
/// message for it. `order_error` is the already-formatted order-level error
/// (captured by the caller before this re-borrows the order).
pub async fn describe_invalid_order(
    order: &mut instant_acme::Order,
    order_error: Option<String>,
) -> String {
    let mut failures: Vec<AuthorizationFailure> = Vec::new();
    let mut walk_error: Option<String> = None;
    let mut authorizations = order.authorizations();
    while let Some(auth) = authorizations.next().await {
        match auth {
            Ok(auth) => {
                // Valid authorizations did not cause the failure.
                if matches!(auth.status, instant_acme::AuthorizationStatus::Valid) {
                    continue;
                }
                let identifier = match auth.identifier().identifier {
                    instant_acme::Identifier::Dns(d) => d.clone(),
                    other => format!("{other:?}"),
                };
                let challenges = auth
                    .challenges
                    .iter()
                    .filter_map(|c| {
                        c.error.as_ref().map(|p| ChallengeFailure {
                            challenge_type: format!("{:?}", c.r#type),
                            problem_type: p.r#type.clone(),
                            detail: p.detail.clone(),
                        })
                    })
                    .collect();
                failures.push(AuthorizationFailure {
                    identifier,
                    status: format!("{:?}", auth.status),
                    challenges,
                });
            }
            Err(e) => {
                walk_error = Some(e.to_string());
                break;
            }
        }
    }
    let mut msg = format_invalid_order(order_error.as_deref(), &failures);
    if let Some(e) = walk_error {
        msg.push_str(&format!("; authorizations unavailable: {e}"));
    }
    msg
}

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
    use super::*;

    /// The 2026-09-29 incident: firewalled tcp/80 gave an empty order-level
    /// error and the cause only on the challenge. The message must name the
    /// identifier and the CA's detail.
    #[test]
    fn invalid_order_message_names_identifier_and_challenge_detail() {
        let msg = format_invalid_order(
            None,
            &[AuthorizationFailure {
                identifier: "mail.example.net".to_string(),
                status: "Invalid".to_string(),
                challenges: vec![ChallengeFailure {
                    challenge_type: "Http01".to_string(),
                    problem_type: Some("urn:ietf:params:acme:error:connection".to_string()),
                    detail: Some("Timeout during connect (likely firewall problem)".to_string()),
                }],
            }],
        );
        assert!(msg.contains("mail.example.net"), "{msg}");
        assert!(
            msg.contains("urn:ietf:params:acme:error:connection"),
            "{msg}"
        );
        assert!(
            msg.contains("Timeout during connect (likely firewall problem)"),
            "{msg}"
        );
    }

    #[test]
    fn invalid_order_message_without_authorizations_still_reports() {
        let msg = format_invalid_order(Some("boom"), &[]);
        assert!(
            msg.contains("boom") && msg.contains("no authorization detail"),
            "{msg}"
        );
    }

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
