//! The per-group identity-succession ceremony — add-successor, then remove-old
//! (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS groups*).
//!
//! MLS has no in-place credential swap: the credential **is** the `actor_id`, so
//! a succeeded identity cannot rename its own leaf. The successor joins as a new
//! member and the old leaf is removed, which is why this is a two-commit
//! ceremony rather than an update.
//!
//! ## Why each half is authored where it is
//!
//! - **add-successor is authored by the OLD leaf.** The successor is not yet a
//!   member, and a non-member cannot author anything in the group. In the theft
//!   case the owner still holds the old seed, so the old leaf is used
//!   constructively one last time (`identity-succession.md:92`).
//! - **remove-old is authored by the NEW leaf.** MLS forbids committing one's
//!   own removal, so the old leaf structurally cannot finish the ceremony —
//!   [`commit_remove_old`] refuses that call by name rather than relaying an
//!   opaque OpenMLS error.
//!
//! Ratcheting past the remove-old commit is what restores per-group forward
//! secrecy against the thief, which is why § Propagation puts group propagation
//! in the **urgent** phase. It is pinned by
//! `tests/succession_ceremony.rs`.
//!
//! ## Authorship is not transport
//!
//! Neither half is *published* here — this module produces bytes, and the caller
//! sends them. That split is what makes the ceremony runnable at all: after the
//! statement lands, the old identity is refused at every authenticated surface
//! (`fauna.auth.handshake` / `verify` / `device_handshake` and inbound content),
//! so it cannot upload its own commit. The **successor's** session uploads both
//! halves via `fauna.conversations.channel.send`, which auto-registers the
//! sender onto the channel roster — so the group's own membership is carried by
//! MLS, and the nest only ever sees a member-shaped upload.
//!
//! Caveat for the caller: a **claimed folder channel** has an owner-managed
//! roster (only the claimant may drive an auto-register), so a succeeded
//! claimant's folder channels are not reachable by this path. They are out of
//! scope here and tracked separately.
//!
//! ## What this module deliberately does not carry
//!
//! § Propagation also says the succession statement rides in-group alongside the
//! add, so members render continuity instead of "someone added a stranger". That
//! carrier exists — [`crate::types::GroupMetaMessage::Succession`], posted by the
//! sweep driver between the two commits (`fauna_client_recovery::group_sweep`) —
//! but it is still not *this* module's job: the ceremony authors commits and the
//! statement is an application message the caller seals and publishes.

use fauna_core::identity::ActorId;
use openmls::prelude::KeyPackage;
use openmls::prelude::MlsMessageOut;

use crate::engine::MlsEngine;
use crate::error::{MlsError, Result};
use crate::types::ChannelId;

/// The old leaf's half of the ceremony: what the caller must distribute.
pub struct AddSuccessorCommit {
    /// The commit every existing member processes to advance past the add.
    pub commit_bytes: Vec<u8>,
    /// The Welcome the successor joins from.
    pub welcome: MlsMessageOut,
    /// The identity that was added — read back off the KeyPackage credential
    /// rather than taken on the caller's word, so a caller cannot believe it
    /// added a successor it did not.
    pub successor: ActorId,
}

/// Read the `ActorId` a KeyPackage's leaf credential names.
///
/// `add_member` separately enforces that the credential matches the leaf
/// signature key (MLS-2), so this is a read of an already-validated binding at
/// the point of use — never an authorization decision on its own.
fn key_package_actor(key_package: &KeyPackage) -> Result<ActorId> {
    let raw = key_package.leaf_node().credential().serialized_content();
    <[u8; 32]>::try_from(raw).map(ActorId).map_err(|_| {
        MlsError::PolicyViolation("successor credential is not a 32-byte actor id".into())
    })
}

/// Commit **add-successor**, authored by the old (succeeded) leaf.
///
/// Fails when the caller is not a member of the group — a succeeded identity
/// whose leaf is already gone has nothing to author with, and that is the honest
/// answer for a per-group sweep handed a group it never joined.
pub fn commit_add_successor(
    old_engine: &MlsEngine,
    channel_id: &ChannelId,
    successor_key_package: &KeyPackage,
) -> Result<AddSuccessorCommit> {
    let successor = key_package_actor(successor_key_package)?;
    let author = old_engine.identity_actor_id();

    if old_engine
        .find_leaf_by_identity(channel_id, &author)
        .is_none()
    {
        return Err(MlsError::PolicyViolation(format!(
            "add-successor must be authored by a member: {} is not a member of this group",
            hex_id(&author)
        )));
    }
    // Re-running the ceremony over a group that already carries the successor
    // would add a second leaf for the same identity — the sweep must be safe to
    // resume after a partial run.
    if old_engine
        .find_leaf_by_identity(channel_id, &successor)
        .is_some()
    {
        return Err(MlsError::PolicyViolation(format!(
            "successor {} is already a member of this group",
            hex_id(&successor)
        )));
    }

    let (commit_bytes, welcome) = old_engine.add_member(channel_id, successor_key_package)?;
    tracing::info!(
        channel = %channel_id,
        successor = %hex_id(&successor),
        "committed add-successor"
    );
    Ok(AddSuccessorCommit {
        commit_bytes,
        welcome,
        successor,
    })
}

/// Commit **remove-old**, authored by the successor's leaf.
///
/// Every member who processes the returned commit moves to an epoch whose
/// secrets the removed leaves never held.
///
/// ## It removes *every* leaf bearing the old credential, not the first
///
/// Today that is always exactly one leaf, and **deliberately does not rely on
/// it being one**. Multi-seating a credential is currently unrepresentable, but
/// only because of an invariant enforced two layers away: `validate_leaf_binding`
/// makes a fauna credential *be* its leaf signature key, and OpenMLS refuses to
/// seat a duplicate signature key (pinned below by
/// `a_credential_cannot_be_seated_twice`). Reading the first match would make
/// this ceremony silently depend on that — and the failure mode if it ever
/// weakened is the worst one available here: an identity the sweep reports as
/// evicted still reading the group. `find_leaves_by_identity` costs nothing and
/// removes the coupling.
///
/// This is the one eviction rule the client can apply **automatically and
/// without judgment**: the succession statement is proof that this exact
/// credential is compromised, so no leaf carrying it has a legitimate reading.
/// It does *not* extend to leaves bearing *other* credentials the thief may have
/// planted — those are indistinguishable from members added honestly during the
/// compromise window, and are surfaced as residual by the sweep driver rather
/// than guessed at (`fauna_client_recovery::group_sweep`, and
/// `docs/goal/behavior/succession-aftermath.md` § Re-key scope → MLS groups).
pub fn commit_remove_old(
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    old_actor_id: &ActorId,
) -> Result<Vec<u8>> {
    // MLS forbids committing one's own removal, so this call is always a
    // mis-ordered ceremony (the old leaf trying to finish its own succession).
    // Naming it here keeps the caller from reading an OpenMLS error and
    // concluding the group is broken.
    if successor_engine.identity_actor_id() == *old_actor_id {
        return Err(MlsError::PolicyViolation(
            "remove-old cannot remove the caller's own leaf — MLS forbids committing your own \
             removal, so this half is authored by the successor"
                .into(),
        ));
    }

    let leaves = successor_engine.find_leaves_by_identity(channel_id, old_actor_id);
    if leaves.is_empty() {
        return Err(MlsError::PolicyViolation(format!(
            "{} is not a member of this group — nothing to remove",
            hex_id(old_actor_id)
        )));
    }

    let commit = successor_engine.remove_members(channel_id, &leaves)?;
    tracing::info!(
        channel = %channel_id,
        removed = %hex_id(old_actor_id),
        leaves = leaves.len(),
        "committed remove-old"
    );
    Ok(commit)
}

fn hex_id(actor: &ActorId) -> String {
    actor.to_hex()
}
