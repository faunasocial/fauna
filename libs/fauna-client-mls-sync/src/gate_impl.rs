//! The production [`CommitGate`] implementation — [`FaunaCommitGate`] closes the
//! injection seam `fauna-conversations` declares (`backend::CommitGate`) over
//! this crate's rebase loop, and [`BackendCatchUp`] closes the loop's
//! [`CommitCatchUp`] seam over the shared inbound driver `poll_inbound_conv`
//! (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync;
//! design tracked internally, §3).
//!
//! **Non-generic by design.** `dyn CommitGate` sits behind the backend's
//! native-`Send` `RailBackend` boundary, so this impl's futures must be `Send`
//! — which a generic `R: RpcRequester` transport cannot prove (see the
//! `store.rs` module doc). Every transport-facing member is type-erased:
//! `MlsStateSync` over `dyn MlsReplicaTransport`, the conversations plane over
//! `Arc<dyn ConversationsRpc>` (uniform with the backend's own `rpc` field).
//!
//! **The cycle break.** The backend holds the gate **strongly**
//! (`OnceLock<Arc<dyn CommitGate>>`, set once post-construction); the gate's
//! catch-up holds the backend and manager **weakly**, so no `Arc` cycle forms.
//! Construction order (the slice-5 leg): manager → engine → conv rpc → backend
//! → `register_backend` → `MlsStateSync` → [`BackendCatchUp::new`] →
//! [`FaunaCommitGate::new`] → `backend.set_commit_gate(gate)`.
//!
//! **Serialization** (module doc of [`crate::commit_gate`]): a channel's gated
//! loop and its background inbound poll must not run concurrently — the engine
//! rejects processing an inbound `Commit` while a gated send has one staged. This
//! is now **backend-owned shared Rust** (`FaunaMlsBackend::channel_lock`, slice
//! 5): the background poll and the gated branches take a per-channel async lock;
//! this gate's own [`BackendCatchUp`] runs its inner `poll_inbound_conv` *inside*
//! the gated section, so it must never re-take that lock (it calls the lock-free
//! `poll_inbound_conv` directly — never the lock-taking `poll_bound`).
//!
//! **Cursor** ([`MlsSyncCursor`]): the leg also injects this crate's
//! [`fauna_conversations::backend::ChannelCursor`] impl so the background poll
//! resumes each channel from the `MlsStateSync` processed-seq cursor (the restored
//! `history/<ch>` watermark), not `0`.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use fauna_conversations::ConversationsManager;
use fauna_conversations::backend::{BackendError, ChannelCursor, CommitGate, ConvRpcError};
use fauna_conversations::backends::fauna_mls::{
    FaunaMlsBackend, poll_inbound_conv, poll_inbound_folder,
};
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::ChannelId;

use crate::commit_gate::{CommitCatchUp, CommitGateError, GatedCommitSend};
use crate::sync::MlsStateSync;

/// Collapse a loop failure onto the seam's non-generic [`BackendError`], so the
/// backend's gated branch degrades exactly like its optimistic branch would:
/// a conversations-plane rejection keeps its three-way classification
/// (`From<ConvRpcError>` — `NeedsUpdate` survives to the non-retry affordance),
/// a catch-up failure is already a `BackendError`, and the loop-internal
/// failures (engine primitive, envelope encode, replica CAS, exhausted rebase
/// retries) surface as retryable `Transport` with the loop's rendered context.
fn gate_error(e: CommitGateError) -> BackendError {
    match e {
        CommitGateError::Conv(c) => BackendError::from(c),
        CommitGateError::CatchUp(be) => be,
        other @ (CommitGateError::Engine(_)
        | CommitGateError::Encode(_)
        | CommitGateError::Replica(_)
        | CommitGateError::RetriesExhausted) => BackendError::Internal(other.to_string()),
    }
}

/// Production [`CommitCatchUp`]: on a stale-epoch rejection, drain the
/// channel's intervening records through the shared inbound driver
/// [`poll_inbound_conv`] (processing foreign `Commit`s into the engine and
/// ingesting applications into the thread store), returning the new high-water
/// seq for the loop to `advance_processed_seq` with. Holds the backend and
/// manager **weakly** — the backend owns the gate (which owns this), so strong
/// references here would leak the whole session graph.
pub struct BackendCatchUp {
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    /// Page size handed to `poll_inbound_conv`; `0` = drain-all, matching every
    /// production caller of the driver.
    page_limit: i64,
}

impl BackendCatchUp {
    /// Build over the session's backend + manager (downgraded — see the struct
    /// doc). Call after `register_backend`, before [`FaunaCommitGate::new`].
    pub fn new(backend: &Arc<FaunaMlsBackend>, manager: &Arc<ConversationsManager>) -> Self {
        Self {
            backend: Arc::downgrade(backend),
            manager: Arc::downgrade(manager),
            page_limit: 0,
        }
    }
}

/// Production [`GatedCommitSend`]: forward the gate's commit to
/// `FaunaMlsBackend::send_on_channel`, the one place the `send` vs `send_remote`
/// pick is made — so a gated commit routes exactly like the application send
/// that follows it.
///
/// **Why the gate does not hold a `ConversationsRpc` any more.** It did, and
/// called `channel_send` unconditionally: on a cross-nest shared folder (home
/// recorded from the Welcome's `nest_url`) the member's device-owned-epoch
/// takeover therefore landed on the member's OWN nest while the advertisement
/// it was taken for routed to the home nest — so co-members were sealed an epoch
/// behind and could not decrypt it, and the `expect_no_commit_since` the round
/// gated on came from the home nest's cursor but was evaluated by the local one,
/// two unrelated seq spaces. Routing through the backend makes the rpc
/// unreachable from the gate, so the mis-pick is not expressible.
///
/// Holds the backend **weakly**, for [`BackendCatchUp`]'s reason exactly: the
/// backend owns the gate, which owns this.
pub struct BackendChannelSend {
    backend: Weak<FaunaMlsBackend>,
}

impl BackendChannelSend {
    /// Build over the session's backend (downgraded — see the struct doc). Pass
    /// the same `Arc` the rest of the leg wires, so the gate sends on the plane
    /// the backend sends on.
    pub fn new(backend: &Arc<FaunaMlsBackend>) -> Self {
        Self {
            backend: Arc::downgrade(backend),
        }
    }
}

impl GatedCommitSend for BackendChannelSend {
    async fn send_gated_commit(
        &self,
        channel: &ChannelId,
        envelope: Vec<u8>,
        expect_no_commit_since: i64,
    ) -> Result<i64, ConvRpcError> {
        let backend = self.backend.upgrade().ok_or_else(|| {
            ConvRpcError::transient("mls backend dropped during a gated commit send")
        })?;
        backend
            .send_on_channel(channel, envelope, Some(expect_no_commit_since), Vec::new())
            .await
    }
}

impl CommitCatchUp for BackendCatchUp {
    /// Advance past `channel`'s records with `seq > from_seq` via the shared
    /// inbound driver for the channel's **rail**: [`poll_inbound_conv`] for a
    /// thread-bound chat channel, [`poll_inbound_folder`] for a folder
    /// channel (which has **no bound thread**, so `poll_inbound_conv` would
    /// return without touching a single record and the rebase would spin to
    /// exhaustion — the gap that blocked the folder `CommitGate` adoption,
    /// `devices.md` § Implementation status, item 2). The local cursor is this
    /// call's only cursor — `MlsStateSync`'s processed-seq is advanced by the
    /// rebase loop itself, never from here (the loop owns that ordering).
    ///
    /// The rail is decided by `FaunaMlsBackend::is_folder_rail` — the **durable,
    /// engine-derived** predicate, never the in-memory `mark_folder_channel`
    /// marker. That marker is written only by the *recipient's* welcome-join and
    /// empties on relaunch, while the only actor that ever gates a commit on a
    /// folder channel is the **owner** removing a member, whose backend never
    /// marks at all — so routing on it sent every contested owner-side removal to
    /// the thread-less chat poll and span the rebase to `RetriesExhausted`. See
    /// that predicate's doc for the full trace; keep this dispatch off the marker.
    ///
    /// A walk on **either rail** that **stalls** (an intervening commit the
    /// engine could not incorporate and the resync arm could not heal) is
    /// surfaced as an error rather than a silently short seq: advancing the
    /// rebase baseline past an unincorporated commit would let the rebuilt
    /// commit be accepted for an epoch other members already left — a permanent
    /// fork. Failing the catch-up aborts the gated send (pending cleared,
    /// retryable) and leaves the heal to the next poll/resync pass.
    ///
    /// The chat rail carried exactly that fork until `ConvPollOutcome` gave
    /// `poll_inbound_conv` a stall channel: it returned a bare count, so this
    /// branch could not tell a drained log from a walk that stopped on an
    /// unincorporated own-leaf commit, and a gated `remove_participant` could
    /// land a commit on an epoch the group had left — every other member
    /// quiet-skipping it as `PastEpochCommit` while the sender merged and
    /// reported success, leaving the "removed" member in the live group. Both
    /// branches now guard identically; keep them that way.
    async fn catch_up_after(
        &self,
        channel: &ChannelId,
        from_seq: i64,
    ) -> Result<i64, BackendError> {
        let backend = self.backend.upgrade().ok_or_else(|| {
            BackendError::Internal("mls backend dropped during commit catch-up".into())
        })?;
        let mut seq = from_seq;
        if backend.is_folder_rail(channel) {
            let outcome = poll_inbound_folder(&backend, channel, &mut seq, self.page_limit).await?;
            if outcome.stalled {
                return Err(BackendError::Internal(format!(
                    "folder catch-up stalled before an unincorporated commit on {channel} \
                     (own-leaf resync pending); gated send aborted to avoid a forked epoch"
                )));
            }
            return Ok(seq);
        }
        let manager = self.manager.upgrade().ok_or_else(|| {
            BackendError::Internal("conversations manager dropped during commit catch-up".into())
        })?;
        let outcome =
            poll_inbound_conv(&backend, &manager, channel, &mut seq, self.page_limit).await?;
        if outcome.stalled {
            return Err(BackendError::Internal(format!(
                "chat catch-up stalled before an unincorporated commit on {channel} \
                 (own-leaf resync pending); gated send aborted to avoid a forked epoch"
            )));
        }
        Ok(seq)
    }
}

/// The production [`CommitGate`]: each method is one run of the
/// device-owned-epoch rebase loop ([`MlsStateSync::send_commit_gated`] /
/// [`MlsStateSync::ensure_epoch_takeover`]) over the session's shared engine
/// and conversations plane. The backend routes its commit-producing paths here
/// when injected (`set_commit_gate`); with no gate injected, its optimistic
/// paths stand untouched.
pub struct FaunaCommitGate {
    sync: Arc<MlsStateSync>,
    engine: Arc<MlsEngine>,
    send: BackendChannelSend,
    catch_up: BackendCatchUp,
}

impl FaunaCommitGate {
    /// Assemble over the session's parts. `engine` MUST be the same instance the
    /// backend holds (the leg clones the same `Arc` into both) — the loop stages
    /// commits in the engine the backend sends from.
    ///
    /// The conversations plane is no longer passed: [`BackendChannelSend`] takes
    /// it from the backend, which removes the "MUST be the same instance the
    /// backend holds" hazard on that half and, more importantly, makes the
    /// foreign-homed mis-route unrepresentable (see that type's doc).
    pub fn new(
        sync: Arc<MlsStateSync>,
        engine: Arc<MlsEngine>,
        send: BackendChannelSend,
        catch_up: BackendCatchUp,
    ) -> Self {
        Self {
            sync,
            engine,
            send,
            catch_up,
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CommitGate for FaunaCommitGate {
    async fn gated_add_member(
        &self,
        channel: ChannelId,
        key_package_bytes: Vec<u8>,
    ) -> Result<(i64, Vec<u8>), BackendError> {
        self.sync
            .send_commit_gated(&self.engine, &self.send, &self.catch_up, &channel, || {
                self.engine
                    .add_member_staged_from_bytes(&channel, &key_package_bytes)
            })
            .await
            .map_err(gate_error)
    }

    async fn gated_remove_member(
        &self,
        channel: ChannelId,
        leaf: u32,
    ) -> Result<i64, BackendError> {
        self.sync
            .send_commit_gated(&self.engine, &self.send, &self.catch_up, &channel, || {
                self.engine
                    .remove_member_staged(&channel, leaf)
                    .map(|commit| (commit, ()))
            })
            .await
            .map(|(seq, ())| seq)
            .map_err(gate_error)
    }

    async fn gated_set_room_policy(
        &self,
        channel: ChannelId,
        rebuild: fauna_conversations::backend::RoomPolicyRebuild,
    ) -> Result<i64, BackendError> {
        self.sync
            .send_commit_gated(&self.engine, &self.send, &self.catch_up, &channel, || {
                // Re-derived on every attempt from the policy the channel holds
                // NOW — after a catch-up folded another governor's change the
                // version must still advance by exactly one, or every other
                // member refuses what this device would merge
                // (`RoomPolicyRebuild`).
                let current = match self.engine.room_policy(&channel) {
                    Some(Ok(current)) => current,
                    Some(Err(e)) => return Err(e),
                    None => {
                        return Err(fauna_mls::error::MlsError::PolicyViolation(
                            "a policy-less room carries no policy and cannot acquire one in place"
                                .into(),
                        ));
                    }
                };
                let next = rebuild(&current)?;
                self.engine
                    .set_room_policy_staged(&channel, &next)
                    .map(|commit| (commit, ()))
            })
            .await
            .map(|(seq, ())| seq)
            .map_err(gate_error)
    }

    async fn ensure_takeover(&self, channel: ChannelId) -> Result<(), BackendError> {
        self.sync
            .ensure_epoch_takeover(&self.engine, &self.send, &self.catch_up, &channel)
            .await
            .map_err(gate_error)
    }

    async fn resync_channel(
        &self,
        channel: ChannelId,
        logged_commit_epoch: u64,
        logged_commit_hash: [u8; 32],
    ) -> Result<(), BackendError> {
        let Some(restored) = self
            .sync
            .resync_provider(&self.engine)
            .await
            .map_err(|e| BackendError::Internal(format!("own-leaf resync: {e}")))?
        else {
            // An own-leaf commit exists on the log but no replica is stored —
            // the other device has not uploaded yet (or the nest served
            // nothing). Nothing to restore; the next resync trigger or launch
            // load heals it.
            return Err(BackendError::Internal(
                "own-leaf resync: no provider replica stored yet".into(),
            ));
        };
        // Crash-window convergence (design §3 takeover crash-safety): a replica
        // restored at exactly the logged commit's epoch is the other device's
        // step-2 upload — its still-pending commit is the logged record, so
        // merging it advances us onto the logged epoch. A restored epoch past the
        // commit's is the normal post-merge upload — already converged.
        match self.engine.current_epoch(&channel) {
            Ok(epoch) if epoch == logged_commit_epoch => {
                // Resync-identity hardening: merge the restored step-2 pending
                // ONLY when its stamped identity matches the logged commit — else
                // a malicious nest that rolled the replica back to a superseded
                // step-2 upload (a genuine own-device pending that never landed on
                // the log) would fork this device onto a never-landed commit.
                match restored.pending_commit_hash(&channel) {
                    Some(h) if h == logged_commit_hash => {
                        // The restored pending IS the logged commit — converge.
                        let _ = self.engine.merge_pending_commit(&channel);
                    }
                    Some(_) => {
                        // Identity mismatch: the restored pending is NOT the logged
                        // commit. Do not merge — that would fork onto a commit the
                        // authoritative log never carried. Leave the inert pending
                        // staged: openmls blocks a takeover from staging over it, so
                        // this device cannot send (and so cannot fork); a later
                        // honest resync / launch `load()` swaps the whole provider
                        // KV and converges. Provably convergent-only.
                        tracing::warn!(
                            logged_commit_epoch,
                            "own-leaf resync: restored pending's commit identity ≠ the logged commit (possible malicious-nest rollback); not merging — awaiting the authoring device's post-merge replica"
                        );
                    }
                    None => {
                        // No commit identity: the engine stamps one whenever it
                        // stages a commit, so a current replica carrying a pending
                        // always names it — `None` means nothing staged to merge,
                        // or a pending whose identity cannot be checked. Either
                        // way, never merge. (The epoch-only merge a stamp-less
                        // replica once got was retired by the compat-remnant
                        // sweep — `version-compatibility.md` § Dimension 2,
                        // program 4.)
                        tracing::warn!(
                            logged_commit_epoch,
                            "own-leaf resync: restored replica carries no commit identity for this channel; not merging"
                        );
                    }
                }
            }
            Ok(epoch) if epoch < logged_commit_epoch => {
                // The nest served a replica older than the log (stale-replica
                // residual (d)) — restored, but not yet at the logged epoch.
                tracing::warn!(
                    restored_epoch = epoch,
                    logged_commit_epoch,
                    "own-leaf resync restored a replica behind the logged commit; awaiting the other device's next upload"
                );
            }
            _ => {}
        }
        // Whatever the restored state, the epoch was authored by the other
        // device — this one must take over before its next application send.
        self.sync.mark_foreign_epoch(&channel);
        Ok(())
    }

    fn note_foreign_commit(&self, _channel: ChannelId) {
        // Deliberately does NOT revoke epoch authorship. Every commit that reaches
        // this notification is another MEMBER's (a same-account device's commit
        // classifies as the `OwnLeafCommit` resync signal and never gets here),
        // and another member's commit does not touch this device's own leaf:
        // per `devices.md` § Cross-device MLS group-state sync the send right
        // rides the device's own leaf's latest commit, and the fresh-chain
        // property holds because every epoch gives every member a fresh sender
        // chain while the own-leaf rule keeps at most ONE of this account's
        // devices sending per own-leaf lineage (a sibling device starts with
        // `authored == false` and must take over first; the own-leaf resync
        // arm above still revokes on a sibling's commit). Revoking here
        // instead made two actively-sending MEMBERS take the epoch over from
        // each other on every pump pass — an endless commit ping-pong that
        // kept the folder channel's epoch racing ahead of every in-flight
        // application envelope (the share plane's advertisement never
        // decrypted; measured 2026-08-24, `--app linux`).
    }
}

/// The production [`ChannelCursor`]: the slice-5 leg injects it on the backend
/// (`set_channel_cursor`) so the background inbound poll resumes each channel
/// from the [`MlsStateSync`] processed-seq cursor — seeded on `load` from each
/// `history/<ch>` replica's `watermark`, advanced as the poll folds records, and
/// snapshotted back by the next history save (design §5). The cross-device
/// successor to the loop's caller-owned `after_seq`: without it a restored device
/// re-walks the whole channel log from `0` (own messages skip-decrypt, foreign
/// commits re-`process`), which mis-orders and double-classifies pre-restore
/// history. Shares the **same** `Arc<MlsStateSync>` the [`FaunaCommitGate`] holds,
/// so the poll's advances and the gate's rebase advances land on one cursor.
pub struct MlsSyncCursor {
    sync: Arc<MlsStateSync>,
}

impl MlsSyncCursor {
    /// Wrap the session's `MlsStateSync` (the same `Arc` handed to
    /// [`FaunaCommitGate::new`]). Inject the result via
    /// `FaunaMlsBackend::set_channel_cursor` after `sync.load()`.
    pub fn new(sync: Arc<MlsStateSync>) -> Self {
        Self { sync }
    }
}

impl ChannelCursor for MlsSyncCursor {
    fn resume_seq(&self, channel: &ChannelId) -> i64 {
        self.sync.processed_seq(channel)
    }

    fn advance(&self, channel: &ChannelId, seq: i64) {
        self.sync.advance_processed_seq(channel, seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MlsReplicaTransport, MlsTransportError, PATH_PROVIDER, PutOutcome};
    use crate::test_conv::{ConvNest, EventLog, FakeConvNest, block_on};
    use fauna_conversations::backend::ConversationsRpc;
    use fauna_conversations::thread::ThreadId;
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::types::ChannelEnvelope;
    use fauna_protocol::mls_replica::ReplicaBase;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Trivial CAS-enforcing in-memory replica transport — no wire types (the
    /// wire-level encode + conflict classification are covered by the
    /// `store`/`sync`/`commit_gate` suites over `rpc_transport_{get,put}`).
    /// Pushes a `"provider-put"` event per accepted `provider` put so the
    /// crash-safety ordering stays observable through the production gate.
    /// `Clone` shares the underlying store — two clones model one nest serving
    /// two devices of the same identity.
    #[derive(Default, Clone)]
    struct MemReplica {
        stored: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        events: EventLog,
    }

    #[async_trait]
    impl MlsReplicaTransport for MemReplica {
        async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            Ok(self.stored.lock().unwrap().get(&path).cloned())
        }
        async fn put(
            &self,
            path: String,
            blob: Vec<u8>,
            base: ReplicaBase,
        ) -> Result<PutOutcome, MlsTransportError> {
            let mut map = self.stored.lock().unwrap();
            let current = map.get(&path).map(|b| *blake3::hash(b).as_bytes());
            let matches = match base {
                ReplicaBase::Absent => current.is_none(),
                ReplicaBase::Hash(h) => current == Some(h),
            };
            if !matches {
                return Ok(PutOutcome::Conflict);
            }
            if path == PATH_PROVIDER {
                self.events.lock().unwrap().push("provider-put");
            }
            map.insert(path, blob);
            Ok(PutOutcome::Stored)
        }
    }

    fn keypair(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    /// One assembled device: the full production graph the slice-5 leg will
    /// build — backend + manager registered and channel-bound, `MlsStateSync`
    /// loaded, `BackendCatchUp` + [`FaunaCommitGate`] wired per the module-doc
    /// construction order, **and the gate injected** (`set_commit_gate`) so
    /// `poll_inbound_conv`'s Commit arm drives the resync/note paths.
    struct Device {
        sync: Arc<MlsStateSync>,
        gate: Arc<FaunaCommitGate>,
        backend: Arc<FaunaMlsBackend>,
        manager: Arc<ConversationsManager>,
    }

    fn assemble_device(
        engine: &Arc<MlsEngine>,
        seed: u8,
        handle: &str,
        conv_nest: &Arc<FakeConvNest>,
        replica: &MemReplica,
        channel: fauna_mls::types::ChannelId,
    ) -> Device {
        assemble_device_for(engine, seed, handle, conv_nest, replica, channel, false)
    }

    fn assemble_device_for(
        engine: &Arc<MlsEngine>,
        seed: u8,
        handle: &str,
        conv_nest: &Arc<FakeConvNest>,
        replica: &MemReplica,
        channel: fauna_mls::types::ChannelId,
        as_folder: bool,
    ) -> Device {
        let conv: Arc<dyn ConversationsRpc> = Arc::new(ConvNest(conv_nest.clone()));
        let manager = ConversationsManager::new();
        let backend = Arc::new(FaunaMlsBackend::new(
            engine.clone(),
            conv.clone(),
            handle,
            engine.identity_actor_id(),
        ));
        manager.register_backend(backend.clone());
        if as_folder {
            // A folder channel has NO bound thread — the shape that forces the
            // catch-up's folder dispatch (`poll_inbound_conv` would no-op on it).
            //
            // Deliberately does NOT call `mark_folder_channel`: that is the
            // **owner's** production shape, and the owner never marks (it creates
            // the group through `FolderGroupCrypto for Arc<MlsEngine>`, which
            // cannot reach the backend). A fixture that marked would manufacture a
            // routing input production never produces — which is exactly how the
            // marker-based dispatch stayed green while being dead code in the
            // field. Leaving the channel unmarked and unbound is the real shape:
            // an engine group with no chat thread, which `is_folder_rail`
            // derives from the engine.
        } else {
            // The inbound driver routes by the thread↔channel binding; a Commit-only
            // log never writes the thread itself, so a synthetic id suffices.
            backend.bind_channel(ThreadId(format!("gate-test-{handle}-{seed}")), channel);
        }

        let sync = Arc::new(MlsStateSync::new(Box::new(replica.clone()), &keypair(seed)));
        block_on(sync.load()).unwrap(); // lift the provider-put gate
        let catch_up = BackendCatchUp::new(&backend, &manager);
        let gate = Arc::new(FaunaCommitGate::new(
            sync.clone(),
            engine.clone(),
            BackendChannelSend::new(&backend),
            catch_up,
        ));
        backend.set_commit_gate(gate.clone());
        Device {
            sync,
            gate,
            backend,
            manager,
        }
    }

    /// A two-member group (Alice runs the gate; Bob is the other member) with
    /// Alice's device fully assembled.
    struct GateWorld {
        alice: Arc<MlsEngine>,
        bob: Arc<MlsEngine>,
        channel: fauna_mls::types::ChannelId,
        sync: Arc<MlsStateSync>,
        gate: Arc<FaunaCommitGate>,
        conv_nest: Arc<FakeConvNest>,
        events: EventLog,
        replica: MemReplica,
        backend: Arc<FaunaMlsBackend>,
        manager: Arc<ConversationsManager>,
    }

    fn gate_world() -> GateWorld {
        let events: EventLog = Default::default();
        let conv_nest = Arc::new(FakeConvNest::with_events(events.clone()));
        let replica = MemReplica {
            events: events.clone(),
            ..Default::default()
        };
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        let device = assemble_device(&alice, 1, "alice", &conv_nest, &replica, channel);
        GateWorld {
            alice,
            bob,
            channel,
            sync: device.sync,
            gate: device.gate,
            conv_nest,
            events,
            replica,
            backend: device.backend,
            manager: device.manager,
        }
    }

    fn commit_sends(events: &EventLog) -> usize {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == "commit-send")
            .count()
    }

    /// A foreign commit as it would sit in the channel log: Bob self-updates and
    /// merges locally, and the wire-wrapped commit is injected out-of-band.
    fn inject_bob_commit(w: &GateWorld) -> i64 {
        let c = w.bob.self_update(&w.channel).unwrap();
        w.bob.merge_pending_commit(&w.channel).unwrap();
        w.conv_nest.inject(
            &w.channel.to_string(),
            ChannelEnvelope::Commit(c).to_bytes().unwrap(),
        )
    }

    /// The production gate drives a real add: the commit merges (epoch bump),
    /// authorship is recorded, the newcomer joins from the returned Welcome
    /// bytes, and the design-§3 crash-safety order holds end-to-end.
    #[test]
    fn gated_add_member_admits_newcomer_and_marks_authored() {
        let w = gate_world();
        let carol = MlsEngine::new_in_memory(keypair(3)).unwrap();
        let carol_kp = carol.generate_key_packages_bytes(1).unwrap().remove(0);
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();

        let (seq, welcome) = block_on(w.gate.gated_add_member(w.channel, carol_kp)).unwrap();

        assert_eq!(seq, 1, "sole writer accepts at seq 1");
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before + 1,
            "the accepted add merged"
        );
        assert!(w.sync.authored_current_epoch(&w.channel));
        assert_eq!(
            carol.join_from_welcome_bytes(&welcome).unwrap(),
            w.channel,
            "the returned Welcome admits the newcomer to the same channel"
        );
        assert_eq!(
            *w.events.lock().unwrap(),
            vec!["provider-put", "commit-send", "provider-put"],
            "design §3 order survives the production gate"
        );
    }

    /// The production gate drives a real remove: the commit merges, authorship
    /// is recorded, and the evicted member — after processing the commit — can
    /// no longer operate on the group.
    #[test]
    fn gated_remove_member_evicts_peer() {
        let w = gate_world();
        let bob_leaf = w
            .alice
            .find_leaf_by_identity(&w.channel, &w.bob.identity_actor_id())
            .expect("bob is a member");
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();

        let seq = block_on(w.gate.gated_remove_member(w.channel, bob_leaf)).unwrap();

        assert_eq!(seq, 1);
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before + 1,
            "the accepted remove merged"
        );
        assert!(w.sync.authored_current_epoch(&w.channel));

        // Bob processes his own removal from the log; the group is dead to him.
        let log = w.conv_nest.fetch_after(&w.channel.to_string(), 0);
        let Ok(ChannelEnvelope::Commit(cb)) = ChannelEnvelope::from_bytes(&log[0].1) else {
            panic!("the logged envelope is the removal commit");
        };
        let _ = w.bob.process_commit(&w.channel, &cb);
        assert!(
            w.bob.self_update(&w.channel).is_err(),
            "the evicted member cannot commit on the group"
        );
    }

    /// The rebase path through the PRODUCTION catch-up: a foreign commit lands
    /// first, the gate-send is stale, and `BackendCatchUp` upgrades its `Weak`s
    /// and drains the log via `poll_inbound_conv` — then the rebuilt takeover
    /// lands on the advanced epoch.
    #[test]
    fn stale_rejection_rebases_through_backend_catch_up() {
        let w = gate_world();
        let injected_seq = inject_bob_commit(&w);
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();

        block_on(w.gate.ensure_takeover(w.channel)).unwrap();

        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before + 2,
            "foreign commit processed via poll_inbound_conv, then own takeover merged"
        );
        assert!(w.sync.authored_current_epoch(&w.channel));
        assert_eq!(
            w.sync.processed_seq(&w.channel),
            injected_seq,
            "the loop advanced its cursor with the catch-up's returned seq"
        );
    }

    /// `ensure_takeover` posts exactly one self-update, then no-ops while the
    /// device still owns the epoch.
    #[test]
    fn takeover_posts_once_then_noops() {
        let w = gate_world();
        block_on(w.gate.ensure_takeover(w.channel)).unwrap();
        assert_eq!(commit_sends(&w.events), 1, "one takeover commit");
        block_on(w.gate.ensure_takeover(w.channel)).unwrap();
        assert_eq!(
            commit_sends(&w.events),
            1,
            "no second takeover once the epoch is owned"
        );
    }

    /// **The share
    /// plane's advertisement door posts a `Commit`**.
    ///
    /// `send_share_endpoints` and its two siblings `send_custody_payload` /
    /// `send_custody_receipt` all funnel through `post_app_message`, which —
    /// with a `CommitGate` injected — runs the device-owned-epoch takeover
    /// (`devices.md` § Cross-device MLS group-state sync: "a device may send
    /// application traffic on a channel only in an epoch whose latest own-leaf
    /// commit it authored"). A **member** never authored the folder epoch (the
    /// owner did, when it added them), so the first advertisement pass stages a
    /// self-`Update` and posts it. That takeover is exactly what the
    /// 2026-08-24 roster-membership commit admission exists to let through
    /// (`federation.md` § Cross-nest shared folders + channel append); without it a nest
    /// refuses the Commit outright, killing the member leg — the
    /// linux red.
    ///
    /// This asserts the door's mechanism (the Commit + epoch bump), not any
    /// nest verdict: the fake conv plane applies no commit gate at all. Bob is
    /// the member; Alice authored the current epoch by creating the group.
    #[test]
    fn a_members_share_endpoint_advertisement_posts_a_commit() {
        let w = gate_world();
        let bob_replica = MemReplica {
            events: w.events.clone(),
            ..Default::default()
        };
        // Bob's own device, folder-shaped: an engine group with no bound chat
        // thread — the real member shape for a shared set's channel.
        let bob_dev = assemble_device_for(
            &w.bob,
            2,
            "bob",
            &w.conv_nest,
            &bob_replica,
            w.channel,
            true,
        );
        assert!(
            !bob_dev.sync.authored_current_epoch(&w.channel),
            "precondition: the member did not author the folder epoch — the owner did"
        );
        let commits_before = commit_sends(&w.events);
        let epoch_before = w.bob.current_epoch(&w.channel).unwrap();

        block_on(
            bob_dev
                .backend
                .send_share_endpoints(&w.channel.to_string(), b"endpoints".to_vec()),
        )
        .expect("the fake conv plane has no claimant gate, so the door completes here");

        assert_eq!(
            commit_sends(&w.events) - commits_before,
            1,
            "the advertisement door posted a Commit — the member takeover the \
             2026-08-24 roster-membership admission lets through (and a pre-\
             admission nest refuses — the linux red)"
        );
        assert_eq!(
            w.bob.current_epoch(&w.channel).unwrap(),
            epoch_before + 1,
            "and it advanced the epoch — so \"no Commit, no epoch change\" is false \
             on both halves"
        );
    }

    /// The refusal shape: when the nest refuses a member's Commit (a
    /// terminal refusal, e.g. a non-claimant's Commit), a member's advertisement **fails
    /// outright** and nothing reaches the channel — no Commit, and no
    /// advertisement either.
    ///
    /// This was a linux red end-to-end (`--app linux`
    /// reported **0 item rows** — the advertisement never left the device);
    /// it stays pinned because a refused takeover degrades to
    /// exactly this refused-and-retrying behavior, which must stay an error
    /// the caller sees, never a silent "sent". The sibling test
    /// [`a_members_share_endpoint_advertisement_posts_a_commit`] shows the
    /// mechanism (the takeover); this one shows the consequence.
    #[test]
    fn a_member_cannot_advertise_when_the_nest_refuses_its_takeover() {
        let w = gate_world();
        let bob_replica = MemReplica {
            events: w.events.clone(),
            ..Default::default()
        };
        let bob_dev = assemble_device_for(
            &w.bob,
            2,
            "bob",
            &w.conv_nest,
            &bob_replica,
            w.channel,
            true,
        );
        // The nest is now the real one: Commits from this member are refused.
        w.conv_nest.refuse_commits();
        let log_before = w.conv_nest.fetch_after(&w.channel.to_string(), 0).len();

        let err = block_on(
            bob_dev
                .backend
                .send_share_endpoints(&w.channel.to_string(), b"endpoints".to_vec()),
        )
        .expect_err("the takeover Commit is refused, so the advertisement cannot go out");

        assert!(
            format!("{err:?}").contains("claimant"),
            "the failure is the claimant gate's refusal, surfaced verbatim: {err:?}"
        );
        assert_eq!(
            w.conv_nest.fetch_after(&w.channel.to_string(), 0).len(),
            log_before,
            "nothing reached the channel — not the Commit, and not the advertisement \
             the member actually wanted to send"
        );
    }

    /// A refused takeover must leave **no pending commit in the saved
    /// replica**. Design step 2 deliberately CAS-puts a replica carrying the
    /// staged pending, so a crash between send and merge still converges — but
    /// a commit the nest *refused* can never reach the log, so that replica
    /// would be a lie. `send_commit_gated`'s `unstage_saved_pending` re-seals
    /// the cleared state on both failure exits.
    ///
    /// Why it is load-bearing: without the unstage, a relaunch's
    /// `restore_and_wire` brings the pending back and `resync_channel`'s own
    /// note records the consequence — "openmls blocks a takeover from staging
    /// over it, so this device cannot send". A member wedged that way would
    /// have stayed wedged *after* the 2026-08-24 commit-admission widening,
    /// reading as "the fix didn't work". Red-verified by construction: this
    /// test asserted the opposite
    /// (`is_some`) and passed before the unstage landed.
    #[test]
    fn a_refused_takeover_leaves_no_pending_commit_in_the_saved_replica() {
        let w = gate_world();
        let bob_replica = MemReplica {
            events: w.events.clone(),
            ..Default::default()
        };
        let bob_dev = assemble_device_for(
            &w.bob,
            2,
            "bob",
            &w.conv_nest,
            &bob_replica,
            w.channel,
            true,
        );
        w.conv_nest.refuse_commits();

        block_on(
            bob_dev
                .backend
                .send_share_endpoints(&w.channel.to_string(), b"endpoints".to_vec()),
        )
        .expect_err("refused");

        // What a relaunch would restore: reload this device's own replica and
        // ask it directly what it is carrying.
        let restored = block_on(bob_dev.sync.load())
            .unwrap()
            .provider
            .expect("step 2 uploaded a provider before the refused gate-send");

        assert!(
            restored.pending_commit_hash(&w.channel).is_none(),
            "the refused takeover's pending was unstaged from the replica — a \
             relaunched member must not restore a commit the nest already refused"
        );
    }

    /// **The takeover of a FOREIGN-HOMED channel lands on the channel's home
    /// nest, not the member's own** (`federation.md` § Cross-nest shared
    /// folders + channel append — *"a foreign member's device-owned-epoch
    /// takeover must land"*).
    ///
    /// Drives the real share-plane door end to end — `send_share_endpoints` →
    /// `post_app_message` → `gate.ensure_takeover` — on a channel whose home is
    /// another nest, exactly as `ingest_folder_welcome` records it for a
    /// cross-nest shared-folder member.
    ///
    /// This is the shape nothing in this crate could express before: the fake
    /// modelled ONE nest and failed loud on any relay, so a commit that
    /// blackholed into the member's own log looked like a clean success. With
    /// the home nest registered as a peer, both routings "work" and the test
    /// asks the only question that separates them — **which log holds the
    /// Commit**. Against the pre-fix gate (a bare `conv.channel_send`) the
    /// takeover lands on `w.conv_nest` and the home nest sees only the
    /// advertisement: co-members, reading the home log, are sealed an epoch
    /// behind and cannot decrypt it.
    #[test]
    fn a_foreign_homed_gated_takeover_lands_on_the_home_nest() {
        let w = gate_world();
        let home = Arc::new(FakeConvNest::with_events(w.events.clone()));
        w.conv_nest
            .register_peer_nest("https://home.example", home.clone());

        let bob_replica = MemReplica {
            events: w.events.clone(),
            ..Default::default()
        };
        let bob_dev = assemble_device_for(
            &w.bob,
            2,
            "bob",
            &w.conv_nest,
            &bob_replica,
            w.channel,
            true,
        );
        // What a cross-nest Welcome records (`fauna_mls.rs::ingest_folder_welcome`).
        bob_dev
            .backend
            .record_channel_home(w.channel, "https://home.example");

        block_on(
            bob_dev
                .backend
                .send_share_endpoints(&w.channel.to_string(), b"endpoints".to_vec()),
        )
        .expect("the share-plane advertisement lands over the relay");

        assert_eq!(
            home.commits_on(&w.channel),
            1,
            "the device-owned-epoch takeover must reach the nest that HOMES the \
             channel — that is the log every co-member fetches"
        );
        assert_eq!(
            w.conv_nest.records_on(&w.channel),
            0,
            "nothing may be appended to the member's OWN nest for a foreign-homed \
             channel: that log is the send blackhole, and MLS ciphertext resting \
             there has no reader and no cleanup path"
        );
        assert_eq!(
            home.records_on(&w.channel),
            2,
            "the takeover and the advertisement it was taken for land on the SAME \
             log, in that order — the advertisement is sealed at the epoch the \
             takeover created"
        );
    }

    /// The negative half: a **same-nest** channel still sends through
    /// `channel.send`. The routing fix must not turn every ordinary member into
    /// a federation relay caller — with no home recorded the fake's relay arm
    /// fails loud, so a mis-pick in that direction cannot pass either.
    #[test]
    fn a_same_nest_gated_takeover_still_lands_locally() {
        let w = gate_world();
        w.conv_nest.register_peer_nest(
            "https://home.example",
            Arc::new(FakeConvNest::with_events(w.events.clone())),
        );

        let bob_replica = MemReplica {
            events: w.events.clone(),
            ..Default::default()
        };
        let bob_dev = assemble_device_for(
            &w.bob,
            2,
            "bob",
            &w.conv_nest,
            &bob_replica,
            w.channel,
            true,
        );
        // No `record_channel_home` — the common case.

        block_on(
            bob_dev
                .backend
                .send_share_endpoints(&w.channel.to_string(), b"endpoints".to_vec()),
        )
        .expect("a same-nest advertisement needs no relay");

        assert_eq!(
            w.conv_nest.commits_on(&w.channel),
            1,
            "a channel with no recorded home commits on its own nest, unchanged"
        );
    }

    /// LEAD ② blocker (a) (`devices.md` § Implementation status, item 2): a
    /// gated remove on a **folder** channel — which has NO bound thread — must
    /// rebase through the catch-up's folder dispatch. `poll_inbound_conv`
    /// returns without touching a record for a thread-less channel, so before
    /// the dispatch this exact sequence spun to `RetriesExhausted` (every round
    /// stale, cursor never advancing). Three members so the group survives the
    /// removal: Bob (foreign committer) races a self-update in first; Alice's
    /// gated remove of Carol goes stale, catches up over Bob's commit via
    /// `poll_inbound_folder`, then lands on the advanced epoch.
    #[test]
    fn gated_remove_on_folder_channel_rebases_through_folder_catch_up() {
        let events: EventLog = Default::default();
        let conv_nest = Arc::new(FakeConvNest::with_events(events.clone()));
        let replica = MemReplica {
            events: events.clone(),
            ..Default::default()
        };
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let carol = Arc::new(MlsEngine::new_in_memory(keypair(3)).unwrap());
        let bob_kp = bob.generate_key_packages(1).unwrap();
        let carol_kp = carol.generate_key_packages(1).unwrap();
        let kps: Vec<_> = bob_kp.into_iter().chain(carol_kp).collect();
        let (channel, welcome) = alice.create_group(&kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        let device = assemble_device_for(&alice, 1, "alice", &conv_nest, &replica, channel, true);
        // The OWNER's production shape: an engine group with no bound chat thread
        // and — decisively — NO in-memory marker. The marker is the recipient's
        // join bookkeeping and empties on relaunch; the owner never sets it. The
        // dispatch must derive the rail from the engine, so this routes correctly
        // with the marker absent.
        assert!(
            !device.backend.is_folder_channel(&channel),
            "the owner never marks — a fixture that marked would manufacture the routing state"
        );
        assert!(
            device.backend.is_folder_rail(&channel),
            "and the rail is still derived from the engine (thread-less MLS group)"
        );

        // Bob's commit lands on the log first — Alice's first gate-send is stale.
        let c = bob.self_update(&channel).unwrap();
        bob.merge_pending_commit(&channel).unwrap();
        let injected_seq = conv_nest.inject(
            &channel.to_string(),
            ChannelEnvelope::Commit(c).to_bytes().unwrap(),
        );
        let epoch_before = alice.current_epoch(&channel).unwrap();

        let carol_leaf = alice
            .find_leaf_by_identity(&channel, &carol.identity_actor_id())
            .expect("carol is a member");
        let seq = block_on(device.gate.gated_remove_member(channel, carol_leaf))
            .expect("rebases through the folder catch-up instead of exhausting retries");

        assert!(seq > injected_seq, "accepted after the foreign commit");
        assert_eq!(
            alice.current_epoch(&channel).unwrap(),
            epoch_before + 2,
            "foreign commit processed via poll_inbound_folder, then own remove merged"
        );
        assert!(
            alice
                .find_leaf_by_identity(&channel, &carol.identity_actor_id())
                .is_none(),
            "carol is removed from the group"
        );
        assert_eq!(
            device.sync.processed_seq(&channel),
            injected_seq,
            "the loop advanced its cursor with the folder catch-up's returned seq"
        );
    }

    /// The
    /// chat-rail half of the catch-up's Rule-2 guard, and the reason it exists.
    ///
    /// An own-leaf commit sits on the chat log that this device cannot resync
    /// past (the gate-less sibling shape: the optimistic membership path merges
    /// without ever CAS-putting the provider replica, so there is no newer
    /// replica to converge onto — `apply_inbound_commit` reports
    /// `Stalled { future_epoch: false }`). A gated `remove_participant` now goes
    /// stale, and its catch-up must **fail** rather than hand back a baseline
    /// past that unincorporated epoch transition.
    ///
    /// **What this pins is the baseline**, which is the defect's name: before the
    /// guard, `poll_inbound_conv` returned a bare count and consumed the record,
    /// so `advance_processed_seq` moved `expect_no_commit_since` *past* an epoch
    /// transition the engine never applied. That baseline is what lets the nest
    /// see "no commit since" and accept a rebuilt commit for an epoch the group
    /// has already left — every other member quiet-skipping it as
    /// `PastEpochCommit` while the sender merges and reports success, so a
    /// `remove_participant` "succeeds" with the removed member still in the live
    /// group. Assert the baseline invariant directly (`processed_seq` never
    /// reaches the commit's seq) rather than the downstream acceptance: in *this*
    /// fixture the round-2 rebuild happens to trip an openmls pending-commit
    /// defense first, so an `is_err()` alone would not discriminate — the old
    /// code failed too, just for the wrong reason and after violating Rule 2.
    /// Failing loudly at the catch-up (pending cleared, retryable) is the only
    /// safe outcome; the heal is the next poll/resync pass.
    #[test]
    fn gated_remove_over_an_unresyncable_own_leaf_commit_errors_instead_of_forking() {
        let events: EventLog = Default::default();
        let conv_nest = Arc::new(FakeConvNest::with_events(events.clone()));
        let replica = MemReplica {
            events: events.clone(),
            ..Default::default()
        };
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let carol = Arc::new(MlsEngine::new_in_memory(keypair(3)).unwrap());
        let bob_kp = bob.generate_key_packages(1).unwrap();
        let carol_kp = carol.generate_key_packages(1).unwrap();
        let kps: Vec<_> = bob_kp.into_iter().chain(carol_kp).collect();
        let (channel, welcome) = alice.create_group(&kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        // A thread-bound CHAT channel — the conv rail, not the folder rail.
        let device = assemble_device(&alice, 1, "alice", &conv_nest, &replica, channel);
        assert!(
            !device.backend.is_folder_rail(&channel),
            "a thread-bound chat channel — this exercises the conv dispatch"
        );

        // Alice's own commit lands on the log with no pending to merge and no
        // newer replica to resync onto: unincorporated, and unhealable this pass.
        let own = alice.self_update(&channel).unwrap();
        alice.clear_pending_commit(&channel).unwrap();
        let injected_seq = conv_nest.inject(
            &channel.to_string(),
            ChannelEnvelope::Commit(own).to_bytes().unwrap(),
        );
        let epoch_before = alice.current_epoch(&channel).unwrap();

        let carol_leaf = alice
            .find_leaf_by_identity(&channel, &carol.identity_actor_id())
            .expect("carol is a member");
        let err = block_on(device.gate.gated_remove_member(channel, carol_leaf))
            .expect_err("the catch-up must fail rather than rebase past an unincorporated commit");

        // THE invariant (Rule 2): the rebase baseline never reaches the commit
        // the engine could not incorporate. This is the assertion that goes red
        // on the unguarded catch-up — the one the fork was built on.
        assert!(
            device.sync.processed_seq(&channel) < injected_seq,
            "rebase baseline advanced to {} — past the unincorporated commit at {injected_seq} \
             (Rule 2 violated: a rebuilt commit could now be accepted for an epoch the group \
             has left)",
            device.sync.processed_seq(&channel)
        );
        assert_eq!(
            alice.current_epoch(&channel).unwrap(),
            epoch_before,
            "no forked commit was merged — the device stays at its own epoch"
        );
        assert!(
            alice
                .find_leaf_by_identity(&channel, &carol.identity_actor_id())
                .is_some(),
            "carol is NOT reported removed: a failed remove must not claim success while she \
             keeps group membership"
        );
        assert!(
            format!("{err}").contains("stalled"),
            "and it fails *at the stall*, not incidentally downstream, got: {err}"
        );
    }

    /// Finding 4, slice-3 half: a device that was
    /// **removed** while the group concurrently advanced must stay unusable —
    /// its transient/resurrected local state can never land a commit past its
    /// MLS removal. The rebase makes this structural: the stale rejection
    /// forces the device to process the intervening records first, its own
    /// removal included, after which the rebuild fails in the engine (the group
    /// is inactive) — it never force-merges onto the post-removal epoch.
    /// Slice 4c end-to-end (design §3 "own-leaf foreign commit = resync
    /// signal"): the user's second device A2 (same identity, replica-restored)
    /// posts a gated takeover; device A1's poll meets the own-leaf commit it
    /// did not author, and the gate resyncs — A1 refetches A2's post-merge
    /// replica, lands on A2's epoch, and its authorship is cleared so its next
    /// send takes over first.
    /// `devices.md` § Cross-device MLS group-state sync (:887-888): *"no device
    /// ever encrypts with a sender ratchet it loaded — every launch and resync
    /// takes the epoch over before its first send"*. The **launch** half has
    /// always held, structurally: `load()` builds a fresh `authored` map and
    /// `authored_current_epoch` reads `unwrap_or(false)`, so every channel is
    /// un-authored until its own takeover. This pins the **resync** half, which
    /// did not hold until the fix this test was written against.
    ///
    /// The asymmetry is the whole defect: `resync_provider` restores through
    /// `ProviderReplica::restore_into`, which swaps the engine's **whole**
    /// provider KV (`state_replica.rs` -> `restore_unconditionally` ->
    /// `engine.restore_from_provider_storage(values, &group_ids)` — all values,
    /// all group ids), while the caller revokes the send right for the **one**
    /// channel whose own-leaf commit triggered it ([`resync_channel`] ->
    /// `mark_foreign_epoch(&channel)`). So every *other* channel on the leaf
    /// kept `authored = true` over a sender ratchet that had just been rewound,
    /// and its next application send reused a generation the peer had already
    /// consumed — nonce reuse, two plaintexts under one AEAD key, both durable
    /// in the nest's channel log.
    ///
    /// The FIRST post-resync assertion is on the **send right**, not on a
    /// peer's decrypt error, and deliberately so: the send right is the
    /// invariant `devices.md` ratifies and what the fix restores. But a
    /// cleared bookkeeping flag is not yet proof of anything a peer observes —
    /// probe 1's own criterion — so the test goes on to drive a REAL
    /// post-resync send through the lazy takeover and confirm the peer
    /// actually accepts it, plus that the resync itself posts no takeover
    /// storm. `SecretReuseError` at the peer was the finding's own measured
    /// symptom; pinning it directly is what closes the loop from "the flag
    /// moved" to "the wire behavior changed".
    #[test]
    fn a_resync_clears_the_send_right_on_every_channel_not_just_the_trigger() {
        use fauna_core::data::Timestamp;
        use fauna_mls::types::{ChannelMessage, ChannelMessageBody};

        let w = gate_world();

        // A SECOND channel on the same leaf — the one no resync is triggered
        // for, and the one the whole-KV swap silently rewinds.
        let bob_kps = w.bob.generate_key_packages(1).unwrap();
        let (other, welcome) = w.alice.create_group(&bob_kps).unwrap();
        w.bob.join_from_welcome(welcome).unwrap();
        // A1 authored `other`'s current epoch — it created the group. Recorded
        // here the way `commit_gate` records it after any accepted own commit.
        w.sync.mark_authored(&other);
        assert!(
            w.sync.authored_current_epoch(&other),
            "precondition: A1 holds the send right on the untriggered channel"
        );

        // A1 uploads the provider AFTER `other` exists but BEFORE it ever sends
        // on it, so the snapshot A2 restores from — and the one that comes back
        // to A1 on resync — carries `other` at its unused, pre-send position.
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();

        // A1 sends a REAL application message on `other`, after the snapshot
        // above — bob genuinely consumes this generation, so a later restore
        // to the pre-send snapshot has a real collision waiting, not merely a
        // bookkeeping fiction.
        let pre_resync = ChannelMessage {
            sender: w.alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("pre-resync".into()),
            timestamp: Timestamp::now(),
        };
        let pre_resync_ct = w.alice.encrypt(&other, &pre_resync).unwrap();
        w.bob
            .decrypt(&other, &pre_resync_ct)
            .expect("bob accepts A1's real pre-resync send");

        // Device A2: same identity, fresh engine, restored from the replica.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let d2 = assemble_device(&alice2, 1, "alice", &w.conv_nest, &w.replica, w.channel);
        block_on(d2.sync.load())
            .unwrap()
            .provider
            .expect("A2 bootstraps from A1's replica")
            .restore_into_unchecked(&alice2)
            .unwrap();

        // A2 takes over on `w.channel` only, and uploads. A1 polls that channel,
        // meets the own-leaf commit, and resyncs its WHOLE provider KV.
        block_on(d2.gate.ensure_takeover(w.channel)).unwrap();
        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        assert!(
            !w.sync.authored_current_epoch(&w.channel),
            "the triggering channel's send right is revoked (this half always held)"
        );
        assert!(
            !w.sync.authored_current_epoch(&other),
            "the resync swapped the whole provider KV, so `other`'s sender \
             ratchet was rewound too — its send right must be revoked as well, \
             or A1's next send on `other` reuses a generation the peer already \
             consumed (devices.md:887-888)"
        );

        // No takeover storm: clearing the bookkeeping flag posts nothing by
        // itself — `ensure_epoch_takeover` only fires lazily, on a channel's
        // own NEXT send (`commit_gate.rs:296`) — so `other`'s log is still
        // empty here, even though its send right was just revoked above.
        assert!(
            w.conv_nest.fetch_after(&other.to_string(), 0).is_empty(),
            "the resync itself must not post a self-Update on `other` — only \
             an actual later send may (no eager per-channel takeover storm)"
        );

        // The send right being revoked must translate into an ACCEPTED next
        // send, not merely a cleared bookkeeping flag — probe 1's own
        // criterion (route (a)), genuinely un-pinned until now.
        block_on(w.gate.ensure_takeover(other))
            .expect("the lazy takeover posts a self-Update before the next send");
        let takeover_log = w.conv_nest.fetch_after(&other.to_string(), 0);
        let Ok(ChannelEnvelope::Commit(cb)) =
            ChannelEnvelope::from_bytes(&takeover_log.last().unwrap().1)
        else {
            panic!("the logged envelope is the takeover's own self-Update commit");
        };
        w.bob.process_commit(&other, &cb).unwrap();

        let post_resync = ChannelMessage {
            sender: w.alice.identity_actor_id(),
            sequence: 2,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("post-resync".into()),
            timestamp: Timestamp::now(),
        };
        let post_resync_ct = w.alice.encrypt(&other, &post_resync).unwrap();
        assert!(
            w.bob.decrypt(&other, &post_resync_ct).is_ok(),
            "the peer accepts the post-resync send — the sender ratchet was \
             not reused"
        );
    }

    #[test]
    fn twin_device_takeover_resyncs_the_other_device() {
        let w = gate_world();
        // A1 uploads its provider (the launch save) so A2 can bootstrap.
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();

        // Device A2: same identity, fresh engine, restored from the replica.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let d2 = assemble_device(&alice2, 1, "alice", &w.conv_nest, &w.replica, w.channel);
        block_on(d2.sync.load())
            .unwrap()
            .provider
            .expect("A2 bootstraps from A1's replica")
            .restore_into_unchecked(&alice2)
            .unwrap();

        // A2 takes the epoch over (gated self-update + post-merge upload).
        block_on(d2.gate.ensure_takeover(w.channel)).unwrap();
        let a2_epoch = alice2.current_epoch(&w.channel).unwrap();

        // A1 polls the channel: the own-leaf commit is unprocessable → the
        // typed resync signal → gate.resync_channel refetches + reloads.
        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            a2_epoch,
            "A1 converged onto A2's epoch via the replica resync"
        );
        assert!(
            !w.sync.authored_current_epoch(&w.channel),
            "the resynced epoch is A2's — A1 must take over before its next send"
        );
        // And A1 can commit again: its next takeover lands (the group state is
        // live, not a stale fork).
        block_on(w.gate.ensure_takeover(w.channel)).unwrap();
        assert!(w.sync.authored_current_epoch(&w.channel));
    }

    /// The accounting behind the twin-device **barrier** —
    /// `fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY` /
    /// `FaunaMlsBackend::folded_commits`, which the two cross-device e2e tests
    /// anchor on instead of waiting out poll cycles.
    ///
    /// This is the same journey as
    /// [`twin_device_takeover_resyncs_the_other_device`] read from the observable's
    /// side: A1's count for the channel is **absent (⇒ 0) before** A2's takeover
    /// and **exactly 1 after** the poll folds it in. The exact-1 matters — a
    /// barrier waiting for "> baseline" is only sound if one fold-in produces one
    /// bump, and the resync arm reaches `Advanced` through a different route than
    /// a plain foreign commit does.
    ///
    /// ⚠ This is the pin that makes the e2e barrier mean anything, because the
    /// e2e itself cannot see the difference: A1 can never render A2's message
    /// (one leaf, and a sender cannot decrypt its own application messages), so
    /// there is no GUI-visible consequence of the fold-in to assert against.
    #[test]
    fn a_twin_device_takeover_bumps_the_other_device_folded_commit_count() {
        let w = gate_world();
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();

        assert_eq!(
            w.backend.folded_commits().get(&w.channel.to_string()),
            None,
            "a fresh session has folded nothing in — the barrier's baseline is \
             absent, which a consumer reads as 0"
        );

        // Device A2 bootstraps from the replica and takes the epoch over.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let d2 = assemble_device(&alice2, 1, "alice", &w.conv_nest, &w.replica, w.channel);
        block_on(d2.sync.load())
            .unwrap()
            .provider
            .expect("A2 bootstraps from A1's replica")
            .restore_into_unchecked(&alice2)
            .unwrap();
        block_on(d2.gate.ensure_takeover(w.channel)).unwrap();

        // A1's poll meets the own-leaf commit and resyncs onto A2's epoch.
        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        assert_eq!(
            w.backend.folded_commits().get(&w.channel.to_string()),
            Some(&1),
            "A1 folded A2's takeover in exactly once — the own-leaf resync arm \
             reaches CommitApplyOutcome::Advanced and bumps the count the \
             cross-device e2e barrier polls"
        );
    }

    /// The other half of the barrier's soundness: **re-walking the log must not
    /// manufacture fold-ins.** A commit already incorporated comes back around as
    /// `MlsError::PastEpochCommit` ⇒ `CommitApplyOutcome::Skipped`, which must not
    /// bump — and the chat rail *does* re-walk from 0 on its own (the Rule-2
    /// future-epoch heal), so this is a reachable path, not a hypothetical.
    ///
    /// Without this, a barrier waiting for "count > baseline" could be released by
    /// a heal re-reading history rather than by the peer's new commit — the
    /// pre-trigger world satisfying a post-trigger assertion, which is precisely
    /// the vacuity a count is chosen over a flag to avoid.
    #[test]
    fn re_walking_the_log_does_not_re_count_an_already_folded_commit() {
        let w = gate_world();
        inject_bob_commit(&w);

        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();
        assert_eq!(
            w.backend.folded_commits().get(&w.channel.to_string()),
            Some(&1),
            "the foreign commit advanced A's epoch — one fold-in, one bump"
        );

        // Re-walk the same log from the start, as the Rule-2 heal does.
        let mut rewound = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut rewound,
            0,
        ))
        .unwrap();
        assert_eq!(
            w.backend.folded_commits().get(&w.channel.to_string()),
            Some(&1),
            "the re-walk re-read a commit already held (PastEpochCommit ⇒ \
             Skipped) — the count must NOT move, or a heal could release a \
             barrier the peer's commit never reached"
        );
    }

    /// Test (c) — a restored pending with **no commit-identity stamp is not
    /// merged**. The design-§3 crash window: A2 staged its takeover, its step-2
    /// replica was uploaded, the commit landed on the log (step 3), then A2 died
    /// before merging (step 4) — but here the uploaded replica's stamp is
    /// stripped, the stamp-less shape only a build predating the resync-identity
    /// hardening sealed. The epoch-only merge such a replica once got was
    /// retired by the compat-remnant sweep (`version-compatibility.md`
    /// § Dimension 2, program 4): A1's resync restores it at exactly the logged
    /// commit's epoch, finds no identity, and does **not** merge — it stays at
    /// the staged epoch, exactly like the mismatch arm (test (b)).
    #[test]
    fn a_restored_pending_without_a_commit_identity_is_not_merged() {
        let w = gate_world();
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();

        // A2 bootstraps, then crashes mid-takeover: stage (1) → upload the
        // pending-carrying replica, stamp stripped (2) → the commit lands (3).
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let d2 = assemble_device(&alice2, 1, "alice", &w.conv_nest, &w.replica, w.channel);
        block_on(d2.sync.load())
            .unwrap()
            .provider
            .expect("A2 bootstraps from A1's replica")
            .restore_into_unchecked(&alice2)
            .unwrap();
        let staged_epoch = alice2.current_epoch(&w.channel).unwrap();
        let commit = alice2.self_update(&w.channel).unwrap(); // staged, unmerged
        let stripped = fauna_mls::state_replica::ProviderReplica::from_engine(&alice2)
            .with_pending_hashes(&[]);
        assert_eq!(stripped.pending_commit_hash(&w.channel), None);
        block_on(d2.sync.save_provider_if_changed(&stripped)).unwrap();
        w.conv_nest.inject(
            &w.channel.to_string(),
            ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        );
        // (A2 crashes here — nothing more from it.)

        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            staged_epoch,
            "a pending with no commit identity is never merged on epoch equality alone"
        );
        assert!(!w.sync.authored_current_epoch(&w.channel));
    }

    /// Test (a) — the **identity-match** crash-window merge (resync-identity
    /// hardening). Same honest crash window as test (c), but A2's step-2
    /// replica now carries the commit-identity **stamp**. No simulation is
    /// needed: `self_update` stages the commit and the engine records its
    /// identity in the same provider KV, so `from_engine` captures the two
    /// together — exactly the production capture. The stamp equals `blake3` of
    /// the logged commit, so A1's resync takes the **matching** arm and merges —
    /// convergent. This is the "given the honest replica, resync converges" half
    /// the self-heal in test (b) leans on.
    #[test]
    fn crash_window_resync_merges_when_commit_identity_matches() {
        let w = gate_world();
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();

        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let d2 = assemble_device(&alice2, 1, "alice", &w.conv_nest, &w.replica, w.channel);
        block_on(d2.sync.load())
            .unwrap()
            .provider
            .expect("A2 bootstraps from A1's replica")
            .restore_into_unchecked(&alice2)
            .unwrap();
        let staged_epoch = alice2.current_epoch(&w.channel).unwrap();
        let commit = alice2.self_update(&w.channel).unwrap(); // staged, unmerged
        assert_eq!(
            alice2.pending_commit_hash(&w.channel),
            Some(*blake3::hash(&commit).as_bytes()),
            "the engine stamps the staged commit's identity — the gate no longer has to",
        );
        block_on(d2.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&alice2),
        ))
        .unwrap();
        w.conv_nest.inject(
            &w.channel.to_string(),
            ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        );
        // (A2 crashes here.)

        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            staged_epoch + 1,
            "the restored pending's identity matched the logged commit — merged, converged"
        );
        assert!(!w.sync.authored_current_epoch(&w.channel));
    }

    /// Test (b) — the **attack the hardening closes**. Two of alice's
    /// devices stage at the *same* epoch N concurrently: `a_win` authors X and it
    /// **lands** on the log; `a_lose` stages Y (a genuine own-device commit) whose
    /// send is gate-rejected, so Y **never lands**. A malicious nest does a
    /// targeted rollback — it serves A1 the superseded step-2 replica carrying
    /// pending Y (stamped `blake3(Y)`) instead of `a_win`'s post-merge replica,
    /// while the log's own-leaf record is X.
    ///
    /// Under epoch-only equality this forked A1 onto the never-landed Y. With the
    /// identity gate, A1's resync sees `blake3(Y) ≠ blake3(X)` and **does not
    /// merge** — it stays at epoch N with the inert pending, so it cannot even
    /// take over (openmls blocks staging over the pending) and therefore cannot
    /// fork. Then, once the honest post-merge replica is served (the rollback
    /// ends), a plain resync **converges** A1 onto the merged epoch. Provably
    /// convergent-only.
    #[test]
    fn attack_superseded_step2_replica_does_not_fork_then_heals() {
        use crate::sync::MlsStateSync;
        let w = gate_world();
        // A1 uploads its provider (epoch N) — the shared state both concurrent
        // devices bootstrap from.
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();
        let r0 = fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice);
        let epoch_n = w.alice.current_epoch(&w.channel).unwrap();

        // a_win: authors X and it LANDS on the log (own-leaf, epoch N → N+1).
        let a_win = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        r0.restore_into_unchecked(&a_win).unwrap();
        let x = a_win.self_update(&w.channel).unwrap();
        a_win.merge_pending_commit(&w.channel).unwrap();
        w.conv_nest.inject(
            &w.channel.to_string(),
            ChannelEnvelope::Commit(x.clone()).to_bytes().unwrap(),
        );

        // a_lose: stages Y at the same epoch N; its send is gate-rejected so Y is
        // never injected — but its step-2 replica (with the blake3(Y) stamp) was
        // uploaded, and the malicious nest keeps serving it.
        let a_lose = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        r0.restore_into_unchecked(&a_lose).unwrap();
        let y = a_lose.self_update(&w.channel).unwrap(); // staged, never lands
        assert_ne!(
            blake3::hash(&x).as_bytes(),
            blake3::hash(&y).as_bytes(),
            "X and Y are distinct commits"
        );
        let lose_sync = MlsStateSync::new(Box::new(w.replica.clone()), &keypair(1));
        block_on(lose_sync.load()).unwrap();
        // `a_lose.self_update` staged Y and the engine stamped blake3(Y) in its
        // provider KV, so `from_engine` captures the identity — no manual stamp.
        block_on(lose_sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&a_lose),
        ))
        .unwrap();

        // A1 polls: meets X (own-leaf) → resync served the superseded R_Y.
        let mut cursor = 0i64;
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        // Safety: the identity mismatch blocked the merge — A1 did NOT fork onto Y.
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_n,
            "A1 did not merge the superseded pending Y — no fork onto a never-landed commit"
        );
        assert!(!w.sync.authored_current_epoch(&w.channel));
        // No-fork corollary: A1 cannot take over — the inert restored pending
        // blocks staging a new commit, so A1 can never send onto the fork.
        assert!(
            block_on(w.gate.ensure_takeover(w.channel)).is_err(),
            "a device served a superseded replica cannot stage over the inert pending — so cannot fork"
        );

        // Self-heal: the honest post-merge replica (epoch N+1, no pending) is now
        // served (the rollback ends). A plain resync converges A1 onto it.
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&a_win),
        ))
        .unwrap();
        block_on(w.sync.resync_provider(&w.alice))
            .unwrap()
            .expect("the honest replica is now served");
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_n + 1,
            "a clean honest replica converges A1 onto the merged epoch (self-heal)"
        );
    }

    /// Another **member's** commit does NOT revoke this device's epoch
    /// authorship — the takeover ping-pong fix.
    /// The send right rides the device's own leaf's latest commit
    /// (`devices.md` § Cross-device MLS group-state sync); Bob's commit does
    /// not touch Alice's leaf, so Alice keeps sending with no re-takeover —
    /// where the old revoke-on-any-foreign-commit rule made two
    /// actively-sending members take the epoch over from each other on every
    /// pump pass, an endless commit churn that kept every in-flight
    /// application envelope undecryptable at its receiver. (A same-account
    /// sibling device's commit still revokes — via the own-leaf resync arm,
    /// pinned by `twin_device_takeover_resyncs_the_other_device`.)
    #[test]
    fn foreign_member_commit_keeps_authorship_no_retakeover() {
        let w = gate_world();
        block_on(w.gate.ensure_takeover(w.channel)).unwrap();
        assert!(w.sync.authored_current_epoch(&w.channel));
        assert_eq!(commit_sends(&w.events), 1);

        // Bob (another member) advances the epoch; A1's poll processes it Ok.
        // Bob must first process A1's takeover commit to stay current.
        let log = w.conv_nest.fetch_after(&w.channel.to_string(), 0);
        let Ok(ChannelEnvelope::Commit(cb)) = ChannelEnvelope::from_bytes(&log[0].1) else {
            panic!("A1's takeover commit heads the log");
        };
        w.bob.process_commit(&w.channel, &cb).unwrap();
        inject_bob_commit(&w);

        let mut cursor = log[0].0; // resume past A1's own takeover record
        block_on(poll_inbound_conv(
            &w.backend,
            &w.manager,
            &w.channel,
            &mut cursor,
            0,
        ))
        .unwrap();

        assert!(
            w.sync.authored_current_epoch(&w.channel),
            "another member's commit leaves this device's own-leaf authorship standing"
        );
        // The next send needs NO new takeover: one gate-send total.
        block_on(w.gate.ensure_takeover(w.channel)).unwrap();
        assert_eq!(
            commit_sends(&w.events),
            1,
            "no re-takeover after another member's commit — the ping-pong is dead"
        );
    }

    #[test]
    fn removed_device_stays_unusable_after_concurrent_advance() {
        let w = gate_world();
        // Bob removes Alice; the removal lands in the log before her next send.
        let alice_leaf = w
            .bob
            .find_leaf_by_identity(&w.channel, &w.alice.identity_actor_id())
            .expect("alice is a member in bob's view");
        let removal = w.bob.remove_member(&w.channel, alice_leaf).unwrap();
        w.conv_nest.inject(
            &w.channel.to_string(),
            ChannelEnvelope::Commit(removal).to_bytes().unwrap(),
        );

        // Alice, not yet knowing she was removed, tries to take over the epoch
        // for an application send.
        let err = block_on(w.gate.ensure_takeover(w.channel))
            .expect_err("a removed device must not take over the epoch");
        assert!(
            matches!(err, BackendError::Internal(_)),
            "degrades to the retryable transport class, got {err}"
        );
        assert!(
            !w.sync.authored_current_epoch(&w.channel),
            "no authorship recorded post-removal"
        );
        let log = w.conv_nest.fetch_after(&w.channel.to_string(), 0);
        assert_eq!(
            log.len(),
            1,
            "only the removal commit is in the log — nothing landed from the removed device"
        );
    }

    /// Slice 5, item 1 (the per-channel serialization lock,
    /// `FaunaMlsBackend::channel_lock`): a gated commit driven through the
    /// **backend** (`RailBackend::remove_participant`, which *takes* the lock)
    /// whose send is stale rebases through `BackendCatchUp` — and that catch-up's
    /// inner `poll_inbound_conv` runs *inside* the held per-channel lock. If that
    /// inner poll re-took the lock, the no-op-waker `block_on` would spin forever
    /// (a deadlock manifests as a hang, failing the test on timeout). It
    /// completes because the lock lives only on the background poll + the gated
    /// branches, never in `poll_inbound_conv` itself. Faithfully exercises the
    /// lock on the gated path (the direct-`gate` tests above bypass it).
    #[test]
    fn gated_remove_through_backend_rebases_without_relocking_catch_up() {
        use fauna_conversations::TypedAddress;
        use fauna_conversations::backend::RailBackend;

        let w = gate_world();
        // A foreign commit lands first, so the gated remove's send is stale and
        // the rebase drains the log via poll_inbound_conv while the backend holds
        // this channel's lock.
        let injected_seq = inject_bob_commit(&w);
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();
        let bob_addr = TypedAddress::Fauna {
            handle: "bob".into(),
            actor_id: w.bob.identity_actor_id(),
        };
        // The synthetic thread `assemble_device` bound (`gate-test-<handle>-<seed>`).
        let thread = ThreadId("gate-test-alice-1".to_string());

        block_on(w.backend.remove_participant(thread, bob_addr))
            .expect("the gated remove completes — no self-deadlock on the held channel lock");

        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before + 2,
            "foreign commit processed via the in-lock catch-up, then the rebuilt remove merged"
        );
        assert!(
            w.alice
                .find_leaf_by_identity(&w.channel, &w.bob.identity_actor_id())
                .is_none(),
            "bob's leaf is gone — the remove landed on the advanced epoch"
        );
        assert_eq!(
            w.sync.processed_seq(&w.channel),
            injected_seq,
            "the in-lock catch-up advanced the processed-seq cursor"
        );
    }

    /// Slice 5, item 2 (the `ChannelCursor` seam / [`MlsSyncCursor`]): a device
    /// whose `MlsStateSync` cursor was seeded from a restored `history/<ch>`
    /// watermark resumes its inbound poll from that watermark, **not `0`** —
    /// pre-watermark records are skipped, not re-processed. Drives the exact
    /// seed→poll→advance dance `poll_bound`/`poll_conversations` run: seed the poll
    /// cursor from `channel_cursor().resume_seq`, poll, then `advance` back.
    /// (The `load()`→cursor-from-watermark step is covered by the `sync` suite;
    /// this proves the seam surfaces it and the poll honors it.)
    #[test]
    fn restored_watermark_resumes_the_poll_past_pre_restore_records() {
        let w = gate_world();
        // Inject a foreign commit at seq 1 that a from-`0` poll WOULD process
        // (advancing alice's epoch); a resumed poll past a higher watermark skips it.
        let pre_restore_seq = inject_bob_commit(&w);
        assert_eq!(pre_restore_seq, 1);
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();

        // Restored watermark: this identity had already folded up to seq 5 on its
        // prior device (seeded from the replica on `load`; set directly here).
        const WATERMARK: i64 = 5;
        w.sync.advance_processed_seq(&w.channel, WATERMARK);
        w.backend
            .set_channel_cursor(Arc::new(MlsSyncCursor::new(w.sync.clone())));

        assert_eq!(
            w.backend
                .channel_cursor()
                .expect("cursor seam injected")
                .resume_seq(&w.channel),
            WATERMARK,
            "the seam surfaces the restored watermark as the resume seq"
        );

        // The poll seeds from the seam (as `poll_bound` does), polls, advances back.
        let mut cur = w.backend.channel_cursor().unwrap().resume_seq(&w.channel);
        let ingested = block_on(poll_inbound_conv(
            &w.backend, &w.manager, &w.channel, &mut cur, 0,
        ))
        .unwrap()
        .ingested;
        w.backend.channel_cursor().unwrap().advance(&w.channel, cur);

        assert_eq!(ingested, 0, "no records above the watermark to fold");
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before,
            "the pre-watermark foreign commit at seq 1 was NOT processed — the poll resumed from 5, not 0"
        );
        assert_eq!(
            w.sync.processed_seq(&w.channel),
            WATERMARK,
            "advance reports the resumed cursor back onto MlsStateSync (monotonic, unchanged here)"
        );
    }

    /// Slice 5 two-device-journey capstone — the crypto rationale for the
    /// `history/<ch>` replica (design §3): a device restored from another of its
    /// **own** devices' `provider` replica shares that identity's single MLS leaf,
    /// so — exactly like the original sender — it can NEVER MLS-decrypt that leaf's
    /// own application messages off the channel log. Log replay reconstructs *other*
    /// members' traffic (deterministic receiver ratchets) but never this identity's
    /// own, which is precisely why the history slice must carry own-message
    /// plaintext. Proven with three real engines: alice sends; alice's twin
    /// (restored provider, same key) fails to decrypt it; the other member (bob)
    /// decrypts it fine — so it is a valid log message, just opaque to the twin,
    /// which must read it from the restored history slice instead (that store-level
    /// restore is proven by `store::history::tests`; two-device convergence by
    /// `twin_device_takeover_resyncs_the_other_device`). Together these three cover
    /// the design's "device B imports the key, syncs, reads history, posts back
    /// convergently" success criterion at the client-logic tier; the nest wire
    /// plane is the tier_3 `test_mls_replica_sync.py`.
    #[test]
    fn twin_device_cannot_decrypt_its_own_log_message_but_bob_can() {
        use fauna_core::data::Timestamp;
        use fauna_mls::types::{ChannelMessage, ChannelMessageBody};

        let w = gate_world();
        // alice uploads her provider (the launch save) so her twin can bootstrap.
        block_on(w.sync.save_provider_if_changed(
            &fauna_mls::state_replica::ProviderReplica::from_engine(&w.alice),
        ))
        .unwrap();

        // alice sends an own application message on the channel.
        let msg = ChannelMessage {
            sender: w.alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("hi from A".into()),
            timestamp: Timestamp::now(),
        };
        let ciphertext = w.alice.encrypt(&w.channel, &msg).unwrap();

        // alice's twin A2: fresh engine, same identity, restored from her replica.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let d2 = assemble_device(&alice2, 1, "alice", &w.conv_nest, &w.replica, w.channel);
        block_on(d2.sync.load())
            .unwrap()
            .provider
            .expect("A2 bootstraps from alice's replica")
            .restore_into_unchecked(&alice2)
            .unwrap();

        // The crux: A2 shares alice's leaf, so — like alice herself — it cannot
        // MLS-decrypt alice's own application message off the log. The history
        // replica is the ONLY path by which A2 obtains alice's own-message history.
        assert!(
            alice2.decrypt(&w.channel, &ciphertext).is_err(),
            "a twin device cannot decrypt its own identity's log message"
        );
        // But it IS a valid message: the OTHER member (bob) decrypts it fine.
        let got = w
            .bob
            .decrypt(&w.channel, &ciphertext)
            .expect("the other member decrypts alice's message");
        assert!(
            matches!(got.body, ChannelMessageBody::Text(t) if t == "hi from A"),
            "bob reads alice's plaintext from the log — only the twin cannot"
        );
    }
}
