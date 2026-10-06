//! ARC (Authenticated Received Chain, RFC 8617) seal-on-relay primitive.
//!
//! Lifted from `libs/fauna-bridge-smtp/src/outbound.rs::arc_sign` so the Go
//! mail bridge AND the legacy in-nest plaintext path can share one
//! implementation. Implements `docs/goal/behavior/smtp-server.md` § Outbound
//! delivery (ARC seal-on-relay).
//!
//! Unlike DKIM signing (origin-side, always-on outbound), ARC is for
//! **relay** scenarios — a message arrives at this domain with one or more
//! prior `Authentication-Results` headers (the relay has computed its own AR
//! header before re-emitting). The relay's ARC chain pins the prior verdict
//! so downstream verifiers can trust the relay's judgement when DKIM
//! verification would otherwise fail on the forwarded body.
//!
//! Today's external caller surface: none in-tree. The original `arc_sign`
//! shipped speculatively for a future relay path. This lift to shared Rust
//! is a priority-#4 unify-ahead move: keeps the impl in one place when the
//! future Go relay caller materializes (Phase E or later). The legacy
//! `libs/fauna-bridge-smtp::arc_sign`
//! shape is preserved behind a delegating wrapper so any callers added
//! between now and the cutover keep working.

use mail_auth::common::crypto::{Ed25519Key, RsaKey, Sha256};
use mail_auth::common::headers::HeaderWriter;
use mail_auth::{ArcOutput, AuthenticatedMessage, AuthenticationResults, arc::ArcSealer};
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;

use crate::outbound::dkim::{SigningAlg, SigningKey, rsa_modulus_bits};

/// Header set canonicalised + signed by the ARC sealer. Includes the
/// existing `DKIM-Signature` header so downstream verifiers can confirm
/// the relay didn't substitute the DKIM payload between authentication
/// and seal.
const SEALED_HEADERS: [&str; 6] = [
    "From",
    "To",
    "Subject",
    "Date",
    "Message-ID",
    "DKIM-Signature",
];

/// ARC sealing errors. Distinguished from generic `anyhow` so the FFI
/// surface (when the relay path adds one) and Go consumers can match on
/// the cause.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("no signing keys provided")]
    Empty,
    #[error("parse message for ARC sealing failed")]
    Parse,
    #[error("parse RSA PEM: {0}")]
    RsaPemParse(String),
    #[error("parse RSA key: {0}")]
    RsaKey(String),
    #[error("RSA ARC key too small: {0} bits (RFC 8301 floor is 2048)")]
    RsaKeyTooSmall(usize),
    #[error("parse Ed25519 key: {0}")]
    Ed25519Key(String),
    #[error("ARC seal: {0}")]
    Seal(String),
}

/// ARC-seal a raw RFC 5322 message being relayed (RFC 8617).
///
/// The relay computes `auth_results` from `verify_inbound`'s verdicts and
/// passes `arc_output` from a fresh ARC chain parse. The first key in
/// `keys` is used to sign the ARC set; production runs single-key (matching
/// the pre-lift legacy shape), but the slice signature future-proofs the
/// dual-sign case the DKIM primitive already exposes.
///
/// Returns the rebuilt message with `ARC-Seal` + `ARC-Message-Signature` +
/// `ARC-Authentication-Results` + `Authentication-Results` headers
/// prepended to the original bytes.
///
/// # Errors
///
/// * [`SealError::Empty`] if `keys` is empty.
/// * [`SealError::Parse`] if `raw_message` is not a parseable RFC 5322
///   message (no end-of-headers, malformed shape).
/// * [`SealError::RsaPemParse`] / [`SealError::RsaKey`] / [`SealError::Ed25519Key`]
///   if the first key's `priv_key` bytes don't parse for the declared `alg`.
/// * [`SealError::RsaKeyTooSmall`] if an RSA key's modulus is below the RFC 8301
///   2048-bit floor.
/// * [`SealError::Seal`] if `mail_auth::arc::ArcSealer::seal` fails (chain
///   inconsistency, builder rejection).
pub fn seal<'x>(
    raw_message: &[u8],
    auth_results: &'x AuthenticationResults<'x>,
    arc_output: &ArcOutput<'x>,
    keys: &[SigningKey],
) -> Result<Vec<u8>, SealError> {
    if keys.is_empty() {
        return Err(SealError::Empty);
    }

    let auth_msg = AuthenticatedMessage::parse(raw_message).ok_or(SealError::Parse)?;

    // Single-key ARC seal matches the pre-lift behaviour; the slice
    // signature lets future relay code emit one ARC set per configured
    // key without an API break.
    let key = &keys[0];
    let arc_set = match key.alg {
        SigningAlg::Ed25519 => {
            let ed_key = Ed25519Key::from_pkcs8_maybe_unchecked_der(&key.priv_key)
                .map_err(|e| SealError::Ed25519Key(e.to_string()))?;
            let sealer = ArcSealer::from_key(ed_key)
                .domain(&key.domain)
                .selector(&key.selector)
                .headers(SEALED_HEADERS);
            sealer
                .seal(&auth_msg, auth_results, arc_output)
                .map_err(|e| SealError::Seal(format!("{e:?}")))?
        }
        SigningAlg::RsaSha256 => {
            let key_der = PrivateKeyDer::from_pem_slice(&key.priv_key)
                .map_err(|e| SealError::RsaPemParse(e.to_string()))?;
            let bits = rsa_modulus_bits(key_der.secret_der()).map_err(SealError::RsaKey)?;
            if bits < 2048 {
                return Err(SealError::RsaKeyTooSmall(bits));
            }
            let pk = RsaKey::<Sha256>::from_key_der(key_der)
                .map_err(|e| SealError::RsaKey(e.to_string()))?;
            let sealer = ArcSealer::from_key(pk)
                .domain(&key.domain)
                .selector(&key.selector)
                .headers(SEALED_HEADERS);
            sealer
                .seal(&auth_msg, auth_results, arc_output)
                .map_err(|e| SealError::Seal(format!("{e:?}")))?
        }
    };

    let arc_header = arc_set.to_header();
    let ar_header = auth_results.to_header();

    let mut result = Vec::with_capacity(raw_message.len() + arc_header.len() + ar_header.len());
    result.extend_from_slice(arc_header.as_bytes());
    result.extend_from_slice(ar_header.as_bytes());
    result.extend_from_slice(raw_message);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ar<'x>() -> AuthenticationResults<'x> {
        AuthenticationResults::new("example.test")
    }

    fn empty_arc<'x>() -> ArcOutput<'x> {
        ArcOutput::default()
    }

    /// Empty keys → `SealError::Empty`. Guards against an accidental
    /// "seal without configured keys" callsite shipping an unsealed
    /// relayed message.
    #[test]
    fn empty_keys_errors() {
        let err = seal(b"hello\r\n", &ar(), &empty_arc(), &[]).unwrap_err();
        assert!(matches!(err, SealError::Empty));
    }

    /// Garbage (no header section, no CRLFCRLF) → `SealError::Parse`.
    /// `mail-auth` returns `None` for messages it cannot tokenize at all.
    #[test]
    fn malformed_message_errors() {
        let err = seal(
            b"\x00\x01\x02not a message",
            &ar(),
            &empty_arc(),
            &[SigningKey {
                alg: SigningAlg::Ed25519,
                priv_key: vec![0u8; 4], // never reached — parse fails first
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SealError::Parse), "got {err:?}");
    }

    /// Malformed Ed25519 key bytes → `SealError::Ed25519Key`. Same intent
    /// as DKIM's malformed-Ed25519 test.
    #[test]
    fn malformed_ed25519_key_errors() {
        let err = seal(
            b"From: a@example.test\r\nTo: b@example.test\r\nSubject: hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <x@example.test>\r\n\r\nbody\r\n",
            &ar(),
            &empty_arc(),
            &[SigningKey {
                alg: SigningAlg::Ed25519,
                priv_key: vec![0u8; 4],
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SealError::Ed25519Key(_)), "got {err:?}");
    }

    /// Malformed RSA PEM → `SealError::RsaPemParse`. Same intent as DKIM's
    /// malformed-RSA-PEM test.
    #[test]
    fn malformed_rsa_pem_errors() {
        let err = seal(
            b"From: a@example.test\r\nTo: b@example.test\r\nSubject: hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <x@example.test>\r\n\r\nbody\r\n",
            &ar(),
            &empty_arc(),
            &[SigningKey {
                alg: SigningAlg::RsaSha256,
                priv_key: b"not a PEM".to_vec(),
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SealError::RsaPemParse(_)), "got {err:?}");
    }

    /// Generate an RSA private key of the given modulus size, PKCS#8 PEM
    /// encoded exactly as `SigningKey::priv_key` expects for `RsaSha256`
    /// (`seal`'s RSA arm parses via `PrivateKeyDer::from_pem_slice`). Copied
    /// verbatim from `dkim.rs`'s test module (test modules are private to
    /// their file; a 10-line duplicate beats restructuring test modules for
    /// this).
    fn generate_rsa_pem(bits: usize) -> Vec<u8> {
        use rsa::pkcs8::{EncodePrivateKey as _, LineEnding};
        let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, bits).expect("rsa keygen");
        key.to_pkcs8_pem(LineEnding::LF)
            .expect("rsa pkcs8 pem")
            .as_bytes()
            .to_vec()
    }

    /// A sub-2048-bit RSA key must be rejected at the seal site (RFC 8301's
    /// 2048-bit floor, which RFC 8617 §4.1.2 carries over from DKIM to ARC),
    /// never silently sealed with a weak key.
    #[test]
    fn rsa_key_below_2048_bits_errors() {
        let priv_key = generate_rsa_pem(1024);
        let msg = b"From: a@example.test\r\nTo: b@example.test\r\nSubject: hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <x@example.test>\r\n\r\nbody\r\n";
        let err = seal(
            msg,
            &ar(),
            &empty_arc(),
            &[SigningKey {
                alg: SigningAlg::RsaSha256,
                priv_key,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, SealError::RsaKeyTooSmall(1024)), "{err:?}");
    }

    /// Companion green case: a conformant 2048-bit RSA key still seals,
    /// proving the floor admits exactly what RFC 8301 requires and nothing
    /// less.
    #[test]
    fn rsa_key_at_2048_bits_seals() {
        let priv_key = generate_rsa_pem(2048);
        let msg = b"From: a@example.test\r\nTo: b@example.test\r\nSubject: hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <x@example.test>\r\n\r\nbody\r\n";
        let sealed = seal(
            msg,
            &ar(),
            &empty_arc(),
            &[SigningKey {
                alg: SigningAlg::RsaSha256,
                priv_key,
                selector: "sel1".to_string(),
                domain: "example.test".to_string(),
            }],
        );
        assert!(sealed.is_ok(), "{:?}", sealed.err());
    }
}
