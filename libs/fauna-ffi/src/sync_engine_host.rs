//! UniFFI surface for the in-process file-sync engine host — the
//! construct-run-drop engine work the apple and android apps run in their own
//! process (`docs/goal/behavior/sync-engine-deployments.md` § Apple apps —
//! convergence design).
//!
//! Every **resident** (watcher-driven) engine lives in the per-user
//! `fauna-sync-agent` process (`sync-agent.md` § Control plane split); this host
//! runs none. What it serves is the work an app does *itself*, in process: the
//! photo-library / watched-directory sealed ingest, the per-file display-state
//! and transfer-backlog reads, and the macOS `MacRestoreView` restore walk.
//!
//! ## What this module is, and is not
//!
//! It is **wiring**, deliberately: every decision of consequence lives in shared
//! Rust one layer down. What happens when an engine is *built* (device
//! registration, the **fail-closed** content-key binding, the seal roots) is
//! [`fauna_sync_engine::engine_lifecycle`] — shared with the File Provider host,
//! so the security-critical binding decision has exactly one implementation.
//!
//! The resident half this host used to carry (a live engine per device-local
//! `location-map.json` binding, iOS scene-phase pause/resume, the iOS one-shot
//! pass over those bindings) was retired 2026-09-25: no app binds a location
//! in-process any more (`on-demand-files.md` § Hosting multiple on-demand
//! folders).
//!
//! ## Why a dedicated worker thread (the `FfiBackupCoordinator` precedent)
//!
//! [`SyncEngine`](fauna_sync_engine::engine::SyncEngine) is `Send` but
//! intentionally **`!Sync`** (its rusqlite `Connection`), with `&self` async
//! methods held across `.await`s — so it cannot be `tokio::spawn`ed, and UniFFI's
//! async exports demand a `Send` future. The host therefore keeps its engines on
//! a dedicated OS thread with a current-thread runtime and talks to it over a
//! channel; the futures returned across the FFI boundary await a `oneshot` and
//! are themselves `Send`. This is exactly the reasoning (and shape) of the
//! `FfiBackupCoordinator` that used to live in `segment_backup.rs` (deleted
//! 2026-08-16 with the client upload driver — the reasoning is reproduced above
//! precisely because its worked example is gone). The channel/thread/runtime
//! plumbing itself is [`crate::worker_thread`]'s `spawn_worker_thread`, shared
//! with [`crate::file_provider_host`].
//!
//! Each request builds the engine it needs, uses it, and drops it — an iOS
//! background task gets a bounded slice of wall-clock and must not hold a
//! filesystem watch across suspension.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use fauna_client::{AuthClient, NestClient};
use fauna_core::data::ContentHash;
use fauna_core::folder_keys::FolderRef;
use fauna_mls::engine::MlsEngine;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SnapshotFileToRestore;
use fauna_sync_engine::engine_lifecycle::{
    EngineCredential, EngineParams, build_engine, build_restore_engine,
};
use fauna_sync_engine::progress::ProgressTx;

use crate::FfiError;

/// One file's sync state as the client renders it: the six-state display
/// vocabulary (`file-sync.md` § Per-file sync-status display), never the engine's
/// internal eight.
///
/// The converged apple engine is the ratified **first consumer** of that map
/// (`SyncState::to_display()`); the label text comes from the shared
/// `sync_display_state_label`, and only the badge's icon/color is a per-app
/// render.
#[derive(uniffi::Record)]
pub struct FfiSyncFileState {
    /// Path relative to the folder's root.
    pub path: String,
    /// Where the bytes are, in the user-facing vocabulary.
    pub state: fauna_core::format::SyncDisplayState,
    /// File size in bytes as last recorded by the engine.
    pub size_bytes: i64,
}

/// One bound set's transfer backlog + freshness stamps, as Swift sees it
/// ([`FfiSyncEngineHost::transfer_backlog`]).
///
/// A projection of `fauna_sync_engine::db::TransferBacklog`, whose doc comment
/// owns every field's meaning — kept as a separate record only because UniFFI
/// needs the derive and the engine type is shared with non-FFI consumers.
/// `last_sync` is the type's own `last_sync_at()` (the freshest of the two
/// stamps), resolved here so each app does not re-derive the max.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiSyncBacklog {
    /// Tracked files whose display badge reads `Uploading`/`Downloading`.
    pub files_pending: u64,
    /// Whole-file sizes of those files — the queue reading, never a progress
    /// bar (remaining-chunk counts do not exist locally; manifests are remote).
    pub bytes_pending: u64,
    /// When this device was last **known consistent** with the nest, epoch
    /// secs. `None` before the first transfer or clean pass.
    pub last_sync: Option<i64>,
}

/// The outcome of a client-side full restore
/// ([`FfiSyncEngineHost::restore_snapshot_to_dir`]).
///
/// `Debug` because this summary IS the restore's own witness — the bug was
/// a `files_restored: 0` that still returned `Ok`, so a failing assertion on it
/// has to be able to print what it actually got. Three counters, no user content.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiRestoreSummary {
    /// Files decrypted + written into the chosen directory.
    pub files_restored: u64,
    /// Total plaintext bytes written.
    pub bytes_written: u64,
    /// Entries skipped by the path-traversal guard (never written).
    pub skipped: u64,
}

/// Parse the `folder_id` every set-addressing call on this surface takes — a
/// [`FolderRef`] wire string (`local:<id>` / `foreign:<hex>`, the value the
/// shared `folder_ref_for_row` mints for a row) — refusing anything else.
///
/// The ref is a set's only key; a name is a label two sets can share
/// (`on-demand-files.md` § Hosting multiple on-demand folders). So a caller
/// still holding only a name gets a refusal here, never a by-name lookup:
/// resolving a name to a ref is the caller's act, over the row it displays.
pub(crate) fn parse_folder_id(folder_id: &str) -> Result<FolderRef, FfiError> {
    FolderRef::parse(folder_id).ok_or_else(|| FfiError::General {
        msg: format!("not a folder ref: {folder_id:?} (expected `local:<id>` or `foreign:<hex>`)"),
    })
}

/// One request to the construct-run-drop worker thread.
enum Request {
    /// Seal + ingest one already-exported file into the set (photo library).
    Ingest {
        folder_ref: FolderRef,
        source_path: PathBuf,
        relative_path: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Read every tracked file's display state for the set.
    States {
        folder_ref: FolderRef,
        reply: oneshot::Sender<Result<Vec<FfiSyncFileState>, String>>,
    },
    /// Read the set's aggregate transfer backlog + freshness stamps.
    Backlog {
        folder_ref: FolderRef,
        reply: oneshot::Sender<Result<FfiSyncBacklog, String>>,
    },
    /// Restore a folder snapshot's files into `output_dir` via the client-side
    /// walk (the macOS `MacRestoreView` full restore).
    Restore {
        snapshot_id: i64,
        output_dir: PathBuf,
        reply: oneshot::Sender<Result<FfiRestoreSummary, String>>,
    },
}

/// The in-process engine host handed to the app at login.
///
/// Drop semantics: dropping the handle closes the request channel, so the worker
/// thread's loop exits and its runtime shuts down. Every `SyncDb` connection is
/// opened and dropped inside one request on that thread, which is what rusqlite
/// wants.
#[derive(uniffi::Object)]
pub struct FfiSyncEngineHost {
    /// Construct-run-drop requests (ingest / state read / backlog / restore).
    requests: mpsc::Sender<Request>,
}

/// Everything a request needs to build its own `SyncEngine`, cloned per use.
#[derive(Clone)]
pub(crate) struct HostContext {
    state_dir: PathBuf,
    device_id: [u8; 32],
    device_label: String,
    auth: Arc<AuthClient>,
    nest_rpc: Arc<NestClient>,
    mls: Option<Arc<MlsEngine>>,
    credential: EngineCredential,
    /// The account's folder-key custody every build of this host reads — the
    /// seat's store on the in-process app host, the capability host's cold
    /// fleet replica on the app-dead one (`on-demand-files.md` § Shared sets on
    /// a capability host, decision 1′).
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
    /// What this host does with a set the account may only read: the
    /// control-inverted File Provider host builds it read-only, the in-process
    /// app host (over the user's own directories) refuses it.
    reader_hosting: fauna_sync_engine::engine_lifecycle::ReaderHosting,
    /// The account's attested predecessors, each paired with its retired
    /// owner key, nearest hop first — the registry's one walk
    /// (`AccountRegistry::predecessor_backup_keys_by_actor`), resolved in
    /// Rust off the registry the app hands over
    /// (`writer-signed-change-records.md` ruling (8)(b) source (ii) and
    /// (8)(c): a row a retired identity signed verifies as this account's and
    /// opens only under that identity's root and earlier ones). Empty for an
    /// identity that never succeeded — and for a host handed no registry,
    /// whose engines then prove the ids by the statement walk
    /// ([`Self::learned_predecessors`]) and open under no retired root.
    predecessors: Vec<fauna_core::file_download::PredecessorSealKey>,
    /// What this host's engines proved by the statement walk, shared across
    /// every build: these hosts construct an engine per request (or per
    /// rebuild), and a link proven once is neither asked for again nor read
    /// as a fresh gain by the next build.
    learned_predecessors: fauna_client_sync::row_judge::LearnedPredecessors,
}

impl HostContext {
    /// The engine params for one set at `watch_dir`.
    pub(crate) fn params(
        &self,
        watch_dir: PathBuf,
        folder_ref: FolderRef,
        progress_tx: ProgressTx,
    ) -> EngineParams {
        EngineParams {
            state_dir: self.state_dir.clone(),
            watch_dir,
            folder_ref,
            device_id: self.device_id,
            device_label: Some(self.device_label.clone()),
            auth: Arc::clone(&self.auth),
            nest_rpc: Arc::clone(&self.nest_rpc),
            mls: self.mls.clone(),
            credential: self.credential.clone(),
            progress_tx,
            // The seed-holding app host signs directly with the identity key
            // (`build_engine`'s `Seed` arm); the seed-less File Provider host
            // sets the machine principal's signer the app provisioned it
            // (`file_provider_host::Resident::build`).
            change_signer: None,
            // The paired chain names its own ids (`binding_predecessors`), so
            // no separate attested list is passed; a host handed no chain
            // proves the link by the statement walk. No in-process host
            // observes the park flag.
            predecessor_backup_keys: self.predecessors.clone(),
            predecessor_actor_ids: Vec::new(),
            learned_predecessors: self.learned_predecessors.clone(),
            access_gate: None,
            folder_keys: Arc::clone(&self.folder_keys),
            reader_hosting: self.reader_hosting,
        }
    }

    /// Read `folder_ref`'s binding basis now — its row, or a cross-nest set's
    /// custody record ([`fauna_sync_engine::engine_lifecycle::fetch_binding_basis`])
    /// — for a resident host's edges.
    #[cfg(feature = "file-provider-host")]
    pub(crate) async fn binding_basis(
        &self,
        folder_ref: FolderRef,
    ) -> Option<Option<fauna_sync_engine::engine_lifecycle::BindingBasis>> {
        fauna_sync_engine::engine_lifecycle::fetch_binding_basis(
            &self.nest_rpc,
            &*self.folder_keys,
            folder_ref,
        )
        .await
    }

    /// The host's nest control-plane client. The File Provider host connects it
    /// before serving (the seed-based `FfiSyncEngineHost` is instead handed the
    /// app's already-connected client).
    #[cfg(feature = "file-provider-host")]
    pub(crate) fn nest_rpc(&self) -> &Arc<NestClient> {
        &self.nest_rpc
    }

    /// Where this host's per-set state DBs live — what the share plane's
    /// serve half opens its own read connection under.
    #[cfg(all(feature = "file-provider-host", feature = "p2p-share"))]
    pub(crate) fn state_dir(&self) -> &Path {
        &self.state_dir
    }
}

impl FfiSyncEngineHost {
    /// Rust-side constructor (the `FfiNestClient::sync_engine_host` factories
    /// call this). Spawns the construct-run-drop worker.
    ///
    /// Does **not** require an ambient tokio runtime: the worker thread brings its
    /// own (production callers are inside an async context anyway, but this
    /// contract keeps the handle constructible from sync code and from tests).
    pub(crate) fn start(ctx: HostContext) -> Arc<Self> {
        Arc::new(Self {
            requests: spawn_request_worker(ctx),
        })
    }

    /// Send a request to the worker and await its reply, mapping a dead worker
    /// (thread gone / host dropped) onto an [`FfiError`] rather than a panic.
    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Request,
    ) -> Result<T, FfiError> {
        let (tx, rx) = oneshot::channel();
        self.requests
            .send(make(tx))
            .await
            .map_err(|_| FfiError::General {
                msg: "sync engine host is not running".into(),
            })?;
        rx.await
            .map_err(|_| FfiError::General {
                msg: "sync engine host stopped before replying".into(),
            })?
            .map_err(|msg| FfiError::General { msg })
    }
}

#[fauna_uniffi_async::export]
impl FfiSyncEngineHost {
    /// **Sealed per-file ingest** — the photo-library ingress path (`file-sync.md`
    /// § convergence design, library-ingress bullet). An OS-owned photo library is
    /// not a folder, so it gets no watch-dir engine: Swift exports one asset to a
    /// temp file and calls this, which pushes it through the ordinary **sealed**
    /// chunk pipeline (+ `changes.record` custody upsert) and deletes
    /// the temp.
    ///
    /// No `watch_dir` semantics, no reconcile pass — so deleting the staged file
    /// cannot tombstone the ingested one, and the device never carries a duplicate
    /// photo library on disk.
    ///
    /// `folder_id` is the set's `FolderRef` wire string ([`parse_folder_id`];
    /// the photo set's comes back from `photo_library_set`); `relative_path` is
    /// the file's path *within the set* (e.g. `2026/07/IMG_0042.heic`);
    /// `source_path` is the temp file Swift exported.
    pub async fn ingest_file(
        &self,
        folder_id: String,
        source_path: String,
        relative_path: String,
    ) -> Result<(), FfiError> {
        let folder_ref = parse_folder_id(&folder_id)?;
        self.call(|reply| Request::Ingest {
            folder_ref,
            source_path: PathBuf::from(source_path),
            relative_path,
            reply,
        })
        .await
    }

    /// Every tracked file in the set `folder_id` names ([`parse_folder_id`])
    /// with its **display** state — the read behind the `sync-state-badge` on
    /// the media page. Deleted files are omitted (they have no row to render).
    pub async fn file_states(&self, folder_id: String) -> Result<Vec<FfiSyncFileState>, FfiError> {
        let folder_ref = parse_folder_id(&folder_id)?;
        self.call(|reply| Request::States { folder_ref, reply })
            .await
    }

    /// The set's aggregate transfer backlog + freshness stamps — the same
    /// `SyncDb::transfer_backlog()` projection the desktop agent serves over
    /// its pipe as `SyncStatusInfo`/`EngineInfo` (`sync-agent.md` § Local agent
    /// health → *the sync-status projection*, which owns every field's
    /// meaning).
    ///
    /// Exposed here because iOS and in-app macOS run their engines **in
    /// process** via this host rather than behind `fauna-sync-agent`, so the
    /// pipe that carries these numbers on windows/linux does not exist for
    /// them; without this they have no route to the projection at all. One
    /// call per bound set — aggregation across sets is the caller's, exactly
    /// as it is for the agent's own `aggregate_backlog` helper.
    ///
    /// This is the **durable queue** half ("3 items, 2.4 GB", survives a
    /// restart, polled). The live per-byte push is a different channel with
    /// different semantics — see that same section's *live transfer window*.
    pub async fn transfer_backlog(&self, folder_id: String) -> Result<FfiSyncBacklog, FfiError> {
        let folder_ref = parse_folder_id(&folder_id)?;
        self.call(|reply| Request::Backlog { folder_ref, reply })
            .await
    }

    /// Restore a **folder** snapshot's files into `output_dir` via the client-side
    /// walk (fetch manifest + chunks by store key, decrypt under the owner
    /// `BackupKey`, verify the content address, write). This is the macOS
    /// `MacRestoreView` full restore; unlike the legacy server-side ZIP route it
    /// works on **sealed** snapshots, because the plaintext only ever materializes
    /// where the owner's keys live. `output_dir` is the user-chosen folder.
    pub async fn restore_snapshot_to_dir(
        &self,
        snapshot_id: i64,
        output_dir: String,
    ) -> Result<FfiRestoreSummary, FfiError> {
        self.call(|reply| Request::Restore {
            snapshot_id,
            output_dir: PathBuf::from(output_dir),
            reply,
        })
        .await
    }
}

/// Spawn the construct-run-drop worker on its dedicated OS thread (a current-thread
/// runtime — see the module docs on why the host keeps its engines off the shared
/// async executor) and return the request channel.
fn spawn_request_worker(ctx: HostContext) -> mpsc::Sender<Request> {
    crate::worker_thread::spawn_worker_thread(
        "fauna-sync-ffi-host",
        "sync-engine",
        32,
        move |requests| request_worker(ctx, requests),
    )
}

/// The construct-run-drop worker: services one request at a time on the caller's
/// current-thread runtime. Each request builds the engine it needs and drops it
/// before the reply is sent, so no `SyncDb` connection outlives its request.
async fn request_worker(ctx: HostContext, mut requests: mpsc::Receiver<Request>) {
    while let Some(request) = requests.recv().await {
        match request {
            Request::Ingest {
                folder_ref,
                source_path,
                relative_path,
                reply,
            } => {
                let result = ingest(&ctx, folder_ref, source_path, relative_path).await;
                let _ = reply.send(result);
            }
            Request::States { folder_ref, reply } => {
                let _ = reply.send(file_states(&ctx.state_dir, folder_ref));
            }
            Request::Backlog { folder_ref, reply } => {
                let _ = reply.send(transfer_backlog(&ctx.state_dir, folder_ref));
            }
            Request::Restore {
                snapshot_id,
                output_dir,
                reply,
            } => {
                let result = restore(&ctx, snapshot_id, output_dir).await;
                let _ = reply.send(result);
            }
        }
    }
}

/// Seal + ingest one exported asset into the set, then delete the temp file.
///
/// The engine is built over a **staging dir** rather than a watch dir: the ingest
/// path only ever calls `upload_file`, which runs the full sealed chunk pipeline
/// and the `changes.record` custody upsert, and never `reconcile` (which is what
/// would otherwise read a just-deleted staged file as a deletion and tombstone the
/// ingested one). The staging dir is keyed like the state DB — by the set's
/// identity (`FolderRef::db_component`), so two same-named sets never share one.
async fn ingest(
    ctx: &HostContext,
    folder_ref: FolderRef,
    source_path: PathBuf,
    relative_path: String,
) -> Result<(), String> {
    let folder = folder_ref.to_wire();
    let staging = ctx.state_dir.join("ingest").join(folder_ref.db_component());
    let staged = staging.join(&relative_path);
    if let Some(parent) = staged.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create staging dir: {e}"))?;
    }
    // Move when we can (same volume — free); fall back to copy across volumes.
    if std::fs::rename(&source_path, &staged).is_err() {
        std::fs::copy(&source_path, &staged).map_err(|e| format!("stage asset: {e}"))?;
        let _ = std::fs::remove_file(&source_path);
    }

    let params = ctx.params(staging, folder_ref, None);
    let result = async {
        let built = build_engine(params).await.ok_or_else(|| {
            format!("folder {folder}: content-key binding indeterminate — refusing to ingest")
        })?;
        built
            .engine
            .upload_file(&relative_path)
            .await
            .map(|_outcome| ())
            .map_err(|e| format!("ingest {relative_path}: {e}"))
    }
    .await;

    // The staged copy is scratch either way: on success the bytes are sealed on the
    // nest, on failure Swift still holds the asset in PhotoKit and the pass re-runs.
    let _ = std::fs::remove_file(&staged);
    result
}

/// Restore a **folder** snapshot's files into `output_dir` via the client-side
/// walk — the macOS `MacRestoreView` full restore
/// (`docs/goal/behavior/backup-restore.md` § 4 + § Restoring Files, re-ratified
/// 2026-07-14).
///
/// The snapshot's per-file `manifest_hash` (which the FFI snapshot mirror
/// deliberately drops) is read here server-side from the `SnapshotsClient::get`
/// protocol reply and never crosses back to Swift. Each file is fetched +
/// decrypted + verified + written by [`build_restore_engine`] +
/// [`fauna_sync_engine::engine::SyncEngine::restore_snapshot_files_to_dir`], so a
/// **sealed** snapshot — which the server-side ZIP route now refuses loudly —
/// restores correctly here, where the owner `BackupKey` lives.
///
/// ⚠ That last claim was **false from the S9 flip until 2026-08-03**, and the shape of the lie is worth keeping: the *bytes* half
/// was always right — the seed was here and the chunks decrypted fine — while
/// the *listing* half read keyless, so there were never any rows to walk. The
/// two halves of "restores correctly" are wired separately, and only one of them
/// was. A doc comment asserting an end-to-end property is worth exactly the
/// weakest link it does not mention.
///
/// Message-kind (`mail` / `calendar` / `conv`) snapshots are refused: they restore
/// through `fauna.filesync.snapshot.restore_message_kind`, not a file walk.
async fn restore(
    ctx: &HostContext,
    snapshot_id: i64,
    output_dir: PathBuf,
) -> Result<FfiRestoreSummary, String> {
    // Owner restore seals/opens under the identity-derived `BackupKey`, so it needs
    // the seed. Only the identity-holding host (`FfiSyncEngineHost`) reaches this
    // path; a seed-less File Provider host has no restore surface.
    //
    // Resolved BEFORE the fetch, because the listing read needs it too: the same
    // seed derives the label custody that opens the snapshot's sealed paths. It
    // also means a seed-less host fails fast, without a pointless round-trip.
    let secret_hex = match &ctx.credential {
        EngineCredential::Seed(secret_hex) => secret_hex.as_str(),
        EngineCredential::BackupKey(_) => {
            return Err("snapshot restore requires the identity seed (seed-less host)".into());
        }
    };
    // `crate::crypto` is the crate's ONE owner-secret length check (the five
    // copies that once made the bare `crate::secret32` glob E0659-ambiguous were
    // consolidated there). Module-qualified rather than glob-resolved so a future
    // second copy cannot silently re-point this call.
    let secret = crate::crypto::secret32(
        &hex::decode(secret_hex)
            .map_err(|e| format!("snapshot restore: identity seed is not hex: {e}"))?,
    )
    .map_err(|e| format!("snapshot restore: {e}"))?;

    // ⚠ `get_for_restore`, and custody-wired — NOT the bare `SnapshotsClient::new`
    // this used to build. A keyless client renders every
    // sealed row to `Omit`, `render_paths` drops them, and this fn returned
    // `files_restored: 0` with `Ok` — an empty directory reported as a successful
    // restore, while the seed that would have opened them sat in `ctx.credential`
    // and got spent on the chunks a few lines below. Both halves matter: the
    // custody makes the rows render, and `get_for_restore` makes any FUTURE
    // regression loud instead of silent (`fauna-client-snapshots`).
    let snapshot =
        crate::snapshots_client::owner_read_snapshots_client(Arc::clone(&ctx.nest_rpc), &secret)
            .get_for_restore(snapshot_id)
            .await
            .map_err(|e| format!("fetch snapshot {snapshot_id}: {e}"))?;
    if let Some(kind) = &snapshot.message_kind {
        return Err(format!(
            "snapshot {snapshot_id} is a '{kind}' message-kind snapshot — restore it via \
             fauna.filesync.snapshot.restore_message_kind, not the file walk"
        ));
    }

    let files = snapshot_files_to_restore(&snapshot.files)?;

    let engine = build_restore_engine(
        Arc::clone(&ctx.auth),
        Arc::clone(&ctx.nest_rpc),
        ctx.device_id,
        secret_hex,
    );
    let summary = engine
        .restore_snapshot_files_to_dir(&files, &output_dir)
        .await
        .map_err(|e| format!("restore snapshot {snapshot_id}: {e:#}"))?;

    // The shared walk (`fauna_sync_engine::engine::restore_snapshot_walk`) no
    // longer aborts the whole restore on one bad file — it writes everything
    // recoverable and names the rest in `summary.unverified`. Fail the command
    // loudly on a non-empty list rather than let `Ok` read as "every file
    // restored" when some were refused: the same contract `cmd_restore`'s own
    // `restore_summary_error` keeps for the CLI host of this walk.
    if !summary.unverified.is_empty() {
        return Err(format!(
            "restore incomplete: {} file(s) failed their whole-file integrity check and \
             were NOT written (everything else was restored): {}",
            summary.unverified.len(),
            summary.unverified.join(", ")
        ));
    }

    Ok(FfiRestoreSummary {
        files_restored: summary.files_restored,
        bytes_written: summary.bytes_written,
        skipped: summary.skipped,
    })
}

/// Map a snapshot's file entries to the restore walk's inputs.
///
/// **Only regular files** carry a restorable manifest (the nest records the value
/// `"regular"`, **not** `"file"` — `bins/fauna-nest/src/db/sync_storage.rs`).
/// Directories are recreated by the write path and symlinks are not materialized
/// by this v1 walk, so both are dropped here. A malformed `manifest_hash` (not 32
/// bytes) is a hard error — a corrupt listing must not silently restore a subset.
pub(crate) fn snapshot_files_to_restore(
    files: &[fauna_protocol::filesync::SnapshotFileEntry],
) -> Result<Vec<SnapshotFileToRestore>, String> {
    files
        .iter()
        .filter(|f| f.file_type == "regular")
        .map(|f| {
            let raw: [u8; 32] = f
                .manifest_hash
                .as_ref()
                .try_into()
                .map_err(|_| format!("{}: manifest hash is not 32 bytes", f.path))?;
            Ok(SnapshotFileToRestore {
                relative_path: f.path.clone(),
                manifest_hash: ContentHash::from_digest_raw(raw),
                // Owner folder backup snapshots seal under the owner `BackupKey`
                // (no M2 content-key generation), matching `cmd_restore`.
                content_key_version: None,
            })
        })
        .collect()
}

/// Read every tracked file's display state straight from the set's state DB.
///
/// Opens its own `SyncDb` connection rather than routing through a live engine:
/// `SyncDb::open` sets a 5s `busy_timeout` precisely so a second connection can
/// read while an engine holds the DB.
///
/// **A set with no state DB reports no rows and creates nothing.** `SyncDb::open`
/// creates on open (works-out-of-the-box for the *hosting* paths), so before
/// this check a badge read for a set this device hosts nowhere — every set on
/// iOS, a remote-only set on macOS — left a stray empty `fsid-<ref>.db` behind
/// wherever it looked. On macOS's two-root layout that stray would land in
/// whichever root the read was routed to, so the read must be a pure read.
/// Same shape as [`transfer_backlog`]; takes `state_dir` rather than the whole
/// [`HostContext`] for the same reason it does.
///
/// Keyed by the set's identity ([`FolderRef::state_db_path`]) — the SAME file
/// the `fauna-sync-agent` writes for an agent-hosted set and the File Provider
/// host writes for a domain, which is what makes this a read of the writer's
/// state rather than of a file nobody writes.
fn file_states(state_dir: &Path, folder_ref: FolderRef) -> Result<Vec<FfiSyncFileState>, String> {
    let folder = folder_ref.to_wire();
    let path = folder_ref.state_db_path(state_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let db = SyncDb::open(path).map_err(|e| format!("open state db for {folder}: {e}"))?;
    let entries = db
        .list_all()
        .map_err(|e| format!("read file states for {folder}: {e}"))?;
    Ok(entries
        .into_iter()
        // `to_display()` is `None` for a deleted file — it has no row to render.
        .filter_map(|entry| {
            entry.state.to_display().map(|state| FfiSyncFileState {
                path: entry.path,
                state,
                size_bytes: entry.size_bytes,
            })
        })
        .collect())
}

/// [`FfiSyncEngineHost::file_states`] as a free function over an explicit
/// state dir — the **steward read** the macOS app makes into the app-group
/// container for its File-Provider-bound sets (`on-demand-files.md` § Apple
/// File Provider binding, *state unification*: macOS has two consent domains,
/// the extension hosts FP-bound sets in the container, the app's own host runs
/// in the user domain, and the Media-badge fold spans both). The host object
/// reads its own root; this reads any root the caller can reach, so the app
/// needs no second host — and no second nest connection — just to look at the
/// extension's `fsid-<ref>.db`. Pure read: a set with no DB there yields no rows
/// and creates nothing (the two-root fold routes every set to exactly one
/// root, and a read that minted a DB in the other would fake a local presence
/// the one-local-presence rule forbids). `folder_id` is the set's `FolderRef`
/// wire string ([`parse_folder_id`]).
#[uniffi::export]
pub fn sync_file_states(
    state_dir: String,
    folder_id: String,
) -> Result<Vec<FfiSyncFileState>, FfiError> {
    let folder_ref = parse_folder_id(&folder_id)?;
    file_states(Path::new(&state_dir), folder_ref).map_err(|msg| FfiError::General { msg })
}

/// One bound set's transfer backlog, read the same way [`file_states`] reads
/// its rows: a second connection on the worker thread, which `SyncDb::open`'s
/// 5s `busy_timeout` exists to make safe while an engine holds the DB.
///
/// A set with no state DB yet reports honest zeros rather than failing — the
/// agent's own handler makes the same call ("a bound-but-never-served set
/// reports honest zeros", `sync-agent.md` § Local agent health), and a status
/// read must not error just because nothing has synced yet.
/// Takes `state_dir` rather than the whole [`HostContext`] because that is all
/// it reads — which also makes it directly unit-testable against a temp dir,
/// where building a context would mean standing up a live nest connection.
fn transfer_backlog(state_dir: &Path, folder_ref: FolderRef) -> Result<FfiSyncBacklog, String> {
    let folder = folder_ref.to_wire();
    let path = folder_ref.state_db_path(state_dir);
    if !path.exists() {
        return Ok(FfiSyncBacklog {
            files_pending: 0,
            bytes_pending: 0,
            last_sync: None,
        });
    }
    let db = SyncDb::open(path).map_err(|e| format!("open state db for {folder}: {e}"))?;
    let backlog = db
        .transfer_backlog()
        .map_err(|e| format!("read transfer backlog for {folder}: {e}"))?;
    Ok(FfiSyncBacklog {
        files_pending: backlog.files_pending,
        bytes_pending: backlog.bytes_pending,
        last_sync: backlog.last_sync_at(),
    })
}

/// Build the host's context from an `FfiNestClient`'s live connection — the
/// `FfiNestClient::sync_engine_host` factory's other half, kept here so the
/// context's fields stay private to this module. `folder_keys` is where the
/// host reads the account's folder-key custody: a seat hands its account
/// runtime's door ([`crate::account_runtime::folder_key_store`]), a process
/// that hosts no runtime its own reader.
#[allow(clippy::too_many_arguments)]
pub(crate) fn host_context(
    state_dir: String,
    secret: [u8; 32],
    device_id: [u8; 32],
    device_label: String,
    nest_rpc: Arc<NestClient>,
    mls: Option<Arc<MlsEngine>>,
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
    predecessors: Vec<fauna_core::file_download::PredecessorSealKey>,
) -> HostContext {
    HostContext {
        state_dir: PathBuf::from(state_dir),
        device_id,
        device_label,
        // A fresh self-authenticating HTTP client for the chunk transport, paired
        // with the app's already-connected WS-RPC control plane (it
        // authenticates lazily on first request, so construction stays
        // non-blocking).
        auth: Arc::new(AuthClient::new(
            nest_rpc.nest_url().to_string(),
            fauna_core::identity::ActorKeypair::from_secret(secret),
        )),
        nest_rpc,
        mls,
        credential: EngineCredential::Seed(hex::encode(secret)),
        folder_keys,
        reader_hosting: fauna_sync_engine::engine_lifecycle::ReaderHosting::Refuse,
        predecessors,
        learned_predecessors: Default::default(),
    }
}

/// Build a **seed-less** [`HostContext`] for the app-dead File Provider extension
/// (`docs/goal/behavior/file-sync.md` § Who runs the hydration host): a
/// bearer-authenticated nest connection + a pre-derived owner `BackupKey`, and
/// **no identity seed**. The apple mirror of the bearer-only Windows sync-service
/// build (`AuthClient::bearer_only` + `NestClient::with_auth` over a live
/// [`BearerSource`](fauna_nest_http::BearerSource)).
///
/// The returned context's `nest_rpc` is **unconnected** — the FP host worker
/// connects it before serving (the seed-based [`host_context`] is instead handed
/// the app's already-connected client). `mls` is `None`, and stays so: a
/// capability host is never a group member — a bound set's content keys come
/// from the account's folder-key custody, read as the enrolled device the host
/// is a process of through its cold fleet replica
/// ([`crate::file_provider_host::CapabilityHostFolderKeys`]; `on-demand-files.md`
/// § Shared sets on a capability host, decision 1′).
#[cfg(feature = "file-provider-host")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn file_provider_host_context(
    nest_url: String,
    actor_id: [u8; 32],
    device_id: [u8; 32],
    device_label: String,
    backup_key: [u8; 32],
    bearer: Arc<dyn fauna_nest_http::BearerSource>,
    signer: Arc<dyn crate::file_provider_host::FfiChangeSignerProvider>,
    state_dir: String,
    predecessors: Vec<fauna_core::file_download::PredecessorSealKey>,
) -> HostContext {
    let http = fauna_client::pinned_http_client(&nest_url);
    let auth = Arc::new(AuthClient::bearer_only(nest_url, actor_id, bearer, http));
    let nest_rpc = NestClient::with_auth(Arc::clone(&auth));
    let folder_keys = Arc::new(crate::file_provider_host::CapabilityHostFolderKeys {
        auth: Arc::clone(&auth),
        actor_id,
        backup_key: fauna_core::crypto::BackupKey::from_bytes(backup_key),
        signer,
    });
    HostContext {
        state_dir: PathBuf::from(state_dir),
        device_id,
        device_label,
        auth,
        nest_rpc,
        mls: None,
        credential: EngineCredential::BackupKey(fauna_core::crypto::BackupKey::from_bytes(
            backup_key,
        )),
        folder_keys,
        // Control-inverted: no directory the user can write behind the engine,
        // so a reader's set is served read-only (`on-demand-files.md` § Shared
        // sets on a capability host, decision 3).
        reader_hosting: fauna_sync_engine::engine_lifecycle::ReaderHosting::ReadOnly,
        predecessors,
        learned_predecessors: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::filesync::SnapshotFileEntry;
    use std::collections::BTreeMap;

    fn entry(path: &str, file_type: &str, manifest: Vec<u8>) -> SnapshotFileEntry {
        SnapshotFileEntry {
            path: path.into(),
            manifest_hash: ByteBuf::from(manifest),
            size_bytes: 0,
            mtime: 0,
            mode: 0,
            file_type: file_type.into(),
            symlink_target: None,
            path_hash: None,
            path_sealed: None,
            extra: BTreeMap::new(),
        }
    }

    /// A status read must not *create* the thing it reads. `SyncDb::open` is
    /// open-**or-create** (it even `create_dir_all`s the parent), so reading a
    /// bound-but-never-served set through it would materialize an empty state
    /// DB as a side effect of asking "how much is pending?". Honest zeros
    /// instead — the same answer the desktop agent's handler gives for that
    /// case (`sync-agent.md` § Local agent health).
    #[test]
    fn a_never_served_set_reports_zeros_without_creating_a_db() {
        let tmp = tempfile::tempdir().unwrap();
        let documents = FolderRef::Local(3);
        let got = transfer_backlog(tmp.path(), documents).expect("read must not fail");
        assert_eq!(got.files_pending, 0);
        assert_eq!(got.bytes_pending, 0);
        assert_eq!(got.last_sync, None);
        assert!(
            !documents.state_db_path(tmp.path()).exists(),
            "a status read must not create the state DB"
        );
    }

    /// Every set-addressing call takes the set's `FolderRef` wire string and
    /// refuses anything else — a bare name in particular, which is exactly the
    /// key these calls used to take. The refusal is at the FFI boundary: no
    /// request reaches the worker, nothing is read or staged.
    #[test]
    fn a_bare_name_is_refused_at_the_boundary_and_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let host = FfiSyncEngineHost::start(test_ctx(tmp.path()));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let err = rt
            .block_on(host.ingest_file(
                "Photo Library".into(),
                tmp.path().join("nope.jpg").to_string_lossy().into_owned(),
                "2026/09/nope.jpg".into(),
            ))
            .expect_err("a name is not a folder ref");
        assert!(err.to_string().contains("not a folder ref"), "{err}");
        assert!(
            rt.block_on(host.file_states("documents".into())).is_err(),
            "the badge read refuses a name too"
        );
        assert!(
            rt.block_on(host.transfer_backlog("documents".into()))
                .is_err()
        );
        assert!(
            sync_file_states(
                tmp.path().to_string_lossy().into_owned(),
                "documents".into()
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "a refused call stages and creates nothing"
        );

        // The same calls with a real ref pass the boundary (and, with no DB,
        // answer empty — the pure-read contract pinned below).
        assert!(
            rt.block_on(host.file_states(FolderRef::Local(3).to_wire()))
                .expect("a ref passes")
                .is_empty()
        );
    }

    /// The FFI projection carries the engine's numbers through unchanged — the
    /// apple apps run their engines in process, so this call is their only
    /// route to the projection the pipe carries on windows/linux. A mapping
    /// that silently reported zeros would read as "nothing pending, all
    /// synced" on a device with a stuck backlog.
    #[test]
    fn a_pending_upload_reaches_the_ffi_record() {
        use fauna_sync_engine::db::SyncState;

        let tmp = tempfile::tempdir().unwrap();
        let documents = FolderRef::Local(3);
        let db = SyncDb::open(documents.state_db_path(tmp.path())).unwrap();
        db.upsert_entry(
            "a.bin",
            None,
            None,
            None,
            SyncState::Uploading,
            0,
            0,
            2_400,
            1,
            None,
        )
        .unwrap();
        // A synced file is not backlog — it must not inflate either counter.
        db.upsert_entry(
            "b.bin",
            None,
            None,
            None,
            SyncState::Synced,
            0,
            0,
            9_999,
            1,
            None,
        )
        .unwrap();

        let got = transfer_backlog(tmp.path(), documents).expect("read");
        assert_eq!(got.files_pending, 1);
        assert_eq!(
            got.bytes_pending, 2_400,
            "whole-file size of the uploading file only"
        );
    }

    /// Guards the `file_type == "regular"` predicate. The nest records regular
    /// files as `"regular"` (NOT `"file"`); a wrong literal here would silently
    /// map **zero** files and restore an empty folder with no error — the
    /// silent-feature-death class (testing.md convention 10). Directories +
    /// symlinks are correctly dropped.
    #[test]
    fn only_regular_files_map_to_restore_inputs() {
        let files = vec![
            entry("a.txt", "regular", vec![1u8; 32]),
            entry("sub", "directory", vec![]),
            entry("link", "symlink", vec![2u8; 32]),
            entry("nested/c.bin", "regular", vec![3u8; 32]),
        ];
        let out = snapshot_files_to_restore(&files).expect("map");
        assert_eq!(out.len(), 2, "only the two regular files survive");
        assert_eq!(out[0].relative_path, "a.txt");
        assert_eq!(out[1].relative_path, "nested/c.bin");
        assert_eq!(
            out[0].manifest_hash,
            ContentHash::from_digest_raw([1u8; 32]),
            "raw 32-byte manifest_hash resolves to the right ContentHash"
        );
        assert!(out.iter().all(|f| f.content_key_version.is_none()));
    }

    /// A corrupt listing (manifest hash not 32 bytes) is a hard error, never a
    /// silent partial restore.
    #[test]
    fn malformed_manifest_hash_is_a_hard_error() {
        let files = vec![entry("bad.txt", "regular", vec![1u8; 16])];
        assert!(snapshot_files_to_restore(&files).is_err());
    }

    /// Build a host context over `state_dir` with an unconnected fake nest — enough
    /// to construct a host and drive the calls that never touch the network.
    fn test_ctx(state_dir: &std::path::Path) -> HostContext {
        test_ctx_succeeding(state_dir, Vec::new())
    }

    fn test_ctx_succeeding(
        state_dir: &std::path::Path,
        predecessors: Vec<fauna_core::file_download::PredecessorSealKey>,
    ) -> HostContext {
        let nest = NestClient::with_auth(Arc::new(AuthClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        )));
        host_context(
            state_dir.to_string_lossy().into_owned(),
            [7u8; 32],
            [9u8; 32],
            "fauna-test".into(),
            nest,
            None,
            crate::account_runtime::folder_key_store(),
            predecessors,
        )
    }

    /// Every engine this host builds is handed the account's retired roots,
    /// each named by its identity (the reader binding takes its predecessor
    /// ids off the pairs — `engine_lifecycle::binding_predecessors`), and ONE
    /// statement-walk memory: a link an engine proved is there for the next
    /// build (`writer-signed-change-records.md` ruling (8)(b) source (ii),
    /// (8)(c)).
    #[tokio::test]
    async fn every_engine_build_carries_the_hosts_predecessor_chain_and_one_walk_memory() {
        let tmp = tempfile::tempdir().expect("tmp");
        let retired = fauna_core::identity::ActorKeypair::from_secret([8u8; 32]);
        let ctx = test_ctx_succeeding(
            tmp.path(),
            vec![fauna_core::file_download::PredecessorSealKey::named(
                retired.actor_id(),
                fauna_core::crypto::BackupKey::derive(retired.secret_bytes()),
            )],
        );
        let params =
            |ctx: &HostContext| ctx.params(tmp.path().to_path_buf(), FolderRef::Local(1), None);
        let first = params(&ctx);
        assert_eq!(
            first
                .predecessor_backup_keys
                .iter()
                .map(|k| k.actor_id)
                .collect::<Vec<_>>(),
            vec![Some(retired.actor_id())]
        );

        // The memory is the host's, not the build's: what one build's engine
        // learns, the next build's params already hold.
        assert!(
            params(&ctx)
                .learned_predecessors
                .shares_memory_with(&first.learned_predecessors)
        );

        // A host handed no chain binds nothing — its engines walk.
        assert!(
            params(&test_ctx(tmp.path()))
                .predecessor_backup_keys
                .is_empty()
        );
    }

    /// The badge read is a PURE read: a set this device hosts nowhere has no
    /// `fsid-<ref>.db`, and looking at it must report "no rows" without minting
    /// one — `SyncDb::open` creates on open, so without the exists-check every
    /// Media-page set the device never hosted left a stray empty DB behind
    /// (and, on macOS's two-root layout, a fake local presence in whichever
    /// root the fold looked in). Pins both the host-internal read and the
    /// free-function steward read, which are one function — and that the read
    /// looks at the identity-keyed file a writer (the agent, the FP host)
    /// actually writes, not a same-named neighbour's.
    #[test]
    fn file_states_of_a_never_hosted_set_is_empty_and_creates_no_db() {
        let tmp =
            std::env::temp_dir().join(format!("fauna-file-states-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("mk tmp");

        let never_hosted = FolderRef::Local(1);
        let states = file_states(&tmp, never_hosted).expect("a missing DB is not an error");
        assert!(states.is_empty(), "no DB → no rows");
        assert!(
            !never_hosted.state_db_path(&tmp).exists(),
            "the read must not create the set's state DB"
        );
        let exported = sync_file_states(tmp.to_string_lossy().into_owned(), never_hosted.to_wire())
            .expect("the exported read is the same read");
        assert!(exported.is_empty());
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            0,
            "nothing at all was created under the state dir"
        );

        // And once a host HAS written the set's DB, the same read — through
        // the same free function the app's steward read uses — returns its
        // rows from exactly that root. The writer keys by identity
        // (`state_db_path`), so the read finds it only by the same key.
        let hosted = FolderRef::Local(2);
        let db = SyncDb::open(hosted.state_db_path(&tmp)).expect("open");
        db.upsert_entry(
            "photos/a.jpg",
            None,
            None,
            None,
            fauna_sync_engine::db::SyncState::Synced,
            0,
            0,
            42,
            1,
            None,
        )
        .expect("upsert");
        drop(db);
        let rows = sync_file_states(tmp.to_string_lossy().into_owned(), hosted.to_wire())
            .expect("read the hosted set");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "photos/a.jpg");
        assert_eq!(rows[0].size_bytes, 42);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The resident half is retired: the host runs no engine it was not asked
    /// for — starting it writes nothing under the state dir (no binding file, no
    /// state DB), so no in-app engine can come back to life at construction and
    /// race the `fauna-sync-agent`. Headless, no nest, no human.
    #[test]
    fn starting_the_host_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let _host = FfiSyncEngineHost::start(test_ctx(tmp.path()));

        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "nothing at all was created under the state dir"
        );
    }
}
