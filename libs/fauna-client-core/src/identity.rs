//! Identity key generation and derivation.

use fauna_core::identity::ActorKeypair;

/// Generate a new random Ed25519 keypair.
pub fn generate_keypair() -> ActorKeypair {
    ActorKeypair::generate()
}

/// Derive the public ActorId bytes from a keypair.
pub fn actor_id(kp: &ActorKeypair) -> [u8; 32] {
    kp.actor_id().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_keypair_produces_valid_key() {
        let kp = generate_keypair();
        let id = actor_id(&kp);
        assert_ne!(id, [0u8; 32]);
    }

    #[test]
    fn actor_id_is_deterministic() {
        let kp = ActorKeypair::from_secret([42u8; 32]);
        let id1 = actor_id(&kp);
        let id2 = actor_id(&kp);
        assert_eq!(id1, id2);
    }
}
