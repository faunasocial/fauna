//! Shared restore/save orchestration for the cross-device MLS state-replica
//! plane — the leg-agnostic half of what was, until this module, duplicated
//! byte-for-byte across the linux (`conv_backend.rs`) and web
//! (`fauna-wasm/conversations.rs`) legs, and about to be transcribed a third
//! time into the native FFI factory (`libs/fauna-ffi` `conversations_session`,
//! the apple/windows/android legs). Priority #2 (maximize shared Rust) + #1/#4
//! (one shape, no per-leg divergence).
//!
//! A leg keeps only the platform-specific *trigger* glue — GTK `glib` debounce +
//! `runtime.spawn` on linux, `future_to_promise` on web — and calls:
//!
//! * on launch, before the first poll: [`restore_and_wire`] (design §5
//!   restore-before-first-poll) — `sync.load()` → restore the `provider` + each
//!   `history/<ch>` slice, re-bind its channel, then assemble + inject the
//!   device-owned-epoch [`FaunaCommitGate`] + [`MlsSyncCursor`];
//! * after a debounced engine/store mutation: [`snapshot_replica`] on the owning
//!   thread (engine + manager state lives there), then [`save_snapshot`]
//!   off-thread.
//!
//! The split between the synchronous snapshot and the async upload is
//! deliberate: both legs must snapshot on the thread that owns the engine (GTK
//! main / JS main) and only then hand the owned snapshot to an off-thread upload,
//! so the snapshot can never race a concurrent engine mutation. This module owns
//! *what* to snapshot/restore/wire; the leg owns *when* and *on which thread*.

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::ConversationsManager;
use fauna_conversations::backend::{
    BackendError, HistoryPersist, ProviderPersist, SiblingGroupAdopter,
};
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_conversations::store::history::ChannelHistorySlice;
use fauna_mls::engine::MlsEngine;
use fauna_mls::state_replica::ProviderReplica;
use fauna_mls::types::ChannelId;
use fauna_protocol::reconnect::Backoff;

use crate::gate_impl::{BackendCatchUp, BackendChannelSend, FaunaCommitGate, MlsSyncCursor};
use crate::store::MlsReplicaClientError;
use crate::sync::MlsStateSync;

/// A point-in-time snapshot of the replica plane, taken synchronously on the
/// thread that owns the engine + manager, ready to hand to an off-thread
/// [`save_snapshot`]. Separating the (sync) snapshot from the (async) upload is
/// what lets every leg guarantee the snapshot never races a concurrent engine
/// mutation.
pub struct ReplicaSnapshot {
    /// The whole openMLS `provider` KV (all groups), sealed at rest as `provider`.
    pub provider: ProviderReplica,
    /// One `history/<ch>` slice per bound channel that had folded messages at
    /// snapshot time (`save_history_if_changed` drops the unchanged ones).
    pub slices: Vec<ChannelHistorySlice>,
    /// The engine `provider` was exported from, so a landed write records its
    /// listing there ([`MlsEngine::note_replica_listed`]; `devices.md` → *A
    /// sibling's deletion is not a join*).
    pub engine: Arc<MlsEngine>,
}

/// Which half of a [`save_snapshot`] upload failed — so a leg's single log line
/// keeps the provider-vs-channel context the two inline loops used to carry.
#[derive(Debug)]
pub enum SaveReplicaError {
    /// The `provider` put failed.
    Provider(MlsReplicaClientError),
    /// A `history/<ch>` put failed; `channel_hex` names which channel.
    History {
        channel_hex: String,
        source: MlsReplicaClientError,
    },
}

impl std::fmt::Display for SaveReplicaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "provider save failed: {e}"),
            Self::History {
                channel_hex,
                source,
            } => write!(f, "history save for {channel_hex} failed: {source}"),
        }
    }
}

impl std::error::Error for SaveReplicaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(e) | Self::History { source: e, .. } => Some(e),
        }
    }
}

/// Restore the cross-device MLS state replica into a just-built session and
/// inject the device-owned-epoch gate + cursor — the shared body of what was
/// linux `wire_mls_state_sync` / web `restoreMlsState` (design §5
/// restore-before-first-poll). Runs in the login task, **before** both the
/// session-owned key-package replenish and the first poll — the replenish is
/// sequenced *after* this restore precisely so a package minted before it can't
/// lose its private init key to the restore's provider swap (`session.rs`; the
/// linux leg states the same at `app.rs`, `devices.md` § Durability rules Rule
/// 3). In order:
///
/// 1. `sync.load()` fetches + unseals the `provider` + `history/<ch>` replicas
///    (seeding `MlsStateSync`'s per-channel cursor from the **`provider`**, which
///    carries it — one blob, one CAS, so the cursor can never outrun the crypto
///    state it indexes; `devices.md` Rule 2. A provider blob carrying no cursor for a
///    channel falls back to that channel's slice watermark).
/// 2. `provider.restore_into(engine)` rebuilds the openMLS group crypto state —
///    **iff** the snapshot's own-leaf seating is this identity's; that door asks
///    for itself and refuses a predecessor's or an unreadable snapshot
///    (`succession-aftermath.md` rule (1)), and this leg only chooses how to
///    react to a refusal.
/// 3. Each `history/<ch>` slice is restored into the thread store (own-message
///    plaintext incl.) and its channel re-bound (`bind_channel`), so the poll
///    routes — and `bound_channels` enumerates — the restored channels. A
///    malformed hex in a replica this identity itself sealed is near-unreachable;
///    skip the one channel rather than abort the whole restore.
/// 4. `BackendCatchUp` + `BackendChannelSend` + `FaunaCommitGate` assemble over
///    the SAME backend / engine and inject (`set_commit_gate`); `MlsSyncCursor`
///    injects (`set_channel_cursor`) so the poll resumes from the restored
///    cursor. The conversations plane is reached **through the backend**, never
///    handed in separately — that is what routes a gated commit on a
///    foreign-homed channel to the nest that homes it (`BackendChannelSend`'s
///    doc owns the reasoning).
///
/// Returns the number of channels restored. The **only** failure is the
/// `sync.load()` fetch (everything after it is infallible); an `Err` leaves the
/// gate un-injected — today's single-device behavior — so the caller logs + stays
/// single-device. A later save can never then clobber the real nest replica: the
/// un-lifted launch gate no-ops every upload until a successful `load()`.
///
/// Legs call this through [`restore_and_wire_with_retry`], which re-attempts a
/// *transient* (nest-unreachable) failure with backoff so a launch blip does not
/// degrade the whole session to single-device.
pub async fn restore_and_wire(
    sync: Arc<MlsStateSync>,
    backend: &Arc<FaunaMlsBackend>,
    manager: &Arc<ConversationsManager>,
) -> Result<usize, MlsReplicaClientError> {
    let loaded = sync.load().await?;
    let engine = backend.engine();
    if let Some(provider) = &loaded.provider {
        // The succession-time instance of the invariant `restore_and_wire_with_retry`
        // states below: `restore_into` swaps the engine's WHOLE provider KV, so
        // it may only ever be fed this identity's own crypto state. After a
        // succession the successor's path holds the PREDECESSOR's snapshot
        // (ownership moved in the nest's transaction; the `__mls` re-seal
        // re-keyed it so it opens), and restoring it would seat this engine as
        // the retired leaf — decrypting as the credential the ceremony evicts,
        // and answering `CannotRemoveSelf` to its own remove-old. Recognized on
        // the snapshot's own-leaf credential per group, never on the re-seal's
        // outcome (a crashed first session re-runs it as `AlreadyCurrent`). The
        // engine's own state stands — the ceremony device's SQLite holds its
        // join, and the successor's own snapshot is what the ceremony publishes
        // before the switch — and the first successor-side save replaces the
        // occupant by CAS. History slices and thread bindings below restore
        // regardless: readable history is the successor's, a leaf is not
        // (`succession-aftermath.md` § Re-key scope → *What a successor's
        // replica restore may take from a predecessor's*).
        //
        // ⚠ Three answers, three arms. The verdict used to be a bare list of
        // foreign seats restored on empty, which read "could not examine a
        // single group" as "clean" — the fail-open shape rule (1) forbids.
        //
        // The decision itself is `restore_into`'s, not this caller's: it asks
        // the seating question of the engine it would restore into and swaps
        // nothing unless the answer is clean. This arm reads the returned
        // verdict only to choose how to REACT to a refusal — which of the two
        // refusals it was, and therefore whether the save gate must be held.
        let verdict = provider.restore_into(&engine)?;
        if !verdict.foreign.is_empty() {
            // Positively another identity's. Replacing it is the DESIGN: the
            // successor's own first save takes the path back by CAS.
            tracing::warn!(
                channels = verdict.foreign.len(),
                unexaminable = verdict.unexaminable,
                "mls-sync: the provider snapshot at this identity's path seats its groups \
                 under another identity's leaf (a predecessor's, after a succession) — \
                 not restored; this engine keeps its own state and its first save \
                 replaces the snapshot"
            );
        } else if !verdict.is_clean() {
            // Nothing foreign was found, but nothing was established either.
            // This snapshot may well be THIS identity's own — so unlike the arm
            // above, letting the ordinary save path CAS-replace it from a
            // near-empty engine would destroy state we merely failed to read.
            // Hold the provider save gate: this session runs on its own engine
            // state and writes no `provider` blob at all, leaving the snapshot
            // for a build that can read it.
            sync.hold_provider_saves();
            tracing::warn!(
                unexaminable = verdict.unexaminable,
                groups = provider.channel_ids().len(),
                "mls-sync: the provider snapshot at this identity's path could not be \
                 examined (no group's seating could be established) — not restored, and \
                 the provider save gate is HELD so this session cannot overwrite a \
                 snapshot it could not read"
            );
        }
    }
    let restored = loaded.history.len();
    for slice in &loaded.history {
        bind_restored_slice(backend, manager, slice);
    }
    // Then the routing input the slices above CANNOT carry. A slice with no
    // recorded `home_nest_url` decodes as `None`, which is
    // indistinguishable from a genuine same-nest channel, so the loop just ran
    // cannot recover a cross-nest channel whose home was never recorded (a
    // folder group, or the provider-without-slice window) — and
    // the Welcome that could re-teach it is acked once drained, so an
    // established channel is normally never offered one again. For the
    // **folder** half of that population the member's own `ForeignFolder`
    // record is a durable datum independent of the slice, and this is the read
    // that spends it: one custody read, filling only holes, so a channel
    // whose slice did carry a home keeps it (`federation.md` § Cross-nest
    // shared folders + channel append).
    let seeded = backend.seed_channel_homes_from_custody().await;
    if seeded > 0 {
        tracing::info!(
            "mls-sync: seeded {seeded} channel home(s) from this member's foreign-set \
             records — folder channels joined before the slice carried a home"
        );
    }
    // The FaunaMls rail's participant harvest is now an answer about this
    // account rather than a statement about how far the launch has got — but
    // ONLY when this restore actually carried the account's evidence. Until
    // then a foreign peer that does not answer the anonymous discovery probe is
    // a failed lookup, not an email fallthrough (`federation.md` § Peer-auth
    // model → *Discovery-failure semantics*, case 2; `DomainEvidence::
    // Unloaded`).
    //
    // ⚠ **Running is not carrying.** `load()` answers `Ok` with `provider:
    // None` for an account that simply has no replica blob yet, and the history
    // loop above lives inside that `Some`, so the mark used to fire over an
    // EMPTY store — every foreign domain then reporting a positively
    // *established* absence, which is the one thing the email arm may never
    // rest on. The replica IS the account's channel list (the nest's
    // `fauna.conversations.channel.list_for_actor` roster is a delivery-side
    // record — an actor a Welcome was delivered to is on it whether or not
    // any device ever joined — not the account's held groups, and no app
    // reads it; `ThreadStore` is in-memory, rebuilt every launch), so without
    // the replica this client has not loaded the account's conversations and
    // cannot honestly say they are absent.
    //
    // Two ways to have carried it, and the second is not optional: a genuinely
    // empty account has no replica either, and reading that as "not loaded"
    // would deny the email carve-out forever to exactly the user it was written
    // for — one holding no Fauna expectation. The engine's own durable group
    // set separates them: no groups means nothing to load, which is an
    // established emptiness rather than an unread one.
    //
    // ⚠ **And a replica is not the evidence either — the SLICES are.** The
    // provider blob is the account's channel LIST, and nothing downstream reads
    // it: the set the rail spends is `FaunaMlsBackend::known_domains`, harvested
    // by `observe_participants` from thread-store participants, and participants
    // live only in the `history/<ch>` slices — a separate blob behind a separate
    // fetch, which `load()` tolerates missing on purpose (`sync.rs`, "a polled
    // channel whose history blob has not landed yet"). So `provider: Some` with
    // no slices marked the account loaded over an empty `ThreadStore` just as
    // surely as `provider: None` did, and every foreign domain again reported a
    // positively *established* absence.
    //
    // The two blobs come apart with no fault at all: `save_snapshot` below holds
    // Rule 2, but the commit gate's `save_provider_snapshot` CAS-puts the
    // provider ALONE by design (crash-safety steps 2 and 4), so between a first
    // send on a new channel and the debounce that writes its slice the durable
    // state IS `{provider lists X, no history/X}` — which a second device
    // launching in that window reads verbatim.
    //
    // Hence: every CHAT channel the provider lists must have arrived with its
    // slice. `load()` pushes one slice per listed channel that had a stored
    // blob, so the check is containment — the listed channels carrying the
    // durable chat marker (stamped at `bind_channel`, riding this very blob:
    // `devices.md` § Cross-device MLS group-state sync) against the slices
    // that landed. Keyed on the marker and NOT a bare count of the listing,
    // because a scheduling or folder channel is an engine group that never
    // gets a slice, by design — the history blob is what marks a chat thread
    // at restore (`devices.md` § Durability rules, Rule 3) — and counted, one
    // such group held the whole account at `Unloaded` on every launch for
    // ever. A listed channel with neither marker
    // nor slice is a folder group or a channel in the provider-without-slice
    // window, and
    // reads as thread-less: the declared residual, since such a chat channel
    // can never bind again either way. Deliberately NOT the engine's own
    // members instead — `MlsEngine::group_members` answers `ActorId`s and a
    // domain needs a handle, which is not on that path.
    let arrived: std::collections::HashSet<&str> = loaded
        .history
        .iter()
        .map(|s| s.channel_id_hex.as_str())
        .collect();
    let (listed, chat_listed, missing_chat) = match loaded.provider.as_ref() {
        Some(p) => {
            let listed = p.channel_ids();
            let chat: Vec<String> = listed
                .iter()
                .filter(|ch| p.is_channel_chat(ch))
                .map(|ch| ch.to_string())
                .collect();
            let missing: Vec<String> = chat
                .iter()
                .filter(|hex| !arrived.contains(hex.as_str()))
                .cloned()
                .collect();
            (listed.len(), chat.len(), missing)
        }
        None => (0, 0, Vec::new()),
    };
    let carried = loaded.provider.is_some() && missing_chat.is_empty();
    let evidence_landed = carried || backend.engine().list_groups().is_empty();
    if evidence_landed {
        // Here, not at the end of the function: this is the exact instant the
        // restore loop above finished, and nothing below it touches the store.
        // All four legs (FFI-native, tui, linux, web) reach the plane through
        // this function, so one call covers every one of them.
        backend.mark_conversations_loaded();
    } else {
        tracing::info!(
            listed,
            chat_listed,
            carried_slices = loaded.history.len(),
            missing_chat_channels = ?missing_chat,
            "mls-sync: this launch did NOT carry the account's evidence (no provider \
             replica, or one listing chat channels whose history slices did not arrive) \
             while the engine holds groups — a foreign non-answer stays a failed lookup \
             rather than an email fallthrough"
        );
    }
    let catch_up = BackendCatchUp::new(backend, manager);
    let gate = Arc::new(FaunaCommitGate::new(
        Arc::clone(&sync),
        engine,
        BackendChannelSend::new(backend),
        catch_up,
    ));
    backend.set_commit_gate(gate);
    backend.set_channel_cursor(Arc::new(MlsSyncCursor::new(Arc::clone(&sync))));
    // Rule 3 (durable-before-done / save-before-publish): the awaited provider
    // flush a key-package mint drives BEFORE it publishes a package, so a
    // package's fresh private init keys are durable in the replica before a peer
    // can fetch it (`devices.md` § Durability rules Rule 3; closes the
    // launch-window strand). Weak refs — the
    // backend owns this seam strongly, so strong refs would cycle.
    backend.set_provider_persist(Arc::new(MlsProviderPersist {
        backend: Arc::downgrade(backend),
        manager: Arc::downgrade(manager),
        sync: Arc::clone(&sync),
    }));
    // Rule 3 (durable-before-done): the awaited per-mutation history flush the
    // manager drives after every own store mutation, and `bootstrap_group`
    // before the first send's takeover. Weak backend/manager — the backend
    // holds this seam strongly (`OnceLock`), so strong refs would cycle.
    backend.set_history_persist(Arc::new(MlsHistoryPersist {
        backend: Arc::downgrade(backend),
        manager: Arc::downgrade(manager),
        sync: Arc::clone(&sync),
    }));
    // The mid-session door for a group another of this account's devices
    // joined (`devices.md` § Cross-device MLS group-state sync → *A
    // sibling-joined group is adopted mid-session by a targeted import*): the
    // receive sweep runs it first, every trigger. Same weak-ref cycle break.
    backend.set_sibling_group_adopter(Arc::new(MlsSiblingGroupAdopter {
        backend: Arc::downgrade(backend),
        manager: Arc::downgrade(manager),
        sync,
    }));
    Ok(restored)
}

/// Bind one restored `history/<ch>` slice: seed its thread in the store, bind
/// thread ↔ channel on the backend, and re-establish the channel's routing
/// home. The one binding path for a channel this device did not join in this
/// session — the launch restore's loop and the mid-session adoption door both
/// come through here, so a channel adopted from a sibling is bound exactly as
/// one restored at launch. Returns the channel, or `None` for a slice whose
/// channel hex does not parse (warn-logged and skipped, as before).
pub fn bind_restored_slice(
    backend: &Arc<FaunaMlsBackend>,
    manager: &Arc<ConversationsManager>,
    slice: &ChannelHistorySlice,
) -> Option<ChannelId> {
    let thread_id = manager.restore_channel_slice(slice);
    match ChannelId::from_hex(&slice.channel_id_hex) {
        Ok(channel) => {
            backend.bind_channel(thread_id, channel);
            // Rebind the ROUTING input too, not just thread<->channel.
            // `channel_home` is written only by the three Welcome-join
            // paths, and a restored channel's Welcome belongs to a session
            // that is over — so without this the map stays empty and every
            // consumer of `channel_home_url` reads a foreign-homed channel
            // as same-nest: `send_on_channel` takes the local arm (the send
            // blackhole `federation.md` states structurally cannot happen,
            // and an `expect_no_commit_since` handed to a nest that does
            // not share the home log's seq space, i.e. ungated), and all
            // three inbound drains fetch from a nest holding none of the
            // channel's records.
            if let Some(home) = &slice.home_nest_url {
                backend.record_channel_home(channel, home);
            } else if slice.home_same_nest {
                // Re-establish the explicit `SameNest` marker (the total
                // encoding's local half) so a restored local channel is
                // KNOWN same-nest, not merely absent — the pre-guard cannot
                // then re-home it to a peer-declared URL. A slice with `home_same_nest =
                // false` stays absent (unknown): the declared residual.
                backend.record_channel_home(channel, "");
            }
            Some(channel)
        }
        Err(e) => {
            tracing::warn!(
                "mls-sync: skipping channel with bad hex {}: {e}",
                slice.channel_id_hex
            );
            None
        }
    }
}

/// The production [`SiblingGroupAdopter`] — the fifth injected seam, wired by
/// [`restore_and_wire`] beside [`MlsHistoryPersist`]. Runs
/// [`MlsStateSync::adopt_sibling_groups`] over the session's engine and binds
/// each adopted channel whose slice has landed through [`bind_restored_slice`],
/// exactly as the launch binds a restored one. Weak backend/manager — the
/// backend owns this seam strongly; a failed upgrade is a session tearing down,
/// nothing to adopt.
struct MlsSiblingGroupAdopter {
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    sync: Arc<MlsStateSync>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl SiblingGroupAdopter for MlsSiblingGroupAdopter {
    async fn adopt_if_changed(&self) -> Result<usize, BackendError> {
        let (Some(backend), Some(manager)) = (self.backend.upgrade(), self.manager.upgrade())
        else {
            return Ok(0);
        };
        let adopted = self
            .sync
            .adopt_sibling_groups(&backend.engine())
            .await
            .map_err(|e| BackendError::Internal(format!("sibling-group adoption: {e}")))?;
        let mut bound = 0;
        for channel in adopted {
            if let Some(slice) = &channel.slice
                && bind_restored_slice(&backend, &manager, slice).is_some()
            {
                bound += 1;
            }
        }
        Ok(bound)
    }
}

/// Snapshot ONE bound channel's `history/<ch>` slice and CAS-save it, awaited —
/// the single-channel twin of [`snapshot_replica`] + [`save_snapshot`], driving
/// `devices.md` § Durability rules **Rule 3 (durable-before-done)**. Returns
/// `Ok(false)` when the channel has no bound thread (nothing to persist — e.g.
/// a scheduling/folder channel, or a teardown race). Like the debounced
/// autosave's snapshot, the slice capture is a synchronous point-in-time copy
/// under the store's own locks; only the upload awaits.
pub async fn persist_channel_history(
    backend: &Arc<FaunaMlsBackend>,
    manager: &Arc<ConversationsManager>,
    sync: &MlsStateSync,
    channel: ChannelId,
) -> Result<bool, MlsReplicaClientError> {
    let Some(thread_id) = backend.thread_for_channel(&channel) else {
        return Ok(false);
    };
    let Some(mut slice) = manager.snapshot_channel_slice(
        &thread_id,
        &channel.to_string(),
        sync.processed_seq(&channel),
    ) else {
        return Ok(false);
    };
    // Stamp the channel's home onto the slice — BOTH halves of the total
    // encoding. The store knows threads, not nests, so this is the one layer
    // holding both: `channel_home` is a RAM map learned from the cross-nest
    // Welcome (or the local-creation marker), and until it was persisted here a
    // relaunch lost it for good. The same-nest marker is
    // the security half: persisting it lets the restore
    // re-establish `SameNest`, so the pre-guard cannot re-home this channel
    // after a relaunch.
    slice.home_nest_url = backend.channel_home_url(&channel);
    slice.home_same_nest = backend.channel_is_same_nest(&channel);
    sync.save_history_if_changed(&slice).await?;
    Ok(true)
}

/// The production [`HistoryPersist`] — the third injected seam beside
/// [`FaunaCommitGate`] and [`MlsSyncCursor`], wired by [`restore_and_wire`].
/// Holds the backend/manager **weakly** (the backend owns this seam strongly,
/// the same cycle break as [`BackendCatchUp`]); a failed upgrade means the
/// session is tearing down — nothing left worth persisting, no-op `Ok`.
struct MlsHistoryPersist {
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    sync: Arc<MlsStateSync>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl HistoryPersist for MlsHistoryPersist {
    async fn persist_channel(&self, channel: ChannelId) -> Result<(), BackendError> {
        let (Some(backend), Some(manager)) = (self.backend.upgrade(), self.manager.upgrade())
        else {
            return Ok(());
        };
        persist_channel_history(&backend, &manager, &self.sync, channel)
            .await
            .map(|_| ())
            .map_err(|e| BackendError::Internal(format!("durable history flush: {e}")))
    }
}

/// The production [`ProviderPersist`] — the Rule-3 **save-before-publish** flush a
/// key-package mint drives BEFORE it publishes a package, wired by
/// [`restore_and_wire`] beside [`MlsHistoryPersist`]. Holds the backend/manager
/// **weakly** (the backend owns this seam strongly, the same cycle break as
/// [`MlsHistoryPersist`]); a failed upgrade means the session is tearing down —
/// nothing to persist, and reporting "not durable" (`Ok(false)`) is the safe
/// answer, so a racing mint refuses to publish rather than ship a doomed package.
struct MlsProviderPersist {
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    sync: Arc<MlsStateSync>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl ProviderPersist for MlsProviderPersist {
    async fn persist_provider(&self) -> Result<bool, BackendError> {
        let (Some(backend), Some(manager)) = (self.backend.upgrade(), self.manager.upgrade())
        else {
            return Ok(false);
        };
        // Rule 2 ordering (history first, then the `provider` blob that holds the
        // fresh init keys + the cursor) — [`save_snapshot`] returns whether the
        // provider blob durably landed. Both a transport failure and a
        // launch-gated no-op mean "not durable"; the backend refuses to publish a
        // key package on anything but a genuine write, closing the launch-window
        // strand.
        let snapshot = snapshot_replica(&backend, &manager, &self.sync);
        save_snapshot(&self.sync, &snapshot)
            .await
            .map_err(|e| BackendError::Internal(format!("durable provider flush: {e}")))
    }
}

/// How a [`restore_and_wire_with_retry`] launch ended.
#[derive(Debug)]
pub enum RestoreRetryEnd {
    /// The plane is fully wired; carries the number of channels restored
    /// (`restore_and_wire`'s `Ok`).
    Wired(usize),
    /// A **permanent** failure — a nest-answered rejection (any refusal), a seal/codec
    /// failure, an over-cap blob. The leg logs and stays single-device for the
    /// session, exactly today's one-shot behavior; the launch gate stays down,
    /// so no save can clobber the real replica.
    Failed(MlsReplicaClientError),
    /// The session was torn down while the retry waited (a `Weak` upgrade
    /// failed) — logout, or the linux e2e session re-injection. Nothing to do.
    SessionDropped,
}

/// First backoff before re-attempting a transient launch `load()` failure.
const RESTORE_RETRY_INITIAL_MS: u64 = 1_000;
/// Backoff ceiling — `DEFAULT_CONV_POLL_SECS` magnitude, so a long outage is
/// probed about as often as the receive loop would poll.
const RESTORE_RETRY_CAP_MS: u64 = 30_000;

/// [`restore_and_wire`] with launch resilience: a **transient** `sync.load()`
/// failure (the nest unreachable at launch) is retried with exponential backoff
/// — 1 s doubling to a 30 s cap, indefinitely — so the session converges to the
/// fully-wired state (channels restored, gate + cursor injected) without user
/// action once the nest is reachable, instead of degrading to single-device
/// until relaunch (`devices.md` § Cross-device MLS group-state sync, launch
/// resilience).
///
/// Three invariants:
///
/// * **Restore-before-first-poll is preserved** (design §5): every leg awaits
///   this before its first poll, so the retry *blocks* the poll rather than
///   re-loading after inbound processing ran. That is load-bearing, not a
///   convenience: `ProviderReplica::restore_into` **swaps** the engine's whole
///   provider KV, so a late re-load would clobber post-launch engine state —
///   rewinding sender ratchets (nonce reuse) and wiping groups joined since
///   launch. Blocking is free: while the nest is unreachable, every poll would
///   fail too.
/// * **The retry goes through `load()` itself, never around it** — each attempt
///   re-runs [`restore_and_wire`] whole, so the launch save-gate lifts only
///   inside a successful `load()` (the review-pinned `loaded = true` ordering).
/// * **Only a transport fault retries.** A nest-answered rejection is permanent
///   ([`MlsReplicaClientError::is_transient`]): any nest refusal
///   must yield today's single-device fallback after ONE
///   attempt, not a forever-blocked receive loop.
///
/// `backend`/`manager` are `Weak` and upgraded per attempt (and not held across
/// the sleep), so a session torn down mid-retry ends the loop instead of a
/// zombie task pinning session internals forever.
pub async fn restore_and_wire_with_retry(
    sync: Arc<MlsStateSync>,
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
) -> RestoreRetryEnd {
    retry_restore(sync, backend, manager, sleep_ms).await
}

/// [`restore_and_wire_with_retry`] generic over the sleep, so the tier_1 tests
/// drive the loop with a recording no-op timer under the crate's executor-free
/// `block_on` (which cannot park on a real timer).
async fn retry_restore<S, F>(
    sync: Arc<MlsStateSync>,
    backend: Weak<FaunaMlsBackend>,
    manager: Weak<ConversationsManager>,
    mut sleep: S,
) -> RestoreRetryEnd
where
    S: FnMut(u64) -> F,
    F: core::future::Future<Output = ()>,
{
    let mut backoff = Backoff::new(
        Duration::from_millis(RESTORE_RETRY_INITIAL_MS),
        Duration::from_millis(RESTORE_RETRY_CAP_MS),
    );
    let mut attempt: u32 = 1;
    loop {
        // Upgrade inside the block so the strong refs drop before the sleep —
        // a long retry must not pin a torn-down session's internals.
        let outcome = {
            let (Some(backend), Some(manager)) = (backend.upgrade(), manager.upgrade()) else {
                return RestoreRetryEnd::SessionDropped;
            };
            restore_and_wire(Arc::clone(&sync), &backend, &manager).await
        };
        match outcome {
            Ok(restored) => return RestoreRetryEnd::Wired(restored),
            Err(e) if e.is_transient() => {
                let backoff_ms = backoff.ceiling().as_millis() as u64;
                tracing::warn!(
                    "mls-sync: launch replica load attempt {attempt} failed transiently ({e}); \
                     retrying in {backoff_ms} ms"
                );
                sleep(backoff_ms).await;
                backoff.grow();
                attempt += 1;
            }
            Err(e) => {
                // Permanent: the session stays single-device and no restore will
                // swap the provider, so a key-package mint may publish directly
                // again (`FaunaMlsBackend::expect_replica_restore`).
                if let Some(backend) = backend.upgrade() {
                    backend.abandon_replica_restore();
                }
                return RestoreRetryEnd::Failed(e);
            }
        }
    }
}

/// Milliseconds-shaped call-site adapter over the shared cross-target sleep, for
/// the launch-retry backoff. ⚠ Milliseconds (the `fauna-onboarding-machine`
/// prior art, including its ms-vs-secs lesson).
///
/// Kept as a named function rather than inlined: `retry_restore` takes the sleep
/// as a parameter so the tests can substitute `recording_sleep`, and that
/// injection seam is the point (`e2e-conventions.md` § convention 14).
async fn sleep_ms(ms: u64) {
    fauna_sleep::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Snapshot the provider + every bound channel's history slice — the shared body
/// of linux `arm_replica_debounce`'s snapshot / web `saveMlsState`'s snapshot.
/// **Synchronous, and must run on the thread that owns the engine + manager**
/// (their state lives there); hand the result to [`save_snapshot`] off-thread.
/// Every bound channel is snapshotted — `save_history_if_changed`'s dedup makes
/// unchanged channels no-op uploads, so no per-channel dirty tracking is needed.
/// Each slice's watermark is `MlsStateSync`'s processed-seq cursor (advanced by
/// the poll loop), so it reflects exactly what this device folded. The *durable*
/// cursor is `cursor_snapshot()` folded into the `provider` blob **here**, at
/// snapshot time (`devices.md` Rule 2); the slice's watermark is informational,
/// plus the fallback for a channel the `provider` carries no cursor for.
pub fn snapshot_replica(
    backend: &Arc<FaunaMlsBackend>,
    manager: &Arc<ConversationsManager>,
    sync: &MlsStateSync,
) -> ReplicaSnapshot {
    // Capture the ingest cursor BEFORE the crypto values, and fold it into the
    // snapshot's `provider`, so the autosave seals a content-consistent
    // `{values, cursor}` pair @ snapshot time (T0) — never the LIVE cursor at
    // upload time (T1), which the poll may have advanced past these values in the
    // save window.
    // Cursor-before-values guarantees `cursor <= values`: a foreign record folded
    // between the two reads advances the real cursor past our captured one, but
    // its crypto lands in `values`, so a restore re-walks it idempotently (the
    // safe lag). The reverse order could seal a cursor ahead of the values.
    let cursors = sync.cursor_snapshot();
    let engine = backend.engine();
    let provider = ProviderReplica::from_engine(&engine).with_cursors(&cursors);
    let slices = backend
        .bound_channels()
        .into_iter()
        .filter_map(|ch| {
            let thread_id = backend.thread_for_channel(&ch)?;
            let mut slice = manager.snapshot_channel_slice(
                &thread_id,
                &ch.to_string(),
                sync.processed_seq(&ch),
            )?;
            // Stamp the home here too, exactly as `persist_channel_history`
            // does. This is the SECOND writer of `history/<ch>`, and on the
            // side that actually has a foreign home it is usually the FIRST to
            // run: `persist_channel_history` fires on `bootstrap_group` (the
            // creator — same-nest by construction) and on the Rule-3 flush
            // after an **own mutation**, so a member who joined by cross-nest
            // Welcome and has only *received* since persists solely through
            // this debounced autosave. Leaving it `None` here wrote the
            // routing datum away as absent and re-created the stuck population
            // an earlier finding had closed — on today's binaries, not only
            // pre-fix ones. It also restores
            // `save_history_if_changed`'s dedup for a foreign-homed channel,
            // whose stamped stored slice could never equal an unstamped
            // snapshot, so every tick re-sealed and re-PUT it.
            slice.home_nest_url = backend.channel_home_url(&ch);
            slice.home_same_nest = backend.channel_is_same_nest(&ch);
            Some(slice)
        })
        .collect();
    ReplicaSnapshot {
        provider,
        slices,
        engine,
    }
}

/// Seal + upload a [`ReplicaSnapshot`] — the shared async body of the two legs'
/// debounced save. Attempts **every** `history/<ch>` slice (so a transient failure
/// on one upload never skips the rest — own-message history is user-irrecoverable,
/// so we maximize what lands per tick), then the `provider` **iff every history
/// upload succeeded**. Returns the FIRST error, or `Ok(wrote)` where `wrote` is
/// whether the `provider` blob **durably landed** (a fresh CAS write, not a
/// launch-gated / unchanged no-op) — the debounced-autosave callers ignore it,
/// but the Rule-3 [`ProviderPersist`] gate ([`MlsProviderPersist`]) uses it to
/// decide whether a key-package mint may publish. The `_if_changed` dedup + the
/// launch gate make an unchanged or pre-restore upload a no-op. The
/// caller decides how to surface the error (linux warns + swallows; web rejects its
/// promise and the SPA logs + swallows) — a transient save must never surface on the
/// page.
///
/// # Why history first, and why the provider is skipped on any history failure
///
/// `devices.md` § Cross-device MLS group-state sync, **Rule 2 (save-ordering)**:
/// persist what cannot be reconstructed before the state that records having
/// consumed it. The `provider` blob carries the per-channel ingest cursor
/// (`ProviderReplica::with_cursors`, folded in by `snapshot_replica` at snapshot
/// time), so landing it declares "this device folded every record up to seq C". If a
/// `history/<ch>` upload failed, the plaintext for some of `(..C]` did **not** land
/// — and it is unrecoverable: a sender cannot MLS-decrypt its own application
/// messages, and a foreign record in that range cannot be re-decrypted either
/// because the restored provider has already consumed those ratchet generations.
///
/// So the two partial outcomes are not symmetric:
///
/// * **history lands, provider does not** (this function's failure mode) — durable
///   state is `{provider @ older, history @ newer}`. The next launch restores the
///   older provider, seeds the cursor from *inside* it, and re-walks the gap
///   idempotently. Nothing is lost; the cursor can never outrun the provider,
///   because they are one blob and one CAS.
/// * **provider lands, history does not** (what saving the provider first allowed) —
///   durable state is `{provider @ newer, history @ older}`. The cursor says
///   "consumed up to C", so the next launch resumes past records whose plaintext was
///   never persisted. **Permanent, user-irrecoverable loss.**
///
/// Skipping the provider costs one deferred tick — the live engine is simply ahead
/// of its durable snapshot until the next debounce, exactly as it is between ticks
/// anyway.
pub async fn save_snapshot(
    sync: &MlsStateSync,
    snapshot: &ReplicaSnapshot,
) -> Result<bool, SaveReplicaError> {
    let mut first_err: Option<SaveReplicaError> = None;
    for slice in &snapshot.slices {
        if let Err(e) = sync.save_history_if_changed(slice).await {
            first_err.get_or_insert(SaveReplicaError::History {
                channel_hex: slice.channel_id_hex.clone(),
                source: e,
            });
        }
    }
    match first_err {
        // Rule 2: the cursor inside `provider` must never claim a record whose
        // history did not land. Leave the durable provider where it is; the next
        // tick re-snapshots and retries the whole pair. (Provider not saved, so
        // the Rule-3 gate treats this as "not durable".)
        Some(e) => Err(e),
        // The provider already carries the SNAPSHOT-time cursor (folded in
        // `snapshot_replica`); seal it as-is. Do NOT re-fold the live cursor —
        // the poll may have advanced it past these T0 values in the save window.
        // The bool is whether the provider blob durably landed (`false` = a
        // launch-gated / unchanged no-op) — the Rule-3 gate refuses to publish
        // a key package on anything but a genuine write.
        None => match sync
            .save_provider_folded_if_changed(&snapshot.provider)
            .await
        {
            Err(e) => Err(SaveReplicaError::Provider(e)),
            Ok(wrote) => {
                // A landed write is a listing this device authored: record it
                // on the engine, the swap's ancestor (`devices.md` → *A
                // sibling's deletion is not a join*).
                if wrote {
                    snapshot
                        .engine
                        .note_replica_listed(&snapshot.provider.channel_ids());
                }
                Ok(wrote)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        MlsReplicaTransport, MlsTransportError, PATH_PROVIDER, PutOutcome, history_path,
    };
    use crate::sync::MlsStateSync;
    use crate::test_conv::{ConvNest, FakeConvNest, block_on};
    use async_trait::async_trait;
    use fauna_conversations::ConversationsManager;
    use fauna_conversations::address::TypedAddress;
    use fauna_conversations::backend::{ConversationsRpc, RailBackend};
    use fauna_conversations::backends::fauna_mls::{
        FaunaMlsBackend, ingest_welcome, poll_inbound_conv,
    };
    use fauna_conversations::message::{MessageBadges, MessageId, MessageSnapshot};
    use fauna_conversations::thread::ThreadFlavor;
    use fauna_core::data::Timestamp;
    use fauna_core::identity::{ActorId, ActorKeypair};
    use fauna_core::render::RenderDocument;
    use fauna_mls::engine::MlsEngine;
    use fauna_mls::types::{ChannelEnvelope, ChannelMessage, ChannelMessageBody};
    use fauna_protocol::mls_replica::ReplicaBase;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Trivial CAS-enforcing in-memory replica transport (the `store`/`sync`
    /// suites cover the wire encode + conflict classification). `Clone` shares
    /// the store — two clones model one nest serving two devices of one identity.
    ///
    /// `fail_prefix` injects a hard transport fault on every `put` whose path
    /// starts with it (`""` disables) — the partial-save faults Rule 2 is about.
    /// `fail_gets` injects a queue of errors served one per `get` — the
    /// launch-`load()` faults the retry loop is about.
    #[derive(Default, Clone)]
    struct MemReplica {
        stored: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        fail_prefix: Arc<Mutex<String>>,
        fail_gets: Arc<Mutex<Vec<MlsTransportError>>>,
    }

    impl MemReplica {
        /// Make every subsequent `put` to a path under `prefix` fail hard.
        fn fail_puts_under(&self, prefix: &str) {
            *self.fail_prefix.lock().unwrap() = prefix.to_string();
        }
        fn clear_faults(&self) {
            self.fail_prefix.lock().unwrap().clear();
        }
        /// Queue errors to serve on the next `get` calls (drained FIFO; empty ⇒
        /// gets succeed) — models a nest unreachable for the first N launch
        /// fetches, or a nest rejecting the restore.
        fn fail_next_gets(&self, errors: Vec<MlsTransportError>) {
            let mut q = self.fail_gets.lock().unwrap();
            *q = errors;
            q.reverse(); // serve in the given order via pop()
        }
        /// The blob durably stored at `path`, unsealed back into a `ProviderReplica`
        /// is the caller's job; here we only need presence/bytes.
        fn raw(&self, path: &str) -> Option<Vec<u8>> {
            self.stored.lock().unwrap().get(path).cloned()
        }
        /// A short digest of the blob at `path` — sealed replica blobs are kilobytes,
        /// so a raw `assert_eq!` on them buries the failure message.
        fn digest(&self, path: &str) -> Option<String> {
            self.raw(path)
                .map(|b| blake3::hash(&b).to_hex()[..16].into())
        }
    }

    #[async_trait]
    impl MlsReplicaTransport for MemReplica {
        async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
            if let Some(err) = self.fail_gets.lock().unwrap().pop() {
                return Err(err);
            }
            Ok(self.stored.lock().unwrap().get(&path).cloned())
        }
        async fn put(
            &self,
            path: String,
            blob: Vec<u8>,
            base: ReplicaBase,
        ) -> Result<PutOutcome, MlsTransportError> {
            {
                let prefix = self.fail_prefix.lock().unwrap();
                if !prefix.is_empty() && path.starts_with(prefix.as_str()) {
                    return Err(MlsTransportError::fault(format!(
                        "injected fault on {path}"
                    )));
                }
            }
            let mut map = self.stored.lock().unwrap();
            let current = map.get(&path).map(|b| *blake3::hash(b).as_bytes());
            let matches = match base {
                ReplicaBase::Absent => current.is_none(),
                ReplicaBase::Hash(h) => current == Some(h),
            };
            if !matches {
                return Ok(PutOutcome::Conflict);
            }
            map.insert(path, blob);
            Ok(PutOutcome::Stored)
        }
    }

    fn keypair(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    /// A one-own-message history slice for `channel_hex` at `watermark` — the
    /// cross-device payload the nest-ordered log can NEVER reconstruct on a second
    /// device (the owner's own sent plaintext: a sender can't MLS-decrypt its own
    /// application messages). Built directly (not via a `ThreadStore`) so the test
    /// asserts against a known `MessageId`/body; mirrors `sync.rs`'s `history_slice`.
    fn own_message_slice(channel_hex: &str, watermark: i64) -> ChannelHistorySlice {
        let addr = |n: &str| TypedAddress::Email {
            email_address: format!("{n}@example.com"),
        };
        ChannelHistorySlice {
            channel_id_hex: channel_hex.to_string(),
            label: "peer".to_string(),
            flavor: ThreadFlavor::OneToOne,
            participants: vec![addr("me"), addr("peer")],
            messages: vec![MessageSnapshot {
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
            }],
            watermark,
            ..Default::default()
        }
    }

    /// Build the production device graph (engine + registered backend + loaded
    /// sync) the slice-5 legs assemble, up to the point [`restore_and_wire`] takes
    /// over. `load` is called here for device A only (to lift the save gate before
    /// a snapshot); device B hands an unloaded `sync` to `restore_and_wire`.
    fn device(
        engine: &Arc<MlsEngine>,
        conv_nest: &Arc<FakeConvNest>,
        replica: &MemReplica,
        seed: u8,
    ) -> (
        Arc<FaunaMlsBackend>,
        Arc<ConversationsManager>,
        Arc<MlsStateSync>,
    ) {
        let conv: Arc<dyn ConversationsRpc> = Arc::new(ConvNest(conv_nest.clone()));
        let manager = ConversationsManager::new();
        let backend = Arc::new(FaunaMlsBackend::new(
            engine.clone(),
            conv,
            "alice",
            engine.identity_actor_id(),
        ));
        manager.register_backend(backend.clone());
        let sync = Arc::new(MlsStateSync::new(Box::new(replica.clone()), &keypair(seed)));
        (backend, manager, sync)
    }

    /// The same channel bound to a thread that carries **no messages yet** — the
    /// window between a welcome materializing the thread and the first message
    /// folding into it, in which the 1.5 s autosave debounce can fire.
    fn empty_slice(channel_hex: &str) -> ChannelHistorySlice {
        let mut slice = own_message_slice(channel_hex, 0);
        slice.messages.clear();
        slice
    }

    /// A fresh, message-less 1:1 thread with a **Fauna** peer, as
    /// `send_new_thread` materializes it before the first send. Created via the
    /// Rust-only `restore_channel_slice` (the manager's compose/picker surface
    /// needs a resolving nest); the placeholder hex never matters — the first
    /// `send` bootstraps a real channel and re-keys the thread to it.
    fn unsent_fauna_thread_slice(me: ActorId, peer: ActorId) -> ChannelHistorySlice {
        ChannelHistorySlice {
            channel_id_hex: "unbootstrapped".to_string(),
            label: "bob".to_string(),
            flavor: ThreadFlavor::OneToOne,
            participants: vec![
                TypedAddress::Fauna {
                    handle: "alice".into(),
                    actor_id: me,
                },
                TypedAddress::Fauna {
                    handle: "bob".into(),
                    actor_id: peer,
                },
            ],
            messages: vec![],
            watermark: 0,
            ..Default::default()
        }
    }

    /// Assemble a device the way the production launch does — [`restore_and_wire`]
    /// over the shared replica, which injects the commit gate + cursor (and the
    /// history-persist seam) exactly as every leg's login task does. Returns the
    /// restored-channel count alongside the parts.
    #[allow(clippy::type_complexity)]
    fn launched_device(
        engine: &Arc<MlsEngine>,
        conv_nest: &Arc<FakeConvNest>,
        replica: &MemReplica,
        seed: u8,
    ) -> (
        Arc<FaunaMlsBackend>,
        Arc<ConversationsManager>,
        Arc<MlsStateSync>,
        usize,
    ) {
        let (backend, manager, sync) = device(engine, conv_nest, replica, seed);
        let restored = block_on(restore_and_wire(Arc::clone(&sync), &backend, &manager))
            .expect("launch restore ok");
        (backend, manager, sync, restored)
    }

    /// Persist `channel`'s slice with **no home recorded** —
    /// carrying neither the foreign home nor the same-nest marker. The current
    /// `persist_channel_history` stamps both from `channel_home`, and a
    /// locally-created channel is now marked `SameNest` (`bootstrap_group`), so
    /// a home-less fixture must strip them: the home-less cross-nest
    /// population is exactly the slices persisted with neither datum recorded
    /// (`home_nest_url: None`, `home_same_nest: false` → restores as **absent**
    /// from the routing map, the recovery target and the
    /// bounded residual).
    ///
    /// The stored blob must be **cleared first**: `save_history_cas` merges the
    /// slice being saved with whatever is at rest, and `merge_history_slices`
    /// (correctly, as defence in depth) preserves a `SameNest` marker across the
    /// merge — so a home-less slice cannot be produced by overwriting the
    /// `SameNest` slice `bootstrap_group` already wrote; it must be written into
    /// an empty slot.
    fn persist_homeless_slice(
        backend: &Arc<FaunaMlsBackend>,
        manager: &Arc<ConversationsManager>,
        sync: &Arc<MlsStateSync>,
        replica: &MemReplica,
        channel: ChannelId,
    ) {
        let thread_id = backend
            .thread_for_channel(&channel)
            .expect("the fixture channel is bound");
        let mut slice = manager
            .snapshot_channel_slice(
                &thread_id,
                &channel.to_string(),
                sync.processed_seq(&channel),
            )
            .expect("the fixture channel has a slice");
        slice.home_nest_url = None;
        slice.home_same_nest = false;
        // Drop the SameNest slice the bootstrap wrote, so the CAS save lands the
        // home-less slice verbatim instead of merging the marker back in.
        replica
            .stored
            .lock()
            .unwrap()
            .remove(&history_path(&channel.to_string()));
        block_on(sync.save_history_if_changed(&slice)).expect("home-less history persist");
    }

    /// **A foreign-homed channel must still route to its home nest after a
    /// relaunch** (`federation.md` § Cross-nest shared folders + channel append;
    /// `devices.md` § the routed-send precondition).
    ///
    /// `FaunaMlsBackend::channel_home` is the input `send_on_channel` picks
    /// `channel.send` vs `channel.send_remote` from, and the same input all
    /// three inbound drains pass as `channel_fetch`'s `home_nest_url`. Its only
    /// production writers are the three Welcome-**join** paths, so a channel
    /// established in an earlier session has no recorded home in this one — and
    /// the launch restore, which rebinds thread↔channel, did not rebind it.
    /// Every consumer then read a foreign-homed channel as same-nest.
    ///
    /// The assertion is WHICH NEST holds the record, not that the send returned
    /// `Ok`: a mis-routed send succeeds locally, which is exactly why the
    /// blackhole was invisible. `federation.md` states structurally that a
    /// gated commit "cannot address a nest the channel does not live on", and
    /// `devices.md` adds that a commit sent to any other nest is not merely
    /// mis-filed but **ungated**, because `expect_no_commit_since` is a seq in
    /// the home log's space.
    ///
    /// ⚠ The two pre-existing routing tests cannot express this: `gate_impl.rs`
    /// records the home by hand in the *sending* session, and its sibling has no
    /// home at all — so "no home recorded" and "same nest" are one state to that
    /// suite, and that conflation is what let this through. Here the home is
    /// recorded ONLY in the session that ends, and the send is made by the
    /// session that follows it.
    #[test]
    fn a_foreign_homed_channel_still_routes_home_after_a_relaunch() {
        const HOME: &str = "https://home.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let home_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(HOME, Arc::clone(&home_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // ── Session 1: the channel is established and learns its home, the way
        // a cross-nest Welcome teaches it. ──
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "before the relaunch".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();

        backend_a.record_channel_home(channel, HOME);
        assert_eq!(
            backend_a.channel_home_url(&channel).as_deref(),
            Some(HOME),
            "fixture: session 1 knows the home, or the relaunch below proves nothing"
        );
        // Persist with the home known — this is the write the restore reads.
        block_on(persist_channel_history(
            &backend_a, &manager_a, &sync_a, channel,
        ))
        .expect("history persist");

        // ── Session 2: a relaunch. Its `channel_home` starts empty, and no
        // Welcome will ever be re-delivered for an established channel. ──
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, _manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(restored, 1, "fixture: the channel restored");

        assert_eq!(
            backend_b.channel_home_url(&channel).as_deref(),
            Some(HOME),
            "the relaunched session lost the channel's home: the routing input \
             lives one session, so every consumer of `channel_home_url` now \
             reads this foreign-homed channel as same-nest"
        );

        // The product observable: which nest the record lands on.
        let before_home = home_nest.records_on(&channel);
        let before_local = conv_nest.records_on(&channel);
        block_on(backend_b.send_on_channel(
            &channel,
            b"after the relaunch".to_vec(),
            None,
            Vec::new(),
        ))
        .expect("the routed send succeeds");

        assert_eq!(
            home_nest.records_on(&channel),
            before_home + 1,
            "the send did not reach the channel's HOME nest — co-members read \
             the home log, so a record that never arrives there is one nobody \
             sees"
        );
        assert_eq!(
            conv_nest.records_on(&channel),
            before_local,
            "the send landed on the member's OWN nest instead: the blackhole \
             `federation.md` states structurally cannot happen, and an \
             `expect_no_commit_since` handed to a nest that does not share the \
             home log's seq space is ungated, not merely mis-filed"
        );
    }

    /// **The debounced autosave must stamp the channel's home too — it is the
    /// only writer a Welcome *recipient* runs until their first own mutation**
    /// (`federation.md` § Cross-nest shared folders + channel append).
    ///
    /// `history/<ch>` has two writers. [`persist_channel_history`] (the Rule-3
    /// flush) stamped `home_nest_url`; [`snapshot_replica`] did not. Their split
    /// is exactly wrong for the side that *has* a foreign home: the Rule-3 flush
    /// fires on `bootstrap_group` — the creator, same-nest by construction — and
    /// after an **own** store mutation, so a member who joined by cross-nest
    /// Welcome and has since only *received* persists solely through the
    /// autosave. Their slice went to rest saying `home_nest_url: None`, and the
    /// next launch read the channel as same-nest: the same blackhole an earlier
    /// finding closed, still being created on current
    /// binaries.
    ///
    /// ⚠ This is why the sibling above cannot stand alone as the pin: it calls
    /// [`persist_channel_history`] by hand, so it exercises the one writer that
    /// was already correct. Here that call is deliberately **absent** — the only
    /// persist is `snapshot_replica` + `save_snapshot`, the real autosave pair.
    #[test]
    fn the_debounced_autosave_persists_a_channels_home_not_only_the_rule_3_flush() {
        const HOME: &str = "https://home.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let home_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(HOME, Arc::clone(&home_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // Session 1: establish the channel, learn its home, and persist ONLY the
        // way a receiving member does — the debounced autosave.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "before the relaunch".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();

        backend_a.record_channel_home(channel, HOME);
        assert_eq!(
            backend_a.channel_home_url(&channel).as_deref(),
            Some(HOME),
            "fixture: session 1 knows the home, or the relaunch below proves nothing"
        );

        // The ONLY persist from here on. No `persist_channel_history` — that is
        // the whole point: this models the member who has not sent since joining.
        let snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);
        block_on(save_snapshot(&sync_a, &snapshot)).expect("autosave ok");

        // Session 2: a relaunch reading what the autosave left at rest.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, _manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(restored, 1, "fixture: the channel restored");

        assert_eq!(
            backend_b.channel_home_url(&channel).as_deref(),
            Some(HOME),
            "the autosave wrote the slice away with no home, so the relaunch \
             reads this foreign-homed channel as same-nest — a recipient who \
             has not sent since joining never runs the Rule-3 flush that was \
             the only stamping writer"
        );

        // The product observable: which nest the record lands on.
        let before_home = home_nest.records_on(&channel);
        let before_local = conv_nest.records_on(&channel);
        block_on(backend_b.send_on_channel(
            &channel,
            b"after the relaunch".to_vec(),
            None,
            Vec::new(),
        ))
        .expect("the routed send succeeds");
        assert_eq!(
            home_nest.records_on(&channel),
            before_home + 1,
            "the send did not reach the channel's HOME nest"
        );
        assert_eq!(
            conv_nest.records_on(&channel),
            before_local,
            "the send landed on the member's OWN nest instead — the blackhole \
             `federation.md` states structurally cannot happen"
        );
    }

    /// **A re-delivered Welcome must re-teach the home of a channel restored
    /// from a HOME-LESS slice** (`federation.md` § Cross-nest shared folders +
    /// channel append — the "Stated, not closed" paragraph).
    ///
    /// That paragraph named a re-delivered Welcome as the recovery for a
    /// cross-nest channel whose slice carries no `ChannelHistorySlice.home_nest_url`. No path performed it: all three recorded the home *below* their
    /// idempotency guard, and `ingest_welcome`'s guard is `thread_for_channel` —
    /// the very map the launch restore repopulates — so on a restored channel it
    /// returned before the record every time. The doc asserted a recovery the
    /// same commit's own test comment denied.
    ///
    /// ⚠ The fixture starts from `home_nest_url: None` **with a foreign home** —
    /// the at-rest state of a provider-without-slice window or a folder group, which no existing
    /// pin can express: they all start from a slice this commit's own code
    /// wrote, where "no home recorded" and "same nest" are one state.
    #[test]
    fn a_re_delivered_welcome_re_records_the_home_of_a_home_less_channel() {
        const HOME: &str = "https://home.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let home_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(HOME, Arc::clone(&home_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // Session 1: a home-less slice. The channel is cross-nest, but its
        // slice goes to rest carrying neither the home nor the same-nest marker,
        // because neither datum was recorded.
        // `persist_homeless_slice` reproduces exactly that at-rest state — the
        // current writer would stamp `SameNest` for this locally-bootstrapped
        // channel, which is the same-nest-marker fix and would
        // (correctly) refuse the re-delivery below; the genuine home-less
        // population is the absent one.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "written before the restore".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();
        persist_homeless_slice(&backend_a, &manager_a, &sync_a, &replica, channel);

        // Session 2: a current binary restores that home-less slice.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(restored, 1, "fixture: the channel restored");
        assert_eq!(
            backend_b.channel_home_url(&channel),
            None,
            "fixture: the restore cannot invent a home the slice never carried \
             — this is the stuck state the recovery has to lift"
        );
        assert!(
            !backend_b.channel_is_same_nest(&channel),
            "fixture: a home-less slice restores as ABSENT (unknown), not \
             SameNest — else the pre-guard recovery below is (correctly) refused"
        );

        // The recovery the goal doc names: a Welcome is re-delivered for this
        // already-joined channel. It short-circuits at the idempotency guard (no
        // second, init-key-spending join — the bytes are never parsed on that
        // path), and that return is precisely what the home record used to sit
        // below.
        block_on(ingest_welcome(
            &backend_b,
            &manager_b,
            &channel_hex,
            b"",
            HOME,
        ))
        .expect("a re-delivered welcome is idempotent, not an error");

        assert_eq!(
            backend_b.channel_home_url(&channel).as_deref(),
            Some(HOME),
            "the re-delivered Welcome taught this session nothing: it returned \
             at the idempotency guard, above the home record — so the recovery \
             `federation.md` names for the home-less population never ran"
        );

        // The product observable: which nest the record lands on.
        let before_home = home_nest.records_on(&channel);
        let before_local = conv_nest.records_on(&channel);
        block_on(backend_b.send_on_channel(
            &channel,
            b"after the recovery".to_vec(),
            None,
            Vec::new(),
        ))
        .expect("the routed send succeeds");
        assert_eq!(
            home_nest.records_on(&channel),
            before_home + 1,
            "the send did not reach the channel's HOME nest"
        );
        assert_eq!(
            conv_nest.records_on(&channel),
            before_local,
            "the send landed on the member's OWN nest instead"
        );
    }

    /// **A blank-home Welcome must not cost an absent channel its recovery**.
    ///
    /// The sibling below bounds *which* home a peer may name. This one bounds
    /// *whether* it names one at all — the case that bound does not reach,
    /// because a blank has no URL for the nest side to resolve against the
    /// sender's verified origin. Omitting `origin_nest_url` on the **federation**
    /// door takes its no-declared-origin arm and relays `nest_url: None`, which
    /// the client blanks; the pre-guard used to write `SameNest` for it, and
    /// `SameNest` is deliberately immovable.
    ///
    /// So the general property under test is **recoverability, not routing**:
    /// `SameNest` and absent both read as "no foreign home", so the immediate
    /// send goes to the same place either way. What the plant destroyed was the
    /// channel's ability to ever learn its real home again — the genuine
    /// cross-nest re-delivery found no hole, and the durable folder seed fills
    /// `Vacant` only. **No unauthenticated write may produce a routing state a
    /// later authenticated datum cannot correct.**
    ///
    /// Both writes run through the real door (`ingest_welcome`'s re-delivery
    /// arm, whose idempotency guard returns before the join — so garbage bytes
    /// never reach the engine and the pre-guard record above it is exactly the
    /// write under test). The fixture is the population the pre-guard exists to
    /// serve: a **home-less** slice, restoring as absent.
    #[test]
    fn a_blank_welcome_cannot_cost_an_absent_channel_its_real_home() {
        const REAL_HOME: &str = "https://real-home.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let home_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(REAL_HOME, Arc::clone(&home_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // ── Session 1: bind a channel, then put it to rest with
        // no home recorded — no foreign home, no same-nest marker. That is the
        // population the pre-guard exists to recover, and the only one it can
        // still fill. ──
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "a dm whose home is elsewhere".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();
        persist_homeless_slice(&backend_a, &manager_a, &sync_a, &replica, channel);

        // ── Session 2: the relaunch that restores it as ABSENT. ──
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(restored, 1, "fixture: the channel restored");
        assert_eq!(
            backend_b.channel_home_url(&channel),
            None,
            "fixture: a home-less slice must restore with NO recorded home — \
             otherwise the pre-guard has no hole and this test proves nothing"
        );
        assert!(
            !backend_b.channel_is_same_nest(&channel),
            "fixture: absent, not SameNest — the two are distinguishable and \
             this test is about the entry that is genuinely absent"
        );

        // The hostile write: an unauthenticated re-delivery declaring NO origin.
        block_on(ingest_welcome(
            &backend_b,
            &manager_b,
            &channel_hex,
            b"",
            "",
        ))
        .expect("a re-delivered welcome is idempotent, not an error");

        // The genuine cross-nest re-delivery, arriving afterwards. THIS is the
        // authenticated datum the blank must not have locked out.
        block_on(ingest_welcome(
            &backend_b,
            &manager_b,
            &channel_hex,
            b"",
            REAL_HOME,
        ))
        .expect("a re-delivered welcome is idempotent, not an error");

        assert_eq!(
            backend_b.channel_home_url(&channel),
            Some(REAL_HOME.to_string()),
            "the blank pre-guard write pinned the channel and the genuine \
             cross-nest re-delivery could no longer record its home: an \
             unauthenticated peer made a routing state no authenticated datum \
             can correct"
        );
    }

    /// **A Welcome naming an already-joined SAME-NEST channel cannot plant a
    /// foreign home**.
    ///
    /// The pre-guard `record_channel_home_if_absent` runs at the top of all
    /// three Welcome paths, before any MLS authentication of the re-delivery —
    /// that position is the re-delivery recovery and must stay. What confines
    /// it is the map's total encoding: a channel this device joined or created
    /// under current code carries an explicit same-nest record, so the
    /// unauthenticated pre-guard write finds no hole to fill. Before the
    /// marker, a same-nest channel had NO entry — absence, not a recorded
    /// "same nest" — and the pre-guard's `or_insert` filled it with any
    /// peer-declared URL, silently redirecting the victim's drain (and its
    /// fetch metadata) for every ordinary local DM and group to a nest of the
    /// attacker's choosing; the autosave then made the plant durable.
    ///
    /// The attack runs through the real door (`ingest_welcome`'s re-delivery
    /// arm), across a relaunch (the plant was durable, so the marker must be
    /// too), and the assertion ends on the product observable: the send still
    /// lands on the LOCAL nest. Channels persisted with no home recorded
    /// remain fillable — the declared, bounded residual (their slices carry no
    /// marker and route (a) of `federation.md`'s conversations-remainder entry
    /// rules out inventing one); what bounds the plant there is the nest-side
    /// verified-origin resolution pinned in `federation_handlers.rs`.
    #[test]
    fn a_planted_welcome_cannot_rehome_a_same_nest_channel() {
        const EVIL: &str = "https://evil.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let evil_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(EVIL, Arc::clone(&evil_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // ── Session 1: an ordinary same-nest channel, established by this
        // device's own send (`bootstrap_group` — the creator's channel is
        // same-nest by construction, and the bootstrap marks it so). ──
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "an ordinary local dm".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();

        // In-session plant attempt: the pre-guard write a re-delivered hostile
        // Welcome performs. The explicit same-nest record must leave no hole.
        backend_a.record_channel_home_if_absent(channel, EVIL);
        assert_eq!(
            backend_a.channel_home_url(&channel),
            None,
            "the pre-guard filled a same-nest channel with a peer-declared URL: \
             the channel this device itself created reads as a hole"
        );

        // Persist with the marker known — the write the restore reads.
        block_on(persist_channel_history(
            &backend_a, &manager_a, &sync_a, channel,
        ))
        .expect("history persist");

        // ── Session 2: a relaunch, then the re-delivered hostile Welcome
        // (garbage bytes never reach the engine — the idempotency guard
        // returns the existing thread first, and the pre-guard record runs
        // above it, which is exactly the write under test). ──
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(restored, 1, "fixture: the channel restored");

        block_on(ingest_welcome(
            &backend_b,
            &manager_b,
            &channel_hex,
            b"",
            EVIL,
        ))
        .expect("a re-delivered welcome is idempotent, not an error");

        assert_eq!(
            backend_b.channel_home_url(&channel),
            None,
            "the planted URL appeared: the same-nest marker did not survive the \
             relaunch, so the pre-guard re-homed a local channel to the attacker"
        );

        // The product observable: the send still lands on the LOCAL nest.
        let before_local = conv_nest.records_on(&channel);
        let before_evil = evil_nest.records_on(&channel);
        block_on(backend_b.send_on_channel(&channel, b"still local".to_vec(), None, Vec::new()))
            .expect("the send succeeds");
        assert_eq!(
            conv_nest.records_on(&channel),
            before_local + 1,
            "the send did not reach the member's own nest"
        );
        assert_eq!(
            evil_nest.records_on(&channel),
            before_evil,
            "the send was relayed to the planted nest"
        );
    }

    /// A [`FolderCustodySink`] that answers only the population read — the
    /// member's own foreign-set rows, which is all the launch
    /// seed consults. Every other method keeps its default.
    struct ForeignSetRecords(Vec<([u8; 32], String)>);

    #[async_trait]
    impl fauna_conversations::backend::FolderCustodySink for ForeignSetRecords {
        // The custody-ingest half of the seam is not what the launch seed
        // consults; a member with no sealed envelope to fetch is exactly the
        // state a relaunch finds.
        async fn fetch_sealed_envelope(
            &self,
            _channel_id_hex: &str,
        ) -> Option<fauna_conversations::backend::FetchedEnvelope> {
            None
        }

        async fn merge_and_persist(
            &self,
            _channel_id: &[u8; 32],
            _payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
            _may_move: bool,
        ) -> bool {
            true
        }

        async fn foreign_homes(&self) -> Vec<([u8; 32], String)> {
            self.0.clone()
        }
    }

    /// **A home-less cross-nest FOLDER channel must recover its home from the
    /// member's own `ForeignFolder` record — no Welcome re-delivery, no init
    /// key spent** (`federation.md` § Cross-nest shared folders + channel
    /// append — the "Stated, not closed" paragraph, which names this record as
    /// one of the two data durable independently of the slice).
    ///
    /// The sibling pin above proves the *mechanism* — a re-delivered Welcome
    /// now re-teaches the home. It does not make the population whole, and the
    /// goal doc says so: an inbox Welcome is acked once drained, so a channel
    /// established in an earlier session is normally never offered one again.
    /// That leaves the home-less cross-nest population reachable only by a
    /// datum outside the slice — for a shared folder, the `ForeignFolder` row
    /// the join wrote into this member's own folder-keys custody.
    ///
    /// ⚠ The fixture is the same one the sibling uses and for the same reason:
    /// a slice at rest with `home_nest_url: None` **and** a foreign home, which
    /// is that population's actual state and which no pin written
    /// against this commit's own writer can express. What differs is the
    /// recovery under test — here nothing is re-delivered at all; the second
    /// session's restore is handed only the durable record, exactly as a
    /// relaunch is.
    ///
    /// The assertion is WHICH NEST holds the record. A mis-routed send returns
    /// `Ok` from the member's own nest, which is precisely why this blackhole
    /// was invisible for the whole home-less population.
    ///
    /// This is also the **seed-seam trust pin**: the launch spends the at-rest record AS
    /// WRITTEN — the decided semantics for records with no verified origin,
    /// accepted as the inviter-binding TOFU
    /// residual (`federation.md` § Cross-nest shared folders + channel append,
    /// the declared-residual prose; the seed's own doc comment carries the
    /// grounds). A future change that makes the seed distrust unmarked records
    /// reds this pin — deliberately, since that change deletes the recovery
    /// under test.
    #[test]
    fn a_home_less_folder_channels_home_is_seeded_from_its_foreign_set_record() {
        const HOME: &str = "https://home.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let home_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(HOME, Arc::clone(&home_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // Session 1: a home-less slice. The channel is cross-nest, but its
        // slice goes to rest carrying neither the home nor the same-nest marker,
        // because neither datum was recorded.
        // `persist_homeless_slice` reproduces that at-rest state (the current
        // writer would stamp `SameNest` for this locally-bootstrapped channel).
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "written before the restore".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();
        persist_homeless_slice(&backend_a, &manager_a, &sync_a, &replica, channel);

        // Session 2: a current binary relaunches. The member's own folder-keys custody
        // still holds the foreign-set row its cross-nest folder join wrote —
        // the one datum that outlived both the slice and the drained Welcome.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        backend_b.set_folder_custody_sink(Arc::new(ForeignSetRecords(vec![(
            channel.0,
            HOME.to_string(),
        )])));
        let restored = block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("launch restore ok");
        assert_eq!(restored, 1, "fixture: the channel restored");

        assert_eq!(
            backend_b.channel_home_url(&channel).as_deref(),
            Some(HOME),
            "the restore left this channel home-less: the slice could not carry \
             the home and no Welcome was re-delivered, so the member's own \
             `ForeignFolder` record is the only datum left — and the launch did \
             not spend it"
        );

        // The product observable: which nest the record lands on.
        let before_home = home_nest.records_on(&channel);
        let before_local = conv_nest.records_on(&channel);
        block_on(backend_b.send_on_channel(&channel, b"after the seed".to_vec(), None, Vec::new()))
            .expect("the routed send succeeds");
        assert_eq!(
            home_nest.records_on(&channel),
            before_home + 1,
            "the send did not reach the channel's HOME nest"
        );
        assert_eq!(
            conv_nest.records_on(&channel),
            before_local,
            "the send landed on the member's OWN nest instead"
        );
    }

    /// **The seed may only ever fill a hole** — a channel whose own slice
    /// carries a home keeps it, even when a stale `ForeignFolder` record names
    /// a different nest.
    ///
    /// The two data can disagree: `ForeignFolder` is the *set's* record and is
    /// refreshed on re-accept, while `ChannelHistorySlice.home_nest_url` is the
    /// **channel's** own and is stamped by both `history/<ch>` writers. The
    /// channel's own record is the more specific of the two, which is why the
    /// seed runs after the restore's slice loop and writes `if_absent`. Without
    /// that ordering the recovery for the home-less population would become a
    /// way to *re-route* a correctly-homed channel — turning a fix for stuck
    /// routing into a source of it.
    #[test]
    fn the_custody_seed_never_overwrites_a_home_the_slice_carried() {
        const SLICE_HOME: &str = "https://slice-home.example";
        const STALE_RECORD: &str = "https://stale-record.example";

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let slice_home_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(SLICE_HOME, Arc::clone(&slice_home_nest));
        let stale_nest = Arc::new(FakeConvNest::default());
        conv_nest.register_peer_nest(STALE_RECORD, Arc::clone(&stale_nest));

        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // Session 1: a CURRENT binary — the channel's home is recorded, so the
        // slice carries it to rest.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "written by the current binary".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        let channel = ChannelId::from_hex(&channel_hex).unwrap();
        backend_a.record_channel_home(channel, SLICE_HOME);
        block_on(persist_channel_history(
            &backend_a, &manager_a, &sync_a, channel,
        ))
        .expect("history persist");

        // Session 2: the relaunch sees BOTH data, disagreeing.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        backend_b.set_folder_custody_sink(Arc::new(ForeignSetRecords(vec![(
            channel.0,
            STALE_RECORD.to_string(),
        )])));
        block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("launch restore ok");

        assert_eq!(
            backend_b.channel_home_url(&channel).as_deref(),
            Some(SLICE_HOME),
            "the custody seed overwrote the home the channel's OWN slice \
             carried — it may only ever fill a hole"
        );

        let before = slice_home_nest.records_on(&channel);
        block_on(backend_b.send_on_channel(
            &channel,
            b"still routed by the slice".to_vec(),
            None,
            Vec::new(),
        ))
        .expect("the routed send succeeds");
        assert_eq!(
            slice_home_nest.records_on(&channel),
            before + 1,
            "the send was re-routed away from the home the slice named"
        );
        assert_eq!(
            stale_nest.records_on(&channel),
            0,
            "the send followed the stale foreign-set record instead of the \
             channel's own at-rest home"
        );
    }

    /// **The durability contract: an own message is durable once `send()`
    /// returns** (`devices.md` § Durability rules, Rule 3). Drives the REAL
    /// production path end to end — `manager.send` → `bootstrap_group`
    /// (key-package fetch, `create_group`, `bind_channel`, Welcome) →
    /// `ensure_takeover` (Rule-1 provider CAS-puts) → wire send → own-message
    /// append — then simulates a **hard quit inside `REPLICA_DEBOUNCE`**: no
    /// debounced autosave ever fires (none is attached), so the only durable
    /// state is what the send path itself persisted. The relaunched device must
    /// restore the conversation WITH the own message; a sender cannot
    /// MLS-decrypt its own application messages, so nothing else can ever
    /// rebuild it (the user-reported "list populated, zero bubbles" loss).
    #[test]
    fn own_message_survives_a_hard_quit_immediately_after_send() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // The peer has published one key package (the nest pool), so the real
        // MLS bootstrap can fetch it. Its engine never has to join.
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        // Device A launches clean (empty replica) and sends on a new 1:1.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, _sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        manager_a.set_compose_body(tid.clone(), "own message before the quit".into());
        block_on(manager_a.send(tid.clone())).expect("send succeeds");
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("the first send bootstrapped + bound a channel");
        // ── The process exits HERE, inside the debounce window. ──

        // Relaunch as a fresh same-identity device (the same-device restart
        // restores from the replica identically — `restore_into` swaps the KV).
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(
            restored, 1,
            "the conversation must restore after a quit right after send() \
             returned — its history slice never became durable"
        );
        let channel = ChannelId::from_hex(&channel_hex).unwrap();
        let tid_b = backend_b
            .thread_for_channel(&channel)
            .expect("the restored channel re-bound to a thread");
        let slice_b = manager_b
            .snapshot_channel_slice(&tid_b, &channel_hex, 0)
            .expect("restored thread present");
        assert_eq!(
            slice_b.messages.len(),
            1,
            "the relaunched device restored the own message — send() must not \
             return before the message is durable"
        );
        assert!(slice_b.messages[0].is_own);
        assert_eq!(slice_b.messages[0].body, "own message before the quit");
    }

    /// **A quit mid-send (after the bootstrap, before the manager's append)
    /// must still restore a reachable thread.** `bootstrap_group` binds the
    /// channel and delivers the Welcome, and the takeover CAS-puts the
    /// `provider` — so from that instant the durable provider lists a chat
    /// channel. If no `history/<ch>` blob accompanies it, the next launch
    /// creates no thread and `poll_inbound_conv` early-returns with no binding:
    /// the channel is permanently invisible AND unreachable while the peer
    /// (who got the Welcome) keeps talking into it — wrong-fix (i) of the
    /// `devices.md` KNOWN GAP, reachable today in this crash window. Driving
    /// `backend.send` directly IS the production prefix of `manager.send`; the
    /// append/flush that follow it are exactly what the crash skips.
    #[test]
    fn a_quit_mid_send_after_bootstrap_still_restores_a_reachable_thread() {
        use fauna_conversations::compose::ComposeState;

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        conv_nest.register_key_package(
            &bob.identity_actor_id(),
            bob.generate_key_packages_bytes(1).unwrap().remove(0),
        );

        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, _sync_a, _) = launched_device(&alice, &conv_nest, &replica, 1);
        let tid = manager_a.restore_channel_slice(&unsent_fauna_thread_slice(
            alice.identity_actor_id(),
            bob.identity_actor_id(),
        ));
        let detail = manager_a
            .thread_detail(tid.clone())
            .expect("materialized thread");
        let compose = ComposeState {
            body_draft: "never appended".into(),
            ..Default::default()
        };
        block_on(backend_a.send(&detail, &compose, &[])).expect("wire send ok");
        // ── The process dies HERE — inside `manager::send`, after the wire
        //    send, before the own-message append. ──
        let channel_hex = backend_a
            .channel_binding_hex(&tid)
            .expect("bootstrap bound a channel");

        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, _manager_b, _sync_b, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(
            restored, 1,
            "a (possibly empty) history slice must accompany any provider-listed \
             chat channel, so the thread restores instead of going invisible"
        );
        let channel = ChannelId::from_hex(&channel_hex).unwrap();
        assert!(
            backend_b.bound_channels().contains(&channel),
            "the restored channel is re-bound — peers' later messages surface \
             instead of the channel being permanently unreachable"
        );
    }

    /// **An empty `history/<ch>` slice saved first must never hide the messages a
    /// later save carries.**
    ///
    /// `snapshot_replica` walks every bound channel unconditionally, so the
    /// debounced autosave can seal a slice with `messages: []` in the window after
    /// `bind_channel` but before the first message folds. If that empty blob won,
    /// the next launch would restore the thread — so the conversation still LISTS —
    /// with **zero message bubbles**, and the owner's own sent plaintext (which the
    /// nest-ordered log can never re-serve, since a sender cannot MLS-decrypt its
    /// own application messages) would be permanently unrecoverable. That is a
    /// user-irrecoverable data loss, not a cosmetic gap.
    ///
    /// The CAS union in `save_history_cas` (`store.rs`) is what prevents it. This
    /// pins the heal against the production save *ordering* — empty first, full
    /// second — which the sibling `snapshot_restore_roundtrips_bound_channel_history`
    /// never exercises (it populates the thread before its only save).
    #[test]
    fn an_empty_first_slice_never_hides_a_later_saves_messages() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, _welcome) = alice.create_group(&bob_kps).unwrap();
        let channel_hex = channel.to_string();

        let (backend_a, manager_a, sync_a) = device(&alice, &conv_nest, &replica, 1);
        block_on(sync_a.load()).unwrap(); // lift the save gate

        // 1. The welcome window: the thread exists and the channel is bound, but
        //    nothing has folded yet. The debounce fires and seals a message-less slice.
        let tid = manager_a.restore_channel_slice(&empty_slice(&channel_hex));
        backend_a.bind_channel(tid, channel);
        let empty_snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);
        assert_eq!(
            empty_snapshot.slices[0].messages.len(),
            0,
            "precondition: the first autosave seals a message-less slice"
        );
        block_on(save_snapshot(&sync_a, &empty_snapshot)).expect("empty save ok");

        // 2. The owner's own message lands; the next debounce snapshots + saves it.
        manager_a.restore_channel_slice(&own_message_slice(&channel_hex, 7));
        sync_a.advance_processed_seq(&channel, 7);
        let full_snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);
        assert_eq!(
            full_snapshot.slices[0].messages.len(),
            1,
            "precondition: the second autosave carries the owner's message"
        );
        block_on(save_snapshot(&sync_a, &full_snapshot)).expect("full save ok");

        // 3. Relaunch as a fresh same-identity device: the restore must yield the
        //    message, not the empty slice that landed first.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        let restored = block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("restore ok");
        assert_eq!(restored, 1, "device B restored the one history slice");

        let tid_b = backend_b
            .thread_for_channel(&channel)
            .expect("channel bound to a thread on device B");
        let slice_b = manager_b
            .snapshot_channel_slice(&tid_b, &channel_hex, 0)
            .expect("restored thread present on device B");
        assert_eq!(
            slice_b.messages.len(),
            1,
            "the relaunched device restored the thread WITH its message — an empty \
             first slice must never hide a later save (a conversation that lists but \
             shows zero bubbles after a restart is exactly this failure)"
        );
    }

    /// The extraction contract: [`snapshot_replica`] + [`save_snapshot`] on one
    /// device round-trip the openMLS `provider` crypto state, and
    /// [`restore_and_wire`] rebuilds it into a fresh same-identity device and
    /// injects the gate/cursor without panicking. The populated-`history/<ch>`
    /// path (restore_channel_slice + bind_channel) is covered by the sibling
    /// `snapshot_restore_roundtrips_bound_channel_history`, and end-to-end by the
    /// linux + web tier_3 GUI proofs.
    #[test]
    fn snapshot_save_restore_roundtrips_provider() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // Device A: alice runs a real two-member group, then snapshots + saves.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, _welcome) = alice.create_group(&bob_kps).unwrap();
        let epoch_a = alice.current_epoch(&channel).unwrap();

        let (backend_a, manager_a, sync_a) = device(&alice, &conv_nest, &replica, 1);
        block_on(sync_a.load()).unwrap(); // lift the save gate

        let snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);
        block_on(save_snapshot(&sync_a, &snapshot)).expect("save ok");
        assert!(
            block_on(replica.get(PATH_PROVIDER.to_string()))
                .unwrap()
                .is_some(),
            "save_snapshot uploaded the provider blob"
        );

        // Device B: a fresh same-identity device restores from the same replica.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);

        let restored =
            block_on(restore_and_wire(sync_b, &backend_b, &manager_b)).expect("restore ok");

        assert_eq!(
            restored, 0,
            "device A saved no history slices (no bound thread)"
        );
        assert_eq!(
            alice2.current_epoch(&channel).unwrap(),
            epoch_a,
            "restore_and_wire rebuilt alice's group crypto into the fresh device",
        );
    }

    /// **The restore DECISION, not the verdict: `restore_into` swaps nothing
    /// for an unexaminable snapshot, and this session cannot overwrite it
    /// either.**
    ///
    /// Asserted at the real seam rather than on the verdict struct, because a
    /// fix could keep the verdict's list empty and gate the restore elsewhere —
    /// what matters is that the engine is not re-seated and the blob is not
    /// replaced (`succession-aftermath.md` § Re-key scope → *What a successor's
    /// replica restore may take from a predecessor's*, rule (1)).
    ///
    /// The snapshot here decodes cleanly and names one well-formed 32-byte
    /// channel, but its `values` hold nothing that group loads from — the
    /// version-skew shape rule (3) makes ordinary, since the blob crosses to
    /// the identity's other devices and those may run different versions within
    /// a major. Before the fix this scored "clean" and its whole KV was swapped
    /// into the engine.
    #[test]
    fn an_unexaminable_provider_snapshot_is_neither_restored_nor_overwritten() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // Alice's device has a real group of its own — the state that must
        // survive an unreadable snapshot sitting at her path.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (own_channel, _welcome) = alice.create_group(&bob_kps).unwrap();
        let own_epoch = alice.current_epoch(&own_channel).unwrap();

        // Plant a snapshot nothing can examine at the provider path.
        let unreadable = ProviderReplica::unexaminable_for_test(ChannelId([9u8; 32]));
        let (backend, manager, sync) = device(&alice, &conv_nest, &replica, 1);
        block_on(sync.publish_provider(&unreadable)).expect("plant the snapshot");
        let planted = block_on(replica.get(PATH_PROVIDER.to_string()))
            .unwrap()
            .expect("the snapshot is at the path");

        block_on(restore_and_wire(Arc::clone(&sync), &backend, &manager))
            .expect("an unreadable snapshot must not fail the launch");

        // The engine kept working on its own group throughout — a sanity check,
        // NOT the discriminator (measured: an in-memory engine keeps groups it
        // has already loaded even after a KV swap, so leaf, epoch and even
        // `from_engine` reads all pass against the very bug this pins).
        assert_eq!(alice.current_epoch(&own_channel).unwrap(), own_epoch);

        // THE discriminator: the blob at the path. `load` already made the
        // unreadable snapshot the save baseline, so with the restore refused
        // but the save gate open, the first save CAS-replaces a snapshot that
        // may well be this identity's own with this engine's state. That is the
        // whole consequence of restoring — or of refusing without holding the
        // gate — and it is the one thing the fixture can observe.
        let snapshot = snapshot_replica(&backend, &manager, &sync);
        block_on(save_snapshot(&sync, &snapshot)).expect("save path runs");
        assert_eq!(
            block_on(replica.get(PATH_PROVIDER.to_string())).unwrap(),
            Some(planted),
            "the unreadable snapshot must still be at the path — a session that \
             could not READ it must not be able to DESTROY it"
        );
    }

    /// Rule-3 **save-before-publish** at the real seam ([`MlsProviderPersist`],
    /// the one [`restore_and_wire`] injects): the flush a key-package mint awaits
    /// reports the provider **durably landed** only AFTER the launch restore has
    /// lifted the save gate. So a mint in the launch window is told "not durable" and the
    /// backend refuses to publish, while a mint on the healthy path both lands the
    /// provider blob (fresh init keys included) and reports `true`.
    ///
    /// The counterpart to the fauna-conversations
    /// `keypackage_mint_refuses_to_publish_until_the_provider_replica_is_durable`
    /// pin (which drives the backend's refusal through a *mock* seam): here the
    /// REAL `snapshot_replica` + `save_snapshot` seam is exercised with **no**
    /// synchronous autosave observer — durability rides the save-before-publish
    /// flush alone, exactly as production does. Pre-fix, the mint had no such
    /// flush and published against the debounced autosave, which does not exist
    /// yet in this window.
    #[test]
    fn provider_persist_reports_durable_only_after_the_launch_gate_lifts() {
        use fauna_conversations::backend::ProviderPersist;

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a) = device(&alice, &conv_nest, &replica, 1);

        let seam = MlsProviderPersist {
            backend: Arc::downgrade(&backend_a),
            manager: Arc::downgrade(&manager_a),
            sync: Arc::clone(&sync_a),
        };

        // ── The launch window: the cross-device restore has NOT run, so the save
        //    gate is down. The real seam reports NOT durable and nothing lands —
        //    a mint here refuses to publish (the sharpened strand, trigger 3).
        assert!(
            !block_on(seam.persist_provider()).expect("no transport error pre-load"),
            "before the launch restore lifts the gate, the provider is NOT durable",
        );
        assert!(
            block_on(replica.get(PATH_PROVIDER.to_string()))
                .unwrap()
                .is_none(),
            "no provider blob may land before the launch gate lifts",
        );

        // ── Post-restore: the gate is up, and a fresh mint changes the provider.
        //    The seam durably saves it and reports `true` → the mint may publish.
        block_on(sync_a.load()).expect("load lifts the gate");
        let _fresh = alice.generate_key_packages_bytes(1).unwrap(); // fresh init keys → provider changed
        assert!(
            block_on(seam.persist_provider()).expect("no transport error post-load"),
            "after the gate lifts, a mint's fresh init keys are durably saved",
        );
        assert!(
            block_on(replica.get(PATH_PROVIDER.to_string()))
                .unwrap()
                .is_some(),
            "the provider blob (with the fresh init keys) is now durable — a later \
             swap-restore preserves it, so a peer's Welcome can open",
        );
    }

    /// The populated-`history/<ch>` path the provider-only test above punts to the
    /// linux + web tier_3 GUI proofs — closed here at tier_1, so a refactor of the
    /// `snapshot_replica` / `restore_and_wire` manager+backend integration can't
    /// regress silently on every leg (all six inherit this shared body). On device A
    /// a bound channel carries an own-message; `snapshot_replica` must walk
    /// `bound_channels → thread_for_channel → snapshot_channel_slice` to capture it,
    /// and device B's `restore_and_wire` must `restore_channel_slice → bind_channel`
    /// so the channel re-enumerates AND the own sent plaintext (the nest log can't
    /// give it) survives — the actual cross-device value proposition.
    #[test]
    fn snapshot_restore_roundtrips_bound_channel_history() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // Device A: alice runs a real two-member group, binds the channel to a
        // thread carrying her own sent message, then snapshots + saves.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, _welcome) = alice.create_group(&bob_kps).unwrap();
        let channel_hex = channel.to_string();

        let (backend_a, manager_a, sync_a) = device(&alice, &conv_nest, &replica, 1);
        block_on(sync_a.load()).unwrap(); // lift the save gate

        // Populate + bind the channel exactly as production's own restore leg does
        // (manager.restore_channel_slice → backend.bind_channel).
        let slice_a = own_message_slice(&channel_hex, 7);
        let tid_a = manager_a.restore_channel_slice(&slice_a);
        backend_a.bind_channel(tid_a, channel);
        // The snapshot's watermark is device A's live ingest cursor (not the setup
        // slice's), so advance it as the poll loop would after folding up to seq 7.
        sync_a.advance_processed_seq(&channel, 7);

        let snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);
        assert_eq!(
            snapshot.slices.len(),
            1,
            "the one bound channel is walked into a slice"
        );
        assert_eq!(
            snapshot.slices[0].messages.len(),
            1,
            "the own-message plaintext is captured in the slice"
        );
        block_on(save_snapshot(&sync_a, &snapshot)).expect("save ok");

        // Device B: a fresh same-identity device restores from the same replica.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);

        let restored = block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("restore ok");

        assert_eq!(restored, 1, "device B restored the one history slice");
        assert!(
            backend_b.bound_channels().contains(&channel),
            "restore_and_wire re-bound channel → thread so the poll routes it"
        );
        assert_eq!(
            sync_b.processed_seq(&channel),
            7,
            "the ingest cursor seeded from the slice watermark"
        );
        let tid_b = backend_b
            .thread_for_channel(&channel)
            .expect("channel bound to a thread on device B");
        let restored_slice = manager_b
            .snapshot_channel_slice(&tid_b, &channel_hex, 0)
            .expect("restored thread present on device B");
        assert_eq!(
            restored_slice.messages, slice_a.messages,
            "device B recovered alice's own sent plaintext verbatim"
        );
        assert_eq!(
            alice2.current_epoch(&channel).unwrap(),
            alice.current_epoch(&channel).unwrap(),
            "the provider crypto state restored alongside the history",
        );
    }

    /// A two-member group where bob has joined, bound to a thread on alice's device
    /// A, with the replica's launch gate lifted. Returns everything the Rule-2 tests
    /// drive.
    #[allow(clippy::type_complexity)]
    fn bound_pair(
        replica: &MemReplica,
        conv_nest: &Arc<FakeConvNest>,
    ) -> (
        Arc<MlsEngine>,
        Arc<MlsEngine>,
        fauna_mls::types::ChannelId,
        Arc<FaunaMlsBackend>,
        Arc<ConversationsManager>,
        Arc<MlsStateSync>,
    ) {
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        let (backend_a, manager_a, sync_a) = device(&alice, conv_nest, replica, 1);
        block_on(sync_a.load()).unwrap(); // lift the save gate

        let slice_a = own_message_slice(&channel.to_string(), 0);
        let tid_a = manager_a.restore_channel_slice(&slice_a);
        backend_a.bind_channel(tid_a, channel);

        (alice, bob, channel, backend_a, manager_a, sync_a)
    }

    /// Inject one of bob's application messages onto the channel log.
    fn bob_says(
        conv_nest: &Arc<FakeConvNest>,
        bob: &MlsEngine,
        channel: &fauna_mls::types::ChannelId,
        text: &str,
        sequence: u64,
    ) -> i64 {
        let ct = bob
            .encrypt(
                channel,
                &ChannelMessage {
                    sender: bob.identity_actor_id(),
                    sequence,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text(text.into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        conv_nest.inject(
            &channel.to_string(),
            ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        )
    }

    /// Drive the production inbound poll from the sync cursor and write it back, as
    /// the per-leg poll loop does through the `MlsSyncCursor` seam.
    fn poll_from_cursor(
        backend: &Arc<FaunaMlsBackend>,
        manager: &Arc<ConversationsManager>,
        sync: &MlsStateSync,
        channel: &fauna_mls::types::ChannelId,
    ) -> i64 {
        let mut after = sync.processed_seq(channel);
        block_on(poll_inbound_conv(backend, manager, channel, &mut after, 0)).unwrap();
        sync.advance_processed_seq(channel, after);
        after
    }

    /// **Rule 2, tear 1 — the durable cursor may never outrun the durable provider.**
    ///
    /// A tick whose `history/<ch>` PUT lands while the `provider` PUT faults used to
    /// leave `{provider @ epoch N, watermark past the N→N+1 commit}`, because the
    /// cursor was seeded from the *history* blob (`sync.rs` `cursor.insert(channel,
    /// slice.watermark)`) — a different blob under a different CAS. The next launch
    /// then resumed the poll **after** a commit it had never applied, every later
    /// foreign commit quiet-skipped as `PastEpochCommit`, and the device was stranded
    /// at epoch N with no self-heal.
    ///
    /// Now the cursor rides inside the `provider` blob, so the pair is one CAS: the
    /// provider that did not land carries the cursor that did not advance. Device B
    /// restores the older pair, re-walks the gap idempotently, and reaches the
    /// current epoch.
    #[test]
    fn provider_save_failure_never_strands_the_next_launch() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let (alice, bob, channel, backend_a, manager_a, sync_a) = bound_pair(&replica, &conv_nest);

        // Tick 1: bob speaks, A folds it, and the whole pair saves cleanly.
        let said = bob_says(&conv_nest, &bob, &channel, "before the commit", 1);
        assert_eq!(
            poll_from_cursor(&backend_a, &manager_a, &sync_a, &channel),
            said
        );
        let snap1 = snapshot_replica(&backend_a, &manager_a, &sync_a);
        block_on(save_snapshot(&sync_a, &snap1)).expect("tick 1 saves cleanly");
        let epoch_before = alice.current_epoch(&channel).unwrap();

        // Bob commits; A ingests it, advancing BOTH her epoch and her cursor.
        let commit = bob.self_update(&channel).unwrap();
        let commit_seq = conv_nest.inject(
            &channel.to_string(),
            ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        );
        assert_eq!(
            poll_from_cursor(&backend_a, &manager_a, &sync_a, &channel),
            commit_seq
        );
        let epoch_after = alice.current_epoch(&channel).unwrap();
        assert!(
            epoch_after > epoch_before,
            "bob's commit advanced device A past epoch {epoch_before}"
        );

        // Tick 2: the history PUT lands; the provider PUT faults.
        replica.fail_puts_under(PATH_PROVIDER);
        let snap2 = snapshot_replica(&backend_a, &manager_a, &sync_a);
        let err = block_on(save_snapshot(&sync_a, &snap2)).expect_err("the provider PUT faulted");
        assert!(
            matches!(err, SaveReplicaError::Provider(_)),
            "history landed first, then the provider faulted; got {err}"
        );
        replica.clear_faults();

        // The tear precondition really is on the nest: history is ahead of provider.
        assert!(
            replica.raw(&history_path(&channel.to_string())).is_some(),
            "the history slice landed (own-message plaintext is user-irrecoverable)"
        );

        // Device B: a fresh same-identity device restores the torn-but-safe pair.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("restore ok");

        assert_eq!(
            sync_b.processed_seq(&channel),
            said,
            "the cursor came from the last-landed PROVIDER, not the further-ahead history watermark"
        );
        assert_eq!(
            alice2.current_epoch(&channel).unwrap(),
            epoch_before,
            "device B restored the older provider, consistent with that cursor",
        );

        // The heal: re-walk the gap. Bob's commit is re-applied; nothing is skipped.
        poll_from_cursor(&backend_b, &manager_b, &sync_b, &channel);
        assert_eq!(
            alice2.current_epoch(&channel).unwrap(),
            epoch_after,
            "device B re-walked from the restored cursor and reached the current epoch",
        );
    }

    /// **Rule 2, tear 2 — the provider must not land when history did not.**
    ///
    /// The symmetric partial, and the worse one: `save_snapshot` used to PUT the
    /// provider *first*, so a provider-lands / history-faults tick durably recorded
    /// "consumed up to seq C" while the plaintext for `(..C]` never landed. That data
    /// is user-irrecoverable — a sender cannot MLS-decrypt its own application
    /// messages, and a foreign record in that range cannot be re-decrypted either
    /// (the restored provider already consumed those ratchet generations). Ordering
    /// history first and skipping the provider on any history failure makes the
    /// durable pair always `{history ⊒ provider's cursor}`.
    #[test]
    fn history_save_failure_skips_the_provider_save_entirely() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let (_alice, bob, channel, backend_a, manager_a, sync_a) = bound_pair(&replica, &conv_nest);

        // Tick 1: clean save, so a durable provider exists to compare against.
        let said = bob_says(&conv_nest, &bob, &channel, "landed", 1);
        poll_from_cursor(&backend_a, &manager_a, &sync_a, &channel);
        let snap1 = snapshot_replica(&backend_a, &manager_a, &sync_a);
        block_on(save_snapshot(&sync_a, &snap1)).expect("tick 1 saves cleanly");
        let provider_after_tick1 = replica.digest(PATH_PROVIDER).expect("provider landed");

        // Bob speaks again; A folds it, so the cursor and the crypto state both move.
        let said2 = bob_says(&conv_nest, &bob, &channel, "not persisted", 2);
        assert_eq!(
            poll_from_cursor(&backend_a, &manager_a, &sync_a, &channel),
            said2
        );
        assert!(said2 > said);

        // Tick 2: the history PUT faults.
        replica.fail_puts_under("history/");
        let snap2 = snapshot_replica(&backend_a, &manager_a, &sync_a);
        let err = block_on(save_snapshot(&sync_a, &snap2)).expect_err("the history PUT faulted");
        assert!(matches!(err, SaveReplicaError::History { .. }), "got {err}");
        replica.clear_faults();

        assert_eq!(
            replica.digest(PATH_PROVIDER),
            Some(provider_after_tick1),
            "the provider save was SKIPPED: its cursor must never claim a record whose \
             history did not land",
        );

        // Device B therefore restores a cursor at `said`, and the re-walk re-folds
        // `said2` into history rather than skipping past it.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("restore ok");
        assert_eq!(
            sync_b.processed_seq(&channel),
            said,
            "the restored cursor sits at the last record whose history landed",
        );
    }

    /// **Rule 2, tear 1 — a MODERN replica must not seal the cursor ahead of the
    /// crypto values it indexes**. The debounced
    /// autosave snapshots the provider at T0, then uploads (seconds of I/O) and
    /// folds the cursor at save-time T1 — and the poll advances `st.cursor` in RAM
    /// in between. `save_provider_if_changed` used to fold the LIVE (T1) cursor
    /// into the SNAPSHOT-time (T0) crypto values, sealing `{values @ T0, cursor @
    /// T1}`: a foreign record folded in the window makes the durable cursor outrun
    /// both the provider values and the just-saved history. Blob-level atomicity
    /// (one CAS) is not content-level atomicity. The fix captures the cursor at
    /// snapshot time and seals THAT, so `{values, history, cursor}` are all
    /// T0-consistent.
    #[test]
    fn autosave_seals_the_snapshot_time_cursor_not_the_live_one() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let (_alice, bob, channel, backend_a, manager_a, sync_a) = bound_pair(&replica, &conv_nest);

        // Fold one record, so the channel has a real cursor + history at T0.
        let said = bob_says(&conv_nest, &bob, &channel, "at T0", 1);
        assert_eq!(
            poll_from_cursor(&backend_a, &manager_a, &sync_a, &channel),
            said
        );

        // T0: snapshot the provider + history at cursor `said`.
        let snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);

        // The poll folds a foreign record into RAM DURING the save window: only
        // `st.cursor` advances here (the engine + history stay at the snapshot, as
        // they do until the NEXT tick re-snapshots them). This is the T0/T1 skew
        // the fix's own tests never exercised (they set the cursor before the
        // snapshot).
        sync_a.advance_processed_seq(&channel, said + 1);

        // T1: the autosave uploads the T0 snapshot. It must seal the T0 cursor.
        block_on(save_snapshot(&sync_a, &snapshot)).expect("save ok");

        // A fresh same-identity device restores. The sealed cursor decides where
        // it resumes: it must be `said` (T0), never `said + 1` (the live cursor),
        // which would resume PAST a record whose history landed only at `said`.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("restore ok");
        assert_eq!(
            sync_b.processed_seq(&channel),
            said,
            "the sealed cursor is the SNAPSHOT-time watermark, never the live cursor \
             that advanced in the save window — else the restore resumes past a record \
             whose history did not land at that cursor (tear 1)",
        );
    }

    /// **The no-progress guard must classify the walk INCOMPLETE, not clean**. When a
    /// nest violates the "every served seq exceeds `after`" contract (a truncated
    /// or stuck page), the guard stops the walk — but it used to `break` and fall
    /// through to the arm-2 resumed-pending clear AND return `stalled: false`,
    /// laundering positive evidence of nest misbehavior into a clean gated-send
    /// baseline. It must instead return `stalled: true` (like the unhealable-stall
    /// arm): the early return skips arm-2, and `catch_up_after` aborts a gated
    /// send rather than take a baseline from a walk it knows is incomplete.
    #[test]
    fn no_progress_guard_reports_stalled_not_a_clean_completion() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let (_alice, _bob, channel, backend_a, manager_a, _sync_a) =
            bound_pair(&replica, &conv_nest);

        // The nest MISBEHAVES on the next fetch: a non-empty page that does not
        // advance past `after`. A from-0 walk hits it immediately, so `started_at
        // == 0` makes the arm-2 clear eligible — exactly when the guard must NOT
        // let the walk read as complete.
        conv_nest.serve_a_non_advancing_page_once();
        let mut after = 0;
        let outcome = block_on(poll_inbound_conv(
            &backend_a, &manager_a, &channel, &mut after, 0,
        ))
        .unwrap();

        assert!(
            outcome.stalled,
            "a detected server-contract violation is an INCOMPLETE walk — it must \
             report stalled (the early return skips the arm-2 clear and blocks a \
             gated-send baseline), never a clean completion",
        );
        assert_eq!(
            after, 0,
            "the guard stopped the walk without advancing the cursor"
        );
    }

    /// **Rule 2 heal — a torn replica converges after one poll
    /// cycle** (`devices.md` § Cross-device MLS group-state sync, Implementation
    /// status). A `provider` blob carrying no cursor for the channel makes
    /// `load()` fall back to the `history/<ch>` watermark — which, on a
    /// replica whose provider was saved before its cursor landed, sits PAST the
    /// bridging commit. The rewind also covers any other un-processable hole.
    /// The heal:
    /// the poll meets the next commit as `FutureEpochCommit`, rewinds its
    /// in-flight cursor to 0 (once per channel per session), re-walks the log,
    /// and lands on the head epoch — after which the next provider save writes a
    /// consistent `{provider, cursor}` pair, retiring the torn state for good.
    #[test]
    fn torn_replica_heals_to_head_epoch_after_one_poll() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let (alice, bob, channel, _backend_a, _manager_a, sync_a) =
            bound_pair(&replica, &conv_nest);
        let channel_hex = channel.to_string();

        // The channel log: seq1 = bob's bridging commit, seq2 = a message sealed
        // past it, seq3 = the commit the torn device meets as FutureEpochCommit.
        let cb1 = bob.self_update(&channel).unwrap();
        bob.merge_pending_commit(&channel).unwrap();
        let seq1 = conv_nest.inject(
            &channel_hex,
            ChannelEnvelope::Commit(cb1).to_bytes().unwrap(),
        );
        let said = bob_says(&conv_nest, &bob, &channel, "bridge me", 1);
        let cb2 = bob.self_update(&channel).unwrap();
        bob.merge_pending_commit(&channel).unwrap();
        let seq3 = conv_nest.inject(
            &channel_hex,
            ChannelEnvelope::Commit(cb2).to_bytes().unwrap(),
        );
        assert!(seq1 < said && said < seq3);

        // Seed the TORN pair on the nest: the provider is alice's engine
        // at the pre-bridge epoch, saved while the live cursor map has no entry
        // for the channel — so the sealed blob carries no cursor (a blob with
        // no cursor) — while the history watermark sits PAST the bridging commit
        // (a partial-save tear).
        block_on(sync_a.save_provider_if_changed(&ProviderReplica::from_engine(&alice)))
            .expect("cursor-less provider saved");
        block_on(sync_a.save_history_if_changed(&own_message_slice(&channel_hex, seq1)))
            .expect("torn history watermark saved");

        // Device B restores: the watermark fallback seeds the cursor from the
        // watermark — past the bridging commit. This is the stranded state.
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        block_on(restore_and_wire(
            Arc::clone(&sync_b),
            &backend_b,
            &manager_b,
        ))
        .expect("restore ok");
        assert_eq!(
            sync_b.processed_seq(&channel),
            seq1,
            "precondition: the watermark fallback seeded the cursor past the bridging commit"
        );
        let stale_epoch = alice2.current_epoch(&channel).unwrap();
        assert!(
            stale_epoch < bob.current_epoch(&channel).unwrap(),
            "precondition: the restored provider is behind the log"
        );

        // ONE poll cycle: seq3 arrives as FutureEpochCommit → rewind → re-walk
        // → the bridging commit applies → head epoch.
        poll_from_cursor(&backend_b, &manager_b, &sync_b, &channel);
        assert_eq!(
            alice2.current_epoch(&channel).unwrap(),
            bob.current_epoch(&channel).unwrap(),
            "the heal converged the torn replica to the head epoch"
        );
        assert_eq!(
            sync_b.processed_seq(&channel),
            seq3,
            "the durable cursor advanced monotonically to the head (never durably rewound)"
        );
    }

    // ── launch resilience: `restore_and_wire_with_retry` ────────────────────

    /// A recording no-op sleep for [`retry_restore`] — captures the backoff
    /// schedule instead of parking (the crate's busy-poll `block_on` cannot
    /// drive a real timer).
    fn recording_sleep(log: Arc<Mutex<Vec<u64>>>) -> impl FnMut(u64) -> std::future::Ready<()> {
        move |ms| {
            log.lock().unwrap().push(ms);
            std::future::ready(())
        }
    }

    /// **Launch resilience, the success bar** (`devices.md` § Cross-device MLS
    /// group-state sync, launch resilience): the first two launch `load()`
    /// fetches fail transiently (nest unreachable), the transport recovers, and
    /// the session converges — via backoff retries *through* `load()` — to a
    /// state indistinguishable from a clean launch: channels restored, own
    /// plaintext recovered, cursor seeded, gate injected, crypto state current.
    #[test]
    fn transient_launch_failure_retries_to_clean_launch_state() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // Device A: a bound channel with an own message, saved to the replica.
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, _welcome) = alice.create_group(&bob_kps).unwrap();
        let channel_hex = channel.to_string();
        let (backend_a, manager_a, sync_a) = device(&alice, &conv_nest, &replica, 1);
        block_on(sync_a.load()).unwrap(); // lift the save gate
        let slice_a = own_message_slice(&channel_hex, 7);
        let tid_a = manager_a.restore_channel_slice(&slice_a);
        backend_a.bind_channel(tid_a, channel);
        sync_a.advance_processed_seq(&channel, 7);
        let snapshot = snapshot_replica(&backend_a, &manager_a, &sync_a);
        block_on(save_snapshot(&sync_a, &snapshot)).expect("save ok");

        // Device B launches while the nest is unreachable for two fetches.
        replica.fail_next_gets(vec![
            MlsTransportError::fault("connection refused"),
            MlsTransportError::fault("connection refused"),
        ]);
        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b) = device(&alice2, &conv_nest, &replica, 1);
        let sleeps = Arc::new(Mutex::new(Vec::new()));

        let end = block_on(retry_restore(
            Arc::clone(&sync_b),
            Arc::downgrade(&backend_b),
            Arc::downgrade(&manager_b),
            recording_sleep(Arc::clone(&sleeps)),
        ));

        assert!(
            matches!(end, RestoreRetryEnd::Wired(1)),
            "third attempt restored the one channel; got {end:?}"
        );
        assert_eq!(
            *sleeps.lock().unwrap(),
            vec![1_000, 2_000],
            "exponential backoff between the two transient failures"
        );
        // Indistinguishable from a clean launch:
        assert!(
            backend_b.bound_channels().contains(&channel),
            "channel re-bound so the poll routes it"
        );
        assert_eq!(
            sync_b.processed_seq(&channel),
            7,
            "cursor seeded from the restored replica"
        );
        assert!(
            backend_b.commit_gate().is_some(),
            "device-owned-epoch gate injected"
        );
        assert_eq!(
            alice2.current_epoch(&channel).unwrap(),
            alice.current_epoch(&channel).unwrap(),
            "group crypto state restored"
        );
        let tid_b = backend_b.thread_for_channel(&channel).expect("bound");
        let restored_slice = manager_b
            .snapshot_channel_slice(&tid_b, &channel_hex, 0)
            .expect("restored thread present");
        assert_eq!(
            restored_slice.messages, slice_a.messages,
            "own sent plaintext recovered verbatim"
        );
    }

    /// **The nest-rejection pin**: a
    /// nest-answered rejection — any refusal of the kind — must yield today's single-device fallback after ONE
    /// attempt, never a forever-blocked receive loop. The launch save-gate
    /// stays down, so a later save cannot clobber a replica that was never
    /// loaded.
    #[test]
    fn nest_rejection_fails_fast_and_keeps_the_save_gate_down() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        replica.fail_next_gets(vec![MlsTransportError::rejection(
            "unknown kind fauna.mls.get",
        )]);

        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend, manager, sync) = device(&alice, &conv_nest, &replica, 1);
        backend.expect_replica_restore();
        let sleeps = Arc::new(Mutex::new(Vec::new()));

        let end = block_on(retry_restore(
            Arc::clone(&sync),
            Arc::downgrade(&backend),
            Arc::downgrade(&manager),
            recording_sleep(Arc::clone(&sleeps)),
        ));

        assert!(
            matches!(end, RestoreRetryEnd::Failed(ref e) if !e.is_transient()),
            "a rejection is permanent; got {end:?}"
        );
        assert!(
            !backend.replica_restore_pending(),
            "single-device fallback: no restore is coming, so key-package mints publish directly"
        );
        assert!(
            sleeps.lock().unwrap().is_empty(),
            "no retry was attempted for a nest-answered rejection"
        );
        assert!(backend.commit_gate().is_none(), "gate stays un-injected");
        assert!(
            !block_on(sync.save_provider_if_changed(&ProviderReplica::from_engine(&alice)))
                .expect("gated save is Ok(false), not an error"),
            "the launch save-gate is still down after the failed launch"
        );
    }

    /// A session torn down mid-retry (logout, or the linux e2e session
    /// re-injection) ends the loop via the failed `Weak` upgrade instead of a
    /// zombie task retrying forever against an unreachable nest.
    #[test]
    fn session_drop_mid_retry_ends_the_loop() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        // More queued faults than the loop will consume — the drop ends it first.
        replica.fail_next_gets(
            (0..10)
                .map(|_| MlsTransportError::fault("connection refused"))
                .collect(),
        );

        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend, manager, sync) = device(&alice, &conv_nest, &replica, 1);

        // The sleep stands in for "the session is dropped while the retry
        // waits": it releases the only strong backend/manager refs.
        let holders = Arc::new(Mutex::new(Some((backend, manager))));
        let sleep_holders = Arc::clone(&holders);
        let end = block_on(retry_restore(
            Arc::clone(&sync),
            {
                let g = holders.lock().unwrap();
                Arc::downgrade(&g.as_ref().unwrap().0)
            },
            {
                let g = holders.lock().unwrap();
                Arc::downgrade(&g.as_ref().unwrap().1)
            },
            move |_ms| {
                sleep_holders.lock().unwrap().take();
                std::future::ready(())
            },
        ));

        assert!(
            matches!(end, RestoreRetryEnd::SessionDropped),
            "the failed Weak upgrade ended the retry; got {end:?}"
        );
    }
    /// **A successor's engine is seated only by its own joins** —
    /// `succession-aftermath.md` § Re-key scope → *What a successor's replica
    /// restore may take from a predecessor's*, the succession-time instance of
    /// the whole-KV-swap invariant `restore_and_wire_with_retry` states.
    ///
    /// The ceremony's shape, in-process: the old leaf adds the successor to a
    /// group it shares with bob, the successor joins from the Welcome (its
    /// engine now holds the group under its OWN leaf — the ceremony device's
    /// SQLite after `sweep_as_successor`). The successor's `provider` path then
    /// holds the OLD engine's snapshot, sealed under the successor's key —
    /// exactly what the `__mls` re-seal leaves there. The launch restore must
    /// leave the successor seated as itself; before the guard it re-seated it
    /// as the old leaf (`CannotRemoveSelf` on the retry, measured 2026-08-27).
    /// Then the ceremony device's own move — publishing the successor's snapshot
    /// before the switch — is what a *fresh* engine of the successor (web's
    /// post-reload page, a second device) restores from, seated as itself.
    #[test]
    fn a_successors_launch_never_restores_a_predecessors_provider_over_its_own_join() {
        use fauna_mls::channel::GroupChannel;
        use fauna_mls::state_replica::ProviderReplica;
        use fauna_mls::succession::commit_add_successor;

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let old = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let successor = Arc::new(MlsEngine::new_in_memory(keypair(3)).unwrap());
        let old_id = old.identity_actor_id();
        let successor_id = successor.identity_actor_id();

        // The group the old leaf shares with bob, then add-successor + join.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (old_group, welcome) = GroupChannel::create(&old, &bob_kps).unwrap();
        GroupChannel::join(&bob, welcome).unwrap();
        let channel = old_group.channel_id;
        let successor_kp = successor.generate_key_packages(1).unwrap();
        let add = commit_add_successor(&old, &channel, &successor_kp[0]).unwrap();
        bob.process_commit(&channel, &add.commit_bytes).unwrap();
        GroupChannel::join(&successor, add.welcome).unwrap();
        assert_eq!(successor.own_leaf_identity(&channel), Some(successor_id));

        // The predecessor's snapshot, at the successor's path, under the
        // successor's key — the re-sealed occupant a first launch meets.
        let (_, _, seed_sync) = device(&successor, &conv_nest, &replica, 3);
        block_on(seed_sync.publish_provider(&ProviderReplica::from_engine(&old)))
            .expect("the predecessor's snapshot lands at the successor's path");

        // The successor's launch: restore must NOT re-seat it as the old leaf.
        let (_, _, sync, _) = launched_device(&successor, &conv_nest, &replica, 3);
        assert!(
            successor.has_group(&channel),
            "the successor's own join survives launch"
        );
        assert_eq!(
            successor.own_leaf_identity(&channel),
            Some(successor_id),
            "a predecessor's provider must never restore over the successor's own join — \
             it would seat the successor as the leaf remove-old exists to evict"
        );
        assert!(
            !successor
                .find_leaves_by_identity(&channel, &old_id)
                .is_empty(),
            "the old leaf is still a member: remove-old is owed and now authorable"
        );

        // The ceremony device's publish, and what a FRESH successor engine
        // (web after the reload; a second device) restores from it.
        block_on(sync.publish_provider(&ProviderReplica::from_engine(&successor)))
            .expect("the successor's own snapshot replaces the predecessor's");
        let fresh = Arc::new(MlsEngine::new_in_memory(keypair(3)).unwrap());
        assert!(!fresh.has_group(&channel));
        let (_, _, _, _) = launched_device(&fresh, &conv_nest, &replica, 3);
        assert_eq!(
            fresh.own_leaf_identity(&channel),
            Some(successor_id),
            "the successor's own snapshot restores into a fresh engine of the successor, \
             seated as itself — the ordinary single-leaf state-sync"
        );
    }

    /// **The SECOND door: a mid-session `resync_provider` refuses a
    /// predecessor's snapshot exactly as the launch does** —
    /// `succession-aftermath.md` § Re-key scope → *What a successor's replica
    /// restore may take from a predecessor's*, rule (1).
    ///
    /// Rule (1) is an absolute over the snapshot's own bytes, so it binds every
    /// door that swaps the provider KV. `restore_into` had exactly two
    /// production callers and only the launch one asked; the sibling above
    /// pinned that door while this one — driven by `FaunaCommitGate::
    /// resync_channel` whenever the inbound loop meets an own-leaf commit this
    /// device did not author — swapped the same bytes in unasked. The window is not a race: on the foreign arm the launch
    /// deliberately leaves the predecessor's snapshot at the path until the
    /// successor's first save replaces it by CAS, so an own-leaf commit
    /// anywhere in that span re-seated the successor as the predecessor.
    ///
    /// Both halves are asserted, because a refusal that never lifts would be a
    /// different bug: the refusal leaves the engine seated as itself, and once
    /// the successor's own snapshot is at the path the very same call converges.
    #[test]
    fn a_mid_session_resync_never_restores_a_predecessors_provider_either() {
        use fauna_mls::channel::GroupChannel;
        use fauna_mls::state_replica::ProviderReplica;
        use fauna_mls::succession::commit_add_successor;

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let old = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let successor = Arc::new(MlsEngine::new_in_memory(keypair(3)).unwrap());
        let old_id = old.identity_actor_id();
        let successor_id = successor.identity_actor_id();

        // The ceremony's shape, in-process: the old leaf adds the successor to
        // the group it shares with bob; the successor joins from the Welcome, so
        // its engine holds the group under its OWN leaf.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (old_group, welcome) = GroupChannel::create(&old, &bob_kps).unwrap();
        GroupChannel::join(&bob, welcome).unwrap();
        let channel = old_group.channel_id;
        let successor_kp = successor.generate_key_packages(1).unwrap();
        let add = commit_add_successor(&old, &channel, &successor_kp[0]).unwrap();
        bob.process_commit(&channel, &add.commit_bytes).unwrap();
        GroupChannel::join(&successor, add.welcome).unwrap();
        assert_eq!(successor.own_leaf_identity(&channel), Some(successor_id));

        // The predecessor's snapshot, at the successor's path, under the
        // successor's key — what the `__mls` re-seal leaves there.
        let (_, _, seed_sync) = device(&successor, &conv_nest, &replica, 3);
        block_on(seed_sync.publish_provider(&ProviderReplica::from_engine(&old)))
            .expect("the predecessor's snapshot lands at the successor's path");

        // The launch refuses it (the sibling pin's subject) and — the foreign
        // arm's design — leaves it at the path for the first save to replace.
        let (_, _, sync, _) = launched_device(&successor, &conv_nest, &replica, 3);
        assert_eq!(successor.own_leaf_identity(&channel), Some(successor_id));

        // THE DOOR UNDER TEST. An own-leaf commit this device did not author
        // drives `resync_channel` → `resync_provider` inside that window.
        let resynced =
            block_on(sync.resync_provider(&successor)).expect("a refusal is not a transport error");
        // The HARM first, so a regression fails on the security property itself
        // rather than on the shape of the return value: with the door unchecked
        // this is where the successor is found seated as its predecessor.
        assert_eq!(
            successor.own_leaf_identity(&channel),
            Some(successor_id),
            "a predecessor's provider must never restore through the MID-SESSION door \
             either — it would seat the successor as the leaf remove-old exists to evict, \
             and its own remove-old would answer CannotRemoveSelf"
        );
        assert!(
            resynced.is_none(),
            "the mid-session resync must refuse a snapshot seated under another \
             identity's leaf — `None` is the refusal its caller maps to a stalled \
             channel (cursor stops before the commit; the next pass retries)"
        );
        assert!(
            !successor
                .find_leaves_by_identity(&channel, &old_id)
                .is_empty(),
            "the old leaf is still a member: remove-old is owed and still authorable"
        );

        // The refusal lifts on its own terms: once the successor's first save
        // has taken the path back by CAS, the very same call converges.
        block_on(sync.publish_provider(&ProviderReplica::from_engine(&successor)))
            .expect("the successor's own snapshot replaces the predecessor's");
        assert!(
            block_on(sync.resync_provider(&successor))
                .expect("resync ok")
                .is_some(),
            "a clean snapshot at the path is restored by the same door that refused the \
             predecessor's — the refusal is per-snapshot, not a permanent shutdown"
        );
        assert_eq!(
            successor.own_leaf_identity(&channel),
            Some(successor_id),
            "and it is still seated as itself afterwards"
        );
    }

    /// **A restore that carried NOTHING must not report an established
    /// absence** — `federation.md` § Peer-auth model → *Discovery-failure
    /// semantics*, case 2.
    ///
    /// The email carve-out rests on the user holding no Fauna expectation, and
    /// only an absence this client actually *established* can show that. Before
    /// this, `restore_and_wire` marked the account loaded **unconditionally**:
    /// `load()` answers `Ok` with `provider: None` for an account that has no
    /// replica blob yet, the history loop lives inside that `Some`, so the mark
    /// fired over an empty `ThreadStore` and every foreign domain reported
    /// `AbsentFromLoadedEvidence`. A peer this account had threads with for
    /// months then resolved with no evidence, and a peer nest that does not
    /// answer the anonymous probe fell through to plaintext SMTP — the exact
    /// window the three-state split was landed to close, disguised as closed.
    ///
    /// The replica *is* the account's channel list — the nest exposes no "list
    /// my channels" call and `ThreadStore` is in-memory, rebuilt every launch —
    /// so no replica means the conversations were never loaded.
    ///
    /// Both halves are asserted, because refusing everywhere would be a
    /// different bug: an account with **no groups at all** has no replica
    /// either, and reading that as "not loaded" would deny the email carve-out
    /// forever to exactly the user it exists for.
    #[test]
    fn a_restore_that_carried_no_replica_does_not_establish_absence() {
        use fauna_conversations::backend::{
            ConvRpcError, DomainEvidence, classify_foreign_non_answer,
        };

        let non_answer = ConvRpcError::Transient {
            message: "no route to host".into(),
        };

        // ── Shape 1: real conversations, no replica to restore them from (the
        // first launch of the multi-device plane for an existing account).
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (_channel, _welcome) = alice.create_group(&bob_kps).unwrap();

        let (backend, _manager, _sync, _restored) =
            launched_device(&alice, &conv_nest, &replica, 1);

        assert_eq!(
            backend.domain_evidence("peer.test"),
            DomainEvidence::Unloaded,
            "the account holds groups and this launch restored no replica, so its \
             conversations are NOT loaded — reporting an established absence here is \
             what downgraded a known peer to plaintext SMTP"
        );
        assert!(
            classify_foreign_non_answer(&non_answer, backend.domain_evidence("peer.test")).terminal,
            "a foreign non-answer must stay a failed lookup — never an email fallthrough \
             — while the account's evidence is unread"
        );

        // ── The other half: an account with nothing to load. No replica and no
        // groups is an ESTABLISHED emptiness, and the carve-out must open.
        let replica2 = MemReplica::default();
        let conv_nest2 = Arc::new(FakeConvNest::default());
        let fresh = Arc::new(MlsEngine::new_in_memory(keypair(7)).unwrap());
        let (backend2, _m2, _s2, _r2) = launched_device(&fresh, &conv_nest2, &replica2, 7);

        assert_eq!(
            backend2.domain_evidence("peer.test"),
            DomainEvidence::AbsentFromLoadedEvidence,
            "an account with no groups at all has nothing to load, so its emptiness IS \
             established — denying the email carve-out here would break the very user \
             it was written for"
        );
        assert!(
            !classify_foreign_non_answer(&non_answer, backend2.domain_evidence("peer.test"))
                .terminal,
            "with the absence established, first contact falls through to email by ruling"
        );
    }

    /// **A restore that carried the channel LIST but none of its participants
    /// must not report an established absence either** — `federation.md`
    /// § Peer-auth model → *Discovery-failure semantics*, case 2.
    ///
    /// The sibling above closed the `provider: None` route. This is the
    /// `provider: Some` one, and it reaches the identical state. The mark used
    /// to be spent on `loaded.provider.is_some()` — the account's channel
    /// **list** — while the evidence the rail actually spends is
    /// `known_domains`, harvested from thread-store **participants**, which live
    /// only in the `history/<ch>` slices: a separate blob, a separate fetch, and
    /// one `load()` tolerates missing on purpose.
    ///
    /// No fault is needed to reach it. The commit gate's
    /// `save_provider_snapshot` CAS-puts the provider alone by design, so after
    /// a first send on a new channel the durable state is `{provider lists X, no
    /// history/X}` until the debounce writes the slice — and a second device
    /// launching inside that window reads exactly that. An ordinary
    /// multi-device race, no crash.
    ///
    /// The `groups >= 1` assertion is what makes this test non-vacuous: it
    /// proves the launch restored a real MLS channel **with a peer in it** from
    /// the very blob it read, and would still have called the account's
    /// conversations loaded — an absence "established" over a store that had
    /// never seen a single participant.
    #[test]
    fn a_restore_that_carried_the_channel_list_but_no_slices_does_not_establish_absence() {
        use fauna_conversations::backend::{
            ConvRpcError, DomainEvidence, classify_foreign_non_answer,
        };
        use fauna_mls::state_replica::ProviderReplica;

        let non_answer = ConvRpcError::Transient {
            message: "no route to host".into(),
        };

        // A first device with a real channel: alice holds a group with bob in
        // it, so her provider replica lists that channel. It is a CHAT channel
        // the way production makes one: `bootstrap_group` → `bind_channel`
        // stamps the durable chat marker before the takeover's provider put,
        // so the marker rides the very blob this fixture publishes. Without
        // it the listed channel would read as thread-less (a folder or
        // scheduling group, which never gets a slice by design) and the
        // predicate under test would — correctly — not hold the account.
        let replica = MemReplica::default();
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, _welcome) = alice.create_group(&bob_kps).unwrap();
        alice.mark_channel_chat(&channel);

        // Publish the provider ALONE — no `history/<ch>` slice for it. This is
        // the commit gate's own gesture (`save_provider_snapshot`), not a
        // contrived one, and it is the durable state every first send passes
        // through on its way to the debounce.
        let publisher = MlsStateSync::new(Box::new(replica.clone()), &keypair(1));
        block_on(publisher.publish_provider(&ProviderReplica::from_engine(&alice)))
            .expect("the provider blob lands on its own — the commit gate's step 2");

        // A second device launches on that replica.
        let conv_nest = Arc::new(FakeConvNest::default());
        let second = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend, _manager, _sync, restored_slices) =
            launched_device(&second, &conv_nest, &replica, 1);

        assert_eq!(
            restored_slices, 0,
            "the fixture's whole point: the channel list arrived, not one slice did"
        );
        assert!(
            !second.list_groups().is_empty(),
            "and the restore DID bring a real MLS channel with a peer in it, out of the \
             very blob it read — without this the test would pass vacuously on a replica \
             that carried nothing at all, which is the sibling test's case, not this one"
        );

        assert_eq!(
            backend.domain_evidence("peer.test"),
            DomainEvidence::Unloaded,
            "the channel list is not the evidence: participants live in the history \
             slices, none of which arrived, so this client has established nothing about \
             `peer.test` and must not say it has"
        );
        assert!(
            classify_foreign_non_answer(&non_answer, backend.domain_evidence("peer.test")).terminal,
            "a foreign non-answer must stay a failed lookup — an email chip and a send in \
             the clear is the one thing a merely-empty store may never buy"
        );
    }

    /// **A thread-less channel in the listing does not hold the account at
    /// `Unloaded`** — `devices.md` § Durability rules, Rule 3's closing
    /// parenthesis: a provider-listed channel WITHOUT a history blob is the
    /// legitimate shape of the deliberately thread-less scheduling and folder
    /// channels, and the history blob is what marks "this is a chat thread" at
    /// restore. `federation.md` § Peer-auth model → *Discovery-failure
    /// semantics* keys the launch-evidence predicate on the durable chat
    /// marker for exactly that reason: the slices the evidence lives in are
    /// owed by chat channels, and by nothing else.
    ///
    /// Before this pin the predicate was a bare count — every listed channel
    /// against every arrived slice — so an account holding ONE folder or
    /// scheduling group stayed `Unloaded` on every launch for ever: it could
    /// address no email recipient, and no incoming message could fix it (an
    /// unbound channel is exactly what `poll_inbound_conv` early-returns on).
    /// The 2026-09-22 whole-suite linux sweep's eight recipient-lookup reds
    /// were this shape: `listed=N carried_slices=N-1` on 53 consecutive
    /// launches, the one missing channel never bound.
    ///
    /// Three groups, one slice: the chat channel's slice arrives and IS the
    /// evidence (its peer's domain reads `KnownFauna`), the two thread-less
    /// groups restore beside it and count for nothing, and a domain the slices
    /// never mention is an established absence.
    #[test]
    fn a_thread_less_listed_channel_does_not_hold_the_account_unloaded() {
        use fauna_conversations::backend::{
            ConvRpcError, DomainEvidence, classify_foreign_non_answer,
        };
        use fauna_mls::state_replica::ProviderReplica;

        let non_answer = ConvRpcError::Transient {
            message: "no route to host".into(),
        };

        let replica = MemReplica::default();
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let carol = Arc::new(MlsEngine::new_in_memory(keypair(3)).unwrap());
        let dave = Arc::new(MlsEngine::new_in_memory(keypair(4)).unwrap());

        // The chat channel, marked the way `bind_channel` marks it.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (chat, _welcome) = alice.create_group(&bob_kps).unwrap();
        alice.mark_channel_chat(&chat);
        // A folder-shaped group: an engine group with no thread and no marker
        // — the owner-side `FolderGroupCrypto for Arc<MlsEngine>` shape, which
        // never binds, never marks, and never writes a slice.
        let carol_kps = carol.generate_key_packages(1).unwrap();
        let (_folder, _welcome) = alice.create_group(&carol_kps).unwrap();
        // A scheduling delivery: durably marked scheduling, never bound.
        let dave_kps = dave.generate_key_packages(1).unwrap();
        let (scheduling, _welcome) = alice.create_group(&dave_kps).unwrap();
        alice.mark_channel_scheduling(&scheduling);

        // At rest: the provider listing all three, and the ONE slice a chat
        // channel owes — Rule 2's order, history then provider, is the
        // debounced save's; here both simply land.
        let publisher = MlsStateSync::new(Box::new(replica.clone()), &keypair(1));
        block_on(publisher.load()).expect("an empty replica loads");
        block_on(publisher.publish_provider(&ProviderReplica::from_engine(&alice)))
            .expect("the provider blob lands");
        let mut slice =
            unsent_fauna_thread_slice(alice.identity_actor_id(), bob.identity_actor_id());
        slice.channel_id_hex = chat.to_string();
        slice.participants = vec![
            TypedAddress::Fauna {
                handle: "alice@home.test".into(),
                actor_id: alice.identity_actor_id(),
            },
            TypedAddress::Fauna {
                handle: "bob@peer.test".into(),
                actor_id: bob.identity_actor_id(),
            },
        ];
        assert!(
            block_on(publisher.save_history_if_changed(&slice)).expect("the slice lands"),
            "fixture: the chat channel's slice is at rest"
        );

        // A second device launches on that replica.
        let conv_nest = Arc::new(FakeConvNest::default());
        let second = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend, manager, _sync, restored) = launched_device(&second, &conv_nest, &replica, 1);

        assert_eq!(restored, 1, "fixture: the one chat slice arrived and bound");
        assert_eq!(
            second.list_groups().len(),
            3,
            "fixture: all three groups restored — two of them thread-less by design, \
             which is the whole point"
        );
        assert!(
            second.is_channel_scheduling(&scheduling),
            "fixture: the scheduling marker rode the replica"
        );
        // The harvest `probe_address` runs before every resolve: every thread's
        // participants, shown to the rail. Done here by hand so the evidence
        // asserted below is what the RESTORE carried, not what the fixture
        // wrote.
        let thread_id = backend
            .thread_for_channel(&chat)
            .expect("fixture: the chat channel bound its restored thread");
        let detail = manager
            .thread_detail(thread_id)
            .expect("fixture: the restored thread is in the store");
        backend.observe_participants(&detail.participants);

        assert_eq!(
            backend.domain_evidence("peer.test"),
            DomainEvidence::KnownFauna,
            "the chat slice's participant is the evidence, and it arrived"
        );
        assert_eq!(
            backend.domain_evidence("elsewhere.test"),
            DomainEvidence::AbsentFromLoadedEvidence,
            "every slice a chat channel owes arrived, so the account's conversations ARE \
             loaded — a thread-less folder or scheduling group owes no slice and must not \
             hold the whole account at `Unloaded`"
        );
        assert!(
            !classify_foreign_non_answer(&non_answer, backend.domain_evidence("elsewhere.test"))
                .terminal,
            "with the absence established, first contact falls through to email by ruling \
             — the carve-out this account was being denied"
        );
    }

    /// **A chat Welcome-join persists its (still empty) slice before returning**
    /// — `devices.md` § Durability rules, Rule 3, the join-side twin of
    /// `bootstrap_group`'s persist.
    ///
    /// `ingest_welcome` binds the thread (which stamps the durable chat marker)
    /// and used to leave the slice to the 1.5 s autosave. In that window the
    /// commit gate's provider-only put — on ANY channel — lists a chat-marked
    /// channel with no `history/<ch>` at rest, which is the one durable pairing
    /// `federation.md` § *Discovery-failure semantics* says must be
    /// unrepresentable: a device restoring it holds the whole account at
    /// `Unloaded`, and a chat channel restored without its slice never binds,
    /// so nothing this device does can heal it. The producer is the device
    /// that HAS the thread, so the fix is at the producer: the join persists
    /// the slice the way the bootstrap does.
    #[test]
    fn a_welcome_join_persists_its_slice_before_the_provider_can_list_it() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let (backend, manager, _sync, _restored) = launched_device(&alice, &conv_nest, &replica, 1);

        // Bob founds the group with alice's key package; alice joins from the
        // Welcome through the real door.
        let alice_kps = alice.generate_key_packages(1).unwrap();
        let (channel, welcome) = bob.create_group(&alice_kps).unwrap();
        let welcome_bytes = welcome.to_bytes().expect("serialize welcome");
        let path = history_path(&channel.to_string());
        assert!(
            replica.raw(&path).is_none(),
            "fixture: nothing at rest for the channel before the join"
        );

        block_on(ingest_welcome(
            &backend,
            &manager,
            &channel.to_string(),
            &welcome_bytes,
            "",
        ))
        .expect("the welcome join succeeds");

        assert!(
            backend.thread_for_channel(&channel).is_some(),
            "fixture: the join bound a thread"
        );
        assert!(
            alice.is_channel_chat(&channel),
            "the join stamped the durable chat marker — the very thing that makes \
             this channel OWE a slice at the next provider put"
        );
        assert!(
            replica.raw(&path).is_some(),
            "the joined channel's slice is at rest before `ingest_welcome` returns: a \
             provider put in the debounce window can no longer list a chat channel \
             whose history blob does not exist"
        );
    }

    /// **A chat Welcome-join persists the `provider` blob before returning** —
    /// `devices.md` § Durability rules, Rule 3 on a spent init key: the joined
    /// crypto state exists nowhere but this engine until the replica carries
    /// it, and there is no re-Welcome for a member already in the group. The
    /// launch door's merge keeps a native store's copy; a web engine has no
    /// store to merge from, so the producer persists. Pinned the way a user
    /// would see it: a second device of the same identity, launched the
    /// instant the join returned, holds the group and decrypts the peer's next
    /// message on it.
    #[test]
    fn a_welcome_join_persists_the_provider_before_returning() {
        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());
        let alice = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(keypair(2)).unwrap());
        let (backend, manager, _sync, _restored) = launched_device(&alice, &conv_nest, &replica, 1);

        let alice_kps = alice.generate_key_packages(1).unwrap();
        let (channel, welcome) = bob.create_group(&alice_kps).unwrap();
        let welcome_bytes = welcome.to_bytes().expect("serialize welcome");
        block_on(ingest_welcome(
            &backend,
            &manager,
            &channel.to_string(),
            &welcome_bytes,
            "",
        ))
        .expect("the welcome join succeeds");

        let alice2 = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (_backend2, _manager2, _sync2, restored) =
            launched_device(&alice2, &conv_nest, &replica, 1);
        assert_eq!(restored, 1, "the join's slice is at rest (the slice pin)");
        assert!(
            alice2.has_group(&channel),
            "the `provider` blob at rest lists the joined group when `ingest_welcome` \
             returns — a quit inside the autosave debounce no longer loses the join"
        );
        let post = bob
            .encrypt(
                &channel,
                &ChannelMessage {
                    sender: bob.identity_actor_id(),
                    sequence: 1,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("after the join".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        let got = alice2
            .decrypt(&channel, &post)
            .expect("the second device decrypts the peer's next message");
        assert!(matches!(got.body, ChannelMessageBody::Text(t) if t == "after the join"));
    }

    /// **A device launched BEFORE the mint is not addressed by the Welcome, and
    /// the push arm says so below `warn`** — `devices.md` § Cross-device MLS
    /// group-state sync → *Who may consume a Welcome — any device that holds its
    /// key*, the paragraph that settles what the non-holder does.
    ///
    /// The sibling test above is the holders' case: every device launched from
    /// the mint's flush holds the addressed init key, so every one of them
    /// joins. This is the other one, and it is the *steady state* of any account
    /// whose second app stays open for days while the first replenishes the
    /// pool: D launched before the package existed, so no key of D's is
    /// addressed by a Welcome sent through it — on both arms, forever.
    ///
    /// That is not a fault, and this pins all four halves of saying so: D's
    /// ingest fails through the production door with the **typed** engine
    /// verdict carried across the seam (`WelcomeNotAddressedHere`, asked of our
    /// own provider storage — never parsed out of a dependency's error text);
    /// the receive loop's push arm reports it at `info`, off the `error` line a
    /// genuine ingest fault produces and out of the ring the user reads; D's
    /// flush is as quiet (it joined nothing to disagree about); and the group
    /// still reaches D — by the other door, the targeted sibling-group import,
    /// once the sibling that *could* join has flushed.
    #[test]
    fn a_device_launched_before_the_mint_is_not_addressed_and_the_push_arm_says_so_below_warn() {
        use crate::test_tracing::capture_tracing_at_info;
        use fauna_conversations::backend::WelcomeChannelKind;
        use fauna_conversations::session::report_welcome_ingest_failure;

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // Device D launches FIRST — the account has no key package at all yet,
        // so D's provider can never hold the one minted after this point.
        let engine_d = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_d, manager_d, sync_d, _) = launched_device(&engine_d, &conv_nest, &replica, 1);

        // Device A launches, mints the account's pool package, flushes it
        // durably (the mint's durable-before-publish order).
        let engine_a = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&engine_a, &conv_nest, &replica, 1);
        let kp = engine_a.generate_key_packages(1).unwrap();
        assert!(
            block_on(sync_a.save_provider_if_changed(&ProviderReplica::from_engine(&engine_a)))
                .unwrap(),
            "the mint's init key is durable before the package can be published"
        );

        // A peer welcomes the account through that package.
        let peer = MlsEngine::new_in_memory(keypair(2)).unwrap();
        let (channel, welcome) = peer.create_group(&kp).unwrap();
        let welcome_bytes = welcome.to_bytes().unwrap();
        let hex = channel.to_string();

        // The nest fans the push to both live connections. A holds the key and
        // joins, as the holders' test pins.
        let tid_a = block_on(ingest_welcome(
            &backend_a,
            &manager_a,
            &hex,
            &welcome_bytes,
            "",
        ))
        .expect("A joins from the push");
        assert_eq!(backend_a.thread_for_channel(&channel), Some(tid_a));

        // D takes the same push through the same door and cannot join. The
        // verdict is typed, and the push arm's own reporting door — the one the
        // receive loop calls with exactly this pair — keeps it below `warn`.
        let (err, lines_d) = capture_tracing_at_info(|| {
            let err = block_on(ingest_welcome(
                &backend_d,
                &manager_d,
                &hex,
                &welcome_bytes,
                "",
            ))
            .expect_err("D holds no init key for the package this Welcome addresses");
            report_welcome_ingest_failure(&WelcomeChannelKind::Dm, &err);
            err
        });
        assert!(
            matches!(err, BackendError::WelcomeNotAddressedHere),
            "the non-holder's failure is the engine's own typed verdict, not a \
             diagnostic indistinguishable from a real ingest fault; got {err:?}"
        );
        assert!(
            !lines_d
                .iter()
                .any(|l| l.starts_with("[WARN]") || l.starts_with("[ERROR]")),
            "the multi-device steady state must not reach the ring as a fault; got {lines_d:?}"
        );
        assert!(
            lines_d
                .iter()
                .any(|l| l.starts_with("[INFO]") && l.contains("does not hold")),
            "…but it is still said once, at info; got {lines_d:?}"
        );
        assert_eq!(
            backend_d.thread_for_channel(&channel),
            None,
            "nothing is bound on D by a Welcome it could not open"
        );
        assert!(!engine_d.has_group(&channel));

        // D's flush is as quiet: it joined nothing, so it has no transition to
        // disagree with A's about.
        let (_, lines_flush) = capture_tracing_at_info(|| {
            block_on(sync_d.save_provider_if_changed(&ProviderReplica::from_engine(&engine_d)))
                .unwrap()
        });
        assert!(
            !lines_flush.iter().any(|l| l.starts_with("[WARN]")),
            "a device that could not join is not a second writer; got {lines_flush:?}"
        );

        // A's join flushed its provider itself (Rule 3 at the join, 2026-09-22),
        // so the autosave has nothing left to land. D's next sweep runs the
        // targeted import first, and the group arrives by that door — the
        // delivery the un-acked row was always waiting on.
        assert!(
            !block_on(sync_a.save_provider_if_changed(&ProviderReplica::from_engine(&engine_a)))
                .unwrap(),
            "A's join persisted the provider before returning — nothing left to flush"
        );
        let adopter = backend_d
            .sibling_group_adopter()
            .expect("the launch wired the targeted import")
            .clone();
        block_on(adopter.adopt_if_changed()).expect("the targeted import pass runs");
        assert!(
            engine_d.has_group(&channel),
            "the group reaches the non-holder by the other door, once the sibling's flush lands"
        );

        // And it is a live member: the peer's next message decrypts on D too.
        let post = peer
            .encrypt(
                &channel,
                &ChannelMessage {
                    sender: peer.identity_actor_id(),
                    sequence: 1,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("to the device that could not join".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        let got = engine_d
            .decrypt(&channel, &post)
            .expect("D decrypts the peer's message through the imported group");
        assert!(
            matches!(got.body, ChannelMessageBody::Text(t) if t == "to the device that could not join")
        );
    }

    /// **Two online devices both consume one Welcome through the production
    /// door, and neither flush reports a conflict** — `devices.md` § Cross-device
    /// MLS group-state sync → *Who may consume a Welcome*.
    ///
    /// Device A mints the account's pool package and flushes (the mint's
    /// durable-before-publish order); devices B and C launch from that flush,
    /// so all three hold the package's private init key. A peer welcomes the
    /// account through it. The nest fans the push to every live connection —
    /// A and B — and each runs `ingest_welcome`, the one function the receive
    /// loop's push arm and the durable drain both dispatch to. The engine-level
    /// pin (`fauna-mls::state_replica::two_online_devices_both_consume_one_welcome_as_one_identical_leaf`)
    /// shows the two joins are byte-identical; this pins that the assembly
    /// above agrees: both bind a thread for the channel, both flushes land,
    /// the second's three-way merge says nothing at the `info` filter the log
    /// ring runs, and the peer's next message decrypts on both. C — offline at
    /// push time — consumes the same Welcome later from its drain, still joins
    /// (it holds the key too), and its flush is as quiet: a Welcome is never
    /// stranded on a device that merely missed the push.
    #[test]
    fn two_online_devices_both_consume_one_welcome_and_neither_flush_reports_a_conflict() {
        use crate::test_tracing::capture_tracing_at_info;

        let replica = MemReplica::default();
        let conv_nest = Arc::new(FakeConvNest::default());

        // Device A: launch clean, mint the pool package, flush it durably.
        let engine_a = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_a, manager_a, sync_a, _) = launched_device(&engine_a, &conv_nest, &replica, 1);
        let kp = engine_a.generate_key_packages(1).unwrap();
        assert!(
            block_on(sync_a.save_provider_if_changed(&ProviderReplica::from_engine(&engine_a)))
                .unwrap(),
            "the mint's init key is durable before the package can be published"
        );

        // Devices B and C launch from that flush — every launched device holds
        // the private init key the pool package addresses.
        let engine_b = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_b, manager_b, sync_b, _) = launched_device(&engine_b, &conv_nest, &replica, 1);
        let engine_c = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (backend_c, manager_c, sync_c, _) = launched_device(&engine_c, &conv_nest, &replica, 1);

        // A peer welcomes the account through the package.
        let peer = MlsEngine::new_in_memory(keypair(2)).unwrap();
        let (channel, welcome) = peer.create_group(&kp).unwrap();
        let welcome_bytes = welcome.to_bytes().unwrap();
        let hex = channel.to_string();

        // The push reaches A and B; each ingests through the push arm's door.
        // Each join persists its provider before returning (Rule 3 at the
        // join, 2026-09-22), so B's join merges against A's landed replica
        // right there: one transition applied twice is the same bytes —
        // nothing to report.
        let tid_a = block_on(ingest_welcome(
            &backend_a,
            &manager_a,
            &hex,
            &welcome_bytes,
            "",
        ))
        .expect("A joins from the push");
        let (tid_b, lines_b) = capture_tracing_at_info(|| {
            block_on(ingest_welcome(
                &backend_b,
                &manager_b,
                &hex,
                &welcome_bytes,
                "",
            ))
            .expect("B joins from the same push")
        });
        assert_eq!(backend_a.thread_for_channel(&channel), Some(tid_a));
        assert_eq!(backend_b.thread_for_channel(&channel), Some(tid_b));
        assert!(
            !lines_b.iter().any(|l| l.starts_with("[WARN]")),
            "a double-consumed Welcome is not a two-writer collision; got {lines_b:?}"
        );

        // The autosave has nothing left to land on either device.
        for (name, sync, engine) in [("A", &sync_a, &engine_a), ("B", &sync_b, &engine_b)] {
            assert!(
                !block_on(sync.save_provider_if_changed(&ProviderReplica::from_engine(engine)))
                    .unwrap(),
                "device {name}'s join persisted the provider before returning"
            );
        }

        // Both are live members.
        let post = peer
            .encrypt(
                &channel,
                &ChannelMessage {
                    sender: peer.identity_actor_id(),
                    sequence: 1,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("to whichever device is open".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        for (name, engine) in [("A", &engine_a), ("B", &engine_b)] {
            let got = engine
                .decrypt(&channel, &post)
                .unwrap_or_else(|e| panic!("device {name} decrypts the peer's next message: {e}"));
            assert!(
                matches!(got.body, ChannelMessageBody::Text(t) if t == "to whichever device is open")
            );
        }

        // C missed the push. Its drain hands it the same Welcome later: C holds
        // the key, joins, and the join's own flush against the twice-landed
        // replica is equally quiet — nothing is stranded on the device that
        // was offline.
        let (tid_c, lines_c) = capture_tracing_at_info(|| {
            block_on(ingest_welcome(
                &backend_c,
                &manager_c,
                &hex,
                &welcome_bytes,
                "",
            ))
            .expect("C joins from its drain")
        });
        assert_eq!(backend_c.thread_for_channel(&channel), Some(tid_c));
        assert!(
            !lines_c.iter().any(|l| l.starts_with("[WARN]")),
            "the late consumer is the same leaf too; got {lines_c:?}"
        );
        assert!(
            !block_on(sync_c.save_provider_if_changed(&ProviderReplica::from_engine(&engine_c)))
                .unwrap(),
            "C's join persisted the provider before returning"
        );
        let got = engine_c
            .decrypt(&channel, &post)
            .expect("C decrypts the peer's message too");
        assert!(
            matches!(got.body, ChannelMessageBody::Text(t) if t == "to whichever device is open")
        );

        // The durable path lists the channel exactly once, whichever device
        // wrote last — and its slice is at rest beside it: each join persisted
        // the still-empty slice before returning (Rule 3's join-side persist),
        // the CAS merge folding the three identical writes into one blob, so a
        // fourth launch binds the thread rather than restoring a bare group.
        let fresh = Arc::new(MlsEngine::new_in_memory(keypair(1)).unwrap());
        let (_, _, sync_d, restored) = launched_device(&fresh, &conv_nest, &replica, 1);
        assert!(
            fresh.has_group(&channel),
            "a fourth launch restores the group"
        );
        assert_eq!(
            restored, 1,
            "…and binds it from the slice the joins put at rest"
        );
        drop(sync_d);
    }
}
