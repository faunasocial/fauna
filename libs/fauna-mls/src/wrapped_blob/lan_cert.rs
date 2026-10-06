//! LAN-TLS cert distribution over the namespace-sync channel.
//!
//! The optional publicly-trusted TLS cert for a **private (home) nest's**
//! LAN-bound IMAP/CalDAV listener is distributed from the issuing side to the
//! private nest as a `namespace_entries` payload — the existing namespace-sync
//! channel, **no new transport** — per
//! `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
//! § MUA reach / § Cert provisioning summary / § Implementation status Slice 4.
//!
//! **Trust shape (the load-bearing decision).** A semi-trusted public relay
//! nest carries the entry but must not read the cert's private key, and
//! nest-core on the private side cannot open *actor*-sealed content (only the
//! MDA can — it alone unwraps the user's MSEK-derived snapshot). So the cert is
//! HPKE-sealed to the **private nest's identity-derived x25519 pubkey** (the key
//! nest-core holds, already published + pinned by clients at pairing), and the
//! entry is **Ed25519-signed by the actor** so a substituted cert from a
//! compromised relay is rejected. The private nest verifies the signature,
//! unseals with its identity x25519 secret, and writes the PEM plaintext to its
//! `acme_dir`, where the existing MDA `fetch_tls_cert_blob` seal-on-read serves
//! it on the LAN listener (zero Go/MDA changes).
//!
//! The Ed25519↔x25519 conversion (`fauna_core::identity::to_x25519_{public,
//! secret}`) happens at the call site (nest-core / the client producer);
//! this module is conversion-agnostic — it takes raw x25519 key bytes. Both the
//! producer (the admin's client — the client-driven DNS-01 issuance flow in
//! `fauna-client-dns` drives it via `seal_lan_tls_cert_entry`) and the
//! consumer (nest-core) share the one convention here.

// verify-ok(caller-supplied key): the verifying key arrives as a parameter from
// a caller that already knows whose signature it expects — it never rides the
// wire beside the signature, so the weak-key class does not apply (and the AEAD
// gate authenticates the payload first). A self-describing key would owe
// `fauna_core::identity::verify_detached`.
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::wrapped_blob::format::{TlsCertBlob, TlsCertBundle, UnwrapError, WrapError};
use crate::wrapped_blob::{seal_tls_cert, unseal_tls_cert};

/// Well-known `namespace_entries.entry_id` addressing the LAN IMAP/CalDAV cert
/// within the actor's self-namespace. (`entry_id` is an opaque BLOB — unlike
/// `namespace`, it is not parsed as a 32-byte id — so a readable tag is fine.)
pub const LAN_TLS_CERT_ENTRY_ID: &[u8] = b"fauna.lan-tls.imap-caldav.v1";

/// Synthetic `TlsCertBlob` index marking a namespace-sync-distributed LAN cert.
/// It is HPKE AAD binding only (seal and open agree via the blob's own index);
/// it is **not** a real bridge enrollment and is never stored in the
/// `tls_cert_blobs` table.
const LAN_CERT_BRIDGE_ROLE: &str = "mda";
const LAN_CERT_BRIDGE_ID: &str = "lan-namespace-sync";

/// Ed25519 signature domain-separation prefix for the actor's authorization of
/// a LAN-cert namespace entry. The actor signs `CONTEXT || ciphertext`.
const LAN_CERT_SIG_CONTEXT: &[u8] = b"fauna.lan-tls-cert.actor-sig.v1\0";

const SIGNATURE_LEN: usize = 64;

fn sig_message(ciphertext: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(LAN_CERT_SIG_CONTEXT.len() + ciphertext.len());
    m.extend_from_slice(LAN_CERT_SIG_CONTEXT);
    m.extend_from_slice(ciphertext);
    m
}

/// Failure modes opening a LAN-cert namespace entry. All are "skip this entry,
/// keep serving the current cert" on the consumer — never fatal.
#[derive(Debug)]
pub enum LanCertError {
    /// `actor_sig` is malformed or does not verify against the actor's key —
    /// a substituted/forged entry; reject without touching disk.
    BadSignature,
    /// The sealed `TlsCertBlob` could not be decoded from the ciphertext.
    Decode(UnwrapError),
    /// HPKE-open failed (wrong nest key, tampered ciphertext, or AAD mismatch).
    Unseal(UnwrapError),
}

impl std::fmt::Display for LanCertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LanCertError::BadSignature => write!(f, "actor signature did not verify"),
            LanCertError::Decode(e) => write!(f, "decode lan cert blob: {e}"),
            LanCertError::Unseal(e) => write!(f, "unseal lan cert: {e}"),
        }
    }
}

impl std::error::Error for LanCertError {}

/// What the consumer recovers from a verified + unsealed LAN-cert entry.
pub struct OpenedLanCert {
    /// The cert chain + private-key PEM bundle (the private key zeroizes on
    /// drop). Written plaintext to the private nest's `acme_dir`.
    pub bundle: TlsCertBundle,
    /// The hostname the cert was issued for (the sealed blob's index domain),
    /// passed through to `store_acme_material`.
    pub domain: String,
}

/// **Producer.** Seal `bundle` (issued for `domain`) to the private nest's
/// identity x25519 pubkey and sign the resulting ciphertext with the actor's
/// Ed25519 key. Returns `(ciphertext, actor_sig)` for a `namespace_entries` row
/// keyed by [`LAN_TLS_CERT_ENTRY_ID`] in the actor's self-namespace.
///
/// # Errors
/// Returns [`WrapError`] if HPKE seal or CBOR encoding fails.
pub fn seal_lan_tls_cert_entry(
    bundle: &TlsCertBundle,
    domain: &str,
    nest_x25519_pubkey: &[u8; 32],
    actor_signing_key: &SigningKey,
) -> Result<(Vec<u8>, Vec<u8>), WrapError> {
    let blob = seal_tls_cert(
        bundle,
        LAN_CERT_BRIDGE_ROLE,
        LAN_CERT_BRIDGE_ID,
        domain,
        nest_x25519_pubkey,
    )?;
    let ciphertext = blob.to_canonical_bytes()?;
    let sig = actor_signing_key.sign(&sig_message(&ciphertext));
    Ok((ciphertext, sig.to_bytes().to_vec()))
}

/// **Consumer.** Verify the actor signed `ciphertext`, then unseal it with the
/// private nest's identity x25519 secret. Returns the cert bundle + the domain
/// it was issued for.
///
/// # Errors
/// - [`LanCertError::BadSignature`] — `actor_sig` is the wrong length, is
///   malformed, or does not verify (a forged/substituted entry).
/// - [`LanCertError::Decode`] — the ciphertext is not a valid `TlsCertBlob`.
/// - [`LanCertError::Unseal`] — HPKE-open failed (wrong nest key / tampering).
pub fn open_lan_tls_cert_entry(
    ciphertext: &[u8],
    actor_sig: &[u8],
    actor_verifying_key: &VerifyingKey,
    nest_x25519_secret: &[u8; 32],
) -> Result<OpenedLanCert, LanCertError> {
    if actor_sig.len() != SIGNATURE_LEN {
        return Err(LanCertError::BadSignature);
    }
    let mut sig_bytes = [0u8; SIGNATURE_LEN];
    sig_bytes.copy_from_slice(actor_sig);
    let sig = Signature::from_bytes(&sig_bytes);
    actor_verifying_key
        .verify(&sig_message(ciphertext), &sig)
        .map_err(|_| LanCertError::BadSignature)?;

    let blob = TlsCertBlob::from_canonical_bytes(ciphertext).map_err(LanCertError::Decode)?;
    let domain = blob.index.2.clone();
    let bundle = unseal_tls_cert(&blob, nest_x25519_secret).map_err(LanCertError::Unseal)?;
    Ok(OpenedLanCert { bundle, domain })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapped_blob::generate_x25519_keypair;

    fn sample_bundle() -> TlsCertBundle {
        TlsCertBundle {
            cert_chain: b"-----BEGIN CERTIFICATE-----\nLANCERT\n-----END CERTIFICATE-----\n"
                .to_vec(),
            priv_key: b"-----BEGIN PRIVATE KEY-----\nLANKEY\n-----END PRIVATE KEY-----\n".to_vec(),
            issued_at: 1_715_000_000,
            expires_at: 1_715_000_000 + 90 * 24 * 3600,
        }
    }

    fn actor_key(seed_byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed_byte; 32])
    }

    #[test]
    fn round_trip_seal_sign_verify_unseal() {
        let (nest_sec, nest_pub) = generate_x25519_keypair();
        let actor = actor_key(7);
        let bundle = sample_bundle();

        let (ciphertext, sig) =
            seal_lan_tls_cert_entry(&bundle, "home.example.com", &nest_pub, &actor).unwrap();

        let opened = open_lan_tls_cert_entry(&ciphertext, &sig, &actor.verifying_key(), &nest_sec)
            .expect("verified + unsealed");
        assert_eq!(opened.domain, "home.example.com");
        assert_eq!(opened.bundle.cert_chain, bundle.cert_chain);
        assert_eq!(opened.bundle.priv_key, bundle.priv_key);
    }

    #[test]
    fn tampered_actor_sig_is_rejected_before_unseal() {
        let (nest_sec, nest_pub) = generate_x25519_keypair();
        let actor = actor_key(7);
        let (ciphertext, mut sig) =
            seal_lan_tls_cert_entry(&sample_bundle(), "home.example.com", &nest_pub, &actor)
                .unwrap();
        sig[0] ^= 0xff;
        let res = open_lan_tls_cert_entry(&ciphertext, &sig, &actor.verifying_key(), &nest_sec);
        assert!(matches!(res, Err(LanCertError::BadSignature)));
    }

    #[test]
    fn wrong_actor_key_is_rejected() {
        let (nest_sec, nest_pub) = generate_x25519_keypair();
        let actor = actor_key(7);
        let attacker = actor_key(9);
        let (ciphertext, sig) =
            seal_lan_tls_cert_entry(&sample_bundle(), "home.example.com", &nest_pub, &actor)
                .unwrap();
        // Verifying against a different actor's key must fail.
        let res = open_lan_tls_cert_entry(&ciphertext, &sig, &attacker.verifying_key(), &nest_sec);
        assert!(matches!(res, Err(LanCertError::BadSignature)));
    }

    #[test]
    fn wrong_nest_secret_fails_unseal() {
        let (_nest_sec, nest_pub) = generate_x25519_keypair();
        let (other_sec, _other_pub) = generate_x25519_keypair();
        let actor = actor_key(7);
        let (ciphertext, sig) =
            seal_lan_tls_cert_entry(&sample_bundle(), "home.example.com", &nest_pub, &actor)
                .unwrap();
        // Signature is valid (right actor) but the nest secret is wrong → unseal fails.
        let res = open_lan_tls_cert_entry(&ciphertext, &sig, &actor.verifying_key(), &other_sec);
        assert!(matches!(res, Err(LanCertError::Unseal(_))));
    }
}
