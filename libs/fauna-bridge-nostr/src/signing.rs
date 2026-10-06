use secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};
use sha2::{Digest, Sha256};

use crate::serialize::compute_event_id;
use crate::types::{Event, UnsignedEvent};

/// A secp256k1 keypair for Nostr signing (BIP-340 schnorr).
pub struct Keypair {
    secret: SecretKey,
    x_only: XOnlyPublicKey,
}

impl Keypair {
    /// Generate a new random keypair.
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (secret, public) = secp.generate_keypair(&mut rand::thread_rng());
        let (x_only, _parity) = public.x_only_public_key();
        Self { secret, x_only }
    }

    /// Create a keypair from raw 32-byte secret.
    pub fn from_secret_bytes(bytes: [u8; 32]) -> anyhow::Result<Self> {
        let secret = SecretKey::from_slice(&bytes)?;
        let secp = Secp256k1::new();
        let public = secret.public_key(&secp);
        let (x_only, _parity) = public.x_only_public_key();
        Ok(Self { secret, x_only })
    }

    /// Returns the 32-byte x-only public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.x_only.serialize()
    }

    /// Returns the 32-byte secret key.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.secret_bytes()
    }

    /// Returns the x-only public key hex string.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key_bytes())
    }

    /// Sign a message (raw bytes) with schnorr. Hashes the message with SHA-256 first.
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        let secp = Secp256k1::new();
        let hash = Sha256::digest(msg);
        let kp = secp256k1::Keypair::from_secret_key(&secp, &self.secret);
        let sig = secp.sign_schnorr(&hash, &kp);
        sig.to_byte_array()
    }

    /// Verify a schnorr signature on a message.
    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        let secp = Secp256k1::new();
        let hash = Sha256::digest(msg);
        let signature = match secp256k1::schnorr::Signature::from_slice(sig) {
            Ok(s) => s,
            Err(_) => return false,
        };
        secp.verify_schnorr(&signature, &hash, &self.x_only).is_ok()
    }

    /// Sign an unsigned event, returning a fully signed Event.
    pub fn sign_event(&self, unsigned: UnsignedEvent) -> Event {
        let id_bytes = compute_event_id(&unsigned);
        let id_hex = hex::encode(id_bytes);

        let secp = Secp256k1::new();
        let kp = secp256k1::Keypair::from_secret_key(&secp, &self.secret);
        let sig = secp.sign_schnorr(&id_bytes, &kp);

        Event {
            id: id_hex,
            pubkey: self.public_key_hex(),
            created_at: unsigned.created_at,
            kind: unsigned.kind,
            tags: unsigned.tags,
            content: unsigned.content,
            sig: hex::encode(sig.to_byte_array()),
        }
    }
}

/// Verify a signed event: check that the event ID matches the canonical
/// serialization and that the schnorr signature is valid.
pub fn verify_event(event: &Event) -> bool {
    let pubkey_bytes = match hex::decode(&event.pubkey) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return false,
    };

    let unsigned = UnsignedEvent {
        pubkey: pubkey_bytes,
        created_at: event.created_at,
        kind: event.kind,
        tags: event.tags.clone(),
        content: event.content.clone(),
    };

    let expected_id = compute_event_id(&unsigned);
    let expected_id_hex = hex::encode(expected_id);

    if event.id != expected_id_hex {
        return false;
    }

    let secp = Secp256k1::new();
    let x_only = match XOnlyPublicKey::from_slice(&pubkey_bytes) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let sig_bytes = match hex::decode(&event.sig) {
        Ok(b) if b.len() == 64 => b,
        _ => return false,
    };
    let signature = match secp256k1::schnorr::Signature::from_slice(&sig_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };

    secp.verify_schnorr(&signature, &expected_id, &x_only)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let keypair = Keypair::generate();
        let message = b"test message";
        let sig = keypair.sign(message);
        assert!(keypair.verify(message, &sig));
    }

    #[test]
    fn wrong_key_rejects() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let sig = kp1.sign(b"test");
        assert!(!kp2.verify(b"test", &sig));
    }

    #[test]
    fn from_secret_bytes_roundtrip() {
        let kp = Keypair::generate();
        let secret = kp.secret_bytes();
        let kp2 = Keypair::from_secret_bytes(secret).unwrap();
        assert_eq!(kp.public_key_bytes(), kp2.public_key_bytes());
    }

    #[test]
    fn sign_event_and_verify() {
        let kp = Keypair::generate();
        let unsigned = UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at: 1234567890,
            kind: 1,
            tags: vec![],
            content: "hello nostr".to_string(),
        };
        let event = kp.sign_event(unsigned);
        assert!(verify_event(&event));
    }

    #[test]
    fn tampered_event_fails_verification() {
        let kp = Keypair::generate();
        let unsigned = UnsignedEvent {
            pubkey: kp.public_key_bytes(),
            created_at: 1234567890,
            kind: 1,
            tags: vec![],
            content: "hello".to_string(),
        };
        let mut event = kp.sign_event(unsigned);
        event.content = "tampered".to_string();
        assert!(!verify_event(&event));
    }
}
