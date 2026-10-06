//! Owner-side **content-key + envelope orchestration** for shared folders —
//! the high-level `bind_set` / `remove_member` / `republish_envelope` calls that
//! wrap the thin [`FoldersClient`] kinds with the M2 content-key custody dance,
//! so all 7 apps dispatch **one** call instead of re-implementing the
//! rotate-on-removal sequence (priority #2;
//! `docs/goal/architecture/mls-group-key-material.md` § M2 content-key mechanism).
//!
//! The direct analog of `fauna-client-subscriptions::orchestration`
//! ([`SubscriptionsAuthor`]), differing only in the distribution channel: a
//! shared set distributes its content key over an **MLS group** (a per-group
//! content-key *envelope* sealed under the group's current epoch secret) instead
//! of per-subscriber X25519 `KeyBlob` wraps. The MLS-group operations (remove a
//! member + advance the epoch, seal the envelope under the current epoch) are
//! abstracted behind the [`FolderGroupCrypto`] seam so this crate stays free of a
//! `fauna-mls` dependency (wasm-light + deterministically testable); the real
//! `MlsEngine`-backed implementation is a thin adapter at the FFI / wasm boundary
//! where `fauna-mls` already lives.
//!
//! ## What each call does
//!
//! - **`bind_set`** records a fresh genesis content key in custody (`fauna.state.folder-keys`)
//!   and publishes the genesis envelope (seal under the current epoch →
//!   `content_key.put`). The MLS group is created by the share flow
//!   (`fauna.folders.share`); this gives it its first content key. Idempotent.
//! - **`republish_envelope`** re-seals the **same** generation bundle under the
//!   group's current epoch and re-publishes it — what a member *add* needs
//!   (history-on-join: the new joiner reads the back-catalogue) with **no**
//!   rotation (adding a member doesn't break forward secrecy).
//!   **⚠ NOT WIRED — it has zero callers repo-wide (not even a test).** It is the
//!   orphaned half of the *add-to-existing-set* flow, which is unbuilt: this
//!   orchestration exposes no `add_member` (and
//!   `libs/fauna-ffi/src/folders_author.rs` says so at its `folders_share` doc).
//!   Sharing today creates the group with its member already in it, so nothing needs
//!   a republish yet. Stated explicitly because the sentence above used to read as
//!   though clients drive it — the same "exported, documented, uncalled" shape that
//!   let `resume_pending_removals` sit dark on all six apps for months (fixed
//!   2026-07-12; see `mls-group-key-material.md` § M2 *Rotate-on-removal*). When the
//!   add flow is built, wire this and delete this warning.
//! - **`remove_member`** *rotates* to a fresh content key (forward secrecy: the
//!   leaver must not read future content): MLS Remove (epoch advances) →
//!   re-publish the envelope under the **new** epoch (only continuing members /
//!   the rotated generation) → evict the removed member from the nest roster
//!   (F1/OBS-1) → commit the rotation into custody.
//!
//! ## Crash-safety (the load-bearing design)
//!
//! A removal's fresh content key is **irrecoverable** once the nest stores the
//! re-sealed envelope: lose it after the publish and the owner can neither decrypt
//! their own future content nor grant it to new joiners — a no-user-data-loss
//! violation. So a removal **stages** the new generation in
//! the custody's `pending_removals` (a `fauna.state.folder-keys` removal row) and **persists it before the publish**,
//! committing it into the set's `current` only once the nest confirms. A crash
//! between publish and commit is healed by [`FoldersAuthor::resume_pending_removals`],
//! which re-drives the staged generation idempotently (the MLS Remove is a no-op if
//! the member is already gone; the `content_key.put` upserts; the `members.evict`
//! `DELETE` is idempotent). This is the same resume-sentinel shape as
//! `SubscriptionsAuthor::resume_pending_removals`, which is why this orchestration
//! **owns** persistence (a [`FolderKeyStore`]) rather than taking `&mut FoldersConfig`.
//!
//! The **MLS Remove commit itself** has its own, stricter crash discipline —
//! Rule 1 of `devices.md` § Cross-device MLS group-state sync (never merge an
//! epoch-advancing commit until its bytes, or the staged pending that produces
//! them, are durable somewhere the restart path will find them). See
//! [`FoldersAuthor::drive_removal`]: the gated route rides the device-owned-epoch
//! commit rebase loop via the [`FolderCommitGate`] seam; the ungated fallback
//! persists the staged pending + the sentinel bytes before the merge and never
//! rebuilds over durable bytes.
//!
//! ## Ordering (the forward-secrecy invariant)
//!
//! [`FoldersAuthor::remove_member`] performs the MLS Remove (which advances the
//! group epoch) **strictly before** sealing the rotated envelope, so the new
//! generation is sealed under the **post-removal** epoch — a still-old-epoch
//! removed member can read neither the re-sealed envelope (MLS forward secrecy)
//! nor post-removal content (new generation). Sealing under the *old* epoch would
//! hand the new generation to the removed member. The seal seam reads the group's
//! *current* epoch, so calling it after the Remove naturally seals under the new
//! one.

use crate::key_reader::FolderKeyStore;
use fauna_core::data::{FolderPendingRemoval, FoldersConfig, Timestamp};
use fauna_core::folder_keys::{ContentKeyGeneration, FolderContentKeys, serve_custody_channel_id};
use fauna_core::identity::ActorId;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::conversations::{ChannelEnvelope, ChannelSendReply, ChannelSendRequest};
use fauna_protocol::folders::{ContentKeyPutRequest, MemberEvictRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use zeroize::Zeroizing;

use crate::FoldersClient;
use crate::custody::{self, CustodyError};
#[cfg(feature = "mls")]
use crate::webdav_provision::WebdavProvisionError;
#[cfg(feature = "mls")]
use fauna_client_capabilities::{
    DEFAULT_GRANT_WINDOW_SECS, DerivedGrantGeneration, MintGrantError, folder_paywall_generation,
    grant_log, mint_folder_grant,
};
#[cfg(feature = "mls")]
use fauna_mls::wrapped_blob::{GrantWindow, WrapError};
#[cfg(feature = "mls")]
use fauna_protocol::wrapped_blob::{
    FetchBridgePubkeyReply, FetchBridgePubkeyRequest, MintGrantReply, MintGrantRequest,
    RevokeGrantReply, RevokeGrantRequest,
};

/// The MLS-group operations the content-key orchestration needs, abstracted so
/// this crate does not depend on `fauna-mls` (kept wasm-light + deterministically
/// testable). The real implementation wraps an `MlsEngine` at the FFI / wasm
/// boundary; tests use an in-memory fake. All ops are synchronous (the engine
/// locks its in-memory group state) — only the nest calls are async.
pub trait FolderGroupCrypto {
    /// A group-side failure (MLS state error, member-not-found, encode).
    type Error: core::fmt::Display + core::fmt::Debug;

    /// Create a fresh MLS group admitting `member_key_packages` (TLS-serialized
    /// KeyPackage bytes, one per initial member) and merge the add commit so the
    /// owner's group is at its first post-add epoch. The share flow's group-create
    /// step (mirrors `MlsEngine::create_group` + `group_id_bytes`). Returns the
    /// [`CreatedGroup`] the share orchestration threads into `fauna.folders.share`
    /// (the raw group id), `bind_set` (the derived ChannelId), and the per-member
    /// `welcome.deliver` (the serialized Welcome).
    fn create_group(&self, member_key_packages: &[Vec<u8>]) -> Result<CreatedGroup, Self::Error>;

    /// Whether this client's engine currently **holds** the set's MLS group
    /// (`channel_id` = the derived ChannelId custody stores). The share flow's
    /// add-path branch probes it: a set the nest reports bound but whose group
    /// this engine does not hold is a *fresh device before its replica restore*,
    /// and the add refuses retryably rather than minting a fresh group (which
    /// would re-bind the set and drop the earlier members — the very bug M2
    /// *Admitting a member* fixes). Distinct from [`Self::contains_member`],
    /// which asks about a *member's* leaf and returns `false` for a group this
    /// client doesn't hold at all — conflating the two would send a fresh device
    /// down the first-share path.
    fn holds_group(&self, channel_id: &[u8; 32]) -> Result<bool, Self::Error>;

    /// Derive the set's **ChannelId** (custody's storage key + roster address)
    /// from its raw openMLS group id — `ChannelId::from_group_id(raw)`, the SAME
    /// fold the nest applies server-side. The add-path branch reads the raw group
    /// id off the nest's `mls_group_id` and derives the channel through this seam
    /// (rather than depending on `fauna-mls` directly) to probe [`Self::holds_group`]
    /// and address the group, keeping the derivation on the one MLS boundary.
    fn channel_id_for_group(&self, raw_group_id: &[u8]) -> [u8; 32];

    /// Stage an MLS **Add** commit admitting the member whose TLS-serialized
    /// KeyPackage is `key_package_bytes` into the set's **existing** group
    /// (`channel_id`) **without merging it** — the add-path twin of
    /// [`Self::remove_member_staged`], returning `(commit_bytes, welcome_bytes)`.
    /// The owner's epoch does *not* advance until [`Self::merge_pending_commit`];
    /// the caller distributes the commit to the existing members and delivers the
    /// Welcome to the newcomer (`mls-group-key-material.md` § M2 *Admitting a
    /// member*: no rotation on admit — the newcomer receives the full generation
    /// bundle re-sealed under the post-Add epoch their Welcome places them at).
    /// The Welcome is MLS-mintable **only** inside this Add commit, so the caller
    /// must deliver these exact bytes; a lost Welcome is not re-constructible and
    /// heals only through the re-share evict-then-re-add gesture. Merge/clear
    /// discipline mirrors [`Self::remove_member_staged`]: distribute-then-merge on
    /// an accepted send (Rule-1), [`Self::clear_pending_commit`] on a failed one.
    fn add_member_staged(
        &self,
        channel_id: &[u8; 32],
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), Self::Error>;

    /// Whether `member` is currently in the set's MLS group. Used to skip a
    /// pointless rotation when removing a member who was never in the group.
    fn contains_member(&self, channel_id: &[u8; 32], member: &ActorId)
    -> Result<bool, Self::Error>;

    /// Build the MLS Remove commit for `member` **without merging it** — the
    /// owner's epoch does *not* advance yet. Returns the commit bytes the caller
    /// must first persist and then distribute to the remaining members; `None` if
    /// `member` was already absent (an idempotent resumed removal produces no new
    /// commit). The group carries a *pending* commit until
    /// [`Self::merge_pending_commit`] or [`Self::clear_pending_commit`].
    ///
    /// Staged rather than optimistic because the merge is the **point of no
    /// return**: MLS cannot re-produce a commit for an already-merged transition
    /// and remaining members cannot skip epochs, so the bytes must be durable
    /// before the epoch advances (see [`FoldersAuthor::drive_removal`]).
    fn remove_member_staged(
        &self,
        channel_id: &[u8; 32],
        member: &ActorId,
    ) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Merge the group's own pending Remove commit, advancing the owner's epoch
    /// (the subsequent seal then seals under the new epoch). Called
    /// only once [`Self::remove_member_staged`]'s bytes are durably persisted.
    fn merge_pending_commit(&self, channel_id: &[u8; 32]) -> Result<(), Self::Error>;

    /// Whether the group currently carries a staged, unmerged pending commit.
    /// `false` for a group this client doesn't hold. Lets a resumed drive
    /// distinguish "merge the restored pending" from "the merge already
    /// happened" without tripping the engine's no-pending error.
    fn has_pending_commit(&self, channel_id: &[u8; 32]) -> Result<bool, Self::Error>;

    /// The **commit identity** — `blake3` of the wire commit bytes — of the
    /// group's staged pending commit, or `None` when nothing is pending (or the
    /// pending carries no identity stamp, which a byted resume refuses like a
    /// foreign pending).
    /// Durable next to the pending itself, so a restart restores the two
    /// together.
    ///
    /// A byted resume compares this against `blake3(sentinel.commit)` **before
    /// merging**: an equal hash proves the restored pending is this sentinel's
    /// own commit and is safe to merge; a *different* hash means the engine holds
    /// a **foreign** pending (a sibling drive's gate-staged commit), which the
    /// resume must refuse to merge — merging it while broadcasting the sentinel's
    /// bytes forks the
    /// group.
    fn pending_commit_hash(&self, channel_id: &[u8; 32]) -> Result<Option<[u8; 32]>, Self::Error>;

    /// Discard a pending commit left by an interrupted drive. A **safe no-op**
    /// when nothing is pending (and when the group is unknown to this client).
    /// Only called when the commit is **provably undistributed** (no durable
    /// sentinel bytes exist — bytes are persisted before the merge, which
    /// precedes the send), so rebuilding after the clear cannot fork the group.
    fn clear_pending_commit(&self, channel_id: &[u8; 32]) -> Result<(), Self::Error>;

    /// Durably persist the group's crypto state — **including a staged pending
    /// commit** — to this client's local store, so a crash-restarted engine can
    /// still merge the exact commit whose bytes the sentinel carries
    /// (`devices.md` § Cross-device MLS group-state sync, Rule 1: the staged
    /// pending must be durable somewhere the restart path will find it). The
    /// native adapter snapshots the provider KV to SQLite
    /// (`MlsEngine::save_state`); on wasm this is a no-op — a web engine's only
    /// persistence is the nest replica, whose durability the **gated** route
    /// provides (an ungated wasm client keeps the pre-existing
    /// reload-rolls-back posture).
    fn persist_group_state(&self, channel_id: &[u8; 32]) -> Result<(), Self::Error>;

    /// Seal the envelope `payload` — the **full** generation bundle
    /// (history-on-join), the set's live nonce with its minter and the set's
    /// lineage (`ContentKeyEnvelopePayload`) — under the group's **current**
    /// epoch secret, returning `(sealed_bytes, epoch)`. Called after
    /// [`Self::remove_member`] on a rotation so the seal is under the
    /// post-removal epoch. The owner's signature over the sealed bytes is the
    /// author's (`fauna_protocol::folder_envelope_sig`).
    fn seal_envelope(
        &self,
        channel_id: &[u8; 32],
        payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    ) -> Result<(Vec<u8>, u64), Self::Error>;

    /// The group's current epoch — what a stored envelope sealed under an
    /// earlier one is behind (`writer-signed-change-records.md` ruling
    /// (11)(b): the owner re-publishes whenever the published epoch is behind
    /// the group's).
    fn envelope_epoch(&self, channel_id: &[u8; 32]) -> Result<u64, Self::Error>;

    /// Open a sealed envelope at the group's current epoch — the owner reading
    /// back what it published, to tell whether it is current.
    fn open_envelope(
        &self,
        channel_id: &[u8; 32],
        sealed: &[u8],
    ) -> Result<fauna_core::folder_keys::ContentKeyEnvelopePayload, Self::Error>;
}

/// A freshly-created MLS group for a shared set — the output of
/// [`FolderGroupCrypto::create_group`], threaded through the share flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedGroup {
    /// The **derived** `ChannelId` (`ChannelId::from_group_id(raw_group_id)`) —
    /// custody's storage key + the `bind_set` / `content_key.*` / roster address.
    pub channel_id: [u8; 32],
    /// The **raw** openMLS group id (variable length). Sent (hex) to
    /// `fauna.folders.share`, which re-derives the identical ChannelId
    /// server-side; also rides the `welcome.deliver` `WelcomeKind::Group { group_id }`.
    pub raw_group_id: Vec<u8>,
    /// The serialized Welcome to deliver to the (single) new member so they join
    /// the group and land on the nest `actor_channels` roster.
    pub welcome: Vec<u8>,
}

/// The result of a successful [`FoldersAuthor::share_set`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareOutcome {
    /// The shared set's derived `ChannelId` (hex-decodable to the custody key) —
    /// the value `fauna.folders.share` echoed back, equal to the locally-created
    /// group's [`CreatedGroup::channel_id`].
    pub channel_id: [u8; 32],
    /// The nest inbox row id the member's Welcome was delivered to
    /// (`welcome.deliver` reply) — surfaced so a caller can confirm delivery.
    pub inbox_id: i64,
}

/// The result of a [`FoldersAuthor::remove_member`] (or a resumed removal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    /// The MLS Remove commit bytes this removal produced — **already distributed
    /// to the remaining members** by [`FoldersAuthor::drive_removal`] (5d(d)):
    /// the orchestration posts them to the set's channel as a
    /// `ChannelEnvelope::Commit` via `fauna.conversations.channel.send`, so the
    /// remaining members' poll advances their epoch and they can open the
    /// re-sealed envelope. Returned for observability only — callers need NOT
    /// (and must not separately) re-send it. `None` when the member was already
    /// absent from the group (a resumed/no-op removal produced no new commit; a
    /// resumed ungated drive re-sends the sentinel's staged bytes instead) —
    /// and always `None` on the **gated** route ([`FolderCommitGate`]), whose
    /// commit is built, sent, and merged inside the rebase loop.
    pub commit: Option<Vec<u8>>,
    /// Whether the nest roster row was actually removed (`false` on a resumed
    /// eviction — already gone).
    pub evicted: bool,
    /// Whether a rotation actually happened. `false` ⇒ the member was never in the
    /// group and nothing was staged (a pure no-op, no rotation/publish/evict).
    pub rotated: bool,
}

/// How a [`FolderCommitGate::gated_remove`] pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedRemoval {
    /// No [`FolderCommitGate`] is wired on the author at all — a
    /// test-only single-device deployment. The caller falls back to the ungated
    /// staged discipline. **Implementors never return this**: it is the author's
    /// own marker for "no gate to consult" (and for the byted-resume bypass).
    /// A wired gate whose backend has no live `CommitGate` reports
    /// [`Self::GateNotEngaged`] instead — the two are NOT interchangeable (see
    /// that variant).
    NoGate,
    /// The gate is wired, but the backend carries no live `CommitGate` — the
    /// multi-device replica plane is still restoring, or its load permanently
    /// failed (a replica restore or load failure, which degrades to
    /// the single-device fallback).
    ///
    /// **This is not [`Self::NoGate`], and the difference is fork-safety.** With
    /// no gate there is no walk-to-head, so the caller cannot answer the one
    /// question that makes a rebuild safe — *was a commit for this removal
    /// already distributed?* Only an **engaged** gated attempt can distribute
    /// without recording bytes on the sentinel (the commit rides the rebase
    /// loop), and every such attempt durably stamps the sentinel's
    /// `gated_attempted` BEFORE the gate runs — so the sentinel itself answers
    /// it: stamped `false` is provably undistributed and drives ungated
    /// (the single-device fallback — on a nest whose plane never engages, this
    /// is what lets the removal complete at all); stamped `true` defers
    /// retryably, exactly as [`FoldersAuthor::recover_poisoned_removal`] does.
    GateNotEngaged,
    /// The member is not (or no longer) in the set's MLS group — a resumed
    /// removal whose commit already landed, or a genuine no-op. No commit was
    /// produced; the caller completes the rotation-only remainder.
    AlreadyAbsent,
    /// The Remove commit was staged, made durable in the provider replica,
    /// gate-sent under `expect_no_commit_since`, accepted, and merged — the
    /// Rule-1-conformant path (`devices.md` § Cross-device MLS group-state
    /// sync). The group's epoch is already advanced; the caller must NOT send
    /// the commit again.
    Removed,
    /// The channel carries an epoch transition this device has not incorporated
    /// and this pass could not heal (a restored still-pending commit awaiting
    /// its own-leaf resync, or a future-epoch strand). Building a fresh commit
    /// now could fork the group — the caller must abort transiently and retry
    /// after the background poll / resync converges.
    PendingUnconverged,
}

/// How a [`FolderCommitGate::gated_add`] pass ended — the add-path twin of
/// [`GatedRemoval`]. An add stages no irrecoverable key material and keeps no
/// resume sentinel (`mls-group-key-material.md` § M2 *Admitting a member*: the
/// crash story is the visible off-roster ghost + the re-share heal, not a
/// staged-key sentinel), so this carries neither the `AlreadyAbsent` idempotence
/// arm nor the `gated_attempted` distinction `GatedRemoval` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatedAdd {
    /// No [`FolderCommitGate`] is wired on the author at all — the author's own
    /// marker for "no gate to consult" (implementors never return this). The
    /// caller falls back to the ungated Rule-1 staged discipline.
    NoGate,
    /// The gate is wired, but the backend carries no live `CommitGate` (the
    /// multi-device replica plane is still restoring or its load failed). Unlike
    /// a removal, a fresh add has no prior distributed attempt to reason about,
    /// so the caller safely falls back to the ungated Rule-1 discipline (as it
    /// does for [`Self::NoGate`]).
    GateNotEngaged,
    /// The channel carries an epoch transition this device has not incorporated
    /// and this pass could not heal. Building a fresh Add commit now could fork
    /// the group — the caller aborts retryably and re-attempts once the
    /// background poll / resync converges.
    PendingUnconverged,
    /// The Add commit was staged, made durable in the provider replica, gate-sent,
    /// accepted, and merged (the Rule-1-conformant path). The group's epoch is
    /// already advanced; the caller must NOT re-send the commit — it delivers the
    /// carried Welcome bytes to the newcomer.
    Added(Vec<u8>),
}

/// The **gated commit route** for a shared folder — the seam through which
/// [`FoldersAuthor::drive_removal`] / [`FoldersAuthor::drive_add`] reach the
/// device-owned-epoch commit rebase loop
/// (`fauna-conversations::backend::CommitGate`) without this crate owning the
/// backend, the replica sync, or the conversations plane. (Named
/// `FolderCommitGate`, not `RemovalCommitGate`, since the same seam carries both
/// membership mutations — an Add on the 2nd..Nth share and a Remove on eviction.)
///
/// The production impl (`mls_adapter.rs`, `mls` feature) wraps the session's
/// `Arc<FaunaMlsBackend>`: it serializes on the backend's per-channel lock,
/// walks the folder channel log to head (healing an own-leaf crash-window
/// commit via the shared resync arm), clears a provably-unsent restored
/// pending, and routes the fresh commit through
/// `CommitGate::gated_{remove,add}_member`. Errors are rendered strings — the
/// caller surfaces them as [`FoldersAuthorError::RemovalGate`] /
/// [`FoldersAuthorError::AddDeferred`] (retryable posture, like a transport
/// fault).
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait FolderCommitGate: fauna_core::MaybeSendSync {
    /// Run one gated-removal pass for `member` on the set bound to
    /// `channel_id` (the derived ChannelId bytes custody stores). Idempotent:
    /// an already-removed member reports [`GatedRemoval::AlreadyAbsent`].
    async fn gated_remove(
        &self,
        channel_id: &[u8; 32],
        member: &ActorId,
    ) -> Result<GatedRemoval, String>;

    /// Run one gated-**add** pass admitting the member whose TLS-serialized
    /// KeyPackage is `key_package_bytes` into the set's existing group
    /// (`channel_id`) — the add-path twin of [`Self::gated_remove`]. Stages the
    /// Add commit, walks the channel to head first (healing a crash-window
    /// commit), gate-sends + merges it through the rebase loop, and returns the
    /// newcomer's Welcome bytes ([`GatedAdd::Added`]) for the caller to deliver.
    /// The caller only reaches this after discriminating the roster (a member
    /// already in the group is a caller-side error — the share flow evicts a
    /// ghost leaf first), so no `AlreadyAbsent`-style arm is needed.
    async fn gated_add(
        &self,
        channel_id: &[u8; 32],
        key_package_bytes: &[u8],
    ) -> Result<GatedAdd, String>;

    /// Whether the gate is **engaged** — a live `CommitGate` is injected on the
    /// backend, so a [`Self::gated_remove`] pass *can distribute* a Remove
    /// commit (inside the rebase loop, recording no bytes on the sentinel).
    /// [`FoldersAuthor::drive_removal`] probes this BEFORE calling
    /// [`Self::gated_remove`], to durably stamp the sentinel's
    /// `gated_attempted` first — the stamp must precede anything that can
    /// distribute, and a not-engaged pass must NOT stamp (on a nest whose
    /// `fauna.mls` plane never engages, stamping every probe would defer the
    /// sentinel forever).
    /// Engagement is one-way (the backend's `CommitGate` slot is a latched
    /// `OnceLock`), so `true` here cannot revert before the `gated_remove`
    /// call it guards.
    fn engaged(&self) -> bool;

    /// The backend's per-channel serialization lock, when this gate fronts a
    /// live backend (`FaunaMlsBackend::channel_lock`). `drive_removal` holds it
    /// across the whole **ungated** commit section (stage → merge → send, and
    /// the resume-merge leg) so the background folder poll — which merges
    /// inbound commits under the same lock — cannot merge a foreign commit
    /// mid-window: openmls drops the staged pending at *any* merge, which would
    /// destroy the durable-bytes merge source and strand the removal in the
    /// fail-loud arm (second route). Default `None`: no backend, no concurrent poll to
    /// serialize against (the gate-less single-device plane and test fakes).
    fn ungated_channel_lock(
        &self,
        channel_id: &[u8; 32],
    ) -> Option<std::sync::Arc<futures_util::lock::Mutex<()>>> {
        let _ = channel_id;
        None
    }
}

/// A failure from the owner-side content-key orchestration. Generic over the
/// transport error `E` (native `NestClientError` / wasm `WsRpcError`) and the
/// group-crypto error `GE`.
#[derive(Debug)]
pub enum FoldersAuthorError<E, GE> {
    /// A `fauna.folders.*` WS-RPC call failed at the transport or with a
    /// non-retryable server rejection.
    Transport(E),
    /// Reading or writing the folder-key custody (`fauna.state.folder-keys`,
    /// through the author's [`FolderKeyStore`]) failed — the account runtime
    /// absent, the writer door refusing while no tip resolves, a port fault.
    CustodyStore(anyhow::Error),
    /// An MLS-group operation (remove / seal / membership) failed.
    Group(GE),
    /// A custody transition (commit-generation) failed.
    Custody(CustodyError),
    /// Encoding the Remove-commit `ChannelEnvelope` for distribution failed
    /// (canonical dag-cbor encode — should be unreachable for byte payloads).
    Envelope(String),
    /// The owner holds no content keys for this set (it was never bound, so there
    /// is nothing to rotate / publish). Carries the set's derived `ChannelId`.
    NoContentKeys([u8; 32]),
    /// The share target has no KeyPackage available on the nest, so the owner
    /// cannot create the MLS group to share with them (they must publish key
    /// packages first). Carries the member's `ActorId`.
    NoKeyPackage(ActorId),
    /// The `ChannelId` the nest derived for `fauna.folders.share` did not match
    /// the locally-created group's derived ChannelId — a derivation drift between
    /// this crate's `MlsEngine` and the nest's `ChannelId::from_group_id`. Should
    /// be unreachable (both BLAKE3-derive from the same raw group id); a guard.
    ChannelMismatch { local: [u8; 32], nest: String },
    /// The [`FolderCommitGate`] failed or reported
    /// [`GatedRemoval::PendingUnconverged`] — the gated Remove could not run to
    /// completion this pass. Retryable: the staged rotation sentinel is intact,
    /// so a later [`FoldersAuthor::resume_pending_removals`] (or a retried
    /// remove) completes it once the channel converges.
    RemovalGate(String),
    /// The add path (2nd..Nth share) could not proceed this pass and is
    /// **retryable** — the set is bound to a group this device does not hold yet
    /// (a fresh device before its replica restore), or the gated add reported
    /// [`GatedAdd::PendingUnconverged`] (the channel carries an unincorporated
    /// commit). No irrecoverable state is staged (an add mints no key material —
    /// `mls-group-key-material.md` § M2 *Admitting a member*), so re-driving the
    /// share once the device converges completes it; a crashed mid-flight add
    /// heals through the re-share evict-then-re-add gesture. (A pre-existing
    /// **removal** blocking the add surfaces as [`Self::RemovalGate`] instead —
    /// the resume-then-add guard drives it and propagates its own deferral.)
    AddDeferred(String),
    /// A byted removal sentinel whose staged pending is **unrecoverable**: the
    /// sentinel carries commit bytes but this engine holds no matching pending
    /// and the member is still in the group, so the commit can be neither merged
    /// nor safely rebuilt (rebuilding forks; sealing would go under the
    /// pre-removal epoch). Reachable without a crash — a wasm tab reload between
    /// the stage and the send, or a cross-device union of the sentinel onto a
    /// peer that never staged the pending.
    ///
    /// **Distinct from [`Self::RemovalGate`] on purpose**: this is the one
    /// removal error that is *not* fixed by simply retrying — the pending will
    /// never reappear — so [`FoldersAuthor::resume_pending_removals`] recognises
    /// it, keeps driving the OTHER sentinels (never `?`-aborts on it), and it is
    /// discharged only by the client-reachable
    /// [`FoldersAuthor::recover_poisoned_removal`]. Carries the set's derived
    /// `ChannelId` + the stranded member so a client can surface + recover it
    /// without opening the sentinel (`devices.md` § Cross-device MLS group-state
    /// sync; the *No client-causable unrecoverable nest state* product invariant —
    /// every nest state a client can reach must be recoverable by a client).
    PoisonedRemovalSentinel {
        channel_id: [u8; 32],
        member: ActorId,
    },
    /// The owner's custody holds no live set nonce for a set whose served-era
    /// rows need adopting (`writer-signed-change-records.md` ruling (7)(b)):
    /// nothing can be signed for it, and the flip OFF stays refused. The
    /// custody reconcile restores the nonce; then the toggle again.
    NoSetNonce,
    /// A served row the sweep could not build a statement over (a malformed
    /// hash or device id the nest served) — the adoption's page is not sent.
    MalformedServedRow(String),
    /// A principal's folder read twin could not be revoked before the
    /// serve-off rotation: the owner's grant log was unreadable, or the
    /// `Revoke`s could not be recorded (a nest refusal is
    /// [`Self::Transport`]). The set stays served in custody and the
    /// rotation owed.
    PrincipalGrants(String),
}

/// What the served-era sweep did ([`FoldersAuthor::adopt_served_rows`],
/// summed across [`FoldersAuthor::serve_disable`]'s rounds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServedRowsAdoption {
    /// Rows this device's sweep signed in place.
    pub adopted: u64,
    /// Unsigned pseudo-device rows the shared predicate rejected — never
    /// signed; zero on an honest nest (ruling (7)(b)(i)(1)).
    pub skipped: u64,
}

/// How many sweep-then-flip rounds [`FoldersAuthor::serve_disable`] runs
/// before surfacing `served_rows_unadopted`: each round signs the DAV writes
/// that landed since the previous sweep, and the flip is what stops them.
pub const SERVE_DISABLE_ADOPTION_ROUNDS: u32 = 3;

impl<E, GE> FoldersAuthorError<E, GE> {
    /// A set lifecycle helper's error on the author's surface: the custody
    /// write is [`Self::Config`], the nest call [`Self::Transport`].
    fn from_lifecycle(e: crate::set_lifecycle::SetLifecycleError<E>) -> Self {
        match e {
            crate::set_lifecycle::SetLifecycleError::Custody(e) => Self::CustodyStore(e),
            crate::set_lifecycle::SetLifecycleError::Nest(e) => Self::Transport(e),
        }
    }
}

impl<E: core::fmt::Display, GE: core::fmt::Display> core::fmt::Display
    for FoldersAuthorError<E, GE>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "folders transport: {e}"),
            Self::CustodyStore(e) => write!(f, "folders custody store: {e:#}"),
            Self::Group(e) => write!(f, "folders group crypto: {e}"),
            Self::Custody(e) => write!(f, "folders custody: {e}"),
            Self::Envelope(e) => write!(f, "folders commit-envelope encode: {e}"),
            Self::NoContentKeys(ch) => {
                write!(f, "no content keys held for folder {}", hex::encode(ch))
            }
            Self::RemovalGate(e) => write!(f, "folders gated removal: {e}"),
            Self::AddDeferred(e) => write!(f, "folders add deferred (retryable): {e}"),
            Self::NoKeyPackage(member) => write!(
                f,
                "share target {} has no key package available",
                hex::encode(member.0)
            ),
            Self::ChannelMismatch { local, nest } => write!(
                f,
                "share channel mismatch: local-derived {} != nest-derived {nest}",
                hex::encode(local)
            ),
            Self::PoisonedRemovalSentinel { channel_id, member } => write!(
                f,
                "poisoned removal sentinel for folder {} member {}: byted sentinel \
                 with no recoverable pending, member still present — not retryable, \
                 needs client recovery (recover_poisoned_removal)",
                hex::encode(channel_id),
                hex::encode(member.0),
            ),
            Self::NoSetNonce => write!(
                f,
                "no live set nonce in custody to adopt the served rows under"
            ),
            Self::MalformedServedRow(e) => write!(f, "malformed served row: {e}"),
            Self::PrincipalGrants(e) => write!(f, "principal folder grant revoke: {e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug, GE: core::fmt::Display + core::fmt::Debug>
    std::error::Error for FoldersAuthorError<E, GE>
{
}

type AuthorResult<T, R, G> =
    Result<T, FoldersAuthorError<<R as RpcRequester>::Error, <G as FolderGroupCrypto>::Error>>;

/// Error from the composed per-set WebDAV serve toggle
/// ([`FoldersAuthor::serve_set`] — the `folder-webdav-toggle` production
/// caller). Distinguishes the two legs so a client can react to each: a
/// `Provision(NoMsek)` in particular means the actor has no mail credential yet,
/// so the UI keeps the toggle disabled with a "set up mail first" hint rather
/// than surfacing a raw failure (webdav-server.md § Independent enablement).
#[cfg(feature = "mls")]
#[derive(Debug)]
pub enum ServeSetError<E, GE> {
    /// The serve-enable / serve-disable orchestration failed (content-key
    /// genesis or rotation, envelope republish for a shared set, or the nest
    /// `fauna.folders.update` `webdav_enabled` flag flip).
    Serve(FoldersAuthorError<E, GE>),
    /// Re-provisioning the MSEK-sealed `WebdavKeysBlob` failed after the flag
    /// flip. The flag change is already committed, so the MDA is fail-closed on
    /// this set until the next successful reconcile; the client should retry.
    Provision(WebdavProvisionError<E>),
}

#[cfg(feature = "mls")]
impl<E: core::fmt::Display, GE: core::fmt::Display> core::fmt::Display for ServeSetError<E, GE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Serve(e) => write!(f, "serve orchestration: {e}"),
            Self::Provision(e) => write!(f, "webdav-keys re-provision: {e}"),
        }
    }
}

#[cfg(feature = "mls")]
impl<E: core::fmt::Display + core::fmt::Debug, GE: core::fmt::Display + core::fmt::Debug>
    std::error::Error for ServeSetError<E, GE>
{
}

/// [`FoldersAuthor::paywall_set`] failed — the web-paywall analogue of
/// [`ServeSetError`]. It composes the same custody genesis / re-seal + a nest
/// flag flip (`Paywall`) with the additional capability-grant mint to the
/// web-serve holder (`Mint`, the grant plane replacing the WebDAV blob channel).
#[cfg(feature = "mls")]
#[derive(Debug)]
pub enum PaywallSetError<E, GE> {
    /// The custody genesis / re-seal stage, the `fauna.folders.set_web_paywall`
    /// flag flip, or the `fauna.capabilities.mint` send failed. A crash here
    /// leaves the set's exposure ≤ intent (files fail closed, never leak — see
    /// [`FoldersAuthor::paywall_set`]); the client should retry (idempotent).
    Paywall(FoldersAuthorError<E, GE>),
    /// Building the `content.read{folder:set}` grant blob failed
    /// (`mint_folder_grant`: no custody for the set, an unwrappable payload, or
    /// an HPKE seal failure — e.g. a malformed holder pubkey). Encoding the built
    /// blob for the wire also surfaces here (its `WrapError`).
    Mint(MintGrantError),
    /// The owner's grant log could not record the paywall grant's event, or
    /// refused it: no log is wired ([`FoldersAuthor::with_grant_log`]), the
    /// log store refused the read or the write, the event could not be signed,
    /// or the log already ends this grant id with a `Revoke` (a re-mint under
    /// it would be invisible to the owner's log and swept by the trust
    /// facet's reconcile). Nothing was deposited (`ui/nests.md` § Trust facet
    /// — grants → *Record-then-deposit*).
    GrantLog(String),
}

#[cfg(feature = "mls")]
impl<E: core::fmt::Display, GE: core::fmt::Display> core::fmt::Display for PaywallSetError<E, GE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Paywall(e) => write!(f, "paywall orchestration: {e}"),
            Self::Mint(e) => write!(f, "folder grant mint: {e}"),
            Self::GrantLog(e) => write!(f, "paywall grant log: {e}"),
        }
    }
}

#[cfg(feature = "mls")]
impl<E: core::fmt::Display + core::fmt::Debug, GE: core::fmt::Display + core::fmt::Debug>
    std::error::Error for PaywallSetError<E, GE>
{
}

/// The owner-side shared-folder content-key orchestration. Bundles the thin
/// `fauna.folders.*` call surface ([`FoldersClient`]) with the owner-private
/// state it mutates (a [`FolderKeyStore`] — the content-key custody +
/// crash-recovery sentinel, `fauna.state.folder-keys`), the identity it acts as
/// (the owner's keypair, whose actor id a served set's keys blob is sealed for
/// reconcile seals under) and the MLS-group seam ([`FolderGroupCrypto`]). One
/// instance per actor on each Fauna app.
pub struct FoldersAuthor<R: RpcRequester, G: FolderGroupCrypto> {
    files: FoldersClient<R>,
    /// The owner's identity: it signs the served-era adoption
    /// ([`Self::adopt_served_rows`]), and under `mls` builds the serve
    /// reconcile and the paywall grants.
    keypair: ActorKeypair,
    custody: std::sync::Arc<dyn FolderKeyStore>,
    /// The account's mail custody (`fauna.state.mail`) — the MSEK the served
    /// sets' `WebdavKeysBlob` is sealed under ([`Self::serve_set`]'s
    /// reconcile), so read only where `mls` builds that composition.
    #[cfg_attr(not(feature = "mls"), allow(dead_code))]
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    group: G,
    /// The gated Remove-commit route (`devices.md` § Cross-device MLS
    /// group-state sync, Rule 1) — `Some` when the leg wired the multi-device
    /// plane ([`Self::with_commit_gate`]); `None` drives the ungated staged
    /// discipline in [`Self::drive_removal`].
    commit_gate: Option<std::sync::Arc<dyn FolderCommitGate>>,
    /// The flipping client's served-set walk (`webdav-server.md` § Key model,
    /// the app-side seams bullet, part (c)) — `Some` when the face wired the
    /// seat's byte plane ([`Self::with_served_set_converge`]); [`Self::serve_set`]
    /// runs it right after an enable of an unshared set succeeds. This crate
    /// holds no byte plane, so the walk is injected, never built here.
    served_converge: Option<std::sync::Arc<dyn fauna_core::nest_reseal::ServedSetConverge>>,
    /// The owner's signed grant log (`fauna.state.succession-ledger`) the
    /// web-paywall grant's `Mint`, `Renew` and `Revoke` events are recorded in
    /// ([`Self::with_grant_log`]) — the log the Nests trust facet projects and
    /// its reconcile sweep judges by. `None` makes every paywall leg refuse
    /// before any side effect rather than deposit a grant the log omits.
    #[cfg_attr(not(feature = "mls"), allow(dead_code))]
    grant_log: Option<std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore>>,
}

impl<R: RpcRequester, G: FolderGroupCrypto> FoldersAuthor<R, G>
where
    R::Error: RpcErrorClass,
{
    /// Build over the thin folders client, the owner's config client (its
    /// identity), the account's folder-key custody — the seat's
    /// `PlaneFolderKeys` (web: the account port's forwarder) — its mail
    /// custody (the MSEK a served set's `WebdavKeysBlob` seals under), and the
    /// MLS-group seam.
    pub fn new(
        files: FoldersClient<R>,
        keypair: ActorKeypair,
        custody: std::sync::Arc<dyn FolderKeyStore>,
        mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
        group: G,
    ) -> Self {
        Self {
            files,
            keypair,
            custody,
            mail,
            group,
            commit_gate: None,
            served_converge: None,
            grant_log: None,
        }
    }

    /// Wire the owner's grant log (see the `grant_log` field) — the face passes
    /// its account-store ledger seam. The web-paywall legs
    /// ([`Self::paywall_set`], [`Self::rotate_paywall_grant`],
    /// [`Self::unpaywall_set`]) refuse without it; nothing else reads it.
    pub fn with_grant_log(
        mut self,
        ledger: std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    ) -> Self {
        self.grant_log = Some(ledger);
        self
    }

    /// The principals' folder read twins over the wired grant log
    /// (`webdav-server.md` § Key model → *A principal's read* rule (4)): the
    /// serve reconcile renews them, the serve-off tail revokes them. `None`
    /// without a log — no twin can be found.
    #[cfg(feature = "mls")]
    fn principal_grants(&self) -> Option<crate::principal_grants::PrincipalFolderGrants> {
        self.grant_log.as_ref().map(|log| {
            crate::principal_grants::PrincipalFolderGrants::new(
                std::sync::Arc::clone(log),
                &self.keypair,
            )
        })
    }

    /// **Rule (4)'s unserve leg** — revoke every principal's folder read twin
    /// over `name`, nest first then the signed `Revoke`s, in the serve-off
    /// tail BEFORE the key rotates ([`Self::serve_disable`],
    /// [`Self::unserve_sets_the_nest_holds_off`]), so the honest-box end
    /// lands with the owner's act. A failure leaves the rotation unrun and
    /// the set served in custody; the launch pass's unserve arm re-drives
    /// the tail. No log wired, or a build without `mls`: nothing to revoke.
    async fn revoke_principal_grants(&self, name: &str) -> AuthorResult<usize, R, G> {
        #[cfg(feature = "mls")]
        if let Some(principals) = self.principal_grants() {
            return principals
                .revoke_over(self.files.requester(), name, Timestamp::now().0 / 1_000_000)
                .await
                .map_err(|e| match e {
                    crate::principal_grants::FolderGrantError::Transport(e) => {
                        FoldersAuthorError::Transport(e)
                    }
                    other => FoldersAuthorError::PrincipalGrants(other.to_string()),
                });
        }
        let _ = name;
        Ok(0)
    }

    /// Wire the served-set walk the serve composition runs after an enable
    /// (see the `served_converge` field) — the face passes
    /// [`crate::served_set_converge`] over its own session and the app's
    /// recording device.
    pub fn with_served_set_converge(
        mut self,
        converge: std::sync::Arc<dyn fauna_core::nest_reseal::ServedSetConverge>,
    ) -> Self {
        self.served_converge = Some(converge);
        self
    }

    /// Wire the [`FolderCommitGate`] — the per-app leg passes the session
    /// backend's adapter so member removals route through the device-owned-epoch
    /// commit rebase loop instead of the ungated staged path. The seam itself
    /// reports [`GatedRemoval::GateNotEngaged`] when the backend has no
    /// `CommitGate` injected, so wiring this unconditionally is safe on a
    /// single-device client: a **freshly staged** removal still falls back to the
    /// ungated discipline. (Only a *resumed* sentinel defers there — it may carry
    /// an already-distributed commit; see [`GatedRemoval::GateNotEngaged`].)
    ///
    /// Every production leg wires this — native (`mls_sync_launch`,
    /// `folders_author`), linux, and web — so [`GatedRemoval::NoGate`] is a
    /// test-only state in practice.
    pub fn with_commit_gate(mut self, gate: std::sync::Arc<dyn FolderCommitGate>) -> Self {
        self.commit_gate = Some(gate);
        self
    }

    /// Borrow the thin folders client for the pure reads the UI renders from
    /// (`list`, `members_list`, `devices`, …) — those need no orchestration.
    pub fn client(&self) -> &FoldersClient<R> {
        &self.files
    }

    /// Load → mutate → join the custody (the store's join is the conflict
    /// resolution — no CAS, no retry); answers the custody as it now reads.
    async fn update_custody(
        &self,
        edit: impl FnOnce(&mut FoldersConfig),
    ) -> anyhow::Result<FoldersConfig> {
        Ok(crate::key_reader::update(&*self.custody, edit).await?.0)
    }

    /// Seal `payload` under the group's current epoch and sign the sealed
    /// bytes as the owner (`writer-signed-change-records.md` ruling (11)(b):
    /// every envelope is owner-signed, so a member — which can seal under the
    /// group-held AEAD — can never publish one). Returns the blob the nest
    /// stores and the sealing epoch.
    fn seal_and_sign(
        &self,
        channel_id: &[u8; 32],
        payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    ) -> AuthorResult<(Vec<u8>, u64), R, G> {
        let (sealed, epoch) = self
            .group
            .seal_envelope(channel_id, payload)
            .map_err(FoldersAuthorError::Group)?;
        Ok((
            fauna_protocol::folder_envelope_sig::sign(&self.keypair, channel_id, &sealed),
            epoch,
        ))
    }

    /// Publish the content-key envelope for `channel_id` from `cfg`'s custody by
    /// sealing the full generation bundle under the group's current epoch.
    async fn publish_envelope(
        &self,
        cfg: &FoldersConfig,
        name: &str,
        channel_id: &[u8; 32],
    ) -> AuthorResult<u64, R, G> {
        let keys = custody::content_keys(cfg, channel_id)
            .ok_or(FoldersAuthorError::NoContentKeys(*channel_id))?;
        let payload = custody::envelope_payload(cfg, Some(name), channel_id, keys);
        self.publish_payload(name, channel_id, &payload).await
    }

    /// Seal, sign and store `payload` as the set's content-key envelope —
    /// [`Self::publish_envelope`]'s second half, and what the envelope
    /// reconcile publishes its join through ([`Self::converge_envelopes`]).
    async fn publish_payload(
        &self,
        name: &str,
        channel_id: &[u8; 32],
        payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    ) -> AuthorResult<u64, R, G> {
        let (sealed, epoch) = self.seal_and_sign(channel_id, payload)?;
        self.files
            .content_key_put(ContentKeyPutRequest {
                name: name.to_string(),
                epoch: epoch as i64,
                sealed: hex::encode(&sealed),
                // The owner-stamped version floor for non-owner records (KMH
                // § M2; nest-side monotonic — an older stamp never lowers it).
                current_version: payload.keys.current_version(),
                ..Default::default()
            })
            .await
            .map_err(FoldersAuthorError::Transport)?;
        Ok(epoch)
    }

    /// Create a set through the one create helper
    /// ([`crate::set_lifecycle::create_set`] — its nonce minted into custody
    /// before the nest sees the set).
    pub async fn create_set(
        &self,
        req: fauna_protocol::folders::FolderCreateRequest,
    ) -> AuthorResult<fauna_protocol::folders::FolderCreateReply, R, G> {
        crate::set_lifecycle::create_set(&self.files, &*self.custody, req)
            .await
            .map_err(FoldersAuthorError::from_lifecycle)
    }

    /// Delete a set through the one delete helper
    /// ([`crate::set_lifecycle::delete_set`] — its custody retired first).
    pub async fn delete_set(
        &self,
        name: &str,
    ) -> AuthorResult<fauna_protocol::folders::FolderDeleteReply, R, G> {
        crate::set_lifecycle::delete_set(&self.files, &*self.custody, name)
            .await
            .map_err(FoldersAuthorError::from_lifecycle)
    }

    /// Bind a freshly-shared set's genesis content key + publish the genesis
    /// envelope. **Custody-first + persist-first** so a crash after the share but
    /// before the key persists cannot strand a set the owner can't seal for:
    /// [`custody::record_new_set`] is idempotent (a retry keeps the existing key),
    /// and re-publishing upserts. Call after `fauna.folders.share` created the
    /// MLS group.
    ///
    /// **Pre-bind re-seal (M2 history-on-join):** nothing to stage here. Files
    /// that existed *before* the bind are sealed under the owner-only path and
    /// undecryptable by a joiner until re-sealed under the genesis content key;
    /// that re-seal is the sync agent's ungated, idempotent
    /// `SyncEngine::reseal_pending_under_current` on every engine start, which
    /// picks the set up from the pushed custody this write creates
    /// (`mls-group-key-material.md` § M2 *Pre-bind re-seal migration*; the
    /// custody sentinel that once marked the set as owing a pass was retired
    /// 2026-09-25 — no reader was left).
    pub async fn bind_set(&self, name: &str, channel_id: [u8; 32]) -> AuthorResult<(), R, G> {
        // Cross-device-safe persist of the fresh (irrecoverable) content key:
        // a join, so a peer device's concurrent custody write
        // is never dropped. `record_new_set` is the infallible mutation;
        // `update_custody` answers the stored custody.
        let cfg = self
            .update_custody(|cfg| {
                // Share-after-serve: adopt any custody recorded under the serve
                // pseudo-channel (the set was WebDAV-served before being shared
                // — `webdav-server.md` § Key model custody note). Moving the
                // generations to the real derived ChannelId BEFORE the
                // idempotent `record_new_set` means the existing served-era key
                // history (files are already stamped with its versions) becomes
                // the set's group custody and rides the envelope to members —
                // never a second, divergent genesis.
                custody::migrate_set_identity(
                    cfg,
                    &serve_custody_channel_id(name),
                    channel_id,
                    Timestamp::now().0,
                );
                custody::key_named_set(
                    cfg,
                    name,
                    channel_id,
                    *fresh_content_key(),
                    Timestamp::now().0,
                );
            })
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        self.publish_envelope(&cfg, name, &channel_id).await?;
        self.stamp_set_name(&cfg, name, &channel_id).await;
        Ok(())
    }

    /// Move the set's sealed NAME to the audience a bind just created: re-seal
    /// it under the set's current content-key generation and push it by hash
    /// (`path-sealing.md` § the set-name plane). The create sealed the name
    /// under the owner's root, which no member holds, and a sealed set rests no
    /// plaintext name — so until this stamp lands a member can open the set's
    /// files but not its name, and the set is omitted from their Folders page.
    ///
    /// The owner's sync engine re-stamps at every start too
    /// (`SyncEngine::stamp_sealed_set_name`), but only a set bound to a local
    /// folder on some owner device has an engine; the bind is the one writer
    /// every shared set has. Convergent, so the two never disagree.
    /// **Best-effort**: a name is a display label and must never fail the bind
    /// it rides on — a failed push is logged and the engine's stamp repairs it.
    async fn stamp_set_name(
        &self,
        cfg: &fauna_core::data::FoldersConfig,
        name: &str,
        channel_id: &[u8; 32],
    ) {
        let Some(keys) = custody::content_keys(cfg, channel_id) else {
            return;
        };
        let root = fauna_core::path_crypto::LabelRoot::content_key(
            *keys.current_key(),
            keys.current_version(),
        );
        let sealed = match fauna_core::label_custody::seal_set_name(&root, name) {
            Ok(Some(sealed)) => sealed,
            // A reserved `__` set carries no sealed name.
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(name),
                    error = %format!("{e:#}"),
                    "sealing the set name for its new audience failed"
                );
                return;
            }
        };
        if let Err(e) = self
            .files
            .update(fauna_protocol::folders::FolderUpdateRequest {
                name: name.to_string(),
                name_sealed: Some(fauna_protocol::ByteBuf::from(sealed)),
                ..Default::default()
            })
            .await
        {
            tracing::warn!(
                folder = %fauna_core::log_redact::log_folder_name(name),
                error = %e,
                "the set-name stamp for the set's new audience did not land; an owner                  engine's start re-stamps it"
            );
        }
    }

    /// Enable WebDAV serving for a set — the **serve-ON orchestration**
    /// (`webdav-server.md` § Key model / § Independent enablement): ensure the
    /// set is content-keyed and flip the nest per-set `webdav_enabled` flag (the
    /// one-time re-seal of its pre-existing files is the sync agent's ungated
    /// pass, as for [`Self::bind_set`]).
    ///
    /// `channel_id` is the set's real derived `ChannelId` when it is **shared**
    /// (the caller derives it from `FolderSummary.mls_group_id` at the MLS
    /// boundary, as for [`Self::bind_set`]); `None` for an unshared set, whose
    /// custody is keyed by the serve pseudo-channel
    /// ([`serve_custody_channel_id`] — the channel-optional keying extension).
    ///
    /// Ordering (each crash point leaves MDA capability ≤ user intent):
    /// 1. **Custody-first, one atomic write** — genesis content key for an
    ///    unshared set (idempotent reuse if already keyed); the sync agent's
    ///    ungated pass re-seals the back-catalogue under `current` from that
    ///    custody. A **shared** set must already hold custody from its bind; a
    ///    second genesis here would diverge from the envelope members hold, so
    ///    that is an error, never a silent re-key. **The same write stamps
    ///    the owner's serve-on** ([`custody::serve_on`] —
    ///    `writer-signed-change-records.md` ruling (7)(b)(ii) rule (1)): the
    ///    served state every client judgement reads is this stamp, written by
    ///    this gesture and by nothing else, so a set custody calls served
    ///    always holds keys.
    /// 2. **Shared set: publish the envelope** — members read the owner's
    ///    stamps there (rule (4)). Before the flag: a publish that fails
    ///    leaves {custody served, flag off}, which the next launch's unserve
    ///    arm reverts ([`Self::unserve_sets_the_nest_holds_off`]).
    /// 3. **Flag ON** (`fauna.folders.update`) — the nest-side DAV gate.
    /// 4. The caller then re-provisions the `WebdavKeysBlob`
    ///    (`reconcile_webdav_keys_blob`, mls-gated — sealing needs `fauna-mls` +
    ///    MSEK, which this wasm-light crate deliberately does not hold). A crash
    ///    before it leaves the set served-but-blobless: the MDA fails closed and
    ///    the next reconcile heals.
    pub async fn serve_enable(
        &self,
        name: &str,
        channel_id: Option<[u8; 32]>,
    ) -> AuthorResult<(), R, G> {
        let custody_key = channel_id.unwrap_or_else(|| serve_custody_channel_id(name));
        if channel_id.is_some() {
            // Shared set: custody must exist from `bind_set` (see doc above).
            let cfg = self
                .custody
                .load_for_write()
                .await
                .map_err(FoldersAuthorError::CustodyStore)?;
            if custody::content_keys(&cfg, &custody_key).is_none() {
                return Err(FoldersAuthorError::NoContentKeys(custody_key));
            }
        }
        let now = Timestamp::now().0;
        let cfg = self
            .update_custody(|cfg| {
                if channel_id.is_none() {
                    custody::key_named_set(cfg, name, custody_key, *fresh_content_key(), now);
                }
                custody::serve_on(cfg, &custody_key, now);
            })
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        if channel_id.is_some() {
            self.publish_envelope(&cfg, name, &custody_key).await?;
        }
        self.set_webdav_flag(name, true).await
    }

    /// Disable WebDAV serving for a set — the **serve-OFF orchestration**
    /// (`webdav-server.md` § Key model: "unflagging a set rotates its content
    /// key … and re-provisions the blob without it").
    ///
    /// Ordering (each crash point leaves MDA capability ≤ user intent):
    /// 0. **Adopt the served era** (`writer-signed-change-records.md` ruling
    ///    (7)(b)): sign every adoptable WebDAV pseudo-device row of the set in
    ///    place ([`Self::adopt_served_rows`]) — once the flag falls no reader
    ///    exempts them, and an unsigned one would vanish from every app. The
    ///    nest refuses the flip `served_rows_unadopted` while one is owed (a
    ///    DAV write landing between the sweep and the flip); the sweep and
    ///    flip repeat, at most [`SERVE_DISABLE_ADOPTION_ROUNDS`] times, then
    ///    the refusal surfaces. A crash here leaves the set served with some
    ///    rows signed — exempt either way, nothing torn.
    /// 1. **Flag OFF first** — the nest flag is the *actual* DAV access gate
    ///    (`webdav_list_folders` / byte-token minting are served-set-scoped),
    ///    so revocation is immediate; the rotation below is defense-in-depth
    ///    against an already-exfiltrated blob covering *future* content.
    /// 2. **Rotate** the content key ([`custody::rotate_set`]) — committed
    ///    directly into the union-merged custody entry (no staging field an
    ///    older device's merge could drop; the fresh key is created and
    ///    committed in the same persist, so no irrecoverable window) — **and
    ///    stamp the owner's serve-off in that one write**
    ///    ([`custody::serve_off`], ruling (7)(b)(ii) rule (1)): custody keeps
    ///    saying served across the sweep-and-flip rounds, so a DAV row landing
    ///    between a sweep and a refused flip stays exempt until the next round
    ///    adopts it. Re-checked inside the write: a sibling's launch pass that
    ///    already unserved the set rotated it, and one flip rotates once.
    /// 3. **Shared set:** re-publish the content-key envelope under the current
    ///    epoch so members receive the new generation (no MLS commit — the
    ///    membership is unchanged; this is `republish_envelope`'s no-rotation
    ///    twin with the rotation already committed).
    /// 4. The caller then re-provisions the `WebdavKeysBlob` **without** the
    ///    set (`reconcile_webdav_keys_blob`).
    ///
    /// **The adoption never runs outside a served window** (rule (3)): when
    /// custody does not call the set served — the nest flags it and the owner
    /// never served it, or a sibling already unserved it — this pushes the
    /// flag off and signs nothing, rotates nothing. A row a lying nest planted
    /// is never signed on the nest's say-so; the nest's refusal
    /// (`served_rows_unadopted`) surfaces as the error it is.
    pub async fn serve_disable(
        &self,
        name: &str,
        channel_id: Option<[u8; 32]>,
    ) -> AuthorResult<ServedRowsAdoption, R, G> {
        let mut adoption = ServedRowsAdoption::default();
        let custody_key = channel_id.unwrap_or_else(|| serve_custody_channel_id(name));
        let cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        if !custody::channel_served(&cfg, &custody_key) {
            self.set_webdav_flag(name, false).await?;
            return Ok(adoption);
        }
        let mut round = 0;
        loop {
            round += 1;
            let swept = self.adopt_served_rows(name, channel_id).await?;
            adoption.adopted += swept.adopted;
            adoption.skipped = swept.skipped;
            match self.set_webdav_flag(name, false).await {
                Ok(()) => break,
                Err(FoldersAuthorError::Transport(e))
                    if round < SERVE_DISABLE_ADOPTION_ROUNDS
                        && e.as_rpc_error().is_some_and(|r| {
                            r.code == fauna_protocol::RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTED
                        }) => {}
                Err(e) => return Err(e),
            }
        }
        if adoption.skipped > 0 {
            // Ruling (7)(b)(i)(1): on an honest nest this is zero (the recorder
            // refuses every other shape); a skipped row is one the nest holds
            // that the honest recorder never wrote, and readers refuse it now.
            tracing::warn!(
                skipped = adoption.skipped,
                "serve-off: WebDAV rows outside the recorder's shape were left unsigned"
            );
        }
        self.revoke_principal_grants(name).await?;
        let cfg = self.unserve_in_custody(&custody_key).await?.0;
        if channel_id.is_some() {
            self.publish_envelope(&cfg, name, &custody_key).await?;
        }
        Ok(adoption)
    }

    /// **The serve-off tail, in ONE custody write**: rotate the content key
    /// and stamp the owner's serve-off — the gesture's last step
    /// ([`Self::serve_disable`]) and the launch pass's unserve arm
    /// ([`Self::unserve_sets_the_nest_holds_off`]). `is_served` is re-checked
    /// against the custody the write starts from, so two devices never rotate
    /// twice for one flip. Returns the custody as written and whether this
    /// call unserved the set.
    async fn unserve_in_custody(
        &self,
        custody_key: &[u8; 32],
    ) -> AuthorResult<(FoldersConfig, bool), R, G> {
        let fresh = fresh_content_key();
        let now = Timestamp::now().0;
        let mut unserved = false;
        let cfg = self
            .update_custody(|cfg| {
                if custody::channel_served(cfg, custody_key) {
                    custody::rotate_set(cfg, custody_key, *fresh, now);
                    unserved = custody::serve_off(cfg, custody_key, now);
                }
            })
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        Ok((cfg, unserved))
    }

    /// **The launch pass's unserve arm** (`writer-signed-change-records.md`
    /// ruling (7)(b)(ii) rule (3)): for every owned set the nest lists with
    /// its flag OFF while custody calls it served, unserve it in custody —
    /// the serve-off tail ([`Self::unserve_in_custody`]). That state is the
    /// crash state of both gestures (a serve-on that wrote custody and died
    /// before the flag, a serve-off whose flip landed and died before the
    /// custody write) and what a nest lying "off" produces; the three are
    /// indistinguishable and this one answer is safe for all — exposure ≤
    /// intent, and a lying nest achieves one rotation per owner serve-on.
    ///
    /// **{custody not served, flag on} is left alone**: the flag is never
    /// pushed (a lagging device would undo a sibling's serve-on), and
    /// nothing is ever stamped served from the nest's flag. Every judgement
    /// of the owner's already follows custody; the provisioned copies are
    /// brought to it by the two reconciles that follow this arm in
    /// [`Self::resume_pending_removals`] — the `WebdavKeysBlob` and, for a
    /// shared set, the envelope ([`Self::converge_envelopes`], which is what
    /// republishes the rotation and the stamp this arm wrote).
    ///
    /// Custody is read as a write reads it (a fresh fetch, so a lagging
    /// device acts on the plane's state). A set with a staged member removal
    /// is left to that removal's own rotation first. Per-set error isolation.
    /// Returns how many sets were unserved.
    pub async fn unserve_sets_the_nest_holds_off(&self) -> AuthorResult<usize, R, G> {
        let cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        let listed = self
            .files
            .list_wire()
            .await
            .map_err(FoldersAuthorError::Transport)?;
        let mut unserved = 0;
        for set in custody::named_from_custody(listed.folders, &cfg)
            .iter()
            .filter(|s| !s.webdav_enabled && s.role.as_deref() != Some("member"))
        {
            let Ok(channel) = crate::engine_binding::owned_custody_channel(set) else {
                continue;
            };
            if !custody::channel_served(&cfg, &channel)
                || cfg.pending_removals.iter().any(|r| r.channel_id == channel)
            {
                continue;
            }
            if let Err(e) = self.revoke_principal_grants(&set.name).await {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&set.name),
                    "folders: revoking a principal's folder grant before the unserve failed \
                     (the set stays served, retried at the next launch): {e}"
                );
                continue;
            }
            match self.unserve_in_custody(&channel).await {
                Ok((_, true)) => unserved += 1,
                Ok((_, false)) => {}
                Err(e) => tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&set.name),
                    "folders: unserving a set the nest holds off failed (retried at the next \
                     launch): {e}"
                ),
            }
        }
        Ok(unserved)
    }

    /// Re-provision the `WebdavKeysBlob` from custody
    /// ([`crate::reconcile_webdav_keys_blob`] — owned ∩ served): the launch
    /// pass's blob reconcile (ruling (7)(b)(ii) rule (3)), run unconditionally
    /// on an MSEK-holding device, so a blob a sibling provisioned with a set
    /// custody no longer calls served is brought back to custody; the re-run
    /// on the custody nudge is [`crate::ServedBlobFollower`]'s, over the same
    /// reconcile. `Ok(None)` on a device that holds no MSEK — nothing to do.
    #[cfg(feature = "mls")]
    pub async fn reprovision_webdav_keys(
        &self,
    ) -> Result<Option<usize>, WebdavProvisionError<R::Error>> {
        match crate::reconcile_webdav_keys_blob(
            &self.files,
            &self.keypair.actor_id(),
            &*self.custody,
            self.mail.as_ref(),
            self.principal_grants().as_ref(),
        )
        .await
        {
            Ok(served) => Ok(Some(served)),
            Err(WebdavProvisionError::NoMsek) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// **The served-era sweep** (`writer-signed-change-records.md` ruling
    /// (7)(b) and (i)): read the set's rows as served, sign every unsigned
    /// WebDAV pseudo-device row that
    /// [`fauna_protocol::sync_writer_sig::served_row_adoptable`] accepts — the
    /// statement the stored row with its own pseudo `device_id`, the owner as
    /// actor, the set's live nonce — and send them in pages
    /// ([`fauna_protocol::folders::SERVED_ROWS_ADOPT_PAGE`]). A row the
    /// predicate rejects is skipped and counted, never signed. Idempotent: a
    /// row already signed is not read as owed. A set with nothing owed sends
    /// nothing and needs no nonce.
    pub async fn adopt_served_rows(
        &self,
        name: &str,
        channel_id: Option<[u8; 32]>,
    ) -> AuthorResult<ServedRowsAdoption, R, G> {
        use fauna_protocol::sync_writer_sig::{ChangeSigner, SignedChange, served_row_adoptable};

        let owner = self.keypair.actor_id().0;
        let pseudo = hex::encode(fauna_core::label_custody::webdav_pseudo_device_id(&owner));
        let rows = self
            .files
            .changes_raw(name)
            .await
            .map_err(FoldersAuthorError::Transport)?;
        let mut adoption = ServedRowsAdoption::default();
        let owed: Vec<_> = rows
            .iter()
            .filter(|r| r.device_id.as_deref() == Some(pseudo.as_str()) && r.signature.is_none())
            .filter(|r| {
                let adoptable = served_row_adoptable(r);
                adoption.skipped += u64::from(!adoptable);
                adoptable
            })
            .collect();
        if owed.is_empty() {
            return Ok(adoption);
        }
        // The nonce every reader of this served set binds rows to — the same
        // resolution the engine binding makes for a served owner row
        // (`engine_binding::custody_channel_for`).
        let cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        let channel = channel_id.unwrap_or_else(|| serve_custody_channel_id(name));
        let nonce = custody::set_nonces_for(&cfg, Some(name), Some(&channel))
            .0
            .ok_or(FoldersAuthorError::NoSetNonce)?;
        let signer = ChangeSigner::direct(&self.keypair);
        for page in owed.chunks(fauna_protocol::folders::SERVED_ROWS_ADOPT_PAGE) {
            let mut signatures = Vec::with_capacity(page.len());
            for row in page {
                let statement = SignedChange::for_row_as(row, nonce, owner)
                    .map_err(|e| FoldersAuthorError::MalformedServedRow(e.to_string()))?;
                signatures.push(fauna_protocol::folders::ServedRowSignature {
                    seq: row.seq,
                    signature: fauna_protocol::ByteBuf::from(
                        signer.sign_statement(&statement).to_vec(),
                    ),
                    ..Default::default()
                });
            }
            let reply = self
                .files
                .served_rows_adopt(fauna_protocol::folders::ServedRowsAdoptRequest {
                    name: name.to_string(),
                    signer_key: fauna_protocol::ByteBuf::from(signer.signer_key().to_vec()),
                    signatures,
                    ..Default::default()
                })
                .await
                .map_err(FoldersAuthorError::Transport)?;
            adoption.adopted += reply.adopted;
        }
        Ok(adoption)
    }

    /// Flip a set's WebDAV serve state end-to-end — the **production caller** of
    /// the slice-2 serve orchestration that the Settings → Folders
    /// `folder-webdav-toggle` drives (`webdav-server.md` § Independent
    /// enablement point 2). This is the one shared composition the FFI + wasm
    /// faces (and the linux test-agent) all call, so the "flip then re-provision"
    /// pairing is enforced once for every app (priority #2) rather than
    /// re-sequenced per surface.
    ///
    /// Gate the UI on [`can_serve_webdav`](Self::can_serve_webdav): this flips
    /// the nest flag *before* the re-provision, so an enable by an MSEK-less
    /// actor commits the flag and then fails `Provision(NoMsek)`, leaving the
    /// set served-but-blobless until a later reconcile heals it.
    ///
    /// - `enable = true` → [`serve_enable`](Self::serve_enable) (content-key
    ///   genesis/reuse + the nest flag ON) then
    ///   re-provisions the `WebdavKeysBlob` **with** the set.
    /// - `enable = false` → [`serve_disable`](Self::serve_disable) (flag OFF +
    ///   content-key rotation + envelope republish for a shared set) then
    ///   re-provisions the blob **without** the set.
    ///
    /// `channel_id` is the set's derived `ChannelId` when it is shared
    /// (`FolderSummary::mls_group_id` → `ChannelId::from_group_id`), `None` when
    /// owner-only (the orchestration derives the serve pseudo-channel itself).
    /// Returns the number of served sets the re-provisioned blob now carries.
    ///
    /// Both legs always run: a serve flip whose blob re-provision is skipped
    /// would leave the MDA's key capability out of sync with the nest flag
    /// (fail-closed but stale). If the re-provision fails after a committed flag
    /// flip the error is [`ServeSetError::Provision`] and the caller should
    /// retry the reconcile (idempotent atomic replace).
    ///
    /// **Then, on an enable of an unshared set, the pre-serve back-catalogue**
    /// (`webdav-server.md` § Key model, the app-side seams bullet, part (c)):
    /// with a walk wired ([`Self::with_served_set_converge`]) the flipping
    /// client re-seals the set's nest-resident heads onto the served key before
    /// answering, so a file no sync agent holds opens through the mount.
    /// Best-effort — the flip and the re-provision are already committed, a
    /// failure is warned, and the next launch ([`Self::converge_served_sets`])
    /// or a later flip re-drives the walk (idempotent under the deterministic
    /// seal). A shared set is the M2 pre-bind pass's, never
    /// this walk's (`mls-group-key-material.md` § M2 → *Pre-bind re-seal
    /// migration* (C)).
    #[cfg(feature = "mls")]
    pub async fn serve_set(
        &self,
        name: &str,
        channel_id: Option<[u8; 32]>,
        enable: bool,
    ) -> Result<usize, ServeSetError<R::Error, G::Error>> {
        if enable {
            self.serve_enable(name, channel_id)
                .await
                .map_err(ServeSetError::Serve)?;
        } else {
            self.serve_disable(name, channel_id)
                .await
                .map_err(ServeSetError::Serve)?;
        }
        let served = crate::reconcile_webdav_keys_blob(
            &self.files,
            &self.keypair.actor_id(),
            &*self.custody,
            self.mail.as_ref(),
            self.principal_grants().as_ref(),
        )
        .await
        .map_err(ServeSetError::Provision)?;
        if enable
            && channel_id.is_none()
            && let Some(converge) = self.served_converge.as_ref()
        {
            match converge.converge_served_set(name).await {
                Ok(tally) => tracing::info!(
                    resealed = tally.resealed,
                    skipped = tally.skipped,
                    failed = tally.failed,
                    "served-set walk: pre-serve files converged onto the served key"
                ),
                Err(e) => tracing::warn!(
                    error = %format!("{e:#}"),
                    "served-set walk did not run; the next launch retries it"
                ),
            }
        }
        Ok(served)
    }

    /// [`crate::custody::named_from_custody`] over this author's own custody —
    /// the launch resumes below address a set by its name, and this client
    /// holds no label custody to render one. Custody unreadable ⇒ no sealed row
    /// is named this pass (the resume retries at the next launch).
    #[cfg(feature = "mls")]
    async fn named_from_custody(
        &self,
        rows: Vec<fauna_protocol::folders::FolderSummary>,
    ) -> Vec<fauna_protocol::folders::FolderSummary> {
        let cfg = self.custody.load().await.unwrap_or_else(|e| {
            tracing::warn!(
                error = %format!("{e:#}"),
                "folders custody unreadable; sealed sets are not named this pass"
            );
            Default::default()
        });
        crate::custody::named_from_custody(rows, &cfg)
    }

    /// **The served-set walk's launch resume** (`webdav-server.md` § Key model,
    /// the app-side seams bullet, part (c)): re-drive the wired walk
    /// ([`Self::with_served_set_converge`]) over every set this account owns
    /// that custody calls served (ruling (7)(b)(ii) rule (2) — never the
    /// nest's flag) and is group-less — the sets [`Self::serve_set`] walks
    /// right after an enable. A walk the app was killed in, or one that could not
    /// start because the set's custody had not synced yet, completes here
    /// without the user re-flipping (a re-flip is an unserve, which rotates the
    /// key, then a serve).
    ///
    /// Run by [`Self::resume_pending_removals`], the launch resume every face
    /// already drives. Idempotent and cheap when nothing is owed: one
    /// `fauna.folders.list`, then one change-log walk per served set — a head
    /// already stamped at the set's current generation is not owed, and the
    /// deterministic seal makes a repeated re-seal converge on the same
    /// manifest. No walk wired ⇒ no read at all. Per-set error isolation: a set
    /// whose walk cannot start is warned and retried at the next launch, never
    /// starving the others. Returns the summed tally.
    pub async fn converge_served_sets(
        &self,
    ) -> AuthorResult<fauna_core::nest_reseal::ServedSetConvergence, R, G> {
        let mut total = fauna_core::nest_reseal::ServedSetConvergence::default();
        let Some(converge) = self.served_converge.as_ref() else {
            return Ok(total);
        };
        let listed = self
            .files
            .list_wire()
            .await
            .map_err(FoldersAuthorError::Transport)?;
        let cfg = self.custody.load().await.unwrap_or_else(|e| {
            tracing::warn!(
                error = %format!("{e:#}"),
                "folders custody unreadable; no served set is walked this pass"
            );
            Default::default()
        });
        let listed = custody::named_from_custody(listed.folders, &cfg);
        for set in listed.into_iter().filter(|s| {
            s.mls_group_id.is_none()
                && s.role.as_deref() != Some("member")
                && custody::channel_served(&cfg, &serve_custody_channel_id(&s.name))
        }) {
            match converge.converge_served_set(&set.name).await {
                Ok(tally) => {
                    total.resealed += tally.resealed;
                    total.skipped += tally.skipped;
                    total.failed += tally.failed;
                }
                Err(e) => tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&set.name),
                    error = %format!("{e:#}"),
                    "served-set walk did not run at launch; the next launch retries it"
                ),
            }
        }
        Ok(total)
    }

    /// The owner's grant log as stored, and where the set's paywall grant
    /// stands in it — the shared prelude of the three paywall legs (mint,
    /// rotate, revoke), which must all name the same `(owner, grant_id)` pair.
    /// The id is the generation walk's (`folder_paywall_generation`,
    /// `webdav-server.md` § Key model → *A principal's read* rule (1) → *The
    /// generation*): the live generation's when one is live, else the first
    /// unspent one's, so an unpaywall spends a generation and the next
    /// paywall mints under a fresh id the log reads live.
    ///
    /// The derivation is over the **owner's identity secret**, so a seedless
    /// client (the app-dead sync agent, which holds a `BackupKey` and no seed)
    /// cannot compute it. Paywalling a set is a seed-holding author's
    /// operation (R4 (account-data-plane.md § The ratified decisions)), and
    /// the keypair this author is built over is that seed's.
    #[cfg(feature = "mls")]
    async fn paywall_walk(
        &self,
        name: &str,
    ) -> Result<
        (
            fauna_core::succession_ledger::SuccessionLedger,
            DerivedGrantGeneration,
        ),
        PaywallSetError<R::Error, G::Error>,
    > {
        let stored = self
            .paywall_log()?
            .load()
            .await
            .map_err(|e| PaywallSetError::GrantLog(e.to_string()))?;
        let walk =
            folder_paywall_generation(&stored.grant_events, self.keypair.secret_bytes(), name);
        Ok((stored, walk))
    }

    /// The wired grant log, or the refusal every paywall leg answers without
    /// one — before any side effect, never a deposit the log omits.
    #[cfg(feature = "mls")]
    fn paywall_log(
        &self,
    ) -> Result<&dyn fauna_client_config::SuccessionLedgerStore, PaywallSetError<R::Error, G::Error>>
    {
        self.grant_log.as_deref().ok_or_else(|| {
            PaywallSetError::GrantLog("no grant log is wired to this folder author".into())
        })
    }

    /// Paywall a `web`-mode folder to a subscription tier — the web-paywall
    /// analogue of [`serve_set`](Self::serve_set) (`monetization.md` § Pillar 2,
    /// the folder half; `web-content-hosting.md` § Sealed static files). The one
    /// shared composition every app drives, so the "content-key → flag → grant"
    /// sequencing is written once (priority #2) rather than per surface. Composes,
    /// in one crash-safe order:
    ///
    /// 1. **Custody-first** — genesis content key for an unshared set (idempotent
    ///    reuse if already keyed); the sync agent's ungated pass re-seals the
    ///    back-catalogue under `current` from that custody. A **shared** set must
    ///    already hold custody from its bind — a second genesis would diverge from
    ///    the envelope members hold, so that is an error, never a silent re-key
    ///    (exactly as [`serve_enable`](Self::serve_enable)).
    /// 2. **Flag ON** (`fauna.folders.set_web_paywall`) — the nest gate that
    ///    makes a sealed `web_files` row serve through the paywall token instead of
    ///    fail-closed 404.
    /// 3. **Mint the grant** (`fauna.capabilities.mint`) — one
    ///    `content.read{folder:set}` grant wrapping **every** content-key
    ///    generation (`epoch = version`) to the web-serve holder
    ///    ([`mint_folder_grant`]), so an entitled visitor's short-lived token
    ///    opens the seal. This is the grant plane replacing the WebDAV blob channel
    ///    as the holder-delivery mechanism (`mls-group-key-material.md` § M2).
    ///
    /// Each crash point leaves the set's exposure **≤ user intent**: a crash after
    /// (1) seals the back-catalogue but leaves the flag off ⇒ those files fail
    /// closed (404), never leak; a crash after (2) paywalls the set with no grant
    /// yet ⇒ the teaser, never the content. A retry is idempotent — genesis reuses,
    /// the flag re-sets, and a fresh grant supersedes (the holder keeps every
    /// candidate; the AEAD tag disambiguates).
    ///
    /// `channel_id` is the set's derived `ChannelId` when it is **shared**
    /// (`FolderSummary::mls_group_id` → `ChannelId::from_group_id`), `None` when
    /// owner-only (the serve pseudo-channel is derived here). `holder_pubkey` /
    /// `holder_mlkem_ek` are the web-serve holder's published X25519 (+ optional
    /// ML-KEM ek for the X-Wing hybrid wrap), which the caller (the FFI / wasm
    /// face) discovers via `fauna.bridges.fetch_bridge_pubkey`
    /// (`"content-processor"` / `"web-serve"`) — mirroring `fauna-client-pair`'s
    /// holder-discovery → mint flow, so this wasm-light crate stays bridges-free.
    ///
    /// Rotation (revoke-and-re-provision) appends the new generation's wrap via
    /// `fauna.capabilities.renew`; that leg is the caller's, keyed off the same
    /// custody the re-seal advances.
    /// Sign `event` as the owner and join it into the grant log; answers the
    /// log as the write stored it.
    #[cfg(feature = "mls")]
    async fn record_paywall_event(
        &self,
        event: fauna_core::grant_event::GrantEvent,
    ) -> Result<fauna_core::succession_ledger::SuccessionLedger, PaywallSetError<R::Error, G::Error>>
    {
        use grant_log::GrantEventSigner as _;
        let signed = grant_log::KeypairGrantEventSigner::new(&self.keypair)
            .sign_grant_event(event)
            .map_err(|e| PaywallSetError::GrantLog(e.to_string()))?;
        self.paywall_log()?
            .merge(
                fauna_core::succession_ledger::SuccessionLedger::events_replica(
                    self.keypair.actor_id(),
                    vec![signed],
                ),
            )
            .await
            .map_err(|e| PaywallSetError::GrantLog(e.to_string()))
    }

    /// Record the paywall grant's `Mint`, then deposit `blob` — the
    /// record-then-deposit order every minting site keeps (`ui/nests.md`
    /// § Trust facet — grants): the blob is released only against the log the
    /// write stored ([`grant_log::UndepositedGrant::release`]), so the grant is
    /// on the owner's facet, and survives its reconcile sweep, before it is
    /// live on the nest.
    #[cfg(feature = "mls")]
    async fn mint_paywall_grant(
        &self,
        blob: &fauna_mls::wrapped_blob::GrantBlob,
        grant_id: [u8; 16],
        holder_pubkey: [u8; 32],
        window: (u64, u64),
        now_secs: u64,
    ) -> Result<(), PaywallSetError<R::Error, G::Error>> {
        let blob_bytes = blob
            .to_canonical_bytes()
            .map_err(|e| PaywallSetError::Mint(MintGrantError::Wrap(e)))?;
        let pending = grant_log::UndepositedGrant::new(grant_id, blob_bytes);
        let stored = self
            .record_paywall_event(grant_log::build_mint_event(
                grant_id,
                holder_pubkey,
                grant_log::event_scope_of(&blob.scope),
                window.0,
                window.1,
                now_secs,
            ))
            .await?;
        let blob_bytes = pending
            .release(&grant_log::RecordedGrants::from_stored(&stored))
            .map_err(|e| PaywallSetError::GrantLog(e.to_string()))?;
        let _: MintGrantReply = self
            .files
            .requester()
            .request(
                "fauna.capabilities.mint",
                MintGrantRequest {
                    grant_blob: fauna_protocol::ByteBuf::from(blob_bytes),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;
        Ok(())
    }

    #[cfg(feature = "mls")]
    pub async fn paywall_set(
        &self,
        name: &str,
        tier: &str,
        channel_id: Option<[u8; 32]>,
        holder_pubkey: [u8; 32],
        holder_mlkem_ek: Option<Vec<u8>>,
    ) -> Result<(), PaywallSetError<R::Error, G::Error>> {
        let custody_key = channel_id.unwrap_or_else(|| serve_custody_channel_id(name));
        // 0. The grant log is readable and names the generation this mint
        //    goes to — checked before any side effect, so a refusal leaves
        //    nothing half-paywalled. A set unpaywalled before mints the next
        //    generation; a retry of a live one replaces it in place.
        let (_, walk) = self.paywall_walk(name).await?;
        let grant_id = walk.grant_id;

        // 1. Custody-first (mirrors `serve_enable`): a shared set must already
        //    hold custody from its bind; a second genesis would fork the members'
        //    envelope.
        if channel_id.is_some() {
            let cfg = self
                .custody
                .load_for_write()
                .await
                .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::CustodyStore(e)))?;
            if custody::content_keys(&cfg, &custody_key).is_none() {
                return Err(PaywallSetError::Paywall(FoldersAuthorError::NoContentKeys(
                    custody_key,
                )));
            }
        }
        let cfg = self
            .update_custody(|cfg| {
                if channel_id.is_none() {
                    custody::key_named_set(
                        cfg,
                        name,
                        custody_key,
                        *fresh_content_key(),
                        Timestamp::now().0,
                    );
                }
            })
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::CustodyStore(e)))?;

        // 2. Flag ON — the nest gate that makes a sealed row serve through the token.
        self.files
            .set_web_paywall(name.to_string(), Some(tier.to_string()))
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;

        // 3. Mint the content.read{folder:set} grant to the web-serve holder,
        // recorded in the owner's grant log first. The grant id is derived
        // (not random) so the rotation/revoke legs can name the same
        // (owner, grant_id) later — see `folder_paywall_grant_id`.
        // The generation is the walk's above.
        let now_secs = Timestamp::now().0 / 1_000_000;
        let window_end = now_secs + DEFAULT_GRANT_WINDOW_SECS;
        let blob = mint_folder_grant(
            &cfg,
            // The grant's creator is the session identity, never an id read
            // off the config (`config-dissolution.md` → *The ledger*).
            &self.keypair.actor_id().0,
            &grant_id,
            &holder_pubkey,
            holder_mlkem_ek.as_deref(),
            GrantWindow(now_secs, window_end),
            name,
            &custody_key,
        )
        .map_err(PaywallSetError::Mint)?;
        self.mint_paywall_grant(
            &blob,
            grant_id,
            holder_pubkey,
            (now_secs, window_end),
            now_secs,
        )
        .await
    }

    /// Re-provision a paywalled set's grant after its content key rotated — the
    /// **rotation leg** of the web-paywall lifecycle (`monetization.md` § Pillar 2
    /// folder half; `mls-group-key-material.md` § M2: *"Rotation appends the new
    /// generation's wrap via `fauna.capabilities.renew`"*). Call it **after** the
    /// content-key generation has advanced (a `custody::rotate_set` /
    /// rotate-on-removal, then the back-catalogue re-seal) so the web-serve holder
    /// gains the new generation's `WrappedScopeKey` and an entitled visitor's fresh
    /// token opens the newest-sealed bytes; without it the holder's grant lacks the
    /// new generation and those bytes darken to the teaser.
    ///
    /// Rebuilds the **full current** generation bundle ([`mint_folder_grant`], one
    /// wrap per generation, `epoch = version`) and sends every wrap in the renew's
    /// `appended_keys`. The nest folds them into the stored grant blob, **deduped by
    /// `(scope, epoch)`** — so re-sending the generations already present is a
    /// harmless no-op and only genuinely-new generations land. This is why the leg
    /// carries no per-generation delta state: the owner cannot read its own grant
    /// back (the nest exposes no owner-facing grant list, `fauna.capabilities.fetch`
    /// is holder-scoped), so it always resends the whole current bundle and lets the
    /// nest's dedup do the diff. Idempotent + crash-safe: a retry re-sends the same
    /// bundle to the same `(owner, grant_id)`; the renew also refreshes the grant
    /// window, so it doubles as a keep-alive.
    ///
    /// `grant_id` is the **derived** `folder_paywall_grant_id` the original
    /// [`paywall_set`](Self::paywall_set) stamped — found here by the generation
    /// walk over the owner's grant log (owner secret + set name + the live
    /// generation), so no grant-id state is threaded across the mint→rotate
    /// boundary. `channel_id` / `holder_pubkey` / `holder_mlkem_ek` mirror
    /// [`paywall_set`](Self::paywall_set): the shared set's `ChannelId` or `None`
    /// for owner-only, and the web-serve holder the FFI / wasm face discovers via
    /// `fauna.bridges.fetch_bridge_pubkey`. The appended wraps must be sealed to the
    /// **same** holder the grant already targets (unchanged for a stable service-user
    /// identity), else the holder cannot unseal them.
    #[cfg(feature = "mls")]
    pub async fn rotate_paywall_grant(
        &self,
        name: &str,
        channel_id: Option<[u8; 32]>,
        holder_pubkey: [u8; 32],
        holder_mlkem_ek: Option<Vec<u8>>,
    ) -> Result<(), PaywallSetError<R::Error, G::Error>> {
        let custody_key = channel_id.unwrap_or_else(|| serve_custody_channel_id(name));
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::CustodyStore(e)))?;
        let (ledger, walk) = self.paywall_walk(name).await?;
        let grant_id = walk.grant_id;
        let now_secs = Timestamp::now().0 / 1_000_000;
        let new_epoch_end = now_secs + DEFAULT_GRANT_WINDOW_SECS;
        // Rebuild the full current bundle; the nest dedups (scope, epoch), so only
        // generations newer than the stored grant actually land.
        let blob = mint_folder_grant(
            &cfg,
            // The grant's creator is the session identity, never an id read
            // off the config (`config-dissolution.md` → *The ledger*).
            &self.keypair.actor_id().0,
            &grant_id,
            &holder_pubkey,
            holder_mlkem_ek.as_deref(),
            GrantWindow(now_secs, new_epoch_end),
            name,
            &custody_key,
        )
        .map_err(PaywallSetError::Mint)?;
        // No live generation for a set still paywalled: a grant deposited
        // before the paywall legs recorded (unlogged, so the trust facet's
        // reconcile sweep ends it), or one revoked from the Nests page. Heal
        // by minting it whole under the walk's fresh generation, recorded
        // first like any mint.
        let Some(current) = walk.live else {
            return self
                .mint_paywall_grant(
                    &blob,
                    grant_id,
                    holder_pubkey,
                    (now_secs, new_epoch_end),
                    now_secs,
                )
                .await;
        };
        // The renew widens the window, so the log records it first. The start
        // stays where the mint put it, as on the nest (`new_epoch_start: None`).
        let renew = grant_log::build_renew_event(
            &ledger,
            &grant_id,
            current.window_start,
            new_epoch_end,
            now_secs,
        )
        .map_err(|e| PaywallSetError::GrantLog(e.to_string()))?;
        self.record_paywall_event(renew).await?;
        crate::principal_grants::renew_with_bundle(
            self.files.requester(),
            &blob,
            grant_id,
            new_epoch_end,
        )
        .await
        .map_err(|e| match e {
            crate::principal_grants::RenewFailure::Mint(e) => PaywallSetError::Mint(e),
            crate::principal_grants::RenewFailure::Transport(e) => {
                PaywallSetError::Paywall(FoldersAuthorError::Transport(e))
            }
        })
    }

    /// Discover the nest's **web-serve holder** — the paywall grant's HPKE seal
    /// target — via `fauna.bridges.fetch_bridge_pubkey` for role
    /// `content-processor` + bridge id `web-serve` (`WebServeHolder`'s enrollment,
    /// `bins/fauna-nest/src/web_content/holder.rs`). Returns its X25519 pubkey
    /// (+ the optional ML-KEM ek for the X-Wing hybrid wrap). The single home of
    /// the discovery (priority #2): the FFI / wasm faces and the automatic
    /// re-provision paths ([`Self::renew_paywalled_grants`],
    /// [`Self::finish_rotation`]'s hook) all resolve the holder here. A nest with
    /// no enrolled web-serve holder surfaces `fauna.bridges.not_found` as a
    /// `Transport` rejection.
    #[cfg(feature = "mls")]
    pub async fn discover_web_serve_holder(
        &self,
    ) -> Result<([u8; 32], Option<Vec<u8>>), PaywallSetError<R::Error, G::Error>> {
        let holder: FetchBridgePubkeyReply = self
            .files
            .requester()
            .request(
                "fauna.bridges.fetch_bridge_pubkey",
                FetchBridgePubkeyRequest {
                    bridge_role: "content-processor".to_string(),
                    bridge_id: "web-serve".to_string(),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;
        let holder_pubkey: [u8; 32] = holder.x25519_pubkey.as_slice().try_into().map_err(|_| {
            PaywallSetError::Mint(MintGrantError::Wrap(WrapError::InvalidInput(format!(
                "web-serve holder returned a malformed X25519 pubkey ({} bytes, want 32)",
                holder.x25519_pubkey.len()
            ))))
        })?;
        Ok((holder_pubkey, holder.mlkem_ek.map(|ek| ek.into_vec())))
    }

    /// Re-provision **every owned paywalled set's** grant — the launch-time
    /// convergence + keep-alive pass (`monetization.md` § Pillar 2: *"rotation
    /// re-provisions the grant"*; the automatic caller of
    /// [`Self::rotate_paywall_grant`]). Two jobs in one idempotent sweep:
    ///
    /// 1. **Retry a lost inline re-provision** — [`Self::finish_rotation`] renews
    ///    best-effort after a rotate-on-removal commits; a crash / offline nest in
    ///    that window leaves the web-serve holder's grant without the new
    ///    generation's wrap (fresh-sealed content serves as the teaser to entitled
    ///    visitors) until this pass re-sends the bundle.
    /// 2. **Keep-alive** — the grant window is finite
    ///    (`DEFAULT_GRANT_WINDOW_SECS`, 90 days) and the renew's window bump is
    ///    the only extension, so without a periodic caller every paywalled set
    ///    silently darkens when its mint-time window lapses. Running this at
    ///    every app launch (it rides [`Self::resume_pending_removals`], which
    ///    all apps call at startup) keeps any actively-used deployment renewed.
    ///
    /// Reads the owner's sets (`fauna.folders.list`), filters
    /// `web_paywall_tier.is_some()` (skipping `role == "member"` rows — only the
    /// owner can renew its own derived grant), discovers the holder **once**, and
    /// renews each set under its custody key (`ChannelId::from_group_id` when
    /// bound, else the serve pseudo-channel). Cheap in steady state: no paywalled
    /// sets ⇒ one list read, nothing else. Per-entry error isolation (the
    /// [`Self::resume_pending_removals`] posture): one set's failed renew never
    /// starves the others; the first error is surfaced after the full pass.
    /// Returns the number of grants renewed.
    ///
    /// No `web_paywall_tier` reported (an unpaywalled folder) ⇒ clean no-op. The
    /// crashed-`paywall_set` edge (tier flag ON, mint never ran) makes the renew
    /// a `not_found` rejection — surfaced here, retried next launch; re-driving
    /// [`Self::paywall_set`] heals it (its mint is `INSERT OR REPLACE` on the
    /// same derived id).
    #[cfg(feature = "mls")]
    pub async fn renew_paywalled_grants(
        &self,
    ) -> Result<usize, PaywallSetError<R::Error, G::Error>> {
        let listed = self
            .files
            .list_wire()
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;
        let paywalled: Vec<_> = self
            .named_from_custody(listed.folders)
            .await
            .into_iter()
            .filter(|s| s.web_paywall_tier.is_some() && s.role.as_deref() != Some("member"))
            .collect();
        if paywalled.is_empty() {
            return Ok(0);
        }
        let (holder_pubkey, holder_mlkem_ek) = self.discover_web_serve_holder().await?;
        let mut renewed = 0usize;
        let mut first_err: Option<PaywallSetError<R::Error, G::Error>> = None;
        for set in &paywalled {
            let channel_id = set
                .mls_group_id
                .as_deref()
                .and_then(|h| hex::decode(h).ok())
                .map(|raw| fauna_mls::types::ChannelId::from_group_id(&raw).0);
            match self
                .rotate_paywall_grant(
                    &set.name,
                    channel_id,
                    holder_pubkey,
                    holder_mlkem_ek.clone(),
                )
                .await
            {
                Ok(()) => renewed += 1,
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(renewed),
        }
    }

    /// The **inline** automatic re-provision — called by [`Self::finish_rotation`]
    /// right after a rotate-on-removal commits: if the set is paywalled
    /// (`web_paywall_tier` on its `fauna.folders.list` row), discover the
    /// web-serve holder and [`Self::rotate_paywall_grant`] so the grant gains the
    /// new generation's wrap before the next visitor request. Returns whether a
    /// renew was sent. Errors propagate to the caller, which treats the whole
    /// call as best-effort (the removal — the security half — is already
    /// committed; availability convergence is retried by the launch-time
    /// [`Self::renew_paywalled_grants`] pass).
    #[cfg(feature = "mls")]
    async fn renew_paywall_grant_if_paywalled(
        &self,
        name: &str,
        channel_id: Option<[u8; 32]>,
    ) -> Result<bool, PaywallSetError<R::Error, G::Error>> {
        // Unrendered and matched by hash: this author's client holds no label
        // custody, and a sealed set's row rests no plaintext name (schema 114).
        let listed = self
            .files
            .list_wire()
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;
        let paywalled = listed.folders.iter().any(|s| {
            s.is_named(name) && s.web_paywall_tier.is_some() && s.role.as_deref() != Some("member")
        });
        if !paywalled {
            return Ok(false);
        }
        let (holder_pubkey, holder_mlkem_ek) = self.discover_web_serve_holder().await?;
        self.rotate_paywall_grant(name, channel_id, holder_pubkey, holder_mlkem_ek)
            .await?;
        Ok(true)
    }

    /// Un-paywall a `web`-mode folder — the **revoke leg**, the inverse of
    /// [`paywall_set`](Self::paywall_set) and the shared-Rust core the client's
    /// "clear the paywall" control drives (`monetization.md` § Pillar 2:
    /// *"revoking a grant darkens exactly that slice"*). Two nest calls in a
    /// crash-safe order:
    ///
    /// 1. **Revoke the grant** (`fauna.capabilities.revoke` on the derived
    ///    `grant_id`) — the web-serve holder's next registry fetch zeroizes the
    ///    key, so the sealed bytes darken to the teaser at use time (honest-box
    ///    revocation).
    /// 2. **Record the `Revoke`** in the owner's grant log — after the nest's,
    ///    since revoke narrows (`ui/nests.md` § Trust facet — grants).
    /// 3. **Clear the nest tier flag** (`fauna.folders.set_web_paywall(None)`) —
    ///    a sealed `web_files` row with no tier is not served at all (404), the
    ///    fail-closed rule.
    ///
    /// Revoke-first so every crash-intermediate is **≤ darkened**: after (1) the
    /// slice is already dark (teaser); a crash before (2) leaves the log showing
    /// a grant the nest no longer holds (a row the user can clear), and before
    /// (3) the flag on but the grant gone, never leaking content. A retry is
    /// idempotent — revoke of an absent grant is a no-op `ok`, a revoked grant
    /// records nothing more, and the flag re-clears. The files stay **sealed at
    /// rest** (un-paywalling darkens; making them public again is a separate
    /// re-seal-to-plaintext flow, not "clear = revoke").
    ///
    /// The stronger admin-proof-er posture — rotating the content key on top so
    /// even a compromised holder cannot open the pre-revoke bytes — is a
    /// `custody::rotate_set` after this; the honest-box revoke here is the
    /// darkening contract the goal doc names.
    #[cfg(feature = "mls")]
    pub async fn unpaywall_set(
        &self,
        name: &str,
    ) -> Result<(), PaywallSetError<R::Error, G::Error>> {
        // The live generation's id when one is live; otherwise the walk's
        // fresh id, whose nest revoke is a no-op `ok` (record-then-deposit
        // means no grant rests under an id the log never minted).
        let (_, walk) = self.paywall_walk(name).await?;
        let grant_id = walk.grant_id;
        // 1. Revoke the grant — darkens the sealed slice at the holder's next fetch.
        let _: RevokeGrantReply = self
            .files
            .requester()
            .request(
                "fauna.capabilities.revoke",
                RevokeGrantRequest {
                    grant_id: fauna_protocol::ByteBuf::from(grant_id.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;
        // 2. Record the `Revoke` of the live generation, which spends it: the
        //    next paywall walks on to a fresh id. Nothing live, nothing to
        //    record.
        if let Some(current) = walk.live {
            let holder = <[u8; 32]>::try_from(current.holder.as_slice())
                .map_err(|_| PaywallSetError::GrantLog("a grant holder is not 32 bytes".into()))?;
            let now_secs = Timestamp::now().0 / 1_000_000;
            self.record_paywall_event(grant_log::build_revoke_event(grant_id, holder, now_secs))
                .await?;
        }
        // 3. Clear the nest tier flag — sealed rows with no tier fail closed (404).
        self.files
            .set_web_paywall(name.to_string(), None)
            .await
            .map_err(|e| PaywallSetError::Paywall(FoldersAuthorError::Transport(e)))?;
        Ok(())
    }

    // An audience transition needs NO author-side orchestration — deleted, on
    // purpose, with the remedy. Every direction, the bound `→shared`
    // flip-back included, is a keyless `fauna.folders.update` through
    // `FoldersClient::set_audience`: the projected audience itself is the
    // cross-device signal, and `SyncEngine::converge_corpus_to_audience`
    // dispatches the sealed direction on bound-ness so it converges on every
    // seat — the members' engines a per-actor custody sentinel could never
    // reach. An earlier `set_audience(name, audience, channel_id)` here staged
    // the (since retired) pre-bind re-seal sentinel for `→shared`; no production
    // caller ever passed the channel, and re-adding a second signal is exactly
    // what phase 4's no-sentinel ruling refuses. Do not resurrect it.

    /// Flip the folder's `website_enabled` flag (`fauna.folders.update` — the
    /// phase-4 slice-1 setter; schema v41, `DEFAULT 0`).
    ///
    /// **This is the only way to make a website folder.** Phase 2 slice e retired
    /// the create wizard's mode step — a folder has no type — which left no door
    /// at all between then and this one (`folders.md` § Implementation status
    /// today records that gap as accepted, and explicitly not to be patched by
    /// re-adding a mode control). A website folder is now an ordinary folder with
    /// this flag on.
    ///
    /// Serving is a *fan-out of the head*, not a separate plane: the nest re-keyed
    /// the former `mode == "web"` fan-out to this flag at both ingest rails. The
    /// flag is orthogonal to audience — it says "publish this folder's head", while
    /// the audience says who may read what is published. Turning it on for a folder
    /// that is neither `public` nor paywalled is *allowed and inert*: nothing is
    /// readable until one of those lands, which is why the UI hints rather than
    /// refuses (`ui/folders.md` § Audience and website serving).
    ///
    /// The cross-toggle refusals live nest-side and are about audience, never this
    /// flag: WebDAV-serve ⊕ public and paywall ⊕ public, each refused whichever
    /// side moves second (`folders.md` § Target re-model).
    /// Delegates to [`crate::FoldersClient::set_website_enabled`] — the request
    /// shape has ONE owner, and it is the keyless client, since nothing here
    /// needs the author's key material. This face exists so a caller already
    /// holding an author does not have to reach past it.
    pub async fn set_website_enabled(&self, name: &str, enabled: bool) -> AuthorResult<(), R, G> {
        self.files
            .set_website_enabled(name, enabled)
            .await
            .map_err(FoldersAuthorError::Transport)?;
        Ok(())
    }

    /// Flip the nest per-set `webdav_enabled` flag (`fauna.folders.update` —
    /// the slice-1a-gated setter).
    async fn set_webdav_flag(&self, name: &str, enabled: bool) -> AuthorResult<(), R, G> {
        self.files
            .update(fauna_protocol::folders::FolderUpdateRequest {
                name: name.to_string(),
                webdav_enabled: Some(enabled),
                // Struct-update, not a hand-listed literal (the repo's
                // fixture-shape convention): this request type keeps growing
                // "leave unchanged" fields, and every one of them is `None` here.
                ..Default::default()
            })
            .await
            .map_err(FoldersAuthorError::Transport)?;
        Ok(())
    }

    /// **Converge every owned bound set's published envelope** on what custody
    /// says now — the launch pass's envelope reconcile
    /// (`writer-signed-change-records.md` ruling (11)(a)/(b) and ruling
    /// (7)(b)(ii) rule (3)): re-read what the nest stores, open it at the
    /// current epoch, and **publish the JOIN of custody and the envelope, and
    /// only when custody is strictly ahead** ([`custody::envelope_join`] — a
    /// generation, a serve stamp, a nonce, a minter or a lineage nonce the
    /// envelope lacks), never a stale bundle over a newer one, which a
    /// lagging device would otherwise push. So a re-mint, a rotation or a
    /// serve flip reaches members even when a crash fell between the custody
    /// write and the publish.
    ///
    /// Ruling (11)'s two re-seal arms stand beside it, since neither envelope
    /// is one a member can use: an envelope **not signed by this identity**
    /// (a predecessor's, after a succession) and one **sealed at an epoch
    /// behind the group's** are re-published — as the join when they still
    /// open, from custody when they do not. An absent envelope is published.
    ///
    /// Two skips: a channel with a **staged pending removal** (its Remove may
    /// be merged and its generation unpublished — publishing custody's keys
    /// under the post-Remove epoch would hand the departing member the
    /// window; the removal's own drive publishes), and an envelope of this
    /// identity's at the current epoch that **does not open** (nothing is
    /// published on a failed read). An envelope that names custody's live
    /// nonce as retired is left alone: this device lags a cut.
    ///
    /// Custody is read as a write reads it (a fresh fetch). Best-effort per
    /// set: one set's failure is logged, never another's. Returns how many
    /// envelopes were re-published.
    pub async fn converge_envelopes(&self) -> AuthorResult<usize, R, G> {
        let cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        let identity = self.keypair.actor_id();
        let mut republished = 0;
        for (name, channel) in custody::owned_bound_sets(&cfg) {
            if !self.group.holds_group(&channel).unwrap_or(false)
                || cfg.pending_removals.iter().any(|r| r.channel_id == channel)
            {
                continue;
            }
            let Some(keys) = custody::content_keys(&cfg, &channel) else {
                continue;
            };
            let expected = custody::envelope_payload(&cfg, Some(&name), &channel, keys);
            let current = self.group.envelope_epoch(&channel).ok();
            let stored = self
                .files
                .content_key_get(fauna_protocol::folders::ContentKeyGetRequest {
                    name: name.clone(),
                    ..Default::default()
                })
                .await;
            let publish = match stored {
                Ok(reply) => {
                    let signed = hex::decode(reply.sealed.trim()).ok().and_then(|blob| {
                        fauna_protocol::folder_envelope_sig::verify(&blob, &channel).ok()
                    });
                    // Ruling (11): an envelope a member cannot use is re-sealed.
                    let reseal = current.is_some_and(|c| (reply.epoch as u64) < c)
                        || !signed.as_ref().is_some_and(|v| v.signer == identity);
                    match signed.and_then(|v| self.group.open_envelope(&channel, &v.sealed).ok()) {
                        Some(published) => custody::envelope_join(&published, &expected)
                            .and_then(|(joined, ahead)| (ahead || reseal).then_some(joined)),
                        None => reseal.then_some(expected),
                    }
                }
                // Answered: nothing published — publish. A transport fault
                // decides nothing.
                Err(e) if e.is_rejection() => Some(expected),
                Err(_) => None,
            };
            let Some(payload) = publish else {
                continue;
            };
            match self.publish_payload(&name, &channel, &payload).await {
                Ok(_) => republished += 1,
                Err(e) => tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&name),
                    "folders: re-publishing a stale content-key envelope failed (retried at the \
                     next launch): {e}"
                ),
            }
        }
        Ok(republished)
    }

    /// **The succession's content-key rotation** (`succession-aftermath.md`
    /// § Re-key scope, the MLS groups row): every owned bound set whose current
    /// generation predates the succession cut
    /// ([`custody::sets_owing_succession_rotation`]) gets a fresh generation —
    /// committed in the custody write that mints it, as the serve-disable
    /// rotation is ([`custody::rotate_set`]) — and its envelope published
    /// under the group's current epoch, so the custody a retired seed holds
    /// opens nothing sealed after it.
    ///
    /// **Only once remove-old has landed on this engine's group**: a set whose
    /// group this engine does not hold, or still seats a predecessor's leaf (a
    /// sweep that has not reached the group yet), is left for a later pass —
    /// an envelope sealed at an epoch the retired leaf holds would hand it the
    /// new generation. A set with a staged member removal is left to that
    /// removal's own rotation first. A publish that fails after the custody
    /// write is [`Self::converge_envelopes`]'s to finish. Returns how many
    /// sets were rotated.
    pub async fn rotate_succeeded_sets(&self) -> AuthorResult<usize, R, G> {
        let cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        let identity = self.keypair.actor_id();
        let mut rotated = 0;
        for owed in custody::sets_owing_succession_rotation(&cfg, &identity) {
            let channel = owed.channel_id;
            if !self.group.holds_group(&channel).unwrap_or(false)
                || cfg.pending_removals.iter().any(|r| r.channel_id == channel)
                // Unreadable membership decides nothing — the leaf may be there.
                || owed
                    .predecessors
                    .iter()
                    .any(|p| self.group.contains_member(&channel, p).unwrap_or(true))
            {
                continue;
            }
            let fresh = fresh_content_key();
            let now = Timestamp::now().0.max(owed.cut_at);
            let mut minted = false;
            let cfg = self
                .update_custody(|cfg| {
                    // Re-checked against the custody the write starts from: a
                    // peer device's rotation may have merged in since the read.
                    if custody::sets_owing_succession_rotation(cfg, &identity)
                        .iter()
                        .any(|o| o.channel_id == channel)
                    {
                        minted = custody::rotate_set(cfg, &channel, *fresh, now).is_some();
                    }
                })
                .await
                .map_err(FoldersAuthorError::CustodyStore)?;
            if !minted {
                continue;
            }
            rotated += 1;
            if let Err(e) = self.publish_envelope(&cfg, &owed.name, &channel).await {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(&owed.name),
                    "folders: publishing the succession's rotated content-key envelope failed \
                     (the convergence pass retries): {e}"
                );
            }
        }
        Ok(rotated)
    }

    /// Re-publish the envelope (same generations, no rotation) under the group's
    /// current epoch — what a member *add* needs so the new joiner (at the new
    /// epoch) can read the full back-catalogue (history-on-join). Adding a member
    /// does not break forward secrecy, so there is no rotation.
    pub async fn republish_envelope(
        &self,
        name: &str,
        channel_id: [u8; 32],
    ) -> AuthorResult<(), R, G> {
        let cfg = self
            .custody
            .load()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        self.publish_envelope(&cfg, name, &channel_id).await?;
        Ok(())
    }

    /// Remove a member from a shared set: MLS Remove (epoch advances) → rotate to a
    /// fresh content key → re-publish the envelope under the new epoch → evict the
    /// member from the nest roster → commit the rotation into custody.
    ///
    /// Crash-safe: the fresh key is **staged + persisted before** the publish and
    /// committed only on a confirmed publish (see the module docs). Resumes an
    /// already-staged removal for this `(channel_id, member)` rather than
    /// re-rotating. A genuine no-op (the member isn't in the group and nothing is
    /// staged) returns early without rotating.
    pub async fn remove_member(
        &self,
        name: &str,
        channel_id: [u8; 32],
        member: ActorId,
    ) -> AuthorResult<RemoveOutcome, R, G> {
        let mut cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;

        let removal = match custody::find_pending_removal(&cfg, &channel_id, &member) {
            // A sentinel is already staged for this `(channel, member)` — an
            // interrupted earlier attempt, not a fresh removal. Clicking Remove
            // again resumes it; whether a rebuild is safe is answered by the
            // sentinel itself (its bytes, or its `gated_attempted` stamp — see
            // [`Self::drive_removal`]), never by rebuilding blind.
            Some(existing) => existing.clone(),
            None => {
                // Nothing staged: skip a pointless rotation if the member is not in
                // the group (mirrors `SubscriptionsAuthor::remove_subscriber`'s
                // absent-subscriber no-op, but checked locally against the
                // client-side MLS group rather than a nest roster read).
                if !self
                    .group
                    .contains_member(&channel_id, &member)
                    .map_err(FoldersAuthorError::Group)?
                {
                    return Ok(RemoveOutcome {
                        commit: None,
                        evicted: false,
                        rotated: false,
                    });
                }
                let current = custody::current_generation(&cfg, &channel_id)
                    .ok_or(FoldersAuthorError::NoContentKeys(channel_id))?;
                let removal = FolderPendingRemoval {
                    channel_id,
                    name: name.to_string(),
                    removed_member: member,
                    new_generation: ContentKeyGeneration {
                        version: current.version + 1,
                        key: fresh_content_key().into(),
                        rotated_at: next_rotated_at(current.rotated_at),
                    },
                    // The Remove commit doesn't exist yet — `drive_removal`
                    // records it onto this sentinel before the channel send.
                    commit: None,
                    // No engaged gated attempt has run for this sentinel (it is
                    // being created right here). `false` is what makes a later
                    // byte-less resume on a never-engaged gate provably safe to
                    // rebuild instead of deferring forever; the drive's gated
                    // leg stamps it `true` before the gate can distribute
                    // anything.
                    gated_attempted: false,
                };
                // Persist the fresh (irrecoverable) key BEFORE any network
                // publish (crash-safety) — cross-device-safe via CAS + merge so a
                // peer's concurrent staged key is never dropped.
                custody::stage_pending_removal(&mut cfg, removal.clone());
                self.custody
                    .merge(cfg.clone())
                    .await
                    .map_err(FoldersAuthorError::CustodyStore)?;
                removal
            }
        };

        self.drive_removal(removal).await
    }

    /// Re-drive every staged member-removal whose publish was interrupted by a
    /// crash. Call on client startup (and after a config sync that may have merged
    /// a peer device's staging). Each is completed idempotently. Returns the count
    /// successfully resumed.
    ///
    /// **Byted sentinels resume first**: a sentinel carrying
    /// durable commit bytes may hold the engine's restored staged pending as its
    /// merge source, and a byte-less sibling's fresh build defers while one
    /// exists on its channel (see [`Self::drive_removal_commit_ungated`]) — so
    /// FIFO order would deadlock the loop on the deferral error. Driving byted
    /// sentinels first completes (and clears) them, unblocking the siblings.
    ///
    /// **Per-entry error isolation**: a single sentinel that
    /// cannot complete this pass — a *poisoned* one
    /// ([`FoldersAuthorError::PoisonedRemovalSentinel`], whose pending is gone
    /// and never returns), or a byte-less sibling that defers because a poisoned
    /// byted sibling still holds the channel — must **not** starve the healthy
    /// removals on other channels. Since byted sentinels are selected first, a
    /// `?`-abort on the first error would strand every later removal behind an
    /// unrecoverable one, keeping a member the user removed in the group with the
    /// live content key (violating the no-unrecoverable-state invariant). So this records each
    /// failing `(channel, member)` and **continues** to the next sentinel instead
    /// of aborting; a failed entry stays staged for a later retry or for
    /// [`Self::recover_poisoned_removal`], but never blocks a healthy sibling.
    ///
    /// A failed set is bounded by the (finite) sentinel list and never reselected
    /// this pass, so the loop always terminates. A config-load failure is still
    /// fatal (nothing can proceed without the config) and propagates immediately.
    /// Every *drive* failure is isolated so the other sentinels get their pass;
    /// the first such error is surfaced once they have (a caller still learns a
    /// removal is stuck, but only after the healthy ones completed).
    pub async fn resume_pending_removals(&self) -> AuthorResult<usize, R, G> {
        let mut count = 0;
        // `(channel, member)` of sentinels that errored this pass — never
        // reselected, so one that can't complete can neither starve the others
        // nor spin the loop forever (byted-first would otherwise keep picking a
        // poisoned entry every iteration).
        let mut failed: Vec<([u8; 32], ActorId)> = Vec::new();
        // The first drive error — surfaced only after every sentinel had its
        // pass, so a stuck removal stays observable without aborting the healthy
        // ones ahead of it in byted-first order.
        let mut first_error = None;
        loop {
            // Read as a write does: the resume is a custody writer (each drive
            // ends in a settle), and its launch trigger fires when the MLS
            // restore has wired the gate — which on any seat can be before the
            // account runtime has assembled. A plain read would answer "not
            // running" and skip the recovery until a launch that wins the race.
            // The wait is why a launch trigger never awaits this in the path
            // of its receive loop while custody is unreadable
            // (`launch_resume`'s module doc).
            let cfg = self
                .custody
                .load_for_write()
                .await
                .map_err(FoldersAuthorError::CustodyStore)?;
            let pending = &cfg.pending_removals;
            let untried = |r: &&FolderPendingRemoval| {
                !failed
                    .iter()
                    .any(|(c, m)| c == &r.channel_id && m == &r.removed_member)
            };
            let removal = match pending
                .iter()
                .filter(untried)
                .find(|r| r.commit.is_some())
                .or_else(|| pending.iter().find(untried))
            {
                Some(r) => r.clone(),
                None => break,
            };
            let outcome = match self.drive_removal(removal.clone()).await {
                // A poisoned sentinel can't complete through a plain drive — but it
                // is not a permanent brick. Escalate to `recover_poisoned_removal`,
                // which routes through the gate's walk-to-head to heal a
                // distributed commit or safely re-stage. On the real (gated)
                // planes this discharges the poison automatically on launch — the
                // client-reachable recovery the invariant requires, with no user
                // step. If no gate is engaged yet, recovery defers (retryable next
                // resume). This never re-poisons: recovery clears the sentinel on
                // success, or leaves it staged on a deferral.
                Err(FoldersAuthorError::PoisonedRemovalSentinel { channel_id, member }) => {
                    self.recover_poisoned_removal(channel_id, member).await
                }
                other => other,
            };
            match outcome {
                Ok(_) => count += 1,
                // Isolate: record the entry and move on. It stays staged — a
                // transient gate deferral retries on the next resume.
                Err(e) => {
                    failed.push((removal.channel_id, removal.removed_member));
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        // Launch-time paywall-grant convergence + keep-alive: re-provision every
        // owned paywalled set's grant (retries a renew a crashed removal lost,
        // and bumps the finite grant window so an actively-used deployment never
        // silently darkens at `DEFAULT_GRANT_WINDOW_SECS`). Best-effort here —
        // this method's contract is the removal resume; a caller that wants the
        // renew result calls [`Self::renew_paywalled_grants`] directly.
        #[cfg(feature = "mls")]
        let _ = self.renew_paywalled_grants().await;

        // Launch-time set-custody reconcile (the set-nonce ruling (g)): heal a
        // delete that crashed between its custody retire and the nest call,
        // settle a same-name create race, and bring the nest's nonce copy in
        // line with custody. Best-effort for the same reason as the renew.
        match crate::set_lifecycle::reconcile_set_custody(
            &self.files,
            &*self.custody,
            self.keypair.actor_id(),
        )
        .await
        {
            Ok(report) if report != Default::default() => {
                tracing::info!(?report, "folders: set custody reconciled at launch")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("folders: set custody reconcile failed at launch: {e}"),
        }
        // The succession's content-key rotation, after the cut the reconcile
        // just ran and before the envelopes converge on what custody says.
        match self.rotate_succeeded_sets().await {
            Ok(0) => {}
            Ok(rotated) => tracing::info!(
                rotated,
                "folders: the succession rotated the inherited bound sets' content keys"
            ),
            Err(e) => {
                tracing::warn!("folders: the succession's content-key rotation failed: {e}")
            }
        }
        // The served state (ruling (7)(b)(ii) rule (3)), after the custody
        // reconcile, its re-mint and the succession's rotation settled the
        // entries the stamps rest on, and before the two provisioning
        // reconciles: a set custody calls served that the nest holds off is
        // unserved in custody — only ever in the ≤-intent direction.
        match self.unserve_sets_the_nest_holds_off().await {
            Ok(0) => {}
            Ok(unserved) => tracing::info!(
                unserved,
                "folders: sets the nest holds off were unserved in custody at launch"
            ),
            Err(e) => tracing::warn!("folders: the launch pass's unserve arm failed: {e}"),
        }
        // The first provisioned copy brought to custody: the `WebdavKeysBlob`,
        // unconditionally on an MSEK-holding device.
        #[cfg(feature = "mls")]
        if let Err(e) = self.reprovision_webdav_keys().await {
            tracing::warn!("folders: the WebDAV keys re-provision failed at launch: {e}");
        }
        // The second: every owned bound set's envelope after the custody it
        // carries settled (ruling (11)(a)/(b)) — the re-mint's re-publish, a
        // rotation's or a serve flip's, their crash recovery, and an envelope
        // a retried sweep left behind the epoch.
        match self.converge_envelopes().await {
            Ok(0) => {}
            Ok(republished) => {
                tracing::info!(
                    republished,
                    "folders: content-key envelopes converged at launch"
                )
            }
            Err(e) => tracing::warn!("folders: envelope convergence failed at launch: {e}"),
        }

        // Launch-time served-set walk (`webdav-server.md` § Key model (c)):
        // finish a pre-serve re-seal a crash or not-yet-synced custody
        // interrupted. After the custody reconcile, so a healed set is walked
        // under its settled custody. Best-effort for the same reason as the
        // renew.
        match self.converge_served_sets().await {
            Ok(tally) if tally != Default::default() => tracing::info!(
                resealed = tally.resealed,
                skipped = tally.skipped,
                failed = tally.failed,
                "served-set walk: resumed at launch"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!("folders: served-set walk resume failed at launch: {e}"),
        }

        match first_error {
            Some(e) => Err(e),
            None => Ok(count),
        }
    }

    /// The rotate→publish→evict→commit core shared by [`Self::remove_member`] and
    /// [`Self::resume_pending_removals`]. Re-reads the folder-key custody fresh
    /// for both the pre-publish custody read and the post-publish commit, whose
    /// writes are per-row joins at the plane door, so the commit converges with
    /// any concurrent peer-device write rather than overwriting it (the fresh
    /// read already carries the staged sentinel that was persisted before the
    /// publish).
    ///
    /// **The Remove commit obeys Rule 1** (`devices.md` § Cross-device MLS
    /// group-state sync: never merge an epoch-advancing commit until its bytes —
    /// or the staged pending that produces them — are durable somewhere the
    /// restart path will find them) by one of two routes:
    ///
    /// - **Gated** ([`FolderCommitGate`] wired and a `CommitGate` injected):
    ///   the device-owned-epoch rebase loop stages the commit, CAS-puts the
    ///   provider replica (a durable, identity-stamped staged pending),
    ///   gate-sends under `expect_no_commit_since`, and merges only on accept.
    ///   Send-before-merge means a crash on either side of the send converges
    ///   via the replica + the shared own-leaf resync arm; the sentinel carries
    ///   no commit bytes on this route.
    /// - **Ungated fallback**: stage → persist the staged pending locally →
    ///   record the bytes on the sentinel (CAS) → merge → persist → send. On
    ///   resume, durable sentinel bytes are **preferred and never rebuilt
    ///   over** — the interrupted commit may already have been distributed, and
    ///   rebuilding a second commit for the same transition is the permanent
    ///   fork.
    ///
    /// **Durable sentinel bytes outrank the gate**: a byted sentinel is always
    /// resumed through the ungated leg, *without* consulting the gate. Bytes are
    /// only ever recorded by the ungated route, whose merge precedes its send —
    /// so they may describe a **merged-but-undistributed** commit (a plain
    /// failed send reaches this, no crash needed). The gate would classify that
    /// engine state as [`GatedRemoval::AlreadyAbsent`] (the member is off the
    /// merged leaf) and complete rotation-only — silently stranding every
    /// remaining member at the pre-removal epoch. The ungated resume leg
    /// re-sends the bytes verbatim instead (a duplicate append is quiet-skipped
    /// as past-epoch).
    ///
    /// **A byte-less sentinel is NOT proof that nothing was distributed**.
    /// Bytes are recorded only by the *ungated* route; the **gated** route
    /// distributes its commit inside the rebase loop and records none. So when
    /// the gate is wired but not engaged ([`GatedRemoval::GateNotEngaged`])
    /// there is no walk-to-head to ask the log what happened — the sentinel's
    /// durable `gated_attempted` stamp answers it instead: the gated
    /// leg below stamps `true` BEFORE the gate can distribute anything,
    /// so a byte-less sentinel still stamped `false` is **provably
    /// undistributed** and drives ungated — the single-device fallback, and on
    /// a nest whose `fauna.mls` plane never engages the only way the removal
    /// can ever complete. A sentinel stamped `true` **defers retryably**
    /// rather than rebuild blind and risk forking the group.
    ///
    /// The stamp answers *"was anything distributed for **this sentinel**?"*.
    /// It is NOT proof that no removal for this `(channel, member)` was ever
    /// distributed by a **peer device**: the stamp rides the synced config, so
    /// a peer's engaged attempt is visible only after its config write syncs —
    /// and the ungated leg's freshness guard is a *local* group-membership
    /// check, which a stale device passes. That staleness window is the
    /// accepted cost of the single-device fallback (it predates the stamp; the
    /// gate's walk-to-head is the only closure for it), stated here so nobody
    /// leans on the stamp as a cross-device safety proof.
    async fn drive_removal(
        &self,
        removal: FolderPendingRemoval,
    ) -> AuthorResult<RemoveOutcome, R, G> {
        let gated = if removal.commit.is_some() {
            // Durable bytes exist ⇒ ungated resume leg, gate not consulted
            // (see the doc above). `removal` is cloned from a freshly-loaded
            // config by both callers, so `commit` reflects the durable sentinel.
            GatedRemoval::NoGate
        } else {
            match &self.commit_gate {
                // Probe engagement WITHOUT running the gate: a not-engaged pass
                // cannot distribute, and must not stamp (stamping every probe
                // would defer the sentinel forever on a plane that never
                // engages). Engagement is a one-way latch,
                // so `true` cannot revert before the `gated_remove` below.
                Some(gate) if gate.engaged() => {
                    // Durably stamp the sentinel BEFORE the gate can distribute
                    // (the gated route records no bytes): a crash anywhere past
                    // this point leaves a sentinel that provably may have been
                    // distributed, so every later gate-less resume defers
                    // instead of rebuilding blind. A stamp that fails to
                    // persist fails the attempt (retryable) — proceeding
                    // unstamped would break that invariant.
                    self.stamp_gated_attempted(&removal).await?;
                    gate.gated_remove(&removal.channel_id, &removal.removed_member)
                        .await
                        .map_err(FoldersAuthorError::RemovalGate)?
                }
                Some(_) => GatedRemoval::GateNotEngaged,
                None => GatedRemoval::NoGate,
            }
        };

        // The freshly-built commit bytes (ungated route only) — observability on
        // the returned `RemoveOutcome`; the gated route's commit is distributed
        // inside the rebase loop and not re-surfaced here.
        let mut commit: Option<Vec<u8>> = None;
        match gated {
            GatedRemoval::PendingUnconverged => {
                // The channel carries an epoch transition this device hasn't
                // incorporated (a restored crash-window pending awaiting its
                // own-leaf resync). Building a commit now could fork the group;
                // the sentinel stays staged, so a later resume completes the
                // rotation once the background poll / resync converges.
                return Err(FoldersAuthorError::RemovalGate(
                    "channel carries an unincorporated commit awaiting resync; \
                     removal deferred (sentinel retained — resume completes it)"
                        .into(),
                ));
            }
            // Removed / AlreadyAbsent: the Remove commit is handled (merged +
            // distributed by the rebase loop) or moot (member already gone) —
            // the epoch is already advanced; only the rotation remainder is left.
            GatedRemoval::Removed | GatedRemoval::AlreadyAbsent => {}
            // The gate is wired but never engaged (the plane is still
            // restoring, or its load permanently failed), and this sentinel is
            // stamped `true` — an earlier **engaged** gated attempt
            // started, and may have distributed a Remove commit for it,
            // recording no bytes (the gated route's commit rides the rebase
            // loop). With no
            // gate there is no walk-to-head to settle it: a fresh commit built
            // here would be blind — no `PendingUnconverged` check, no
            // incorporation of the log — and could fork the group. Defer
            // retryably; the sentinel stays staged for a later, wired launch
            // (the same posture `recover_poisoned_removal` takes).
            GatedRemoval::GateNotEngaged if removal.gated_attempted => {
                return Err(FoldersAuthorError::RemovalGate(
                    "removal deferred: the commit gate is wired but not engaged \
                     (multi-device replica plane still restoring, or its load \
                     failed), so a staged removal cannot be safely rebuilt — an \
                     earlier gated attempt may already have distributed its commit. \
                     Sentinel retained; a later launch with the plane wired \
                     completes it"
                        .into(),
                ));
            }
            // Ungated by design: no gate wired at all, a byted resume (durable
            // bytes outrank the gate), or a not-engaged plane
            // with the sentinel still stamped `false` — no engaged gated
            // attempt ever started for it, so nothing can have been distributed
            // (the ungated route persists its bytes before it merges or sends)
            // and its rebuild is not blind. This is the single-device /
            // no-`fauna.mls`-plane fallback; deferring it would leave the user
            // unable to remove a member at all.
            GatedRemoval::NoGate | GatedRemoval::GateNotEngaged => {
                // Serialize the whole ungated commit section (stage → merge →
                // send, and the resume-merge leg) against the background
                // folder poll via the backend's per-channel lock, when a
                // backend is wired (second route): a foreign
                // commit merged mid-window drops the staged pending. The gate
                // is NOT holding this lock here — `gated_remove` released it
                // before returning (or was never consulted on a byted resume).
                let section_lock = self
                    .commit_gate
                    .as_ref()
                    .and_then(|g| g.ungated_channel_lock(&removal.channel_id));
                let _section_guard = match &section_lock {
                    Some(lock) => Some(lock.lock().await),
                    None => None,
                };
                commit = self.drive_removal_commit_ungated(&removal).await?;
            }
        }

        // The epoch is advanced (or the member was already gone); publish the
        // rotated envelope, evict, and commit the generation.
        self.finish_rotation(&removal, commit).await
    }

    /// Durably stamp `removal`'s sentinel `gated_attempted = true` —
    /// called by [`Self::drive_removal`]'s gated leg strictly BEFORE
    /// [`FolderCommitGate::gated_remove`] runs on an engaged gate, so the
    /// stamp precedes anything that can distribute (race-free on this device;
    /// engagement is a one-way latch). One-way and idempotent: an
    /// already-stamped sentinel skips the write; a missing one (already
    /// resolved — the gate reports `AlreadyAbsent`) has nothing to protect.
    async fn stamp_gated_attempted(
        &self,
        removal: &FolderPendingRemoval,
    ) -> AuthorResult<(), R, G> {
        let mut cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        if custody::mark_removal_gated_attempted(
            &mut cfg,
            &removal.channel_id,
            &removal.removed_member,
        ) {
            self.custody
                .merge(cfg.clone())
                .await
                .map_err(FoldersAuthorError::CustodyStore)?;
        }
        Ok(())
    }

    /// The rotation remainder shared by [`Self::drive_removal`] and
    /// [`Self::recover_poisoned_removal`], run once the MLS Remove commit is
    /// handled (merged + distributed, or moot because the member is already off
    /// the leaf) and the owner's epoch is advanced: seal the rotated content-key
    /// envelope under the now-current epoch, publish it, evict the member from
    /// the nest roster (F1/OBS-1), then commit the staged generation into
    /// `current` and clear the sentinel — all under one CAS so a concurrent
    /// peer-device write converges rather than clobbering. `commit` is the
    /// freshly-built commit bytes (ungated route) surfaced on the outcome; `None`
    /// on a resume, an already-absent member, or the recovery route.
    async fn finish_rotation(
        &self,
        removal: &FolderPendingRemoval,
        commit: Option<Vec<u8>>,
    ) -> AuthorResult<RemoveOutcome, R, G> {
        // Load fresh (with its CAS base) for the pre-publish custody read and the
        // post-publish commit; the freshly-loaded config carries the staged
        // sentinel persisted before this drive.
        let mut cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;

        // The post-rotation bundle to seal + commit: the current custody history
        // with the staged generation merged in as `current` (old current → prior).
        // Reads custody (still the OLD current — the rotation isn't committed until
        // the nest confirms), so a resumed drive produces the identical envelope.
        let current_keys = custody::content_keys(&cfg, &removal.channel_id)
            .ok_or(FoldersAuthorError::NoContentKeys(removal.channel_id))?;
        let rotated = current_keys.merge(&FolderContentKeys {
            current: removal.new_generation.clone(),
            prior: Vec::new(),
        });

        // Seal under the now-advanced current epoch + publish opaque to the nest.
        let payload = custody::envelope_payload(
            &cfg,
            Some(&removal.name),
            &removal.channel_id,
            rotated.clone(),
        );
        let (sealed, epoch) = self.seal_and_sign(&removal.channel_id, &payload)?;
        self.files
            .content_key_put(ContentKeyPutRequest {
                name: removal.name.clone(),
                epoch: epoch as i64,
                sealed: hex::encode(&sealed),
                // Rotation advances the floor: the rotated bundle's new current
                // generation becomes the minimum a non-owner record may stamp
                // (KMH § M2 version floor — closes the evictee's stale-writer
                // window server-side).
                current_version: rotated.current_version(),
                ..Default::default()
            })
            .await
            .map_err(FoldersAuthorError::Transport)?;

        // Evict the removed member from the nest roster (F1/OBS-1).
        let evicted = self
            .files
            .members_evict(MemberEvictRequest {
                name: removal.name.clone(),
                member: hex::encode(removal.removed_member.0),
                ..Default::default()
            })
            .await
            .map_err(FoldersAuthorError::Transport)?
            .evicted;

        // Commit: move the staged generation into `current` (idempotent) and
        // join it into the store, then settle the sentinel — the store's one
        // removal, which lands the generation too and keeps the staging out of
        // every replica's fold (ruling (l); `clear_pending_removal`'s plane twin).
        custody::commit_generation(&mut cfg, &removal.channel_id, &removal.new_generation)
            .map_err(FoldersAuthorError::Custody)?;
        self.custody
            .merge(cfg)
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        self.custody
            .settle_removal(removal.clone())
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;

        // Rotation committed — if the set is paywalled, re-provision the
        // web-serve holder's grant with the new generation so entitled visitors
        // keep reading fresh-sealed content (`monetization.md` § Pillar 2:
        // "rotation re-provisions the grant"). Best-effort by design: the
        // removal (the security half) is already complete and must not fail on
        // an availability step; a lost renew here is re-driven by the
        // launch-time [`Self::renew_paywalled_grants`] pass.
        #[cfg(feature = "mls")]
        let _ = self
            .renew_paywall_grant_if_paywalled(&removal.name, Some(removal.channel_id))
            .await;

        let _ = epoch; // epoch rides the published envelope; kept for clarity.
        Ok(RemoveOutcome {
            commit,
            evicted,
            rotated: true,
        })
    }

    /// **Discharge a poisoned removal sentinel** — the client-reachable recovery
    /// for [`FoldersAuthorError::PoisonedRemovalSentinel`], the state where a
    /// byted sentinel's staged pending is gone (a wasm tab reload before the
    /// send; a cross-device union of the sentinel onto a peer that never staged
    /// it) so [`Self::resume_pending_removals`] can neither merge nor safely
    /// rebuild it. Without this, a member the user removed keeps MLS-group
    /// membership and the live content key indefinitely with no client-side fix —
    /// exactly the *client-causable unrecoverable nest state* the product invariants
    /// forbid (`pending_removals` lives in the account plane's
    /// `fauna.state.folder-keys` rows). No SSH, no DB surgery: the client calls this.
    ///
    /// The recovery routes the removal through the **commit gate's walk-to-head**
    /// (the same entry protocol [`FolderCommitGate::gated_remove`] runs), which
    /// resolves the one thing the poisoned resume could not — *was the sentinel's
    /// commit ever distributed?* — by reading the channel log:
    ///
    /// - The walk **heals a distributed commit** (its own-leaf resync merges it):
    ///   the member goes off the leaf → [`GatedRemoval::AlreadyAbsent`] → this
    ///   finishes the rotation the original drive never published. The stale
    ///   bytes are simply dropped (re-sending them is unnecessary; the members
    ///   already have them).
    /// - A **clean walk with the member still present** proves the commit was
    ///   **never** distributed (the send is the only distribution path, and it is
    ///   not in the log), so the gate safely **re-stages a fresh** Remove through
    ///   the rebase loop → [`GatedRemoval::Removed`] → this finishes the rotation.
    ///   Fork-free precisely because the clean walk ruled out a distributed
    ///   twin.
    ///
    /// Idempotent + safe to retry: a sentinel already cleared (recovered, or the
    /// member left) is a no-op; a gate that is not yet engaged
    /// ([`GatedRemoval::GateNotEngaged`] — the multi-device replica plane is still
    /// restoring, or its load failed) or a log still converging
    /// ([`GatedRemoval::PendingUnconverged`]) returns a **retryable** error and
    /// leaves the sentinel staged for a later call. Recovery therefore needs the
    /// gate; a pure single-device deployment that somehow reaches a poisoned
    /// sentinel (native persistence makes it near-unreachable) resolves it by
    /// re-sharing the set, which re-establishes the group.
    pub async fn recover_poisoned_removal(
        &self,
        channel_id: [u8; 32],
        member: ActorId,
    ) -> AuthorResult<RemoveOutcome, R, G> {
        let cfg = self
            .custody
            .load()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;
        let Some(removal) = custody::find_pending_removal(&cfg, &channel_id, &member).cloned()
        else {
            // Already resolved (recovered, or the member voluntarily left).
            return Ok(RemoveOutcome {
                commit: None,
                evicted: false,
                rotated: false,
            });
        };

        // Recovery is only meaningful once the gate can walk the log to head —
        // the walk is what makes dropping the (possibly-distributed) stale bytes
        // safe. Route through `gated_remove`, never the ungated rebuild (which
        // would rebuild blind and could fork a distributed commit).
        let gate = self.commit_gate.as_ref().ok_or_else(|| {
            FoldersAuthorError::RemovalGate(
                "poisoned-removal recovery needs the multi-device commit gate to \
                 walk the channel log to head; unavailable on this client — \
                 re-share the set to re-establish the group"
                    .into(),
            )
        })?;

        match gate
            .gated_remove(&channel_id, &member)
            .await
            .map_err(FoldersAuthorError::RemovalGate)?
        {
            // The plane isn't ready to walk the log yet (replica still restoring),
            // or the log carries an unincorporated commit — both retryable. Leave
            // the sentinel staged; a later call completes it.
            GatedRemoval::NoGate | GatedRemoval::GateNotEngaged => {
                Err(FoldersAuthorError::RemovalGate(
                    "poisoned-removal recovery deferred: the commit gate is not yet \
                     engaged (multi-device replica plane still restoring); retry once \
                     the client has finished launching"
                        .into(),
                ))
            }
            GatedRemoval::PendingUnconverged => Err(FoldersAuthorError::RemovalGate(
                "poisoned-removal recovery deferred: the channel log carries an \
                 unincorporated commit awaiting resync; retry after the background \
                 poll converges (sentinel retained)"
                    .into(),
            )),
            // Distributed-and-healed (member off the leaf) or freshly re-staged +
            // distributed by the rebase loop — the epoch is advanced either way,
            // and the stale bytes are moot. Finish the rotation the original
            // poisoned drive never published, clearing the sentinel.
            GatedRemoval::AlreadyAbsent | GatedRemoval::Removed => {
                self.finish_rotation(&removal, None).await
            }
        }
    }

    /// The **ungated** Remove-commit discipline (no [`FolderCommitGate`] wired,
    /// or no `CommitGate` injected on the backend — single-device plane). Builds
    /// or resumes the Remove commit and distributes it, honoring Rule 1
    /// (`devices.md` § Cross-device MLS group-state sync) end-to-end:
    ///
    /// - **Resume (durable sentinel bytes exist):** those bytes may already have
    ///   been distributed, so they are re-sent verbatim and NEVER rebuilt — a
    ///   second commit for the same transition is the permanent fork. The restored still-staged pending (persisted by
    ///   [`FolderGroupCrypto::persist_group_state`] before the sentinel write)
    ///   is merged if present; with no pending the merge already happened
    ///   pre-crash. Bytes + no pending + member still present means a **byted
    ///   sentinel** whose pending is unrecoverable here (a wasm reload, a
    ///   cross-device union) — fail loudly:
    ///   sealing now would go under the PRE-removal epoch and hand the rotated
    ///   generation to the removed member.
    /// - **Fresh build (no durable bytes):** nothing was distributed (bytes are
    ///   persisted before the merge, which precedes the send), so clearing an
    ///   interrupted stage and rebuilding is safe — **unless a BYTED sibling
    ///   sentinel exists on this channel**: a live engine
    ///   pending may then be that sibling's restored merge source, and clearing
    ///   it (or staging over the channel) would strand the sibling in the
    ///   fail-loud arm permanently. The fresh build defers (retryable, sentinel
    ///   retained) until [`Self::resume_pending_removals`] — which drives byted
    ///   sentinels first — completes the sibling. Order: stage → persist the
    ///   pending locally → record the bytes on the sentinel (CAS) → merge →
    ///   persist the merged state → send. Every crash point resumes as either
    ///   "rebuild (nothing durable)" or "merge-and-resend the same bytes" —
    ///   a merged-but-**unrecoverable** commit is unrepresentable (a merged
    ///   commit always has durable sentinel bytes to re-send; the send itself
    ///   can still fail, leaving a merged-but-undistributed commit that the
    ///   next resume re-sends), and a distributed-but-unmergeable one is
    ///   unrepresentable outright.
    ///
    /// Returns the freshly-built commit bytes (`None` on a resume or when the
    /// member was already absent) for `RemoveOutcome.commit` observability.
    async fn drive_removal_commit_ungated(
        &self,
        removal: &FolderPendingRemoval,
    ) -> AuthorResult<Option<Vec<u8>>, R, G> {
        let mut cfg = self
            .custody
            .load_for_write()
            .await
            .map_err(FoldersAuthorError::CustodyStore)?;

        let staged_bytes =
            custody::find_pending_removal(&cfg, &removal.channel_id, &removal.removed_member)
                .and_then(|r| r.commit.clone());

        let mut fresh: Option<Vec<u8>> = None;
        let to_send = if let Some(bytes) = staged_bytes {
            if self
                .group
                .has_pending_commit(&removal.channel_id)
                .map_err(FoldersAuthorError::Group)?
            {
                // A pending exists — but is it THIS sentinel's commit? On the
                // ungated single-device plane, the stage + the sentinel write
                // happen in one drive and nothing else stages a folder commit,
                // so it always is. But the durable-bytes-outrank-the-gate fix
                // (see the doc above) routes a byted sentinel
                // through here on the GATED plane too, where `gated_remove` staged
                // a **different** member's Remove into the shared engine (the
                // provider replica carries pendings by design). Merging that
                // foreign pending while broadcasting THIS sentinel's bytes forks
                // the group and clears the wrong
                // sentinel.
                //
                // The engine stamps every staged commit's identity durably beside
                // the pending, so compare before merging: only `blake3(bytes)`
                // matching the engine's pending identity proves the restored
                // pending is ours. On a mismatch, refuse (fail-loud) and — crucially
                // — do NOT clear it: it may be a sibling drive's only merge source. The resume retries after the sibling completes.
                let want = *blake3::hash(&bytes).as_bytes();
                let have = self
                    .group
                    .pending_commit_hash(&removal.channel_id)
                    .map_err(FoldersAuthorError::Group)?;
                if have != Some(want) {
                    return Err(FoldersAuthorError::RemovalGate(
                        "byted removal resume found a FOREIGN pending in the engine \
                         (its commit identity ≠ this sentinel's bytes) — refusing to \
                         merge it as ours (that would fork the group and clear the \
                         wrong sentinel); sentinel retained, resume retries once the \
                         sibling drive completes"
                            .into(),
                    ));
                }
                // Our own pending — merge it and make the merged state durable.
                self.group
                    .merge_pending_commit(&removal.channel_id)
                    .map_err(FoldersAuthorError::Group)?;
                self.group
                    .persist_group_state(&removal.channel_id)
                    .map_err(FoldersAuthorError::Group)?;
            } else if self
                .group
                .contains_member(&removal.channel_id, &removal.removed_member)
                .map_err(FoldersAuthorError::Group)?
            {
                // Bytes + no pending + member still present. Historically this was
                // dismissed as a "legacy pre-durability crash remnant", but it is
                // reachable in two ordinary ways — a wasm tab reload between the
                // stage and the send (`persist_group_state` is a no-op on wasm),
                // and a cross-device union of a byted sentinel onto a peer that
                // never staged the pending. In every case the pending is
                // unrecoverable HERE, so a plain merge is impossible; rebuilding
                // would fork and sealing would go under the pre-removal epoch.
                // Fail loud — but the whole reason per-entry error isolation
                // exists is that this must not be a permanent brick: `resume_pending_removals`
                // isolates this error and a client-reachable recovery path
                // (`recover_poisoned_removal`) re-derives or abandons the sentinel.
                return Err(FoldersAuthorError::PoisonedRemovalSentinel {
                    channel_id: removal.channel_id,
                    member: removal.removed_member,
                });
            }
            Some(bytes)
        } else {
            // While a BYTED sibling sentinel exists on this
            // channel, a live engine pending may be ITS restored merge source
            // (not this drive's leftover) — the clear below would destroy it,
            // and staging a new commit over the channel would equally evict it
            // (openmls holds one pending per group). Defer this whole fresh
            // build; `resume_pending_removals` drives byted sentinels first,
            // so the sibling completes (clearing its sentinel) and unblocks
            // this one.
            if custody::has_other_pending_removal_with_commit(
                &cfg,
                &removal.channel_id,
                &removal.removed_member,
            ) {
                return Err(FoldersAuthorError::RemovalGate(
                    "channel holds a sibling removal sentinel with durable commit \
                     bytes; deferring this fresh build so its restored pending is \
                     not destroyed (sentinel retained — resume completes the byted \
                     sibling first, then this removal)"
                        .into(),
                ));
            }

            // Discard a commit staged by an interrupted earlier drive: with no
            // durable bytes it was provably never distributed, so rebuilding is
            // safe — and openmls refuses to build a second commit over an
            // unmerged one. A no-op when nothing is pending (the common pass).
            // (With the guard above, a pending cleared here provably belongs to
            // an interrupted BYTE-LESS drive on this channel, never to a byted
            // sibling.)
            self.group
                .clear_pending_commit(&removal.channel_id)
                .map_err(FoldersAuthorError::Group)?;

            // Advance the epoch (MLS Remove) BEFORE sealing — but
            // *stage* the commit rather than merging, so the merge can wait for
            // durability. Idempotent: an already-absent member stages `None`.
            let built = self
                .group
                .remove_member_staged(&removal.channel_id, &removal.removed_member)
                .map_err(FoldersAuthorError::Group)?;
            if let Some(bytes) = &built {
                // 1. The staged pending durable locally — the restart path's only
                //    way to ever merge THESE bytes (a pending is not
                //    reconstructible: MLS cannot re-produce a commit for a
                //    transition, and an owner cannot process its own commit).
                self.group
                    .persist_group_state(&removal.channel_id)
                    .map_err(FoldersAuthorError::Group)?;
                // 2. The bytes durable on the sentinel — the restart path's only
                //    way to re-distribute them.
                custody::stage_removal_commit(
                    &mut cfg,
                    &removal.channel_id,
                    &removal.removed_member,
                    bytes.clone(),
                );
                if let Err(e) = self.custody.merge(cfg.clone()).await {
                    // Leave the group operational: scrub the staged pending (and
                    // its just-persisted snapshot) so it can't linger as a
                    // dangling pending blocking the next drive's rebuild.
                    let _ = self.group.clear_pending_commit(&removal.channel_id);
                    let _ = self.group.persist_group_state(&removal.channel_id);
                    return Err(FoldersAuthorError::CustodyStore(e));
                }
                // 3. Both durabilities hold — cross the point of no return, and
                //    persist the merged state so a crash before the send resumes
                //    as "re-send the same bytes", never as a rebuild.
                self.group
                    .merge_pending_commit(&removal.channel_id)
                    .map_err(FoldersAuthorError::Group)?;
                self.group
                    .persist_group_state(&removal.channel_id)
                    .map_err(FoldersAuthorError::Group)?;
            }
            fresh = built.clone();
            built
        };

        // Distribute the Remove commit to the **remaining** members over the
        // set's channel — their epoch-advance liveness (the removed member's
        // *exclusion* never depends on this; it rests on rotate-on-removal).
        // Same plane as conversations' ungated `remove_participant`: a
        // `ChannelEnvelope::Commit` posted ungated to
        // `fauna.conversations.channel.send`; members' folder commit poll
        // applies it via `process_commit`, and the roster push-nudges them. A
        // resumed drive re-sends the sentinel's bytes — the duplicate append is
        // harmless (members quiet-skip a past-epoch commit). A **byte-less**
        // sentinel (the gated route records none) has nothing to send.
        if let Some(bytes) = to_send {
            let envelope = ChannelEnvelope::Commit(bytes)
                .to_bytes()
                .map_err(FoldersAuthorError::Envelope)?;
            let _: ChannelSendReply = self
                .files
                .requester()
                .request(
                    "fauna.conversations.channel.send",
                    ChannelSendRequest {
                        channel_id: hex::encode(removal.channel_id),
                        envelope,
                        expect_no_commit_since: None,
                        attachment_refs: Vec::new(),
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(FoldersAuthorError::Transport)?;
        }
        Ok(fresh)
    }
}

/// The client-side folder **share flow** (piece 5d(b-pre)) — gated behind the
/// `mls` feature alongside the real [`FolderGroupCrypto`] adapter, since it needs
/// both a live group ([`FolderGroupCrypto::create_group`]) and the shared
/// conversations MLS-channel-setup client (`fauna-client-conversations`, which pulls
/// `fauna-mls` and so must stay out of the base crate's wasm-light graph).
#[cfg(feature = "mls")]
impl<R: RpcRequester, G: FolderGroupCrypto> FoldersAuthor<R, G>
where
    R::Error: RpcErrorClass,
{
    /// Share an owner-only folder with `member` end-to-end, mirroring
    /// conversations' `FaunaMlsBackend::bootstrap_group` (fetch key package → create
    /// group → deliver welcome) with the folder bind spliced in:
    ///
    /// 1. fetch `member`'s KeyPackage (`convs.keypackage_fetch`),
    /// 2. create the MLS group admitting them ([`FolderGroupCrypto::create_group`]),
    /// 3. `fauna.folders.share` the **raw** group id (the nest re-derives + claims
    ///    the channel, registering the owner on the roster) — asserting the
    ///    nest-derived ChannelId matches the locally-created one,
    /// 4. [`Self::bind_set`] the genesis content key + publish the envelope
    ///    (custody-first, crash-safe),
    /// 5. deliver the Welcome to `member` (`convs.welcome_deliver`), which registers
    ///    them on the nest `actor_channels` roster.
    ///
    /// `member_nest_url` is `None` for a same-nest member (the common case) or the
    /// peer nest's base URL for a cross-nest share (federation relay, threaded
    /// through both conversations RPCs). Returns the derived `channel_id` (the set's
    /// custody/roster address) + the Welcome's nest inbox id.
    ///
    /// **Crash-safety:** no pending-stage sentinel is needed (unlike
    /// [`Self::remove_member`]) — the genesis content key is created custody-first in
    /// step 4 only **after** the share binds, so a crash mid-flow loses no
    /// irrecoverable key; the set is simply re-shareable (the nest `share`-claim is an
    /// idempotent re-bind by the same owner).
    ///
    /// **Welcome kind:** delivered as [`WelcomeKind::Folder`](fauna_protocol::conversations::WelcomeKind::Folder)
    /// — it carries the group id and lands the member on the roster like a `Group`
    /// welcome, but the tag routes the recipient's receive loop to the folder
    /// pending-share surface, not a phantom chat thread (mirroring
    /// `WelcomeKind::Scheduling`; `docs/goal/ui/folders.md` § Sharing, *Recipient
    /// routing + gate*). The recipient contact-status gate + Welcome-staging (auto
    /// vs knock) rides on this tag and is a separate slice.
    /// `access` is the invited member's grant — `Some("writer")` for a
    /// read-write share, `None`/`Some("reader")` for the read-only default
    /// (multi-writer Phase 1; the nest records it with the bind, so the grant
    /// exists before the Welcome is even delivered).
    pub async fn share_set(
        &self,
        convs: &fauna_client_conversations::ConversationsClient<R>,
        name: &str,
        member: ActorId,
        member_nest_url: Option<String>,
        access: Option<String>,
    ) -> AuthorResult<ShareOutcome, R, G> {
        // Discover the set's current binding — the nest is authoritative for the
        // name → group mapping (custody is keyed by the *derived* ChannelId, not
        // the name). An owner-scoped `list` projects `mls_group_id` for a bound
        // set (`folder_handlers.rs::owner_summary`); its absence means the set
        // was never shared, so this is the first share (first-binder path). Its
        // presence means the 2nd..Nth share, which must ADD to the existing group
        // rather than mint a fresh one — the M2 *Admitting a member* fix.
        //
        // Unrendered and matched by HASH (`FolderSummary::is_named`): a sealed
        // set's row rests no plaintext name (schema 114), and a by-name miss
        // here would send a 2nd share down the first-binder path — a fresh
        // group minted over the bound set.
        let bound_group = self
            .files
            .list_wire()
            .await
            .map_err(FoldersAuthorError::Transport)?
            .folders
            .into_iter()
            .find(|s| s.is_named(name))
            .and_then(|s| s.mls_group_id)
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty());

        match bound_group {
            None => {
                self.share_set_first_binder(convs, name, member, member_nest_url, access)
                    .await
            }
            Some(group_id_hex) => {
                let raw_group_id = hex::decode(&group_id_hex).map_err(|_| {
                    // The nest stores the raw group id and round-trips it as hex,
                    // so this is unreachable data corruption — surface it as a
                    // retryable add rather than silently minting a fresh group
                    // (which would re-bind and drop the earlier members).
                    FoldersAuthorError::AddDeferred(format!(
                        "set '{name}' has a malformed stored group id ({group_id_hex:?})"
                    ))
                })?;
                let channel_id = self.group.channel_id_for_group(&raw_group_id);
                // A set bound to a group this engine does not hold is a fresh
                // device before its replica restore — minting a fresh group here
                // would re-bind the set and drop the earlier members. Refuse
                // retryably; a later launch with the group restored completes it.
                if !self
                    .group
                    .holds_group(&channel_id)
                    .map_err(FoldersAuthorError::Group)?
                {
                    return Err(FoldersAuthorError::AddDeferred(format!(
                        "set '{name}' is bound to a group this device does not hold yet \
                         (replica restoring); retry the share after restore"
                    )));
                }
                self.share_set_add(
                    convs,
                    name,
                    member,
                    member_nest_url,
                    access,
                    raw_group_id,
                    channel_id,
                )
                .await
            }
        }
    }

    /// The **first share** of a set (`share_set`'s `None`-binding arm): fetch the
    /// member's KeyPackage → create a fresh MLS group admitting them →
    /// `fauna.folders.share` (the nest re-derives + claims the channel) →
    /// [`Self::bind_set`] the genesis key + publish → deliver the Welcome. No
    /// pending-stage sentinel is needed (the genesis key is created custody-first
    /// only *after* the share binds, so a crash mid-flow loses no irrecoverable
    /// key; the set is simply re-shareable).
    async fn share_set_first_binder(
        &self,
        convs: &fauna_client_conversations::ConversationsClient<R>,
        name: &str,
        member: ActorId,
        member_nest_url: Option<String>,
        access: Option<String>,
    ) -> AuthorResult<ShareOutcome, R, G> {
        // 1. The member must have published a KeyPackage for the owner to admit.
        let kp = convs
            .keypackage_fetch(hex::encode(member.0), member_nest_url.clone())
            .await
            .map_err(FoldersAuthorError::Transport)?
            .key_package
            .ok_or(FoldersAuthorError::NoKeyPackage(member))?;

        // 2. Create the MLS group admitting the member (engine op via the seam).
        let created = self
            .group
            .create_group(&[kp])
            .map_err(FoldersAuthorError::Group)?;

        // 3. Bind on the nest: it re-derives `ChannelId::from_group_id(raw)` and
        //    first-binder-wins-claims the channel (registering the owner on the
        //    roster). Guard the derivation invariant: a mismatch means this crate's
        //    `MlsEngine` and the nest's `from_group_id` disagree (should be
        //    unreachable — both BLAKE3 the same raw id).
        let reply = self
            .files
            .share(fauna_protocol::folders::FolderShareRequest {
                name: name.to_string(),
                group_id: hex::encode(&created.raw_group_id),
                // Share-time grant (D5): recorded with the bind, before the
                // Welcome, so a joining writer's grant is never unset.
                member_actor_id: Some(hex::encode(member.0)),
                access,
                ..Default::default()
            })
            .await
            .map_err(|e| {
                // The nest refuses a share naming a group other than the set's own
                // (`already_bound` — `key-material-hierarchy.md`
                // § M2 *Admitting a member*). Reaching it from *this* arm is a
                // benign race, not a bug: `share_set` read an unbound set from the
                // list and another device bound it in the window before this call.
                // Surface it as retryable — the retry re-reads the binding and
                // takes the add path — rather than as an opaque transport error.
                if e.as_rpc_error()
                    .is_some_and(|r| r.code == "fauna.folders.already_bound")
                {
                    return FoldersAuthorError::AddDeferred(format!(
                        "set '{name}' was bound to an MLS group by another device \
                         while this share was in flight; retry the share"
                    ));
                }
                FoldersAuthorError::Transport(e)
            })?;
        let nest_channel = hex::decode(reply.channel_id.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok());
        if nest_channel != Some(created.channel_id) {
            return Err(FoldersAuthorError::ChannelMismatch {
                local: created.channel_id,
                nest: reply.channel_id,
            });
        }

        // 4. Genesis content key + envelope publish (custody-first, idempotent).
        self.bind_set(name, created.channel_id).await?;

        // 5. Deliver the Welcome → the member lands on the roster. Tagged
        //    `Folder` (not `Group`) so the recipient's receive loop routes it to
        //    the folder pending-share surface, never a phantom chat thread
        //    (folders.md § Sharing, *Recipient routing + gate*).
        let delivered = convs
            .welcome_deliver(
                hex::encode(member.0),
                hex::encode(created.channel_id),
                created.welcome.clone(),
                fauna_protocol::conversations::WelcomeKind::Folder {
                    group_id: hex::encode(&created.raw_group_id),
                },
                member_nest_url,
            )
            .await
            .map_err(FoldersAuthorError::Transport)?;

        Ok(ShareOutcome {
            channel_id: created.channel_id,
            inbox_id: delivered.inbox_id,
        })
    }

    /// The **2nd..Nth share** (`share_set`'s `Some`-binding arm) — admit `member`
    /// into the set's **existing** MLS group instead of minting a fresh one
    /// (`mls-group-key-material.md` § M2 *Admitting a member*; `ui/folders.md`
    /// § Sharing → *Adding the 2nd..Nth member*). `raw_group_id`/`channel_id` are
    /// the existing binding (the caller already verified this engine holds it).
    ///
    /// The share gesture is **idempotent + self-healing** (Q1):
    /// - **On the roster already** → refresh only their access row (an idempotent
    ///   `share` re-bind); no MLS op, no Welcome.
    /// - **In the group but off the roster** → a crashed earlier share left a
    ///   ghost leaf (its Welcome never landed). Evict it through the proven
    ///   rotate-on-removal machine, then admit fresh below.
    /// - **Neither** → a genuine newcomer: fetch KP → `share` (grant row +
    ///   idempotent re-bind, D5) → resume-then-add guard (Q4) → stage + distribute
    ///   the Add commit (gated / ungated Rule-1) → re-publish the envelope under
    ///   the post-Add epoch **before** the Welcome (Q3) → deliver the Welcome with
    ///   the **existing** raw group id.
    ///
    /// **No custody sentinel** (Q1): an add stages no irrecoverable key material,
    /// so a crash mid-flight leaves the newcomer in-group + off-roster (rendered
    /// truthfully as not-yet-shared) and heals through the re-share gesture — not
    /// a launch-time resume.
    #[allow(clippy::too_many_arguments)]
    async fn share_set_add(
        &self,
        convs: &fauna_client_conversations::ConversationsClient<R>,
        name: &str,
        member: ActorId,
        member_nest_url: Option<String>,
        access: Option<String>,
        raw_group_id: Vec<u8>,
        channel_id: [u8; 32],
    ) -> AuthorResult<ShareOutcome, R, G> {
        let member_hex = hex::encode(member.0);
        let group_id_hex = hex::encode(&raw_group_id);

        // Roster discriminator (Q1): the owner-visible "Shared with" roster.
        let rostered = self
            .files
            .actor_members_list(name)
            .await
            .map_err(FoldersAuthorError::Transport)?
            .members
            .iter()
            .any(|m| m.actor_id.eq_ignore_ascii_case(&member_hex));

        if rostered {
            // Already fully shared — only refresh the access row (idempotent
            // claimant re-bind with the existing group id). No Welcome: their
            // membership is untouched. `inbox_id` 0 marks "nothing delivered".
            self.files
                .share(fauna_protocol::folders::FolderShareRequest {
                    name: name.to_string(),
                    group_id: group_id_hex,
                    member_actor_id: Some(member_hex),
                    access,
                    ..Default::default()
                })
                .await
                .map_err(FoldersAuthorError::Transport)?;
            return Ok(ShareOutcome {
                channel_id,
                inbox_id: 0,
            });
        }

        // Off the roster but still on a leaf ⇒ a ghost from a crashed earlier
        // share (the Add merged, the Welcome never landed). Evict it through the
        // ordinary rotate-on-removal machine (rotation is unnecessary here but
        // harmless — reusing the proven crash-safe machine beats a bespoke evict),
        // then admit fresh with a new KeyPackage + a new Welcome.
        if self
            .group
            .contains_member(&channel_id, &member)
            .map_err(FoldersAuthorError::Group)?
        {
            self.remove_member(name, channel_id, member).await?;
        }

        // A fresh KeyPackage for the newcomer (a Welcome is MLS-mintable only
        // inside the Add commit, so a lost one is not re-constructible — the
        // ghost heal above is why a new KP is fetched each attempt).
        let kp = convs
            .keypackage_fetch(member_hex.clone(), member_nest_url.clone())
            .await
            .map_err(FoldersAuthorError::Transport)?
            .key_package
            .ok_or(FoldersAuthorError::NoKeyPackage(member))?;

        // Nest `share` FIRST (Q5): with the existing group id it is an idempotent
        // re-bind whose real work is recording the newcomer's access row BEFORE
        // the Welcome (D5). Assert the nest re-derives the same channel id.
        let reply = self
            .files
            .share(fauna_protocol::folders::FolderShareRequest {
                name: name.to_string(),
                group_id: group_id_hex.clone(),
                member_actor_id: Some(member_hex.clone()),
                access,
                ..Default::default()
            })
            .await
            .map_err(FoldersAuthorError::Transport)?;
        let nest_channel = hex::decode(reply.channel_id.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok());
        if nest_channel != Some(channel_id) {
            return Err(FoldersAuthorError::ChannelMismatch {
                local: channel_id,
                nest: reply.channel_id,
            });
        }

        // Resume-then-add (Q4): a staged removal on this channel holds openmls's
        // one pending slot, so drive it to completion before staging the Add. A
        // deferral surfaces as a retryable error; the add never builds over an
        // unmerged pending, never clears another machine's sentinel.
        self.resume_pending_removals_on_channel(&channel_id).await?;

        // Stage + distribute the Add commit (gated rebase loop / ungated Rule-1),
        // yielding the newcomer's Welcome bytes.
        let welcome = self.drive_add(&channel_id, &kp).await?;

        // Re-publish the envelope under the now-current (post-Add) epoch BEFORE
        // the Welcome (Q3): the joiner's join-time custody ingest opens it at
        // their post-Add epoch instead of failing closed against a stale-epoch
        // seal with no timely retrigger. Same generations (the version floor is
        // unchanged; the nest-side monotonic MAX makes the re-put safe).
        self.republish_envelope(name, channel_id).await?;

        // Deliver the Welcome with the EXISTING raw group id → the newcomer lands
        // on the roster (same-nest `actor_channels`; cross-nest at the relay).
        let delivered = convs
            .welcome_deliver(
                member_hex,
                hex::encode(channel_id),
                welcome,
                fauna_protocol::conversations::WelcomeKind::Folder {
                    group_id: group_id_hex,
                },
                member_nest_url,
            )
            .await
            .map_err(FoldersAuthorError::Transport)?;

        Ok(ShareOutcome {
            channel_id,
            inbox_id: delivered.inbox_id,
        })
    }

    /// Drive any staged member-removals on `channel_id` to completion before an
    /// add stages its own pending (Q4 resume-then-add). Idempotent:
    /// [`Self::drive_removal`] clears each sentinel on success, so the loop
    /// shrinks; a deferral (gate not engaged / unconverged / poisoned) propagates
    /// as a retryable error and the add refuses — never building the Add over an
    /// unmerged pending, never clearing another machine's sentinel.
    async fn resume_pending_removals_on_channel(
        &self,
        channel_id: &[u8; 32],
    ) -> AuthorResult<(), R, G> {
        loop {
            let cfg = self
                .custody
                .load()
                .await
                .map_err(FoldersAuthorError::CustodyStore)?;
            let Some(removal) = cfg
                .pending_removals
                .iter()
                .find(|r| &r.channel_id == channel_id)
                .cloned()
            else {
                return Ok(());
            };
            self.drive_removal(removal).await?;
        }
    }

    /// Stage + distribute an MLS **Add** commit admitting the member whose
    /// KeyPackage is `kp_bytes` into the set's group, returning the newcomer's
    /// Welcome bytes. The add-path twin of [`Self::drive_removal`]:
    ///
    /// - **Gated** (a [`FolderCommitGate`] wired and engaged): the Add rides the
    ///   device-owned-epoch rebase loop ([`FolderCommitGate::gated_add`]).
    /// - **Ungated** (no gate, or wired-but-not-engaged): the Rule-1 staged
    ///   discipline below. Unlike a resumed removal, a fresh add has no prior
    ///   distributed attempt, so a not-engaged gate falls back safely.
    async fn drive_add(
        &self,
        channel_id: &[u8; 32],
        kp_bytes: &[u8],
    ) -> AuthorResult<Vec<u8>, R, G> {
        let outcome = match &self.commit_gate {
            Some(gate) if gate.engaged() => gate
                .gated_add(channel_id, kp_bytes)
                .await
                .map_err(FoldersAuthorError::AddDeferred)?,
            _ => GatedAdd::NoGate,
        };
        match outcome {
            GatedAdd::Added(welcome) => Ok(welcome),
            GatedAdd::PendingUnconverged => Err(FoldersAuthorError::AddDeferred(
                "channel carries an unincorporated commit awaiting resync; add \
                 deferred (retry after the background poll converges)"
                    .into(),
            )),
            GatedAdd::NoGate | GatedAdd::GateNotEngaged => {
                // Hold the backend's per-channel lock (when wired) across the
                // ungated stage → send → merge section so the background poll
                // can't merge a foreign inbound commit mid-window and drop the
                // staged pending (the removal ungated section's discipline).
                let section_lock = self
                    .commit_gate
                    .as_ref()
                    .and_then(|g| g.ungated_channel_lock(channel_id));
                let _section_guard = match &section_lock {
                    Some(lock) => Some(lock.lock().await),
                    None => None,
                };
                self.drive_add_commit_ungated(channel_id, kp_bytes).await
            }
        }
    }

    /// The **ungated** Add-commit discipline (no gate wired, or wired-but-not-
    /// engaged): Rule-1 ordering (`devices.md` § Cross-device MLS group-state
    /// sync) — stage the Add WITHOUT merging → persist the local pending → send
    /// the commit on the set's channel → merge only on an accepted send, clear on
    /// a failed one. Mirrors `FaunaMlsBackend::add_participant`'s gate-less arm:
    /// a merged-but-undistributed Add would strand the remaining members an epoch
    /// behind with no way to re-issue the commit. Returns the Welcome bytes.
    async fn drive_add_commit_ungated(
        &self,
        channel_id: &[u8; 32],
        kp_bytes: &[u8],
    ) -> AuthorResult<Vec<u8>, R, G> {
        let (commit_bytes, welcome_bytes) = self
            .group
            .add_member_staged(channel_id, kp_bytes)
            .map_err(FoldersAuthorError::Group)?;
        // The staged pending durable locally BEFORE the send — the restart path's
        // only way to ever merge THESE bytes (a pending is not reconstructible).
        self.group
            .persist_group_state(channel_id)
            .map_err(FoldersAuthorError::Group)?;
        let envelope = ChannelEnvelope::Commit(commit_bytes)
            .to_bytes()
            .map_err(FoldersAuthorError::Envelope)?;
        let send: Result<ChannelSendReply, _> = self
            .files
            .requester()
            .request(
                "fauna.conversations.channel.send",
                ChannelSendRequest {
                    channel_id: hex::encode(channel_id),
                    envelope,
                    expect_no_commit_since: None,
                    attachment_refs: Vec::new(),
                    extra: Default::default(),
                },
            )
            .await;
        match send {
            Ok(_) => {
                self.group
                    .merge_pending_commit(channel_id)
                    .map_err(FoldersAuthorError::Group)?;
                self.group
                    .persist_group_state(channel_id)
                    .map_err(FoldersAuthorError::Group)?;
            }
            Err(e) => {
                // Return the group to its pre-commit state so the share is cleanly
                // retryable (openmls refuses a second commit over an unmerged one).
                let _ = self.group.clear_pending_commit(channel_id);
                let _ = self.group.persist_group_state(channel_id);
                return Err(FoldersAuthorError::Transport(e));
            }
        }
        Ok(welcome_bytes)
    }
}

/// A fresh 32-byte per-set content key. Kept out of the pure `custody`
/// transitions so those stay deterministic + RNG-free.
/// Independent of every prior generation.
///
/// The CSPRNG call and the non-`Copy` *Carrier shape* rule belong to
/// [`fauna_core::secret::fresh_secret_32`], shared with the two sibling minters
/// (`fresh_msek`, `fresh_period_key`) rather than restated here — the family
/// this doc block names is exactly the kind that gets split by fixing the
/// member a finding happened to name (priority #4).
pub fn fresh_content_key() -> Zeroizing<[u8; 32]> {
    fauna_core::secret::fresh_secret_32()
}

/// Compile-time pin that [`fresh_content_key`] keeps handing its output out
/// non-`Copy` — see `key-material-hierarchy.md` § Carrier shape → *Pinned at
/// compile time*.
const _FRESH_CONTENT_KEY_IS_NOT_COPY: fn() -> Zeroizing<[u8; 32]> = fresh_content_key;

/// A `rotated_at` (micros) strictly above `floor` and at least the wall clock — so
/// each generation's `rotated_at` strictly increases (total generation order even
/// under clock skew).
fn next_rotated_at(floor: u64) -> u64 {
    Timestamp::now().0.max(floor.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_core::identity::ActorKeypair;
    use fauna_protocol::folders::{MemberEvictReply, MemberEvictRequest};
    #[cfg(feature = "mls")]
    use fauna_protocol::wrapped_blob::{RenewGrantReply, RenewGrantRequest};
    use std::collections::BTreeSet;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    // ── in-memory fake MLS group ──────────────────────────────────────────────

    /// Tracks current group membership + a monotonic epoch (advanced on remove) +
    /// the recorded seals (so a test can assert the seal happened under the
    /// post-removal epoch). Seals deterministically by canonical-
    /// encoding the generation bundle, so `open` would round-trip.
    /// What [`FolderGroupCrypto::persist_group_state`] captured — the fake's
    /// "SQLite provider snapshot". A simulated restart rebuilds the group from
    /// this: everything mutated since the last persist is lost, exactly like a
    /// crashed engine reloading `mls_state.db`.
    #[derive(Clone)]
    struct PersistedGroup {
        members: BTreeSet<[u8; 32]>,
        epoch: u64,
        pending_removal: Option<[u8; 32]>,
        /// A staged (unmerged) MLS **Add** — the add-path twin of
        /// `pending_removal`. At most one of the two is `Some` (openmls holds one
        /// pending per group); the merge applies whichever is set.
        pending_add: Option<[u8; 32]>,
        /// The staged commit's wire bytes, restored alongside `pending_removal` /
        /// `pending_add` so `{pending, its identity}` survive a restart together —
        /// modelling the real engine's provider-KV-resident `pending_commit_hash`.
        pending_commit: Option<Vec<u8>>,
    }

    #[derive(Default)]
    struct FakeGroup {
        members: Mutex<BTreeSet<[u8; 32]>>,
        epoch: Mutex<u64>,
        seals: Mutex<Vec<(FolderContentKeys, u64)>>,
        /// The member staged for removal by `remove_member_staged`, not yet merged
        /// — openmls's "pending commit". Modelling it is what lets a test observe
        /// that the epoch does NOT advance until the bytes are durable.
        pending_removal: Mutex<Option<[u8; 32]>>,
        /// The member staged for **addition** by `add_member_staged`, not yet
        /// merged — the add-path twin of `pending_removal`. At most one of the two
        /// is `Some` at a time (openmls holds one pending per group).
        pending_add: Mutex<Option<[u8; 32]>>,
        /// The staged commit's wire bytes, kept in lockstep with `pending_removal`
        /// / `pending_add` — the real engine records `blake3` of these in its
        /// provider KV, so the fake's `pending_commit_hash` hashes them.
        pending_commit: Mutex<Option<Vec<u8>>>,
        /// Whether this fake's engine holds the group — `true` for a normally
        /// constructed group; a test flips it to model a fresh device whose
        /// replica has not restored the group yet (the add-path refusal).
        holds: Mutex<bool>,
        /// Monotonic counter so each staged commit's bytes differ, proving a
        /// rebuilt commit replaces (rather than reuses) an un-persisted one.
        staged_count: Mutex<u8>,
        /// The last `persist_group_state` snapshot (seeded at construction — the
        /// real engine persists at group create). [`Self::restart`] restores it.
        persisted: Mutex<Option<PersistedGroup>>,
    }
    impl FakeGroup {
        fn with_members(members: &[ActorId]) -> Arc<Self> {
            let members: BTreeSet<[u8; 32]> = members.iter().map(|a| a.0).collect();
            Arc::new(Self {
                members: Mutex::new(members.clone()),
                epoch: Mutex::new(1),
                seals: Mutex::new(Vec::new()),
                pending_removal: Mutex::new(None),
                pending_add: Mutex::new(None),
                pending_commit: Mutex::new(None),
                holds: Mutex::new(true),
                staged_count: Mutex::new(0),
                persisted: Mutex::new(Some(PersistedGroup {
                    members,
                    epoch: 1,
                    pending_removal: None,
                    pending_add: None,
                    pending_commit: None,
                })),
            })
        }

        /// Simulate a process crash + engine reload: a fresh group holding only
        /// what the last `persist_group_state` captured. The staged-bytes
        /// counter carries over so a post-restart rebuild provably produces
        /// DIFFERENT bytes than the pre-crash commit (the fork observable).
        fn restart(&self) -> Arc<Self> {
            let persisted = self
                .persisted
                .lock()
                .unwrap()
                .clone()
                .expect("constructor seeds a persisted snapshot");
            Arc::new(Self {
                members: Mutex::new(persisted.members.clone()),
                epoch: Mutex::new(persisted.epoch),
                seals: Mutex::new(Vec::new()),
                pending_removal: Mutex::new(persisted.pending_removal),
                pending_add: Mutex::new(persisted.pending_add),
                pending_commit: Mutex::new(persisted.pending_commit.clone()),
                holds: Mutex::new(*self.holds.lock().unwrap()),
                staged_count: Mutex::new(*self.staged_count.lock().unwrap()),
                persisted: Mutex::new(Some(persisted)),
            })
        }
    }
    impl FolderGroupCrypto for Arc<FakeGroup> {
        type Error = Infallible;
        fn create_group(
            &self,
            member_key_packages: &[Vec<u8>],
        ) -> Result<CreatedGroup, Infallible> {
            // Test convention: each "key package" IS the member's 32-byte ActorId
            // (what `FakeNest`'s keypackage.fetch returns), so admit them — mirrors
            // the real `create_group` adding the members to the new group.
            {
                let mut members = self.members.lock().unwrap();
                for kp in member_key_packages {
                    if let Ok(id) = <[u8; 32]>::try_from(kp.as_slice()) {
                        members.insert(id);
                    }
                }
            }
            // Deterministic raw id from the admitted kps; the derived channel id is
            // the SAME fold `FakeNest`'s share handler applies, so `share_set`'s
            // mismatch guard agrees (the real engine + nest both BLAKE3 `from_group_id`).
            let raw_group_id = [b"fakegroup-".as_slice(), &member_key_packages.concat()].concat();
            Ok(CreatedGroup {
                channel_id: fake_channel(&raw_group_id),
                raw_group_id,
                welcome: b"fake-welcome".to_vec(),
            })
        }
        fn holds_group(&self, _ch: &[u8; 32]) -> Result<bool, Infallible> {
            Ok(*self.holds.lock().unwrap())
        }
        fn channel_id_for_group(&self, raw_group_id: &[u8]) -> [u8; 32] {
            // The SAME fold `create_group` + `FakeNest`'s share handler apply, so
            // the add path's channel matches the fake nest's derived channel.
            fake_channel(raw_group_id)
        }
        fn add_member_staged(
            &self,
            _ch: &[u8; 32],
            key_package_bytes: &[u8],
        ) -> Result<(Vec<u8>, Vec<u8>), Infallible> {
            // Test convention: the "key package" IS the member's 32-byte ActorId
            // (what `FakeNest`'s keypackage.fetch returns). Stage only — membership
            // + epoch stay put until the merge (mirrors the real staged add).
            let member = <[u8; 32]>::try_from(key_package_bytes)
                .expect("fake key package is a 32-byte actor id");
            *self.pending_add.lock().unwrap() = Some(member);
            let mut n = self.staged_count.lock().unwrap();
            *n += 1;
            // Distinct byte marker from a staged Remove (`0xc0,0x77,n`) so a test
            // can tell an Add commit envelope from a Remove one on the channel.
            let commit = vec![0xADu8, 0x0D, *n];
            *self.pending_commit.lock().unwrap() = Some(commit.clone());
            let welcome = [b"fake-welcome-add-".as_slice(), &member].concat();
            Ok((commit, welcome))
        }
        fn contains_member(&self, _ch: &[u8; 32], member: &ActorId) -> Result<bool, Infallible> {
            Ok(self.members.lock().unwrap().contains(&member.0))
        }
        fn remove_member_staged(
            &self,
            _ch: &[u8; 32],
            member: &ActorId,
        ) -> Result<Option<Vec<u8>>, Infallible> {
            if self.members.lock().unwrap().contains(&member.0) {
                // Staged only: membership + epoch stay put until the merge.
                *self.pending_removal.lock().unwrap() = Some(member.0);
                let mut n = self.staged_count.lock().unwrap();
                *n += 1;
                let bytes = vec![0xc0, 0x77, *n];
                *self.pending_commit.lock().unwrap() = Some(bytes.clone());
                Ok(Some(bytes))
            } else {
                Ok(None) // already absent — no new commit
            }
        }

        fn merge_pending_commit(&self, _ch: &[u8; 32]) -> Result<(), Infallible> {
            // Whichever of the two pendings is staged applies on the merge (openmls
            // holds at most one at a time); the epoch advances either way.
            let mut advanced = false;
            if let Some(m) = self.pending_removal.lock().unwrap().take() {
                self.members.lock().unwrap().remove(&m);
                advanced = true;
            }
            if let Some(m) = self.pending_add.lock().unwrap().take() {
                self.members.lock().unwrap().insert(m);
                advanced = true;
            }
            if advanced {
                *self.epoch.lock().unwrap() += 1; // epoch advances on the merge
            }
            *self.pending_commit.lock().unwrap() = None;
            Ok(())
        }

        fn has_pending_commit(&self, _ch: &[u8; 32]) -> Result<bool, Infallible> {
            Ok(self.pending_removal.lock().unwrap().is_some()
                || self.pending_add.lock().unwrap().is_some())
        }

        fn pending_commit_hash(&self, _ch: &[u8; 32]) -> Result<Option<[u8; 32]>, Infallible> {
            // Mirror the real engine: an identity only while a pending exists.
            if self.pending_removal.lock().unwrap().is_none()
                && self.pending_add.lock().unwrap().is_none()
            {
                return Ok(None);
            }
            Ok(self
                .pending_commit
                .lock()
                .unwrap()
                .as_ref()
                .map(|b| *blake3::hash(b).as_bytes()))
        }

        fn clear_pending_commit(&self, _ch: &[u8; 32]) -> Result<(), Infallible> {
            *self.pending_removal.lock().unwrap() = None; // no-op safe
            *self.pending_add.lock().unwrap() = None;
            *self.pending_commit.lock().unwrap() = None;
            Ok(())
        }

        fn persist_group_state(&self, _ch: &[u8; 32]) -> Result<(), Infallible> {
            *self.persisted.lock().unwrap() = Some(PersistedGroup {
                members: self.members.lock().unwrap().clone(),
                epoch: *self.epoch.lock().unwrap(),
                pending_removal: *self.pending_removal.lock().unwrap(),
                pending_add: *self.pending_add.lock().unwrap(),
                pending_commit: self.pending_commit.lock().unwrap().clone(),
            });
            Ok(())
        }
        fn seal_envelope(
            &self,
            _ch: &[u8; 32],
            payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
        ) -> Result<(Vec<u8>, u64), Infallible> {
            let epoch = *self.epoch.lock().unwrap();
            self.seals
                .lock()
                .unwrap()
                .push((payload.keys.clone(), epoch));
            Ok((payload.encode().unwrap(), epoch))
        }
        fn envelope_epoch(&self, _ch: &[u8; 32]) -> Result<u64, Infallible> {
            Ok(*self.epoch.lock().unwrap())
        }
        fn open_envelope(
            &self,
            _ch: &[u8; 32],
            sealed: &[u8],
        ) -> Result<fauna_core::folder_keys::ContentKeyEnvelopePayload, Infallible> {
            Ok(fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(sealed).unwrap())
        }
    }

    // ── in-memory fake nest (content_key.put + members.evict) ────────

    /// A **transport** fault (`is_rejection() == false`, no `RpcError`) — the
    /// shape of a client that simply died.
    #[derive(Debug)]
    enum FakeErr {
        Crashed,
        /// A **server rejection** carrying its wire code — the shape a coded
        /// refusal (e.g. the nest's `already_bound`) arrives in, so the author's
        /// code-mapping arms can be exercised.
        Rejected(fauna_protocol::RpcError),
    }
    impl core::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Crashed => write!(f, "fake transport fault (client crashed)"),
                Self::Rejected(e) => write!(f, "fake rejection ({})", e.code),
            }
        }
    }
    impl RpcErrorClass for FakeErr {
        fn is_rejection(&self) -> bool {
            matches!(self, Self::Rejected(_))
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            match self {
                Self::Crashed => None,
                Self::Rejected(e) => Some(e),
            }
        }
    }

    #[derive(Default)]
    struct FakeNest {
        /// Number of upcoming custody writes (the store's `merge`) to fail,
        /// modelling a crash at a specific durable write.
        fail_next_config_puts: Mutex<usize>,
        /// The seat's account runtime is still assembling: a plain custody
        /// read answers "not running" at once, while a write and the read a
        /// write starts from wait the assembly out
        /// (`fauna_account_seams::folder_keys`), modelled here as succeeding.
        runtime_assembling: Mutex<bool>,
        /// The folder-key custody the author writes through — this fake IS
        /// the author's [`FolderKeyStore`], so a test can inject a crash at a
        /// custody write through `fail_next_config_puts`.
        custody: crate::key_reader::MemoryFolderKeyStore,
        puts: Mutex<Vec<ContentKeyPutRequest>>,
        evicts: Mutex<Vec<MemberEvictRequest>>,
        /// Recorded `fauna.folders.update` requests (the serve-flag flips).
        updates: Mutex<Vec<fauna_protocol::folders::FolderUpdateRequest>>,
        /// Channels with a live roster row for the (single) test member — so
        /// `evict` can return `evicted = false` on a resume (already gone).
        evicted_already: Mutex<bool>,
        /// Recorded `welcome.deliver` requests (the share flow's member-registration
        /// step) so the `share_set` test asserts the member + channel + kind.
        #[cfg(feature = "mls")]
        welcomes: Mutex<Vec<fauna_protocol::conversations::WelcomeDeliverRequest>>,
        /// Recorded `fauna.folders.set_web_paywall` flag flips — the
        /// `paywall_set` tests assert the name + tier.
        #[cfg(feature = "mls")]
        paywalls: Mutex<Vec<fauna_protocol::folders::FolderSetWebPaywallRequest>>,
        /// Recorded `fauna.capabilities.mint` grant_blob bytes — the minted
        /// `content.read{folder:set}` grants the `paywall_set` tests decode.
        #[cfg(feature = "mls")]
        mints: Mutex<Vec<Vec<u8>>>,
        /// Recorded `fauna.capabilities.renew` requests — the `rotate_paywall_grant`
        /// leg's appended-key re-provision (grant id + the per-generation wraps).
        #[cfg(feature = "mls")]
        renews: Mutex<Vec<RenewGrantRequest>>,
        /// Recorded `fauna.capabilities.revoke` grant ids — the `unpaywall_set` leg.
        #[cfg(feature = "mls")]
        revokes: Mutex<Vec<Vec<u8>>>,
        /// Fail the next N `fauna.capabilities.revoke`s with a transport fault —
        /// a principal twin's revoke refused before the serve-off rotation.
        #[cfg(feature = "mls")]
        fail_next_revokes: Mutex<usize>,
        /// Owned-set summaries the `fauna.folders.list` arm serves — the
        /// automatic paywall re-provision reads `web_paywall_tier` off these.
        /// Empty by default (⇒ nothing paywalled, the hook no-ops).
        #[cfg(feature = "mls")]
        list_summaries: Mutex<Vec<fauna_protocol::folders::FolderSummary>>,
        /// The web-serve holder pubkey the `fauna.bridges.fetch_bridge_pubkey`
        /// arm serves. `None` panics the arm — a test asserting "no discovery
        /// happens" simply leaves it unset.
        #[cfg(feature = "mls")]
        holder_pubkey: Mutex<Option<[u8; 32]>>,
        /// Fail the next N `fauna.capabilities.renew`s with a transport fault —
        /// the removal-must-not-fail-on-a-renew-failure window.
        #[cfg(feature = "mls")]
        fail_next_renews: Mutex<usize>,
        /// Recorded `fauna.conversations.channel.send` requests — the 5d(d)
        /// Remove-commit distribution (`drive_removal` posts the commit envelope
        /// to the set's channel so remaining members advance epochs).
        sends: Mutex<Vec<ChannelSendRequest>>,
        /// Fail the next N `channel.send`s with a transport fault — the
        /// crash-at-the-distribution-send window.
        fail_next_sends: Mutex<usize>,
        /// Recorded `fauna.folders.share` requests, and — derived from them —
        /// the per-set binding + roster the multi-member add-path tests read back:
        /// `list` projects `mls_group_id` for a bound set, and
        /// `members.list_actors` returns each rostered member. A `share` with a
        /// `member_actor_id` rosters that member; the first `share` on a name
        /// records its `group_id` binding.
        #[cfg(feature = "mls")]
        shares: Mutex<Vec<fauna_protocol::folders::FolderShareRequest>>,
        /// Make every `fauna.folders.share` reject with the nest's
        /// `already_bound` — models the benign race where
        /// another device bound the set between this client's `list` and its
        /// share.
        #[cfg(feature = "mls")]
        reject_share_already_bound: Mutex<bool>,
        /// The set's change rows the `fauna.sync.changes.list` arm serves —
        /// the served-era sweep's input; an adopt fills their signature.
        served_rows: Mutex<Vec<fauna_protocol::sync::SyncChange>>,
        /// Recorded `fauna.folders.served_rows.adopt` pages.
        adopts: Mutex<Vec<fauna_protocol::folders::ServedRowsAdoptRequest>>,
        /// Refuse the next N flips OFF `served_rows_unadopted` — a DAV write
        /// landing between the sweep and the flip.
        refuse_next_flips_off: Mutex<usize>,
        /// Every kind requested, in order (the composition's ordering).
        calls: Mutex<Vec<&'static str>>,
        /// Recorded `fauna.bridges.provision_webdav_keys_blob` blobs — the
        /// launch pass's blob reconcile, and `serve_set`'s.
        provisioned: Mutex<Vec<Vec<u8>>>,
    }
    impl RpcRequester for FakeNest {
        type Error = FakeErr;
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
            self.calls.lock().unwrap().push(kind);
            let out: Vec<u8> = match kind {
                "fauna.sync.changes.list" => {
                    fauna_protocol::encode_canonical(&fauna_protocol::sync::SyncChangesListReply {
                        changes: self.served_rows.lock().unwrap().clone(),
                        ..Default::default()
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.folders.served_rows.adopt" => {
                    let req: fauna_protocol::folders::ServedRowsAdoptRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode adopt");
                    let mut rows = self.served_rows.lock().unwrap();
                    let mut adopted = 0;
                    for s in &req.signatures {
                        let row = rows
                            .iter_mut()
                            .find(|r| r.seq == s.seq)
                            .expect("a known seq");
                        if row.signature.is_none() {
                            row.signature = Some(s.signature.clone());
                            row.signer_key = Some(req.signer_key.clone());
                            adopted += 1;
                        }
                    }
                    self.adopts.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::folders::ServedRowsAdoptReply {
                            adopted,
                            remaining: 0,
                            extra: Default::default(),
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                "fauna.conversations.channel.send" => {
                    {
                        let mut left = self.fail_next_sends.lock().unwrap();
                        if *left > 0 {
                            *left -= 1;
                            return Err(FakeErr::Crashed);
                        }
                    }
                    let req: ChannelSendRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode channel.send");
                    let seq = self.sends.lock().unwrap().len() as i64 + 1;
                    self.sends.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(&ChannelSendReply {
                        seq,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.folders.content_key.put" => {
                    let req: ContentKeyPutRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode content_key.put");
                    let channel_id = req.sealed.clone(); // not used; echo a stub
                    self.puts.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(&fauna_protocol::folders::ContentKeyPutReply {
                        ok: true,
                        channel_id: channel_id.chars().take(64).collect(),
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                // What the nest stores: the last envelope put under the name.
                "fauna.folders.content_key.get" => {
                    let req: fauna_protocol::folders::ContentKeyGetRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode content_key.get");
                    let stored = self
                        .puts
                        .lock()
                        .unwrap()
                        .iter()
                        .rev()
                        .find(|p| p.name == req.name)
                        .cloned();
                    let Some(stored) = stored else {
                        return Err(FakeErr::Rejected(fauna_protocol::RpcError::new(
                            "fauna.folders.not_published",
                            "error.folders.not_published",
                        )));
                    };
                    fauna_protocol::encode_canonical(&fauna_protocol::folders::ContentKeyGetReply {
                        epoch: stored.epoch,
                        sealed: stored.sealed,
                        ..Default::default()
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.folders.update" => {
                    let req: fauna_protocol::folders::FolderUpdateRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode update");
                    if req.webdav_enabled == Some(false) {
                        let mut left = self.refuse_next_flips_off.lock().unwrap();
                        if *left > 0 {
                            *left -= 1;
                            return Err(FakeErr::Rejected(fauna_protocol::RpcError::new(
                                fauna_protocol::RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTED,
                                "error.folders.served_rows_unadopted",
                            )));
                        }
                    }
                    self.updates.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(&fauna_protocol::folders::FolderUpdateReply {
                        ok: true,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.folders.members.evict" => {
                    let req: MemberEvictRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode members.evict");
                    self.evicts.lock().unwrap().push(req);
                    // First evict removes the row; a resumed one is a no-op.
                    let already = *self.evicted_already.lock().unwrap();
                    *self.evicted_already.lock().unwrap() = true;
                    fauna_protocol::encode_canonical(&MemberEvictReply {
                        ok: true,
                        channel_id: "00".repeat(32),
                        evicted: !already,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                // ── share flow (5d(b-pre)) — only under the `mls` feature ─────────
                #[cfg(feature = "mls")]
                "fauna.conversations.keypackage.fetch" => {
                    let req: fauna_protocol::conversations::KeypackageFetchRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode kp fetch");
                    // The "key package" is the target's 32-byte ActorId (the FakeGroup
                    // convention); the all-`0xEE` sentinel models a member who has
                    // published no key package (the nest's `None` / HTTP-twin 404).
                    let kp = if req.actor_id.trim() == "ee".repeat(32) {
                        None
                    } else {
                        hex::decode(req.actor_id.trim()).ok()
                    };
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::conversations::KeypackageFetchReply {
                            key_package: kp,
                            extra: Default::default(),
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.folders.share" => {
                    if *self.reject_share_already_bound.lock().unwrap() {
                        return Err(FakeErr::Rejected(fauna_protocol::RpcError::new(
                            "fauna.folders.already_bound",
                            "folder is already bound to a different MLS group",
                        )));
                    }
                    let req: fauna_protocol::folders::FolderShareRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode share");
                    let raw = hex::decode(req.group_id.trim()).expect("hex group id");
                    let name = req.name.clone();
                    self.shares.lock().unwrap().push(req);
                    // Re-derive the channel id the SAME way FakeGroup did (mirrors the
                    // nest's `ChannelId::from_group_id`) so `share_set`'s guard agrees.
                    fauna_protocol::encode_canonical(&fauna_protocol::folders::FolderShareReply {
                        ok: true,
                        folder: name,
                        channel_id: hex::encode(fake_channel(&raw)),
                        ..Default::default()
                    })
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.folders.members.list_actors" => {
                    let req: fauna_protocol::folders::ActorMembersListRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode list_actors");
                    // The roster is every member a `share` on this set named — the
                    // owner-visible "Shared with" list the add-path discriminator
                    // reads (first-share + each subsequent add push a row).
                    let mut members: Vec<fauna_protocol::folders::FolderActorMember> = Vec::new();
                    let mut seen: BTreeSet<String> = BTreeSet::new();
                    for s in self.shares.lock().unwrap().iter() {
                        if s.name_hash != req.name_hash {
                            continue;
                        }
                        if let Some(m) = &s.member_actor_id
                            && seen.insert(m.clone())
                        {
                            members.push(fauna_protocol::folders::FolderActorMember {
                                actor_id: m.clone(),
                                handle: String::new(),
                                role: "member".into(),
                                ..Default::default()
                            });
                        }
                    }
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::folders::ActorMembersListReply {
                            members,
                            ..Default::default()
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.conversations.welcome.deliver" => {
                    let req: fauna_protocol::conversations::WelcomeDeliverRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode welcome");
                    self.welcomes.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::conversations::WelcomeDeliverReply {
                            inbox_id: 42,
                            extra: Default::default(),
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                // ── web-paywall flow (`paywall_set`) — only under `mls` ──────────
                #[cfg(feature = "mls")]
                "fauna.folders.set_web_paywall" => {
                    let req: fauna_protocol::folders::FolderSetWebPaywallRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode set_web_paywall");
                    self.paywalls.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::folders::FolderSetWebPaywallReply {
                            ok: true,
                            extra: Default::default(),
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.capabilities.mint" => {
                    let req: fauna_protocol::wrapped_blob::MintGrantRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode capabilities.mint");
                    self.mints.lock().unwrap().push(req.grant_blob.into_vec());
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::wrapped_blob::MintGrantReply {
                            grant_id: fauna_protocol::ByteBuf::from(vec![0u8; 16]),
                            ok: true,
                            extra: Default::default(),
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.capabilities.renew" => {
                    {
                        let mut left = self.fail_next_renews.lock().unwrap();
                        if *left > 0 {
                            *left -= 1;
                            return Err(FakeErr::Crashed);
                        }
                    }
                    let req: RenewGrantRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode capabilities.renew");
                    self.renews.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(&RenewGrantReply {
                        ok: true,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                // Ungated: the base-feature resume path (`reconcile_set_custody`)
                // lists too. Only the share/paywall projection needs `mls`; without
                // it the double serves an empty listing.
                "fauna.folders.list" => {
                    let _req: fauna_protocol::folders::FoldersListRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode folders.list");
                    // Project `mls_group_id` from the first recorded `share` per set
                    // name (the binding the multi-member add path reads back), then
                    // overlay any manually-seeded `list_summaries` (paywall tests) —
                    // those two sets of names are disjoint in practice.
                    #[cfg(feature = "mls")]
                    let folders = {
                        let mut by_name: std::collections::BTreeMap<
                            String,
                            fauna_protocol::folders::FolderSummary,
                        > = std::collections::BTreeMap::new();
                        for s in self.shares.lock().unwrap().iter() {
                            // A share reaches the nest by hash alone; recover the
                            // plaintext from the names these tests share under,
                            // standing in for the sealed name a real nest serves.
                            let name = ["docs", "premium", "shared", "photos"]
                                .into_iter()
                                .find(|n| fauna_protocol::folders::SetAddressed::addresses(s, n))
                                .unwrap_or_default()
                                .to_string();
                            by_name.entry(name.clone()).or_insert_with(|| {
                                fauna_protocol::folders::FolderSummary {
                                    name,
                                    mls_group_id: Some(s.group_id.clone()),
                                    ..Default::default()
                                }
                            });
                        }
                        for summary in self.list_summaries.lock().unwrap().iter() {
                            by_name.insert(summary.name.clone(), summary.clone());
                        }
                        by_name.into_values().collect::<Vec<_>>()
                    };
                    #[cfg(not(feature = "mls"))]
                    let folders = Vec::<fauna_protocol::folders::FolderSummary>::new();
                    fauna_protocol::encode_canonical(&fauna_protocol::folders::FoldersListReply {
                        folders,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.bridges.fetch_bridge_pubkey" => {
                    let req: FetchBridgePubkeyRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode fetch_bridge_pubkey");
                    assert_eq!(req.bridge_role, "content-processor");
                    assert_eq!(req.bridge_id, "web-serve");
                    let pk = self.holder_pubkey.lock().unwrap().expect(
                        "FakeNest: fetch_bridge_pubkey called but no holder was configured \
                         (a test asserting no-discovery leaves holder_pubkey unset)",
                    );
                    fauna_protocol::encode_canonical(&FetchBridgePubkeyReply {
                        ed25519_pubkey: vec![0u8; 32],
                        x25519_pubkey: pk.to_vec(),
                        mlkem_ek: None,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                #[cfg(feature = "mls")]
                "fauna.capabilities.revoke" => {
                    let req: RevokeGrantRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode capabilities.revoke");
                    {
                        let mut left = self.fail_next_revokes.lock().unwrap();
                        if *left > 0 {
                            *left -= 1;
                            return Err(FakeErr::Crashed);
                        }
                    }
                    self.revokes.lock().unwrap().push(req.grant_id.into_vec());
                    fauna_protocol::encode_canonical(&RevokeGrantReply {
                        ok: true,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                "fauna.bridges.provision_webdav_keys_blob" => {
                    let req: fauna_protocol::wrapped_blob::ProvisionWebdavKeysBlobRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode provision");
                    self.provisioned.lock().unwrap().push(req.blob.into_vec());
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::wrapped_blob::ProvisionReply {
                            ok: true,
                            extra: Default::default(),
                        },
                    )
                    .unwrap()
                    .to_vec()
                }
                other => panic!("FakeNest: unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&out).expect("decode reply"))
        }
    }

    fn member(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    /// Deterministic channel-id fold shared by [`FakeGroup::create_group`] and
    /// `FakeNest`'s share handler — stands in for the real `ChannelId::from_group_id`
    /// so `share_set`'s mismatch guard sees the two sides agree.
    #[cfg_attr(not(feature = "mls"), allow(dead_code))]
    fn fake_channel(raw: &[u8]) -> [u8; 32] {
        let mut c = [0u8; 32];
        for (i, b) in raw.iter().enumerate() {
            c[i % 32] ^= *b;
        }
        c
    }

    #[async_trait::async_trait]
    impl crate::key_reader::FolderKeyReader for FakeNest {
        async fn load(&self) -> anyhow::Result<fauna_core::data::FoldersConfig> {
            if *self.runtime_assembling.lock().unwrap() {
                anyhow::bail!("the account runtime is not running");
            }
            self.custody.load().await
        }
    }

    #[async_trait::async_trait]
    impl FolderKeyStore for FakeNest {
        async fn load_for_write(&self) -> anyhow::Result<fauna_core::data::FoldersConfig> {
            crate::key_reader::FolderKeyReader::load(&self.custody).await
        }

        async fn merge(
            &self,
            replica: fauna_core::data::FoldersConfig,
        ) -> anyhow::Result<fauna_core::data::FoldersConfig> {
            // The injected crash stays strictly ahead of the store, so the
            // "client died mid-write" window this fake models: the write
            // never lands.
            {
                let mut left = self.fail_next_config_puts.lock().unwrap();
                if *left > 0 {
                    *left -= 1;
                    anyhow::bail!("fake custody write fault (client crashed)");
                }
            }
            self.custody.merge(replica).await
        }

        async fn settle_removal(
            &self,
            removal: FolderPendingRemoval,
        ) -> anyhow::Result<fauna_core::data::FoldersConfig> {
            self.custody.settle_removal(removal).await
        }
    }

    type Author = FoldersAuthor<Arc<FakeNest>, Arc<FakeGroup>>;

    fn author(nest: Arc<FakeNest>, group: Arc<FakeGroup>) -> Author {
        author_with_log(nest, group).0
    }

    /// [`author`] plus a handle on the owner's grant log it records in.
    fn author_with_log(
        nest: Arc<FakeNest>,
        group: Arc<FakeGroup>,
    ) -> (
        Author,
        fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
    ) {
        let log = fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
            ActorKeypair::from_secret([0xA0; 32]).actor_id(),
        );
        let author = bare_author(nest, group).with_grant_log(Arc::new(log.clone()));
        (author, log)
    }

    /// An author with no grant log wired.
    fn bare_author(nest: Arc<FakeNest>, group: Arc<FakeGroup>) -> Author {
        let keypair = ActorKeypair::from_secret([0xA0; 32]);
        let files = FoldersClient::new(nest.clone());
        FoldersAuthor::new(
            files,
            keypair,
            nest,
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::with(
                &fauna_core::data::MailConfig {
                    msek: Some([0x5E; 32].into()),
                    ..Default::default()
                },
            )),
            group,
        )
    }

    fn load_cfg(a: &Author) -> fauna_core::data::FoldersConfig {
        block_on(a.custody.load()).expect("reload custody")
    }

    const CH: [u8; 32] = [0x5a; 32];

    #[test]
    fn bind_set_records_genesis_and_publishes_envelope() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());

        block_on(a.bind_set("docs", CH)).expect("bind");

        // Custody holds gen 1; the genesis envelope was published once.
        let cfg = load_cfg(&a);
        let keys = custody::content_keys(&cfg, &CH).expect("keys");
        assert_eq!(keys.current_version(), 1);
        assert!(keys.prior.is_empty());
        let puts = nest.puts.lock().unwrap();
        assert_eq!(puts.len(), 1);
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &puts[0], "docs"
        ));
        assert_eq!(puts[0].epoch, 1, "sealed under the current (genesis) epoch");
        // The fake sealed the gen-1 bundle.
        assert_eq!(group.seals.lock().unwrap()[0].0, keys);
    }

    /// A bind moves the set's NAME to the audience it just created: the name
    /// re-sealed under the genesis content key, pushed by hash. Without it a
    /// member holds the key that opens the set's files but not its name — the
    /// name still rests under the owner's root from the create — and, since a
    /// sealed set rests no plaintext name (schema 114), the set never reaches
    /// the member's Folders page unless an owner engine happens to be bound to
    /// re-stamp it.
    #[test]
    fn bind_set_moves_the_set_name_to_the_genesis_key() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);

        block_on(a.bind_set("docs", CH)).expect("bind");

        let keys = custody::content_keys(&load_cfg(&a), &CH).expect("keys");
        let updates = nest.updates.lock().unwrap();
        let stamp = updates
            .iter()
            .find(|u| u.name_sealed.is_some())
            .expect("the bind stamps the set's sealed name");
        assert!(
            fauna_protocol::folders::SetAddressed::addresses(stamp, "docs")
                && stamp.name.is_empty(),
            "addressed by hash, the plaintext name off the request"
        );
        let root = fauna_core::path_crypto::LabelRoot::content_key(
            *keys.current_key(),
            keys.current_version(),
        );
        assert_eq!(
            stamp.name_sealed.as_ref().map(|b| b.to_vec()),
            fauna_core::label_custody::seal_set_name(&root, "docs").unwrap(),
            "sealed under the genesis generation — the root every member holds"
        );
    }

    /// Ruling (11)(a)/(b): a cut that crashed after its custody write — before
    /// its push and its re-publish — is finished by the next launch pass: the
    /// pick pushed to the nest, and an envelope published that the owner
    /// signed, naming the new nonce, its minter and the lineage. A second pass
    /// finds the published envelope current and publishes nothing.
    #[cfg(feature = "mls")]
    #[test]
    fn a_cut_crashed_before_its_push_is_finished_by_the_next_launch() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        let own = ActorKeypair::from_secret([0xA0; 32]).actor_id();
        let pred = ActorId([0xB0; 32]);
        block_on(crate::key_reader::update(&nest.custody, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(pred), 10);
            custody::key_named_set(cfg, "docs", CH, [7; 32], 10);
        }))
        .unwrap();
        // The nest lists the set under the predecessor's nonce.
        nest.list_summaries
            .lock()
            .unwrap()
            .push(fauna_protocol::folders::FolderSummary {
                name: "docs".into(),
                set_nonce: Some(fauna_protocol::ByteBuf::from(vec![1; 32])),
                ..Default::default()
            });
        // The cut's marker and custody write landed; the process died there.
        block_on(crate::key_reader::FolderKeyStore::record_adoption_marker(
            &nest.custody,
            [1; 32],
        ))
        .unwrap();
        block_on(crate::key_reader::update(&nest.custody, |cfg| {
            custody::remint_owned_entries(cfg, &own, &[[1; 32]], 20, || [2; 32])
        }))
        .unwrap();

        block_on(a.resume_pending_removals()).expect("the launch pass");
        let pushed: Vec<_> = nest
            .updates
            .lock()
            .unwrap()
            .iter()
            .filter_map(|u| u.set_nonce.as_ref().map(|b| b.to_vec()))
            .collect();
        assert_eq!(pushed, vec![vec![2; 32]], "the nest's copy, pushed");
        let puts = nest.puts.lock().unwrap().clone();
        assert_eq!(puts.len(), 1, "the envelope, re-published");
        let blob = hex::decode(&puts[0].sealed).unwrap();
        let signed = fauna_protocol::folder_envelope_sig::verify(&blob, &CH).expect("owner-signed");
        assert_eq!(signed.signer, own);
        let payload =
            fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&signed.sealed).unwrap();
        assert_eq!(payload.set_nonce, Some([2; 32]));
        assert_eq!(payload.minted_by, Some(own));
        assert_eq!(
            payload
                .retired_set_nonces
                .iter()
                .map(|r| (r.nonce, r.minted_by))
                .collect::<Vec<_>>(),
            vec![([1; 32], Some(pred))]
        );

        block_on(a.resume_pending_removals()).expect("the next launch");
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            1,
            "converged — nothing re-published"
        );
    }

    /// An envelope stored behind the group's epoch (a retried sweep advanced
    /// it after the publish) is re-sealed at the next launch.
    #[cfg(feature = "mls")]
    #[test]
    fn an_envelope_behind_the_groups_epoch_is_republished() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        assert_eq!(nest.puts.lock().unwrap().len(), 1);
        block_on(a.resume_pending_removals()).expect("launch");
        assert_eq!(nest.puts.lock().unwrap().len(), 1, "current — left alone");
        *group.epoch.lock().unwrap() += 1;
        block_on(a.resume_pending_removals()).expect("launch");
        let puts = nest.puts.lock().unwrap();
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[1].epoch, 2);
    }

    /// A bound set a predecessor minted and keyed at `CH` under `[7; 32]`,
    /// listed by the nest under the predecessor's nonce, and cut by the
    /// successor (this fake keeps no adoption marker, so the launch reconcile
    /// would defer the cut; it is landed here as the aftermath's arm lands
    /// it). Returns the predecessor.
    #[cfg(feature = "mls")]
    fn inherited_bound_set(nest: &FakeNest) -> ActorId {
        let pred = ActorId([0xB0; 32]);
        let own = ActorKeypair::from_secret([0xA0; 32]).actor_id();
        block_on(crate::key_reader::update(&nest.custody, |cfg| {
            custody::record_created_set(cfg, "docs", [1; 32], Some(pred), 10);
            custody::key_named_set(cfg, "docs", CH, [7; 32], 10);
        }))
        .unwrap();
        nest.list_summaries
            .lock()
            .unwrap()
            .push(fauna_protocol::folders::FolderSummary {
                name: "docs".into(),
                set_nonce: Some(fauna_protocol::ByteBuf::from(vec![1; 32])),
                ..Default::default()
            });
        block_on(crate::key_reader::update(&nest.custody, |cfg| {
            custody::remint_owned_entries(cfg, &own, &[[1; 32]], 20, || [2; 32])
        }))
        .unwrap();
        pred
    }

    /// `succession-aftermath.md` § Re-key scope, the MLS groups row: once the
    /// sweep's remove-old has landed, the successor's launch pass rotates each
    /// inherited bound set's content key and publishes the envelope — so the
    /// custody the retired seed held opens no generation minted after the
    /// succession. A second pass rotates nothing.
    #[cfg(feature = "mls")]
    #[test]
    fn a_succession_rotates_the_inherited_bound_sets_content_key_once() {
        let nest = Arc::new(FakeNest::default());
        // The sweep ran: the predecessor's leaf is off the group.
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        inherited_bound_set(&nest);
        // What the retired seed's custody holds: the set as the ceremony found it.
        let retired_custody = load_cfg(&a);

        block_on(a.resume_pending_removals()).expect("the launch pass");

        let keys = custody::content_keys(&load_cfg(&a), &CH).expect("keys");
        assert_eq!(
            keys.current_version(),
            2,
            "a generation the successor minted"
        );
        assert_eq!(
            keys.prior.iter().map(|g| g.version).collect::<Vec<_>>(),
            vec![1],
            "the predecessor's generation is history, kept"
        );
        let held = custody::content_keys(&retired_custody, &CH).expect("the retired custody");
        assert!(
            held.generations()
                .all(|g| g.version != 2 && g.key != keys.current.key),
            "the retired seed's custody does not open the live generation"
        );
        let puts = nest.puts.lock().unwrap().clone();
        let last = puts.last().expect("the envelope, published");
        assert_eq!(last.current_version, 2, "the floor advances with it");
        let blob = hex::decode(&last.sealed).unwrap();
        let signed = fauna_protocol::folder_envelope_sig::verify(&blob, &CH).expect("owner-signed");
        let payload =
            fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&signed.sealed).unwrap();
        assert_eq!(payload.keys.current_version(), 2);
        assert_eq!(
            payload.set_nonce,
            custody::live_set_nonce(&load_cfg(&a), "docs")
        );

        let published = puts.len();
        block_on(a.resume_pending_removals()).expect("the next launch");
        assert_eq!(
            custody::content_keys(&load_cfg(&a), &CH)
                .unwrap()
                .current_version(),
            2,
            "rotated once per succession"
        );
        assert_eq!(nest.puts.lock().unwrap().len(), published);
    }

    /// The rotation waits for remove-old: while the predecessor's leaf still
    /// sits in the group, an envelope sealed at that epoch would hand the new
    /// generation to the retired seat. The pass after the sweep lands rotates.
    #[cfg(feature = "mls")]
    #[test]
    fn the_succession_rotation_waits_for_the_predecessors_leaf_to_leave() {
        let nest = Arc::new(FakeNest::default());
        let pred = ActorId([0xB0; 32]);
        let group = FakeGroup::with_members(&[pred]);
        let a = author(nest.clone(), group.clone());
        inherited_bound_set(&nest);

        block_on(a.resume_pending_removals()).expect("the launch pass");
        assert_eq!(
            custody::content_keys(&load_cfg(&a), &CH)
                .unwrap()
                .current_version(),
            1,
            "the retired leaf still holds the epoch — nothing minted"
        );

        // The retried sweep's remove-old lands.
        group.members.lock().unwrap().remove(&pred.0);
        *group.epoch.lock().unwrap() += 1;
        block_on(a.resume_pending_removals()).expect("the next launch");
        assert_eq!(
            custody::content_keys(&load_cfg(&a), &CH)
                .unwrap()
                .current_version(),
            2
        );
        let puts = nest.puts.lock().unwrap();
        assert_eq!(
            puts.last().unwrap().epoch,
            2,
            "sealed under the post-remove epoch"
        );
        assert_eq!(puts.last().unwrap().current_version, 2);
    }

    /// A set the launching identity minted itself owes no succession rotation.
    #[cfg(feature = "mls")]
    #[test]
    fn a_set_no_predecessor_minted_is_not_rotated_at_launch() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        block_on(a.bind_set("docs", CH)).expect("bind");
        block_on(a.resume_pending_removals()).expect("launch");
        assert_eq!(
            custody::content_keys(&load_cfg(&a), &CH)
                .unwrap()
                .current_version(),
            1
        );
    }

    #[test]
    fn bind_set_is_idempotent() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        let k1 = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        // A second bind must NOT clobber the live genesis key.
        block_on(a.bind_set("docs", CH)).expect("re-bind");
        let k2 = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        assert_eq!(k1, k2, "genesis key preserved across re-bind");
    }

    #[test]
    fn serve_enable_unshared_records_genesis_and_flags() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let pseudo = serve_custody_channel_id("docs");

        block_on(a.serve_enable("docs", None)).expect("serve-on");

        let cfg = load_cfg(&a);
        let keys = custody::content_keys(&cfg, &pseudo).expect("genesis at pseudo-channel");
        assert_eq!(keys.current_version(), 1);
        let updates = nest.updates.lock().unwrap();
        assert_eq!(updates.len(), 1);
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &updates[0],
            "docs"
        ));
        assert_eq!(updates[0].webdav_enabled, Some(true));
        // No MLS group ⇒ no content-key envelope publish.
        assert!(nest.puts.lock().unwrap().is_empty());
        // Ruling (7)(b)(ii) rule (1): the same custody write stamps the
        // owner's serve-on — the served state every client judgement reads.
        assert!(custody::channel_served(&cfg, &pseudo));
    }

    /// Ruling (7)(b)(ii) rules (1) + (4): serve-on of a SHARED set writes the
    /// stamp in a custody write of its own and publishes the envelope that
    /// carries it to members — before the nest's flag.
    #[test]
    fn serve_enable_shared_stamps_custody_and_publishes_before_the_flag() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(a.bind_set("docs", CH)).expect("bind");
        assert!(
            !custody::channel_served(&load_cfg(&a), &CH),
            "bound ≠ served"
        );
        nest.calls.lock().unwrap().clear();

        block_on(a.serve_enable("docs", Some(CH))).expect("serve-on");

        let cfg = load_cfg(&a);
        assert!(custody::channel_served(&cfg, &CH));
        let puts = nest.puts.lock().unwrap();
        assert_eq!(puts.len(), 2, "the bind's envelope, then the serve-on's");
        let blob = hex::decode(&puts[1].sealed).unwrap();
        let signed = fauna_protocol::folder_envelope_sig::verify(&blob, &CH).unwrap();
        let payload =
            fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&signed.sealed).unwrap();
        assert_eq!(
            (payload.served_at, payload.unserved_at),
            custody::serve_stamps(&cfg, &CH)
        );
        assert!(payload.served_at.is_some());
        let calls = nest.calls.lock().unwrap();
        let put_at = calls
            .iter()
            .position(|k| *k == "fauna.folders.content_key.put")
            .unwrap();
        let flip_at = calls
            .iter()
            .position(|k| *k == "fauna.folders.update")
            .unwrap();
        assert!(put_at < flip_at, "publish before the flag: {calls:?}");
    }

    /// Ruling (7)(b)(ii) rule (3): a roster flagging served a set the owner
    /// never served — keyed by a bind, stamp-less — gets nothing signed and
    /// nothing rotated from `serve_disable`; only the flag is pushed off.
    #[test]
    fn serve_disable_outside_a_served_window_signs_nothing() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(a.update_custody(|cfg| {
            custody::record_created_set(cfg, "docs", SET_NONCE, None, 1);
            custody::key_named_set(cfg, "docs", serve_custody_channel_id("docs"), [7; 32], 2);
        }))
        .unwrap();
        // The rows a lying nest planted while it called the set served.
        let pseudo = fauna_core::label_custody::webdav_pseudo_device_id(&a.keypair.actor_id().0);
        *nest.served_rows.lock().unwrap() = vec![dav_row(1, pseudo, false)];
        nest.calls.lock().unwrap().clear();

        let adoption = block_on(a.serve_disable("docs", None)).expect("serve-off");
        assert_eq!(adoption, ServedRowsAdoption::default());
        assert!(nest.adopts.lock().unwrap().is_empty(), "nothing signed");
        assert!(
            !nest
                .calls
                .lock()
                .unwrap()
                .contains(&"fauna.sync.changes.list"),
            "the sweep never ran"
        );
        assert_eq!(
            nest.updates.lock().unwrap().last().unwrap().webdav_enabled,
            Some(false),
            "the flag is pushed off"
        );
        let cfg = load_cfg(&a);
        let keys = custody::content_keys(&cfg, &serve_custody_channel_id("docs")).unwrap();
        assert_eq!(keys.current_version(), 1, "nothing rotated");
        assert_eq!(
            custody::serve_stamps(&cfg, &serve_custody_channel_id("docs")),
            (None, None),
            "and nothing stamped"
        );
    }

    #[test]
    fn serve_enable_unshared_is_idempotent() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let pseudo = serve_custody_channel_id("docs");

        block_on(a.serve_enable("docs", None)).expect("serve-on");
        let k1 = custody::content_keys(&load_cfg(&a), &pseudo).unwrap();
        block_on(a.serve_enable("docs", None)).expect("re-enable");
        let cfg = load_cfg(&a);
        assert_eq!(
            custody::content_keys(&cfg, &pseudo).unwrap(),
            k1,
            "live content key preserved across a re-enable"
        );
    }

    #[test]
    fn serve_enable_shared_reuses_bind_custody_never_a_second_genesis() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(a.bind_set("docs", CH)).expect("bind");
        let bound = custody::content_keys(&load_cfg(&a), &CH).unwrap();

        block_on(a.serve_enable("docs", Some(CH))).expect("serve-on");

        let cfg = load_cfg(&a);
        assert_eq!(
            custody::content_keys(&cfg, &CH).unwrap(),
            bound,
            "the bind-time genesis stays the served key"
        );
        // No custody appears at the pseudo-channel for a shared set.
        assert!(custody::content_keys(&cfg, &serve_custody_channel_id("docs")).is_none());
        // The serve flip — the bind's own update before it is the name stamp.
        let updates = nest.updates.lock().unwrap();
        let flips: Vec<_> = updates.iter().filter_map(|u| u.webdav_enabled).collect();
        assert_eq!(flips, vec![true]);
    }

    #[test]
    fn serve_enable_shared_without_custody_errors_and_flips_nothing() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));

        let err = block_on(a.serve_enable("docs", Some(CH))).unwrap_err();
        assert!(matches!(err, FoldersAuthorError::NoContentKeys(ch) if ch == CH));
        let cfg = load_cfg(&a);
        assert!(cfg.sets.is_empty(), "no custody conjured");
        assert!(nest.updates.lock().unwrap().is_empty(), "flag untouched");
    }

    #[test]
    fn serve_disable_unshared_flags_off_and_rotates() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let pseudo = serve_custody_channel_id("docs");
        block_on(a.serve_enable("docs", None)).expect("serve-on");

        block_on(a.serve_disable("docs", None)).expect("serve-off");

        let cfg = load_cfg(&a);
        let keys = custody::content_keys(&cfg, &pseudo).unwrap();
        assert_eq!(keys.current_version(), 2, "rotated on disable");
        assert_eq!(keys.prior.len(), 1, "generation 1 retained");
        assert!(
            !custody::channel_served(&cfg, &pseudo),
            "the rotation's write stamps the serve-off"
        );
        let updates = nest.updates.lock().unwrap();
        assert_eq!(updates.last().unwrap().webdav_enabled, Some(false));
        // Unshared ⇒ no envelope republish.
        assert!(nest.puts.lock().unwrap().is_empty());
    }

    #[test]
    fn serve_disable_shared_rotates_and_republishes_envelope() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind"); // envelope put #1
        block_on(a.serve_enable("docs", Some(CH))).expect("serve-on"); // put #2

        block_on(a.serve_disable("docs", Some(CH))).expect("serve-off");

        let cfg = load_cfg(&a);
        let keys = custody::content_keys(&cfg, &CH).unwrap();
        assert_eq!(keys.current_version(), 2, "rotated on disable");
        // The re-published envelope carries the FULL rotated bundle (members keep
        // reading history and receive the new generation) under the unchanged
        // epoch (no MLS commit — membership did not change).
        let seals = group.seals.lock().unwrap();
        let (sealed_keys, epoch) = seals.last().unwrap();
        assert_eq!(sealed_keys, &keys);
        assert_eq!(*epoch, 1, "no epoch advance on serve-off");
        let puts = nest.puts.lock().unwrap();
        assert_eq!(puts.len(), 3, "envelope re-published");
        // Members learn the serve-off from the envelope (rule (4)).
        let blob = hex::decode(&puts[2].sealed).unwrap();
        let signed = fauna_protocol::folder_envelope_sig::verify(&blob, &CH).unwrap();
        let payload =
            fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&signed.sealed).unwrap();
        assert!(payload.unserved_at > payload.served_at);
        assert!(!custody::channel_served(&cfg, &CH));
    }

    #[test]
    fn serve_disable_never_served_set_is_a_safe_noop() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));

        block_on(a.serve_disable("docs", None)).expect("serve-off");

        let cfg = load_cfg(&a);
        assert!(cfg.sets.is_empty(), "no custody conjured");
        // The flag still flips off (idempotent nest-side).
        assert_eq!(nest.updates.lock().unwrap()[0].webdav_enabled, Some(false));
        assert!(nest.puts.lock().unwrap().is_empty());
    }

    const SET_NONCE: [u8; 32] = [0x4e; 32];

    /// One served row in the honest DAV recorder's shape under `device`
    /// (a strict delete when `delete`), its label under generation 1.
    fn dav_row(seq: i64, device: [u8; 32], delete: bool) -> fauna_protocol::sync::SyncChange {
        let label = fauna_core::path_crypto::SealedLabel {
            v: fauna_core::path_crypto::SEALED_LABEL_V1,
            generation: Some(1),
            nonce: None,
            ct: fauna_protocol::ByteBuf::from(vec![seq as u8; 20]),
        };
        fauna_protocol::sync::SyncChange {
            seq,
            path_hash: hex::encode([seq as u8; 32]),
            manifest_hash: (!delete).then(|| hex::encode([0x30 + seq as u8; 32])),
            size_bytes: if delete { 0 } else { 9 },
            change_type: if delete { "delete" } else { "create" }.into(),
            device_id: Some(hex::encode(device)),
            content_key_version: (!delete).then_some(1),
            author_actor_id: Some(hex::encode(device)),
            path_sealed: Some(fauna_protocol::ByteBuf::from(label.to_bytes().unwrap())),
            ..Default::default()
        }
    }

    /// A served set whose custody holds `SET_NONCE`, with rows the sweep must
    /// tell apart: two adoptable pseudo-device rows (a head and a strict
    /// delete), one unstamped pseudo-device row, one row of the owner's own
    /// device, and one pseudo-device row already signed.
    fn served_with_rows(nest: &Arc<FakeNest>, a: &Author) -> [u8; 32] {
        block_on(a.update_custody(|cfg| {
            custody::record_created_set(cfg, "docs", SET_NONCE, None, 1);
        }))
        .unwrap();
        block_on(a.serve_enable("docs", None)).expect("serve-on");
        let owner = a.keypair.actor_id().0;
        let pseudo = fauna_core::label_custody::webdav_pseudo_device_id(&owner);
        let mut unstamped = dav_row(3, pseudo, false);
        unstamped.content_key_version = None;
        let mut signed = dav_row(5, pseudo, false);
        signed.signature = Some(fauna_protocol::ByteBuf::from(vec![1; 64]));
        *nest.served_rows.lock().unwrap() = vec![
            dav_row(1, pseudo, false),
            dav_row(2, pseudo, true),
            unstamped,
            dav_row(4, [0x0d; 32], false),
            signed,
        ];
        owner
    }

    /// Ruling (7)(b) + (i): the sweep signs exactly the adoptable unsigned
    /// pseudo-device rows — each over the shared statement of the row as
    /// served, the owner as actor, the set's nonce — skips the unstamped one,
    /// and the flag falls only after.
    #[test]
    fn serve_disable_adopts_the_served_rows_before_the_flip() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let owner = served_with_rows(&nest, &a);
        nest.calls.lock().unwrap().clear();

        let adoption = block_on(a.serve_disable("docs", None)).expect("serve-off");
        assert_eq!(
            adoption,
            ServedRowsAdoption {
                adopted: 2,
                skipped: 1
            }
        );

        let adopts = nest.adopts.lock().unwrap();
        assert_eq!(adopts.len(), 1, "one page");
        let page = &adopts[0];
        {
            use fauna_protocol::folders::SetAddressed as _;
            assert!(page.addresses("docs"));
            assert!(page.name.is_empty(), "by hash — the name is off the wire");
        }
        assert_eq!(page.signer_key.as_slice(), &owner[..], "a direct signature");
        assert_eq!(
            page.signatures.iter().map(|s| s.seq).collect::<Vec<_>>(),
            vec![1, 2],
            "only the adoptable, unsigned pseudo-device rows"
        );
        let rows = nest.served_rows.lock().unwrap();
        for s in &page.signatures {
            let row = rows.iter().find(|r| r.seq == s.seq).unwrap();
            let statement =
                fauna_protocol::sync_writer_sig::SignedChange::for_row_as(row, SET_NONCE, owner)
                    .unwrap();
            fauna_protocol::sync_writer_sig::verify_statement(
                &statement,
                &s.signature,
                &owner,
                &fauna_protocol::sync_writer_sig::SignerCertCache::new(),
                fauna_core::data::Timestamp::now(),
            )
            .unwrap_or_else(|e| panic!("seq {} verifies under the set nonce: {e}", s.seq));
        }

        let calls = nest.calls.lock().unwrap();
        let adopt_at = calls
            .iter()
            .position(|k| *k == "fauna.folders.served_rows.adopt")
            .unwrap();
        let flip_at = calls
            .iter()
            .position(|k| *k == "fauna.folders.update")
            .unwrap();
        assert!(adopt_at < flip_at, "adopt before the flip: {calls:?}");
        assert_eq!(
            nest.updates.lock().unwrap().last().unwrap().webdav_enabled,
            Some(false)
        );
    }

    /// A DAV write between the sweep and the flip: the nest refuses the flip,
    /// the composition sweeps again and the flip lands; past
    /// `SERVE_DISABLE_ADOPTION_ROUNDS` the refusal surfaces and nothing is
    /// rotated.
    #[test]
    fn serve_disable_retries_a_refused_flip_and_then_surfaces_it() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        served_with_rows(&nest, &a);
        *nest.refuse_next_flips_off.lock().unwrap() = 1;
        nest.calls.lock().unwrap().clear();
        block_on(a.serve_disable("docs", None)).expect("the second round lands");
        let lists = nest
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|k| **k == "fauna.sync.changes.list")
            .count();
        assert_eq!(lists, 2, "one sweep per round");

        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        served_with_rows(&nest, &a);
        *nest.refuse_next_flips_off.lock().unwrap() = 99;
        nest.calls.lock().unwrap().clear();
        let err = block_on(a.serve_disable("docs", None)).unwrap_err();
        assert!(
            matches!(&err, FoldersAuthorError::Transport(FakeErr::Rejected(e))
                if e.code == fauna_protocol::RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTED),
            "{err}"
        );
        let lists = nest
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|k| **k == "fauna.sync.changes.list")
            .count();
        assert_eq!(lists, SERVE_DISABLE_ADOPTION_ROUNDS as usize, "bounded");
        let keys = custody::content_keys(&load_cfg(&a), &serve_custody_channel_id("docs")).unwrap();
        assert_eq!(
            keys.current_version(),
            1,
            "no rotation while the flag stands"
        );
    }

    /// Rows owed and no nonce in custody: nothing is signed, the flag stays.
    #[test]
    fn serve_disable_without_a_set_nonce_signs_nothing_and_keeps_the_flag() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(a.serve_enable("docs", None)).expect("serve-on");
        let pseudo = fauna_core::label_custody::webdav_pseudo_device_id(&a.keypair.actor_id().0);
        *nest.served_rows.lock().unwrap() = vec![dav_row(1, pseudo, false)];

        let err = block_on(a.serve_disable("docs", None)).unwrap_err();
        assert!(matches!(err, FoldersAuthorError::NoSetNonce), "{err}");
        assert!(nest.adopts.lock().unwrap().is_empty());
        assert!(
            nest.updates
                .lock()
                .unwrap()
                .iter()
                .all(|u| u.webdav_enabled != Some(false)),
            "the flag never fell"
        );
    }

    #[test]
    fn bind_set_migrates_serve_custody_to_the_real_channel() {
        // Share-after-serve: the set was WebDAV-served (custody + sentinel at the
        // pseudo-channel, files already stamped under its generations), then the
        // owner shares it. `bind_set` must adopt the served-era history — never
        // mint a second divergent genesis.
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        let pseudo = serve_custody_channel_id("docs");
        block_on(a.serve_enable("docs", None)).expect("serve-on");
        let served = custody::content_keys(&load_cfg(&a), &pseudo).unwrap();

        block_on(a.bind_set("docs", CH)).expect("bind");

        let cfg = load_cfg(&a);
        assert!(
            !cfg.sets
                .iter()
                .any(|s| s.is_live() && s.channel_id == Some(pseudo)),
            "no live entry is left on the pseudo identity"
        );
        let bound = custody::content_keys(&cfg, &CH).expect("migrated");
        assert_eq!(
            bound, served,
            "served-era generations preserved (version stamps stay resolvable)"
        );
        // The published envelope carries exactly the migrated history.
        assert_eq!(group.seals.lock().unwrap()[0].0, bound);
    }

    #[cfg(feature = "mls")]
    #[test]
    fn share_set_creates_group_binds_and_delivers_welcome() {
        use fauna_client_conversations::ConversationsClient;

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]); // empty until the share admits one
        let a = author(nest.clone(), group.clone());
        let convs = ConversationsClient::new(nest.clone());
        let bob = member(2);

        let out = block_on(a.share_set(&convs, "docs", bob, None, None)).expect("share");

        // create_group admitted bob; the nest-derived channel matched the locally
        // created one (the mismatch guard passed → `share_set` returned Ok).
        let expected_raw = [b"fakegroup-".as_slice(), bob.0.as_slice()].concat();
        assert_eq!(out.channel_id, fake_channel(&expected_raw));
        assert_eq!(out.inbox_id, 42, "the welcome's inbox row id is surfaced");
        assert!(
            group.contains_member(&out.channel_id, &bob).unwrap(),
            "share admitted the member to the MLS group"
        );

        // bind_set ran: custody holds gen 1 + the genesis envelope published once.
        let keys = custody::content_keys(&load_cfg(&a), &out.channel_id).expect("keys");
        assert_eq!(keys.current_version(), 1);
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            1,
            "genesis envelope published exactly once"
        );
        // The Welcome went to bob on the derived channel, tagged Folder with the raw
        // group id (routes the recipient away from the chat UI, not `Group`),
        // carrying the create_group welcome bytes (so bob can join).
        let welcomes = nest.welcomes.lock().unwrap();
        assert_eq!(welcomes.len(), 1);
        assert_eq!(welcomes[0].recipient_actor_id, hex::encode(bob.0));
        assert_eq!(welcomes[0].channel_id, hex::encode(out.channel_id));
        assert_eq!(welcomes[0].welcome_bytes, b"fake-welcome");
        assert_eq!(
            welcomes[0].kind,
            fauna_protocol::conversations::WelcomeKind::Folder {
                group_id: hex::encode(&expected_raw)
            }
        );
    }

    /// The nest refuses a share naming a group other
    /// than the set's own (`mls-group-key-material.md` § M2 *Admitting a member*).
    /// Reaching that from the **first-binder** arm is a benign race — another
    /// device bound the set between this client's `list` and its share — so it
    /// surfaces as the retryable `AddDeferred`, not an opaque transport error.
    #[cfg(feature = "mls")]
    #[test]
    fn share_set_maps_the_nests_already_bound_refusal_to_a_retryable_deferral() {
        use fauna_client_conversations::ConversationsClient;

        let nest = Arc::new(FakeNest::default());
        *nest.reject_share_already_bound.lock().unwrap() = true;
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let convs = ConversationsClient::new(nest.clone());

        let err = block_on(a.share_set(&convs, "docs", member(2), None, None))
            .expect_err("the nest refused the bind");
        match err {
            FoldersAuthorError::AddDeferred(msg) => {
                assert!(
                    msg.contains("retry the share"),
                    "the deferral says what to do, got {msg:?}"
                );
            }
            other => panic!("expected a retryable deferral, got {other:?}"),
        }
        assert!(
            nest.welcomes.lock().unwrap().is_empty(),
            "a refused bind delivers no Welcome"
        );
        assert!(
            nest.puts.lock().unwrap().is_empty(),
            "a refused bind publishes no genesis envelope"
        );
    }

    /// The M2 *Admitting a member* fix (`ui/folders.md` § Sharing): sharing an
    /// already-shared set ADDS the newcomer to the set's **existing** MLS group
    /// (keeping the earlier member) instead of minting a fresh group and re-binding
    /// the set. The unit-level twin of the real-nest RED pin
    /// `conformance_shared_folders.rs::sharing_with_a_second_member_keeps_the_first…`.
    #[cfg(feature = "mls")]
    #[test]
    fn share_set_second_member_adds_to_the_existing_group_keeping_the_first() {
        use fauna_client_conversations::ConversationsClient;

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        let convs = ConversationsClient::new(nest.clone());
        let bob = member(2);
        let carol = member(3);

        let first = block_on(a.share_set(&convs, "docs", bob, None, None)).expect("share #1 → bob");
        let epoch_after_first = *group.epoch.lock().unwrap();
        assert_eq!(
            nest.sends.lock().unwrap().len(),
            0,
            "the first share distributes no commit"
        );

        let second =
            block_on(a.share_set(&convs, "docs", carol, None, None)).expect("share #2 → carol");

        // The set stayed on ONE group — carol was ADDED, not re-bound to a fresh
        // group whose roster would exclude bob.
        assert_eq!(
            second.channel_id, first.channel_id,
            "the set stayed on one MLS group"
        );
        assert!(
            group.contains_member(&first.channel_id, &bob).unwrap(),
            "bob kept in the group"
        );
        assert!(
            group.contains_member(&first.channel_id, &carol).unwrap(),
            "carol added"
        );
        assert!(
            *group.epoch.lock().unwrap() > epoch_after_first,
            "the merged Add advanced the epoch (a real membership commit)"
        );

        // Both are on the owner-visible roster (`members.list_actors`).
        let rostered: Vec<String> = block_on(a.files.actor_members_list("docs"))
            .unwrap()
            .members
            .into_iter()
            .map(|m| m.actor_id)
            .collect();
        assert!(rostered.contains(&hex::encode(bob.0)), "bob rostered");
        assert!(rostered.contains(&hex::encode(carol.0)), "carol rostered");

        // Exactly one Add commit was distributed on the channel (Rule-1 ungated).
        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "the Add commit was distributed once");
        assert!(
            matches!(
                ChannelEnvelope::from_bytes(&sends[0].envelope).unwrap(),
                ChannelEnvelope::Commit(_)
            ),
            "an MLS Commit envelope"
        );
        drop(sends);

        // carol's Welcome (the second delivered) rides the EXISTING raw group id and
        // is the ADD welcome (minted inside the Add), not the genesis welcome.
        let expected_raw = [b"fakegroup-".as_slice(), bob.0.as_slice()].concat();
        let welcomes = nest.welcomes.lock().unwrap();
        assert_eq!(welcomes.len(), 2, "bob's (share #1) + carol's (share #2)");
        assert_eq!(welcomes[1].recipient_actor_id, hex::encode(carol.0));
        assert_eq!(welcomes[1].channel_id, hex::encode(first.channel_id));
        assert_eq!(
            welcomes[1].kind,
            fauna_protocol::conversations::WelcomeKind::Folder {
                group_id: hex::encode(&expected_raw)
            }
        );
        assert_eq!(
            welcomes[1].welcome_bytes,
            [b"fake-welcome-add-".as_slice(), carol.0.as_slice()].concat(),
            "carol got the Add-minted Welcome, not the genesis one"
        );
        drop(welcomes);

        // The envelope was re-published under the post-Add epoch BEFORE carol's
        // Welcome (genesis put on #1 + republish on #2 = 2 puts).
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            2,
            "the envelope was republished before carol's Welcome (Q3 ordering)"
        );
    }

    /// Q1 idempotence: re-sharing with a person already on the roster only refreshes
    /// their access row — no MLS Add, no Welcome, no epoch advance, no republish.
    #[cfg(feature = "mls")]
    #[test]
    fn share_set_re_share_to_an_existing_member_only_refreshes_access() {
        use fauna_client_conversations::ConversationsClient;

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        let convs = ConversationsClient::new(nest.clone());
        let bob = member(2);

        let first = block_on(a.share_set(&convs, "docs", bob, None, None)).expect("share #1 → bob");
        let epoch_before = *group.epoch.lock().unwrap();
        let (sends0, welcomes0, puts0) = (
            nest.sends.lock().unwrap().len(),
            nest.welcomes.lock().unwrap().len(),
            nest.puts.lock().unwrap().len(),
        );

        // Re-share to bob (already rostered) with a new access → refresh only.
        let again = block_on(a.share_set(&convs, "docs", bob, None, Some("writer".into())))
            .expect("re-share → bob");

        assert_eq!(again.channel_id, first.channel_id, "same set, same group");
        assert_eq!(
            again.inbox_id, 0,
            "no Welcome delivered on an access refresh"
        );
        assert_eq!(
            nest.sends.lock().unwrap().len(),
            sends0,
            "no Add commit distributed"
        );
        assert_eq!(
            nest.welcomes.lock().unwrap().len(),
            welcomes0,
            "no new Welcome"
        );
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            puts0,
            "no envelope republish"
        );
        assert_eq!(
            *group.epoch.lock().unwrap(),
            epoch_before,
            "epoch unchanged"
        );
        assert_eq!(
            nest.shares
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .access
                .as_deref(),
            Some("writer"),
            "the access row was refreshed via an idempotent re-bind"
        );
    }

    /// Q1 ghost heal: a crashed earlier share can leave a member IN the group but
    /// off the roster (its Welcome never landed). Re-sharing with them evicts the
    /// ghost leaf through the ordinary rotate-on-removal machine, then admits fresh
    /// with a new KeyPackage + Welcome — no launch-time resume machinery.
    #[cfg(feature = "mls")]
    #[test]
    fn share_set_heals_a_ghost_leaf_by_evicting_then_readmitting() {
        use fauna_client_conversations::ConversationsClient;

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        let convs = ConversationsClient::new(nest.clone());
        let bob = member(2);
        let carol = member(3);

        let first = block_on(a.share_set(&convs, "docs", bob, None, None)).expect("share #1 → bob");

        // Simulate a crashed earlier share of carol: she is on a LEAF (in the group)
        // but never rostered — no `share` recorded her, so her Welcome never landed.
        group.members.lock().unwrap().insert(carol.0);
        assert!(
            group.contains_member(&first.channel_id, &carol).unwrap(),
            "carol is a ghost leaf (in-group)"
        );

        let out =
            block_on(a.share_set(&convs, "docs", carol, None, None)).expect("heal + admit carol");

        assert_eq!(out.channel_id, first.channel_id, "same set, same group");
        assert!(
            group.contains_member(&first.channel_id, &carol).unwrap(),
            "carol is re-admitted after the ghost eviction"
        );
        // A Remove (evict the ghost) then an Add (re-admit) were both distributed.
        assert_eq!(
            nest.sends.lock().unwrap().len(),
            2,
            "a Remove (evict ghost) then an Add (re-admit)"
        );
        // carol got a fresh Welcome (the eviction delivers none).
        let welcomes = nest.welcomes.lock().unwrap();
        assert_eq!(
            welcomes.last().unwrap().recipient_actor_id,
            hex::encode(carol.0)
        );
        assert_eq!(out.inbox_id, 42, "carol's fresh Welcome was delivered");
    }

    #[cfg(feature = "mls")]
    #[test]
    fn share_set_errors_when_member_has_no_key_package() {
        use fauna_client_conversations::ConversationsClient;

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        let convs = ConversationsClient::new(nest.clone());

        // member(0xEE) is the FakeNest sentinel whose keypackage.fetch returns None.
        let err = block_on(a.share_set(&convs, "docs", member(0xEE), None, None)).unwrap_err();
        assert!(
            matches!(err, FoldersAuthorError::NoKeyPackage(_)),
            "no KP ⇒ NoKeyPackage, before any group/bind/welcome side effect"
        );
        assert!(
            nest.puts.lock().unwrap().is_empty(),
            "nothing bound/published"
        );
        assert!(
            nest.welcomes.lock().unwrap().is_empty(),
            "no welcome delivered"
        );
    }

    #[test]
    fn remove_member_rotates_evicts_commits_and_seals_under_new_epoch() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        let gen1_key = *custody::content_keys(&load_cfg(&a), &CH)
            .unwrap()
            .current_key();

        let out = block_on(a.remove_member("docs", CH, member(2))).expect("remove");
        assert!(out.rotated);
        assert!(out.evicted, "the member's roster row was removed");
        assert_eq!(
            out.commit,
            Some(vec![0xc0, 0x77, 1]),
            "commit returned for distribution (FakeGroup's 1st staged commit)"
        );

        // Custody rotated to gen 2 with a FRESH key; gen 1 retained (history).
        let keys = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        assert_eq!(keys.current_version(), 2);
        assert_ne!(
            keys.current_key(),
            &gen1_key,
            "fs-m2-fresh: new key independent"
        );
        assert_eq!(keys.key_for(1), Some(&gen1_key), "gen 1 still readable");
        assert!(
            load_cfg(&a).pending_removals.is_empty(),
            "sentinel cleared after commit"
        );

        // The rotation envelope was sealed under the POST-removal
        // epoch (epoch advanced 1 → 2 by the Remove, then the seal recorded 2).
        let seals = group.seals.lock().unwrap();
        let (sealed_keys, sealed_epoch) = seals.last().unwrap();
        assert_eq!(*sealed_epoch, 2, "sealed under the post-removal epoch");
        assert_eq!(
            sealed_keys.current_version(),
            2,
            "the rotated bundle was sealed"
        );
        // The member is gone from the group + the nest got exactly one evict.
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
        assert_eq!(nest.evicts.lock().unwrap().len(), 1);
        assert_eq!(
            nest.evicts.lock().unwrap()[0].member,
            hex::encode(member(2).0)
        );

        // 5d(d): the Remove commit was distributed to the remaining members —
        // exactly one channel.send, addressed to the set's channel, carrying the
        // commit as a `ChannelEnvelope::Commit` (ungated).
        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "one Remove-commit distribution send");
        assert_eq!(sends[0].channel_id, hex::encode(CH));
        assert_eq!(sends[0].expect_no_commit_since, None);
        let env = ChannelEnvelope::from_bytes(&sends[0].envelope).expect("decode envelope");
        assert!(
            matches!(env, ChannelEnvelope::Commit(b) if b == vec![0xc0, 0x77, 1]),
            "the distributed envelope is the MLS Remove commit"
        );
    }

    #[test]
    fn resume_resends_staged_remove_commit_bytes() {
        // Crash AFTER the MLS Remove merged + the commit bytes were persisted
        // onto the sentinel, but BEFORE the channel send: the member is already
        // gone from the group (no new commit on resume), so the resumed drive
        // must re-send the sentinel's STAGED bytes — remaining members would
        // otherwise be stranded at the old epoch.
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1)]); // member(2) already removed
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let staged_commit = vec![0xAB, 0xCD, 0xEF];
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(staged_commit.clone()),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);

        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "the staged commit was re-sent");
        let env = ChannelEnvelope::from_bytes(&sends[0].envelope).expect("decode envelope");
        assert!(matches!(env, ChannelEnvelope::Commit(b) if b == staged_commit));
        // The rotation still committed + the sentinel cleared.
        assert!(load_cfg(&a).pending_removals.is_empty());
    }

    #[test]
    fn crash_at_commit_persist_neither_merges_nor_strands_remaining_members() {
        // The commit-distribution crash window. The MLS Remove commit is
        // **unrecomputable** once merged (the member is off the leaf, so a resumed
        // drive produces no new commit) and remaining members cannot skip epochs —
        // so if the owner's group merges before the bytes are durable, a crash in
        // between strands every remaining member at the old epoch forever, with no
        // detector and no repair. `drive_removal` must therefore treat the merge as
        // the point of no return and cross it only once the bytes are persisted.
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        // A sentinel staged by `remove_member` but whose commit bytes were never
        // written (exactly the state the outer stage leaves behind).
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: None,
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        // The client dies on the durable write of the commit bytes.
        *nest.fail_next_config_puts.lock().unwrap() = 1;
        block_on(a.resume_pending_removals()).expect_err("the drive dies at the persist");

        // Persist-before-merge: the epoch must NOT have advanced past a commit whose
        // bytes never reached durable storage — otherwise the bytes are gone for good.
        assert!(
            group.members.lock().unwrap().contains(&member(2).0),
            "the group merged the Remove commit before its bytes were durable — \
             a crash here loses an unrecomputable commit and strands remaining members"
        );
        assert!(
            nest.sends.lock().unwrap().is_empty(),
            "nothing was distributed before the crash"
        );

        // Next launch: the resumed drive rebuilds (or re-sends) and distributes.
        block_on(a.resume_pending_removals()).expect("resumed drive completes");

        let sends = nest.sends.lock().unwrap();
        assert_eq!(
            sends.len(),
            1,
            "remaining members receive the Remove commit after the crash-resume"
        );
        let env = ChannelEnvelope::from_bytes(&sends[0].envelope).expect("decode envelope");
        assert!(
            // The 2nd staged commit: the 1st was built but never persisted, so the
            // resumed drive cleared it and rebuilt rather than distributing bytes
            // the group had already merged away from.
            matches!(env, ChannelEnvelope::Commit(b) if b == vec![0xc0, 0x77, 2]),
            "the distributed envelope is the *rebuilt* MLS Remove commit"
        );
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
        assert!(
            group.pending_removal.lock().unwrap().is_none(),
            "no pending commit left dangling on the group"
        );
        assert!(load_cfg(&a).pending_removals.is_empty());
    }

    /// The fork pin. A crash left: durable sentinel bytes B1 AND a restored staged
    /// pending for the same member (the state `persist_group_state` preserves
    /// when the crash hit between the sentinel write and the merge — or between
    /// the merge and its persist). The old drive cleared the pending and
    /// REBUILT a second commit B2, overwriting B1 — if B1 had reached the
    /// channel, members sat at N+1(B1) while the owner merged N+1(B2): a hard
    /// fork with no detector, and the durable recovery bytes destroyed. The
    /// resume must instead merge the restored pending and re-send B1 verbatim.
    #[test]
    fn resume_with_restored_pending_merges_and_resends_same_bytes_never_rebuilds() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        // The restored world: pending staged for member(2) + sentinel carrying
        // the bytes that pending produces. The restored pending carries its
        // identity (the engine persists it beside the pending), and here that
        // identity IS blake3(b1) — the sentinel's bytes were produced by this
        // very pending — so the byted-resume foreign-pending guard admits it.
        let b1 = vec![0xB1, 0xB1, 0xB1];
        *group.pending_removal.lock().unwrap() = Some(member(2).0);
        *group.pending_commit.lock().unwrap() = Some(b1.clone());
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(b1.clone()),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);

        // The restored pending merged (member gone, epoch advanced) …
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
        assert_eq!(*group.epoch.lock().unwrap(), 2);
        assert!(group.pending_removal.lock().unwrap().is_none());
        // … the merged state was made durable …
        assert_eq!(group.persisted.lock().unwrap().as_ref().unwrap().epoch, 2);
        // … and the DURABLE bytes were re-sent — never a rebuilt commit.
        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        let env = ChannelEnvelope::from_bytes(&sends[0].envelope).expect("decode envelope");
        assert!(
            matches!(env, ChannelEnvelope::Commit(b) if b == b1),
            "the distributed envelope is the sentinel's B1, not a rebuilt B2 (fork)"
        );
        assert_eq!(
            *group.staged_count.lock().unwrap(),
            0,
            "no fresh commit was ever staged on the resume"
        );
        assert!(load_cfg(&a).pending_removals.is_empty());
    }

    /// The restart-shaped end-to-end of the same window: the drive merges +
    /// persists, then dies at the distribution send. The restarted engine
    /// (rebuilt from the `persist_group_state` snapshot — [`FakeGroup::restart`])
    /// resumes by re-sending the sentinel's bytes; it never rebuilds, and the
    /// rotation completes.
    #[test]
    fn crash_at_send_restart_resumes_by_resending_not_rebuilding() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        // The client dies at the distribution send (post-merge, post-persist).
        *nest.fail_next_sends.lock().unwrap() = 1;
        block_on(a.remove_member("docs", CH, member(2))).expect_err("dies at the send");
        assert!(
            !group.members.lock().unwrap().contains(&member(2).0),
            "the merge crossed the point of no return before the send"
        );
        assert_eq!(
            group.persisted.lock().unwrap().as_ref().unwrap().epoch,
            2,
            "the merged state was durable BEFORE the send (Rule 1 ungated order)"
        );

        // "Restart": a fresh engine holding only the persisted snapshot.
        let restarted = group.restart();
        let a2 = author(nest.clone(), restarted.clone());
        assert_eq!(block_on(a2.resume_pending_removals()).expect("resume"), 1);

        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "exactly one (re-)send after the crash");
        let env = ChannelEnvelope::from_bytes(&sends[0].envelope).expect("decode envelope");
        assert!(
            matches!(env, ChannelEnvelope::Commit(b) if b == vec![0xc0, 0x77, 1]),
            "the re-sent bytes are the ORIGINAL staged commit, not a rebuild"
        );
        assert_eq!(
            *restarted.staged_count.lock().unwrap(),
            1,
            "the restarted engine staged nothing new"
        );
        assert!(load_cfg(&a2).pending_removals.is_empty());
    }

    /// A **poisoned** sentinel: bytes exist, but the engine holds no pending
    /// (never durably persisted — a wasm tab reload, or a cross-device union onto
    /// a peer that never staged it) and the member is still present, so the
    /// commit that could merge those bytes is unrecoverable HERE. The resume must
    /// fail LOUDLY: rebuilding would fork (the bytes may have been distributed),
    /// and sealing would seal the rotated generation under the PRE-removal epoch,
    /// handing it to the removed member. It surfaces the **typed**
    /// [`FoldersAuthorError::PoisonedRemovalSentinel`] (not the generic retryable
    /// `RemovalGate`) so `resume_pending_removals` can isolate it and
    /// [`FoldersAuthor::recover_poisoned_removal`] can discharge it — the
    /// sentinel is retained, never a permanent brick.
    #[test]
    fn poisoned_sentinel_bytes_without_pending_fails_loud_never_rebuilds_or_seals() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        let puts_after_bind = nest.puts.lock().unwrap().len();

        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(vec![0xB1]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        // Gate-less: the drive detects the poison and resume escalates to
        // recovery, which can't walk the log without a gate — so it defers
        // retryably rather than rebuilding blind. Nothing is merged, sealed, or
        // distributed; the sentinel is retained for a gated retry.
        let err = block_on(a.resume_pending_removals()).expect_err("loud failure");
        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(ref m) if m.contains("recovery")),
            "got {err}"
        );
        assert!(
            group.members.lock().unwrap().contains(&member(2).0),
            "no rebuild happened"
        );
        assert!(nest.sends.lock().unwrap().is_empty(), "nothing distributed");
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            puts_after_bind,
            "no envelope published under the pre-removal epoch"
        );
        assert!(
            !load_cfg(&a).pending_removals.is_empty(),
            "sentinel retained for `recover_poisoned_removal` to discharge"
        );
    }

    /// A byted sentinel resume must NOT merge a pending it doesn't own. The
    /// durable-bytes-outrank-the-gate rule routes a
    /// byted sentinel through the ungated merge leg even on the gated plane, where
    /// `gated_remove` may have staged a **different** member's Remove into the
    /// shared engine. Merging that foreign pending while broadcasting THIS
    /// sentinel's bytes forks the group and clears the wrong sentinel. The
    /// engine-durable commit identity lets the resume detect the mismatch and
    /// refuse — never merging, never clearing the foreign pending (it may be a
    /// sibling drive's only merge source).
    #[test]
    fn byted_resume_refuses_to_merge_a_foreign_pending() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2), member(3)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        // A FOREIGN pending sits in the shared engine — member(3)'s Remove, staged
        // by a concurrent (gated) drive. Its identity is blake3 of ITS bytes.
        let foreign_bytes = group
            .remove_member_staged(&CH, &member(3))
            .unwrap()
            .expect("staged member(3)");
        assert!(
            foreign_bytes != vec![0xAA],
            "foreign bytes differ from the sentinel's"
        );

        // The resumed sentinel is member(2)'s, carrying UNRELATED bytes.
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(vec![0xAA]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist byted sentinel");

        let err = block_on(a.resume_pending_removals()).expect_err("foreign pending refused");
        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(ref m) if m.contains("FOREIGN")),
            "got {err}"
        );
        // The foreign pending was NOT merged (member(3) still present, its pending
        // intact) and NOT cleared (it stays the sibling's merge source).
        assert!(
            group.members.lock().unwrap().contains(&member(3).0),
            "the foreign pending must not have been merged as ours"
        );
        assert_eq!(
            *group.pending_removal.lock().unwrap(),
            Some(member(3).0),
            "the foreign pending is left intact for its own drive"
        );
        assert!(nest.sends.lock().unwrap().is_empty(), "nothing distributed");
        assert!(
            !load_cfg(&a).pending_removals.is_empty(),
            "member(2)'s sentinel retained"
        );
    }

    /// A poisoned sentinel, selected first under byted-first ordering, must not
    /// abort the loop before a healthy sentinel gets its pass. Here member(3)'s
    /// removal is healthy (its own pending is durable) and completes; member(2)'s
    /// is poisoned (no pending, still present). With no gate wired, its automatic
    /// recovery can't run (no log walk), so it defers **retryably** — but the
    /// healthy removal still lands, proving one stuck entry never starves another.
    #[test]
    fn resume_isolates_a_poisoned_sentinel_and_completes_the_healthy_one() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2), member(3)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        // Healthy member(3): its restored pending is in the engine, identity
        // matching its sentinel bytes. Staged FIRST so byted-first drives it
        // before the poisoned one.
        let healthy_bytes = group
            .remove_member_staged(&CH, &member(3))
            .unwrap()
            .expect("staged member(3)");
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(3),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(healthy_bytes),
                gated_attempted: true,
            },
        );
        // Poisoned member(2): byted, but no engine pending owns those bytes.
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 2,
                    key: [0x4e; 32].into(),
                    rotated_at: base.rotated_at + 2,
                },
                commit: Some(vec![0xB2]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist both sentinels");

        let err = block_on(a.resume_pending_removals()).expect_err("poison surfaces");
        // Gate-less: the poisoned sentinel defers — a retryable gate error,
        // never a permanent abort. Which deferral surfaces first is the custody
        // fold's canonical staging order: driven before its healthy sibling it
        // meets that sibling's pending in the engine and defers as foreign;
        // driven after, it defers because auto-recovery has no log walk.
        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(ref m)
                if m.contains("recovery") || m.contains("FOREIGN pending")),
            "got {err}"
        );
        // The healthy removal completed despite the poisoned sibling.
        assert!(
            !group.members.lock().unwrap().contains(&member(3).0),
            "the healthy removal landed — its member is gone"
        );
        assert_eq!(
            nest.sends.lock().unwrap().len(),
            1,
            "the healthy commit was distributed"
        );
        let cfg = load_cfg(&a);
        assert!(
            custody::find_pending_removal(&cfg, &CH, &member(3)).is_none(),
            "the healthy sentinel was cleared"
        );
        assert!(
            custody::find_pending_removal(&cfg, &CH, &member(2)).is_some(),
            "the poisoned sentinel is retained for recovery"
        );
    }

    // ── gated-route tests (the FolderCommitGate seam) ───────────────────────

    /// A scripted [`FolderCommitGate`]: returns the configured outcome; on
    /// `Removed` it models what the real rebase loop's accept does to the
    /// engine — member off the leaf, epoch advanced (merge included).
    struct FakeRemovalGate {
        group: Arc<FakeGroup>,
        response: Mutex<GatedRemoval>,
        calls: Mutex<u32>,
        /// The backend channel lock the gate hands the ungated section
        /// (`ungated_channel_lock`) — `None` models a gate-less backend.
        lock: Option<Arc<futures_util::lock::Mutex<()>>>,
    }
    impl FakeRemovalGate {
        fn new(group: &Arc<FakeGroup>, response: GatedRemoval) -> Arc<Self> {
            Arc::new(Self {
                group: group.clone(),
                response: Mutex::new(response),
                calls: Mutex::new(0),
                lock: None,
            })
        }

        fn with_lock(
            group: &Arc<FakeGroup>,
            response: GatedRemoval,
            lock: Arc<futures_util::lock::Mutex<()>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                group: group.clone(),
                response: Mutex::new(response),
                calls: Mutex::new(0),
                lock: Some(lock),
            })
        }
    }
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl FolderCommitGate for FakeRemovalGate {
        fn engaged(&self) -> bool {
            // Mirrors the production adapter: a gate scripted to report
            // `GateNotEngaged` models a wired backend with no live
            // `CommitGate` — the probe sees it as not engaged.
            *self.response.lock().unwrap() != GatedRemoval::GateNotEngaged
        }

        async fn gated_remove(
            &self,
            _channel_id: &[u8; 32],
            member: &ActorId,
        ) -> Result<GatedRemoval, String> {
            *self.calls.lock().unwrap() += 1;
            let r = *self.response.lock().unwrap();
            if r == GatedRemoval::Removed {
                self.group.members.lock().unwrap().remove(&member.0);
                *self.group.epoch.lock().unwrap() += 1;
            }
            Ok(r)
        }

        async fn gated_add(
            &self,
            _channel_id: &[u8; 32],
            key_package_bytes: &[u8],
        ) -> Result<GatedAdd, String> {
            // `drive_add` only calls this on an engaged gate, so model the accept:
            // the newcomer joins the group and the epoch advances (what the real
            // rebase loop's accepted+merged Add does). The fake "key package" is
            // the member's 32-byte ActorId (the `FakeNest` convention).
            *self.calls.lock().unwrap() += 1;
            let member = <[u8; 32]>::try_from(key_package_bytes)
                .map_err(|_| "fake key package is a 32-byte actor id".to_string())?;
            self.group.members.lock().unwrap().insert(member);
            *self.group.epoch.lock().unwrap() += 1;
            Ok(GatedAdd::Added(
                [b"fake-welcome-add-".as_slice(), &member].concat(),
            ))
        }

        fn ungated_channel_lock(
            &self,
            _channel_id: &[u8; 32],
        ) -> Option<Arc<futures_util::lock::Mutex<()>>> {
            self.lock.clone()
        }
    }

    /// Stage a poisoned member(2) sentinel: byted, but the engine holds no
    /// matching pending and the member is still present — the state
    /// `resume_pending_removals` surfaces as
    /// [`FoldersAuthorError::PoisonedRemovalSentinel`].
    fn stage_poisoned_member2(a: &Author) {
        let mut cfg = load_cfg(a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(vec![0xB2]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist poisoned sentinel");
    }

    /// Stage a **byte-less** member(2) sentinel with the given `gated_attempted`
    /// stamp. `true` is the state an interrupted **gated** attempt leaves
    /// behind (the drive stamped the sentinel before the gate ran; the gated
    /// route distributes its commit inside the rebase loop and records **no
    /// bytes**, so a crash between the CAS-put and the rotation-publish leaves
    /// exactly this — a sentinel whose commit may or may not have reached the
    /// channel log; only the gate's walk-to-head can tell, so it defers). `false` is a
    /// sentinel no engaged gated attempt ever started for — provably
    /// undistributed, the one shape safe to rebuild ungated.
    fn stage_byteless_member2(a: &Author, gated_attempted: bool) {
        let mut cfg = load_cfg(a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: None,
                gated_attempted,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist byte-less sentinel");
    }

    /// The **launch resume** must not rebuild a byte-less sentinel when the gate is
    /// wired but never engaged (`RestoreRetryEnd::Failed` — a
    /// replica restore failure). The sentinel may have been staged by an earlier
    /// *gated* attempt that already distributed its commit inside the rebase loop
    /// (recording no bytes), and with no walk-to-head we cannot tell — so building a
    /// fresh commit here could **fork the group**. Defer retryably; the sentinel
    /// stays staged for a later, wired launch.
    ///
    /// Before the fix the resume took the `NoGate` arm and drove the ungated
    /// stage → merge → send: no walk-to-head, no `PendingUnconverged` check.
    #[test]
    fn resume_defers_a_byteless_sentinel_when_the_gate_is_wired_but_not_engaged() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::GateNotEngaged);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        stage_byteless_member2(&a, true);

        let err = block_on(a.resume_pending_removals()).expect_err("deferred, not rebuilt");

        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(_)),
            "a retryable gate deferral, got {err}"
        );
        assert!(
            group.members.lock().unwrap().contains(&member(2).0),
            "no blind rebuild — the group is untouched"
        );
        assert!(
            nest.sends.lock().unwrap().is_empty(),
            "nothing was distributed — a fresh commit here could fork the group"
        );
        assert!(
            !load_cfg(&a).pending_removals.is_empty(),
            "the sentinel is retained for a later, wired launch"
        );
    }

    /// The same fork hazard on the **foreground** path — wider than the launch
    /// resume the review scoped it to. `remove_member` *resumes* an already-staged
    /// sentinel for its `(channel, member)` rather than re-staging (see its
    /// `find_pending_removal` arm), so a user who simply clicks Remove again after
    /// an interrupted gated attempt reaches the identical blind rebuild — with no
    /// launch and no crash-recovery involved. It must defer too.
    #[test]
    fn remove_member_defers_a_resumed_byteless_sentinel_when_gate_not_engaged() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::GateNotEngaged);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        stage_byteless_member2(&a, true);

        let err = block_on(a.remove_member("docs", CH, member(2))).expect_err("deferred");

        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(_)),
            "a retryable gate deferral, got {err}"
        );
        assert!(
            group.members.lock().unwrap().contains(&member(2).0),
            "no blind rebuild on the foreground path either"
        );
        assert!(nest.sends.lock().unwrap().is_empty(), "nothing distributed");
        assert!(
            !load_cfg(&a).pending_removals.is_empty(),
            "the sentinel is retained"
        );
    }

    /// On a nest whose `fauna.mls` plane never engages, the gate is
    /// `GateNotEngaged` for the life of the nest — so a byte-less sentinel that
    /// deferred unconditionally could never complete: the member is never
    /// evicted, the content key never rotates, and the removed member keeps the
    /// live key with no client action able to recover it (the
    /// no-client-causable-unrecoverable-state invariant). But a sentinel still
    /// stamped `gated_attempted: false` is **provably undistributed** —
    /// only an engaged gated attempt can distribute without recording bytes,
    /// and none ever started — so the resume must rebuild it ungated (the
    /// single-device fallback) and complete the removal.
    ///
    /// Byte-less-but-unstamped is cheap to produce with no crash: the fresh
    /// drive CAS-persists the sentinel, then fails transiently on the durable
    /// write of the commit bytes — before anything merged or sent.
    #[test]
    fn resume_rebuilds_a_byteless_sentinel_never_gate_attempted() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::GateNotEngaged);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        stage_byteless_member2(&a, false);

        assert_eq!(
            block_on(a.resume_pending_removals()).expect("rebuilt, not bricked"),
            1,
        );
        assert_eq!(
            *gate.calls.lock().unwrap(),
            0,
            "the gate was never engaged — the ungated fallback drove it"
        );
        assert!(
            !group.members.lock().unwrap().contains(&member(2).0),
            "the member is removed"
        );
        assert!(
            !nest.sends.lock().unwrap().is_empty(),
            "the rebuilt Remove commit was distributed"
        );
        assert!(
            load_cfg(&a).pending_removals.is_empty(),
            "the sentinel is cleared — the removal completed on the plane-less nest"
        );
    }

    /// The stamp that makes the rebuild above safe: the drive's gated leg
    /// durably writes `gated_attempted = true` BEFORE the gate runs —
    /// so even when the gated attempt dies mid-flight (here: the gate errors),
    /// the sentinel already records that a distribute-capable attempt started,
    /// and every later gate-less resume defers instead of rebuilding blind.
    #[test]
    fn drive_stamps_gated_attempted_durably_before_the_gate_runs() {
        struct FailingGate;
        #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
        #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
        impl FolderCommitGate for FailingGate {
            fn engaged(&self) -> bool {
                true
            }
            async fn gated_remove(
                &self,
                _channel_id: &[u8; 32],
                _member: &ActorId,
            ) -> Result<GatedRemoval, String> {
                Err("gate died mid-rebase (models a crash after a possible send)".into())
            }
            async fn gated_add(
                &self,
                _channel_id: &[u8; 32],
                _key_package_bytes: &[u8],
            ) -> Result<GatedAdd, String> {
                Err("gate died mid-rebase (models a crash after a possible send)".into())
            }
        }

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone()).with_commit_gate(Arc::new(FailingGate));
        block_on(a.bind_set("docs", CH)).expect("bind");

        block_on(a.remove_member("docs", CH, member(2))).expect_err("the gate died");

        let cfg = load_cfg(&a);
        let sentinel = custody::find_pending_removal(&cfg, &CH, &member(2))
            .expect("the sentinel is retained for resume");
        assert!(
            sentinel.gated_attempted,
            "stamped before the gate ran — a later gate-less resume must defer"
        );
    }

    /// Per-entry error isolation (part b — recovery): `recover_poisoned_removal` discharges a
    /// poisoned sentinel by routing it through the gate's walk-to-head. Here the
    /// walk finds the member still present (the commit was never distributed), so
    /// the gate re-stages a fresh Remove (`Removed`) and the recovery finishes the
    /// rotation the poisoned drive never published — the member is evicted, the
    /// generation is committed, and the sentinel is cleared. No SSH, no DB
    /// surgery: the client calls one method.
    #[test]
    fn recover_poisoned_removal_restages_via_gate_and_finishes_rotation() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::Removed);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        let puts_after_bind = nest.puts.lock().unwrap().len();
        stage_poisoned_member2(&a);
        assert!(group.members.lock().unwrap().contains(&member(2).0));

        // Recovery routes through the gate's walk-to-head and completes.
        let out = block_on(a.recover_poisoned_removal(CH, member(2))).expect("recovered");
        assert_eq!(
            *gate.calls.lock().unwrap(),
            1,
            "the gate walked the log to head"
        );
        assert!(out.rotated && out.evicted);
        assert!(
            !group.members.lock().unwrap().contains(&member(2).0),
            "the member is gone after recovery"
        );
        assert_eq!(
            custody::content_keys(&load_cfg(&a), &CH)
                .unwrap()
                .current_version(),
            2,
            "the rotation was committed",
        );
        assert!(
            nest.puts.lock().unwrap().len() > puts_after_bind,
            "the rotated envelope was published"
        );
        assert!(
            load_cfg(&a).pending_removals.is_empty(),
            "the sentinel is cleared — no longer a brick"
        );
    }

    /// The invariant discharge in practice: on a **gated** plane (every real
    /// deployment — native/linux/web all wire the gate), the launch-time
    /// `resume_pending_removals` auto-recovers a poisoned sentinel through the
    /// gate with no user step. The member is removed and the sentinel cleared just
    /// by the client starting up — the poisoned state is never a permanent brick.
    #[test]
    fn resume_auto_recovers_a_poisoned_sentinel_when_gated() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::Removed);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        stage_poisoned_member2(&a);

        // No explicit recovery call — a plain launch resume discharges it.
        assert_eq!(
            block_on(a.resume_pending_removals()).expect("auto-recovered"),
            1,
        );
        assert_eq!(
            *gate.calls.lock().unwrap(),
            1,
            "recovery walked the log via the gate"
        );
        assert!(
            !group.members.lock().unwrap().contains(&member(2).0),
            "the member is gone after the launch resume"
        );
        assert!(
            load_cfg(&a).pending_removals.is_empty(),
            "the poisoned sentinel cleared itself on launch",
        );
    }

    /// Recovery is idempotent: a sentinel already resolved (recovered earlier, or
    /// the member left) is a no-op, not an error — a client may call it freely.
    #[test]
    fn recover_poisoned_removal_is_a_noop_when_already_resolved() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::Removed);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let out = block_on(a.recover_poisoned_removal(CH, member(2))).expect("no-op");
        assert!(!out.rotated && !out.evicted, "nothing to do");
        assert_eq!(
            *gate.calls.lock().unwrap(),
            0,
            "the gate was never consulted"
        );
    }

    /// Recovery needs the gate's log walk to be safe (it is what proves the stale
    /// bytes were never distributed). When the gate is wired but not engaged
    /// ([`GatedRemoval::GateNotEngaged`] — the replica plane is still restoring,
    /// or its load failed), recovery defers with a retryable error and leaves the
    /// sentinel staged, never rebuilding blind.
    #[test]
    fn recover_poisoned_removal_defers_when_gate_not_engaged() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::GateNotEngaged);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        stage_poisoned_member2(&a);

        let err = block_on(a.recover_poisoned_removal(CH, member(2))).expect_err("deferred");
        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(_)),
            "got {err}"
        );
        assert!(
            group.members.lock().unwrap().contains(&member(2).0),
            "no blind rebuild — member untouched"
        );
        assert!(nest.sends.lock().unwrap().is_empty(), "nothing distributed");
        assert!(
            !load_cfg(&a).pending_removals.is_empty(),
            "the sentinel is retained for a later retry"
        );
    }

    /// The gated route (LEAD ②): the Remove commit rides the rebase loop, so
    /// the drive performs NO ungated send and records NO sentinel bytes — the
    /// provider replica is the durable carrier. The rotation remainder (seal
    /// under the post-removal epoch, publish, evict, custody commit) still runs.
    #[test]
    fn gated_removal_skips_ungated_send_and_sentinel_bytes_but_completes_rotation() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::Removed);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let out = block_on(a.remove_member("docs", CH, member(2))).expect("gated remove");

        assert_eq!(*gate.calls.lock().unwrap(), 1);
        assert!(out.rotated);
        assert!(out.evicted);
        assert_eq!(
            out.commit, None,
            "the gated commit is distributed inside the rebase loop, not re-surfaced"
        );
        assert!(
            nest.sends.lock().unwrap().is_empty(),
            "NO ungated channel.send — the gate-send happened inside the loop"
        );
        assert_eq!(
            *group.staged_count.lock().unwrap(),
            0,
            "the ungated staged path never ran"
        );
        // Rotation remainder intact: sealed under the post-removal epoch,
        // custody committed, sentinel cleared.
        let seals = group.seals.lock().unwrap();
        let (sealed_keys, sealed_epoch) = seals.last().unwrap();
        assert_eq!(*sealed_epoch, 2, "sealed under the post-removal epoch");
        assert_eq!(sealed_keys.current_version(), 2);
        let keys = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        assert_eq!(keys.current_version(), 2);
        assert!(load_cfg(&a).pending_removals.is_empty());
        assert_eq!(nest.evicts.lock().unwrap().len(), 1);
    }

    /// [`GatedRemoval::PendingUnconverged`] defers the whole drive: the staged
    /// rotation sentinel is retained (a later resume completes it), nothing is
    /// sent, sealed, or evicted, and the member is untouched.
    #[test]
    fn gated_pending_unconverged_defers_and_retains_sentinel() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::PendingUnconverged);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate);
        block_on(a.bind_set("docs", CH)).expect("bind");
        let puts_after_bind = nest.puts.lock().unwrap().len();

        let err = block_on(a.remove_member("docs", CH, member(2))).expect_err("deferred");
        assert!(matches!(err, FoldersAuthorError::RemovalGate(_)));

        assert!(group.members.lock().unwrap().contains(&member(2).0));
        assert!(nest.sends.lock().unwrap().is_empty());
        assert_eq!(nest.puts.lock().unwrap().len(), puts_after_bind);
        assert!(nest.evicts.lock().unwrap().is_empty());
        let cfg = load_cfg(&a);
        assert_eq!(
            cfg.pending_removals.len(),
            1,
            "the rotation sentinel (with its irrecoverable fresh key) is retained"
        );
        // A later resume — once converged — completes it.
        *nest.sends.lock().unwrap() = Vec::new();
        let gate2 = FakeRemovalGate::new(&group, GatedRemoval::Removed);
        let a2 = author(nest.clone(), group.clone()).with_commit_gate(gate2);
        assert_eq!(block_on(a2.resume_pending_removals()).expect("resume"), 1);
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
        assert!(load_cfg(&a2).pending_removals.is_empty());
    }

    /// **The single-device / no-plane fallback must survive the fork guard.** A
    /// **freshly staged** removal on a wired-but-not-engaged gate
    /// ([`GatedRemoval::GateNotEngaged`] — e.g. a replica restore
    /// failure, which degrades to single-device) still falls back to the ungated
    /// staged discipline: staged in this very call, its `gated_attempted` is
    /// `false` — no engaged gated attempt ever started for it, so nothing
    /// can have been distributed, the rebuild is not blind, and deferring it
    /// would strand the user with no way to remove a member at all.
    ///
    /// This is the regression guard on the `GateNotEngaged` defer below — the
    /// defer keys on the sentinel's **`gated_attempted` stamp**, not merely on
    /// the gate's state.
    #[test]
    fn fresh_removal_falls_back_to_ungated_when_gate_not_engaged() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::GateNotEngaged);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let out = block_on(a.remove_member("docs", CH, member(2))).expect("ungated fallback");

        assert_eq!(
            *gate.calls.lock().unwrap(),
            0,
            "a not-engaged gate is probed, never run — nothing to distribute"
        );
        assert_eq!(
            out.commit,
            Some(vec![0xc0, 0x77, 1]),
            "the ungated staged path built + returned the commit"
        );
        assert_eq!(
            nest.sends.lock().unwrap().len(),
            1,
            "the ungated distribution send ran"
        );
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
    }

    #[test]
    fn resume_without_staged_commit_bytes_completes_without_send() {
        // A byte-less sentinel (no commit bytes) whose member is already gone:
        // nothing to distribute — the resume still completes the rotation
        // (publish + evict + custody commit) rather than erroring.
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1)]); // member(2) already removed
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x4e; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: None,
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);
        assert!(nest.sends.lock().unwrap().is_empty(), "nothing to re-send");
        assert!(load_cfg(&a).pending_removals.is_empty());
    }

    /// The launch resume fires as soon as the MLS restore has wired the gate,
    /// which on a seat whose account runtime is still assembling (web's tab at
    /// every launch) is before custody is readable by a plain read. The resume
    /// is a custody writer — it drives each staging to its settle — so it
    /// reads as a write does and waits the assembly out, rather than failing
    /// "not running" and leaving the removed member keyed until a launch that
    /// happens to win the race.
    #[test]
    fn resume_before_the_runtime_assembles_waits_rather_than_skipping_the_launch() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1)]); // member(2) already removed
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x4e; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: None,
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        // The relaunch: the resume is due before the runtime has assembled.
        *nest.runtime_assembling.lock().unwrap() = true;
        assert_eq!(
            block_on(a.resume_pending_removals()).expect("the resume waits for the runtime"),
            1
        );
        *nest.runtime_assembling.lock().unwrap() = false;
        let after = load_cfg(&a);
        assert!(after.pending_removals.is_empty(), "the staging settled");
        assert_eq!(
            custody::current_generation(&after, &CH).unwrap().version,
            base.version + 1,
            "the rotation the removal staged is committed"
        );
    }

    #[test]
    fn remove_absent_member_is_a_noop_without_rotating() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let out = block_on(a.remove_member("docs", CH, member(9))).expect("noop");
        assert!(!out.rotated && !out.evicted && out.commit.is_none());
        // No rotation, no extra publish (only the genesis put), no evict.
        assert_eq!(
            custody::content_keys(&load_cfg(&a), &CH)
                .unwrap()
                .current_version(),
            1
        );
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            1,
            "only the genesis publish"
        );
        assert!(nest.evicts.lock().unwrap().is_empty());
    }

    #[test]
    fn resume_drives_staged_removal_with_the_irrecoverable_key() {
        // Crash AFTER staging the fresh key but BEFORE the publish: stage + persist
        // by hand, then resume.
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let staged_key = [0x5A; 32];
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: staged_key.into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: None,
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);

        // The committed current MUST be the staged (irrecoverable) key.
        let keys = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        assert_eq!(keys.current_key(), &staged_key);
        assert_eq!(keys.current_version(), 2);
        assert!(load_cfg(&a).pending_removals.is_empty());
        // The Remove + evict ran.
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
        assert_eq!(nest.evicts.lock().unwrap().len(), 1);
    }

    #[test]
    fn resume_treats_already_removed_member_idempotently() {
        // Crash AFTER the publish + MLS Remove (member already gone from group +
        // nest roster) but BEFORE the local commit: resume must still commit the
        // staged key locally, not error.
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1)]); // member(2) already gone
        *nest.evicted_already.lock().unwrap() = true; // nest roster already evicted
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let staged_key = [0x7C; 32];
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: staged_key.into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: None,
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);
        let keys = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        assert_eq!(keys.current_key(), &staged_key, "staged key committed");
        assert!(load_cfg(&a).pending_removals.is_empty());
        // The MLS Remove was a no-op (already gone) → no new commit; evict no-op.
        let out_evict = nest.evicts.lock().unwrap();
        assert_eq!(out_evict.len(), 1, "evict still re-driven (idempotent)");
    }

    /// A sentinel carrying durable commit bytes resumes through the UNGATED leg without
    /// consulting the gate. The modelled crash window: the ungated drive merged
    /// (member absent, epoch advanced, no pending) but the distribution send
    /// failed. The gate would report `AlreadyAbsent` and silently discard the
    /// bytes — stranding the remaining members at the pre-removal epoch; the
    /// resume must re-send them verbatim instead.
    #[test]
    fn byted_sentinel_resumes_ungated_without_consulting_gate() {
        let nest = Arc::new(FakeNest::default());
        // member(2) already merged out (post-merge, pre-send crash state).
        let group = FakeGroup::with_members(&[member(1)]);
        let gate = FakeRemovalGate::new(&group, GatedRemoval::AlreadyAbsent);
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(vec![0xB1]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist byted sentinel");

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 1);

        assert_eq!(
            *gate.calls.lock().unwrap(),
            0,
            "durable bytes outrank the gate — it must not be consulted"
        );
        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "the durable bytes were re-sent");
        assert_eq!(
            sends[0].envelope,
            ChannelEnvelope::Commit(vec![0xB1]).to_bytes().unwrap(),
            "re-sent VERBATIM — never rebuilt"
        );
        drop(sends);
        // The rotation remainder completed and the sentinel is gone.
        let keys = custody::content_keys(&load_cfg(&a), &CH).unwrap();
        assert_eq!(keys.current_version(), 2);
        assert!(load_cfg(&a).pending_removals.is_empty());
    }

    /// A fresh ungated build defers while a BYTED sibling sentinel exists on the
    /// channel — the engine's live restored pending may be that sibling's merge
    /// source, and both `clear_pending_commit` and staging over it would destroy
    /// it (stranding the sibling in the fail-loud arm permanently).
    #[test]
    fn fresh_build_defers_while_byted_sibling_sentinel_exists() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2), member(3)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        // Byted sibling: member(2)'s interrupted drive left durable bytes AND
        // the engine's restored staged pending (crash between CAS and merge).
        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(vec![0xB1]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist byted sibling");
        // The sibling's restored pending carries its identity (blake3 of its own
        // sentinel bytes) — the fresh member(3) build defers before ever reaching
        // a merge, but keep the restored-pending model honest.
        *group.pending_removal.lock().unwrap() = Some(member(2).0);
        *group.pending_commit.lock().unwrap() = Some(vec![0xB1]);

        let err = block_on(a.remove_member("docs", CH, member(3))).expect_err("deferred");
        assert!(
            matches!(err, FoldersAuthorError::RemovalGate(_)),
            "got {err}"
        );
        assert_eq!(
            *group.pending_removal.lock().unwrap(),
            Some(member(2).0),
            "the sibling's restored pending was NOT cleared"
        );
        assert_eq!(
            *group.staged_count.lock().unwrap(),
            0,
            "no fresh commit was staged over the sibling's pending"
        );
        assert!(nest.sends.lock().unwrap().is_empty(), "nothing distributed");
        let cfg = load_cfg(&a);
        assert_eq!(
            cfg.pending_removals.len(),
            2,
            "both sentinels retained (the deferral is retryable via resume)"
        );
    }

    /// `resume_pending_removals` drives BYTED
    /// sentinels before byte-less siblings, so the byted one merges its own
    /// restored pending + re-sends its bytes, then the byte-less sibling
    /// rebuilds fresh — instead of FIFO destroying the byted sibling's pending
    /// (or, with the guard, deadlocking the loop on the deferral error).
    #[test]
    fn resume_completes_byted_sentinel_before_byteless_sibling() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2), member(3)]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");

        let mut cfg = load_cfg(&a);
        let base = custody::current_generation(&cfg, &CH).unwrap();
        // FIFO-first: byte-less member(3) sentinel (interrupted before its
        // ungated stage).
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(3),
                new_generation: ContentKeyGeneration {
                    version: base.version + 2,
                    key: [0x4e; 32].into(),
                    rotated_at: base.rotated_at + 2,
                },
                commit: None,
                gated_attempted: true,
            },
        );
        // FIFO-second: BYTED member(2) sentinel whose restored pending the
        // engine still holds (crash between CAS and merge).
        custody::stage_pending_removal(
            &mut cfg,
            FolderPendingRemoval {
                channel_id: CH,
                name: "docs".into(),
                removed_member: member(2),
                new_generation: ContentKeyGeneration {
                    version: base.version + 1,
                    key: [0x3d; 32].into(),
                    rotated_at: base.rotated_at + 1,
                },
                commit: Some(vec![0xB1]),
                gated_attempted: true,
            },
        );
        block_on(a.custody.merge(cfg.clone())).expect("persist both sentinels");
        // The restored pending for member(2) carries its identity = blake3 of the
        // sentinel's own bytes, so the byted-resume merge guard admits it.
        *group.pending_removal.lock().unwrap() = Some(member(2).0);
        *group.pending_commit.lock().unwrap() = Some(vec![0xB1]);

        assert_eq!(block_on(a.resume_pending_removals()).expect("resume"), 2);

        let sends = nest.sends.lock().unwrap();
        assert_eq!(sends.len(), 2, "both removals distributed");
        assert_eq!(
            sends[0].envelope,
            ChannelEnvelope::Commit(vec![0xB1]).to_bytes().unwrap(),
            "the BYTED sentinel's bytes went first (its pending merged, not cleared)"
        );
        drop(sends);
        let members = group.members.lock().unwrap();
        assert!(
            !members.contains(&member(2).0) && !members.contains(&member(3).0),
            "both members removed"
        );
        drop(members);
        assert!(load_cfg(&a).pending_removals.is_empty());
    }

    /// The ungated commit section runs under the
    /// gate's backend channel lock — the same lock the background folder poll
    /// merges inbound commits under — so a foreign merge cannot drop the staged
    /// pending mid-window. Holding the lock blocks the drive; releasing it lets
    /// the drive complete.
    #[test]
    fn ungated_section_blocks_on_the_gate_channel_lock() {
        use std::task::{Context, Poll, Waker};

        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let lock = Arc::new(futures_util::lock::Mutex::new(()));
        let gate = FakeRemovalGate::with_lock(&group, GatedRemoval::GateNotEngaged, lock.clone());
        let a = author(nest.clone(), group.clone()).with_commit_gate(gate);
        block_on(a.bind_set("docs", CH)).expect("bind");

        // Simulate the background poll holding the channel lock.
        let poll_guard = block_on(lock.lock());

        let mut cx = Context::from_waker(Waker::noop());
        let mut fut = Box::pin(a.remove_member("docs", CH, member(2)));
        assert!(
            fut.as_mut().poll(&mut cx).is_pending(),
            "the ungated section must wait for the channel lock"
        );
        assert_eq!(
            *group.staged_count.lock().unwrap(),
            0,
            "nothing staged while the poll holds the lock"
        );

        drop(poll_guard);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => {
                out.expect("ungated remove completes once the lock frees");
            }
            Poll::Pending => panic!("drive still blocked after the lock was released"),
        }
        assert!(!group.members.lock().unwrap().contains(&member(2).0));
        assert_eq!(nest.sends.lock().unwrap().len(), 1);
    }

    #[test]
    fn fresh_content_key_is_random() {
        assert_ne!(fresh_content_key(), fresh_content_key());
    }

    #[cfg(feature = "mls")]
    #[test]
    fn paywall_set_content_keys_flags_and_mints_the_grant() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        // The holder's published X25519 pubkey (what the FFI/wasm face fetches
        // from `fetch_bridge_pubkey`); a real keypair so the HPKE wrap succeeds.
        let (_holder_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");

        let cfg = load_cfg(&a);
        // 1. Genesis: custody holds gen 1 for the serve pseudo-channel (the sync
        //    agent's ungated pass re-seals the back-catalogue from it).
        let custody_key = serve_custody_channel_id("premium");
        let keys = custody::content_keys(&cfg, &custody_key).expect("genesis content key");
        assert_eq!(keys.current.version, 1);

        // 2. The nest paywall flag flipped to the tier.
        let paywalls = nest.paywalls.lock().unwrap();
        assert_eq!(paywalls.len(), 1);
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &paywalls[0],
            "premium"
        ));
        assert_eq!(paywalls[0].tier.as_deref(), Some("gold"));

        // 3. Exactly one content.read{folder:premium} grant, wrapped to the
        //    holder, was minted — the grant plane that lights up the paywall.
        let mints = nest.mints.lock().unwrap();
        assert_eq!(mints.len(), 1);
        let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&mints[0])
            .expect("decode grant");
        assert_eq!(
            blob.index.0,
            ActorKeypair::from_secret([0xA0; 32]).actor_id().0.to_vec(),
            "owner is the creator"
        );
        assert_eq!(
            &blob.holder[..],
            &holder_pk[..],
            "wrapped to the web-serve holder"
        );
        assert_eq!(blob.scope.len(), 1);
        assert_eq!(blob.scope[0].class, "content.read");
        assert_eq!(blob.scope[0].kind.as_deref(), Some("folder"));
        assert_eq!(
            blob.scope[0].set,
            Some(fauna_mls::wrapped_blob::ScopeTuple::folder_set_qualifier(
                &fauna_core::path_crypto::set_name_hash("premium")
            ))
        );
        assert_eq!(
            blob.wrapped_keys.len(),
            1,
            "one wrap for the single content-key generation"
        );
    }

    #[cfg(feature = "mls")]
    #[test]
    fn paywall_set_shared_without_custody_refuses_before_any_side_effect() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        // A shared set (channel_id = Some) must already hold custody from its
        // bind; without it, paywall_set fails closed BEFORE any nest write — no
        // half-paywalled state a crash could strand.
        let err = block_on(a.paywall_set("shared", "gold", Some(CH), holder_pk, None))
            .expect_err("must refuse a shared set with no custody");
        assert!(
            matches!(
                err,
                PaywallSetError::Paywall(FoldersAuthorError::NoContentKeys(_))
            ),
            "expected NoContentKeys, got {err:?}"
        );
        assert!(
            nest.paywalls.lock().unwrap().is_empty(),
            "no flag flip on refusal"
        );
        assert!(nest.mints.lock().unwrap().is_empty(), "no grant on refusal");
    }

    #[cfg(feature = "mls")]
    #[test]
    fn rotate_paywall_grant_re_provisions_the_current_bundle() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        let (_holder_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        // Paywall (owner-only) → genesis gen 1 + the initial mint.
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");

        // A content-key rotation advances custody to gen 2 (a member evict /
        // explicit rotate does this in production; here we drive the primitive).
        let custody_key = serve_custody_channel_id("premium");
        block_on(crate::key_reader::update(&*a.custody, |cfg| {
            custody::rotate_set(cfg, &custody_key, *fresh_content_key(), Timestamp::now().0);
        }))
        .expect("rotate custody to gen 2");

        // The rotation leg re-provisions the grant via renew.
        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None)).expect("rotate grant");

        let renews = nest.renews.lock().unwrap();
        assert_eq!(renews.len(), 1, "one renew re-provision");
        // Named the SAME derived (owner, grant_id) the mint used — no persisted
        // grant-id state threaded across the mint→rotate boundary.
        let expected_id = paywall_id("premium", 0);
        assert_eq!(
            renews[0].grant_id.as_ref(),
            &expected_id[..],
            "renew targets the mint's derived grant id"
        );
        // The full current bundle (both generations) rode in appended_keys; the
        // nest dedups (scope, epoch), so the already-present gen 1 is a no-op and
        // only gen 2 actually lands.
        assert_eq!(renews[0].appended_keys.len(), 2, "gen 1 + gen 2 wraps");
        let epochs: std::collections::BTreeSet<Option<u64>> = renews[0]
            .appended_keys
            .iter()
            .map(|k| {
                let wk = fauna_mls::wrapped_blob::WrappedScopeKey::from_canonical_bytes(k)
                    .expect("decode appended wrap");
                assert_eq!(
                    wk.scope.set,
                    Some(fauna_mls::wrapped_blob::ScopeTuple::folder_set_qualifier(
                        &fauna_core::path_crypto::set_name_hash("premium")
                    ))
                );
                assert_eq!(wk.scope.kind.as_deref(), Some("folder"));
                wk.epoch
            })
            .collect();
        assert_eq!(
            epochs,
            [Some(1), Some(2)].into_iter().collect(),
            "one wrap per content-key generation, epoch = version"
        );
        assert!(
            renews[0].new_epoch_end > 0,
            "renew refreshes the grant window (keep-alive)"
        );
    }

    /// The test author's paywall grant id for `name` at `generation`.
    #[cfg(feature = "mls")]
    fn paywall_id(name: &str, generation: u32) -> [u8; 16] {
        fauna_client_capabilities::folder_paywall_grant_id(&[0xA0; 32], name, generation)
    }

    /// The paywall grant's events at `generation` in the owner's log, oldest
    /// first.
    #[cfg(feature = "mls")]
    fn paywall_events(
        log: &fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
        name: &str,
        generation: u32,
    ) -> Vec<fauna_core::grant_event::GrantEventKind> {
        let ledger = log.current();
        let mut events = grant_log::history_for_grant(&ledger, &paywall_id(name, generation));
        events.reverse();
        events.into_iter().map(|e| e.kind).collect()
    }

    /// The paywall grant is in the owner's log before it is on the nest
    /// (`ui/nests.md` § Trust facet — grants → *Record-then-deposit*): a log
    /// write the store refuses leaves nothing deposited.
    #[cfg(feature = "mls")]
    #[test]
    fn paywall_set_records_the_mint_before_the_deposit() {
        use fauna_core::grant_event::GrantEventKind;
        let nest = Arc::new(FakeNest::default());
        let (a, log) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        log.refuse_next_merges(1);
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None))
            .expect_err("an unrecorded mint must not deposit");
        assert!(nest.mints.lock().unwrap().is_empty(), "nothing deposited");

        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");
        assert_eq!(nest.mints.lock().unwrap().len(), 1);
        assert_eq!(
            paywall_events(&log, "premium", 0),
            vec![GrantEventKind::Mint]
        );
        let ledger = log.current();
        let current = grant_log::current_grants(&ledger);
        assert_eq!(current.len(), 1);
        assert_eq!(
            current[0].holder,
            holder_pk.to_vec(),
            "the web-serve holder"
        );
        assert_eq!(current[0].scope[0].class, "content.read");
        assert_eq!(current[0].scope[0].kind.as_deref(), Some("folder"));
    }

    /// The trust facet's reconcile sweep revokes every grant id the nest
    /// holds that the log does not hold live (`reconcile_sweep` →
    /// `grant_log::unrecognized_grant_ids`). Before the paywall legs recorded,
    /// that was the paywall grant, at every refresh; now a live paywall grant
    /// survives it, and so do its renewals.
    #[cfg(feature = "mls")]
    #[test]
    fn a_live_paywall_grant_survives_the_reconcile_sweep() {
        let nest = Arc::new(FakeNest::default());
        let (a, log) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        let id = paywall_id("premium", 0);

        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");
        assert!(grant_log::unrecognized_grant_ids(&log.current(), [id]).is_empty());

        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None)).expect("rotate");
        assert!(grant_log::unrecognized_grant_ids(&log.current(), [id]).is_empty());
    }

    /// The rotation leg records its window slide before the nest's renew.
    #[cfg(feature = "mls")]
    #[test]
    fn rotate_paywall_grant_records_the_renew() {
        use fauna_core::grant_event::GrantEventKind;
        let nest = Arc::new(FakeNest::default());
        let (a, log) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");

        log.refuse_next_merges(1);
        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None))
            .expect_err("an unrecorded renew must not reach the nest");
        assert!(nest.renews.lock().unwrap().is_empty());

        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None)).expect("rotate");
        assert_eq!(nest.renews.lock().unwrap().len(), 1);
        assert_eq!(
            paywall_events(&log, "premium", 0),
            vec![GrantEventKind::Mint, GrantEventKind::Renew]
        );
    }

    /// A paywall grant the log has no event for — deposited before the legs
    /// recorded, so swept as unrecognized — is healed by the rotation leg (the
    /// launch-time keep-alive drives it) as a recorded re-mint, not a renew of
    /// a row the sweep may already have deleted.
    #[cfg(feature = "mls")]
    #[test]
    fn rotate_paywall_grant_re_mints_a_grant_the_log_never_recorded() {
        use fauna_core::grant_event::GrantEventKind;
        let nest = Arc::new(FakeNest::default());
        let (a, log) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");
        // The log as it stood before the paywall legs recorded.
        log.mutate(|l| l.grant_events.clear());

        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None)).expect("heal");
        assert!(
            nest.renews.lock().unwrap().is_empty(),
            "no renew of an unlogged row"
        );
        assert_eq!(nest.mints.lock().unwrap().len(), 2, "re-minted whole");
        assert_eq!(
            paywall_events(&log, "premium", 0),
            vec![GrantEventKind::Mint]
        );
    }

    /// Unpaywalling records the `Revoke` after the nest's, which spends the
    /// generation (`webdav-server.md` § Key model → *A principal's read* rule
    /// (1) → *The generation*): a paywall → unpaywall → paywall round trip
    /// mints generation 1 under a fresh id the log holds live — so the
    /// reconcile sweep keeps it — and a rotate after it renews generation 1.
    #[cfg(feature = "mls")]
    #[test]
    fn a_set_paywalled_unpaywalled_and_paywalled_again_mints_the_next_generation() {
        use fauna_core::grant_event::GrantEventKind;
        let nest = Arc::new(FakeNest::default());
        let (a, log) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");

        block_on(a.unpaywall_set("premium")).expect("unpaywall");
        assert_eq!(
            paywall_events(&log, "premium", 0),
            vec![GrantEventKind::Mint, GrantEventKind::Revoke]
        );
        assert!(grant_log::current_grants(&log.current()).is_empty());
        // A retry records nothing more.
        block_on(a.unpaywall_set("premium")).expect("unpaywall retry");
        assert_eq!(paywall_events(&log, "premium", 0).len(), 2);

        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("re-paywall");
        let mints = nest.mints.lock().unwrap();
        assert_eq!(mints.len(), 2, "a second deposit");
        let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&mints[1]).unwrap();
        assert_eq!(
            blob.index.1,
            paywall_id("premium", 1).to_vec(),
            "generation 1's id"
        );
        drop(mints);
        assert_eq!(
            paywall_events(&log, "premium", 1),
            vec![GrantEventKind::Mint]
        );
        let live = grant_log::current_grants(&log.current());
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].grant_id, paywall_id("premium", 1).to_vec());
        assert!(
            grant_log::unrecognized_grant_ids(&log.current(), [paywall_id("premium", 1)])
                .is_empty(),
            "the sweep keeps generation 1"
        );

        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None)).expect("rotate");
        let renews = nest.renews.lock().unwrap();
        assert_eq!(renews.len(), 1);
        assert_eq!(renews[0].grant_id.as_ref(), &paywall_id("premium", 1)[..]);
        assert_eq!(
            paywall_events(&log, "premium", 1),
            vec![GrantEventKind::Mint, GrantEventKind::Renew]
        );
        assert_eq!(
            paywall_events(&log, "premium", 0).len(),
            2,
            "generation 0 stays spent"
        );
    }

    /// A paywall grant revoked from the Nests page while its set stays
    /// paywalled is healed by the launch keep-alive's rotate as a recorded
    /// mint of the next generation, never a renew of the spent one.
    #[cfg(feature = "mls")]
    #[test]
    fn rotate_paywall_grant_re_mints_a_revoked_generation_as_the_next() {
        use fauna_core::grant_event::GrantEventKind;
        let nest = Arc::new(FakeNest::default());
        let (a, log) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");
        // The Nests page's revoke: a `Revoke` in the log, the flag untouched.
        let kp = ActorKeypair::from_secret([0xA0; 32]);
        log.mutate(|l| {
            grant_log::record_revoke(l, kp.signing_key(), paywall_id("premium", 0), holder_pk, 50)
                .unwrap();
        });

        block_on(a.rotate_paywall_grant("premium", None, holder_pk, None)).expect("heal");
        assert!(
            nest.renews.lock().unwrap().is_empty(),
            "no renew of a spent id"
        );
        assert_eq!(nest.mints.lock().unwrap().len(), 2);
        assert_eq!(
            paywall_events(&log, "premium", 1),
            vec![GrantEventKind::Mint]
        );
    }

    /// Without a grant log every paywall leg refuses before touching the nest.
    #[cfg(feature = "mls")]
    #[test]
    fn paywall_legs_refuse_without_a_grant_log() {
        let nest = Arc::new(FakeNest::default());
        let a = bare_author(nest.clone(), FakeGroup::with_members(&[]));
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        let err = block_on(a.paywall_set("premium", "gold", None, holder_pk, None))
            .expect_err("no log, no paywall");
        assert!(matches!(err, PaywallSetError::GrantLog(_)), "got {err:?}");
        let err = block_on(a.unpaywall_set("premium")).expect_err("no log, no unpaywall");
        assert!(matches!(err, PaywallSetError::GrantLog(_)), "got {err:?}");
        assert!(nest.paywalls.lock().unwrap().is_empty());
        assert!(nest.mints.lock().unwrap().is_empty());
        assert!(nest.revokes.lock().unwrap().is_empty());
    }

    #[cfg(feature = "mls")]
    #[test]
    fn unpaywall_set_revokes_the_grant_and_clears_the_flag() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        let (_holder_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");

        block_on(a.unpaywall_set("premium")).expect("unpaywall");

        // The grant was revoked by the SAME derived id the mint stamped.
        let revokes = nest.revokes.lock().unwrap();
        assert_eq!(revokes.len(), 1, "one revoke");
        let expected_id = paywall_id("premium", 0);
        assert_eq!(
            revokes[0],
            expected_id.to_vec(),
            "revoke targets the derived id"
        );

        // The nest tier flag was cleared (a second set_web_paywall, tier = None),
        // so a sealed row now fails closed to 404.
        let paywalls = nest.paywalls.lock().unwrap();
        assert_eq!(paywalls.len(), 2, "flip-on then clear");
        assert!(fauna_protocol::folders::SetAddressed::addresses(
            &paywalls[1],
            "premium"
        ));
        assert_eq!(paywalls[1].tier, None, "tier cleared on unpaywall");
    }

    /// A paywalled summary row for the FakeNest `fauna.folders.list` arm.
    #[cfg(feature = "mls")]
    fn summary(
        name: &str,
        tier: Option<&str>,
        role: &str,
        mls_group_id: Option<String>,
    ) -> fauna_protocol::folders::FolderSummary {
        fauna_protocol::folders::FolderSummary {
            name: name.to_string(),
            website_enabled: true,
            web_paywall_tier: tier.map(str::to_string),
            role: Some(role.to_string()),
            mls_group_id,
            ..Default::default()
        }
    }

    /// Decode a renew's appended wraps into their `(scope-checked) epoch` set.
    #[cfg(feature = "mls")]
    fn renewed_epochs(
        renew: &RenewGrantRequest,
        set: &str,
    ) -> std::collections::BTreeSet<Option<u64>> {
        renew
            .appended_keys
            .iter()
            .map(|k| {
                let wk = fauna_mls::wrapped_blob::WrappedScopeKey::from_canonical_bytes(k)
                    .expect("decode appended wrap");
                assert_eq!(
                    wk.scope.set,
                    Some(fauna_mls::wrapped_blob::ScopeTuple::folder_set_qualifier(
                        &fauna_core::path_crypto::set_name_hash(set)
                    ))
                );
                assert_eq!(wk.scope.kind.as_deref(), Some("folder"));
                wk.epoch
            })
            .collect()
    }

    #[cfg(feature = "mls")]
    #[test]
    fn remove_member_re_provisions_the_paywall_grant_after_rotation() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group);
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        // A shared∩paywalled set: bind (gen-1 custody) then paywall it.
        block_on(a.bind_set("docs", CH)).expect("bind");
        block_on(a.paywall_set("docs", "gold", Some(CH), holder_pk, None)).expect("paywall");
        *nest.list_summaries.lock().unwrap() = vec![summary("docs", Some("gold"), "owner", None)];
        *nest.holder_pubkey.lock().unwrap() = Some(holder_pk);

        // Rotate-on-removal → the grant is automatically re-provisioned.
        let out = block_on(a.remove_member("docs", CH, member(2))).expect("remove");
        assert!(out.rotated);

        let renews = nest.renews.lock().unwrap();
        assert_eq!(
            renews.len(),
            1,
            "rotate-on-removal re-provisions the paywall grant (monetization.md § Pillar 2)"
        );
        let expected_id = paywall_id("docs", 0);
        assert_eq!(
            renews[0].grant_id.as_ref(),
            &expected_id[..],
            "renew targets the mint's derived grant id"
        );
        assert_eq!(
            renewed_epochs(&renews[0], "docs"),
            [Some(1), Some(2)].into_iter().collect(),
            "the full post-rotation bundle rode the renew — the fresh gen 2 included"
        );
    }

    #[cfg(feature = "mls")]
    #[test]
    fn remove_member_on_an_unpaywalled_set_sends_no_renew() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group);
        block_on(a.bind_set("docs", CH)).expect("bind");
        // list_summaries stays empty (nothing paywalled) and holder_pubkey stays
        // None — the fetch_bridge_pubkey arm would panic if discovery ran.

        let out = block_on(a.remove_member("docs", CH, member(2))).expect("remove");
        assert!(out.rotated);
        assert!(
            nest.renews.lock().unwrap().is_empty(),
            "no paywall ⇒ no renew (and no holder discovery)"
        );
    }

    #[cfg(feature = "mls")]
    #[test]
    fn remove_member_succeeds_when_the_renew_fails() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[member(1), member(2)]);
        let a = author(nest.clone(), group);
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        // Production-shaped binding: custody keyed by the REAL derived ChannelId,
        // and the nest summary carries the raw group id — the launch pass
        // re-derives the custody key from exactly that hex.
        let raw_group = [0xB7u8; 32];
        let ch = fauna_mls::types::ChannelId::from_group_id(&raw_group).0;
        block_on(a.bind_set("docs", ch)).expect("bind");
        block_on(a.paywall_set("docs", "gold", Some(ch), holder_pk, None)).expect("paywall");
        *nest.list_summaries.lock().unwrap() = vec![summary(
            "docs",
            Some("gold"),
            "owner",
            Some(hex::encode(raw_group)),
        )];
        *nest.holder_pubkey.lock().unwrap() = Some(holder_pk);
        *nest.fail_next_renews.lock().unwrap() = 1;

        // The renew fails (nest offline mid-window) — the removal, the security
        // half, still completes and commits; the launch pass retries the renew.
        let out = block_on(a.remove_member("docs", ch, member(2)))
            .expect("removal must not fail on a best-effort renew");
        assert!(out.rotated);
        let keys = custody::content_keys(&load_cfg(&a), &ch).unwrap();
        assert_eq!(keys.current_version(), 2, "rotation committed");
        assert!(
            load_cfg(&a).pending_removals.is_empty(),
            "sentinel cleared — the removal is done"
        );
        assert!(nest.renews.lock().unwrap().is_empty(), "the renew was lost");

        // …and the launch-time pass converges the grant.
        let renewed = block_on(a.renew_paywalled_grants()).expect("launch pass");
        assert_eq!(renewed, 1);
        let renews = nest.renews.lock().unwrap();
        assert_eq!(renews.len(), 1);
        assert_eq!(
            renewed_epochs(&renews[0], "docs"),
            [Some(1), Some(2)].into_iter().collect(),
            "the retry re-sends the full bundle (nest dedups (scope, epoch))"
        );
    }

    #[cfg(feature = "mls")]
    #[test]
    fn renew_paywalled_grants_renews_every_owned_paywalled_set() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        // Set A: unshared paywalled — custody under the serve pseudo-channel.
        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall A");
        // Set B: bound paywalled — custody under the REAL derived ChannelId (the
        // launch pass re-derives it from the summary's hex mls_group_id).
        let raw_group = [0xB7u8; 32];
        let ch_b = fauna_mls::types::ChannelId::from_group_id(&raw_group).0;
        block_on(a.bind_set("docs", ch_b)).expect("bind B");
        block_on(a.paywall_set("docs", "silver", Some(ch_b), holder_pk, None)).expect("paywall B");

        *nest.list_summaries.lock().unwrap() = vec![
            summary("premium", Some("gold"), "owner", None),
            summary(
                "docs",
                Some("silver"),
                "owner",
                Some(hex::encode(raw_group)),
            ),
            // Unpaywalled — skipped.
            summary("plain", None, "owner", None),
            // Shared WITH the caller — never renewed (only the owner can renew
            // its own derived grant).
            summary("theirs", Some("gold"), "member", None),
        ];
        *nest.holder_pubkey.lock().unwrap() = Some(holder_pk);

        let renewed = block_on(a.renew_paywalled_grants()).expect("launch pass");
        assert_eq!(renewed, 2, "both owned paywalled sets, nothing else");

        let renews = nest.renews.lock().unwrap();
        assert_eq!(renews.len(), 2);
        let ids: Vec<&[u8]> = renews.iter().map(|r| r.grant_id.as_ref()).collect();
        assert!(ids.contains(&&paywall_id("premium", 0)[..]));
        assert!(ids.contains(&&paywall_id("docs", 0)[..]));
        for r in renews.iter() {
            assert!(
                r.new_epoch_end > 0,
                "each renew bumps the grant window (keep-alive)"
            );
        }
    }

    #[cfg(feature = "mls")]
    #[test]
    fn resume_pending_removals_runs_the_paywall_keep_alive_pass() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group);
        let (_sk, holder_pk) = fauna_mls::wrapped_blob::generate_x25519_keypair();

        block_on(a.paywall_set("premium", "gold", None, holder_pk, None)).expect("paywall");
        *nest.list_summaries.lock().unwrap() =
            vec![summary("premium", Some("gold"), "owner", None)];
        *nest.holder_pubkey.lock().unwrap() = Some(holder_pk);

        // No pending removals — the launch resume still keep-alives the grant.
        let resumed = block_on(a.resume_pending_removals()).expect("resume");
        assert_eq!(resumed, 0, "nothing staged");
        assert_eq!(
            nest.renews.lock().unwrap().len(),
            1,
            "the launch resume renewed the paywalled set's grant window"
        );
    }

    /// A served-set walk double: records every set it is asked to converge,
    /// answers `failing` sets with the "custody not synced yet" error the real
    /// walk bails with, and every other set with one re-sealed head.
    #[cfg(feature = "mls")]
    #[derive(Default)]
    struct RecordingWalk {
        walked: Mutex<Vec<String>>,
        failing: Vec<&'static str>,
    }

    #[cfg(feature = "mls")]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::nest_reseal::ServedSetConverge for RecordingWalk {
        async fn converge_served_set(
            &self,
            folder: &str,
        ) -> anyhow::Result<fauna_core::nest_reseal::ServedSetConvergence> {
            self.walked.lock().unwrap().push(folder.to_string());
            if self.failing.contains(&folder) {
                anyhow::bail!("the served set's content keys are not in this seat's custody yet");
            }
            Ok(fauna_core::nest_reseal::ServedSetConvergence {
                resealed: 1,
                ..Default::default()
            })
        }
    }

    #[cfg(feature = "mls")]
    fn listed(
        name: &str,
        served: bool,
        role: &str,
        group: Option<&str>,
    ) -> fauna_protocol::folders::FolderSummary {
        fauna_protocol::folders::FolderSummary {
            name: name.to_string(),
            webdav_enabled: served,
            role: Some(role.to_string()),
            mls_group_id: group.map(str::to_string),
            ..Default::default()
        }
    }

    /// `webdav-server.md` § Key model (c) — the walk resumes at app start, not
    /// only on a re-flip: the launch resume re-drives the wired walk over
    /// exactly the owned, served, group-less sets (the ones `serve_set` walks),
    /// never an unserved set, a shared set (the M2 pre-bind pass's) or a set
    /// shared *with* this account; and a set whose walk cannot start yet does
    /// not starve the next one.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_resume_redrives_the_served_set_walk_over_owned_unshared_served_sets() {
        let nest = Arc::new(FakeNest::default());
        *nest.list_summaries.lock().unwrap() = vec![
            listed("interrupted", true, "owner", None),
            listed("unsynced", true, "owner", None),
            listed("private", false, "owner", None),
            // Flagged by the nest, never served by the owner: not walked.
            listed("lied", true, "owner", None),
            listed("shared", true, "owner", Some("ab")),
            listed("theirs", true, "member", None),
        ];
        let walk = Arc::new(RecordingWalk {
            failing: vec!["unsynced"],
            ..Default::default()
        });
        let a = author(nest.clone(), FakeGroup::with_members(&[]))
            .with_served_set_converge(walk.clone());
        // The owner's word: the two sets it served.
        block_on(a.serve_enable("interrupted", None)).expect("serve-on");
        block_on(a.serve_enable("unsynced", None)).expect("serve-on");

        let resumed = block_on(a.resume_pending_removals()).expect("resume");
        assert_eq!(resumed, 0, "nothing staged");
        let mut walked = walk.walked.lock().unwrap().clone();
        walked.sort();
        assert_eq!(
            walked,
            vec!["interrupted".to_string(), "unsynced".to_string()],
            "the launch resume walks every owned, served, unshared set"
        );

        // The entry point itself sums the tally over the sets it could walk.
        let tally = block_on(a.converge_served_sets()).expect("converge");
        assert_eq!(tally.resealed, 1, "the unsynced set is warned, not fatal");
    }

    /// The sets and keys the last-provisioned `WebdavKeysBlob` carries.
    #[cfg(feature = "mls")]
    fn provisioned_sets(nest: &FakeNest) -> Vec<String> {
        let blobs = nest.provisioned.lock().unwrap();
        let blob = fauna_mls::wrapped_blob::WebdavKeysBlob::from_canonical_bytes(
            blobs.last().expect("a blob was provisioned"),
        )
        .unwrap();
        let pt = fauna_mls::wrapped_blob::unseal_webdav_keys_blob(&blob, &[0x5E; 32]).unwrap();
        let mut names: Vec<String> =
            fauna_mls::wrapped_blob::WebdavKeysPlaintext::from_canonical_bytes(&pt)
                .unwrap()
                .served_sets
                .into_iter()
                .map(|s| s.set_name)
                .collect();
        names.sort();
        names
    }

    /// Ruling (7)(b)(ii) rule (3), the unserve arm: {custody served, flag
    /// off} — a serve-on that died before the flag, a serve-off that died
    /// after it, a nest lying "off" — is unserved in custody at launch: one
    /// rotation and the serve-off stamp in one write, and only one across two
    /// devices; the blob is re-provisioned without the set.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_unserves_a_set_the_nest_holds_off_once_across_two_devices() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let pseudo = serve_custody_channel_id("docs");
        block_on(a.serve_enable("docs", None)).expect("serve-on");
        *nest.list_summaries.lock().unwrap() = vec![listed("docs", false, "owner", None)];
        nest.updates.lock().unwrap().clear();

        block_on(a.resume_pending_removals()).expect("the launch pass");
        let cfg = load_cfg(&a);
        assert!(
            !custody::channel_served(&cfg, &pseudo),
            "unserved in custody"
        );
        assert_eq!(
            custody::content_keys(&cfg, &pseudo)
                .unwrap()
                .current_version(),
            2,
            "one rotation"
        );
        assert!(
            nest.updates
                .lock()
                .unwrap()
                .iter()
                .all(|u| u.webdav_enabled.is_none()),
            "the flag is never pushed"
        );
        assert_eq!(provisioned_sets(&nest), Vec::<String>::new());

        // A second device's launch pass over the same custody rotates nothing.
        let b = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(b.resume_pending_removals()).expect("the sibling's launch pass");
        assert_eq!(
            custody::content_keys(&load_cfg(&b), &pseudo)
                .unwrap()
                .current_version(),
            2,
            "one rotation for one flip"
        );
    }

    /// Ruling (7)(b)(ii) rule (3): {custody NOT served, flag on} — a lying
    /// nest, a stamp-less keyed entry (a paywall's, a bind's, a set served
    /// before the build) — is left alone: no flag push, nothing stamped, no
    /// rotation, and the blob provisioned without the set while it carries
    /// the set the owner did serve.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_leaves_a_flag_the_owner_never_set_and_provisions_from_custody() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        let lied = serve_custody_channel_id("lied");
        block_on(a.update_custody(|cfg| {
            custody::record_created_set(cfg, "lied", [0x11; 32], None, 1);
            custody::key_named_set(cfg, "lied", lied, [7; 32], 2);
        }))
        .unwrap();
        block_on(a.serve_enable("served", None)).expect("serve-on");
        *nest.list_summaries.lock().unwrap() = vec![
            listed("lied", true, "owner", None),
            listed("served", true, "owner", None),
        ];
        nest.updates.lock().unwrap().clear();

        block_on(a.resume_pending_removals()).expect("the launch pass");
        let cfg = load_cfg(&a);
        assert_eq!(
            custody::serve_stamps(&cfg, &lied),
            (None, None),
            "never stamped"
        );
        assert_eq!(
            custody::content_keys(&cfg, &lied)
                .unwrap()
                .current_version(),
            1,
            "never rotated"
        );
        assert!(
            nest.updates
                .lock()
                .unwrap()
                .iter()
                .all(|u| u.webdav_enabled.is_none()),
            "the flag is left alone"
        );
        assert_eq!(provisioned_sets(&nest), vec!["served".to_string()]);
    }

    /// The decoded payload of the envelope last stored for `CH`.
    #[cfg(feature = "mls")]
    fn stored_payload(nest: &FakeNest) -> fauna_core::folder_keys::ContentKeyEnvelopePayload {
        let puts = nest.puts.lock().unwrap();
        let blob = hex::decode(&puts.last().expect("an envelope").sealed).unwrap();
        let signed = fauna_protocol::folder_envelope_sig::verify(&blob, &CH).unwrap();
        fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&signed.sealed).unwrap()
    }

    /// Ruling (7)(b)(ii) rule (3), the envelope reconcile: a serve-off that
    /// died between its custody write and its republish is healed at launch —
    /// the stale envelope is republished as the JOIN.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_republishes_a_stale_envelope_as_the_join() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(a.bind_set("docs", CH)).expect("bind");
        block_on(a.serve_enable("docs", Some(CH))).expect("serve-on");
        *nest.list_summaries.lock().unwrap() = vec![listed("docs", false, "owner", None)];
        // The serve-off's flip landed and its custody write too; the process
        // died before the republish.
        block_on(a.unserve_in_custody(&CH)).unwrap();
        let before = nest.puts.lock().unwrap().len();
        assert!(stored_payload(&nest).unserved_at.is_none(), "stale");

        block_on(a.resume_pending_removals()).expect("the launch pass");
        assert_eq!(nest.puts.lock().unwrap().len(), before + 1, "republished");
        let payload = stored_payload(&nest);
        let cfg = load_cfg(&a);
        assert_eq!(
            (payload.served_at, payload.unserved_at),
            custody::serve_stamps(&cfg, &CH)
        );
        assert_eq!(payload.keys.current_version(), 2, "the rotation rides it");

        block_on(a.resume_pending_removals()).expect("the next launch");
        assert_eq!(
            nest.puts.lock().unwrap().len(),
            before + 1,
            "converged — nothing re-published"
        );
    }

    /// A lagging device — its custody behind the stored envelope — publishes
    /// nothing: never a stale bundle over a newer one.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_never_republishes_from_a_custody_behind_the_envelope() {
        let nest = Arc::new(FakeNest::default());
        let group = FakeGroup::with_members(&[]);
        let a = author(nest.clone(), group.clone());
        block_on(a.bind_set("docs", CH)).expect("bind");
        // A sibling served, rotated and published; this device's custody has
        // none of it yet.
        let cfg = load_cfg(&a);
        let mut ahead = cfg.clone();
        custody::serve_on(&mut ahead, &CH, 500);
        custody::rotate_set(&mut ahead, &CH, [9; 32], 600);
        let newer = custody::envelope_payload(
            &ahead,
            Some("docs"),
            &CH,
            custody::content_keys(&ahead, &CH).unwrap(),
        );
        block_on(a.publish_payload("docs", &CH, &newer)).unwrap();
        let before = nest.puts.lock().unwrap().len();

        assert_eq!(block_on(a.converge_envelopes()).unwrap(), 0);
        assert_eq!(nest.puts.lock().unwrap().len(), before, "left alone");
        assert_eq!(stored_payload(&nest), newer);
    }

    /// A channel with a staged pending removal is skipped: its Remove may be
    /// merged and its generation unpublished.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_skips_the_envelope_of_a_channel_with_a_staged_removal() {
        let nest = Arc::new(FakeNest::default());
        let a = author(nest.clone(), FakeGroup::with_members(&[]));
        block_on(a.bind_set("docs", CH)).expect("bind");
        block_on(a.update_custody(|cfg| {
            custody::serve_on(cfg, &CH, 500);
            custody::stage_pending_removal(
                cfg,
                FolderPendingRemoval {
                    channel_id: CH,
                    name: "docs".into(),
                    removed_member: member(9),
                    new_generation: ContentKeyGeneration {
                        version: 2,
                        key: [8; 32].into(),
                        rotated_at: 700,
                    },
                    commit: None,
                    gated_attempted: false,
                },
            );
        }))
        .unwrap();
        let before = nest.puts.lock().unwrap().len();
        assert_eq!(block_on(a.converge_envelopes()).unwrap(), 0);
        assert_eq!(nest.puts.lock().unwrap().len(), before);
    }

    /// No walk wired (a face that names no recording device) ⇒ the launch
    /// resume reads nothing for it.
    #[cfg(feature = "mls")]
    #[test]
    fn launch_resume_without_a_walk_reads_nothing() {
        let nest = Arc::new(FakeNest::default());
        *nest.list_summaries.lock().unwrap() = vec![listed("served", true, "owner", None)];
        let a = author(nest, FakeGroup::with_members(&[]));
        let tally = block_on(a.converge_served_sets()).expect("converge");
        assert_eq!(tally, Default::default());
    }

    // ── a principal's folder read twin: renew on rotation, revoke on unserve
    //    (`webdav-server.md` § Key model → *A principal's read* rule (4)) ──

    #[cfg(feature = "mls")]
    const PRINCIPAL: [u8; 32] = [0xC1; 32];
    #[cfg(feature = "mls")]
    const TWIN_END: u64 = 4_000_000_000;

    /// A principal twin's id over `set` at `generation` — what the consent
    /// planner minted under (the author's secret is `[0xA0; 32]`).
    #[cfg(feature = "mls")]
    fn twin_id(holder: [u8; 32], set: &str, generation: u32) -> [u8; 16] {
        fauna_client_capabilities::folder_principal_grant_id(&[0xA0; 32], &holder, set, generation)
    }

    #[cfg(feature = "mls")]
    fn log_event(
        ledger: &fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
        event: fauna_core::grant_event::GrantEvent,
    ) {
        ledger.mutate(|l| l.grant_events.push(event));
    }

    #[cfg(feature = "mls")]
    fn log_mint(
        ledger: &fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
        grant_id: [u8; 16],
        holder: [u8; 32],
    ) {
        log_event(
            ledger,
            fauna_client_capabilities::grant_log::build_mint_event(
                grant_id,
                holder,
                vec![],
                1_000,
                TWIN_END,
                1_000,
            ),
        );
    }

    #[cfg(feature = "mls")]
    fn live_ids(
        ledger: &fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
    ) -> BTreeSet<Vec<u8>> {
        fauna_core::grant_event::current_grants_of(&ledger.current().grant_events)
            .into_iter()
            .map(|g| g.grant_id)
            .collect()
    }

    /// An unshared set served by the owner, listed as owned with its flag
    /// as `webdav_enabled` says.
    #[cfg(feature = "mls")]
    fn served_docs(nest: &Arc<FakeNest>, a: &Author, webdav_enabled: bool) {
        block_on(a.serve_enable("docs", None)).expect("serve-on");
        *nest.list_summaries.lock().unwrap() = vec![fauna_protocol::folders::FolderSummary {
            webdav_enabled,
            ..summary("docs", None, "owner", None)
        }];
    }

    /// The serve reconcile renews a principal's twin over a served set with
    /// the set's full bundle — the rotation's generation included — at the
    /// grant's RECORDED window end, and renews nothing else: not the
    /// principal's records grant, not its twin over another set.
    #[cfg(feature = "mls")]
    #[test]
    fn a_rotation_renews_a_principals_folder_grant_at_its_recorded_end() {
        let nest = Arc::new(FakeNest::default());
        let (a, ledger) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        served_docs(&nest, &a, true);
        log_mint(&ledger, twin_id(PRINCIPAL, "docs", 0), PRINCIPAL);
        log_mint(&ledger, twin_id(PRINCIPAL, "photos", 0), PRINCIPAL);
        log_mint(&ledger, [0x77; 16], PRINCIPAL); // the consent's records grant
        let pseudo = serve_custody_channel_id("docs");
        block_on(a.update_custody(|cfg| {
            custody::rotate_set(cfg, &pseudo, [0x33; 32], Timestamp::now().0);
        }))
        .expect("rotate");

        block_on(a.reprovision_webdav_keys()).expect("reconcile");

        let renews = nest.renews.lock().unwrap();
        assert_eq!(renews.len(), 1, "exactly the docs twin is renewed");
        assert_eq!(
            renews[0].grant_id.as_ref(),
            &twin_id(PRINCIPAL, "docs", 0)[..]
        );
        assert_eq!(
            renews[0].new_epoch_end, TWIN_END,
            "the recorded end, never slid"
        );
        assert_eq!(renews[0].new_epoch_start, None);
        assert_eq!(
            renewed_epochs(&renews[0], "docs"),
            [Some(1), Some(2)].into_iter().collect(),
            "the full bundle — the nest dedups, so only generation 2 lands"
        );
        assert_eq!(
            nest.provisioned.lock().unwrap().len(),
            1,
            "the blob still provisions"
        );
    }

    /// A lapsed twin reads nothing and is never renewed; the paywall grant's
    /// own renew keeps sliding its window (`rotate_paywall_grant`, unchanged).
    #[cfg(feature = "mls")]
    #[test]
    fn a_lapsed_principal_twin_is_not_renewed() {
        let nest = Arc::new(FakeNest::default());
        let (a, ledger) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        served_docs(&nest, &a, true);
        log_event(
            &ledger,
            fauna_client_capabilities::grant_log::build_mint_event(
                twin_id(PRINCIPAL, "docs", 0),
                PRINCIPAL,
                vec![],
                1_000,
                2_000,
                1_000,
            ),
        );
        block_on(a.reprovision_webdav_keys()).expect("reconcile");
        assert!(nest.renews.lock().unwrap().is_empty());
    }

    /// The unserve revokes the principal's twin over the set — nest first,
    /// then a signed `Revoke` in the log — and nothing else: the records
    /// grant and the twin over another set stay live.
    #[cfg(feature = "mls")]
    #[test]
    fn an_unserve_revokes_a_principals_folder_grant_and_only_it() {
        let nest = Arc::new(FakeNest::default());
        let (a, ledger) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        served_docs(&nest, &a, true);
        log_mint(&ledger, twin_id(PRINCIPAL, "docs", 0), PRINCIPAL);
        log_mint(&ledger, twin_id(PRINCIPAL, "photos", 0), PRINCIPAL);
        log_mint(&ledger, [0x77; 16], PRINCIPAL);

        block_on(a.serve_disable("docs", None)).expect("serve-off");

        assert_eq!(
            *nest.revokes.lock().unwrap(),
            vec![twin_id(PRINCIPAL, "docs", 0).to_vec()]
        );
        let revoke = ledger
            .current()
            .grant_events
            .into_iter()
            .find(|e| e.kind == fauna_core::grant_event::GrantEventKind::Revoke)
            .expect("a Revoke recorded");
        assert_eq!(revoke.grant_id, twin_id(PRINCIPAL, "docs", 0).to_vec());
        revoke
            .verify(&ActorKeypair::from_secret([0xA0; 32]).actor_id())
            .expect("signed by the owner");
        assert_eq!(
            live_ids(&ledger),
            [twin_id(PRINCIPAL, "photos", 0).to_vec(), vec![0x77; 16]]
                .into_iter()
                .collect(),
            "the records grant and the other set's twin are untouched"
        );
        let keys = custody::content_keys(&load_cfg(&a), &serve_custody_channel_id("docs")).unwrap();
        assert_eq!(keys.current_version(), 2, "the rotation still runs");
    }

    /// The revoke runs BEFORE the rotation: a refused revoke surfaces and
    /// leaves the set served and unrotated, so the launch pass's unserve arm
    /// re-drives the whole tail — which revokes, then rotates.
    #[cfg(feature = "mls")]
    #[test]
    fn a_refused_principal_revoke_leaves_the_rotation_unrun_until_the_launch_arm() {
        let nest = Arc::new(FakeNest::default());
        let (a, ledger) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        served_docs(&nest, &a, true);
        log_mint(&ledger, twin_id(PRINCIPAL, "docs", 0), PRINCIPAL);
        *nest.fail_next_revokes.lock().unwrap() = 1;
        let pseudo = serve_custody_channel_id("docs");

        block_on(a.serve_disable("docs", None)).expect_err("the revoke was refused");
        let cfg = load_cfg(&a);
        assert!(
            custody::channel_served(&cfg, &pseudo),
            "still served in custody"
        );
        assert_eq!(
            custody::content_keys(&cfg, &pseudo)
                .unwrap()
                .current_version(),
            1
        );
        assert!(live_ids(&ledger).contains(&twin_id(PRINCIPAL, "docs", 0)[..]));

        // The flag fell; the launch pass finds {custody served, flag off}.
        *nest.list_summaries.lock().unwrap() = vec![summary("docs", None, "owner", None)];
        assert_eq!(
            block_on(a.unserve_sets_the_nest_holds_off()).expect("launch arm"),
            1
        );
        assert_eq!(
            *nest.revokes.lock().unwrap(),
            vec![twin_id(PRINCIPAL, "docs", 0).to_vec()]
        );
        assert!(!live_ids(&ledger).contains(&twin_id(PRINCIPAL, "docs", 0)[..]));
        let cfg = load_cfg(&a);
        assert!(!custody::channel_served(&cfg, &pseudo));
        assert_eq!(
            custody::content_keys(&cfg, &pseudo)
                .unwrap()
                .current_version(),
            2
        );
    }

    /// The generation walk: after a revoke of generation 0 and a re-approve
    /// minting generation 1, the rotation renews generation 1 and the
    /// unserve revokes generation 1 — never the spent generation 0.
    #[cfg(feature = "mls")]
    #[test]
    fn renew_and_revoke_follow_the_live_generation() {
        let nest = Arc::new(FakeNest::default());
        let (a, ledger) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        served_docs(&nest, &a, true);
        log_mint(&ledger, twin_id(PRINCIPAL, "docs", 0), PRINCIPAL);
        log_event(
            &ledger,
            fauna_client_capabilities::grant_log::build_revoke_event(
                twin_id(PRINCIPAL, "docs", 0),
                PRINCIPAL,
                1_500,
            ),
        );
        log_mint(&ledger, twin_id(PRINCIPAL, "docs", 1), PRINCIPAL);

        block_on(a.reprovision_webdav_keys()).expect("reconcile");
        let renewed: Vec<Vec<u8>> = nest
            .renews
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.grant_id.to_vec())
            .collect();
        assert_eq!(renewed, vec![twin_id(PRINCIPAL, "docs", 1).to_vec()]);

        block_on(a.serve_disable("docs", None)).expect("serve-off");
        assert_eq!(
            *nest.revokes.lock().unwrap(),
            vec![twin_id(PRINCIPAL, "docs", 1).to_vec()]
        );
    }

    /// The launch pass's unserve arm revokes the twin over a set the nest
    /// holds off, before it rotates — the same tail as the gesture.
    #[cfg(feature = "mls")]
    #[test]
    fn the_launch_unserve_arm_revokes_a_principals_folder_grant() {
        let nest = Arc::new(FakeNest::default());
        let (a, ledger) = author_with_log(nest.clone(), FakeGroup::with_members(&[]));
        served_docs(&nest, &a, false);
        log_mint(&ledger, twin_id(PRINCIPAL, "docs", 0), PRINCIPAL);
        log_mint(&ledger, [0x77; 16], PRINCIPAL);

        assert_eq!(
            block_on(a.unserve_sets_the_nest_holds_off()).expect("launch arm"),
            1
        );
        assert_eq!(
            *nest.revokes.lock().unwrap(),
            vec![twin_id(PRINCIPAL, "docs", 0).to_vec()]
        );
        assert_eq!(live_ids(&ledger), [vec![0x77; 16]].into_iter().collect());
    }
}
