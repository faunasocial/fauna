//! Synthetic identity derivation and RSA keypair management.

use anyhow::Result;
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use rsa::signature::SignatureEncoding;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::Sha256;

use fauna_core::identity::ActorId;

const SYNTHETIC_DOMAIN: &str = "fauna-activitypub-synthetic";

/// Derive a deterministic Fauna ActorId from an AP actor URI.
/// Uses BLAKE3 keyed hash with a domain-separation constant.
pub fn synthetic_actor_id(actor_uri: &str) -> ActorId {
    let hash = blake3::derive_key(SYNTHETIC_DOMAIN, actor_uri.as_bytes());
    ActorId(hash)
}

/// Generate an RSA-2048 keypair for HTTP Signatures.
/// Returns (private_key_der, public_key_pem).
pub fn generate_rsa_keypair() -> Result<(Vec<u8>, String)> {
    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048)?;
    let privkey_der = private_key.to_pkcs1_der()?.as_bytes().to_vec();
    let pubkey_pem = private_key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)?;
    Ok((privkey_der, pubkey_pem))
}

/// Sign data with an RSA private key (PKCS#1 v1.5, SHA-256).
pub fn rsa_sign(privkey_der: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let private_key = RsaPrivateKey::from_pkcs1_der(privkey_der)?;
    let signing_key = SigningKey::<Sha256>::new(private_key);
    let signature = rsa::signature::Signer::sign(&signing_key, data);
    Ok(signature.to_bytes().into_vec())
}

/// Verify an RSA signature (PKCS#1 v1.5, SHA-256) against a PEM public key.
pub fn rsa_verify(pubkey_pem: &str, data: &[u8], signature: &[u8]) -> Result<bool> {
    use rsa::pkcs1v15::Signature;
    use rsa::pkcs1v15::VerifyingKey;
    use rsa::pkcs8::DecodePublicKey;
    let public_key = RsaPublicKey::from_public_key_pem(pubkey_pem)?;
    let verifying_key = VerifyingKey::<Sha256>::new(public_key);
    let sig = Signature::try_from(signature)?;
    Ok(rsa::signature::Verifier::verify(&verifying_key, data, &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_actor_id_is_deterministic() {
        let id1 = synthetic_actor_id("https://mastodon.social/users/alice");
        let id2 = synthetic_actor_id("https://mastodon.social/users/alice");
        assert_eq!(id1, id2);
    }

    #[test]
    fn synthetic_actor_id_differs_for_different_uris() {
        let id1 = synthetic_actor_id("https://mastodon.social/users/alice");
        let id2 = synthetic_actor_id("https://mastodon.social/users/bob");
        assert_ne!(id1, id2);
    }

    #[test]
    fn rsa_keypair_generation() {
        let (privkey_der, pubkey_pem) = generate_rsa_keypair().unwrap();
        assert!(!privkey_der.is_empty());
        assert!(pubkey_pem.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert!(pubkey_pem.ends_with("-----END PUBLIC KEY-----\n"));
    }

    #[test]
    fn rsa_keypair_sign_verify_roundtrip() {
        let (privkey_der, pubkey_pem) = generate_rsa_keypair().unwrap();
        let data = b"test signing string";
        let signature = rsa_sign(&privkey_der, data).unwrap();
        assert!(!signature.is_empty());
        let valid = rsa_verify(&pubkey_pem, data, &signature).unwrap();
        assert!(valid);
    }
}
