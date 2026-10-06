//! The Windows Cloud Files API OS shim: the `extern "system"` callbacks Windows
//! fires on the on-demand sync root, plus sync-root lifecycle (register /
//! connect / disconnect / unregister).
//!
//! This is the thin, untested-by-necessity boundary between the OS and the
//! deterministically-tested core in [`crate::bridge`]. The callbacks fire on
//! arbitrary OS threads, extract only `Send` primitives (connection key,
//! transfer key, path, offset/length), wrap the cfapi completion handles in a
//! [`TransferSink`] / [`PlaceholderSink`], look up the right engine by the
//! callback's connection key, and hand a [`HydrationCommand`] to that engine's
//! `!Sync` driving loop. The bytes / placeholders are delivered later by the
//! driving thread via `CfExecute` — cfapi permits async completion keyed by the
//! transfer key. See `docs/goal/architecture/apps/windows.md` § On-demand
//! hydration host and `docs/goal/behavior/file-sync.md` § On-Demand Files.
//!
//! ## Per-root callback context, keyed by connection key
//!
//! The on-demand host serves N sync roots concurrently (one per bound folder —
//! [`crate::engine_driver`]), each registered by its own per-engine future.
//! cfapi fires the `extern "system"` callbacks on arbitrary OS threads and tags
//! each with the root's `CF_CONNECTION_KEY` (returned by `connect`), so the
//! callback context is a `HashMap<i64, CallbackCtx>` keyed by that connection
//! key — every callback routes to *its own* root's driving-thread command sender
//! and sync-root path. (A single-root host is just the `N == 1` case.) See
//! `docs/goal/architecture/apps/windows.md` § On-demand hydration host
//! (*Multi-root*): "the callback context is a `HashMap` keyed by connection key,
//! not a single process-global".

#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use tokio::sync::mpsc;
use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Storage::CloudFilters::{
    CF_CALLBACK_INFO, CF_CALLBACK_PARAMETERS, CF_CONNECTION_KEY,
};

use fauna_sync_engine::enumerate::DirChild;

use crate::bridge::{HydrationCommand, PlaceholderSink, TransferSink};
use crate::path_map::{callback_full_path, normalized_path_to_rel};

/// STATUS_UNSUCCESSFUL — reported to cfapi when hydration can't produce a file's
/// bytes, so the opening process fails cleanly instead of hanging.
const STATUS_UNSUCCESSFUL: NTSTATUS = NTSTATUS(0xC0000001u32 as i32);

/// Display name shown for the sync root in Explorer / the cloud-provider UI.
/// The test-harness filter-only [`register_and_connect`] path shows the bare brand; the
/// product path shows the per-folder [`sync_root_display_for`].
const SYNC_ROOT_DISPLAY: &str = "Fauna";

/// The per-folder display name a shell-registered root shows in Explorer's
/// nav pane / provider grouping — the `OneDrive – Personal` shape. The brand is
/// untranslated and the folder name is user data, so no i18n key applies.
fn sync_root_display_for(folder: &str) -> String {
    format!("Fauna – {folder}")
}

/// The `account` segment of a shell registration's `Fauna!SID!account` Id, for
/// one (folder, folder) binding. Two properties are load-bearing:
///
/// - **Recomputable** — derived from what every teardown site already knows, so
///   no id is ever persisted.
/// - **Unique per binding** — the folder-path hash means a re-bind of the same
///   folder to a new folder is a *different* root, and two concurrent test
///   runs (or two parallel dev checkouts) serving a same-named folder under
///   different temp dirs cannot steal each other's user-global
///   `SyncRootManager` entry — the same user-global-registry race the
///   shell-ext HKCU tests hit.
fn shell_account_id(folder: &str, sync_root: &Path) -> String {
    // Keep alphanumerics plus `-` and `_`, everything else `_`: a safe single
    // path component.
    let safe: String = folder
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let norm = sync_root
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_lowercase();
    // blake3 via the shared helper (no new dep); 4 bytes ≙ 8 hex chars is plenty
    // for "distinct bindings on one machine".
    let hash = fauna_core::sync::path_hash(&norm);
    format!("{safe}-{}", hex::encode(&hash[..4]))
}

// ---------------------------------------------------------------------------
// Per-root callback context, keyed by connection key (see module docs)
// ---------------------------------------------------------------------------

struct CallbackCtx {
    /// Command channel into the driving thread that owns this root's `!Sync` engine.
    cmd_tx: mpsc::UnboundedSender<HydrationCommand>,
    /// This root's registered path, used to map a callback's full NormalizedPath
    /// back to a folder-relative path (each root maps under its own path).
    sync_root: PathBuf,
}

/// Process-global map of live callback contexts, keyed by the root's
/// `CF_CONNECTION_KEY` (`i64`). One entry per registered sync root.
fn ctx_map() -> &'static Mutex<HashMap<i64, CallbackCtx>> {
    static CALLBACK_CTX: OnceLock<Mutex<HashMap<i64, CallbackCtx>>> = OnceLock::new();
    CALLBACK_CTX.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a root's callback context under its connection key (called by
/// [`register_and_connect`] once `connect` returns the key).
fn ctx_insert(conn_key: i64, ctx: CallbackCtx) {
    ctx_map().lock().unwrap().insert(conn_key, ctx);
}

/// Drop a root's callback context (called by [`CfApiConnection`]'s `Drop` after
/// the root is disconnected + unregistered).
fn ctx_remove(conn_key: i64) {
    ctx_map().lock().unwrap().remove(&conn_key);
}

/// Snapshot the callback context for `conn_key`: a clone of that root's
/// driving-thread sender plus its sync-root path. `None` when no root is
/// registered under this key (a stray callback racing teardown).
fn current_ctx(conn_key: i64) -> Option<(mpsc::UnboundedSender<HydrationCommand>, String)> {
    let map = ctx_map().lock().unwrap();
    map.get(&conn_key).map(|cx| {
        (
            cx.cmd_tx.clone(),
            cx.sync_root.to_string_lossy().into_owned(),
        )
    })
}

// ---------------------------------------------------------------------------
// Sync-root lifecycle
// ---------------------------------------------------------------------------

/// What a [`CfApiConnection`]'s `Drop` does with the **registration** (the
/// connection itself always disconnects). Decided at construction:
///
/// - [`register_and_connect`] (harness / filter-only tests) →
///   [`TeardownMode::UnregisterOnDrop`]: the pre-persistence behavior — eager
///   filter unregistration, so a test's temp root never leaves filter state
///   behind, even on panic.
/// - [`register_and_connect_shell`] (the product path) →
///   [`TeardownMode::KeepUnlessMarked`]: **the registration outlives the
///   process.** A service stop/restart/crash only disconnects — a graceful
///   unregister was measured (2026-07-16) to REMOVE un-hydrated cloud-only
///   placeholders from disk, so per-serve-session unregistration made every
///   service restart empty and repopulate the folder in Explorer. Full teardown
///   (filter + shell) happens only when [`mark_root_for_full_teardown`] flagged
///   this root first — the unbind / folder-removal / re-bind / mode-flip cases,
///   which only [`crate::engine_driver::reconcile_engines`] can distinguish.
///
/// The intent is consumed in `Drop` (not in post-cancel loop code) because drop
/// is the ONE path that runs however the engine future ends: graceful return,
/// `EngineHost` cancellation dropping the future mid-poll, or a panic unwind. A
/// panicked engine has no intent marked and so keeps its registration — correct,
/// a panic is not an unbind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeardownMode {
    UnregisterOnDrop,
    KeepUnlessMarked,
}

/// Roots flagged for **full** teardown (filter + shell) when their connection
/// guard next drops. Keyed by sync-root path; written by
/// [`mark_root_for_full_teardown`] (from `reconcile_engines`, *before* it sends
/// the Stop/Start that cancels the engine), consumed by [`CfApiConnection`]'s
/// `Drop`. Process-global for the same reason as [`ctx_map`]: the flag must
/// reach a drop that can happen on the engine-host worker thread.
fn full_teardown_marks() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static MARKS: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    MARKS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Flag `sync_root` so the next drop of its [`CfApiConnection`] tears the
/// registration down fully (disconnect → filter unregister → shell unregister)
/// instead of keeping it. Call **before** stopping/restarting the engine that
/// serves it. Idempotent; a mark with no live connection is consumed by the next
/// connection's drop for the same path (which is the re-bind race resolving in
/// the right direction: the old root's registration goes down).
pub fn mark_root_for_full_teardown(sync_root: &Path) {
    full_teardown_marks()
        .lock()
        .unwrap()
        .insert(sync_root.to_path_buf());
}

/// Take (consume) a pending full-teardown mark for `sync_root`.
fn take_full_teardown_mark(sync_root: &Path) -> bool {
    full_teardown_marks().lock().unwrap().remove(sync_root)
}

/// A live cfapi sync-root registration + connection — a **drop guard**. Its
/// `Drop` always runs `disconnect` (blocks until in-flight callbacks drain) and
/// removes this root's entry from the keyed callback map (last, so callbacks
/// draining during `disconnect` can still route); what happens to the
/// **registration** in between is the [`TeardownMode`] chosen at construction
/// (see there — the product keeps it, the harness unregisters). When the
/// [`EngineHost`](fauna_sync_engine::engine_host::EngineHost) cancels this
/// engine's token, the per-engine future returns (or is dropped) and drops this
/// guard; a callback that races teardown after the entry is gone finds no
/// context and fails benignly.
pub struct CfApiConnection {
    key: CF_CONNECTION_KEY,
    sync_root: PathBuf,
    teardown: TeardownMode,
    /// Run inside `Drop`, on the full-teardown arm only, AFTER the disconnect and
    /// BEFORE the unregister that removes every cloud-only placeholder from the
    /// disk — [`Self::before_full_teardown`].
    before_full_teardown: Option<Box<dyn FnOnce() + Send>>,
}

impl CfApiConnection {
    /// Register `hook` to run when this guard's drop tears the registration down
    /// FULLY (an unbind / re-bind / mode flip — [`mark_root_for_full_teardown`]),
    /// strictly before the unregister that removes the root's un-hydrated
    /// placeholders. The product hangs the seen-mark hygiene here
    /// (`delete-propagation.md` § *An offline placeholder delete propagates*,
    /// decision (e)): the removal is the product's own act, never a user's delete,
    /// so the marks go first — a crash in between leaves placeholders unmarked on
    /// the disk, which the next scan re-marks, never marks without files. Drop is
    /// the one path that runs however the engine future ends (see
    /// [`TeardownMode`]), which is why the hook lives on the guard. A kept
    /// registration (service stop, restart, crash) never runs it.
    pub fn before_full_teardown(&mut self, hook: impl FnOnce() + Send + 'static) {
        self.before_full_teardown = Some(Box::new(hook));
    }

    /// This root's live connection key — what provider-initiated cfapi operations
    /// (`fauna_cfapi::provider_push_data`) authenticate with. Test-gated until a
    /// production caller exists: the pin-reaction loop chose `CfHydratePlaceholder`
    /// (drives the normal fetch path) over the provider push, so today only the
    /// `diag_pin_reaction_mechanics` probe consumes it.
    #[cfg(test)]
    pub fn key(&self) -> CF_CONNECTION_KEY {
        self.key
    }

    /// Test seam: tear down the **connection** but leave the filter
    /// **registration** in place — the state a *crashed* provider leaves behind
    /// (the OS reclaims the connection at process death; the registration
    /// survives). A graceful drop is different in two measured ways: it
    /// unregisters, and unregistering **removes un-hydrated cloud-only
    /// placeholders from disk** (0x80070002) — so "a pin flipped while the
    /// provider was down" is a state only this path can stage.
    #[cfg(test)]
    pub fn disconnect_keeping_registration(self) {
        fauna_cfapi::disconnect(self.key);
        ctx_remove(self.key.0);
        std::mem::forget(self); // skip Drop's unregister
    }
}

impl Drop for CfApiConnection {
    fn drop(&mut self) {
        fauna_cfapi::disconnect(self.key);
        let root = self.sync_root.to_string_lossy();
        match self.teardown {
            TeardownMode::UnregisterOnDrop => {
                if let Err(e) = fauna_cfapi::unregister_sync_root(&root) {
                    tracing::warn!(error = %e, "unregister_sync_root failed");
                }
            }
            TeardownMode::KeepUnlessMarked => {
                if take_full_teardown_mark(&self.sync_root) {
                    // Unbind / re-bind / mode-flip: the binding ended, so the
                    // registration goes down with it. Measured order (2026-07-16):
                    // the shell unregister fails 0x8007017C while the root is
                    // connected and needs the filter side down first — so
                    // disconnect (above) → filter → shell. The hook runs first
                    // of all: the unregister is what removes the placeholders.
                    if let Some(hook) = self.before_full_teardown.take() {
                        hook();
                    }
                    if let Err(e) = fauna_cfapi::unregister_sync_root(&root) {
                        tracing::warn!(error = %e, "unregister_sync_root failed");
                    }
                    match fauna_cfapi::shell_registration_for(&root) {
                        Ok(Some(id)) => {
                            if let Err(e) = fauna_cfapi::unregister_sync_root_with_shell(&id) {
                                tracing::warn!(error = %e, id, "shell unregister failed");
                            }
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!(error = %e, "shell registration lookup failed"),
                    }
                    tracing::info!(root = %root, "sync root fully unregistered (binding ended)");
                } else {
                    // Service stop/restart/crash: the registration is the
                    // binding's, not the process's — keep it, so Explorer's
                    // placeholders survive and the next start just re-connects.
                    tracing::info!(root = %root, "sync root disconnected; registration kept");
                }
            }
        }
        ctx_remove(self.key.0);
    }
}

/// Register `sync_root` as a cfapi sync root and connect, wiring the FETCH_DATA /
/// FETCH_PLACEHOLDERS / CANCEL / NOTIFY_DEHYDRATE_COMPLETION callbacks to
/// `cmd_tx`. The callback context is
/// published under the connection key **after** `connect` returns it (the key is
/// what routes a callback to this root), so a callback that fires in the narrow
/// window before the insert finds no context and fails benignly. On
/// registration / connect failure nothing is left published.
/// Test/harness-only since the product moved to [`register_and_connect_shell`]
/// (2026-07-16): a bare filter registration with EAGER unregister-on-drop, so a
/// harness temp root never leaves filter state behind, even on panic. The live
/// tests that exercise filter-level mechanics (pin flips, population, guards)
/// keep using it deliberately — shell visibility is irrelevant to them and the
/// eager teardown is what they want.
#[cfg(test)]
pub fn register_and_connect(
    sync_root: &Path,
    cmd_tx: mpsc::UnboundedSender<HydrationCommand>,
) -> Result<CfApiConnection> {
    let root = sync_root.to_string_lossy().into_owned();
    fauna_cfapi::register_sync_root(&root, SYNC_ROOT_DISPLAY)?;
    connect_registered(sync_root, cmd_tx, TeardownMode::UnregisterOnDrop)
}

/// The product's on-demand root connection — [`register_and_connect_shell`] plus
/// the seen-mark hygiene of a full teardown: the guard's
/// [`CfApiConnection::before_full_teardown`] clears every seen mark in the
/// engine's state DB (`state_db`) before the unregister removes the root's
/// cloud-only placeholders (`delete-propagation.md` § *An offline placeholder
/// delete propagates*, decision (e)). The one constructor both the agent
/// (`engine_driver::run_on_demand_root`) and the live tests connect through.
pub fn register_and_connect_product_root(
    sync_root: &Path,
    folder: &str,
    cmd_tx: mpsc::UnboundedSender<HydrationCommand>,
    state_db: PathBuf,
) -> Result<CfApiConnection> {
    let mut conn = register_and_connect_shell(sync_root, folder, cmd_tx)?;
    conn.before_full_teardown(move || {
        let cleared =
            fauna_sync_engine::db::SyncDb::open(&state_db).and_then(|db| db.clear_seen_all());
        if let Err(e) = cleared {
            tracing::error!(
                error = %e,
                "clearing the seen marks before a full teardown failed; an on-demand start \
                 over this state DB clears them as a fresh registration, but any other engine \
                 over it may count the removed placeholders as deletes (a wholesale set is \
                 still held by the mass-delete floor)"
            );
        }
    });
    Ok(conn)
}

/// Did `sync_root`'s registration survive since this binding last served it —
/// shell-registered AND filter-registered? The boot's decision-(e) input
/// (`delete-propagation.md` § *An offline placeholder delete propagates*): a
/// root whose shell or filter registration did not survive had its cloud-only
/// placeholders removed by the OS at the unregister (an uninstall over a leftover
/// state DB, the ghost sweep, an out-of-band unregister), so the engine's seen
/// marks no longer describe the disk and are cleared before the boot sweep. A
/// probe that errors answers `false` — clearing is the fail-safe direction on the
/// destructive axis (fewer deletes, never more).
pub fn registration_survived(sync_root: &Path) -> bool {
    let root = sync_root.to_string_lossy();
    let shell = matches!(fauna_cfapi::shell_registration_for(&root), Ok(Some(_)));
    let filter = matches!(fauna_cfapi::is_filter_registered(&root), Ok(true));
    shell && filter
}

/// The **product** sync-root lifecycle ([`crate::engine_driver`]'s on-demand
/// path): ensure the root is **shell**-registered (`StorageProviderSyncRootManager`
/// — the registration Explorer's cloud verbs / status column / provider grouping
/// key off; the bare filter registration renders none of that UX), then connect.
/// Startup is a two-state reconcile — a fresh folder and a kept registration
/// from a previous run land in the same end state:
///
/// 1. **Already shell-registered** (query): skip straight to the filter
///    `CF_REGISTER_FLAG_UPDATE` re-register + connect — the same proven sequence
///    the crash-heal live test drives.
/// 2. **Fresh**: shell-register directly.
///
/// A folder that is filter-registered but NOT shell-registered (a hard-killed
/// test harness; the product never leaves one — the ended-binding teardown takes
/// the filter side down first, and a failed filter unregister leaves both
/// registrations up) needs no arm of its own: the WinRT `Register` registers
/// over it in place (measured 2026-09-25), so it takes the fresh path and
/// nothing is unregistered.
///
/// The returned guard **keeps the registration on drop** ([`TeardownMode`]):
/// registration belongs to the *binding*, and only a
/// [`mark_root_for_full_teardown`]-flagged drop (unbind / re-bind / mode-flip)
/// takes it down.
pub fn register_and_connect_shell(
    sync_root: &Path,
    folder: &str,
    cmd_tx: mpsc::UnboundedSender<HydrationCommand>,
) -> Result<CfApiConnection> {
    let root = sync_root.to_string_lossy().into_owned();
    let display = sync_root_display_for(folder);

    match fauna_cfapi::shell_registration_for(&root)? {
        Some(id) => {
            tracing::debug!(root = %root, id, "sync root already shell-registered");
        }
        None => {
            let account = shell_account_id(folder, sync_root);
            fauna_cfapi::register_sync_root_with_shell(&root, &display, &account)?;
        }
    }

    // The WinRT registration writes the filter policies too, but the UPDATE
    // re-register is kept unconditionally: it is the measured-tolerated,
    // crash-heal-proven way to refresh the filter side over ANY surviving
    // registration, shell-level included (the hold-harness sequence).
    fauna_cfapi::register_sync_root(&root, SYNC_ROOT_DISPLAY)?;
    connect_registered(sync_root, cmd_tx, TeardownMode::KeepUnlessMarked)
}

/// Shared tail of the two registration flows: connect the already-registered
/// root, publish the callback context, wrap it in the drop guard.
fn connect_registered(
    sync_root: &Path,
    cmd_tx: mpsc::UnboundedSender<HydrationCommand>,
    teardown: TeardownMode,
) -> Result<CfApiConnection> {
    let root = sync_root.to_string_lossy().into_owned();

    let key = match fauna_cfapi::connect(
        &root,
        Some(fetch_data_callback),
        Some(cancel_callback),
        Some(fetch_placeholders_callback),
        Some(dehydrate_completion_callback),
    ) {
        Ok(key) => key,
        Err(e) => {
            // A failed connect on the eager (harness) path cleans its filter
            // registration up; the persistent path keeps it, matching its
            // drop semantics (the registration is the binding's).
            if teardown == TeardownMode::UnregisterOnDrop {
                let _ = fauna_cfapi::unregister_sync_root(&root);
            }
            return Err(e);
        }
    };

    // Publish this root's context keyed by its connection key, so the
    // arbitrary-thread callbacks route to this engine's command channel.
    ctx_insert(
        key.0,
        CallbackCtx {
            cmd_tx,
            sync_root: sync_root.to_path_buf(),
        },
    );

    Ok(CfApiConnection {
        key,
        sync_root: sync_root.to_path_buf(),
        teardown,
        before_full_teardown: None,
    })
}

/// Unregister a single shell sync-root entry — filter half first, then the
/// shell (`SyncRootManager`) half, the full-teardown order (mirrors
/// `CfApiConnection::drop`'s `KeepUnlessMarked` branch: the shell unregister
/// fails while the root is still filter-connected). Best-effort on the filter
/// half — an already-gone or unknown-path registration is fine, the shell
/// entry is the part Explorer keeps rendering.
fn unregister_shell_root(r: &fauna_cfapi::ShellSyncRoot) -> Result<()> {
    if let Some(p) = &r.path {
        let _ = fauna_cfapi::unregister_sync_root(p);
    }
    fauna_cfapi::unregister_sync_root_with_shell(&r.id)
}

/// Startup janitor: unregister **ghost** Fauna shell registrations — entries
/// whose folder no longer resolves on disk (deleted while the service was down,
/// or leaked by a crashed test run). Anything with a live folder is left alone,
/// which is what makes this safe on a shared dev box: a parallel session's
/// served root has a folder that exists. Best-effort — a failed unregister is
/// logged and retried at the next startup. Returns how many ghosts were removed.
pub fn sweep_ghost_shell_registrations() -> usize {
    let roots = match fauna_cfapi::list_shell_sync_roots() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "shell sync-root enumeration failed; skipping sweep");
            return 0;
        }
    };
    let mut removed = 0;
    for r in roots {
        let is_ghost = match &r.path {
            None => true, // the OS can no longer resolve the folder
            Some(p) => !Path::new(p).exists(),
        };
        if !is_ghost {
            continue;
        }
        match unregister_shell_root(&r) {
            Ok(()) => {
                tracing::info!(id = %r.id, path = ?r.path, "swept ghost shell sync root");
                removed += 1;
            }
            Err(e) => tracing::warn!(id = %r.id, error = %e, "ghost shell unregister failed"),
        }
    }
    removed
}

/// Uninstall-time cleanup: unregister **every** Fauna shell sync-root
/// registration on this machine, live folder or not — unlike
/// [`sweep_ghost_shell_registrations`] (which only touches registrations whose
/// folder is gone), this removes ALL of them, because the whole product is
/// going away and no folder's binding should survive uninstall. Un-hydrated
/// cloud-only placeholders are removed by the OS as part of unregistering
/// (measured); hydrated (fully downloaded) files are ordinary files already
/// and are left exactly as they are. Invoked by `fauna-sync-agent.exe
/// --cleanup-roots`, which the MSI's `CleanSyncRoots` uninstall custom action
/// runs (`docs/goal/behavior/file-sync.md` § Per-file sync-status display).
/// Best-effort — a failed unregister is logged, never fatal (an MSI uninstall
/// must not fail over a shell registration). Returns how many were removed.
pub fn unregister_all_shell_sync_roots() -> usize {
    let roots = match fauna_cfapi::list_shell_sync_roots() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "shell sync-root enumeration failed; skipping cleanup");
            return 0;
        }
    };
    let mut removed = 0;
    for r in roots {
        match unregister_shell_root(&r) {
            Ok(()) => {
                tracing::info!(id = %r.id, path = ?r.path, "unregistered sync root at uninstall");
                removed += 1;
            }
            Err(e) => tracing::warn!(id = %r.id, error = %e, "uninstall shell unregister failed"),
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// Path mapping
// ---------------------------------------------------------------------------
//
// `normalized_path_to_rel` (full Windows path → forward-slash, folder-relative
// path) lives in `crate::path_map` (cross-platform — shared with the IPC
// overlay-status query path) and is imported above.

/// Read a NUL-terminated wide string from a cfapi callback into a `String`.
///
/// # Safety
/// `p` must be null or a valid NUL-terminated wide string owned by the caller
/// (cfapi owns the callback's path buffers for the callback's duration).
unsafe fn pcwstr_to_string(p: windows::core::PCWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { p.to_string().unwrap_or_default() }
}

// ---------------------------------------------------------------------------
// cfapi completion sinks (wrap CfExecute; called from the driving thread)
// ---------------------------------------------------------------------------

/// Completes a FETCH_DATA operation via `CfExecute(TRANSFER_DATA)`. Carries only
/// `Send` primitives (the connection key is a `repr(transparent)` `i64`), so it
/// crosses from the callback thread to the driving thread inside a command.
struct CfApiTransferSink {
    conn_key: CF_CONNECTION_KEY,
    transfer_key: i64,
    /// The originating callback's `RequestKey` — cfapi correlates the completion
    /// with the request through it; a zero here is rejected outright.
    request_key: i64,
}

impl TransferSink for CfApiTransferSink {
    fn transfer_data(&self, offset: i64, data: &[u8]) -> Result<()> {
        fauna_cfapi::transfer_data(
            &self.conn_key,
            self.transfer_key,
            self.request_key,
            data,
            offset,
        )
    }
    fn transfer_failed(&self) -> Result<()> {
        fauna_cfapi::transfer_failed(
            &self.conn_key,
            self.transfer_key,
            self.request_key,
            STATUS_UNSUCCESSFUL,
        )
    }
}

/// Completes a FETCH_PLACEHOLDERS operation via
/// `CfExecute(TRANSFER_PLACEHOLDERS)`.
struct CfApiPlaceholderSink {
    conn_key: CF_CONNECTION_KEY,
    transfer_key: i64,
    /// See [`CfApiTransferSink::request_key`].
    request_key: i64,
    /// Folder-relative path of the directory being populated (`""` for the sync root).
    /// Each child's identity is this joined with the child's name — every placeholder must
    /// carry a non-empty `FileIdentity` or cfapi rejects the whole batch
    /// (`fauna_cfapi::transfer_placeholders`).
    parent_rel: String,
}

impl CfApiPlaceholderSink {
    /// The child's folder-relative path: the key `SyncDb` stores it under, and therefore
    /// the identity cfapi hands back to us on later callbacks for it.
    fn child_rel(&self, name: &str) -> String {
        if self.parent_rel.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", self.parent_rel, name)
        }
    }
}

impl PlaceholderSink for CfApiPlaceholderSink {
    /// Reports the rels cfapi provably created — each entry's own result, never the
    /// requested set — which is what the engine's seen mark rests on.
    fn transfer_placeholders(&self, children: &[DirChild]) -> Result<Vec<String>> {
        let infos: Vec<fauna_cfapi::PlaceholderInfo> = children
            .iter()
            .map(|c| fauna_cfapi::PlaceholderInfo {
                rel_path: self.child_rel(&c.name),
                size: c.size,
                mtime: c.mtime,
                is_dir: c.is_dir,
            })
            .collect();
        let transfer = fauna_cfapi::transfer_placeholders(
            &self.conn_key,
            self.transfer_key,
            self.request_key,
            &infos,
        )?;
        Ok(transfer.created)
    }
}

// ---------------------------------------------------------------------------
// The extern "system" callbacks Windows invokes
// ---------------------------------------------------------------------------

/// FETCH_DATA: Windows needs `[RequiredFileOffset, +RequiredLength)` of a
/// placeholder. Map the path to a rel and queue a [`HydrationCommand::Fetch`];
/// the driving thread hydrates and completes via the sink.
unsafe extern "system" fn fetch_data_callback(
    info: *const CF_CALLBACK_INFO,
    params: *const CF_CALLBACK_PARAMETERS,
) {
    if info.is_null() || params.is_null() {
        return;
    }
    // SAFETY: cfapi guarantees `info`/`params` valid for the callback duration.
    let info = unsafe { &*info };
    let fetch = unsafe { (*params).Anonymous.FetchData };
    let conn_key = info.ConnectionKey;
    let transfer_key = info.TransferKey;
    let request_key = info.RequestKey;
    let offset = fetch.RequiredFileOffset;
    let length = fetch.RequiredLength;
    let normalized = unsafe { pcwstr_to_string(info.NormalizedPath) };
    let volume = unsafe { pcwstr_to_string(info.VolumeDosName) };
    // NormalizedPath is volume-relative (no drive letter) under
    // REQUIRE_FULL_FILE_PATH; rejoin VolumeDosName so the prefix match against the
    // drive-lettered sync root hits (see path_map::callback_full_path).
    let full = callback_full_path(&volume, &normalized);

    let Some((cmd_tx, root)) = current_ctx(conn_key.0) else {
        tracing::warn!("FETCH_DATA with no callback context");
        return;
    };
    let sink = CfApiTransferSink {
        conn_key,
        transfer_key,
        request_key,
    };
    match normalized_path_to_rel(&root, &full) {
        Some(rel) => {
            let _ = cmd_tx.send(HydrationCommand::Fetch {
                rel,
                offset,
                length,
                sink: Box::new(sink),
            });
        }
        None => {
            tracing::warn!(full, "FETCH_DATA path not under sync root; failing");
            let _ = sink.transfer_failed();
        }
    }
}

/// FETCH_PLACEHOLDERS: Windows browsed a not-fully-populated directory and needs
/// its children. Map the directory path to a parent rel and queue a
/// [`HydrationCommand::Populate`]; the driving thread lists + completes.
unsafe extern "system" fn fetch_placeholders_callback(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    if info.is_null() {
        return;
    }
    // SAFETY: cfapi guarantees `info` valid for the callback duration. The
    // FETCH_PLACEHOLDERS `Pattern` filter is ignored — under
    // `CF_POPULATION_POLICY_FULL` we return all children and the platform filters.
    let info = unsafe { &*info };
    let conn_key = info.ConnectionKey;
    let transfer_key = info.TransferKey;
    let request_key = info.RequestKey;
    let normalized = unsafe { pcwstr_to_string(info.NormalizedPath) };
    let volume = unsafe { pcwstr_to_string(info.VolumeDosName) };
    // NormalizedPath is volume-relative (no drive letter) under
    // REQUIRE_FULL_FILE_PATH; rejoin VolumeDosName so the prefix match against the
    // drive-lettered sync root hits (see path_map::callback_full_path).
    let full = callback_full_path(&volume, &normalized);

    let Some((cmd_tx, root)) = current_ctx(conn_key.0) else {
        tracing::warn!("FETCH_PLACEHOLDERS with no callback context");
        return;
    };
    // The sink needs `parent_rel` to build each child's FileIdentity, so resolve it first.
    match normalized_path_to_rel(&root, &full) {
        Some(parent_rel) => {
            let sink = CfApiPlaceholderSink {
                conn_key,
                transfer_key,
                request_key,
                parent_rel: parent_rel.clone(),
            };
            let _ = cmd_tx.send(HydrationCommand::Populate {
                parent_rel,
                sink: Box::new(sink),
            });
        }
        None => {
            tracing::warn!(full, "FETCH_PLACEHOLDERS path not under sync root; empty");
            // No children to name, so the parent rel is irrelevant here.
            let sink = CfApiPlaceholderSink {
                conn_key,
                transfer_key,
                request_key,
                parent_rel: String::new(),
            };
            let _ = sink.transfer_placeholders(&[]);
        }
    }
}

/// CANCEL_FETCH_DATA: the user/system cancelled an in-flight fetch. We can't
/// abort the async download mid-stream; the pending `TRANSFER_DATA` for the
/// cancelled transfer key fails benignly. Log only.
unsafe extern "system" fn cancel_callback(
    _info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    tracing::debug!("cfapi FETCH_DATA cancelled");
}

/// NOTIFY_DEHYDRATE_COMPLETION: the OS *has already* freed a placeholder's local
/// bytes (Explorer's native "Free up space" / Storage Sense). A pure post-hoc
/// notification — it needs no acknowledgment and cannot block the dehydration, so
/// handling it can only make the row honest, never break a user freeing space.
/// Map the path to a rel and queue a [`HydrationCommand::Dehydrate`]; the driving
/// thread records the row back to `Placeholder` and pushes
/// `FileStatusChanged{CloudOnly}` so the badge is honest instead of a stale
/// `Synced`-while-empty. Reads only `info` (the params carry only the dehydrate
/// reason, which we don't need).
///
/// Fires only for dehydrations from *outside* this provider process — the Fauna
/// shell menu's own `FreeSpace` verb dehydrates in-process and records the row
/// itself (`pipe_server::handle_free_space`); cfapi suppresses callbacks for the
/// provider's own I/O. See `file-sync.md` § Per-file sync-status display →
/// *Windows OS shell-overlay carve-out* for the measured cfapi behavior and the
/// live-box validation follow-on.
unsafe extern "system" fn dehydrate_completion_callback(
    info: *const CF_CALLBACK_INFO,
    _params: *const CF_CALLBACK_PARAMETERS,
) {
    if info.is_null() {
        return;
    }
    // SAFETY: cfapi guarantees `info` valid for the callback duration.
    let info = unsafe { &*info };
    let conn_key = info.ConnectionKey;
    let normalized = unsafe { pcwstr_to_string(info.NormalizedPath) };
    let volume = unsafe { pcwstr_to_string(info.VolumeDosName) };
    // NormalizedPath is volume-relative (no drive letter) under
    // REQUIRE_FULL_FILE_PATH; rejoin VolumeDosName so the prefix match against the
    // drive-lettered sync root hits (see path_map::callback_full_path).
    let full = callback_full_path(&volume, &normalized);

    let Some((cmd_tx, root)) = current_ctx(conn_key.0) else {
        tracing::warn!("NOTIFY_DEHYDRATE_COMPLETION with no callback context");
        return;
    };
    match normalized_path_to_rel(&root, &full) {
        Some(rel) => {
            let _ = cmd_tx.send(HydrationCommand::Dehydrate { rel });
        }
        None => {
            tracing::warn!(
                full,
                "NOTIFY_DEHYDRATE_COMPLETION path not under sync root; ignoring"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HydrationCommand;

    // ── shell-registration identity ──

    /// The `Fauna!SID!account` account segment: stable (recomputable at every
    /// teardown site with no persisted id), unique per (folder, folder)
    /// binding (so a re-bind is a different root and two concurrent runs with a
    /// same-named set on different temp dirs cannot steal each other's
    /// user-global `SyncRootManager` entry), sanitized to a registry-safe
    /// component, and insensitive to path casing / trailing separators.
    #[test]
    fn shell_account_id_is_stable_unique_and_sanitized() {
        let a = shell_account_id("docs", Path::new(r"C:\Users\u\Docs"));
        // Stable across recomputation.
        assert_eq!(a, shell_account_id("docs", Path::new(r"C:\Users\u\Docs")));
        // Windows path semantics: casing and a trailing separator don't change identity.
        assert_eq!(a, shell_account_id("docs", Path::new(r"c:\users\U\DOCS\")));
        // A different folder for the same set is a DIFFERENT binding.
        assert_ne!(a, shell_account_id("docs", Path::new(r"C:\Elsewhere\Docs")));
        // A different set on the same folder is a DIFFERENT binding.
        assert_ne!(a, shell_account_id("photos", Path::new(r"C:\Users\u\Docs")));
        // Sanitization: the OS-mandated Id format splits on `!`, and the id is a
        // registry key component — anything unsafe becomes `_`, and the folder
        // hash keeps two collapsing names ("a!b" vs "a b") distinct per folder.
        let odd = shell_account_id("my set!", Path::new(r"C:\x"));
        assert!(odd.starts_with("my_set_-"), "got {odd}");
        assert!(
            odd.chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        );
    }

    // ── full-teardown marks ──

    /// A mark is consumed by exactly one take, keyed by path: marking root A
    /// never tears down root B, and a second take (the next serve-session's
    /// drop) finds nothing.
    #[test]
    fn full_teardown_marks_are_per_root_and_consumed_once() {
        let a = Path::new(r"C:\MarkRootA");
        let b = Path::new(r"C:\MarkRootB");
        mark_root_for_full_teardown(a);
        assert!(!take_full_teardown_mark(b), "B was never marked");
        assert!(take_full_teardown_mark(a), "A's mark is present");
        assert!(
            !take_full_teardown_mark(a),
            "a mark is consumed by its take"
        );
    }

    // ── keyed-ctx routing (multi-root) ──

    /// Two registered roots, each under its own connection key, resolve to their
    /// own command channel and carry their own sync-root path; removing one (the
    /// Drop-guard teardown) leaves the other intact.
    #[test]
    fn current_ctx_routes_by_connection_key() {
        let (tx_a, _rx_a) = mpsc::unbounded_channel::<HydrationCommand>();
        let (tx_b, _rx_b) = mpsc::unbounded_channel::<HydrationCommand>();

        // Test-local connection keys unlikely to collide with anything live.
        let key_a: i64 = 0x5151_0001;
        let key_b: i64 = 0x5151_0002;

        ctx_insert(
            key_a,
            CallbackCtx {
                cmd_tx: tx_a.clone(),
                sync_root: PathBuf::from(r"C:\RootA"),
            },
        );
        ctx_insert(
            key_b,
            CallbackCtx {
                cmd_tx: tx_b.clone(),
                sync_root: PathBuf::from(r"C:\RootB"),
            },
        );

        // Key A resolves to A's own channel and A's own root path — never B's.
        let (sender_a, root_a) = current_ctx(key_a).expect("key A registered");
        assert_eq!(root_a, r"C:\RootA");
        assert!(
            sender_a.same_channel(&tx_a),
            "key A must resolve to channel A"
        );
        assert!(
            !sender_a.same_channel(&tx_b),
            "key A must NOT resolve to channel B"
        );

        // Key B resolves independently to B's own channel + root path.
        let (sender_b, root_b) = current_ctx(key_b).expect("key B registered");
        assert_eq!(root_b, r"C:\RootB");
        assert!(
            sender_b.same_channel(&tx_b),
            "key B must resolve to channel B"
        );

        // Removing A's entry (the Drop-guard teardown) drops only A's route.
        ctx_remove(key_a);
        assert!(current_ctx(key_a).is_none(), "A's route removed");
        assert!(
            current_ctx(key_b).is_some(),
            "B's route survives A's removal"
        );

        // Clean up the process-global map so other tests are unaffected.
        ctx_remove(key_b);
    }

    // ── placeholder identity ──

    /// Every placeholder must carry a non-empty `FileIdentity` or cfapi rejects the entire
    /// TRANSFER_PLACEHOLDERS batch with ERROR_CLOUD_FILE_INVALID_REQUEST — the bug that kept
    /// Windows on-demand sync from ever working. The identity is the child's folder-relative
    /// path, which is also the key its `SyncDb` row lives under, so the two agree by
    /// construction. The root case (`parent_rel == ""`) is the one that must not produce a
    /// leading slash — and it is the case every new user hits first.
    #[test]
    fn a_childs_identity_is_its_folder_relative_path() {
        let root_sink = CfApiPlaceholderSink {
            conn_key: CF_CONNECTION_KEY(0),
            transfer_key: 0,
            request_key: 0,
            parent_rel: String::new(),
        };
        assert_eq!(root_sink.child_rel("hello.txt"), "hello.txt");

        let nested = CfApiPlaceholderSink {
            conn_key: CF_CONNECTION_KEY(0),
            transfer_key: 0,
            request_key: 0,
            parent_rel: "docs/2026".to_string(),
        };
        assert_eq!(nested.child_rel("report.pdf"), "docs/2026/report.pdf");
    }

    // Path-mapping (`normalized_path_to_rel`) tests live in `crate::path_map`.
}
