//! Nest identity keypair — Ed25519.
//!
//! **The nest has ONE identity (decision 2026-06-29, `box-recovery.md`
//! § Single-identity unification).** `NestIdentity` is a *view* over the
//! **deployment signing key** (the `nest_keypair` DB row / `nest_deployment.key`
//! file, surfaced as `AppState.nest_signing_key`) — the channel-binding
//! `nest_actor_id` a client TOFU-pins. Both boot paths build it via
//! [`NestIdentity::from_seed`] from the reconciled deployment seed, so
//! `fauna.nest.info`, federation, the backup-destination pubkey, multi-nest
//! pairing, and the nest's own sync `device_id`/`service_id` all key off the
//! *single* custodied deployment seed → one off-box-custodied seed restores the
//! whole nest identity after total box loss. The legacy *separate* random
//! `nest_identity.key` file is retired.

use ed25519_dalek::{SigningKey, VerifyingKey};

/// The nest's own Ed25519 identity — a view over the deployment signing key.
pub struct NestIdentity {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl NestIdentity {
    /// Build the nest identity from a raw 32-byte Ed25519 seed — the nest's
    /// single identity is a view over the **deployment signing key**, so callers
    /// pass the reconciled deployment seed (`AppState.nest_signing_key` bytes).
    /// `box-recovery.md` § Single-identity unification.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(seed);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    /// Generate a fresh random identity. **Defensive fallback only** — used when
    /// the deployment keypair is unexpectedly absent (a boot reconcile failed,
    /// which should not happen) so the nest still has *some* identity rather than
    /// panicking, and as a test convenience. The production identity always
    /// derives from the reconciled deployment seed via [`Self::from_seed`].
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("getrandom failed");
        Self::from_seed(&seed)
    }

    /// The nest's public key as raw bytes.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        *self.verifying_key.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single-identity unification invariant: `NestIdentity::from_seed(seed)`
    /// must derive the **same** public key the deployment seed maps to
    /// (`ed25519(seed).public`), i.e. the channel-binding `nest_actor_id`. This is
    /// what makes `fauna.nest.info`'s `nest_id` equal the deployment identity a
    /// client TOFU-pins (`box-recovery.md` § Single-identity unification).
    #[test]
    fn from_seed_derives_deployment_pubkey() {
        let seed = [0x5Au8; 32];
        let id = NestIdentity::from_seed(&seed);
        let expected = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        assert_eq!(
            id.public_key_bytes(),
            expected,
            "from_seed must derive the deployment seed's public key (the pinned nest_actor_id)"
        );
    }

    /// Deterministic: the same seed always yields the same identity (so a
    /// recovered box re-presents the same `nest_actor_id`).
    #[test]
    fn from_seed_is_deterministic() {
        let seed = [9u8; 32];
        assert_eq!(
            NestIdentity::from_seed(&seed).public_key_bytes(),
            NestIdentity::from_seed(&seed).public_key_bytes(),
        );
    }

    #[test]
    fn public_key_is_32_bytes() {
        let id = NestIdentity::from_seed(&[1u8; 32]);
        assert_eq!(id.public_key_bytes().len(), 32);
    }

    /// The defensive fallback yields a usable, distinct random identity.
    #[test]
    fn generate_produces_random_identity() {
        let a = NestIdentity::generate();
        let b = NestIdentity::generate();
        assert_eq!(a.public_key_bytes().len(), 32);
        assert_ne!(
            a.public_key_bytes(),
            b.public_key_bytes(),
            "two random fallbacks must differ"
        );
    }
}
