//! The Linux FUSE OS shim: the filesystem the agent mounts **over** an on-demand
//! location, plus the mount lifecycle (reach / mount / unmount / ghost sweep /
//! availability probe). The `cfapi_host` twin — see
//! `docs/goal/behavior/on-demand-files.md` § Linux FUSE binding.
//!
//! This is the thin boundary between the kernel and the deterministically-tested
//! core in [`crate::bridge`]. The FUSE session thread translates kernel requests
//! into [`HydrationCommand`]s for the root's `!Sync` driving loop and completes
//! them; it never touches the engine or its state DB itself.
//!
//! ## Mount-over, and the reach
//!
//! The bound directory IS the backing store, and the mount is a view of it: the
//! union of what the underlying directory holds (hydrated files, the user's own
//! new files) and the state DB's `Placeholder` rows (names, sizes, mtimes, no
//! bytes). The agent opens the directory BEFORE mounting and keeps the descriptor;
//! [`Reach::path`] (`/proc/self/fd/<fd>`) names the *underlying* directory for as
//! long as the descriptor lives, mount or no mount. The engine's `watch_dir`, the
//! watcher and every read this module makes of the underlying tree go through
//! it. **Nothing in the agent may read the mount point's own path while the mount
//! is up**: that names the FUSE view, and a read of the view from the driving
//! thread would wait on a request only that thread can serve.
//!
//! ## A placeholder is never on the disk, so it is never *seen*
//!
//! On cfapi a placeholder is a file on the disk and the engine marks its row
//! *seen* once it is there; a seen row missing from a scan is a delete
//! (`delete-propagation.md` § *An offline placeholder delete propagates*). Here a
//! placeholder exists only as its row, so a seen mark would make every listed
//! file read as a user delete at the next sweep. [`ListingSink`] therefore
//! reports **nothing placed**, whatever it lists — the binding's first obligation
//! under the dehydrate rule of § Linux FUSE binding.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fauna_sync_engine::enumerate::DirChild;
use fuser::{
    BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation,
    INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
    WriteFlags,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::{HYDRATING_PREFIX, HydrationCommand, LoopReply, PlaceholderSink};

/// The filesystem subtype every mount of this binding carries: the kernel reports
/// the mount's type as `fuse.<subtype>`, which is how [`ghost_mount_points`]
/// tells this agent's own leftover from anybody else's mount.
const SUBTYPE: &str = "fauna";
/// The `fstype` field `/proc/self/mountinfo` shows for a mount of this binding.
const MOUNTINFO_FSTYPE: &str = "fuse.fauna";
/// The mount's source name (`fsname=`), shown by `mount` and `df`.
const FSNAME: &str = "fauna-sync-agent";
/// The unprivileged mount helper of the distro `fuse3` package (setuid root —
/// the agent itself never runs as root).
const FUSERMOUNT: &str = "fusermount3";

/// How long the kernel may cache an entry or its attributes. Short: a placeholder
/// that hydrates changes size and kind of presence under the same name.
const TTL: Duration = Duration::from_secs(1);
/// How long one directory's placeholder children stay cached on the session
/// thread. `ls -l` asks for a listing and then one lookup per name; without this
/// each lookup would be its own round trip to the driving loop. A cached listing
/// is also dropped as soon as the underlying directory changes (its mtime): the
/// loop frees and lands files there without passing through this view.
const LISTING_TTL: Duration = Duration::from_secs(1);
/// How often a request waiting on the driving loop re-checks that the mount is
/// not being torn down.
const LOOP_POLL: Duration = Duration::from_millis(100);

/// The inode of the mount's root directory (the FUSE protocol's fixed value).
const ROOT_INO: u64 = 1;

// ---------------------------------------------------------------------------
// The reach
// ---------------------------------------------------------------------------

/// The pre-mount descriptor on a bound directory, and the path that names the
/// **underlying** directory through it.
///
/// Open it before the engine is built (the engine's `watch_dir` is fixed at
/// build) and keep it for as long as the engine and the mount live.
pub(crate) struct Reach {
    /// Held for its descriptor: [`Self::path`] is valid exactly as long as this is.
    _dir: std::fs::File,
    mount_point: PathBuf,
    path: PathBuf,
}

impl Reach {
    /// Open `sync_root` — the directory the mount will cover. Call it only while
    /// no mount of this binding is on that path (the boot sweep clears a dead
    /// agent's leftover; a live root's guard unmounts before its engine is
    /// rebuilt), or the descriptor lands on a FUSE view instead of the directory.
    pub(crate) fn open(sync_root: &Path) -> Result<Self> {
        let dir = std::fs::File::open(sync_root)
            .with_context(|| format!("opening the bound directory {}", sync_root.display()))?;
        if !dir.metadata()?.is_dir() {
            bail!("{} is not a directory", sync_root.display());
        }
        let path = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
        Ok(Self {
            _dir: dir,
            mount_point: sync_root.to_path_buf(),
            path,
        })
    }

    /// The underlying directory, by descriptor. Never canonicalize it: that
    /// resolves to the mount point's own text, which names the FUSE view once
    /// the mount is up.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The path the user bound — where the mount goes.
    pub(crate) fn mount_point(&self) -> &Path {
        &self.mount_point
    }
}

// ---------------------------------------------------------------------------
// Mount lifecycle
// ---------------------------------------------------------------------------

/// A mounted root. Dropping it unmounts: the session is a drop guard, like
/// `CfApiConnection`, and the directory is left holding its hydrated files.
pub(crate) struct MountedRoot {
    /// Set before the unmount so a request parked on the driving loop gives up
    /// instead of holding the session thread the unmount joins.
    closing: Arc<AtomicBool>,
    session: Option<fuser::BackgroundSession>,
    /// The process that unmounts this mount if the agent dies holding it
    /// ([`CrashGuard`]); `None` for an unguarded mount (a measurement probe, or a
    /// guard that could not start — then the boot sweep is the only cleanup).
    guard: Option<CrashGuard>,
    mount_point: PathBuf,
}

impl Drop for MountedRoot {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        // The session's own drop unmounts and joins its thread.
        drop(self.session.take());
        // Only now released: the guard finds its mount gone and leaves.
        drop(self.guard.take());
        tracing::info!(path = %self.mount_point.display(), "on-demand root unmounted");
    }
}

/// Mount the on-demand view over `reach`'s directory, routing its requests to the
/// root's driving loop through `cmd_tx`.
///
/// Options are the binding's (§ Linux FUSE binding, the lifecycle rule):
/// `default_permissions` (the kernel enforces the mode bits this filesystem
/// reports) and never `allow_other` — the mount is its owner's alone.
///
/// **No `auto_unmount`, and that is measured, not chosen** (2026-10-01): `fuser`
/// refuses its `AutoUnmount` option on an owner-only mount (it wants
/// `allow_other` or `allow_root`, which this binding never takes), and the same
/// option passed raw to the helper mounts but does not unmount after a `kill -9`
/// (`fuse_live_integration::diag_auto_unmount_on_an_owner_only_mount`). The
/// agent's own [`CrashGuard`] closes that window instead: a crashed agent's dead
/// mount — every access answers `ENOTCONN` — is lazily unmounted within moments,
/// and [`sweep_ghost_mounts`] at the next boot clears any the guard could not
/// (it died with the agent). A clean stop never leaves one: [`MountedRoot`]'s
/// drop unmounts.
pub(crate) fn mount_over(
    reach: &Reach,
    folder: &str,
    cmd_tx: UnboundedSender<HydrationCommand>,
) -> Result<MountedRoot> {
    let mut mounted = mount_with(reach, folder, cmd_tx, Vec::new())?;
    match CrashGuard::spawn(reach.mount_point()) {
        Ok(guard) => mounted.guard = Some(guard),
        Err(e) => tracing::warn!(
            folder,
            path = %reach.mount_point().display(),
            error = %e,
            "no crash guard for the on-demand root; a crash leaves it to the boot sweep"
        ),
    }
    Ok(mounted)
}

/// [`mount_with`] plus `extra` mount options and NO crash guard — for the live
/// harness's measurement probes, which ask what the helper does with an option
/// the product does not pass, and for the crash test of the boot sweep, which
/// needs the ghost a guard would have cleared.
#[cfg(all(test, target_os = "linux", feature = "fuse-live"))]
pub(crate) fn mount_over_probing(
    reach: &Reach,
    folder: &str,
    cmd_tx: UnboundedSender<HydrationCommand>,
    extra: Vec<MountOption>,
) -> Result<MountedRoot> {
    mount_with(reach, folder, cmd_tx, extra)
}

fn mount_with(
    reach: &Reach,
    folder: &str,
    cmd_tx: UnboundedSender<HydrationCommand>,
    extra: Vec<MountOption>,
) -> Result<MountedRoot> {
    let root_meta = std::fs::metadata(reach.path()).context("reading the bound directory")?;
    let closing = Arc::new(AtomicBool::new(false));
    let fs = FuseRoot(Arc::new(Root {
        reach: reach.path().to_path_buf(),
        cmd_tx,
        closing: Arc::clone(&closing),
        uid: root_meta.uid(),
        gid: root_meta.gid(),
        state: Mutex::new(State {
            inodes: Inodes::new(),
            listings: HashMap::new(),
            open_dirs: HashMap::new(),
            open_files: HashMap::new(),
            next_fh: 1,
        }),
    }));
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        MountOption::FSName(FSNAME.to_string()),
        MountOption::Subtype(SUBTYPE.to_string()),
        MountOption::DefaultPermissions,
    ];
    config.mount_options.extend(extra);
    let session = fuser::spawn_mount(fs, reach.mount_point(), &config).with_context(|| {
        format!(
            "mounting the on-demand root over {}",
            reach.mount_point().display()
        )
    })?;
    tracing::info!(
        folder,
        path = %reach.mount_point().display(),
        "on-demand root mounted"
    );
    Ok(MountedRoot {
        closing,
        session: Some(session),
        guard: None,
        mount_point: reach.mount_point().to_path_buf(),
    })
}

// ---------------------------------------------------------------------------
// The crash guard
// ---------------------------------------------------------------------------

/// The hidden first argument that runs the agent binary as a mount's crash
/// guard ([`run_guard_from_env`]) — internal wiring between the agent and the
/// child it starts, never a user's knob.
pub(crate) const GUARD_ARG: &str = "__fuse-guard";
/// The guarded mount's id in the mount table, handed to the guard.
const GUARD_MOUNT_ID_VAR: &str = "FAUNA_FUSE_GUARD_MOUNT_ID";
/// The guarded mount point, handed to the guard.
const GUARD_MOUNT_POINT_VAR: &str = "FAUNA_FUSE_GUARD_MOUNT_POINT";
/// How long the guard waits on its liveness probe of the mount before it
/// decides it cannot tell, and leaves the mount alone.
const GUARD_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The process that unmounts a crashed agent's mount (`on-demand-files.md`
/// § Linux FUSE binding, the lifecycle rule: *closing that window without a
/// restart is owed by the agent's own means*).
///
/// At mount the agent re-executes itself ([`GUARD_ARG`], through
/// `/proc/self/exe`, so an upgrade that replaced the binary on disk does not
/// stop it) as a child holding the read end of a pipe on its stdin; the agent
/// holds the write end. However the agent goes — a crash, a `kill -9` — the
/// kernel closes that end, the guard reads EOF, and unmounts the mount it was
/// given **only if** that mount is still in the table under its id AND is dead
/// (a probe answers `ENOTCONN`): a successor's live mount at the same path —
/// even one that inherited a recycled id — is never touched. The guard runs in
/// a process group of its own, so a signal to the agent's terminal group does
/// not take it along. A guard killed with the agent (a whole-cgroup stop) leaves
/// the ghost to [`sweep_ghost_mounts`] at the next boot.
struct CrashGuard {
    child: std::process::Child,
}

impl CrashGuard {
    /// Start the guard for the mount just made at `mount_point`.
    fn spawn(mount_point: &Path) -> Result<Self> {
        use std::os::unix::process::CommandExt;
        let mountinfo =
            std::fs::read_to_string("/proc/self/mountinfo").context("reading the mount table")?;
        let id = binding_mounts(&mountinfo)
            .into_iter()
            .rev()
            .find_map(|(id, at)| (at == mount_point).then_some(id))
            .context("the new mount is not in the mount table")?;
        let (program, args) = guard_command();
        let child = std::process::Command::new(program)
            .args(args)
            .env(GUARD_MOUNT_ID_VAR, id.to_string())
            .env(GUARD_MOUNT_POINT_VAR, mount_point)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .context("starting the crash guard")?;
        Ok(Self { child })
    }
}

impl Drop for CrashGuard {
    /// A clean release: close the pipe (the guard finds its mount gone — the
    /// caller unmounted first — and exits) and reap it.
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

/// The guard's program and arguments: the agent binary in guard mode. A test
/// binary has no such mode — it runs [`tests::guard_child_body`], which calls
/// the same [`run_guard_from_env`].
#[cfg(not(test))]
fn guard_command() -> (&'static Path, Vec<&'static str>) {
    (Path::new("/proc/self/exe"), vec![GUARD_ARG])
}

#[cfg(test)]
fn guard_command() -> (&'static Path, Vec<&'static str>) {
    (
        Path::new("/proc/self/exe"),
        vec![
            "fuse_host::tests::guard_child_body",
            "--exact",
            "--ignored",
            "--test-threads=1",
        ],
    )
}

/// The agent binary's guard mode ([`GUARD_ARG`]): read the guarded mount from
/// the environment and guard it ([`CrashGuard`]). Blocking; returns when the
/// agent is gone and the mount has been dealt with.
pub(crate) fn run_guard_from_env() -> Result<()> {
    close_inherited_descriptors();
    let id: u64 = std::env::var(GUARD_MOUNT_ID_VAR)
        .context("the guard was given no mount id")?
        .parse()
        .context("the guard's mount id")?;
    let mount_point = PathBuf::from(
        std::env::var_os(GUARD_MOUNT_POINT_VAR).context("the guard was given no mount point")?,
    );
    // Wait for the agent to go: EOF, or the pipe failing, both mean it has.
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 64];
    loop {
        match std::io::Read::read(&mut stdin, &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    if guard_should_unmount(&mountinfo, id, &mount_point, || probe_errno(&mount_point)) {
        std::process::Command::new(FUSERMOUNT)
            .arg("-uz")
            .arg(&mount_point)
            .status()
            .context("running the unmount helper")?;
    }
    Ok(())
}

/// The guard's one decision: unmount `mount_point` only if the mount it was
/// given is still in the table there under `id`, and `probe` says it is dead
/// (`ENOTCONN` — no server answers it). The probe runs only for a mount that
/// matched. Pure over the table, so the rule is pinned by a unit test.
fn guard_should_unmount(
    mountinfo: &str,
    id: u64,
    mount_point: &Path,
    probe: impl FnOnce() -> Option<i32>,
) -> bool {
    binding_mounts(mountinfo)
        .iter()
        .any(|(mid, at)| *mid == id && at == mount_point)
        && probe() == Some(libc::ENOTCONN)
}

/// What a `stat` of `path` answers as an errno (`Some(0)` = it answered), or
/// `None` if no answer came within [`GUARD_PROBE_TIMEOUT`] — a mount some
/// process still holds open without serving, which the guard leaves alone.
fn probe_errno(path: &Path) -> Option<i32> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let errno = match std::fs::metadata(&path) {
            Ok(_) => 0,
            Err(e) => e.raw_os_error().unwrap_or(-1),
        };
        let _ = tx.send(errno);
    });
    rx.recv_timeout(GUARD_PROBE_TIMEOUT).ok()
}

/// Close every descriptor above stderr this process inherited, so the guard can
/// never itself be what keeps a FUSE connection open (a descriptor of
/// `/dev/fuse` held here would keep a crashed agent's mount alive and
/// unanswered, instead of dead).
fn close_inherited_descriptors() {
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return;
    };
    let fds: Vec<i32> = entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd > 2)
        .collect();
    for fd in fds {
        // SAFETY: closing a descriptor number this process holds; the guard
        // owns no Rust object over any of them (it has opened nothing yet but
        // the `read_dir` above, whose descriptor is already closed — a stale
        // number answers `EBADF`, which is ignored).
        unsafe {
            libc::close(fd);
        }
    }
}

/// The mount points among `locations` that carry a mount of this binding, read
/// off a `/proc/self/mountinfo` text. Pure, so the field layout is pinned by a
/// unit test over captured lines.
///
/// A mountinfo line is `id parent major:minor root MOUNT_POINT options [optional
/// fields] - FSTYPE source super-options`; the mount point escapes space, tab,
/// newline and backslash as three-digit octal.
pub(crate) fn ghost_mount_points(mountinfo: &str, locations: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for (_, mount_point) in binding_mounts(mountinfo) {
        if locations.contains(&mount_point) && !found.contains(&mount_point) {
            found.push(mount_point);
        }
    }
    found
}

/// Every mount of this binding in a `/proc/self/mountinfo` text, as
/// `(mount id, mount point)`, in the table's order (a later line is a later
/// mount).
fn binding_mounts(mountinfo: &str) -> Vec<(u64, PathBuf)> {
    let mut found = Vec::new();
    for line in mountinfo.lines() {
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };
        if tail.split(' ').next() != Some(MOUNTINFO_FSTYPE) {
            continue;
        }
        let mut fields = head.split(' ');
        let Some(Ok(id)) = fields.next().map(str::parse::<u64>) else {
            continue;
        };
        let Some(raw) = fields.nth(3) else {
            continue;
        };
        found.push((id, PathBuf::from(OsString::from(unescape_mountinfo(raw)))));
    }
    found
}

/// Undo mountinfo's `\NNN` octal escapes.
fn unescape_mountinfo(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(code) = u8::from_str_radix(&raw[i + 1..i + 4], 8)
        {
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Boot reconcile, before any mount: lazily unmount every leftover mount of this
/// binding at a configured location — the transport-endpoint-not-connected ghost
/// an `auto_unmount` that could not run leaves behind. The linux twin of the
/// windows ghost-registration sweep. Returns how many it unmounted. Blocking (a
/// file read and one helper process per ghost).
pub(crate) fn sweep_ghost_mounts(locations: &[PathBuf]) -> usize {
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return 0;
    };
    let mut swept = 0;
    for mount_point in ghost_mount_points(&mountinfo, locations) {
        match std::process::Command::new(FUSERMOUNT)
            .arg("-uz")
            .arg(&mount_point)
            .status()
        {
            Ok(status) if status.success() => {
                tracing::info!(path = %mount_point.display(), "swept a leftover on-demand mount");
                swept += 1;
            }
            Ok(status) => tracing::warn!(
                path = %mount_point.display(),
                %status,
                "could not unmount a leftover on-demand mount"
            ),
            Err(e) => tracing::warn!(
                path = %mount_point.display(),
                error = %e,
                "could not run the unmount helper on a leftover on-demand mount"
            ),
        }
    }
    swept
}

/// Can this agent mount an on-demand root here? `Err` carries the reason code
/// the status reply reports (`fauna_ipc::sync::ON_DEMAND_REASON_*`): mounting
/// needs an openable `/dev/fuse` and the `fuse3` package's helper on `PATH`. A
/// sandbox that offers neither (Flatpak, Snap) lands here by itself.
pub(crate) fn probe_available() -> std::result::Result<(), &'static str> {
    if std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_err()
    {
        return Err(fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSE_DEVICE);
    }
    if !helper_on_path(std::env::var_os("PATH").as_deref()) {
        return Err(fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSERMOUNT);
    }
    Ok(())
}

/// Is the mount helper a file in one of `path`'s directories?
fn helper_on_path(path: Option<&OsStr>) -> bool {
    path.is_some_and(|path| std::env::split_paths(path).any(|dir| dir.join(FUSERMOUNT).is_file()))
}

// ---------------------------------------------------------------------------
// The binding's PlaceholderInvalidator
// ---------------------------------------------------------------------------

/// The FUSE root's [`PlaceholderInvalidator`] — what the shared loop asks of the OS
/// binding when bytes move (`on-demand-files.md` § Linux FUSE binding, the
/// dehydrate rule and *Placeholder detection needs no OS attribute*).
///
/// A placeholder here is a row, never a file, so this binding is **off-disk**
/// ([`PlaceholderInvalidator::placeholders_off_disk`]): the engine gates and
/// records a dehydrate itself before [`dehydrate`](PlaceholderInvalidator::dehydrate)
/// unlinks ([`crate::bridge::free_local_bytes`]), and pins live in the rows. Every
/// cfapi attribute call — the in-sync bit, the USN, the anchor — has nothing to act
/// on: no file here carries a placeholder attribute, and the view's state is read
/// from the rows on each request.
pub(crate) struct FuseInvalidator {
    /// The underlying directory (the reach) — what the loop's absolute paths are under.
    root: PathBuf,
    /// The root's own command channel: a pinned placeholder hydrates through the
    /// same [`HydrationCommand::Materialize`] an `open` sends.
    cmd_tx: UnboundedSender<HydrationCommand>,
}

impl FuseInvalidator {
    pub(crate) fn new(root: &Path, cmd_tx: UnboundedSender<HydrationCommand>) -> Self {
        Self {
            root: root.to_path_buf(),
            cmd_tx,
        }
    }

    /// `abs`'s path under the root, `/`-joined — `None` for a path outside it.
    fn rel_of(&self, abs: &Path) -> Option<String> {
        let rel = abs.strip_prefix(&self.root).ok()?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    }
}

impl crate::bridge::PlaceholderInvalidator for FuseInvalidator {
    /// The unlink — the second half of the off-disk dehydrate. Only ever reached
    /// through [`crate::bridge::free_local_bytes`], which has already proven the
    /// free lossless and flipped the row: this binding has no OS refusal of its own,
    /// so the gate is the engine's.
    fn dehydrate(&self, abs_path: &Path) -> Result<()> {
        std::fs::remove_file(abs_path)
            .with_context(|| format!("unlinking freed bytes at {}", abs_path.display()))
    }

    /// The unlink, as [`Self::dehydrate`]: a placeholder here is its row, whose size and
    /// mtime the re-point that follows moves — the view reads both from it on every
    /// `getattr`, so there is nothing on the disk left to describe the old version.
    fn supersede(&self, abs_path: &Path, _size: u64, _mtime: i64) -> Result<()> {
        self.dehydrate(abs_path)
    }

    /// Pins live in the rows here, and the off-disk pin sweep reads them there; no
    /// file on the disk carries a pin to classify.
    fn pin_action(&self, _abs_path: &Path) -> Option<crate::pin_reaction::PinAction> {
        None
    }

    /// No in-sync bit to set: the view's state is the row's.
    fn set_in_sync(&self, _abs_path: &Path, _usn: i64) -> Result<()> {
        Ok(())
    }

    /// No update sequence number either; [`Self::set_in_sync`] conditions on nothing.
    fn usn(&self, _abs_path: &Path) -> Result<i64> {
        Ok(0)
    }

    /// Ask the loop to hydrate `abs_path` — the [`HydrationCommand::Materialize`]
    /// an `open` sends, with nobody waiting on the answer. Completion is the
    /// loop's (`mark_hydrated` + the `Synced` event); a duplicate kick shares the
    /// running download or finds the file already hydrated.
    fn kick_hydrate(&self, abs_path: &Path) {
        let Some(rel) = self.rel_of(abs_path) else {
            return;
        };
        let (reply, _nobody_waits) = std::sync::mpsc::sync_channel(1);
        let _ = self
            .cmd_tx
            .send(HydrationCommand::Materialize { rel, reply });
    }

    /// Nothing here is ever a platform "cloud file" needing an anchor: an ordinary
    /// file IS this binding's hydrated file. Answering `true` keeps the shared
    /// in-sync road from trying to convert it.
    fn is_cloud_file(&self, _abs_path: &Path) -> bool {
        true
    }

    /// No anchor to write (see [`Self::is_cloud_file`]).
    fn anchor(&self, _abs_path: &Path, _rel: &str) -> Result<()> {
        Ok(())
    }

    /// No folder ✅ to set: a directory's state is read from its rows.
    fn convert_dir_in_sync(&self, _abs_path: &Path, _rel: &str) -> Result<()> {
        Ok(())
    }

    /// The view lists every directory from the rows at each `readdir`, so nothing
    /// is ever pushed into one ([`crate::bridge::materialize_created`] stays idle).
    fn is_listed_dir(&self, _abs_dir: &Path) -> bool {
        false
    }

    /// Never reached ([`Self::is_listed_dir`] is always `false`): a new row is
    /// listed by the next `readdir`, not placed on the disk.
    fn create_placeholder(
        &self,
        _parent_abs: &Path,
        rel: &str,
        _size: u64,
        _mtime: i64,
        _is_dir: bool,
    ) -> Result<()> {
        bail!("a FUSE root places no placeholder on its disk ({rel})")
    }

    fn placeholders_off_disk(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// The filesystem
// ---------------------------------------------------------------------------

/// Inode numbers, handed out in first-seen order and stable for the mount's
/// lifetime — which is all the protocol asks. Keyed by the folder-relative path
/// (forward-slash; `""` is the root), so a name keeps its number whether it is a
/// placeholder or a hydrated file, across the hydration that turns one into the
/// other.
struct Inodes {
    by_ino: HashMap<u64, String>,
    by_rel: HashMap<String, u64>,
    next: u64,
}

impl Inodes {
    fn new() -> Self {
        Self {
            by_ino: HashMap::from([(ROOT_INO, String::new())]),
            by_rel: HashMap::from([(String::new(), ROOT_INO)]),
            next: ROOT_INO + 1,
        }
    }

    fn rel(&self, ino: u64) -> Option<String> {
        self.by_ino.get(&ino).cloned()
    }

    fn ino(&mut self, rel: &str) -> u64 {
        if let Some(ino) = self.by_rel.get(rel) {
            return *ino;
        }
        let ino = self.next;
        self.next += 1;
        self.by_ino.insert(ino, rel.to_string());
        self.by_rel.insert(rel.to_string(), ino);
        ino
    }

    /// A rename moved `from` (and, for a directory, everything under it) to
    /// `to`: the kernel keeps each moved inode's number under its new name, so
    /// the numbers follow the names. Whatever `to` named before is replaced.
    fn rename(&mut self, from: &str, to: &str) {
        if let Some(old) = self.by_rel.remove(to) {
            self.by_ino.remove(&old);
        }
        let under = format!("{from}/");
        let moved: Vec<(String, u64)> = self
            .by_rel
            .iter()
            .filter(|(rel, _)| rel.as_str() == from || rel.starts_with(&under))
            .map(|(rel, ino)| (rel.clone(), *ino))
            .collect();
        for (rel, ino) in moved {
            let renamed = format!("{to}{}", &rel[from.len()..]);
            self.by_rel.remove(&rel);
            self.by_rel.insert(renamed.clone(), ino);
            self.by_ino.insert(ino, renamed);
        }
    }
}

/// One entry of a directory listing, as `readdir` replies it.
struct Listed {
    ino: u64,
    kind: FileType,
    name: OsString,
}

/// Delivers one directory's placeholder children to the waiting request — and
/// reports **nothing placed**, because nothing is: a FUSE listing puts no file
/// on the disk, and the engine's seen mark rests on exactly the set a sink
/// vouches for ([`PlaceholderSink::transfer_placeholders`]).
struct ListingSink(std::sync::mpsc::SyncSender<Vec<DirChild>>);

impl PlaceholderSink for ListingSink {
    fn transfer_placeholders(&self, children: &[DirChild]) -> Result<Vec<String>> {
        // A receiver that gave up (the mount is closing) is not this call's error.
        let _ = self.0.try_send(children.to_vec());
        Ok(Vec::new())
    }
}

/// The filesystem the session serves. A handle on [`Root`], so a request that
/// waits on the driving loop (a hydration, a placeholder's unlink) can finish on
/// a thread of its own while the session thread goes on serving.
struct FuseRoot(Arc<Root>);

struct Root {
    /// The underlying directory ([`Reach::path`]).
    reach: PathBuf,
    cmd_tx: UnboundedSender<HydrationCommand>,
    closing: Arc<AtomicBool>,
    /// Owner of the bound directory — reported as the owner of every placeholder.
    uid: u32,
    gid: u32,
    /// Requests reach `&self` from several threads; everything mutable sits here.
    state: Mutex<State>,
}

/// [`Root`]'s mutable half.
struct State {
    inodes: Inodes,
    /// Placeholder children per directory rel, with when they were fetched and
    /// the underlying directory's mtime at that moment ([`Root::dir_stamp`]).
    listings: HashMap<String, (Instant, Option<SystemTime>, Vec<DirChild>)>,
    /// A directory handle's listing, snapshotted at `opendir` so the offsets
    /// `readdir` is resumed with stay meaningful across calls.
    open_dirs: HashMap<u64, Vec<Listed>>,
    /// An open file's descriptor on the UNDERLYING file — every read and write
    /// of a materialized file passes through to it.
    open_files: HashMap<u64, Arc<std::fs::File>>,
    next_fh: u64,
}

fn child_rel(parent_rel: &str, name: &str) -> String {
    if parent_rel.is_empty() {
        name.to_string()
    } else {
        format!("{parent_rel}/{name}")
    }
}

fn split_rel(rel: &str) -> (&str, &str) {
    rel.rsplit_once('/').unwrap_or(("", rel))
}

fn secs(t: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(t.max(0) as u64)
}

fn kind_of(meta: &std::fs::Metadata) -> FileType {
    let ft = meta.file_type();
    if ft.is_dir() {
        FileType::Directory
    } else if ft.is_symlink() {
        FileType::Symlink
    } else {
        FileType::RegularFile
    }
}

/// Did this request come from the agent's own process?
///
/// The agent must never wait on its own driving loop through the view — the
/// loop thread is the one that would have to answer. Nothing in the agent reads
/// the mount on purpose (the module rule), but a path resolution of the reach
/// can: `canonicalize` of `/proc/self/fd/<fd>/x` resolves the link to the mount
/// point's text and then looks `x` up THROUGH the view. So a request from this
/// process is answered from the underlying directory alone — no placeholder
/// listing, no hydration — which is exactly the tree the agent's own reads are
/// about. The kernel reports the requesting THREAD's id, which is a task of this
/// process exactly when `/proc/self/task/<id>` exists (procfs — never the view).
fn from_this_process(req: &Request) -> bool {
    Path::new("/proc/self/task")
        .join(req.pid().to_string())
        .exists()
}

/// The `errno` an I/O error carries, `EIO` when it carries none.
fn io_errno(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

fn errno(raw: i32) -> Errno {
    Errno::from_i32(raw)
}

/// What a `setattr` asks for, carried to a hydration's waiting thread when the
/// target is a placeholder.
struct AttrChange {
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    size: Option<u64>,
    atime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    fh: Option<u64>,
}

fn time_of(t: TimeOrNow) -> SystemTime {
    match t {
        TimeOrNow::SpecificTime(t) => t,
        TimeOrNow::Now => SystemTime::now(),
    }
}

impl Root {
    /// The mutable half. Never held across a wait on the driving loop.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn rel_of(&self, ino: INodeNo) -> Option<String> {
        self.state().inodes.rel(ino.0)
    }

    fn underlying(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.reach.clone()
        } else {
            self.reach.join(rel)
        }
    }

    /// The attributes of an entry the underlying directory holds.
    fn attr_of_underlying(ino: u64, meta: &std::fs::Metadata) -> FileAttr {
        FileAttr {
            ino: INodeNo(ino),
            size: meta.size(),
            blocks: meta.blocks(),
            atime: secs(meta.atime()),
            mtime: secs(meta.mtime()),
            ctime: secs(meta.ctime()),
            crtime: secs(meta.ctime()),
            kind: kind_of(meta),
            perm: (meta.mode() & 0o7777) as u16,
            nlink: meta.nlink() as u32,
            uid: meta.uid(),
            gid: meta.gid(),
            rdev: meta.rdev() as u32,
            blksize: meta.blksize() as u32,
            flags: 0,
        }
    }

    /// The attributes of a placeholder: the row's size and mtime, no blocks (its
    /// bytes are not here), owned by the bound directory's owner.
    fn attr_of_placeholder(&self, ino: u64, child: &DirChild) -> FileAttr {
        let mtime = secs(child.mtime);
        FileAttr {
            ino: INodeNo(ino),
            size: child.size,
            blocks: 0,
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: mtime,
            kind: if child.is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if child.is_dir { 0o755 } else { 0o644 },
            nlink: if child.is_dir { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// Wait for the driving loop's answer, giving up when the mount is closing
    /// (its drop joins the session thread) or the loop has ended.
    fn wait_on_loop<T>(&self, rx: &std::sync::mpsc::Receiver<T>) -> Option<T> {
        loop {
            match rx.recv_timeout(LOOP_POLL) {
                Ok(answer) => return Some(answer),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self.closing.load(Ordering::SeqCst) {
                        return None;
                    }
                }
                // The command was dropped unserved: the loop has ended.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// `parent_rel`'s placeholder children, from the driving loop
    /// ([`HydrationCommand::Populate`] → `serve_populate`). `None` when the loop
    /// is gone or the mount is closing. Empty for a request of the agent's own
    /// ([`from_this_process`]): it sees the underlying directory only.
    fn placeholder_children(&self, parent_rel: &str, own: bool) -> Option<Vec<DirChild>> {
        if own {
            return Some(Vec::new());
        }
        // A listing is fresh only while the underlying directory is unchanged: a
        // dehydrate the loop made (an unlink under the mount, never through this
        // view) turns a hydrated file into a placeholder child, and a listing
        // cached before it would drop that name from the view until it expired.
        let stamp = self.dir_stamp(parent_rel);
        if let Some((at, cached_stamp, children)) = self.state().listings.get(parent_rel)
            && at.elapsed() < LISTING_TTL
            && *cached_stamp == stamp
        {
            return Some(children.clone());
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.cmd_tx
            .send(HydrationCommand::Populate {
                parent_rel: parent_rel.to_string(),
                sink: Box::new(ListingSink(tx)),
            })
            .ok()?;
        let children = self.wait_on_loop(&rx)?;
        self.state().listings.insert(
            parent_rel.to_string(),
            (Instant::now(), stamp, children.clone()),
        );
        Some(children)
    }

    /// The underlying directory `rel`'s mtime — what tells a cached listing that
    /// the directory changed under it. `None` for one not on the disk (a
    /// directory that exists only as rows), which no unlink can change.
    fn dir_stamp(&self, rel: &str) -> Option<SystemTime> {
        std::fs::metadata(self.underlying(rel))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Drop the cached listing of `rel`'s parent — its children just changed.
    fn forget_parent_listing(&self, rel: &str) {
        let (parent_rel, _) = split_rel(rel);
        self.state().listings.remove(parent_rel);
    }

    /// The attributes of `rel`, whichever half of the union holds it: an
    /// underlying entry wins, a placeholder row answers otherwise.
    fn attr_of(&self, rel: &str, own: bool) -> std::result::Result<FileAttr, i32> {
        // The root is stat'ed THROUGH the reach (which is itself a link in
        // `/proc`); everything under it is stat'ed as the entry it is.
        let stat = if rel.is_empty() {
            std::fs::metadata(&self.reach)
        } else {
            std::fs::symlink_metadata(self.underlying(rel))
        };
        match stat {
            Ok(meta) => {
                let ino = self.state().inodes.ino(rel);
                return Ok(Self::attr_of_underlying(ino, &meta));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_errno(&e)),
        }
        if rel.is_empty() {
            return Err(libc::ENOENT);
        }
        let (parent_rel, name) = split_rel(rel);
        let children = self
            .placeholder_children(parent_rel, own)
            .ok_or(libc::EIO)?;
        let child = children
            .iter()
            .find(|c| c.name == name)
            .ok_or(libc::ENOENT)?;
        let ino = self.state().inodes.ino(rel);
        Ok(self.attr_of_placeholder(ino, child))
    }

    /// The union listing of directory `rel`: the underlying entries, then every
    /// placeholder child whose name the underlying directory does not hold. A
    /// hydration's temp file is the root's own business and is not listed.
    fn list(&self, ino: u64, rel: &str, own: bool) -> std::result::Result<Vec<Listed>, i32> {
        let (parent_rel, _) = split_rel(rel);
        let parent_ino = self.state().inodes.ino(parent_rel);
        let mut listed = vec![
            Listed {
                ino,
                kind: FileType::Directory,
                name: OsString::from("."),
            },
            Listed {
                ino: parent_ino,
                kind: FileType::Directory,
                name: OsString::from(".."),
            },
        ];
        let mut names = std::collections::HashSet::new();
        // A directory that exists only as placeholder rows has no underlying half.
        match std::fs::read_dir(self.underlying(rel)) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    // A name this binding cannot key (not UTF-8) is passed over:
                    // the engine does not track it either.
                    let Some(name_str) = name.to_str() else {
                        continue;
                    };
                    if name_str.starts_with(HYDRATING_PREFIX) {
                        continue;
                    }
                    let kind = match entry.file_type() {
                        Ok(ft) if ft.is_dir() => FileType::Directory,
                        Ok(ft) if ft.is_symlink() => FileType::Symlink,
                        _ => FileType::RegularFile,
                    };
                    let ino = self.state().inodes.ino(&child_rel(rel, name_str));
                    names.insert(name_str.to_string());
                    listed.push(Listed { ino, kind, name });
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_errno(&e)),
        }
        for child in self.placeholder_children(rel, own).ok_or(libc::EIO)? {
            if names.contains(&child.name) {
                continue;
            }
            let ino = self.state().inodes.ino(&child_rel(rel, &child.name));
            listed.push(Listed {
                ino,
                kind: if child.is_dir {
                    FileType::Directory
                } else {
                    FileType::RegularFile
                },
                name: OsString::from(child.name),
            });
        }
        // Stable order after the two dot entries, so offsets mean the same thing
        // to every reader of this snapshot.
        listed[2..].sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Ok(listed)
    }

    /// Hold `file` open under a fresh handle.
    fn hold_file(&self, file: std::fs::File) -> u64 {
        let mut state = self.state();
        let fh = state.next_fh;
        state.next_fh += 1;
        state.open_files.insert(fh, Arc::new(file));
        fh
    }

    fn file(&self, fh: FileHandle) -> Option<Arc<std::fs::File>> {
        self.state().open_files.get(&fh.0).cloned()
    }

    /// Open the UNDERLYING file at `rel` the way the kernel's `open(2)` flags
    /// ask, never following a final symlink.
    fn open_underlying(&self, rel: &str, flags: i32) -> std::io::Result<std::fs::File> {
        let mut options = open_options(flags);
        options.custom_flags(libc::O_NOFOLLOW);
        options.open(self.underlying(rel))
    }

    fn reply_open(&self, rel: &str, flags: i32, reply: ReplyOpen) {
        match self.open_underlying(rel, flags) {
            Ok(file) => reply.opened(FileHandle(self.hold_file(file)), FopenFlags::empty()),
            Err(e) => reply.error(errno(io_errno(&e))),
        }
    }

    /// Apply a `setattr` to the underlying entry at `rel` and answer with its
    /// attributes afterwards.
    fn apply_setattr(&self, rel: &str, change: &AttrChange) -> std::result::Result<FileAttr, i32> {
        let path = self.underlying(rel);
        let meta = std::fs::symlink_metadata(&path).map_err(|e| io_errno(&e))?;
        // Ownership is not this filesystem's to change: the files are the
        // bound directory's owner's, and the agent never runs as root.
        if change.uid.is_some_and(|uid| uid != meta.uid())
            || change.gid.is_some_and(|gid| gid != meta.gid())
        {
            return Err(libc::EPERM);
        }
        if let Some(mode) = change.mode {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|e| io_errno(&e))?;
        }
        if let Some(size) = change.size {
            let held = change.fh.and_then(|fh| self.file(FileHandle(fh)));
            let result = match held {
                Some(file) => file.set_len(size),
                None => std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .and_then(|file| file.set_len(size)),
            };
            result.map_err(|e| io_errno(&e))?;
        }
        if change.atime.is_some() || change.mtime.is_some() {
            let mut times = std::fs::FileTimes::new();
            if let Some(atime) = change.atime {
                times = times.set_accessed(atime);
            }
            if let Some(mtime) = change.mtime {
                times = times.set_modified(mtime);
            }
            // `futimens` on a read-only descriptor: setting times is an
            // ownership check, not a write.
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .and_then(|file| file.set_times(times))
                .map_err(|e| io_errno(&e))?;
        }
        self.attr_of(rel, false)
    }

    /// Rename underlying `from` to `to`, then let the inode table and the cached
    /// listings follow.
    fn rename_underlying(&self, from: &str, to: &str) -> std::result::Result<(), i32> {
        let dest = self.underlying(to);
        // A destination under a directory that exists only as rows: it becomes
        // real with its first entry.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_errno(&e))?;
        }
        std::fs::rename(self.underlying(from), &dest).map_err(|e| io_errno(&e))?;
        self.state().inodes.rename(from, to);
        self.forget_parent_listing(from);
        self.forget_parent_listing(to);
        Ok(())
    }

    /// Does the view hold `rel` — an underlying entry or a placeholder row?
    fn in_view(&self, rel: &str, own: bool) -> std::result::Result<bool, i32> {
        match self.attr_of(rel, own) {
            Ok(_) => Ok(true),
            Err(libc::ENOENT) => Ok(false),
            Err(raw) => Err(raw),
        }
    }
}

/// The [`std::fs::OpenOptions`] an `open(2)` flag word asks for: the access mode,
/// append and truncate. Creation is `create`'s own request, never an open's.
fn open_options(flags: i32) -> std::fs::OpenOptions {
    let access = flags & libc::O_ACCMODE;
    let write = access == libc::O_WRONLY || access == libc::O_RDWR;
    let mut options = std::fs::OpenOptions::new();
    options
        .read(access != libc::O_WRONLY)
        .write(write)
        .append(write && flags & libc::O_APPEND != 0)
        .truncate(write && flags & libc::O_TRUNC != 0 && flags & libc::O_APPEND == 0);
    options
}

impl FuseRoot {
    /// Hand `rel` to the driving loop as `command`, and finish the request on a
    /// thread of its own once the loop answers — so a slow fetch never holds the
    /// session thread, which goes on serving listings and pass-through I/O.
    /// `then` receives the loop's answer, or an error when the mount closed or
    /// the loop ended first.
    fn after_loop(
        &self,
        command: impl FnOnce(LoopReply) -> HydrationCommand,
        then: impl FnOnce(&Root, std::result::Result<(), String>) + Send + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if self.0.cmd_tx.send(command(tx)).is_err() {
            return then(&self.0, Err("the on-demand root's loop has ended".into()));
        }
        let root = Arc::clone(&self.0);
        let spawned = std::thread::Builder::new()
            .name("fauna-fuse-wait".into())
            .spawn(move || {
                let outcome = root
                    .wait_on_loop(&rx)
                    .unwrap_or_else(|| Err("the on-demand root is closing".into()));
                then(&root, outcome);
            });
        if let Err(e) = spawned {
            // The reply moved into the closure answers `EIO` on its drop.
            tracing::error!(error = %e, "could not start a thread to finish a FUSE request");
        }
    }
}

impl Filesystem for FuseRoot {
    fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(parent_rel) = self.0.rel_of(parent) else {
            return reply.error(Errno::ENOENT);
        };
        let Some(name) = name.to_str() else {
            return reply.error(Errno::ENOENT);
        };
        match self
            .0
            .attr_of(&child_rel(&parent_rel, name), from_this_process(req))
        {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(raw) => reply.error(errno(raw)),
        }
    }

    fn getattr(&self, req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let Some(rel) = self.0.rel_of(ino) else {
            return reply.error(Errno::ENOENT);
        };
        match self.0.attr_of(&rel, from_this_process(req)) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(raw) => reply.error(errno(raw)),
        }
    }

    fn opendir(&self, req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let Some(rel) = self.0.rel_of(ino) else {
            return reply.error(Errno::ENOENT);
        };
        match self.0.list(ino.0, &rel, from_this_process(req)) {
            Ok(listed) => {
                let fh = {
                    let mut state = self.0.state();
                    let fh = state.next_fh;
                    state.next_fh += 1;
                    state.open_dirs.insert(fh, listed);
                    fh
                };
                reply.opened(FileHandle(fh), FopenFlags::empty());
            }
            Err(raw) => reply.error(errno(raw)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let state = self.0.state();
        let Some(listed) = state.open_dirs.get(&fh.0) else {
            return reply.error(Errno::ENOTDIR);
        };
        let Ok(start) = usize::try_from(offset) else {
            return reply.error(Errno::EINVAL);
        };
        for (i, entry) in listed.iter().enumerate().skip(start) {
            // The offset an entry carries is where the NEXT read resumes.
            if reply.add(INodeNo(entry.ino), (i + 1) as u64, entry.kind, &entry.name) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        self.0.state().open_dirs.remove(&fh.0);
        reply.ok();
    }

    /// Open a file. A materialized one opens its underlying file; a placeholder
    /// hydrates first (`HydrationCommand::Materialize`), and the open completes
    /// once its bytes are on the disk — whatever the flags, a truncating or
    /// write-only open included: simplest correct, since the row's identity
    /// must be the served bytes' before any write lands on them.
    fn open(&self, req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let Some(rel) = self.0.rel_of(ino) else {
            return reply.error(Errno::ENOENT);
        };
        let flags = flags.0;
        match std::fs::symlink_metadata(self.0.underlying(&rel)) {
            Ok(_) => return self.0.reply_open(&rel, flags, reply),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return reply.error(errno(io_errno(&e))),
        }
        // The agent never hydrates through its own view (`from_this_process`).
        if from_this_process(req) {
            return reply.error(Errno::ENOENT);
        }
        let command_rel = rel.clone();
        self.after_loop(
            move |reply| HydrationCommand::Materialize {
                rel: command_rel,
                reply,
            },
            move |root, outcome| match outcome {
                Ok(()) => root.reply_open(&rel, flags, reply),
                Err(_) => reply.error(Errno::EIO),
            },
        );
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let Some(file) = self.0.file(fh) else {
            return reply.error(Errno::EBADF);
        };
        let mut buf = vec![0u8; size as usize];
        let mut filled = 0;
        while filled < buf.len() {
            match file.read_at(&mut buf[filled..], offset + filled as u64) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return reply.error(errno(io_errno(&e))),
            }
        }
        reply.data(&buf[..filled]);
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let Some(file) = self.0.file(fh) else {
            return reply.error(Errno::EBADF);
        };
        match file.write_all_at(data, offset) {
            Ok(()) => reply.written(data.len() as u32),
            Err(e) => reply.error(errno(io_errno(&e))),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        // Every write went straight to the underlying file; there is no buffer
        // of this filesystem's own to flush.
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        let Some(file) = self.0.file(fh) else {
            return reply.error(Errno::EBADF);
        };
        let synced = if datasync {
            file.sync_data()
        } else {
            file.sync_all()
        };
        match synced {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(io_errno(&e))),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.0.state().open_files.remove(&fh.0);
        reply.ok();
    }

    /// Change attributes. On a placeholder the file hydrates first, so a
    /// truncate or a `touch` lands on the served bytes and is uploaded as the
    /// edit it is.
    fn setattr(
        &self,
        req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let Some(rel) = self.0.rel_of(ino) else {
            return reply.error(Errno::ENOENT);
        };
        let change = AttrChange {
            mode,
            uid,
            gid,
            size,
            atime: atime.map(time_of),
            mtime: mtime.map(time_of),
            fh: fh.map(|fh| fh.0),
        };
        match std::fs::symlink_metadata(self.0.underlying(&rel)) {
            Ok(_) => {
                return match self.0.apply_setattr(&rel, &change) {
                    Ok(attr) => reply.attr(&TTL, &attr),
                    Err(raw) => reply.error(errno(raw)),
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return reply.error(errno(io_errno(&e))),
        }
        if from_this_process(req) {
            return reply.error(Errno::ENOENT);
        }
        let command_rel = rel.clone();
        self.after_loop(
            move |reply| HydrationCommand::Materialize {
                rel: command_rel,
                reply,
            },
            move |root, outcome| match outcome
                .map_err(|_| libc::EIO)
                .and_then(|()| root.apply_setattr(&rel, &change))
            {
                Ok(attr) => reply.attr(&TTL, &attr),
                Err(raw) => reply.error(errno(raw)),
            },
        );
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let Some(rel) = self.0.rel_of(ino) else {
            return reply.error(Errno::ENOENT);
        };
        match std::fs::read_link(self.0.underlying(&rel)) {
            Ok(target) => reply.data(target.as_os_str().as_bytes()),
            Err(e) => reply.error(errno(io_errno(&e))),
        }
    }

    /// A new file — always an underlying one, which the watcher then adopts.
    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let (Some(parent_rel), Some(name)) = (self.0.rel_of(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let rel = child_rel(&parent_rel, name);
        let path = self.0.underlying(&rel);
        // A directory that exists only as rows becomes real with its first file.
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            return reply.error(errno(io_errno(&e)));
        }
        let mut options = open_options(flags);
        options
            .read(true)
            .write(true)
            .create_new(true)
            .mode(mode & !umask & 0o7777)
            .custom_flags(libc::O_NOFOLLOW);
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(e) => return reply.error(errno(io_errno(&e))),
        };
        self.0.forget_parent_listing(&rel);
        match self.0.attr_of(&rel, false) {
            Ok(attr) => {
                let fh = self.0.hold_file(file);
                reply.created(
                    &TTL,
                    &attr,
                    Generation(0),
                    FileHandle(fh),
                    FopenFlags::empty(),
                );
            }
            Err(raw) => reply.error(errno(raw)),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let (Some(parent_rel), Some(name)) = (self.0.rel_of(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let rel = child_rel(&parent_rel, name);
        let path = self.0.underlying(&rel);
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            return reply.error(errno(io_errno(&e)));
        }
        if let Err(e) = std::fs::DirBuilder::new()
            .mode(mode & !umask & 0o7777)
            .create(&path)
        {
            return reply.error(errno(io_errno(&e)));
        }
        self.0.forget_parent_listing(&rel);
        match self.0.attr_of(&rel, false) {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(raw) => reply.error(errno(raw)),
        }
    }

    /// Unlink. An underlying file is removed, and the watcher's `Remove` drives
    /// the delete as on any resident root. A placeholder was never on the disk,
    /// so no watcher can see it go: the driving loop records its delete
    /// (`HydrationCommand::Unlink` → `handle_delete`) — the ONE way a
    /// placeholder is deleted on this root.
    fn unlink(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let (Some(parent_rel), Some(name)) = (self.0.rel_of(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let rel = child_rel(&parent_rel, name);
        let path = self.0.underlying(&rel);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => return reply.error(Errno::EISDIR),
            Ok(_) => {
                return match std::fs::remove_file(&path) {
                    Ok(()) => {
                        self.0.forget_parent_listing(&rel);
                        reply.ok()
                    }
                    Err(e) => reply.error(errno(io_errno(&e))),
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return reply.error(errno(io_errno(&e))),
        }
        if from_this_process(req) {
            return reply.error(Errno::ENOENT);
        }
        let command_rel = rel.clone();
        self.after_loop(
            move |reply| HydrationCommand::Unlink {
                rel: command_rel,
                reply,
            },
            move |root, outcome| {
                root.forget_parent_listing(&rel);
                match outcome {
                    Ok(()) => reply.ok(),
                    Err(_) => reply.error(Errno::EIO),
                }
            },
        );
    }

    /// Remove a directory — refused while placeholder rows remain under it,
    /// since those are files the user has not deleted.
    fn rmdir(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let (Some(parent_rel), Some(name)) = (self.0.rel_of(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let rel = child_rel(&parent_rel, name);
        match self.0.placeholder_children(&rel, from_this_process(req)) {
            Some(children) if !children.is_empty() => return reply.error(Errno::ENOTEMPTY),
            Some(_) => {}
            None => return reply.error(Errno::EIO),
        }
        match std::fs::remove_dir(self.0.underlying(&rel)) {
            Ok(()) => {
                self.0.forget_parent_listing(&rel);
                reply.ok()
            }
            Err(e) => reply.error(errno(io_errno(&e))),
        }
    }

    /// Rename. A placeholder hydrates first (apple's rule), then moves as the
    /// file it now is. A directory still holding placeholder rows answers
    /// `EXDEV`, so `mv` copies it instead — a whole-tree move of rows the disk
    /// does not hold is not a rename this binding can make atomically.
    fn rename(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let (Some(parent_rel), Some(name), Some(newparent_rel), Some(newname)) = (
            self.0.rel_of(parent),
            name.to_str(),
            self.0.rel_of(newparent),
            newname.to_str(),
        ) else {
            return reply.error(Errno::ENOENT);
        };
        let own = from_this_process(req);
        let flags = flags.bits();
        if flags & libc::RENAME_EXCHANGE != 0 {
            return reply.error(Errno::EINVAL);
        }
        let from = child_rel(&parent_rel, name);
        let to = child_rel(&newparent_rel, newname);
        if flags & libc::RENAME_NOREPLACE != 0 {
            match self.0.in_view(&to, own) {
                Ok(true) => return reply.error(Errno::EEXIST),
                Ok(false) => {}
                Err(raw) => return reply.error(errno(raw)),
            }
        }
        // A destination that is a directory of placeholder rows is not empty.
        match self.0.placeholder_children(&to, own) {
            Some(children) if !children.is_empty() => return reply.error(Errno::ENOTEMPTY),
            Some(_) => {}
            None => return reply.error(Errno::EIO),
        }
        let source = match self.0.attr_of(&from, own) {
            Ok(attr) => attr,
            Err(raw) => return reply.error(errno(raw)),
        };
        let underlying = std::fs::symlink_metadata(self.0.underlying(&from)).is_ok();
        if source.kind == FileType::Directory {
            let holds_rows = match self.0.placeholder_children(&from, own) {
                Some(children) => !children.is_empty(),
                None => return reply.error(Errno::EIO),
            };
            if holds_rows || !underlying {
                return reply.error(errno(libc::EXDEV));
            }
        }
        if underlying {
            return match self.0.rename_underlying(&from, &to) {
                Ok(()) => reply.ok(),
                Err(raw) => reply.error(errno(raw)),
            };
        }
        if own {
            return reply.error(Errno::ENOENT);
        }
        let command_rel = from.clone();
        self.after_loop(
            move |reply| HydrationCommand::Materialize {
                rel: command_rel,
                reply,
            },
            move |root, outcome| match outcome
                .map_err(|_| libc::EIO)
                .and_then(|()| root.rename_underlying(&from, &to))
            {
                Ok(()) => reply.ok(),
                Err(raw) => reply.error(errno(raw)),
            },
        );
    }

    /// Free space is the underlying filesystem's — the mount holds no bytes of
    /// its own, so a file manager's free-space check before a copy is answered
    /// with the truth.
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let dir = match std::fs::File::open(&self.0.reach) {
            Ok(dir) => dir,
            Err(e) => return reply.error(errno(io_errno(&e))),
        };
        // SAFETY: `statvfs` is plain old data, fully written by a successful
        // `fstatvfs` on a descriptor this function owns for the call.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(dir.as_raw_fd(), &mut st) } != 0 {
            return reply.error(errno(io_errno(&std::io::Error::last_os_error())));
        }
        reply.statfs(
            st.f_blocks,
            st.f_bfree,
            st.f_bavail,
            st.f_files,
            st.f_ffree,
            st.f_bsize as u32,
            st.f_namemax as u32,
            st.f_frsize as u32,
        );
    }

    // Links and special files are refused in v1 (§ Linux FUSE binding, *Hydrate
    // on open*): the engine syncs regular files, and a link the nest cannot
    // carry would be a file that silently never syncs.
    fn symlink(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _link_name: &OsStr,
        _target: &Path,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EPERM);
    }

    fn link(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _newparent: INodeNo,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EPERM);
    }

    fn mknod(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        reply.error(Errno::EPERM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from the linux dev machine with a mount of this binding up: the
    /// subtype lands in the fstype field as `fuse.<subtype>`, and `fsname` in the
    /// source field after it.
    const MOUNTINFO: &str = "\
29 1 252:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw
812 29 0:71 / /home/u/Synced rw,nosuid,nodev,relatime shared:440 - fuse.fauna fauna-sync-agent rw,user_id=1000,group_id=1000,default_permissions
813 29 0:72 / /home/u/With\\040Space rw,nosuid,nodev,relatime - fuse.fauna fauna-sync-agent rw,user_id=1000,group_id=1000
814 29 0:73 / /home/u/other rw,nosuid,nodev,relatime - fuse.sshfs host:/x rw,user_id=1000,group_id=1000
815 29 0:74 / /home/u/unconfigured rw,nosuid,nodev,relatime - fuse.fauna fauna-sync-agent rw,user_id=1000,group_id=1000
";

    #[test]
    fn ghost_mounts_are_this_bindings_mounts_at_configured_locations() {
        let configured = [
            PathBuf::from("/home/u/Synced"),
            PathBuf::from("/home/u/With Space"),
            // Somebody else's FUSE mount at a configured location is not ours to
            // unmount.
            PathBuf::from("/home/u/other"),
            PathBuf::from("/home/u/not-mounted"),
        ];
        assert_eq!(
            ghost_mount_points(MOUNTINFO, &configured),
            vec![
                PathBuf::from("/home/u/Synced"),
                PathBuf::from("/home/u/With Space")
            ],
        );
    }

    #[test]
    fn a_mount_of_this_binding_elsewhere_is_left_alone() {
        assert!(ghost_mount_points(MOUNTINFO, &[PathBuf::from("/home/u")]).is_empty());
    }

    #[test]
    fn the_binding_mounts_carry_their_ids() {
        assert_eq!(
            binding_mounts(MOUNTINFO),
            vec![
                (812, PathBuf::from("/home/u/Synced")),
                (813, PathBuf::from("/home/u/With Space")),
                (815, PathBuf::from("/home/u/unconfigured")),
            ]
        );
    }

    /// The guard unmounts its own mount only when it is dead; a live mount under
    /// the same id (a successor that inherited a recycled id), another id at the
    /// same path, its id at another path, and a probe that never answered are
    /// all left alone — and the probe is not even asked unless the mount matched.
    #[test]
    fn the_guard_unmounts_only_its_own_dead_mount() {
        let synced = Path::new("/home/u/Synced");
        let dead = || Some(libc::ENOTCONN);
        assert!(guard_should_unmount(MOUNTINFO, 812, synced, dead));
        assert!(!guard_should_unmount(MOUNTINFO, 812, synced, || Some(0)));
        assert!(!guard_should_unmount(MOUNTINFO, 812, synced, || None));
        assert!(!guard_should_unmount(MOUNTINFO, 999, synced, || {
            panic!("probed a mount that is not the guard's")
        }));
        assert!(!guard_should_unmount(
            MOUNTINFO,
            812,
            Path::new("/home/u/With Space"),
            dead
        ));
        // Somebody else's FUSE mount never matches, whatever its id.
        assert!(!guard_should_unmount(
            MOUNTINFO,
            814,
            Path::new("/home/u/other"),
            dead
        ));
    }

    /// The body of the **guard process** a test binary starts in place of the
    /// agent's [`GUARD_ARG`] mode ([`guard_command`]) — not a test of its own. A
    /// plain `#[test]`, never a tokio one: the guard closes every inherited
    /// descriptor, and a runtime's own would be among them.
    #[test]
    #[ignore = "child-process body: the crash guard, started by mount_over"]
    fn guard_child_body() {
        run_guard_from_env().expect("the crash guard (run only as the child mount_over starts)");
    }

    #[test]
    fn the_helper_is_found_only_as_a_file_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert!(!helper_on_path(Some(&path)));
        assert!(!helper_on_path(None));
        std::fs::write(dir.path().join(FUSERMOUNT), b"").unwrap();
        assert!(helper_on_path(Some(&path)));
    }

    #[test]
    fn a_listing_sink_reports_nothing_placed() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let children = vec![DirChild {
            name: "a.txt".into(),
            size: 3,
            mtime: 7,
            is_dir: false,
        }];
        let placed = ListingSink(tx).transfer_placeholders(&children).unwrap();
        assert!(
            placed.is_empty(),
            "a FUSE listing puts nothing on the disk, so nothing may be marked seen"
        );
        assert_eq!(rx.recv().unwrap(), children);
    }

    #[test]
    fn inode_numbers_are_stable_per_rel() {
        let mut inodes = Inodes::new();
        assert_eq!(inodes.ino(""), ROOT_INO);
        let a = inodes.ino("sub/a.txt");
        assert_ne!(a, ROOT_INO);
        assert_eq!(inodes.ino("sub/a.txt"), a);
        assert_eq!(inodes.rel(a).as_deref(), Some("sub/a.txt"));
        assert_eq!(inodes.rel(a + 100), None);
    }
}
