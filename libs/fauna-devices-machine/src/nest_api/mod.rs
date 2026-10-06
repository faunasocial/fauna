//! Trait abstractions for the page's nest-side surface.
//!
//! The page reads + write gestures go through [`DevicesNestApi`]; opening the
//! embedded folder wizard goes through [`WizardFactory`]. Production code uses
//! the WS-RPC [`ws_rpc::WsRpcDevicesNest`] (over `fauna-client-{folders,sync}`)
//! and [`ws_rpc::WsRpcWizardFactory`]; tests use [`FakeDevicesNestApi`] +
//! [`FakeWizardFactory`] (gated under `#[cfg(any(test, feature = "test-helpers"))]`).
//! Mirrors `fauna_folders_machine::nest_api`.
//!
//! Transport: every read/write rides the authenticated WS-RPC connection — the
//! page runs inside an already-logged-in session, so the seam is constructed
//! with the session's connected requester (`Arc<NestClient>` native /
//! `WsRpcClient` wasm) and needs no per-call URL or token. There is no HTTP impl
//! (the `no-http-ws-rpc-everywhere` directive): a `DevicesMachine` consuming
//! HTTP would extend the surface instead of migrating it.

pub mod fake;
#[cfg(feature = "rpc-glue")]
pub mod ws_rpc;

#[cfg(any(test, feature = "test-helpers"))]
pub use fake::{FakeCall, FakeDevicesNestApi, FakeWizardFactory};
#[cfg(feature = "rpc-glue")]
pub use ws_rpc::build_devices_machine;

use std::sync::Arc;

use async_trait::async_trait;
use fauna_folders_machine::{DeviceOption, FolderWizardMachine, FolderWizardObserver};

use fauna_protocol::folders::SyncConflict;
use fauna_protocol::sync::SyncDevice;

use fauna_protocol::folders::FolderSummary as WireFolderSummary;

/// A choose-winner's pick, as the machine hands it to the seam: the candidate
/// manifest and the conflict's **rendered** path, under whose hash the seam
/// looks the version up (ruling (10)(a)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenWinner {
    pub manifest_hash: String,
    pub path: String,
}

/// The one shared restore decision
/// (`fauna_client_sync::restore_branch::RestoreDecision`) over the judged
/// version a conflict candidate names, as the review list reads it — carried
/// as its own type so the machine core stays free of the `rpc-glue` crates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateVerdict {
    /// Re-point verbatim, recording the VERSION's signed size and stamp —
    /// never the candidate row's.
    Verbatim {
        size_bytes: i64,
        content_key_version: Option<u64>,
    },
    /// The version was signed by another identity and carries no stamp: it
    /// must be opened and re-sealed, which this surface has no byte seam for
    /// — the file's version history (the Media restore) performs it.
    NeedsReseal,
    /// No admitted version of this path in this set carries the manifest: the
    /// candidate is not a candidate.
    NotAVersion,
}

fauna_core::declare_api_error!(
    /// Failure of a page-level nest call. `detail` carries the nest's error
    /// text (e.g. `"folder with that name already exists"`). Mirrors
    /// `fauna_folders_machine::FolderApiError`; the WS-RPC impl keys the
    /// variant off the `RpcError.code` suffix.
    DevicesApiError {
        /// Name taken / concurrent destructive op / a device the nest keeps
        /// (a folder's sole source, a guardian-enrolled device).
        Conflict,
        /// Invalid request (bad role / hex / candidate).
        BadRequest,
        /// Folder, device, or conflict not found / not owned.
        NotFound,
        /// Transport fault / 5xx — retryable.
        Transient,
    }
);

// `MaybeSendSync` supertrait + dual `async_trait` arm so the one seam serves
// native (`Arc<NestClient>`, `Send + Sync`) and wasm (the single-threaded
// `Rc`-based `WsRpcClient`, `!Send`) — see the identical pattern on
// `fauna_folders_machine::FolderNestApi`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait DevicesNestApi: fauna_core::MaybeSendSync + std::fmt::Debug {
    /// `fauna.sync.devices.list` — all registered devices.
    ///
    /// Returns the **wire** rows, not [`DeviceSummary`], for the same reason
    /// [`Self::list_conflicts`] does: the label may rest sealed, and the render
    /// needs the reader's own custody, which lives on the machine rather than on
    /// this adapter (`DevicesMachine::render_devices`).
    async fn list_devices(&self) -> Result<Vec<SyncDevice>, DevicesApiError>;

    /// `fauna.folders.list` — all folders the bearer owns.
    ///
    /// Returns the **wire** rows, not [`crate::snapshots::FolderSummary`], for
    /// the same reason [`Self::list_devices`] and [`Self::list_conflicts`] do:
    /// the selective-sync `include_paths`/`exclude_paths` pair may rest sealed,
    /// and the render needs the reader's own owner custody, which lives on the
    /// machine rather than on this adapter
    /// (`DevicesMachine::render_folders`). Transcribing here instead would
    /// drop the `*_sealed` columns before anything could open them — which is
    /// exactly the bug this signature fixes: post-flip the nest rests no
    /// plaintext for either list, so the page rendered both as empty and a save
    /// from that page overwrote the user's real lists with nothing.
    async fn list_folders(&self) -> Result<Vec<WireFolderSummary>, DevicesApiError>;

    /// Whether each of `rows` is WebDAV-served **by the owner's custody** —
    /// `fauna_client_folders::custody_served` over the account's folder-key
    /// custody, one answer per row in order (ruling (7)(b)(ii) rule (2),
    /// `writer-signed-change-records.md`). The nest's `webdav_enabled` flag is
    /// never the answer: a nest flagging a set served that the owner never
    /// served must not paint the toggle ON.
    ///
    /// `rows` arrive with their set names already rendered — the served
    /// window of an unshared set rests at the pseudo-channel derived from the
    /// plaintext name. Fails closed: this default (a seam with no custody) and
    /// an unreadable custody answer `false` for every row.
    async fn webdav_served(&self, rows: &[WireFolderSummary]) -> Vec<bool> {
        vec![false; rows.len()]
    }

    /// `fauna.web.get_subdomain_enabled` — whether this actor's per-handle web
    /// address is switched on, for the website toggle's tri-state hint
    /// (`fauna_folders_machine::website_serve_hint`; `ui/folders.md`
    /// § Audience and website serving).
    ///
    /// **Best-effort by signature**: `None` means unknown — an adapter that
    /// has not wired it (this default), or a
    /// transport fault — and the hint then degrades to the combined wording
    /// that hedges both halves. Deliberately NOT a `Result`: no answer here
    /// may ever fail the page, and the only consumer treats every failure
    /// identically.
    async fn web_subdomain_enabled(&self) -> Option<bool> {
        None
    }

    /// `fauna.sync.conflicts.list` with `include_resolved = true` — the
    /// review-list read: auto-resolved rows (winner + retained parents) plus
    /// any still-unresolved (mark-only) reports (file-sync.md § Conflicts).
    ///
    /// Returns the **wire** rows, not [`ConflictSummary`]: the machine renders
    /// each path sealed-first under its own label custody before transcribing,
    /// because the summary's `file_info` line is precomputed from `path` and
    /// would otherwise be built from a name the reader was never meant to see —
    /// or from a scrubbed empty one. Same division as `MediaNestApi`, which
    /// likewise hands the machine wire items and lets it render at ingest.
    async fn list_conflicts(&self) -> Result<Vec<SyncConflict>, DevicesApiError>;

    /// `fauna.sync.devices.delete` — unregister a device.
    async fn remove_device(&self, device_id: &str) -> Result<(), DevicesApiError>;

    /// `fauna.sync.devices.p2p_participation.set`, the OWNER arm: ask
    /// another of the account's devices to turn its peer transfers off
    /// (`behavior/p2p.md` § Per-device participation). The only value this
    /// seam can send is `false` — enabling is that device's own local act.
    async fn request_p2p_off(&self, device_id: &str) -> Result<(), DevicesApiError>;

    /// `fauna.folders.delete` — delete a folder (non-cascading).
    async fn delete_folder(&self, name: &str) -> Result<(), DevicesApiError>;

    /// `fauna.sync.conflicts.resolve` — resolve conflict `id`. `None` is the
    /// candidate-free (mark-only) resolve, which signs nothing. `Some(winner)`
    /// keeps that candidate version: the head the nest mints is signed by this
    /// device, so the seam signs it only over a version the judged history
    /// vouches for (`writer-signed-change-records.md` ruling (10)(f)) — found
    /// under the hash of `winner.path`, verbatim-restorable, and equal in
    /// device, size and generation to the candidate row — and otherwise
    /// refuses, nothing sent.
    async fn resolve_conflict(
        &self,
        id: i64,
        winner: Option<ChosenWinner>,
    ) -> Result<(), DevicesApiError>;

    /// What the **judged** version history of `path` in `folder` says of the
    /// version a conflict candidate names by `manifest_hash` (ruling
    /// (10)(a)/(b)): the conflict row is the nest's word, so the review list
    /// re-points only at a version the shared judge admitted, looked up under
    /// the hash of the very path the restore record will carry (`path` is the
    /// rendered path — the seam derives the hash, never reads it off the
    /// conflict row). A seam with no identity or custody cannot judge and
    /// answers an error — it could not sign the record either.
    async fn judge_candidate(
        &self,
        folder: &str,
        path: &str,
        manifest_hash: &str,
    ) -> Result<CandidateVerdict, DevicesApiError>;

    /// `fauna.folders.update` with only the selective-sync path fields set
    /// (retention left unchanged).
    ///
    /// `include_sealed`/`exclude_sealed` carry each list sealed under the owner's
    /// root when the caller holds a key (path-sealing S6-c —
    /// `DevicesMachine::seal_paths_for` mints them). ⚠ They are **not optional
    /// extras**: the nest writes each seal with its plaintext, so passing `None`
    /// beside a `Some` list deliberately *clears* any existing seal rather than
    /// leaving one that opens to the list being replaced.
    async fn set_folder_paths(
        &self,
        name: &str,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
        include_sealed: Option<Vec<u8>>,
        exclude_sealed: Option<Vec<u8>>,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.folders.update` with only `conflict_policy` set (`"auto"` |
    /// `"latest_wins_always"`) — the per-set `folder-conflict-policy-select`
    /// edit. The nest row is the single authoritative source the resolving
    /// device reads (file-sync.md § Conflicts, policy).
    async fn set_folder_conflict_policy(
        &self,
        name: &str,
        conflict_policy: &str,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.folders.update` with only `audience` set — the write behind
    /// `folder-audience-select` (`ui/folders.md` § Audience and website
    /// serving). Every other row field stays unchanged.
    ///
    /// **Keyless**: no MLS engine, no content key. The back-catalogue is moved
    /// by each device's own engine at its next catch-up off the projected
    /// audience (`SyncEngine::converge_corpus_to_audience`), not by this caller.
    ///
    /// The door for **every** direction the picker offers — `"public"`,
    /// `"private"`, and `"shared"` on a bound folder exiting its public window
    /// (the flip-back). No direction stages anything: the projection
    /// is the cross-device signal, and it reaches members where a per-actor
    /// custody sentinel never could.
    ///
    /// **The projection is the signal, not the authority: a `→public` flip
    /// carries the owner's signed attestation** (`encryption-at-rest.md`
    /// § Readable classes → *The declassification is owner-ATTESTED*), minted
    /// by `FoldersClient::set_audience` under `attestor` — the identity key the
    /// machine's build glue wired ([`crate::DevicesMachine::set_audience_attestor`]).
    /// `None` still lands the flip, but no verifying seat unseals the folder;
    /// an embedder offering the audience control must wire it.
    async fn set_folder_audience(
        &self,
        name: &str,
        audience: &str,
        attestor: Option<Arc<fauna_core::identity::ActorKeypair>>,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.folders.update` with only `website_enabled` set — the write behind
    /// `folder-website-toggle` (`ui/folders.md` § Audience and website serving).
    ///
    /// **The only door to a website folder** since phase 2 slice e retired the
    /// wizard's mode step. Orthogonal to the audience, which decides who may
    /// *read* what is published: enabling it on a folder that is neither
    /// `public` nor paywalled is allowed and inert, and the app says so through
    /// `fauna_folders_machine::website_serve_hint` rather than by disabling the
    /// control.
    async fn set_folder_website_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.folders.update` with only `residency` set — the write behind
    /// `folder-nest-residency-select` / `folder-residency-confirm` (folders
    /// re-model phase 5; `file-sync.md` § Content residency). Its own field,
    /// deliberately never folded into the batched `folder-nest-*` policy
    /// record `set_folder_nest_place` sends — an older writer's policy edit
    /// must never silently clear it.
    ///
    /// **The flip to `metadata_only` is consent-gated in the app**: the nest
    /// deletes its chunk bytes for the folder on that write, so the app arms
    /// `folder-residency-confirm` naming exactly that and calls this only once
    /// answered. The flip back to `full` commits on change, like the audience
    /// select's non-destructive directions.
    async fn set_folder_residency(
        &self,
        name: &str,
        residency: &str,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.folders.places.set` — the write behind the expanded owner row's
    /// place editor (`folder-place-row` + its three checkboxes;
    /// `ui/folders.md` § Implementation status today).
    ///
    /// ⚠ **The point applies whole.** All three flags ride every call: pass the
    /// seat's full triple, never just the box that moved, or the two left alone
    /// are silently cleared. **Every** flag point is writable, so this seam
    /// neither refuses nor rounds a point.
    async fn set_folder_place(
        &self,
        name: &str,
        device_id: &str,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.folders.update` carrying the **nest place's whole snapshot
    /// policy** — the `folder-nest-*` editor's save (`backup-restore.md` § 8b
    /// owns the behavior). Every other row field stays unchanged.
    ///
    /// ⚠ **The two `nest_place` knobs are sent whole and applied whole**:
    /// `snapshots` / `quiet_secs` passed as `None` CLEAR back to unset, they do
    /// not mean "leave alone". That is deliberate — it is how three states per
    /// knob (on / off / use-the-default) survive a wire that cannot round-trip a
    /// nested `Option` — so a caller always sends the full policy it wants to
    /// rest, never a delta.
    ///
    /// ⚠⚠ **`retention` is the exception, and getting it wrong is silent.** It
    /// is the same policy's third knob but rides its own
    /// `FolderUpdateRequest::retention_policy`, a plain `Option<String>` whose
    /// `None` means **leave unchanged** — the wire cannot express "clear" there
    /// for the same dag-cbor reason. To clear it, pass the canonical
    /// binds-nothing policy `{"max_snapshots":0,"max_age_days":0}`, which the
    /// nest reads as `FolderRetention::NotSet`. Passing `None` expecting a clear
    /// leaves the old policy in force, and nothing surfaces.
    ///
    /// `version_retention` is the FOURTH per-place knob (`file-versions.md`
    /// § Retention ruling 1), riding the same one update as its own whole
    /// policy: `None` = leave unchanged (the arm an app without the knobs
    /// rides — it can never clear a policy its user cannot see);
    /// `Some(binds-nothing)` = clear (the nest rests the column `NULL`).
    async fn set_folder_nest_place(
        &self,
        name: &str,
        snapshots: Option<bool>,
        quiet_secs: Option<i64>,
        retention: Option<String>,
        version_retention: Option<fauna_folders_machine::VersionRetentionWrite>,
    ) -> Result<(), DevicesApiError>;

    /// `fauna.sync.changes.record` re-point (the shared
    /// `SyncClient::restore_version` record — an ordinary `modify` carrying the
    /// historical manifest verbatim; file-sync.md § Restore). Drives the review
    /// list's one-tap "use the other version".
    ///
    /// `path_sealed` is minted by the machine from its [`LabelCustody`]
    /// (best-effort — `None` records plaintext-only, an S8 backfill row); the
    /// transport carries it verbatim. Sealing at the machine keeps this trait's
    /// impls key-free, the same division `set_folder_paths` uses.
    ///
    /// [`LabelCustody`]: fauna_core::label_custody::LabelCustody
    #[allow(clippy::too_many_arguments)]
    async fn restore_file_version(
        &self,
        folder: &str,
        device_id: &str,
        path: &str,
        manifest_hash: String,
        size_bytes: i64,
        content_key_version: Option<u64>,
        path_sealed: Option<Vec<u8>>,
    ) -> Result<(), DevicesApiError>;
}

/// Builds the embedded folder creation wizard when the user opens it.
///
/// Separated from [`DevicesNestApi`] because the wizard machine carries its own
/// nest seam (`FolderNestApi`) + observer; the factory binds the *same*
/// transport requester the page reads use, so the wizard's `submit()` rides the
/// same WS-RPC connection. (`FolderWizardMachine::new` takes an
/// `Arc<dyn FolderNestApi>` with no FFI ABI, so the page machine can't accept a
/// requester directly — the factory closes over it instead.) Tests pass a
/// [`FakeWizardFactory`] that builds the wizard over a `FakeFolderNestApi`.
pub trait WizardFactory: fauna_core::MaybeSendSync {
    fn build_wizard(
        &self,
        observer: Arc<dyn FolderWizardObserver>,
        available_devices: Vec<DeviceOption>,
    ) -> Arc<FolderWizardMachine>;
}
