//! A LIVE cfapi integration test over the **service's own** on-demand host.
//!
//! `libs/fauna-cfapi/tests/live_population.rs` proved the *primitives* against a real sync
//! root — but through the **harness's** own callback. Everything between cfapi and the engine
//! (`cfapi_host::register_and_connect` → the `extern "system"` callbacks → the keyed context
//! lookup → `CfApiPlaceholderSink`/`CfApiTransferSink` → `HydrationCommand` → the engine
//! thread's deferred `CfExecute` in [`run_hydration_loop`]) was still exercised only by a
//! human with a login and an Explorer window. This closes that gap: the **product's** path,
//! headless, in a plain `cargo test`.
//!
//! **The premise that said this was impossible is refuted.** [`crate::producer_integration`]
//! declares the full tier_3 "not headless-runnable" because serving a bound on-demand folder
//! makes the engine call `register_and_connect` → "a real Cloud Filter sync-root registration
//! needing a live cloud-filter session". It needs no such thing: a sync root registers on a
//! temp dir in milliseconds. That boundary was drawn around an assumption, not a measurement,
//! and it is what let three wrong diagnoses stack up behind one unexercised call (Track Y).
//!
//! ## What is real here, and what is faked
//!
//! Real: the cfapi sync root, the OS callbacks, the connection-key routing, the placeholder
//! sinks (and therefore the `FileIdentity` derivation that Track Y found was fatally wrong),
//! the command channel, the hydration loop, a real on-disk `SyncDb` — the same DB file the
//! IPC overlay-status handler reads, so `GetFileStatus` is asserted through the *live*
//! `handle_request`, not a stub — **and the real `SyncEngine`, including its upload path**.
//! [`DbBackedHost`] is a *decorator* over a production engine, not a lookalike.
//!
//! Faked: only the three **nest-facing** calls — `prepare`/`repull` (which fold a live
//! `changes.list`) and `download_file_bytes` (which fetches chunks over WS-RPC). Their inputs
//! are seeded directly into the `SyncDb`, exactly as `producer_integration` does. A nest is not
//! what these tests are about; the OS boundary is.
//!
//! The two-way tests are the exception, and deliberately so: an upload has nowhere to *go*
//! without a nest, so they point the engine's byte plane at a **wiremock** server and assert the
//! chunk/manifest POSTs actually land. The uploader itself is never faked — a fake uploader
//! would go green while the shipped path stayed broken, which is precisely the failure this
//! track exists to end.
//!
//! ## Two cfapi facts these tests are built on (see `file-sync.md` § On-Demand Files)
//!
//! 1. **cfapi does not fire callbacks for I/O originating in the provider's own process.** An
//!    in-process `read_dir` of the root returns empty with no callback — indistinguishable from
//!    "the root was never populatable". Both tests therefore drive the enumeration and the read
//!    from **another process** (`cmd`), exactly as Explorer would.
//! 2. **Registration alone makes the root on-demand-populatable.** Nothing converts it.
//!
//! ## Timing
//!
//! The read-side tests are ~0.5 s. The two **two-way** tests are a few seconds each and cannot
//! be made instant: they wait out the shared uploader's real 2 s debounce on a real clock (the
//! watcher and the child processes driving it are not fakeable), and one of them additionally
//! waits *past* the debounce to prove a hydration is **not** echoed back up — an assertion that
//! is only meaningful after the window in which the bug would have fired.
//!
//! A **regression** is slow, not hung: cfapi's per-request timeout is 60 s, so a host that stops
//! answering makes each browsing/reading child process wait it out (a sabotaged run of the guard
//! tests took ~300 s and still failed cleanly). That is the cost of asserting against the real
//! OS, and it is worth it — the alternative is what Track Y actually cost, which was three
//! sessions.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::{broadcast, mpsc, watch};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use fauna_core::data::ContentHash;
use fauna_ipc::sync::{
    BearerToken, Event, EventKind, FileStatus, Request, RequestMethod, ResponsePayload,
    ResponseResult, SyncCapability,
};
use fauna_sync_engine::FileHydrator;
use fauna_sync_engine::always_resident::{EngineCommand, LocalWriteHost};
use fauna_sync_engine::db::{SyncDb, SyncState};
use fauna_sync_engine::engine::{PlaceholderFold, StaleHydratedRow};
use fauna_sync_engine::engine_host::CancellationToken;
use fauna_sync_engine::enumerate::{PlaceholderLister, PlaceholderRow};

use crate::bridge::{
    CfapiInvalidator, HydrationHost, agent_control_plane, agent_engine_params,
    flip_recorded_in_sync, run_hydration_loop,
};
use crate::config::{SyncConfig, SyncPaths};
use crate::pipe_server::handle_request;
use crate::state::SyncServiceState;

/// Cross-process critical section for tests that shell-register a Fauna! entry
/// visible to `fauna_cfapi::list_shell_sync_roots` — the machine-wide
/// `SyncRootManager` registry state, not scoped to any one test's tempdir, any
/// process, or any local checkout. Two `cargo test` runs from *different*
/// checkouts on the same Windows box — or two threads inside one test binary,
/// since the harness runs tests concurrently by default — can otherwise sweep
/// each other's live registration mid-test via
/// `uninstall_cleanup_removes_ghost_and_live_roots_alike`'s unconditional (not
/// ghost-scoped) cleanup. Same pattern as `fauna-shell-ext`'s HKCU registry
/// guard (a NAMED kernel mutex — a same-process `std::sync::Mutex` isn't
/// enough for machine-wide state) — both now share `fauna_ipc::test_support`.
/// Every AUTOMATED test that calls `register_sync_root_with_shell` /
/// `register_and_connect_shell` MUST hold this for the registration's
/// lifetime (a test that only calls filter-level `register_sync_root` needs
/// no lock — it never appears in `list_shell_sync_roots`'s enumeration).
/// [`hold_a_live_root_for_explorer`] is the deliberate exception: it holds a
/// root for up to five MINUTES for a human to drive Explorer against, and
/// taking this lock for that whole window would stall every other checkout's
/// cfapi suite on the box — its `#[ignore]` + human-supervised, solo-run
/// nature is what keeps its (conditional, `FAUNA_HOLD_SHELL=1`-gated) shell
/// registration safe without it.
fn shell_sync_root_test_guard() -> fauna_ipc::test_support::NamedMutexTestGuard {
    fauna_ipc::test_support::NamedMutexTestGuard::acquire("FaunaCfapiShellSyncRootTest")
}

/// The folder every test binds its sync root to.
const FOLDER: &str = "docs";
/// The bound set's identity: the agent keys its engine and state DB by it.
const FOLDER_REF: fauna_core::folder_keys::FolderRef = fauna_core::folder_keys::FolderRef::Local(1);

/// A [`HydrationHost`] that is a **decorator over a real production [`SyncEngine`]**, faking
/// only the three calls that would otherwise need a live nest: `prepare`, `repull` (which fold
/// a live `changes.list`) and `download_file_bytes` (which fetches chunks over WS-RPC).
///
/// Everything else — `mark_hydrated`, `repoint_placeholder`, `list_placeholder_rows`, and the
/// whole [`LocalWriteHost`] write half (`upload_file`, `handle_delete`, `converge`, the ignore
/// and recent-download predicates) — **delegates to the shipped engine**. So when a test here
/// asserts that a local edit was uploaded, the thing that uploaded it is the real
/// `SyncEngine::upload_file`, chunking and sealing through the real pipeline. A fake uploader
/// would go green while the shipped path stayed broken, which is the exact failure mode this
/// whole track exists to end.
///
/// It used to hand-roll `mark_hydrated` as a "deliberate twin" of the production write. The
/// twin is gone: delegating is strictly better than maintaining a lookalike that can drift.
struct DbBackedHost {
    /// The real engine, over the same sync root + `SyncDb` file the loop serves.
    engine: fauna_sync_engine::engine::SyncEngine,
    /// The sync root, so the `mark_hydrated` probe below can stat the file the engine is
    /// about to stat — the engine's own `watch_dir` is private.
    root: std::path::PathBuf,
    /// What the "nest" serves for each folder-relative path — the one thing a real
    /// `download_file_bytes` would go over WS-RPC for. Shared, so a test can move the
    /// "nest's" head under a running loop (another device's edit).
    blobs: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
    /// Kept alive (never dropped) so the loop's reconnect arm stays pending rather than
    /// erroring; no test drives a reconnect.
    reconnect: (watch::Sender<u64>, watch::Receiver<u64>),
    /// What the "nest's" `changes.list` carries on a `repull` — folded through the REAL
    /// fold (`SyncEngine::fold_changes_for_test`), so a test that nudges the loop sees a
    /// remote create land exactly as production folds it. Empty for most tests: their
    /// rows are seeded, and an empty fold writes nothing.
    fold_on_repull: Vec<fauna_protocol::sync::SyncChange>,
}

#[async_trait::async_trait(?Send)]
impl FileHydrator for DbBackedHost {
    async fn download_file_bytes(&self, relative_path: &str) -> Result<Vec<u8>> {
        self.blobs
            .lock()
            .unwrap()
            .get(relative_path)
            .cloned()
            .ok_or_else(|| anyhow!("no seeded blob for {relative_path}"))
    }
}

#[async_trait::async_trait(?Send)]
impl PlaceholderLister for DbBackedHost {
    async fn list_placeholder_rows(&self) -> Result<Vec<PlaceholderRow>> {
        self.engine.list_placeholder_rows().await
    }
}

/// The write half — **delegated wholesale to the real engine**. This is what makes
/// `a_local_edit_by_another_process_is_detected_and_uploaded` a test of the shipped uploader
/// rather than of a stub.
#[async_trait::async_trait(?Send)]
impl LocalWriteHost for DbBackedHost {
    fn is_ignored(&self, rel: &str) -> bool {
        LocalWriteHost::is_ignored(&self.engine, rel)
    }

    fn was_recent_download(&self, rel: &str) -> bool {
        LocalWriteHost::was_recent_download(&self.engine, rel)
    }

    fn was_recent_removal(&self, rel: &str) -> bool {
        LocalWriteHost::was_recent_removal(&self.engine, rel)
    }

    async fn upload_file(&self, rel: &str) -> Result<fauna_sync_engine::engine::UploadOutcome> {
        LocalWriteHost::upload_file(&self.engine, rel).await
    }

    async fn handle_delete(&self, rel: &str) -> Result<()> {
        LocalWriteHost::handle_delete(&self.engine, rel).await
    }

    async fn converge(&self, folder: &str) -> Vec<String> {
        LocalWriteHost::converge(&self.engine, folder).await
    }
}

#[async_trait::async_trait(?Send)]
impl HydrationHost for DbBackedHost {
    /// The rows are already in the DB (seeded, as a real fold would have left them), so the
    /// startup fold has nothing to add. This is the only thing standing in for a live nest.
    async fn prepare(&self) -> Result<PlaceholderFold> {
        Ok(PlaceholderFold::default())
    }

    async fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()> {
        // ── PROBE: what did the OS say at the exact instant the engine stats the file? ──
        // `SyncEngine::mark_hydrated` early-returns (state-only, NO local identity) when
        // `is_cloud_placeholder` is still true. That row shape is what makes the watcher's
        // `upload_file` re-upload a file we just downloaded, so this line makes the failure
        // state WHICH HALF is at fault instead of leaving it to be guessed.
        {
            use std::os::windows::fs::MetadataExt;
            let full = self.root.join(rel);
            match std::fs::metadata(&full) {
                Ok(m) => eprintln!(
                    "[probe] mark_hydrated({rel}): attrs=0x{:08x} is_placeholder={} len={}",
                    m.file_attributes(),
                    fauna_sync_engine::placeholder::is_cloud_placeholder(&m),
                    m.len(),
                ),
                Err(e) => eprintln!("[probe] mark_hydrated({rel}): stat failed: {e}"),
            }
        }
        let r = HydrationHost::mark_hydrated(&self.engine, rel, content_hash).await;
        eprintln!(
            "[probe] mark_hydrated({rel}) -> ok={} ; served_hash={}",
            r.is_ok(),
            content_hash,
        );
        r
    }

    async fn mark_placeholder(&self, rel: &str) -> Result<()> {
        HydrationHost::mark_placeholder(&self.engine, rel).await
    }

    async fn mark_seen(&self, rels: &[String]) -> Result<()> {
        HydrationHost::mark_seen(&self.engine, rels).await
    }

    async fn clear_seen_all(&self) -> Result<()> {
        HydrationHost::clear_seen_all(&self.engine).await
    }

    async fn repoint_placeholder(&self, row: &StaleHydratedRow) -> Result<()> {
        HydrationHost::repoint_placeholder(&self.engine, row).await
    }

    async fn repull(&self) -> Result<PlaceholderFold> {
        self.engine.fold_changes_for_test(&self.fold_on_repull)
    }

    // This harness host stands in for a live nest: its rows are seeded and its three
    // nest-facing calls are faked, so the corpus passes (the production engine's,
    // pinned in `fauna-sync-engine`'s own tests and by the flip-back e2e) have
    // nothing to converge here and would only add wiremock traffic the assertions
    // below do not expect. A deliberate no-op, owned here, never a trait default.
    async fn converge_corpus_at_start(&self, _folder: &str) {}

    async fn refresh_and_converge_corpus(&self, _folder: &str) {}

    async fn answer_engine_command(&self, cmd: EngineCommand) {
        HydrationHost::answer_engine_command(&self.engine, cmd).await
    }

    /// Long enough that the rescan tick never fires inside a test: these tests run on a
    /// **real** clock (a blocking child process is what drives them), so a paused-clock
    /// auto-advance would be wrong here.
    async fn rescan_interval(&self) -> Duration {
        Duration::from_secs(3600)
    }

    fn reconnects(&self) -> watch::Receiver<u64> {
        self.reconnect.1.clone()
    }

    async fn subtree_fully_synced(&self, dir_rel: &str) -> bool {
        self.engine.subtree_fully_synced(dir_rel).unwrap_or(false)
    }

    /// The real engine's gate over the real `SyncDb` — so the live pin-reaction
    /// tests exercise the shipped clean-predicate, not a fake's.
    async fn is_dehydration_safe(&self, rel: &str) -> bool {
        self.engine.is_dehydration_safe(rel)
    }
}

/// A live sync root under a temp dir, with the service state + DB the real handlers read.
struct Harness {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    db_path: PathBuf,
    state: Arc<SyncServiceState>,
    /// Where this harness's engines point their **byte plane**.
    ///
    /// The guard tests keep the default `http://127.0.0.1:1` — a port nothing listens on,
    /// deliberately, because every claim they make is that *nothing reaches the nest*, and a
    /// reconcile that reaches for one on a placeholder root **is itself the bug under test**.
    /// The upload tests point it at a wiremock nest instead, because there their claim is the
    /// opposite: the bytes really must arrive.
    nest_base_url: String,
}

/// Build the world a served on-demand folder lives in: a `--data-dir`-scoped [`SyncPaths`],
/// the folder added + bound + set on-demand through the **real** IPC handlers, and a real
/// per-folder `SyncDb` seeded with `rows` as `Placeholder`s (what a nest fold leaves behind).
///
/// No capability is provisioned, so `reconcile_engines` starts nothing — this test drives the
/// hydration host itself, rather than racing the driver for the same sync root.
async fn harness(rows: &[(&str, i64)]) -> Harness {
    harness_against_nest(rows, "http://127.0.0.1:1").await
}

/// The nest head manifest a fold would have recorded for `rel` — a deterministic stand-in.
///
/// **Why this may never be `None`, and why that cost a whole session (2026-08-04).**
/// `record_placeholders_from_changes` writes `Some(manifest_hash)` — *"the hydration anchor"* —
/// on **every** placeholder row it records, 0-byte files included; `download_file_bytes`
/// resolves the manifest to fetch from that very field, so a `None`-manifest placeholder could
/// not hydrate at all in production. This harness fakes `download_file_bytes`, which is the
/// only reason a `None` here was survivable — and it quietly broke a *different* contract:
/// `upload_file`'s skip is `local_hash == file_hash && Synced` **and**
/// `if let Some(recorded_manifest) = entry.manifest_hash && recorded_content_hash == Some(..)`,
/// because the skip must return that manifest as the recorded head. With `None` the `if let`
/// fails, the skip is missed, and the row falls through to *"synced row without a recorded-head
/// proof — re-driving upload + record"*.
///
/// That is what made `a_local_edit_by_another_process_is_detected_and_uploaded` fail
/// `left: 1, right: 0` from the ack-honesty pass that added the manifest
/// conjunct onward. It was reported and hoisted as an **engine regression** in the
/// `mark_hydrated`/`recorded-head-proof` skip, and chased as one across sessions; the engine
/// was innocent the whole time — `mark_hydrated` stamps the served identity correctly, as its
/// probe below prints. The defect was this seed writing a row shape production cannot produce.
/// Keep it `Some`.
fn seeded_manifest_hash(rel: &str) -> ContentHash {
    ContentHash::of_raw(format!("seeded-nest-head-manifest:{rel}").as_bytes())
}

/// [`harness`], but pointing the engines' byte plane at `nest_base_url` — a wiremock nest for
/// the tests that must prove an upload really left the machine.
async fn harness_against_nest(rows: &[(&str, i64)], nest_base_url: &str) -> Harness {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("create sync root");

    let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
    let (event_tx, _event_rx) = broadcast::channel(64);
    let state = SyncServiceState::new(SyncConfig::default(), shutdown_tx, event_tx, paths.clone());

    let folder = root.to_string_lossy().to_string();
    for (id, method) in [
        (
            1u64,
            RequestMethod::AddLocation {
                path: folder.clone(),
            },
        ),
        (
            2,
            RequestMethod::SetLocationFolder {
                path: folder.clone(),
                folder: FOLDER.into(),
                folder_id: FOLDER_REF.to_wire(),
            },
        ),
        (
            3,
            RequestMethod::SetLocationSyncMode {
                path: folder.clone(),
                mode: "on-demand".into(),
            },
        ),
    ] {
        let res = handle_request(&Request { id, method }, &state).await.result;
        assert!(
            matches!(res, ResponseResult::Ok(ResponsePayload::Empty)),
            "handler {id} failed: {res:?}"
        );
    }

    // Seed the rows a nest fold would have written, at the same path the production query
    // path resolves through. Dropped before the host opens its own connection.
    //
    // ⚠ The `manifest_hash` is **`Some` on every row, and that is load-bearing** — see
    // `seeded_manifest_hash`. Seeding `None` here (as this harness did until 2026-08-04)
    // writes a row shape `record_placeholders_from_changes` can never produce, and it
    // silently defeats `upload_file`'s "already synced and recorded, skipping" short-circuit,
    // which needs the manifest to return as the recorded head.
    let db_path = paths.sync_db_path_for_ref(FOLDER_REF);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
    {
        let db = SyncDb::open(&db_path).expect("open per-folder db");
        for (rel, size) in rows {
            db.upsert_entry(
                rel,
                None,
                None,
                Some(seeded_manifest_hash(rel)), // the hydration anchor — never None
                SyncState::Placeholder,
                0,
                1_700_000_000, // Unix SECONDS — the units Track X pinned
                *size,
                1,
                None,
            )
            .expect("seed placeholder row");
        }
    }

    Harness {
        _tmp: tmp,
        root,
        db_path,
        state,
        nest_base_url: nest_base_url.to_string(),
    }
}

impl Harness {
    /// The host that will serve this root: a **real production engine** over the seeded DB
    /// (so the write half is the shipped one), plus whatever bytes the "nest" should answer a
    /// fetch with.
    fn host(&self, blobs: HashMap<String, Vec<u8>>) -> DbBackedHost {
        DbBackedHost {
            engine: self.engine(),
            root: self.root.clone(),
            blobs: Arc::new(std::sync::Mutex::new(blobs)),
            reconnect: watch::channel(0),
            fold_on_repull: Vec::new(),
        }
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    /// The overlay status the **live** IPC handler reports for `rel` — the same
    /// `GetFileStatus` call Explorer's shell extension makes.
    async fn overlay_status(&self, rel: &str, id: u64) -> FileStatus {
        let path = format!("{}\\{}", self.root_str(), rel.replace('/', "\\"));
        let res = handle_request(
            &Request {
                id,
                method: RequestMethod::GetFileStatus { path },
            },
            &self.state,
        )
        .await
        .result;
        match res {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => info.status,
            other => panic!("expected FileStatus payload, got {other:?}"),
        }
    }

    /// Re-read a row's state straight from the DB file (a second connection — WAL allows it
    /// while the host holds its own).
    fn db_state(&self, rel: &str) -> Option<SyncState> {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .get_entry(rel)
            .expect("get entry")
            .map(|e| e.state)
    }

    /// Re-read a row's recorded head size straight from the DB file.
    fn db_size(&self, rel: &str) -> Option<i64> {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .get_entry(rel)
            .expect("get entry")
            .map(|e| e.size_bytes)
    }

    /// Re-read a row's seen mark (`delete-propagation.md` § *An offline placeholder
    /// delete propagates*, decision (a)) straight from the DB file.
    fn db_seen(&self, rel: &str) -> bool {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .get_entry(rel)
            .expect("get entry")
            .is_some_and(|e| e.seen_on_disk)
    }

    /// Force a row's state directly in the DB — a seam for arranging a state without driving the
    /// real transition. Used to stage a `Synced`-row-over-a-placeholder-on-disk (a transient
    /// window: the row before its `NOTIFY_DEHYDRATE_COMPLETION` flip lands, or a never-hydrated
    /// row forced `Synced`) so the reconcile delete-guard can be exercised against it.
    fn force_state(&self, rel: &str, state: SyncState) {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .update_state(rel, state)
            .expect("force state");
    }

    /// A **real production** [`SyncEngine`] over this root + DB — assembled by the shared
    /// builder's construction half (`engine_lifecycle::assemble_engine`) from the very
    /// inputs the driver hands `build_engine` (`agent_engine_params`), minus the row read
    /// (owner-only, same-nest), so these tests assert against the shipped engine and not a
    /// lookalike. There is no role to pass: both roots build the same engine (both watch,
    /// both upload, both honour `.faunaignore`).
    fn engine(&self) -> fauna_sync_engine::engine::SyncEngine {
        // A provisioned capability, because `CapabilityBearer` yields NO bearer without one
        // and every nest-bound request would then fail inside the engine before it ever hit
        // the wire. The guard tests don't care (nothing is supposed to reach the nest); the
        // upload tests would silently "pass" for the wrong reason without it.
        let capability = crate::bearer::CapabilitySlot::new(Some(SyncCapability::new(
            vec![9u8; 32],
            vec![7u8; 32],
            self.nest_base_url.clone(),
            "cfapi-live-test".into(),
            BearerToken::new("test.bearer".into(), 4_000_000_000),
        )));
        fauna_sync_engine::engine_lifecycle::assemble_engine(
            agent_engine_params(
                agent_control_plane(capability, self.nest_base_url.clone(), [9u8; 32]),
                // `db_path` is `FOLDER_REF`'s state DB under the data-root.
                self.db_path
                    .parent()
                    .expect("the state DB sits under the data-root")
                    .to_path_buf(),
                self.root.clone(),
                FOLDER_REF,
                [7u8; 32],
                [3u8; 32],
                &crate::bridge::AgentPredecessors::default(), // no succession in this fixture
                None,                                         // progress_tx
                fauna_sync_engine::access_gate::AccessGate::new(),
                None, // change_signer
                std::sync::Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
            ),
            fauna_sync_engine::engine_lifecycle::ResolvedBinding {
                folder: FOLDER.to_string(),
                ..Default::default()
            },
        )
        .expect("build the production SyncEngine")
        .engine
    }

    /// The row's recorded **local content identity** — what both upload paths compare against
    /// to decide "did this change locally?". A hydrated row carries the served content's hash
    /// (that is what stops the download→upload echo); an uploaded row carries the new bytes'.
    fn db_local_hash(&self, rel: &str) -> Option<ContentHash> {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .get_entry(rel)
            .expect("get entry")
            .and_then(|e| e.local_hash)
    }

    /// **Has `rel` finished uploading, with `want` as its content?**
    ///
    /// Both halves are load-bearing, and getting this wrong makes the test lie. `upload_file`
    /// writes `local_hash` at its **`Uploading`** upsert — *before* a single byte reaches the
    /// wire — and only flips to `Synced` after the chunk+manifest POSTs have all succeeded. So
    /// a hash-only check goes green on a **failed** upload, which is precisely the false pass a
    /// two-way-sync test must never give: it would report "your edit was uploaded" for an edit
    /// still sitting on the disk. `Synced` + the new hash is the terminal state, and the only
    /// honest one.
    fn db_uploaded(&self, rel: &str, want: ContentHash) -> bool {
        self.db_state(rel) == Some(SyncState::Synced) && self.db_local_hash(rel) == Some(want)
    }
}

/// Enumerate the folder **from another process** — cfapi suppresses callbacks for the
/// provider's own I/O, so an in-process `read_dir` would prove nothing (`file-sync.md`
/// § On-Demand Files, the 2026-07-13 correction).
///
/// `pub(crate)`: also the restore path's `restore_byteplane_tier3`'s copy of this
/// exact function (whose own gate is a strict subset of this module's — `tier3-nest`
/// added on top of `test, windows` — so it is always compiled when this is).
pub(crate) fn dir_from_another_process(root: &str) -> Vec<String> {
    let out = std::process::Command::new("cmd")
        .args(["/c", "dir", "/b", root])
        .output()
        .expect("spawn dir");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// **Write a file from another process** — the user's edit, as the OS sees it.
///
/// This must not be an in-process `std::fs::write`: on a cfapi root the provider's own I/O is
/// invisible to the OS's own notifications, and it is *precisely* the "another program wrote
/// my file" case that the two-way root exists to catch. `copy /y /b` from a scratch file
/// outside the root gives byte-exact content with no console-encoding games (an `echo`+redirect
/// gets mangled by Rust's `Command` quoting).
fn write_from_another_process(scratch: &std::path::Path, dst: &std::path::Path, content: &[u8]) {
    std::fs::write(scratch, content).expect("write the scratch source");
    let out = std::process::Command::new("cmd")
        .args([
            "/c",
            "copy",
            "/y",
            "/b",
            &scratch.to_string_lossy(),
            &dst.to_string_lossy(),
        ])
        .output()
        .expect("spawn copy");
    assert!(
        out.status.success(),
        "writing {} from another process failed: {}",
        dst.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A stand-in nest **byte plane**: the chunk/manifest endpoints `SyncEngine::upload_file`
/// POSTs to. Returns the counter of chunk-upload POSTs — the observable that says *the bytes
/// really left this machine*, which is the whole claim of a two-way root.
///
/// ⚠ **Deliberately NOT lifted onto `fauna_sync_engine::test_support::MockNest`**, the shared
/// builder the other eight copies of this shape now delegate to.
/// Two reasons, and the second is the load-bearing one:
///
/// 1. This module is `#[cfg(all(test, windows))]` (`lib.rs`), so no non-Windows session can
///    compile — let alone green — a change to it. The dependency edge is not the obstacle:
///    this crate already deps `fauna-sync-engine` (Cargo.toml `[dependencies]`), so reaching
///    the builder would need only a **dev**-dependency re-declaration naming `test-helpers`
///    (rule (b) forbids naming it on the normal dep line).
/// 2. `docs/goal/behavior/on-demand-files.md` § Headless-first testing rules that this
///    wiremock-nest playbook "should eventually lift onto the same real-nest shape" as
///    `bins/fauna-nest/tests/conformance_*_client.rs`. Investing it in a *wiremock* builder
///    now would move it away from where its owning goal doc says it is going.
///
/// So it stays its own thing on purpose. A session that later executes the real-nest lift
/// deletes this helper rather than porting it.
///
/// The **control plane** (`changes.record`, WS-RPC) stays unconnected and fails-and-logs, so
/// `upload_file` still completes — the same arrangement `fauna-sync-engine`'s
/// `upload_thumbnail_test` runs under.
async fn mount_nest_byte_plane(server: &MockServer) -> Arc<AtomicUsize> {
    // POST /chunks/check → report every requested hash as missing, so the upload actually
    // transfers rather than dedup'ing to a no-op (which would make a broken uploader look fine).
    Mock::given(method("POST"))
        .and(path("/api/v1/chunks/check"))
        .respond_with(|req: &wiremock::Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let missing: Vec<String> = body
                .get("hashes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|h| h.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "missing": missing }))
        })
        .mount(server)
        .await;

    let chunk_posts = Arc::new(AtomicUsize::new(0));
    {
        let chunk_posts = Arc::clone(&chunk_posts);
        Mock::given(method("POST"))
            .and(path("/api/v1/chunks"))
            .respond_with(move |_req: &wiremock::Request| {
                chunk_posts.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200)
            })
            .mount(server)
            .await;
    }

    Mock::given(method("POST"))
        .and(path("/api/v1/manifests"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;

    // Thumbnails ride the same upload; a text file never produces one, but a mount keeps a
    // stray POST from 404-ing into a confusing failure.
    Mock::given(method("POST"))
        .and(path("/api/v1/blob"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({ "hash": "ab".repeat(32) })),
        )
        .mount(server)
        .await;

    chunk_posts
}

/// Drive an **uploading** test: poll the loop and the driver together, and stop as soon as the
/// driver's claims are all made — rather than waiting for the loop to wind down.
///
/// The other tests here `tokio::join!` the two, which is right for them: their loops go idle the
/// moment they are cancelled. An *upload* loop does not. After the byte plane has taken the
/// chunks (which is what these tests assert), `upload_file` goes on to `record_change` over the
/// **WS-RPC control plane** — and this harness has no live one: its nest is a wiremock HTTP
/// server, which cannot answer a WebSocket, so the client spends its whole retry budget (~30 s)
/// discovering that. Joining would make every run pay that, for a call whose failure is expected,
/// already logged, and deliberately not fatal to `upload_file` (in production the control plane
/// *is* connected — `prepare()` opens it — so this is purely an artifact of mocking the nest).
///
/// So: run both, finish with the driver, drop the loop. Nothing is masked — a genuinely stuck
/// upload still fails, loudly, on [`eventually`]'s own 30 s bound.
async fn drive_until_asserted(
    loop_fut: impl Future<Output = ()>,
    driver: impl Future<Output = ()>,
) {
    tokio::select! {
        _ = loop_fut => panic!("the hydration loop returned before the test finished asserting"),
        _ = driver => {}
    }
}

/// Poll `pred` on the **real** clock until it holds, or panic with `what`.
///
/// These tests cannot pause time: child processes and a filesystem watcher drive them, and the
/// shared uploader debounces a write for [`DEBOUNCE_DELAY`](fauna_sync_engine::always_resident::DEBOUNCE_DELAY)
/// (2 s) before it uploads. Awaiting here (rather than blocking) is what keeps the hydration
/// loop being polled on this thread while we wait.
async fn eventually(mut pred: impl FnMut() -> bool, within: Duration, what: &str) {
    let deadline = std::time::Instant::now() + within;
    loop {
        if pred() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out after {within:?} waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Read a placeholder **from another process**, which is what makes Windows fire FETCH_DATA.
/// `copy /b` gives us the exact bytes with no console-encoding games; the destination is
/// outside the sync root, so reading it back is an ordinary file read.
///
/// `pub(crate)`: also the restore path's `restore_byteplane_tier3`'s copy of this
/// exact function (whose own gate is a strict subset of this module's — `tier3-nest`
/// added on top of `test, windows` — so it is always compiled when this is).
pub(crate) fn read_from_another_process(src: &str, dst: &std::path::Path) -> Vec<u8> {
    let status = std::process::Command::new("cmd")
        .args(["/c", "copy", "/b", src, &dst.to_string_lossy()])
        .output()
        .expect("spawn copy");
    assert!(
        status.status.success(),
        "copy of the placeholder failed (the OS could not hydrate it): {}",
        String::from_utf8_lossy(&status.stderr)
    );
    std::fs::read(dst).expect("read the hydrated copy")
}

/// True when the file's bytes are **not on this disk** — the OS's own view, read through the
/// same shared guard the scan and reconcile paths use. Stat-only: it reads attributes, never
/// data, so it cannot itself trigger a recall.
///
/// This is what keeps the dehydrate assertions from passing vacuously: "the row did not flip"
/// proves nothing unless the bytes actually went away.
fn bytes_are_freed(path: &str) -> bool {
    let meta = std::fs::metadata(path).expect("stat the placeholder");
    fauna_sync_engine::placeholder::is_cloud_placeholder(&meta)
}

/// Env var carrying the placeholder path to [`helper_dehydrates_a_placeholder_out_of_process`].
const DEHYDRATE_HELPER_PATH_ENV: &str = "FAUNA_TEST_DEHYDRATE_PATH";

/// The libtest name of the helper, as this binary's own test filter sees it.
const DEHYDRATE_HELPER_TEST: &str =
    "cfapi_live_integration::helper_dehydrates_a_placeholder_out_of_process";

/// Env var selecting which call [`helper_dehydrates_a_placeholder_out_of_process`] makes.
const DEHYDRATE_HELPER_MODE_ENV: &str = "FAUNA_TEST_DEHYDRATE_MODE";

/// Run the helper in `mode` **from another process**, bounded.
///
/// Another process, because cfapi fires no `NOTIFY_*` for I/O originating in the provider's own
/// process — the same rule that makes the populate/hydrate tests drive `dir`/`copy` from `cmd`.
/// A re-exec is needed rather than a shell command because nothing in `cmd` dehydrates on
/// demand: `attrib +U` only *unpins*, leaving the bytes until Storage Sense feels disk pressure.
///
/// **Bounded**, because libtest has no timeout: an unbounded wait on a child that hangs wedges
/// the entire suite forever and reports nothing — which is exactly what one arm here does.
///
/// **`spawn_blocking` this.** Every child-process driver in this module is, because the child
/// can wait on cfapi, which waits on [`run_hydration_loop`] being polled on this test's
/// single-threaded runtime (see `dir_from_another_process`). Note that this is *necessary but
/// not sufficient* for the `dehydrate` arm: it hangs even with the loop fully polled, so loop
/// starvation is **not** the cause of that hang — see [`diag_cross_process_dehydrate`].
fn run_helper(path: &str, mode: &str, within: Duration) -> Result<(), String> {
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = std::process::Command::new(exe)
        .args([DEHYDRATE_HELPER_TEST, "--exact", "--ignored", "--nocapture"])
        .env(DEHYDRATE_HELPER_PATH_ENV, path)
        .env(DEHYDRATE_HELPER_MODE_ENV, mode)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the out-of-process helper");

    let deadline = std::time::Instant::now() + within;
    loop {
        match child.try_wait().expect("try_wait the helper") {
            Some(status) if status.success() => return Ok(()),
            Some(status) => {
                let out = child.wait_with_output().expect("helper output");
                return Err(format!(
                    "helper exited {status}: {} {}",
                    String::from_utf8_lossy(&out.stdout).replace('\n', " "),
                    String::from_utf8_lossy(&out.stderr).replace('\n', " "),
                ));
            }
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("HUNG (no return within {within:?})"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Not a test — the **body** [`run_helper`] re-execs, which is why it is `#[ignore]`d out of
/// ordinary runs. Without the env var it no-ops, so a stray `--include-ignored` cannot fail.
#[test]
#[ignore = "helper body re-exec'd by run_helper; not a test on its own"]
fn helper_dehydrates_a_placeholder_out_of_process() {
    let Ok(path) = std::env::var(DEHYDRATE_HELPER_PATH_ENV) else {
        return;
    };
    let p = std::path::Path::new(&path);
    match std::env::var(DEHYDRATE_HELPER_MODE_ENV)
        .unwrap_or_default()
        .as_str()
    {
        // An ordinary CreateFile — the control: does *any* cross-process open block?
        "plain-open" => {
            let _f = std::fs::File::open(p).expect("plain File::open");
        }
        // Just the oplock open. The handle leaks deliberately: `close_handle` is private to
        // fauna-cfapi, and this process exits immediately, so Windows reclaims it.
        "open" => {
            let _h = fauna_cfapi::open_file_handle(p).expect("CfOpenFileWithOplock");
        }
        // What Explorer's "Free up space" plausibly does: mark unpinned, let cldflt dehydrate.
        "unpin" => fauna_cfapi::set_pin_state(p, false).expect("CfSetPinState(UNPINNED)"),
        // What Explorer's "Always keep on this device" does (measured 2026-07-16).
        "pin" => fauna_cfapi::set_pin_state(p, true).expect("CfSetPinState(PINNED)"),
        _ => fauna_cfapi::dehydrate_placeholder(p).expect("CfDehydratePlaceholder"),
    }
}

/// **The measurement probe for "can anything out-of-process dehydrate a Fauna placeholder?"** —
/// re-measure in ~20 s instead of re-deriving from prose:
///
/// ```text
/// cmd /c "scripts\cargo-win.cmd test -p fauna-sync-agentdiag_cross_process_dehydrate -- --ignored --nocapture"
/// ```
///
/// It reports rather than asserts, because its subject is **OS behavior**, not our code: an
/// assertion here would pin Windows, and the whole point is to notice if Windows ever changes.
/// It is `#[ignore]`d because one arm deliberately waits out a hang.
///
/// **Measured 2026-07-16 on real Windows (Windows 11 ARM64), against a live connected root with a
/// hydrated file:**
///
/// | probe | result |
/// |---|---|
/// | `plain-open` — ordinary `CreateFile` | returns |
/// | `open` — `CfOpenFileWithOplock` | **returns** |
/// | `unpin` — `CfSetPinState(UNPINNED)` | returns; does **not** dehydrate (bytes stay) |
/// | `dehydrate` — `CfDehydratePlaceholder` | **hangs indefinitely** (no return in 120 s) |
///
/// Two of those correct the record. The hang is in `CfDehydratePlaceholder` itself — **not** in
/// `CfOpenFileWithOplock`, which was recorded as the blocker and which measurably
/// returns. And the hang is **not** runtime starvation: `run_helper` is `spawn_blocking`-ed, so
/// [`run_hydration_loop`] is polled throughout. It is also not cfapi's 60 s recall timeout — at
/// 120 s there is still no return, so cldflt is waiting on something that never comes rather
/// than expiring.
///
/// What that does *not* license: concluding the OS gates third-party dehydration on a provider
/// ACK. That was inferred once already from the mis-attributed `CfOpenFileWithOplock` hang, and
/// the reverted `NOTIFY_DEHYDRATE`+ACK handler is not evidence either way — nothing establishes
/// that Explorer's *"Free up space"* even takes this call path. That is a live-box question, and
/// it is the open one.
#[tokio::test]
#[ignore = "measurement probe; one arm waits out a hang. Run with --ignored --nocapture"]
async fn diag_cross_process_dehydrate() {
    const BYTES: &[u8] = b"hello world";

    let h = harness(&[("hello.txt", BYTES.len() as i64)]).await;
    let host = h.host(HashMap::from([("hello.txt".to_string(), BYTES.to_vec())]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\hello.txt");
        let (ar, ap) = (root_str.clone(), placeholder.clone());
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&ar);
            read_from_another_process(&ap, &dst)
        })
        .await
        .expect("hydrate");
        eprintln!(
            "DIAG arranged: row={:?} freed={}",
            h.db_state("hello.txt"),
            bytes_are_freed(&placeholder)
        );

        for mode in ["plain-open", "open", "unpin", "dehydrate"] {
            let (p, m) = (placeholder.clone(), mode.to_string());
            let res =
                tokio::task::spawn_blocking(move || run_helper(&p, &m, Duration::from_secs(15)))
                    .await
                    .expect("helper task");
            // Give any callback a moment to land before sampling the row.
            tokio::time::sleep(Duration::from_millis(500)).await;
            eprintln!(
                "DIAG mode={mode:<11} -> {:<48} row={:?} freed={}",
                format!("{res:?}"),
                h.db_state("hello.txt"),
                bytes_are_freed(&placeholder)
            );
        }

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

/// Drain [`FsEvent`]s until the watcher has been quiet for `quiet`. Bounded by
/// construction: every await is a `timeout`.
async fn drain_fs_events(
    rx: &mut mpsc::Receiver<fauna_sync_engine::watcher::FsEvent>,
    quiet: Duration,
) -> Vec<fauna_sync_engine::watcher::FsEvent> {
    let mut out = Vec::new();
    while let Ok(Some(ev)) = tokio::time::timeout(quiet, rx.recv()).await {
        out.push(ev);
    }
    out
}

/// **The measurement probe for the pin-state reaction loop's three open mechanics** — re-measure in ~30 s instead of trusting prose:
///
/// ```text
/// cmd /c "scripts\cargo-win.cmd test -p fauna-sync-agentdiag_pin_reaction_mechanics -- --ignored --nocapture"
/// ```
///
/// 1. **Observability** — does a cross-process `CfSetPinState` flip surface through the
///    shared `FsWatcher` (ReadDirectoryChangesW → `notify`) at all, and as what
///    `FsEvent`? This decides whether the reaction loop can be event-driven or must
///    lean on the sweep.
/// 2. **In-process `CfHydratePlaceholder`** — does the provider's own hydrate call
///    round-trip through its own FETCH_DATA (row flips `Synced` = our loop served it),
///    or does the own-process suppression eat it (hang / error)? Called off the loop
///    thread, bounded 15 s.
/// 3. **Provider push** — does `CfGetTransferKey` + `CfExecute(TRANSFER_DATA)`
///    materialize a placeholder's bytes with no callback at all, and does a full-range
///    push clear the `OFFLINE`/`RECALL` attributes (i.e. is the file an ordinary local
///    file afterwards)?
///
/// It reports rather than asserts, because its subject is OS behavior — the winning
/// mechanic gets pinned by real tests once chosen ([`diag_cross_process_dehydrate`]
/// sets the precedent).
#[tokio::test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
async fn diag_pin_reaction_mechanics() {
    const BYTES: &[u8] = b"pin reaction probe bytes";

    let h = harness(&[
        ("hydrated.txt", BYTES.len() as i64),
        ("hydrate-me.txt", BYTES.len() as i64),
        ("hydrate-direct.txt", BYTES.len() as i64),
        ("push-me.txt", BYTES.len() as i64),
        ("edited.txt", BYTES.len() as i64),
    ])
    .await;
    let host = h.host(HashMap::from([
        ("hydrated.txt".to_string(), BYTES.to_vec()),
        ("hydrate-me.txt".to_string(), BYTES.to_vec()),
        ("hydrate-direct.txt".to_string(), BYTES.to_vec()),
        ("push-me.txt".to_string(), BYTES.to_vec()),
        ("edited.txt".to_string(), BYTES.to_vec()),
    ]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");
    let conn_key = conn.key();

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        // Arrange: populate the root and hydrate exactly one file, all cross-process.
        let hydrated = format!("{root_str}\\hydrated.txt");
        let hydrate_me = format!("{root_str}\\hydrate-me.txt");
        let hydrate_direct = format!("{root_str}\\hydrate-direct.txt");
        let push_me = format!("{root_str}\\push-me.txt");
        let (ar, ap) = (root_str.clone(), hydrated.clone());
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&ar);
            read_from_another_process(&ap, &dst)
        })
        .await
        .expect("hydrate");

        // ── Probe 1: does a cross-process pin flip surface through the shared watcher? ──
        let mut watcher =
            fauna_sync_engine::watcher::FsWatcher::start(&h.root).expect("FsWatcher on the root");
        // Let the watcher settle, then drop whatever backlog the arrange step left.
        let backlog = drain_fs_events(&mut watcher.events, Duration::from_secs(2)).await;
        eprintln!("DIAG watcher backlog after arrange: {backlog:?}");

        for (label, path, rel, mode) in [
            ("unpin(hydrated)", &hydrated, "hydrated.txt", "unpin"),
            ("pin(placeholder)", &hydrate_me, "hydrate-me.txt", "pin"),
        ] {
            let (p, m) = (path.clone(), mode.to_string());
            let res =
                tokio::task::spawn_blocking(move || run_helper(&p, &m, Duration::from_secs(15)))
                    .await
                    .expect("helper task");
            let events = drain_fs_events(&mut watcher.events, Duration::from_secs(2)).await;
            eprintln!(
                "DIAG {label:<17} -> {:<8} watcher_events={} pin_state={:?} freed={} row={:?}",
                format!("{res:?}"),
                events.len(),
                fauna_cfapi::pin_state(std::path::Path::new(path)),
                bytes_are_freed(path),
                h.db_state(rel),
            );
        }

        // ── Probe 1b: does the pin flip itself poison the TRACK_ALL in-sync state? ──
        // `hydrated.txt` was hydrated by an ordinary read (never edited) and then
        // unpinned by probe 1. If the unpin counted as a "metadata change" under
        // CF_INSYNC_POLICY_TRACK_ALL, the file is now not-in-sync and this dehydrate
        // refuses; if it refuses, retry after CfSetInSyncState(IN_SYNC) to prove the
        // repair works. Decides whether the reaction arm's set-in-sync step is
        // load-bearing on the ordinary path or a repair for genuinely-edited files.
        let bare = fauna_cfapi::dehydrate_placeholder(std::path::Path::new(&hydrated));
        eprintln!("DIAG dehydrate(unpinned, never-edited) without set_in_sync -> {bare:?}");
        if bare.is_err() {
            let p = std::path::Path::new(&hydrated);
            let repaired = fauna_cfapi::file_usn(p)
                .and_then(|usn| fauna_cfapi::set_in_sync(p, usn))
                .and_then(|()| fauna_cfapi::dehydrate_placeholder(p));
            eprintln!("DIAG dehydrate after set_in_sync -> {repaired:?}");
        }
        eprintln!(
            "DIAG hydrated.txt after probe 1b: freed={} pin_state={:?}",
            bytes_are_freed(&hydrated),
            fauna_cfapi::pin_state(std::path::Path::new(&hydrated)),
        );

        // ── Probe 1c: does set_in_sync release the refusal after a GENUINE edit? ──
        // Probe 1b's repair ran on a never-edited file (where the bare dehydrate
        // already succeeds); the live repair path concerns a file whose content
        // actually changed (size included). Edit `edited.txt` cross-process, then:
        // bare dehydrate (expected refuse), set_in_sync, dehydrate again — measured
        // separately because a size-changed local copy may need more than the
        // in-sync bit (CfUpdatePlaceholder?) before cldflt lets go of the bytes.
        let edited = format!("{root_str}\\edited.txt");
        let (rs2, ep) = (root_str.clone(), edited.clone());
        let dst2 = h._tmp.path().join("edited-copy.bin");
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&rs2);
            read_from_another_process(&ep, &dst2)
        })
        .await
        .expect("hydrate edited.txt");
        let scratch = h._tmp.path().join("scratch-edit.bin");
        write_from_another_process(
            &scratch,
            std::path::Path::new(&edited),
            b"completely different and longer content than before",
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let bare2 = fauna_cfapi::dehydrate_placeholder(std::path::Path::new(&edited));
        eprintln!("DIAG dehydrate(edited, no repair) -> {bare2:?}");
        let in_sync = fauna_cfapi::file_usn(std::path::Path::new(&edited))
            .and_then(|usn| fauna_cfapi::set_in_sync(std::path::Path::new(&edited), usn));
        let after = fauna_cfapi::dehydrate_placeholder(std::path::Path::new(&edited));
        eprintln!(
            "DIAG set_in_sync -> {in_sync:?}; dehydrate(edited, after set_in_sync) -> {after:?} freed={}",
            bytes_are_freed(&edited),
        );
        // The measured answer to the failures above (0x80070178 — the copy /y
        // REPLACED the placeholder with an ordinary file): re-anchor it NOT in-sync
        // (identity = rel), assert in-sync conditioned on its USN, retry the bare
        // dehydrate — the road `assert_recorded_in_sync` takes —
        // a MARK_IN_SYNC convert takes no USN condition, so it is never used on a file.
        let ep = std::path::Path::new(&edited);
        let converted = fauna_cfapi::convert_to_placeholder_anchored(ep, "edited.txt")
            .and_then(|()| fauna_cfapi::file_usn(ep))
            .and_then(|usn| fauna_cfapi::set_in_sync(ep, usn))
            .and_then(|()| fauna_cfapi::dehydrate_placeholder(ep));
        use std::os::windows::fs::MetadataExt;
        let attrs = std::fs::metadata(&edited).map(|m| m.file_attributes());
        eprintln!(
            "DIAG anchor + set_in_sync(usn) + dehydrate(edited) -> {converted:?} freed={} attrs={attrs:x?}",
            bytes_are_freed(&edited),
        );

        // ── Probe 2: in-process CfHydratePlaceholder (off the loop thread, bounded). ──
        // On a genuine, never-pinned cloud-only placeholder (run 1 used the file probe 1
        // had just pinned — cldflt had already auto-hydrated it, confounding the probe).
        // If our own FETCH_DATA serves this, the loop's Fetch arm marks the row Synced;
        // a hang means the own-process suppression eats the request.
        eprintln!(
            "DIAG hydrate-direct precondition: freed={} row={:?}",
            bytes_are_freed(&hydrate_direct),
            h.db_state("hydrate-direct.txt"),
        );
        let p = hydrate_direct.clone();
        let call = tokio::task::spawn_blocking(move || {
            fauna_cfapi::hydrate_placeholder(std::path::Path::new(&p))
        });
        let outcome = match tokio::time::timeout(Duration::from_secs(15), call).await {
            Ok(Ok(res)) => format!("{res:?}"),
            Ok(Err(join)) => format!("join error: {join}"),
            Err(_) => "HUNG (no return within 15s; blocking thread leaked)".to_string(),
        };
        // Give the loop's mark-hydrated a moment to land before sampling the row.
        tokio::time::sleep(Duration::from_millis(500)).await;
        eprintln!(
            "DIAG in-process CfHydratePlaceholder -> {outcome} row={:?} freed={}",
            h.db_state("hydrate-direct.txt"),
            bytes_are_freed(&hydrate_direct),
        );

        // ── Probe 3: provider push (CfGetTransferKey + TRANSFER_DATA), no callback. ──
        let p = push_me.clone();
        let push = tokio::task::spawn_blocking(move || {
            fauna_cfapi::provider_push_data(&conn_key, std::path::Path::new(&p), BYTES)
        })
        .await
        .expect("push task");
        let meta = std::fs::metadata(&push_me).expect("stat push-me.txt");
        eprintln!(
            "DIAG provider_push_data -> {push:?} freed={} attrs={:#010x} row={:?}",
            bytes_are_freed(&push_me),
            std::os::windows::fs::MetadataExt::file_attributes(&meta),
            h.db_state("push-me.txt"),
        );
        // Only read the content back if the OS says the bytes are local — an
        // in-process read of a still-cloud-only placeholder stalls 60 s by design.
        if !bytes_are_freed(&push_me) {
            let content = std::fs::read(&push_me).expect("read pushed file");
            eprintln!(
                "DIAG provider_push_data content roundtrip: {}",
                if content == BYTES {
                    "BYTES MATCH"
                } else {
                    "MISMATCH"
                }
            );
        }

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
    drop(conn);
}

// ---------------------------------------------------------------------------
// The pin-state reaction loop, live (`file-sync.md` § Per-file sync-status
// display, answered question 2): Explorer's cloud verbs are pure CfSetPinState
// writes, and the provider owes the byte work. These four tests drive the four
// distinct paths — live unpin (watcher arm), live pin (cldflt auto-hydration
// through our fetch arm), edit+unpin (the upload-first guard and the stale
// in-sync repair), and flips-while-down (the startup sweep).
// ---------------------------------------------------------------------------

/// Drain every `FileStatusChanged` the loop broadcast so far, in order.
fn drained_status_events(rx: &mut broadcast::Receiver<Event>) -> Vec<(String, FileStatus)> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let EventKind::FileStatusChanged { path, status } = ev.event {
            out.push((path, status));
        }
    }
    out
}

/// **Explorer's "Free up space" frees space.** A cross-process
/// `CfSetPinState(UNPINNED)` — byte-identical to what the verb does (measured) — on a hydrated file in a served root is observed by the live
/// watcher arm, and the provider does the byte work the OS never does: the bytes
/// are freed on disk, the row flips `Placeholder`, and `FileStatusChanged
/// {CloudOnly}` is pushed. Before the reaction loop, this exact sequence was a
/// silent no-op: the user asked for disk back and nothing happened.
#[tokio::test]
async fn an_explorer_unpin_dehydrates_and_flips_the_badge() {
    const BYTES: &[u8] = b"free up this space please";

    let h = harness(&[("doc.txt", BYTES.len() as i64)]).await;
    let host = h.host(HashMap::from([("doc.txt".to_string(), BYTES.to_vec())]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\doc.txt");
        let (rs, ph) = (root_str.clone(), placeholder.clone());
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&rs);
            read_from_another_process(&ph, &dst)
        })
        .await
        .expect("hydrate");
        assert_eq!(h.db_state("doc.txt"), Some(SyncState::Synced));
        assert!(!bytes_are_freed(&placeholder), "precondition: bytes local");

        let p = placeholder.clone();
        tokio::task::spawn_blocking(move || run_helper(&p, "unpin", Duration::from_secs(15)))
            .await
            .expect("unpin task")
            .expect("CfSetPinState(UNPINNED) from another process");

        eventually(
            || {
                bytes_are_freed(&placeholder)
                    && h.db_state("doc.txt") == Some(SyncState::Placeholder)
            },
            Duration::from_secs(30),
            "the unpinned file's bytes to be freed and its row to flip Placeholder — \
             the watcher saw the pin flip (an ordinary Modified event), the debouncer \
             released it, and the reaction arm dehydrated. Bytes still local = the \
             pre-reaction-loop bug: 'Free up space' was a silent no-op",
        )
        .await;

        assert!(
            drained_status_events(&mut event_rx)
                .contains(&(placeholder.clone(), FileStatus::CloudOnly)),
            "a FileStatusChanged(CloudOnly) must be pushed so the Explorer overlay flips live"
        );

        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// **Explorer's "Always keep on this device" keeps it.** A cross-process
/// `CfSetPinState(PINNED)` on a cloud-only placeholder makes **cldflt itself**
/// request hydration through the provider's own FETCH_DATA (measured,
/// `diag_pin_reaction_mechanics` — the live pin direction needs no provider
/// reaction at all); our fetch arm serves it, marks the row `Synced`, and pushes
/// the badge event. This pins the OS half of the contract the pin-reaction
/// design leans on — if Windows ever stops auto-hydrating pinned placeholders,
/// this goes red and the sweep must take over the live direction too.
#[tokio::test]
async fn an_explorer_pin_on_a_placeholder_hydrates_through_our_fetch_arm() {
    const BYTES: &[u8] = b"keep these bytes on this device";

    let h = harness(&[("keep.txt", BYTES.len() as i64)]).await;
    let host = h.host(HashMap::from([("keep.txt".to_string(), BYTES.to_vec())]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");

    let root = h.root.clone();
    let root_str = h.root_str();
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\keep.txt");
        let rs = root_str.clone();
        tokio::task::spawn_blocking(move || dir_from_another_process(&rs))
            .await
            .expect("populate");
        assert!(
            bytes_are_freed(&placeholder),
            "precondition: a cloud-only placeholder"
        );

        let p = placeholder.clone();
        tokio::task::spawn_blocking(move || run_helper(&p, "pin", Duration::from_secs(15)))
            .await
            .expect("pin task")
            .expect("CfSetPinState(PINNED) from another process");

        eventually(
            || !bytes_are_freed(&placeholder) && h.db_state("keep.txt") == Some(SyncState::Synced),
            Duration::from_secs(30),
            "the pinned placeholder to hydrate (cldflt requests, our fetch arm serves) \
             and its row to flip Synced",
        )
        .await;

        assert_eq!(
            std::fs::read(&placeholder).expect("read the now-local file"),
            BYTES,
            "the hydrated content must be the served bytes"
        );
        assert!(
            drained_status_events(&mut event_rx)
                .contains(&(placeholder.clone(), FileStatus::Synced)),
            "a FileStatusChanged(Synced) must be pushed so the overlay flips live"
        );

        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// **The iron rule survives "Free up space": an edit is uploaded, never destroyed
/// — and NOT freed until its change record proves it recoverable.** A user edits a
/// tracked file and immediately unpins it (both cross-process, landing in the same
/// debounce window). The reaction arm must sequence: upload the edit (the shared
/// uploader), and free the bytes **only** once `is_dehydration_safe` proves the
/// recorded head reassembles to the disk content.
///
/// This harness leaves the **control plane unconnected** (§ mount_nest_byte_plane:
/// `changes.record` fails-and-logs), so the edit's chunks reach the byte plane but
/// its **record never lands** — the row's `manifest_hash` stays at the pre-edit
/// head (here the seeded placeholder's [`seeded_manifest_hash`], i.e. the stale
/// pre-edit nest head — exactly what a real fold would have left). Freeing the bytes would then
/// re-anchor the placeholder on that stale head and the next open would download
/// the OLD content, destroying the edit. So the gate MUST refuse, and the bytes
/// MUST stay local — exactly the data-loss window the 2026-07-18 `recorded_content_hash`
/// hardening closes (`is_dehydration_safe` compares disk to the *recorded head*,
/// not the optimistically-advanced `local_hash`; `file-sync.md` § Per-file
/// sync-status display). The positive "recorded → freed" half is proven where the
/// record can genuinely land: engine `commit_recorded_head_makes_the_file_dehydration_safe`
/// / tier_3 `a_successful_record_leaves_the_file_dehydration_safe`, and the freeing
/// primitive itself by `live_population::an_anchored_plain_file_frees_only_after_a_conditioned_in_sync_assertion`.
#[tokio::test]
async fn an_edit_plus_unpin_uploads_the_edit_but_does_not_free_it_until_recorded() {
    const ORIGINAL: &[u8] = b"original content";
    const EDITED: &[u8] = b"the user's final words, which MUST reach the nest";

    let nest = MockServer::start().await;
    let chunk_posts = mount_nest_byte_plane(&nest).await;

    let h = harness_against_nest(&[("notes.txt", ORIGINAL.len() as i64)], &nest.uri()).await;
    let host = h.host(HashMap::from([(
        "notes.txt".to_string(),
        ORIGINAL.to_vec(),
    )]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");

    let root = h.root.clone();
    let root_str = h.root_str();
    let scratch = h._tmp.path().join("scratch.bin");
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\notes.txt");
        let (rs, ph) = (root_str.clone(), placeholder.clone());
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&rs);
            read_from_another_process(&ph, &dst)
        })
        .await
        .expect("hydrate");

        // The user edits, then immediately asks for the space back.
        write_from_another_process(&scratch, &root.join("notes.txt"), EDITED);
        let p = placeholder.clone();
        tokio::task::spawn_blocking(move || run_helper(&p, "unpin", Duration::from_secs(15)))
            .await
            .expect("unpin task")
            .expect("CfSetPinState(UNPINNED) from another process");

        // The edit's chunks must reach the nest's byte plane (converge uploads it)...
        let want = ContentHash::of_raw(EDITED);
        eventually(
            || h.db_uploaded("notes.txt", want),
            Duration::from_secs(30),
            "the edited content to finish uploading (Synced + the NEW content hash)",
        )
        .await;
        assert!(
            chunk_posts.load(Ordering::SeqCst) > 0,
            "the edited bytes must actually reach the nest's byte plane"
        );

        // ...but because the change RECORD never landed, the recorded head does
        // not match this content, so the gate MUST refuse to free the bytes.
        assert!(
            !h.host(HashMap::new())
                .is_dehydration_safe("notes.txt")
                .await,
            "a chunks-uploaded-but-not-RECORDED edit is unrecoverable from the nest \
             (the head is still the pre-edit manifest), so is_dehydration_safe must \
             refuse — freeing it would destroy the edit"
        );

        // Give the reaction arm ample time to (not) free the bytes, then assert
        // they are still local and the row is still Synced — the edit is protected.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !bytes_are_freed(&placeholder),
            "the un-recorded edit must NOT be freed — its bytes are the only copy of \
             the user's edit until the record lands (freeing is the exact data loss \
             the recorded-head gate prevents)"
        );
        assert_eq!(
            h.db_state("notes.txt"),
            Some(SyncState::Synced),
            "the protected file stays Synced with its edit on disk, never flipped to \
             Placeholder behind the stale head"
        );

        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// Hydrate `notes.txt` (served as `recorded`) through the product's own fetch arm, then
/// stop the loop: the row is `Synced` and `mark_hydrated` has stamped `recorded` as the
/// recorded content (`recorded_content_hash`), so `recorded` is exactly what a landed
/// change record would have left. The root stays connected (the returned guard).
async fn hydrate_recorded_notes(
    h: &Harness,
    recorded: &[u8],
) -> crate::cfapi_host::CfApiConnection {
    let host = h.host(HashMap::from([(
        "notes.txt".to_string(),
        recorded.to_vec(),
    )]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        None,
        h.root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let driver = async {
        let placeholder = format!("{root_str}\\notes.txt");
        let rs = root_str.clone();
        let got = tokio::task::spawn_blocking(move || {
            dir_from_another_process(&rs);
            read_from_another_process(&placeholder, &dst)
        })
        .await
        .expect("hydrate");
        assert_eq!(got, recorded, "the hydrated bytes are the served content");
        eventually(
            || h.db_state("notes.txt") == Some(SyncState::Synced),
            Duration::from_secs(10),
            "the hydrate-on-open flip to Synced",
        )
        .await;
        cancel.cancel();
    };
    tokio::join!(loop_fut, driver);
    conn
}

/// **A save that lands during an upload survives the upload's in-sync flip and a
/// following "Free up space".** The flip runs after a change record lands, for the content
/// the upload READ; a newer save (an autosaving editor, a large file) has meanwhile tripped
/// `CF_INSYNC_POLICY_TRACK_ALL`'s not-in-sync bit — the one thing that makes the bare
/// dehydrate every caller tries first refuse. An unconditional flip cleared that bit for the
/// NEWER bytes, and the next dehydrate (an unpin in the same batch, the shell's *Free up
/// space*, a remote change's invalidation) freed them: the latest edit gone, no conflict copy,
/// nothing on the nest to re-hydrate it from (`file-sync.md` § Per-file sync-status display —
/// freeing an unrecorded edit is the data loss the recorded-head gate refuses).
///
/// Staged against the real OS: `recorded` is hydrated through the product's fetch arm (so it
/// IS the recorded content), `newer` is written from another process, then the flip runs for
/// the recorded rel — exactly the post-record call the loop makes. The bare dehydrate must
/// still refuse, the newer bytes must still be on disk, and they must still upload.
#[tokio::test]
async fn a_save_during_an_upload_survives_the_in_sync_flip_and_a_free_up_space() {
    const RECORDED: &[u8] = b"E1: the content whose change record reached the nest";
    const NEWER: &[u8] = b"E2: saved while E1 was uploading -- the only copy of it anywhere";

    let nest = MockServer::start().await;
    let chunk_posts = mount_nest_byte_plane(&nest).await;
    let h = harness_against_nest(&[("notes.txt", RECORDED.len() as i64)], &nest.uri()).await;
    let _conn = hydrate_recorded_notes(&h, RECORDED).await;

    let host = h.host(HashMap::new());
    let abs = h.root.join("notes.txt");
    let placeholder = abs.to_string_lossy().to_string();
    assert!(
        host.is_dehydration_safe("notes.txt").await,
        "precondition: the hydrated bytes are the recorded content"
    );

    // E2 lands after the upload read E1 — before the post-record flip.
    write_from_another_process(&h._tmp.path().join("scratch.bin"), &abs, NEWER);

    // E1's change record landed: the loop flips the recorded rel.
    flip_recorded_in_sync(
        &host,
        &CfapiInvalidator,
        &h.root,
        &["notes.txt".to_string()],
    )
    .await;

    // Any dehydrate caller now tries the bare dehydrate first.
    let freed = fauna_cfapi::dehydrate_placeholder(&abs);
    assert!(
        !bytes_are_freed(&placeholder),
        "the newer save was FREED after the in-sync flip (dehydrate -> {freed:?}): the flip \
         vouched for bytes it never uploaded, so the not-in-sync refusal was gone"
    );
    assert!(
        freed.is_err(),
        "the bare dehydrate must refuse the newer, un-uploaded save"
    );
    assert_eq!(
        std::fs::read(&abs).expect("read the file back"),
        NEWER,
        "the newer save's bytes must survive verbatim"
    );

    // And the newer save still reaches the nest.
    let want = ContentHash::of_raw(NEWER);
    tokio::select! {
        _ = host.upload_file("notes.txt") => {}
        _ = eventually(
            || h.db_uploaded("notes.txt", want),
            Duration::from_secs(30),
            "the newer save to upload (Synced + its content hash)",
        ) => {}
    }
    assert!(
        h.db_uploaded("notes.txt", want),
        "the newer save must upload"
    );
    assert!(
        chunk_posts.load(Ordering::SeqCst) > 0,
        "the newer save's bytes must actually reach the nest's byte plane"
    );
}

/// The real cfapi invalidator, except that a save lands IN PLACE on the file just before
/// each in-sync assertion — after the host read the USN and proved the content, the one
/// window no proof can see. In-place (not `copy /y`, which replaces the file and would make
/// the assertion fail for the unrelated "not a cloud file" reason).
struct SaveBeforeAssert {
    newer: &'static [u8],
}

impl crate::bridge::PlaceholderInvalidator for SaveBeforeAssert {
    fn dehydrate(&self, abs_path: &std::path::Path) -> Result<()> {
        CfapiInvalidator.dehydrate(abs_path)
    }
    fn supersede(&self, abs_path: &std::path::Path, size: u64, mtime: i64) -> Result<()> {
        CfapiInvalidator.supersede(abs_path, size, mtime)
    }
    fn pin_action(&self, abs_path: &std::path::Path) -> Option<crate::pin_reaction::PinAction> {
        CfapiInvalidator.pin_action(abs_path)
    }
    fn set_in_sync(&self, abs_path: &std::path::Path, usn: i64) -> Result<()> {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .open(abs_path)
            .and_then(|mut f| f.write_all(self.newer))
            .expect("the racing in-place save");
        CfapiInvalidator.set_in_sync(abs_path, usn)
    }
    fn usn(&self, abs_path: &std::path::Path) -> Result<i64> {
        CfapiInvalidator.usn(abs_path)
    }
    fn kick_hydrate(&self, abs_path: &std::path::Path) {
        CfapiInvalidator.kick_hydrate(abs_path)
    }
    fn is_cloud_file(&self, abs_path: &std::path::Path) -> bool {
        CfapiInvalidator.is_cloud_file(abs_path)
    }
    fn anchor(&self, abs_path: &std::path::Path, rel: &str) -> Result<()> {
        CfapiInvalidator.anchor(abs_path, rel)
    }
    fn convert_dir_in_sync(&self, abs_path: &std::path::Path, rel: &str) -> Result<()> {
        CfapiInvalidator.convert_dir_in_sync(abs_path, rel)
    }
    fn is_listed_dir(&self, abs_dir: &std::path::Path) -> bool {
        CfapiInvalidator.is_listed_dir(abs_dir)
    }
    fn create_placeholder(
        &self,
        parent_abs: &std::path::Path,
        rel: &str,
        size: u64,
        mtime: i64,
        is_dir: bool,
    ) -> Result<()> {
        CfapiInvalidator.create_placeholder(parent_abs, rel, size, mtime, is_dir)
    }
}

/// **A save landing between the content proof and the in-sync assertion is
/// refused atomically.** The flip reads the file's USN, proves the disk is the recorded
/// content, then asserts in-sync conditioned on that USN — so a save landing after the
/// proof (here: injected immediately before the assertion) makes cfapi refuse it, and the
/// next bare dehydrate refuses the newer bytes. A hash-then-assert without the USN would
/// vouch for the newer save; red if the assertion is unconditioned or the USN is re-read
/// after the proof.
#[tokio::test]
async fn a_save_between_the_proof_and_the_in_sync_assertion_is_refused() {
    const RECORDED: &[u8] = b"E1: the content whose change record reached the nest";
    // At least as long as RECORDED: the in-place write must replace every byte.
    const NEWER: &[u8] = b"E2: saved in the instant after the proof -- the only copy anywhere";

    let h = harness(&[("notes.txt", RECORDED.len() as i64)]).await;
    let _conn = hydrate_recorded_notes(&h, RECORDED).await;
    let host = h.host(HashMap::new());
    let abs = h.root.join("notes.txt");
    let placeholder = abs.to_string_lossy().to_string();

    // The recorded content, re-saved: the not-in-sync bit is set, and the proof holds.
    write_from_another_process(&h._tmp.path().join("scratch.bin"), &abs, RECORDED);
    assert!(
        host.is_dehydration_safe("notes.txt").await,
        "precondition: the disk holds exactly the recorded content"
    );

    flip_recorded_in_sync(
        &host,
        &SaveBeforeAssert { newer: NEWER },
        &h.root,
        &["notes.txt".to_string()],
    )
    .await;

    let freed = fauna_cfapi::dehydrate_placeholder(&abs);
    assert!(
        !bytes_are_freed(&placeholder),
        "the save that landed after the proof was FREED (dehydrate -> {freed:?}): the in-sync \
         assertion vouched for bytes the proof never saw"
    );
    assert!(
        freed.is_err(),
        "the bare dehydrate must refuse the newer save"
    );
    assert_eq!(
        std::fs::read(&abs).expect("read the file back"),
        NEWER,
        "the newer save's bytes must survive verbatim"
    );
}

/// **Pin flips made while the service was down are healed at the next start** —
/// the sweep half of the reaction loop. Explorer writes pin state whether or not
/// the provider is running, so a stopped service accumulates debt: an unpinned
/// file still holding bytes, a pinned file still cloud-only. The restarted
/// host's startup sweep settles both — dehydrating the one (no watcher event to
/// react to; only the sweep sees it) and kicking a real in-process
/// `CfHydratePlaceholder` for the other, served by the host's own fetch arm
/// (measured own-process round-trip, `diag_pin_reaction_mechanics`).
#[tokio::test]
async fn pin_flips_made_while_the_service_was_down_are_healed_at_startup() {
    const FREE_BYTES: &[u8] = b"downtime unpin target";
    const KEEP_BYTES: &[u8] = b"downtime pin target";

    let h = harness(&[
        ("freeme.txt", FREE_BYTES.len() as i64),
        ("keepme.txt", KEEP_BYTES.len() as i64),
    ])
    .await;
    let blobs = HashMap::from([
        ("freeme.txt".to_string(), FREE_BYTES.to_vec()),
        ("keepme.txt".to_string(), KEEP_BYTES.to_vec()),
    ]);
    let root_str = h.root_str();
    let free_ph = format!("{root_str}\\freeme.txt");
    let keep_ph = format!("{root_str}\\keepme.txt");

    // ── Session 1: populate the root and hydrate freeme.txt, then stop. ──
    {
        let host = h.host(blobs.clone());
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = broadcast::channel(64);
        let cancel = CancellationToken::new();
        let conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect #1");
        let dst = h._tmp.path().join("hydrated-copy.bin");
        let loop_fut = run_hydration_loop(
            host,
            CfapiInvalidator,
            cmd_rx,
            Some(event_tx),
            h.root.clone(),
            FOLDER.to_string(),
            cancel.clone(),
            None,
            None,
        );
        let (rs, ph) = (root_str.clone(), free_ph.clone());
        let driver = async {
            tokio::task::spawn_blocking(move || {
                dir_from_another_process(&rs);
                read_from_another_process(&ph, &dst)
            })
            .await
            .expect("hydrate");
            assert_eq!(h.db_state("freeme.txt"), Some(SyncState::Synced));
            cancel.cancel();
        };
        drive_until_asserted(loop_fut, driver).await;
        // A CRASHED service: the connection is gone (the OS reclaims it at process
        // death) but the filter registration survives. Deliberately NOT drop(conn)
        // — two measured reasons: a graceful teardown's unregister REMOVES
        // un-hydrated cloud-only placeholders from disk (keepme.txt stops
        // existing, 0x80070002), so "pinned while down" is a state only a crash
        // can leave behind; and a merely-forgotten connection blocks connect #2
        // (0x8007017A "already connected with another cloud sync provider").
        conn.disconnect_keeping_registration();
    }

    // ── Downtime: Explorer writes pin state with no provider running. ──
    // In-process is fine here — with the provider gone there is no own-I/O
    // suppression question, and nothing is listening either way.
    fauna_cfapi::set_pin_state(std::path::Path::new(&free_ph), false)
        .expect("unpin while the service is down");
    fauna_cfapi::set_pin_state(std::path::Path::new(&keep_ph), true)
        .expect("pin while the service is down");
    assert!(!bytes_are_freed(&free_ph), "debt: unpinned but bytes local");
    assert!(bytes_are_freed(&keep_ph), "debt: pinned but cloud-only");

    // ── Session 2: a fresh host over the same root heals both at startup. ──
    let host = h.host(blobs);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect #2");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        h.root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        eventually(
            || {
                bytes_are_freed(&free_ph)
                    && h.db_state("freeme.txt") == Some(SyncState::Placeholder)
            },
            Duration::from_secs(30),
            "the while-down unpin to be healed by the startup sweep (bytes freed, row \
             Placeholder) — no watcher event exists for it, so only the sweep can see it",
        )
        .await;
        eventually(
            || !bytes_are_freed(&keep_ph) && h.db_state("keepme.txt") == Some(SyncState::Synced),
            Duration::from_secs(30),
            "the while-down pin to be healed by the startup sweep's hydration kick \
             (bytes local, row Synced) — served by this host's own fetch arm",
        )
        .await;
        assert_eq!(
            std::fs::read(&keep_ph).expect("read healed file"),
            KEEP_BYTES,
            "the healed pin must carry the served content"
        );
        let events = drained_status_events(&mut event_rx);
        assert!(
            events.contains(&(free_ph.clone(), FileStatus::CloudOnly)),
            "healing the unpin must push CloudOnly (got {events:?})"
        );
        assert!(
            events.contains(&(keep_ph.clone(), FileStatus::Synced)),
            "healing the pin must push Synced (got {events:?})"
        );
        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// The `parent_rel` of every FETCH_PLACEHOLDERS the OS fires, recorded in order while
/// forwarding each command to the loop unchanged — the observable that tells *why* a
/// placeholder did or did not come back (the OS re-armed a directory's population, or not).
fn recording_populates(
    mut outer: mpsc::UnboundedReceiver<crate::bridge::HydrationCommand>,
) -> (
    mpsc::UnboundedReceiver<crate::bridge::HydrationCommand>,
    Arc<std::sync::Mutex<Vec<String>>>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Some(cmd) = outer.recv().await {
            if let crate::bridge::HydrationCommand::Populate { parent_rel, .. } = &cmd {
                log.lock().unwrap().push(parent_rel.clone());
            }
            if tx.send(cmd).is_err() {
                break;
            }
        }
    });
    (rx, seen)
}

/// What [`delete_a_placeholder_while_the_service_is_down`] observed after the restart.
struct DownDeleteOutcome {
    /// The deleted placeholder's path, and whether it is on disk again after the browse.
    gone_path: String,
    exists: bool,
    /// Every directory the OS asked us to populate during the restarted session.
    populates_after_restart: Vec<String>,
    /// The deleted placeholder's row at the instant the root was CONNECTED — read inside
    /// the connect itself, so it witnesses the boot order (decision (c)): the sweep has
    /// already seen the absence before any population could re-list the row.
    row_at_connect: Option<SyncState>,
    /// The deleted placeholder's row after the post-restart browse and one more real
    /// `reconcile` (the tick sweep's retry).
    row: Option<SyncState>,
    /// That `reconcile`'s delete count.
    deleted_files: usize,
    /// The untouched sibling still lists — the vacuity guard for `exists`.
    sibling_listed: bool,
}

/// **Delete a dehydrated placeholder while no watcher runs, then restart and browse.**
///
/// The product lifecycle, faithfully: a shell-registered root, populated by another
/// process's browse — which is what marks the placeholder **seen**
/// (`serve_populate` → `HydrationHost::mark_seen`, from what cfapi reports it created) —
/// then a **graceful stop**, the drop that keeps the registration and every placeholder
/// ([`a_shell_registered_root_keeps_placeholders_across_a_service_stop`]). With no
/// provider connected, another process deletes one placeholder; the next session boots
/// through the product's own on-demand start (`serve_hydration_root`, with the product's
/// connect `register_and_connect_product_root` and its decision-(e) probe
/// `registration_survived`), browses the root and the placeholder's directory from
/// another process, and runs a real `reconcile`.
///
/// This harness's nest is deliberately unreachable (`harness`), so every record FAILS:
/// the outcome here is the ruling's **nest-down-at-boot** shape — the delete propagated
/// as far as this device can take it (`LocallyDeleted`, owed, never listed back), with the
/// record landing on the next pass once the nest answers. That landing half needs a nest
/// that acks, and is pinned over a real `NestClient` with only the socket mocked
/// (`fauna-sync-engine` `offline_placeholder_delete_test::a_delete_owed_while_the_nest_is_down_lands_on_the_next_pass`).
///
/// `dir` is the placeholder's parent directory: `""` for the root, else a subdirectory.
async fn delete_a_placeholder_while_the_service_is_down(dir: &str) -> DownDeleteOutcome {
    const GONE_BYTES: &[u8] = b"deleted while down";
    const STAY_BYTES: &[u8] = b"untouched sibling";
    let join = |leaf: &str| {
        if dir.is_empty() {
            leaf.to_string()
        } else {
            format!("{dir}/{leaf}")
        }
    };
    let (gone_rel, stay_rel) = (join("gone.txt"), join("stay.txt"));

    let h = harness(&[
        (&gone_rel, GONE_BYTES.len() as i64),
        (&stay_rel, STAY_BYTES.len() as i64),
    ])
    .await;
    let blobs = HashMap::from([
        (gone_rel.clone(), GONE_BYTES.to_vec()),
        (stay_rel.clone(), STAY_BYTES.to_vec()),
    ]);
    let root_str = h.root_str();
    let dir_str = if dir.is_empty() {
        root_str.clone()
    } else {
        format!("{root_str}\\{}", dir.replace('/', "\\"))
    };
    let gone_ph = format!("{dir_str}\\gone.txt");
    // Browse the root, then (if different) the placeholder's directory — lazy population
    // materializes a subdirectory's children only on its own first browse.
    let browse = |root: String, dir: String| {
        tokio::task::spawn_blocking(move || {
            let top = dir_from_another_process(&root);
            if dir == root {
                top
            } else {
                dir_from_another_process(&dir)
            }
        })
    };

    // ── Session 1: populate the root + the placeholder's directory, then a graceful stop. ──
    {
        let host = h.host(blobs.clone());
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let _conn = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
            .expect("connect #1");
        let loop_fut = run_hydration_loop(
            host,
            CfapiInvalidator,
            cmd_rx,
            None,
            h.root.clone(),
            FOLDER.to_string(),
            cancel.clone(),
            None,
            None,
        );
        let driver = async {
            let listing = browse(root_str.clone(), dir_str.clone())
                .await
                .expect("browse #1");
            assert!(
                listing.contains(&"gone.txt".to_string()),
                "precondition: the placeholder must materialize on browse. Got {listing:?}"
            );
            cancel.cancel();
        };
        drive_until_asserted(loop_fut, driver).await;
        // `_conn` drops here: the graceful stop, registration + placeholders kept.
    }
    assert!(
        bytes_are_freed(&gone_ph),
        "precondition: a dehydrated placeholder survives the stop"
    );
    assert!(
        h.db_seen(&gone_rel),
        "precondition: the population that put it on the disk marked it seen"
    );

    // ── Downtime: the user deletes it. Nothing is listening. ──
    let del = std::process::Command::new("cmd")
        .args(["/c", "del", "/f", "/q", &gone_ph])
        .output()
        .expect("spawn del");
    assert!(
        del.status.success() && !std::path::Path::new(&gone_ph).exists(),
        "precondition: the placeholder is deletable with no provider connected ({})",
        String::from_utf8_lossy(&del.stderr)
    );
    assert_eq!(h.db_state(&gone_rel), Some(SyncState::Placeholder));

    // ── Session 2: the product's boot — sweep, THEN connect; browse; reconcile. ──
    let fresh_registration = !crate::cfapi_host::registration_survived(&h.root);
    assert!(
        !fresh_registration,
        "a graceful stop keeps the registration — the decision-(e) probe must say so, or \
         every restart would clear the marks and no offline delete could ever propagate"
    );
    let host = h.host(blobs);
    let (cmd_tx, outer_rx) = mpsc::unbounded_channel();
    let (cmd_rx, populates) = recording_populates(outer_rx);
    let cancel = CancellationToken::new();
    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
    let connect = {
        let (root, db_path, gone_rel) = (h.root.clone(), h.db_path.clone(), gone_rel.clone());
        move || {
            // Read the row as the root connects: the boot sweep has run by now.
            let row_at_connect = SyncDb::open(&db_path)
                .expect("reopen db")
                .get_entry(&gone_rel)
                .expect("get entry")
                .map(|e| e.state);
            let conn = crate::cfapi_host::register_and_connect_product_root(
                &root, FOLDER, cmd_tx, db_path,
            )
            .expect("connect #2 over the kept registration");
            let _ = connected_tx.send(row_at_connect);
            conn
        }
    };
    let loop_fut = crate::bridge::serve_hydration_root(
        host,
        CfapiInvalidator,
        crate::bridge::RootBoot {
            fresh_registration,
            connect,
        },
        cmd_rx,
        None,
        h.root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
        None,
    );
    let mut outcome = None;
    let driver = async {
        // No browse before the root is connected — the product's order is the subject.
        let row_at_connect = connected_rx.await.expect("the loop connected the root");
        let listing = browse(root_str.clone(), dir_str.clone())
            .await
            .expect("browse #2");
        let exists = std::path::Path::new(&gone_ph).exists();
        let stats = h.engine().reconcile().await.expect("reconcile");
        outcome = Some(DownDeleteOutcome {
            gone_path: gone_ph.clone(),
            exists,
            populates_after_restart: populates.lock().unwrap().clone(),
            row_at_connect,
            row: h.db_state(&gone_rel),
            deleted_files: stats.deleted_files,
            sibling_listed: listing.contains(&"stay.txt".to_string()),
        });
        // The binding ends here — full teardown when the loop's guard drops, no ghost
        // root left behind.
        crate::cfapi_host::mark_root_for_full_teardown(&h.root);
        cancel.cancel();
    };
    drive_until_asserted(loop_fut, driver).await;
    outcome.expect("the driver ran")
}

/// The assertions both service-down pins share — the ruling's propagated outcome
/// (`delete-propagation.md` § *An offline placeholder delete propagates*), in the
/// nest-down shape this harness stages (its nest never acks).
fn assert_propagated_while_the_nest_is_down(o: &DownDeleteOutcome) {
    assert!(
        o.sibling_listed,
        "vacuity guard: the directory must list its untouched sibling"
    );
    assert_eq!(
        o.row_at_connect,
        Some(SyncState::LocallyDeleted),
        "decision (c): the boot sweep must have found the delete BEFORE the root connected"
    );
    assert!(
        !o.exists,
        "decision (d): a delete owed to the nest is never listed back onto the disk: {}",
        o.gone_path
    );
    assert_eq!(
        o.row,
        Some(SyncState::LocallyDeleted),
        "the delete is durable and still owed — tombstoned only on the nest's ack"
    );
    assert_eq!(
        o.deleted_files, 0,
        "the retry attempted the record and the (unreachable) nest refused it — nothing is \
         tombstoned before an ack"
    );
}

/// **A ROOT-level placeholder deleted while the service is down is PROPAGATED**
/// (`delete-propagation.md` § *An offline placeholder delete propagates*, decision (c)).
/// Before the ruling this was silently reverted: the stop leaves the root without its
/// provider reparse point, the restart's re-registration re-arms the ROOT's on-demand
/// population, and the first browse listed the still-tracked `Placeholder` row back
/// onto disk. The boot sweep now runs over the tree as the downtime left it BEFORE the
/// connect, so the row is already `LocallyDeleted` when the re-armed population lists
/// the root — and it is not listed.
#[tokio::test]
async fn a_root_placeholder_deleted_while_the_service_was_down_is_propagated() {
    let _serial = shell_sync_root_test_guard();
    let o = delete_a_placeholder_while_the_service_is_down("").await;
    assert!(
        o.populates_after_restart.contains(&String::new()),
        "the re-registration still re-arms the root's population (got {:?}) — the sweep's \
         order, not a quieter OS, is what keeps the row off the disk",
        o.populates_after_restart
    );
    assert_propagated_while_the_nest_is_down(&o);
}

/// **A SUBDIRECTORY placeholder deleted while the service is down is PROPAGATED**
/// (decision (a) — the evidence is the engine's own per-row seen mark, never the OS's
/// populated bit). Before the ruling it was lost: the re-registration re-arms the root
/// only, the subdirectory stays marked populated, nothing listed the row back — and
/// `reconcile`'s universe was the `Synced` rows, so the scan never counted it and the
/// row dangled as `Placeholder` with no file. The row is seen (the first session's
/// browse put it on the disk), so its absence is a delete.
#[tokio::test]
async fn a_subdirectory_placeholder_deleted_while_the_service_was_down_is_propagated() {
    let _serial = shell_sync_root_test_guard();
    let o = delete_a_placeholder_while_the_service_is_down("sub").await;
    assert!(
        !o.populates_after_restart.contains(&"sub".to_string()),
        "the populated subdirectory is not re-armed (got {:?})",
        o.populates_after_restart
    );
    assert_propagated_while_the_nest_is_down(&o);
}

/// **Decision (e) on the graceful full teardown** — an unbind, a re-bind or a mode flip
/// (`mark_root_for_full_teardown`) unregisters the root, and the unregister removes every
/// cloud-only placeholder from the disk (measured). That removal is the product's own act,
/// never the user's delete, so the product connection's drop clears every seen mark
/// FIRST: a later engine over this state DB (the mode flip back, a re-bind to the same
/// path) finds no marks and reads nothing the teardown removed as a delete.
#[tokio::test]
async fn a_full_teardown_clears_the_seen_marks_before_the_unregister() {
    let _serial = shell_sync_root_test_guard();
    let h = harness(&[("a.txt", 5)]).await;
    SyncDb::open(&h.db_path)
        .expect("open db")
        .mark_seen(&["a.txt"])
        .expect("mark");
    assert!(h.db_seen("a.txt"), "precondition: seen");

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let conn = crate::cfapi_host::register_and_connect_product_root(
        &h.root,
        FOLDER,
        cmd_tx,
        h.db_path.clone(),
    )
    .expect("connect");
    // A kept registration (a service stop) is NOT a teardown: the marks survive it.
    drop(conn);
    assert!(h.db_seen("a.txt"), "a service stop keeps the marks");
    assert!(
        crate::cfapi_host::registration_survived(&h.root),
        "…and the registration"
    );

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let conn = crate::cfapi_host::register_and_connect_product_root(
        &h.root,
        FOLDER,
        cmd_tx,
        h.db_path.clone(),
    )
    .expect("reconnect");
    crate::cfapi_host::mark_root_for_full_teardown(&h.root);
    drop(conn);
    assert!(
        !h.db_seen("a.txt"),
        "the binding ended: the marks go before the unregister removes the placeholders"
    );
    assert!(
        !crate::cfapi_host::registration_survived(&h.root),
        "…and the registration is gone — the next start would be a fresh registration"
    );
}

/// A nest `SyncChange` creating `rel` — what another device's upload leaves in
/// `changes.list` for this host's fold.
fn remote_create(seq: i64, rel: &str, size: i64) -> fauna_protocol::sync::SyncChange {
    fauna_protocol::sync::SyncChange {
        seq,
        path_hash: format!("path-hash-{rel}"),
        // Any well-formed 32-byte head: nothing here hydrates it.
        manifest_hash: Some("5a".repeat(32)),
        size_bytes: size,
        change_type: "create".to_string(),
        created_at: 1_700_000_000_000,
        path: Some(rel.to_string()),
        device_id: Some("another-device".to_string()),
        ..Default::default()
    }
}

/// **A remote create into a directory the OS already listed MATERIALIZES — without a
/// restart and without a re-browse** (`delete-propagation.md` § *The floor on an on-demand
/// root*, decision (f); `on-demand-files.md` § *Sync direction*: two-way by default).
///
/// cfapi lists a directory ONCE: `transfer_placeholders` completes it
/// `DISABLE_ON_DEMAND_POPULATION`, so the OS never fires FETCH_PLACEHOLDERS for it again.
/// A fold that only writes the new row therefore leaves a file created on another device
/// invisible on this one — for the root until the next restart re-arms it, for a browsed
/// subdirectory for ever. The fold's new rows are materialized eagerly instead, and ONLY
/// into a directory the OS holds populated:
///
/// - `sub/new.txt` — a new file in a browsed subdirectory → a placeholder appears, badged
///   `CloudOnly`;
/// - `fresh/deep.txt` — a new file in a new directory under the browsed root → the root
///   gains the directory `fresh`, left LAZY, which lists `deep.txt` on its own first browse;
/// - `lazy/new.txt` — a new file in a directory the OS has never listed → nothing is
///   created (lazy population stays lazy: the directory lists it on its first browse, with
///   no clash against an eagerly created twin).
#[tokio::test]
async fn a_remote_create_into_a_browsed_directory_materializes() {
    let h = harness(&[("top.txt", 3), ("sub/old.txt", 3), ("lazy/old.txt", 3)]).await;
    let mut host = h.host(HashMap::new());
    host.fold_on_repull = vec![
        remote_create(10, "sub/new.txt", 5),
        remote_create(11, "fresh/deep.txt", 6),
        remote_create(12, "lazy/new.txt", 7),
    ];
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = broadcast::channel(64);
    let (wake_tx, wake_rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");

    let root_str = h.root_str();
    let at = |rel: &str| format!("{root_str}\\{}", rel.replace('/', "\\"));
    let state = |rel: &str| {
        fauna_cfapi::placeholder_state(std::path::Path::new(&at(rel)))
            .map(|s| format!("0x{s:x}"))
            .unwrap_or_else(|e| format!("err({e})"))
    };
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        h.root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        Some(wake_rx),
        None,
    );

    let driver = async {
        eprintln!("[measure] root before any browse: {}", state(""));
        let (r, s) = (at(""), at("sub"));
        let (top, sub) = tokio::task::spawn_blocking(move || {
            (dir_from_another_process(&r), dir_from_another_process(&s))
        })
        .await
        .expect("browse");
        assert!(
            top.contains(&"sub".to_string()) && top.contains(&"lazy".to_string()),
            "precondition: the root lists its directories. Got {top:?}"
        );
        assert_eq!(sub, vec!["old.txt".to_string()], "precondition: sub lists");
        eprintln!(
            "[measure] after browsing root + sub: root={} sub={} lazy={}",
            state(""),
            state("sub"),
            state("lazy")
        );

        // Another device creates three files; the nest nudges this folder.
        wake_tx.send(()).await.expect("nudge the loop");
        eventually(
            || h.db_state("lazy/new.txt") == Some(SyncState::Placeholder),
            Duration::from_secs(10),
            "the nudge's re-pull to fold the remote creates into rows",
        )
        .await;
        eventually(
            || std::path::Path::new(&at("sub/new.txt")).exists(),
            Duration::from_secs(10),
            "the remote create in the browsed subdirectory to materialize on disk — \
             absent = the fold wrote the row only, and the OS never lists `sub` again",
        )
        .await;

        let (r, s, f) = (at(""), at("sub"), at("fresh"));
        let (top, sub, fresh) = tokio::task::spawn_blocking(move || {
            (
                dir_from_another_process(&r),
                dir_from_another_process(&s),
                dir_from_another_process(&f),
            )
        })
        .await
        .expect("re-browse");
        assert!(
            sub.contains(&"new.txt".to_string()),
            "Explorer must list the remote create in the browsed subdirectory. Got {sub:?}"
        );
        assert!(
            top.contains(&"fresh".to_string()),
            "the new directory must appear in the browsed root. Got {top:?}"
        );
        assert_eq!(
            fresh,
            vec!["deep.txt".to_string()],
            "the eagerly created directory is left lazy — its own first browse lists it"
        );
        assert!(
            bytes_are_freed(&at("sub/new.txt")),
            "the materialized file is a cloud-only placeholder, not bytes"
        );
        // Decision (a): the create put the placeholder on the disk, so its row is
        // seen — its later absence is a delete. The never-listed directory's row is
        // not on the disk, so it is not seen until its own listing puts it there.
        assert!(
            h.db_seen("sub/new.txt"),
            "a materialized create is seen, beside `transfer_placeholders`' own marks"
        );
        assert!(
            !h.db_seen("lazy/new.txt"),
            "a row pushed nowhere is not seen before its directory is browsed"
        );

        // The never-listed directory stayed lazy: nothing was pushed into it, and its
        // first browse lists old and new alike.
        eprintln!("[measure] lazy before its first browse: {}", state("lazy"));
        let l = at("lazy");
        let mut lazy = tokio::task::spawn_blocking(move || dir_from_another_process(&l))
            .await
            .expect("browse lazy");
        lazy.sort();
        assert_eq!(
            lazy,
            vec!["new.txt".to_string(), "old.txt".to_string()],
            "a never-listed directory populates lazily, once, with every row"
        );
        eventually(
            || h.db_seen("lazy/new.txt"),
            Duration::from_secs(10),
            "the lazy directory's listing to mark its placeholders seen",
        )
        .await;
        eprintln!("[measure] lazy after its first browse: {}", state("lazy"));

        let events = drained_status_events(&mut event_rx);
        assert!(
            events.contains(&(at("sub/new.txt"), FileStatus::CloudOnly)),
            "the materialized placeholder's overlay is pushed CloudOnly (got {events:?})"
        );
        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// A nest `SyncChange` moving `rel`'s head to a new version of `size` bytes, made at
/// `created_at_ms` — another device's edit, as `changes.list` carries it.
fn remote_modify(
    seq: i64,
    rel: &str,
    size: i64,
    created_at_ms: i64,
) -> fauna_protocol::sync::SyncChange {
    fauna_protocol::sync::SyncChange {
        change_type: "modify".to_string(),
        // Unlike every seeded head, so the fold sees the head move.
        manifest_hash: Some("5b".repeat(32)),
        created_at: created_at_ms,
        ..remote_create(seq, rel, size)
    }
}

/// How this device holds the file when another device's edit lands.
#[derive(Clone, Copy, Debug)]
enum HeldAs {
    /// Opened here before: a hydrated (`Synced`) copy, which the fold reports stale and
    /// [`crate::bridge::apply_stale_hydrated`] invalidates.
    Hydrated,
    /// Listed but never opened: a cloud-only placeholder, whose row the fold re-points.
    CloudOnly,
}

/// **Another device's edit reaches this one whole, whatever it did to the file's size**
/// (`on-demand-files.md` § *Sync direction*: an on-demand root downloads remote edits like a
/// full root; § On-Demand Files: a placeholder shows the right size and modification time).
///
/// The edit moves the head; the nest nudges the folder; the loop's re-pull folds the change.
/// Before anything opens the file, its placeholder must already describe the NEW version —
/// size and mtime — because cfapi asks the provider for exactly `[0, the placeholder's
/// size)`. A placeholder left at the old size made the next open serve a PREFIX of a grown
/// file, which the engine then recorded, read back as a local edit and uploaded over the
/// real one: the 2026-10-08 native seat pair's lost update at `grow`. Then the file is
/// opened from another process, and the reader must get the new version, every byte.
async fn a_remote_edit_reaches_the_file_whole(held: HeldAs, old: &[u8], new: &[u8]) {
    const REL: &str = "edit.txt";
    const EDITED_AT_S: i64 = 1_700_000_100;

    let h = harness(&[(REL, old.len() as i64)]).await;
    let mut host = h.host(HashMap::from([(REL.to_string(), old.to_vec())]));
    host.fold_on_repull = vec![remote_modify(10, REL, new.len() as i64, EDITED_AT_S * 1000)];
    let nest_serves = Arc::clone(&host.blobs);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (wake_tx, wake_rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        None,
        h.root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        Some(wake_rx),
        None,
    );

    let abs = h.root.join(REL);
    let placeholder = abs.to_string_lossy().to_string();
    let measure = |when: &str| {
        let len = std::fs::metadata(&abs).map(|m| m.len());
        let state = fauna_cfapi::placeholder_state(&abs);
        eprintln!("[measure] {held:?} {when}: len={len:?} state={state:x?}");
    };
    let driver = async {
        let (root_str, p, dst) = (
            h.root_str(),
            placeholder.clone(),
            h._tmp.path().join("before.bin"),
        );
        let before = tokio::task::spawn_blocking(move || {
            dir_from_another_process(&root_str);
            match held {
                HeldAs::Hydrated => Some(read_from_another_process(&p, &dst)),
                HeldAs::CloudOnly => None,
            }
        })
        .await
        .expect("browse");
        if let Some(before) = before {
            assert_eq!(
                before, old,
                "precondition: the first open serves the old version"
            );
            eventually(
                || h.db_state(REL) == Some(SyncState::Synced),
                Duration::from_secs(10),
                "the first open's flip to Synced",
            )
            .await;
        }
        measure("before the edit");

        // Another device's edit lands on the nest, and the nest nudges this folder.
        nest_serves
            .lock()
            .unwrap()
            .insert(REL.to_string(), new.to_vec());
        wake_tx.send(()).await.expect("nudge the loop");
        eventually(
            || {
                h.db_state(REL) == Some(SyncState::Placeholder)
                    && h.db_size(REL) == Some(new.len() as i64)
            },
            Duration::from_secs(10),
            "the re-pull to point the row at the new head",
        )
        .await;
        eventually(
            || std::fs::metadata(&abs).is_ok_and(|m| m.len() == new.len() as u64),
            Duration::from_secs(5),
            "the placeholder on disk to describe the new version's size — a placeholder left \
             at the old size makes the next open serve [0, old size) of the new body",
        )
        .await;
        measure("after the edit");
        assert_eq!(
            std::fs::metadata(&abs)
                .and_then(|m| m.modified())
                .expect("stat the placeholder"),
            std::time::UNIX_EPOCH + Duration::from_secs(EDITED_AT_S as u64),
            "the placeholder must carry the new version's modification time"
        );

        let (p, dst) = (placeholder.clone(), h._tmp.path().join("after.bin"));
        let after = tokio::task::spawn_blocking(move || read_from_another_process(&p, &dst))
            .await
            .expect("open after the edit");
        assert_eq!(
            after,
            new,
            "the open after another device's edit must read the new version whole \
             (got {} bytes of {})",
            after.len(),
            new.len()
        );
        measure("after the second open");
        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// 6 B → 248 B on a copy opened here — the native seat pair's `grow` step.
#[tokio::test]
async fn a_remote_edit_that_grows_a_hydrated_file_reaches_it_whole() {
    a_remote_edit_reaches_the_file_whole(HeldAs::Hydrated, b"short\n", &[b'g'; 248]).await;
}

/// 248 B → 6 B on a copy opened here — the seat pair's `shrink` step.
#[tokio::test]
async fn a_remote_edit_that_shrinks_a_hydrated_file_reaches_it_whole() {
    a_remote_edit_reaches_the_file_whole(HeldAs::Hydrated, &[b's'; 248], b"short\n").await;
}

/// A copy opened here, emptied on another device.
#[tokio::test]
async fn a_remote_edit_that_empties_a_hydrated_file_reaches_it() {
    a_remote_edit_reaches_the_file_whole(HeldAs::Hydrated, b"soon gone\n", b"").await;
}

/// 6 B → 248 B on a file this device listed but never opened.
#[tokio::test]
async fn a_remote_edit_that_grows_a_cloud_only_file_reaches_it_whole() {
    a_remote_edit_reaches_the_file_whole(HeldAs::CloudOnly, b"short\n", &[b'g'; 248]).await;
}

/// **A remote edit never frees an unsynced local edit.** The invalidation of a superseded
/// copy is gated by the platform's dirty-file refusal and by nothing else
/// ([`crate::bridge::PlaceholderInvalidator::dehydrate`]'s contract): a file written since
/// it was hydrated is not in sync, so the invalidation must be REFUSED, the row left `Synced`
/// for the conflict path, and the local bytes left whole. Written in place (a replace-save
/// would destroy the placeholder and refuse for an unrelated reason).
#[tokio::test]
async fn a_remote_edit_never_frees_an_unsynced_local_edit() {
    const RECORDED: &[u8] = b"the version both devices hold";
    // At least as long as RECORDED: the in-place write must replace every byte.
    const LOCAL: &[u8] = b"edited here and not uploaded yet -- the only copy of it anywhere";

    let h = harness(&[("notes.txt", RECORDED.len() as i64)]).await;
    let _conn = hydrate_recorded_notes(&h, RECORDED).await;
    let abs = h.root.join("notes.txt");
    {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&abs)
            .and_then(|mut f| f.write_all(LOCAL))
            .expect("the in-place local edit");
    }
    let state = fauna_cfapi::placeholder_state(&abs).expect("read the placeholder state");
    eprintln!("[measure] after the local edit: state=0x{state:x}");
    assert_eq!(
        state & CF_PLACEHOLDER_STATE_IN_SYNC,
        0,
        "precondition: the local edit cleared the in-sync bit"
    );

    let host = h.host(HashMap::new());
    let superseded_by = StaleHydratedRow {
        relative_path: "notes.txt".to_string(),
        manifest_hash: ContentHash::of_raw(b"the other device's head"),
        size_bytes: 300,
        content_key_version: None,
        remote_mtime: 1_700_000_100,
        version_num: 1,
    };
    let invalidated = crate::bridge::apply_stale_hydrated(
        &host,
        &CfapiInvalidator,
        &h.root,
        &[superseded_by],
        None,
    )
    .await;
    assert_eq!(invalidated, 0, "the invalidation must be refused");
    assert_eq!(
        std::fs::read(&abs).expect("read the local edit back"),
        LOCAL,
        "the unsynced local edit must survive whole"
    );
    assert_eq!(
        h.db_state("notes.txt"),
        Some(SyncState::Synced),
        "the row is left for the conflict path, not re-pointed"
    );
}

/// **The primitive, on its own: `supersede_placeholder` frees a hydrated, in-sync file and
/// leaves it describing the new version** — size, mtime, cloud-only, still in sync (so the
/// next remote edit's update is not refused). The platform's answers, measured.
#[tokio::test]
async fn supersede_placeholder_redescribes_a_hydrated_file() {
    const RECORDED: &[u8] = b"the version both devices hold";

    let h = harness(&[("notes.txt", RECORDED.len() as i64)]).await;
    let _conn = hydrate_recorded_notes(&h, RECORDED).await;
    let abs = h.root.join("notes.txt");
    eprintln!(
        "[measure] hydrated: state={:x?}",
        fauna_cfapi::placeholder_state(&abs)
    );

    let superseded = fauna_cfapi::supersede_placeholder(&abs, 300, 1_700_000_100);
    let state = fauna_cfapi::placeholder_state(&abs);
    eprintln!("[measure] supersede -> {superseded:?}; state={state:x?}");
    superseded.expect("CfUpdatePlaceholder on a hydrated, in-sync file");

    let meta = std::fs::metadata(&abs).expect("stat");
    assert_eq!(meta.len(), 300, "the new version's size");
    assert_eq!(
        meta.modified().expect("mtime"),
        std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_100),
        "the new version's mtime"
    );
    assert!(
        bytes_are_freed(&abs.to_string_lossy()),
        "the old version's bytes are freed"
    );
    assert_ne!(
        state.expect("state") & CF_PLACEHOLDER_STATE_IN_SYNC,
        0,
        "still in sync: the next remote edit's update must not be refused"
    );
}

/// **A pinned file ("Always keep on this device") another device edited: what the platform
/// answers.** Measurement only — the update dehydrates, and cfapi may refuse that on a pinned
/// file; whether it does decides whether a pinned file ever receives a remote edit.
#[tokio::test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
async fn diag_supersede_a_pinned_file() {
    const RECORDED: &[u8] = b"the version both devices hold";

    let h = harness(&[("notes.txt", RECORDED.len() as i64)]).await;
    let _conn = hydrate_recorded_notes(&h, RECORDED).await;
    let abs = h.root.join("notes.txt");
    fauna_cfapi::set_pin_state(&abs, true).expect("pin");
    let attrs = |p: &std::path::Path| {
        use std::os::windows::fs::MetadataExt;
        std::fs::metadata(p).map(|m| format!("0x{:08x}", m.file_attributes()))
    };
    eprintln!("[measure] pinned: attrs={:?}", attrs(&abs));
    let superseded = fauna_cfapi::supersede_placeholder(&abs, 300, 1_700_000_100);
    eprintln!(
        "[measure] supersede a pinned file -> {superseded:?}; attrs={:?} len={:?}",
        attrs(&abs),
        std::fs::metadata(&abs).map(|m| m.len())
    );
    let bare = fauna_cfapi::dehydrate_placeholder(&abs);
    eprintln!("[measure] bare dehydrate of it -> {bare:?}");
}

/// `CF_PLACEHOLDER_STATE_IN_SYNC` — the bit `CF_INSYNC_POLICY_TRACK_ALL` clears on a local
/// write.
const CF_PLACEHOLDER_STATE_IN_SYNC: u32 = 0x8;

/// **The live-box bridge: hold a real, served sync root open so Explorer (or a UIA script) has
/// something to right-click.** It asserts nothing — it is the missing piece: every headless dehydrate trigger is measured
/// ([`diag_cross_process_dehydrate`]) and none reproduces an OS dehydrate, so whether Explorer's
/// *"Free up space"* verb appears / completes / flips the badge is observable only against a
/// real shell. This holds a registered root with one **hydrated** (`Synced`) file — without the
/// brittle FaunaApp-login → capability-provision chain — while a dev-fleet UIA
/// script drives the menu against the printed path.
///
/// ```text
/// cmd /c "scripts\cargo-win.cmd test -p fauna-sync-agenthold_a_live_root_for_explorer -- --ignored --nocapture"
/// ```
///
/// Prints `HOLD:`-prefixed machine-parseable lines: `root=`/`file=` once, then a
/// `row=… freed=… badge=…` line at start and on every change, plus every `Event` the loop
/// pushes — so the badge-flip half of the LEAD's question is answered from this log,
/// headlessly, the moment the shell verb acts.
///
/// `FAUNA_HOLD_SECS` bounds the hold (default 300) — libtest has no timeout, so an unbounded
/// hold would wedge a stray `--include-ignored` run forever.
#[tokio::test]
#[ignore = "live-box harness: holds a served root open for Explorer. Run with --ignored --nocapture"]
async fn hold_a_live_root_for_explorer() {
    const BYTES: &[u8] = b"hello from the fauna live root\r\n";
    let hold = Duration::from_secs(
        std::env::var("FAUNA_HOLD_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300),
    );

    let h = harness(&[("hello.txt", BYTES.len() as i64)]).await;
    let host = h.host(HashMap::from([("hello.txt".to_string(), BYTES.to_vec())]));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    // `FAUNA_HOLD_SHELL=1` registers the root the way the PRODUCT does
    // (`register_and_connect_shell`: `StorageProviderSyncRootManager` + filter +
    // connect) — the registration Explorer's cloud verbs ("Free up space") are
    // keyed on; the bare filter registration renders none of that UX. So a UIA
    // pass against this hold observes the exact registration a real user's root
    // carries, per-folder display name and all.
    let with_shell = std::env::var("FAUNA_HOLD_SHELL").as_deref() == Ok("1");
    let conn = if with_shell {
        crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
            .expect("connect (product shell registration)")
    } else {
        crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("connect")
    };
    let shell_id = fauna_cfapi::shell_registration_for(&h.root_str()).expect("shell query");

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\hello.txt");
        // Populate + hydrate from another process, so the file starts `Synced` — the state
        // Explorer offers "Free up space" *from*.
        let (ar, ap) = (root_str.clone(), placeholder.clone());
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&ar);
            read_from_another_process(&ap, &dst)
        })
        .await
        .expect("hydrate");

        eprintln!("HOLD: root={root_str}");
        eprintln!("HOLD: file={placeholder}");
        eprintln!("HOLD: shell={}", shell_id.as_deref().unwrap_or("none"));
        eprintln!(
            "HOLD: holding for {}s — drive Explorer against the file path above now",
            hold.as_secs()
        );

        let deadline = std::time::Instant::now() + hold;
        let mut req_id = 100u64;
        let mut last = None;
        loop {
            while let Ok(ev) = event_rx.try_recv() {
                eprintln!("HOLD: event={:?}", ev.event);
            }
            req_id += 1;
            let cur = (
                h.db_state("hello.txt"),
                bytes_are_freed(&placeholder),
                h.overlay_status("hello.txt", req_id).await,
            );
            if last.as_ref() != Some(&cur) {
                eprintln!("HOLD: row={:?} freed={} badge={:?}", cur.0, cur.1, cur.2);
                last = Some(cur);
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);

    // Remove the registration before the temp root vanishes — a stale
    // `SyncRootManager` row keeps a ghost provider in Explorer's nav pane. The
    // product guard keeps the registration on a plain drop by design, so mark
    // the binding as ended first; the drop then runs the full measured-order
    // teardown (disconnect → filter → shell).
    if with_shell {
        crate::cfapi_host::mark_root_for_full_teardown(&h.root);
    }
    drop(conn);
}

/// **The core contract: a directory listing populates lazily** (`file-sync.md` § On-Demand
/// Files) — driven through the *service's own* cfapi host, end to end.
///
/// Another process browses a real sync root; Windows fires FETCH_PLACEHOLDERS into
/// `cfapi_host`'s `extern "system"` callback; it routes by connection key to this engine's
/// command channel; `run_hydration_loop` serves it off the real `SyncDb` and completes it via
/// the real `CfApiPlaceholderSink`. The placeholders then exist for **another process** — the
/// user's ground truth — and the overlay-status query the shell extension makes reports them
/// `CloudOnly`.
///
/// This is the test that would have caught the `FileIdentity` bug (Track Y) at the product
/// boundary rather than after a human noticed Explorer was empty.
#[tokio::test]
async fn the_services_own_cfapi_host_populates_a_real_sync_root() {
    let h = harness(&[("hello.txt", 11), ("sub/deep.txt", 3)]).await;
    let host = h.host(HashMap::new());

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    // The real product call: register + connect the sync root and wire the real callbacks.
    // The returned guard unregisters the root on the way out, even on an assertion unwind.
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        // Blocking, and it must be: the child process waits on cfapi, which waits on our loop.
        // `spawn_blocking` keeps the loop being polled on this thread while `dir` runs.
        let listing = tokio::task::spawn_blocking(move || dir_from_another_process(&root_str))
            .await
            .expect("dir task");

        assert!(
            listing.contains(&"hello.txt".to_string()),
            "the tracked file must materialize as a placeholder another process can see — \
             that is what on-demand population *is*, and what the user sees in Explorer. \
             Got: {listing:?}"
        );
        assert!(
            listing.contains(&"sub".to_string()),
            "a deeper row's first segment must appear as a synthesized directory child \
             (enumerate::immediate_children). Got: {listing:?}"
        );

        // The overlay-badge query path, over the live IPC handler, on the same DB: a
        // populated-but-unhydrated file is CloudOnly.
        assert_eq!(
            h.overlay_status("hello.txt", 10).await,
            FileStatus::CloudOnly,
            "a populated placeholder must query as CloudOnly — the badge Explorer draws"
        );

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

/// Does the OS say this file's bytes are **not on this disk**?
///
/// Read straight from the raw attribute bits rather than through the engine's own
/// `is_cloud_placeholder` — a test that asks the code under test whether the code under test is
/// right proves nothing. These are the values *measured* on a live sync root (2026-07-14):
///
/// | state | attrs | OFFLINE | RECALL_ON_DATA_ACCESS |
/// |---|---|---|---|
/// | cloud-only placeholder | `0x00401620` | yes | yes |
/// | hydrated | `0x00000420` | no | no |
/// | overwritten by another process | `0x00000020` | no | no |
fn on_disk_is_cloud_only(path: &std::path::Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x0000_1000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    let attrs = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("metadata({}) failed: {e}", path.display()))
        .file_attributes();
    attrs & (OFFLINE | RECALL_ON_DATA_ACCESS) != 0
}

/// **THE DATA-INTEGRITY GUARD.** A reconcile pass over an on-demand root must never read,
/// re-hash, mark, or upload a **cloud-only placeholder** — and must not stall trying.
///
/// This is the test that fails loudly if a future session reaches for the one-line "fix" of
/// calling [`SyncEngine::reconcile`] from the on-demand rescan tick. **What that actually does
/// was measured, not assumed** (2026-07-14) — and the answer is not the one the hazard was
/// written up as:
///
/// - A placeholder does **not** read as a 0-byte file. `metadata.len()` is the *real* size, and
///   `full_scan_filtered` reports it faithfully. So there is no "upload emptiness over the
///   nest's copy" path — that predicted data loss **does not exist**.
/// - What *does* happen: `content_hash_streaming` opens it, the read fires a `FETCH_DATA` that —
///   **by cfapi's own rule — is never delivered to the provider's own process** — and the read
///   therefore blocks for cfapi's full **60-second** timeout before failing
///   (`ERROR_CLOUD_FILE_REQUEST_TIMEOUT`, os error 426). `reconcile` then hits `continue`.
///
/// So the naive fix does not corrupt the nest; it **wedges the root**. Those 60 s are burned on
/// the engine's single driving thread — the *same* thread that serves hydration — so an
/// N-placeholder set spends N × 60 s unable to hydrate anything at all. The feature dies quietly
/// while every log line still says "reconciliation complete".
///
/// Hence the assertions: rows untouched, nothing uploaded, files still cloud-only on disk — and
/// **it finishes fast**. The timing assertion is the one that goes red on today's code (3
/// placeholders ≈ 180 s), and it is not a flaky perf check: 60 s is a hard OS constant, and a
/// guarded pass over 3 files is milliseconds. The bound below sits an order of magnitude from
/// both.
#[tokio::test]
async fn a_cloud_only_placeholder_is_never_uploaded() {
    let rows: &[(&str, i64)] = &[("a.txt", 11), ("b.txt", 11), ("c.txt", 11)];
    let h = harness(rows).await;
    // No blobs: nothing may hydrate during this test. A pass that somehow *did* fetch would
    // fail here rather than quietly succeed.
    let host = h.host(HashMap::new());

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        // Materialize the placeholders on disk, from another process (the provider's own I/O
        // fires no callbacks), exactly as Explorer would.
        let rs = root_str.clone();
        tokio::task::spawn_blocking(move || dir_from_another_process(&rs))
            .await
            .expect("dir task");
        for (rel, _) in rows {
            assert!(
                on_disk_is_cloud_only(&root.join(rel)),
                "{rel} must be a cloud-only placeholder before the reconcile — otherwise this \
                 test is not exercising the hazard at all"
            );
        }

        // The real production engine, over the real root and the real DB.
        let engine = h.engine();

        let started = std::time::Instant::now();
        let stats = engine
            .reconcile()
            .await
            .expect("a reconcile over a placeholder root must succeed, not error");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(15),
            "reconcile took {elapsed:?} for {} placeholders. It must SKIP them on their \
             attributes, never open them: the provider's own read of its own placeholder \
             blocks for cfapi's 60 s timeout (measured), and it burns that on the single \
             thread that also serves hydration — so this is the whole on-demand root going \
             dead, not a slow scan.",
            rows.len()
        );

        assert_eq!(
            (stats.new_files, stats.modified_files, stats.deleted_files),
            (0, 0, 0),
            "a cloud-only placeholder is not new, not modified, and not deleted — it is simply \
             not here. Marking one LocallyModified is what would queue it for upload."
        );
        assert_eq!(
            stats.placeholder_files,
            rows.len(),
            "every placeholder must be *accounted for* as skipped, not silently absent — an \
             unaccounted file is one the delete-detection could still take for a deletion"
        );

        let (recorded, bytes) = engine
            .upload_pending(2)
            .await
            .expect("upload_pending must not fail");
        assert_eq!(
            (recorded.len(), bytes),
            (0, 0),
            "nothing may be queued for upload from a root whose files are all cloud-only"
        );

        for (rel, _) in rows {
            assert_eq!(
                h.db_state(rel),
                Some(SyncState::Placeholder),
                "{rel} must still be Placeholder — the reconcile must not have touched its row"
            );
            assert!(
                on_disk_is_cloud_only(&root.join(rel)),
                "{rel} must still be cloud-only on disk: the scan must not have hydrated it as \
                 a side effect of looking at it"
            );
        }

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

/// **THE SECOND TRAP — and the one that really does destroy data.**
///
/// The obvious remedy for the 60 s stall above is "drop placeholders from the scan". That is a
/// *worse* bug than the one it fixes, and it is silent.
///
/// `reconcile`'s delete-detection reads every `Synced` row that the scan did **not** report and
/// concludes the user deleted the file: it removes the row and records a `delete` on the nest,
/// which propagates to **every other device**. So a file that is `Synced` in the DB but a
/// placeholder on disk — the transient window before an OS "Free up space"
/// (`NOTIFY_DEHYDRATE_COMPLETION`, now registered) flips the row, or any residual
/// `Synced`-over-placeholder state — would be deleted *everywhere* by the next rescan. Freeing
/// disk space would delete your file. The placeholder guard is what makes it safe regardless of
/// whether the row-flip has landed yet.
///
/// The rule this pins: a placeholder is **present-but-unreadable**, never **absent**. It stays
/// in the scan (so delete-detection sees it) and carries a flag (so the hash path skips it).
#[tokio::test]
async fn a_dehydrated_file_is_never_recorded_as_a_delete() {
    let h = harness(&[("kept.txt", 11)]).await;
    let host = h.host(HashMap::new());

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let rs = root_str.clone();
        tokio::task::spawn_blocking(move || dir_from_another_process(&rs))
            .await
            .expect("dir task");

        // The dehydrated-but-Synced state: bytes gone from disk, row still says Synced.
        h.force_state("kept.txt", SyncState::Synced);
        assert!(on_disk_is_cloud_only(&root.join("kept.txt")));

        let engine = h.engine();
        let stats = engine.reconcile().await.expect("reconcile must succeed");

        assert_eq!(
            stats.deleted_files, 0,
            "a file whose bytes were freed is still THE USER'S FILE. Recording a delete here \
             would erase it from the nest and from every other device — because they chose to \
             save disk space."
        );
        assert!(
            h.db_state("kept.txt").is_some(),
            "the row must survive: reconcile deletes the row and records the delete on the nest \
             in the same breath, so a missing row IS the data loss"
        );

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

/// **The other half of the contract: opening a placeholder hydrates it** (`file-sync.md`
/// § On-Demand Files) — again through the service's own host.
///
/// Another process reads the placeholder; Windows fires FETCH_DATA; the real callback routes a
/// `HydrationCommand::Fetch` to the loop; `serve_fetch` pulls the bytes and completes the
/// transfer via the real `CfApiTransferSink`. Then the producer half must fire too: the row
/// flips to `Synced` in the real DB, the overlay query agrees, and a `FileStatusChanged{Synced}`
/// is pushed so Explorer's badge flips live without a re-query.
///
/// **This is the flip that `file-sync.md` called "a manual check".** It is not one any more.
#[tokio::test]
async fn opening_a_placeholder_hydrates_it_and_flips_the_overlay_to_synced() {
    const BYTES: &[u8] = b"hello world";

    let h = harness(&[("hello.txt", BYTES.len() as i64)]).await;
    let host = h.host(HashMap::from([("hello.txt".to_string(), BYTES.to_vec())]));

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\hello.txt");
        let dst2 = dst.clone();
        // Populate first (the placeholder must exist before it can be opened), then read it —
        // both from another process, both driving real cfapi callbacks.
        let bytes = tokio::task::spawn_blocking(move || {
            dir_from_another_process(&root_str);
            read_from_another_process(&placeholder, &dst2)
        })
        .await
        .expect("hydrate task");

        assert_eq!(
            bytes, BYTES,
            "the OS must have received exactly the bytes the engine served — this is \
             CfExecute(TRANSFER_DATA) round-tripping through the real sync root"
        );

        // The producer half: hydration must be *recorded*, or the badge and the query path
        // disagree with what is actually on disk.
        assert_eq!(
            h.db_state("hello.txt"),
            Some(SyncState::Synced),
            "a served fetch must mark the row Synced (HydrationHost::mark_hydrated)"
        );
        assert_eq!(
            h.overlay_status("hello.txt", 10).await,
            FileStatus::Synced,
            "the overlay query must agree with the hydrated on-disk state"
        );

        // A served fetch pushes *two* kinds of event, in order: per-chunk `SyncProgress` (the
        // in-flight progress bar, from `ProgressTransferSink`) and then the terminal
        // `FileStatusChanged{Synced}` (the badge flip). Drain and assert on both rather than
        // on whichever happens to be first — the ordering is the product's business.
        let mut progressed = false;
        let mut flipped = None;
        while let Ok(Event { event }) = event_rx.try_recv() {
            match event {
                EventKind::SyncProgress(p) => {
                    progressed = true;
                    assert_eq!(
                        (p.bytes_done, p.bytes_total),
                        (BYTES.len() as u64, BYTES.len() as u64),
                        "progress must account for every byte served"
                    );
                }
                EventKind::FileStatusChanged { path, status } => flipped = Some((path, status)),
                _ => {}
            }
        }

        assert!(
            progressed,
            "hydrating a file must emit per-chunk SyncProgress — the in-flight UX"
        );
        let (path, status) = flipped.expect(
            "hydrating a file must push a terminal FileStatusChanged — without it Explorer's \
             badge never flips until the shell's 30 s cache re-queries",
        );
        assert_eq!(
            status,
            FileStatus::Synced,
            "the badge flips CloudOnly → Synced"
        );
        assert!(
            path.ends_with("hello.txt"),
            "the pushed path must be the absolute file path Explorer keys on, got {path}"
        );

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

/// **The provider's own dehydrate fires nothing — which is why Fauna's shell verb records the
/// row itself.**
///
/// cfapi suppresses `NOTIFY_*` for I/O originating in the provider's own process — the same rule
/// that makes the populate/hydrate tests drive `dir`/`copy` from `cmd`. So Fauna's *own* "Free up
/// space" menu verb cannot learn of its own dehydrate from a callback, and
/// `pipe_server::handle_free_space` therefore marks the row and pushes the event **directly**
/// (`file-sync.md` § Per-file sync-status display). The `NOTIFY_DEHYDRATE_COMPLETION`
/// registration exists solely for the *other* dehydrate — the OS/user-initiated one, via
/// Explorer's native "Free up space" or Storage Sense.
///
/// This pins the asymmetry instead of leaving it in prose, because it is load-bearing in both
/// directions: it is why `handle_free_space`'s manual row-record is not redundant, and it is why
/// an in-process dehydrate can never stand in as a test of the callback. The bytes really are
/// freed here — asserted on disk — and still no callback arrives: that is suppression, not a
/// no-op.
#[tokio::test]
async fn an_in_process_dehydrate_fires_no_callback() {
    const BYTES: &[u8] = b"hello world";

    let h = harness(&[("hello.txt", BYTES.len() as i64)]).await;
    let host = h.host(HashMap::from([("hello.txt".to_string(), BYTES.to_vec())]));

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("hydrated-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root,
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let placeholder = format!("{root_str}\\hello.txt");

        let (arrange_root, arrange_path) = (root_str.clone(), placeholder.clone());
        tokio::task::spawn_blocking(move || {
            dir_from_another_process(&arrange_root);
            read_from_another_process(&arrange_path, &dst)
        })
        .await
        .expect("hydrate task");
        assert_eq!(
            h.db_state("hello.txt"),
            Some(SyncState::Synced),
            "precondition: the file is hydrated and recorded Synced"
        );

        // Act — dehydrate as the provider itself, in this very process.
        let act_path = placeholder.clone();
        tokio::task::spawn_blocking(move || {
            fauna_cfapi::dehydrate_placeholder(std::path::Path::new(&act_path))
                .expect("the provider's own dehydrate still frees the bytes")
        })
        .await
        .expect("dehydrate task");

        assert!(
            bytes_are_freed(&placeholder),
            "the dehydrate itself must have worked — otherwise the assertion below is vacuous"
        );

        // A callback, had one been coming, would beat this by orders of magnitude: the
        // out-of-process test above sees the row flip in well under a second.
        tokio::time::sleep(Duration::from_secs(2)).await;

        assert_eq!(
            h.db_state("hello.txt"),
            Some(SyncState::Synced),
            "cfapi must fire NO callback for the provider's own dehydrate, leaving the row \
             stale-but-Synced. If this ever flips, cfapi changed: the re-exec in \
             `dehydrate_from_another_process` is then obsolete, and `handle_free_space`'s \
             manual row-record became a double-write"
        );

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// OS-initiated dehydrate ("Free up space") — what is proven, and what is still open
// ─────────────────────────────────────────────────────────────────────────────────────────
//
// PROVEN, headlessly:
//   * The row-flip mechanism, deterministically, by
//     `bridge::tests::loop_marks_placeholder_and_emits_cloud_only_on_dehydrate`: a
//     `HydrationCommand::Dehydrate` marks the row `Placeholder` (via `SyncEngine::mark_placeholder`,
//     its own engine test) and pushes `FileStatusChanged{CloudOnly}`.
//   * The suppression asymmetry that makes the whole design hang together, by
//     `an_in_process_dehydrate_fires_no_callback`: the provider's own dehydrate frees the bytes
//     and fires nothing, which is why `pipe_server::handle_free_space` records the row itself and
//     why `NOTIFY_DEHYDRATE_COMPLETION` is only about the OS/user-initiated dehydrate.
//
// STILL OPEN: whether the OS actually DELIVERS `NOTIFY_DEHYDRATE_COMPLETION` on a real Explorer
// "Free up space" / Storage Sense. No probe available headlessly reproduces an OS dehydrate —
// see `diag_cross_process_dehydrate` for the four that were measured, and re-run it (20 s) rather
// than trusting this comment. Summary: an ordinary open, `CfOpenFileWithOplock` and
// `CfSetPinState(UNPINNED)` all succeed cross-process but none dehydrate; `CfDehydratePlaceholder`
// cross-process hangs indefinitely.
//
// ⚠ Do NOT re-derive the retracted claim. It recorded the hang as being in
// `CfOpenFileWithOplock` and inferred from it that the OS gates third-party dehydration on the
// provider ACK-ing a pre-`NOTIFY_DEHYDRATE`. Both were retracted 2026-07-16: the oplock open
// measurably returns, and the inference never followed anyway — `dehydrate_placeholder` opens
// BEFORE it dehydrates (`fauna_cfapi::dehydrate_placeholder`), so a hang at the open precedes any
// dehydrate request, and there is no request for a NOTIFY_DEHYDRATE handler to ACK. That is why
// adding one changed nothing. The ACK design remains unjustified on present evidence, and
// registering the vetoable `NOTIFY_DEHYDRATE` is not free: a callback the provider MUST answer can
// block a user reclaiming disk, which is precisely why `_COMPLETION` was chosen
// (`file-sync.md` § Per-file sync-status display → *Windows OS shell-overlay carve-out*).
//
// The open question is a live-box one and needs a real Explorer, not a bigger unit harness:
// does "Free up space" appear on a Fauna cloud file, does it complete, and does the badge flip?
//

// ═══════════════════════════════════════════════════════════════════════════════════════
// TWO-WAY: an on-demand root uploads local edits (TRACK T, `file-sync.md` § Sync direction)
// ═══════════════════════════════════════════════════════════════════════════════════════

/// **THE CONTRACT THIS WHOLE TRACK EXISTS FOR: a local edit to a tracked file is uploaded.**
///
/// On-demand is a *storage* choice, never a *direction* choice (USER-ratified 2026-07-14). A
/// file that carries a Fauna badge is a file whose changes **must** reach the nest — no mode,
/// folder setting, or host may suppress that. Before this landed, the Windows on-demand root
/// ran **no watcher at all**: a user's edit to a hydrated placeholder was silently orphaned
/// (live-measured — two rescans logged `placeholders=0`, the row stayed `placeholder`, the
/// badge went on claiming `CloudOnly`). A client that badges a file as synced and then drops
/// the user's edit has told them their data is safe while losing it.
///
/// The test drives the *whole* real path: a real cfapi sync root, the real callbacks, the real
/// shared watcher/debouncer, and the **real `SyncEngine::upload_file`** — chunked, sealed and
/// POSTed to a wiremock nest. The edit is made **from another process**, because that is the
/// only way the OS reports it (cfapi fires nothing for the provider's own I/O) and because it
/// is what a user's editor actually is.
///
/// It pins BOTH halves of the two-way contract, and the first is as important as the second:
///
/// 1. **Hydration must NOT echo back up.** Serving a fetch writes bytes into the root, which
///    the watcher sees as an ordinary `Modified` event. If `mark_hydrated` recorded only
///    `state = Synced` and no content hash (as it did before 2026-07-14), `upload_file`'s
///    "already synced, skipping" short-circuit would miss, and the root would re-upload every
///    file it had just downloaded — bumping a nest version per hydration. So: after hydrating,
///    wait out the debounce and assert **nothing was uploaded**.
/// 2. **A real edit must go up.** Then write new bytes from another process and assert the
///    chunk POSTs land and the row's local identity becomes the *new* content's.
#[tokio::test]
async fn a_local_edit_by_another_process_is_detected_and_uploaded() {
    const ORIGINAL: &[u8] = b"hello world";
    const EDITED: &[u8] = b"the user typed this instead";

    let nest = MockServer::start().await;
    let chunk_posts = mount_nest_byte_plane(&nest).await;

    let h = harness_against_nest(&[("notes.txt", ORIGINAL.len() as i64)], &nest.uri()).await;
    let host = h.host(HashMap::from([(
        "notes.txt".to_string(),
        ORIGINAL.to_vec(),
    )]));

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let scratch = h._tmp.path().join("scratch.bin");
    let hydrated_copy = h._tmp.path().join("hydrated-copy.bin");

    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        // ── Arrange: populate, then hydrate, both from another process ──
        let placeholder = format!("{root_str}\\notes.txt");
        let (rs, ph, dst) = (root_str.clone(), placeholder.clone(), hydrated_copy.clone());
        let bytes = tokio::task::spawn_blocking(move || {
            dir_from_another_process(&rs);
            read_from_another_process(&ph, &dst)
        })
        .await
        .expect("hydrate task");
        assert_eq!(
            bytes, ORIGINAL,
            "the file must hydrate before it can be edited"
        );
        assert_eq!(
            h.db_state("notes.txt"),
            Some(SyncState::Synced),
            "a hydrated file is Synced — the precondition for the echo assertion below"
        );

        // The row's LOCAL IDENTITY — the thing the echo actually turns on, asserted rather
        // than inferred. `Synced` alone is a weak precondition: `mark_hydrated`'s partial-
        // hydration early return sets exactly that while leaving `local_hash` None. Splitting
        // the two means a red run says WHICH half broke — whether the engine failed to stamp
        // the served identity, or it stamped it and something downstream re-uploaded anyway.
        // (In 2026-08-04's investigation this line is what retired the entire upstream half:
        // the identity was present and correct, so the engine was never the defect.)
        assert_eq!(
            h.db_local_hash("notes.txt"),
            Some(ContentHash::of_raw(ORIGINAL)),
            "a full hydration must stamp the served content's identity on the row — that is \
             what `upload_file`'s skip compares against. `Synced` with `local_hash: None` is \
             `mark_hydrated`'s partial-hydration arm, and it re-uploads everything it downloads"
        );
        assert!(
            !bytes_are_freed(&placeholder),
            "the file is hydrated, so the OS must no longer report it as a cloud placeholder"
        );

        // ── Assert (1): the hydration itself must NOT be mistaken for a local edit ──
        // Hydration wrote bytes into the root; the watcher saw a Modified event. Wait past the
        // 2 s debounce and confirm the shared uploader refused to echo it back to the nest.
        tokio::time::sleep(Duration::from_millis(3500)).await;
        assert_eq!(
            chunk_posts.load(Ordering::SeqCst),
            0,
            "hydrating a file must NOT upload it straight back. The watcher sees our own \
             hydration write as an ordinary Modified event, so the ONLY thing standing between \
             a two-way root and re-uploading everything it downloads is `mark_hydrated` \
             recording the served content's identity (hash), which makes `upload_file`'s \
             'already synced, skipping' short-circuit fire. If this is non-zero, that regressed \
             and every hydration now bumps a nest version."
        );

        // ── Act: another process edits the tracked file ──
        write_from_another_process(&scratch, &root.join("notes.txt"), EDITED);

        // ── Assert (2): it is detected and really uploaded ──
        let want = ContentHash::of_raw(EDITED);
        eventually(
            || h.db_uploaded("notes.txt", want),
            Duration::from_secs(30),
            "the edited file to reach Synced carrying its NEW content identity — i.e. the shared \
             watcher saw the write, the debouncer released it, and the real \
             `SyncEngine::upload_file` chunked, sealed and *completed* the upload of THESE \
             bytes. A row stuck at `Placeholder` with the old hash is the pre-2026-07-14 bug: \
             the edit was never observed at all",
        )
        .await;

        assert!(
            chunk_posts.load(Ordering::SeqCst) > 0,
            "the edited bytes must actually reach the nest's byte plane. The DB row alone is \
             not the contract — 'uploaded' means the chunks left this machine."
        );

        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// **An archive-point seat bound to an on-demand folder actually backs up** — the
/// highest-severity instance of the one-way bug, and the one whose failure was *silent*.
///
/// A seat's sync mode (`Sync`/`Backup` — resolved from its **place** in the nest roster;
/// the folder-row `mode` that once carried it retired with the folders mode contraction) and
/// `LocationMode` (`Always`/`OnDemand` — a **device-local folder** property) are fully
/// orthogonal: different types, different layers, set by two unrelated calls, and
/// `handle_set_location_folder` never reads the seat's mode. So a user can bind a **Backup**
/// seat to an **on-demand** folder — and Backup is *upload-only*, so before this landed that
/// folder did **nothing in either direction**. Worse, because no change was ever *recorded*,
/// the snapshot scheduler (which fires on new recorded changes / `max_change_seq`) never ran
/// either: no record → no snapshot → **no point-in-time recovery**. The user's protection was
/// not degraded, it was *absent*. And the trap is human: Backup is the mode you choose because
/// you care about not losing the file; on-demand is the one you choose to save disk.
///
/// **The service is deliberately mode-blind, and that is why this is fixed by construction.**
/// There is no separate "backup detector": backup's local-change detection *is* this shared
/// watch→`upload_file` loop, and every folder's record lands on the one `sync_changes` head.
/// So what a backup seat needs from
/// this host is exactly what this test asserts: **a file appearing in the folder is adopted and
/// uploaded.** ("New files are adopted" is itself ratified — `file-sync.md` § Sync direction.)
///
/// The co-assertion is the sharp one. This runs a live watcher on a live cfapi root, where our
/// **own** `CfExecute(TRANSFER_PLACEHOLDERS)` materializes cloud-only files that the OS reports
/// as ordinary `Created` events — indistinguishable, to a naive watcher, from the user dropping
/// a file in. So the test demands the watcher tell them apart: the real new file goes up, and
/// the placeholder is *never touched* (uploading one would stall 60 s on a fetch cfapi never
/// delivers to the provider's own process, wedging the root).
#[tokio::test]
async fn a_backup_mode_set_on_an_on_demand_folder_uploads() {
    const FRESH: &[u8] = b"a file the user just dropped into their backup folder";

    let nest = MockServer::start().await;
    let chunk_posts = mount_nest_byte_plane(&nest).await;

    // One tracked, cloud-only file already in the set — the placeholder the watcher must NOT
    // mistake for a user's creation when we materialize it.
    let h = harness_against_nest(&[("cloud.txt", 11)], &nest.uri()).await;
    let host = h.host(HashMap::new());

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();

    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let scratch = h._tmp.path().join("scratch.bin");

    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        // Materialize the placeholder from another process — this is what fires the `Created`
        // events the watcher must learn to ignore.
        let rs = root_str.clone();
        tokio::task::spawn_blocking(move || dir_from_another_process(&rs))
            .await
            .expect("dir task");
        assert!(
            on_disk_is_cloud_only(&root.join("cloud.txt")),
            "cloud.txt must be a cloud-only placeholder, or the co-assertion below is vacuous"
        );

        // The user drops a brand-new file into the backup folder.
        write_from_another_process(&scratch, &root.join("fresh.txt"), FRESH);

        // It is adopted and uploaded — this is the backup that used to never happen.
        let want = ContentHash::of_raw(FRESH);
        eventually(
            || h.db_uploaded("fresh.txt", want),
            Duration::from_secs(30),
            "the new file to be adopted into the set and uploaded to completion. This IS the \
             backup path: backup has no separate detector — it is this shared watch→upload \
             loop, and no folder type changes which nest table the record lands in. \
             A folder that never uploads never records a change, and a set that never records a \
             change is never snapshotted: no point-in-time recovery at all",
        )
        .await;

        assert!(
            chunk_posts.load(Ordering::SeqCst) > 0,
            "the new file's bytes must actually reach the nest — an un-uploaded 'backup' is \
             not a backup"
        );

        // ── The co-assertion: the placeholder was NOT swept up with it ──
        assert_eq!(
            h.db_state("cloud.txt"),
            Some(SyncState::Placeholder),
            "the cloud-only placeholder must be untouched. Our OWN \
             CfExecute(TRANSFER_PLACEHOLDERS) materialized it, and the OS reported that as an \
             ordinary `Created` event — exactly like the user's new file. \
             `LocalWrites::is_user_write` is what tells them apart; without it the root tries \
             to upload every file it just advertised, each attempt stalling 60 s on a fetch \
             cfapi never delivers to the provider's own process."
        );
        assert!(
            on_disk_is_cloud_only(&root.join("cloud.txt")),
            "and it must still be cloud-only on disk — the watcher must not have hydrated it as \
             a side effect of looking at it"
        );

        cancel.cancel();
    };

    drive_until_asserted(loop_fut, driver).await;
}

// ═══════════════════════════════════════════════════════════════════════════════════════
// 0-BYTE PLACEHOLDERS (2026-07-15, `file-sync.md` § On-Demand Files → present-but-unreadable)
//
// Measured against a real cfapi root (the exploratory `measure_*` test these two replace):
//   M1 — a 0-byte entry DOES materialize as a placeholder another process can list.
//   M2 — after population it is a cloud-only placeholder on disk (attrs 0x00401420 =
//        OFFLINE|RECALL, minus the SPARSE bit a full-size placeholder carries).
//   M3 — opening it from another process CLEARS those bits (attrs 0x00000420, the "hydrated"
//        signature — the file is genuinely present-and-empty on disk) while delivering NO
//        FETCH_DATA to the host.
// So the row is stranded `Placeholder` → `CloudOnly` forever unless it is recorded `Synced`
// at *population* — the fetch path can never rescue it. The fix is in shared Rust
// (`record_placeholders_from_changes` + `list_placeholder_rows`); these pin its two observable
// halves at the real OS boundary.
// ═══════════════════════════════════════════════════════════════════════════════════════

/// **The measured justification for the size-aware badge — the fetch path can never fix this.**
///
/// A 0-byte placeholder is opened by another process. The OS clears its OFFLINE/RECALL bits —
/// the file is now present-and-empty on disk — but NO FETCH_DATA reaches the host, so
/// `mark_hydrated` never runs and the row STAYS a `Placeholder` (deliberately: a Synced-while-
/// absent row would be deleted by reconcile's delete-detection). A future session tempted to
/// "fix" the CloudOnly badge at the fetch callback will find there is no callback to hook — the
/// fix is at the overlay instead: `effective_for_size` reads a 0-byte placeholder as `Synced`.
#[tokio::test]
async fn opening_a_zero_byte_placeholder_never_flips_its_row_yet_it_badges_synced() {
    let h = harness(&[("empty.txt", 0)]).await;
    // A blob is seeded in case a fetch fires (it must not) — serving 0 bytes if it did.
    let host = h.host(HashMap::from([("empty.txt".to_string(), Vec::new())]));

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root = h.root.clone();
    let root_str = h.root_str();
    let dst = h._tmp.path().join("empty-copy.bin");
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        let empty_path = root.join("empty.txt");
        let placeholder = format!("{root_str}\\empty.txt");
        let dst2 = dst.clone();
        let rs = root_str.clone();

        // Populate, then read — both from another process (cfapi ignores the provider's own I/O).
        let (listing, bytes) = tokio::task::spawn_blocking(move || {
            let listing = dir_from_another_process(&rs);
            let bytes = read_from_another_process(&placeholder, &dst2);
            (listing, bytes)
        })
        .await
        .expect("populate+read task");

        assert!(
            listing.contains(&"empty.txt".to_string()),
            "a 0-byte entry must materialize as a placeholder another process can see, got {listing:?}"
        );
        assert!(
            bytes.is_empty(),
            "reading a 0-byte file yields no bytes, got {bytes:?}"
        );

        // The OS cleared the placeholder bits — the file is genuinely present-and-empty now...
        assert!(
            !on_disk_is_cloud_only(&empty_path),
            "opening a 0-byte placeholder must clear its OFFLINE/RECALL bits — the file is \
             present-and-empty on disk (measured 0x00000420, the hydrated signature)"
        );
        // ...yet the fetch path never told the host, so the ROW stays Placeholder — the fetch
        // path structurally cannot flip it (there is no callback to hook).
        assert_eq!(
            h.db_state("empty.txt"),
            Some(SyncState::Placeholder),
            "cfapi delivers NO FETCH_DATA for a 0-byte file, so `mark_hydrated` never runs and \
             the row cannot flip to Synced via the fetch path. It deliberately STAYS a \
             Placeholder — a Synced-while-absent row would be deleted by reconcile — so the badge \
             is made correct at the overlay instead. Do not try to fix this at the fetch layer."
        );
        // ...but the overlay reads Synced regardless, via `SyncState::effective_for_size`: a
        // 0-byte placeholder is present-and-empty. THIS is the fix, and it depends on no fetch
        // callback ever firing.
        assert_eq!(
            h.overlay_status("empty.txt", 10).await,
            FileStatus::Synced,
            "a 0-byte placeholder must badge Synced (effective_for_size), even though its row is \
             a Placeholder and no FETCH_DATA ever fired"
        );

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

/// **The fix, end-to-end against a real cfapi root: a 0-byte file badges `Synced` and lifts its
/// folder** (`file-sync.md` § On-Demand Files; the NEXT success criterion).
///
/// The rows are seeded exactly as the fold leaves them — every 0-byte file a `Placeholder`
/// (never Synced-while-absent). This asserts the size-aware overlay reads them right: the 0-byte
/// file materializes and badges `Synced`, a folder whose only tracked descendant is a 0-byte
/// file lifts to `Synced`, and a non-empty placeholder is untouched (`CloudOnly`).
#[tokio::test]
async fn a_zero_byte_file_badges_synced_and_lifts_its_folder() {
    // Every 0-byte file is a Placeholder row (what the fold leaves) — so is the non-empty
    // control. `empty.txt` at the root so its materialization is asserted over the top-level
    // browse; `sub/deep.txt` gives a folder whose sole descendant is a 0-byte file, for the
    // folder-lift assertion (`overlay_status` on a folder folds `descendant_states`).
    let h = harness(&[("empty.txt", 0), ("sub/deep.txt", 0), ("full.txt", 11)]).await;
    let host = h.host(HashMap::new());

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _event_rx) = broadcast::channel(64);
    let cancel = CancellationToken::new();
    let _conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx)
        .expect("register_and_connect a real cfapi sync root");

    let root_str = h.root_str();
    let loop_fut = run_hydration_loop(
        host,
        CfapiInvalidator,
        cmd_rx,
        Some(event_tx),
        h.root.clone(),
        FOLDER.to_string(),
        cancel.clone(),
        None,
        None,
    );

    let driver = async {
        // Materialize the root from another process. A 0-byte placeholder materializes like any
        // other Placeholder row (measured M1); the non-empty `full.txt` does too.
        let rs = root_str.clone();
        let listing = tokio::task::spawn_blocking(move || dir_from_another_process(&rs))
            .await
            .expect("dir task");
        assert!(
            listing.contains(&"empty.txt".to_string()),
            "a 0-byte file must materialize in the directory. Got {listing:?}"
        );
        assert!(
            listing.contains(&"full.txt".to_string()),
            "the non-empty placeholder must materialize too. Got {listing:?}"
        );

        // Part 1: the 0-byte file badges Synced (effective_for_size), not CloudOnly.
        assert_eq!(
            h.overlay_status("empty.txt", 10).await,
            FileStatus::Synced,
            "a 0-byte placeholder must query Synced — it is present-and-empty"
        );
        // Part 2: a folder whose only tracked descendant is a 0-byte file lifts to Synced
        // (the folder-badge aggregate folds the effective state).
        assert_eq!(
            h.overlay_status("sub", 11).await,
            FileStatus::Synced,
            "a folder holding only a 0-byte file lifts to Synced"
        );
        // Part 3: a non-empty placeholder is unaffected — still CloudOnly until it is opened.
        assert_eq!(
            h.overlay_status("full.txt", 12).await,
            FileStatus::CloudOnly,
            "the fix must touch only 0-byte files — a real placeholder still badges CloudOnly"
        );

        cancel.cancel();
    };

    tokio::join!(loop_fut, driver);
}

// ---------------------------------------------------------------------------
// Product shell registration + the persistent-registration lifecycle (Gap A,
// `file-sync.md` § Per-file sync-status display, answered question 1): Explorer's
// cloud verbs / status column key off the SHELL registration, which the product
// host now writes (`register_and_connect_shell`), and the registration belongs
// to the BINDING, not the serve-session — a graceful unregister was measured to
// REMOVE un-hydrated cloud-only placeholders from disk, so the old
// per-serve-session register/unregister emptied every on-demand folder in
// Explorer at each service restart.
// ---------------------------------------------------------------------------

/// **A service stop no longer empties the folder.** The product registration
/// survives a graceful engine stop (drop with no ended-binding mark): the
/// shell entry stays, the placeholder file stays on disk, and a second "service
/// run" reconnects over the kept registration (the `CF_REGISTER_FLAG_UPDATE`
/// path) and serves hydration as before. Under the retired per-serve-session
/// lifecycle the drop unregistered, which destroyed the un-hydrated placeholder
/// — the transient "my files are gone" churn this lifecycle exists to end.
#[tokio::test]
async fn a_shell_registered_root_keeps_placeholders_across_a_service_stop() {
    let _serial = shell_sync_root_test_guard();
    const BYTES: &[u8] = b"still here after the restart";

    let h = harness(&[("keep.txt", BYTES.len() as i64)]).await;
    let placeholder = format!("{}\\keep.txt", h.root_str());

    // --- first "service run": product registration, root populated ---
    {
        let host = h.host(HashMap::from([("keep.txt".to_string(), BYTES.to_vec())]));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let _conn = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
            .expect("product shell registration + connect");
        assert!(
            fauna_cfapi::shell_registration_for(&h.root_str())
                .expect("shell query")
                .is_some(),
            "the product path must write the SyncRootManager entry Explorer's verbs key off"
        );

        let loop_fut = run_hydration_loop(
            host,
            CfapiInvalidator,
            cmd_rx,
            None,
            h.root.clone(),
            FOLDER.to_string(),
            cancel.clone(),
            None,
            None,
        );
        let root_str = h.root_str();
        let driver = async {
            let rs = root_str.clone();
            let listing = tokio::task::spawn_blocking(move || dir_from_another_process(&rs))
                .await
                .expect("browse");
            assert!(
                listing.contains(&"keep.txt".to_string()),
                "the placeholder must materialize on browse. Got {listing:?}"
            );
            cancel.cancel();
        };
        drive_until_asserted(loop_fut, driver).await;
        // `_conn` drops here: a GRACEFUL STOP — no ended-binding mark.
    }

    // --- the registration and the placeholder both survive the stop ---
    assert!(
        std::path::Path::new(&placeholder).exists(),
        "the un-hydrated placeholder must SURVIVE a graceful service stop — it vanishing \
         is the measured unregister-on-every-stop churn this lifecycle retired"
    );
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_some(),
        "the shell registration must survive a graceful service stop"
    );

    // --- second "service run": reconnect over the KEPT registration, serve as before ---
    {
        let host = h.host(HashMap::from([("keep.txt".to_string(), BYTES.to_vec())]));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let conn = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
            .expect("re-connect over a kept registration (the UPDATE path)");

        let loop_fut = run_hydration_loop(
            host,
            CfapiInvalidator,
            cmd_rx,
            None,
            h.root.clone(),
            FOLDER.to_string(),
            cancel.clone(),
            None,
            None,
        );
        let dst = h._tmp.path().join("hydrated-after-restart.bin");
        let ph = placeholder.clone();
        let driver = async {
            let got = tokio::task::spawn_blocking(move || read_from_another_process(&ph, &dst))
                .await
                .expect("hydrate after restart");
            assert_eq!(
                got, BYTES,
                "hydration must serve the same bytes through the re-connected root"
            );
            cancel.cancel();
        };
        drive_until_asserted(loop_fut, driver).await;

        // Cleanup: the binding ends here — full teardown via the marked drop.
        crate::cfapi_host::mark_root_for_full_teardown(&h.root);
        drop(conn);
    }
    assert_eq!(
        fauna_cfapi::shell_registration_for(&h.root_str()).expect("shell query"),
        None,
        "an ended binding must remove the shell entry (no ghost in Explorer's nav pane)"
    );
}

/// **A filter-registered (non-shell) root is shell-registered in place — never
/// unregistered.** Staged with a bare `CfRegisterSyncRoot` filter registration
/// holding a hydrated file, `register_and_connect_shell` takes the fresh path:
/// the WinRT `Register` succeeds over the filter registration (measured
/// 2026-09-25; the pre-sweep migrate step that filter-unregistered first, on a
/// `0x8007018B` refusal that no longer reproduces, was removed with program 4's
/// at-rest remnants), the root comes up shell-registered, and the hydrated
/// bytes are untouched.
#[tokio::test]
async fn a_filter_registered_root_shell_registers_in_place() {
    let _serial = shell_sync_root_test_guard();
    const HYD: &[u8] = b"hydrated under the bare filter registration";
    let h = harness(&[("hyd.txt", HYD.len() as i64)]).await;
    let hyd_path = format!("{}\\hyd.txt", h.root_str());

    // Stage a populated filter-only root: bare registration, browsed and one
    // file hydrated, then a crash-style disconnect that keeps the registration.
    {
        let host = h.host(HashMap::from([("hyd.txt".to_string(), HYD.to_vec())]));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let conn = crate::cfapi_host::register_and_connect(&h.root, cmd_tx).expect("bare connect");
        let loop_fut = run_hydration_loop(
            host,
            CfapiInvalidator,
            cmd_rx,
            None,
            h.root.clone(),
            FOLDER.to_string(),
            cancel.clone(),
            None,
            None,
        );
        let dst = h._tmp.path().join("hydrated-bare.bin");
        let (rs, ph) = (h.root_str(), hyd_path.clone());
        let driver = async {
            tokio::task::spawn_blocking(move || {
                dir_from_another_process(&rs);
                read_from_another_process(&ph, &dst)
            })
            .await
            .expect("hydrate hyd.txt under the bare root");
            cancel.cancel();
        };
        drive_until_asserted(loop_fut, driver).await;
        conn.disconnect_keeping_registration();
    }

    assert!(
        fauna_cfapi::is_filter_registered(&h.root_str()).expect("filter query"),
        "precondition: the staged root is filter-registered"
    );

    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let conn = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
        .expect("a filter-registered root must shell-register in place");
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_some(),
        "the root must now be shell-registered"
    );
    // An ordinary in-process read is safe: the file is hydrated.
    assert_eq!(
        std::fs::read(&hyd_path).expect("read the hydrated file"),
        HYD,
        "shell registration over a filter registration must not touch hydrated bytes"
    );

    crate::cfapi_host::mark_root_for_full_teardown(&h.root);
    drop(conn);
    assert_eq!(
        fauna_cfapi::shell_registration_for(&h.root_str()).expect("shell query"),
        None,
        "cleanup: the ended binding must fully unregister"
    );
}

/// **An ended binding tears the whole registration down — filter and shell.**
/// The unbind / folder-removal / re-bind case: `reconcile_engines` marks the
/// root, the engine's drop guard runs the measured-order teardown, and nothing
/// ghosts in Explorer's nav pane. Filter-side removal is proven by
/// [`fauna_cfapi::is_filter_registered`] — never by a fresh shell registration
/// succeeding, which it does over a live filter registration too (measured
/// 2026-09-25).
#[tokio::test]
async fn an_ended_binding_fully_unregisters_filter_and_shell() {
    let _serial = shell_sync_root_test_guard();
    let h = harness(&[("f.txt", 4)]).await;
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();

    let conn = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
        .expect("product shell registration + connect");
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_some()
    );

    crate::cfapi_host::mark_root_for_full_teardown(&h.root);
    drop(conn);

    assert_eq!(
        fauna_cfapi::shell_registration_for(&h.root_str()).expect("shell query"),
        None,
        "the shell entry must be gone after an ended binding"
    );
    assert!(
        !fauna_cfapi::is_filter_registered(&h.root_str()).expect("filter query"),
        "the filter registration must be gone after an ended binding"
    );
}

/// **The startup janitor sweeps ghosts and spares the living.** Two shell
/// registrations: one whose folder is deleted out from under it (the
/// crashed-test / removed-while-down leftover Explorer would keep rendering
/// forever) and one whose folder exists. The sweep removes exactly the ghost —
/// folder existence is what makes it safe to run on a shared dev box where
/// parallel sessions hold live roots.
#[tokio::test]
async fn the_startup_sweep_removes_ghosts_and_spares_live_roots() {
    let _serial = shell_sync_root_test_guard();
    let tmp = tempfile::tempdir().expect("tempdir");
    let ghost_dir = tmp.path().join("ghost-root");
    let live_dir = tmp.path().join("live-root");
    std::fs::create_dir_all(&ghost_dir).expect("mk ghost");
    std::fs::create_dir_all(&live_dir).expect("mk live");

    let ghost_id = fauna_cfapi::register_sync_root_with_shell(
        &ghost_dir.to_string_lossy(),
        "Fauna sweep-ghost",
        "sweep-ghost",
    )
    .expect("register ghost root");
    let live_id = fauna_cfapi::register_sync_root_with_shell(
        &live_dir.to_string_lossy(),
        "Fauna sweep-live",
        "sweep-live",
    )
    .expect("register live root");

    // Unregister the ghost's FILTER side first so the folder can be removed, then
    // delete the folder — leaving exactly the orphaned SHELL entry the janitor
    // exists for (a crashed run's leftover after its temp dir vanished).
    fauna_cfapi::unregister_sync_root(&ghost_dir.to_string_lossy()).expect("ghost filter down");
    std::fs::remove_dir_all(&ghost_dir).expect("delete the ghost folder");

    crate::cfapi_host::sweep_ghost_shell_registrations();

    let ids: Vec<String> = fauna_cfapi::list_shell_sync_roots()
        .expect("list")
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        !ids.contains(&ghost_id),
        "the ghost (deleted folder) must be swept. Present: {ids:?}"
    );
    assert!(
        ids.contains(&live_id),
        "a registration whose folder exists must be SPARED. Present: {ids:?}"
    );

    // Cleanup the live probe (filter first, then shell — measured order).
    fauna_cfapi::unregister_sync_root(&live_dir.to_string_lossy()).expect("live filter down");
    fauna_cfapi::unregister_sync_root_with_shell(&live_id).expect("live shell down");
}

/// **The uninstall wipe is SELF-HEALED by the next product registration** —
/// the coexistence hazard `installers/windows.md` § Data continuity names, made
/// a mechanism instead of a manual pass.
///
/// On a both-channels box (Store package + direct-download MSI), uninstalling
/// the MSI runs its `CleanSyncRoots` custom action, and that action is
/// unconditional: it takes down **every** `Fauna!*` binding on the machine,
/// including the ones the *surviving* Store agent owns (pinned by
/// `uninstall_cleanup_removes_ghost_and_live_roots_alike`, directly below).
/// Without a heal, that user's Explorer silently loses its Fauna locations
/// until someone reinstalls — a box a client cannot talk itself out of.
///
/// The heal is a property of the product registration path rather than any
/// recovery code: `register_and_connect_shell` asks
/// `shell_registration_for` first and registers when the answer is `None`, so
/// the next reconcile after a wipe re-registers by construction. This test
/// pins that end to end — register the product way, run the CA's exact
/// entry point, prove the binding is gone, reconcile, prove it is back.
///
/// Scope, deliberately: this is the *same-identity* half, which is the half
/// that decides whether the heal exists at all. Whether an UNPACKAGED MSI
/// uninstall can even reach a root registered by a PACKAGED Store agent is a
/// separate question (if it cannot, the hazard is narrower than the goal doc
/// assumes) and needs a provisioned packaged agent to answer.
#[tokio::test]
async fn uninstall_cleanup_is_self_healed_by_the_next_product_registration() {
    let _serial = shell_sync_root_test_guard();
    let h = harness(&[]).await;

    // --- the surviving agent's root, registered the way the product does it ---
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let conn = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx)
        .expect("product shell registration + connect");
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_some(),
        "the product path must write the SyncRootManager entry before we wipe it"
    );
    // The MSI's `KillFaunaSync` stops the agent before `CleanSyncRoots` runs, so
    // the connection is down by the time the wipe happens. Dropping it must NOT
    // itself remove the binding (`TeardownMode::KeepUnlessMarked`) — otherwise
    // the next assertion would pass for the wrong reason.
    drop(conn);
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_some(),
        "dropping the connection must LEAVE the registration — a teardown that \
         unregistered here would make this test blind to what the CA does"
    );

    // --- the MSI uninstall's CleanSyncRoots CA, at its exact entry point ---
    // `Package.wxs` runs `fauna-sync-agent.exe --cleanup-roots`, whose whole body
    // is this call (`lib.rs` → `unregister_all_shell_sync_roots`).
    let removed = crate::cfapi_host::unregister_all_shell_sync_roots();
    assert!(
        removed >= 1,
        "the uninstall CA must have taken at least this root down, got {removed}"
    );
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_none(),
        "the CA left the binding standing — then there is no hazard to heal and \
         this test is asserting nothing"
    );

    // --- the surviving agent's next reconcile ---
    let (cmd_tx2, _cmd_rx2) = mpsc::unbounded_channel();
    let conn2 = crate::cfapi_host::register_and_connect_shell(&h.root, FOLDER, cmd_tx2)
        .expect("reconcile must re-register a root the uninstall CA wiped");
    assert!(
        fauna_cfapi::shell_registration_for(&h.root_str())
            .expect("shell query")
            .is_some(),
        "the root did NOT come back after a reconcile — a both-channels user \
         would lose their Explorer locations on every MSI uninstall"
    );

    // Give the machine-wide registry back (filter first, then shell — measured order).
    drop(conn2);
    let root = h.root_str();
    let _ = fauna_cfapi::unregister_sync_root(&root);
    if let Ok(Some(id)) = fauna_cfapi::shell_registration_for(&root) {
        let _ = fauna_cfapi::unregister_sync_root_with_shell(&id);
    }
}

/// **Uninstall cleanup removes EVERY registration, ghost or live** — the
/// opposite selectivity from the startup sweep above. The whole product is
/// going away, so a folder that still exists (the user's real, un-uninstalled
/// files) must lose its sync-root binding too; only the binding goes, never
/// the folder or its files.
#[tokio::test]
async fn uninstall_cleanup_removes_ghost_and_live_roots_alike() {
    let _serial = shell_sync_root_test_guard();
    let tmp = tempfile::tempdir().expect("tempdir");
    let ghost_dir = tmp.path().join("uninstall-ghost-root");
    let live_dir = tmp.path().join("uninstall-live-root");
    std::fs::create_dir_all(&ghost_dir).expect("mk ghost");
    std::fs::create_dir_all(&live_dir).expect("mk live");

    let ghost_id = fauna_cfapi::register_sync_root_with_shell(
        &ghost_dir.to_string_lossy(),
        "Fauna uninstall-ghost",
        "uninstall-ghost",
    )
    .expect("register ghost root");
    let live_id = fauna_cfapi::register_sync_root_with_shell(
        &live_dir.to_string_lossy(),
        "Fauna uninstall-live",
        "uninstall-live",
    )
    .expect("register live root");

    // Same ghost shape as the sweep test (folder deleted after filter teardown) —
    // exercises both branches through the shared per-entry unregister path.
    fauna_cfapi::unregister_sync_root(&ghost_dir.to_string_lossy()).expect("ghost filter down");
    std::fs::remove_dir_all(&ghost_dir).expect("delete the ghost folder");

    let removed = crate::cfapi_host::unregister_all_shell_sync_roots();
    assert!(
        removed >= 2,
        "must remove both the ghost and the live entry, got {removed}"
    );

    let ids: Vec<String> = fauna_cfapi::list_shell_sync_roots()
        .expect("list")
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        !ids.contains(&ghost_id),
        "ghost must be gone. Present: {ids:?}"
    );
    assert!(
        !ids.contains(&live_id),
        "the LIVE entry must also be gone — uninstall removes every binding \
         regardless of folder existence. Present: {ids:?}"
    );
    assert!(
        live_dir.exists(),
        "the live folder itself (and any user files in it) must survive — only \
         the sync-root BINDING is removed at uninstall, never the folder"
    );
}

/// Stage the root an engine RESTART leaves behind: registered, connected once for
/// so short a time that nothing browsed it (its directory still asks the provider
/// for its children), then disconnected with the registration kept — the old
/// engine's connection dropping before the new engine's build. `before` runs on
/// the plain directory first.
fn root_disconnected_before_any_population(
    tmp: &tempfile::TempDir,
    before: impl FnOnce(&std::path::Path),
) -> PathBuf {
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("create sync root");
    before(&root);
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    let conn = crate::cfapi_host::register_and_connect(&root, cmd_tx).expect("connect");
    conn.disconnect_keeping_registration();
    root
}

/// Reconnect and drop gracefully, so the unregister lets the temp dir go.
fn release_root(root: &std::path::Path) {
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
    drop(crate::cfapi_host::register_and_connect(root, cmd_tx).expect("reconnect to release"));
}

/// The engine build after a restart must not die on the root the restart left
/// unconnected. Its `.faunaignore` read is a name lookup in a directory that still
/// asks the provider for its children, and with no provider connected the cloud
/// filter answers `ERROR_FLT_INVALID_NAME_REQUEST` (0x801F0005, raw os error
/// -2145452027) — outside the `ERROR_CLOUD_FILE_*` range the load reads as "no
/// ignore file". Every rebuild was refused (`retry_in=60s`, the same answer each
/// time), the folder stayed inert, and every write into it failed: the windows
/// native seat pair's first write, 2026-10-08.
#[test]
fn the_ignore_file_loads_from_a_root_whose_provider_dropped_before_population() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = root_disconnected_before_any_population(&tmp, |_| {});

    let loaded = fauna_sync_engine::ignore::IgnoreMatcher::load(&root);
    release_root(&root);

    let matcher = loaded.expect("a root with no provider connected has no .faunaignore to read");
    assert!(
        matcher.is_ignored("Thumbs.db"),
        "the built-in defaults still apply"
    );
    assert!(!matcher.is_ignored("notes.txt"));
}

/// The premise that makes "the filter could not answer" mean "no ignore file":
/// a `.faunaignore` that EXISTS is an ordinary local file (dotfiles never enter a
/// folder, so it is never a placeholder), and the same disconnected root still
/// opens it without a provider. Were this to fail, reading the filter's answer as
/// absence would silently drop the user's ignore list and upload what it names.
#[test]
fn a_present_ignore_file_still_loads_from_a_root_whose_provider_dropped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = root_disconnected_before_any_population(&tmp, |dir| {
        std::fs::write(dir.join(".faunaignore"), "secret-*.txt\n").expect("write ignore file");
    });

    let loaded = fauna_sync_engine::ignore::IgnoreMatcher::load(&root);
    release_root(&root);

    let matcher = loaded.expect("a local .faunaignore opens without a provider");
    assert!(
        matcher.is_ignored("secret-plans.txt"),
        "the user's own pattern must be read, never defaulted away"
    );
}
