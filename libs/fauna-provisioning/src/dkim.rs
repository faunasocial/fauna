//! DKIM keypair generation (RSA-2048, RFC 6376; Ed25519, RFC 8463).
//!
//! The nest mints each mail domain's DKIM key with [`mint_signing_key`], seals
//! the private half under its own key-encryption key
//! (`bins/fauna-nest/src/mail_dkim_key.rs`), signs with it at the outbound
//! hand-out, and serves the public DNS TXT value the admin publishes from the
//! `admin-dns` page (`docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM
//! provisioning).
//!
//! There is **no** client-side cloud-init DKIM path: a client-generated key
//! embedded in cloud-init is one the nest never signs with (`dkim=fail` from
//! day one). The keygen's default selectors are `default` (RSA) / `ed25519`;
//! the nest stores each key under the domain's configured selector.

use base64::Engine;
use ed25519_dalek::pkcs8::EncodePrivateKey as _;
use rand::rngs::OsRng;
use rsa::pkcs8::{EncodePublicKey as _, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};

use crate::error::ProvisionError;

/// A freshly-generated DKIM keypair: PKCS#8 PEM private key plus the public
/// key formatted as the DNS TXT value. Produced by [`generate_rsa_2048`] /
/// [`generate_ed25519`] and consumed by [`mint_signing_key`], which re-encodes
/// the private half for the signer and hands the public TXT value back to the
/// nest to persist + publish.
#[derive(Debug, Clone)]
pub struct DkimKeypair {
    /// `default` for RSA, `ed25519` for Ed25519. The DNS record *name* is
    /// `{selector}._domainkey.{domain}`; the nest stores the key under the
    /// domain's configured selector instead.
    pub selector: String,
    /// PKCS#8 PEM-encoded private key. [`mint_signing_key`] re-encodes it into
    /// the signer's encoding.
    pub private_pem: String,
    /// Full DKIM TXT record value, e.g. `v=DKIM1; k=rsa; p=...`. The nest
    /// persists it beside the sealed key and the admin publishes it.
    pub public_dns_value: String,
}

/// Generate a fresh RSA-2048 DKIM keypair under the `default` selector.
///
/// Public-key encoding matches RFC 6376 §3.6.1: base64 of the DER-encoded
/// PKIX SubjectPublicKeyInfo (i.e. the contents of `RsaPublicKey::to_public_key_der`),
/// stripped of PEM framing. RSA keygen is the heavy step here — ~50ms on
/// desktop, ~200ms on phone.
pub fn generate_rsa_2048() -> Result<DkimKeypair, ProvisionError> {
    let mut rng = OsRng;
    let private_key = RsaPrivateKey::new(&mut rng, 2048)
        .map_err(|e| ProvisionError::Other(format!("rsa keygen: {e}")))?;
    let public_key = RsaPublicKey::from(&private_key);

    let private_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| ProvisionError::Other(format!("rsa pkcs8 pem: {e}")))?
        .to_string();

    let spki_der = public_key
        .to_public_key_der()
        .map_err(|e| ProvisionError::Other(format!("rsa spki der: {e}")))?;
    let spki_b64 = base64::engine::general_purpose::STANDARD.encode(spki_der.as_bytes());

    Ok(DkimKeypair {
        selector: "default".to_string(),
        private_pem,
        public_dns_value: format!("v=DKIM1; k=rsa; p={spki_b64}"),
    })
}

/// Generate a fresh Ed25519 DKIM keypair under the `ed25519` selector.
///
/// Per RFC 8463 §3 the DKIM public-key value is the base64 of the raw
/// 32-byte Ed25519 public key (NOT the SPKI wrapper used for RSA). Keygen
/// is sub-millisecond.
pub fn generate_ed25519() -> Result<DkimKeypair, ProvisionError> {
    let mut rng = OsRng;
    let signing_key = ed25519_dalek::SigningKey::generate(&mut rng);
    let verifying_key = signing_key.verifying_key();

    let private_pem = signing_key
        .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
        .map_err(|e| ProvisionError::Other(format!("ed25519 pkcs8 pem: {e}")))?
        .to_string();

    let public_b64 = base64::engine::general_purpose::STANDARD.encode(verifying_key.to_bytes());

    Ok(DkimKeypair {
        selector: "ed25519".to_string(),
        private_pem,
        public_dns_value: format!("v=DKIM1; k=ed25519; p={public_b64}"),
    })
}

// ── The signer's key encoding ───────────────────────────────────────────────
//
// The nest-held key (`bins/fauna-nest/src/mail_dkim_key.rs`, sealed under the
// nest's own key-encryption key) is minted here.

/// DKIM key algorithm for [`mint_signing_key`]. [`Self::wire_alg`] gives its
/// stored name (`"ed25519"` / `"rsa-sha256"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DkimAlgorithm {
    /// Ed25519 (RFC 8463). Fast keygen; the modern default.
    Ed25519,
    /// RSA-2048 (RFC 6376). Slower keygen; maximal interop.
    Rsa2048,
}

impl DkimAlgorithm {
    /// The algorithm's name where a key rests: the nest's
    /// `mail_dkim_keys.alg`.
    pub fn wire_alg(self) -> &'static str {
        match self {
            DkimAlgorithm::Ed25519 => "ed25519",
            DkimAlgorithm::Rsa2048 => "rsa-sha256",
        }
    }
}

/// A freshly minted DKIM key in the encoding the signer consumes
/// (`fauna_mail::outbound::dkim::SigningKey::priv_key`), beside the DNS TXT
/// value that publishes its public half.
pub struct DkimSigningMaterial {
    /// PKCS#8 **DER** for Ed25519 (`Ed25519Key::from_pkcs8_maybe_unchecked_der`),
    /// PKCS#8 **PEM** for RSA (`RsaPrivateKey::from_pkcs8_pem`). The keygen
    /// above emits PEM for both; this is where the two encodings meet.
    pub priv_key: Vec<u8>,
    /// Full DKIM TXT record value, e.g. `v=DKIM1; k=ed25519; p=...`.
    pub public_dns_value: String,
}

/// Mint a DKIM keypair for `alg` in the signer's encoding. Pure modulo keygen
/// randomness; the caller seals `priv_key` and drops it.
pub fn mint_signing_key(alg: DkimAlgorithm) -> Result<DkimSigningMaterial, ProvisionError> {
    use ed25519_dalek::pkcs8::DecodePrivateKey as _;

    let keypair = match alg {
        DkimAlgorithm::Ed25519 => generate_ed25519(),
        DkimAlgorithm::Rsa2048 => generate_rsa_2048(),
    }?;
    let priv_key = match alg {
        DkimAlgorithm::Ed25519 => {
            let sk = ed25519_dalek::SigningKey::from_pkcs8_pem(&keypair.private_pem)
                .map_err(|e| ProvisionError::Other(format!("dkim ed25519 pem parse: {e}")))?;
            sk.to_pkcs8_der()
                .map_err(|e| ProvisionError::Other(format!("dkim ed25519 der encode: {e}")))?
                .as_bytes()
                .to_vec()
        }
        DkimAlgorithm::Rsa2048 => keypair.private_pem.into_bytes(),
    };
    Ok(DkimSigningMaterial {
        priv_key,
        public_dns_value: keypair.public_dns_value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsa_2048_pem_round_trip() {
        let kp = generate_rsa_2048().expect("rsa keygen");
        assert_eq!(kp.selector, "default");
        assert!(kp.private_pem.starts_with("-----BEGIN PRIVATE KEY-----")); // gitleaks:allow
        assert!(kp.private_pem.contains("-----END PRIVATE KEY-----"));
        // Re-parse the PEM to confirm it round-trips.
        use rsa::pkcs8::DecodePrivateKey as _;
        RsaPrivateKey::from_pkcs8_pem(&kp.private_pem).expect("re-parse private pem");
    }

    #[test]
    fn rsa_2048_dns_value_format() {
        let kp = generate_rsa_2048().expect("rsa keygen");
        assert!(kp.public_dns_value.starts_with("v=DKIM1; k=rsa; p="));
        let p_part = kp
            .public_dns_value
            .strip_prefix("v=DKIM1; k=rsa; p=")
            .unwrap();
        assert!(!p_part.is_empty());
        // Base64 round-trip — the public DER decodes back to a public key.
        let der = base64::engine::general_purpose::STANDARD
            .decode(p_part)
            .expect("base64 decode");
        use rsa::pkcs8::DecodePublicKey as _;
        RsaPublicKey::from_public_key_der(&der).expect("spki decode");
    }

    #[test]
    fn ed25519_dns_value_format() {
        let kp = generate_ed25519().expect("ed25519 keygen");
        assert_eq!(kp.selector, "ed25519");
        assert!(kp.public_dns_value.starts_with("v=DKIM1; k=ed25519; p="));
        let p_part = kp
            .public_dns_value
            .strip_prefix("v=DKIM1; k=ed25519; p=")
            .unwrap();
        // RFC 8463: 32 raw bytes, base64-encoded → 44 characters.
        assert_eq!(p_part.len(), 44);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(p_part)
            .expect("base64 decode");
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn ed25519_pem_round_trip() {
        let kp = generate_ed25519().expect("ed25519 keygen");
        assert!(kp.private_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        use ed25519_dalek::pkcs8::DecodePrivateKey as _;
        ed25519_dalek::SigningKey::from_pkcs8_pem(&kp.private_pem).expect("re-parse ed25519 pem");
    }

    /// The lifted keygen hands back each algorithm's key in the encoding the
    /// signer parses: PKCS#8 DER for Ed25519, PKCS#8 PEM for RSA.
    #[test]
    fn mint_signing_key_emits_the_signers_encoding() {
        use ed25519_dalek::pkcs8::DecodePrivateKey as _;
        let ed = mint_signing_key(DkimAlgorithm::Ed25519).expect("ed25519");
        ed25519_dalek::SigningKey::from_pkcs8_der(&ed.priv_key).expect("ed25519 pkcs8 der");
        assert!(ed.public_dns_value.starts_with("v=DKIM1; k=ed25519; p="));
        let rsa = mint_signing_key(DkimAlgorithm::Rsa2048).expect("rsa");
        RsaPrivateKey::from_pkcs8_pem(std::str::from_utf8(&rsa.priv_key).expect("pem is utf-8"))
            .expect("rsa pkcs8 pem");
        assert!(rsa.public_dns_value.starts_with("v=DKIM1; k=rsa; p="));
    }

    #[test]
    fn keys_are_unique_across_calls() {
        let a = generate_rsa_2048().unwrap();
        let b = generate_rsa_2048().unwrap();
        assert_ne!(a.public_dns_value, b.public_dns_value);
        let c = generate_ed25519().unwrap();
        let d = generate_ed25519().unwrap();
        assert_ne!(c.public_dns_value, d.public_dns_value);
    }
}
