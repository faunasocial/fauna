//! Stateful cross-device MLS replica sync: [`MlsStateSync`] wraps a
//! [`MlsReplicaClient`] with the state every app leg needs *identically* — a
//! launch gate, per-path last-saved baselines, and **the per-channel
//! processed-seq cursor** — so the six per-app legs (slice 5) stay pure
//! trigger glue (a debounce timer + the engine/store snapshot calls) and never
//! re-implement the safety properties. Writing them once, here, is the point:
//! this replica carries user-irrecoverable data (own-message plaintext history,
//! live ratchet state), so a copy-pasted gate breaking across six apps is
//! exactly where user data would be lost.
//!
//! Mirrors `fauna_client_drafts::DraftsSync`, with two additions the replica
//! plane needs that drafts doesn't:
//!
//! 1. **The CAS merge** lives in [`MlsReplicaClient`] (`store.rs`), so
//!    `save_*_if_changed` here is a thin gate+dedup wrapper over the
//!    merge-retry loop — no data-loss-on-conflict.
//! 2. **The per-channel processed-seq cursor.** Today the conversation poll loop
//!    owns this as `poll_inbound_conv`'s `after_seq: &mut i64`
//!    (`fauna-conversations::backends::fauna_mls`). The replica plane makes it a
//!    *cross-device* quantity: a device resumes log ingest from the highest seq
//!    the replica reflects. So the wrapper owns it — seeded on [`load`] from the
//!    **`provider` blob**, which carries it, advanced by the poll loop via
//!    [`advance_processed_seq`], and folded back into the `provider` on every
//!    save. It rides in the `provider` rather than in `history/<ch>` because it is
//!    the read position of the crypto state: one blob, one CAS, so a torn
//!    `{provider, cursor}` pair is unrepresentable and a device can never resume
//!    past a commit it never applied (`devices.md` § Cross-device MLS group-state
//!    sync, Rule 2). Slice 4b/4c's rebase + resync read/advance it here.
//!
//! Like `DraftsSync`, this is deliberately **trigger-agnostic**: the ~1.5 s
//! debounce and the bounded quit-flush live in the per-app legs (slice 5),
//! not here — the wrapper only holds the gate, baselines, cursor, and the CAS.
//! Also **manager-agnostic**: [`load`] returns the [`LoadedReplica`] for the
//! caller to restore into its `MlsEngine` + `ThreadStore`, and the `save_*`
//! methods take snapshots the caller captured — no `fauna-conversations`
//! manager handle is held.
//!
//! [`load`]: MlsStateSync::load
//! [`advance_processed_seq`]: MlsStateSync::advance_processed_seq

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use fauna_conversations::store::history::ChannelHistorySlice;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_mls::error::MlsError;
use fauna_mls::state_replica::ProviderReplica;
use fauna_mls::types::ChannelId;
use fauna_protocol::mls_replica::ReplicaBase;

use crate::store::{MlsReplicaClient, MlsReplicaClientError, MlsReplicaTransport};

/// One channel [`MlsStateSync::adopt_sibling_groups`] put into the engine this
/// pass or an earlier one, with the `history/<ch>` slice to bind a thread from
/// once it has landed (`None` = the group is live in the engine but its slice
/// has not arrived yet; the next pass asks again).
pub struct AdoptedChannel {
    pub channel: ChannelId,
    pub slice: Option<ChannelHistorySlice>,
}

/// What [`MlsStateSync::load`] fetched for the caller to restore into its engine
/// and thread store on launch / cross-device catch-up. The wrapper has already
/// recorded the baselines and seeded the cursor from the slices' watermarks; the
/// caller only wires the returned data into `MlsEngine::restore_from_provider_storage`
/// (via `ProviderReplica::restore_into`) and `ThreadStore::restore_channel_slice`.
#[derive(Default)]
pub struct LoadedReplica {
    /// The openMLS `provider` snapshot, or `None` on first run (no replica yet).
    pub provider: Option<ProviderReplica>,
    /// One [`ChannelHistorySlice`] per channel the provider carries that has a
    /// stored history blob (a channel with no slice yet is absent).
    pub history: Vec<ChannelHistorySlice>,
}

#[derive(Default)]
struct State {
    /// Set true once [`MlsStateSync::load`] has run. Until then every
    /// `save_*_if_changed` is a no-op (the launch gate — never PUT the local
    /// state before the cross-device GET has merged in the other devices').
    loaded: bool,
    /// Set by [`MlsStateSync::hold_provider_saves`] when the loaded `provider`
    /// snapshot could not be examined — see that method. While held, every
    /// gated provider save is a no-op, so a session that could not READ the
    /// snapshot can never overwrite it either.
    provider_saves_held: bool,
    /// The last-synced `provider` replica: the dedup baseline **and** the
    /// three-way merge ancestor `save_provider_cas` needs. `None` = nothing
    /// synced yet.
    ///
    /// **Invariant: the last `provider` state this device is entitled to
    /// supersede wholesale** — what the engine *adopted* (`load`,
    /// `resync_provider`; the foreign-seat launch arm, where replacement is the
    /// design) or what it *authored* (its own last export). Never a CAS merge
    /// result the engine has not adopted: a merge folds a concurrent device's
    /// keys into the path, and a baseline equal to that path lets the next
    /// flush replace it without merging — dropping those keys
    /// (`save_provider_folded_if_changed`).
    provider_base: Option<ProviderReplica>,
    /// The `blake3` of the `provider` blob this device last loaded, stored or
    /// restored — the tip as this device knows it. The once-per-sweep probe
    /// ([`MlsStateSync::adopt_sibling_groups`]) compares the nest's current
    /// digest against it: equal means nobody else wrote, and the blob is not
    /// fetched. `None` after a write whose digest this device did not compute
    /// (`publish_provider`), which simply costs one blob fetch on the next
    /// probe.
    tip_hash: Option<[u8; 32]>,
    /// Channels [`MlsStateSync::adopt_sibling_groups`] imported into the engine
    /// whose `history/<ch>` slice had not landed yet — a chat channel binds its
    /// thread from that slice, so these are re-asked for on every pass until it
    /// arrives (a provider can be flushed ahead of its slice by a gate save).
    adopted_unbound: HashSet<ChannelId>,
    /// The last-synced `history/<hex>` slice per channel hex — the dedup
    /// baseline (the history merge is commutative, so it needs no ancestor).
    history_base: HashMap<String, ChannelHistorySlice>,
    /// The per-channel processed-seq cursor: the highest channel `seq` this
    /// device has folded, per channel. Seeded from the loaded `provider`'s
    /// cursors (fallback: the loaded slices' watermarks); the poll loop
    /// advances it (slice 4b/4c); every `save_provider_if_changed` folds it back
    /// into the `provider` blob (`devices.md` Rule 2).
    cursor: HashMap<ChannelId, i64>,
    /// Per-channel **did-I-author-the-current-epoch** flag — the device-owned-epoch
    /// invariant's bookkeeping (design §3c). `true` once this device's own gated
    /// commit was accepted + merged for the channel; `false` (the default, and
    /// after processing any *foreign* commit that advanced the epoch) means this
    /// device must post a takeover `self_update` before its next application send.
    /// Absent ⇒ `false`, so a fresh device (empty map after [`MlsStateSync::load`])
    /// always takes over per channel on first send.
    authored: HashMap<ChannelId, bool>,
}

/// Where [`MlsStateSync`] reports the post-succession re-seal's progress — the
/// app's render channel, wrapped. Boxed rather than generic so the plane stays
/// `dyn`-assemblable at session build.
pub type ResealSink = Box<dyn Fn(crate::ReplicaResealProgress) + Send + Sync>;

/// Launch gate + per-path baselines + the per-channel processed-seq cursor
/// guarding a [`MlsReplicaClient`]. One instance per actor on each Fauna app,
/// held for the app's lifetime. Non-generic over the transport (the
/// [`MlsReplicaTransport`] seam type-erases it) so `FaunaCommitGate` can hold it
/// behind the object-safe native-`Send` `CommitGate` boundary — see the
/// `store.rs` module doc.
pub struct MlsStateSync {
    client: MlsReplicaClient,
    /// Retired identities' `BackupKey`s, for the post-succession `__mls` re-seal
    /// ([`MlsReplicaClient::reseal_from_predecessors`]). Empty for every
    /// identity that never succeeded, which is what makes the pass free for
    /// them: [`Self::load`] skips it entirely rather than paying a round trip.
    predecessors: Vec<crate::BackupKey>,
    /// Where the re-seal pass reports what it did, so the surface
    /// § Re-key scope requires ("surfaced with progress") is fed from the one
    /// place the work provably happens. `None` on a build with no UI to tell.
    reseal_sink: Option<ResealSink>,
    state: Mutex<State>,
}

impl MlsStateSync {
    /// Build over a transport adapter and the user's identity keypair. The at-rest
    /// `BackupKey` is derived inside the wrapped [`MlsReplicaClient`].
    pub fn new(nest: Box<dyn MlsReplicaTransport>, keypair: &ActorKeypair) -> Self {
        Self {
            client: MlsReplicaClient::new(nest, keypair),
            predecessors: Vec::new(),
            reseal_sink: None,
            state: Mutex::new(State::default()),
        }
    }

    /// Offer the retired identities' `BackupKey`s, so [`Self::load`] re-seals a
    /// replica a succession left sealed to a predecessor before it tries to read
    /// it (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus row).
    ///
    /// **This is a barrier, not a head start, and that distinction is the whole
    /// point of putting it here.** The re-seal cannot be an ordinary post-auth
    /// hook racing the plane's launch: an unseal failure is classified
    /// [`crate::orchestration::RestoreRetryEnd::Failed`] — *permanent* — so a
    /// restore that wins the race leaves the successor's conversations dark for
    /// the entire session, not until the next retry. Hanging the pass on
    /// `load()` — the single chokepoint every restore goes through — makes
    /// "re-sealed before read" true by construction instead of by ordering.
    ///
    /// Free for everyone else: an empty slice skips the pass without a round
    /// trip.
    pub fn with_predecessors(mut self, predecessors: Vec<crate::BackupKey>) -> Self {
        self.predecessors = predecessors;
        self
    }

    /// Where the re-seal pass reports its [`crate::ReplicaResealProgress`].
    ///
    /// The sink hangs off the pass rather than off an app-side hook because the
    /// pass is a barrier inside [`Self::load`]: a second, app-side call would
    /// race this one and usually observe `AlreadyCurrent`, rendering *nothing*
    /// for a user whose conversations were in fact just unlocked. One owner of
    /// the work, one reporter of it.
    pub fn with_reseal_sink(mut self, sink: ResealSink) -> Self {
        self.reseal_sink = Some(sink);
        self
    }

    /// How many retired keys this replica will offer [`Self::load`]'s re-seal
    /// barrier.
    ///
    /// Exists for the **per-app call-site pin** and nothing else — this
    /// crate's own tests cannot see an app stop passing the walk, the same
    /// vacuity `fauna_client_drafts::DraftsSync::predecessor_count` exists to
    /// catch on the drafts plane. Asserted the same way.
    #[must_use]
    pub fn predecessor_count(&self) -> usize {
        self.predecessors.len()
    }

    /// Fetch + unseal the `provider` snapshot and every stored `history/<ch>`
    /// slice for the launch / cross-device catch-up. Records the baselines (so
    /// the immediate post-restore `save_*` tick is a no-op), **seeds the
    /// per-channel cursor from the `provider`'s own cursors** (falling back to a
    /// slice's `watermark` only for a channel the `provider`
    /// carries no cursor for — a slice before its first cursor), and lifts the save gate. The caller restores the returned
    /// [`LoadedReplica`] into its engine + thread store, then resumes each
    /// channel's poll from [`processed_seq`](Self::processed_seq).
    /// `provider: None` is first run.
    pub async fn load(&self) -> Result<LoadedReplica, MlsReplicaClientError> {
        // The post-succession `__mls` re-seal, ahead of the first read that
        // would fail on it. See [`Self::with_predecessors`] for why this is a
        // barrier here rather than a post-auth hook: a restore that raced it
        // would classify the unseal failure as *permanent* and leave the
        // successor's conversations dark for the whole session. Empty for every
        // identity that never succeeded, so the ordinary path pays nothing.
        if !self.predecessors.is_empty() {
            if let Some(sink) = &self.reseal_sink {
                sink(crate::ReplicaResealProgress::Running);
            }
            let settled = self
                .client
                .reseal_from_predecessors(&self.predecessors)
                .await;
            if let Some(sink) = &self.reseal_sink {
                sink(match &settled {
                    Ok(outcome) => crate::ReplicaResealProgress::Settled(*outcome),
                    // Best-effort reporting: the error is still returned below
                    // and fails the restore, but the user gets the line saying
                    // it retries rather than a silent dark plane.
                    Err(e) => crate::ReplicaResealProgress::Failed(format!("{e}")),
                });
            }
            let outcome = settled?;
            tracing::info!(?outcome, "the __mls replica re-seal pass settled at load");
        }
        let (provider, base) = self.client.load_provider_with_base().await?;
        let tip_hash = match base {
            ReplicaBase::Hash(h) => Some(h),
            ReplicaBase::Absent => None,
        };
        let mut history = Vec::new();
        let mut cursor = HashMap::new();
        let mut history_base = HashMap::new();
        if let Some(provider) = &provider {
            for channel in provider.channel_ids() {
                let hex = channel.to_string();
                // The cursor rides **inside** the provider blob (Rule 2), so it can
                // never outrun the crypto state it indexes — one blob, one CAS.
                // A slice persisted before its first cursor carries none;
                // fall back to the `history/<ch>` watermark (the recorded live
                // fallback, mirroring `pending_commit_hash`).
                // Note the fallback keeps the torn-replica hazard for a provider
                // saved before its cursor landed — `MlsError::FutureEpochCommit`
                // makes that strand loud instead of silent.
                let from_provider = provider.cursor(&channel);
                if let (Some(slice), _) = self.client.load_history_with_base(&hex).await? {
                    cursor.insert(channel, from_provider.unwrap_or(slice.watermark));
                    history_base.insert(hex, slice.clone());
                    history.push(slice);
                } else if let Some(seq) = from_provider {
                    // A polled channel whose history blob has not landed yet (Rule 2
                    // orders history first, so this is a channel with nothing folded).
                    cursor.insert(channel, seq);
                }
            }
        }
        {
            let mut st = self.state.lock().unwrap();
            st.loaded = true;
            st.provider_base = provider.clone();
            st.tip_hash = tip_hash;
            st.history_base = history_base;
            st.cursor = cursor;
        }
        Ok(LoadedReplica { provider, history })
    }

    /// **Stop this session from writing the `provider` blob at all.**
    ///
    /// The one caller is `orchestration::restore_and_wire`, on the arm where the
    /// loaded snapshot's seating could not be established
    /// ([`fauna_mls::state_replica::SeatingVerdict`]). That arm does not
    /// restore — rule (1) forbids restoring bytes whose leaf is unknown — but
    /// *not restoring* is only half the answer: `load` has already set
    /// `provider_base` to the snapshot, so the ordinary save path would happily
    /// CAS-replace it with this engine's near-empty state and destroy a
    /// snapshot that may well be this identity's own, merely unreadable to this
    /// build.
    ///
    /// ⚠ Deliberately NOT applied to the foreign-seat arm, where replacement is
    /// the design: a snapshot positively identified as a predecessor's is
    /// *supposed* to be taken back by the successor's first save.
    ///
    /// One-way for the life of this `MlsStateSync`: nothing clears it, because
    /// nothing in this session can turn an unreadable snapshot into a read one.
    /// A later launch loads afresh and decides afresh.
    pub fn hold_provider_saves(&self) {
        self.state.lock().unwrap().provider_saves_held = true;
    }

    /// Live-fold the current per-channel **ingest cursors** into `current`, then
    /// seal + CAS-persist it (via [`save_provider_folded_if_changed`]). Returns
    /// `Ok(true)` when it wrote, `Ok(false)` when skipped (pre-load gate or
    /// unchanged).
    ///
    /// **Fold the LIVE cursor only when `current` was captured at the same instant
    /// as `st.cursor`.** That holds for the synchronous **gate** crash-safety
    /// step-2/step-4 saves (`commit_gate`): they capture `from_engine` and reach
    /// this method with no `.await` between the engine mutation and the cursor
    /// read, so the cursor and the values are the same instant. It does **NOT**
    /// hold for the debounced **autosave**: its snapshot (T0) and its upload (T1)
    /// are seconds apart, and the poll advances `st.cursor` in that window — so the
    /// autosave folds the SNAPSHOT-time cursor in [`snapshot_replica`] and seals
    /// via [`save_provider_folded_if_changed`] instead. Folding the live cursor
    /// over snapshot-time values sealed a cursor ahead of the crypto values it
    /// indexed.
    ///
    /// The per-channel **pending-commit identities** need no fold: the engine
    /// stamps one when it stages a commit and drops it on merge/clear, so
    /// `ProviderReplica::from_engine` already captured them, in agreement with the
    /// pendings inside its own `values`.
    pub async fn save_provider_if_changed(
        &self,
        current: &ProviderReplica,
    ) -> Result<bool, MlsReplicaClientError> {
        // Fold under the lock, release before the await — never hold a std Mutex
        // across `.await`.
        let folded = {
            let st = self.state.lock().unwrap();
            if !st.loaded {
                return Ok(false);
            }
            let cursors: Vec<(ChannelId, i64)> =
                st.cursor.iter().map(|(c, seq)| (*c, *seq)).collect();
            current.clone().with_cursors(&cursors)
        };
        self.save_provider_folded_if_changed(&folded).await
    }

    /// [`Self::save_provider_if_changed`] over `engine`'s own export, **and
    /// the landed listing recorded on the engine** — the save door every
    /// production flush that holds the engine takes (the commit gate's
    /// crash-safety saves; the autosave's twin is
    /// [`crate::orchestration::save_snapshot`]).
    ///
    /// The record is what lets the swap tell a sibling's deletion from a join
    /// the replica has not seen yet (`devices.md` § Cross-device MLS
    /// group-state sync → *A group the engine holds but the snapshot does not
    /// list survives the swap*): a group this device's own write listed, and a
    /// later snapshot does not, was taken off by another device. Only a
    /// genuine write records — an unchanged export is already recorded (its
    /// listing is the baseline this device adopted or authored), and a gated
    /// no-op landed nothing.
    pub async fn save_engine_provider_if_changed(
        &self,
        engine: &MlsEngine,
    ) -> Result<bool, MlsReplicaClientError> {
        let current = ProviderReplica::from_engine(engine);
        let wrote = self.save_provider_if_changed(&current).await?;
        if wrote {
            engine.note_replica_listed(&current.channel_ids());
        }
        Ok(wrote)
    }

    /// **Publish `current` as this identity's `provider`, replacing whatever the
    /// path holds — no launch gate, no baseline, no merge.** The ceremony
    /// device's one call, made *before* the switch to the successor
    /// (`succession-aftermath.md` § Re-key scope → *What a successor's replica
    /// restore may take from a predecessor's*).
    ///
    /// Every other provider write goes through [`Self::save_provider_if_changed`],
    /// whose three properties are exactly wrong here: the gate no-ops before a
    /// successful [`load`](Self::load), and this identity has never loaded (it
    /// was minted seconds ago); the baseline is a device's last-synced state,
    /// and there is none; and the CAS *merge* would fold the path's current
    /// occupant — the **predecessor's** snapshot, moved here by the nest's
    /// succession transaction — into the successor's, re-importing the retired
    /// leaf's keys and groups this write exists to replace. A conflict here is
    /// therefore re-read-and-put, never merge: the only writers of a successor's
    /// path this early are the ceremony device and a sibling seat re-sealing the
    /// same bytes, and both want the successor's own state to be what the first
    /// post-switch load finds. Web has no other durable home for the successor's
    /// join at all (its engine is in-memory, the page reloads as the successor);
    /// native's SQLite carries it too, but a device that meets the predecessor's
    /// snapshot first still skips it by the own-leaf rule and publishes its own
    /// at the first flush.
    pub async fn publish_provider(
        &self,
        current: &ProviderReplica,
    ) -> Result<(), MlsReplicaClientError> {
        self.client.publish_provider(current).await?;
        // The tip is now bytes this call did not hash; forget it so the next
        // probe fetches once rather than mistaking the publish for a sibling's.
        self.state.lock().unwrap().tip_hash = None;
        Ok(())
    }

    /// Seal + CAS-persist an **already-cursor-folded** `provider` **iff** a launch
    /// [`load`](Self::load) has completed *and* it differs from the last-synced
    /// baseline. Returns `Ok(true)` when it wrote, `Ok(false)` when skipped
    /// (pre-load gate or unchanged). The CAS loop (`store.rs`) merges in any
    /// concurrent device's state — no clobber, and the baseline it leaves
    /// behind is this engine's own export, so a merged-in sibling contribution
    /// the engine has not adopted is folded forward again on every later flush
    /// rather than replaced on the next one (the `provider_base` invariant).
    ///
    /// The caller MUST have folded an ingest cursor **consistent with `folded`'s
    /// crypto values** — captured at the same instant. The autosave path does this
    /// in [`snapshot_replica`] (which folds `cursor_snapshot()` into the provider
    /// *before* reading the crypto values, so the cursor can only lag, never lead),
    /// giving content-level `{values, history, cursor}` atomicity, not merely the
    /// blob-level atomicity of one CAS.
    pub async fn save_provider_folded_if_changed(
        &self,
        folded: &ProviderReplica,
    ) -> Result<bool, MlsReplicaClientError> {
        // Snapshot the ancestor under the lock, then release before the await.
        let last_synced = {
            let st = self.state.lock().unwrap();
            if !st.loaded {
                return Ok(false);
            }
            // A snapshot this session could not examine must not be overwritten
            // by it (`hold_provider_saves`).
            if st.provider_saves_held {
                return Ok(false);
            }
            if st.provider_base.as_ref() == Some(folded) {
                return Ok(false);
            }
            st.provider_base.clone().unwrap_or_default()
        };
        let (stored, tip) = self
            .client
            .save_provider_cas_reporting_hash(folded, &last_synced)
            .await?;
        // The baseline is what THIS ENGINE authored, not what the CAS stored.
        // The two differ exactly when the loop three-way-merged a concurrent
        // device's write in: that merge result holds keys the running engine
        // never adopted (the restore doors are `load` and `resync_provider`
        // only), so recording it as the ancestor made the next flush meet a
        // path equal to its ancestor, skip the merge, and replace the sibling's
        // durably-saved groups with this engine's export. Keeping the engine's
        // own export as the ancestor makes every later flush see the sibling's
        // contribution as "theirs advanced" and fold it forward until a launch
        // or resync adopts it. Pinned by
        // `a_siblings_group_survives_this_devices_next_flush`; ruling in
        // `devices.md` § Cross-device MLS group-state sync → *A conflict-free
        // merge is kept by the durable plane, not the engine*.
        let merged_a_sibling_in = stored != *folded;
        let mut st = self.state.lock().unwrap();
        st.provider_base = Some(folded.clone());
        // And the tip digest the adoption detector compares against is "the
        // tip THIS DEVICE'S ENGINE STATE wrote" — which a merged write is not.
        // A flush that folded a sibling's contribution in stored a blob
        // listing groups this engine does not hold; recording that blob's hash
        // as this device's own made `adopt_sibling_groups` read the next probe
        // as "nothing new" and never fetch it, so a sibling that joined and
        // then went quiet reached this device only at its next launch (the
        // join's own Rule-3 provider put, 2026-09-22, made the ordering
        // ordinary: the sibling's join lands before this device's next
        // autosave). Leaving the digest unset costs one blob fetch on the next
        // sweep, which then adopts what the merge carried. Pinned by
        // `a_device_launched_before_the_mint_is_not_addressed_and_the_push_arm_says_so_below_warn`
        // (`orchestration`), whose non-holder flushes after the holder's join.
        st.tip_hash = if merged_a_sibling_in { None } else { Some(tip) };
        Ok(true)
    }

    /// **The mid-session door for a group another of the user's devices joined
    /// or created** (`devices.md` § Cross-device MLS group-state sync → *A
    /// sibling-joined group is adopted mid-session by a targeted import*).
    /// Called once per receive sweep by the injected `SiblingGroupAdopter`.
    ///
    /// Cheap in the steady state by construction: one `hash_only` probe of the
    /// `provider` tip, compared against the digest this device last loaded,
    /// stored or restored. Only a tip some other device wrote is fetched, and
    /// then every group it lists that `engine` lacks is imported through
    /// [`ProviderReplica::import_group_into`] — the per-group door that asks the
    /// seating question and touches nothing the engine already holds. Each
    /// import is folded into the merge baseline (the engine's next export
    /// carries the group; a baseline that did not would report it as a
    /// concurrent-writer conflict) and seeds the ingest cursor the way `load`
    /// does. Then every adopted channel whose `history/<ch>` slice has landed is
    /// returned with it, for the caller to bind a thread from; one whose slice
    /// has not landed stays remembered and is asked for again next pass.
    ///
    /// Gated exactly as the saves are: a no-op before `load`, and while the
    /// snapshot this session could not examine holds the save gate (a session
    /// that may not overwrite a snapshot it cannot read has no business
    /// importing from it either).
    pub async fn adopt_sibling_groups(
        &self,
        engine: &MlsEngine,
    ) -> Result<Vec<AdoptedChannel>, MlsReplicaClientError> {
        let (last_tip, mut to_bind) = {
            let st = self.state.lock().unwrap();
            if !st.loaded || st.provider_saves_held {
                return Ok(Vec::new());
            }
            let pending: Vec<ChannelId> = st.adopted_unbound.iter().copied().collect();
            (st.tip_hash, pending)
        };
        let tip = self.client.provider_tip_hash().await?;
        if tip.is_some() && tip != last_tip {
            let (provider, base) = self.client.load_provider_with_base().await?;
            if let Some(provider) = &provider {
                for channel in provider.channel_ids() {
                    if engine.has_group(&channel) {
                        continue;
                    }
                    match provider.import_group_into(engine, &channel) {
                        Ok(adopted) => {
                            let mut st = self.state.lock().unwrap();
                            st.provider_base
                                .get_or_insert_with(ProviderReplica::default)
                                .absorb_adopted_group(&adopted);
                            if let Some(seq) = adopted.cursor {
                                st.cursor.entry(channel).or_insert(seq);
                            }
                            st.adopted_unbound.insert(channel);
                            to_bind.push(channel);
                            tracing::info!(
                                channel = %channel,
                                entries = adopted.entries.len(),
                                "mls-sync: adopted a group another of this account's devices \
                                 joined, from the replica, without a relaunch"
                            );
                        }
                        // A blank seat is the ordinary state of every group this
                        // identity was evicted from — listed forever, met again on
                        // every sibling flush, and refused by design. Not an
                        // anomaly, so not a warning on every sweep.
                        Err(e @ MlsError::NotSeated(_)) => tracing::debug!(
                            channel = %channel,
                            "mls-sync: the replica lists a group this identity is not seated \
                             in — stays unadopted: {e}"
                        ),
                        Err(e) => tracing::warn!(
                            channel = %channel,
                            "mls-sync: a group the replica lists could not be adopted into \
                             this engine — left for the next launch restore: {e}"
                        ),
                    }
                }
            }
            self.state.lock().unwrap().tip_hash = match base {
                ReplicaBase::Hash(h) => Some(h),
                ReplicaBase::Absent => None,
            };
        }
        to_bind.sort_unstable_by_key(|c| c.0);
        to_bind.dedup();
        let mut out = Vec::with_capacity(to_bind.len());
        for channel in to_bind {
            let hex = channel.to_string();
            let (slice, _) = self.client.load_history_with_base(&hex).await?;
            let mut st = self.state.lock().unwrap();
            match slice {
                Some(slice) => {
                    // Same seeding rule as `load`: the provider's cursor if it
                    // carried one (already seeded above), else the slice's
                    // watermark.
                    st.cursor.entry(channel).or_insert(slice.watermark);
                    st.history_base.insert(hex, slice.clone());
                    st.adopted_unbound.remove(&channel);
                    out.push(AdoptedChannel {
                        channel,
                        slice: Some(slice),
                    });
                }
                None => out.push(AdoptedChannel {
                    channel,
                    slice: None,
                }),
            }
        }
        Ok(out)
    }

    /// Seal + CAS-persist `current` `history/<ch>` **iff** loaded *and* changed
    /// from the last-synced slice for that channel. The CAS loop unions in any
    /// concurrent device's messages (own history is user-irrecoverable — no
    /// message dropped). The per-app trigger calls this with
    /// `ThreadStore::snapshot_channel_slice(id, hex, processed_seq(channel))`.
    pub async fn save_history_if_changed(
        &self,
        current: &ChannelHistorySlice,
    ) -> Result<bool, MlsReplicaClientError> {
        {
            let st = self.state.lock().unwrap();
            if !st.loaded {
                return Ok(false);
            }
            if st.history_base.get(&current.channel_id_hex) == Some(current) {
                return Ok(false);
            }
        }
        let stored = self.client.save_history_cas(current).await?;
        self.state
            .lock()
            .unwrap()
            .history_base
            .insert(stored.channel_id_hex.clone(), stored);
        Ok(true)
    }

    /// The §5 **mid-session resync**: refetch the `provider` replica and
    /// restore it into `engine` (reloading every group it lists), updating the
    /// last-synced baseline so the next `save_provider_if_changed` merges from
    /// the refetched state. Deliberately narrower than [`load`](Self::load) —
    /// the cursor and history baselines are untouched (the poll owns the
    /// cursor; history is unaffected by a provider reload). Returns the restored
    /// [`ProviderReplica`] (so the caller can read its per-channel pending-commit
    /// identity for the crash-window merge decision) or `None` when no replica is
    /// stored (nothing to restore). Called via the `CommitGate` seam when the
    /// inbound driver meets an own-leaf commit this device did not author
    /// (`MlsError::OwnLeafCommit`).
    ///
    /// ⚠ **A snapshot that fails the seating verdict is refused here exactly as
    /// it is at launch, and `None` is the refusal** (`succession-aftermath.md`
    /// § Re-key scope → *What a successor's replica restore may take from a
    /// predecessor's*, rule (1)). Rule (1) is stated over the snapshot's own
    /// bytes, so it binds every door that swaps the provider KV, not only the
    /// launch one; `ProviderReplica::restore_into` is where it is asked.
    ///
    /// **Why refusing is the right answer, and what it costs.** The caller
    /// ([`crate::gate_impl`]'s `CommitGate::resync_channel`) maps a `None` to
    /// `Err` and thence to `CommitApplyOutcome::Stalled`, which is Rule-2-safe:
    /// the ingest cursor stops *before* the own-leaf commit and the next pass
    /// retries, so nothing is folded, dropped or forked. That leaves one real
    /// cost — a channel that stalls for as long as the snapshot stays
    /// non-clean — and it lands differently on the two arms:
    ///
    /// * **Foreign seat** — self-healing. The launch arm's design is that this
    ///   engine keeps its own state and its first save takes the path back by
    ///   CAS; once that save lands, the snapshot at the path is this identity's
    ///   own and the next resync reads clean.
    /// * **Unexaminable** — stalls for the session, by design. The launch has
    ///   already held provider saves ([`Self::hold_provider_saves`]), precisely
    ///   so bytes this build could not read are not destroyed, so nothing
    ///   replaces the snapshot and the verdict cannot change within the
    ///   session. A stalled channel is the strictly better half of that trade:
    ///   the alternative is swapping this engine's whole KV for bytes whose
    ///   leaf is unknown. A later launch — a build that can read them, or a
    ///   sibling device's fresh upload — heals it.
    ///
    /// **On the baseline write.** The refusal returns *before* `provider_base`
    /// is set, so a snapshot this door refused is never made the merge ancestor
    /// the next `save_provider_cas` would fold. That subsumes the narrower
    /// question of whether `provider_saves_held` should gate the write: under
    /// the hold the write is already inert — `provider_base` has exactly one
    /// reader, [`Self::save_provider_folded_if_changed`], which returns early
    /// while held — but inert-by-a-distant-guard is not a reason to record a
    /// refused snapshot as the baseline, and now it is not recorded at all.
    pub async fn resync_provider(
        &self,
        engine: &MlsEngine,
    ) -> Result<Option<ProviderReplica>, MlsReplicaClientError> {
        let (provider, base) = self.client.load_provider_with_base().await?;
        let Some(provider) = provider else {
            return Ok(None);
        };
        // Rule (1) is an absolute over the SNAPSHOT, not over the launch: this
        // door meets the same bytes `restore_and_wire` refuses, and until
        // 2026-08-30 it swapped them in unasked. `restore_into` now asks for
        // itself and restores nothing on a non-clean verdict; a refusal is
        // reported to the caller as "no replica restored".
        let verdict = provider.restore_into(engine)?;
        if !verdict.is_clean() {
            tracing::warn!(
                foreign = verdict.foreign.len(),
                unexaminable = verdict.unexaminable,
                "mls-sync: the mid-session provider resync refused a snapshot whose seating \
                 is not this identity's — not restored, and the last-synced baseline is \
                 left untouched; this channel stalls before the commit rather than \
                 re-seating this engine under another identity's leaf"
            );
            return Ok(None);
        }
        {
            let mut st = self.state.lock().unwrap();
            // Every channel loses its send right, not just the one whose commit
            // triggered this. `restore_into` above swaps the engine's WHOLE
            // provider KV, so it rewinds the sender ratchet of every group the
            // engine holds — while the only caller revokes authorship for the
            // single channel it was called about. Clearing here rather than
            // there is deliberate: the scope of the revocation has to match the
            // scope of the swap, and the swap happens on this line, not at any
            // caller. Leaving the rest of the map alone let a device keep
            // `authored = true` over a rewound ratchet and encrypt its next
            // application message at a generation the peer had already consumed
            // — nonce reuse, two plaintexts under one AEAD key, both durable in
            // the nest's channel log. openMLS surfaces it as a
            // `SecretReuseError` at the RECEIVER, which is a liveness symptom
            // and not a mitigation: by the time it fires, both ciphertexts are
            // written.
            //
            // This is exactly what a launch already does — `load()` builds a
            // fresh map and `authored_current_epoch` reads `unwrap_or(false)`,
            // so every channel is un-authored until its own takeover — and it is
            // what `devices.md` § Cross-device MLS group-state sync already
            // ratifies for BOTH doors: "no device ever encrypts with a sender
            // ratchet it loaded — every launch and resync takes the epoch over
            // before its first send". Until this line the sentence was true of
            // the launch door only.
            //
            // Clearing the whole map, rather than the restored replica's own
            // `channel_ids()`: the swap is wholesale, and
            // `restore_from_provider_storage` carries the engine's own
            // local-only groups across it (and drops the rest), so
            // `channel_ids()` under-counts what the swap actually touched.
            // The cost of over-clearing is one
            // self-`Update` per channel on its next send — the same cost every
            // launch already pays, and the same commit the takeover would post
            // anyway.
            st.authored.clear();
            st.provider_base = Some(provider.clone());
            st.tip_hash = match base {
                ReplicaBase::Hash(h) => Some(h),
                ReplicaBase::Absent => None,
            };
        }
        Ok(Some(provider))
    }

    /// The highest channel `seq` this device has folded for `channel` — where the
    /// poll loop resumes (`0` if this device has never processed the channel).
    /// The cross-device successor to `poll_inbound_conv`'s caller-owned
    /// `after_seq`.
    pub fn processed_seq(&self, channel: &ChannelId) -> i64 {
        self.state
            .lock()
            .unwrap()
            .cursor
            .get(channel)
            .copied()
            .unwrap_or(0)
    }

    /// Advance the processed-seq cursor for `channel` to `seq` (monotonic — a
    /// lower value is ignored, matching the poll loop's `if seq > after_seq`
    /// advance). The poll loop calls this as it folds each record; the next
    /// `history/<ch>` save snapshots the resulting watermark.
    pub fn advance_processed_seq(&self, channel: &ChannelId, seq: i64) {
        let mut st = self.state.lock().unwrap();
        let e = st.cursor.entry(*channel).or_insert(0);
        if seq > *e {
            *e = seq;
        }
    }

    /// A copy of the full per-channel ingest cursor map — every channel this
    /// device has folded, at the highest `seq` folded. [`snapshot_replica`] reads
    /// this **before** it captures the provider's crypto values and folds the
    /// result into the snapshot's `provider`, so the autosave seals a cursor that
    /// can only *lag* the values it indexes (a restore then re-walks the gap
    /// idempotently), never *lead* them. Folding the live cursor at upload time
    /// instead sealed it ahead of the T0 values.
    pub fn cursor_snapshot(&self) -> Vec<(ChannelId, i64)> {
        self.state
            .lock()
            .unwrap()
            .cursor
            .iter()
            .map(|(c, seq)| (*c, *seq))
            .collect()
    }

    // ── Device-owned-epoch authorship (design §3c) ──────────────────────

    /// Whether this device authored `channel`'s current epoch — i.e. it may send
    /// application traffic without a takeover first. `false` for a channel this
    /// device has never committed on (a fresh device, or one whose epoch a foreign
    /// commit has since advanced): the sender must post a takeover `self_update`
    /// via [`send_commit_gated`](crate::commit_gate) before its first app send.
    pub fn authored_current_epoch(&self, channel: &ChannelId) -> bool {
        self.state
            .lock()
            .unwrap()
            .authored
            .get(channel)
            .copied()
            .unwrap_or(false)
    }

    /// Record that this device now owns `channel`'s current epoch — set after its
    /// own gated commit (add/remove/takeover) is accepted and merged. After this
    /// the device may send application traffic on the epoch without re-taking-over.
    pub fn mark_authored(&self, channel: &ChannelId) {
        self.state.lock().unwrap().authored.insert(*channel, true);
    }

    /// Record that a commit on this device's **own leaf** was authored elsewhere
    /// (another of the user's devices — the `OwnLeafCommit` resync arm) — this
    /// device no longer owns its leaf's lineage, so its next application send
    /// must take over first. Since 2026-08-24 another *member's* commit does NOT
    /// route here (the takeover ping-pong fix — `FaunaCommitGate::
    /// note_foreign_commit` is a documented no-op): the send right rides the own
    /// leaf's latest commit, per `devices.md` § Cross-device MLS group-state
    /// sync's device-owned-epoch invariant.
    pub fn mark_foreign_epoch(&self, channel: &ChannelId) {
        self.state.lock().unwrap().authored.insert(*channel, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        MlsTransportError, PutOutcome, history_path, rpc_transport_get, rpc_transport_get_hash,
        rpc_transport_put,
    };
    use async_trait::async_trait;
    use fauna_client_testkit::block_on;
    use fauna_conversations::address::{Rail, TypedAddress};
    use fauna_conversations::keying::ThreadKey;
    use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
    use fauna_conversations::store::threads::ThreadStore;
    use fauna_conversations::thread::ThreadFlavor;
    use fauna_core::render::RenderDocument;
    use fauna_mls::engine::MlsEngine;
    use fauna_protocol::mls_replica::{
        GetMlsReplicaReply, GetMlsReplicaRequest, KIND_GET, KIND_PUT, PutMlsReplicaReply,
        PutMlsReplicaRequest, ReplicaBase,
    };
    use fauna_protocol::{RpcErrorClass, RpcRequester};
    use serde_bytes::ByteBuf;
    use std::sync::Arc;

    /// Same CAS-enforcing per-path fake nest as `store::tests`, shared behind an
    /// `Arc` so two `MlsStateSync` instances model two devices of one identity.
    /// Honours the `hash_only` probe the way the real handler does (the digest,
    /// no bytes) and counts the two read shapes apart, so a test can pin that a
    /// steady-state sweep costs a probe and never the blob.
    #[derive(Default)]
    struct FakeMlsNest {
        stored: Mutex<HashMap<String, Vec<u8>>>,
        puts: Mutex<u32>,
        blob_gets: Mutex<u32>,
        hash_probes: Mutex<u32>,
    }
    /// `RpcError`-wrapping fake error implementing `RpcErrorClass` so the wrapper's
    /// bounded impl (and its CAS retry) resolves — `RpcError` alone does not
    /// implement `RpcErrorClass`. Mirrors `store::tests::FakeError`.
    #[derive(Debug)]
    enum FakeError {
        Rpc(fauna_protocol::RpcError),
    }
    impl core::fmt::Display for FakeError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Rpc(e) => write!(f, "rpc {}", e.code),
            }
        }
    }
    impl RpcErrorClass for FakeError {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            match self {
                Self::Rpc(e) => Some(e),
            }
        }
    }

    struct SharedNest(Arc<FakeMlsNest>);
    impl RpcRequester for SharedNest {
        type Error = FakeError;
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
                        return Err(FakeError::Rpc(fauna_protocol::RpcError::new(
                            fauna_protocol::mls_replica::CODE_CONFLICT,
                            "error.mls.conflict",
                        )));
                    }
                    *self.0.puts.lock().unwrap() += 1;
                    map.insert(req.path, req.blob.into_vec());
                    fauna_protocol::encode_canonical(&PutMlsReplicaReply {
                        ok: true,
                        extra: Default::default(),
                    })
                }
                KIND_GET => {
                    let req: GetMlsReplicaRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode get");
                    let stored = self.0.stored.lock().unwrap().get(&req.path).cloned();
                    let hash = stored
                        .as_ref()
                        .map(|b| ByteBuf::from(blake3::hash(b).as_bytes().to_vec()));
                    let blob = if req.hash_only {
                        *self.0.hash_probes.lock().unwrap() += 1;
                        None
                    } else {
                        *self.0.blob_gets.lock().unwrap() += 1;
                        stored.map(ByteBuf::from)
                    };
                    fauna_protocol::encode_canonical(&GetMlsReplicaReply {
                        blob,
                        hash,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The transport seam over the fake — the same two-line delegation a slice-5
    /// leg adapter writes (see `store::tests`).
    #[async_trait]
    impl MlsReplicaTransport for SharedNest {
        async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            rpc_transport_get(self, path).await
        }
        async fn get_hash(&self, path: String) -> Result<Option<[u8; 32]>, MlsTransportError> {
            rpc_transport_get_hash(self, path).await
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

    fn keypair() -> ActorKeypair {
        ActorKeypair::from_secret([7u8; 32])
    }

    fn sync(nest: Arc<FakeMlsNest>) -> MlsStateSync {
        MlsStateSync::new(Box::new(SharedNest(nest)), &keypair())
    }

    /// A real one-group provider replica + its ChannelId (so the history hex
    /// lines up with what `provider.channel_ids()` yields).
    fn provider_with_channel() -> (ProviderReplica, ChannelId) {
        let engine = MlsEngine::new_in_memory(keypair()).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::from_secret([200u8; 32])).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let (channel, _welcome) = engine.create_group(&peer_kps).unwrap();
        (ProviderReplica::from_engine(&engine), channel)
    }

    fn history_slice(channel_hex: &str, watermark: i64) -> ChannelHistorySlice {
        let store = ThreadStore::new();
        let addr = |n: &str| TypedAddress::Email {
            email_address: format!("{n}@example.com"),
        };
        let id = store.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: channel_hex.to_string(),
            },
            vec![addr("me"), addr("peer")],
            ThreadFlavor::OneToOne,
            Some("peer".to_string()),
        );
        store.append_message(
            &id,
            MessageSnapshot {
                message_id: MessageId(format!("conv:{channel_hex}:{watermark}")),
                sender: addr("me"),
                sender_display: String::new(),
                body: "own message".into(),
                document: RenderDocument::default(),
                timestamp_ms: watermark * 10,
                subject_line: None,
                badges: MessageBadges::default(),
                reply_to: None,
                reactions: vec![],
                deleted: false,
                is_own: true,
                legal_takedown_ref: None,
                labels: vec![],
                plane_ref: None,
                can_delete: false,
            },
        );
        store
            .snapshot_channel_slice(&id, channel_hex, watermark)
            .unwrap()
    }

    /// Launch gate: a save before `load` is a no-op and writes nothing — the gate
    /// that keeps a stray startup tick from clobbering the user's replica with the
    /// local-only state before the cross-device GET has merged in the others'.
    #[test]
    fn save_before_load_is_gated() {
        let nest = Arc::new(FakeMlsNest::default());
        let s = sync(nest.clone());
        let (provider, _) = provider_with_channel();
        assert!(!block_on(s.save_provider_if_changed(&provider)).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 0, "must not PUT before load");
    }

    /// First run: load returns provider=None, the gate lifts, and the first
    /// genuine provider is persisted; a redundant re-save of the same is skipped.
    #[test]
    fn first_run_saves_then_dedups() {
        let nest = Arc::new(FakeMlsNest::default());
        let s = sync(nest.clone());
        let loaded = block_on(s.load()).unwrap();
        assert!(loaded.provider.is_none(), "first run has no replica");

        let (provider, _) = provider_with_channel();
        assert!(block_on(s.save_provider_if_changed(&provider)).unwrap());
        assert_eq!(*nest.puts.lock().unwrap(), 1);
        // Unchanged → skipped (baseline dedup).
        assert!(!block_on(s.save_provider_if_changed(&provider)).unwrap());
        assert_eq!(
            *nest.puts.lock().unwrap(),
            1,
            "no churn for an unchanged provider"
        );
    }

    /// **A session that could not READ the snapshot must not be able to
    /// OVERWRITE it either.**
    ///
    /// `orchestration::restore_and_wire` refuses to restore a `provider`
    /// snapshot whose seating it could not establish
    /// (`fauna_mls::state_replica::SeatingVerdict`, rule (1)). Refusing is only
    /// half the answer: `load` has already set `provider_base` to that
    /// snapshot, so the ordinary save path would CAS-replace it with this
    /// engine's near-empty state on the first save — destroying a snapshot that
    /// may well be this identity's own, merely unreadable to this build. So
    /// that arm also holds the provider save gate.
    ///
    /// ⚠ Deliberately NOT the foreign-seat arm's behaviour: a snapshot
    /// positively identified as a predecessor's is *supposed* to be taken back
    /// by the successor's first save, and this pin must not be read as
    /// forbidding that.
    #[test]
    fn a_held_provider_gate_writes_nothing_even_when_the_replica_changed() {
        let nest = Arc::new(FakeMlsNest::default());
        let s = sync(nest.clone());
        block_on(s.load()).unwrap();

        let (provider, _) = provider_with_channel();
        s.hold_provider_saves();

        assert!(
            !block_on(s.save_provider_if_changed(&provider)).unwrap(),
            "a held gate must report `false` (skipped), not a write"
        );
        assert_eq!(
            *nest.puts.lock().unwrap(),
            0,
            "…and must put NOTHING: overwriting a snapshot this session could not \
             examine is the data loss the hold exists to prevent"
        );
    }

    /// **A sibling-only group survives this device's next flush.** The CAS
    /// merge folds a concurrent device's groups into the stored replica, but the
    /// running engine never adopts them — the restore doors are the launch
    /// `load` and the own-leaf-commit resync only — so this device's next
    /// export still lacks them. Until 2026-09-01 the merge *result* became the
    /// next merge ancestor, so a flush met a path equal to its ancestor, skipped
    /// the merge, and replaced the path wholesale: every key the sibling had
    /// durably saved was dropped until the sibling's own next flush, and for
    /// good if the sibling never flushed again. The
    /// ancestor is now this device's own last export, so an unadopted sibling
    /// contribution is re-folded on every flush until a launch or resync adopts
    /// it. Ruling: `devices.md` § Cross-device MLS group-state sync → *A
    /// conflict-free merge is kept by the durable plane, not the engine*.
    #[test]
    fn a_siblings_group_survives_this_devices_next_flush() {
        let nest = Arc::new(FakeMlsNest::default());
        let me = sync(nest.clone());
        let sibling = sync(nest.clone());
        block_on(me.load()).unwrap();
        block_on(sibling.load()).unwrap();
        let (mine, my_channel) = provider_with_channel();
        let (theirs, their_channel) = provider_with_channel();
        assert_ne!(
            my_channel, their_channel,
            "two distinct groups under one leaf"
        );

        let stored_channels = || {
            let (p, _) = block_on(me.client.load_provider_with_base()).unwrap();
            let mut v: Vec<String> = p
                .unwrap()
                .channel_ids()
                .iter()
                .map(|c| c.to_string())
                .collect();
            v.sort();
            v
        };
        let both = {
            let mut v = vec![my_channel.to_string(), their_channel.to_string()];
            v.sort();
            v
        };

        // The sibling processed a Welcome this device never saw, and flushed.
        assert!(block_on(sibling.save_provider_if_changed(&theirs)).unwrap());
        // This device's flush meets the sibling's write → three-way merge → union.
        assert!(block_on(me.save_provider_if_changed(&mine)).unwrap());
        assert_eq!(stored_channels(), both, "the first flush merges by union");

        // The next tick: the engine is unchanged (it never adopted the sibling's
        // group), so the export is byte-identical to the last one.
        block_on(me.save_provider_if_changed(&mine)).unwrap();
        assert_eq!(
            stored_channels(),
            both,
            "an unchanged re-export must not take the sibling's group off the path"
        );

        // A later tick with real progress (the ingest cursor moved): a genuine
        // write, and the sibling's group is still folded forward with it.
        me.advance_processed_seq(&my_channel, 8);
        assert!(block_on(me.save_provider_if_changed(&mine)).unwrap());
        let (after, _) = block_on(me.client.load_provider_with_base()).unwrap();
        let after = after.unwrap();
        let mut hexes: Vec<String> = after.channel_ids().iter().map(|c| c.to_string()).collect();
        hexes.sort();
        assert_eq!(
            hexes, both,
            "this device's progress lands without dropping the sibling's group"
        );
        assert_eq!(
            after.cursor(&my_channel),
            Some(8),
            "…and carries this device's own cursor"
        );
    }

    /// **A group a sibling device joined reaches this running engine without a
    /// relaunch** — the door `devices.md` § Cross-device MLS group-state sync
    /// → *A sibling-joined group is adopted mid-session by a targeted import*
    /// ratifies. Two `MlsStateSync` over one nest,
    /// two real engines of one identity. The sibling creates a group with a
    /// peer and flushes its provider; this device's next pass imports the
    /// group (the engine has it, the peer's next message decrypts here) but
    /// hands out no slice yet — a provider can land ahead of its history — and
    /// the pass after the slice lands hands it out to bind, with the cursor
    /// seeded from its watermark. In the steady state a pass costs the tip
    /// probe and never the blob. And this device's own next flush keeps the
    /// sibling's group at the path — the baseline absorbed the import.
    #[test]
    fn a_siblings_group_is_adopted_mid_session_and_bound_once_its_history_lands() {
        use fauna_core::data::Timestamp;
        use fauna_mls::types::{ChannelMessage, ChannelMessageBody};

        let nest = Arc::new(FakeMlsNest::default());
        let me = sync(nest.clone());
        block_on(me.load()).unwrap();
        let my_engine = MlsEngine::new_in_memory(keypair()).unwrap();
        assert!(
            block_on(me.adopt_sibling_groups(&my_engine))
                .unwrap()
                .is_empty(),
            "an empty path adopts nothing"
        );
        assert_eq!(
            *nest.blob_gets.lock().unwrap(),
            1,
            "only the launch load fetched the blob"
        );

        let sibling = sync(nest.clone());
        block_on(sibling.load()).unwrap();
        let sib_engine = MlsEngine::new_in_memory(keypair()).unwrap();
        let peer = MlsEngine::new_in_memory(ActorKeypair::from_secret([200u8; 32])).unwrap();
        let peer_kps = peer.generate_key_packages(1).unwrap();
        let (channel, welcome) = sib_engine.create_group(&peer_kps).unwrap();
        peer.join_from_welcome(welcome).unwrap();
        assert!(
            block_on(sibling.save_provider_if_changed(&ProviderReplica::from_engine(&sib_engine)))
                .unwrap()
        );

        // Provider landed, history not yet: the group is live here, nothing binds.
        let pass = block_on(me.adopt_sibling_groups(&my_engine)).unwrap();
        assert!(
            my_engine.has_group(&channel),
            "the group is imported into the running engine"
        );
        assert_eq!(pass.len(), 1);
        assert_eq!(pass[0].channel, channel);
        assert!(
            pass[0].slice.is_none(),
            "no history slice yet — nothing to bind from"
        );

        // The sibling's slice lands → handed out to bind, cursor from its watermark.
        block_on(sibling.save_history_if_changed(&history_slice(&channel.to_string(), 3))).unwrap();
        let pass = block_on(me.adopt_sibling_groups(&my_engine)).unwrap();
        assert_eq!(pass.len(), 1);
        assert!(
            pass[0].slice.is_some(),
            "the slice is handed out once it has landed"
        );
        assert_eq!(
            me.processed_seq(&channel),
            3,
            "cursor seeded from the slice's watermark"
        );

        // Steady state: a probe, never the blob, nothing to hand out.
        let blobs = *nest.blob_gets.lock().unwrap();
        let probes = *nest.hash_probes.lock().unwrap();
        assert!(
            block_on(me.adopt_sibling_groups(&my_engine))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            *nest.blob_gets.lock().unwrap(),
            blobs,
            "an unchanged tip is never fetched"
        );
        assert_eq!(
            *nest.hash_probes.lock().unwrap(),
            probes + 1,
            "…it is probed"
        );

        // The device that never saw the Welcome decrypts the peer's next message.
        let post = peer
            .encrypt(
                &channel,
                &ChannelMessage {
                    sender: peer.identity_actor_id(),
                    sequence: 9,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("seen on the other device".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        let got = my_engine.decrypt(&channel, &post).unwrap();
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "seen on the other device"));

        // This device's own flush keeps the adopted group at the path.
        assert!(
            block_on(me.save_provider_if_changed(&ProviderReplica::from_engine(&my_engine)))
                .unwrap()
        );
        let (stored, _) = block_on(me.client.load_provider_with_base()).unwrap();
        assert_eq!(stored.unwrap().channel_ids(), vec![channel]);
    }

    /// Two devices of one identity sharing one group `G`, flushed by `A` (who
    /// created it) and adopted by `B`'s launch; then `B` leaves `G` — the
    /// folder-leave's engine half, `forget_group`, which writes no MLS commit —
    /// and flushes. Returns `(nest, A's still-holding engine, G, the peer)`.
    fn a_sibling_left_a_group_this_device_still_holds()
    -> (Arc<FakeMlsNest>, MlsEngine, ChannelId, MlsEngine) {
        let nest = Arc::new(FakeMlsNest::default());
        let peer = MlsEngine::new_in_memory(ActorKeypair::from_secret([200u8; 32])).unwrap();

        let a = sync(nest.clone());
        block_on(a.load()).unwrap();
        let a_engine = MlsEngine::new_in_memory(keypair()).unwrap();
        let (group, _welcome) = a_engine
            .create_group(&peer.generate_key_packages(1).unwrap())
            .unwrap();
        assert!(block_on(a.save_engine_provider_if_changed(&a_engine)).unwrap());

        let b = sync(nest.clone());
        let loaded = block_on(b.load()).unwrap();
        let b_engine = MlsEngine::new_in_memory(keypair()).unwrap();
        assert!(
            loaded
                .provider
                .unwrap()
                .restore_into(&b_engine)
                .unwrap()
                .is_clean()
        );
        assert!(b_engine.has_group(&group), "fixture: B's launch adopted G");
        b_engine.forget_group(&group).unwrap();
        assert!(block_on(b.save_engine_provider_if_changed(&b_engine)).unwrap());
        assert!(
            !listed(&nest).contains(&group),
            "fixture: the leaver's flush takes G off the path (the three-way presence merge)"
        );
        (nest, a_engine, group, peer)
    }

    fn listed(nest: &Arc<FakeMlsNest>) -> Vec<ChannelId> {
        let (p, _) = block_on(sync(nest.clone()).client.load_provider_with_base()).unwrap();
        p.map(|p| p.channel_ids()).unwrap_or_default()
    }

    /// **A folder left on one device stays left on every device of the
    /// account — across a sibling's launch.** `devices.md` § Cross-device MLS
    /// group-state sync → *A group the engine holds but the snapshot does not
    /// list survives the swap*: the swap carries a local-only group only when
    /// it was joined since this device last saw the replica's listing; a group
    /// that listing named and the new snapshot does not was deleted by
    /// another device, and is dropped. Before the rule the carry arm could not
    /// tell the two apart, so the sibling that did not leave carried `G`, its
    /// next flush read `G` as its own addition and listed it again, and the
    /// leaver's next launch held the folder it had left. The join half of the rule is pinned alongside: a group
    /// this device joined after its last flush is carried by the same swap.
    #[test]
    fn a_folder_left_on_one_device_stays_left_across_a_siblings_launch() {
        let (nest, a_engine, group, peer) = a_sibling_left_a_group_this_device_still_holds();
        // A joins another group inside its autosave debounce — never flushed.
        let (joined, _welcome) = a_engine
            .create_group(&peer.generate_key_packages(1).unwrap())
            .unwrap();

        // A relaunches: a fresh sync plane restores the post-leave snapshot into
        // the engine that still holds both.
        let a = sync(nest.clone());
        let loaded = block_on(a.load()).unwrap();
        assert!(
            loaded
                .provider
                .unwrap()
                .restore_into(&a_engine)
                .unwrap()
                .is_clean()
        );
        assert!(
            !a_engine.has_group(&group),
            "the group a sibling left is not carried by this device's launch"
        );
        assert!(
            a_engine.has_group(&joined),
            "a group joined since this device's last flush is still carried"
        );
        assert!(block_on(a.save_engine_provider_if_changed(&a_engine)).unwrap());
        let after = listed(&nest);
        assert!(
            !after.contains(&group),
            "this device's next flush does not list the left group again"
        );
        assert!(after.contains(&joined), "…and lists the join");

        // B relaunches into a fresh engine: the folder stays left.
        let b = sync(nest.clone());
        let loaded = block_on(b.load()).unwrap();
        let b_engine = MlsEngine::new_in_memory(keypair()).unwrap();
        assert!(
            loaded
                .provider
                .unwrap()
                .restore_into(&b_engine)
                .unwrap()
                .is_clean()
        );
        assert!(
            !b_engine.has_group(&group),
            "the leaver's next launch does not hold the folder it left"
        );
    }

    /// The same rule at the other door the swap has: the own-leaf resync
    /// (`resync_provider`) restores the post-leave snapshot mid-session, and
    /// drops the group a sibling left rather than carrying it.
    #[test]
    fn a_folder_left_on_one_device_stays_left_across_a_siblings_resync() {
        let (nest, a_engine, group, _peer) = a_sibling_left_a_group_this_device_still_holds();
        let a = sync(nest.clone());
        block_on(a.load()).unwrap();
        assert!(block_on(a.resync_provider(&a_engine)).unwrap().is_some());
        assert!(
            !a_engine.has_group(&group),
            "the group a sibling left is not carried by the resync"
        );
        block_on(a.save_engine_provider_if_changed(&a_engine)).unwrap();
        assert!(
            !listed(&nest).contains(&group),
            "…and is not listed again by this device's next flush"
        );
    }

    /// The cursor is seeded from the loaded history slice's watermark, and
    /// `advance_processed_seq` moves it forward monotonically.
    #[test]
    fn cursor_seeds_from_watermark_and_advances() {
        let nest = Arc::new(FakeMlsNest::default());
        // Device A stores a provider + a history slice at watermark 5.
        let a = sync(nest.clone());
        block_on(a.load()).unwrap();
        let (provider, channel) = provider_with_channel();
        block_on(a.save_provider_if_changed(&provider)).unwrap();
        block_on(a.save_history_if_changed(&history_slice(&channel.to_string(), 5))).unwrap();

        // Device B (same identity) loads → cursor seeded to 5 from the slice.
        let b = sync(nest.clone());
        let loaded = block_on(b.load()).unwrap();
        assert_eq!(loaded.history.len(), 1, "B pulls A's history slice");
        assert_eq!(b.processed_seq(&channel), 5, "cursor seeded from watermark");

        // The poll loop folds later records → advance is monotonic.
        b.advance_processed_seq(&channel, 8);
        assert_eq!(b.processed_seq(&channel), 8);
        b.advance_processed_seq(&channel, 3); // a lower seq is ignored
        assert_eq!(b.processed_seq(&channel), 8);
    }

    /// The end-to-end bootstrap: device A creates a group, saves provider +
    /// history; device B (fresh, same seed) loads and receives the whole thing —
    /// the provider snapshot, the own-message history slice, and the resume
    /// cursor. This is the slice-4a foundation the 4b/4c rebase + resync compose.
    #[test]
    fn second_device_bootstraps_from_first() {
        let nest = Arc::new(FakeMlsNest::default());
        let a = sync(nest.clone());
        block_on(a.load()).unwrap();
        let (provider, channel) = provider_with_channel();
        let hex = channel.to_string();
        block_on(a.save_provider_if_changed(&provider)).unwrap();
        block_on(a.save_history_if_changed(&history_slice(&hex, 2))).unwrap();

        // Sanity: both blobs are stored under the expected paths.
        {
            let map = nest.stored.lock().unwrap();
            assert!(map.contains_key("provider"));
            assert!(map.contains_key(&history_path(&hex)));
        }

        let b = sync(nest.clone());
        let loaded = block_on(b.load()).unwrap();
        assert_eq!(
            loaded.provider.as_ref().map(|p| p.channel_ids()),
            Some(vec![channel]),
            "B restores A's group from the provider snapshot"
        );
        assert_eq!(loaded.history.len(), 1);
        assert!(
            loaded.history[0].messages.iter().any(|m| m.is_own),
            "B receives A's own-message plaintext (log replay can't reconstruct it)"
        );
        assert_eq!(
            b.processed_seq(&channel),
            2,
            "B resumes ingest from A's watermark"
        );
    }
}
