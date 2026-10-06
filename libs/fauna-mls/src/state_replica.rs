//! Cross-device MLS state replica — the `provider` path family of the `__mls`
//! reserved folder.
//!
//! Authority: `docs/goal/behavior/devices.md` § Cross-device MLS group-state
//! sync; at-rest shape `docs/goal/behavior/file-sync.md` § MLS state replica
//! (design tracked internally).
//!
//! [`ProviderReplica`] is the serialised form of the whole openMLS provider
//! snapshot — the exact inputs `MlsEngine::restore_from_provider_storage`
//! takes. The bytes are canonical CBOR; the *caller* (the shared client sync
//! wrapper, slice 4) seals them under the owner's `BackupKey` before upload,
//! mirroring `DraftStore::snapshot_bytes` in `libs/fauna-conversations`:
//! encode at the state layer, seal at the sync layer, nest stores opaque
//! (`key-material-hierarchy.md` rule #7 — no nest holds `BackupKey`).
//!
//! Concurrent-device writes are CAS-gated on the nest (`fauna.mls.put` `base` /
//! `fauna.mls.conflict`); on a conflict the client resolves with the three-way
//! [`merge_provider_replicas`] against the locally-kept last-synced base and
//! retries. Under the device-owned-epoch invariant a same-key both-changed
//! conflict does not occur (same-group concurrent advance is excluded); if one
//! is met anyway, the merge takes THEIRS — the nest-landed side — and reports
//! the key so the caller reloads the affected local group state.
//!
//! No `Debug` on the replica types: the provider KV carries group secrets.

use crate::engine::MlsEngine;
use crate::error::{MlsError, Result};
use crate::types::ChannelId;
use fauna_core::identity::ActorId;
use openmls::prelude::{GroupId, MlsGroup};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::{HashMap, HashSet};

/// The serialised openMLS provider snapshot: the raw KV store plus the
/// `(ChannelId, raw MLS GroupId)` pairs needed to reload each group.
///
/// Both vecs are **sorted** (by key / by channel id) so encoding is
/// byte-stable for equal logical state — the same property
/// `DraftsSnapshot` guarantees, making "byte-equal replica" a meaningful
/// cross-device dedup baseline for the sync wrapper.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProviderReplica {
    /// Sorted `(key, value)` pairs of the raw provider KV store
    /// ([`MlsEngine::export_provider_storage`]).
    values: Vec<(ByteBuf, ByteBuf)>,
    /// Sorted `(channel_id bytes, raw MLS group id)` pairs
    /// ([`MlsEngine::list_groups_with_raw_ids`]).
    group_ids: Vec<(ByteBuf, ByteBuf)>,
    /// Sorted `(channel_id bytes, blake3(wire commit bytes))` pairs — the
    /// **commit identity** of each channel's still-pending (staged, unmerged)
    /// commit, stamped at gate-send time so a crash-window resync merges the
    /// step-2 replica's pending **only when it is the same commit as the logged
    /// own-leaf record** (`FaunaCommitGate::resync_channel`, design §3 takeover
    /// crash-safety). Binds the epoch-equality merge to commit identity, closing
    /// the malicious-nest targeted-rollback fork.
    ///
    /// `#[serde(default)]` (the additive discipline; empty = no pending). A
    /// channel with no identity here is **never** merged by the resync arm —
    /// the epoch-only equality a stamp-less replica once got was retired by the
    /// compat-remnant sweep (`version-compatibility.md` § Dimension 2, program
    /// 4). A `Vec`-of-pairs, not `Option`: dag-cbor nested `Option` is not
    /// round-trippable.
    #[serde(default)]
    pending_commit_hashes: Vec<(ByteBuf, ByteBuf)>,
    /// Sorted `(channel_id bytes, processed-seq)` pairs — the per-channel **ingest
    /// cursor this provider snapshot was captured at**: the highest channel `seq`
    /// the engine in these `values` had folded. The durable read position of the
    /// crypto state, living in the *same blob* as the crypto state it indexes, so
    /// one CAS persists both and a torn `{provider, cursor}` pair is
    /// unrepresentable (`devices.md` § Cross-device MLS group-state sync, Rule 2).
    ///
    /// Before this field the cursor was seeded from each `history/<ch>` slice's
    /// `watermark` — a *different* blob, a *different* CAS. A tick whose provider
    /// PUT failed while a history PUT succeeded left `{provider @ epoch N,
    /// watermark past the N→N+1 commit}`: the next launch resumed the poll after a
    /// commit it had never applied, every later foreign commit quiet-skipped as
    /// `PastEpochCommit`, and the device was stranded at epoch N with no self-heal.
    ///
    /// **Additive**, same shape as `pending_commit_hashes`:
    /// `#[serde(default)]`, so a replica with no cursors (a device that holds a
    /// slice before its first poll) decodes with an empty vec and the loader
    /// falls back to the slice watermark (the recorded live keep). `i64` matches `ChannelHistorySlice::watermark`.
    #[serde(default)]
    cursors: Vec<(ByteBuf, i64)>,
}

/// Outcome of [`merge_provider_replicas`]: the merged replica plus the keys
/// where both sides changed to different values **and the change was a
/// genuine two-writer collision** (theirs won). A both-changed key that the
/// merge could attribute to a group and classify as ordinary same-leaf
/// progress — two devices at different points of one stream, or one device
/// lagging a commit the other folded — is *reconciled* instead: the side that
/// is further along wins, and the key is counted in `reconciled_keys`, never
/// reported (`merge_provider_replicas` states the classification).
///
/// **What a caller may do with `conflicted_keys`, and what it may not.** Until
/// 2026-09-01 this comment told the caller to "reload the affected local group
/// state from the merged replica". No caller ever did, and none should: provider
/// KV entries are not independently swappable — splicing the winning bytes for
/// a few keys into a running engine composes a state no engine ever authored
/// (one side's ratchet tree beside the other's secret tree), which is strictly
/// worse than either side alone. The only coherent reload is the **whole**
/// snapshot, and that door already exists with its own detector and its own
/// seating refusal: `fauna_client_mls_sync::MlsStateSync::resync_provider`,
/// driven from the inbound path when a commit arrives on this device's own leaf
/// that it did not author.
///
/// So the field is a **report**, not a repair trigger: a key both sides changed
/// means two writers advanced this account's one MLS leaf concurrently, and the
/// resolution here is lossy for ratchet state. Its consumer is the loud warning
/// in `fauna_client_mls_sync::MlsReplicaClient::save_provider_cas`; the ruling
/// and the two surfaces it deliberately does *not* use live in
/// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
/// *A provider CAS conflict is reported, not repaired*.
pub struct ProviderMergeOutcome {
    pub merged: ProviderReplica,
    pub conflicted_keys: Vec<Vec<u8>>,
    /// Both-changed keys the classification resolved as same-leaf progress
    /// (a count, never the keys — same reason as the report's consumer).
    pub reconciled_keys: usize,
}

/// What [`ProviderReplica::import_group_into`] put into the engine: one group's
/// entries, as openMLS itself attributes them, plus the per-channel facts the
/// snapshot carried for it. Handed back so the caller can fold the same bytes
/// into its merge baseline ([`ProviderReplica::absorb_adopted_group`]) and seed
/// its ingest cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedGroup {
    pub channel: ChannelId,
    /// The raw MLS group id (the `group_ids` pair's value).
    pub raw_group_id: Vec<u8>,
    /// The snapshot's ingest cursor for the channel, if it carried one.
    pub cursor: Option<i64>,
    /// The snapshot's pending-commit identity for the channel, if any.
    pub pending_commit_hash: Option<[u8; 32]>,
    /// Sorted `(key, value)` pairs — exactly what was inserted into the engine.
    pub entries: Vec<(Vec<u8>, Vec<u8>)>,
}

/// What [`ProviderReplica::seating_verdict`] could establish about a snapshot's
/// groups — the answer to *"may this identity restore these bytes?"*.
///
/// Two counts, not one list, because the question has three answers and the old
/// `Vec` could only carry two. An empty `foreign` used to mean *restore*, which
/// silently conflated **"examined every group, none is another identity's"**
/// with **"could not examine anything"** — and the second is precisely the
/// state a successor must not restore from
/// (`succession-aftermath.md` § Re-key scope → *What a successor's replica
/// restore may take from a predecessor's*, rule (1): the leaf a snapshot seats
/// is a fact of its bytes, so a snapshot whose bytes will not yield that fact
/// establishes nothing).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SeatingVerdict {
    /// Groups positively identified as seated under a leaf that is **not** this
    /// identity's. Non-empty means the snapshot belongs to another identity —
    /// after a succession, the predecessor's.
    pub foreign: Vec<ChannelId>,
    /// Groups the snapshot lists whose seating could not be established at all:
    /// a channel id that is not 32 bytes, or a group that would not load out of
    /// the snapshot's own KV. **Not** a group that loaded and no longer seats
    /// this identity — that one is examined (see
    /// [`ProviderReplica::seating_verdict`]).
    pub unexaminable: usize,
}

impl SeatingVerdict {
    /// Every listed group was examined, and none of them is another identity's.
    /// The only verdict that permits [`ProviderReplica::restore_into`].
    pub fn is_clean(&self) -> bool {
        self.foreign.is_empty() && self.unexaminable == 0
    }
}

impl ProviderReplica {
    /// Capture the engine's current provider state.
    pub fn from_engine(engine: &MlsEngine) -> Self {
        let mut values: Vec<(ByteBuf, ByteBuf)> = engine
            .export_provider_storage()
            .into_iter()
            .map(|(k, v)| (ByteBuf::from(k), ByteBuf::from(v)))
            .collect();
        values.sort();
        let mut group_ids: Vec<(ByteBuf, ByteBuf)> = engine
            .list_groups_with_raw_ids()
            .into_iter()
            .map(|(cid, gid)| (ByteBuf::from(cid.0.to_vec()), ByteBuf::from(gid)))
            .collect();
        group_ids.sort();
        // Pending-commit identities come straight off the engine, which stamps one
        // whenever it stages a commit and drops it on merge/clear — so a captured
        // replica's `pending_commit_hashes` always agrees with the pendings living
        // in its own `values`. (Before the engine owned this, the identities lived
        // in an in-memory `MlsStateSync` map that a restart silently emptied while
        // the restored provider still held the pending: the next autosave then
        // sealed a replica whose pending carried *no* identity, dropping the resync
        // arm back to epoch-only equality — the very targeted-rollback fork
        // `pending_commit_hashes` exists to close.) The ingest cursors still live
        // in `MlsStateSync` and are folded in at the save chokepoint via
        // [`Self::with_cursors`].
        let mut pending_commit_hashes: Vec<(ByteBuf, ByteBuf)> = group_ids
            .iter()
            .filter_map(|(cid, _)| {
                let channel = ChannelId(cid.as_slice().try_into().ok()?);
                let hash = engine.pending_commit_hash(&channel)?;
                Some((cid.clone(), ByteBuf::from(hash.to_vec())))
            })
            .collect();
        pending_commit_hashes.sort();
        ProviderReplica {
            values,
            group_ids,
            pending_commit_hashes,
            cursors: Vec::new(),
        }
    }

    /// Return a copy carrying the given per-channel pending-commit identities
    /// (`blake3` of each channel's staged commit wire bytes), replacing any it
    /// already held. Sorted by channel-id bytes for byte-stable encoding.
    ///
    /// Production capture reads the identities off the engine in
    /// [`Self::from_engine`]; this setter exists for tests that construct a
    /// replica without one.
    pub fn with_pending_hashes(mut self, hashes: &[(ChannelId, [u8; 32])]) -> Self {
        let mut v: Vec<(ByteBuf, ByteBuf)> = hashes
            .iter()
            .map(|(cid, h)| (ByteBuf::from(cid.0.to_vec()), ByteBuf::from(h.to_vec())))
            .collect();
        v.sort();
        self.pending_commit_hashes = v;
        self
    }

    /// The stamped pending-commit identity for `channel` (`blake3` of its staged
    /// commit wire bytes), or `None` when this replica carries none for it — a
    /// channel with no staged commit. The resync arm never merges on `None`
    /// (the engine stamps every commit it stages, so a current replica holding
    /// a pending always names it).
    pub fn pending_commit_hash(&self, channel: &ChannelId) -> Option<[u8; 32]> {
        let key = channel.0.as_slice();
        self.pending_commit_hashes
            .iter()
            .find(|(cid, _)| cid.as_slice() == key)
            .and_then(|(_, h)| h.as_slice().try_into().ok())
    }

    /// Return a copy carrying the given per-channel ingest cursors (the highest
    /// channel `seq` the captured engine had folded). Sorted by channel-id bytes
    /// for byte-stable encoding. Folded in at the `save_provider_*` chokepoint from
    /// the `MlsStateSync` cursor map — `from_engine` alone cannot know them.
    pub fn with_cursors(mut self, cursors: &[(ChannelId, i64)]) -> Self {
        let mut v: Vec<(ByteBuf, i64)> = cursors
            .iter()
            .map(|(cid, seq)| (ByteBuf::from(cid.0.to_vec()), *seq))
            .collect();
        v.sort();
        self.cursors = v;
        self
    }

    /// The ingest cursor this snapshot was captured at for `channel`, or `None`
    /// when this replica carries none for it — a channel the device had not polled
    /// (it may still hold a slice). The loader treats
    /// `None` as "fall back to the `history/<ch>` slice's watermark"
    /// (see the field doc).
    pub fn cursor(&self, channel: &ChannelId) -> Option<i64> {
        let key = channel.0.as_slice();
        self.cursors
            .iter()
            .find(|(cid, _)| cid.as_slice() == key)
            .map(|(_, seq)| *seq)
    }

    /// The channels whose group, loaded from this snapshot, is seated under a
    /// leaf whose credential is **not** `identity` — a snapshot captured by
    /// another identity's engine. Empty for a snapshot that is `identity`'s
    /// own (every own leaf names it) and for one holding no groups.
    ///
    /// **Why the question is asked of the snapshot, per group, and never of the
    /// path it was fetched from or the key that opened it.** A `provider` is the
    /// crypto state of a *leaf*, and a leaf is an identity. After a succession
    /// the successor's `__mls` path holds the predecessor's snapshot (ownership
    /// moved in the nest's succession transaction; the re-seal re-keyed it so it
    /// opens), and restoring it seats the successor as the **old** leaf — the
    /// very leaf remove-old exists to ratchet out, so the successor decrypts as
    /// the credential the ceremony evicts and its own remove-old answers
    /// `CannotRemoveSelf` (`succession-aftermath.md` § Re-key scope → *What a
    /// successor's replica restore may take from a predecessor's*). The re-seal's
    /// outcome cannot carry this verdict: a first session that crashed before
    /// its own state replaced the snapshot re-runs the pass as `AlreadyCurrent`.
    /// The own-leaf credential is a fact of the bytes, so it survives every
    /// ordering.
    ///
    /// ⚠ **A group this cannot examine is NOT a vote of confidence.** The
    /// verdict used to be a bare `Vec` of positively-identified foreign seats,
    /// and its caller restored on empty — so a snapshot whose groups would not
    /// load at all scored "clean" and was restored wholesale, which is exactly
    /// what rule (1) forbids. The count of unexaminable groups therefore rides
    /// alongside, and [`SeatingVerdict::is_clean`] requires both halves.
    ///
    /// **What counts as examined** matters as much as the count: a group that
    /// LOADS but whose own leaf is absent from `members()` is examined and not
    /// foreign — that is an ordinary state (another member's remove-old already
    /// evicted this identity, and the engine has no self-removal forget path),
    /// and treating it as unknown would refuse the restore of every snapshot
    /// belonging to a user who has ever been removed from one group.
    pub fn seating_verdict(&self, identity: &ActorId) -> SeatingVerdict {
        // A scratch openMLS storage over this snapshot's KV.
        //
        // ⚠ This is NOT byte-identical to the state the engine loads from:
        // `MlsEngine::restore_from_provider_storage` re-stores the signer into
        // the provider after swapping the values, and this scratch provider has
        // no signer at all. What this therefore establishes is narrower than
        // "what loads here is what would be restored there" (the claim that
        // used to stand here, and is false as written): it establishes the
        // seating of the groups it *does* load, and reports the rest as
        // unexaminable rather than as clean. Whether the missing signer can
        // change a load outcome is unresolved — settling it means reading
        // openMLS's own `MlsGroup::load`, which this project does not do.
        let provider = scratch_over(&self.values);
        let mut verdict = SeatingVerdict::default();
        for (cid, gid) in &self.group_ids {
            let Ok(bytes) = <[u8; 32]>::try_from(cid.as_slice()) else {
                // A channel id we cannot even name is a group we cannot check.
                verdict.unexaminable += 1;
                continue;
            };
            let Ok(Some(group)) = MlsGroup::load(provider.storage(), &GroupId::from_slice(gid))
            else {
                verdict.unexaminable += 1;
                continue;
            };
            let own = group.own_leaf_index();
            let seated_as = group
                .members()
                .find(|m| m.index.u32() == own.u32())
                .map(|m| m.credential.serialized_content().to_vec());
            // `None` here is EXAMINED, not unknown: the group loaded and simply
            // does not seat this identity any more (an eviction that already
            // landed). See the doc comment above for why that distinction is
            // load-bearing rather than pedantic.
            if let Some(cred) = seated_as
                && cred.as_slice() != identity.0.as_slice()
            {
                verdict.foreign.push(ChannelId(bytes));
            }
        }
        verdict
    }

    /// A snapshot that decodes cleanly, names one well-formed 32-byte channel,
    /// and whose KV holds **nothing that group loads from** — the shape
    /// [`Self::seating_verdict`] must report as *unexaminable* rather than
    /// clean, and the shape its caller must refuse to restore.
    ///
    /// Test-only, and cross-crate on purpose: the refusal is
    /// [`Self::restore_into`]'s, but what a refusal must *cost* is each door's
    /// own answer — the launch holds the save gate
    /// (`fauna_client_mls_sync::orchestration::restore_and_wire`), the
    /// mid-session resync stalls the channel
    /// (`fauna_client_mls_sync::sync::MlsStateSync::resync_provider`) — so the
    /// pins that prove an engine is not re-seated live over there, and
    /// `ProviderReplica`'s fields are private. It is not reachable from a
    /// production build (`test-helpers`), which is the same posture the crate's
    /// other attacker-simulation seams take.
    ///
    /// It is not an exotic shape: rule (3) publishes this blob to the nest for
    /// the identity's *other* devices, and those may run different versions
    /// within a major, so a device meeting a group serialization it cannot load
    /// reaches it with no attacker and no bug elsewhere.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn unexaminable_for_test(channel: ChannelId) -> Self {
        Self {
            values: vec![],
            group_ids: vec![(
                ByteBuf::from(channel.0.to_vec()),
                ByteBuf::from(b"a-group-nothing-loads".to_vec()),
            )],
            ..Default::default()
        }
    }

    /// Restore this replica into `engine` — **iff its own-leaf seating is this
    /// engine's identity's**. Returns the [`SeatingVerdict`]; the restore
    /// happened exactly when [`SeatingVerdict::is_clean`] holds, and the caller
    /// reads the verdict only to choose how to *react* to a refusal, never to
    /// decide it.
    ///
    /// ⚠ **The check lives here because this swaps the engine's WHOLE provider
    /// KV.** A `provider` is the crypto state of a *leaf*, and a leaf is an
    /// identity, so restoring a snapshot another identity's engine captured
    /// seats this engine as that identity — after a succession, as the very
    /// leaf remove-old exists to ratchet out (`succession-aftermath.md`
    /// § Re-key scope → *What a successor's replica restore may take from a
    /// predecessor's*, rule (1): a provider seated under a foreign leaf is
    /// **never** restored, and one whose bytes will not yield that fact
    /// establishes nothing, so it is not restored either).
    ///
    /// **Why the door asks, and not its callers.** Rule (1) is an absolute over
    /// the *snapshot*, so a per-caller check can only ever be as complete as the
    /// caller census — and that census has now failed twice on this one
    /// function. Asking here makes a third miss unrepresentable: every
    /// production restore door is this function, the identity comes off the
    /// engine being restored into rather than from a caller that could pass the
    /// wrong one, and the un-checked swap is reachable only as
    /// `restore_into_unchecked`, which no production build compiles.
    ///
    /// A `group_ids` entry whose channel-id bytes are not 32 bytes long is
    /// skipped by the swap (same skip-don't-fail posture as the group-load loop)
    /// — but it counts as *unexaminable* in the verdict, so such a snapshot is
    /// refused before the swap can skip anything.
    /// **Two refusals, one door.** The verdict answers rule (1); the `Err`
    /// answers the quiesce contract — a **retired** engine refuses every
    /// group-state mutation, this one included (`account-data-plane.md`
    /// § Multi-instance concurrency), and it is reachable with no attacker (a
    /// late launch-restore retry, or a predecessor's still-draining receive
    /// loop driving a mid-session resync). They are kept as separate answers on
    /// purpose: "this snapshot is not yours" and "this engine is gone" call for
    /// different reactions from the caller, and folding the second into the
    /// verdict would make a retired engine look like an unexaminable snapshot.
    #[must_use = "the verdict says whether the restore happened; a refusal needs the caller's own answer"]
    pub fn restore_into(&self, engine: &MlsEngine) -> Result<SeatingVerdict> {
        let verdict = self.seating_verdict(&engine.identity_actor_id());
        if verdict.is_clean() {
            self.restore_unconditionally(engine)?;
        }
        Ok(verdict)
    }

    /// **Adopt ONE group this engine has never held from a sibling device's
    /// snapshot, touching nothing else** — the mid-session door for a group
    /// another of the user's devices joined or created (`devices.md`
    /// § Cross-device MLS group-state sync → *A sibling-joined group is adopted
    /// mid-session by a targeted import*). The whole-KV swap
    /// ([`Self::restore_into`]) is the launch's door and the own-leaf resync's;
    /// swapping a running engine's entire KV every time a sibling flushes would
    /// rewind every other group's in-flight state. This imports only the
    /// entries of `channel`'s group, and only into an engine that holds no
    /// entry of it.
    ///
    /// **Attribution comes from openMLS itself, never from parsing its keys.**
    /// The group is loaded out of a scratch store over this snapshot's KV and
    /// then deleted from that scratch store; the keys the delete removed ARE the
    /// group's entries, by the one authority that knows its own key layout. A
    /// key-encoding change in a dependency bump therefore cannot silently
    /// mis-attribute an entry: the delete moves with it.
    ///
    /// **Rule (1) is asked here as at every door**: the group's own leaf must be
    /// seated as this engine's identity, else the import is refused — the same
    /// per-group question [`Self::seating_verdict`] asks, on the same bytes.
    ///
    /// **Why this is not the partial reload the conflict ruling rejects.** That
    /// rejection is about splicing a few winning keys into a group the engine
    /// already runs — composing a state no engine authored. Here the engine
    /// holds *no* entry of the group ([`MlsEngine::adopt_group_entries`] refuses
    /// on any overlap), so the imported entries are exactly one snapshot's
    /// coherent view of the group, loaded by openMLS as a unit and then
    /// persisted the way a joined group is.
    ///
    /// **Only a group that positively seats this identity is imported.** The
    /// seating question has three answers and this door refuses two of them:
    /// a group seated under *another* identity's leaf is a predecessor's
    /// ([`MlsError::PolicyViolation`] — rule (1), `succession-aftermath.md`
    /// § Re-key scope), and a group whose own leaf seats *nobody* — the shape
    /// of every group this identity was evicted from — is
    /// [`MlsError::NotSeated`]. The second is where this door parts from
    /// [`Self::seating_verdict`], which counts the same blank seat as
    /// examined-and-clean: that verdict is over the whole snapshot, so a
    /// blank seat there is protected by its sibling groups (a predecessor's
    /// snapshot positively seats the predecessor somewhere, and the aggregate
    /// is refused), while a per-group import has no sibling to answer for it.
    /// An evicted group stays unadopted — the end state an eviction means.
    ///
    /// Returns what was imported so the caller can fold it into its merge
    /// baseline ([`Self::absorb_adopted_group`]): the engine's next export
    /// carries the group, and a baseline that does not would report it as a
    /// concurrent-writer conflict.
    pub fn import_group_into(
        &self,
        engine: &MlsEngine,
        channel: &ChannelId,
    ) -> Result<AdoptedGroup> {
        if engine.has_group(channel) {
            return Err(MlsError::PolicyViolation(format!(
                "adopt {channel}: this engine already holds the group — the targeted import \
                 is for a group it has never seen"
            )));
        }
        let Some((_, raw_group_id)) = self
            .group_ids
            .iter()
            .find(|(cid, _)| cid.as_slice() == channel.0.as_slice())
        else {
            return Err(MlsError::ChannelNotFound(format!(
                "adopt {channel}: the snapshot lists no group for it"
            )));
        };
        // A scratch openMLS store over this snapshot's KV — the same scratch
        // `seating_verdict` reads, for the same reason: the question is asked of
        // the snapshot's own bytes.
        let scratch = scratch_over(&self.values);
        let examined = match examine_group_in(&scratch, raw_group_id) {
            Ok(examined) => examined,
            Err(ExamineFailure::DoesNotLoad) => {
                return Err(MlsError::Storage(format!(
                    "adopt {channel}: the group does not load out of the snapshot's own KV \
                     (unexaminable — not adopted)"
                )));
            }
            Err(ExamineFailure::ScratchDelete(e)) => {
                return Err(MlsError::OpenMls(format!(
                    "adopt {channel}: scratch delete: {e}"
                )));
            }
            Err(ExamineFailure::NoEntries) => {
                return Err(MlsError::Storage(format!(
                    "adopt {channel}: deleting the group from the scratch store removed no \
                     entry — attribution established nothing, not adopted"
                )));
            }
        };
        match examined.seated_as {
            Some(ref cred) if cred.as_slice() == engine.identity_actor_id().0.as_slice() => {}
            Some(_) => {
                return Err(MlsError::PolicyViolation(format!(
                    "adopt {channel}: the snapshot seats this group under another identity's \
                     leaf — never imported (succession-aftermath.md § Re-key scope, rule (1))"
                )));
            }
            // ⚠ The same `None` that `seating_verdict` counts as EXAMINED is
            // refused here — two doors, two answers, deliberately. There the
            // verdict is over the whole snapshot, and a blank seat is protected
            // by its siblings: a predecessor's snapshot positively seats the
            // predecessor somewhere, so the aggregate is refused as foreign.
            // This door is per-group, which strips exactly that protection and
            // puts nothing in its place — refusing one blank-seat group vetoes
            // nothing. So the identity question must be answered POSITIVELY or
            // the import does not happen; a group this identity was evicted
            // from stays unadopted, which is the end state an eviction means.
            None => {
                return Err(MlsError::NotSeated(format!(
                    "adopt {channel}: the snapshot's own leaf seats nobody in this group (an \
                     eviction that already landed) — only a group that positively seats this \
                     identity is imported; it stays unadopted"
                )));
            }
        }
        let entries = examined.entries;

        engine.adopt_group_entries(&entries, channel, raw_group_id)?;
        Ok(AdoptedGroup {
            channel: *channel,
            raw_group_id: raw_group_id.to_vec(),
            cursor: self.cursor(channel),
            pending_commit_hash: self.pending_commit_hash(channel),
            entries,
        })
    }

    /// Fold an [`AdoptedGroup`] into this replica — the merge-baseline half of
    /// the import. The engine's next export carries the adopted entries; a
    /// baseline that does not would classify them as "both sides added,
    /// different bytes" the moment the engine advances the group, and report a
    /// concurrent-writer conflict that never happened. Adding exactly the
    /// imported entries, the group id, and the cursor + pending stamp the
    /// snapshot carried keeps the baseline at what the engine now holds.
    pub fn absorb_adopted_group(&mut self, adopted: &AdoptedGroup) {
        for (k, v) in &adopted.entries {
            let key = ByteBuf::from(k.clone());
            match self
                .values
                .binary_search_by(|(existing, _)| existing.cmp(&key))
            {
                Ok(i) => self.values[i].1 = ByteBuf::from(v.clone()),
                Err(i) => self.values.insert(i, (key, ByteBuf::from(v.clone()))),
            }
        }
        let cid = ByteBuf::from(adopted.channel.0.to_vec());
        let gid = ByteBuf::from(adopted.raw_group_id.clone());
        match self.group_ids.binary_search_by(|(c, _)| c.cmp(&cid)) {
            Ok(i) => self.group_ids[i].1 = gid,
            Err(i) => self.group_ids.insert(i, (cid.clone(), gid)),
        }
        if let Some(seq) = adopted.cursor {
            match self.cursors.binary_search_by(|(c, _)| c.cmp(&cid)) {
                Ok(i) => self.cursors[i].1 = seq,
                Err(i) => self.cursors.insert(i, (cid.clone(), seq)),
            }
        }
        if let Some(h) = adopted.pending_commit_hash {
            let hash = ByteBuf::from(h.to_vec());
            match self
                .pending_commit_hashes
                .binary_search_by(|(c, _)| c.cmp(&cid))
            {
                Ok(i) => self.pending_commit_hashes[i].1 = hash,
                Err(i) => self.pending_commit_hashes.insert(i, (cid, hash)),
            }
        }
    }

    /// The whole-KV swap with **no seating question asked** — the shape rule (1)
    /// forbids in production, kept reachable for tests that must stage a
    /// pre-fix engine state (a predecessor's snapshot forced into a successor's
    /// engine, a stale replica wiping an init key) or that restore a snapshot
    /// whose cleanliness is the fixture's premise rather than the thing under
    /// test.
    ///
    /// Test-only by construction: no production build compiles it, so the
    /// caller census [`Self::restore_into`] replaces cannot be re-opened by a
    /// new call site here.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn restore_into_unchecked(&self, engine: &MlsEngine) -> Result<()> {
        self.restore_unconditionally(engine)
    }

    /// The swap itself. Private: every caller reaches it through
    /// [`Self::restore_into`] (which asks rule (1)'s question first) or the
    /// test-only [`Self::restore_into_unchecked`].
    fn restore_unconditionally(&self, engine: &MlsEngine) -> Result<()> {
        let values: HashMap<Vec<u8>, Vec<u8>> = self
            .values
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();
        let group_ids: Vec<(ChannelId, Vec<u8>)> = self
            .group_ids
            .iter()
            .filter_map(|(cid, gid)| {
                let bytes: [u8; 32] = cid.as_slice().try_into().ok()?;
                Some((ChannelId(bytes), gid.to_vec()))
            })
            .collect();
        engine.restore_from_provider_storage(values, &group_ids)
    }

    /// Canonical CBOR bytes (byte-stable for equal logical state).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        fauna_cbor::encode_canonical(self).map_err(|e| MlsError::Encoding(e.to_string()))
    }

    /// Decode from [`Self::to_bytes`] output.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        fauna_cbor::decode_strict(bytes).map_err(|e| MlsError::Encoding(e.to_string()))
    }

    /// The channels this replica carries, for callers that iterate groups
    /// after a restore (e.g. to fetch each `history/<channel_hex>` slice).
    pub fn channel_ids(&self) -> Vec<ChannelId> {
        self.group_ids
            .iter()
            .filter_map(|(cid, _)| {
                let bytes: [u8; 32] = cid.as_slice().try_into().ok()?;
                Some(ChannelId(bytes))
            })
            .collect()
    }

    /// Whether this replica's own bytes carry `channel_id`'s **durable chat
    /// marker** ([`MlsEngine::mark_channel_chat`] — stamped at bind, riding
    /// the provider KV this blob is a snapshot of). Answered from the
    /// snapshot, not an engine: the launch-evidence predicate asks it of the
    /// blob it just read, before and regardless of whether the restore into
    /// the engine was permitted. Like the engine-side read, `false` means
    /// "not marked" and never "provably not chat": a folder channel, or a chat group
    /// before its bind stamps the marker, carries no key.
    pub fn is_channel_chat(&self, channel_id: &ChannelId) -> bool {
        let key = crate::engine::channel_kind_chat_key(channel_id);
        self.values
            .iter()
            .any(|(k, _)| k.as_slice() == key.as_slice())
    }
}

/// A scratch openMLS provider whose storage holds exactly `values` — the one
/// way this module asks a question of a snapshot's own bytes without loading
/// them into a live engine (`seating_verdict`, `import_group_into`, and the
/// merge's classification all read through it).
fn scratch_over(values: &[(ByteBuf, ByteBuf)]) -> OpenMlsRustCrypto {
    let scratch = OpenMlsRustCrypto::default();
    {
        let mut store = scratch.storage().values.write().unwrap();
        *store = values
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();
    }
    scratch
}

/// [`scratch_over`] for a raw KV map — the engine's own live store, cloned, so
/// the launch door can ask the same questions of the engine's pre-swap bytes
/// that the adoption door asks of a snapshot's
/// (`MlsEngine::restore_from_provider_storage`).
pub(crate) fn scratch_over_map(values: &HashMap<Vec<u8>, Vec<u8>>) -> OpenMlsRustCrypto {
    let scratch = OpenMlsRustCrypto::default();
    {
        let mut store = scratch.storage().values.write().unwrap();
        *store = values.clone();
    }
    scratch
}

/// What one group's bytes say about themselves, read off a scratch store: who
/// its own leaf seats, and which entries openMLS itself attributes to it.
pub(crate) struct ExaminedGroup {
    /// The own leaf's serialized credential content, `None` when the leaf
    /// seats nobody (an eviction that already landed).
    pub seated_as: Option<Vec<u8>>,
    /// Sorted `(key, value)` pairs — exactly the entries openMLS's own delete
    /// removed for this group, no more (a global entry such as a key package
    /// is never attributed) and no less (a missing entry would not load).
    pub entries: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Why [`examine_group_in`] could establish nothing. The two doors turn each
/// arm into their own error or log line; the reasons are the same.
pub(crate) enum ExamineFailure {
    /// The group does not load out of these bytes — unexaminable.
    DoesNotLoad,
    /// openMLS refused to delete the group from the scratch store.
    ScratchDelete(String),
    /// The delete removed no entry — attribution established nothing.
    NoEntries,
}

/// Load `raw_group_id` out of `scratch`, read its own-leaf seat, and establish
/// its entry set by **openMLS's own delete** — the attribution the adoption
/// door ([`ProviderReplica::import_group_into`]) and the launch door's carry
/// of a local-only group (`MlsEngine::restore_from_provider_storage`) share.
/// The group is gone from `scratch` afterwards, so one scratch serves several
/// groups in turn: openMLS's keys are group-scoped
/// (`every_openmls_written_key_of_a_group_embeds_its_serialised_group_id`), so
/// deleting one group never touches another's entries.
///
/// **Attribution comes from openMLS itself, never from parsing its keys.** The
/// keys the delete removed ARE the group's entries, by the one authority that
/// knows its own key layout; a key-encoding change in a dependency bump
/// therefore cannot silently mis-attribute an entry — the delete moves with
/// it.
pub(crate) fn examine_group_in(
    scratch: &OpenMlsRustCrypto,
    raw_group_id: &[u8],
) -> std::result::Result<ExaminedGroup, ExamineFailure> {
    let group_id = GroupId::from_slice(raw_group_id);
    let Ok(Some(mut group)) = MlsGroup::load(scratch.storage(), &group_id) else {
        return Err(ExamineFailure::DoesNotLoad);
    };
    let own = group.own_leaf_index();
    let seated_as = group
        .members()
        .find(|m| m.index.u32() == own.u32())
        .map(|m| m.credential.serialized_content().to_vec());

    let before: HashMap<Vec<u8>, Vec<u8>> = scratch.storage().values.read().unwrap().clone();
    group
        .delete(scratch.storage())
        .map_err(|e| ExamineFailure::ScratchDelete(format!("{e:?}")))?;
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = {
        let after = scratch.storage().values.read().unwrap();
        before
            .into_iter()
            .filter(|(k, _)| !after.contains_key(k))
            .collect()
    };
    entries.sort();
    if entries.is_empty() {
        return Err(ExamineFailure::NoEntries);
    }
    Ok(ExaminedGroup { seated_as, entries })
}

/// **The byte sequence a provider KV key contains iff it carries this group's
/// state AND openMLS itself wrote it** — openMLS's own serialisation of its own
/// `GroupId`, which its storage keys embed verbatim. Fauna's own seven
/// `fauna:`-prefixed per-channel markers (`engine.rs`) also carry a group's
/// state, keyed by the raw `ChannelId` instead — this needle does not, and
/// cannot, find those.
///
/// **Not the raw group id.** The keys embed the id *encoded* — a `GroupState…`
/// key reads `{"value":{"vec":[31,103,…]}}`, the JSON form of the `GroupId`
/// newtype — so a raw-bytes search finds nothing. This function obtains the
/// encoding by handing the `GroupId` back to the same JSON codec the storage
/// layer keys with; it never reconstructs a key layout here. The shape was
/// observed through **our own** export in
/// `every_openmls_written_key_of_a_group_embeds_its_serialised_group_id`, never
/// by reading the dependency's source — which this project forbids, and which
/// would in any case make this an assumption about openMLS's internals in place
/// of a fact about the bytes we ourselves wrote.
///
/// **Both failure directions are safe.** An id that will not serialise (`None`)
/// and a needle that matches nothing are alike "unattributed", which
/// [`merge_provider_replicas`] answers with the `min` rule. So a dependency bump
/// that changed the storage codec costs the re-walk this attribution saves and
/// reds that test — it can never carry a cursor forward over a state that is a
/// mixture, which is the direction `devices.md` Rule 2 forbids.
fn group_id_needle(raw_group_id: &[u8]) -> Option<Vec<u8>> {
    serde_json::to_vec(&GroupId::from_slice(raw_group_id)).ok()
}

/// **Which channel a provider KV key carries OPENMLS-WRITTEN group state
/// for**, by the [`group_id_needle`] found inside it. `needles` is `(channel id
/// bytes, needle)` in any order — built from the union of the merge's three
/// sides, so a group only one side holds is still attributable in that side's
/// keys.
///
/// `Some` only when **exactly one** channel's needle appears in the key: zero
/// matches is either a true global (non-group) entry — a key package, the
/// identity's signature key pair — or one of fauna's own channel-scoped
/// `fauna:`-prefixed markers (`engine.rs`), which this needle cannot see
/// (`group_id_needle`'s own doc); and two or more is ambiguous. None of the
/// three may speak for a channel's read position, so all answer `None` and
/// leave the cursor on the `min` rule.
///
/// **Why a byte match here, when [`group_sides`] asks openMLS itself.** That is the
/// sounder oracle and stays the one the classification uses — but it costs a group
/// load per group per side, which is why the classification pays it only when a
/// contested key exists. The per-channel cursor attribution has no such trigger: an
/// unadopted remainder makes **every** flush a merge, almost none of them contested,
/// so paying group loads there would put ratchet-tree deserialisation on the save
/// path to save a re-walk that is only ever paid on a restore. So this rule matches
/// bytes, and `every_openmls_written_key_of_a_group_embeds_its_serialised_group_id`
/// pins it against [`group_sides`]' answer over a real engine export.
fn channel_of_key<'a>(key: &[u8], needles: &'a [(Vec<u8>, Vec<u8>)]) -> Option<&'a Vec<u8>> {
    let mut hit: Option<&Vec<u8>> = None;
    for (channel, needle) in needles {
        if needle.is_empty() || key.len() < needle.len() {
            continue;
        }
        if key.windows(needle.len()).any(|w| w == needle.as_slice()) {
            if hit.is_some_and(|c| c != channel) {
                return None;
            }
            hit = Some(channel);
        }
    }
    hit
}

/// The exporter label under which one epoch of one group names itself for the
/// merge's classification: two sides at the same epoch export the same bytes,
/// two sides whose epochs differ — by number or by lineage — do not. Derived
/// through the public exporter, the way every other epoch-bound key in this
/// crate is (`fauna.chunk.v1`, `fauna.blob.v1`); never by reading how openMLS
/// lays its epoch out in the KV.
const EPOCH_IDENTITY_LABEL: &str = "fauna.merge.epoch-identity.v1";

/// One group as ONE SIDE of a merge holds it: the epoch number, the epoch's
/// exporter identity, and the KV entries openMLS itself attributes to the
/// group (its delete on a scratch store — the `import_group_into` technique,
/// so a dependency bump cannot silently mis-attribute an entry).
struct GroupSide {
    epoch: u64,
    identity: Vec<u8>,
    entries: HashSet<Vec<u8>>,
}

/// Every loadable group of `replica`, keyed by channel-id bytes. A group that
/// does not load, or whose epoch cannot be exported, is simply absent — the
/// caller treats a key it cannot attribute as the loud default.
fn group_sides(replica: &ProviderReplica) -> HashMap<Vec<u8>, GroupSide> {
    let scratch = scratch_over(&replica.values);
    let mut sides = HashMap::new();
    for (cid, gid) in &replica.group_ids {
        let Ok(Some(mut group)) = MlsGroup::load(scratch.storage(), &GroupId::from_slice(gid))
        else {
            continue;
        };
        let epoch = group.epoch().as_u64();
        let Ok(identity) = group.export_secret(scratch.crypto(), EPOCH_IDENTITY_LABEL, &[], 32)
        else {
            continue;
        };
        let before: HashSet<Vec<u8>> = scratch
            .storage()
            .values
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        if group.delete(scratch.storage()).is_err() {
            continue;
        }
        let entries = {
            let after = scratch.storage().values.read().unwrap();
            before
                .into_iter()
                .filter(|k| !after.contains_key(k))
                .collect()
        };
        sides.insert(
            cid.to_vec(),
            GroupSide {
                epoch,
                identity,
                entries,
            },
        );
    }
    sides
}

/// How one both-changed-differently key is resolved.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Resolution {
    /// Same-leaf progress; `mine` is the side further along — it wins, silently.
    Mine,
    /// Same-leaf progress; `theirs` is the side further along — it wins, silently.
    Theirs,
    /// Two writers, or a key the merge cannot attribute: theirs wins and the
    /// key is reported (the ruling's loud default).
    Genuine,
}

/// The classification of one channel's both-changed keys — see
/// [`merge_provider_replicas`] for the rule it applies. `None` when the
/// channel's group could not be examined on both changed sides.
fn classify_channel(
    cid: &[u8],
    base_sides: &HashMap<Vec<u8>, GroupSide>,
    mine_sides: &HashMap<Vec<u8>, GroupSide>,
    theirs_sides: &HashMap<Vec<u8>, GroupSide>,
    mine: &ProviderReplica,
    theirs: &ProviderReplica,
) -> Option<Resolution> {
    let m = mine_sides.get(cid)?;
    let t = theirs_sides.get(cid)?;
    if m.identity == t.identity {
        // One epoch, two positions on its stream: the receiver ratchets are a
        // function of the stream position, and the ingest cursor is the only
        // reading of that position either side carries — so the higher cursor
        // takes it.
        //
        // The cursor is a **lagging proxy**, not the position itself: a snapshot
        // folds its cursor in before reading the crypto values
        // (`fauna_client_mls_sync::MlsStateSync::snapshot_replica`, whose contract
        // is that the cursor "can only lag, never lead"), and two independent
        // snapshots may lag by different amounts. So the ranking can pick the side
        // that folded FEWER — measured, mine at cursor 2 having folded 2 beating
        // theirs at cursor 1 having folded 3. That is bounded and self-healing,
        // which is why the ranking stands: the discarded fold is a receiver
        // ratchet the winner has not consumed, so its message is simply
        // re-ingested, and no side is left at an epoch it cannot reach. Do not
        // rewrite this as "the cursor ranks the values" — it does not, and a
        // reader who believes it will build on a premise the save path contradicts
        // in writing. The rule itself is ratified in `devices.md`
        // § Cross-device MLS group-state sync → *A provider CAS conflict is
        // reported, not repaired*; changing it needs a ruling there, not here.
        //
        // A side carrying no cursors at all (it has polled nothing) cannot be placed —
        // loud default. Equal cursors with differing bytes is the epoch owner's own
        // send moving the sender ratchet, which a restore never authors with (the
        // takeover-before-first-send bound): either value is sound at the path,
        // theirs by the tie rule.
        //
        // ⚠ That bound is load-bearing here, so it is worth naming WHERE it is
        // enforced rather than leaving it as an assumption a reader has to take
        // on faith: `MlsStateSync::resync_provider` clears the whole `authored`
        // map whenever it restores, and a launch starts with the map empty. Both
        // doors therefore hand back a device with no send right on any channel,
        // and it must take the epoch over before its first send.
        //
        // It has not always been true of both. Until 2026-09-02 the resync door revoked authorship for only the channel
        // whose commit triggered it, while the restore it performed swapped the
        // whole provider KV — so a tie resolved here could hand a device a rewound
        // sender ratchet it still believed it owned. The sentence above was
        // sound about the launch door and wrong about the other one; it reads
        // the same today because the code changed, not the claim.
        if mine.cursors.is_empty() || theirs.cursors.is_empty() {
            return None;
        }
        let bytes: [u8; 32] = cid.try_into().ok()?;
        let channel = ChannelId(bytes);
        let (cm, ct) = (
            mine.cursor(&channel).unwrap_or(0),
            theirs.cursor(&channel).unwrap_or(0),
        );
        return Some(if cm > ct {
            Resolution::Mine
        } else {
            Resolution::Theirs
        });
    }
    match base_sides.get(cid) {
        // Exactly one side moved off the ancestor's epoch, **forward**: it folded
        // a commit the other has not met yet — the lagging-sibling shape, which
        // the lagging side closes by processing that commit (or resyncing). Both
        // moved: two commits off one ancestor, the genuine case.
        //
        // The epoch guard is not decoration. Epoch *identity* alone cannot tell a
        // side that advanced from one that went BACKWARDS — a stale snapshot the
        // nest still holds and replays satisfies "moved off the ancestor's epoch"
        // exactly as a folded commit does, and took the silent verdict, so the
        // whole merged replica became the older state with nothing recorded. A
        // regression is not a fold: the side that moved must have moved forward,
        // and an equal epoch number under a different lineage is a fork, not
        // progress. Both fall to the loud default, which is where a shape the
        // merge cannot tell from a real two-writer collision belongs.
        Some(b) => match (m.identity == b.identity, t.identity == b.identity) {
            (true, false) if t.epoch > b.epoch => Some(Resolution::Theirs),
            (false, true) if m.epoch > b.epoch => Some(Resolution::Mine),
            _ => Some(Resolution::Genuine),
        },
        // No ancestor holds the group (both sides added it): with nothing to
        // compare lineage against, a strictly later epoch number is the side
        // that advanced (a join followed by its takeover beside a bare join);
        // equal numbers with different identities are two commits off one
        // join — genuine.
        None => Some(match m.epoch.cmp(&t.epoch) {
            core::cmp::Ordering::Greater => Resolution::Mine,
            core::cmp::Ordering::Less => Resolution::Theirs,
            core::cmp::Ordering::Equal => Resolution::Genuine,
        }),
    }
}

/// The provider-key prefix under which openMLS stores a group's message-secrets
/// store (its storage label, followed by the group id).
const MESSAGE_SECRETS_KEY_PREFIX: &[u8] = b"MessageSecrets";

/// Whether two stored values for `key` hold the same MLS state.
///
/// Byte equality, except for one field. Since openMLS 0.9 the message-secrets
/// store records a wall-clock `added_at` for the current epoch and for every
/// past epoch it keeps, read only by openMLS's time-based deletion of past-epoch
/// secrets — never by decryption. Two devices that fold the same commit a few
/// milliseconds apart therefore write different bytes for the same state, and
/// without this every such pair would look like two writers. Either side's
/// timestamp is a correct one, so the merge treats values that differ only there
/// as equal and keeps whichever its ordinary rule picks. A value that does not
/// parse is compared by bytes alone (the loud side).
pub(crate) fn provider_values_equivalent(key: &[u8], a: Option<&[u8]>, b: Option<&[u8]>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) if a != b && key.starts_with(MESSAGE_SECRETS_KEY_PREFIX) => {
            match (without_added_at(a), without_added_at(b)) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            }
        }
        (a, b) => a == b,
    }
}

/// The stored JSON with every `added_at` field removed, or `None` if it does not
/// parse.
fn without_added_at(bytes: &[u8]) -> Option<serde_json::Value> {
    fn strip(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                map.remove("added_at");
                map.values_mut().for_each(strip);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    strip(&mut value);
    Some(value)
}

/// Three-way per-key merge of concurrent provider replicas.
///
/// For every key across `base`/`mine`/`theirs` (a missing key is a deletion):
/// changed on one side only → that side wins (disjoint unions are the normal
/// case — different groups touch different keys); unchanged → kept; changed on
/// both sides to the same value → kept. Changed on both sides to **different**
/// values is first **classified**, because the account's one MLS leaf lives on
/// every device and its receiver ratchet is one KV key per group — two devices
/// that folded the same stream to different points, or one that folded a
/// commit the other has not met, both change that key legitimately. The key
/// is attributed to its group by openMLS's own delete on a scratch store, and
/// the group's epoch is read on each side (`EPOCH_IDENTITY_LABEL`):
///
/// * same epoch on `mine` and `theirs` → same-leaf progress; the side whose
///   ingest cursor for the channel is higher wins (tie → theirs), silently;
/// * exactly one side still at the ancestor's epoch → the other side folded a
///   commit; it wins, silently (with no ancestor for the group, the strictly
///   later epoch number wins);
/// * both sides moved off the ancestor's epoch, equal numbers off no ancestor,
///   a side with no cursors, or a key no group claims → **theirs wins** and
///   the key is reported in [`ProviderMergeOutcome::conflicted_keys`] — the
///   two-writer case, or one the merge cannot examine, kept loud.
///
/// The ingest cursor is then resolved **per channel**, to the side whose values
/// won it: a channel whose keys were reconciled takes the winning side's, and a
/// channel only one side's keys changed at all takes that side's — in both cases
/// the merged state for the channel is wholly that side's, so its cursor indexes
/// it exactly. A channel both sides changed, or whose keys [`channel_of_key`]
/// cannot attribute, keeps the `min` rule below. `group_ids` merge by union (a
/// channel's raw group id is content-derived — `ChannelId` is a hash of the
/// GroupId — so same channel ⇒ same raw id).
pub fn merge_provider_replicas(
    base: &ProviderReplica,
    mine: &ProviderReplica,
    theirs: &ProviderReplica,
) -> ProviderMergeOutcome {
    let as_map = |r: &ProviderReplica| -> HashMap<Vec<u8>, Vec<u8>> {
        r.values
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect()
    };
    let base_m = as_map(base);
    let mine_m = as_map(mine);
    let theirs_m = as_map(theirs);

    let mut all_keys: Vec<&Vec<u8>> = base_m
        .keys()
        .chain(mine_m.keys())
        .chain(theirs_m.keys())
        .collect();
    all_keys.sort();
    all_keys.dedup();

    // Pass 1: the keys both sides changed to different values.
    let contested: Vec<&Vec<u8>> = all_keys
        .iter()
        .copied()
        .filter(|key| {
            let same = |x: Option<&Vec<u8>>, y: Option<&Vec<u8>>| {
                provider_values_equivalent(key, x.map(Vec::as_slice), y.map(Vec::as_slice))
            };
            let b = base_m.get(*key);
            let m = mine_m.get(*key);
            let t = theirs_m.get(*key);
            !same(m, b) && !same(t, b) && !same(m, t)
        })
        .collect();

    // Pass 2: classify them per channel. The scratch loads run only when there
    // is something to classify — an out-of-step flush, not every merge.
    let mut resolution: HashMap<Vec<u8>, Resolution> = HashMap::new();
    let mut reconciled_cursor: HashMap<Vec<u8>, i64> = HashMap::new();
    if !contested.is_empty() {
        let base_sides = group_sides(base);
        let mine_sides = group_sides(mine);
        let theirs_sides = group_sides(theirs);
        let mut per_channel: HashMap<Vec<u8>, Resolution> = HashMap::new();
        for key in &contested {
            let owner = mine_sides
                .iter()
                .chain(theirs_sides.iter())
                .find(|(_, side)| side.entries.contains(*key))
                .map(|(cid, _)| cid.clone());
            let Some(cid) = owner else {
                continue; // unattributable: the loud default below
            };
            let verdict = match per_channel.get(&cid) {
                Some(v) => *v,
                None => {
                    let v = classify_channel(
                        &cid,
                        &base_sides,
                        &mine_sides,
                        &theirs_sides,
                        mine,
                        theirs,
                    )
                    .unwrap_or(Resolution::Genuine);
                    per_channel.insert(cid.clone(), v);
                    v
                }
            };
            if verdict != Resolution::Genuine {
                let winner = if verdict == Resolution::Mine {
                    mine
                } else {
                    theirs
                };
                if let Ok(bytes) = <[u8; 32]>::try_from(cid.as_slice()) {
                    let seq = winner.cursor(&ChannelId(bytes)).unwrap_or(0);
                    reconciled_cursor.insert(cid.clone(), seq);
                }
            }
            resolution.insert((*key).clone(), verdict);
        }
    }

    // Pass 3's input: the needle table [`channel_of_key`] reads, built from **all
    // three** sides' group ids so a group only one side holds is still attributable
    // in that side's keys — which is exactly the residual (a sibling-only channel
    // reset to 0) the per-channel cursor attribution exists to remove.
    let mut needles: Vec<(Vec<u8>, Vec<u8>)> = base
        .group_ids
        .iter()
        .chain(mine.group_ids.iter())
        .chain(theirs.group_ids.iter())
        .filter_map(|(c, g)| Some((c.to_vec(), group_id_needle(g)?)))
        .collect();
    needles.sort();
    needles.dedup();
    // Per channel: did this side's keys for it change relative to `base`?
    let mut touched: HashMap<Vec<u8>, (bool, bool)> = HashMap::new();

    let mut values: Vec<(ByteBuf, ByteBuf)> = Vec::new();
    let mut conflicted_keys: Vec<Vec<u8>> = Vec::new();
    let mut reconciled_keys = 0usize;
    for key in all_keys {
        let b = base_m.get(key);
        let m = mine_m.get(key);
        let t = theirs_m.get(key);
        let same = |x: Option<&Vec<u8>>, y: Option<&Vec<u8>>| {
            provider_values_equivalent(key, x.map(Vec::as_slice), y.map(Vec::as_slice))
        };
        let mine_changed = !same(m, b);
        let theirs_changed = !same(t, b);
        if (mine_changed || theirs_changed)
            && let Some(channel) = channel_of_key(key, &needles)
        {
            let seen = touched.entry(channel.clone()).or_insert((false, false));
            seen.0 |= mine_changed;
            seen.1 |= theirs_changed;
        }
        let winner = match (mine_changed, theirs_changed) {
            (false, false) => b,
            (true, false) => m,
            (false, true) => t,
            (true, true) => {
                if same(m, t) {
                    t
                } else {
                    match resolution.get(key).copied().unwrap_or(Resolution::Genuine) {
                        Resolution::Mine => {
                            reconciled_keys += 1;
                            m
                        }
                        Resolution::Theirs => {
                            reconciled_keys += 1;
                            t
                        }
                        Resolution::Genuine => {
                            conflicted_keys.push(key.clone());
                            t
                        }
                    }
                }
            }
        };
        if let Some(v) = winner {
            values.push((ByteBuf::from(key.clone()), ByteBuf::from(v.clone())));
        }
    }
    // values is built in sorted key order (all_keys was sorted).

    // group_ids get the same three-way presence merge (keyed by channel id):
    // a plain union would resurrect a group one side deliberately dropped
    // (`MlsEngine::forget_group` — the folder-leave flow).
    let groups_map = |r: &ProviderReplica| -> HashMap<Vec<u8>, Vec<u8>> {
        r.group_ids
            .iter()
            .map(|(c, g)| (c.to_vec(), g.to_vec()))
            .collect()
    };
    let base_g = groups_map(base);
    let mine_g = groups_map(mine);
    let theirs_g = groups_map(theirs);
    let mut all_channels: Vec<&Vec<u8>> = base_g
        .keys()
        .chain(mine_g.keys())
        .chain(theirs_g.keys())
        .collect();
    all_channels.sort();
    all_channels.dedup();
    let mut group_ids: Vec<(ByteBuf, ByteBuf)> = Vec::new();
    for ch in all_channels {
        let b = base_g.get(ch);
        let m = mine_g.get(ch);
        let t = theirs_g.get(ch);
        // The raw group id for a channel is content-derived (ChannelId is a
        // hash of the GroupId), so only presence/absence can differ.
        let winner = match (m != b, t != b) {
            (false, false) => b,
            (true, false) => m,
            (false, true) => t,
            (true, true) => t,
        };
        if let Some(g) = winner {
            group_ids.push((ByteBuf::from(ch.clone()), ByteBuf::from(g.clone())));
        }
    }

    // pending_commit_hashes: the same three-way presence merge keyed by channel
    // id (theirs-wins on a same-channel difference, mirroring the KV / group_ids
    // rule), so a concurrent device's step-2 pending stamp is preserved rather
    // than dropped on a CAS conflict. A device clears its own channel's stamp on
    // accept-merge, so a genuine same-channel both-changed case does not arise
    // under the device-owned-epoch invariant; theirs-wins is the safe default.
    let hashes_map = |r: &ProviderReplica| -> HashMap<Vec<u8>, Vec<u8>> {
        r.pending_commit_hashes
            .iter()
            .map(|(c, h)| (c.to_vec(), h.to_vec()))
            .collect()
    };
    let base_h = hashes_map(base);
    let mine_h = hashes_map(mine);
    let theirs_h = hashes_map(theirs);
    let mut all_hash_channels: Vec<&Vec<u8>> = base_h
        .keys()
        .chain(mine_h.keys())
        .chain(theirs_h.keys())
        .collect();
    all_hash_channels.sort();
    all_hash_channels.dedup();
    let mut pending_commit_hashes: Vec<(ByteBuf, ByteBuf)> = Vec::new();
    for ch in all_hash_channels {
        let b = base_h.get(ch);
        let m = mine_h.get(ch);
        let t = theirs_h.get(ch);
        let winner = match (m != b, t != b) {
            (false, false) => b,
            (true, false) => m,
            (false, true) => t,
            (true, true) => t,
        };
        if let Some(h) = winner {
            pending_commit_hashes.push((ByteBuf::from(ch.clone()), ByteBuf::from(h.clone())));
        }
    }

    // cursors: **the cursor of the side whose values won the channel**, and `min`
    // wherever no single side did — deliberately NOT the theirs-wins rule the three
    // fields above use.
    //
    // A cursor is the read position *of a particular crypto state*, so it is sound
    // exactly when the merged state for its channel IS that side's state. Two
    // separate findings establish that, in the order the loop asks them: the
    // classification above, for a channel whose contested keys it reconciled to one
    // side, and `touched`, for a channel only one side changed at all — the ordinary
    // shape of every flush of an unadopted era, where nothing is contested. A
    // channel neither answers for keeps `min`:
    //
    // The merged cursor then has to index a merged `values` that is a *per-KV-key*
    // mixture of both sides, so neither side's cursor is known-sound for it. The two
    // errors are wildly asymmetric. A cursor **behind** the merged state's true read
    // position costs only an idempotent re-walk: already-applied commits skip as
    // `PastEpochCommit`, records whose ratchet generation the restored state already
    // consumed fail to decrypt and are skipped (`poll_inbound_conv`'s Application
    // arm), and anything re-folded dedups by `message_id` — and no message is lost,
    // because `history/*` union-merges independently. A cursor **ahead** of it skips
    // a commit that can never be re-applied and strands the device at a dead epoch
    // forever. So round toward the safe side.
    //
    // Contribution rules, both about *absence*:
    // * A side whose `cursors` is entirely empty is a device that has polled
    //   nothing. It carries no cursor information, so it contributes nothing
    //   rather than dragging the other side to 0. Safe: a device with no cursor entries has advanced no
    //   group's KV by polling either, so the other side's KV — and hence its cursor —
    //   wins the merge for every group it touched.
    // * A channel absent from a side that *does* carry cursors means that device has
    //   read position 0 there, so it contributes 0 and the merged cursor is 0 — the
    //   safe direction (a full, idempotent re-walk).
    //
    // `base` is deliberately excluded: cursors advance monotonically from the loaded
    // base on each device, so `min(mine, theirs) >= base` always, and folding `base`
    // in would rewind every CAS conflict to the common ancestor for no safety gain.
    let cursors_map = |r: &ProviderReplica| -> Option<HashMap<Vec<u8>, i64>> {
        (!r.cursors.is_empty()).then(|| r.cursors.iter().map(|(c, s)| (c.to_vec(), *s)).collect())
    };
    let mine_c = cursors_map(mine);
    let theirs_c = cursors_map(theirs);
    let mut all_cursor_channels: Vec<&Vec<u8>> = mine_c
        .iter()
        .chain(theirs_c.iter())
        .flat_map(|m| m.keys())
        .collect();
    all_cursor_channels.sort();
    all_cursor_channels.dedup();
    let mut cursors: Vec<(ByteBuf, i64)> = Vec::new();
    for ch in all_cursor_channels {
        // A channel whose contested keys were reconciled holds exactly one side's
        // group state, so that side's cursor indexes it — no rounding needed.
        if let Some(seq) = reconciled_cursor.get(ch) {
            cursors.push((ByteBuf::from(ch.clone()), *seq));
            continue;
        }
        // The same soundness, reached without a contest: if only ONE side's keys
        // for the channel changed at all, every key of it the merge kept is that
        // side's — the other side's equal `base` and so lose or stay put — the
        // merged state for the channel IS that side's state, and its cursor
        // indexes it exactly. Both sides changed it, or no key of it is
        // attributable, and nothing established a winner: `min` below.
        let attributed = match touched.get(ch) {
            Some((true, false)) => mine_c.as_ref(),
            Some((false, true)) => theirs_c.as_ref(),
            _ => None,
        };
        if let Some(seq) = attributed.and_then(|side| side.get(ch).copied()) {
            cursors.push((ByteBuf::from(ch.clone()), seq));
            continue;
        }
        // `.map(|m| m.get(ch).copied().unwrap_or(0))`: a cursor-carrying side that
        // omits this channel has read position 0 for it.
        let contributions = [&mine_c, &theirs_c]
            .into_iter()
            .flatten()
            .map(|m| m.get(ch).copied().unwrap_or(0));
        if let Some(seq) = contributions.min() {
            cursors.push((ByteBuf::from(ch.clone()), seq));
        }
    }

    ProviderMergeOutcome {
        merged: ProviderReplica {
            values,
            group_ids,
            pending_commit_hashes,
            cursors,
        },
        conflicted_keys,
        reconciled_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChannelMessage, ChannelMessageBody};
    use fauna_core::crypto::{BackupKey, decrypt_backup_chunk, encrypt_backup_chunk};
    use fauna_core::data::Timestamp;
    use fauna_core::identity::ActorKeypair;
    use std::collections::BTreeSet;

    /// Number of keys whose values hold different MLS state on the two sides
    /// ([`provider_values_equivalent`]) — byte equality but for openMLS's
    /// wall-clock `added_at`. Panics if the key sets differ.
    fn differing_state(a: &[(ByteBuf, ByteBuf)], b: &[(ByteBuf, ByteBuf)]) -> usize {
        let a_keys: Vec<&ByteBuf> = a.iter().map(|(k, _)| k).collect();
        let b_keys: Vec<&ByteBuf> = b.iter().map(|(k, _)| k).collect();
        assert_eq!(a_keys, b_keys, "the same set of provider keys");
        a.iter()
            .zip(b)
            .filter(|((k, va), (_, vb))| !provider_values_equivalent(k, Some(va), Some(vb)))
            .count()
    }

    #[test]
    fn message_secrets_differing_only_in_added_at_are_one_state() {
        let key = b"MessageSecrets{\"value\":[1,2]}";
        let at = |nanos: u32, epoch_secret: u8| {
            format!(
                "{{\"max_epochs\":1,\"past_epoch_trees\":[{{\"epoch\":3,\"message_secrets\":\
                 {{\"secret\":[{epoch_secret}],\"added_at\":{{\"secs_since_epoch\":5,\
                 \"nanos_since_epoch\":{nanos}}}}}}}],\"message_secrets\":{{\"secret\":[9],\
                 \"added_at\":{{\"secs_since_epoch\":6,\"nanos_since_epoch\":{nanos}}}}}}}"
            )
            .into_bytes()
        };
        let (a, b, other) = (at(1, 7), at(2, 7), at(1, 8));
        assert!(provider_values_equivalent(key, Some(&a), Some(&b)));
        assert!(!provider_values_equivalent(key, Some(&a), Some(&other)));
        assert!(!provider_values_equivalent(key, Some(&a), None));
        // Only the message-secrets store gets the exception.
        assert!(!provider_values_equivalent(
            b"EpochKeyPairs{}",
            Some(&a),
            Some(&b)
        ));
        // Unparseable values fall back to bytes.
        assert!(!provider_values_equivalent(key, Some(b"x1"), Some(b"x2")));

        // The merge reads both sides as one state: no contest, nothing reported.
        let base = kv(&[(key, &at(0, 6))], &[]);
        let mine = kv(&[(key, &a)], &[]);
        let theirs = kv(&[(key, &b)], &[]);
        let outcome = merge_provider_replicas(&base, &mine, &theirs);
        assert!(outcome.conflicted_keys.is_empty());
        assert_eq!(outcome.reconciled_keys, 0);
    }

    fn kv(replica_pairs: &[(&[u8], &[u8])], groups: &[(&[u8], &[u8])]) -> ProviderReplica {
        let mut values: Vec<(ByteBuf, ByteBuf)> = replica_pairs
            .iter()
            .map(|(k, v)| (ByteBuf::from(k.to_vec()), ByteBuf::from(v.to_vec())))
            .collect();
        values.sort();
        let mut group_ids: Vec<(ByteBuf, ByteBuf)> = groups
            .iter()
            .map(|(c, g)| (ByteBuf::from(c.to_vec()), ByteBuf::from(g.to_vec())))
            .collect();
        group_ids.sort();
        ProviderReplica {
            values,
            group_ids,
            pending_commit_hashes: Vec::new(),
            cursors: Vec::new(),
        }
    }

    fn get<'a>(r: &'a ProviderReplica, k: &[u8]) -> Option<&'a [u8]> {
        r.values
            .iter()
            .find(|(key, _)| key.as_slice() == k)
            .map(|(_, v)| v.as_slice())
    }

    /// The slice-1 flow assertion:
    /// device A's engine state → replica bytes → sealed under `BackupKey` →
    /// fresh engine (same identity) restores → decrypts a *subsequent*
    /// third-member message. (Pre-snapshot traffic was already consumed by A —
    /// receiver ratchets only move forward; history rides the
    /// `history/<channel_hex>` slice in `fauna-conversations`, not this blob.)
    #[test]
    fn replica_seal_round_trip_restores_group_for_future_traffic() {
        let alice_secret = ActorKeypair::generate().signing_key().to_bytes();
        let alice = MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        // Pre-snapshot traffic: bob → alice, alice consumes it.
        let pre = bob
            .encrypt(
                &channel_id,
                &ChannelMessage {
                    sender: bob.identity_actor_id(),
                    sequence: 1,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("before snapshot".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        alice.decrypt(&channel_id, &pre).unwrap();

        // Snapshot alice → sealed bytes (the whole __mls `provider` blob path).
        let replica = ProviderReplica::from_engine(&alice);
        let key = BackupKey::derive(&alice_secret);
        let sealed = encrypt_backup_chunk(&key, &replica.to_bytes().unwrap()).unwrap();

        // A "second device": fresh in-memory engine, same identity, restores.
        let unsealed = decrypt_backup_chunk(&key, &sealed).unwrap();
        let restored = ProviderReplica::from_bytes(&unsealed).unwrap();
        let alice2 = MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).unwrap();
        restored.restore_into_unchecked(&alice2).unwrap();

        assert!(alice2.has_group(&channel_id), "restored group must load");
        assert_eq!(restored.channel_ids(), vec![channel_id]);

        // Post-snapshot traffic decrypts on the second device.
        let post = bob
            .encrypt(
                &channel_id,
                &ChannelMessage {
                    sender: bob.identity_actor_id(),
                    sequence: 2,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("after snapshot".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        let got = alice2.decrypt(&channel_id, &post).unwrap();
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "after snapshot"));
    }

    /// Two "second devices" of one identity and a group only one of them
    /// joined — the fixture every targeted-import test below starts from.
    /// Returns `(the sibling that holds the group, the peer in it, the channel,
    /// this device — same identity, holding an unrelated group of its own)`.
    fn sibling_holds_a_group_this_device_lacks() -> (MlsEngine, MlsEngine, ChannelId, MlsEngine) {
        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let sibling = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let (channel, welcome) = sibling.create_group(&peer_kps).unwrap();
        peer.join_from_welcome(welcome).unwrap();

        let me = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let other = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let other_kps = other.generate_key_packages(1).unwrap();
        let _ = me.create_group(&other_kps).unwrap();
        (sibling, peer, channel, me)
    }

    fn text(engine: &MlsEngine, channel: &ChannelId, sequence: u64, body: &str) -> Vec<u8> {
        engine
            .encrypt(
                channel,
                &ChannelMessage {
                    sender: engine.identity_actor_id(),
                    sequence,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text(body.into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap()
    }

    /// **The targeted import adopts a never-seen group and touches nothing
    /// else.** The sibling's snapshot lists a group this engine never joined;
    /// after the import this engine decrypts the peer's next message on it,
    /// every entry it held before is byte-identical after, and the set of new
    /// entries is exactly what the import reports — the entries openMLS's own
    /// delete attributes to the group, no more (a global entry such as a key
    /// package would be a splice) and no less (a missing entry would not load).
    #[test]
    fn a_never_seen_group_is_adopted_from_a_siblings_snapshot_and_nothing_else_moves() {
        let (sibling, peer, channel, me) = sibling_holds_a_group_this_device_lacks();
        let snapshot = ProviderReplica::from_engine(&sibling);
        let before = me.export_provider_storage();
        let my_own_groups = me.list_groups();
        assert!(!me.has_group(&channel));

        let adopted = snapshot.import_group_into(&me, &channel).unwrap();

        assert!(me.has_group(&channel), "the group is live in this engine");
        let mut groups_now = me.list_groups();
        groups_now.retain(|c| *c != channel);
        assert_eq!(groups_now, my_own_groups, "this engine's own groups stand");

        let after = me.export_provider_storage();
        for (k, v) in &before {
            assert_eq!(
                after.get(k),
                Some(v),
                "an entry this engine held is untouched"
            );
        }
        let mut added: Vec<(Vec<u8>, Vec<u8>)> = after
            .into_iter()
            .filter(|(k, _)| !before.contains_key(k))
            .collect();
        added.sort();
        assert_eq!(
            added, adopted.entries,
            "exactly the attributed entries were inserted"
        );
        assert!(!adopted.entries.is_empty());
        assert_eq!(adopted.channel, channel);

        let post = text(&peer, &channel, 7, "after the import");
        let got = me.decrypt(&channel, &post).unwrap();
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "after the import"));
    }

    /// The door is for a group the engine has never seen: a held group is
    /// refused before anything is read, and the engine is untouched.
    #[test]
    fn importing_a_group_the_engine_already_holds_is_refused() {
        let (sibling, _peer, channel, me) = sibling_holds_a_group_this_device_lacks();
        let snapshot = ProviderReplica::from_engine(&sibling);
        snapshot.import_group_into(&me, &channel).unwrap();
        let before = me.export_provider_storage();

        let err = snapshot.import_group_into(&me, &channel).unwrap_err();
        assert!(matches!(err, MlsError::PolicyViolation(_)), "{err}");
        assert_eq!(me.export_provider_storage(), before, "nothing moved");
    }

    /// Rule (1) at this door: a snapshot captured by ANOTHER identity's engine
    /// seats the group under that identity's leaf, and importing it would seat
    /// this engine as that identity for the group — refused, nothing inserted.
    #[test]
    fn a_group_seated_under_another_identitys_leaf_is_never_imported() {
        let (_sibling, peer, channel, me) = sibling_holds_a_group_this_device_lacks();
        // The PEER's snapshot of the same group: seated as the peer, not as me.
        let foreign = ProviderReplica::from_engine(&peer);
        let before = me.export_provider_storage();

        let err = foreign.import_group_into(&me, &channel).unwrap_err();
        assert!(matches!(err, MlsError::PolicyViolation(_)), "{err}");
        assert!(!me.has_group(&channel));
        assert_eq!(me.export_provider_storage(), before, "nothing moved");
    }

    /// A channel the snapshot does not list is a plain not-found, not an
    /// unexaminable snapshot.
    #[test]
    fn importing_a_channel_the_snapshot_does_not_list_is_not_found() {
        let (sibling, _peer, _channel, me) = sibling_holds_a_group_this_device_lacks();
        let snapshot = ProviderReplica::from_engine(&sibling);
        let err = snapshot
            .import_group_into(&me, &ChannelId([9u8; 32]))
            .unwrap_err();
        assert!(matches!(err, MlsError::ChannelNotFound(_)), "{err}");
    }

    /// An identity, a group it was evicted from, and its snapshot after the
    /// eviction landed — the fixture the blank-seat refusals below start from.
    /// Returns `(the evicted engine, its identity secret, the channel)`: the
    /// evicted engine still lists the group (nothing prunes the entry), and
    /// the group loads out of its snapshot with an own leaf that names nobody.
    fn bob_was_evicted_and_still_lists_the_group() -> (MlsEngine, [u8; 32], ChannelId) {
        use crate::channel::GroupChannel;

        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let owner = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (group, welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();
        GroupChannel::join(&bob, welcome).unwrap();
        let channel = group.channel_id;
        // Leaf 1 — bob joined second (same literal `engine.rs`'s own
        // `add_and_remove_member` uses).
        let commit = owner.remove_member(&channel, 1).unwrap();
        bob.process_commit(&channel, &commit).unwrap();
        assert!(
            bob.has_group(&channel),
            "the evicted engine still lists the group"
        );
        (bob, secret, channel)
    }

    /// **The ordinary, no-attacker consequence of a blank seat: this identity's
    /// OTHER device does not adopt a group this identity was evicted from.**
    /// The sibling's snapshot lists it (nothing prunes an evicted group's
    /// entry), this device never held it, and the door refuses it as
    /// [`MlsError::NotSeated`] — not as a policy violation, because it is a
    /// state every removed member's snapshot carries forever, met again on
    /// every sibling flush. The group stays unadopted, the engine is untouched.
    /// Without the refusal the device adopted the group, bound a thread for
    /// it, folded it into its merge baseline and re-published the evicted group
    /// into the account replica for every later restore to pick up.
    #[test]
    fn an_evicted_group_stays_unadopted_on_this_identitys_other_device() {
        let (bob, secret, channel) = bob_was_evicted_and_still_lists_the_group();
        let snapshot = ProviderReplica::from_engine(&bob);
        let other_device = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        assert!(!other_device.has_group(&channel));
        let before = other_device.export_provider_storage();

        let err = snapshot
            .import_group_into(&other_device, &channel)
            .unwrap_err();

        assert!(
            matches!(err, MlsError::NotSeated(_)),
            "a blank seat is its own refusal, not a policy violation: {err}"
        );
        assert!(
            !other_device.has_group(&channel),
            "the evicted group stays unadopted"
        );
        assert_eq!(
            other_device.export_provider_storage(),
            before,
            "nothing moved"
        );
    }

    /// **Rule (1) at this door with the seat blank: a FOREIGN identity cannot
    /// import a group out of another identity's snapshot merely because that
    /// snapshot's own leaf names nobody.** The guard used to compare the seated
    /// credential only when there was one, so `None` fell straight through to
    /// the import and a stranger's engine came to hold a group it never
    /// joined. The identity question is answered positively or the import
    /// does not happen.
    #[test]
    fn a_foreign_identity_cannot_import_a_blank_seat_group_out_of_anothers_snapshot() {
        let (bob, _secret, channel) = bob_was_evicted_and_still_lists_the_group();
        let snapshot = ProviderReplica::from_engine(&bob);
        let stranger = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let before = stranger.export_provider_storage();

        let err = snapshot.import_group_into(&stranger, &channel).unwrap_err();

        assert!(matches!(err, MlsError::NotSeated(_)), "{err}");
        assert!(
            !stranger.has_group(&channel),
            "RULE (1) BYPASSED: a foreign identity imported a group out of another \
             identity's snapshot because the snapshot's own leaf seats nobody"
        );
        assert_eq!(stranger.export_provider_storage(), before, "nothing moved");
    }

    /// **The two doors agree on one predecessor-shaped snapshot.** Bob's
    /// snapshot holds two groups — A, bob evicted (blank seat), and B, bob
    /// still seated — which is what a real predecessor's snapshot looks like.
    /// A stranger's engine meets it at both doors: the launch door's verdict
    /// is not clean (B is foreign), `restore_into` swaps nothing, and the
    /// per-group door refuses B as foreign **and A as unseated**. Before the
    /// fix the per-group door imported A into the stranger's live engine while
    /// the launch door refused the very same bytes — the aggregate refusal was
    /// the only thing protecting the blank-seat group, and the per-group door
    /// had stripped it.
    #[test]
    fn the_launch_door_and_the_adoption_door_agree_on_a_predecessor_shaped_snapshot() {
        use crate::channel::GroupChannel;

        let (bob, _secret, a) = bob_was_evicted_and_still_lists_the_group();
        let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (group_b, welcome) = GroupChannel::create(&carol, &bob_kps).unwrap();
        GroupChannel::join(&bob, welcome).unwrap();
        let b = group_b.channel_id;
        let snapshot = ProviderReplica::from_engine(&bob);
        assert_eq!(
            snapshot.channel_ids().len(),
            2,
            "A (evicted) and B (seated)"
        );

        let stranger = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let before = stranger.export_provider_storage();

        // The launch door: not clean, nothing swapped.
        let verdict = snapshot.restore_into(&stranger).unwrap();
        assert!(!verdict.is_clean(), "{verdict:?}");
        assert_eq!(verdict.unexaminable, 0, "both groups load: {verdict:?}");
        assert_eq!(
            verdict.foreign,
            vec![b],
            "B is positively another identity's"
        );
        assert!(
            !stranger.has_group(&a) && !stranger.has_group(&b),
            "nothing restored"
        );

        // The per-group door: B foreign, A unseated — both refused.
        let err_b = snapshot.import_group_into(&stranger, &b).unwrap_err();
        assert!(matches!(err_b, MlsError::PolicyViolation(_)), "{err_b}");
        let err_a = snapshot.import_group_into(&stranger, &a).unwrap_err();
        assert!(
            matches!(err_a, MlsError::NotSeated(_)),
            "DOORS DISAGREE: the launch restore refused this whole snapshot as another \
             identity's, but the per-group door let its blank-seat group through: {err_a}"
        );
        assert!(
            !stranger.has_group(&a),
            "the blank-seat group was not imported"
        );
        assert_eq!(
            stranger.export_provider_storage(),
            before,
            "nothing moved at either door"
        );
    }

    /// **A group the engine holds but the snapshot does not list survives the
    /// swap — and the relaunch after it** (`devices.md` § Cross-device MLS
    /// group-state sync → *A group the engine holds but the snapshot does not
    /// list survives the swap*).
    ///
    /// The shape: a Welcome join lands in the native store (`persist_group`)
    /// and the process quits inside the autosave debounce, so the replica at
    /// rest never lists the group. The next launch reloads it from SQLite, then
    /// the launch door restores the snapshot sealed before the join. The
    /// whole-KV swap used to wipe the group's entries while the insert-only
    /// group loop kept it LISTED — so the next snapshot named a group with no
    /// state, `retire()`'s flush wrote that KV back over the local copy too,
    /// and one launch later `MlsGroup::load` answered `None`: the join was
    /// gone from every store the account has, and a spent init key has no
    /// re-Welcome. The door both callers share (`restore_into` — the launch's
    /// and the own-leaf resync's) now re-adopts the engine's own local-only
    /// groups through the per-group carve-out, so this pins both doors.
    #[cfg(feature = "native")]
    #[test]
    fn a_group_the_snapshot_does_not_list_survives_the_swap_and_the_next_relaunch() {
        use tempfile::NamedTempFile;

        let db = NamedTempFile::new().unwrap();
        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let other = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();

        // Launch 1: an own group flushed to the replica; then, inside the
        // debounce, a Welcome join that reaches the native store only.
        let (snapshot_before_join, own, channel) = {
            let me = MlsEngine::new(ActorKeypair::from_secret(secret), db.path()).unwrap();
            let other_kps = other.generate_key_packages(1).unwrap();
            let (own, _welcome) = me.create_group(&other_kps).unwrap();
            let snapshot = ProviderReplica::from_engine(&me);
            let my_kps = me.generate_key_packages(1).unwrap();
            let (channel, welcome) = peer.create_group(&my_kps).unwrap();
            me.join_from_welcome(welcome).unwrap();
            me.retire();
            (snapshot, own, channel)
        };
        assert!(
            !snapshot_before_join.channel_ids().contains(&channel),
            "fixture: the replica at rest never listed the join"
        );

        // Launch 2: SQLite reloads both groups; the launch door restores the
        // snapshot sealed before the join.
        let snapshot_after_swap = {
            let me = MlsEngine::new(ActorKeypair::from_secret(secret), db.path()).unwrap();
            assert!(
                me.has_group(&own) && me.has_group(&channel),
                "fixture: the native store reloaded both groups"
            );
            let verdict = snapshot_before_join.restore_into(&me).unwrap();
            assert!(
                verdict.is_clean(),
                "fixture: this identity's own snapshot: {verdict:?}"
            );
            assert!(me.has_group(&own), "the snapshot's own group loaded");
            assert!(
                me.has_group(&channel),
                "the local-only group survives the swap"
            );
            let got = me
                .decrypt(&channel, &text(&peer, &channel, 1, "after the swap"))
                .expect("the peer's next message decrypts on the surviving group");
            assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "after the swap"));
            let snapshot = ProviderReplica::from_engine(&me);
            assert!(
                snapshot.channel_ids().contains(&channel),
                "the next snapshot lists the group"
            );
            me.retire();
            snapshot
        };

        // Launch 3: the relaunch from the snapshot sealed after the swap —
        // where the pre-fix shape lost the group for good.
        let me = MlsEngine::new(ActorKeypair::from_secret(secret), db.path()).unwrap();
        let verdict = snapshot_after_swap.restore_into(&me).unwrap();
        assert!(
            verdict.is_clean(),
            "the snapshot sealed after the swap must carry the entries of every group it \
             lists — a listed group with no state is unexaminable: {verdict:?}"
        );
        assert!(
            me.has_group(&channel),
            "one launch later the Welcome-joined group is still held"
        );
        let got = me
            .decrypt(&channel, &text(&peer, &channel, 2, "one launch later"))
            .expect("the peer's next message decrypts one launch later");
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "one launch later"));
    }

    /// **A group a sibling device deleted from the listing is dropped at this
    /// device's next launch — and a join since its last flush is still
    /// carried** (`devices.md` § Cross-device MLS group-state sync → *A group
    /// the engine holds but the snapshot does not list survives the swap* → *A
    /// sibling's deletion is not a join*). The launch door runs in a fresh
    /// process, so the ancestor the swap asks against has to survive the
    /// relaunch: a native engine over a temp db records the listing its landed
    /// flush named, quits, and relaunches from SQLite into a snapshot a
    /// sibling wrote after leaving `left`.
    #[cfg(feature = "native")]
    #[test]
    fn a_group_a_sibling_deleted_is_dropped_at_the_next_launch_but_a_join_is_carried() {
        use tempfile::NamedTempFile;

        let db = NamedTempFile::new().unwrap();
        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();

        // Launch 1: a group whose flush landed (recorded), then a join inside
        // the debounce (never flushed, never recorded).
        let (left, joined) = {
            let me = MlsEngine::new(ActorKeypair::from_secret(secret), db.path()).unwrap();
            let (left, _welcome) = me
                .create_group(&peer.generate_key_packages(1).unwrap())
                .unwrap();
            me.note_replica_listed(&ProviderReplica::from_engine(&me).channel_ids());
            let (joined, welcome) = peer
                .create_group(&me.generate_key_packages(1).unwrap())
                .unwrap();
            me.join_from_welcome(welcome).unwrap();
            me.retire();
            (left, joined)
        };

        // The sibling's snapshot after its leave: this identity's own, listing
        // neither group.
        let sibling = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let (kept, _welcome) = sibling
            .create_group(&peer.generate_key_packages(1).unwrap())
            .unwrap();
        let after_leave = ProviderReplica::from_engine(&sibling);

        // Launch 2: SQLite reloads both; the launch door restores the snapshot.
        {
            let me = MlsEngine::new(ActorKeypair::from_secret(secret), db.path()).unwrap();
            assert!(
                me.has_group(&left) && me.has_group(&joined),
                "fixture: the native store reloaded both groups"
            );
            assert!(after_leave.restore_into(&me).unwrap().is_clean());
            assert!(me.has_group(&kept), "the snapshot's own group loaded");
            assert!(
                !me.has_group(&left),
                "a group the recorded listing named and the snapshot does not was deleted \
                 by a sibling — not carried"
            );
            assert!(
                me.has_group(&joined),
                "a group joined since the last landed flush is still carried"
            );
            assert!(
                !ProviderReplica::from_engine(&me)
                    .channel_ids()
                    .contains(&left),
                "the next snapshot does not list the left group again"
            );
            me.retire();
        }

        // Launch 3: the dropped group's native row went with it.
        let me = MlsEngine::new(ActorKeypair::from_secret(secret), db.path()).unwrap();
        assert!(
            !me.has_group(&left),
            "the swap swept the left group's active_groups row"
        );
    }

    /// **A local-only group whose own leaf seats nobody is dropped by the
    /// swap — and no longer LISTED.** The per-group rule of the adoption door,
    /// asked of the engine's own state: a group this identity was evicted from
    /// inside the debounce is not re-adopted (the end state an eviction means),
    /// and unlike the pre-fix insert-only loop the map does not keep naming a
    /// group the KV holds no byte of.
    #[test]
    fn a_local_only_group_that_does_not_seat_this_identity_is_dropped_not_listed() {
        let (bob, secret, evicted) = bob_was_evicted_and_still_lists_the_group();
        // This identity's snapshot from a sibling that never held the group.
        let sibling = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let other = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let other_kps = other.generate_key_packages(1).unwrap();
        let (own, _welcome) = sibling.create_group(&other_kps).unwrap();
        let snapshot = ProviderReplica::from_engine(&sibling);

        let verdict = snapshot.restore_into(&bob).unwrap();
        assert!(verdict.is_clean(), "{verdict:?}");
        assert!(bob.has_group(&own), "the snapshot's group loaded");
        assert!(
            !bob.has_group(&evicted),
            "a local-only group whose own leaf seats nobody is not re-adopted — and is no \
             longer listed either: the map never names a group with no state"
        );
        assert_eq!(
            ProviderReplica::from_engine(&bob).channel_ids(),
            vec![own],
            "the next snapshot lists exactly what the engine holds state for"
        );
    }

    /// **A local-only group seated under ANOTHER identity's leaf is dropped by
    /// the swap** — rule (1) asked of the engine's own state, positively, as
    /// the adoption door asks it. Reachable only through the test-only
    /// unchecked swap (a predecessor's snapshot forced into a successor's
    /// engine); a clean restore of the successor's own snapshot then leaves
    /// none of the predecessor's groups behind.
    #[test]
    fn a_local_only_group_seated_under_another_identity_is_dropped_by_the_swap() {
        let predecessor = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let (foreign, _welcome) = predecessor.create_group(&peer_kps).unwrap();

        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let successor = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        ProviderReplica::from_engine(&predecessor)
            .restore_into_unchecked(&successor)
            .unwrap();
        assert!(
            successor.has_group(&foreign),
            "fixture: the predecessor's group was forced in"
        );

        let sibling = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let other = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let other_kps = other.generate_key_packages(1).unwrap();
        let (own, _welcome) = sibling.create_group(&other_kps).unwrap();
        let verdict = ProviderReplica::from_engine(&sibling)
            .restore_into(&successor)
            .unwrap();
        assert!(verdict.is_clean(), "{verdict:?}");
        assert!(successor.has_group(&own));
        assert!(
            !successor.has_group(&foreign),
            "a local-only group seated under another identity's leaf is never re-adopted"
        );
        assert_eq!(
            ProviderReplica::from_engine(&successor).channel_ids(),
            vec![own]
        );
    }

    /// The baseline half: absorbing the adopted group into a replica that was
    /// this engine's last export yields exactly the export the engine now
    /// produces — so the next three-way merge sees the adopted group as
    /// already accounted for, not as a concurrent addition.
    #[test]
    fn absorbing_the_adopted_group_makes_the_baseline_equal_the_engines_next_export() {
        let (sibling, _peer, channel, me) = sibling_holds_a_group_this_device_lacks();
        let mut baseline = ProviderReplica::from_engine(&me);
        let snapshot = ProviderReplica::from_engine(&sibling).with_cursors(&[(channel, 41)]);

        let adopted = snapshot.import_group_into(&me, &channel).unwrap();
        baseline.absorb_adopted_group(&adopted);

        let export_now = ProviderReplica::from_engine(&me).with_cursors(&[(channel, 41)]);
        assert_eq!(baseline.to_bytes().unwrap(), export_now.to_bytes().unwrap());
        assert_eq!(baseline.cursor(&channel), Some(41));
    }

    /// Equal logical state ⇒ equal bytes (HashMap iteration order must not
    /// leak into the encoding), and decode∘encode is identity.
    #[test]
    fn replica_bytes_are_stable_and_round_trip() {
        let alice = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let _ = alice.create_group(&bob_kps).unwrap();

        let a = ProviderReplica::from_engine(&alice).to_bytes().unwrap();
        let b = ProviderReplica::from_engine(&alice).to_bytes().unwrap();
        assert_eq!(a, b, "two exports of the same state must be byte-equal");

        let decoded = ProviderReplica::from_bytes(&a).unwrap();
        assert_eq!(decoded.to_bytes().unwrap(), a);
    }

    #[test]
    fn merge_disjoint_changes_union() {
        let base = kv(&[(b"k1", b"v1")], &[(b"c1", b"g1")]);
        let mine = kv(&[(b"k1", b"v1"), (b"k2", b"m2")], &[(b"c1", b"g1")]);
        let theirs = kv(
            &[(b"k1", b"v1"), (b"k3", b"t3")],
            &[(b"c1", b"g1"), (b"c2", b"g2")],
        );
        let out = merge_provider_replicas(&base, &mine, &theirs);
        assert!(out.conflicted_keys.is_empty());
        assert_eq!(get(&out.merged, b"k1"), Some(b"v1".as_slice()));
        assert_eq!(get(&out.merged, b"k2"), Some(b"m2".as_slice()));
        assert_eq!(get(&out.merged, b"k3"), Some(b"t3".as_slice()));
        assert_eq!(out.merged.group_ids.len(), 2);
    }

    #[test]
    fn merge_one_side_changed_wins_including_deletion() {
        let base = kv(&[(b"k1", b"v1"), (b"k2", b"v2")], &[]);
        // mine deletes k1; theirs rewrites k2.
        let mine = kv(&[(b"k2", b"v2")], &[]);
        let theirs = kv(&[(b"k1", b"v1"), (b"k2", b"t2")], &[]);
        let out = merge_provider_replicas(&base, &mine, &theirs);
        assert!(out.conflicted_keys.is_empty());
        assert_eq!(get(&out.merged, b"k1"), None, "my deletion propagates");
        assert_eq!(get(&out.merged, b"k2"), Some(b"t2".as_slice()));
    }

    #[test]
    fn merge_both_changed_same_value_is_not_a_conflict() {
        let base = kv(&[(b"k", b"old")], &[]);
        let mine = kv(&[(b"k", b"new")], &[]);
        let theirs = kv(&[(b"k", b"new")], &[]);
        let out = merge_provider_replicas(&base, &mine, &theirs);
        assert!(out.conflicted_keys.is_empty());
        assert_eq!(get(&out.merged, b"k"), Some(b"new".as_slice()));
    }

    #[test]
    fn merge_both_changed_differently_theirs_wins_and_reports() {
        let base = kv(&[(b"k", b"old")], &[]);
        let mine = kv(&[(b"k", b"mine")], &[]);
        let theirs = kv(&[(b"k", b"theirs")], &[]);
        let out = merge_provider_replicas(&base, &mine, &theirs);
        assert_eq!(out.conflicted_keys, vec![b"k".to_vec()]);
        assert_eq!(get(&out.merged, b"k"), Some(b"theirs".as_slice()));
    }

    /// Item 1 compat gate (the at-rest-data-is-production concern): a `provider`
    /// blob sealed **before** `pending_commit_hashes` existed must still decode —
    /// else an upgrading device's launch `load()` fails, the save gate never
    /// lifts, and the user's whole replica plane breaks. The struct is a CBOR map
    /// keyed by field name (the codec's `serde(flatten)` support proves it), so
    /// `#[serde(default)]` lets the missing key decode to an empty vec. The
    /// fixture is the exact pre-field shape, encoded through the same canonical
    /// codec `to_bytes` uses.
    #[test]
    fn replica_decodes_legacy_bytes_without_pending_hashes_field() {
        #[derive(Serialize)]
        struct LegacyProviderReplica {
            values: Vec<(ByteBuf, ByteBuf)>,
            group_ids: Vec<(ByteBuf, ByteBuf)>,
        }
        let legacy = LegacyProviderReplica {
            values: vec![(ByteBuf::from(b"k1".to_vec()), ByteBuf::from(b"v1".to_vec()))],
            group_ids: vec![(
                ByteBuf::from([9u8; 32].to_vec()),
                ByteBuf::from(b"gid".to_vec()),
            )],
        };
        let legacy_bytes = fauna_cbor::encode_canonical(&legacy).unwrap();

        let decoded = ProviderReplica::from_bytes(&legacy_bytes)
            .expect("field-less provider bytes must still decode");
        assert_eq!(get(&decoded, b"k1"), Some(b"v1".as_slice()));
        assert_eq!(
            decoded.pending_commit_hash(&ChannelId([9u8; 32])),
            None,
            "an absent field decodes as no pending-commit identity (never merged by the resync arm)"
        );
        // And it re-encodes cleanly (now with the empty field) and round-trips
        // (ProviderReplica has no Debug — it holds group secrets — so compare
        // bytes, as the other round-trip tests do).
        let re = decoded.to_bytes().unwrap();
        assert_eq!(
            ProviderReplica::from_bytes(&re)
                .unwrap()
                .to_bytes()
                .unwrap(),
            re
        );
    }

    /// `with_pending_hashes` stamps a channel's commit identity, `pending_commit_hash`
    /// reads it back, and the stamp survives the canonical seal round-trip.
    #[test]
    fn pending_hash_stamp_round_trips() {
        let ch = ChannelId([3u8; 32]);
        let h = [7u8; 32];
        let r = kv(&[(b"k", b"v")], &[]).with_pending_hashes(&[(ch, h)]);
        assert_eq!(r.pending_commit_hash(&ch), Some(h));
        assert_eq!(r.pending_commit_hash(&ChannelId([4u8; 32])), None);
        let back = ProviderReplica::from_bytes(&r.to_bytes().unwrap()).unwrap();
        assert_eq!(back.pending_commit_hash(&ch), Some(h));
    }

    /// The CAS conflict merge unions the new field: a concurrent device's
    /// step-2 pending stamp is preserved (disjoint channels union; a same-channel
    /// difference takes theirs, mirroring the KV / group_ids rule).
    #[test]
    fn merge_unions_pending_commit_hashes() {
        let ch_mine = ChannelId([1u8; 32]);
        let ch_theirs = ChannelId([2u8; 32]);
        let base = kv(&[], &[]);
        let mine = kv(&[], &[]).with_pending_hashes(&[(ch_mine, [0xAA; 32])]);
        let theirs = kv(&[], &[]).with_pending_hashes(&[(ch_theirs, [0xBB; 32])]);
        let out = merge_provider_replicas(&base, &mine, &theirs);
        assert_eq!(out.merged.pending_commit_hash(&ch_mine), Some([0xAA; 32]));
        assert_eq!(out.merged.pending_commit_hash(&ch_theirs), Some([0xBB; 32]));

        // Same channel, different stamp → theirs wins (nest-ordered side).
        let mine2 = kv(&[], &[]).with_pending_hashes(&[(ch_mine, [0xAA; 32])]);
        let theirs2 = kv(&[], &[]).with_pending_hashes(&[(ch_mine, [0xCC; 32])]);
        let out2 = merge_provider_replicas(&base, &mine2, &theirs2);
        assert_eq!(out2.merged.pending_commit_hash(&ch_mine), Some([0xCC; 32]));
    }

    /// A provider KV key that carries `gid`'s group state, shaped the way openMLS's
    /// own keys are — the *serialised* group id embedded in the key, which is what
    /// [`every_openmls_written_key_of_a_group_embeds_its_serialised_group_id`] pins
    /// against a real export. Building the fixture through [`group_id_needle`] rather
    /// than from the raw bytes is the point: a hand-written raw-bytes key would make
    /// the merge tests below pass over an encoding no engine ever writes.
    fn group_key(gid: &[u8], suffix: &[u8]) -> Vec<u8> {
        let mut key = b"openmls:".to_vec();
        key.extend_from_slice(&group_id_needle(gid).expect("the group id serialises"));
        key.extend_from_slice(suffix);
        key
    }

    /// **The premise the merge's cheap per-channel attribution rests on: every
    /// provider KV key openMLS itself writes for a group embeds that group's
    /// serialised MLS id, and no key of another group or of the account's global
    /// state does.**
    ///
    /// **Narrower than "every key of a group".** Fauna's own seven `fauna:`-prefixed per-channel marker families
    /// (`PENDING_COMMIT_HASH_PREFIX` and its siblings in `engine.rs`) are also
    /// channel-scoped provider KV entries, keyed by the raw `ChannelId`, not by
    /// openMLS's serialised `GroupId` — so this rule and [`group_sides`] (below) are
    /// blind to them in exactly the same way, and set equality between the two says
    /// nothing about them. The second assertion below checks that blind spot
    /// directly rather than describing it in prose: every key neither oracle
    /// attributes must be a recognised `fauna:` marker for a real channel, or must
    /// name neither channel at all (a true openMLS global, e.g. a signature key
    /// pair). Anything else failing that split would mean a key this rule silently
    /// drops — the actual regression class this test exists to catch.
    ///
    /// [`channel_of_key`] matches bytes because the cursor attribution runs on every
    /// flush of an unadopted era, where [`group_sides`]' group-load-per-side is
    /// affordable only under a contested key. The oracle it is checked against is
    /// therefore `group_sides` itself — openMLS deleting the group from a scratch
    /// store, the keys that vanish being its entries. Over a real engine holding two
    /// groups and real traffic in both directions, the two must agree key for key —
    /// over the openMLS-written keys both of them can see.
    ///
    /// **This test already earned its keep**: the *raw* group id the row that asked
    /// for this attribution assumed — and that `MlsReplicaClient::save_provider_cas`
    /// named in its warn — appears in no key at all. openMLS keys with the serialised
    /// form (`{"value":{"vec":[…]}}`), so a raw-bytes needle matched nothing and every
    /// channel would have quietly kept `min` while looking fixed. A key kind that
    /// stopped embedding the id would fall to "global" the same way and make a channel
    /// look one-sided when it is not — the unsafe direction, a cursor carried forward
    /// over a state that is a mixture. So a red here after a dependency bump means the
    /// merge must change, not this test.
    #[test]
    fn every_openmls_written_key_of_a_group_embeds_its_serialised_group_id() {
        let me = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let peer_a = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let peer_b = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let (ch_a, welcome_a) = me
            .create_group(&peer_a.generate_key_packages(1).unwrap())
            .unwrap();
        peer_a.join_from_welcome(welcome_a).unwrap();
        let (ch_b, welcome_b) = me
            .create_group(&peer_b.generate_key_packages(1).unwrap())
            .unwrap();
        peer_b.join_from_welcome(welcome_b).unwrap();
        // Traffic both ways, so the export carries more key kinds than a bare create
        // leaves behind (a consumed ratchet generation on one group, an authored one
        // on the other).
        let inbound = text(&peer_a, &ch_a, 1, "from the peer");
        me.decrypt(&ch_a, &inbound).unwrap();
        let _ = text(&me, &ch_b, 1, "one of my own");

        let snapshot = ProviderReplica::from_engine(&me);
        let mut listed = snapshot.channel_ids();
        listed.sort_by_key(|c| c.0);
        let mut expected = vec![ch_a, ch_b];
        expected.sort_by_key(|c| c.0);
        assert_eq!(listed, expected, "both channels, and only those");

        let needles: Vec<(Vec<u8>, Vec<u8>)> = snapshot
            .group_ids
            .iter()
            .map(|(c, g)| (c.to_vec(), group_id_needle(g).expect("serialises")))
            .collect();
        let by_openmls = group_sides(&snapshot);
        assert_eq!(by_openmls.len(), 2, "openMLS examined both groups");

        for (cid, side) in &by_openmls {
            let by_key_rule: BTreeSet<Vec<u8>> = snapshot
                .values
                .iter()
                .filter(|(k, _)| {
                    channel_of_key(k, &needles).is_some_and(|c| c.as_slice() == cid.as_slice())
                })
                .map(|(k, _)| k.to_vec())
                .collect();
            let by_delete: BTreeSet<Vec<u8>> = side.entries.iter().cloned().collect();
            assert!(
                !by_delete.is_empty(),
                "openMLS attributes at least one entry to channel {}",
                hex::encode(cid),
            );
            assert_eq!(
                by_key_rule,
                by_delete,
                "the serialised-group-id key match and openMLS's own delete must name \
                 the same entries for a group; they disagreed for channel {}",
                hex::encode(cid),
            );
        }

        // The blind spot itself, checked rather than described: every key NEITHER
        // oracle attributes to a channel must be one of the two shapes the doc
        // comment above names — a `fauna:`-prefixed marker for a channel this run
        // actually created, or a key naming neither channel's raw id at all (a true
        // openMLS global). A key that is unattributed for any OTHER reason is the
        // exact regression this test exists to catch.
        let unattributed: Vec<&serde_bytes::ByteBuf> = snapshot
            .values
            .iter()
            .filter(|(k, _)| channel_of_key(k, &needles).is_none())
            .map(|(k, _)| k)
            .collect();
        assert!(
            !unattributed.is_empty(),
            "this fixture must exercise the blind spot it pins — no unattributed key \
             means the fauna: markers, or the global keys, or both are missing from \
             this export"
        );
        let names_a_channel = |k: &[u8]| k.windows(32).any(|w| w == ch_a.0 || w == ch_b.0);
        for key in &unattributed {
            let is_fauna_marker = key.starts_with(b"fauna:");
            assert_eq!(
                is_fauna_marker,
                names_a_channel(key),
                "an unattributed key must be EITHER a `fauna:` marker naming a real \
                 channel (the documented blind spot) OR a true global naming neither \
                 channel — this one was {}: {}",
                if is_fauna_marker {
                    "a fauna: marker that names no channel at all"
                } else {
                    "not a fauna: marker, yet it names a channel — a key the byte-match \
                     rule silently drops"
                },
                String::from_utf8_lossy(key),
            );
        }
    }

    /// [`channel_of_key`]'s two `None` answers, which are what keeps the cheap rule on
    /// the safe side of its own ambiguity: a key naming no group at all is
    /// account-global state, and a key naming two is evidence for neither.
    #[test]
    fn a_key_naming_no_group_or_two_speaks_for_no_channel() {
        let one = vec![1u8; 32];
        let two = vec![2u8; 32];
        let needles = vec![
            (one.clone(), b"needle-one".to_vec()),
            (two, b"needle-two".to_vec()),
        ];

        assert_eq!(
            channel_of_key(b"GroupState needle-one \x00\x01", &needles),
            Some(&one),
        );
        assert_eq!(
            channel_of_key(b"KeyPackage 0011 \x00\x01", &needles),
            None,
            "a global entry belongs to no channel",
        );
        assert_eq!(
            channel_of_key(b"needle-one and needle-two", &needles),
            None,
            "a key naming two groups is evidence for neither",
        );
        assert_eq!(
            channel_of_key(b"needle-one, needle-one", &needles),
            Some(&one),
            "the same group named twice is still that one group",
        );
    }

    /// **The residual `devices.md`'s conflict-free-merge ruling carried, removed: a
    /// channel only the sibling holds keeps the sibling's read position instead of
    /// being reset to 0** — and, in the same merge, a channel only this device
    /// advanced keeps this device's, instead of being dragged back to the stored
    /// blob's. Neither channel has a contested key, so the classification never runs:
    /// this is the ordinary flush of an unadopted era.
    ///
    /// The fixture is that era exactly as `fauna_client_mls_sync::MlsStateSync`
    /// produces it: `base` is this device's OWN last export (the ancestor the CAS
    /// save keeps),
    /// `mine` is its current export, and `theirs` is the stored blob carrying a
    /// sibling's group this device has never held.
    #[test]
    fn each_one_sided_channel_keeps_the_cursor_of_the_side_whose_values_won() {
        let my_gid = b"raw-group-id-mine".to_vec();
        let sibling_gid = b"raw-group-id-siblings".to_vec();
        let mine_ch = ChannelId([1u8; 32]);
        let sibling_ch = ChannelId([2u8; 32]);
        let k_mine = group_key(&my_gid, b"/tree");
        let k_sibling = group_key(&sibling_gid, b"/tree");

        let base = kv(&[(&k_mine, b"epoch-4")], &[(&mine_ch.0[..], &my_gid)])
            .with_cursors(&[(mine_ch, 40)]);
        // This device polled its own channel forward since that export.
        let mine = kv(&[(&k_mine, b"epoch-5")], &[(&mine_ch.0[..], &my_gid)])
            .with_cursors(&[(mine_ch, 60)]);
        // The stored blob: this device's own state as it left it, plus a group only
        // the sibling holds, read to seq 900.
        let theirs = kv(
            &[(&k_mine, b"epoch-4"), (&k_sibling, b"epoch-2")],
            &[(&mine_ch.0[..], &my_gid), (&sibling_ch.0[..], &sibling_gid)],
        )
        .with_cursors(&[(mine_ch, 40), (sibling_ch, 900)]);

        let out = merge_provider_replicas(&base, &mine, &theirs);
        assert!(
            out.conflicted_keys.is_empty() && out.reconciled_keys == 0,
            "no key is contested here — the attribution, not the classification, \
             is what resolves these cursors",
        );
        assert_eq!(
            out.merged.cursor(&sibling_ch),
            Some(900),
            "only the sibling's keys carry this channel, so its cursor is the sound \
             one — zeroing it costs the whole channel's history on the next restore",
        );
        assert_eq!(
            out.merged.cursor(&mine_ch),
            Some(60),
            "only this device's keys changed for this channel, so the merged state \
             for it IS this device's and its cursor indexes it",
        );
    }

    /// The half of the `min` rule that stays: a channel **both** sides changed, with
    /// no contested key for the classification to reconcile, merges to a per-key
    /// mixture of the two that neither side's cursor indexes — so the merged cursor
    /// rounds to the safe side (`devices.md` Rule 2).
    #[test]
    fn a_channel_both_sides_advanced_still_takes_the_minimum() {
        let gid = b"raw-group-id-shared".to_vec();
        let ch = ChannelId([3u8; 32]);
        let tree = group_key(&gid, b"/tree");
        let secrets = group_key(&gid, b"/secrets");

        let base = kv(&[(&tree, b"t0"), (&secrets, b"s0")], &[(&ch.0[..], &gid)])
            .with_cursors(&[(ch, 10)]);
        let mine = kv(&[(&tree, b"t1"), (&secrets, b"s0")], &[(&ch.0[..], &gid)])
            .with_cursors(&[(ch, 30)]);
        let theirs = kv(&[(&tree, b"t0"), (&secrets, b"s2")], &[(&ch.0[..], &gid)])
            .with_cursors(&[(ch, 20)]);

        assert_eq!(
            merge_provider_replicas(&base, &mine, &theirs)
                .merged
                .cursor(&ch),
            Some(20),
            "a genuinely mixed channel keeps min — no side's values wholly won it",
        );
    }

    /// The ingest cursor's **fallback**, which is where a channel lands when neither
    /// the classification nor [`channel_of_key`] establishes a winner — as here, where
    /// the replicas carry no group ids at all, so no key is attributable: **`min`**,
    /// not the theirs-wins rule the other three fields use. With no side established
    /// as the winner of the channel's values, the merged state for it may be a per-key
    /// mixture, so only a cursor at or behind its true read position is sound. A cursor
    /// behind it costs an idempotent re-walk; a cursor ahead of it skips a commit and
    /// strands the device at a dead epoch forever (`devices.md` § Cross-device MLS
    /// group-state sync, Rule 2). The attributed cases are
    /// [`each_one_sided_channel_keeps_the_cursor_of_the_side_whose_values_won`].
    #[test]
    fn merge_takes_the_minimum_ingest_cursor_when_no_side_owns_the_channel() {
        let ch = ChannelId([7u8; 32]);
        let other = ChannelId([9u8; 32]);
        let base = kv(&[], &[]);

        // Same channel, both sides advanced → the laggard wins.
        let mine = kv(&[], &[]).with_cursors(&[(ch, 10)]);
        let theirs = kv(&[], &[]).with_cursors(&[(ch, 12)]);
        assert_eq!(
            merge_provider_replicas(&base, &mine, &theirs)
                .merged
                .cursor(&ch),
            Some(10),
            "min(mine, theirs) — never adopt the further-ahead read position",
        );
        // Symmetric: the rule is not "mine wins", it is "min wins".
        assert_eq!(
            merge_provider_replicas(&base, &theirs, &mine)
                .merged
                .cursor(&ch),
            Some(10),
        );

        // A channel a cursor-carrying side omits ⇒ that device has read position 0
        // there ⇒ the merged cursor is 0, the safe direction (a full re-walk).
        let mine_two = kv(&[], &[]).with_cursors(&[(ch, 10), (other, 4)]);
        let theirs_one = kv(&[], &[]).with_cursors(&[(ch, 12)]);
        let merged = merge_provider_replicas(&base, &mine_two, &theirs_one).merged;
        assert_eq!(merged.cursor(&ch), Some(10));
        assert_eq!(
            merged.cursor(&other),
            Some(0),
            "theirs carries cursors but omits `other` ⇒ its read position there is 0",
        );
    }

    /// A **cursor-less** replica (a device that has not polled) carries no cursor
    /// information at all, so it must not drag a cursor-carrying side down to 0. Safe
    /// because a device with no cursor entries has advanced no group's KV by polling
    /// either, so the other side's state — and hence its cursor — wins the merge.
    #[test]
    fn merge_lets_a_cursor_carrying_side_win_over_a_cursor_less_replica() {
        let ch = ChannelId([7u8; 32]);
        let base = kv(&[], &[]);
        let cursor_less = kv(&[], &[]); // no `cursors` at all
        let modern = kv(&[], &[]).with_cursors(&[(ch, 12)]);

        assert_eq!(
            cursor_less.cursor(&ch),
            None,
            "cursor-less carries no cursor"
        );
        assert_eq!(
            merge_provider_replicas(&base, &cursor_less, &modern)
                .merged
                .cursor(&ch),
            Some(12),
        );
        assert_eq!(
            merge_provider_replicas(&base, &modern, &cursor_less)
                .merged
                .cursor(&ch),
            Some(12),
        );
    }

    /// The same at-rest compat gate as
    /// [`replica_decodes_legacy_bytes_without_pending_hashes_field`], now for
    /// `cursors`: a `provider` blob sealed before the field existed must still
    /// decode (alpha began 2026-06-14 — at-rest evolution is additive and
    /// bidirectional, never data-destroying). The absence must read back as "no
    /// cursor", which is exactly what makes `MlsStateSync::load` fall back to the
    /// `history/<ch>` slice's watermark for a legacy replica.
    #[test]
    fn replica_decodes_legacy_bytes_without_cursors_field() {
        #[derive(Serialize)]
        struct LegacyProviderReplica {
            values: Vec<(ByteBuf, ByteBuf)>,
            group_ids: Vec<(ByteBuf, ByteBuf)>,
            pending_commit_hashes: Vec<(ByteBuf, ByteBuf)>,
        }
        let legacy = LegacyProviderReplica {
            values: vec![(ByteBuf::from(b"k1".to_vec()), ByteBuf::from(b"v1".to_vec()))],
            group_ids: vec![(
                ByteBuf::from([9u8; 32].to_vec()),
                ByteBuf::from(b"gid".to_vec()),
            )],
            pending_commit_hashes: vec![],
        };
        let legacy_bytes = fauna_cbor::encode_canonical(&legacy).unwrap();

        let decoded = ProviderReplica::from_bytes(&legacy_bytes)
            .expect("legacy (pre-`cursors`) provider bytes must still decode");
        assert_eq!(get(&decoded, b"k1"), Some(b"v1".as_slice()));
        assert_eq!(
            decoded.cursor(&ChannelId([9u8; 32])),
            None,
            "a legacy replica carries no cursor ⇒ the loader falls back to the slice watermark",
        );

        // And a modern replica round-trips its cursors through the same codec.
        let modern = kv(&[], &[]).with_cursors(&[(ChannelId([9u8; 32]), 42)]);
        let decoded = ProviderReplica::from_bytes(&modern.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded.cursor(&ChannelId([9u8; 32])), Some(42));
    }
    /// **A predecessor's snapshot is recognized by the leaf it seats, never by
    /// the path or key it came with** (`succession-aftermath.md` § Re-key scope
    /// → *What a successor's replica restore may take from a predecessor's*).
    ///
    /// The ceremony's shape in-process: the owner's leaf adds the successor to
    /// a group it shares with bob, the successor joins from the Welcome. The
    /// owner's snapshot then seats that group under the OWNER's leaf; asked on
    /// the successor's behalf it names the channel, asked on the owner's it is
    /// silent, and the successor's own snapshot is silent for the successor.
    /// The last block is the bug itself, demonstrated at the engine layer:
    /// restoring the owner's snapshot into the successor re-seats it as the
    /// owner — which is what the orchestration guard exists to refuse.
    #[test]
    fn a_predecessors_snapshot_is_recognized_by_the_leaf_it_seats() {
        use crate::channel::GroupChannel;
        use crate::succession::commit_add_successor;

        let owner = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let successor = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let owner_id = owner.identity_actor_id();
        let successor_id = successor.identity_actor_id();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (owner_group, welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();
        GroupChannel::join(&bob, welcome).unwrap();
        let channel = owner_group.channel_id;

        let successor_kp = successor.generate_key_packages(1).unwrap();
        let add = commit_add_successor(&owner, &channel, &successor_kp[0]).unwrap();
        bob.process_commit(&channel, &add.commit_bytes).unwrap();
        GroupChannel::join(&successor, add.welcome).unwrap();
        assert_eq!(successor.own_leaf_identity(&channel), Some(successor_id));

        let owners_snapshot = ProviderReplica::from_engine(&owner);
        assert_eq!(
            owners_snapshot.seating_verdict(&successor_id).foreign,
            vec![channel],
            "the owner's snapshot seats this group under the owner's leaf — for the \
             successor that is another identity's seat"
        );
        assert!(
            owners_snapshot.seating_verdict(&owner_id).is_clean(),
            "for the owner it is its own — examined, and none foreign"
        );
        assert!(
            ProviderReplica::from_engine(&successor)
                .seating_verdict(&successor_id)
                .is_clean(),
            "the successor's own snapshot seats the group under the successor's leaf"
        );

        // The bug, at the layer that has it: a whole-KV swap seats the successor
        // as the owner — the leaf remove-old is meant to evict.
        owners_snapshot.restore_into_unchecked(&successor).unwrap();
        assert_eq!(
            successor.own_leaf_identity(&channel),
            Some(owner_id),
            "restoring a predecessor's snapshot re-seats the engine as the predecessor"
        );
    }

    /// **The verdict must not read "could not examine" as "clean".**
    ///
    /// A snapshot that decodes, lists a well-formed group, and whose KV holds
    /// nothing that group loads from used to score `[]` — and the caller
    /// restores on `[]`, so the whole KV was swapped into the successor's
    /// engine. That is the outcome `succession-aftermath.md` § Re-key scope →
    /// *What a successor's replica restore may take from a predecessor's*
    /// rule (1) states as an absolute, reached without the check ever
    /// objecting: the rule justifies itself on the leaf being "a fact of its
    /// bytes", and the old implementation asserted the safe answer precisely
    /// when it could not reach that fact.
    ///
    /// It is not hypothetical. Rule (3) publishes the snapshot to the nest
    /// because the identity's OTHER devices restore from it, and those devices
    /// may run different versions within a major
    /// (`version-compatibility.md`) — so an older device meeting a group
    /// serialization it cannot load takes this path with no attacker and no
    /// bug anywhere else.
    #[test]
    fn a_snapshot_whose_groups_cannot_be_examined_is_not_clean() {
        let identity = ActorKeypair::generate().actor_id();
        let replica = ProviderReplica::unexaminable_for_test(ChannelId([7u8; 32]));

        let verdict = replica.seating_verdict(&identity);
        assert!(
            verdict.foreign.is_empty(),
            "nothing can be positively identified as foreign — that is the point"
        );
        assert_eq!(verdict.unexaminable, 1, "…but the group was NOT examined");
        assert!(
            !verdict.is_clean(),
            "a snapshot whose seating could not be established must never read as \
             clean: `is_clean` is what the caller restores on"
        );
    }

    /// A channel id that is not 32 bytes is unexaminable on the same terms — a
    /// group we cannot even name is a group we did not check.
    #[test]
    fn a_malformed_channel_id_is_unexaminable_not_clean() {
        let identity = ActorKeypair::generate().actor_id();
        let replica = ProviderReplica {
            values: vec![],
            group_ids: vec![(ByteBuf::from(vec![1, 2, 3]), ByteBuf::from(b"gid".to_vec()))],
            ..Default::default()
        };

        let verdict = replica.seating_verdict(&identity);
        assert_eq!(verdict.unexaminable, 1);
        assert!(!verdict.is_clean());
    }

    /// **The counter-pin, and the one that keeps the fix from being worse than
    /// the bug.** A group that LOADS but no longer seats this identity — another
    /// member's remove-old already evicted it, and the engine has no
    /// self-removal forget path (`MlsEngine::forget_group` is voluntary, so the
    /// entry simply stays) — is EXAMINED, not unknown.
    ///
    /// Counting it as unexaminable is the easy reading of "we could not
    /// determine the seat", and it would refuse the cross-device restore of
    /// every snapshot belonging to a user who has ever been removed from a
    /// single group — permanently, since nothing ever removes the entry. That
    /// would be a worse bug than the fail-open it replaces, so it is pinned
    /// against the real eviction rather than argued.
    #[test]
    fn a_group_this_identity_no_longer_seats_is_examined_not_unknown() {
        use crate::channel::GroupChannel;

        let owner = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let bob_id = bob.identity_actor_id();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (group, welcome) = GroupChannel::create(&owner, &bob_kps).unwrap();
        GroupChannel::join(&bob, welcome).unwrap();
        let channel = group.channel_id;

        // The owner evicts bob, and bob processes his own removal — the
        // ordinary end of a membership, and the state every removed user's
        // snapshot carries from then on.
        // Leaf 1 — bob joined second (same literal `engine.rs`'s own
        // `add_and_remove_member` uses).
        let commit = owner.remove_member(&channel, 1).unwrap();
        bob.process_commit(&channel, &commit).unwrap();

        let verdict = ProviderReplica::from_engine(&bob).seating_verdict(&bob_id);

        assert_eq!(
            verdict.unexaminable, 0,
            "the group LOADED — counting an eviction as `unknown` would refuse the \
             restore of every user ever removed from a group, permanently, since \
             nothing prunes the entry: {verdict:?}"
        );
        assert!(
            verdict.is_clean(),
            "…and bob's own snapshot stays restorable by bob: {verdict:?}"
        );
    }

    // ── Welcome consumption across two online devices  ──────────

    /// Two devices of one identity and a pool package they BOTH hold: device A
    /// minted it, the provider flush replicated it, device B restored the
    /// flush. A peer then welcomes the account through that package. Returns
    /// `(device_a, device_b, peer, channel, the pool replica both restored
    /// from, the Welcome's wire bytes, the account's seed)` — the state at the
    /// instant the nest fans the Welcome push to both connections, before
    /// either has processed it.
    fn two_online_devices_share_one_pool_package() -> (
        MlsEngine,
        MlsEngine,
        MlsEngine,
        ChannelId,
        ProviderReplica,
        Vec<u8>,
        [u8; 32],
    ) {
        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let device_a = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let kps = device_a.generate_key_packages(1).unwrap();
        let pool = ProviderReplica::from_engine(&device_a);
        let device_b = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        pool.restore_into_unchecked(&device_b).unwrap();
        assert_eq!(
            ProviderReplica::from_engine(&device_b).values,
            pool.values,
            "B restored A's pool verbatim — both now hold the package's private init key"
        );

        let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let (channel, welcome) = peer.create_group(&kps).unwrap();
        let welcome_bytes = welcome.to_bytes().unwrap();
        (
            device_a,
            device_b,
            peer,
            channel,
            pool,
            welcome_bytes,
            secret,
        )
    }

    /// The keys whose value `after` holds differently from `before` (added,
    /// changed, or removed), for a diagnostic count.
    fn keys_changed(before: &ProviderReplica, after: &ProviderReplica) -> usize {
        let b: HashMap<&[u8], &[u8]> = before
            .values
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        let a: HashMap<&[u8], &[u8]> = after
            .values
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        let mut n = 0;
        for (k, v) in &a {
            if b.get(k) != Some(v) {
                n += 1;
            }
        }
        for k in b.keys() {
            if !a.contains_key(k) {
                n += 1;
            }
        }
        n
    }

    /// **Two online devices that both hold the init key both process one
    /// Welcome — and land on the SAME leaf, byte for byte.** The nest fans the
    /// Welcome push to every connection of the actor, and the replica carries
    /// the pool's private init keys to every device, so nothing stops both from
    /// joining. This pins what that double-join IS: one transition applied
    /// twice. The two engines' provider exports are byte-identical (same keys,
    /// same values, same group list), the consumed package is gone from both
    /// (a second join of the same Welcome fails on each), the three-way merge
    /// B's flush runs after A's has landed reports **zero** conflicted keys —
    /// the conflict ruling's own "same transition, same bytes" case — and both
    /// decrypt the peer's next message. The premise this refutes: that the
    /// account ends up with "two engines that each believe they are the
    /// group's single leaf" as a two-writer violation. They are one leaf in one
    /// state, which is exactly the multi-device model (one leaf, N devices,
    /// replica-synced); the device-owned-epoch invariant governs what happens
    /// when either *advances* it, as it does for a group reached by any door.
    #[test]
    fn two_online_devices_both_consume_one_welcome_as_one_identical_leaf() {
        let (device_a, device_b, peer, channel, pool, welcome_bytes, _secret) =
            two_online_devices_share_one_pool_package();

        // The push reaches both; both process it.
        assert_eq!(
            device_a.join_from_welcome_bytes(&welcome_bytes).unwrap(),
            channel
        );
        assert_eq!(
            device_b.join_from_welcome_bytes(&welcome_bytes).unwrap(),
            channel
        );
        let a_joined = ProviderReplica::from_engine(&device_a);
        let b_joined = ProviderReplica::from_engine(&device_b);

        // Same keys, same state under every key (bytes, but for openMLS's
        // wall-clock `added_at`), same group list.
        let differing = differing_state(&a_joined.values, &b_joined.values);
        assert_eq!(
            differing,
            0,
            "the two joins wrote the same state under every key ({differing} of {} differ)",
            a_joined.values.len()
        );
        assert_eq!(a_joined.group_ids, b_joined.group_ids);
        assert_eq!(a_joined.channel_ids(), vec![channel]);

        // The package is consumed on both — the join deleted its entries from
        // each KV (observed through our own door, not a dependency's source: a
        // second join of the same Welcome finds no init key on either engine).
        assert!(
            keys_changed(&pool, &a_joined) > 0,
            "the join changed the KV against the pool it started from"
        );
        assert!(
            device_a.join_from_welcome_bytes(&welcome_bytes).is_err(),
            "A cannot join the same Welcome twice — its init key is consumed"
        );
        assert!(
            device_b.join_from_welcome_bytes(&welcome_bytes).is_err(),
            "B cannot either — its own copy of the init key is consumed by its own join"
        );

        // B flushes after A's flush landed: base = the pool B last loaded, mine =
        // B's export, theirs = A's landed replica. Same transition, same bytes:
        // nothing to report.
        let outcome = merge_provider_replicas(&pool, &b_joined, &a_joined);
        assert!(
            outcome.conflicted_keys.is_empty(),
            "a double-join is not a two-writer conflict: {} key(s) reported",
            outcome.conflicted_keys.len()
        );
        assert_eq!(outcome.merged.values, a_joined.values);
        assert_eq!(outcome.merged.group_ids, a_joined.group_ids);

        // And both are live members: the peer's next message decrypts on each.
        let m = text(&peer, &channel, 1, "to whichever device is open");
        for (name, dev) in [("A", &device_a), ("B", &device_b)] {
            let got = dev
                .decrypt(&channel, &m)
                .unwrap_or_else(|e| panic!("device {name}: {e}"));
            assert!(
                matches!(got.body, ChannelMessageBody::Text(t) if t == "to whichever device is open")
            );
        }
    }

    /// **The same leaf on two devices folds the peer's stream at different
    /// paces, and the merge reconciles it silently — the side further along
    /// wins, with its cursor.** After the double-join above both devices poll
    /// the channel; A is ahead (three records folded) when B (one folded)
    /// flushes against A's landed replica. The receiver ratchet lives under
    /// ONE provider key per group, so the two changed the same key to different
    /// values — measured 2026-09-01 as a reported "two-writer" conflict, the
    /// steady state every two-device account lives in whenever its sweeps flush
    /// out of step, whatever door brought the group.
    /// Now the merge attributes the key to its group, sees one epoch on both
    /// sides, and takes the higher ingest cursor's value — in either merge
    /// direction — reporting nothing. The data converges the same way it did
    /// before: B's catch-up export equals A's, and B's next merge off its OWN
    /// last export (the ancestor rule) is one-sided.
    #[test]
    fn two_devices_folding_one_stream_at_different_paces_reconcile_to_the_side_further_along() {
        let (device_a, device_b, peer, channel, _pool, welcome_bytes, _secret) =
            two_online_devices_share_one_pool_package();
        device_a.join_from_welcome_bytes(&welcome_bytes).unwrap();
        device_b.join_from_welcome_bytes(&welcome_bytes).unwrap();
        let joined = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 0)]);

        let m1 = text(&peer, &channel, 1, "one");
        let m2 = text(&peer, &channel, 2, "two");
        let m3 = text(&peer, &channel, 3, "three");
        device_a.decrypt(&channel, &m1).unwrap();
        device_a.decrypt(&channel, &m2).unwrap();
        device_a.decrypt(&channel, &m3).unwrap();
        device_b.decrypt(&channel, &m1).unwrap();
        let a_ahead = ProviderReplica::from_engine(&device_a).with_cursors(&[(channel, 3)]);
        let b_behind = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 1)]);
        assert_eq!(
            (
                keys_changed(&joined, &a_ahead),
                keys_changed(&joined, &b_behind)
            ),
            (1, 1),
            "folding the peer's records changes exactly one provider key per group — \
             the receiver ratchet — on each device"
        );
        assert_ne!(a_ahead.values, b_behind.values);

        // B flushes behind A: theirs is further along.
        let outcome = merge_provider_replicas(&joined, &b_behind, &a_ahead);
        assert!(
            outcome.conflicted_keys.is_empty(),
            "two positions on one stream are not two writers"
        );
        assert_eq!(outcome.reconciled_keys, 1);
        assert_eq!(
            outcome.merged.values, a_ahead.values,
            "the ahead side's state"
        );
        assert_eq!(
            outcome.merged.cursor(&channel),
            Some(3),
            "…with the ahead side's cursor"
        );

        // A flushes ahead of B: mine is further along — the same answer.
        let outcome = merge_provider_replicas(&joined, &a_ahead, &b_behind);
        assert!(outcome.conflicted_keys.is_empty());
        assert_eq!(outcome.reconciled_keys, 1);
        assert_eq!(outcome.merged.values, a_ahead.values);
        assert_eq!(outcome.merged.cursor(&channel), Some(3));

        // B catches up → the same bytes as A; its next merge, off its own last
        // export, has one side changed only.
        device_b.decrypt(&channel, &m2).unwrap();
        device_b.decrypt(&channel, &m3).unwrap();
        let b_caught_up = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 3)]);
        assert_eq!(differing_state(&b_caught_up.values, &a_ahead.values), 0);
        let next = merge_provider_replicas(&b_behind, &b_caught_up, &a_ahead);
        assert!(next.conflicted_keys.is_empty());
        assert_eq!(next.reconciled_keys, 0);
        assert_eq!(next.merged.values, a_ahead.values);
    }

    /// **The epoch owner's own send moves the same key with no cursor lead —
    /// and that is reconciled too, in either direction, because a value at
    /// the path never authors a message.** Both devices fold the peer's one
    /// record (equal cursors); A then sends (the sender ratchet lives under the
    /// receiver ratchet's key, and a send is not a poll, so A's cursor does not
    /// move). The two sides differ at equal cursors: same epoch, tie → theirs,
    /// silently. When B's value wins, the path holds a sender ratchet A has
    /// advanced past — sound, because no device ever encrypts with a sender
    /// ratchet it loaded from the path: every launch and resync takes the
    /// epoch over with a fresh `self_update` before its first send
    /// (`fauna-client-mls-sync::commit_gate::takeover_commits_once_then_noops`,
    /// `gate_impl::twin_device_takeover_resyncs_the_other_device`). Pinned at
    /// the engine: a fresh device restoring the winning value, taking over,
    /// and sending is decrypted by the peer; and A, whose in-memory ratchet the
    /// merge never touched, keeps sending.
    #[test]
    fn an_own_send_moves_the_same_key_without_a_cursor_lead_and_still_reconciles() {
        let (device_a, device_b, peer, channel, _pool, welcome_bytes, secret) =
            two_online_devices_share_one_pool_package();
        device_a.join_from_welcome_bytes(&welcome_bytes).unwrap();
        device_b.join_from_welcome_bytes(&welcome_bytes).unwrap();
        let joined = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 0)]);

        let m1 = text(&peer, &channel, 1, "one");
        device_a.decrypt(&channel, &m1).unwrap();
        device_b.decrypt(&channel, &m1).unwrap();
        let own = text(&device_a, &channel, 2, "from device A");
        let got = peer.decrypt(&channel, &own).unwrap();
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "from device A"));
        let a_sent = ProviderReplica::from_engine(&device_a).with_cursors(&[(channel, 1)]);
        let b_received = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 1)]);
        assert_eq!(keys_changed(&joined, &a_sent), 1);
        assert_eq!(keys_changed(&joined, &b_received), 1);
        assert_ne!(
            a_sent.values, b_received.values,
            "the send changed the receiver-ratchet key with no cursor lead"
        );

        // Equal cursors, same epoch: silent, theirs by the tie rule — both ways.
        let outcome = merge_provider_replicas(&joined, &b_received, &a_sent);
        assert!(outcome.conflicted_keys.is_empty());
        assert_eq!(outcome.reconciled_keys, 1);
        assert_eq!(outcome.merged.values, a_sent.values);
        let outcome = merge_provider_replicas(&joined, &a_sent, &b_received);
        assert!(outcome.conflicted_keys.is_empty());
        assert_eq!(outcome.reconciled_keys, 1);
        assert_eq!(
            outcome.merged.values, b_received.values,
            "the path may hold the sender ratchet A has moved past"
        );

        // …which is sound: a device restoring that value takes the epoch over
        // before it sends, so the rewound sender ratchet never authors a message.
        let device_c = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        outcome.merged.restore_into_unchecked(&device_c).unwrap();
        let takeover = device_c.self_update(&channel).unwrap();
        device_c.merge_pending_commit(&channel).unwrap();
        peer.process_commit(&channel, &takeover).unwrap();
        let from_c = text(&device_c, &channel, 3, "from the restored device");
        let got = peer.decrypt(&channel, &from_c).unwrap();
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "from the restored device"));

        // And A's in-memory sender chain was never the merge's to touch: it
        // still produces a message (the peer, now on C's epoch, is another
        // matter — that is the own-leaf-commit resync's job on A).
        assert!(!text(&device_a, &channel, 4, "A again").is_empty());
    }

    /// **A sibling still at the ancestor's epoch yields silently to the side
    /// that folded the commit.** Both devices fold the peer's record; the peer
    /// then commits (a `self_update`) and only A processes it, so A is one epoch
    /// past the ancestor while B — receive progress and all — is still at it.
    /// The contested key is the same one; the merge reads the epochs and takes
    /// the side that moved **forward**, with its cursor, reporting nothing: the
    /// lagging side closes the gap by processing the commit. (Which of the two is
    /// `mine` and which is `theirs` does not matter — that is the loop below. A
    /// side that moved *backwards* is a different shape entirely, and stays loud:
    /// [`a_side_behind_the_ancestors_epoch_is_reported_not_silently_adopted`].)
    #[test]
    fn a_sibling_at_the_ancestors_epoch_yields_silently_to_the_side_that_folded_the_commit() {
        let (device_a, device_b, peer, channel, _pool, welcome_bytes, _secret) =
            two_online_devices_share_one_pool_package();
        device_a.join_from_welcome_bytes(&welcome_bytes).unwrap();
        device_b.join_from_welcome_bytes(&welcome_bytes).unwrap();
        let joined = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 0)]);

        let m1 = text(&peer, &channel, 1, "one");
        device_a.decrypt(&channel, &m1).unwrap();
        device_b.decrypt(&channel, &m1).unwrap();
        let commit = peer.self_update(&channel).unwrap();
        peer.merge_pending_commit(&channel).unwrap();
        device_a.process_commit(&channel, &commit).unwrap();
        assert_eq!(
            device_a.current_epoch(&channel).unwrap(),
            device_b.current_epoch(&channel).unwrap() + 1
        );
        let a_folded = ProviderReplica::from_engine(&device_a).with_cursors(&[(channel, 2)]);
        let b_lagging = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 1)]);

        for (mine, theirs) in [(&b_lagging, &a_folded), (&a_folded, &b_lagging)] {
            let outcome = merge_provider_replicas(&joined, mine, theirs);
            assert!(
                outcome.conflicted_keys.is_empty(),
                "a sibling one commit behind is not a second writer"
            );
            assert!(outcome.reconciled_keys >= 1);
            assert_eq!(
                outcome.merged.values, a_folded.values,
                "the side that moved"
            );
            assert_eq!(outcome.merged.cursor(&channel), Some(2));
        }

        // The lagging side closes the gap the ordinary way.
        device_b.process_commit(&channel, &commit).unwrap();
        assert_eq!(
            differing_state(
                &ProviderReplica::from_engine(&device_b).values,
                &a_folded.values
            ),
            0
        );
    }

    /// **A side BEHIND the ancestor's epoch is reported, not silently adopted.**
    /// The mirror image of the test above, and the shape it could not tell apart:
    /// "moved off the ancestor's epoch" is satisfied by a side one epoch *behind*
    /// it just as well as by one that folded a commit, so a stale replica — the
    /// pre-commit snapshot the nest still holds, replayed — took the silent
    /// verdict and became the merged output with no record that a rollback had
    /// happened.
    ///
    /// A regression is not a fold, so the classification refuses it: the side
    /// that moved must have moved **forward**, and anything else falls to the
    /// loud default. Which bytes land is unchanged — theirs-wins is that
    /// default too — so what this pins is the *report*, which
    /// `MlsReplicaClient::save_provider_cas` calls the only record that the
    /// event occurred at all.
    #[test]
    fn a_side_behind_the_ancestors_epoch_is_reported_not_silently_adopted() {
        let (device_a, device_b, peer, channel, _pool, welcome_bytes, _secret) =
            two_online_devices_share_one_pool_package();
        device_a.join_from_welcome_bytes(&welcome_bytes).unwrap();
        device_b.join_from_welcome_bytes(&welcome_bytes).unwrap();

        // Both fold the peer's record; the peer then commits and only A processes
        // it. A is at E+1, B is still at E.
        let m1 = text(&peer, &channel, 1, "one");
        device_a.decrypt(&channel, &m1).unwrap();
        device_b.decrypt(&channel, &m1).unwrap();
        let commit = peer.self_update(&channel).unwrap();
        peer.merge_pending_commit(&channel).unwrap();
        device_a.process_commit(&channel, &commit).unwrap();
        assert_eq!(
            device_a.current_epoch(&channel).unwrap(),
            device_b.current_epoch(&channel).unwrap() + 1,
        );

        // The ancestor is A's own last export, at E+1.
        let ancestor = ProviderReplica::from_engine(&device_a).with_cursors(&[(channel, 1)]);
        // `mine` folds one more record at E+1: the ancestor's epoch, later bytes.
        let m2 = text(&peer, &channel, 2, "two");
        device_a.decrypt(&channel, &m2).unwrap();
        let mine = ProviderReplica::from_engine(&device_a).with_cursors(&[(channel, 2)]);
        // `theirs` is the E snapshot — one epoch BELOW the ancestor.
        let stale = ProviderReplica::from_engine(&device_b).with_cursors(&[(channel, 1)]);

        for (label, m, t) in [
            ("theirs stale", &mine, &stale),
            ("mine stale", &stale, &mine),
        ] {
            let outcome = merge_provider_replicas(&ancestor, m, t);
            assert!(
                !outcome.conflicted_keys.is_empty(),
                "{label}: a side below the ancestor's epoch is a regression, not a \
                 fold — the merge must keep its only record of it",
            );
            assert_eq!(
                outcome.reconciled_keys, 0,
                "{label}: nothing here is same-leaf progress to reconcile",
            );
        }
    }

    /// **Two commits off one ancestor stay the genuine conflict.** The same
    /// shape `fauna-client-mls-sync::store`'s `diverged_over_one_group` pins
    /// through the CAS save: two devices of one leaf each `self_update` off the
    /// same snapshot, landing at the SAME epoch number with different bytes.
    /// The classification sees both sides moved off the ancestor's epoch and
    /// keeps the loud default — theirs wins, the keys are reported, nothing is
    /// reconciled.
    #[test]
    fn two_commits_off_one_ancestor_stay_the_genuine_conflict() {
        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let origin = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let (channel, welcome) = origin.create_group(&peer_kps).unwrap();
        peer.join_from_welcome(welcome).unwrap();
        let base = ProviderReplica::from_engine(&origin).with_cursors(&[(channel, 0)]);

        let advance = || {
            let engine = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
            base.restore_into_unchecked(&engine).unwrap();
            engine.self_update(&channel).unwrap();
            engine.merge_pending_commit(&channel).unwrap();
            ProviderReplica::from_engine(&engine).with_cursors(&[(channel, 0)])
        };
        let mine = advance();
        let theirs = advance();
        assert_ne!(mine.values, theirs.values);

        let outcome = merge_provider_replicas(&base, &mine, &theirs);
        assert!(
            !outcome.conflicted_keys.is_empty(),
            "two writers off one ancestor are reported"
        );
        assert_eq!(outcome.reconciled_keys, 0, "…and nothing is reconciled");
        for key in &outcome.conflicted_keys {
            assert_eq!(
                get(&outcome.merged, key),
                get(&theirs, key),
                "theirs wins every reported key"
            );
        }
    }

    /// **The last-resort package is consumed by EVERY Welcome addressed to
    /// it, on every device.** The nest never deletes a last-resort package
    /// (`take_key_package` hands it out again once the one-time pool drains),
    /// so several Welcomes address one package over its life — and the
    /// consumption rule above has to hold for each of them, on each device.
    /// Observed through our own door, not a dependency's source: two peers
    /// welcome the account through the same last-resort package in turn, and
    /// both devices join both groups, landing byte-identical after each. (A
    /// one-time package is consumed by its first join — the sibling test
    /// above pins that.)
    #[test]
    fn two_online_devices_both_consume_every_welcome_addressed_to_the_last_resort_package() {
        let secret = ActorKeypair::generate().signing_key().to_bytes();
        let device_a = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let lr_bytes = device_a.generate_last_resort_key_package_bytes().unwrap();
        let pool = ProviderReplica::from_engine(&device_a);
        let device_b = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        pool.restore_into_unchecked(&device_b).unwrap();

        let mut welcomes = Vec::new();
        for _ in 0..2 {
            let peer = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
            let kp = peer.key_package_from_bytes(&lr_bytes).unwrap();
            let (channel, welcome) = peer.create_group(std::slice::from_ref(&kp)).unwrap();
            welcomes.push((channel, welcome.to_bytes().unwrap()));
        }
        for (i, (channel, bytes)) in welcomes.iter().enumerate() {
            let a = device_a.join_from_welcome_bytes(bytes).unwrap_or_else(|e| {
                panic!("device A, welcome {i} via the last-resort package: {e}")
            });
            let b = device_b.join_from_welcome_bytes(bytes).unwrap_or_else(|e| {
                panic!("device B, welcome {i} via the last-resort package: {e}")
            });
            assert_eq!(a, *channel);
            assert_eq!(b, *channel);
            assert_eq!(
                differing_state(
                    &ProviderReplica::from_engine(&device_a).values,
                    &ProviderReplica::from_engine(&device_b).values,
                ),
                0,
                "after welcome {i} the two devices are one leaf (bytes, but for openMLS's \
                 wall-clock `added_at`)"
            );
        }
        assert_eq!(device_a.list_groups().len(), 2);
        assert_eq!(device_b.list_groups().len(), 2);
    }
}
