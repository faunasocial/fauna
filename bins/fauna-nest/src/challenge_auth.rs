//! Ed25519 challenge-response authentication — the nonce store.
//!
//! Two-step silent sign-in: the client requests a nonce, signs the
//! domain-tagged `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id` (built by the single
//! source `fauna_protocol::auth::challenge_verify_signed_message`, tagged-only
//! since 2026-08-17) with its Ed25519 secret, and submits the signature; the
//! nest verifies it, looks up the actor's registration, and issues a bearer
//! token. Unregistered actors get `fauna.auth.not_registered`, which drops the
//! client into the `nest_register` / `nest_claim` onboarding stage.
//!
//! The ceremony rides the pre-identity WS-RPC kinds `fauna.auth.challenge` /
//! `fauna.auth.verify` (`auth_handlers`, sharing the transport-agnostic
//! `auth_core::{issue_challenge_core, verify_core}` ceremony). The old
//! `POST /api/v1/auth/{challenge,verify}` HTTP twins were deleted in the
//! WS-RPC-everywhere rip-out once every consumer migrated; this module now
//! holds only the in-memory `ChallengeStore` the WS-RPC handlers depend on.

use std::collections::HashMap;

use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// How long a challenge nonce remains valid.
const CHALLENGE_TTL_SECS: u64 = 300;

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

struct ChallengeEntry {
    actor_id: [u8; 32],
    expires_at: u64,
}

#[derive(Default)]
pub struct ChallengeStore {
    /// nonce -> the challenge it was issued for (owning actor + expiry).
    ///
    /// Keyed by the random nonce, **not** the actor: two clients of one actor
    /// (the same identity launching in two tabs / on two devices) that request
    /// a challenge concurrently each get their own entry, so neither clobbers
    /// the other's outstanding nonce. The nonce is 256 random bits, so a
    /// cross-actor key collision is not a practical concern; `consume`
    /// re-checks the owning actor regardless.
    entries: RwLock<HashMap<[u8; 32], ChallengeEntry>>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh nonce for `actor_id`. Returns `(nonce, expires_at)`.
    ///
    /// Concurrent issues for the same actor coexist (the store keys by the
    /// random nonce), each valid until consumed or TTL-expired.
    pub async fn issue(&self, actor_id: [u8; 32]) -> ([u8; 32], u64) {
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).expect("getrandom failed");
        let expires_at = now_secs() + CHALLENGE_TTL_SECS;
        let mut map = self.entries.write().await;
        map.insert(
            nonce,
            ChallengeEntry {
                actor_id,
                expires_at,
            },
        );
        (nonce, expires_at)
    }

    /// Consume the `expected` nonce, removing it. Returns `true` only if it is
    /// still valid (unexpired) and was issued to `actor_id`.
    ///
    /// A mismatched actor or an unknown/expired nonce returns `false` **without
    /// burning** a valid entry — only a full match removes the nonce, keeping
    /// it single-use. (In production `consume` is only reached after `verify_core`
    /// has checked the Ed25519 signature over the tagged
    /// `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id`, so the actor re-check is
    /// defense-in-depth.)
    pub async fn consume(&self, actor_id: &[u8; 32], expected: &[u8; 32]) -> bool {
        let mut map = self.entries.write().await;
        let valid = matches!(
            map.get(expected),
            Some(e) if e.expires_at > now_secs() && &e.actor_id == actor_id
        );
        if valid {
            map.remove(expected);
        }
        valid
    }

    /// Drop nonces whose TTL expired more than 60 seconds ago.
    pub async fn gc(&self) -> usize {
        let cutoff = now_secs().saturating_sub(60);
        let mut map = self.entries.write().await;
        crate::ttl_gc::gc_before_cutoff(&mut map, |e| e.expires_at, cutoff)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    // verify-ok(test): this module signs with a locally generated key and
    // checks its own signature back — no wire-supplied key reaches it, so the
    // permissive trait is harmless here. Production verification goes through
    // `fauna_core::identity::verify_detached`; the walk guard
    // `state::tests::nest_has_one_ed25519_verification_shape` enforces that and
    // reads this marker.
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

    #[tokio::test]
    async fn issue_then_consume_once() {
        let store = ChallengeStore::new();
        let actor = [0xaa; 32];
        let (nonce, _) = store.issue(actor).await;
        assert!(store.consume(&actor, &nonce).await);
        // second consume fails — one-shot
        assert!(!store.consume(&actor, &nonce).await);
    }

    #[tokio::test]
    async fn consume_wrong_nonce_fails() {
        let store = ChallengeStore::new();
        let actor = [0xbb; 32];
        let (_real, _) = store.issue(actor).await;
        let fake = [0u8; 32];
        assert!(!store.consume(&actor, &fake).await);
    }

    #[tokio::test]
    async fn concurrent_issue_same_actor_both_consume() {
        // Two clients of the SAME actor each request a challenge before either
        // verifies (e.g. the same identity launching in two tabs / on two
        // devices). Both nonces must remain independently consumable — neither
        // client's challenge may clobber the other's. The store keys by the
        // random nonce, not the actor, so concurrent issues coexist.
        let store = ChallengeStore::new();
        let actor = [0xdd; 32];
        let (n1, _) = store.issue(actor).await;
        let (n2, _) = store.issue(actor).await;
        assert_ne!(n1, n2);
        assert!(
            store.consume(&actor, &n1).await,
            "first client's nonce clobbered"
        );
        assert!(
            store.consume(&actor, &n2).await,
            "second client's nonce clobbered"
        );
    }

    #[tokio::test]
    async fn consume_rejects_mismatched_actor() {
        // A nonce issued to actor A cannot be consumed by a verify claiming
        // actor B (defense-in-depth: consume re-checks the owning actor now
        // that the store keys by nonce). The mismatch must NOT burn the nonce —
        // actor A can still use it.
        let store = ChallengeStore::new();
        let actor_a = [0xa1; 32];
        let actor_b = [0xb2; 32];
        let (nonce, _) = store.issue(actor_a).await;
        assert!(!store.consume(&actor_b, &nonce).await);
        assert!(store.consume(&actor_a, &nonce).await);
    }

    #[test]
    fn signature_roundtrip() {
        let seed = [7u8; 32];
        let sk = SigningKey::from_bytes(&seed);
        let pk = sk.verifying_key().to_bytes();

        let nonce = [42u8; 32];
        let mut msg = Vec::with_capacity(64);
        msg.extend_from_slice(&pk);
        msg.extend_from_slice(&nonce);
        let sig = sk.sign(&msg);

        let vk = VerifyingKey::from_bytes(&pk).unwrap();
        assert!(vk.verify(&msg, &sig).is_ok());
    }

    #[tokio::test]
    async fn gc_respects_the_sixty_second_grace_period() {
        // No prior test called gc() directly — insert entries with a
        // controlled expires_at (rather than waiting out the real 300s TTL)
        // so this exercises the ttl_gc::gc_before_cutoff delegation, and its
        // grace-period cutoff specifically, without a wall-clock wait.
        let store = ChallengeStore::new();
        let actor = [0xee; 32];
        {
            let mut map = store.entries.write().await;
            map.insert(
                [1u8; 32],
                ChallengeEntry {
                    actor_id: actor,
                    // Expired 120s ago — past the 60s grace window.
                    expires_at: now_secs().saturating_sub(120),
                },
            );
            map.insert(
                [2u8; 32],
                ChallengeEntry {
                    actor_id: actor,
                    // Expired 10s ago — still inside the 60s grace window.
                    expires_at: now_secs().saturating_sub(10),
                },
            );
            map.insert(
                [3u8; 32],
                ChallengeEntry {
                    actor_id: actor,
                    expires_at: now_secs() + 300,
                },
            );
        }
        assert_eq!(store.gc().await, 1);
        let map = store.entries.read().await;
        assert_eq!(map.len(), 2);
        assert!(!map.contains_key(&[1u8; 32]));
    }
}
