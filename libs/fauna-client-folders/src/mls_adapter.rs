//! The real MLS-engine-backed [`FolderGroupCrypto`] adapter — the only contents
//! of the optional `mls` feature.
//!
//! `impl FolderGroupCrypto for Arc<MlsEngine>`: the owner-side group-crypto seam
//! the [`FoldersAuthor`](crate::orchestration::FoldersAuthor) drives, backed by
//! the live MLS engine that holds the folder's group. Because the
//! [`FolderGroupCrypto`] trait is **local to this crate**, the orphan rule permits
//! the impl right here on the foreign `Arc<MlsEngine>` — so there is exactly **one**
//! shared, natively-tested adapter rather than a newtype copied into both `fauna-ffi`
//! and `fauna-wasm` (priority #2/#4). The base crate stays `fauna-mls`-free
//! (wasm-light) for the thin-client / pure-custody consumers; `fauna-ffi` +
//! `fauna-wasm` (which already carry `fauna-mls`) flip on `mls` to wire
//! `FoldersAuthor`.
//!
//! ## The `channel_id` is the DERIVED ChannelId — do NOT re-derive
//!
//! Every trait method takes `channel_id: &[u8; 32]` already equal to the engine's
//! group key — the derived `ChannelId::from_group_id(group_id)` value that custody
//! stores and `FoldersAuthor` threads through (see [`crate::custody`] and
//! `mls-group-key-material.md` § M2). So the adapter wraps it **directly** as
//! [`ChannelId`]`(*channel_id)`. Calling `ChannelId::from_group_id` here would
//! derive a *second* time and look up a group that does not exist (→ every op
//! `ChannelNotFound`).
//!
//! ## Ordering lives in the orchestration, not here
//!
//! The forward-secrecy ordering (Remove → seal under the new epoch)
//! is enforced by [`FoldersAuthor::remove_member`](crate::orchestration::FoldersAuthor::remove_member),
//! which calls [`Self::remove_member`] *before* [`Self::seal_envelope`]. This adapter
//! only forwards each op to the engine; [`Self::seal_envelope`] always seals under
//! the group's *current* epoch, so sealing after a Remove naturally seals under the
//! post-removal one.

use std::sync::Arc;

use fauna_core::identity::ActorId;
use fauna_mls::engine::MlsEngine;
use fauna_mls::error::MlsError;
use fauna_mls::types::ChannelId;

use crate::orchestration::{
    CreatedGroup, FolderCommitGate, FolderGroupCrypto, GatedAdd, GatedRemoval,
};

impl FolderGroupCrypto for Arc<MlsEngine> {
    type Error = MlsError;

    fn create_group(&self, member_key_packages: &[Vec<u8>]) -> Result<CreatedGroup, Self::Error> {
        // Deserialize + validate each member KeyPackage through the engine's crypto
        // provider (where the validation lives). `key_package_from_bytes` is an
        // inherent method (no `FolderGroupCrypto` shadow) so plain deref reaches it.
        let kps = member_key_packages
            .iter()
            .map(|b| self.key_package_from_bytes(b))
            .collect::<Result<Vec<_>, _>>()?;
        // UFCS to the *inherent* `MlsEngine::create_group` — `self.create_group(..)`
        // would re-resolve to THIS trait method (infinite recursion). It returns the
        // DERIVED `ChannelId` (= `from_group_id(raw)`); `group_id_bytes` recovers the
        // raw openMLS id the nest re-derives the identical ChannelId from.
        let (channel_id, welcome) = MlsEngine::create_group(self, &kps)?;
        // Arm the owner-managed-roster commit policy on the owner's own seat:
        // the minting actor IS the folder owner (and the nest-side channel
        // claimant — `share_set_first_binder` binds this same group next), so
        // the owner's engine refuses any other member's proposal-carrying
        // commit while merging their legitimate self-`Update` takeovers
        // (`federation.md` § Cross-nest shared folders + channel append). The
        // member seats stamp the same marker at `join_folder_welcome`.
        MlsEngine::mark_folder_channel_owner(self, &channel_id, &self.identity_actor_id());
        let raw_group_id = self
            .group_id_bytes(&channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(hex::encode(channel_id.0)))?;
        let welcome = welcome
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("welcome serialize: {e:?}")))?;
        Ok(CreatedGroup {
            channel_id: channel_id.0,
            raw_group_id,
            welcome,
        })
    }

    fn holds_group(&self, channel_id: &[u8; 32]) -> Result<bool, Self::Error> {
        // The engine keeps a raw group id for exactly the groups whose provider
        // state it holds; a set the nest reports bound but this engine has no
        // group for (a fresh device pre-restore) returns `None` here.
        Ok(self.group_id_bytes(&ChannelId(*channel_id)).is_some())
    }

    fn channel_id_for_group(&self, raw_group_id: &[u8]) -> [u8; 32] {
        // The same BLAKE3 fold the nest applies server-side (`from_group_id`).
        ChannelId::from_group_id(raw_group_id).0
    }

    fn add_member_staged(
        &self,
        channel_id: &[u8; 32],
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), Self::Error> {
        // UFCS to the *inherent* engine method — the trait impl on `Arc<MlsEngine>`
        // shadows the deref-reachable inherent (see the module doc's recursion
        // warning). The inherent twin deserializes + validates the KeyPackage,
        // stages the Add commit WITHOUT merging it, and returns the Welcome as
        // bytes; the orchestration merges only once the commit is distributed.
        MlsEngine::add_member_staged_from_bytes(self, &ChannelId(*channel_id), key_package_bytes)
    }

    fn contains_member(
        &self,
        channel_id: &[u8; 32],
        member: &ActorId,
    ) -> Result<bool, Self::Error> {
        Ok(self
            .find_leaf_by_identity(&ChannelId(*channel_id), member)
            .is_some())
    }

    fn remove_member_staged(
        &self,
        channel_id: &[u8; 32],
        member: &ActorId,
    ) -> Result<Option<Vec<u8>>, Self::Error> {
        let channel = ChannelId(*channel_id);
        match self.find_leaf_by_identity(&channel, member) {
            // UFCS to reach the *inherent* engine method — `self.remove_member_staged(..)`
            // would re-resolve to THIS trait method (infinite recursion), since the
            // trait impl on `Arc<MlsEngine>` shadows the deref-reachable inherent.
            // The inherent twin builds the commit WITHOUT merging it; the
            // orchestration merges only once the bytes are durable.
            Some(leaf) => MlsEngine::remove_member_staged(self, &channel, leaf).map(Some),
            // Idempotent: an already-absent member (a resumed/no-op removal)
            // produces no new commit — the `FolderGroupCrypto::remove_member_staged`
            // `None` contract the orchestration relies on. Also covers a channel this
            // client holds no group for (`find_leaf_by_identity` → `None`).
            None => Ok(None),
        }
    }

    fn merge_pending_commit(&self, channel_id: &[u8; 32]) -> Result<(), Self::Error> {
        MlsEngine::merge_pending_commit(self, &ChannelId(*channel_id))
    }

    fn has_pending_commit(&self, channel_id: &[u8; 32]) -> Result<bool, Self::Error> {
        Ok(MlsEngine::has_pending_commit(self, &ChannelId(*channel_id)))
    }

    fn pending_commit_hash(&self, channel_id: &[u8; 32]) -> Result<Option<[u8; 32]>, Self::Error> {
        Ok(MlsEngine::pending_commit_hash(
            self,
            &ChannelId(*channel_id),
        ))
    }

    fn persist_group_state(&self, channel_id: &[u8; 32]) -> Result<(), Self::Error> {
        // The provider snapshot is engine-wide (one KV for every group); the
        // channel identifies the group being persisted only for observability.
        let _ = channel_id;
        #[cfg(not(target_arch = "wasm32"))]
        {
            MlsEngine::save_state(self)
        }
        #[cfg(target_arch = "wasm32")]
        {
            // A wasm engine has no local store — its only persistence is the
            // nest replica, whose crash-safety the GATED removal route provides
            // (`send_commit_gated` CAS-puts the provider around the send). On
            // the ungated wasm fallback this no-op means a reload can restore a
            // pre-stage engine while the sentinel already carries bytes; the
            // resume then takes the conservative loud-error arm (never a fork
            // or a pre-removal-epoch seal) until the replica autosave catches
            // up or the gated plane comes back.
            Ok(())
        }
    }

    fn clear_pending_commit(&self, channel_id: &[u8; 32]) -> Result<(), Self::Error> {
        let channel = ChannelId(*channel_id);
        // A group this client doesn't hold has nothing pending. The engine's
        // inherent `clear_pending_commit` would raise `ChannelNotFound`, which would
        // regress the resumed-drive-on-a-forgotten-group path that the old
        // `remove_member` tolerated via `find_leaf_by_identity` → `Ok(None)`.
        if self.group_id_bytes(&channel).is_none() {
            return Ok(());
        }
        MlsEngine::clear_pending_commit(self, &channel)
    }

    fn seal_envelope(
        &self,
        channel_id: &[u8; 32],
        payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    ) -> Result<(Vec<u8>, u64), Self::Error> {
        let sealed = self.seal_content_key_envelope_payload(&ChannelId(*channel_id), payload)?;
        Ok((sealed.sealed, sealed.epoch))
    }

    fn envelope_epoch(&self, channel_id: &[u8; 32]) -> Result<u64, Self::Error> {
        MlsEngine::current_epoch(self, &ChannelId(*channel_id))
    }

    fn open_envelope(
        &self,
        channel_id: &[u8; 32],
        sealed: &[u8],
    ) -> Result<fauna_core::folder_keys::ContentKeyEnvelopePayload, Self::Error> {
        self.open_content_key_envelope_payload(&ChannelId(*channel_id), sealed)
    }
}

/// The share leg's M2 admission consult over the live engine — the
/// [`fauna_peer_share::admission::SetMembership`] seam: the channel-proven
/// actor key checked against the set's MLS roster, which is the evaluator's
/// **own** store (offline-available), never a registry lookup (`p2p-shared-set-build.md`
/// § Build contract, the M2-witness bullet). A channel this engine holds no
/// group for answers `false` — fail closed, exactly
/// [`FolderGroupCrypto::contains_member`]'s reading. A newtype rather than a
/// direct impl because trait and type are both foreign here (orphan rule);
/// the leaf consult is shared with `contains_member` by construction — both
/// are `find_leaf_by_identity`.
#[cfg(feature = "p2p-share")]
pub struct MlsSetMembership(pub Arc<MlsEngine>);

#[cfg(feature = "p2p-share")]
impl fauna_peer_share::admission::SetMembership for MlsSetMembership {
    fn is_member(&self, channel_id: &[u8; 32], member: &fauna_core::identity::ActorId) -> bool {
        self.0
            .find_leaf_by_identity(&ChannelId(*channel_id), member)
            .is_some()
    }
}

/// The production [`FolderCommitGate`]: routes a folder member removal
/// through the session backend's injected `CommitGate` — the device-owned-epoch
/// commit rebase loop — so the Remove commit obeys Rule 1 exactly like a chat
/// rail membership commit (`devices.md` § Cross-device MLS group-state sync;
/// the goal doc's Implementation-status item 2). Implemented here (this crate
/// owns the trait; `Arc<FaunaMlsBackend>` is foreign) for the same orphan-rule
/// reason as the `FolderGroupCrypto` adapter above.
///
/// ## The entry protocol (why this is more than one gate call)
///
/// A crash can leave a restored **staged pending** in the engine (the provider
/// replica carries pendings by design — `send_commit_gated` step 2). Whether
/// that pending's commit ever reached the channel log decides everything:
///
/// - **It was sent** (crash between accept and merge): the ONLY safe heal is
///   the shared own-leaf resync arm merging the replica's identity-stamped
///   pending. Building a fresh commit would fork; even clearing the local
///   pending is unsafe, because the next gate round would CAS-put a *different*
///   pending over the replica — destroying the resync's heal source and
///   livelocking the identity check.
/// - **It was never sent** (crash between stage/CAS-put and send): no resync
///   will ever fire (the commit is on no log), so the pending must be cleared
///   or every future commit on the channel is bricked.
///
/// The log — walked to head — distinguishes them: after a clean (unstalled)
/// walk, any commit that WAS sent has either been incorporated (own-leaf
/// resync merged it → no pending remains) or reported a stall. So: walk to
/// head; a stall aborts ([`GatedRemoval::PendingUnconverged`] — retry after
/// the background poll converges); a pending that survives a clean walk is
/// provably unsent and safely cleared; then the member's leaf is resolved
/// fresh and the commit rides the rebase loop.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl FolderCommitGate for Arc<fauna_conversations::backends::fauna_mls::FaunaMlsBackend> {
    fn engaged(&self) -> bool {
        // The backend's `CommitGate` slot is a latched `OnceLock` — injected by
        // `restore_and_wire` when the multi-device plane comes up, never unset
        // (ratified the latch) — so a `true` here cannot revert
        // before the `gated_remove` call it guards.
        self.commit_gate().is_some()
    }

    async fn gated_remove(
        &self,
        channel_id: &[u8; 32],
        member: &ActorId,
    ) -> Result<GatedRemoval, String> {
        let Some(gate) = self.commit_gate() else {
            // Wired, but the plane never engaged (still restoring, or a permanent
            // load failure). NOT
            // `NoGate`: without the walk-to-head below we cannot tell whether an
            // earlier gated attempt already distributed a commit for this removal,
            // so a *resumed* sentinel must defer rather than rebuild blind. The
            // caller (`drive_removal`) owns that distinction.
            return Ok(GatedRemoval::GateNotEngaged);
        };
        let gate = std::sync::Arc::clone(gate);
        let channel = ChannelId(*channel_id);

        // Serialize against the background folder poll (the same discipline as
        // the chat rail's gated branches — the engine rejects processing an
        // inbound commit while a gated send has one staged). The rebase loop's
        // inner catch-up never re-takes this lock.
        let lock = self.channel_lock(&channel);
        let _guard = lock.lock().await;

        // Entry catch-up: walk the channel log to head (local cursor — the
        // rebase loop owns the MlsStateSync cursor). Heals a sent crash-window
        // commit via the shared own-leaf resync arm inside
        // `apply_inbound_commit`; idempotent from 0 (past-epoch quiet skip).
        let mut cur = 0i64;
        let outcome = fauna_conversations::backends::fauna_mls::poll_inbound_folder(
            self, &channel, &mut cur, 0,
        )
        .await
        .map_err(|e| format!("folder removal entry catch-up: {e}"))?;
        if outcome.stalled {
            return Ok(GatedRemoval::PendingUnconverged);
        }

        // UFCS to the *inherent* engine methods — the `FolderGroupCrypto` impl
        // on `Arc<MlsEngine>` shadows the deref-reachable inherents (see the
        // module doc's recursion warning).
        let engine = self.engine();
        if MlsEngine::has_pending_commit(&engine, &channel) {
            // Survived a clean walk to head ⇒ provably unsent (see the impl
            // doc) — clear it so the rebase loop can stage.
            MlsEngine::clear_pending_commit(&engine, &channel)
                .map_err(|e| format!("clear provably-unsent pending: {e}"))?;
        }

        let Some(leaf) = engine.find_leaf_by_identity(&channel, member) else {
            return Ok(GatedRemoval::AlreadyAbsent);
        };
        gate.gated_remove_member(channel, leaf)
            .await
            .map_err(|e| format!("gated remove: {e}"))?;
        Ok(GatedRemoval::Removed)
    }

    async fn gated_add(
        &self,
        channel_id: &[u8; 32],
        key_package_bytes: &[u8],
    ) -> Result<GatedAdd, String> {
        let Some(gate) = self.commit_gate() else {
            // Wired, but the plane never engaged. Unlike a resumed removal, a
            // fresh add has no earlier distributed attempt to reason about, so
            // the caller safely rebuilds ungated — report `GateNotEngaged` and
            // let `drive_add` take the Rule-1 fallback.
            return Ok(GatedAdd::GateNotEngaged);
        };
        let gate = std::sync::Arc::clone(gate);
        let channel = ChannelId(*channel_id);

        // Serialize against the background folder poll — the same discipline as
        // `gated_remove` above and the chat rail's gated add branch.
        let lock = self.channel_lock(&channel);
        let _guard = lock.lock().await;

        // Entry catch-up: walk to head so the Add commit rebases on the current
        // epoch (a stale-epoch Add would fork). Heals a sent crash-window commit
        // via the shared own-leaf resync arm; idempotent from 0.
        let mut cur = 0i64;
        let outcome = fauna_conversations::backends::fauna_mls::poll_inbound_folder(
            self, &channel, &mut cur, 0,
        )
        .await
        .map_err(|e| format!("folder add entry catch-up: {e}"))?;
        if outcome.stalled {
            return Ok(GatedAdd::PendingUnconverged);
        }

        // A pending that survived a clean walk to head is provably unsent (see the
        // impl doc) — clear it so the rebase loop can stage the Add.
        let engine = self.engine();
        if MlsEngine::has_pending_commit(&engine, &channel) {
            MlsEngine::clear_pending_commit(&engine, &channel)
                .map_err(|e| format!("clear provably-unsent pending: {e}"))?;
        }

        let (_seq, welcome) = gate
            .gated_add_member(channel, key_package_bytes.to_vec())
            .await
            .map_err(|e| format!("gated add: {e}"))?;
        Ok(GatedAdd::Added(welcome))
    }

    fn ungated_channel_lock(
        &self,
        channel_id: &[u8; 32],
    ) -> Option<Arc<futures_util::lock::Mutex<()>>> {
        // The same per-channel lock the background folder poll and the gated
        // branches serialize on — holding it across the ungated stage→merge→send
        // section keeps a foreign inbound merge from dropping the staged pending.
        // `gated_remove` above released its hold
        // before the ungated leg runs, so this never self-deadlocks.
        Some(self.channel_lock(&ChannelId(*channel_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::folder_keys::{ContentKeyEnvelopePayload, FolderContentKeys};
    use fauna_core::identity::ActorKeypair;

    /// The payload a seal carries: `keys` and `set_nonce`, no lineage.
    fn payload(keys: &FolderContentKeys, set_nonce: Option<[u8; 32]>) -> ContentKeyEnvelopePayload {
        ContentKeyEnvelopePayload {
            keys: keys.clone(),
            set_nonce,
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        }
    }

    /// A 2-member group at a shared epoch: alice (owner, returned as the adapter
    /// `Arc<MlsEngine>`) + bob (a separate engine that joined via the Welcome).
    /// Returns the derived channel-id bytes (what custody/orchestration pass) and
    /// bob's `ActorId`.
    fn two_member_group() -> (Arc<MlsEngine>, MlsEngine, [u8; 32], ActorId) {
        let alice = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (cid, welcome) = alice.create_group(&bob_kps).unwrap();
        let bob_cid = bob.join_from_welcome(welcome).unwrap();
        assert_eq!(cid, bob_cid);
        let bob_actor = bob.identity_actor_id();
        (Arc::new(alice), bob, cid.0, bob_actor)
    }

    #[test]
    fn create_group_admits_member_and_returns_joinable_welcome() {
        // The adapter's `FolderGroupCrypto::create_group` over a member's serialized
        // key-package bytes — the share flow's group-create step (5d(b-pre)). Proves
        // the derived channel id is `from_group_id(raw)` (what the nest re-derives) and
        // the returned Welcome lets the member join the same channel.
        let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob_kp_bytes = bob.generate_key_packages_bytes(1).unwrap();

        let created = FolderGroupCrypto::create_group(&alice, &bob_kp_bytes).unwrap();

        assert_eq!(
            created.channel_id,
            ChannelId::from_group_id(&created.raw_group_id).0,
            "derived channel id == from_group_id(raw) — the value the nest re-derives"
        );
        let bob_actor = bob.identity_actor_id();
        assert!(
            alice
                .contains_member(&created.channel_id, &bob_actor)
                .unwrap(),
            "create_group admitted the member to the new group"
        );
        let joined = bob.join_from_welcome_bytes(&created.welcome).unwrap();
        assert_eq!(
            joined.0, created.channel_id,
            "the member joins the same channel from the returned Welcome"
        );
        assert_eq!(
            alice.folder_channel_owner(&joined),
            Some(alice.identity_actor_id()),
            "the mint stamped the owner's own seat as the folder owner — the \
             owner-managed-roster commit policy is armed from creation"
        );
    }

    #[test]
    fn contains_member_reflects_the_live_roster() {
        let (alice, _bob, ch, bob_actor) = two_member_group();
        assert!(alice.contains_member(&ch, &bob_actor).unwrap());
        // A stranger never admitted to the group.
        let stranger = ActorKeypair::generate().actor_id();
        assert!(!alice.contains_member(&ch, &stranger).unwrap());
    }

    /// The share leg's admission consult is the same leaf lookup as
    /// `contains_member`, fail-closed in every direction: member yes, stranger
    /// no, and a channel this engine holds no group for — no.
    #[cfg(feature = "p2p-share")]
    #[test]
    fn set_membership_admits_the_roster_and_nobody_else() {
        use fauna_peer_share::admission::SetMembership as _;
        let (alice, _bob, ch, bob_actor) = two_member_group();
        let consult = MlsSetMembership(Arc::clone(&alice));
        assert!(consult.is_member(&ch, &bob_actor));
        assert!(!consult.is_member(&ch, &ActorKeypair::generate().actor_id()));
        assert!(
            !consult.is_member(&[0xEE; 32], &bob_actor),
            "an unheld channel answers false, never an error"
        );
    }

    #[test]
    fn seal_envelope_seals_under_the_current_epoch_and_a_member_opens_it() {
        let (alice, bob, ch, _bob_actor) = two_member_group();
        let keys = FolderContentKeys::genesis([7u8; 32], 1_000);
        let (sealed, epoch) = alice.seal_envelope(&ch, &payload(&keys, None)).unwrap();
        assert!(
            !sealed.is_empty(),
            "the seal is real, not the plaintext bundle"
        );
        assert_eq!(
            epoch,
            alice.current_epoch(&ChannelId(ch)).unwrap(),
            "sealed under the group's current epoch"
        );
        // Bob, at the same epoch, reconstructs the exact key bundle.
        let opened = bob
            .open_content_key_envelope(&ChannelId(ch), &sealed)
            .unwrap();
        assert_eq!(
            opened, keys,
            "member reconstructs the keys (history-on-join)"
        );
    }

    #[test]
    fn a_member_opens_the_set_nonce_sealed_beside_the_generations() {
        let (alice, bob, ch, _bob_actor) = two_member_group();
        let keys = FolderContentKeys::genesis([7u8; 32], 1_000);
        let (sealed, _) = alice
            .seal_envelope(&ch, &payload(&keys, Some([0x5E; 32])))
            .unwrap();
        let payload = bob
            .open_content_key_envelope_payload(&ChannelId(ch), &sealed)
            .unwrap();
        assert_eq!(payload.keys, keys);
        assert_eq!(payload.set_nonce, Some([0x5E; 32]));
    }

    #[test]
    fn staged_remove_defers_the_epoch_advance_until_merge_and_is_idempotent() {
        // The crash-safety property `FoldersAuthor::drive_removal` rests on, proven
        // against the REAL engine: staging a Remove commit does not advance the
        // owner's epoch, so a crash before the bytes are durable loses nothing
        // recomputable. Only the merge is the point of no return.
        let (alice, _bob, ch, bob_actor) = two_member_group();
        let before = alice.current_epoch(&ChannelId(ch)).unwrap();

        let commit = alice.remove_member_staged(&ch, &bob_actor).unwrap();
        assert!(
            commit.is_some(),
            "a real removal returns the commit bytes to distribute"
        );
        assert_eq!(
            alice.current_epoch(&ChannelId(ch)).unwrap(),
            before,
            "a STAGED commit must not advance the epoch (nothing is durable yet)"
        );
        assert!(
            alice.contains_member(&ch, &bob_actor).unwrap(),
            "the member is still on the roster until the merge"
        );

        alice.merge_pending_commit(&ch).unwrap();
        let after = alice.current_epoch(&ChannelId(ch)).unwrap();
        assert!(
            after > before,
            "the merged Remove commit advanced the epoch"
        );
        assert!(
            !alice.contains_member(&ch, &bob_actor).unwrap(),
            "the member is gone from the roster"
        );

        // A second remove of the now-absent member is a no-op (`None`) — the
        // resumed-removal idempotency `FoldersAuthor` depends on.
        assert!(
            alice
                .remove_member_staged(&ch, &bob_actor)
                .unwrap()
                .is_none(),
            "removing an absent member produces no new commit"
        );
    }

    #[test]
    fn clearing_a_staged_commit_lets_the_drive_rebuild_it() {
        // The resumed-drive path: an interrupted drive left a pending commit whose
        // bytes never reached durable storage. `clear_pending_commit` must return
        // the group to its pre-commit state so a fresh commit can be built (openmls
        // refuses to build a second commit over an unmerged one).
        let (alice, _bob, ch, bob_actor) = two_member_group();
        let before = alice.current_epoch(&ChannelId(ch)).unwrap();

        let first = alice.remove_member_staged(&ch, &bob_actor).unwrap();
        assert!(first.is_some());
        alice.clear_pending_commit(&ch).unwrap();
        assert_eq!(alice.current_epoch(&ChannelId(ch)).unwrap(), before);

        // Rebuilding now succeeds — the pending commit is gone.
        let second = alice.remove_member_staged(&ch, &bob_actor).unwrap();
        assert!(second.is_some(), "the drive can rebuild after a clear");
        alice.merge_pending_commit(&ch).unwrap();
        assert!(alice.current_epoch(&ChannelId(ch)).unwrap() > before);
        assert!(!alice.contains_member(&ch, &bob_actor).unwrap());
    }

    #[test]
    fn clear_pending_commit_is_a_noop_for_an_unknown_channel() {
        // A resumed drive on a group this client no longer holds must not error —
        // the old `remove_member` tolerated it via `find_leaf_by_identity` → `None`.
        let (alice, _bob, _ch, _bob_actor) = two_member_group();
        alice
            .clear_pending_commit(&[0x99; 32])
            .expect("unknown channel has nothing pending");
    }

    #[test]
    fn rotation_seals_under_post_removal_epoch_and_removed_member_cannot_open() {
        // at the adapter level: with the orchestration's Remove → seal
        // order, the new generation seals under the post-removal epoch, and the
        // removed member (still at the old epoch — never processed the Remove) can
        // open neither the new envelope (MLS forward secrecy) nor, therefore, the
        // rotated content key.
        let (alice, bob, ch, bob_actor) = two_member_group();
        let pre = alice.current_epoch(&ChannelId(ch)).unwrap();

        // gen-1 sealed pre-removal — bob CAN open it.
        let gen1 = FolderContentKeys::genesis([1u8; 32], 1_000);
        let (sealed_gen1, _) = alice.seal_envelope(&ch, &payload(&gen1, None)).unwrap();
        assert!(
            bob.open_content_key_envelope(&ChannelId(ch), &sealed_gen1)
                .is_ok(),
            "a current member opens the pre-removal envelope"
        );

        // Remove bob → epoch advances (the orchestration does this BEFORE sealing;
        // stage then merge, as `drive_removal` does once the bytes are durable).
        alice.remove_member_staged(&ch, &bob_actor).unwrap();
        alice.merge_pending_commit(&ch).unwrap();
        let post = alice.current_epoch(&ChannelId(ch)).unwrap();
        assert!(post > pre);

        // gen-2 sealed POST-removal → under the new epoch.
        let mut gen2 = gen1.clone();
        gen2.rotate([2u8; 32], 2_000);
        let (sealed_gen2, sealed_epoch) = alice.seal_envelope(&ch, &payload(&gen2, None)).unwrap();
        assert_eq!(
            sealed_epoch, post,
            "the rotation envelope sealed under the post-removal epoch"
        );

        // The removed member, stuck at the old epoch, cannot open the new envelope.
        assert!(
            bob.open_content_key_envelope(&ChannelId(ch), &sealed_gen2)
                .is_err(),
            "removed member cannot open the post-rotation envelope (forward secrecy)"
        );
    }

    #[test]
    fn add_member_staged_defers_the_epoch_and_a_third_member_joins_the_welcome() {
        // The add-path adapter method (`FolderGroupCrypto::add_member_staged`) over
        // a member's serialized key-package bytes — the 2nd..Nth share's group-add
        // step, proven against the REAL engine: staging the Add does not advance the
        // owner's epoch (nothing durable yet), the merge is the point of no return,
        // and the returned Welcome lets a *third* member join the same channel.
        let (alice, _bob, ch, _bob_actor) = two_member_group();
        assert!(
            alice.holds_group(&ch).unwrap(),
            "the owner holds the bound group"
        );
        assert_eq!(
            alice.channel_id_for_group(alice.group_id_bytes(&ChannelId(ch)).unwrap().as_slice()),
            ch,
            "channel_id_for_group == from_group_id(raw)"
        );

        let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let carol_kp = carol.generate_key_packages_bytes(1).unwrap();
        let carol_actor = carol.identity_actor_id();
        let before = alice.current_epoch(&ChannelId(ch)).unwrap();

        let (commit, welcome) =
            FolderGroupCrypto::add_member_staged(&alice, &ch, &carol_kp[0]).unwrap();
        assert!(
            !commit.is_empty() && !welcome.is_empty(),
            "real commit + Welcome bytes"
        );
        assert_eq!(
            alice.current_epoch(&ChannelId(ch)).unwrap(),
            before,
            "a STAGED add must not advance the epoch (nothing durable yet)"
        );
        assert!(
            !alice.contains_member(&ch, &carol_actor).unwrap(),
            "carol is not a member until the merge"
        );

        // Merge → epoch advances, carol joins the roster, and she joins the SAME
        // channel from the returned Welcome (history-on-join).
        FolderGroupCrypto::merge_pending_commit(&alice, &ch).unwrap();
        assert!(
            alice.current_epoch(&ChannelId(ch)).unwrap() > before,
            "the merged Add advanced the epoch"
        );
        assert!(
            alice.contains_member(&ch, &carol_actor).unwrap(),
            "carol is now a member"
        );
        let joined = carol.join_from_welcome_bytes(&welcome).unwrap();
        assert_eq!(joined.0, ch, "carol joined the existing channel");
    }

    #[test]
    fn holds_group_is_false_for_a_channel_this_engine_never_created() {
        let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        assert!(
            !alice.holds_group(&[0x42; 32]).unwrap(),
            "a fresh device holds no group for an unknown channel (the add-path refusal)"
        );
    }
}
