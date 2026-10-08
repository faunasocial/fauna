//! The device-owned-epoch **commit rebase loop** — the multi-writer-safe send
//! primitive that composes the 3a `MlsEngine` staged commits, the 3a
//! `ConversationsRpc::channel_send` commit gate, and the 4a [`MlsStateSync`]
//! cursor + `provider` CAS into one correct, crash-safe operation
//! (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync;
//! design tracked internally, §3).
//!
//! **Why it lives here (the cycle constraint).** The loop needs the cursor and
//! the `provider` CAS-put, both of which are [`MlsStateSync`]'s job — and
//! `MlsStateSync` sits *above* `fauna-conversations` (it depends on it). So the
//! loop cannot live in `fauna-conversations` (that would make
//! `fauna-conversations → fauna-client-mls-sync → fauna-conversations` a cycle);
//! it lives here, where the `MlsEngine` (`fauna-mls`), the `ConversationsRpc`
//! seam (`fauna-conversations`), and the cursor (this crate) can all be composed
//! acyclically. Catch-up (poll + process the intervening records) is
//! `fauna_conversations::backends::fauna_mls::poll_inbound_conv`, reachable from
//! here but needing the backend + manager this crate must not own — so it is
//! abstracted behind the [`CommitCatchUp`] seam the caller supplies.
//!
//! **The invariant** (design §3): a device may send application traffic on a
//! channel only in an epoch whose latest own-leaf commit it authored. Every
//! device switch costs one self-`Update` (fresh secret tree ⇒ fresh sender
//! chains), so a shared single leaf never forks a ratchet generation. The loop
//! enforces it for *all* commit-producing flows (add / remove / takeover
//! self-update); [`MlsStateSync::ensure_epoch_takeover`] enforces the takeover
//! before an application send.
//!
//! **Serialization.** The loop stages a pending commit in the engine; openmls
//! errors if a second commit is staged over an unmerged one. So a channel's
//! gated loop and its background inbound poll must not run concurrently — the
//! per-app leg serializes them (slice 5).

use std::future::Future;

use fauna_conversations::backend::{BackendError, ConvRpcError};
use fauna_mls::engine::MlsEngine;
use fauna_mls::error::MlsError;
use fauna_mls::types::{ChannelEnvelope, ChannelId};

use crate::store::MlsReplicaClientError;
use crate::sync::MlsStateSync;

/// Max rebase rounds before [`MlsStateSync::send_commit_gated`] gives up. A stale
/// rejection clears in a single round unless another writer keeps racing the same
/// channel; the bound guards a pathological live-lock. Mirrors the `save_*_cas`
/// [`MAX_CAS_RETRIES`](crate::store) in spirit.
const MAX_GATE_ROUNDS: usize = 8;

/// Process a channel's intervening log records so a rebuilt commit lands on the
/// latest epoch — the **catch-up** leg of the rebase, invoked on a
/// [`ConvRpcError::StaleCommit`] rejection.
///
/// The implementation advances the MLS engine over inbound `Commit`s (and, in
/// production, ingests inbound applications) starting after `from_seq`, returning
/// the new highest processed `seq`. The production impl (slice-5 leg) wraps
/// `fauna_conversations::backends::fauna_mls::poll_inbound_conv`; unit tests
/// process a fake nest's log directly. Kept as a caller-supplied seam so this
/// crate never owns a `FaunaMlsBackend` / `ConversationsManager` (which would
/// re-introduce the dependency cycle the module doc describes).
pub trait CommitCatchUp {
    /// Advance past `channel`'s records with `seq > from_seq`; return the new
    /// high-water `seq` (≥ `from_seq`). Errors are surfaced as
    /// [`CommitGateError::CatchUp`] and abort the rebase.
    fn catch_up_after(
        &self,
        channel: &ChannelId,
        from_seq: i64,
    ) -> impl Future<Output = Result<i64, BackendError>>;
}

/// Append a gated `Commit` envelope to `channel`'s log — the **routing** leg of
/// the rebase, and the one door [`MlsStateSync::send_commit_gated`] is allowed
/// to send through.
///
/// A raw `ConversationsRpc::channel_send` is deliberately NOT this seam. A
/// channel whose home is a *foreign* nest (a cross-nest shared folder the member
/// joined from someone else's Welcome) is read over the relay, and must be
/// *written* over it too: sending straight to the member's own nest appends the
/// Commit to a log no member ever fetches — the send blackhole
/// `FaunaMlsBackend::send_on_channel` exists to close, and which the gate fell
/// into for every foreign-homed channel while it called `channel_send`
/// unconditionally. The gate cannot make the pick itself (the channel-to-home
/// map is the backend's private state), so the pick is handed in, exactly as
/// [`CommitCatchUp`] hands in the rail walk. The production impl
/// ([`BackendChannelSend`](crate::gate_impl::BackendChannelSend)) forwards to
/// `send_on_channel`, so a gated commit and an ordinary application send route
/// through the same one place.
///
/// The rule this seam serves: `federation.md` § Cross-nest shared folders +
/// channel append — *"a foreign member's device-owned-epoch takeover must
/// land"*.
pub trait GatedCommitSend {
    /// Append `envelope` to `channel`, refused as
    /// [`ConvRpcError::StaleCommit`] when a `Commit` landed after
    /// `expect_no_commit_since`. Returns the accepted record's `seq`.
    fn send_gated_commit(
        &self,
        channel: &ChannelId,
        envelope: Vec<u8>,
        expect_no_commit_since: i64,
    ) -> impl Future<Output = Result<i64, ConvRpcError>>;
}

/// Failure from [`MlsStateSync::send_commit_gated`]. Distinguishes the four things
/// that can go wrong composing the plane so a leg can render / retry
/// appropriately; a [`ConvRpcError::StaleCommit`] is **never** surfaced here — it
/// is the loop's internal rebase signal, not an error.
#[derive(Debug)]
pub enum CommitGateError {
    /// A `provider` CAS-put (crash-safety step 2 or 4) failed — the mls-plane
    /// transport / seal / codec / size / conflict-exhaustion error.
    Replica(MlsReplicaClientError),
    /// The gate-send failed for a reason other than the stale-epoch rebase signal
    /// (a hard `Rejected`, an `Transient` transport fault, a nest-outdated
    /// `NeedsUpdate`). The pending commit has been cleared before this is raised.
    Conv(ConvRpcError),
    /// A staged-commit / merge / clear engine primitive failed.
    Engine(MlsError),
    /// Encoding the `ChannelEnvelope::Commit` to its wire bytes failed.
    Encode(String),
    /// The [`CommitCatchUp`] seam failed while processing the intervening records.
    CatchUp(BackendError),
    /// Every one of [`MAX_GATE_ROUNDS`] rounds hit a stale-epoch rejection —
    /// another writer kept winning the channel. Surfaced so a live-lock fails
    /// loudly rather than dropping the commit. The pending commit has been cleared.
    RetriesExhausted,
}

impl core::fmt::Display for CommitGateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Replica(e) => write!(f, "commit-gate provider CAS: {e}"),
            Self::Conv(e) => write!(f, "commit-gate gate-send: {e}"),
            Self::Engine(e) => write!(f, "commit-gate engine primitive: {e}"),
            Self::Encode(e) => write!(f, "commit-gate envelope encode: {e}"),
            Self::CatchUp(e) => write!(f, "commit-gate catch-up: {e}"),
            Self::RetriesExhausted => write!(f, "commit-gate exhausted its rebase retries"),
        }
    }
}

impl std::error::Error for CommitGateError {}

impl MlsStateSync {
    /// Run the device-owned-epoch **commit rebase loop** for `channel` (design
    /// §3b), returning the accepted server `seq` and the `build`'s extra payload
    /// (the Welcome for an add; `()` for remove / self-update).
    ///
    /// Each round follows the design-§3 **crash-safety sequence** exactly:
    ///
    /// 1. `build()` stages a pending commit in the engine (persisted in the
    ///    provider KV, so it survives a crash);
    /// 2. **CAS-put `provider`** — the replica now carries the pending commit, so a
    ///    peer converges on a crash between the send (3) and the merge (4);
    /// 3. **gate-send** the commit with `expect_no_commit_since = processed_seq`,
    ///    through the caller's [`GatedCommitSend`] — so a foreign-homed channel's
    ///    commit is relayed to the nest that homes it, the same nest the
    ///    `processed_seq` this round gates on was read from;
    /// 4. on **accept**: `merge_pending_commit`, mark this device the epoch author,
    ///    **CAS-put `provider` again** (the merged state), and return.
    ///
    /// On a [`ConvRpcError::StaleCommit`] rejection: `clear_pending_commit` (a safe
    /// no-op if none is staged), [`CommitCatchUp::catch_up_after`] to advance the
    /// engine + cursor past the intervening records, then rebuild on the new epoch
    /// and retry — bounded by [`MAX_GATE_ROUNDS`]. On any *other* error the staged
    /// pending is cleared before the error propagates, so the engine is never left
    /// with a dangling pending commit that would block the next staged commit.
    ///
    /// The **cursor is not advanced on accept**: the background inbound poll drains
    /// the `(expect, seq)` applications and skips this device's own merged commit
    /// (today's `process_commit` own-message warn-skip — slice 4c refines it). This
    /// keeps a concurrent application that landed in that window from being skipped.
    ///
    /// `build` is `FnMut` because a rebuild re-runs it on the new epoch; the
    /// returned extra is the *accepted* build's. `send`/`catch_up` are borrowed for
    /// the call only. Requires a prior [`load`](MlsStateSync::load) to lift the
    /// provider-put gate.
    pub async fn send_commit_gated<S, H, X, Build>(
        &self,
        engine: &MlsEngine,
        send: &S,
        catch_up: &H,
        channel: &ChannelId,
        mut build: Build,
    ) -> Result<(i64, X), CommitGateError>
    where
        // The send is a routing-aware seam, never a bare `ConversationsRpc`:
        // see [`GatedCommitSend`] for why a raw `channel_send` here blackholes
        // every foreign-homed channel's commit.
        S: GatedCommitSend,
        H: CommitCatchUp,
        Build: FnMut() -> Result<(Vec<u8>, X), MlsError>,
    {
        let hex = channel.to_string();
        for round in 0..MAX_GATE_ROUNDS {
            // 1. stage a pending commit. The engine records its identity in the
            //    same provider KV (design §3 resync-identity hardening: a
            //    crash-window peer merges the step-2 pending only if it is *this
            //    same* commit as the logged record), so step-2's replica carries
            //    the identity by construction — no separate bookkeeping to keep
            //    in step with the engine's pending lifecycle.
            let (commit_bytes, extra) = build().map_err(CommitGateError::Engine)?;
            tracing::debug!(channel = %hex, round, "commit gate: staged the pending commit; CAS-putting the replica");

            // 2. CAS-put the pending provider. On failure clear the just-staged
            //    pending so the engine isn't stranded, then surface.
            if let Err(e) = self.save_provider_snapshot(engine).await {
                let _ = engine.clear_pending_commit(channel);
                return Err(e);
            }

            // 3. gate-send under the device-owned-epoch commit gate.
            let envelope = match ChannelEnvelope::Commit(commit_bytes).to_bytes() {
                Ok(b) => b,
                Err(e) => {
                    let _ = engine.clear_pending_commit(channel);
                    return Err(CommitGateError::Encode(e));
                }
            };
            let expect = self.processed_seq(channel);
            tracing::debug!(channel = %hex, round, expect, "commit gate: sending the commit");
            match send.send_gated_commit(channel, envelope, expect).await {
                Ok(seq) => {
                    tracing::debug!(channel = %hex, round, seq, "commit gate: commit accepted; merging + re-sealing");
                    // 4. accept: merge, record authorship, re-CAS-put the merged
                    //    provider. Authorship is set before the (fallible) upload
                    //    because the merge already advanced us locally. The merge
                    //    drops the engine's pending stamp, so the step-4 re-save
                    //    seals the MERGED state carrying no pending for this
                    //    channel.
                    engine
                        .merge_pending_commit(channel)
                        .map_err(CommitGateError::Engine)?;
                    self.mark_authored(channel);
                    self.save_provider_snapshot(engine).await?;
                    tracing::debug!(channel = %hex, round, seq, "commit gate: commit landed");
                    return Ok((seq, extra));
                }
                Err(ConvRpcError::StaleCommit { .. }) => {
                    // Stale epoch: discard our pending, catch up over the
                    // intervening records, then rebuild + retry.
                    engine
                        .clear_pending_commit(channel)
                        .map_err(CommitGateError::Engine)?;
                    let from = self.processed_seq(channel);
                    tracing::debug!(channel = %hex, round, from, "commit gate: stale epoch; catching up before the rebuild");
                    let new_seq = catch_up
                        .catch_up_after(channel, from)
                        .await
                        .map_err(CommitGateError::CatchUp)?;
                    tracing::debug!(channel = %hex, round, new_seq, "commit gate: caught up; rebuilding on the new epoch");
                    self.advance_processed_seq(channel, new_seq);
                }
                Err(other) => {
                    // Every non-stale refusal collapses here identically — a
                    // terminal `Rejected` and a retryable `Transient` (the shape
                    // a `fauna.conversations.rate_limited` refusal takes once
                    // `RpcError::action()` classifies it) leave by the same door,
                    // and the advertisement pump above retries either with no
                    // backoff (`fauna-sync-engine/src/share_glue.rs`). Harmless
                    // while the nest's commit limiter does not extend its own
                    // lockout, but the retryable/terminal distinction is
                    // available here and unconsumed.
                    tracing::debug!(channel = %hex, round, "commit gate: the nest refused the commit: {other}");
                    let _ = engine.clear_pending_commit(channel);
                    self.unstage_saved_pending(engine).await;
                    return Err(CommitGateError::Conv(other));
                }
            }
        }
        // Bounded retries exhausted: clear any dangling pending and fail loudly.
        tracing::debug!(channel = %hex, rounds = MAX_GATE_ROUNDS, "commit gate: every gate round hit a stale epoch; giving up");
        let _ = engine.clear_pending_commit(channel);
        self.unstage_saved_pending(engine).await;
        Err(CommitGateError::RetriesExhausted)
    }

    /// Ensure this device owns `channel`'s current epoch before its first
    /// application send (design §3c takeover). No-op if it already authored the
    /// epoch; otherwise it posts a self-`Update` commit through
    /// [`send_commit_gated`] (which marks authorship on accept). A read-only
    /// device never calls this, so it never commits.
    pub async fn ensure_epoch_takeover<S, H>(
        &self,
        engine: &MlsEngine,
        send: &S,
        catch_up: &H,
        channel: &ChannelId,
    ) -> Result<(), CommitGateError>
    where
        S: GatedCommitSend,
        H: CommitCatchUp,
    {
        if self.authored_current_epoch(channel) {
            tracing::debug!(channel = %channel, "commit gate: epoch already authored; no takeover needed");
            return Ok(());
        }
        tracing::debug!(channel = %channel, "commit gate: epoch not authored; posting a self-Update takeover");
        let outcome = self
            .send_commit_gated(engine, send, catch_up, channel, || {
                engine.self_update(channel).map(|commit| (commit, ()))
            })
            .await
            .map(|_| ());
        match &outcome {
            Ok(()) => tracing::debug!(channel = %channel, "commit gate: takeover landed"),
            Err(e) => tracing::debug!(channel = %channel, "commit gate: takeover failed: {e}"),
        }
        outcome
    }

    /// Undo step 2's upload after a commit that will NEVER land.
    ///
    /// Step 2 deliberately CAS-puts a replica **carrying** the staged pending, so
    /// a crash between the send and the merge still converges. But a commit the
    /// nest *refused* is not a crash window — it can never appear on the log, so
    /// that replica has become a lie. Left standing it is load-bearing harm rather
    /// than untidiness: a relaunch's `restore_and_wire` brings the pending back,
    /// and `resync_channel`'s own note records the consequence — "openmls blocks a
    /// takeover from staging over it, so this device cannot send". The device
    /// would come back unable to send on that channel.
    ///
    /// Runs after the engine's pending is cleared, so it seals the cleared state —
    /// the very operation the ACCEPT path already performs (step 4 seals the
    /// merged state, which likewise carries no pending). Best-effort by design:
    /// the caller is already returning an error and must surface *that* one, not
    /// this one. A failed unstage simply leaves today's behaviour.
    ///
    /// Pinned by `gate_impl.rs::a_refused_takeover_leaves_no_pending_commit_in_the_saved_replica`.
    async fn unstage_saved_pending(&self, engine: &MlsEngine) {
        if let Err(e) = self.save_provider_snapshot(engine).await {
            tracing::debug!(
                "commit gate: could not unstage the refused pending from the replica: {e}"
            );
        }
    }

    /// Snapshot the engine's provider state and CAS-put it (crash-safety steps 2
    /// and 4). Delegates to the gated + deduped + 3-way-merging
    /// [`save_engine_provider_if_changed`](MlsStateSync::save_engine_provider_if_changed),
    /// which also records a landed listing on the engine.
    async fn save_provider_snapshot(&self, engine: &MlsEngine) -> Result<(), CommitGateError> {
        self.save_engine_provider_if_changed(engine)
            .await
            .map(|_| ())
            .map_err(CommitGateError::Replica)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        MlsReplicaTransport, MlsTransportError, PutOutcome, rpc_transport_get, rpc_transport_put,
    };
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::mls_replica::{
        CODE_CONFLICT, GetMlsReplicaReply, GetMlsReplicaRequest, KIND_GET, KIND_PUT,
        PutMlsReplicaReply, PutMlsReplicaRequest, ReplicaBase,
    };
    use fauna_protocol::{RpcErrorClass, RpcRequester};
    use serde_bytes::ByteBuf;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // The conversations-plane fake (the gated channel log), the shared event
    // log, and the no-IO executor live in `crate::test_conv` — shared with the
    // `gate_impl` tests.
    use crate::test_conv::{ConvNest, EventLog, FakeConvNest, block_on};

    // ── mls-plane fake (the `fauna.mls.{get,put}` transport) ────────────

    #[derive(Debug)]
    enum MlsFakeError {
        Rpc(fauna_protocol::RpcError),
    }
    impl core::fmt::Display for MlsFakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Rpc(e) => write!(f, "rpc {}", e.code),
            }
        }
    }
    impl RpcErrorClass for MlsFakeError {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            match self {
                Self::Rpc(e) => Some(e),
            }
        }
    }

    /// CAS-enforcing per-path fake for the mls plane (mirrors `store::tests`),
    /// pushing a `"provider-put"` event on every accepted `provider` put so the
    /// crash-safety ordering is observable.
    #[derive(Default)]
    struct FakeMlsNest {
        stored: Mutex<HashMap<String, Vec<u8>>>,
        events: EventLog,
    }
    struct MlsNest(Arc<FakeMlsNest>);
    impl RpcRequester for MlsNest {
        type Error = MlsFakeError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                KIND_PUT => {
                    let req: PutMlsReplicaRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode put");
                    let mut map = self.0.stored.lock().unwrap();
                    let current = map.get(&req.path).map(|b| *blake3::hash(b).as_bytes());
                    let ok = match &req.base {
                        ReplicaBase::Absent => current.is_none(),
                        ReplicaBase::Hash(h) => current.as_ref() == Some(h),
                    };
                    if !ok {
                        return Err(MlsFakeError::Rpc(fauna_protocol::RpcError::new(
                            CODE_CONFLICT,
                            "error.mls.conflict",
                        )));
                    }
                    if req.path == crate::store::PATH_PROVIDER {
                        self.0.events.lock().unwrap().push("provider-put");
                    }
                    map.insert(req.path, req.blob.into_vec());
                    fauna_protocol::encode_canonical(&PutMlsReplicaReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_GET => {
                    let req: GetMlsReplicaRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode get");
                    let blob = self
                        .0
                        .stored
                        .lock()
                        .unwrap()
                        .get(&req.path)
                        .cloned()
                        .map(ByteBuf::from);
                    // A current nest carries the digest on every reply.
                    let hash = blob
                        .as_ref()
                        .map(|b| ByteBuf::from(blake3::hash(b).as_bytes().to_vec()));
                    fauna_protocol::encode_canonical(&GetMlsReplicaReply {
                        blob,
                        hash,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected mls-plane kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The transport seam over the fake — the same two-line delegation a slice-5
    /// leg adapter writes (see `store::tests`).
    #[async_trait::async_trait]
    impl MlsReplicaTransport for MlsNest {
        async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            rpc_transport_get(self, path).await
        }
        async fn put(
            &self,
            path: String,
            blob: Vec<u8>,
            base: ReplicaBase,
        ) -> Result<PutOutcome, MlsTransportError> {
            rpc_transport_put(self, path, blob, base).await
        }
    }

    // ── catch-up seam (process the fake conv-log's commits into our engine) ──

    struct TestCatchUp<'a> {
        engine: &'a MlsEngine,
        nest: Arc<FakeConvNest>,
    }
    impl CommitCatchUp for TestCatchUp<'_> {
        async fn catch_up_after(
            &self,
            channel: &ChannelId,
            from_seq: i64,
        ) -> Result<i64, BackendError> {
            let hex = channel.to_string();
            let mut high = from_seq;
            for (seq, env) in self.nest.fetch_after(&hex, from_seq) {
                if let Ok(ChannelEnvelope::Commit(cb)) = ChannelEnvelope::from_bytes(&env) {
                    // Foreign commits advance our epoch; our own merged commit is
                    // rejected as an own-message and skipped (poll_inbound_conv's
                    // tolerance) — the loop never stalls on it.
                    let _ = self.engine.process_commit(channel, &cb);
                }
                if seq > high {
                    high = seq;
                }
            }
            Ok(high)
        }
    }

    // ── harness ─────────────────────────────────────────────────────────

    fn keypair(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    /// A real two-member group: Alice (the loop-runner) + Bob (a peer joined via
    /// the Welcome), sharing the given event log across both planes' fakes.
    struct World {
        alice: MlsEngine,
        bob: MlsEngine,
        channel: ChannelId,
        sync: MlsStateSync,
        conv: ConvNest,
        conv_nest: Arc<FakeConvNest>,
        events: EventLog,
    }

    fn world() -> World {
        let events: EventLog = Default::default();
        let mls_nest = Arc::new(FakeMlsNest {
            events: events.clone(),
            ..Default::default()
        });
        let conv_nest = Arc::new(FakeConvNest::with_events(events.clone()));
        let alice = MlsEngine::new_in_memory(keypair(1)).unwrap();
        let bob = MlsEngine::new_in_memory(keypair(2)).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let sync = MlsStateSync::new(Box::new(MlsNest(mls_nest)), &keypair(1));
        block_on(sync.load()).unwrap(); // lift the provider-put gate
        World {
            alice,
            bob,
            channel,
            sync,
            conv: ConvNest(conv_nest.clone()),
            conv_nest,
            events,
        }
    }

    /// Bob's self-`Update` commit, wire-wrapped as a `ChannelEnvelope::Commit`
    /// (Bob merges it locally so his engine stays consistent) — the "another device
    /// advanced the epoch" record, exactly as it would sit in the channel log.
    fn bob_commit_envelope(w: &World) -> Vec<u8> {
        let c = w.bob.self_update(&w.channel).unwrap();
        w.bob.merge_pending_commit(&w.channel).unwrap();
        ChannelEnvelope::Commit(c).to_bytes().unwrap()
    }

    fn self_update_build<'a>(
        engine: &'a MlsEngine,
        channel: &'a ChannelId,
    ) -> impl FnMut() -> Result<(Vec<u8>, ()), MlsError> + 'a {
        move || engine.self_update(channel).map(|c| (c, ()))
    }

    // ── tests ───────────────────────────────────────────────────────────

    /// Happy path: no prior commit → the gate accepts on the first round. Proves
    /// the crash-safety ordering (provider-put → commit-send → provider-put),
    /// that the epoch advanced (merge happened), and that authorship is recorded.
    #[test]
    fn happy_accept_orders_puts_around_the_send() {
        let w = world();
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();
        let catch = TestCatchUp {
            engine: &w.alice,
            nest: w.conv_nest.clone(),
        };
        assert!(!w.sync.authored_current_epoch(&w.channel));

        let (seq, ()) = block_on(w.sync.send_commit_gated(
            &w.alice,
            &w.conv,
            &catch,
            &w.channel,
            self_update_build(&w.alice, &w.channel),
        ))
        .expect("first-round accept");

        assert_eq!(seq, 1, "sole writer accepts at seq 1");
        assert_eq!(
            *w.events.lock().unwrap(),
            vec!["provider-put", "commit-send", "provider-put"],
            "design §3 order: CAS-put pending, gate-send, CAS-put merged"
        );
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before + 1,
            "the accepted self-update merged (epoch bumped)"
        );
        assert!(
            w.sync.authored_current_epoch(&w.channel),
            "this device now owns the epoch"
        );
    }

    /// The crux of §3b: a commit from another device landed first, so the first
    /// gate-send is `StaleCommit` → clear → catch-up (process the foreign commit)
    /// → rebuild on the new epoch → retry → accept. Two send attempts; the final
    /// commit rides the post-catch-up epoch.
    #[test]
    fn stale_rejection_rebases_then_accepts() {
        let w = world();
        // Another device's commit lands at seq 1 (before Alice's send).
        let foreign = bob_commit_envelope(&w);
        let injected_seq = w.conv_nest.inject(&w.channel.to_string(), foreign);
        assert_eq!(injected_seq, 1);
        let epoch_before = w.alice.current_epoch(&w.channel).unwrap();
        let catch = TestCatchUp {
            engine: &w.alice,
            nest: w.conv_nest.clone(),
        };

        let (seq, ()) = block_on(w.sync.send_commit_gated(
            &w.alice,
            &w.conv,
            &catch,
            &w.channel,
            self_update_build(&w.alice, &w.channel),
        ))
        .expect("rebase then accept");

        assert!(seq > injected_seq, "accepted after the foreign commit");
        assert_eq!(
            w.sync.processed_seq(&w.channel),
            injected_seq,
            "catch-up advanced the cursor over the foreign commit"
        );
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            epoch_before + 2,
            "epoch advanced twice: foreign commit processed, then own accepted"
        );
        assert!(w.sync.authored_current_epoch(&w.channel));
        // The gate saw two commit-sends (the stale attempt + the accepted retry).
        assert_eq!(
            w.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| **e == "commit-send")
                .count(),
            1,
            "only the accepted send is logged; the stale one was rejected pre-append"
        );
    }

    /// A relentless racer keeps advancing the commit high-water on every round →
    /// the loop exhausts its bound and fails loudly, with no dangling pending
    /// (a fresh staged commit afterwards succeeds).
    #[test]
    fn exhausts_retries_and_leaves_no_pending() {
        let w = world();
        // A catch-up that *also* injects a new foreign commit each round, so the
        // next gate-send is always stale — a pathological live-lock.
        struct Relentless<'a> {
            engine: &'a MlsEngine,
            bob: &'a MlsEngine,
            channel: ChannelId,
            nest: Arc<FakeConvNest>,
        }
        impl CommitCatchUp for Relentless<'_> {
            async fn catch_up_after(
                &self,
                channel: &ChannelId,
                from_seq: i64,
            ) -> Result<i64, BackendError> {
                let hex = channel.to_string();
                let mut high = from_seq;
                for (seq, env) in self.nest.fetch_after(&hex, from_seq) {
                    if let Ok(ChannelEnvelope::Commit(cb)) = ChannelEnvelope::from_bytes(&env) {
                        let _ = self.engine.process_commit(channel, &cb);
                    }
                    if seq > high {
                        high = seq;
                    }
                }
                // Inject a *fresh* foreign commit past where we just caught up, so
                // the rebuilt commit is stale again next round.
                let c = self.bob.self_update(&self.channel).unwrap();
                self.bob.merge_pending_commit(&self.channel).unwrap();
                self.nest
                    .inject(&hex, ChannelEnvelope::Commit(c).to_bytes().unwrap());
                Ok(high)
            }
        }
        let catch = Relentless {
            engine: &w.alice,
            bob: &w.bob,
            channel: w.channel,
            nest: w.conv_nest.clone(),
        };
        // Seed the first stale commit.
        let c0 = bob_commit_envelope(&w);
        w.conv_nest.inject(&w.channel.to_string(), c0);

        let err = block_on(w.sync.send_commit_gated(
            &w.alice,
            &w.conv,
            &catch,
            &w.channel,
            self_update_build(&w.alice, &w.channel),
        ))
        .expect_err("relentless racer exhausts the bound");
        assert!(
            matches!(err, CommitGateError::RetriesExhausted),
            "got {err}"
        );
        // No dangling pending: a fresh staged commit succeeds (openmls errors if
        // one were still pending).
        w.alice
            .self_update(&w.channel)
            .expect("engine has no dangling pending commit");
    }

    /// `ensure_epoch_takeover`: a non-authoring device posts one self-update, then
    /// a second call is a no-op (already authored).
    #[test]
    fn takeover_commits_once_then_noops() {
        let w = world();
        let catch = TestCatchUp {
            engine: &w.alice,
            nest: w.conv_nest.clone(),
        };
        assert!(!w.sync.authored_current_epoch(&w.channel));

        block_on(
            w.sync
                .ensure_epoch_takeover(&w.alice, &w.conv, &catch, &w.channel),
        )
        .unwrap();
        assert!(
            w.sync.authored_current_epoch(&w.channel),
            "takeover authored"
        );
        let sends_after_first = w
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| **e == "commit-send")
            .count();
        assert_eq!(sends_after_first, 1, "exactly one takeover commit");

        // Second call: already authored → no new commit.
        block_on(
            w.sync
                .ensure_epoch_takeover(&w.alice, &w.conv, &catch, &w.channel),
        )
        .unwrap();
        assert_eq!(
            w.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| **e == "commit-send")
                .count(),
            1,
            "no second takeover once the epoch is owned"
        );
    }

    /// The theirs-wins premise: a device on a stale epoch **rebases** — it
    /// never force-merges its own commit onto the other device's epoch, so the
    /// same-key `provider` conflict path is never taken in the happy path. Proven
    /// by: the accepted commit's epoch is strictly the post-foreign-commit epoch
    /// (the loop processed theirs before rebuilding ours), never a fork of the
    /// pre-foreign epoch.
    #[test]
    fn stale_epoch_rebases_never_force_merges() {
        let w = world();
        let foreign = bob_commit_envelope(&w);
        w.conv_nest.inject(&w.channel.to_string(), foreign);
        // Bob (who authored the foreign commit) is now one epoch ahead of where
        // Alice started.
        let bob_epoch = w.bob.current_epoch(&w.channel).unwrap();
        let catch = TestCatchUp {
            engine: &w.alice,
            nest: w.conv_nest.clone(),
        };

        block_on(w.sync.send_commit_gated(
            &w.alice,
            &w.conv,
            &catch,
            &w.channel,
            self_update_build(&w.alice, &w.channel),
        ))
        .unwrap();

        // Alice's own accepted commit sits exactly one epoch past Bob's — i.e. she
        // adopted his epoch (rebase) then advanced, rather than forking from her
        // stale pre-foreign epoch (which would leave her at bob_epoch, not +1).
        assert_eq!(
            w.alice.current_epoch(&w.channel).unwrap(),
            bob_epoch + 1,
            "rebased onto the foreign epoch, then advanced — no forked force-merge"
        );
    }
}
