//! The per-group succession **sweep driver** — run the add-successor /
//! remove-old ceremony over every group a succeeded identity belongs to
//! (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS groups*).
//!
//! [`fauna_mls::succession`] composes the two commits but deliberately never
//! publishes them ("authorship is not transport"). This module is the other
//! half: it enumerates the succeeded identity's groups, runs the pair over each,
//! and posts both commits so the *other* members actually advance. Until it
//! runs, a succession re-points the account while the succeeded credential keeps
//! reading every group — which is why § Propagation puts group propagation in
//! the **urgent** phase. What running it does and does not settle is
//! § *What the sweep can and cannot attest* below; read that before building a
//! surface on the report.
//!
//! ## Why it lives here and not in `fauna-mls`
//!
//! The sweep needs a nest connection, and `fauna_mls::succession`'s whole
//! contract is that it has none. This crate already holds both halves it needs —
//! `fauna-mls` (the ceremony) and `fauna-protocol` (the `channel.send` wire
//! types) — so the driver adds no dependency, and it sits beside
//! [`crate::succession::succeed_identity`], whose own doc comment names this as
//! the aftermath it stops short of.
//!
//! ## Two engines, one process
//!
//! The ceremony is unrunnable with one engine: add-successor can only be
//! authored by the **old** leaf (the successor is not yet a member) and
//! remove-old only by the **successor** (MLS forbids committing your own
//! removal). So [`sweep_groups`] takes both, and is deliberately indifferent to
//! how the caller obtained the old one — there are exactly two paths and they
//! converge here:
//!
//! - **Theft** — the owner still holds the device, so the old identity's MLS
//!   store is on disk: `MlsEngine::new(old_keypair, old_db_path)`.
//! - **Loss with escrow** — the restored seed unwraps the `BackupKey`, which
//!   unseals the `__mls` [`fauna_mls::state_replica::ProviderReplica`];
//!   `restore_into` rehydrates a fresh engine to the same state. This is what
//!   § Propagation means by *"Loss-with-escrow: identical (the restored seed
//!   drives it)"*.
//!
//! Because both engines are co-resident, the Welcome never goes over the wire —
//! it is handed straight to the successor. What is published per group is the
//! two **commits** (so the remaining members advance their epochs) plus the
//! **in-group succession statement** between them (§ below).
//!
//! ## The statement rides between the two commits
//!
//! § Propagation has the statement ride in-group alongside the add, so members
//! render continuity instead of "someone added a stranger". The sweep posts it
//! as an application message — [`GroupMetaMessage::Succession`], carrying the
//! **verbatim canonical `SignedIdentitySuccession` bytes** — authored by the
//! successor at the post-add epoch, *after* the add commit (members cannot
//! decrypt an epoch-N+1 message before processing the N+1 transition) and
//! *before* the remove-old commit. A statement-publish failure aborts that
//! group's sweep **before** remove-old is posted, so within this driver
//! "the old leaf is removed" implies "the statement was offered to the
//! members"; the resume path ([`GroupSweepState::ResumedRemoveOnly`]) re-posts
//! the statement before its remove for the same reason — a duplicate is
//! harmless (consumers verify and apply idempotently), a silent gap is not.
//! Receivers treat the bytes as a **claim** and verify before rendering
//! anything ([`GroupMetaMessage::Succession`]'s trust rule); a receiver
//! that does not recognise the variant skips the record and stays in the group.
//!
//! ## The successor is the only one who can upload
//!
//! After the statement lands, the old identity is refused at every authenticated
//! surface, so it cannot post its own commit. Both halves go up over the
//! **successor's** session via `fauna.conversations.channel.send`, one of the
//! raw roster auto-register paths — so MLS carries membership and the nest only
//! ever sees a member-shaped upload. There is no nest kind for this and none is
//! needed.
//!
//! ## Resumability
//!
//! A sweep can be interrupted at any point, so it reconstructs its position from
//! **membership facts** rather than a stored cursor — nothing to keep in sync,
//! and a fresh client can resume a sweep it did not start. The ordering below is
//! what makes those facts unambiguous: the add commit is **published before the
//! successor joins locally**, so "the successor holds this group" implies "the
//! members were told". The inverse order would let a crash produce a successor
//! that silently posts a remove commit no member can process.
//!
//! One state is genuinely unrecoverable on this path and is therefore *reported*
//! rather than hidden: a crash between authoring the add and publishing it
//! leaves the old engine an epoch ahead of the group with the Welcome gone
//! ([`GroupSweepState::NeedsMemberReAdd`]). § Propagation already declares the
//! remedy — another member re-adds the successor after verifying the statement.
//!
//! ## What the sweep can and cannot attest
//!
//! The ceremony evicts **the succeeded credential** — every leaf bearing
//! `old_actor_id` ([`fauna_mls::succession::commit_remove_old`], which removes
//! all of them rather than depending on MLS's duplicate-signature-key rule to
//! guarantee there is one). That is the whole of what it can do automatically,
//! and it is genuinely decisive: the succession statement is proof that
//! credential is compromised, so no leaf carrying it has a legitimate reading.
//!
//! It does **not** follow that the thief is out. The attacker in this threat
//! model held the seed, therefore the old leaf's authority to author
//! `add_member`, so they may have seated a leaf under some *other* identity they
//! control at any point in the pre-succession window. That leaf survives this
//! ceremony, and the client has no way to tell it from a member added honestly
//! during the same window — the compromise had no announced start, and nothing
//! in this codebase's engine surface dates a leaf's arrival (`group_members`
//! answers *who*, `find_leaves_by_identity` answers *where in the tree*; neither
//! answers *since when*, and leaf index is a tree slot, reused after a removal,
//! not a join order).
//!
//! So the sweep **reports rather than guesses**: every surviving leaf it did not
//! itself add or remove lands in
//! [`GroupSweepOutcome::unattested_members`], and
//! [`SweepReport::unattested_members`] collects them across groups for the one
//! party who *can* classify them — the user, who knows who belongs in their own
//! conversations.
//!
//! **Why not simply remove every leaf the ceremony did not add?** Because that
//! rule's normal case is catastrophic, not its edge case: the ceremony adds
//! exactly one leaf (the successor), so "remove the rest" empties every group of
//! every honest correspondent on every run. A remedy whose ordinary outcome is
//! that the user loses all their conversations does more reliable harm than the
//! planted leaf it targets, and a user who must re-invite everyone will stop
//! running the remedy. The narrower rule that *sounds* right — "remove leaves
//! added during the compromise window" — is not implementable from client state
//! for the dating reason above.
//!
//! The accessors are named for exactly what they answer
//! ([`GroupSweepState::old_leaf_removed`],
//! [`SweepReport::old_leaf_removed_everywhere`]) and there is deliberately no
//! combined "is the user safe" boolean; see the note on [`SweepReport`].
//!
//! ## Claimed folder channels
//!
//! A **claimed folder channel** has an owner-managed roster on both sides. The
//! nest's side — only the claimant may drive the `channel.send` auto-register
//! this sweep uploads through — is answered by the succession transaction,
//! which moves `folder_channel_claims.claimed_by` to the successor before any
//! sweep runs (`succession-aftermath.md` § Re-key scope, the MLS-groups row;
//! built 2026-08-03), so by sweep time the successor *is* the claimant. The
//! member's side is the durable **folder-owner marker** every seat holds
//! (`MlsEngine::folder_channel_owner`): it admits the owner's roster commits
//! and anchors the owner-attested declassification, and it must follow the
//! succession too (`federation.md` § Cross-nest shared folders + channel append
//! → *The marker follows the owner's verified succession*). The members' seats
//! re-stamp it from the in-group statement this sweep posts between its two
//! commits; the successor's own seat is stamped **here**, at its join
//! ([`stamp_successor_folder_owner`]) — the marker the predecessor's engine
//! held, re-pointed to the successor where the predecessor was the owner — and
//! reaches the successor's other devices through the MLS state-replica plane
//! like every other provider value. A nest that still refuses the post is
//! reported as [`GroupSweepState::Failed`] rather than aborting the run.

use fauna_core::data::Timestamp;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::SignedIdentitySuccession;
use fauna_mls::engine::MlsEngine;
use fauna_mls::succession::{commit_add_successor, commit_remove_old};
use fauna_mls::types::{ChannelId, ChannelMessage, ChannelMessageBody, GroupMetaMessage};
use fauna_protocol::conversations::{ChannelEnvelope, ChannelSendReply, ChannelSendRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::error::{RecoveryError, Result};
use crate::kit::hex32;
use crate::nest::RecoveryClient;
use crate::succession::{ReconciledSuccession, reconcile_succession};

/// What the sweep did to one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupSweepState {
    /// Both halves ran in this call: the successor was added, the members were
    /// told, and the old leaf is gone.
    Swept,
    /// The add half was already done by an earlier (interrupted) run, so only
    /// remove-old was owed — and it ran.
    ResumedRemoveOnly,
    /// Nothing was owed: the successor is a member and the old leaf is already
    /// gone. A completed sweep re-run lands every group here, which is what
    /// makes re-running it safe.
    AlreadySwept,
    /// An earlier run authored the add commit but never published it, and the
    /// Welcome did not survive. The old engine is an epoch ahead of the group
    /// and cannot re-author, so this group needs § Propagation's member-side
    /// remedy: another member re-adds the successor after verifying the
    /// statement.
    NeedsMemberReAdd,
    /// This group could not be swept. The reason is carried verbatim so a
    /// claimed folder channel's roster refusal is distinguishable from a
    /// transport fault.
    Failed(String),
}

impl GroupSweepState {
    /// Whether the **succeeded credential** is gone from this group — every leaf
    /// bearing `old_actor_id`, which `commit_remove_old` evicts together.
    ///
    /// ## This is deliberately not named "is the thief out"
    ///
    /// It used to be, and the two questions are not the same one. The ceremony
    /// evicts the credential the succession statement proves compromised; a
    /// thief who held the seed could also have seated a leaf under a *different*
    /// identity they control, and that leaf survives this eviction untouched.
    /// See [`GroupSweepOutcome::unattested_members`], which is where that half
    /// of the answer lives.
    ///
    /// A surface asking "is the user safe now?" must consult **both**. Answering
    /// it from this accessor alone tells a compromised user they are done.
    pub fn old_leaf_removed(&self) -> bool {
        matches!(
            self,
            Self::Swept | Self::ResumedRemoveOnly | Self::AlreadySwept
        )
    }
}

/// One group's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSweepOutcome {
    pub channel_id: ChannelId,
    pub state: GroupSweepState,
    /// Members still in the group that this ceremony can vouch for **nothing**
    /// about: the post-sweep roster minus the successor it just added (the old
    /// leaf is already gone by then, or the state says why not).
    ///
    /// Ordinarily these are the user's honest correspondents, and a normal
    /// healthy group has them — this is **not** a failure signal. It is the
    /// list a recovering user must actually look at, because the one leaf class
    /// the client cannot classify is exactly the one a thief would have planted
    /// (§ *What the sweep can and cannot attest* in the module doc).
    ///
    /// Empty where the group could not be read at all (a `Failed` group whose
    /// roster the engine does not hold).
    pub unattested_members: Vec<ActorId>,
}

/// What a whole sweep did.
///
/// A sweep never fails wholesale: one unreachable group must not deny the
/// remaining ones their re-key, so every group is attempted and its verdict
/// recorded. The caller decides what to surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub outcomes: Vec<GroupSweepOutcome>,
}

impl SweepReport {
    /// How many groups the succeeded credential is gone from.
    pub fn groups_old_leaf_removed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.state.old_leaf_removed())
            .count()
    }

    /// Groups where the ceremony did not finish — the ones still owing work,
    /// and the ones to retry or escalate to § Propagation's member-side re-add.
    pub fn groups_owing_ceremony(&self) -> Vec<&GroupSweepOutcome> {
        self.outcomes
            .iter()
            .filter(|o| !o.state.old_leaf_removed())
            .collect()
    }

    /// Whether the succeeded credential is gone from **every** group.
    ///
    /// ## Deliberately not called `is_complete`
    ///
    /// It was, and a UI reading "complete" as "the user is safe" is exactly the
    /// misreading this rename exists to stop: a `true` here says the ceremony
    /// ran everywhere, *not* that no leaf the thief controls remains. Pair it
    /// with [`Self::unattested_members`] before telling a compromised user
    /// anything reassuring.
    pub fn old_leaf_removed_everywhere(&self) -> bool {
        self.outcomes.iter().all(|o| o.state.old_leaf_removed())
    }

    /// Every member, across every swept group, that the ceremony cannot vouch
    /// for — deduplicated, so a surface asks the user about *identities* rather
    /// than repeating one correspondent per shared group.
    ///
    /// Sorted, so a caller diffing two sweeps or rendering a list gets a stable
    /// order rather than the engine's roster order.
    pub fn unattested_members(&self) -> Vec<ActorId> {
        let mut seen: Vec<ActorId> = self
            .outcomes
            .iter()
            .flat_map(|o| o.unattested_members.iter().copied())
            .collect();
        seen.sort_unstable_by_key(|a| a.0);
        seen.dedup();
        seen
    }

    /// Whether any member survived the sweep that it cannot vouch for.
    pub fn has_unattested_members(&self) -> bool {
        self.outcomes
            .iter()
            .any(|o| !o.unattested_members.is_empty())
    }

    // NOTE — there is deliberately no `is_safe()` / `is_complete()` accessor
    // combining the two axes above, and adding one would undo the fix.
    // "Is the user safe?" is not answerable from client state: the honest answer
    // is "the credential is out, and here are the members I cannot classify",
    // which is two values because it is two facts. A single boolean can only be
    // computed by guessing at one of them, and every guess available here
    // rounds *up* to "safe".
}

/// Run the succession ceremony over every group the succeeded identity holds.
///
/// `client` must be the **successor's** signed-in session (see the module doc:
/// the old identity is refused at every authenticated surface once the statement
/// has landed). `old_engine` and `successor_engine` are the two co-resident MLS
/// engines.
///
/// Both actor ids are read off the engines rather than taken as parameters, so
/// a caller cannot sweep with a mismatched pair.
///
/// ## Persistence is the caller's
///
/// This driver never calls `save_state`: a web engine's only persistence is the
/// nest replica, and a native engine's is its SQLite store, so *when* to persist
/// is a platform decision. Persist both engines after the sweep (or per group,
/// from the report) exactly as every other engine mutation in this codebase is
/// persisted by its caller.
pub async fn sweep_groups<R>(
    client: &RecoveryClient<R>,
    old_engine: &MlsEngine,
    successor_engine: &MlsEngine,
    statement: &SignedIdentitySuccession,
) -> SweepReport
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let old_actor = old_engine.identity_actor_id();
    let successor_actor = successor_engine.identity_actor_id();

    // The same rule as reading both actor ids off the engines: a caller cannot
    // sweep with a mismatched triple. A statement naming any other pair would be
    // posted into every group as a *true, verifiable* statement about the wrong
    // identities — members would verify it and re-point the wrong person — so
    // this refuses before anything is published.
    if statement.statement.old_actor_id != old_actor
        || statement.statement.new_actor_id != successor_actor
    {
        let reason = format!(
            "the statement names {} → {} but the engines are {} → {}: refusing to sweep",
            hex32(&statement.statement.old_actor_id.0),
            hex32(&statement.statement.new_actor_id.0),
            hex32(&old_actor.0),
            hex32(&successor_actor.0),
        );
        tracing::error!("{reason}");
        return failed_sweep_report(
            old_engine,
            successor_engine,
            &old_actor,
            &successor_actor,
            reason,
        );
    }

    // Encoded once, verbatim for every group: these are the bytes the
    // signatures cover, identical to what the nest stores and
    // `succession.lookup` serves.
    let statement_bytes = match fauna_core::encoding::canonical_encode(statement) {
        Ok(bytes) => bytes,
        Err(e) => {
            let reason = format!("the succession statement does not encode: {e}");
            tracing::error!("{reason}");
            // This arm hard-coded `unattested_members: Vec::new()` for every
            // group until 2026-08-10 — reporting *nobody* out of the branch built
            // for a disaster, while its sibling arm four lines up computed the
            // real roster from the very same two engines.
            //
            // It is not fixed by copying the sibling's loop here, because no
            // test can construct this arm to keep a copy honest — and that is a
            // property of the type rather than an untested gap:
            // `SignedIdentitySuccession` is byte arrays, `Vec<u8>`s, a `u64` and
            // a `Timestamp` (no float, no non-string map key, nothing dag-cbor
            // refuses), and `IdentitySuccession::sign` has already
            // canonical-encoded the inner statement for every value that
            // exists. So both arms call ONE helper: the mismatch arm's pin is
            // the only pin either of them can have, and it covers this arm only
            // for as long as there is nothing arm-specific here to drift.
            return failed_sweep_report(
                old_engine,
                successor_engine,
                &old_actor,
                &successor_actor,
                reason,
            );
        }
    };

    let mut outcomes = Vec::new();
    for channel_id in old_engine.list_groups() {
        let state = sweep_one(
            client,
            old_engine,
            successor_engine,
            &channel_id,
            &old_actor,
            &successor_actor,
            &statement_bytes,
        )
        .await
        .unwrap_or_else(|e| GroupSweepState::Failed(e.to_string()));

        // Read the roster AFTER the ceremony, off whichever engine still holds
        // the group: the successor's once it has joined, else the old one (a
        // group the successor never reached is exactly where a planted leaf goes
        // unseen, so falling back is what keeps a `Failed` group from reporting
        // an empty — and falsely reassuring — roster).
        let unattested = unattested_members(
            old_engine,
            successor_engine,
            &channel_id,
            &old_actor,
            &successor_actor,
        );

        tracing::info!(
            channel = %channel_id,
            outcome = ?state,
            unattested = unattested.len(),
            "succession sweep: group done"
        );
        outcomes.push(GroupSweepOutcome {
            channel_id,
            state,
            unattested_members: unattested,
        });
    }

    let report = SweepReport { outcomes };
    tracing::info!(
        old = %hex32(&old_actor.0),
        new = %hex32(&successor_actor.0),
        old_leaf_removed = report.groups_old_leaf_removed(),
        owing_ceremony = report.groups_owing_ceremony().len(),
        unattested = report.unattested_members().len(),
        "succession sweep complete"
    );
    report
}

/// Every group of the old engine, reported [`GroupSweepState::Failed`] for one
/// whole-sweep reason — each still carrying the roster [`unattested_members`]
/// can read.
///
/// ## Why the two disaster arms share this rather than each writing their own
///
/// They diverged once, and in the direction that hurts: the statement-**mismatch** arm computed
/// every group's roster, while the statement-**does-not-encode** arm hard-coded
/// an empty one. Both land as a `Ran` sweep, and an empty roster reaches
/// `raise_succession_member_reviews` as `NothingToRaise` — so the branch written
/// for a disaster was the branch that told the user there was nobody to review,
/// permanently and without a sound. A disaster arm getting the *weaker*
/// parachute is a recurring shape, and one
/// code path is how this pair stops being able to have it: only one of the two
/// arms is constructible from a test, so a pin can cover both only while there
/// is nothing arm-specific left in either.
fn failed_sweep_report(
    old_engine: &MlsEngine,
    successor_engine: &MlsEngine,
    old_actor: &ActorId,
    successor_actor: &ActorId,
    reason: String,
) -> SweepReport {
    let outcomes = old_engine
        .list_groups()
        .into_iter()
        .map(|channel_id| {
            let unattested = unattested_members(
                old_engine,
                successor_engine,
                &channel_id,
                old_actor,
                successor_actor,
            );
            GroupSweepOutcome {
                channel_id,
                state: GroupSweepState::Failed(reason.clone()),
                unattested_members: unattested,
            }
        })
        .collect();
    SweepReport { outcomes }
}

/// The post-ceremony roster of one group, minus the successor this sweep added
/// and minus the old leaf it evicted.
///
/// An old-leaf entry surviving here is not filtered away as noise — it means the
/// eviction did not take, which the group's [`GroupSweepState`] reports on its
/// own axis; excluding it keeps this list answering exactly one question ("who
/// else is here that I cannot classify?").
fn unattested_members(
    old_engine: &MlsEngine,
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    old_actor: &ActorId,
    successor_actor: &ActorId,
) -> Vec<ActorId> {
    // ⚠ The `else` is load-bearing AT REST, not a defensive nicety: since the
    // aftermath writes this roster down, `SweepReport::unattested_members` is
    // the permanent, cross-device basis of the review surfaces (§ Propagation →
    // *Removing a flagged member*, rule (1)). Reading only the successor's
    // engine — which reads as the more correct "ask the current identity" —
    // reports **nobody** for precisely the groups the successor never joined,
    // and `MlsEngine::group_members` answers `Vec::new()` for a group it does
    // not hold, so that mistake is silent and falsely reassuring rather than
    // loud. Pinned by `a_failed_group_reports_the_roster_the_old_engine_still_holds`;
    // before it, severing this fallback reddened nothing.
    let roster = if successor_engine.has_group(channel_id) {
        successor_engine.group_members(channel_id)
    } else {
        old_engine.group_members(channel_id)
    };

    let mut others: Vec<ActorId> = roster
        .into_iter()
        .filter(|m| m != successor_actor && m != old_actor)
        .collect();
    others.sort_unstable_by_key(|a| a.0);
    others.dedup();
    others
}

/// One group. Split out so a per-group failure is a caught `Err`, never a lost
/// sweep.
async fn sweep_one<R>(
    client: &RecoveryClient<R>,
    old_engine: &MlsEngine,
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    old_actor: &ActorId,
    successor_actor: &ActorId,
    statement_bytes: &[u8],
) -> Result<GroupSweepState>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    // Position is reconstructed from membership, not a cursor. The successor
    // holding the group is the load-bearing fact: the publish happens *before*
    // the local join, so it implies the members were told about the add.
    if successor_engine.has_group(channel_id) {
        if successor_engine
            .find_leaf_by_identity(channel_id, old_actor)
            .is_none()
        {
            return Ok(GroupSweepState::AlreadySwept);
        }
        // Membership facts cannot say whether the interrupted run got as far as
        // the statement — or the owner-marker stamp — so the resume redoes
        // both before its remove: a duplicate is harmless (consumers verify
        // and apply idempotently; the stamp is idempotent), a silent gap would
        // leave every member rendering a stranger join, or this seat refusing
        // its own roster commits.
        stamp_successor_folder_owner(
            old_engine,
            successor_engine,
            channel_id,
            old_actor,
            successor_actor,
        );
        publish_statement(client, successor_engine, channel_id, statement_bytes).await?;
        let remove = commit_remove_old(successor_engine, channel_id, old_actor)?;
        publish_commit(client, channel_id, remove).await?;
        return Ok(GroupSweepState::ResumedRemoveOnly);
    }

    // The successor does not hold the group. If the old engine nonetheless
    // already carries a successor leaf, an earlier run authored the add and lost
    // the Welcome before the successor could join — unrecoverable here, and
    // § Propagation's member-side re-add is the declared remedy.
    if old_engine
        .find_leaf_by_identity(channel_id, successor_actor)
        .is_some()
    {
        return Ok(GroupSweepState::NeedsMemberReAdd);
    }

    // ── step 0: on a GOVERNED room, the OLD leaf records its succession ─────
    // A room carrying a policy (`conversation-rooms.md` § Roles and
    // authorization; `fauna_mls::room_policy`) admits the two commits below
    // — an Add by a leaf whose role may forbid inviting, a Remove by a leaf
    // whose role forbids removing — through exactly one door: a
    // `RecordedSuccession` the old leaf itself appends to the group context,
    // naming the successor. Recorded FIRST so the record is agreed state by
    // the time any member judges the add, and so the successor's Welcome
    // carries it; every role then resolves through the chain (an owner's
    // successor owns the room). A policy-less room has no policy and no door to
    // open; an already-recorded succession (an interrupted earlier run) is
    // not recorded twice.
    if let Some(Ok(policy)) = old_engine.room_policy(channel_id)
        && policy.successor_of(old_actor) != Some(*successor_actor)
    {
        let recorded = policy
            .with_succession(fauna_mls::room_policy::RecordedSuccession {
                old: *old_actor,
                new: *successor_actor,
            })
            .map_err(|e| RecoveryError::Crypto(format!("recording the succession: {e}")))?;
        let record = old_engine.set_room_policy_staged(channel_id, &recorded)?;
        publish_commit(client, channel_id, record).await?;
        old_engine.merge_pending_commit(channel_id)?;
    }

    // ── step 1: the OLD leaf commits add-successor ──────────────────────────
    // Refuses a non-member author, which is the honest answer for a group this
    // identity never joined, and refuses a duplicate successor leaf.
    let key_package = successor_engine
        .generate_key_packages(1)?
        .into_iter()
        .next()
        .ok_or_else(|| RecoveryError::Crypto("no successor KeyPackage was minted".into()))?;
    let add = commit_add_successor(old_engine, channel_id, &key_package)?;

    // Publish BEFORE the local join, so that "the successor holds this group"
    // can never be true while the members are still unaware of the add.
    publish_commit(client, channel_id, add.commit_bytes).await?;

    // ── the successor joins, in-process — the Welcome never hits the wire ────
    let joined = successor_engine.join_from_welcome(add.welcome)?;
    if joined != *channel_id {
        return Err(RecoveryError::Crypto(format!(
            "successor joined {joined} from a Welcome authored for {channel_id}"
        )));
    }
    // The sweep stamps its own join (module doc, *Claimed folder channels*).
    stamp_successor_folder_owner(
        old_engine,
        successor_engine,
        channel_id,
        old_actor,
        successor_actor,
    );

    // ── the statement rides between the two commits ─────────────────────────
    // Authored by the successor at the post-add epoch (members process the add
    // commit first, so they can open it), and published BEFORE remove-old: a
    // failure here aborts the group's sweep, which is what keeps "the old leaf
    // is removed" implying "the statement was offered to the members".
    publish_statement(client, successor_engine, channel_id, statement_bytes).await?;

    // ── step 2: the NEW leaf commits remove-old ─────────────────────────────
    // This is the half that ratchets the thief out: every member who processes
    // it moves to an epoch whose secrets the removed leaf never held.
    let remove = commit_remove_old(successor_engine, channel_id, old_actor)?;
    publish_commit(client, channel_id, remove).await?;

    Ok(GroupSweepState::Swept)
}

/// Carry the predecessor's **folder-owner marker** for `channel_id` onto the
/// successor's fresh seat — the successor's own half of *The marker follows the
/// owner's verified succession* (`federation.md` § Cross-nest shared folders +
/// channel append). An in-process `join_from_welcome` stamps nothing (the
/// folder-Welcome join's stamp belongs to the conversations rail), so a
/// channel the old engine held a marker for would otherwise leave the
/// successor on open commit processing. Where the predecessor **was** the
/// owner the marker re-points to the successor — the identity that now owns
/// the set and must author its roster commits; where the predecessor merely
/// joined someone else's folder channel the recorded owner is copied as is. A
/// channel the old engine never stamped (a chat or scheduling group) gets no
/// marker, as before.
fn stamp_successor_folder_owner(
    old_engine: &MlsEngine,
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    old_actor: &ActorId,
    successor_actor: &ActorId,
) {
    if let Some(owner) = old_engine.folder_channel_owner(channel_id) {
        let owner = if owner == *old_actor {
            *successor_actor
        } else {
            owner
        };
        successor_engine.mark_folder_channel_owner(channel_id, &owner);
    }
}

/// Seal and post the in-group succession statement over the successor's
/// session, as a [`GroupMetaMessage::Succession`] application message at the
/// group's current epoch.
///
/// `sequence` is stamped `1` (the [`MlsEngine::build_scheduling_delivery`]
/// precedent for an engine with no thread-store counter): the chat rail's
/// receive path does not consume app-message sequence numbers — MLS itself
/// authenticates and orders the record — and the successor's ordinary
/// conversations session keeps its own counter.
async fn publish_statement<R>(
    client: &RecoveryClient<R>,
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    statement_bytes: &[u8],
) -> Result<()>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let epoch = successor_engine
        .current_epoch(channel_id)
        .map_err(|e| RecoveryError::Crypto(format!("reading the post-add epoch: {e}")))?;
    let message = ChannelMessage {
        sender: successor_engine.identity_actor_id(),
        sequence: 1,
        channel_epoch: epoch,
        body: ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(statement_bytes.to_vec())),
        timestamp: Timestamp::now(),
    };
    let envelope = successor_engine
        .encrypt_to_envelope(channel_id, &message)
        .map_err(|e| RecoveryError::Crypto(format!("sealing the succession statement: {e}")))?;

    let _: ChannelSendReply = client
        .transport()
        .request(
            "fauna.conversations.channel.send",
            ChannelSendRequest {
                channel_id: hex32(&channel_id.0),
                envelope,
                expect_no_commit_since: None,
                attachment_refs: Vec::new(),
                extra: Default::default(),
            },
        )
        .await
        .map_err(RecoveryError::from_transport)?;
    Ok(())
}

/// What [`retry_group_sweep`] found before or instead of sweeping.
///
/// The non-`Swept` arms exist so an app renders the truth rather than a
/// generic failure: `NotLanded` means there is nothing to re-run, and
/// `LandedForAnother` means retrying under the held key would post a statement
/// every member verifies and refuses — both are terminal answers, not retries.
#[derive(Debug)]
pub enum SweepRetryOutcome {
    /// The succession was found on the chain and the sweep ran; the report
    /// says per group what happened. Idempotent over finished groups — a
    /// re-run posts nothing (`sweep_groups`'s membership-facts contract).
    Swept(SweepReport),
    /// No succession for this old identity has landed — nothing to re-run.
    NotLanded,
    /// The account was re-pointed to a different successor; the sweep this
    /// caller could author names a pair the chain does not authorize.
    LandedForAnother {
        /// The successor the chain actually authorizes.
        new_actor_id: ActorId,
    },
}

/// Re-run the per-group succession sweep **after** the ceremony that owed it —
/// the retry for a ceremony whose sweep never ran (`NoEngine`: conversations
/// were not up) or died mid-way (`sweep_partial`, the common network-flake
/// arm). Until this existed the only remedy was member-side: find another
/// member of each affected group and ask them to act.
///
/// The ceremony's own sweep runs pre-switch, where the statement is in hand
/// (`SuccessionHandoff::statement`). Post-switch that handoff is gone, so this
/// re-acquires the **verbatim** landed statement through
/// [`reconcile_succession`] — the chain-verified walk that serves the crash
/// case — and then runs the same [`sweep_groups`] over the same engine pair.
/// Nothing here is a second mechanism: acquisition and sweep are the two
/// existing halves, composed so all 7 apps inherit one retry rather than
/// seven compositions of it.
///
/// ## The caller's engine contract (the app half)
///
/// * `old_engine` — rebuilt from the **surviving** old-identity state: the old
///   registry entry keeps the seed and the old scoped MLS store survives the
///   account switch on disk. A device that never held conversation state for
///   the old identity has nothing to rebuild — and nothing this retry could
///   do: the member-side remedy stands there, and the caller should say so
///   rather than sweep an empty fresh store into existence.
/// * `successor_engine` — the live conversations engine when one is running
///   (never a second engine over the same store), else one constructed over
///   the successor's own scope, exactly as the ceremony's sweep did.
/// * `client` — the **successor's** signed-in session ([`sweep_groups`]'s own
///   requirement).
/// * Persistence stays the caller's, as for [`sweep_groups`]: save both
///   engines after the run.
pub async fn retry_group_sweep<R>(
    client: &RecoveryClient<R>,
    old_engine: &MlsEngine,
    successor_engine: &MlsEngine,
    successor: &ActorKeypair,
) -> Result<SweepRetryOutcome>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let old_actor = old_engine.identity_actor_id();
    match reconcile_succession(client, old_actor, successor, None).await? {
        ReconciledSuccession::Landed(handoff) => {
            // The same driver the ceremony runs — `sweep_groups` re-checks the
            // statement/engine triple itself, and recognises finished groups
            // from membership facts, so a partial sweep resumes and a finished
            // one posts nothing.
            let report =
                sweep_groups(client, old_engine, successor_engine, &handoff.statement).await;
            Ok(SweepRetryOutcome::Swept(report))
        }
        ReconciledSuccession::NotLanded => Ok(SweepRetryOutcome::NotLanded),
        ReconciledSuccession::LandedForAnother { new_actor_id } => {
            Ok(SweepRetryOutcome::LandedForAnother { new_actor_id })
        }
    }
}

/// Post one MLS commit to a channel over the successor's session.
///
/// `expect_no_commit_since` is deliberately `None`. That precondition serializes
/// *one identity's own devices* against advancing the same leaf twice in an
/// epoch (`devices.md` § Cross-device MLS group-state sync); a succession sweep
/// is the only writer of these two commits, and gating them on a seq the
/// successor has never fetched would refuse the ceremony rather than protect it.
/// A duplicate append is harmless — members quiet-skip a past-epoch commit.
async fn publish_commit<R>(
    client: &RecoveryClient<R>,
    channel_id: &ChannelId,
    commit_bytes: Vec<u8>,
) -> Result<()>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let envelope = ChannelEnvelope::Commit(commit_bytes)
        .to_bytes()
        .map_err(|e| RecoveryError::Crypto(format!("encoding the commit envelope: {e}")))?;

    let _: ChannelSendReply = client
        .transport()
        .request(
            "fauna.conversations.channel.send",
            ChannelSendRequest {
                channel_id: hex32(&channel_id.0),
                envelope,
                expect_no_commit_since: None,
                attachment_refs: Vec::new(),
                extra: Default::default(),
            },
        )
        .await
        .map_err(RecoveryError::from_transport)?;
    Ok(())
}
