//! A LIVE FUSE integration test over the **agent's own** on-demand host — the linux
//! twin of `cfapi_live_integration.rs` (`docs/goal/behavior/on-demand-files.md`
//! § Linux FUSE binding → *Headless proof*).
//!
//! Real: the mount (`/dev/fuse` + the distro's `fusermount3`), the kernel's
//! requests, [`crate::fuse_host`], the command channel, the product's boot order
//! ([`serve_hydration_root`] — fold, mark hygiene, boot sweep, THEN the mount), a
//! real on-disk `SyncDb`, and the real `SyncEngine` built over the descriptor
//! reach. Faked: only the nest-facing calls (`prepare`/`repull`, the byte fetch),
//! exactly as the cfapi harness fakes them — its rows are seeded instead.
//!
//! ## Every look at the mount is from another process, off this thread
//!
//! The driving loop runs on the test's own thread. A read of the mount from that
//! thread would wait on a request only that thread can serve, so each assertion
//! about the view runs a child process (`ls`, `stat`) under `spawn_blocking`. The
//! directory UNDER the mount is read in-process, through the reach — which is the
//! binding's own rule, and what [`the_descriptor_reach_serves_engine_reads_under_the_mount`]
//! proves works.
//!
//! Opt-in (`--features fuse-live`): it needs an openable `/dev/fuse` and
//! `fusermount3`, which a sandboxed build box may not offer.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::{broadcast, mpsc, watch};

use fauna_core::data::ContentHash;
use fauna_ipc::sync::{BearerToken, Event, EventKind, FileStatus, SyncCapability};
use fauna_sync_engine::FileHydrator;
use fauna_sync_engine::always_resident::{EngineCommand, LocalWriteHost};
use fauna_sync_engine::db::{SyncDb, SyncState};
use fauna_sync_engine::engine::{PlaceholderFold, StaleHydratedRow};
use fauna_sync_engine::engine_host::CancellationToken;
use fauna_sync_engine::enumerate::{PlaceholderLister, PlaceholderRow};

use crate::bridge::{
    HydrationCommand, HydrationHost, RootBoot, RowPin, agent_control_plane, agent_engine_params,
    serve_hydration_root,
};
use crate::config::SyncPaths;
use crate::fuse_host::{
    FuseInvalidator, Reach, ghost_mount_points, mount_over, sweep_ghost_mounts,
};

const FOLDER: &str = "docs";
const FOLDER_REF: fauna_core::folder_keys::FolderRef = fauna_core::folder_keys::FolderRef::Local(1);

/// The one file whose bytes are on the disk when a test starts.
const REAL: &str = "real.txt";
const REAL_BYTES: &[u8] = b"these bytes are on this disk";
/// The placeholder rows a nest fold would have left: `(rel, size)`.
const CLOUD: &[(&str, i64)] = &[("cloud.txt", 1234), ("sub/deep.bin", 99)];
/// Unix seconds — what every seeded placeholder reports as its mtime.
const CLOUD_MTIME: i64 = 1_700_000_000;

/// The plaintext the (faked) nest serves for a placeholder of `size` bytes:
/// distinct per file, and exactly the row's size, as a real fold records it.
fn cloud_bytes(rel: &str, size: i64) -> Vec<u8> {
    rel.bytes().cycle().take(size as usize).collect()
}

/// A nest address nothing listens on — for the tests whose every claim is about
/// the OS boundary.
const NO_NEST: &str = "http://127.0.0.1:1";

/// A [`HydrationHost`] that is a decorator over a real production `SyncEngine`,
/// faking only what would need a live nest — the cfapi harness's `DbBackedHost`,
/// for the same reason: the write half and the placeholder listing are the
/// shipped ones.
struct DbBackedHost {
    engine: fauna_sync_engine::engine::SyncEngine,
    reconnect: (watch::Sender<u64>, watch::Receiver<u64>),
    rescan: Duration,
    /// Rescan ticks served (each one re-pulls) — how a test knows a full
    /// `converge` pass has run since it acted.
    ticks: Arc<AtomicUsize>,
    /// What the next re-pull's fold reports as stale hydrated rows — how a test
    /// moves a nest head under a hydrated file.
    stale: Arc<std::sync::Mutex<Vec<StaleHydratedRow>>>,
}

#[async_trait::async_trait(?Send)]
impl FileHydrator for DbBackedHost {
    /// The faked byte plane: a tracked row's [`cloud_bytes`], at the row's size.
    async fn download_file_bytes(&self, relative_path: &str) -> Result<Vec<u8>> {
        let entry = self
            .engine
            .db()
            .get_entry(relative_path)?
            .ok_or_else(|| anyhow!("the faked nest serves no {relative_path}"))?;
        Ok(cloud_bytes(relative_path, entry.size_bytes))
    }
}

#[async_trait::async_trait(?Send)]
impl PlaceholderLister for DbBackedHost {
    async fn list_placeholder_rows(&self) -> Result<Vec<PlaceholderRow>> {
        self.engine.list_placeholder_rows().await
    }
}

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
    /// The rows are seeded, as a real fold would have left them.
    async fn prepare(&self) -> Result<PlaceholderFold> {
        Ok(PlaceholderFold::default())
    }

    async fn mark_hydrated(&self, rel: &str, content_hash: ContentHash) -> Result<()> {
        HydrationHost::mark_hydrated(&self.engine, rel, content_hash).await
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
        self.ticks.fetch_add(1, Ordering::SeqCst);
        Ok(PlaceholderFold {
            stale_hydrated: std::mem::take(&mut *self.stale.lock().unwrap()),
            ..PlaceholderFold::default()
        })
    }

    async fn set_placeholders_off_disk(&self) -> Result<()> {
        HydrationHost::set_placeholders_off_disk(&self.engine).await
    }

    async fn dehydrate_off_disk(&self, rel: &str) -> Result<bool> {
        HydrationHost::dehydrate_off_disk(&self.engine, rel).await
    }

    async fn replace_superseded_own_record(&self, row: &StaleHydratedRow) -> Result<bool> {
        HydrationHost::replace_superseded_own_record(&self.engine, row).await
    }

    async fn delete_placeholder(&self, rel: &str) -> Result<()> {
        HydrationHost::delete_placeholder(&self.engine, rel).await
    }

    async fn row_pin(&self, rel: &str) -> Result<Option<RowPin>> {
        HydrationHost::row_pin(&self.engine, rel).await
    }

    async fn set_pinned(&self, rel: &str, pinned: bool) -> Result<bool> {
        HydrationHost::set_pinned(&self.engine, rel, pinned).await
    }

    async fn pinned_placeholders(&self) -> Result<Vec<String>> {
        HydrationHost::pinned_placeholders(&self.engine).await
    }

    // No nest here, so nothing for the corpus passes to converge — a no-op this
    // harness owns, never a trait default.
    async fn converge_corpus_at_start(&self, _folder: &str) {}

    async fn refresh_and_converge_corpus(&self, _folder: &str) {}

    async fn answer_engine_command(&self, cmd: EngineCommand) {
        HydrationHost::answer_engine_command(&self.engine, cmd).await
    }

    /// Long enough that the rescan tick never fires inside a test (real clock:
    /// child processes drive these tests) — unless the test asks for ticks.
    async fn rescan_interval(&self) -> Duration {
        self.rescan
    }

    fn reconnects(&self) -> watch::Receiver<u64> {
        self.reconnect.1.clone()
    }

    async fn subtree_fully_synced(&self, dir_rel: &str) -> bool {
        self.engine.subtree_fully_synced(dir_rel).unwrap_or(false)
    }

    async fn is_dehydration_safe(&self, rel: &str) -> bool {
        self.engine.is_dehydration_safe(rel)
    }
}

/// A bound directory holding one hydrated file, and a state DB seeded with that
/// file's `Synced` row plus the [`CLOUD`] placeholder rows.
struct Harness {
    _tmp: tempfile::TempDir,
    /// The bound directory — the mount point. Canonical, so it compares equal to
    /// what `/proc/self/mountinfo` reports.
    root: PathBuf,
    db_path: PathBuf,
    /// The engine's nest: [`NO_NEST`], or a mock byte plane's address.
    nest: String,
}

/// How [`Harness::serve_with`] runs the root.
struct Serve {
    events: Option<broadcast::Sender<Event>>,
    rescan: Duration,
    ticks: Arc<AtomicUsize>,
    /// The pipe server's side of the engine's command channel — *Free up space*
    /// and the pins arrive here in the product.
    commands: Option<mpsc::Receiver<EngineCommand>>,
    /// Stale hydrated rows the next re-pull reports ([`DbBackedHost::stale`]).
    stale: Arc<std::sync::Mutex<Vec<StaleHydratedRow>>>,
}

impl Default for Serve {
    fn default() -> Self {
        Self {
            events: None,
            rescan: Duration::from_secs(3600),
            ticks: Arc::default(),
            commands: None,
            stale: Arc::default(),
        }
    }
}

/// The nest head manifest a fold would have recorded for `rel`. Never `None`: the
/// fold writes one on every row, and a `None` here is a row shape production
/// cannot produce (the cfapi harness's 2026-08-04 lesson).
fn seeded_manifest_hash(rel: &str) -> ContentHash {
    ContentHash::of_raw(format!("seeded-nest-head-manifest:{rel}").as_bytes())
}

/// A temp dir the distro lets a FUSE mount land under.
///
/// Not `tempfile::tempdir()`: that follows `TMPDIR`, and Ubuntu confines
/// `fusermount3` with an AppArmor profile that admits a mount point only under
/// the user's home, `/mnt`, `/media`, `/run/user/<uid>` or `/tmp` — anywhere else
/// the helper answers *mount failed: Permission denied* (measured 2026-10-01 with
/// `TMPDIR` on another volume). `/tmp` is in every such list.
fn mountable_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("fauna-fuse-live-")
        .tempdir_in("/tmp")
        .expect("a temp dir under /tmp")
}

fn harness() -> Harness {
    harness_against(NO_NEST)
}

/// [`harness`], with the engine's byte plane at `nest`.
fn harness_against(nest: &str) -> Harness {
    let tmp = mountable_tempdir();
    let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).expect("create the bound directory");
    let root = root.canonicalize().expect("canonical bound directory");
    std::fs::write(root.join(REAL), REAL_BYTES).expect("write the hydrated file");

    let db_path = paths.sync_db_path_for_ref(FOLDER_REF);
    std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
    {
        let db = SyncDb::open(&db_path).expect("open per-folder db");
        // The hydrated file, exactly as a completed sync leaves it: local identity,
        // size and mtime matching the disk, so the boot sweep reads it unchanged
        // and reaches for no nest.
        let meta = std::fs::metadata(root.join(REAL)).expect("stat the hydrated file");
        let hash = fauna_core::chunker_stream::content_hash_streaming(&root.join(REAL))
            .expect("hash the hydrated file");
        let mtime = fauna_core::data::Timestamp::secs_or_zero(meta.modified().unwrap());
        db.upsert_entry(
            REAL,
            Some(hash),
            None,
            Some(seeded_manifest_hash(REAL)),
            SyncState::Synced,
            mtime,
            mtime,
            meta.len() as i64,
            1,
            None,
        )
        .expect("seed the hydrated row");
        for (rel, size) in CLOUD {
            db.upsert_entry(
                rel,
                None,
                None,
                Some(seeded_manifest_hash(rel)),
                SyncState::Placeholder,
                0,
                CLOUD_MTIME,
                *size,
                1,
                None,
            )
            .expect("seed a placeholder row");
        }
    }
    Harness {
        _tmp: tmp,
        root,
        db_path,
        nest: nest.to_string(),
    }
}

impl Harness {
    /// The host over a real production engine whose `watch_dir` is `dir` — the
    /// reach, as the product builds it. Its nest is [`Harness::nest`]; the byte
    /// fetch a hydration makes is faked from [`cloud_bytes`].
    fn host(&self, dir: &Path, serve: &Serve) -> DbBackedHost {
        let nest = self.nest.clone();
        let capability = crate::bearer::CapabilitySlot::new(Some(SyncCapability::new(
            vec![9u8; 32],
            vec![7u8; 32],
            nest.clone(),
            "fuse-live-test".into(),
            BearerToken::new("test.bearer".into(), 4_000_000_000),
        )));
        let engine = fauna_sync_engine::engine_lifecycle::assemble_engine(
            agent_engine_params(
                agent_control_plane(capability, nest, [9u8; 32]),
                self.db_path
                    .parent()
                    .expect("the state DB sits under the data-root")
                    .to_path_buf(),
                dir.to_path_buf(),
                FOLDER_REF,
                [7u8; 32],
                [3u8; 32],
                &crate::bridge::AgentPredecessors::default(),
                None,
                fauna_sync_engine::access_gate::AccessGate::new(),
                None,
                std::sync::Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
            ),
            fauna_sync_engine::engine_lifecycle::ResolvedBinding {
                folder: FOLDER.to_string(),
                // A same-nest seat's residency, read off its row: full. The
                // default is *unknown* — a cross-nest record no home nest has
                // stamped — under which the holder-keeps gate frees nothing.
                metadata_only_residency: Some(false),
                ..Default::default()
            },
        )
        .expect("build the production SyncEngine")
        .engine;
        DbBackedHost {
            engine,
            reconnect: watch::channel(0),
            rescan: serve.rescan,
            ticks: Arc::clone(&serve.ticks),
            stale: Arc::clone(&serve.stale),
        }
    }

    /// Stamp `rel`'s recorded head from its local identity — the state a recorded
    /// upload or a hydration leaves, and the only one a dehydrate lets go.
    fn stamp_recorded(&self, rel: &str) {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .stamp_recorded_content_from_local(rel, fauna_sync_engine::db::ProofOrigin::OwnRecord)
            .expect("stamp the recorded head");
    }

    /// Pin `rel` in its row before the root starts.
    fn pin(&self, rel: &str) {
        assert!(
            SyncDb::open(&self.db_path)
                .expect("reopen db")
                .set_pinned(rel, true)
                .expect("pin"),
            "{rel} has a row"
        );
    }

    /// Seed one more placeholder row, as a fold would have left it.
    fn seed_placeholder(&self, rel: &str, size: i64) {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .upsert_entry(
                rel,
                None,
                None,
                Some(seeded_manifest_hash(rel)),
                SyncState::Placeholder,
                0,
                CLOUD_MTIME,
                size,
                1,
                None,
            )
            .expect("seed a placeholder row");
    }

    fn entry(&self, rel: &str) -> Option<fauna_sync_engine::db::SyncEntry> {
        SyncDb::open(&self.db_path)
            .expect("reopen db")
            .get_entry(rel)
            .expect("get entry")
    }

    /// Is a mount of this binding on the bound directory right now?
    fn mounted(&self) -> bool {
        is_mounted(&self.root)
    }

    /// The product's on-demand start over this root: the boot sweep, then the
    /// mount as the root's connect step.
    fn serve<'a>(
        &'a self,
        reach: &'a Reach,
        cancel: CancellationToken,
    ) -> impl Future<Output = ()> + 'a {
        self.serve_with(reach, cancel, Serve::default())
    }

    /// [`Self::serve`], with events, a rescan cadence and a tick counter.
    fn serve_with<'a>(
        &'a self,
        reach: &'a Reach,
        cancel: CancellationToken,
        mut serve: Serve,
    ) -> impl Future<Output = ()> + 'a {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HydrationCommand>();
        let connect = {
            let cmd_tx = cmd_tx.clone();
            move || mount_over(reach, FOLDER, cmd_tx).expect("mount over the bound directory")
        };
        // The product's invalidator for this binding (`engine_driver::run_on_demand_root`).
        let invalidator = FuseInvalidator::new(reach.path(), cmd_tx.clone());
        let commands = serve.commands.take();
        async move {
            // Held for the loop's lifetime, as the product holds it.
            let _cmd_tx = cmd_tx;
            serve_hydration_root(
                self.host(reach.path(), &serve),
                invalidator,
                RootBoot {
                    fresh_registration: true,
                    connect,
                },
                cmd_rx,
                serve.events.clone(),
                reach.path().to_path_buf(),
                FOLDER.to_string(),
                cancel,
                None,
                commands,
                None,
            )
            .await
        }
    }
}

fn is_mounted(root: &Path) -> bool {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").expect("read mountinfo");
    !ghost_mount_points(&mountinfo, &[root.to_path_buf()]).is_empty()
}

/// The names an in-process `read_dir` of `dir` finds, sorted, minus the engine's
/// own dot-directories (its causal store and merge bases live beside the files).
/// For a directory with no mount on it, or the reach — never the mounted view.
fn visible_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Run both; finish with the driver and drop the loop (which unmounts).
async fn drive_until_asserted(
    loop_fut: impl Future<Output = ()>,
    driver: impl Future<Output = ()>,
) {
    tokio::select! {
        _ = loop_fut => panic!("the hydration loop returned before the test finished asserting"),
        _ = driver => {}
    }
}

/// Poll `pred` on the real clock until it holds, or panic with `what`. Awaiting
/// (never blocking) keeps the hydration loop polled on this thread meanwhile.
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Run `program args…` in **another process**, off this thread, and return its
/// stdout lines. The only way this module looks at the mounted view.
async fn from_another_process(program: &'static str, args: Vec<String>) -> Vec<String> {
    tokio::task::spawn_blocking(move || {
        let out = std::process::Command::new(program)
            .args(&args)
            .output()
            .expect("spawn the child process");
        assert!(
            out.status.success(),
            "{program} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    })
    .await
    .expect("child process task")
}

async fn ls(dir: &Path) -> Vec<String> {
    from_another_process("ls", vec!["-1".into(), dir.to_string_lossy().into_owned()]).await
}

/// `size mtime-seconds file-type` of `path`, as another process's `stat` sees it.
async fn stat(path: &Path) -> String {
    from_another_process(
        "stat",
        vec![
            "-c".into(),
            "%s %Y %F".into(),
            path.to_string_lossy().into_owned(),
        ],
    )
    .await
    .join("\n")
}

/// Populate: `readdir` from another process lists each placeholder row's name with
/// the row's size and mtime, beside the hydrated file — and listing marks nothing
/// seen, so a browsed placeholder can never read as a delete (the binding's first
/// obligation under the dehydrate rule).
#[tokio::test]
async fn the_agents_own_fuse_host_lists_placeholders_beside_hydrated_files() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let cancel = CancellationToken::new();
    let loop_fut = h.serve(&reach, cancel);

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        // The fstype line the ghost sweep keys on, as this kernel really reports it.
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
        let line = mountinfo
            .lines()
            .find(|l| l.contains(&*h.root.to_string_lossy()))
            .expect("the mount's own mountinfo line");
        eprintln!("[fuse-live] mountinfo: {line}");
        assert!(
            line.contains(" - fuse.fauna fauna-sync-agent "),
            "the subtype lands in the fstype field and fsname in the source field: {line}"
        );
        assert!(
            !line.contains("allow_other"),
            "the mount must never be allow_other: {line}"
        );

        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
        assert_eq!(ls(&h.root.join("sub")).await, vec!["deep.bin"]);
        assert_eq!(
            stat(&h.root.join("cloud.txt")).await,
            format!("1234 {CLOUD_MTIME} regular file"),
            "a placeholder reports its row's size and mtime"
        );
        assert_eq!(
            stat(&h.root.join("sub/deep.bin")).await,
            format!("99 {CLOUD_MTIME} regular file")
        );
        assert!(
            stat(&h.root.join("sub")).await.ends_with("directory"),
            "a directory that exists only as rows is listed as one"
        );
        assert_eq!(
            stat(&h.root.join(REAL)).await.split(' ').next(),
            Some(REAL_BYTES.len().to_string().as_str()),
            "the hydrated file reports its own size"
        );

        // Listing put nothing on the disk, so nothing is marked seen — and the rows
        // are still placeholders.
        for (rel, _) in CLOUD {
            let entry = h.entry(rel).expect("the placeholder row");
            assert_eq!(entry.state, SyncState::Placeholder, "{rel}");
            assert!(
                !entry.seen_on_disk,
                "{rel}: a FUSE listing must never mark a placeholder seen"
            );
        }
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// The mounted session is a drop guard: ending the root's loop unmounts, and the
/// directory is left an ordinary directory holding exactly its hydrated files.
#[tokio::test]
async fn dropping_the_session_unmounts() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let cancel = CancellationToken::new();
    let loop_fut = h.serve(&reach, cancel.clone());

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
        cancel.cancel();
    };
    tokio::join!(loop_fut, driver);

    assert!(!h.mounted(), "the loop's end unmounts the root");
    // No mount: an in-process read is the directory itself.
    assert_eq!(
        visible_names(&h.root),
        vec![REAL],
        "only the hydrated file is on the disk"
    );
    assert_eq!(std::fs::read(h.root.join(REAL)).unwrap(), REAL_BYTES);
}

/// The body of the **child process** the crash tests kill — not a test of its own.
/// It mounts over the directory the parent names and then waits to be killed, so
/// the parent can observe what a dead agent leaves behind. Run only by
/// [`spawn_mounting_child`]; a plain `--ignored` run finds no directory named and
/// returns.
#[tokio::test]
#[ignore = "child-process body for the crash tests"]
async fn child_mounts_and_waits() {
    let Some(root) = std::env::var_os(CHILD_ROOT_VAR) else {
        return;
    };
    let extra = if std::env::var_os(CHILD_AUTO_UNMOUNT_VAR).is_some() {
        vec![fuser::MountOption::CUSTOM("auto_unmount".into())]
    } else {
        Vec::new()
    };
    let reach = Reach::open(Path::new(&root)).expect("child: open the reach");
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<HydrationCommand>();
    // The product's mount — crash guard included — or the bare one the boot
    // sweep's test needs a ghost from.
    let _mounted = if std::env::var_os(CHILD_GUARDED_VAR).is_some() {
        mount_over(&reach, FOLDER, cmd_tx).expect("child: mount")
    } else {
        crate::fuse_host::mount_over_probing(&reach, FOLDER, cmd_tx, extra).expect("child: mount")
    };
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

/// Names the directory [`child_mounts_and_waits`] mounts over.
const CHILD_ROOT_VAR: &str = "FAUNA_FUSE_LIVE_CHILD_ROOT";
/// Set: the child also passes `auto_unmount` to the mount helper.
const CHILD_AUTO_UNMOUNT_VAR: &str = "FAUNA_FUSE_LIVE_CHILD_AUTO_UNMOUNT";
/// Set: the child mounts as the product does ([`mount_over`], crash guard and
/// all); unset, bare — no guard, so a kill leaves the ghost the boot sweep is for.
const CHILD_GUARDED_VAR: &str = "FAUNA_FUSE_LIVE_CHILD_GUARDED";

/// [`spawn_mounting_child`] mounting as the product does: with its crash guard.
fn spawn_guarded_mounting_child(root: &Path) -> std::process::Child {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("this test binary"));
    cmd.args([
        "fuse_live_integration::child_mounts_and_waits",
        "--exact",
        "--ignored",
        "--nocapture",
    ])
    .env(CHILD_ROOT_VAR, root)
    .env(CHILD_GUARDED_VAR, "1")
    .stdout(std::process::Stdio::null());
    cmd.spawn().expect("spawn the mounting child")
}

/// The crash guard (`on-demand-files.md` § Linux FUSE binding, the lifecycle
/// rule): an agent killed while mounted leaves no dead mount behind for long —
/// its guard, a process of its own, sees the agent go and lazily unmounts, so
/// within seconds the directory is an ordinary directory of its hydrated files
/// again, with no boot sweep and no restart. Everything observed from outside
/// the killed process.
#[tokio::test]
async fn a_killed_agents_mount_is_unmounted_by_its_crash_guard() {
    let h = harness();
    let mut child = spawn_guarded_mounting_child(&h.root);
    eventually(
        || h.mounted(),
        Duration::from_secs(60),
        "the child's mount to come up",
    )
    .await;
    child.kill().expect("kill -9 the mounting child");
    child.wait().expect("reap the mounting child");

    eventually(
        || !h.mounted(),
        Duration::from_secs(10),
        "the crash guard to unmount the dead mount",
    )
    .await;
    assert_eq!(read_dir_errno(&h.root), 0, "the directory answers again");
    assert_eq!(ls(&h.root).await, vec!["real.txt"]);
    assert_eq!(cat(&h.root.join(REAL)).await, REAL_BYTES);
}

/// The per-location refusal, measured against the real helper: a mount point
/// outside the set a confined `fusermount3` admits (`/var/tmp` — every user's,
/// and under none of home, `/mnt`, `/media`, `/run/user/<uid>`, `/tmp`) fails,
/// and that failure classifies as the LOCATION's code, the one whose line tells
/// the user to pick another folder (`on-demand-files.md` § Linux FUSE binding,
/// the lifecycle rule). On a box whose helper is not confined the mount simply
/// stands and there is nothing to classify.
#[tokio::test]
async fn a_mount_outside_the_admitted_locations_is_refused_as_the_locations() {
    let dir = tempfile::Builder::new()
        .prefix("fauna-fuse-refused-")
        .tempdir_in("/var/tmp")
        .expect("a directory under /var/tmp");
    let reach = Reach::open(dir.path()).expect("open the reach");
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<HydrationCommand>();
    match mount_over(&reach, FOLDER, cmd_tx) {
        Ok(_mounted) => {
            eprintln!("fusermount3 is not confined on this box: the mount under /var/tmp stood")
        }
        Err(e) => assert_eq!(
            crate::engine_driver::mount_error_code(&e),
            fauna_ipc::sync::ON_DEMAND_MOUNT_REFUSED,
            "a refused location must read as the location's refusal, got: {e:#}"
        ),
    }
}

/// A clean drop of a guarded mount: the mount comes down, and the guard —
/// released after the unmount — finds nothing of its own to do. A fresh mount
/// at the same path straight after is untouched by the guard that went before.
#[tokio::test]
async fn a_released_guard_leaves_a_successor_mount_alone() {
    let h = harness();
    let first = Reach::open(&h.root).expect("open the reach");
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<HydrationCommand>();
    let mounted = mount_over(&first, FOLDER, cmd_tx).expect("mount");
    assert!(h.mounted());
    drop(mounted);
    assert!(!h.mounted(), "a clean drop unmounts");
    drop(first);

    let second = Reach::open(&h.root).expect("reopen the reach");
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<HydrationCommand>();
    let _successor = mount_over(&second, FOLDER, cmd_tx).expect("mount again");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(h.mounted(), "the successor mount is still up");
}

/// Start a child process — this very test binary — that mounts over `root` and
/// waits. The caller kills it: that is the agent crashing.
fn spawn_mounting_child(root: &Path, auto_unmount: bool) -> std::process::Child {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("this test binary"));
    cmd.args([
        "fuse_live_integration::child_mounts_and_waits",
        "--exact",
        "--ignored",
        "--nocapture",
    ])
    .env(CHILD_ROOT_VAR, root)
    .stdout(std::process::Stdio::null());
    if auto_unmount {
        cmd.env(CHILD_AUTO_UNMOUNT_VAR, "1");
    }
    cmd.spawn().expect("spawn the mounting child")
}

/// What a read of `dir` answers right now, as an errno (`0` = it listed).
fn read_dir_errno(dir: &Path) -> i32 {
    match std::fs::read_dir(dir) {
        Ok(_) => 0,
        Err(e) => e.raw_os_error().unwrap_or(-1),
    }
}

/// `ENOTCONN` — what every access to a mount whose server died answers.
const ENOTCONN: i32 = 107;

/// Boot reconcile, over a REAL ghost: an agent killed while mounted leaves the
/// mount in place and dead (every access answers `ENOTCONN`); the boot sweep
/// lazily unmounts it, the directory is an ordinary directory of its hydrated
/// files again, and a fresh root mounts over it and serves.
#[tokio::test]
async fn a_ghost_mount_is_swept_at_boot() {
    let h = harness();
    let mut child = spawn_mounting_child(&h.root, false);
    eventually(
        || h.mounted(),
        Duration::from_secs(60),
        "the child's mount to come up",
    )
    .await;
    child.kill().expect("kill the mounting child");
    child.wait().expect("reap the mounting child");

    // The crash's residue, measured: the mount is still there and it is dead.
    assert!(
        h.mounted(),
        "a killed agent's mount stays in the mount table"
    );
    let mut answered = read_dir_errno(&h.root);
    let asked = std::time::Instant::now();
    while answered != ENOTCONN && asked.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        answered = read_dir_errno(&h.root);
    }
    assert_eq!(
        answered, ENOTCONN,
        "a dead mount answers every access with ENOTCONN"
    );

    let elsewhere = h._tmp.path().join("not-a-location");
    assert_eq!(
        sweep_ghost_mounts(&[elsewhere]),
        0,
        "a mount at a location that is not configured is not this sweep's"
    );
    assert!(h.mounted());

    assert_eq!(sweep_ghost_mounts(std::slice::from_ref(&h.root)), 1);
    assert!(!h.mounted(), "the ghost is gone");
    assert_eq!(
        std::fs::read(h.root.join(REAL)).unwrap(),
        REAL_BYTES,
        "the directory is an ordinary directory of its hydrated files again"
    );

    // A fresh root now mounts over the real directory and serves its listing.
    let reach = Reach::open(&h.root).expect("open the reach after the sweep");
    let cancel = CancellationToken::new();
    let loop_fut = h.serve(&reach, cancel);
    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the fresh mount to come up",
        )
        .await;
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
    };
    drive_until_asserted(loop_fut, driver).await;
}

/// Measurement probe: does the mount helper's own `auto_unmount` clean up after a
/// killed agent on an **owner-only** mount (no `allow_other`, no `allow_root`)?
/// `fuser` refuses its typed `AutoUnmount` option on such a mount, so the option
/// is passed raw here to ask the helper directly. Prints what it measured.
///
/// **Measured 2026-10-01** (fusermount3 3.18.2, the linux dev machine): the mount
/// comes up, and ten seconds after the kill it is still in the mount table,
/// answering `ENOTCONN`. So no: with this crate and this helper a crashed agent's
/// mount is cleared by the boot sweep, not by the helper — which is what
/// [`a_ghost_mount_is_swept_at_boot`] pins and `on-demand-files.md` § Linux FUSE
/// binding records.
#[tokio::test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
async fn diag_auto_unmount_on_an_owner_only_mount() {
    let h = harness();
    let mut child = spawn_mounting_child(&h.root, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !h.mounted() && std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll the child") {
            eprintln!(
                "DIAG the child exited before mounting: {status} (the helper refused the option)"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(h.mounted(), "the child's mount never came up");
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    let line = mountinfo
        .lines()
        .find(|l| l.contains(&*h.root.to_string_lossy()))
        .unwrap_or("<no line>");
    eprintln!("DIAG mounted with auto_unmount: {line}");

    child.kill().expect("kill the mounting child");
    child.wait().expect("reap the mounting child");
    let mut gone_after = None;
    let killed = std::time::Instant::now();
    while killed.elapsed() < Duration::from_secs(10) {
        if !h.mounted() {
            gone_after = Some(killed.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!(
        "DIAG after kill -9: mount gone after {gone_after:?}; still mounted={}; read_dir errno={}",
        h.mounted(),
        read_dir_errno(&h.root)
    );
    // Leave nothing behind whatever the answer was.
    sweep_ghost_mounts(std::slice::from_ref(&h.root));
}

/// **The design's gate.** With the mount up, the descriptor reach still names the
/// directory UNDER it: the engine's path-based reads, its scan, and the shared
/// watcher all work through `/proc/self/fd/<fd>` and see only what is really on
/// the disk — never the placeholders the view adds. If this cannot hold, the
/// mount-over shape is wrong and the staging-root fallback is the design.
#[tokio::test]
async fn the_descriptor_reach_serves_engine_reads_under_the_mount() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let cancel = CancellationToken::new();
    let loop_fut = h.serve(&reach, cancel);

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        // The view, for contrast: placeholders and all.
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);

        // A path-based directory read through the reach is the underlying directory.
        assert_eq!(
            visible_names(reach.path()),
            vec![REAL],
            "no placeholder is on the disk"
        );
        // A path-based file read through it returns the bytes.
        assert_eq!(std::fs::read(reach.path().join(REAL)).unwrap(), REAL_BYTES);
        // The engine's own scan through it sees exactly the same.
        let scanned: Vec<String> = fauna_sync_engine::watcher::full_scan(reach.path())
            .expect("scan through the reach")
            .into_iter()
            .map(|f| f.relative_path)
            .collect();
        assert_eq!(scanned, vec![REAL]);

        // The watcher: a file that appears in the underlying directory is observed
        // by the loop's own `LocalWrites` over the reach, debounced, and handed to
        // the real `upload_file` — whose first act is the row. (The upload itself
        // has no nest to reach here; the row is what proves the event arrived and
        // the read through the reach worked.)
        std::fs::write(reach.path().join("new.txt"), b"written under the mount").unwrap();
        eventually(
            || h.entry("new.txt").is_some(),
            Duration::from_secs(30),
            "the watcher over the reach to observe a new file",
        )
        .await;
        let row = h.entry("new.txt").unwrap();
        assert_eq!(
            row.local_hash,
            Some(
                fauna_core::chunker_stream::content_hash_streaming(&reach.path().join("new.txt"))
                    .unwrap()
            ),
            "the engine hashed the file through the reach"
        );

        // And the view shows it too, as the underlying entry it is.
        assert_eq!(
            ls(&h.root).await,
            vec!["cloud.txt", "new.txt", "real.txt", "sub"]
        );
    };

    drive_until_asserted(loop_fut, driver).await;
}

// ---------------------------------------------------------------------------
// S2: hydrate on open, pass-through I/O, the write half, placeholder unlink
// ---------------------------------------------------------------------------

/// Run `program args…` in another process, off this thread; its exit status and
/// raw stdout. For the acts whose failure is itself the observation.
async fn run_in_another_process(program: &'static str, args: Vec<String>) -> (bool, Vec<u8>) {
    tokio::task::spawn_blocking(move || {
        let out = std::process::Command::new(program)
            .args(&args)
            .output()
            .expect("spawn the child process");
        if !out.status.success() {
            eprintln!(
                "[fuse-live] {program} {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        (out.status.success(), out.stdout)
    })
    .await
    .expect("child process task")
}

/// `cat path` from another process — the file's bytes as a reader sees them.
async fn cat(path: &Path) -> Vec<u8> {
    let (ok, bytes) =
        run_in_another_process("cat", vec![path.to_string_lossy().into_owned()]).await;
    assert!(ok, "cat {} failed", path.display());
    bytes
}

/// Write `bytes` to `path` from another process (`sh -c 'printf … > path'`),
/// through whatever the path names — here, the mounted view.
async fn write_through(path: &Path, bytes: &str) {
    let (ok, _) = run_in_another_process(
        "sh",
        vec![
            "-c".into(),
            "printf %s \"$1\" > \"$2\"".into(),
            "sh".into(),
            bytes.into(),
            path.to_string_lossy().into_owned(),
        ],
    )
    .await;
    assert!(ok, "writing {} from another process failed", path.display());
}

/// Every `FileStatusChanged` the loop has pushed so far.
fn statuses(rx: &mut broadcast::Receiver<Event>) -> Vec<(String, FileStatus)> {
    let mut seen = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let EventKind::FileStatusChanged { path, status } = ev.event {
            seen.push((path, status));
        }
    }
    seen
}

/// How many manifests the mock nest has been sent — an upload's last byte-plane
/// act, so "nothing was uploaded" is "this stayed 0".
fn manifests(store: &fauna_sync_engine::test_support::BlobStore) -> usize {
    store.manifests.lock().unwrap().len()
}

/// Hydrate: `cat` from another process opens a placeholder → the loop downloads
/// it into the directory UNDER the mount, records the served identity, pushes
/// `Synced` for the path the user sees → the open completes and `cat` prints the
/// bytes. The row is `Synced`, carries the identity, and is still never seen.
#[tokio::test]
async fn opening_a_placeholder_hydrates_it_and_flips_the_row_to_synced() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let (events, mut rx) = broadcast::channel(64);
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            events: Some(events),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        let want = cloud_bytes("cloud.txt", 1234);
        assert_eq!(cat(&h.root.join("cloud.txt")).await, want);
        // A placeholder under a directory that exists only as rows, too.
        assert_eq!(
            cat(&h.root.join("sub/deep.bin")).await,
            cloud_bytes("sub/deep.bin", 99)
        );

        let row = h.entry("cloud.txt").expect("the row");
        assert_eq!(row.state, SyncState::Synced);
        assert_eq!(row.local_hash, Some(ContentHash::of_raw(&want)));
        assert_eq!(
            row.recorded_content_hash,
            Some(ContentHash::of_raw(&want)),
            "a hydration proves the head reassembles to these bytes"
        );
        assert!(!row.seen_on_disk, "this root never writes the seen mark");

        // The bytes are on the disk under the mount, with no temp file beside them.
        assert_eq!(std::fs::read(reach.path().join("cloud.txt")).unwrap(), want);
        assert_eq!(visible_names(reach.path()), vec!["cloud.txt", REAL, "sub"]);

        let pushed = statuses(&mut rx);
        assert!(
            pushed.contains(&(
                format!("{}/cloud.txt", h.root.display()),
                FileStatus::Synced
            )),
            "a Synced status for the MOUNT POINT's path, never /proc/self/fd/…: {pushed:?}"
        );
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// Zero bytes: an empty placeholder lists as 0 bytes, opens, reads empty, and
/// leaves an empty file under the mount and a `Synced` row.
#[tokio::test]
async fn a_zero_byte_placeholder_reads_synced_and_hydrates_to_an_empty_file() {
    let h = harness();
    h.seed_placeholder("empty.txt", 0);
    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(
            stat(&h.root.join("empty.txt")).await,
            format!("0 {CLOUD_MTIME} regular empty file")
        );
        assert!(cat(&h.root.join("empty.txt")).await.is_empty());
        assert_eq!(
            h.entry("empty.txt").expect("the row").state,
            SyncState::Synced
        );
        assert_eq!(
            std::fs::read(reach.path().join("empty.txt")).unwrap(),
            Vec::<u8>::new()
        );
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// A cloud-only placeholder is never uploaded and never deleted: browse it from
/// another process, let full rescan passes (`converge`) run over the root, and the
/// rows are untouched placeholders — never seen, no local identity — and the nest
/// was sent nothing.
#[tokio::test]
async fn a_cloud_only_placeholder_is_never_uploaded() {
    let nest = wiremock::MockServer::start().await;
    let store = fauna_sync_engine::test_support::MockNest::new()
        .mount(&nest)
        .await;
    let h = harness_against(&nest.uri());
    let reach = Reach::open(&h.root).expect("open the reach");
    let ticks = Arc::new(AtomicUsize::new(0));
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            rescan: Duration::from_millis(500),
            ticks: Arc::clone(&ticks),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
        assert_eq!(ls(&h.root.join("sub")).await, vec!["deep.bin"]);
        stat(&h.root.join("cloud.txt")).await;
        let browsed = ticks.load(Ordering::SeqCst);
        eventually(
            || ticks.load(Ordering::SeqCst) >= browsed + 2,
            Duration::from_secs(30),
            "two full rescan passes after the browse",
        )
        .await;

        for (rel, _) in CLOUD {
            let row = h.entry(rel).expect("the placeholder row");
            assert_eq!(row.state, SyncState::Placeholder, "{rel}");
            assert!(!row.seen_on_disk, "{rel}: browsing never marks a row seen");
            assert_eq!(row.local_hash, None, "{rel}: nothing local was adopted");
        }
        assert_eq!(manifests(&store), 0, "nothing was uploaded");
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// The cfapi twin's contract on a FUSE root. (1) A hydration is NOT echoed back
/// up: the rename the loop makes is seen by the watcher, and only the recorded
/// identity keeps the real `upload_file` from re-uploading it. (2) An edit by
/// another process — written THROUGH the mount — passes through to the file
/// under it, the watcher sees it, and the real `upload_file` sends the new bytes.
#[tokio::test]
async fn a_local_edit_by_another_process_is_detected_and_uploaded() {
    const EDITED: &str = "the user typed this instead";

    let nest = wiremock::MockServer::start().await;
    let store = fauna_sync_engine::test_support::MockNest::new()
        .mount(&nest)
        .await;
    let h = harness_against(&nest.uri());
    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(
            cat(&h.root.join("cloud.txt")).await,
            cloud_bytes("cloud.txt", 1234)
        );
        // Past the shared uploader's 2 s debounce: the hydration's own write
        // must not have been taken for an edit.
        tokio::time::sleep(Duration::from_millis(3500)).await;
        assert_eq!(
            manifests(&store),
            0,
            "hydrating a file must not upload it straight back"
        );

        write_through(&h.root.join("cloud.txt"), EDITED).await;
        assert_eq!(
            std::fs::read(reach.path().join("cloud.txt")).unwrap(),
            EDITED.as_bytes(),
            "the write passed through to the file under the mount"
        );
        let want = ContentHash::of_raw(EDITED.as_bytes());
        eventually(
            || h.entry("cloud.txt").and_then(|r| r.local_hash) == Some(want),
            Duration::from_secs(30),
            "the watcher to see the edit and upload_file to take its identity",
        )
        .await;
        eventually(
            || manifests(&store) > 0,
            Duration::from_secs(30),
            "the edited bytes to reach the nest's byte plane",
        )
        .await;
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// A new file written through the mount is an ordinary file under it: the
/// watcher adopts it and the real `upload_file` sends it.
#[tokio::test]
async fn a_new_file_written_through_the_mount_is_adopted_and_uploaded() {
    const FRESH: &str = "a file the user just made";

    let nest = wiremock::MockServer::start().await;
    let store = fauna_sync_engine::test_support::MockNest::new()
        .mount(&nest)
        .await;
    let h = harness_against(&nest.uri());
    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        // Into a directory that exists only as rows, so the create makes it real.
        write_through(&h.root.join("sub/fresh.txt"), FRESH).await;
        assert_eq!(
            std::fs::read(reach.path().join("sub/fresh.txt")).unwrap(),
            FRESH.as_bytes()
        );
        assert_eq!(ls(&h.root.join("sub")).await, vec!["deep.bin", "fresh.txt"]);
        let want = ContentHash::of_raw(FRESH.as_bytes());
        eventually(
            || h.entry("sub/fresh.txt").and_then(|r| r.local_hash) == Some(want),
            Duration::from_secs(30),
            "the watcher to adopt the new file",
        )
        .await;
        eventually(
            || manifests(&store) > 0,
            Duration::from_secs(30),
            "the new file's bytes to reach the nest's byte plane",
        )
        .await;
        assert_eq!(
            h.entry("sub/deep.bin").expect("the row").state,
            SyncState::Placeholder,
            "its placeholder neighbour is untouched"
        );
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// `rm` of a placeholder from another process is the user's delete, and the
/// one way a placeholder is deleted here: the loop's `handle_delete` moves the
/// row to `LocallyDeleted` BEFORE it records (no nest answers in this harness,
/// so the record stays owed — the row is the evidence), and the name is gone
/// from the view.
#[tokio::test]
async fn unlinking_a_placeholder_records_a_delete_first() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
        let (removed, _) = run_in_another_process(
            "rm",
            vec![h.root.join("cloud.txt").to_string_lossy().into_owned()],
        )
        .await;
        assert!(removed, "rm of a placeholder succeeds");
        let row = h
            .entry("cloud.txt")
            .expect("the row survives as the evidence");
        assert_eq!(
            row.state,
            SyncState::LocallyDeleted,
            "the delete is owed to the nest, and never listed again"
        );
        assert_eq!(ls(&h.root).await, vec!["real.txt", "sub"]);
        // The hydrated neighbour and the other placeholder are untouched.
        assert_eq!(h.entry(REAL).expect("the row").state, SyncState::Synced);
        assert_eq!(
            h.entry("sub/deep.bin").expect("the row").state,
            SyncState::Placeholder
        );
    };

    drive_until_asserted(loop_fut, driver).await;
}

// ── S3: dehydrate, pins, and the never-a-delete invariant ──────────────────────

/// Send the pipe server's verb for `rel` through the engine's command channel and
/// wait for the loop's answer — `handle_free_space` / `handle_pin_file` /
/// `handle_unpin_file` on linux (`pipe_server::route_row_verb`), minus the path
/// resolution.
async fn verb(
    commands: &mpsc::Sender<EngineCommand>,
    make: impl FnOnce(tokio::sync::oneshot::Sender<Result<()>>) -> EngineCommand,
) -> Result<()> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    commands
        .send(make(reply))
        .await
        .map_err(|_| anyhow!("the loop is gone"))?;
    answer
        .await
        .map_err(|_| anyhow!("the loop dropped the reply"))?
}

fn free_space(rel: &str) -> impl FnOnce(tokio::sync::oneshot::Sender<Result<()>>) -> EngineCommand {
    let rel = rel.to_string();
    move |reply| EngineCommand::FreeSpace { rel, reply }
}

fn set_pinned(
    rel: &str,
    pinned: bool,
) -> impl FnOnce(tokio::sync::oneshot::Sender<Result<()>>) -> EngineCommand {
    let rel = rel.to_string();
    move |reply| EngineCommand::SetPinned { rel, pinned, reply }
}

/// **A dehydrated file is never recorded as a delete** — the cfapi twin's name,
/// over this binding's own mechanism. *Free up space* on a hydrated file: the
/// bytes leave the disk under the mount, the name stays in the view (now listed
/// from its row), the row is `Placeholder` and never seen, `CloudOnly` is pushed
/// for the path the user sees — and after two full rescan passes (`converge` over
/// the reach, the watcher having seen the unlink) the row is still a placeholder:
/// no delete was decided. The nest here is unreachable, so a decided delete would
/// show as a `LocallyDeleted` row (`handle_delete` flips it before recording).
#[tokio::test]
async fn a_dehydrated_file_is_never_recorded_as_a_delete() {
    let h = harness();
    h.stamp_recorded(REAL);
    let reach = Reach::open(&h.root).expect("open the reach");
    let (events, mut rx) = broadcast::channel(64);
    let (commands, commands_rx) = mpsc::channel(2);
    let ticks = Arc::new(AtomicUsize::new(0));
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            events: Some(events),
            rescan: Duration::from_millis(500),
            ticks: Arc::clone(&ticks),
            commands: Some(commands_rx),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        verb(&commands, free_space(REAL))
            .await
            .expect("freeing a recorded file succeeds");

        assert!(
            !reach.path().join(REAL).exists(),
            "the bytes left the disk under the mount"
        );
        assert_eq!(
            ls(&h.root).await,
            vec!["cloud.txt", "real.txt", "sub"],
            "the name stays in the view"
        );
        assert_eq!(
            stat(&h.root.join(REAL)).await.split(' ').next(),
            Some(REAL_BYTES.len().to_string().as_str()),
            "listed from its row, at its size"
        );
        let row = h.entry(REAL).expect("the row");
        assert_eq!(row.state, SyncState::Placeholder);
        assert!(!row.seen_on_disk, "this root never writes the seen mark");
        let pushed = statuses(&mut rx);
        assert!(
            pushed.contains(&(
                format!("{}/{REAL}", h.root.display()),
                FileStatus::CloudOnly
            )),
            "CloudOnly for the MOUNT POINT's path: {pushed:?}"
        );

        let freed = ticks.load(Ordering::SeqCst);
        eventually(
            || ticks.load(Ordering::SeqCst) >= freed + 2,
            Duration::from_secs(30),
            "two full rescan passes after the free",
        )
        .await;
        let row = h.entry(REAL).expect("the row survives");
        assert_eq!(
            row.state,
            SyncState::Placeholder,
            "no delete was decided for a freed file"
        );
        assert!(!row.seen_on_disk);
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// *Free up space* refuses a file whose content the engine has not recorded (an
/// upload whose record never landed): nothing is unlinked, the row stays
/// `Synced`. The gate is the engine's — this binding has no OS refusal.
#[tokio::test]
async fn freeing_an_unrecorded_file_is_refused() {
    let h = harness();
    // Not stamped: the row's recorded head is unproven.
    let reach = Reach::open(&h.root).expect("open the reach");
    let (commands, commands_rx) = mpsc::channel(2);
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            commands: Some(commands_rx),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert!(verb(&commands, free_space(REAL)).await.is_err());
        assert_eq!(std::fs::read(reach.path().join(REAL)).unwrap(), REAL_BYTES);
        assert_eq!(h.entry(REAL).expect("the row").state, SyncState::Synced);
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// *Free up space* refuses this device's own recorded write once the folder
/// reads metadata-only (`file-sync.md` § Relay serving → *A holder keeps what it
/// wrote*): the nest holds no byte of it, so the disk's copy may be the only
/// one. The record landed while the folder was full; the residency the gate
/// reads is the one standing when the verb runs.
#[tokio::test]
async fn freeing_an_own_record_in_a_metadata_only_folder_is_refused() {
    let h = harness();
    h.stamp_recorded(REAL);
    let reach = Reach::open(&h.root).expect("open the reach");
    let (commands, commands_rx) = mpsc::channel(2);
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            commands: Some(commands_rx),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        // What the engine's next folder-list read persists after the flip.
        SyncDb::open(&h.db_path)
            .expect("reopen db")
            .set_residency_reading(true)
            .expect("persist the residency reading");
        assert!(verb(&commands, free_space(REAL)).await.is_err());
        assert_eq!(std::fs::read(reach.path().join(REAL)).unwrap(), REAL_BYTES);
        assert_eq!(h.entry(REAL).expect("the row").state, SyncState::Synced);
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// **A pin hydrates**: pinning a cloud-only file brings its bytes to the disk now
/// (through the same hydration an `open` takes), the row says `Synced` and pinned,
/// and *Free up space* on a pinned file refuses.
#[tokio::test]
async fn a_pin_hydrates() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let (commands, commands_rx) = mpsc::channel(2);
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            commands: Some(commands_rx),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        verb(&commands, set_pinned("cloud.txt", true))
            .await
            .expect("pinning a tracked file succeeds");
        let under = reach.path().join("cloud.txt");
        eventually(
            || {
                h.entry("cloud.txt")
                    .is_some_and(|e| e.state == SyncState::Synced)
            },
            Duration::from_secs(20),
            "the pinned placeholder to hydrate",
        )
        .await;
        assert_eq!(
            std::fs::read(&under).unwrap(),
            cloud_bytes("cloud.txt", 1234)
        );
        assert!(h.entry("cloud.txt").expect("the row").pinned);

        assert!(
            verb(&commands, free_space("cloud.txt")).await.is_err(),
            "a pinned file refuses Free up space"
        );
        assert!(under.exists(), "and keeps its bytes");
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// A pin made while nothing ran (the row carries it) is honoured at the next
/// start: the boot's pin sweep reads the pinned placeholders from the rows — no
/// file on this disk carries a pin — and hydrates them.
#[tokio::test]
async fn a_pinned_placeholder_hydrates_at_boot() {
    let h = harness();
    h.pin("sub/deep.bin");
    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());

    let driver = async {
        eventually(
            || {
                h.entry("sub/deep.bin")
                    .is_some_and(|e| e.state == SyncState::Synced)
            },
            Duration::from_secs(20),
            "the pinned placeholder to hydrate at boot",
        )
        .await;
        assert_eq!(
            std::fs::read(reach.path().join("sub/deep.bin")).unwrap(),
            cloud_bytes("sub/deep.bin", 99)
        );
        assert_eq!(
            h.entry("cloud.txt").expect("the row").state,
            SyncState::Placeholder,
            "an unpinned placeholder stays in the cloud"
        );
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// **An unpin dehydrates**: a pinned, recorded file whose pin is cleared leaves
/// the disk as *Free up space* would — row `Placeholder`, unpinned, never seen.
#[tokio::test]
async fn an_unpin_dehydrates() {
    let h = harness();
    h.stamp_recorded(REAL);
    h.pin(REAL);
    let reach = Reach::open(&h.root).expect("open the reach");
    let (commands, commands_rx) = mpsc::channel(2);
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            commands: Some(commands_rx),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert!(
            reach.path().join(REAL).exists(),
            "a pinned file keeps its bytes"
        );
        verb(&commands, set_pinned(REAL, false))
            .await
            .expect("unpinning a tracked file succeeds");
        assert!(
            !reach.path().join(REAL).exists(),
            "the unpin freed the bytes"
        );
        let row = h.entry(REAL).expect("the row");
        assert_eq!(row.state, SyncState::Placeholder);
        assert!(!row.pinned);
        assert!(!row.seen_on_disk);
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
    };

    drive_until_asserted(loop_fut, driver).await;
}

/// **A remote modify of a hydrated file re-fetches**: the nest head moves under a
/// hydrated file (the fold reports it stale), the root frees the superseded bytes
/// through the off-disk form — row first, never seen, no delete — and re-points
/// the row at the new head, so the next `open` from another process reads the new
/// content.
#[tokio::test]
async fn a_remote_modify_of_a_hydrated_file_refetches() {
    const NEW_SIZE: i64 = 77;
    let h = harness();
    h.stamp_recorded(REAL);
    let reach = Reach::open(&h.root).expect("open the reach");
    let ticks = Arc::new(AtomicUsize::new(0));
    let stale = Arc::new(std::sync::Mutex::new(Vec::new()));
    let new_head = ContentHash::of_raw(b"the nest's new head manifest");
    let loop_fut = h.serve_with(
        &reach,
        CancellationToken::new(),
        Serve {
            rescan: Duration::from_millis(500),
            ticks: Arc::clone(&ticks),
            stale: Arc::clone(&stale),
            ..Serve::default()
        },
    );

    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        stale.lock().unwrap().push(StaleHydratedRow {
            relative_path: REAL.to_string(),
            manifest_hash: new_head,
            size_bytes: NEW_SIZE,
            content_key_version: None,
            remote_mtime: CLOUD_MTIME,
            version_num: 2,
        });
        eventually(
            || {
                h.entry(REAL)
                    .is_some_and(|e| e.manifest_hash == Some(new_head))
            },
            Duration::from_secs(20),
            "the stale row to be re-pointed at the new head",
        )
        .await;
        let row = h.entry(REAL).expect("the row");
        assert_eq!(row.state, SyncState::Placeholder);
        assert!(!row.seen_on_disk);
        assert!(
            !reach.path().join(REAL).exists(),
            "the superseded bytes are gone"
        );

        // A full pass over the freed file decides no delete.
        let freed = ticks.load(Ordering::SeqCst);
        eventually(
            || ticks.load(Ordering::SeqCst) >= freed + 2,
            Duration::from_secs(30),
            "two full rescan passes after the re-point",
        )
        .await;
        assert_eq!(
            h.entry(REAL).expect("the row").state,
            SyncState::Placeholder
        );

        assert_eq!(
            cat(&h.root.join(REAL)).await,
            cloud_bytes(REAL, NEW_SIZE),
            "the next open reads the new head"
        );
        let row = h.entry(REAL).expect("the row");
        assert_eq!(row.state, SyncState::Synced);
        assert_eq!(row.manifest_hash, Some(new_head));
    };

    drive_until_asserted(loop_fut, driver).await;
}

// ---------------------------------------------------------------------------
// Mode flips (`on-demand-files.md` § Linux FUSE binding: *Mode flips and
// unbinds are mount/unmount, nothing else*)
// ---------------------------------------------------------------------------

impl Harness {
    /// Re-point every [`CLOUD`] row at a head the mock byte plane really holds
    /// ([`cloud_bytes`], sealed by the engine's own pipeline), still a
    /// `Placeholder` — so a resident engine can fetch it the way the product
    /// does, where the on-demand loop's faked fetch needs no plane at all.
    async fn seed_real_cloud_heads(&self) {
        let engine = self.host(&self.root, &Serve::default()).engine;
        // The heads are this seat's own records: a head with no persisted
        // signer reads as another writer's and is offered no owner root.
        let own = fauna_core::identity::ActorId::from_hex(&engine.owner_actor_id_hex())
            .expect("the harness's actor id")
            .0;
        for (rel, size) in CLOUD {
            let manifest = engine
                .upload_bytes(cloud_bytes(rel, *size), rel, FOLDER)
                .await
                .expect("store the cloud file on the mock byte plane");
            let db = SyncDb::open(&self.db_path).expect("reopen db");
            db.upsert_entry(
                rel,
                None,
                None,
                Some(manifest),
                SyncState::Placeholder,
                0,
                CLOUD_MTIME,
                *size,
                1,
                None,
            )
            .expect("re-point the placeholder row");
            db.set_head_signed_as(rel, &manifest, &own)
                .expect("stamp the head's signer");
        }
    }

    /// The resident root over the bound directory — the shared watch loop the
    /// agent's `run_always_resident_root` drives, on a fresh production engine
    /// over the same state DB (a flip builds a new engine; it never inherits the
    /// on-demand one's posture).
    fn resident(&self) -> impl Future<Output = ()> + '_ {
        #[allow(clippy::arc_with_non_send_sync)]
        let engine = Arc::new(self.host(&self.root, &Serve::default()).engine);
        fauna_sync_engine::always_resident::run_watch_loop(
            engine,
            self.root.clone(),
            FOLDER.to_string(),
            Duration::from_millis(500),
            None,
            None,
        )
    }

    fn state_of(&self, rel: &str) -> Option<SyncState> {
        self.entry(rel).map(|e| e.state)
    }
}

/// on-demand → always: the loop's end unmounts, the directory holds exactly the
/// files that were hydrated, and the resident engine started next fetches every
/// `Placeholder` row — a pending download, never a delete. Every look at the
/// files is from another process.
#[tokio::test]
async fn a_flip_to_always_unmounts_and_the_resident_engine_fetches_the_placeholders() {
    let nest = wiremock::MockServer::start().await;
    let _store = fauna_sync_engine::test_support::MockNest::new()
        .with_blob_plane()
        .mount(&nest)
        .await;
    let h = harness_against(&nest.uri());
    h.seed_real_cloud_heads().await;

    // On-demand: browse, and hydrate one file by reading it.
    let reach = Reach::open(&h.root).expect("open the reach");
    let cancel = CancellationToken::new();
    let loop_fut = h.serve(&reach, cancel.clone());
    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(
            cat(&h.root.join("cloud.txt")).await,
            cloud_bytes("cloud.txt", 1234)
        );
        assert_eq!(h.state_of("cloud.txt"), Some(SyncState::Synced));
        cancel.cancel();
    };
    tokio::join!(loop_fut, driver);
    drop(reach);

    // The flip's first half: unmounted, the hydrated files left in place.
    assert!(!h.mounted(), "the on-demand root's end unmounts");
    assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt"]);
    assert_eq!(h.state_of("sub/deep.bin"), Some(SyncState::Placeholder));

    // The second half: the resident engine fetches what was cloud-only.
    let driver = async {
        eventually(
            || h.state_of("sub/deep.bin") == Some(SyncState::Synced),
            Duration::from_secs(60),
            "the resident engine to fetch the placeholder",
        )
        .await;
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
        assert_eq!(
            cat(&h.root.join("sub/deep.bin")).await,
            cloud_bytes("sub/deep.bin", 99)
        );
        assert_eq!(cat(&h.root.join(REAL)).await, REAL_BYTES);
        for rel in [REAL, "cloud.txt", "sub/deep.bin"] {
            assert_eq!(h.state_of(rel), Some(SyncState::Synced), "{rel}");
        }
    };
    drive_until_asserted(h.resident(), driver).await;
    assert!(!h.mounted());
}

/// always → on-demand: the resident engine stops, the mount goes up over the
/// directory, and every file already there is simply a hydrated file — read
/// through the mount from another process at its bytes, its row still `Synced`,
/// nothing uploaded, and a cloud-only row listed beside them.
#[tokio::test]
async fn a_flip_to_on_demand_mounts_over_the_files_already_there() {
    let nest = wiremock::MockServer::start().await;
    let store = fauna_sync_engine::test_support::MockNest::new()
        .with_blob_plane()
        .mount(&nest)
        .await;
    let h = harness_against(&nest.uri());
    h.seed_real_cloud_heads().await;
    let before = manifests(&store);

    // Always: the resident root fetches both cloud rows.
    let driver = async {
        eventually(
            || {
                h.state_of("cloud.txt") == Some(SyncState::Synced)
                    && h.state_of("sub/deep.bin") == Some(SyncState::Synced)
            },
            Duration::from_secs(60),
            "the resident engine to fetch the placeholders",
        )
        .await;
    };
    drive_until_asserted(h.resident(), driver).await;
    // A file the nest learns of while the folder flips: cloud-only.
    h.seed_placeholder("later.bin", 7);

    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());
    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        assert_eq!(
            ls(&h.root).await,
            vec!["cloud.txt", "later.bin", "real.txt", "sub"]
        );
        assert_eq!(cat(&h.root.join(REAL)).await, REAL_BYTES);
        assert_eq!(
            cat(&h.root.join("sub/deep.bin")).await,
            cloud_bytes("sub/deep.bin", 99)
        );
        for rel in [REAL, "cloud.txt", "sub/deep.bin"] {
            assert_eq!(h.state_of(rel), Some(SyncState::Synced), "{rel}");
        }
        assert_eq!(h.state_of("later.bin"), Some(SyncState::Placeholder));
        assert_eq!(manifests(&store), before, "the flip uploaded nothing");
    };
    drive_until_asserted(loop_fut, driver).await;
    assert!(!h.mounted());
}

/// `rename(2)` of a directory still holding placeholder rows answers `EXDEV`
/// (kept by S4's review — the plan's *Revision 2026-10-02 (S4)*), so `mv` falls
/// back to copy-then-delete through paths already pinned here; nothing under it
/// moves and no row changes. Asked with a bare `rename(2)` from another process
/// (`perl`, whose exit status is the errno), since `mv` would quietly copy.
#[tokio::test]
async fn renaming_a_directory_that_holds_placeholder_rows_answers_exdev() {
    let h = harness();
    let reach = Reach::open(&h.root).expect("open the reach");
    let loop_fut = h.serve(&reach, CancellationToken::new());
    let driver = async {
        eventually(
            || h.mounted(),
            Duration::from_secs(20),
            "the mount to come up",
        )
        .await;
        let status = tokio::task::spawn_blocking({
            let from = h.root.join("sub");
            let to = h.root.join("moved");
            move || {
                std::process::Command::new("perl")
                    .args(["-e", "rename($ARGV[0], $ARGV[1]) ? exit 0 : exit($! + 0)"])
                    .arg(from)
                    .arg(to)
                    .status()
                    .expect("spawn perl")
            }
        })
        .await
        .expect("rename task");
        assert_eq!(status.code(), Some(libc::EXDEV), "rename answers EXDEV");
        assert_eq!(ls(&h.root).await, vec!["cloud.txt", "real.txt", "sub"]);
        assert_eq!(ls(&h.root.join("sub")).await, vec!["deep.bin"]);
        assert_eq!(
            h.entry("sub/deep.bin").map(|e| e.state),
            Some(SyncState::Placeholder)
        );
        assert!(h.entry("moved/deep.bin").is_none());
    };
    drive_until_asserted(loop_fut, driver).await;
}
