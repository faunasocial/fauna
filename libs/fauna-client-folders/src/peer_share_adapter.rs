//! The share leg's roster-consult adapter — `fauna_peer_share::SetMembership`
//! answered by this crate's [`FolderGroupCrypto`] seam, so the M2-membership
//! witness verifier consults the SAME group state every share/removal
//! decision already runs on (one adapter here instead of one per app shell,
//! priority #2; the seam trait keeps `fauna-peer-share` itself
//! `fauna-mls`-free).
//!
//! Gated behind the `p2p-share` feature: this crate is otherwise not part of
//! the gated plane, and the adapter must never ride into a store-safe or
//! wasm graph (`fauna-peer-share`'s dep tree is native-only).

use fauna_core::identity::ActorId;
use fauna_peer_share::SetMembership;

use crate::orchestration::FolderGroupCrypto;

/// Wraps any [`FolderGroupCrypto`] holder (production: `Arc<MlsEngine>` via
/// the `mls` feature's adapter) as the share leg's roster seam.
pub struct RosterMembership<G>(pub G);

// `Send + Sync` mirrors the trait's own supertraits (the consult is held
// across awaits by serve handlers and the pump); the production holder is
// `Arc<MlsEngine>`, which satisfies both.
impl<G: FolderGroupCrypto + Send + Sync> SetMembership for RosterMembership<G> {
    /// **Fail closed:** a roster consult that errors (engine unavailable,
    /// storage failure) answers "not a member" — an evaluator that cannot
    /// read its own group state must refuse, never guess. A set this
    /// evaluator does not hold answers `false` through the same path.
    fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
        self.0.contains_member(channel_id, actor).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::CreatedGroup;

    /// A fake whose roster answer (or failure) is scripted; every other
    /// seam method is unreachable in this test.
    struct ScriptedRoster(Result<bool, &'static str>);

    impl FolderGroupCrypto for ScriptedRoster {
        type Error = &'static str;

        fn create_group(&self, _: &[Vec<u8>]) -> Result<CreatedGroup, Self::Error> {
            unreachable!()
        }
        fn holds_group(&self, _: &[u8; 32]) -> Result<bool, Self::Error> {
            unreachable!()
        }
        fn channel_id_for_group(&self, _: &[u8]) -> [u8; 32] {
            unreachable!()
        }
        fn add_member_staged(
            &self,
            _: &[u8; 32],
            _: &[u8],
        ) -> Result<(Vec<u8>, Vec<u8>), Self::Error> {
            unreachable!()
        }
        fn contains_member(&self, _: &[u8; 32], _: &ActorId) -> Result<bool, Self::Error> {
            self.0
        }
        fn remove_member_staged(
            &self,
            _: &[u8; 32],
            _: &ActorId,
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            unreachable!()
        }
        fn merge_pending_commit(&self, _: &[u8; 32]) -> Result<(), Self::Error> {
            unreachable!()
        }
        fn has_pending_commit(&self, _: &[u8; 32]) -> Result<bool, Self::Error> {
            unreachable!()
        }
        fn pending_commit_hash(&self, _: &[u8; 32]) -> Result<Option<[u8; 32]>, Self::Error> {
            unreachable!()
        }
        fn clear_pending_commit(&self, _: &[u8; 32]) -> Result<(), Self::Error> {
            unreachable!()
        }
        fn persist_group_state(&self, _: &[u8; 32]) -> Result<(), Self::Error> {
            unreachable!()
        }
        fn seal_envelope(
            &self,
            _: &[u8; 32],
            _: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
        ) -> Result<(Vec<u8>, u64), Self::Error> {
            unreachable!()
        }
        fn envelope_epoch(&self, _: &[u8; 32]) -> Result<u64, Self::Error> {
            unreachable!()
        }
        fn open_envelope(
            &self,
            _: &[u8; 32],
            _: &[u8],
        ) -> Result<fauna_core::folder_keys::ContentKeyEnvelopePayload, Self::Error> {
            unreachable!()
        }
    }

    const SET: [u8; 32] = [0x4F; 32];
    const ACTOR: ActorId = ActorId([0xB2; 32]);

    #[test]
    fn a_roster_yes_is_membership() {
        assert!(RosterMembership(ScriptedRoster(Ok(true))).is_member(&SET, &ACTOR));
    }

    #[test]
    fn a_roster_no_refuses() {
        assert!(!RosterMembership(ScriptedRoster(Ok(false))).is_member(&SET, &ACTOR));
    }

    #[test]
    fn a_failed_consult_fails_closed() {
        assert!(
            !RosterMembership(ScriptedRoster(Err("engine unavailable"))).is_member(&SET, &ACTOR)
        );
    }
}
