//! The on-demand hydration host's **per-engine** layer: the byte/placeholder-
//! serving core the Windows cfapi callbacks bridge to ([`serve_fetch`] /
//! [`serve_populate`]), and the per-engine command loop ([`run_hydration_loop`]).
//! The N-engine multiplexing — one engine + cfapi sync root per bound folder —
//! lives in [`crate::engine_driver`] over the shared
//! [`EngineHost`](fauna_sync_engine::engine_host::EngineHost), and every engine
//! is built by the shared one builder
//! ([`fauna_sync_engine::engine_lifecycle::build_engine`]).
//!
//! ## Why a dedicated driving thread (the `!Sync` engine dictates the shape)
//!
//! [`SyncEngine`] is `Send` but **not `Sync`** (its `SyncDb` wraps a rusqlite
//! `Connection` whose statement cache is a `RefCell`), and its hydration future is
//! `!Send` (`FileHydrator` is `#[async_trait(?Send)]`). The cfapi `CF_CALLBACK` is
//! `extern "system"` and fires on **arbitrary OS threads**, so we can neither share
//! `&engine` across threads nor `block_on` an engine future from a foreign thread.
//!
//! Instead the shared `EngineHost` owns **one** OS thread with a current-thread
//! tokio runtime and polls every engine's [`run_hydration_loop`] future inline; each
//! engine has its own [`HydrationCommand`] channel. The FETCH_DATA callback extracts
//! only `Send` primitives (connection key, transfer key, path), wraps the cfapi
//! completion handle in a [`TransferSink`], looks up the engine by connection key
//! (`crate::cfapi_host`), and sends a [`HydrationCommand::Fetch`] — cfapi permits
//! async completion, so the bytes are delivered later by the driving thread via
//! `CfExecute(TRANSFER_DATA)`. The engines stay on one thread; their channel
//! `Sender`s (which ARE `Send + Sync`) live in the keyed callback context. See
//! `docs/goal/architecture/apps/windows.md` § On-demand hydration host and
//! `docs/goal/behavior/file-sync.md` § On-Demand Files.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc, watch};

use fauna_client::NestClient;
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_ipc::sync::{Event, EventKind, FileStatus, SyncProgressInfo};
use fauna_sync_engine::FileHydrator;
use fauna_sync_engine::always_resident::{
    EngineCommand, LocalWrite, LocalWriteHost, LocalWrites, apply_local_write,
};
use fauna_sync_engine::db::SyncState;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::engine::{PlaceholderFold, StaleHydratedRow};
use fauna_sync_engine::engine_host::CancellationToken;
use fauna_sync_engine::engine_lifecycle::{EngineCredential, EngineParams};
use fauna_sync_engine::enumerate::{
    DirChild, PlaceholderLister, PlaceholderRow, immediate_children,
};

use crate::pin_reaction::PinAction;

/// Bytes per `CfExecute(TRANSFER_DATA)` slice. cfapi accepts arbitrary offset/length,
/// so this only bounds per-call memory pressure for large files.
pub const TRANSFER_CHUNK_BYTES: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Transfer sink — the cfapi completion seam (faked in tests)
// ---------------------------------------------------------------------------

/// Where [`serve_fetch`] writes hydrated bytes. The production impl (TODO: cfapi
/// step) wraps `CfExecute(TRANSFER_DATA)` / failure with a captured connection key +
/// transfer key; tests record the calls. Sync methods (cfapi `CfExecute` is sync).
pub trait TransferSink {
    /// Hand `data` to the OS at absolute file `offset`.
    fn transfer_data(&self, offset: i64, data: &[u8]) -> Result<()>;
    /// Report the fetch failed so the OS does not hang the opening process.
    fn transfer_failed(&self) -> Result<()>;
}

/// Forwarding impl so a shared `Arc<Sink>` can be handed across the command channel
/// (the cfapi sink is constructed once per request; tests share an `Arc` to inspect).
impl<T: TransferSink + ?Sized> TransferSink for Arc<T> {
    fn transfer_data(&self, offset: i64, data: &[u8]) -> Result<()> {
        (**self).transfer_data(offset, data)
    }
    fn transfer_failed(&self) -> Result<()> {
        (**self).transfer_failed()
    }
}

/// A [`TransferSink`] decorator that emits an [`EventKind::SyncProgress`] after
/// each chunk (step 6). Wraps the real cfapi sink so the byte-serving core
/// ([`serve_fetch_chunked`]) needs no progress knowledge. `bytes_total` is the
/// requested transfer length; a best-effort `broadcast::send` is dropped if the
/// app isn't subscribed.
struct ProgressTransferSink {
    inner: Box<dyn TransferSink + Send>,
    event_tx: broadcast::Sender<Event>,
    folder: String,
    bytes_total: u64,
    bytes_done: AtomicU64,
}

impl TransferSink for ProgressTransferSink {
    fn transfer_data(&self, offset: i64, data: &[u8]) -> Result<()> {
        self.inner.transfer_data(offset, data)?;
        let done = self
            .bytes_done
            .fetch_add(data.len() as u64, Ordering::Relaxed)
            + data.len() as u64;
        let _ = self.event_tx.send(Event {
            event: EventKind::SyncProgress(SyncProgressInfo {
                folder: self.folder.clone(),
                files_done: u64::from(done >= self.bytes_total),
                files_total: 1,
                bytes_done: done,
                bytes_total: self.bytes_total,
            }),
        });
        Ok(())
    }
    fn transfer_failed(&self) -> Result<()> {
        self.inner.transfer_failed()
    }
}

// ---------------------------------------------------------------------------
// FETCH_DATA byte-serving logic (deterministically testable against fakes)
// ---------------------------------------------------------------------------

/// Serve one FETCH_DATA request at the default chunk size.
///
/// Returns the hash of the file's **full** content as served — what
/// [`HydrationHost::mark_hydrated`] must record so the freshly-hydrated row does not read as
/// locally modified (see that method: it is the hydration→upload feedback loop).
pub async fn serve_fetch(
    hydrator: &dyn FileHydrator,
    sink: &dyn TransferSink,
    relative_path: &str,
    offset: i64,
    length: i64,
    file_size: i64,
) -> Result<ContentHash> {
    serve_fetch_chunked(
        hydrator,
        sink,
        relative_path,
        offset,
        length,
        file_size,
        TRANSFER_CHUNK_BYTES,
    )
    .await
}

/// Hydrate `relative_path`, then write the requested byte range `[offset, offset+length)`
/// (clamped to the file; `length < 0` means "to EOF") to `sink` in `chunk_bytes` pieces.
/// On hydrate error this is the **sole** failure reporter — it calls
/// [`TransferSink::transfer_failed`] and returns the error for the caller to log.
///
/// `file_size` is the size the OS holds for the placeholder, which is all it will take. A
/// body of any other length is the wrong version for that placeholder (one left describing
/// a version a remote edit superseded — [`PlaceholderInvalidator::supersede`]), and serving
/// it would land a prefix of it, or leave the open waiting on bytes past its end; the
/// transfer FAILS instead, so the open errors and nothing truncated is ever recorded.
pub(crate) async fn serve_fetch_chunked(
    hydrator: &dyn FileHydrator,
    sink: &dyn TransferSink,
    relative_path: &str,
    offset: i64,
    length: i64,
    file_size: i64,
    chunk_bytes: usize,
) -> Result<ContentHash> {
    let fail = |e: anyhow::Error| {
        // Sole failure reporter: tell the OS so the opening process does not hang,
        // then surface the error for the driving thread to log.
        if let Err(report_err) = sink.transfer_failed() {
            tracing::warn!(error = %report_err, "reporting hydrate failure to cfapi failed");
        }
        Err(e)
    };
    let bytes = match hydrator.download_file_bytes(relative_path).await {
        Ok(bytes) => bytes,
        Err(e) => return fail(e),
    };

    let file_len = bytes.len() as i64;
    if file_len != file_size {
        return fail(anyhow::anyhow!(
            "the placeholder describes {file_size} bytes but its version has {file_len}: \
             refusing to serve the wrong version"
        ));
    }
    let start = offset.clamp(0, file_len);
    let end = if length < 0 {
        file_len
    } else {
        offset.saturating_add(length).clamp(start, file_len)
    };
    let chunk = chunk_bytes.max(1) as i64;

    let mut pos = start;
    while pos < end {
        let chunk_end = (pos + chunk).min(end);
        sink.transfer_data(pos, &bytes[pos as usize..chunk_end as usize])?;
        pos = chunk_end;
    }
    // A zero-length range (empty file, or a zero-length request) still needs one
    // completing transfer so `CfExecute(TRANSFER_DATA)` finalizes the OS operation.
    if start == end {
        sink.transfer_data(start, &[])?;
    }
    // The identity of the FULL file, regardless of which range this request asked for — a
    // partial fetch still hydrates the whole file into `bytes`, and it is the whole file the
    // OS will eventually hold.
    Ok(ContentHash::of_raw(&bytes))
}

// ---------------------------------------------------------------------------
// FETCH_PLACEHOLDERS directory-listing logic (deterministically testable)
// ---------------------------------------------------------------------------

/// Where [`serve_populate`] writes a directory's child placeholders. The
/// production impl (cfapi step) wraps `CfExecute(TRANSFER_PLACEHOLDERS)` with a
/// captured connection key + transfer key; tests record the calls. Sync (cfapi
/// `CfExecute` is sync).
pub trait PlaceholderSink {
    /// Materialize `children` as placeholders in the directory being populated,
    /// returning the folder-relative path of every child the platform provably
    /// put on the disk — the operation succeeded AND that entry's own result did.
    /// The engine's seen mark rests on exactly this set (`delete-propagation.md`
    /// § *An offline placeholder delete propagates*, decision (a)), so a sink must
    /// never report a child it cannot vouch for.
    fn transfer_placeholders(&self, children: &[DirChild]) -> Result<Vec<String>>;
}

/// Forwarding impl so a shared `Arc<Sink>` can be inspected by tests.
impl<T: PlaceholderSink + ?Sized> PlaceholderSink for Arc<T> {
    fn transfer_placeholders(&self, children: &[DirChild]) -> Result<Vec<String>> {
        (**self).transfer_placeholders(children)
    }
}

/// Serve one FETCH_PLACEHOLDERS request: list the tracked placeholder rows,
/// compute `parent_rel`'s immediate children, hand them to `sink`, and mark every
/// child the sink reports placed as **seen** — AFTER the transfer succeeded, never
/// before, so a crash loses evidence and never invents it (decision (a)). On a
/// listing error this still completes the operation with an empty set (so the
/// OS does not hang the browsing process) and returns the error to log.
pub(crate) async fn serve_populate<H: HydrationHost>(
    host: &H,
    sink: &dyn PlaceholderSink,
    parent_rel: &str,
) -> Result<()> {
    let rows = match host.list_placeholder_rows().await {
        Ok(rows) => rows,
        Err(e) => {
            if let Err(report_err) = sink.transfer_placeholders(&[]) {
                tracing::warn!(error = %report_err, "completing empty directory population failed");
            }
            return Err(e);
        }
    };
    let children = immediate_children(&rows, parent_rel);
    let placed = sink.transfer_placeholders(&children)?;
    host.mark_seen(&placed).await
}

// ---------------------------------------------------------------------------
// Driving thread: owns the (!Sync) engine, serves commands off a channel
// ---------------------------------------------------------------------------

/// A command for the hydration driving thread. Every field is `Send` (the cfapi
/// callback sends only primitives + the `Send` sink), so the channel crosses threads.
pub enum HydrationCommand {
    /// Serve a FETCH_DATA request: hydrate `rel` and write `[offset, offset+length)`.
    /// Constructed in production only by the `#[cfg(windows)]` cfapi callbacks
    /// ([`crate::cfapi_host`]); on other platforms it is exercised solely by the
    /// cross-platform unit tests, hence the non-windows `allow(dead_code)`.
    #[cfg_attr(not(windows), allow(dead_code))]
    Fetch {
        rel: String,
        offset: i64,
        length: i64,
        /// The size the OS holds for the placeholder ([`serve_fetch_chunked`]).
        file_size: i64,
        sink: Box<dyn TransferSink + Send>,
    },
    /// Serve a FETCH_PLACEHOLDERS request: list `parent_rel`'s immediate children.
    /// Windows-cfapi-driven in production (see [`Self::Fetch`]).
    #[cfg_attr(not(windows), allow(dead_code))]
    Populate {
        parent_rel: String,
        sink: Box<dyn PlaceholderSink + Send>,
    },
    /// Observe an **OS-initiated** dehydrate: Explorer's native "Free up space" or
    /// Storage Sense freed `rel`'s local bytes. Windows fires this on the
    /// `NOTIFY_DEHYDRATE_COMPLETION` callback (registered in
    /// [`crate::cfapi_host`]); the driving thread records the row back to
    /// `Placeholder` and pushes `FileStatusChanged{CloudOnly}`, the symmetric
    /// inverse of the [`Self::Fetch`] success arm. Distinct from the
    /// **provider-initiated** dehydrate (the Fauna shell menu's `FreeSpace` verb,
    /// `pipe_server::handle_free_space`): cfapi fires no callback for the
    /// provider's *own* I/O, so that path records the row itself. Windows-cfapi-
    /// driven in production; on other platforms exercised only by the loop tests.
    #[cfg_attr(not(windows), allow(dead_code))]
    Dehydrate { rel: String },
    /// Hydrate `rel` into the root's own directory — the whole file, landed by
    /// rename — record it `Synced`, push `FileStatusChanged{Synced}`, then
    /// answer `reply`. The linux FUSE root sends it for an `open` (or a `rename`,
    /// a `setattr`) of a placeholder, and completes the kernel's request once the
    /// answer arrives (`on-demand-files.md` § Linux FUSE binding, *Hydrate on
    /// open*). Several may run at once, and two for one `rel` share one download
    /// ([`serve_materialize_batch`]). Linux-FUSE-driven in production.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Materialize { rel: String, reply: LoopReply },
    /// The user unlinked a **placeholder** through the mount: record the delete
    /// through [`HydrationHost::delete_placeholder`] — the ONE way a placeholder is
    /// deleted on a FUSE root, since no watcher ever sees a file that was never
    /// on the disk — then answer `reply`. Linux-FUSE-driven in production.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unlink { rel: String, reply: LoopReply },
}

/// How the driving loop answers a request a FUSE session thread is waiting on:
/// `Ok` once the work is durable, `Err` (a reason for the log) when it failed.
/// A plain `std` channel so the waiting side can poll its mount's `closing`
/// flag between waits; the loop never blocks on it (`try_send` into a slot of 1).
pub(crate) type LoopReply = std::sync::mpsc::SyncSender<std::result::Result<(), String>>;

/// Answer a waiting FUSE request. A waiter that gave up (its mount is closing)
/// is not the loop's error.
fn answer(reply: &LoopReply, outcome: std::result::Result<(), String>) {
    let _ = reply.try_send(outcome);
}

/// The hydration driving thread's view of the engine: hydrate file bytes
/// ([`FileHydrator`]), list placeholder rows for directory population
/// ([`PlaceholderLister`]), **upload local edits** ([`LocalWriteHost`]), plus a
/// one-shot `prepare` run once at thread startup. Implemented by the bearer-only
/// [`SyncEngine`]; faked in tests.
///
/// **[`LocalWriteHost`] is a supertrait on purpose — an on-demand root is TWO-WAY.**
/// On-demand is a *storage* choice, never a *direction* choice: a change to a tracked
/// file **must** be uploaded, and no mode, folder setting or host may suppress it
/// (USER-ratified 2026-07-14, `file-sync.md` § On-Demand Files → *Sync direction*).
/// Requiring the write half here means a download-only hydration host **does not
/// compile** — the rule is enforced by the type system, not by a comment. Until
/// 2026-07-14 this root really was download-only, and a user's edit to a tracked
/// placeholder was silently orphaned (live-measured): the loop had no watcher, so the
/// edit was never even *observed*. That was a missing capability wearing a policy's
/// clothes, and this bound is what stops it growing back.
#[async_trait::async_trait(?Send)]
pub trait HydrationHost: FileHydrator + PlaceholderLister + LocalWriteHost {
    /// Open the WS-RPC control plane and populate the SyncDb with placeholders
    /// from the nest (`changes.list`), returning the [`PlaceholderFold`]: rows
    /// written **plus** the hydrated rows the fold reported rather than rewrote
    /// (a nest head moved under a `Synced` local copy — the caller invalidates
    /// them via [`apply_stale_hydrated`]).
    /// Best-effort by contract: callers log and continue, so a from-scratch host
    /// whose nest is momentarily unreachable (nothing to serve yet — a later
    /// re-pull fills it) and a switch-mode host (entries already on disk) both
    /// still come up.
    async fn prepare(&self) -> Result<PlaceholderFold>;

    /// Record that the file at `rel` is now hydrated (its bytes were served to
    /// the OS): transition its state to `Synced` so the synchronous
    /// `GetFileStatus` query and `list_placeholder_rows` agree with the pushed
    /// `FileStatusChanged` event. See [`SyncEngine::mark_hydrated`].
    /// Record a served file as hydrated, carrying the **full content's hash** so the row's
    /// local identity is honest. A state-only mark is what makes a two-way root re-upload every
    /// file it just downloaded — see [`SyncEngine::mark_hydrated`].
    async fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()>;

    /// Stream `rel`'s current bytes to `dest` and return the whole file's content
    /// hash — what [`mark_hydrated`](Self::mark_hydrated) then records. The FUSE
    /// root's hydration primitive ([`HydrationCommand::Materialize`]): a FUSE
    /// root has no OS transfer window to feed, so it lands whole files.
    ///
    /// The default goes through [`FileHydrator::download_file_bytes`] and holds
    /// the file in memory — right for the test hosts, whose bytes are canned.
    /// [`SyncEngine`] overrides it with its bounded-memory
    /// [`SyncEngine::download_file_to_path`].
    async fn download_to_path(&self, rel: &str, dest: &Path) -> Result<ContentHash> {
        let bytes = self.download_file_bytes(rel).await?;
        std::fs::write(dest, &bytes)?;
        Ok(ContentHash::of_raw(&bytes))
    }

    /// Record that the OS dehydrated `rel` (Explorer "Free up space" / Storage
    /// Sense freed its local bytes): flip the row `Synced` → `Placeholder` so the
    /// badge/overlay is honest, keeping the manifest anchor. The symmetric inverse
    /// of [`mark_hydrated`](Self::mark_hydrated). See
    /// [`SyncEngine::mark_placeholder`].
    async fn mark_placeholder(&self, rel: &str) -> Result<()>;

    /// Mark `rels` **seen** — [`serve_populate`] just put their placeholders on the
    /// disk (`delete-propagation.md` § *An offline placeholder delete propagates*,
    /// decision (a)). A seen `Placeholder` row's later absence from a scan is a
    /// delete; a never-seen one's is evidence of nothing. See
    /// [`fauna_sync_engine::db::SyncDb::mark_seen`].
    async fn mark_seen(&self, rels: &[String]) -> Result<()>;

    /// Clear every seen mark — decision (e): the root's registration did not
    /// survive the downtime, so the OS removed its placeholders at the unregister,
    /// and that removal is never a user's delete. Run by [`serve_hydration_root`]
    /// BEFORE its boot sweep. See [`fauna_sync_engine::db::SyncDb::clear_seen_all`].
    async fn clear_seen_all(&self) -> Result<()>;

    /// Re-point a stale hydrated row at the nest's head and mark it
    /// `Placeholder` — the durable half of invalidating a superseded local copy.
    /// **Only call after the file's bytes have actually been freed** (see
    /// [`apply_stale_hydrated`]). See [`SyncEngine::repoint_hydrated_to_placeholder`].
    async fn repoint_placeholder(&self, row: &StaleHydratedRow) -> Result<()>;

    /// Re-fold the nest's `changes.list` into this root's SyncDb, returning the
    /// same [`PlaceholderFold`] [`prepare`](Self::prepare) does — new/moved
    /// placeholder rows written, stale hydrated rows *reported*.
    ///
    /// The steady-state twin of `prepare`, minus the connect: the control plane is
    /// already open (and [`NestClient`] reconnects itself indefinitely), so this is
    /// just the fold. Idempotent by contract — an unchanged head writes nothing —
    /// which is what lets [`run_hydration_loop`] call it on a bare timer.
    async fn repull(&self) -> Result<PlaceholderFold>;

    /// How often to [`repull`](Self::repull) — the constant reconcile cadence
    /// (`DEFAULT_RESCAN_INTERVAL`; phase 5's de-knob, `file-sync.md` § Config:
    /// the backstop is a hard-coded constant, never a row or carrier read).
    /// Kept as a trait method purely as a drive-loop seam: test hosts stretch
    /// it so the tick never fires inside a test.
    async fn rescan_interval(&self) -> Duration;

    // NB the trait used to carry `sync_mode()` here — the once-per-process
    // resolution `engine_driver` installed above the loop. Retired 2026-08-02: once-per-process meant a role the user changed never
    // reached a running engine, and its `Err → Sync` degrade silently disarmed
    // a backup seat for the process lifetime. The engine itself now owns the
    // resolution (`SyncEngine::refresh_sync_mode` — role-first, cache-backed,
    // re-run at loop entry + every rescan tick; `file-sync.md` § 4).

    /// Fires whenever the control plane **re**-connects (never on the first
    /// connect). A re-pull on this edge closes the window a dropped socket opens:
    /// changes recorded while this host was offline would otherwise wait out the
    /// full [`rescan_interval`](Self::rescan_interval) tick.
    fn reconnects(&self) -> watch::Receiver<u64>;

    /// Is freeing `rel`'s local bytes provably lossless **by this engine's own
    /// record** — row `Synced` and disk content hashing to the recorded uploaded
    /// identity? The gate for overriding a stale platform dirty-file refusal with
    /// [`PlaceholderInvalidator::set_in_sync`]; fail-closed. See
    /// [`SyncEngine::is_dehydration_safe`] for the full safety argument.
    async fn is_dehydration_safe(&self, rel: &str) -> bool;

    /// Is every known row under `dir_rel/` clean, with at least one live one?
    /// The folder-✅ predicate ([`SyncEngine::subtree_fully_synced`]): gates
    /// [`flip_clean_ancestor_dirs`], never a byte-freeing operation.
    async fn subtree_fully_synced(&self, dir_rel: &str) -> bool;

    /// The once-per-start **corpus passes** — the pre-bind re-seal, the
    /// audience convergence and the post-succession re-seal
    /// ([`fauna_sync_engine::always_resident::converge_corpus_at_start`]) — run by
    /// [`run_hydration_loop`] after its startup `converge`, exactly where the
    /// always-resident root runs them. **Required, never defaulted:** on-demand
    /// is a storage choice, not a direction choice, and until 2026-09-27 the
    /// hydration root simply lacked these passes, so an audience flip never
    /// reached an on-demand folder's back-catalogue (a public window's plaintext
    /// stayed sealed at rest; found by the flip-back e2e on tui-on-windows the
    /// day on-demand became the windows default). A host must say what it does
    /// here; a test host answers with a no-op it owns.
    async fn converge_corpus_at_start(&self, folder: &str);

    /// The per-tick posture refresh + corpus pair — `refresh_sync_mode` (the
    /// one read that installs the seat's mode, audience and website toggle on
    /// the running engine), then audience convergence + website convergence
    /// ([`fauna_sync_engine::always_resident::refresh_and_converge_corpus`]) —
    /// run by [`run_hydration_loop`] at startup and on every rescan tick, the
    /// live path an owner's audience flip or website toggle takes to a RUNNING
    /// on-demand root, as the always-resident loop already runs it at entry and
    /// per tick. The refresh is part of it, not a caller's duty: the pair reads
    /// the posture the refresh installs, and a root without the refresh keeps
    /// its build-time posture for ever.
    async fn refresh_and_converge_corpus(&self, folder: &str);

    /// Answer one invoke-and-reply [`EngineCommand`] the pipe server routed to
    /// this root — the confirm of the mass-delete floor's *"apply N deletions"*
    /// hold, and a page of peer-served share rows — through the SAME shared
    /// answer the always-resident loop runs
    /// ([`fauna_sync_engine::always_resident::answer_engine_command`]), called
    /// by [`run_hydration_loop`]'s command arm. **Required, never defaulted:**
    /// until 2026-09-29 this root had no command arm, so a hold it surfaced
    /// could not be confirmed (`delete-propagation.md` § *The floor on an
    /// on-demand root*, point 4). A host must answer every command it is sent.
    async fn answer_engine_command(&self, cmd: EngineCommand);

    /// Answer one serve ask the host's seat routed to this root — the nest's
    /// relay or a sibling device's (`file-sync.md` § Relay serving): serve the
    /// key from a hydrated body, or decline. Called by [`serve_hydration_root`]
    /// beside the loop. **Defaults to a no-op** (a dropped ask reads as a
    /// decline): every implementor but [`SyncEngine`] is a test double that
    /// announces nothing, so it is never asked.
    async fn answer_serve_ask(&self, _ask: fauna_sync_engine::relay_seat::ServeAsk) {}

    // ── The off-disk placeholder posture (the linux FUSE root) ──────────────────
    //
    // A root whose OS binding keeps placeholders as rows only, never as files on its
    // disk ([`PlaceholderInvalidator::placeholders_off_disk`]; `on-demand-files.md`
    // § Linux FUSE binding, the dehydrate rule). The defaults are for hosts that
    // serve no such root: each refuses or answers "nothing", so a host that has not
    // said how it does this can never be served as one.

    /// Put the host's engine under the off-disk posture before the boot sweep
    /// ([`SyncEngine::set_placeholders_off_disk`]). The default REFUSES, and
    /// [`serve_hydration_root`] then does not serve the root: a sweep over an
    /// engine that still reads placeholders as files would record every browsed
    /// or freed file as a delete.
    async fn set_placeholders_off_disk(&self) -> Result<()> {
        Err(anyhow::anyhow!(
            "this host cannot serve a root whose placeholders are off its disk"
        ))
    }

    /// The off-disk dehydrate's row half ([`SyncEngine::dehydrate_off_disk`]):
    /// gate, flip the row unseen, arm the removal suppression — `false` when the
    /// gate refuses. Run by [`free_local_bytes`] BEFORE the invalidator unlinks.
    async fn dehydrate_off_disk(&self, _rel: &str) -> Result<bool> {
        Err(anyhow::anyhow!("this host frees no bytes off its disk"))
    }

    /// Follow a moved head over a body the holder-keeps gate will not free
    /// ([`SyncEngine::replace_superseded_own_record`]): fetch the new head, write
    /// it over the old body, then move the row. `false` when the row is not that
    /// case; an error when no holder answered, with nothing changed. Run by
    /// [`apply_stale_hydrated`] when the free is refused.
    async fn replace_superseded_own_record(&self, _row: &StaleHydratedRow) -> Result<bool> {
        Ok(false)
    }

    /// Record the user's delete of a **placeholder** — the mount's `unlink`
    /// ([`HydrationCommand::Unlink`]). The inherent [`SyncEngine::handle_delete`],
    /// never the watcher rail's [`LocalWriteHost::handle_delete`], which drops a
    /// `Placeholder` row's remove under the off-disk posture (that remove is the
    /// provider's own unlink). The default is that rail, for test hosts that keep
    /// no posture.
    async fn delete_placeholder(&self, rel: &str) -> Result<()> {
        self.handle_delete(rel).await
    }

    /// `rel`'s pin and presence, from its row — `None` when it has no live row.
    /// Where the platform keeps no pin of its own, the row is the pin.
    async fn row_pin(&self, _rel: &str) -> Result<Option<RowPin>> {
        Ok(None)
    }

    /// Set or clear `rel`'s pin in its row; `false` when it has no row.
    async fn set_pinned(&self, _rel: &str, _pinned: bool) -> Result<bool> {
        Err(anyhow::anyhow!("this host keeps no pins in its rows"))
    }

    /// Every pinned file whose bytes are not on this device — each owed an eager
    /// hydration ([`react_to_pin_sweep`] on an off-disk root).
    async fn pinned_placeholders(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

/// A file's pin and presence as its row records them ([`HydrationHost::row_pin`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowPin {
    /// The user asked to keep it on this device.
    pub pinned: bool,
    /// Its bytes are not on this device (`Placeholder`).
    pub cloud_only: bool,
}

#[async_trait::async_trait(?Send)]
impl HydrationHost for SyncEngine {
    async fn answer_engine_command(&self, cmd: EngineCommand) {
        fauna_sync_engine::always_resident::answer_engine_command(self, cmd).await
    }

    async fn answer_serve_ask(&self, ask: fauna_sync_engine::relay_seat::ServeAsk) {
        SyncEngine::answer_serve_ask(self, ask).await
    }

    async fn set_placeholders_off_disk(&self) -> Result<()> {
        SyncEngine::set_placeholders_off_disk(self);
        Ok(())
    }

    async fn dehydrate_off_disk(&self, rel: &str) -> Result<bool> {
        SyncEngine::dehydrate_off_disk(self, rel)
    }

    async fn replace_superseded_own_record(&self, row: &StaleHydratedRow) -> Result<bool> {
        SyncEngine::replace_superseded_own_record(self, row).await
    }

    async fn delete_placeholder(&self, rel: &str) -> Result<()> {
        SyncEngine::handle_delete(self, rel).await.map(|_| ())
    }

    async fn row_pin(&self, rel: &str) -> Result<Option<RowPin>> {
        Ok(self.db().get_entry(rel)?.and_then(|e| {
            (e.state != SyncState::Deleted && e.state != SyncState::LocallyDeleted).then_some(
                RowPin {
                    pinned: e.pinned,
                    cloud_only: e.state == SyncState::Placeholder,
                },
            )
        }))
    }

    async fn set_pinned(&self, rel: &str, pinned: bool) -> Result<bool> {
        self.db().set_pinned(rel, pinned)
    }

    async fn pinned_placeholders(&self) -> Result<Vec<String>> {
        self.db().pinned_placeholder_paths()
    }

    async fn subtree_fully_synced(&self, dir_rel: &str) -> bool {
        SyncEngine::subtree_fully_synced(self, dir_rel).unwrap_or(false)
    }

    async fn prepare(&self) -> Result<PlaceholderFold> {
        // `changes.list` rides the control plane. The driver connected it before
        // the build (which reads over it), so this is a no-op there — it only
        // opens a plane nobody opened yet. If the connect fails, population fails
        // too (and is skipped via `?`) — both are logged as best-effort by the
        // caller.
        self.connect_control_plane().await?;
        // The on-demand root's audience report: this path never runs the
        // resident tick's `refresh_sync_mode`, so the build's verdict is
        // reported here (and on every repull below) for `ShareFile`.
        self.report_public_audience();
        self.populate_placeholders_from_nest().await
    }

    async fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()> {
        SyncEngine::mark_hydrated(self, rel, content_hash)
    }

    async fn download_to_path(&self, rel: &str, dest: &Path) -> Result<ContentHash> {
        SyncEngine::download_file_to_path(self, rel, dest).await
    }

    async fn mark_placeholder(&self, rel: &str) -> Result<()> {
        SyncEngine::mark_placeholder(self, rel)
    }

    async fn mark_seen(&self, rels: &[String]) -> Result<()> {
        self.db().mark_seen(rels)
    }

    async fn clear_seen_all(&self) -> Result<()> {
        self.db().clear_seen_all()
    }

    async fn repoint_placeholder(&self, row: &StaleHydratedRow) -> Result<()> {
        SyncEngine::repoint_hydrated_to_placeholder(self, row)
    }

    async fn repull(&self) -> Result<PlaceholderFold> {
        self.report_public_audience();
        self.populate_placeholders_from_nest().await
    }

    async fn rescan_interval(&self) -> Duration {
        // Phase 5's de-knob: the constant, for every root — owner, writer
        // member, and cross-nest foreign-routed alike. The row read, the
        // member-projection read, and the foreign engine-key-blob read that
        // used to live here are all retired (`file-sync.md` § Config; the
        // carriers stay stamped for released seats, but new code never reads
        // them). Read through the engine's test seam (`FAUNA_E2E_RESCAN_MS`,
        // compile-gated) so a tier_3 run can size the tick, as every other
        // resident host does.
        fauna_sync_engine::always_resident::rescan_interval()
    }

    // `sync_mode()` impl retired 2026-08-02 with the trait method (see the
    // trait's note): `SyncEngine::refresh_sync_mode` owns the resolution now —
    // same reads, role-first, plus the cache fallback and the unresolved
    // (decline + hold) posture this impl's `Err → Sync` degrade lacked.

    fn reconnects(&self) -> watch::Receiver<u64> {
        self.control_plane().subscribe_reconnects()
    }

    async fn is_dehydration_safe(&self, rel: &str) -> bool {
        SyncEngine::is_dehydration_safe(self, rel)
    }

    async fn converge_corpus_at_start(&self, folder: &str) {
        fauna_sync_engine::always_resident::converge_corpus_at_start(self, folder).await
    }

    async fn refresh_and_converge_corpus(&self, folder: &str) {
        fauna_sync_engine::always_resident::refresh_and_converge_corpus(self, folder).await
    }
}

/// The platform's "free a hydrated file's bytes, back to a placeholder" operation,
/// behind a trait so [`apply_stale_hydrated`]'s ordering is testable without a live
/// cfapi sync root. Production is [`CfapiInvalidator`]; tests fake it, including the
/// dirty-file refusal that is the whole safety story.
pub(crate) trait PlaceholderInvalidator {
    /// Free `abs_path`'s on-disk bytes, converting the file back to a placeholder.
    ///
    /// **Contract: MUST fail if the file carries unsynced local edits.** The
    /// production cfapi impl gets this for free — `CfDehydratePlaceholder` over a
    /// `CF_INSYNC_POLICY_TRACK_ALL` sync root refuses a not-in-sync (locally
    /// edited) file — and that refusal is exactly what stops [`apply_stale_hydrated`]
    /// from destroying a user's edit.
    fn dehydrate(&self, abs_path: &Path) -> Result<()>;

    /// [`dehydrate`](Self::dehydrate) for a file another device's edit SUPERSEDED: free
    /// `abs_path`'s bytes AND leave its placeholder describing the new version — `size`
    /// bytes, last written at `mtime` (Unix seconds). Reached by [`apply_stale_hydrated`] (a
    /// hydrated copy) and [`apply_fold`] (a re-pointed cloud-only one).
    ///
    /// **Same contract: MUST fail if the file carries unsynced local edits.** And a binding
    /// whose placeholders keep their size and mtime on the disk MUST move them here: cfapi asks
    /// for exactly `[0, the placeholder's size)` on the next open, so a placeholder left at
    /// the old size served a prefix of a grown file (then recorded and uploaded over the real
    /// edit) — `on-demand-files.md` § On-Demand Files: a placeholder shows the right size
    /// and modification time. A binding that reads both from the rows unlinks, as
    /// [`dehydrate`](Self::dehydrate) does.
    fn supersede(&self, abs_path: &Path, size: u64, mtime: i64) -> Result<()>;

    /// Classify what, if anything, the provider owes `abs_path` given its pin
    /// state and byte-presence — stat-only ([`crate::pin_reaction::pin_action_for_path`]).
    fn pin_action(&self, abs_path: &Path) -> Option<PinAction>;

    /// Assert to the platform that `abs_path`'s local content is in sync with the
    /// provider's stored copy — releasing a **stale** dirty-file dehydrate refusal
    /// (the edit that tripped it has been uploaded since).
    ///
    /// **Conditioned on `usn`** ([`usn`](Self::usn)): the platform refuses (an `Err`)
    /// if the file was written since. This overrides the platform's own
    /// edit-protection razor ([`dehydrate`](Self::dehydrate)'s refusal), so a file
    /// only ever reaches it through [`assert_recorded_in_sync`], which reads the USN
    /// BEFORE proving the content — prove-then-assert as one atomic step.
    fn set_in_sync(&self, abs_path: &Path, usn: i64) -> Result<()>;

    /// The file's current update sequence number — what [`set_in_sync`](Self::set_in_sync)
    /// is conditioned on. Attributes-only; never a data open.
    fn usn(&self, abs_path: &Path) -> Result<i64>;

    /// Ask the platform to materialize `abs_path`'s bytes, **without blocking the
    /// caller**: the data arrives through the provider's ordinary fetch path
    /// (cfapi: `CfHydratePlaceholder` fires our own FETCH_DATA — measured,
    /// `diag_pin_reaction_mechanics` — so the loop thread must be free to serve
    /// it; the production impl parks the blocking call on its own thread).
    /// Completion is owned by the fetch path (`mark_hydrated` + the `Synced`
    /// event), not observed here — which also makes duplicate kicks harmless.
    fn kick_hydrate(&self, abs_path: &Path);

    /// Is `abs_path` still a **cloud file** (carries the platform's placeholder
    /// anchor)? An editor that saves by replace or truncate-rewrite destroys
    /// placeholder-ness (measured: the saved file has no reparse point and every
    /// placeholder op on it fails 0x80070178), so such a file must be
    /// [`anchor`](Self::anchor)ed before any in-sync assertion can reach it.
    /// Stat-only.
    fn is_cloud_file(&self, abs_path: &Path) -> bool;

    /// Re-anchor an ordinary (locally-created / editor-replaced) file as a
    /// **hydrated, not-in-sync** placeholder with `rel` as its identity — the bytes
    /// stay local and nothing is vouched for (the state an in-place edit leaves), so
    /// it is safe before the content is proven. The in-sync assertion that follows
    /// is the separate, USN-conditioned [`set_in_sync`](Self::set_in_sync): the
    /// platform's convert takes no USN condition, so a convert that also marked a
    /// file in-sync would vouch for whatever bytes were on disk at that instant.
    fn anchor(&self, abs_path: &Path, rel: &str) -> Result<()>;

    /// Re-anchor an ordinary **directory** as an in-sync placeholder with `rel` as
    /// its identity — the folder-✅ leg. Directories only: a directory has no bytes
    /// to free or to overwrite, so its assertion needs no content proof beyond
    /// [`HydrationHost::subtree_fully_synced`]. A FILE is never marked in-sync by a
    /// convert — see [`anchor`](Self::anchor).
    fn convert_dir_in_sync(&self, abs_path: &Path, rel: &str) -> Result<()>;

    /// Has the platform finished listing the directory `abs_dir`, so it will never
    /// ask this provider for its children again? Only such a directory may have a
    /// child pushed into it ([`materialize_created`]); one still to be listed gets
    /// every row from its own listing, and a pushed twin would race it. Fail-closed:
    /// unsure → `false` (lazy).
    fn is_listed_dir(&self, abs_dir: &Path) -> bool;

    /// Push one cloud-only placeholder named by `rel`'s final segment into the
    /// directory `parent_abs`, `rel` its identity — a file (`size`, `mtime`) or, with
    /// `is_dir`, a directory the platform lists lazily on its own first browse.
    fn create_placeholder(
        &self,
        parent_abs: &Path,
        rel: &str,
        size: u64,
        mtime: i64,
        is_dir: bool,
    ) -> Result<()>;

    /// Are this root's placeholders **rows only, never files on its disk**? True
    /// for the linux FUSE binding (`on-demand-files.md` § Linux FUSE binding, the
    /// dehydrate rule); cfapi's placeholders are files. It changes three things:
    /// the engine takes the off-disk posture before the boot sweep
    /// ([`HydrationHost::set_placeholders_off_disk`]); [`dehydrate`](Self::dehydrate)
    /// is only the unlink, gated and preceded by the row flip in
    /// [`free_local_bytes`]; and pins are read from the rows, not the disk
    /// ([`react_to_pin_sweep`]).
    fn placeholders_off_disk(&self) -> bool {
        false
    }
}

/// A reference delegates — so a test can lend its invalidator to the loop and
/// still inspect what was recorded after the loop returns.
impl<T: PlaceholderInvalidator + ?Sized> PlaceholderInvalidator for &T {
    fn dehydrate(&self, abs_path: &Path) -> Result<()> {
        (**self).dehydrate(abs_path)
    }
    fn supersede(&self, abs_path: &Path, size: u64, mtime: i64) -> Result<()> {
        (**self).supersede(abs_path, size, mtime)
    }
    fn pin_action(&self, abs_path: &Path) -> Option<PinAction> {
        (**self).pin_action(abs_path)
    }
    fn set_in_sync(&self, abs_path: &Path, usn: i64) -> Result<()> {
        (**self).set_in_sync(abs_path, usn)
    }
    fn usn(&self, abs_path: &Path) -> Result<i64> {
        (**self).usn(abs_path)
    }
    fn kick_hydrate(&self, abs_path: &Path) {
        (**self).kick_hydrate(abs_path)
    }
    fn is_cloud_file(&self, abs_path: &Path) -> bool {
        (**self).is_cloud_file(abs_path)
    }
    fn anchor(&self, abs_path: &Path, rel: &str) -> Result<()> {
        (**self).anchor(abs_path, rel)
    }
    fn convert_dir_in_sync(&self, abs_path: &Path, rel: &str) -> Result<()> {
        (**self).convert_dir_in_sync(abs_path, rel)
    }
    fn is_listed_dir(&self, abs_dir: &Path) -> bool {
        (**self).is_listed_dir(abs_dir)
    }
    fn create_placeholder(
        &self,
        parent_abs: &Path,
        rel: &str,
        size: u64,
        mtime: i64,
        is_dir: bool,
    ) -> Result<()> {
        (**self).create_placeholder(parent_abs, rel, size, mtime, is_dir)
    }
    fn placeholders_off_disk(&self) -> bool {
        (**self).placeholders_off_disk()
    }
}

#[cfg(windows)]
pub(crate) struct CfapiInvalidator;

#[cfg(windows)]
impl PlaceholderInvalidator for CfapiInvalidator {
    fn dehydrate(&self, abs_path: &Path) -> Result<()> {
        fauna_cfapi::dehydrate_placeholder(abs_path)
    }

    fn supersede(&self, abs_path: &Path, size: u64, mtime: i64) -> Result<()> {
        fauna_cfapi::supersede_placeholder(abs_path, size, mtime)
    }

    fn pin_action(&self, abs_path: &Path) -> Option<PinAction> {
        crate::pin_reaction::pin_action_for_path(abs_path)
    }

    fn set_in_sync(&self, abs_path: &Path, usn: i64) -> Result<()> {
        fauna_cfapi::set_in_sync(abs_path, usn)
    }

    fn usn(&self, abs_path: &Path) -> Result<i64> {
        fauna_cfapi::file_usn(abs_path)
    }

    fn kick_hydrate(&self, abs_path: &Path) {
        // CfHydratePlaceholder blocks until the file is materialized, and the
        // data comes through this process's own FETCH_DATA served by the very
        // loop that called here — so park the call on its own thread. Duplicate
        // kicks for a file already hydrating just block alongside and return;
        // the sweep's cadence (the rescan tick) bounds how many can pile up.
        let path = abs_path.to_path_buf();
        std::thread::spawn(move || match fauna_cfapi::hydrate_placeholder(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "pinned placeholder hydrated"),
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %e,
                "hydrating a pinned placeholder failed (retried on the next sweep)"
            ),
        });
    }

    fn is_cloud_file(&self, abs_path: &Path) -> bool {
        use std::os::windows::fs::MetadataExt;
        /// `FILE_ATTRIBUTE_REPARSE_POINT` — a cloud placeholder carries one
        /// (hydrated `0x420` or cloud-only `0x401620`); an editor-replaced
        /// ordinary file (`0x20`) does not (`placeholder.rs` measured table).
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        std::fs::metadata(abs_path)
            .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .unwrap_or(false)
    }

    fn anchor(&self, abs_path: &Path, rel: &str) -> Result<()> {
        fauna_cfapi::convert_to_placeholder_anchored(abs_path, rel)
    }

    fn convert_dir_in_sync(&self, abs_path: &Path, rel: &str) -> Result<()> {
        fauna_cfapi::convert_to_placeholder_in_sync(abs_path, rel)
    }

    fn is_listed_dir(&self, abs_dir: &Path) -> bool {
        fauna_cfapi::is_listed_directory(abs_dir).unwrap_or(false)
    }

    fn create_placeholder(
        &self,
        parent_abs: &Path,
        rel: &str,
        size: u64,
        mtime: i64,
        is_dir: bool,
    ) -> Result<()> {
        fauna_cfapi::create_placeholder(
            parent_abs,
            &fauna_cfapi::PlaceholderInfo {
                rel_path: rel.to_string(),
                size,
                mtime,
                is_dir,
            },
        )
    }
}

/// Off-Windows there is no cfapi sync root, so nothing is ever hydrated and this is
/// never called in production; it exists only so [`run_hydration_loop`] compiles
/// cross-platform (its tests inject their own invalidator). On linux the FUSE root
/// has its own (`fuse_host::FuseInvalidator`), so only macOS and the tests build it.
#[cfg(not(windows))]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) struct CfapiInvalidator;

#[cfg(not(windows))]
impl PlaceholderInvalidator for CfapiInvalidator {
    fn dehydrate(&self, _abs_path: &Path) -> Result<()> {
        Err(anyhow::anyhow!(
            "cfapi dehydrate unsupported on non-Windows targets"
        ))
    }

    fn supersede(&self, _abs_path: &Path, _size: u64, _mtime: i64) -> Result<()> {
        Err(anyhow::anyhow!(
            "cfapi placeholder update unsupported on non-Windows targets"
        ))
    }

    fn pin_action(&self, _abs_path: &Path) -> Option<PinAction> {
        None
    }

    fn set_in_sync(&self, _abs_path: &Path, _usn: i64) -> Result<()> {
        Err(anyhow::anyhow!(
            "cfapi set-in-sync unsupported on non-Windows targets"
        ))
    }

    fn usn(&self, _abs_path: &Path) -> Result<i64> {
        Err(anyhow::anyhow!(
            "USN read unsupported on non-Windows targets"
        ))
    }

    fn kick_hydrate(&self, _abs_path: &Path) {}

    fn is_cloud_file(&self, _abs_path: &Path) -> bool {
        false
    }

    fn anchor(&self, _abs_path: &Path, _rel: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "cfapi convert unsupported on non-Windows targets"
        ))
    }

    fn convert_dir_in_sync(&self, _abs_path: &Path, _rel: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "cfapi convert unsupported on non-Windows targets"
        ))
    }

    fn is_listed_dir(&self, _abs_dir: &Path) -> bool {
        false
    }

    fn create_placeholder(
        &self,
        _parent_abs: &Path,
        _rel: &str,
        _size: u64,
        _mtime: i64,
        _is_dir: bool,
    ) -> Result<()> {
        Err(anyhow::anyhow!(
            "cfapi placeholder create unsupported on non-Windows targets"
        ))
    }
}

/// Vouch to the platform that `rel`'s on-disk content is the provider's stored copy —
/// **only if it provably is, and atomically**. The one road to the
/// in-sync assertion for a FILE, shared by the post-record flip and the pin reaction's
/// repair:
///
/// 1. an ordinary file (locally created, or an editor's replace-save destroyed the
///    placeholder) is first [`anchor`](PlaceholderInvalidator::anchor)ed as a hydrated,
///    NOT-in-sync placeholder — vouching for nothing, so it is safe unproven;
/// 2. its USN is read;
/// 3. the engine proves the disk content is the recorded content
///    ([`HydrationHost::is_dehydration_safe`]: row `Synced`, disk hash ==
///    `recorded_content_hash`);
/// 4. the in-sync assertion is conditioned on the step-2 USN, so a write landing after
///    it — during or after the proof — makes the platform refuse.
///
/// Whatever bytes step 3 hashed are the bytes step 4 vouches for, or nothing is
/// vouched for. `Ok(false)`: the proof failed (the disk moved past the recorded
/// content) — the not-in-sync bit stands, and the newer content's own upload flips it.
async fn assert_recorded_in_sync<H: HydrationHost, I: PlaceholderInvalidator + ?Sized>(
    hydrator: &H,
    invalidator: &I,
    abs: &Path,
    rel: &str,
) -> Result<bool> {
    if !invalidator.is_cloud_file(abs) {
        invalidator.anchor(abs, rel)?;
    }
    let usn = invalidator.usn(abs)?;
    if !hydrator.is_dehydration_safe(rel).await {
        return Ok(false);
    }
    invalidator.set_in_sync(abs, usn)?;
    Ok(true)
}

/// Flip a batch of just-RECORDED local edits to the platform's ✅ — the last leg
/// of a two-way upload on an on-demand root, and what stops Explorer's Status
/// column showing the sync-pending arrows forever on a file that synced fine
/// (live 2026-07-17). Each rel goes through [`assert_recorded_in_sync`]: the
/// bytes stay local, and the assertion lands only for disk content that IS the
/// recorded content. **Recorded is necessary, not sufficient:** the upload read
/// the file before it recorded, so a save landing in between (an autosaving
/// editor, a large file) is on disk now, and an unconditional flip would clear
/// the not-in-sync refusal protecting it — the next dehydrate (an unpin in the
/// same batch, *Free up space*, a remote change) would then free the only copy
/// of the newer edit.
///
/// Callers pass [`LocalWriteApplied::recorded`] rels. Best-effort per rel: a
/// skipped or failed flip leaves the honest pending state, and the next
/// recorded upload of the same file retries it.
pub(crate) async fn flip_recorded_in_sync<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: &H,
    invalidator: &I,
    sync_root: &Path,
    recorded_rels: &[String],
) {
    for rel in recorded_rels {
        let abs = sync_root.join(rel);
        match assert_recorded_in_sync(hydrator, invalidator, &abs, rel).await {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                rel,
                "not flipping in-sync: the disk moved past the recorded content (a newer \
                 save); its own upload flips it"
            ),
            Err(e) => tracing::warn!(
                rel,
                error = %e,
                "in-sync flip failed after a recorded upload; the file keeps its \
                 sync-pending state until the next upload retries"
            ),
        }
    }
}

/// After [`flip_recorded_in_sync`], flip every ancestor directory of the
/// recorded rels whose known subtree is fully synced — deepest-first, so a
/// nested chain flips leaf-dir upward. A directory has no bytes of its own; the
/// flip is pure platform sync-state (Explorer's folder ✅), gated on the
/// engine's [`HydrationHost::subtree_fully_synced`] so a folder with any
/// in-flight or diverged child keeps its honest pending arrows. An EMPTY
/// directory never flips (the set doesn't represent it — no other device will
/// ever materialize it). Best-effort per dir, like the file flip.
async fn flip_clean_ancestor_dirs<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: &H,
    invalidator: &I,
    sync_root: &Path,
    recorded_rels: &[String],
) {
    // Distinct ancestors of every recorded rel, deepest-first (stable across
    // chains: sort is stable, insertion order breaks depth ties).
    let mut dirs: Vec<String> = Vec::new();
    for rel in recorded_rels {
        let mut node = Path::new(rel);
        while let Some(parent) = node.parent() {
            let dir = parent.to_string_lossy().replace('\\', "/");
            if dir.is_empty() {
                break;
            }
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
            node = parent;
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));

    for dir in dirs {
        if !hydrator.subtree_fully_synced(&dir).await {
            continue;
        }
        let abs = sync_root.join(&dir);
        let result = if invalidator.is_cloud_file(&abs) {
            invalidator
                .usn(&abs)
                .and_then(|usn| invalidator.set_in_sync(&abs, usn))
        } else {
            invalidator.convert_dir_in_sync(&abs, &dir)
        };
        if let Err(e) = result {
            tracing::warn!(
                dir,
                error = %e,
                "folder in-sync flip failed (retries on the folder's next recorded child)"
            );
        }
    }
}

/// Free `rel`'s local bytes through the root's OS binding — the one road every
/// dehydrate takes (a superseded copy, an unpin, *Free up space*).
///
/// On a cfapi root that is [`PlaceholderInvalidator::dehydrate`] alone: the OS's
/// dirty-file refusal is its gate, and the bytes go first. On an off-disk root
/// (`placeholders_off_disk`) nothing in the OS refuses, so the engine gates and
/// records first ([`HydrationHost::dehydrate_off_disk`]: lossless or nothing, the
/// row flipped unseen, the removal suppression armed) and the unlink comes
/// second — the crash-safe order (`on-demand-files.md` § Linux FUSE binding, the
/// dehydrate rule). An unlink that then fails leaves a `Placeholder` row over the
/// bytes, which the next sweep repairs back to `Synced`.
///
/// `superseded_by` names the version a remote edit made, when that is why the bytes go
/// ([`apply_stale_hydrated`]): the freed placeholder must then describe it
/// ([`PlaceholderInvalidator::supersede`]), not the version whose bytes were freed.
pub(crate) async fn free_local_bytes<H: HydrationHost, I: PlaceholderInvalidator + ?Sized>(
    hydrator: &H,
    invalidator: &I,
    abs: &Path,
    rel: &str,
    superseded_by: Option<&StaleHydratedRow>,
) -> Result<()> {
    if invalidator.placeholders_off_disk() && !hydrator.dehydrate_off_disk(rel).await? {
        return Err(anyhow::anyhow!(
            "not freeing: this device may hold the only copy of the file's content — it \
             has not finished syncing, or its folder keeps no content on the server"
        ));
    }
    match superseded_by {
        None => invalidator.dehydrate(abs),
        Some(head) => invalidator.supersede(
            abs,
            u64::try_from(head.size_bytes).unwrap_or(0),
            head.remote_mtime,
        ),
    }
}

/// Invalidate the hydrated rows the fold reported stale ([`StaleHydratedRow`]): the
/// nest changed a file this device holds hydrated on disk, so the local copy is
/// superseded. For each row, **free the bytes first, re-point the row second**, then
/// push a `CloudOnly` overlay event. Returns the number invalidated.
///
/// **Dehydrate-first is the inverse of the restore path's row-first ordering, and
/// deliberately so** (`docs/goal/behavior/file-sync.md` § Restore):
///
/// - *Restore* records content the user explicitly chose, so there is no local edit
///   to protect: it re-points the row, then dehydrates, and a failed dehydrate just
///   leaves a stale cache any later eviction completes.
/// - *This* invalidation may be racing an **unsynced local edit** (this host is
///   download-only — it never uploads, so a locally edited hydrated file has no
///   other guardian). Dehydrate is that guardian: it *fails* on a dirty file, so we
///   run it first as a gate. On failure we leave the row `Synced` and untouched —
///   the edit survives, and a future reconcile pass resolves it as a conflict.
///   Re-pointing first would strand the edit under a `Placeholder` row claiming no
///   local bytes.
///
/// **One refusal is followed instead of skipped.** On an off-disk root the gate
/// also refuses this device's own record in a metadata-only folder (*a holder
/// keeps what it wrote*). That body is **replaced**: the new head is fetched over
/// it and the row moves last ([`HydrationHost::replace_superseded_own_record`]),
/// the file staying on the disk as `Synced`. No holder answering leaves the old
/// version whole for the next pull to retry.
///
/// **The free leaves the placeholder describing the NEW version**
/// ([`PlaceholderInvalidator::supersede`]): cfapi asks for exactly the placeholder's size
/// on the next open, so a placeholder left at the old size served a grown file's prefix,
/// which the engine then recorded and uploaded over the real edit.
///
/// A crash between the free and the re-point is safe and idempotent: the row still
/// resolves the *old* manifest, so an open before the retry gets the old version — or is
/// refused, when the two versions differ in size ([`serve_fetch_chunked`]), never
/// truncated — and the next pull re-reports the row and retries (at a restart, before
/// any open is served). History is append-only and the nest is never wrong.
pub(crate) async fn apply_stale_hydrated<H: HydrationHost>(
    hydrator: &H,
    invalidator: &dyn PlaceholderInvalidator,
    sync_root: &Path,
    rows: &[StaleHydratedRow],
    event_tx: Option<&broadcast::Sender<Event>>,
) -> usize {
    let mut invalidated = 0usize;
    for row in rows {
        let abs =
            crate::path_map::overlay_abs_path(&sync_root.to_string_lossy(), &row.relative_path);
        // Gate: free the stale bytes, leaving the placeholder describing the new version.
        // A dirty (locally edited) file makes this fail, and we then skip the re-point so
        // the edit is preserved.
        if let Err(e) = free_local_bytes(
            hydrator,
            invalidator,
            Path::new(&abs),
            &row.relative_path,
            Some(row),
        )
        .await
        {
            // A refusal may be the holder-keeps gate: this device's own record in
            // a folder whose nest holds no bytes. That body is replaced, not
            // freed — the new head is fetched over it, so the device never holds
            // no version (`file-sync.md` § Relay serving). Off-disk roots only:
            // the cfapi binding of the rule is not built.
            if invalidator.placeholders_off_disk() {
                match hydrator.replace_superseded_own_record(row).await {
                    Ok(true) => {
                        if let Some(tx) = event_tx {
                            let _ = tx.send(Event {
                                event: EventKind::FileStatusChanged {
                                    path: abs,
                                    status: FileStatus::Synced,
                                },
                            });
                        }
                        invalidated += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(fetch) => {
                        tracing::info!(
                            path = %abs,
                            error = %fetch,
                            "keeping this device's own version: the newer one could not be fetched from a device holding it — retried on the next pull"
                        );
                        continue;
                    }
                }
            }
            tracing::warn!(
                path = %abs,
                error = %e,
                "skipping stale-hydrated invalidation (dehydrate refused — likely an unsynced local edit)"
            );
            continue;
        }
        // Bytes are gone: now the row may safely say "hydrate from the new head".
        if let Err(e) = hydrator.repoint_placeholder(row).await {
            tracing::warn!(
                path = %abs,
                error = %e,
                "re-pointing an invalidated hydrated row failed; next open re-materializes the old bytes until the next pull retries"
            );
            continue;
        }
        if let Some(tx) = event_tx {
            let _ = tx.send(Event {
                event: EventKind::FileStatusChanged {
                    path: abs,
                    status: FileStatus::CloudOnly,
                },
            });
        }
        invalidated += 1;
    }
    invalidated
}

/// Push the fold's brand-NEW rows ([`PlaceholderFold::created`]) onto disk wherever
/// the platform has already listed their directory — the remote-create half of
/// two-way (`delete-propagation.md` § *The floor on an on-demand root*, decision (f)).
///
/// A directory is listed once: cfapi completes a FETCH_PLACEHOLDERS with
/// `DISABLE_ON_DEMAND_POPULATION` and never asks again, so a row the fold writes after
/// that listing would otherwise never appear — until a restart re-arms the root, and
/// in a browsed subdirectory never. Per row, walk its path down from the root to the
/// first segment not on disk; if that segment's parent is a listed directory, create
/// the segment — the file itself, or a directory the platform then lists LAZILY on
/// its own first browse (which is why a deeper row under it stops there). A parent
/// still to be listed gets the row from its own listing: nothing is pushed into it.
/// A leaf already on disk (the user's own file of that name, or a listing that raced
/// the fold) is never touched.
///
/// Best-effort per row, like every other disk act of the fold: a failed create is
/// logged and left for the directory's next listing. Returns the rels of the files
/// created — the placeholders this call provably put on the disk, which the caller
/// marks seen (`delete-propagation.md` § *An offline placeholder delete propagates*,
/// decision (a)), exactly as [`serve_populate`] marks a listing's.
pub(crate) fn materialize_created<I: PlaceholderInvalidator + ?Sized>(
    invalidator: &I,
    sync_root: &Path,
    rows: &[PlaceholderRow],
    event_tx: Option<&broadcast::Sender<Event>>,
) -> Vec<String> {
    let root_str = sync_root.to_string_lossy();
    let mut files = Vec::new();
    for row in rows {
        let segments: Vec<&str> = row.rel.split('/').filter(|s| !s.is_empty()).collect();
        let mut parent_abs = sync_root.to_path_buf();
        let mut parent_rel = String::new();
        for (i, seg) in segments.iter().enumerate() {
            let rel = if parent_rel.is_empty() {
                (*seg).to_string()
            } else {
                format!("{parent_rel}/{seg}")
            };
            let abs = parent_abs.join(seg);
            if std::fs::symlink_metadata(&abs).is_ok() {
                parent_abs = abs;
                parent_rel = rel;
                continue;
            }
            if !invalidator.is_listed_dir(&parent_abs) {
                break; // its own listing will carry the row
            }
            let is_dir = i + 1 < segments.len();
            let size = if is_dir { 0 } else { row.size };
            match invalidator.create_placeholder(&parent_abs, &rel, size, row.mtime, is_dir) {
                Ok(()) if !is_dir => {
                    // On the disk now: the caller marks it seen — after the create,
                    // never before, like `transfer_placeholders`' own set.
                    files.push(rel.clone());
                    if let Some(tx) = event_tx {
                        let _ = tx.send(Event {
                            event: EventKind::FileStatusChanged {
                                path: crate::path_map::overlay_abs_path(&root_str, &rel),
                                status: FileStatus::CloudOnly,
                            },
                        });
                    }
                }
                Ok(()) => {}
                Err(e) => tracing::warn!(
                    rel,
                    error = %e,
                    "materializing a remote create failed; it appears with its directory's next listing"
                ),
            }
            break;
        }
    }
    files
}

/// Act on one [`PlaceholderFold`]: the fold already wrote its new/moved
/// `Placeholder` rows, so what remains is the disk — materializing the NEW rows
/// into directories the platform will not list again ([`materialize_created`]) and
/// invalidating the hydrated copies it *reported* stale. Shared by the startup
/// [`HydrationHost::prepare`] and every periodic [`HydrationHost::repull`] — one
/// code path, so a change picked up on a timer lands in exactly the state a restart
/// would have produced.
async fn apply_fold<H: HydrationHost>(
    hydrator: &H,
    invalidator: &dyn PlaceholderInvalidator,
    sync_root: &Path,
    fold: PlaceholderFold,
    event_tx: Option<&broadcast::Sender<Event>>,
    phase: &'static str,
) {
    tracing::info!(
        phase,
        placeholders = fold.recorded,
        stale_hydrated = fold.stale_hydrated.len(),
        "folded the nest's changes into placeholders"
    );
    if !fold.created.is_empty() {
        let placed = materialize_created(invalidator, sync_root, &fold.created, event_tx);
        tracing::info!(
            phase,
            created = fold.created.len(),
            materialized = placed.len(),
            "materialized remote creates into listed directories"
        );
        if let Err(e) = hydrator.mark_seen(&placed).await {
            // Best-effort like the create itself: the next scan observes the
            // placeholders and marks them (the self-heal), so a lost write costs a
            // pass, never a delete.
            tracing::warn!(phase, error = %e, "marking materialized creates seen failed");
        }
    }
    if !fold.repointed.is_empty() {
        let n = redescribe_repointed(invalidator, sync_root, &fold.repointed);
        tracing::info!(
            phase,
            repointed = fold.repointed.len(),
            redescribed = n,
            "re-described re-pointed placeholders"
        );
    }
    if fold.stale_hydrated.is_empty() {
        return;
    }
    // A `Synced` (hydrated) local copy whose nest head moved: the fold reported it
    // rather than rewrite on-disk bytes. Invalidate it — dehydrate (which refuses
    // on a local edit), then re-point, then flip the overlay to CloudOnly.
    let n = apply_stale_hydrated(
        hydrator,
        invalidator,
        sync_root,
        &fold.stale_hydrated,
        event_tx,
    )
    .await;
    tracing::info!(phase, invalidated = n, "invalidated stale hydrated copies");
}

/// Leave every placeholder the fold RE-POINTED ([`PlaceholderFold::repointed`]) describing
/// its new version, through [`PlaceholderInvalidator::supersede`] — the fold moved only the
/// row, and a placeholder already on the disk still carries the old version's size and
/// mtime. A placeholder not on the disk (its directory not listed yet) gets both from its
/// own listing, and an off-disk root's are rows only, so neither is touched. Best-effort
/// per row like every other disk act of the fold: a refusal (a local edit since — the
/// update is refused unless the placeholder is in sync) or a failure is logged, and the
/// open of a placeholder that still disagrees with its row fails rather than serving the
/// wrong bytes ([`serve_fetch`]). Returns how many were re-described.
fn redescribe_repointed<I: PlaceholderInvalidator + ?Sized>(
    invalidator: &I,
    sync_root: &Path,
    rows: &[PlaceholderRow],
) -> usize {
    if invalidator.placeholders_off_disk() {
        return 0;
    }
    let mut redescribed = 0;
    for row in rows {
        let abs = PathBuf::from(crate::path_map::overlay_abs_path(
            &sync_root.to_string_lossy(),
            &row.rel,
        ));
        if std::fs::symlink_metadata(&abs).is_err() {
            continue;
        }
        match invalidator.supersede(&abs, row.size, row.mtime) {
            Ok(()) => redescribed += 1,
            Err(e) => tracing::warn!(
                rel = row.rel,
                error = %e,
                "re-describing a re-pointed placeholder failed (a local edit since?); its \
                 next open is refused until it is"
            ),
        }
    }
    redescribed
}

/// Re-pull the nest and apply what came back. Best-effort, exactly like the startup
/// `prepare`: a transient socket/nest failure means this pass saw nothing, and the
/// next tick (or the reconnect edge) retries — [`NestClient`] reconnects itself
/// indefinitely and is never torn down on a stall.
async fn repull_and_apply<H: HydrationHost>(
    hydrator: &H,
    invalidator: &dyn PlaceholderInvalidator,
    sync_root: &Path,
    event_tx: Option<&broadcast::Sender<Event>>,
    phase: &'static str,
) {
    match hydrator.repull().await {
        Ok(fold) => apply_fold(hydrator, invalidator, sync_root, fold, event_tx, phase).await,
        Err(e) => {
            tracing::warn!(phase, error = %e, "re-pulling the nest failed; retrying on the next tick")
        }
    }
}

/// The local-write arm's future, or a forever-pending one when this root has no live
/// watcher (it failed to start, or its channel closed).
///
/// **Cancel-safe**, which is what lets it sit in [`run_hydration_loop`]'s `select!`:
/// [`LocalWrites::next`] only ever awaits an mpsc `recv` and an interval `tick` (both
/// cancel-safe), and [`std::future::pending`] trivially so — dropping either can never
/// lose an event it had already taken.
async fn next_local_write<H: LocalWriteHost>(
    local: &mut Option<LocalWrites>,
    host: &H,
) -> fauna_sync_engine::always_resident::LocalWrite {
    match local {
        Some(w) => w.next(host).await,
        None => std::future::pending().await,
    }
}

/// Record `rel` as dehydrated — flip its row `Placeholder` and push
/// `FileStatusChanged{CloudOnly}` so the Explorer overlay flips live (matching the
/// `GetFileStatus` re-query, which maps `Placeholder` → `CloudOnly` — no flicker).
/// The one bookkeeping tail every dehydrate shares, whoever freed the bytes: the
/// OS (`HydrationCommand::Dehydrate`) or this provider ([`react_to_pin`]).
async fn record_dehydrated<H: HydrationHost>(
    hydrator: &H,
    sync_root: &Path,
    rel: &str,
    event_tx: Option<&broadcast::Sender<Event>>,
) {
    if let Err(e) = hydrator.mark_placeholder(rel).await {
        tracing::warn!(rel, error = %e, "recording dehydrated file Placeholder failed");
    }
    if let Some(tx) = event_tx {
        let path = crate::path_map::overlay_abs_path(&sync_root.to_string_lossy(), rel);
        let _ = tx.send(Event {
            event: EventKind::FileStatusChanged {
                path,
                status: FileStatus::CloudOnly,
            },
        });
    }
}

/// React to one file's pin state — the provider-side byte work behind Explorer's
/// *"Free up space"* / *"Always keep on this device"* verbs, which are pure
/// `CfSetPinState` writes the OS expects the provider to act on
/// (`file-sync.md` § Per-file sync-status display, answered question 2;
/// mechanics measured by `diag_pin_reaction_mechanics`). No-op on files whose pin
/// state and byte-presence already agree, which is what makes it idempotent —
/// safe to call from the live watcher arm and the sweep alike, any number of times.
///
/// **Hydrate** (pinned placeholder — only reachable for flips made while the
/// service was down; a live flip is hydrated by cldflt itself through our
/// FETCH_DATA): kick the platform's hydration request and return. The fetch path
/// owns completion (`mark_hydrated` + the `Synced` event).
///
/// **Dehydrate** (unpinned hydrated file): try the **bare** dehydrate first —
/// cfapi's `TRACK_ALL` dirty-file refusal is the razor protecting a user's
/// unsynced edit, and the common (never-edited) path sails through it (measured:
/// a pin flip alone does not trip it). On refusal, the edit that tripped it has
/// either been uploaded since (the refusal is **stale** — cfapi's in-sync bit
/// only ever resets by provider assertion, and nothing asserted it) or is still
/// pending. [`HydrationHost::is_dehydration_safe`] tells them apart by hashing
/// the disk against the row's recorded uploaded identity — so an edit still
/// sitting in the watcher's debounce window fails the gate and the refusal
/// stands. Only a proven-stale refusal is repaired (`set_in_sync`) and retried;
/// a dirty file is left for `converge` to upload, and the next sweep frees it.
async fn react_to_pin<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: &H,
    invalidator: &I,
    sync_root: &Path,
    rel: &str,
    event_tx: Option<&broadcast::Sender<Event>>,
) {
    let abs = sync_root.join(rel);
    match invalidator.pin_action(&abs) {
        None => {}
        Some(PinAction::Hydrate) => {
            tracing::info!(rel, "pinned placeholder: requesting hydration");
            invalidator.kick_hydrate(&abs);
        }
        Some(PinAction::Dehydrate) => {
            let done = match free_local_bytes(hydrator, invalidator, &abs, rel, None).await {
                Ok(()) => true,
                Err(refusal) => {
                    // The refusal is stale only if the content is provably the
                    // recorded content: release the not-in-sync bit through the
                    // one atomic prove-then-assert road (an editor-replaced
                    // ORDINARY file — 0x80070178 territory — is re-anchored first),
                    // then retry the bare dehydrate. A write landing after the
                    // assertion re-trips the bit, so the retry refuses it too.
                    match assert_recorded_in_sync(hydrator, invalidator, &abs, rel).await {
                        Ok(true) => {
                            match free_local_bytes(hydrator, invalidator, &abs, rel, None).await {
                                Ok(()) => true,
                                Err(e) => {
                                    tracing::warn!(
                                        rel,
                                        error = %e,
                                        "unpinned file: dehydrate failed after repair"
                                    );
                                    false
                                }
                            }
                        }
                        Ok(false) => {
                            tracing::debug!(
                                rel,
                                refusal = %refusal,
                                "unpinned file carries unsynced local content; converge \
                                 uploads it first, the next sweep frees it"
                            );
                            false
                        }
                        Err(e) => {
                            tracing::warn!(
                                rel,
                                error = %e,
                                "unpinned file: the in-sync repair failed"
                            );
                            false
                        }
                    }
                }
            };
            if done {
                tracing::info!(rel, "unpinned file dehydrated (Free up space)");
                record_dehydrated(hydrator, sync_root, rel, event_tx).await;
            }
        }
    }
}

/// Sweep the root for pin-state disagreements and react to each — the backstop
/// that heals flips the watcher never saw (made while the service was down),
/// running at startup and on the rescan tick, exactly as `converge` backstops
/// the upload watcher. Stat-only walk; [`react_to_pin`] re-classifies each
/// candidate at action time, so a file that changed between walk and act simply
/// re-classifies (usually to a steady-state no-op).
async fn react_to_pin_sweep<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: &H,
    invalidator: &I,
    sync_root: &Path,
    event_tx: Option<&broadcast::Sender<Event>>,
) {
    // An off-disk root keeps its pins in the rows, and its pinned placeholders are
    // not on the disk for a walk to find: every one is owed a hydration. (Its unpin
    // frees the bytes at the verb — [`set_pin`] — so the rows hold no pending
    // dehydrate for a sweep to find.)
    if invalidator.placeholders_off_disk() {
        match hydrator.pinned_placeholders().await {
            Ok(rels) => {
                for rel in rels {
                    tracing::info!(rel, "pinned placeholder: requesting hydration");
                    invalidator.kick_hydrate(&sync_root.join(&rel));
                }
            }
            Err(e) => tracing::warn!(error = %e, "reading the pinned placeholders failed"),
        }
        return;
    }
    let candidates =
        crate::pin_reaction::sweep_candidates(sync_root, &|abs| invalidator.pin_action(abs));
    for (rel, _) in candidates {
        react_to_pin(hydrator, invalidator, sync_root, &rel, event_tx).await;
    }
}

/// [`EngineCommand::FreeSpace`] on an off-disk root — the agent's *Free up space*
/// verb where the platform has none (`on-demand-files.md` § Linux FUSE binding,
/// the dehydrate rule). A file already cloud-only has nothing to lose (`Ok`); a
/// pinned file refuses (the user asked to keep it); otherwise the bytes are freed
/// only when that is lossless ([`free_local_bytes`]), and the apps see the file go
/// `CloudOnly`.
async fn free_space<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: &H,
    invalidator: &I,
    sync_root: &Path,
    rel: &str,
    event_tx: Option<&broadcast::Sender<Event>>,
) -> Result<()> {
    let Some(pin) = hydrator.row_pin(rel).await? else {
        return Err(anyhow::anyhow!("not a synced file of this folder"));
    };
    if pin.cloud_only {
        return Ok(());
    }
    if pin.pinned {
        return Err(anyhow::anyhow!(
            "not freeing: the file is kept on this device — stop keeping it first"
        ));
    }
    free_local_bytes(hydrator, invalidator, &sync_root.join(rel), rel, None).await?;
    tracing::info!(rel, "file dehydrated (Free up space)");
    push_status(event_tx, sync_root, rel, FileStatus::CloudOnly);
    Ok(())
}

/// [`EngineCommand::SetPinned`] on an off-disk root: the pin lands in the row,
/// then the bytes follow it — a pinned cloud-only file is hydrated now (through
/// the binding's own hydration, [`PlaceholderInvalidator::kick_hydrate`]), an
/// unpinned file is freed when that is lossless. An unpin that cannot free yet
/// (an edit not yet uploaded) still clears the pin: the file is simply a hydrated
/// file again, freed by the user's next *Free up space*.
async fn set_pin<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: &H,
    invalidator: &I,
    sync_root: &Path,
    rel: &str,
    pinned: bool,
    event_tx: Option<&broadcast::Sender<Event>>,
) -> Result<()> {
    if !hydrator.set_pinned(rel, pinned).await? {
        return Err(anyhow::anyhow!("not a synced file of this folder"));
    }
    let Some(pin) = hydrator.row_pin(rel).await? else {
        return Ok(());
    };
    let abs = sync_root.join(rel);
    if pinned {
        if pin.cloud_only {
            tracing::info!(rel, "pinned placeholder: requesting hydration");
            invalidator.kick_hydrate(&abs);
        }
    } else if !pin.cloud_only {
        match free_local_bytes(hydrator, invalidator, &abs, rel, None).await {
            Ok(()) => {
                tracing::info!(rel, "unpinned file dehydrated");
                push_status(event_tx, sync_root, rel, FileStatus::CloudOnly);
            }
            Err(e) => tracing::info!(rel, reason = %e, "unpinned file keeps its bytes for now"),
        }
    }
    Ok(())
}

/// Serve [`HydrationCommand`]s for one engine off `cmd_rx` until the engine is
/// cancelled ([`CancellationToken`]) or its channel closes. At startup it runs
/// [`HydrationHost::prepare`] once (control-plane connect + placeholder
/// population); thereafter it **re-pulls the nest on a timer**, so a remote change
/// reaches this host with no restart.
///
/// **It is TWO-WAY.** Alongside serving reads, it watches the folder and uploads local
/// edits through the shared [`LocalWrites`] / [`apply_local_write`] pair the
/// always-resident loop drives — one uploader, not two (`file-sync.md` § On-Demand Files
/// → *Sync direction*).
///
/// **The re-pull is the host's only inbound-change signal.** There is no nest-side
/// push for folder changes on the WS-RPC plane, so — exactly as the shipped
/// in-process reference does (linux's `run_engine_loop` drives `pull_remote_changes`
/// on `rescan_tick`) — the cadence is a timer, set from the shared reconcile
/// constant ([`HydrationHost::rescan_interval`]).
/// A **reconnect** re-pulls immediately on top of that: changes recorded while the
/// socket was down would otherwise wait out a full tick.
///
/// This is the body of one engine's future on the shared
/// [`EngineHost`](fauna_sync_engine::engine_host::EngineHost)'s driving thread —
/// the thread + current-thread runtime are owned by the host, not spawned here.
/// Generic over [`HydrationHost`] so the loop is testable with a fake; production
/// passes the bearer-only [`SyncEngine`]. `cancel` is the host's per-engine
/// token: when the engine is stopped (folder removed / switched to always) the
/// token fires and this loop returns, letting the caller's future drop its owned
/// resources (the cfapi sync-root guard) and tear the root down.
///
/// Test-only since the product's on-demand start moved to [`serve_hydration_root`]
/// (decision (c) needs the connection in the loop's hands): the loop-level tests and
/// the live harness connect their roots themselves.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_hydration_loop<H: HydrationHost, I: PlaceholderInvalidator>(
    hydrator: H,
    invalidator: I,
    cmd_rx: mpsc::UnboundedReceiver<HydrationCommand>,
    event_tx: Option<broadcast::Sender<Event>>,
    sync_root: PathBuf,
    folder: String,
    cancel: CancellationToken,
    // The two per-folder channels every resident engine takes, in the shared
    // watch loop's order (`always_resident::run_watch_loop`): the remote-change
    // nudge (`PullFolderNow` → `SyncServiceState::wake_senders`) and the
    // invoke-and-reply `EngineCommand` (`SyncServiceState::engine_cmd_senders`).
    // `None` = not wired (the loop tests that do not drive them).
    wake_rx: Option<mpsc::Receiver<()>>,
    engine_cmd_rx: Option<mpsc::Receiver<EngineCommand>>,
) {
    serve_hydration_root(
        hydrator,
        invalidator,
        RootBoot::already_connected(),
        cmd_rx,
        event_tx,
        sync_root,
        folder,
        cancel,
        wake_rx,
        engine_cmd_rx,
        None,
    )
    .await
}

/// What [`serve_hydration_root`] needs to know about the root before it is
/// connected — the two inputs of the on-demand boot order
/// (`delete-propagation.md` § *An offline placeholder delete propagates*,
/// decisions (c) and (e)).
pub(crate) struct RootBoot<C> {
    /// The root's registration did NOT survive the downtime (or was never made):
    /// the OS removed its cloud-only placeholders at the unregister, so every seen
    /// mark is cleared BEFORE the boot sweep — the product's own removal is never
    /// a user's delete (decision (e)). A root whose registration survived keeps
    /// its marks, and the sweep reads their absence as the deletes they are.
    pub fresh_registration: bool,
    /// Connect the root to the OS — called AFTER the boot sweep, never before
    /// (decision (c)); the returned guard lives as long as the loop.
    pub connect: C,
}

#[cfg(test)]
impl RootBoot<fn()> {
    /// The root is already connected by the caller (the loop-level tests and the
    /// live harness's pre-connected roots): no hygiene, a no-op connect.
    pub(crate) fn already_connected() -> Self {
        fn connected() {}
        RootBoot {
            fresh_registration: false,
            connect: connected,
        }
    }
}

/// [`run_hydration_loop`] with the root's connection in the loop's hands — the
/// product's on-demand root ([`crate::engine_driver`]). The boot order is the
/// ruling's (`delete-propagation.md` § *An offline placeholder delete
/// propagates*, decision (c) — **evidence before re-population**):
///
/// 1. the fold's row half ([`HydrationHost::prepare`]: control plane + rows only);
/// 2. the mark hygiene of decision (e) ([`RootBoot::fresh_registration`]);
/// 3. the boot sweep — the startup `converge` (drain → `reconcile` → upload),
///    whose delete detection needs the DB and the disk and nothing else; with no
///    provider connected nothing can re-populate under it;
/// 4. **then** [`RootBoot::connect`] — the re-registration re-arms the root's
///    population, and the first browse lists every tracked `Placeholder` row back,
///    which is why the sweep must already have seen what the downtime deleted;
/// 5. everything else where it always was: the fold's stale-hydrated invalidation
///    (it dehydrates through cfapi, so it needs the connection), the watcher, the
///    ✅ flips of what the sweep uploaded, the corpus passes, the pin sweep, the
///    serve loop.
///
/// **The relay serve runs beside the loop, never as an arm of it.** `serve` is
/// this folder's asks from the host's relay seat (`file-sync.md` § Relay
/// serving): an on-demand root answers them from its hydrated bodies, and a
/// seat that is itself mid-fetch must still decline at once what it does not
/// hold (`fauna_sync_engine::relay_seat`). `None` = not wired (the loop tests).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_hydration_root<H, I, C, G>(
    hydrator: H,
    invalidator: I,
    boot: RootBoot<C>,
    cmd_rx: mpsc::UnboundedReceiver<HydrationCommand>,
    event_tx: Option<broadcast::Sender<Event>>,
    sync_root: PathBuf,
    folder: String,
    cancel: CancellationToken,
    wake_rx: Option<mpsc::Receiver<()>>,
    engine_cmd_rx: Option<mpsc::Receiver<EngineCommand>>,
    serve: Option<fauna_sync_engine::relay_seat::ServeInbox>,
) where
    H: HydrationHost,
    I: PlaceholderInvalidator,
    C: FnOnce() -> G,
{
    let serving = async {
        match serve {
            Some(mut inbox) => inbox.serve_with(|ask| hydrator.answer_serve_ask(ask)).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        () = serving => {}
        () = drive_hydration_root(
            &hydrator,
            invalidator,
            boot,
            cmd_rx,
            event_tx,
            sync_root,
            folder,
            cancel,
            wake_rx,
            engine_cmd_rx,
        ) => {}
    }
}

/// [`serve_hydration_root`]'s own work — the boot order and the serve loop.
#[allow(clippy::too_many_arguments)]
async fn drive_hydration_root<H, I, C, G>(
    hydrator: &H,
    invalidator: I,
    boot: RootBoot<C>,
    mut cmd_rx: mpsc::UnboundedReceiver<HydrationCommand>,
    event_tx: Option<broadcast::Sender<Event>>,
    sync_root: PathBuf,
    folder: String,
    cancel: CancellationToken,
    mut wake_rx: Option<mpsc::Receiver<()>>,
    mut engine_cmd_rx: Option<mpsc::Receiver<EngineCommand>>,
) where
    H: HydrationHost,
    I: PlaceholderInvalidator,
    C: FnOnce() -> G,
{
    // The off-disk posture comes before anything reads the rows: the fold, the
    // mark hygiene and above all the boot sweep must already treat a placeholder
    // as a row, never a file (`on-demand-files.md` § Linux FUSE binding, the
    // dehydrate rule). A host that cannot take it is not served — fail closed.
    if invalidator.placeholders_off_disk()
        && let Err(e) = hydrator.set_placeholders_off_disk().await
    {
        tracing::error!(
            folder,
            error = %e,
            "the host cannot take the off-disk placeholder posture; not serving this root"
        );
        return;
    }

    // One-shot startup: open the control plane + fold the nest's placeholders so a
    // folder that is on-demand from the start is browsable — the ROW half only; its
    // stale-hydrated invalidation waits for the connection (step 5). The engine is
    // `!Sync`, so this runs on the host's driving thread (not the `Send` IPC task
    // that triggered the start).
    let startup_fold = match hydrator.prepare().await {
        Ok(fold) => Some(fold),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "hydration host prepare failed (control-plane connect or placeholder population); \
                 serving already-tracked entries only until a re-pull succeeds"
            );
            None
        }
    };

    // Decision (e): a registration that did not survive took the placeholders with
    // it. Clear every mark BEFORE the sweep, or the sweep would read the product's
    // own removal as the user deleting every placeholder this disk ever held. A
    // failed clear fails CLOSED: the marks still claim placeholders the OS removed,
    // so neither this sweep nor any later one may run over them — the root is not
    // served, and the next engine start retries the clear.
    if boot.fresh_registration
        && let Err(e) = hydrator.clear_seen_all().await
    {
        tracing::error!(
            folder,
            error = %e,
            "clearing the seen marks of a fresh registration failed; not serving this root \
             (a sweep over stale marks would read the OS's own removal of its placeholders \
             as deletes) — the next engine start retries"
        );
        return;
    }

    // The boot sweep (decision (c)) — and the catch-up for edits made while the
    // service was down. Safe on a placeholder root: `reconcile` skips cloud-only
    // placeholders on their OS attributes (never opening one — that stalls 60 s)
    // while still counting them, so a dehydrated file is never mistaken for a
    // deleted one; a SEEN placeholder that is gone is propagated (or held, with its
    // siblings, by the floor) before any browse can list it back.
    let recorded = hydrator.converge(&folder).await;

    // Only now may the OS re-populate the root.
    //
    // The guard is a local of THIS fn, while the engine (`hydrator`) belongs to
    // the caller — so whatever ends the root (a stop, an unbind, a flip, a
    // sign-out, an account switch: each is a cancel), the binding comes down
    // (the FUSE unmount, the cfapi disconnect) before the engine and its state
    // DB do, and no request through the binding ever finds the engine gone
    // (`on-demand-files.md` § Linux FUSE binding, the flips rule). Keep it here.
    let _connection = (boot.connect)();

    if let Some(fold) = startup_fold {
        apply_fold(
            hydrator,
            &invalidator,
            &sync_root,
            fold,
            event_tx.as_ref(),
            "startup",
        )
        .await
    }

    // ── The two-way half ──────────────────────────────────────────────────────────
    //
    // An on-demand root uploads local edits, exactly as an always-resident one does
    // (`file-sync.md` § On-Demand Files → *Sync direction*): on-demand is a storage
    // choice, never a direction choice. This is the SHARED watcher/debouncer/uploader
    // the always-resident loop drives (`always_resident::LocalWrites`) — one uploader,
    // not a second implementation grown inside the hydration host (priorities #2/#4).
    //
    // `LocalWrites::is_user_write` is what makes a watcher safe on a *cfapi* root: our
    // own `CfExecute(TRANSFER_PLACEHOLDERS)` materializes files that the OS reports as
    // ordinary `Created` events, and `was_recent_download` does not cover placeholder
    // creation — so the placeholder guard is applied at the source.
    //
    // A failed watcher start is NOT fatal: the root still serves hydration, and the
    // rescan tick's `converge` still catches local edits (a slower path, but a real
    // one). Going one-way here would silently drop the user's edits, which is the very
    // bug this closes — so it is loud.
    let mut local = match LocalWrites::start(&sync_root) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::error!(
                folder,
                error = %e,
                "watcher start failed on the on-demand root: local edits will only be picked up \
                 by the rescan tick, not live. A tracked file's change MUST still be uploaded."
            );
            None
        }
    };

    // The boot sweep's recorded uploads (the edits made while the service was down)
    // flip to the platform ✅ exactly as the watcher path's do (below), or the file
    // keeps Explorer's sync-pending arrows until the next *live* edit (live
    // 2026-07-17 startup-converge race). After the connect — the in-sync assertion
    // is a cfapi call — and BEFORE the pin sweep, so an edited-then-unpinned file is
    // already in-sync when its dehydrate runs.
    flip_recorded_in_sync(hydrator, &invalidator, &sync_root, &recorded).await;
    flip_clean_ancestor_dirs(hydrator, &invalidator, &sync_root, &recorded).await;

    // The corpus passes — what a start owes the folder's at-rest bytes, on this
    // root exactly as on an always-resident one: the pre-bind re-seal, the
    // audience convergence (a flip made while this seat was down), the
    // post-succession re-seal, then the posture refresh + per-tick pair
    // (audience + website) the rescan tick below keeps re-running. AFTER
    // converge, so the passes see the uploads it just recorded; BEFORE the pin
    // sweep, so nothing is dehydrated under a pass that could still read it
    // locally (a cloud-only entry is fetched from the nest either way). Until
    // 2026-09-27 this root ran none of them — and never refreshed its posture,
    // so it kept the build-time audience for ever — so an owner's public window
    // never declassified an on-demand folder's back-catalogue and the flip back
    // never re-sealed it.
    hydrator.converge_corpus_at_start(&folder).await;
    hydrator.refresh_and_converge_corpus(&folder).await;

    // Heal pin flips made while the service was down (Explorer's cloud verbs write
    // pin state whether or not the provider is up). AFTER converge, so an
    // edited-then-unpinned file is uploaded before its dehydrate is attempted.
    react_to_pin_sweep(hydrator, &invalidator, &sync_root, event_tx.as_ref()).await;

    // The steady-state inbound-change signal. Read the cadence AFTER `prepare`, so
    // the control plane it rides is already open; a failed read degrades to the
    // shared default rather than leaving the host without a tick (the re-pull is
    // also how a host that came up against a down nest recovers).
    let rescan_interval = hydrator.rescan_interval().await;
    fauna_sync_engine::always_resident::log_rescan_armed(
        &fauna_core::log_redact::log_folder_name(&folder),
        rescan_interval,
    );
    let mut rescan_tick = tokio::time::interval(rescan_interval);
    rescan_tick.tick().await; // consume the immediate first tick — `prepare` just folded
    let mut reconnected = hydrator.reconnects();
    // Cleared if the reconnect watch's sender ever goes away: a closed `watch`
    // makes `changed()` return `Err` *immediately, forever*, which would spin this
    // select into a hot loop. Production can't hit it (the engine owns the
    // `NestClient` that holds the sender, and the engine is owned by this loop) —
    // this just makes that impossible to regress into.
    let mut watch_reconnects = true;

    loop {
        tokio::select! {
            biased;

            // Engine cancelled (folder removed / switched to always, or the whole
            // host dropped). Return so the owning future tears its root down.
            _ = cancel.cancelled() => break,

            // The local-write half. Cancel-safe (`LocalWrites::next` only awaits an mpsc
            // recv + an interval tick), so racing it here can never drop an event it had
            // already taken. `pending()` when the watcher is absent/closed — the root goes
            // on serving hydration, and `converge` on the rescan tick remains the backstop.
            write = next_local_write(&mut local, hydrator) => {
                // A pin flip on a hydrated file arrives as an ordinary Modified event
                // and lands in this batch (measured: CfSetPinState is attribute-only,
                // and ReadDirectoryChangesW reports it like any modification), so the
                // batch's rels are exactly the live pin-reaction candidates. Snapshot
                // them before the write is consumed; react AFTER the uploads, so an
                // edit+unpin in one debounce window uploads first and dehydrates in
                // the same pass.
                let pin_rels: Vec<String> = match &write {
                    LocalWrite::Upload(rels) => rels.clone(),
                    _ => Vec::new(),
                };
                let applied = apply_local_write(hydrator, &folder, write).await;
                if !applied.continue_watching {
                    tracing::error!(
                        folder,
                        "the on-demand root's file watcher closed; local edits now reach the \
                         nest only via the rescan tick's converge, not live"
                    );
                    local = None;
                }
                // The last leg of a two-way upload: a rel whose change record
                // reached the nest flips to the platform's ✅ (in-sync). BEFORE
                // the pin reaction, so an edit+unpin in one debounce window sees
                // the file already in-sync when its dehydrate runs. Then the
                // clean ancestors: the folder ✅ follows its files.
                flip_recorded_in_sync(hydrator, &invalidator, &sync_root, &applied.recorded).await;
                flip_clean_ancestor_dirs(hydrator, &invalidator, &sync_root, &applied.recorded).await;
                for rel in pin_rels {
                    react_to_pin(hydrator, &invalidator, &sync_root, &rel, event_tx.as_ref()).await;
                }
            }

            _ = rescan_tick.tick() => {
                repull_and_apply(hydrator, &invalidator, &sync_root, event_tx.as_ref(), "rescan").await;
                // Inbound first, then outbound. A file edited locally *and* moved on the nest
                // is safe in this order: `apply_stale_hydrated`'s dehydrate REFUSES a file
                // carrying unsynced local edits (`PlaceholderInvalidator::dehydrate`), so the
                // edit survives the fold and this pass uploads it.
                let recorded = hydrator.converge(&folder).await;
                // The last leg, same as the watcher path: converge-driven uploads (rescan
                // catch-up, watcher misses, offline edits) flip ✅ too — before the pin
                // backstop, for the same edit-survival ordering.
                flip_recorded_in_sync(hydrator, &invalidator, &sync_root, &recorded).await;
                flip_clean_ancestor_dirs(hydrator, &invalidator, &sync_root, &recorded).await;
                // The live path an audience flip or a website toggle takes to this
                // RUNNING root — the same posture refresh + per-tick pair the
                // always-resident loop runs (`always_resident::refresh_and_converge_corpus`).
                // After converge, so a re-seal sees the tick's own uploads recorded.
                hydrator.refresh_and_converge_corpus(&folder).await;
                // Pin backstop, after converge for the same edit-survival reason: catches
                // flips the watcher missed and dirty-at-flip-time files converge just uploaded.
                react_to_pin_sweep(hydrator, &invalidator, &sync_root, event_tx.as_ref()).await;
            }

            // The socket came back. Anything recorded while it was down is waiting
            // in `changes.list` — fold it now instead of at the next tick.
            res = reconnected.changed(), if watch_reconnects => match res {
                Ok(()) => {
                    repull_and_apply(hydrator, &invalidator, &sync_root, event_tx.as_ref(), "reconnect").await;
                }
                Err(_) => watch_reconnects = false,
            },

            // Off-cadence remote-change nudge (`PullFolderNow`): the nest
            // signalled a new sync record in this set, so fold it now rather
            // than at the next tick — the same re-pull the tick and the
            // reconnect edge run, as the resident loop's nudge arm pulls.
            Some(()) = fauna_core::select_pending::recv_or_pending(&mut wake_rx) => {
                repull_and_apply(hydrator, &invalidator, &sync_root, event_tx.as_ref(), "nudge").await;
            }

            // Invoke-and-reply command from the pipe server (the mass-delete
            // floor's confirm, a share-ingest page) — answered through the
            // host's shared answer, exactly as the resident loop's command arm
            // (`always_resident::answer_engine_command`).
            //
            // *Free up space* and the pins are this loop's own: the bytes they move
            // are the root's binding's (an off-disk root's; cfapi roots take both
            // from the OS shell and never send them).
            Some(cmd) = fauna_core::select_pending::recv_or_pending(&mut engine_cmd_rx) => match cmd {
                EngineCommand::FreeSpace { rel, reply } => {
                    let _ = reply.send(
                        free_space(hydrator, &invalidator, &sync_root, &rel, event_tx.as_ref()).await,
                    );
                }
                EngineCommand::SetPinned { rel, pinned, reply } => {
                    let _ = reply.send(
                        set_pin(hydrator, &invalidator, &sync_root, &rel, pinned, event_tx.as_ref())
                            .await,
                    );
                }
                cmd => hydrator.answer_engine_command(cmd).await,
            },

            cmd = cmd_rx.recv() => match cmd {
                // A hydration opens a batch: the loop keeps serving listings (and
                // starts further hydrations) while it downloads, and serves the
                // rest after it (`serve_materialize_batch`).
                Some(HydrationCommand::Materialize { rel, reply }) => {
                    let deferred = serve_materialize_batch(
                        hydrator,
                        rel,
                        reply,
                        &mut cmd_rx,
                        &sync_root,
                        event_tx.as_ref(),
                        &cancel,
                    )
                    .await;
                    for cmd in deferred {
                        serve_command(hydrator, cmd, &sync_root, event_tx.as_ref()).await;
                    }
                }
                Some(cmd) => serve_command(hydrator, cmd, &sync_root, event_tx.as_ref()).await,
                // All senders dropped (the platform binding is gone).
                None => break,
            }
        }
    }
}

/// Serve one [`HydrationCommand`] to completion on the driving thread.
async fn serve_command<H: HydrationHost>(
    hydrator: &H,
    cmd: HydrationCommand,
    sync_root: &Path,
    event_tx: Option<&broadcast::Sender<Event>>,
) {
    match cmd {
        HydrationCommand::Fetch {
            rel,
            offset,
            length,
            file_size,
            sink,
        } => {
            // Wrap the sink to emit per-chunk progress when an event
            // subscriber (the app) is wired and the range is known.
            let sink: Box<dyn TransferSink + Send> = match (event_tx, length) {
                (Some(tx), len) if len > 0 => Box::new(ProgressTransferSink {
                    inner: sink,
                    event_tx: tx.clone(),
                    folder: rel.clone(),
                    bytes_total: len as u64,
                    bytes_done: AtomicU64::new(0),
                }),
                _ => sink,
            };
            match serve_fetch(hydrator, &*sink, &rel, offset, length, file_size).await {
                Ok(content_hash) => {
                    // The file's bytes are now served to the OS (hydrated on
                    // disk). Record it Synced so the synchronous GetFileStatus
                    // query agrees, and push a FileStatusChanged so Explorer
                    // overlays flip CloudOnly → Synced live (no re-query).
                    //
                    // The hash is what stops the two-way root from reading this
                    // freshly-downloaded file as a local edit and echoing it
                    // straight back up (`SyncEngine::mark_hydrated`).
                    if let Err(e) = hydrator.mark_hydrated(&rel, content_hash).await {
                        tracing::warn!(rel, error = %e, "marking hydrated file Synced failed");
                    }
                    push_status(event_tx, sync_root, &rel, FileStatus::Synced);
                }
                // serve_fetch already reported failure to the sink.
                Err(e) => tracing::warn!(rel, error = %e, "hydration fetch failed"),
            }
        }
        HydrationCommand::Populate { parent_rel, sink } => {
            if let Err(e) = serve_populate(hydrator, &*sink, &parent_rel).await {
                // serve_populate already completed the op with an empty set.
                // `?e` (not `%e`): anyhow's Display drops the source chain, which is
                // exactly where the underlying windows::core::Error HRESULT lives.
                tracing::warn!(parent_rel, error = ?e, "directory population failed");
            }
        }
        HydrationCommand::Dehydrate { rel } => {
            // The OS freed this file's bytes (Storage Sense under disk
            // pressure — Explorer's "Free up space" is a pin-state write
            // handled by `react_to_pin`, not an OS dehydrate). The bytes
            // are gone but the content on the nest is unchanged, so record
            // the row back to `Placeholder` (keeping the manifest anchor)
            // and push CloudOnly so the overlay flips Synced → CloudOnly
            // live. Symmetric inverse of the Fetch success arm above.
            // Without this the row stays a dishonest `Synced` while the
            // disk is empty (harmless only because the placeholder guard
            // catches it downstream — `file-sync.md` § On-Demand Files →
            // *present-but-unreadable*).
            record_dehydrated(hydrator, sync_root, &rel, event_tx).await;
        }
        // Not reached from the loop, which opens a batch for every hydration
        // (`serve_materialize_batch`) — served alone here for completeness.
        HydrationCommand::Materialize { rel, reply } => {
            let outcome = materialize(hydrator, &rel, sync_root).await;
            finish_materialize(hydrator, &rel, outcome, sync_root, event_tx, &[reply]).await;
        }
        HydrationCommand::Unlink { rel, reply } => {
            // Record-first (`SyncEngine::handle_delete`): a `Placeholder` row
            // leaves for `LocallyDeleted` before the record is attempted, so it
            // is never listed again whatever the nest answers, and a failed
            // record stays owed to the next sweep. So the unlink has happened
            // once the call returns — only an error the row did not survive
            // (a DB failure) fails it.
            let outcome = hydrator.delete_placeholder(&rel).await.map_err(|e| {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(&rel),
                    error = %e,
                    "recording the delete of a placeholder unlinked through the mount failed"
                );
                e.to_string()
            });
            answer(&reply, outcome);
        }
    }
}

/// Push `FileStatusChanged{status}` for `rel` — keyed by the path the user and
/// the apps see (`path_map::overlay_abs_path` maps a linux root's descriptor
/// reach back to its mount point).
fn push_status(
    event_tx: Option<&broadcast::Sender<Event>>,
    sync_root: &Path,
    rel: &str,
    status: FileStatus,
) {
    if let Some(tx) = event_tx {
        let path = crate::path_map::overlay_abs_path(&sync_root.to_string_lossy(), rel);
        let _ = tx.send(Event {
            event: EventKind::FileStatusChanged { path, status },
        });
    }
}

/// The prefix of the temp file a hydration downloads into, beside its target.
/// A dot-name: the watcher, the scans and the ignore rules all pass over a
/// hidden component (`fauna_sync_engine::ignore::has_hidden_component`), so the
/// half-written file is never uploaded, and the FUSE view does not list it.
pub(crate) const HYDRATING_PREFIX: &str = ".fauna-hydrating-";

/// Removes a hydration's temp file on every path that does not rename it into
/// place — a failed download, a target that appeared meanwhile, a hydration
/// dropped by the root's cancellation.
struct RemoveOnDrop(Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Download `rel` beside its target and rename it into place, returning the
/// served content's hash — or `None` when the target was already on the disk
/// (nothing to hydrate: the open is served by the file that is there).
///
/// `sync_root` is the directory the engine works on — on linux the descriptor
/// reach UNDER the mount, never the mounted view.
async fn materialize<H: HydrationHost>(
    hydrator: &H,
    rel: &str,
    sync_root: &Path,
) -> Result<Option<ContentHash>> {
    if !fauna_core::path_guard::is_safe_relative_path(rel) {
        anyhow::bail!("refusing to hydrate an unsafe path");
    }
    let dest = sync_root.join(rel);
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Ok(None);
    }
    let (Some(parent), Some(name)) = (dest.parent(), dest.file_name()) else {
        anyhow::bail!("a hydration target needs a parent and a name");
    };
    // A placeholder under a directory that exists only as rows: the directory
    // becomes real when its first file does.
    std::fs::create_dir_all(parent)?;
    let mut tmp_name = std::ffi::OsString::from(HYDRATING_PREFIX);
    tmp_name.push(name);
    let tmp = parent.join(tmp_name);
    let mut guard = RemoveOnDrop(Some(tmp.clone()));
    let hash = hydrator.download_to_path(rel, &tmp).await?;
    // A file the user wrote at this name while the download ran is theirs: it
    // wins, and the download is discarded.
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Ok(None);
    }
    std::fs::rename(&tmp, &dest)?;
    guard.0 = None;
    Ok(Some(hash))
}

/// After a hydration: record the served identity (so the watcher's event for
/// the rename reads as the download it is, never as a local edit), push the
/// `Synced` status, and answer every request waiting on it.
async fn finish_materialize<H: HydrationHost>(
    hydrator: &H,
    rel: &str,
    outcome: Result<Option<ContentHash>>,
    sync_root: &Path,
    event_tx: Option<&broadcast::Sender<Event>>,
    replies: &[LoopReply],
) {
    let outcome = match outcome {
        Ok(Some(hash)) => {
            if let Err(e) = hydrator.mark_hydrated(rel, hash).await {
                tracing::warn!(
                    path = %fauna_core::log_redact::log_path(rel),
                    error = %e,
                    "marking a hydrated file Synced failed"
                );
            }
            push_status(event_tx, sync_root, rel, FileStatus::Synced);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(rel),
                error = ?e,
                "hydrating a placeholder on open failed"
            );
            Err(e.to_string())
        }
    };
    for reply in replies {
        answer(reply, outcome.clone());
    }
}

/// Serve a hydration and everything that arrives while it runs.
///
/// A download is the one long wait the driving loop has, and a FUSE root's
/// users wait on the loop for listings too: so while the batch is open the loop
/// goes on serving [`HydrationCommand::Populate`], starts every further
/// [`HydrationCommand::Materialize`] alongside the running ones (a second
/// request for a `rel` already downloading joins it — one download, every
/// waiter answered), and defers everything else, which it returns for the
/// caller to serve once the last download is done.
///
/// Deliberately narrow: only downloads overlap — with each other and with
/// listings, whose engine work is a synchronous DB read. The write half, the
/// sweeps and the corpus passes stay strictly one at a time on the engine, as
/// they are on every other root, because they hold the engine across awaits
/// of their own.
#[allow(clippy::too_many_arguments)]
async fn serve_materialize_batch<H: HydrationHost>(
    hydrator: &H,
    first_rel: String,
    first_reply: LoopReply,
    cmd_rx: &mut mpsc::UnboundedReceiver<HydrationCommand>,
    sync_root: &Path,
    event_tx: Option<&broadcast::Sender<Event>>,
    cancel: &CancellationToken,
) -> Vec<HydrationCommand> {
    use futures_util::StreamExt as _;
    use futures_util::stream::FuturesUnordered;

    let start = |rel: String| async move {
        let outcome = materialize(hydrator, &rel, sync_root).await;
        (rel, outcome)
    };
    let mut waiting: std::collections::HashMap<String, Vec<LoopReply>> =
        std::collections::HashMap::from([(first_rel.clone(), vec![first_reply])]);
    let mut running = FuturesUnordered::new();
    running.push(start(first_rel));
    let mut deferred = Vec::new();
    let mut channel_open = true;

    while !running.is_empty() {
        tokio::select! {
            biased;

            // The root is going away: every waiter is answered (its request
            // fails), and dropping the downloads removes their temp files.
            _ = cancel.cancelled() => {
                for replies in waiting.values() {
                    for reply in replies {
                        answer(reply, Err("the on-demand root is stopping".into()));
                    }
                }
                return deferred;
            }

            Some((rel, outcome)) = running.next() => {
                let replies = waiting.remove(&rel).unwrap_or_default();
                finish_materialize(hydrator, &rel, outcome, sync_root, event_tx, &replies).await;
            }

            cmd = cmd_rx.recv(), if channel_open => match cmd {
                Some(HydrationCommand::Materialize { rel, reply }) => {
                    if let Some(replies) = waiting.get_mut(&rel) {
                        replies.push(reply);
                    } else {
                        waiting.insert(rel.clone(), vec![reply]);
                        running.push(start(rel));
                    }
                }
                Some(HydrationCommand::Populate { parent_rel, sink }) => {
                    if let Err(e) = serve_populate(hydrator, &*sink, &parent_rel).await {
                        tracing::warn!(parent_rel, error = ?e, "directory population failed");
                    }
                }
                Some(other) => deferred.push(other),
                None => channel_open = false,
            },
        }
    }
    deferred
}

// ---------------------------------------------------------------------------
// Engine inputs (bearer-only, no keypair, no MLS)
// ---------------------------------------------------------------------------

/// One agent engine's control-plane client: a keypair-less [`AuthClient`] over a
/// [`crate::bearer::CapabilityBearer`] on the caller's OWN nest, unconnected.
///
/// The control plane always rides the own nest, a cross-nest set's included —
/// that is what a cross-nest relay is: this actor has a session only there, and
/// its own nest forwards the federated read/record on its behalf. The driver
/// connects it before the build, which reads over it.
pub(crate) fn agent_control_plane(
    capability: Arc<crate::bearer::CapabilitySlot>,
    nest_base_url: String,
    actor_id: [u8; 32],
) -> Arc<NestClient> {
    NestClient::with_auth(crate::bearer::bearer_only_auth_client(
        capability,
        nest_base_url,
        actor_id,
    ))
}

/// The [`EngineParams`] one agent engine is built from — **the same inputs for
/// both roots**: every engine this agent runs is built by the shared one builder
/// ([`fauna_sync_engine::engine_lifecycle::build_engine`], `on-demand-files.md` §
/// Shared sets on a capability host → *One mechanism*, question 2), which
/// resolves the set's keys from the holder's custody under the capability's
/// `BackupKey`, and — for a cross-nest set — dials its home nest.
///
/// - **No keypair, no MLS.** The engine's `mls` handle feeds only
///   `device_sync_channel_id()`; the seal side picks its key by audience
///   (`effective_backup_key` → `BackupKey`, else `content_seal_root` → the set's
///   content key) and both planes authenticate with the bearer — so a
///   bearer-only engine uploads correctly and fails closed when it holds no key
///   for a bound set.
/// - **No device registration.** The agent shares its app's device id, and the
///   app registers that device under the user's own label.
/// - **Both roots honour `.faunaignore`.** The shared build loads it for every
///   engine, and reading it cannot hit a placeholder: the scan skips every
///   dotfile (`watcher::scan_recursive_filtered`), so `.faunaignore` is never
///   uploaded and never comes back as a placeholder, and the cloud-files error
///   an orphaned placeholder root *directory* answers a lookup with reads as "no
///   ignore file" (`IgnoreMatcher::load`; live 2026-07-17, an orphaned root dir
///   made every engine build fail here, folder permanently inert).
///
/// `access_gate` is the terminal `access-revoked` park flag (D4, `file-sync.md`
/// § Multi-writer shared sets): passed in because a cross-nest set's byte-plane
/// bearer is built before the engine and must share it, and the caller keeps its
/// own handle to observe the park (`engine_driver`: persist it + drop the set
/// from the running plan).
/// The account's succession material as the capability carries it — what an
/// engine this agent builds needs to read a successor's corpus
/// (`sync-agent.md` § Credential model → *Retired owner keys after an identity
/// succession*) and to bound what a predecessor's signature opens
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(c)). Empty for every identity that never succeeded.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentPredecessors {
    /// `SyncCapability::predecessor_backup_keys` — unpaired, nearest first.
    pub(crate) keys: Vec<[u8; 32]>,
    /// `SyncCapability::predecessor_keys_by_actor` — `(actor id, key)` pairs.
    pub(crate) keys_by_actor: Vec<([u8; 32], [u8; 32])>,
    /// `SyncCapability::predecessor_actor_ids` — the attested ids.
    pub(crate) actor_ids: Vec<[u8; 32]>,
}

impl AgentPredecessors {
    pub(crate) fn from_capability(cap: &fauna_ipc::sync::SyncCapability) -> Self {
        Self {
            keys: cap.predecessor_backup_keys(),
            keys_by_actor: cap.predecessor_keys_by_actor(),
            actor_ids: cap.predecessor_actor_ids(),
        }
    }

    /// The engine's read candidates: the paired keys when the app sent them
    /// (only a paired key is offered to a row signed as a predecessor), else
    /// the unpaired ones (an app that hands no account registry, `accounts:
    /// None` — a predecessor's row is then a noted skip, fail-closed).
    fn engine_keys(&self) -> Vec<fauna_core::file_download::PredecessorSealKey> {
        if self.keys_by_actor.is_empty() {
            self.keys
                .iter()
                .map(|k| BackupKey::from_bytes(*k).into())
                .collect()
        } else {
            self.keys_by_actor
                .iter()
                .map(|(id, key)| {
                    fauna_core::file_download::PredecessorSealKey::named(
                        fauna_core::identity::ActorId(*id),
                        BackupKey::from_bytes(*key),
                    )
                })
                .collect()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_engine_params(
    nest_rpc: Arc<NestClient>,
    state_dir: PathBuf,
    watch_dir: PathBuf,
    folder_ref: fauna_core::folder_keys::FolderRef,
    device_id: [u8; 32],
    backup_key: [u8; 32],
    // The account's retired owner keys after an identity succession — **read**
    // candidates only (`sync-agent.md` § Credential model → *Retired owner keys
    // after an identity succession*) — and the attested ids the reader binding
    // admits a predecessor's rows by. Empty for every identity that never
    // succeeded.
    predecessors: &AgentPredecessors,
    // Per-file transfer progress (`FileDone`, …) — `None` disables it. The live
    // driver wires a real sender so completed uploads re-surface as desktop
    // notifications.
    progress_tx: fauna_sync_engine::progress::ProgressTx,
    access_gate: Arc<fauna_sync_engine::access_gate::AccessGate>,
    // The machine's change-record signer (`principal_bundle::load_change_signer`
    // — the principal writer key + its `SyncWrite` grant), resolved by the
    // caller: building the params must not reach the OS credential store (a
    // test would read the developer's real keyring). The build binds it to the
    // set's nonce from custody; `None` records unsigned.
    change_signer: Option<Arc<fauna_protocol::sync_writer_sig::ChangeSigner>>,
    // The account's folder-key custody through the mounted store
    // (`SyncServiceState::folder_keys`).
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
) -> EngineParams {
    EngineParams {
        state_dir,
        watch_dir,
        folder_ref,
        device_id,
        device_label: None,
        auth: Arc::clone(nest_rpc.auth()),
        nest_rpc,
        mls: None,
        credential: EngineCredential::BackupKey(BackupKey::from_bytes(backup_key)),
        progress_tx,
        predecessor_backup_keys: predecessors.engine_keys(),
        predecessor_actor_ids: predecessors.actor_ids.clone(),
        // Resident engines, built once a set: each keeps its own walk memory
        // for an id the capability's attested set lacks.
        learned_predecessors: Default::default(),
        access_gate: Some(access_gate),
        change_signer,
        folder_keys,
        // The agent's engines sit over bound folders the user can write: a
        // reader's set is refused, never built read-only.
        reader_hosting: fauna_sync_engine::engine_lifecycle::ReaderHosting::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use fauna_ipc::sync::{BearerToken, SyncCapability};

    // ── Fakes ──

    struct FakeHydrator {
        /// `None` means "hydrate fails".
        bytes: Option<Vec<u8>>,
        /// Every rel `mark_seen` was handed — what `serve_populate` vouched was put
        /// on the disk.
        seen: Arc<std::sync::Mutex<Vec<String>>>,
        /// The boot's observable order: `prepare`, `clear_seen_all`, `converge` push
        /// their names here, and a test's `RootBoot::connect` pushes `connect`.
        boot_log: Arc<std::sync::Mutex<Vec<&'static str>>>,
        /// `clear_seen_all` fails — the fail-closed arm of decision (e).
        clear_fails: bool,
        /// Placeholder rows the `PlaceholderLister` half returns; `None` means
        /// "listing fails".
        rows: Option<Vec<fauna_sync_engine::enumerate::PlaceholderRow>>,
        /// Records each `mark_hydrated(rel)` call so a test can assert the loop
        /// marks a fetched file Synced. `Arc` so the test keeps a handle after the
        /// hydrator is moved into `run_hydration_loop`.
        hydrated: Arc<std::sync::Mutex<Vec<String>>>,
        /// Records each `mark_placeholder(rel)` call so a test can assert an
        /// OS-dehydrate flips the row back to Placeholder (the Fetch inverse).
        placeheld: Arc<std::sync::Mutex<Vec<String>>>,
        /// Records each `repoint_placeholder(row)` call so a stale-hydrated apply
        /// test can assert which rows were re-pointed (and which were skipped).
        repointed: Arc<std::sync::Mutex<Vec<String>>>,
        /// Every `replace_superseded_own_record(row)` call, by rel.
        replace_asked: Arc<std::sync::Mutex<Vec<String>>>,
        /// What that call answers: `Some(true)` replaced, `Some(false)` not the
        /// rule's case, `None` no holder answered (an error).
        replace_answer: Option<bool>,
        /// What each successive `repull()` returns (front first). Exhausted → an
        /// empty fold, so a loop that keeps ticking stays a harmless no-op. An
        /// `Err` entry fakes an unreachable nest.
        repulls: Arc<std::sync::Mutex<std::collections::VecDeque<Result<PlaceholderFold>>>>,
        /// Every `repull()` call, counted — the assertion that the *timer* (not
        /// just startup) drives the fold.
        repull_calls: Arc<std::sync::Mutex<usize>>,
        /// What `rescan_interval()` reports; `None` → the shared default, which is
        /// what the non-timer tests want (a 300 s tick never fires in them).
        interval: Option<Duration>,
        /// The reconnect watch. A test bumps the sender *before* the loop starts:
        /// the receiver it hands out then has an unseen change, so `changed()`
        /// returns immediately — exactly the edge `NestClient` produces when the
        /// socket comes back. Kept alive here (never dropped) so the loop's arm
        /// stays pending in every other test rather than erroring.
        reconnect: Arc<(watch::Sender<u64>, watch::Receiver<u64>)>,
        /// Cancel the loop once this many `repull()`s have run, so a timer test
        /// terminates deterministically instead of ticking forever.
        stop_after_repulls: Option<(usize, CancellationToken)>,
        /// Every `converge()` call, counted — the assertion that the on-demand root runs the
        /// **local-write catch-up backstop** at startup and on the rescan tick, not just the
        /// nest re-pull. A root that only re-pulls is the one-way root this track killed.
        converges: Arc<std::sync::Mutex<usize>>,
        /// Rels whose `is_dehydration_safe` answers `true` — the engine's own
        /// "this content is provably uploaded" verdict, faked per-rel.
        dehydration_safe: std::collections::HashSet<String>,
        /// A write that lands WHILE the engine proves the content: each
        /// `is_dehydration_safe` call bumps this USN (share it with the
        /// [`FakeInvalidator::usn_now`] under test). The proof still answers from
        /// `dehydration_safe` — it hashed the bytes before the write.
        write_during_proof: Option<Arc<std::sync::atomic::AtomicI64>>,
        /// Dirs whose `subtree_fully_synced` answers `true` — the folder-✅
        /// predicate, faked per-dir.
        clean_subtrees: std::collections::HashSet<String>,
        /// Rels the fake `converge` reports as recorded (its change record reached the
        /// nest). The on-demand host must flip these ✅ exactly as it flips
        /// `apply_local_write`'s `recorded`; the bug this pins is the loop dropping
        /// `converge`'s return, so a startup / offline / rescan-catch-up edit never
        /// flipped until the next *live* edit (live 2026-07-17 startup-converge race).
        converge_records: Vec<String>,
        /// Every `converge_corpus_at_start()` call, counted — the on-demand root owes
        /// the corpus the once-per-start passes exactly as the always-resident root
        /// does (the 2026-09-27 flip-back finding: this root ran none of them).
        corpus_starts: Arc<std::sync::Mutex<usize>>,
        /// Every `refresh_and_converge_corpus()` call, counted — once at startup,
        /// then once per rescan tick: the live path an audience flip takes to a
        /// RUNNING on-demand root.
        corpus_ticks: Arc<std::sync::Mutex<usize>>,
        /// Every `answer_engine_command()` call, by variant name — the loop's
        /// command arm reaching the host. The fake answers with a canned verdict
        /// ([`FAKE_APPLIED`] / an empty share report at cursor
        /// [`FAKE_SHARE_CURSOR`]); the production answer is pinned against a real
        /// engine in `a_real_engine_answers_apply_held_deletes_through_the_shared_verb`.
        engine_commands: Arc<std::sync::Mutex<Vec<&'static str>>>,
        /// Every `download_to_path(rel)` call, in order — one per download the
        /// loop actually started (a joined hydration adds none).
        downloads: Arc<std::sync::Mutex<Vec<String>>>,
        /// Held closed (`false`), every download waits until it opens — so a test
        /// can prove what the loop serves while a hydration is in flight.
        download_gate: Option<watch::Receiver<bool>>,
        /// Every `handle_delete(rel)` call — a placeholder unlinked through a
        /// FUSE mount reaches the host here.
        deleted: Arc<std::sync::Mutex<Vec<String>>>,
    }

    /// The fake host's canned `ApplyHeldDeletes` verdict.
    const FAKE_APPLIED: fauna_sync_engine::engine::AppliedHeldDeletes =
        fauna_sync_engine::engine::AppliedHeldDeletes {
            applied: 3,
            remaining_held: 1,
            floor_was_active: true,
        };

    /// The fake host's canned `ShareIngest` cursor.
    #[cfg(feature = "p2p-share")]
    const FAKE_SHARE_CURSOR: i64 = 7;

    impl Default for FakeHydrator {
        fn default() -> Self {
            Self {
                bytes: None,
                seen: Default::default(),
                boot_log: Default::default(),
                clear_fails: false,
                rows: None,
                hydrated: Default::default(),
                placeheld: Default::default(),
                repointed: Default::default(),
                replace_asked: Default::default(),
                replace_answer: Some(false),
                repulls: Default::default(),
                repull_calls: Default::default(),
                interval: None,
                reconnect: Arc::new(watch::channel(0)),
                stop_after_repulls: None,
                converges: Default::default(),
                dehydration_safe: Default::default(),
                write_during_proof: None,
                clean_subtrees: Default::default(),
                converge_records: Default::default(),
                corpus_starts: Default::default(),
                corpus_ticks: Default::default(),
                engine_commands: Default::default(),
                downloads: Default::default(),
                download_gate: None,
                deleted: Default::default(),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl FileHydrator for FakeHydrator {
        async fn download_file_bytes(&self, _relative_path: &str) -> Result<Vec<u8>> {
            self.bytes
                .clone()
                .ok_or_else(|| anyhow!("fake hydrate error"))
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlaceholderLister for FakeHydrator {
        async fn list_placeholder_rows(
            &self,
        ) -> Result<Vec<fauna_sync_engine::enumerate::PlaceholderRow>> {
            self.rows.clone().ok_or_else(|| anyhow!("fake list error"))
        }
    }

    /// The write half of a two-way root. `HydrationHost` **requires** it, so this impl is not
    /// optional scaffolding — it is the type system refusing to let a download-only on-demand
    /// host exist at all (`file-sync.md` § On-Demand Files → *Sync direction*).
    ///
    /// These loop-level tests never touch a real filesystem, so the write calls only need to be
    /// observable, not real. The uploads that must be *real* are asserted where they can be:
    /// `cfapi_live_integration.rs` drives a live cfapi root whose host delegates this half to a
    /// production `SyncEngine`, so the shipped `upload_file` is what runs there.
    #[async_trait::async_trait(?Send)]
    impl LocalWriteHost for FakeHydrator {
        fn is_ignored(&self, _rel: &str) -> bool {
            false
        }

        fn was_recent_download(&self, _rel: &str) -> bool {
            false
        }

        fn was_recent_removal(&self, _rel: &str) -> bool {
            false
        }

        async fn upload_file(
            &self,
            _rel: &str,
        ) -> Result<fauna_sync_engine::engine::UploadOutcome> {
            Ok(Default::default())
        }

        async fn handle_delete(&self, rel: &str) -> Result<()> {
            self.deleted.lock().unwrap().push(rel.to_string());
            Ok(())
        }

        async fn converge(&self, _folder: &str) -> Vec<String> {
            *self.converges.lock().unwrap() += 1;
            self.boot_log.lock().unwrap().push("converge");
            self.converge_records.clone()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl HydrationHost for FakeHydrator {
        // No network in tests: the driving-thread test asserts the command loop,
        // not the (live-nest) prepare path.
        async fn prepare(&self) -> Result<PlaceholderFold> {
            self.boot_log.lock().unwrap().push("prepare");
            Ok(PlaceholderFold::default())
        }

        async fn mark_seen(&self, rels: &[String]) -> Result<()> {
            self.seen.lock().unwrap().extend(rels.iter().cloned());
            Ok(())
        }

        async fn clear_seen_all(&self) -> Result<()> {
            self.boot_log.lock().unwrap().push("clear_seen_all");
            if self.clear_fails {
                return Err(anyhow!("fake clear error"));
            }
            Ok(())
        }

        async fn mark_hydrated(&self, rel: &str, _content_hash: ContentHash) -> Result<()> {
            self.hydrated.lock().unwrap().push(rel.to_string());
            Ok(())
        }

        async fn download_to_path(&self, rel: &str, dest: &Path) -> Result<ContentHash> {
            self.downloads.lock().unwrap().push(rel.to_string());
            if let Some(gate) = &self.download_gate {
                let mut gate = gate.clone();
                gate.wait_for(|open| *open).await?;
            }
            let bytes = self.download_file_bytes(rel).await?;
            std::fs::write(dest, &bytes)?;
            Ok(ContentHash::of_raw(&bytes))
        }

        async fn mark_placeholder(&self, rel: &str) -> Result<()> {
            self.placeheld.lock().unwrap().push(rel.to_string());
            Ok(())
        }

        async fn repoint_placeholder(&self, row: &StaleHydratedRow) -> Result<()> {
            self.repointed
                .lock()
                .unwrap()
                .push(row.relative_path.clone());
            Ok(())
        }

        async fn replace_superseded_own_record(&self, row: &StaleHydratedRow) -> Result<bool> {
            self.replace_asked
                .lock()
                .unwrap()
                .push(row.relative_path.clone());
            self.replace_answer
                .ok_or_else(|| anyhow!("fake: no holder answered"))
        }

        async fn repull(&self) -> Result<PlaceholderFold> {
            let n = {
                let mut calls = self.repull_calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            // Stop the loop once the test has seen the re-pulls it asked for —
            // otherwise a paused-clock test auto-advances and ticks forever.
            if let Some((limit, cancel)) = &self.stop_after_repulls
                && n >= *limit
            {
                cancel.cancel();
            }
            self.repulls
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(PlaceholderFold::default()))
        }

        async fn rescan_interval(&self) -> Duration {
            self.interval
                .unwrap_or(fauna_client_folders::DEFAULT_RESCAN_INTERVAL)
        }

        fn reconnects(&self) -> watch::Receiver<u64> {
            self.reconnect.1.clone()
        }

        async fn subtree_fully_synced(&self, dir_rel: &str) -> bool {
            self.clean_subtrees.contains(dir_rel)
        }

        async fn is_dehydration_safe(&self, rel: &str) -> bool {
            if let Some(usn) = &self.write_during_proof {
                usn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            self.dehydration_safe.contains(rel)
        }

        // No engine in these loop tests: the passes themselves are the production
        // `SyncEngine`'s and are pinned in `fauna-sync-engine`'s own tests; what the
        // loop tests assert is that the loop CALLS them, at startup and per tick.
        async fn converge_corpus_at_start(&self, _folder: &str) {
            *self.corpus_starts.lock().unwrap() += 1;
        }

        async fn refresh_and_converge_corpus(&self, _folder: &str) {
            *self.corpus_ticks.lock().unwrap() += 1;
        }

        async fn answer_engine_command(&self, cmd: EngineCommand) {
            match cmd {
                EngineCommand::ApplyHeldDeletes { reply } => {
                    self.engine_commands
                        .lock()
                        .unwrap()
                        .push("ApplyHeldDeletes");
                    let _ = reply.send(Ok(FAKE_APPLIED));
                }
                #[cfg(feature = "p2p-share")]
                EngineCommand::ShareIngest { reply, .. } => {
                    self.engine_commands.lock().unwrap().push("ShareIngest");
                    let _ = reply.send(Ok((Default::default(), FAKE_SHARE_CURSOR)));
                }
                EngineCommand::FreeSpace { reply, .. } | EngineCommand::SetPinned { reply, .. } => {
                    self.engine_commands.lock().unwrap().push("PinOrFree");
                    let _ = reply.send(Err(anyhow::anyhow!("the fake host keeps no pins")));
                }
                EngineCommand::ResealInheritedVersion { reply, .. } => {
                    self.engine_commands.lock().unwrap().push("ResealInherited");
                    let _ = reply.send(Err(anyhow::anyhow!("the fake host holds no bytes")));
                }
            }
        }
    }

    /// A fake [`PlaceholderInvalidator`] that records the paths it dehydrated and
    /// can be told to refuse a set of paths (simulating cfapi's dirty-file
    /// refusal — the guardrail against clobbering an unsynced local edit).
    #[derive(Default)]
    struct FakeInvalidator {
        dehydrated: std::sync::Mutex<Vec<String>>,
        /// Every `supersede` the platform ACCEPTED, recorded as (abs, size, mtime) — it
        /// refuses exactly as `dehydrate` does.
        superseded: std::sync::Mutex<Vec<(String, u64, i64)>>,
        /// Absolute paths whose dehydrate should fail (a "dirty" local file).
        refuse: std::collections::HashSet<String>,
        /// The root keeps its placeholders off the disk (`placeholders_off_disk`).
        off_disk: bool,
        /// Absolute paths whose dehydrate refuses **until** `set_in_sync` is
        /// called on them — cfapi's exact TRACK_ALL semantics: a tripped
        /// not-in-sync bit holds until the provider asserts otherwise.
        refuse_until_in_sync: std::collections::HashSet<String>,
        /// Every `set_in_sync` call the platform ACCEPTED, recorded.
        in_synced: std::sync::Mutex<Vec<String>>,
        /// The fake volume's current USN — `usn()` reads it, and `set_in_sync`
        /// refuses a USN that is no longer current: cfapi's conditioned
        /// `CfSetInSyncState`, which refuses once the file was written since.
        usn_now: Arc<std::sync::atomic::AtomicI64>,
        /// Every `kick_hydrate` call, recorded.
        kicked: std::sync::Mutex<Vec<String>>,
        /// Canned per-path pin classifications (fake OS attribute reads),
        /// keyed by absolute path.
        pin_actions: std::collections::HashMap<String, PinAction>,
        /// Paths that are NO LONGER cloud files (an editor replaced the
        /// placeholder with an ordinary file). Everything else reports `true`,
        /// matching a healthy root.
        not_cloud_files: std::collections::HashSet<String>,
        /// Every `anchor` call, recorded as (abs, rel).
        anchored: std::sync::Mutex<Vec<(String, String)>>,
        /// Every `convert_dir_in_sync` call, recorded as (abs, rel).
        converted_in_sync: std::sync::Mutex<Vec<(String, String)>>,
        /// Directories the fake platform has already listed (`is_listed_dir`).
        listed_dirs: std::collections::HashSet<std::path::PathBuf>,
        /// Every `create_placeholder` call, recorded as (rel, is_dir) — and made
        /// real on disk (an empty file or directory), as the platform would.
        created: std::sync::Mutex<Vec<(String, bool)>>,
    }

    impl FakeInvalidator {
        /// The fake platform's dirty-file refusal, shared by `dehydrate` and `supersede`:
        /// the path as a string when it may be freed.
        fn unless_refused(&self, abs_path: &Path) -> Result<String> {
            let s = abs_path.to_string_lossy().to_string();
            if self.refuse.contains(&s) {
                return Err(anyhow!("fake dirty-file refusal"));
            }
            if self.refuse_until_in_sync.contains(&s)
                && !self.in_synced.lock().unwrap().contains(&s)
            {
                return Err(anyhow!("fake not-in-sync refusal"));
            }
            Ok(s)
        }
    }

    impl PlaceholderInvalidator for FakeInvalidator {
        fn placeholders_off_disk(&self) -> bool {
            self.off_disk
        }

        fn dehydrate(&self, abs_path: &Path) -> Result<()> {
            let s = self.unless_refused(abs_path)?;
            self.dehydrated.lock().unwrap().push(s);
            Ok(())
        }

        fn supersede(&self, abs_path: &Path, size: u64, mtime: i64) -> Result<()> {
            let s = self.unless_refused(abs_path)?;
            self.superseded.lock().unwrap().push((s, size, mtime));
            Ok(())
        }

        fn pin_action(&self, abs_path: &Path) -> Option<PinAction> {
            self.pin_actions
                .get(&abs_path.to_string_lossy().to_string())
                .copied()
        }

        fn set_in_sync(&self, abs_path: &Path, usn: i64) -> Result<()> {
            let now = self.usn_now.load(std::sync::atomic::Ordering::SeqCst);
            if usn != now {
                return Err(anyhow!(
                    "fake USN mismatch: asserted at {usn}, file now {now}"
                ));
            }
            self.in_synced
                .lock()
                .unwrap()
                .push(abs_path.to_string_lossy().to_string());
            Ok(())
        }

        fn usn(&self, _abs_path: &Path) -> Result<i64> {
            Ok(self.usn_now.load(std::sync::atomic::Ordering::SeqCst))
        }

        fn kick_hydrate(&self, abs_path: &Path) {
            self.kicked
                .lock()
                .unwrap()
                .push(abs_path.to_string_lossy().to_string());
        }

        fn is_cloud_file(&self, abs_path: &Path) -> bool {
            !self
                .not_cloud_files
                .contains(&abs_path.to_string_lossy().to_string())
        }

        fn anchor(&self, abs_path: &Path, rel: &str) -> Result<()> {
            self.anchored
                .lock()
                .unwrap()
                .push((abs_path.to_string_lossy().to_string(), rel.to_string()));
            Ok(())
        }

        fn convert_dir_in_sync(&self, abs_path: &Path, rel: &str) -> Result<()> {
            self.converted_in_sync
                .lock()
                .unwrap()
                .push((abs_path.to_string_lossy().to_string(), rel.to_string()));
            Ok(())
        }

        fn is_listed_dir(&self, abs_dir: &Path) -> bool {
            self.listed_dirs.contains(abs_dir)
        }

        fn create_placeholder(
            &self,
            parent_abs: &Path,
            rel: &str,
            _size: u64,
            _mtime: i64,
            is_dir: bool,
        ) -> Result<()> {
            let leaf = rel.rsplit('/').next().unwrap_or(rel);
            let at = parent_abs.join(leaf);
            if is_dir {
                std::fs::create_dir(&at)?;
            } else {
                std::fs::write(&at, b"")?;
            }
            self.created.lock().unwrap().push((rel.to_string(), is_dir));
            Ok(())
        }
    }

    // ── materialize_created — the remote-create half of two-way ──

    /// The fold's new rows are pushed onto disk ONLY where the platform already
    /// listed the parent — a file into a listed directory, a new directory
    /// (left lazy, so nothing deeper) into a listed one — and never into a
    /// directory still to be listed, nor over a leaf already on disk
    /// (`delete-propagation.md` § *The floor on an on-demand root*, decision (f)).
    #[test]
    fn materialize_created_pushes_only_into_listed_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["sub", "lazy"] {
            std::fs::create_dir(root.join(d)).unwrap();
        }
        std::fs::write(root.join("sub").join("mine.txt"), b"user's own").unwrap();
        let inv = FakeInvalidator {
            listed_dirs: [root.clone(), root.join("sub")].into_iter().collect(),
            ..Default::default()
        };
        let row = |rel: &str| PlaceholderRow {
            rel: rel.to_string(),
            size: 4,
            mtime: 1_700_000_000,
        };
        let (event_tx, mut event_rx) = broadcast::channel(16);

        let n = materialize_created(
            &inv,
            &root,
            &[
                row("top.txt"),
                row("sub/new.txt"),
                row("sub/mine.txt"),
                row("fresh/a/deep.txt"),
                row("fresh/b.txt"),
                row("lazy/new.txt"),
            ],
            Some(&event_tx),
        );

        assert_eq!(
            *inv.created.lock().unwrap(),
            vec![
                ("top.txt".to_string(), false),
                ("sub/new.txt".to_string(), false),
                ("fresh".to_string(), true),
            ],
            "a file into a listed dir; a new dir into a listed dir, then nothing under \
             it (it lists lazily); nothing over the user's own file; nothing into an \
             unlisted dir"
        );
        assert_eq!(n.len(), 2, "two files materialized");
        assert_eq!(
            std::fs::read(root.join("sub").join("mine.txt")).unwrap(),
            b"user's own",
            "a leaf already on disk is never touched"
        );
        let root_s = root.to_string_lossy();
        assert_eq!(
            status_events(&mut event_rx),
            vec![
                (
                    crate::path_map::overlay_abs_path(&root_s, "top.txt"),
                    FileStatus::CloudOnly
                ),
                (
                    crate::path_map::overlay_abs_path(&root_s, "sub/new.txt"),
                    FileStatus::CloudOnly
                ),
            ],
            "each materialized file's overlay is pushed CloudOnly"
        );
    }

    // ── flip_recorded_in_sync — the last leg of a two-way upload ──

    /// A RECORDED rel flips to the platform ✅ by the right road: a cloud file
    /// (placeholder edited in place) needs only the conditioned `set_in_sync`; an
    /// ordinary file (locally created, or an editor's replace-save destroyed the
    /// placeholder) is first re-anchored NOT-in-sync via the identity-carrying
    /// `anchor` — never the identity-less convert (the `FileIdentity` bug class),
    /// never a convert that marks a file in-sync unconditioned, and never a
    /// byte-freeing one (the user's bytes stay local) — then asserted the same way.
    /// Live 2026-07-17: without this leg, a perfectly-synced local edit showed
    /// Explorer's sync-pending arrows forever.
    #[tokio::test]
    async fn flip_recorded_in_sync_picks_the_repair_by_cloud_file_ness() {
        let root = Path::new("C:\\root");
        let plain_abs = root.join("plain.txt").to_string_lossy().to_string();
        let fake = FakeInvalidator {
            not_cloud_files: [plain_abs.clone()].into_iter().collect(),
            ..Default::default()
        };
        let hydrator = FakeHydrator {
            dehydration_safe: ["plain.txt".to_string(), "edited.txt".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };

        flip_recorded_in_sync(
            &hydrator,
            &fake,
            root,
            &["plain.txt".to_string(), "edited.txt".to_string()],
        )
        .await;

        assert_eq!(
            *fake.anchored.lock().unwrap(),
            vec![(plain_abs.clone(), "plain.txt".to_string())],
            "an ordinary file is re-anchored with its rel as FileIdentity"
        );
        assert_eq!(
            *fake.in_synced.lock().unwrap(),
            vec![
                plain_abs,
                root.join("edited.txt").to_string_lossy().to_string()
            ],
            "both files reach the (conditioned) in-sync assertion; the still-cloud \
             one needs nothing else"
        );
        assert!(
            fake.converted_in_sync.lock().unwrap().is_empty(),
            "a FILE is never marked in-sync by a convert (it takes no USN condition)"
        );
        assert!(
            fake.dehydrated.lock().unwrap().is_empty(),
            "the flip never frees bytes"
        );
    }

    /// **Recorded is not enough: the flip vouches only for the recorded
    /// content.** The upload read the file before its record landed; a save that
    /// landed in between is on disk now, and the engine's proof
    /// (`is_dehydration_safe`: disk hash == recorded content) fails for it. The
    /// flip must then leave the not-in-sync bit alone — it is the refusal that
    /// stops the next dehydrate (an unpin in the same batch, *Free up space*, a
    /// remote change) freeing the only copy of the newer save. Red when the flip
    /// is unconditional.
    #[tokio::test]
    async fn the_flip_never_vouches_for_a_save_newer_than_the_recorded_content() {
        let root = Path::new("C:\\root");
        let abs = root.join("notes.txt").to_string_lossy().to_string();
        let fake = FakeInvalidator {
            refuse_until_in_sync: [abs.clone()].into(),
            ..Default::default()
        };
        // dehydration_safe deliberately empty: the disk holds the newer save.
        let hydrator = FakeHydrator::default();

        flip_recorded_in_sync(&hydrator, &fake, root, &["notes.txt".to_string()]).await;

        assert!(
            fake.in_synced.lock().unwrap().is_empty(),
            "the flip asserted in-sync over a save newer than the recorded content"
        );
        assert!(
            fake.dehydrate(Path::new(&abs)).is_err(),
            "the next bare dehydrate must still refuse the newer save"
        );
    }

    /// **A write landing DURING the proof is refused too.** The proof
    /// hashed the recorded content, but a save lands between that hash and the
    /// assertion. The assertion is conditioned on the USN read BEFORE the proof,
    /// so the platform refuses it and the not-in-sync bit stands. Red if the USN
    /// is read after the proof (or not at all).
    #[tokio::test]
    async fn a_save_landing_during_the_proof_refuses_the_in_sync_assertion() {
        let root = Path::new("C:\\root");
        let abs = root.join("notes.txt").to_string_lossy().to_string();
        let fake = FakeInvalidator {
            refuse_until_in_sync: [abs.clone()].into(),
            ..Default::default()
        };
        let hydrator = FakeHydrator {
            dehydration_safe: ["notes.txt".to_string()].into(),
            write_during_proof: Some(fake.usn_now.clone()),
            ..Default::default()
        };

        flip_recorded_in_sync(&hydrator, &fake, root, &["notes.txt".to_string()]).await;

        assert!(
            fake.in_synced.lock().unwrap().is_empty(),
            "a write after the USN read must make the in-sync assertion refuse"
        );
        assert!(
            fake.dehydrate(Path::new(&abs)).is_err(),
            "the next bare dehydrate must still refuse the save that landed mid-proof"
        );
    }

    /// After the file flips, every ancestor directory whose known subtree is
    /// fully synced flips too — deepest-first — while a dirty subtree keeps its
    /// honest arrows. (Live 2026-07-17: folders were never flipped at all, so
    /// the folder holding a green-checked file showed sync-pending forever.)
    #[tokio::test]
    async fn clean_ancestor_dirs_flip_after_their_files() {
        let root = Path::new("C:\\root");
        let hydrator = FakeHydrator {
            clean_subtrees: ["a/b".to_string(), "a".to_string(), "solo".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        // Every dir is an ordinary (never-converted) directory.
        let invalidator = FakeInvalidator {
            not_cloud_files: ["a", "a/b", "solo", "dirty"]
                .into_iter()
                .map(|d| root.join(d).to_string_lossy().to_string())
                .collect(),
            ..Default::default()
        };

        flip_clean_ancestor_dirs(
            &hydrator,
            &invalidator,
            root,
            &[
                "a/b/f.txt".to_string(),
                "solo/g.txt".to_string(),
                "dirty/h.txt".to_string(),
            ],
        )
        .await;

        let flipped: Vec<String> = invalidator
            .converted_in_sync
            .lock()
            .unwrap()
            .iter()
            .map(|(_, rel)| rel.clone())
            .collect();
        assert_eq!(
            flipped,
            vec!["a/b".to_string(), "a".to_string(), "solo".to_string()],
            "clean ancestors flip deepest-first; the dirty subtree keeps its arrows"
        );
        assert!(
            invalidator.dehydrated.lock().unwrap().is_empty()
                && invalidator.anchored.lock().unwrap().is_empty(),
            "a folder flip never frees bytes"
        );
    }

    fn stale_row(rel: &str, fill: u8) -> StaleHydratedRow {
        StaleHydratedRow {
            relative_path: rel.to_string(),
            manifest_hash: fauna_core::data::ContentHash::from_digest_raw([fill; 32]),
            size_bytes: 99,
            content_key_version: None,
            remote_mtime: 900,
            version_num: 1,
        }
    }

    #[derive(Default)]
    struct RecordingPlaceholderSink {
        children: std::sync::Mutex<Vec<DirChild>>,
        calls: std::sync::Mutex<usize>,
        /// What the platform "created": `None` = every child (a root listing, so a
        /// child's name is its rel); `Some` = exactly these (a per-entry failure).
        created: Option<Vec<String>>,
        /// The whole `CfExecute` fails.
        fails: bool,
    }

    impl PlaceholderSink for RecordingPlaceholderSink {
        fn transfer_placeholders(&self, children: &[DirChild]) -> Result<Vec<String>> {
            *self.calls.lock().unwrap() += 1;
            *self.children.lock().unwrap() = children.to_vec();
            if self.fails {
                return Err(anyhow!("fake CfExecute error"));
            }
            Ok(self
                .created
                .clone()
                .unwrap_or_else(|| children.iter().map(|c| c.name.clone()).collect()))
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        data: std::sync::Mutex<Vec<(i64, Vec<u8>)>>,
        failed: std::sync::Mutex<bool>,
    }

    impl TransferSink for RecordingSink {
        fn transfer_data(&self, offset: i64, data: &[u8]) -> Result<()> {
            self.data.lock().unwrap().push((offset, data.to_vec()));
            Ok(())
        }
        fn transfer_failed(&self) -> Result<()> {
            *self.failed.lock().unwrap() = true;
            Ok(())
        }
    }

    // ── serve_fetch chunking / range / failure ──

    #[tokio::test]
    async fn serve_fetch_transfers_full_file_in_chunks() {
        let data: Vec<u8> = (0u8..10).collect();
        let hydrator = FakeHydrator {
            bytes: Some(data.clone()),
            ..Default::default()
        };
        let sink = RecordingSink::default();
        let len = data.len() as i64;
        serve_fetch_chunked(&hydrator, &sink, "f", 0, len, len, 4)
            .await
            .unwrap();
        assert_eq!(
            *sink.data.lock().unwrap(),
            vec![
                (0, vec![0, 1, 2, 3]),
                (4, vec![4, 5, 6, 7]),
                (8, vec![8, 9]),
            ]
        );
        assert!(!*sink.failed.lock().unwrap());
    }

    #[tokio::test]
    async fn serve_fetch_honors_requested_subrange() {
        let data: Vec<u8> = (0u8..10).collect();
        let hydrator = FakeHydrator {
            bytes: Some(data),
            ..Default::default()
        };
        let sink = RecordingSink::default();
        // Request [2, 2+5) = bytes 2..7.
        serve_fetch_chunked(&hydrator, &sink, "f", 2, 5, 10, 4)
            .await
            .unwrap();
        assert_eq!(
            *sink.data.lock().unwrap(),
            vec![(2, vec![2, 3, 4, 5]), (6, vec![6])]
        );
    }

    #[tokio::test]
    async fn serve_fetch_reports_failure_when_hydrate_errors() {
        let hydrator = FakeHydrator::default(); // bytes: None -> hydrate fails
        let sink = RecordingSink::default();
        let res = serve_fetch_chunked(&hydrator, &sink, "f", 0, 10, 10, 4).await;
        assert!(res.is_err(), "hydrate error must propagate");
        assert!(
            *sink.failed.lock().unwrap(),
            "must report failure to the OS"
        );
        assert!(
            sink.data.lock().unwrap().is_empty(),
            "no data transferred on failure"
        );
    }

    #[tokio::test]
    async fn serve_fetch_empty_file_acks_one_empty_transfer() {
        let hydrator = FakeHydrator {
            bytes: Some(vec![]),
            ..Default::default()
        };
        let sink = RecordingSink::default();
        serve_fetch_chunked(&hydrator, &sink, "f", 0, 0, 0, 4)
            .await
            .unwrap();
        assert_eq!(*sink.data.lock().unwrap(), vec![(0, vec![])]);
    }

    /// A placeholder left describing another version's size (a remote edit superseded it
    /// and nothing re-described it) is never served a prefix of the new body, nor a body
    /// shorter than the range the OS waits on: the transfer fails, nothing is sent, and the
    /// error reaches the loop — which then records nothing hydrated.
    #[tokio::test]
    async fn serve_fetch_refuses_a_version_the_placeholder_does_not_describe() {
        for (described, served) in [(6usize, 248usize), (248, 6)] {
            let hydrator = FakeHydrator {
                bytes: Some(vec![b'n'; served]),
                ..Default::default()
            };
            let sink = RecordingSink::default();
            let res = serve_fetch_chunked(
                &hydrator,
                &sink,
                "f",
                0,
                described as i64,
                described as i64,
                4,
            )
            .await;
            assert!(
                res.is_err(),
                "a {served} B version on a {described} B placeholder must be refused"
            );
            assert!(*sink.failed.lock().unwrap(), "the OS is told it failed");
            assert!(
                sink.data.lock().unwrap().is_empty(),
                "not one byte of the wrong version is served"
            );
        }
    }

    // ── progress decorator (step 6) ──

    #[tokio::test]
    async fn progress_sink_emits_cumulative_progress_per_chunk() {
        let (tx, mut rx) = broadcast::channel(16);
        let inner = Arc::new(RecordingSink::default());
        let sink = ProgressTransferSink {
            inner: Box::new(inner.clone()),
            event_tx: tx,
            folder: "f.txt".into(),
            bytes_total: 10,
            bytes_done: AtomicU64::new(0),
        };
        sink.transfer_data(0, &[0u8; 4]).unwrap();
        sink.transfer_data(4, &[0u8; 6]).unwrap();

        let EventKind::SyncProgress(p1) = rx.try_recv().unwrap().event else {
            panic!("expected SyncProgress");
        };
        let EventKind::SyncProgress(p2) = rx.try_recv().unwrap().event else {
            panic!("expected SyncProgress");
        };
        assert_eq!((p1.bytes_done, p1.files_done), (4, 0), "first chunk: 4/10");
        assert_eq!(
            (p2.bytes_done, p2.files_done),
            (10, 1),
            "second chunk: done"
        );
        assert_eq!(p2.folder, "f.txt");
        // The wrapped sink still received both chunks.
        assert_eq!(inner.data.lock().unwrap().len(), 2);
    }

    // ── apply_stale_hydrated: invalidate a superseded hydrated copy ──

    #[tokio::test]
    async fn apply_stale_hydrated_supersedes_then_repoints_and_emits_cloudonly() {
        let hydrator = FakeHydrator::default();
        let repointed = hydrator.repointed.clone();
        let invalidator = FakeInvalidator::default();
        let (tx, mut rx) = broadcast::channel(16);
        let root = Path::new("C:\\sync");

        let rows = vec![stale_row("a.txt", 0x02)];
        let n = apply_stale_hydrated(&hydrator, &invalidator, root, &rows, Some(&tx)).await;

        assert_eq!(n, 1);
        // Freed the bytes, leaving the placeholder describing the NEW version (its size
        // and mtime — a bare dehydrate kept the old size, and the next open asked for
        // exactly that many bytes of the new body)...
        assert_eq!(
            *invalidator.superseded.lock().unwrap(),
            vec![("C:\\sync\\a.txt".to_string(), 99, 900)]
        );
        assert!(
            invalidator.dehydrated.lock().unwrap().is_empty(),
            "a superseded copy is never freed under the old version's description"
        );
        // ...then re-pointed the row...
        assert_eq!(*repointed.lock().unwrap(), vec!["a.txt".to_string()]);
        // ...then flipped the overlay to CloudOnly.
        let EventKind::FileStatusChanged { path, status } = rx.try_recv().unwrap().event else {
            panic!("expected FileStatusChanged");
        };
        assert_eq!(path, "C:\\sync\\a.txt");
        assert_eq!(status, FileStatus::CloudOnly);
    }

    #[tokio::test]
    async fn apply_stale_hydrated_skips_repoint_when_the_file_is_dirty() {
        // A locally edited hydrated file: cfapi refuses to dehydrate it. We must
        // NOT re-point (that would strand the edit under a "no local bytes" row),
        // NOT emit, and leave the row Synced for a later conflict pass.
        let hydrator = FakeHydrator::default();
        let repointed = hydrator.repointed.clone();
        let invalidator = FakeInvalidator {
            refuse: ["C:\\sync\\a.txt".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let (tx, mut rx) = broadcast::channel(16);
        let root = Path::new("C:\\sync");

        let rows = vec![stale_row("a.txt", 0x02)];
        let n = apply_stale_hydrated(&hydrator, &invalidator, root, &rows, Some(&tx)).await;

        assert_eq!(n, 0, "a dirty file is not invalidated");
        assert!(
            repointed.lock().unwrap().is_empty(),
            "the row must stay Synced — the local edit survives"
        );
        assert!(
            invalidator.superseded.lock().unwrap().is_empty()
                && invalidator.dehydrated.lock().unwrap().is_empty(),
            "the refusing platform recorded nothing"
        );
        assert!(
            rx.try_recv().is_err(),
            "no overlay event for a skipped file"
        );
    }

    #[tokio::test]
    async fn apply_stale_hydrated_processes_each_row_independently() {
        // One dirty file in a batch must not block the others.
        let hydrator = FakeHydrator::default();
        let repointed = hydrator.repointed.clone();
        let invalidator = FakeInvalidator {
            refuse: ["C:\\sync\\b.txt".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let root = Path::new("C:\\sync");

        let rows = vec![
            stale_row("a.txt", 0x0a),
            stale_row("b.txt", 0x0b), // dirty → skipped
            stale_row("c.txt", 0x0c),
        ];
        let n = apply_stale_hydrated(&hydrator, &invalidator, root, &rows, None).await;

        assert_eq!(
            n, 2,
            "the two clean files were invalidated, the dirty one skipped"
        );
        assert_eq!(
            *repointed.lock().unwrap(),
            vec!["a.txt".to_string(), "c.txt".to_string()]
        );
    }

    /// An off-disk root whose gate refuses every free (the fake host's
    /// `dehydrate_off_disk` is the trait's refusing default).
    fn off_disk_root() -> FakeInvalidator {
        FakeInvalidator {
            off_disk: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_refused_free_on_an_off_disk_root_is_followed_by_replacing_the_body() {
        let hydrator = FakeHydrator {
            replace_answer: Some(true),
            ..Default::default()
        };
        let invalidator = off_disk_root();
        let (tx, mut rx) = broadcast::channel(16);
        let root = Path::new("/sync");

        let rows = vec![stale_row("a.txt", 0x02)];
        let n = apply_stale_hydrated(&hydrator, &invalidator, root, &rows, Some(&tx)).await;

        assert_eq!(n, 1, "the stale copy is stale no longer");
        assert_eq!(*hydrator.replace_asked.lock().unwrap(), vec!["a.txt"]);
        assert!(
            hydrator.repointed.lock().unwrap().is_empty()
                && invalidator.dehydrated.lock().unwrap().is_empty()
                && invalidator.superseded.lock().unwrap().is_empty(),
            "nothing is freed and the row is not made a placeholder"
        );
        let EventKind::FileStatusChanged { status, .. } = rx.try_recv().unwrap().event else {
            panic!("expected FileStatusChanged");
        };
        assert_eq!(status, FileStatus::Synced, "the file stays on this device");
    }

    #[tokio::test]
    async fn a_replace_no_holder_answers_leaves_the_row_for_the_next_pull() {
        for answer in [None, Some(false)] {
            let hydrator = FakeHydrator {
                replace_answer: answer,
                ..Default::default()
            };
            let invalidator = off_disk_root();
            let (tx, mut rx) = broadcast::channel(16);
            let root = Path::new("/sync");

            let rows = vec![stale_row("a.txt", 0x02), stale_row("b.txt", 0x03)];
            let n = apply_stale_hydrated(&hydrator, &invalidator, root, &rows, Some(&tx)).await;

            assert_eq!(n, 0);
            assert_eq!(
                hydrator.replace_asked.lock().unwrap().len(),
                2,
                "one row's wait does not stop the next"
            );
            assert!(hydrator.repointed.lock().unwrap().is_empty());
            assert!(rx.try_recv().is_err(), "no status change while it waits");
        }
    }

    #[tokio::test]
    async fn a_cfapi_refusal_is_not_followed_by_a_replace() {
        // The cfapi binding of the rule is unbuilt: the OS's refusal stays a skip.
        let hydrator = FakeHydrator {
            replace_answer: Some(true),
            ..Default::default()
        };
        let invalidator = FakeInvalidator {
            refuse: ["C:\\sync\\a.txt".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let root = Path::new("C:\\sync");
        let rows = vec![stale_row("a.txt", 0x02)];
        let n = apply_stale_hydrated(&hydrator, &invalidator, root, &rows, None).await;
        assert_eq!(n, 0);
        assert!(hydrator.replace_asked.lock().unwrap().is_empty());
    }

    // ── redescribe_repointed: a re-pointed placeholder on the disk follows its row ──

    #[test]
    fn a_repointed_placeholder_on_the_disk_is_redescribed_as_the_new_version() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("listed.txt"), b"").unwrap();
        std::fs::write(root.join("edited.txt"), b"").unwrap();
        let at = |rel: &str| root.join(rel).to_string_lossy().to_string();
        let invalidator = FakeInvalidator {
            refuse: [at("edited.txt")].into_iter().collect(),
            ..Default::default()
        };
        let rows = [
            prow("listed.txt", 248, 1_700_000_100),
            prow("edited.txt", 7, 1_700_000_100), // a local edit since: refused
            prow("unlisted.txt", 9, 1_700_000_100), // not on the disk: its listing carries it
        ];

        let n = redescribe_repointed(&invalidator, root, &rows);

        assert_eq!(n, 1);
        assert_eq!(
            *invalidator.superseded.lock().unwrap(),
            vec![(at("listed.txt"), 248, 1_700_000_100)],
            "only the placeholder on the disk is re-described, with the new size and mtime"
        );
    }

    #[test]
    fn an_off_disk_root_has_no_placeholder_to_redescribe() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"hydrated bytes").unwrap();
        let invalidator = off_disk_root();

        let n = redescribe_repointed(&invalidator, tmp.path(), &[prow("a.txt", 3, 1)]);

        assert_eq!(n, 0);
        assert!(
            invalidator.superseded.lock().unwrap().is_empty(),
            "an off-disk root's placeholders are rows: nothing on its disk is touched"
        );
    }

    // ── serve_populate directory listing ──

    fn prow(rel: &str, size: u64, mtime: i64) -> fauna_sync_engine::enumerate::PlaceholderRow {
        fauna_sync_engine::enumerate::PlaceholderRow {
            rel: rel.to_string(),
            size,
            mtime,
        }
    }

    #[tokio::test]
    async fn serve_populate_lists_immediate_children() {
        let hydrator = FakeHydrator {
            rows: Some(vec![prow("a.txt", 10, 100), prow("sub/b.txt", 20, 200)]),
            ..Default::default()
        };
        let sink = RecordingPlaceholderSink::default();
        serve_populate(&hydrator, &sink, "").await.unwrap();
        let children = sink.children.lock().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].name, "sub");
        assert!(children[0].is_dir);
        assert_eq!(children[1].name, "a.txt");
        assert!(!children[1].is_dir);
    }

    /// Decision (a): the seen mark follows what the platform PUT on the disk — AFTER
    /// the transfer succeeded, and only for the entries it reports created. A
    /// per-entry failure is not marked; a failed transfer marks nothing (a crash or
    /// an error loses evidence, never invents it).
    #[tokio::test]
    async fn serve_populate_marks_seen_only_what_the_platform_placed() {
        let rows = Some(vec![prow("a.txt", 1, 1), prow("b.txt", 1, 1)]);
        let hydrator = FakeHydrator {
            rows: rows.clone(),
            ..Default::default()
        };
        let sink = RecordingPlaceholderSink {
            created: Some(vec!["b.txt".into()]),
            ..Default::default()
        };
        serve_populate(&hydrator, &sink, "").await.unwrap();
        assert_eq!(*hydrator.seen.lock().unwrap(), vec!["b.txt".to_string()]);

        let hydrator = FakeHydrator {
            rows,
            ..Default::default()
        };
        let sink = RecordingPlaceholderSink {
            fails: true,
            ..Default::default()
        };
        assert!(serve_populate(&hydrator, &sink, "").await.is_err());
        assert!(
            hydrator.seen.lock().unwrap().is_empty(),
            "a failed transfer marks nothing"
        );
    }

    #[tokio::test]
    async fn serve_populate_completes_empty_on_list_error() {
        let hydrator = FakeHydrator::default(); // rows: None -> listing fails
        let sink = RecordingPlaceholderSink::default();
        let res = serve_populate(&hydrator, &sink, "").await;
        assert!(res.is_err(), "list error must propagate");
        assert_eq!(
            *sink.calls.lock().unwrap(),
            1,
            "must still complete the op (empty) so the browse does not hang"
        );
        assert!(sink.children.lock().unwrap().is_empty());
    }

    // ── serve loop (run_hydration_loop) ──

    /// A single-threaded runtime to drive `run_hydration_loop` — the loop owns a
    /// `!Send` engine, exactly as on the `EngineHost`'s driving thread.
    fn loop_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Run one on-demand boot over a pre-cancelled token — every startup step runs,
    /// then the serve loop's first `select!` ends it — and return the boot's order
    /// (`FakeHydrator::boot_log`, with `connect` pushed by the root's connect).
    fn boot_order(hydrator: FakeHydrator, fresh_registration: bool) -> Vec<&'static str> {
        let log = hydrator.boot_log.clone();
        let connect_log = log.clone();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (_tx, rx) = mpsc::unbounded_channel();
        loop_runtime().block_on(serve_hydration_root(
            hydrator,
            FakeInvalidator::default(),
            RootBoot {
                fresh_registration,
                connect: move || connect_log.lock().unwrap().push("connect"),
            },
            rx,
            None,
            PathBuf::from("C:\\sync"),
            "docs".into(),
            cancel,
            None,
            None,
            None,
        ));
        log.lock().unwrap().clone()
    }

    /// Decision (c) — **evidence before re-population**: the boot sweep (the startup
    /// `converge`, which runs `reconcile`) sees the tree as the downtime left it,
    /// BEFORE the root is connected. Connecting first re-arms the root's
    /// population, and the first browse lists every tracked placeholder back —
    /// silently reverting a delete the user made while nothing ran.
    #[test]
    fn the_boot_sweep_runs_before_the_root_connects() {
        assert_eq!(
            boot_order(FakeHydrator::default(), false),
            vec!["prepare", "converge", "connect"],
            "a kept registration keeps its marks and sweeps before connecting"
        );
    }

    /// Decision (e): a registration that did not survive the downtime had its
    /// placeholders removed by the OS, so every mark is cleared BEFORE the sweep —
    /// the product's own removal is never a user's delete.
    #[test]
    fn a_fresh_registration_clears_every_mark_before_the_sweep() {
        assert_eq!(
            boot_order(FakeHydrator::default(), true),
            vec!["prepare", "clear_seen_all", "converge", "connect"]
        );
    }

    /// …and a clear that fails fails CLOSED: marks that still claim placeholders the
    /// OS removed must never meet a sweep, so the root is neither swept nor served.
    #[test]
    fn a_failed_clear_neither_sweeps_nor_connects() {
        assert_eq!(
            boot_order(
                FakeHydrator {
                    clear_fails: true,
                    ..Default::default()
                },
                true
            ),
            vec!["prepare", "clear_seen_all"]
        );
    }

    #[test]
    fn loop_serves_fetch_then_stops() {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = Arc::new(RecordingSink::default());
        tx.send(HydrationCommand::Fetch {
            rel: "f".into(),
            offset: 0,
            length: 3,
            file_size: 3,
            sink: Box::new(sink.clone()),
        })
        .unwrap();
        drop(tx); // close the channel so the loop ends after serving the queued command
        loop_runtime().block_on(run_hydration_loop(
            FakeHydrator {
                bytes: Some(vec![10, 20, 30]),
                ..Default::default()
            },
            FakeInvalidator::default(),
            rx,
            None,
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            CancellationToken::new(),
            None,
            None,
        ));
        let got: Vec<u8> = sink
            .data
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(_, d)| d.clone())
            .collect();
        assert_eq!(got, vec![10, 20, 30]);
    }

    /// Drive `run_hydration_loop` over `host` with the engine's two per-folder
    /// channels wired, until `probe` returns — then cancel it. `probe` gets a
    /// 5 s budget, so a loop that never answers fails the test rather than
    /// hanging it.
    fn drive_loop_with_inbox<T>(
        host: FakeHydrator,
        wake_rx: Option<mpsc::Receiver<()>>,
        engine_cmd_rx: Option<mpsc::Receiver<EngineCommand>>,
        probe: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let (_tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        loop_runtime().block_on(async move {
            let probed = async {
                let got = tokio::time::timeout(Duration::from_secs(5), probe)
                    .await
                    .ok();
                stop.cancel();
                got
            };
            let (got, ()) = tokio::join!(
                probed,
                run_hydration_loop(
                    host,
                    FakeInvalidator::default(),
                    rx,
                    None,
                    PathBuf::from(r"C:\Root"),
                    "docs".to_string(),
                    cancel,
                    wake_rx,
                    engine_cmd_rx,
                ),
            );
            got
        })
    }

    /// The mass-delete floor's confirm reaches an ON-DEMAND root: an
    /// `ApplyHeldDeletes` sent on the engine's command channel is answered by
    /// the host and its reply resolves — the arm the resident loop has always
    /// had, which this root lacked, so a hold it surfaced could not be
    /// confirmed (`delete-propagation.md` § *The floor on an on-demand root*,
    /// point 4).
    #[test]
    fn loop_answers_apply_held_deletes_on_its_command_channel() {
        let host = FakeHydrator::default();
        let seen = host.engine_commands.clone();
        let (cmd_tx, cmd_rx) = mpsc::channel(2);
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        assert!(
            cmd_tx
                .try_send(EngineCommand::ApplyHeldDeletes { reply })
                .is_ok()
        );
        let verdict = drive_loop_with_inbox(host, None, Some(cmd_rx), reply_rx)
            .expect("the on-demand loop must answer ApplyHeldDeletes, not leave it queued")
            .expect("the reply is sent, not dropped")
            .expect("the host's verdict is relayed");
        assert_eq!(
            (
                verdict.applied,
                verdict.remaining_held,
                verdict.floor_was_active
            ),
            (
                FAKE_APPLIED.applied,
                FAKE_APPLIED.remaining_held,
                FAKE_APPLIED.floor_was_active
            ),
        );
        assert_eq!(*seen.lock().unwrap(), vec!["ApplyHeldDeletes"]);
    }

    /// Registering the command sender for an on-demand root also routes the
    /// share plane's ingest pages to it (`pipe_server::handle_share_ingest`
    /// resolves through the same registry), so the loop answers `ShareIngest`
    /// through the host too — the resident arm lifted whole.
    #[cfg(feature = "p2p-share")]
    #[test]
    fn loop_answers_share_ingest_on_its_command_channel() {
        let host = FakeHydrator::default();
        let seen = host.engine_commands.clone();
        let (cmd_tx, cmd_rx) = mpsc::channel(2);
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        assert!(
            cmd_tx
                .try_send(EngineCommand::ShareIngest {
                    proven_actor_hex: "ab".into(),
                    rows: Vec::new(),
                    spool_dir: PathBuf::from(r"C:\Spool"),
                    reply,
                })
                .is_ok()
        );
        let (_report, cursor) = drive_loop_with_inbox(host, None, Some(cmd_rx), reply_rx)
            .expect("the on-demand loop must answer ShareIngest, not leave it queued")
            .expect("the reply is sent, not dropped")
            .expect("the host's ingest outcome is relayed");
        assert_eq!(cursor, FAKE_SHARE_CURSOR);
        assert_eq!(*seen.lock().unwrap(), vec!["ShareIngest"]);
    }

    /// The remote-change nudge (`PullFolderNow` → the engine's wake channel)
    /// reaches an ON-DEMAND root: a `()` on the wake channel re-pulls the nest
    /// now, off the rescan cadence (the default 300 s tick never fires here).
    #[test]
    fn loop_repulls_on_a_remote_change_nudge() {
        let host = FakeHydrator::default();
        let calls = host.repull_calls.clone();
        let (wake_tx, wake_rx) = mpsc::channel(1);
        assert!(wake_tx.try_send(()).is_ok());
        let polled = calls.clone();
        let repulled = drive_loop_with_inbox(host, Some(wake_rx), None, async move {
            while *polled.lock().unwrap() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        assert!(
            repulled.is_some(),
            "a nudge must re-pull the nest now, not wait out the rescan tick"
        );
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn loop_serves_populate_then_stops() {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = Arc::new(RecordingPlaceholderSink::default());
        tx.send(HydrationCommand::Populate {
            parent_rel: "".into(),
            sink: Box::new(sink.clone()),
        })
        .unwrap();
        drop(tx); // close the channel so the loop ends after serving the queued command
        loop_runtime().block_on(run_hydration_loop(
            FakeHydrator {
                rows: Some(vec![prow("top.txt", 5, 50), prow("dir/inner.txt", 6, 60)]),
                ..Default::default()
            },
            FakeInvalidator::default(),
            rx,
            None,
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            CancellationToken::new(),
            None,
            None,
        ));
        let names: Vec<String> = sink
            .children
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["dir".to_string(), "top.txt".to_string()]);
    }

    /// Cancellation ends the loop even with the command channel still open (no
    /// `Stop` sent, `tx` held) — the path the `EngineHost` uses to stop one
    /// engine. Proves the loop returns via `cancel`, not a channel close.
    #[test]
    fn loop_breaks_on_cancel_with_channel_open() {
        let (tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let cancel = CancellationToken::new();
        cancel.cancel(); // pre-cancelled: the biased select takes the cancel arm
        loop_runtime().block_on(run_hydration_loop(
            FakeHydrator::default(),
            FakeInvalidator::default(),
            rx,
            None,
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            cancel,
            None,
            None,
        ));
        drop(tx); // tx was held open the whole time — only the cancel ended it
    }

    // ── overlay status emission on hydrate (Task C) ──

    /// A successful fetch marks the file `Synced` in the host's state and pushes
    /// a `FileStatusChanged { <absolute path>, Synced }` so Explorer overlays
    /// update live without re-querying. The absolute path joins the sync root and
    /// the fetched rel — the key the shell extension's overlay cache uses.
    #[test]
    fn loop_marks_synced_and_emits_file_status_on_fetch_success() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let hydrated = Arc::new(std::sync::Mutex::new(Vec::new()));
        tx.send(HydrationCommand::Fetch {
            rel: "sub/f.txt".into(),
            offset: 0,
            length: 3,
            file_size: 3,
            sink: Box::new(RecordingSink::default()),
        })
        .unwrap();
        drop(tx); // close the channel so the loop ends after serving the command
        loop_runtime().block_on(run_hydration_loop(
            FakeHydrator {
                bytes: Some(vec![1, 2, 3]),
                hydrated: hydrated.clone(),
                ..Default::default()
            },
            FakeInvalidator::default(),
            rx,
            Some(evt_tx),
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            CancellationToken::new(),
            None,
            None,
        ));

        assert_eq!(
            *hydrated.lock().unwrap(),
            vec!["sub/f.txt".to_string()],
            "a served file must be marked hydrated (Synced)"
        );

        // The status event may be interleaved with per-chunk SyncProgress events;
        // find the FileStatusChanged among them.
        let mut status = None;
        while let Ok(ev) = evt_rx.try_recv() {
            if let EventKind::FileStatusChanged { path, status: s } = ev.event {
                status = Some((path, s));
            }
        }
        assert_eq!(
            status,
            Some((r"C:\Root\sub\f.txt".to_string(), FileStatus::Synced)),
            "a FileStatusChanged(Synced) for the absolute path must be broadcast"
        );
    }

    // ── the FUSE root's commands: Materialize and Unlink ──

    /// A `Materialize` command's answer, as the waiting FUSE request reads it.
    fn loop_reply() -> (
        LoopReply,
        std::sync::mpsc::Receiver<std::result::Result<(), String>>,
    ) {
        std::sync::mpsc::sync_channel(1)
    }

    /// Run the loop over `cmds` (then a closed channel) on a real directory, and
    /// return the status events it pushed.
    fn serve_commands(
        hydrator: FakeHydrator,
        root: &Path,
        cmds: Vec<HydrationCommand>,
    ) -> Vec<(String, FileStatus)> {
        let (tx, rx) = mpsc::unbounded_channel();
        for cmd in cmds {
            tx.send(cmd).unwrap();
        }
        drop(tx);
        let (evt_tx, mut evt_rx) = broadcast::channel(64);
        let rt = loop_runtime();
        rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(20),
                run_hydration_loop(
                    hydrator,
                    FakeInvalidator::default(),
                    rx,
                    Some(evt_tx),
                    root.to_path_buf(),
                    "docs".to_string(),
                    CancellationToken::new(),
                    None,
                    None,
                ),
            )
            .await
            .expect("the loop served its commands and ended")
        });
        let mut statuses = Vec::new();
        while let Ok(ev) = evt_rx.try_recv() {
            if let EventKind::FileStatusChanged { path, status } = ev.event {
                statuses.push((path, status));
            }
        }
        statuses
    }

    /// Hydrate-on-open's loop half: the file lands whole in the root's own
    /// directory — under a directory that existed only as rows, which becomes
    /// real — the row is marked hydrated with the served bytes, a `Synced`
    /// status is pushed for the path the user sees, and only then is the waiting
    /// open answered.
    #[test]
    fn loop_materializes_then_marks_synced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hydrated = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (reply, answer) = loop_reply();
        let statuses = serve_commands(
            FakeHydrator {
                bytes: Some(b"served bytes".to_vec()),
                hydrated: hydrated.clone(),
                ..Default::default()
            },
            &root,
            vec![HydrationCommand::Materialize {
                rel: "sub/f.txt".into(),
                reply,
            }],
        );

        assert_eq!(answer.try_recv().unwrap(), Ok(()));
        assert_eq!(
            std::fs::read(root.join("sub/f.txt")).unwrap(),
            b"served bytes"
        );
        assert_eq!(*hydrated.lock().unwrap(), vec!["sub/f.txt".to_string()]);
        assert_eq!(
            statuses,
            vec![(
                crate::path_map::overlay_abs_path(&root.to_string_lossy(), "sub/f.txt"),
                FileStatus::Synced
            )]
        );
        let leftovers: Vec<_> = std::fs::read_dir(root.join("sub"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("f.txt")]);
    }

    /// A failed download answers the open with an error and leaves nothing
    /// behind: no file, no temp file, no `Synced` mark, no status — the row stays
    /// the placeholder it was.
    #[test]
    fn a_failed_materialize_replies_err_and_leaves_the_row_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hydrated = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (reply, answer) = loop_reply();
        let statuses = serve_commands(
            FakeHydrator {
                bytes: None,
                hydrated: hydrated.clone(),
                ..Default::default()
            },
            &root,
            vec![HydrationCommand::Materialize {
                rel: "f.txt".into(),
                reply,
            }],
        );

        assert!(answer.try_recv().unwrap().is_err());
        assert!(hydrated.lock().unwrap().is_empty());
        assert!(statuses.is_empty());
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            0,
            "neither the file nor its temp file may survive a failed download"
        );
    }

    /// A file already on the disk is served as it is: nothing is downloaded over
    /// it, and the open is answered at once.
    #[test]
    fn a_materialize_of_a_file_already_on_the_disk_downloads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("f.txt"), b"the user's own bytes").unwrap();
        let downloads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (reply, answer) = loop_reply();
        let statuses = serve_commands(
            FakeHydrator {
                bytes: Some(b"served bytes".to_vec()),
                downloads: downloads.clone(),
                ..Default::default()
            },
            &root,
            vec![HydrationCommand::Materialize {
                rel: "f.txt".into(),
                reply,
            }],
        );

        assert_eq!(answer.try_recv().unwrap(), Ok(()));
        assert!(downloads.lock().unwrap().is_empty());
        assert!(statuses.is_empty());
        assert_eq!(
            std::fs::read(root.join("f.txt")).unwrap(),
            b"the user's own bytes"
        );
    }

    /// A placeholder sink that opens the download gate when a listing is served.
    struct GateOpeningSink(watch::Sender<bool>);

    impl PlaceholderSink for GateOpeningSink {
        fn transfer_placeholders(&self, _children: &[DirChild]) -> Result<Vec<String>> {
            self.0.send_replace(true);
            Ok(Vec::new())
        }
    }

    /// While a hydration is in flight the loop keeps serving: a listing is
    /// answered (it is what releases the held downloads here — had the loop
    /// waited on the download first, this test would never end), a second open
    /// of the same file joins the running download instead of starting another,
    /// and an open of a different file downloads alongside it.
    #[test]
    fn hydrations_overlap_coalesce_and_never_stall_a_listing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (gate_tx, gate_rx) = watch::channel(false);
        let downloads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hydrated = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (first, first_answer) = loop_reply();
        let (joined, joined_answer) = loop_reply();
        let (other, other_answer) = loop_reply();
        serve_commands(
            FakeHydrator {
                bytes: Some(b"served bytes".to_vec()),
                rows: Some(Vec::new()),
                downloads: downloads.clone(),
                hydrated: hydrated.clone(),
                download_gate: Some(gate_rx),
                ..Default::default()
            },
            &root,
            vec![
                HydrationCommand::Materialize {
                    rel: "a.txt".into(),
                    reply: first,
                },
                HydrationCommand::Materialize {
                    rel: "a.txt".into(),
                    reply: joined,
                },
                HydrationCommand::Materialize {
                    rel: "b.txt".into(),
                    reply: other,
                },
                HydrationCommand::Populate {
                    parent_rel: String::new(),
                    sink: Box::new(GateOpeningSink(gate_tx)),
                },
            ],
        );

        assert_eq!(
            *downloads.lock().unwrap(),
            vec!["a.txt".to_string(), "b.txt".to_string()],
            "one download per file, however many opens wait on it"
        );
        for answer in [first_answer, joined_answer, other_answer] {
            assert_eq!(answer.try_recv().unwrap(), Ok(()));
        }
        let mut marked = hydrated.lock().unwrap().clone();
        marked.sort();
        assert_eq!(marked, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    /// A placeholder unlinked through the mount is deleted the one way a
    /// placeholder is deleted on a FUSE root: the loop hands it to the host's
    /// `handle_delete` (record-first), then answers the waiting unlink.
    #[test]
    fn an_unlinked_placeholder_is_handed_to_handle_delete() {
        let dir = tempfile::tempdir().unwrap();
        let deleted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (reply, answer) = loop_reply();
        serve_commands(
            FakeHydrator {
                deleted: deleted.clone(),
                ..Default::default()
            },
            dir.path(),
            vec![HydrationCommand::Unlink {
                rel: "sub/gone.txt".into(),
                reply,
            }],
        );

        assert_eq!(answer.try_recv().unwrap(), Ok(()));
        assert_eq!(*deleted.lock().unwrap(), vec!["sub/gone.txt".to_string()]);
    }

    // ── overlay status emission on OS-dehydrate (Free-up-space observe) ──

    /// An **OS-initiated** dehydrate (Explorer "Free up space" / Storage Sense),
    /// delivered as `HydrationCommand::Dehydrate`, marks the file `Placeholder` in
    /// the host's state and pushes `FileStatusChanged{ <absolute path>, CloudOnly }`
    /// so Explorer's overlay flips Synced → CloudOnly live without re-querying. The
    /// exact symmetric inverse of the fetch-success arm above — same absolute-path
    /// key, opposite state. Without this the row stays a dishonest `Synced` while
    /// the bytes are gone.
    #[test]
    fn loop_marks_placeholder_and_emits_cloud_only_on_dehydrate() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let placeheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        tx.send(HydrationCommand::Dehydrate {
            rel: "sub/f.txt".into(),
        })
        .unwrap();
        drop(tx); // close the channel so the loop ends after serving the command
        loop_runtime().block_on(run_hydration_loop(
            FakeHydrator {
                placeheld: placeheld.clone(),
                ..Default::default()
            },
            FakeInvalidator::default(),
            rx,
            Some(evt_tx),
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            CancellationToken::new(),
            None,
            None,
        ));

        assert_eq!(
            *placeheld.lock().unwrap(),
            vec!["sub/f.txt".to_string()],
            "an OS-dehydrated file must be marked Placeholder (the honest row)"
        );

        let mut status = None;
        while let Ok(ev) = evt_rx.try_recv() {
            if let EventKind::FileStatusChanged { path, status: s } = ev.event {
                status = Some((path, s));
            }
        }
        assert_eq!(
            status,
            Some((r"C:\Root\sub\f.txt".to_string(), FileStatus::CloudOnly)),
            "a FileStatusChanged(CloudOnly) for the absolute path must be broadcast"
        );
    }

    // ── the pin-state reaction (Explorer "Free up space" / "Always keep on this device") ──

    /// A root + one real file for the sweep to walk, plus the invalidator faking
    /// the OS attribute read for it. Returns (tempdir, root, abs-path-string).
    fn pin_root(file: &str) -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(file), b"bytes").unwrap();
        let abs = root.join(file).to_string_lossy().to_string();
        (tmp, root, abs)
    }

    /// Run the loop over `root` until the (pre-dropped) command channel ends it —
    /// long enough for the startup sequence (prepare → converge → pin sweep) to run.
    fn run_startup(
        hydrator: FakeHydrator,
        invalidator: &FakeInvalidator,
        root: PathBuf,
        evt_tx: broadcast::Sender<Event>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        drop(tx);
        loop_runtime().block_on(run_hydration_loop(
            hydrator,
            invalidator,
            rx,
            Some(evt_tx),
            root,
            "docs".to_string(),
            CancellationToken::new(),
            None,
            None,
        ));
    }

    /// **"Free up space" actually frees space.** An unpinned hydrated file found by
    /// the startup sweep is dehydrated, its row flipped `Placeholder`, and a
    /// `CloudOnly` event pushed — the byte work Explorer's verb delegates to the
    /// provider (`file-sync.md` § Per-file sync-status display, answered question
    /// 2: the verb is a pure pin-state write; nothing else does the byte work).
    #[test]
    fn the_pin_sweep_dehydrates_an_unpinned_hydrated_file() {
        let (_tmp, root, abs) = pin_root("f.txt");
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let placeheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invalidator = FakeInvalidator {
            pin_actions: [(abs.clone(), PinAction::Dehydrate)].into(),
            ..Default::default()
        };

        run_startup(
            FakeHydrator {
                placeheld: placeheld.clone(),
                ..Default::default()
            },
            &invalidator,
            root.clone(),
            evt_tx,
        );

        assert_eq!(*invalidator.dehydrated.lock().unwrap(), vec![abs]);
        assert_eq!(*placeheld.lock().unwrap(), vec!["f.txt".to_string()]);
        // The cfapi sweep builds event paths with the platform-native separator; on
        // Windows (the only platform that runs cfapi pin-sweep in production) that is
        // `\`, matching `root.join`. This test drives the neutral sweep logic via fakes
        // on any host, so normalize separators to keep the assertion cross-platform
        // (the agent crate is now built + tested on macOS/Linux too — A1b).
        let got: Vec<(String, FileStatus)> = status_events(&mut evt_rx)
            .into_iter()
            .map(|(p, s)| (p.replace('\\', "/"), s))
            .collect();
        assert_eq!(
            got,
            vec![(
                root.join("f.txt").to_string_lossy().replace('\\', "/"),
                FileStatus::CloudOnly
            )],
            "the overlay must flip CloudOnly live, matching the GetFileStatus re-query"
        );
        assert!(
            invalidator.in_synced.lock().unwrap().is_empty(),
            "a clean dehydrate must never touch the platform's in-sync state — \
             the bare call preserves cfapi's own edit-protection razor"
        );
    }

    /// **"Always keep on this device" after downtime.** A pinned placeholder found
    /// by the sweep gets a hydration kick — and nothing else: completion (the row
    /// flip + `Synced` event) is owned by the fetch path the kick triggers, so a
    /// kick alone must not mark or emit anything.
    #[test]
    fn the_pin_sweep_kicks_hydration_for_a_pinned_placeholder() {
        let (_tmp, root, abs) = pin_root("p.txt");
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let placeheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hydrated = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invalidator = FakeInvalidator {
            pin_actions: [(abs.clone(), PinAction::Hydrate)].into(),
            ..Default::default()
        };

        run_startup(
            FakeHydrator {
                placeheld: placeheld.clone(),
                hydrated: hydrated.clone(),
                ..Default::default()
            },
            &invalidator,
            root,
            evt_tx,
        );

        assert_eq!(*invalidator.kicked.lock().unwrap(), vec![abs]);
        assert!(invalidator.dehydrated.lock().unwrap().is_empty());
        assert!(placeheld.lock().unwrap().is_empty());
        assert!(hydrated.lock().unwrap().is_empty());
        assert!(status_events(&mut evt_rx).is_empty());
    }

    /// **A stale not-in-sync refusal is repaired — for proven-uploaded content only.**
    /// cfapi's TRACK_ALL bit trips on any local write and only resets by provider
    /// assertion; nothing asserted it before this loop existed, so an
    /// edited-then-uploaded file would refuse "Free up space" forever. When the
    /// engine's own record proves the disk content is uploaded
    /// (`is_dehydration_safe`), the loop asserts in-sync and retries.
    #[test]
    fn a_stale_not_in_sync_refusal_is_repaired_for_proven_uploaded_content() {
        let (_tmp, root, abs) = pin_root("edited.txt");
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let placeheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invalidator = FakeInvalidator {
            pin_actions: [(abs.clone(), PinAction::Dehydrate)].into(),
            refuse_until_in_sync: [abs.clone()].into(),
            ..Default::default()
        };

        run_startup(
            FakeHydrator {
                placeheld: placeheld.clone(),
                dehydration_safe: ["edited.txt".to_string()].into(),
                ..Default::default()
            },
            &invalidator,
            root,
            evt_tx,
        );

        assert_eq!(*invalidator.in_synced.lock().unwrap(), vec![abs.clone()]);
        assert_eq!(*invalidator.dehydrated.lock().unwrap(), vec![abs]);
        assert_eq!(*placeheld.lock().unwrap(), vec!["edited.txt".to_string()]);
        assert_eq!(status_events(&mut evt_rx).len(), 1);
    }

    /// **The startup converge's recorded uploads flip to ✅ — not just the watcher's.**
    /// An edit made while the service was down is uploaded by the startup `converge`,
    /// never by `apply_local_write`; before this leg the loop dropped converge's
    /// recorded rels, so the file kept Explorer's sync-pending arrows until the next
    /// *live* edit (live 2026-07-17 12:48 startup-converge race — an edit landing during
    /// startup uploaded via converge, the next watcher batch skipped it as already-synced
    /// with `recorded=false`, and no flip ran until the next real edit). The loop must
    /// flip every rel `converge` reports recorded, exactly as it flips the watcher path.
    #[test]
    fn the_startup_converge_flips_its_recorded_uploads_in_sync() {
        let (_tmp, root, abs) = pin_root("edited.txt");
        let (evt_tx, _evt_rx) = broadcast::channel(16);
        let invalidator = FakeInvalidator::default();

        run_startup(
            FakeHydrator {
                converge_records: vec!["edited.txt".to_string()],
                dehydration_safe: ["edited.txt".to_string()].into(),
                ..Default::default()
            },
            &invalidator,
            root,
            evt_tx,
        );

        assert_eq!(
            *invalidator.in_synced.lock().unwrap(),
            vec![abs],
            "a file uploaded + recorded by the startup converge must flip in-sync \
             (a cloud file → set_in_sync); dropping converge's recorded rels is what \
             left Explorer's sync-pending arrows up until the next live edit"
        );
    }

    /// **The safety rail: unverified content never overrides the platform's
    /// refusal.** A dirty file (an edit the engine hasn't finished uploading —
    /// e.g. still in the watcher's debounce window) fails `is_dehydration_safe`,
    /// so the loop must NOT assert in-sync, must NOT dehydrate, and must NOT flip
    /// the row. Anything else destroys the user's edit — the iron rule
    /// (`file-sync.md` § Sync direction: a change to a tracked file MUST be
    /// uploaded).
    #[test]
    fn a_dirty_unpinned_file_is_never_force_dehydrated() {
        let (_tmp, root, abs) = pin_root("dirty.txt");
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let placeheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invalidator = FakeInvalidator {
            pin_actions: [(abs.clone(), PinAction::Dehydrate)].into(),
            refuse_until_in_sync: [abs.clone()].into(),
            ..Default::default()
        };

        run_startup(
            FakeHydrator {
                placeheld: placeheld.clone(),
                // dehydration_safe deliberately empty: the engine cannot prove
                // the disk content was uploaded.
                ..Default::default()
            },
            &invalidator,
            root,
            evt_tx,
        );

        assert!(
            invalidator.in_synced.lock().unwrap().is_empty(),
            "asserting in-sync over unverified content is how a user's edit dies"
        );
        assert!(invalidator.dehydrated.lock().unwrap().is_empty());
        assert!(
            placeheld.lock().unwrap().is_empty(),
            "the row must keep saying the bytes are local — they are"
        );
        assert!(status_events(&mut evt_rx).is_empty());
    }

    /// **A replacing editor's file is re-anchored, not abandoned.** Most editors
    /// save by replace/truncate, destroying placeholder-ness — every placeholder
    /// op on the saved file fails "not a cloud file" (measured, 0x80070178), so
    /// neither the bare dehydrate nor the in-sync repair can reach it as it is.
    /// For proven-uploaded content the arm re-anchors it (rel identity, NOT
    /// in-sync), asserts in-sync conditioned on the USN read before the proof,
    /// and retries the bare dehydrate — the same road the post-record flip takes,
    /// never a convert that vouches and frees in one unconditioned op.
    #[test]
    fn an_editor_replaced_file_is_reanchored_and_freed() {
        let (_tmp, root, abs) = pin_root("saved.txt");
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let placeheld = Arc::new(std::sync::Mutex::new(Vec::new()));
        let invalidator = FakeInvalidator {
            pin_actions: [(abs.clone(), PinAction::Dehydrate)].into(),
            // The bare dehydrate fails (not a cloud file, then not in sync)...
            refuse_until_in_sync: [abs.clone()].into(),
            // ...because the editor's save replaced the placeholder.
            not_cloud_files: [abs.clone()].into(),
            ..Default::default()
        };

        run_startup(
            FakeHydrator {
                placeheld: placeheld.clone(),
                dehydration_safe: ["saved.txt".to_string()].into(),
                ..Default::default()
            },
            &invalidator,
            root,
            evt_tx,
        );

        assert_eq!(
            *invalidator.anchored.lock().unwrap(),
            vec![(abs.clone(), "saved.txt".to_string())],
            "the replaced file must be re-anchored (identity = rel)"
        );
        assert_eq!(
            *invalidator.in_synced.lock().unwrap(),
            vec![abs.clone()],
            "then asserted in-sync through the conditioned road"
        );
        assert_eq!(
            *invalidator.dehydrated.lock().unwrap(),
            vec![abs],
            "and freed by the retried bare dehydrate"
        );
        assert_eq!(*placeheld.lock().unwrap(), vec!["saved.txt".to_string()]);
        assert_eq!(status_events(&mut evt_rx).len(), 1);
    }

    /// The on-demand root owes the corpus the same passes the always-resident root
    /// runs: the once-per-start set (pre-bind re-seal, audience convergence,
    /// post-succession re-seal) exactly once at startup, and the posture refresh +
    /// per-tick pair (audience + website) at startup and then on every rescan
    /// tick. Until 2026-09-27 the loop ran none of them, so an audience flip never reached an
    /// on-demand folder's back-catalogue — a public window's plaintext stayed
    /// sealed at rest and the flip back never re-sealed it (found by
    /// `test_folder_bound_flip_back.py` on tui-on-windows the day on-demand became
    /// the windows binding default).
    #[test]
    fn the_loop_runs_the_corpus_passes_at_start_and_the_tick_pair_per_rescan() {
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let cancel = CancellationToken::new();
        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            stop_after_repulls: Some((2, cancel.clone())),
            ..Default::default()
        };
        let starts = hydrator.corpus_starts.clone();
        let ticks = hydrator.corpus_ticks.clone();
        let converges = hydrator.converges.clone();

        loop_runtime().block_on(async move {
            // Paused clock: the runtime auto-advances to each timer deadline, so
            // the two 300 s rescan ticks fire without waiting.
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                FakeInvalidator::default(),
                rx,
                None,
                PathBuf::from(r"C:\Root"),
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert_eq!(
            *starts.lock().unwrap(),
            1,
            "the once-per-start passes run exactly once, at startup"
        );
        assert_eq!(
            *ticks.lock().unwrap(),
            3,
            "the per-tick pair runs at startup and then once per rescan tick (two ticks here)"
        );
        assert_eq!(
            *ticks.lock().unwrap(),
            *converges.lock().unwrap(),
            "the corpus pair rides every converge: startup + each tick, never fewer"
        );
    }

    /// The pin sweep is a **backstop like converge**: it runs on the rescan tick
    /// too, not only at startup — a flip the watcher missed heals within one tick.
    #[test]
    fn the_rescan_tick_runs_the_pin_sweep_not_just_startup() {
        let (_tmp, root, abs) = pin_root("f.txt");
        let (evt_tx, _evt_rx) = broadcast::channel(16);
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let cancel = CancellationToken::new();
        let invalidator = FakeInvalidator {
            pin_actions: [(abs.clone(), PinAction::Dehydrate)].into(),
            ..Default::default()
        };
        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            stop_after_repulls: Some((1, cancel.clone())),
            ..Default::default()
        };

        let inv_ref = &invalidator;
        loop_runtime().block_on(async move {
            // Paused clock: the runtime idles and auto-advances to each timer
            // deadline (the watcher's debounce ticks, then the 300 s rescan).
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                inv_ref,
                rx,
                Some(evt_tx),
                root,
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert_eq!(
            *invalidator.dehydrated.lock().unwrap(),
            vec![abs.clone(), abs],
            "one dehydrate from the startup sweep + one from the tick sweep \
             (the fake's pin classification never changes, so the reaction \
             fires on both passes)"
        );
    }

    /// A failed fetch neither marks the file Synced nor emits a status change —
    /// the file is still a cloud-only placeholder, so its overlay must not flip.
    #[test]
    fn loop_does_not_mark_or_emit_on_fetch_failure() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let hydrated = Arc::new(std::sync::Mutex::new(Vec::new()));
        tx.send(HydrationCommand::Fetch {
            rel: "sub/f.txt".into(),
            offset: 0,
            length: 3,
            file_size: 3,
            sink: Box::new(RecordingSink::default()),
        })
        .unwrap();
        drop(tx);
        loop_runtime().block_on(run_hydration_loop(
            FakeHydrator {
                bytes: None, // hydrate fails
                hydrated: hydrated.clone(),
                ..Default::default()
            },
            FakeInvalidator::default(),
            rx,
            Some(evt_tx),
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            CancellationToken::new(),
            None,
            None,
        ));

        assert!(
            hydrated.lock().unwrap().is_empty(),
            "a failed fetch must not mark the file Synced"
        );
        let mut saw_status = false;
        while let Ok(ev) = evt_rx.try_recv() {
            if matches!(ev.event, EventKind::FileStatusChanged { .. }) {
                saw_status = true;
            }
        }
        assert!(
            !saw_status,
            "a failed fetch must not emit FileStatusChanged"
        );
    }

    // ── the no-restart driver: periodic re-pull + reconnect (Track W slice 2) ──

    /// Collect the `FileStatusChanged` events a loop broadcast, in order.
    fn status_events(rx: &mut broadcast::Receiver<Event>) -> Vec<(String, FileStatus)> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let EventKind::FileStatusChanged { path, status } = ev.event {
                out.push((path, status));
            }
        }
        out
    }

    /// One re-pull's worth of nest state: a hydrated file whose head moved.
    fn fold_with_stale(rel: &str) -> Result<PlaceholderFold> {
        Ok(PlaceholderFold {
            recorded: 1,
            stale_hydrated: vec![stale_row(rel, 7)],
            ..Default::default()
        })
    }

    /// **The point of the whole track.** A remote change arriving *while the
    /// service is up* must reach this host with **no restart**: the rescan tick
    /// re-pulls `changes.list`, and a hydrated copy the re-fold reports stale is
    /// invalidated through the very same dehydrate→re-point→`CloudOnly` path the
    /// startup fold uses. Before this, the invalidation only ever fired at
    /// `prepare()`, so the user saw superseded bytes until the next service start.
    #[test]
    fn the_rescan_tick_repulls_and_invalidates_a_stale_hydrated_copy_without_a_restart() {
        // The command channel is held open: only the cancel (fired by the fake
        // after its first re-pull) ends this loop — never a channel close.
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let repointed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let repull_calls = Arc::new(std::sync::Mutex::new(0usize));
        let invalidator = FakeInvalidator::default();

        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            repulls: Arc::new(std::sync::Mutex::new(
                [fold_with_stale("sub/f.txt")].into_iter().collect(),
            )),
            repull_calls: repull_calls.clone(),
            repointed: repointed.clone(),
            stop_after_repulls: Some((1, cancel.clone())),
            ..Default::default()
        };

        loop_runtime().block_on(async move {
            // Paused clock: with every other select arm pending, the runtime idles,
            // so tokio auto-advances straight to the 300 s tick. No sleeping.
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                invalidator,
                rx,
                Some(evt_tx),
                PathBuf::from(r"C:\Root"),
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert_eq!(
            *repull_calls.lock().unwrap(),
            1,
            "the rescan tick must re-pull the nest — startup's prepare() is not the only fold"
        );
        assert_eq!(
            *repointed.lock().unwrap(),
            vec!["sub/f.txt".to_string()],
            "a stale hydrated row found by the PERIODIC re-pull must be invalidated, \
             exactly as one found at startup is"
        );
        assert_eq!(
            status_events(&mut evt_rx),
            vec![(r"C:\Root\sub\f.txt".to_string(), FileStatus::CloudOnly)],
            "the invalidated file's overlay must flip to CloudOnly so Explorer agrees \
             with the row (the next open re-hydrates the new head)"
        );
    }

    /// **The two-way root's catch-up backstop runs — at startup AND on every rescan tick.**
    ///
    /// The watcher only sees writes that happen *while it is watching*. Edits made when the
    /// service was down (the laptop was asleep, the user logged out, the agent crashed) are
    /// invisible to it, and a root that relied on the watcher alone would never upload them —
    /// the file would sit locally modified forever, badged as though it were safe. So
    /// `LocalWriteHost::converge` (drain interrupted uploads → reconcile against the SyncDb →
    /// upload what that leaves pending) runs once at startup and again on every tick.
    ///
    /// This asserts the *loop wiring*, not the uploading itself: a root that only re-pulls the
    /// nest is the one-way root this track existed to kill, and that regression would be
    /// invisible to the read-side tests. The real uploads are asserted against a live cfapi root
    /// and the real `SyncEngine` in `cfapi_live_integration.rs`.
    #[test]
    fn the_rescan_tick_converges_local_writes_not_just_the_nest_repull() {
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let cancel = CancellationToken::new();
        let converges = Arc::new(std::sync::Mutex::new(0usize));

        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            // One tick, then stop — so the count below is exactly "startup + one tick".
            stop_after_repulls: Some((1, cancel.clone())),
            converges: converges.clone(),
            ..Default::default()
        };

        loop_runtime().block_on(async move {
            // Paused clock: every other arm is pending, so tokio idles and auto-advances
            // straight to the 300 s tick. Nothing sleeps.
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                FakeInvalidator::default(),
                rx,
                None,
                PathBuf::from(r"C:\Root"),
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert_eq!(
            *converges.lock().unwrap(),
            2,
            "the on-demand root must converge local writes at startup AND on the rescan tick, \
             not merely re-pull the nest. Startup catches edits made while the service was down; \
             the tick catches anything the watcher missed. A count of 0 is the one-way root \
             (uploads nothing, ever); 1 means the periodic backstop is gone, so an edit the \
             watcher missed would never be uploaded at all."
        );
    }

    /// **The rescan-tick converge flips its recorded uploads too — the second half of
    /// the same wiring.** The startup and tick converge are independent call sites; a
    /// watcher-missed / offline edit is uploaded by the *tick's* converge and must flip
    /// ✅ there as well. Pinned separately from the startup site because deleting the
    /// tick flip while keeping the startup one would otherwise stay green — the leg-5
    /// "unpinned wiring" trap. Startup + one tick each flip the recorded rel, so a
    /// healthy loop records `set_in_sync` twice; drop either call site and it falls to one.
    #[test]
    fn the_rescan_tick_flips_its_recorded_uploads_in_sync() {
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let (evt_tx, _evt_rx) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let root = PathBuf::from(r"C:\Root");
        let abs = root.join("edited.txt").to_string_lossy().to_string();
        let invalidator = FakeInvalidator::default();
        let inv = &invalidator;

        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            // One tick, then stop — so this is exactly "startup + one tick".
            stop_after_repulls: Some((1, cancel.clone())),
            converge_records: vec!["edited.txt".to_string()],
            dehydration_safe: ["edited.txt".to_string()].into(),
            ..Default::default()
        };

        loop_runtime().block_on(async move {
            // Paused clock: every other arm is pending, so tokio idles and auto-advances
            // straight to the 300 s tick. Nothing sleeps.
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                inv,
                rx,
                Some(evt_tx),
                root,
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert_eq!(
            *invalidator.in_synced.lock().unwrap(),
            vec![abs.clone(), abs],
            "startup AND the rescan tick each flip converge's recorded rels in-sync; a \
             count of one means one of the two call sites dropped converge's return"
        );
    }

    /// The socket coming back is its own trigger: changes recorded while this host
    /// was offline are in `changes.list` *now*, and waiting out a full
    /// `rescan_interval` to see them is latency with no upside. `NestClient` bumps
    /// its reconnect watch on every re-connect after the first.
    #[test]
    fn a_reconnect_repulls_immediately_rather_than_waiting_for_the_next_tick() {
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let repointed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let repull_calls = Arc::new(std::sync::Mutex::new(0usize));

        // The socket came back *before* the loop subscribed, so the receiver it
        // gets has an unseen change and `changed()` returns immediately — the same
        // edge production sees, minus the race.
        let reconnect = Arc::new(watch::channel(0u64));
        reconnect.0.send(1).unwrap();

        let hydrator = FakeHydrator {
            // An hour: if the reconnect arm did NOT fire, this test would hang on
            // the tick rather than pass by accident.
            interval: Some(Duration::from_secs(3600)),
            repulls: Arc::new(std::sync::Mutex::new(
                [fold_with_stale("a/b.txt")].into_iter().collect(),
            )),
            repull_calls: repull_calls.clone(),
            repointed: repointed.clone(),
            reconnect,
            stop_after_repulls: Some((1, cancel.clone())),
            ..Default::default()
        };

        loop_runtime().block_on(run_hydration_loop(
            hydrator,
            FakeInvalidator::default(),
            rx,
            Some(evt_tx),
            PathBuf::from(r"C:\Root"),
            "docs".to_string(),
            cancel,
            None,
            None,
        ));

        assert_eq!(
            *repull_calls.lock().unwrap(),
            1,
            "a reconnect must re-pull without waiting for the rescan tick"
        );
        assert_eq!(*repointed.lock().unwrap(), vec!["a/b.txt".to_string()]);
        assert_eq!(
            status_events(&mut evt_rx),
            vec![(r"C:\Root\a\b.txt".to_string(), FileStatus::CloudOnly)]
        );
    }

    /// A dirty (locally edited) file survives a periodic re-pull, not just a
    /// startup one: cfapi refuses the dehydrate, so the gate holds and the row is
    /// left `Synced` for a later conflict pass (`file-sync.md` § Restore ordering
    /// 3). The re-pull must never become a path that quietly eats an unsynced edit.
    #[test]
    fn a_periodic_repull_never_clobbers_an_unsynced_local_edit() {
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let (evt_tx, mut evt_rx) = broadcast::channel(16);
        let cancel = CancellationToken::new();
        let repointed = Arc::new(std::sync::Mutex::new(Vec::new()));

        let invalidator = FakeInvalidator {
            refuse: [r"C:\Root\sub\f.txt".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            repulls: Arc::new(std::sync::Mutex::new(
                [fold_with_stale("sub/f.txt")].into_iter().collect(),
            )),
            repointed: repointed.clone(),
            stop_after_repulls: Some((1, cancel.clone())),
            ..Default::default()
        };

        loop_runtime().block_on(async move {
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                invalidator,
                rx,
                Some(evt_tx),
                PathBuf::from(r"C:\Root"),
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert!(
            repointed.lock().unwrap().is_empty(),
            "a dehydrate refusal must skip the re-point — the local edit is the thing being protected"
        );
        assert!(
            status_events(&mut evt_rx).is_empty(),
            "a file that stayed hydrated must not have its overlay flipped to CloudOnly"
        );
    }

    /// A failed re-pull (nest unreachable, socket mid-reconnect) is best-effort
    /// like `prepare`: it must neither kill the loop nor touch any row. The next
    /// tick retries — which is also how a host that started against a down nest
    /// eventually populates at all.
    #[test]
    fn a_failed_repull_is_survivable_and_touches_nothing() {
        let (_cmd_tx, rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let cancel = CancellationToken::new();
        let repointed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let repull_calls = Arc::new(std::sync::Mutex::new(0usize));

        let hydrator = FakeHydrator {
            interval: Some(Duration::from_secs(300)),
            repulls: Arc::new(std::sync::Mutex::new(
                [
                    Err(anyhow!("nest unreachable")),
                    fold_with_stale("late.txt"),
                ]
                .into_iter()
                .collect(),
            )),
            repull_calls: repull_calls.clone(),
            repointed: repointed.clone(),
            // Run two ticks: the failing one, then the one that succeeds.
            stop_after_repulls: Some((2, cancel.clone())),
            ..Default::default()
        };

        loop_runtime().block_on(async move {
            tokio::time::pause();
            run_hydration_loop(
                hydrator,
                FakeInvalidator::default(),
                rx,
                None,
                PathBuf::from(r"C:\Root"),
                "docs".to_string(),
                cancel,
                None,
                None,
            )
            .await;
        });

        assert_eq!(
            *repull_calls.lock().unwrap(),
            2,
            "a failed re-pull must not end the loop — the next tick has to retry"
        );
        assert_eq!(
            *repointed.lock().unwrap(),
            vec!["late.txt".to_string()],
            "the retry after a failure must apply normally"
        );
    }

    // ── The agent's engine inputs, through the shared builder ──
    //
    // Every agent engine is built by `fauna_sync_engine::engine_lifecycle::
    // build_engine`, whose resolution (the row or custody read, the fail-closed
    // decision, the retired serve generation reaching the re-seal) is pinned by
    // that crate's own build tests. These pin what the agent hands it: its
    // bearer-only inputs, assembled over a hand-made binding by the builder's
    // construction half — no control plane to read.

    /// An engine assembled from the agent's own inputs (`agent_engine_params`),
    /// its own nest at `own_nest`, over `binding`.
    fn assembled(
        own_nest: &str,
        dir: &Path,
        binding: fauna_sync_engine::engine_lifecycle::ResolvedBinding,
    ) -> SyncEngine {
        let slot = crate::bearer::CapabilitySlot::new(Some(SyncCapability::new(
            vec![5u8; 32],
            vec![6u8; 32],
            own_nest.into(),
            "dev-test".into(),
            BearerToken::new("tok".into(), 4_000_000_000),
        )));
        let params = agent_engine_params(
            agent_control_plane(slot, own_nest.to_string(), [6u8; 32]),
            dir.to_path_buf(),
            dir.to_path_buf(),
            fauna_core::folder_keys::FolderRef::Local(1),
            [0u8; 32],
            [5u8; 32],
            &AgentPredecessors::default(), // no succession in this fixture
            None,
            fauna_sync_engine::access_gate::AccessGate::new(),
            None, // change_signer: an unsigned test root
            std::sync::Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
        );
        fauna_sync_engine::engine_lifecycle::assemble_engine(params, binding)
            .expect("engine assembles offline")
            .engine
    }

    fn owner_only(folder: &str) -> fauna_sync_engine::engine_lifecycle::ResolvedBinding {
        fauna_sync_engine::engine_lifecycle::ResolvedBinding {
            folder: folder.into(),
            ..Default::default()
        }
    }

    /// The production host answers the loop's command through the SHARED verb:
    /// `SyncEngine`'s `answer_engine_command` runs `SyncEngine::apply_held_deletes`
    /// (here over an empty folder, where no hold is active, so the verb applies
    /// nothing by design) — never a second implementation.
    #[tokio::test]
    async fn a_real_engine_answers_apply_held_deletes_through_the_shared_verb() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = assembled("http://127.0.0.1:7450", tmp.path(), owner_only("docs"));
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        HydrationHost::answer_engine_command(&engine, EngineCommand::ApplyHeldDeletes { reply })
            .await;
        let verdict = reply_rx
            .await
            .expect("the engine replies")
            .expect("an empty folder's apply succeeds");
        assert_eq!(
            (
                verdict.applied,
                verdict.remaining_held,
                verdict.floor_was_active
            ),
            (0, 0, false),
            "no hold active → the shared verb applies nothing"
        );
    }

    #[tokio::test]
    async fn an_agent_engine_is_bearer_only_and_hydrator() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = assembled("http://127.0.0.1:7450", tmp.path(), owner_only("docs"));
        // Bearer-only host holds no MLS engine.
        assert!(
            engine.device_sync_channel_id().is_none(),
            "bearer-only host must have no MLS device-sync channel"
        );
        // It is the production FileHydrator over a real (empty) SyncDb: an untracked
        // path errors rather than hydrating, proving the db is wired.
        let err = FileHydrator::download_file_bytes(&engine, "absent.txt").await;
        assert!(err.is_err(), "untracked path should error, not hydrate");
    }

    /// **Every** root must load the folder's `.faunaignore` — there is no longer a role that
    /// escapes it, and that is the point of this test.
    ///
    /// The on-demand root used to be handed an *empty* matcher, on the reasoning that "a
    /// download-only host makes no local writes to ignore". That premise died the moment the
    /// root became two-way: an ignored file would have been uploaded the first time the watcher
    /// saw it — an ignore list that silently doesn't apply is worse than none, because the user
    /// believes it does. The `EngineRole` that carried the distinction is gone entirely (it had
    /// no other effect), so this now holds for every engine the driver builds.
    #[tokio::test]
    async fn every_root_honors_faunaignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".faunaignore"), "secret.txt\n").unwrap();

        let engine = assembled("http://127.0.0.1:7450", tmp.path(), owner_only("docs"));

        assert!(
            engine.is_ignored("secret.txt"),
            ".faunaignore must be loaded for EVERY root, on-demand included: both upload now, \
             so an unloaded matcher means an ignored file gets uploaded the first time the \
             watcher sees it"
        );
        assert!(
            !engine.is_ignored("notes.txt"),
            "an unlisted file must still sync"
        );
    }

    /// A cross-nest binding must produce a genuinely **cross-nest** engine —
    /// byte plane at the set's HOME nest (its chunks live nowhere else), control
    /// plane relayed through the caller's OWN nest (the only nest this actor
    /// holds a session on).
    ///
    /// Getting either wrong is silent: a same-nest-built engine POSTs chunks at a
    /// nest that never claimed the set and polls a change log with no rows for it,
    /// so the folder looks bound and simply never syncs.
    #[tokio::test]
    async fn a_foreign_binding_builds_a_cross_nest_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let channel_hex = "ab".repeat(32);
        let engine = assembled(
            "http://127.0.0.1:7450", // the caller's OWN nest
            tmp.path(),
            fauna_sync_engine::engine_lifecycle::ResolvedBinding {
                folder: "xnest-docs".into(),
                mls_group_id: Some(b"foreign-gid".to_vec()),
                content_keys: Some(fauna_core::folder_keys::FolderContentKeys::genesis(
                    [9u8; 32], 1_000,
                )),
                foreign: Some(fauna_sync_engine::engine_lifecycle::ForeignTransport {
                    home_nest_url: "http://127.0.0.1:7999".into(),
                    channel_id_hex: channel_hex.clone(),
                }),
                ..Default::default()
            },
        );

        assert_eq!(
            engine.foreign_routing(),
            Some(("http://127.0.0.1:7999".to_string(), channel_hex)),
            "the control plane must relay changes.record AND changes.list through \
             the own nest to the home nest"
        );
        assert_eq!(
            engine.byte_plane_nest_url(),
            "http://127.0.0.1:7999",
            "chunks and manifests live on the HOME nest — the byte plane never \
             rides the relay"
        );
    }

    /// The same-nest path must be untouched by the cross-nest branch: an
    /// owner-only set keeps both planes on the caller's own nest.
    #[tokio::test]
    async fn a_same_nest_binding_keeps_both_planes_on_the_own_nest() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = assembled("http://127.0.0.1:7450", tmp.path(), owner_only("docs"));

        assert_eq!(engine.foreign_routing(), None);
        assert_eq!(engine.byte_plane_nest_url(), "http://127.0.0.1:7450");
    }
}
