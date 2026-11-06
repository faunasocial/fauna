//! The shared **agent spawner** — the channel-agnostic half of the desktop
//! `AgentSpawner` (`sync-agent.md` § Packaging + lifecycle), lifted out of the
//! linux GTK client so **fauna-tui** consumes the same code (priority #2)
//! rather than a second copy.
//!
//! Cross-platform: agent-binary resolution (the harness pin, sibling-of-exe,
//! PATH), the reaping direct-child spawn ([`ChildSpawner`]), and the
//! [`LingerState`] reading. Per-OS arms inside:
//!
//! * **unix** — the `$XDG_CONFIG_HOME/systemd/user/fauna-sync-agent.service`
//!   path + unit text (with its two-line uninstall guard), `systemctl --user`
//!   driving, and the write/heal/enable/start ensure. The caller supplies the
//!   [`UnitControl`] (native `systemctl` here; the linux app keeps a Flatpak
//!   session-bus arm locally) and the [`UnitExec`] (a plain binary here; linux
//!   builds AppImage / `flatpak run` variants). Ready-made spawner:
//!   [`SystemdUserUnitSpawner`] — production writes+ensures the user unit, e2e
//!   direct-spawns the agent as a reaped child inheriting the launch's isolated
//!   XDG world.
//!   (Every unix but macOS: launchd never reads a systemd unit.)
//! * **macos** — [`LaunchdAgentSpawner`], the Rust lift of the macOS app's
//!   Swift `LaunchdSyncAgentSpawner`, so ONE writer serves both apps that
//!   self-install the per-user `social.fauna.sync-agent` LaunchAgent — the
//!   desktop app (over UniFFI) and fauna-tui on a mac installed from the
//!   archive alone (`sync-agent.md` § Packaging + lifecycle, *A tui-only mac*).
//!   It writes/heals `~/Library/LaunchAgents/social.fauna.sync-agent.plist`,
//!   then `launchctl bootstrap` + `kickstart`; e2e direct-spawns a child as on
//!   linux. There is no linger concept: the `gui/<uid>` domain IS the login
//!   session.
//! * **windows** — [`WindowsDetachedSpawner`], the Rust lift of the C# app's
//!   `SpawnSyncAgentDetached`: probe the installed/dev layouts and start the
//!   agent as a console-less child, forwarding the harness's pipe/data-dir
//!   isolation to it. There is no per-user unit to write — windows keeps the
//!   agent alive with the HKLM `Run` key (installer-owned), and a client only
//!   ever spawns it *on demand*.
//!
//! No `Pause`/`Resume`/enable-disable knob exists on either arm: the agent is
//! artifact wiring, and its *work* is entirely user-chosen through existing UI.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::agent::AgentSpawner;

/// The agent binary's file name on this platform — the one place the `.exe`
/// suffix is decided, so every probe below stays OS-neutral.
#[cfg(not(windows))]
pub const AGENT_BIN_NAME: &str = "fauna-sync-agent";
/// The agent binary's file name on this platform.
#[cfg(windows)]
pub const AGENT_BIN_NAME: &str = "fauna-sync-agent.exe";

/// `fauna-sync-agent.service` — the systemd **user**-unit name
/// (`sync-agent.md` § Packaging + lifecycle (linux)).
#[cfg(all(unix, not(target_os = "macos")))]
pub const UNIT_NAME: &str = "fauna-sync-agent.service";

/// What the user unit runs, plus the file whose removal must silence it.
///
/// **Valid by construction.** A systemd unit is a *structured* text format
/// whose record separator is the newline, so a value carrying one does not
/// merely corrupt its own directive — it invents new ones, at whatever section
/// it lands in. Both fields are therefore private and both constructors are
/// fallible: a `UnitExec` that exists is one whose interpolation into
/// [`unit_contents`] cannot add a line the caller did not write. Fencing here
/// rather than per-caller is what makes the guarantee hold for the *next*
/// input someone routes in (today: the harness pin, `APPIMAGE`, `FLATPAK_ID`).
///
/// **Refuse, don't sanitize.** A stripped path would still be *written* and
/// then compared against the honest one by `ensure_unit_running`/
/// `ConditionFileIsExecutable=`, i.e. a silently-wrong unit — the failure mode
/// [`AGENT_BIN_ENV`] itself rejects. Hostile input yields no unit at all plus a
/// loud log, and the convergence loop retries with whatever the environment
/// says next tick.
#[cfg(all(unix, not(target_os = "macos")))]
pub struct UnitExec {
    /// The `ExecStart=` value — a plain binary path (quoted, since AppImage
    /// paths are user-chosen and may contain spaces) or a `flatpak run` line.
    exec_start: String,
    /// The file whose removal must silence the unit — the agent binary
    /// (native / AppImage) or the deployed Flatpak agent under the stable
    /// `current/active` link. Guarded by BOTH `ConditionFileIsExecutable=` and
    /// `ExecCondition=` (see [`unit_contents`] for why two lines).
    condition: PathBuf,
}

/// Any control character is a refusal, not just `\n`: `\r` splits lines for
/// systemd's parser too, and a `\0`/escape in a unit is never legitimate. The
/// check runs over the exact strings [`unit_contents`] interpolates, so a
/// non-UTF-8 path is judged as its lossy `display()` form — which is what would
/// actually be written.
#[cfg(all(unix, not(target_os = "macos")))]
fn rejects_control_chars(field: &str, value: &str) -> bool {
    if let Some(bad) = value.chars().find(|c| c.is_control()) {
        tracing::warn!(
            "refusing to build a systemd unit: {field} contains control character {bad:?} \
             — a unit directive cannot be composed from it (writing NO unit)"
        );
        return true;
    }
    false
}

#[cfg(all(unix, not(target_os = "macos")))]
impl UnitExec {
    /// Build from a raw `ExecStart=` value + its condition target — the escape
    /// hatch the linux app's AppImage / Flatpak variants use. `None` when
    /// either input carries a control character (see the type's doc).
    pub fn new(exec_start: String, condition: PathBuf) -> Option<Self> {
        let condition_text = condition.display().to_string();
        if rejects_control_chars("ExecStart", &exec_start)
            || rejects_control_chars("the condition path", &condition_text)
        {
            return None;
        }
        Some(Self {
            exec_start,
            condition,
        })
    }

    /// A native install (install.sh, deb, dev tree): the user unit execs the
    /// agent binary installed beside the app. Quoted so a space-containing
    /// install path still parses as one arg — note the quotes defend the
    /// *argument* axis only; the newline refusal in [`Self::new`] is what
    /// defends the *directive* axis.
    pub fn for_binary(binary: PathBuf) -> Option<Self> {
        Self::new(format!("\"{}\"", binary.display()), binary)
    }

    /// The `ExecStart=` value — also what the linux file manager's Share
    /// module runs the agent's `--share-target` helper through, so the
    /// channel's agent command has one spelling
    /// (`apps/linux.md` § The file manager's Share entry (GNOME Files)).
    pub fn exec_start(&self) -> &str {
        &self.exec_start
    }
}

/// The unit text (per-user:
/// `Restart=always`, `WantedBy=default.target`; no `run` subcommand — the
/// agent's default entry point is the run loop).
///
/// The uninstall story is TWO lines guarding the same target
/// ([`UnitExec::condition`]), because they fire on different start paths:
/// - `ConditionFileIsExecutable=` (unit-level) is checked on an *initial* /
///   explicit / boot start — a reinstall-then-relaunch, or a login after
///   uninstall, condition-skips cleanly.
/// - `ExecCondition=` (service-level) is the one that ALSO re-runs on every
///   `Restart=always` **auto-restart**. `Condition*=` is NOT re-evaluated on
///   an auto-restart, so without `ExecCondition` an uninstall that kills the
///   *running* agent (Flatpak's `flatpak uninstall` SIGKILLs the instance)
///   Restart-flaps forever: each auto-restart re-runs `ExecStart` (`flatpak
///   run` → "not installed" → exit 1) every `RestartSec=5`, and because
///   5 × 5 s = 25 s outruns the default 10 s `StartLimitIntervalSec` window
///   the start limit never trips. `ExecCondition` makes each auto-restart
///   condition-skip instead (journal: "Skipped due to 'exec-condition'"),
///   leaving the unit cleanly `inactive`. Verified against the live systemd
///   user manager in `tests/e2e-unified/tests/real_session/`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn unit_contents(exec: &UnitExec) -> String {
    format!(
        "[Unit]\n\
         Description=Fauna per-user sync agent\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         ConditionFileIsExecutable={cond}\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecCondition=/usr/bin/test -x \"{cond}\"\n\
         ExecStart={exec_start}\n\
         Restart=always\n\
         RestartSec=5\n\
         Environment=FAUNA_LOG=info\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        cond = exec.condition.display(),
        exec_start = exec.exec_start,
    )
}

/// `$XDG_CONFIG_HOME|~/.config` + `systemd/user/fauna-sync-agent.service`.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn unit_path() -> PathBuf {
    let config_root = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(xdg) if !xdg.is_empty() => PathBuf::from(xdg),
        _ => {
            let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
            PathBuf::from(home).join(".config")
        }
    };
    config_root.join("systemd").join("user").join(UNIT_NAME)
}

/// Pins the agent binary to an exact path. Set ONLY by the e2e harness (the
/// `FAUNA_E2E_*` runtime-gate family, testing.md point 15) — never in
/// production, where the probes below are the whole story. The read itself
/// ([`pinned_from_env`]) is compiled out of a plain `--release` build
/// (convention 15: an env-var gate alone ships a process-execution redirect
/// in every release artifact); test-capable builds keep it via
/// `debug_assertions` or the `test-helpers` feature.
///
/// It has to win over both probes because **only the linux dev layout happens
/// to co-locate the app and the agent** (`target/debug/{fauna-linux,
/// fauna-sync-agent}`, so the sibling probe hits). macOS builds the app with
/// SwiftPM (`.build/arm64-apple-macosx/debug/FaunaMacOS`) and the agent with
/// cargo; windows splits them the same way. On those clients the sibling probe
/// misses and the bare-name fallback is resolved against `PATH` — which on a
/// developer box silently finds the *installed* agent (`/usr/local/bin/
/// fauna-sync-agent`, a `.pkg`/MSI leftover). That is not a benign miss: the
/// test then drives a stale, machine-global agent shared with the developer's
/// own session (testing.md point 10), and it MASKS a client that spawns nothing
/// at all — which is exactly how the macOS and windows seats of the multiseat
/// live test both looked "bound" while syncing in neither direction
/// (2026-07-24).
pub const AGENT_BIN_ENV: &str = "FAUNA_E2E_SYNC_AGENT_BIN";

/// The pinned agent path from a raw env value. Empty (or unset) means "not
/// pinned"; anything else is taken **verbatim and unconditionally** — a pin
/// that points at a missing file must fail loudly at spawn rather than quietly
/// fall through to a different binary, since silent fallback is the exact
/// failure this pin exists to remove.
pub fn pinned_agent_binary(raw: Option<&OsStr>) -> Option<PathBuf> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn pinned_from_env() -> Option<PathBuf> {
    pinned_agent_binary(std::env::var_os(AGENT_BIN_ENV).as_deref())
}

/// Production twin: a plain `--release` build never consults the pin —
/// resolution starts at the sibling probe (testing.md convention 15).
#[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
fn pinned_from_env() -> Option<PathBuf> {
    None
}

/// The agent binary: the harness pin ([`AGENT_BIN_ENV`]) if set, else beside the
/// app binary (both `/usr/bin` packaged and `target/debug` dev layouts ship them
/// side by side), falling back to a bare `fauna-sync-agent` PATH lookup (fine
/// for child spawns — `Command` resolves PATH itself; units instead need
/// [`agent_binary_absolute`]).
pub fn agent_binary() -> PathBuf {
    if let Some(pinned) = pinned_from_env() {
        return pinned;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(AGENT_BIN_NAME);
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from(AGENT_BIN_NAME)
}

/// Like [`agent_binary`] but never relative: harness pin first, then sibling,
/// then a PATH walk. A systemd unit needs an absolute `ExecStart` target for its
/// `ConditionFileIsExecutable` line to track the same file.
pub fn agent_binary_absolute() -> Option<PathBuf> {
    if let Some(pinned) = pinned_from_env() {
        return Some(pinned);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(AGENT_BIN_NAME);
        if sibling.exists() {
            return Some(sibling);
        }
    }
    path_lookup(std::env::var_os("PATH").as_deref())
}

/// Walk a PATH-style variable for the agent binary.
pub fn path_lookup(path_var: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(AGENT_BIN_NAME))
        .find(|candidate| candidate.is_file())
}

/// How the spawner drives the user manager's reload/enable/start verbs. The
/// native `systemctl --user` impl ([`SystemctlUser`]) lives here; the linux
/// app's Flatpak session-bus arm is a local impl.
#[cfg(all(unix, not(target_os = "macos")))]
pub trait UnitControl {
    /// After a unit write/heal: pick up the new file, enable it persistently,
    /// and start it. `false` on failure (lets a fallible caller — the Flatpak
    /// arm — fall back to app-child residency).
    fn reload_enable_start(&self) -> bool;
    /// Unit present + current but the socket probe said the agent is down —
    /// kick it (covers a stopped or crashed-out unit; a start job on an
    /// already-active unit is a no-op).
    fn start(&self) -> bool;
}

/// `systemctl --user` driving (native / AppImage channels) — best-effort, no
/// fallback residency exists on these channels, so failures only warn.
#[cfg(all(unix, not(target_os = "macos")))]
pub struct SystemctlUser;

#[cfg(all(unix, not(target_os = "macos")))]
impl UnitControl for SystemctlUser {
    fn reload_enable_start(&self) -> bool {
        systemctl_user(&["daemon-reload"]);
        systemctl_user(&["enable", "--now", UNIT_NAME]);
        true
    }

    fn start(&self) -> bool {
        systemctl_user(&["start", UNIT_NAME]);
        true
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemctl_user(args: &[&str]) {
    match Command::new("systemctl").arg("--user").args(args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            tracing::warn!("systemctl --user {args:?} exited with {status}");
        }
        Err(e) => tracing::warn!("systemctl --user {args:?} failed to run: {e}"),
    }
}

/// Whether this user's systemd manager keeps running once their last login
/// session ends — i.e. whether `loginctl enable-linger` is in effect
/// (`sync-agent.md` § Headless deployment).
///
/// This is the difference between a headless box that syncs and one that
/// doesn't: without lingering, the user manager — and with it
/// `fauna-sync-agent.service` — is torn down the moment the SSH session closes,
/// so the agent only ever runs while someone is watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LingerState {
    /// The agent survives logout.
    Enabled,
    /// The agent dies with the last session.
    Disabled,
    /// No `loginctl` to ask (not a systemd-logind box, or it is not on PATH) —
    /// the question does not apply here, so a client offers nothing rather than
    /// claiming either answer.
    Unavailable,
}

/// Parse `loginctl show-user --value -p Linger` output. Split out from the
/// process call so the three readings are unit-pinned without a live logind.
/// Anything that is not a clean `yes`/`no` is [`LingerState::Unavailable`] —
/// silence, an error string, or a future format are all "we do not know", never
/// a guess.
pub fn parse_linger_output(raw: &str) -> LingerState {
    match raw.trim() {
        "yes" => LingerState::Enabled,
        "no" => LingerState::Disabled,
        _ => LingerState::Unavailable,
    }
}

/// How a client reads and flips lingering. A trait for the same reason
/// [`UnitControl`] is one: the real impl shells out to `loginctl`, and a test
/// needs to drive the surface without touching the machine's real logind state.
#[cfg(all(unix, not(target_os = "macos")))]
pub trait LingerControl {
    fn state(&self) -> LingerState;
    /// Enable/disable lingering for the calling user. `false` if the call
    /// failed — on most distros polkit lets a user linger *themselves* without
    /// a password, but a locked-down box can refuse, and the client must
    /// surface that rather than paint an optimistic toggle.
    fn set(&self, enabled: bool) -> bool;
}

/// `loginctl` driving for the calling user (`--user` is implicit: `loginctl
/// enable-linger` with no argument means "me").
#[cfg(all(unix, not(target_os = "macos")))]
pub struct LoginctlUser;

#[cfg(all(unix, not(target_os = "macos")))]
impl LingerControl for LoginctlUser {
    fn state(&self) -> LingerState {
        match Command::new("loginctl")
            .args(["show-user", "--value", "-p", "Linger"])
            .output()
        {
            Ok(out) if out.status.success() => {
                parse_linger_output(&String::from_utf8_lossy(&out.stdout))
            }
            // A non-zero exit is the ordinary "no such user record yet" case on
            // a box where the user has never had a session — not an error worth
            // logging on every poll.
            Ok(_) => LingerState::Unavailable,
            Err(e) => {
                tracing::debug!("loginctl show-user Linger failed to run: {e}");
                LingerState::Unavailable
            }
        }
    }

    fn set(&self, enabled: bool) -> bool {
        let verb = if enabled {
            "enable-linger"
        } else {
            "disable-linger"
        };
        match Command::new("loginctl").arg(verb).status() {
            Ok(status) if status.success() => true,
            Ok(status) => {
                tracing::warn!("loginctl {verb} exited with {status}");
                false
            }
            Err(e) => {
                tracing::warn!("loginctl {verb} failed to run: {e}");
                false
            }
        }
    }
}

/// Write/heal the unit (missing, or exec/condition pointing at a different
/// target) and enable+start it; an existing current unit just gets a `start`
/// kick (covers a `systemctl --user stop`ed or crashed-out unit). Best-effort
/// throughout — the convergence loop retries next tick; the `false` return is
/// what lets a Flatpak caller fall back to a child spawn instead of silently
/// having no agent at all.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn ensure_unit_running(
    exec: &UnitExec,
    unit_path: &std::path::Path,
    ctl: &dyn UnitControl,
) -> bool {
    let desired = unit_contents(exec);
    let existing = std::fs::read_to_string(unit_path).ok();
    if existing.as_deref() != Some(desired.as_str()) {
        if let Some(parent) = unit_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(unit_path, &desired) {
            tracing::warn!("could not write {}: {e}", unit_path.display());
            return false;
        }
        ctl.reload_enable_start()
    } else {
        ctl.start()
    }
}

/// A reaping direct-child spawner for the agent binary — the e2e path and the
/// production Flatpak/Snap `SandboxChild` residency (where no user-unit control
/// surface exists). The agent inherits the launch's private
/// `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR` and dies with the parent's
/// process-group teardown.
#[derive(Default)]
pub struct ChildSpawner {
    /// Spawned children, kept so repeated spawns can reap exited ones
    /// (`try_wait`) instead of accreting zombies for the parent's lifetime.
    children: Mutex<Vec<std::process::Child>>,
}

impl ChildSpawner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn the resolved [`agent_binary`] as a reaped child (unless one is
    /// already live).
    pub fn spawn(&self) {
        self.spawn_command(Command::new(agent_binary()));
    }

    /// Reap exited children, then spawn `cmd` — unless a child is still live.
    /// A live-but-not-yet-bound child (slow sandboxed start) must not get a
    /// sibling every convergence tick/poke: the agent's own `InstanceLock`
    /// (fauna-ipc `unix_transport`) makes a duplicate exit harmlessly, but
    /// gating here avoids the per-tick process churn in the first place.
    pub fn spawn_command(&self, mut cmd: Command) {
        let mut children = self.children.lock().unwrap();
        children.retain_mut(|c| !matches!(c.try_wait(), Ok(Some(_))));
        if !children.is_empty() {
            return;
        }
        match cmd.spawn() {
            Ok(child) => children.push(child),
            Err(e) => tracing::warn!("agent child spawn failed: {e}"),
        }
    }
}

/// The ready-made [`AgentSpawner`] for a client that is always a plain
/// installed binary (fauna-tui; the linux `NativeUnit` channel dispatches to
/// the same two arms). Production writes+ensures the user unit; e2e
/// (`e2e = true`) direct-spawns the agent as a reaped child inheriting the
/// launch's isolated XDG world.
#[cfg(all(unix, not(target_os = "macos")))]
pub struct SystemdUserUnitSpawner {
    e2e: bool,
    child: ChildSpawner,
}

#[cfg(all(unix, not(target_os = "macos")))]
impl SystemdUserUnitSpawner {
    pub fn new(e2e: bool) -> Self {
        Self {
            e2e,
            child: ChildSpawner::new(),
        }
    }
}

// ── windows: the detached-child arm ────────────────────────────────────────
//
// The Rust lift of the C# app's `SpawnSyncAgentDetached`, so retiring that
// stack loses no behavior. Windows has no per-user unit to write: the installer
// owns the HKLM `Run` key that starts the agent per-logon, and a client only
// ever spawns it *on demand* when the endpoint probe finds nothing listening.

/// The candidate agent paths, in resolution order, for a windows app whose
/// own executable lives in `exe_dir`.
///
/// A `pinned` value is the **only** candidate — verbatim and unconditional (see
/// [`AGENT_BIN_ENV`]). Otherwise three probes, in order:
///
/// 1. `<exe-dir>\fauna-sync-agent.exe` — the cargo dev layout AND fauna-tui's
///    install layout, both of which put the two binaries side by side.
/// 2. `<exe-dir>\..\fauna-sync-agent.exe` — the installed WinUI layout (the app
///    under `app\`, services at the install root). Kept so the FFI provisioner
///    can reuse this resolution when the C# stack retires onto it.
/// 3. `%ProgramFiles%\Fauna\fauna-sync-agent.exe`.
///
/// Pure over its inputs so the order is tier_1-pinnable without a real spawn.
#[cfg(windows)]
pub fn windows_agent_candidates(
    pinned: Option<PathBuf>,
    exe_dir: Option<&std::path::Path>,
    program_files: Option<&OsStr>,
) -> Vec<PathBuf> {
    if let Some(pinned) = pinned {
        return vec![pinned];
    }
    let mut candidates = Vec::new();
    if let Some(dir) = exe_dir {
        candidates.push(dir.join(AGENT_BIN_NAME));
        candidates.push(dir.join("..").join(AGENT_BIN_NAME));
    }
    if let Some(pf) = program_files.filter(|pf| !pf.is_empty()) {
        candidates.push(PathBuf::from(pf).join("Fauna").join(AGENT_BIN_NAME));
    }
    candidates
}

/// The agent argv the harness isolation envs imply, exactly as the C# spawn
/// composes it: `--pipe-name \\.\pipe\<leaf>` when [`E2E_PIPE_ENV`] is set, and
/// `--data-dir <dir>` when `FAUNA_E2E_SYNC_AGENT_DATA_DIR` is.
///
/// Both are empty in production — the agent then derives its own per-SID pipe
/// and `%LOCALAPPDATA%` data root. Under a test they are mandatory: windows
/// rendezvouses on a machine-global kernel name and a machine-global data dir,
/// so without them an isolated launch drives (and corrupts) the developer's own
/// agent state, which is the contamination `helpers/windows_sync_agent.py`
/// exists to diagnose.
///
/// Pure over its inputs; the leaf→name translation is the client's
/// [`fauna_ipc::endpoint::pipe_name_from_env`], so the spawner and the connect
/// side cannot disagree about what "set" means.
///
/// [`E2E_PIPE_ENV`]: fauna_ipc::endpoint::E2E_PIPE_ENV
#[cfg(windows)]
pub fn windows_agent_args(
    pipe_leaf: Option<&OsStr>,
    data_dir: Option<&OsStr>,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    if let Some(name) = fauna_ipc::endpoint::pipe_name_from_env(pipe_leaf) {
        args.push("--pipe-name".into());
        args.push(name.into());
    }
    if let Some(dir) = data_dir.filter(|d| !d.is_empty()) {
        args.push("--data-dir".into());
        args.push(dir.to_os_string());
    }
    args
}

/// `CREATE_NO_WINDOW` — the child gets **no console of its own and does not
/// join the parent's**, which is what `CreateNoWindow = true` gives the C# app
/// and what a *terminal* client additionally needs for two reasons the GUI app
/// never had:
///
/// * a console-subsystem child sharing fauna-tui's console would scribble its
///   log lines over the rendered UI, and
/// * a child in the parent's console process group receives that console's
///   Ctrl+C — so quitting fauna-tui would kill the very agent that is supposed
///   to keep syncing app-dead (`sync-agent.md` § Packaging + lifecycle).
///
/// Deliberately NOT `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB`: the child
/// must stay inside the e2e harness's job object so a run still tears the whole
/// tree down (testing.md point 9).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The windows [`AgentSpawner`]: probe the layouts above and start the agent as
/// a console-less child (the C# `SpawnSyncAgentDetached` shape). Spawning goes
/// through [`ChildSpawner`] so a live-but-not-yet-bound agent does not get a
/// sibling on every convergence tick; the agent's own `InstanceLock` (a named
/// mutex keyed on the pipe name) remains the correctness guard.
#[cfg(windows)]
#[derive(Default)]
pub struct WindowsDetachedSpawner {
    child: ChildSpawner,
}

#[cfg(windows)]
impl WindowsDetachedSpawner {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(windows)]
impl AgentSpawner for WindowsDetachedSpawner {
    fn spawn_agent(&self) {
        let pinned = pinned_from_env();
        let is_pinned = pinned.is_some();
        let exe = std::env::current_exe().ok();
        let candidates = windows_agent_candidates(
            pinned,
            exe.as_deref().and_then(|e| e.parent()),
            std::env::var_os("ProgramFiles").as_deref(),
        );
        // The harness-isolation forwards are compiled out with the pin
        // (convention 15): production spawns take no argv, the agent derives
        // its own per-SID pipe and %LOCALAPPDATA% data root.
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        let args = windows_agent_args(
            std::env::var_os(fauna_ipc::endpoint::E2E_PIPE_ENV).as_deref(),
            std::env::var_os(AGENT_DATA_DIR_ENV).as_deref(),
        );
        #[cfg(not(any(test, debug_assertions, feature = "test-helpers")))]
        let args: Vec<std::ffi::OsString> = Vec::new();

        for candidate in &candidates {
            if !candidate.is_file() {
                continue;
            }
            let mut cmd = Command::new(candidate);
            cmd.args(&args)
                .creation_flags(CREATE_NO_WINDOW)
                // No inherited console handles, and nothing to drain: a piped
                // stream nobody reads wedges the child once its ~64KB buffer
                // fills (testing.md point 13). The agent logs to its own file.
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            tracing::info!("spawning sync agent: {} {args:?}", candidate.display());
            self.child.spawn_command(cmd);
            return;
        }

        // is_pinned is constant false in a production build (pinned_from_env's
        // twin); the arm is cfg-gated too so the env name's format string never
        // reaches a release artifact — the strings-grep witness keys on it.
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        if is_pinned {
            // Verbatim and unconditional: a pin that misses spawns NOTHING
            // rather than falling through to the box's installed agent.
            tracing::warn!(
                "pinned agent {:?} ({AGENT_BIN_ENV}) does not exist — spawning nothing",
                candidates.first()
            );
            return;
        }
        let _ = is_pinned;
        tracing::warn!(
            "no {AGENT_BIN_NAME} found beside the app, one level up, or under \
             %ProgramFiles%\\Fauna — the agent may be Run-key-started or absent in dev"
        );
    }
}

/// The harness's agent data-dir override, forwarded to the child as
/// `--data-dir` (windows only: the unix agent inherits an isolated `HOME` /
/// `XDG_CONFIG_HOME` and needs no explicit flag).
#[cfg(windows)]
pub const AGENT_DATA_DIR_ENV: &str = "FAUNA_E2E_SYNC_AGENT_DATA_DIR";

/// The pin-store dir an agent launched with `--data-dir <root>` reads:
/// `<root>/trust`. ONE derivation for both ends — the agent's own path table
/// (`fauna-sync-agent`'s `SyncPaths::at`) and the read replica its app keeps
/// there ([`agent_pin_replica_dir_for`]) — so the two cannot drift apart
/// (`transport-trust.md` § Pin custody across processes, *an isolated agent
/// reads its app's replica*).
pub fn agent_trust_dir(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("trust")
}

/// Where an interactive app keeps a **read replica** of its pin file for the
/// agent it spawns on `data_dir` — the harness's agent data-dir pin
/// (`AGENT_DATA_DIR_ENV`, on windows): `Some(<data_dir>/trust)` when set and
/// non-empty, else `None`. The agent then reads its app's pins without either
/// process touching the box's install-scoped trust home (convention 10) and
/// without a second minter (the replica is the app's file, copied — the
/// mechanism macOS uses for its File Provider extension). Pure over its input,
/// compiled everywhere so it is tested on every box; the env read is
/// [`spawned_agent_pin_replica_dir`].
pub fn agent_pin_replica_dir_for(data_dir: Option<&OsStr>) -> Option<PathBuf> {
    data_dir
        .filter(|d| !d.is_empty())
        .map(|d| agent_trust_dir(std::path::Path::new(d)))
}

/// [`agent_pin_replica_dir_for`] over this process's environment — `Some`
/// exactly when the windows spawner forwards `--data-dir` to the agent: the
/// same `cfg` gate, so a production build (convention 15) and every
/// non-windows target answer `None` and keep the plain writer store. The
/// writers that consult it are the windows app (through `fauna-ffi`'s
/// `install_nest_identity_pin_store`) and fauna-tui.
pub fn spawned_agent_pin_replica_dir() -> Option<PathBuf> {
    #[cfg(all(windows, any(test, debug_assertions, feature = "test-helpers")))]
    {
        agent_pin_replica_dir_for(std::env::var_os(AGENT_DATA_DIR_ENV).as_deref())
    }
    #[cfg(not(all(windows, any(test, debug_assertions, feature = "test-helpers"))))]
    {
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
impl AgentSpawner for SystemdUserUnitSpawner {
    fn spawn_agent(&self) {
        if self.e2e {
            self.child.spawn();
            return;
        }
        // A refused (control-char-bearing) path lands in the same arm as "not
        // found": no unit is written, the refusal is already logged by
        // UnitExec, and the convergence loop retries next tick.
        match agent_binary_absolute().and_then(UnitExec::for_binary) {
            Some(exec) => {
                ensure_unit_running(&exec, &unit_path(), &SystemctlUser);
            }
            None => tracing::warn!(
                "fauna-sync-agent not found beside the app or on PATH — \
                 cannot install the user unit (next convergence tick retries)"
            ),
        }
    }
}

// ── macos: the per-user LaunchAgent arm ─────────────────────────────────────
//
// The Rust lift of the macOS app's Swift `LaunchdSyncAgentSpawner`. The plist
// is one per user, and three parties write it — the `.pkg`'s sync component,
// the desktop app and fauna-tui — so the two that live here share ONE target
// resolution ([`macos_agent_candidates`]) and ONE rendering
// ([`launchd_plist_contents`], byte-identical to the `.pkg`'s), and whichever
// wrote last leaves the same file behind.

/// `social.fauna.sync-agent` — the launchd label (and plist file stem) of the
/// per-user agent job. The Swift side keeps `AppleIdentifiers.syncAgentLaunchAgent`
/// as the bundle-side spelling of the same name; the `.pkg` postinstall is the
/// third (all three pinned equal by `test_apple_identifier_pins.py`).
#[cfg(target_os = "macos")]
pub const LAUNCH_AGENT_LABEL: &str = "social.fauna.sync-agent";

/// The desktop app's bundled agent — the first unpinned candidate whenever the
/// writer is not itself inside an app bundle. A bundle-attributed agent prompts
/// for TCC as "Fauna" on the app's own subject, while a bare `/usr/local/bin`
/// exec prompts as a path (`installers/macos.md` § Identifier domain), so the
/// bundled copy wins whichever app wrote the plist last.
#[cfg(target_os = "macos")]
pub const APP_BUNDLE_AGENT: &str = "/Applications/Fauna.app/Contents/MacOS/fauna-sync-agent";

/// The `.pkg`'s services-only agent copy.
#[cfg(target_os = "macos")]
pub const PKG_AGENT: &str = "/usr/local/bin/fauna-sync-agent";

/// `$HOME/Library/LaunchAgents/<label>.plist`.
#[cfg(target_os = "macos")]
pub fn launchd_plist_path(label: &str) -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into());
    PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"))
}

/// The candidate agent paths, in resolution order, for a writer whose own
/// executable lives in `exe_dir` (`sync-agent.md` § Packaging + lifecycle,
/// *A tui-only mac* — one order whichever app writes).
///
/// A `pinned` value is the **only** candidate — verbatim and unconditional (see
/// [`AGENT_BIN_ENV`]). Otherwise, in order:
///
/// 1. the desktop app's bundled copy — the writer's own sibling when the writer
///    is inside an `.app` bundle (`…/X.app/Contents/MacOS/`), else
///    `app_bundle_agent` ([`APP_BUNDLE_AGENT`]);
/// 2. the writer's own sibling (`$PREFIX/bin/fauna-sync-agent` for the
///    archive's tui; a debug app's build dir);
/// 3. `pkg_agent` ([`PKG_AGENT`]);
/// 4. each `PATH` entry.
///
/// Duplicates are dropped (a bundled writer's 1 and 2 are one path). Pure over
/// its inputs so the order is unit-pinnable with a temp layout.
#[cfg(target_os = "macos")]
pub fn macos_agent_candidates(
    pinned: Option<PathBuf>,
    exe_dir: Option<&std::path::Path>,
    app_bundle_agent: &std::path::Path,
    pkg_agent: &std::path::Path,
    path_var: Option<&OsStr>,
) -> Vec<PathBuf> {
    if let Some(pinned) = pinned {
        return vec![pinned];
    }
    let sibling = exe_dir.map(|dir| dir.join(AGENT_BIN_NAME));
    let writer_is_bundled = exe_dir.is_some_and(|dir| {
        dir.ends_with("Contents/MacOS")
            && dir
                .parent()
                .and_then(|contents| contents.parent())
                .and_then(|bundle| bundle.extension())
                .is_some_and(|ext| ext == "app")
    });
    let mut ordered: Vec<PathBuf> = Vec::new();
    match (&sibling, writer_is_bundled) {
        (Some(own), true) => ordered.push(own.clone()),
        _ => ordered.push(app_bundle_agent.to_path_buf()),
    }
    ordered.extend(sibling);
    ordered.push(pkg_agent.to_path_buf());
    if let Some(path_var) = path_var {
        ordered.extend(
            std::env::split_paths(path_var)
                .filter(|dir| !dir.as_os_str().is_empty())
                .map(|dir| dir.join(AGENT_BIN_NAME)),
        );
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for candidate in ordered {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// The first candidate that is an executable file — or the pin, verbatim, when
/// one is set (a pin that misses must fail loudly at launch, never fall through
/// to the box's installed agent).
#[cfg(target_os = "macos")]
pub fn resolve_macos_agent(candidates: &[PathBuf], pinned: bool) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if pinned {
        return candidates.first().cloned();
    }
    candidates
        .iter()
        .find(|c| {
            std::fs::metadata(c).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .cloned()
}

/// Escape the three characters XML text cannot carry verbatim.
#[cfg(target_os = "macos")]
fn xml_text(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The LaunchAgent plist — `Label`, `ProgramArguments: [binary]`,
/// `RunAtLoad`, `KeepAlive`, and deliberately NO `StandardOutPath` /
/// `StandardErrorPath`: the agent writes its own size-capped log, and launchd
/// never rotates a redirect (one reached 6.6 GB).
///
/// Byte-identical to what the `.pkg`'s sync postinstall writes (its heredoc
/// through `echo`, trailing newline included), so a box whose plist the `.pkg`
/// wrote is already current and is never rewritten — and re-bootstrapped — on
/// the first convergence tick.
///
/// Hand-written XML: the three markup characters are escaped, and a path the
/// file cannot carry honestly — a control character (invalid XML 1.0 text) or
/// a non-UTF-8 name (a lossy form would name a different file) — yields `None`:
/// refuse, don't sanitize, as [`UnitExec`]'s twin rule on the systemd arm.
#[cfg(target_os = "macos")]
pub fn launchd_plist_contents(label: &str, binary: &std::path::Path) -> Option<String> {
    let Some(binary) = binary.to_str() else {
        tracing::warn!(
            "refusing to write a LaunchAgent: agent path {} is not UTF-8 (writing NO plist)",
            binary.display()
        );
        return None;
    };
    for (field, value) in [("label", label), ("agent path", binary)] {
        if let Some(bad) = value.chars().find(|c| c.is_control()) {
            tracing::warn!(
                "refusing to write a LaunchAgent: {field} contains control character {bad:?} \
                 (writing NO plist)"
            );
            return None;
        }
    }
    Some(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"\n  \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n    \
         <key>Label</key>\n    \
         <string>{label}</string>\n    \
         <key>ProgramArguments</key>\n    \
         <array>\n        \
         <string>{binary}</string>\n    \
         </array>\n    \
         <key>RunAtLoad</key>\n    \
         <true/>\n    \
         <key>KeepAlive</key>\n    \
         <true/>\n\
         </dict>\n\
         </plist>\n",
        label = xml_text(label),
        binary = xml_text(binary),
    ))
}

/// How the spawner drives `launchctl` in the user's `gui/<uid>` domain. A
/// trait for the same reason [`UnitControl`] is one on the systemd arm: the
/// unit tests drive [`ensure_launch_agent`] without touching the machine's
/// real launchd.
#[cfg(target_os = "macos")]
pub trait LaunchctlControl {
    /// `launchctl bootstrap gui/<uid> <plist>`. Fails on an already-loaded job
    /// — the steady state — so callers ignore the answer.
    fn bootstrap(&self, plist: &std::path::Path) -> bool;
    /// `launchctl bootout gui/<uid>/<label>` — unload, so a healed plist is
    /// what the next bootstrap reads (launchd keeps the loaded definition, not
    /// the file).
    fn bootout(&self, label: &str) -> bool;
    /// `launchctl kickstart gui/<uid>/<label>` — start a loaded-but-stopped job
    /// (a no-op on a running one).
    fn kickstart(&self, label: &str) -> bool;
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// libSystem's `getuid(2)` — always succeeds, no preconditions.
    safe fn getuid() -> u32;
}

/// The real `/bin/launchctl` (absolute path: never a `PATH`-resolved stand-in),
/// stdout/stderr to null.
#[cfg(target_os = "macos")]
pub struct Launchctl;

#[cfg(target_os = "macos")]
impl Launchctl {
    fn domain() -> String {
        format!("gui/{}", getuid())
    }

    fn run(args: &[&OsStr]) -> bool {
        match Command::new("/bin/launchctl")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) => status.success(),
            Err(e) => {
                tracing::warn!("launchctl {:?} failed to run: {e}", args.first());
                false
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl LaunchctlControl for Launchctl {
    fn bootstrap(&self, plist: &std::path::Path) -> bool {
        let domain = Self::domain();
        Self::run(&["bootstrap".as_ref(), domain.as_ref(), plist.as_os_str()])
    }

    fn bootout(&self, label: &str) -> bool {
        let target = format!("{}/{label}", Self::domain());
        Self::run(&["bootout".as_ref(), target.as_ref()])
    }

    fn kickstart(&self, label: &str) -> bool {
        let target = format!("{}/{label}", Self::domain());
        Self::run(&["kickstart".as_ref(), target.as_ref()])
    }
}

/// Write or heal the plist, then make sure the job is loaded and started.
///
/// * missing → write, `bootstrap`;
/// * present but not byte-equal to [`launchd_plist_contents`] (a different
///   target, an older `.pkg`'s stdout/stderr redirect) → `bootout` the loaded
///   job first (launchd runs the definition it loaded, not the file, so a
///   heal without it would take effect only at the next login), rewrite,
///   `bootstrap`;
/// * current → `bootstrap` anyway (fails harmlessly when loaded — the steady
///   state — and loads a job someone booted out).
///
/// Then `kickstart`, which starts a loaded-but-stopped job. Best-effort: the
/// convergence loop retries next tick. `false` when the plist could not be
/// rendered or written, or the kickstart failed.
#[cfg(target_os = "macos")]
pub fn ensure_launch_agent(
    label: &str,
    binary: &std::path::Path,
    plist_path: &std::path::Path,
    ctl: &dyn LaunchctlControl,
) -> bool {
    let Some(desired) = launchd_plist_contents(label, binary) else {
        return false;
    };
    let existing = std::fs::read_to_string(plist_path).ok();
    if existing.as_deref() != Some(desired.as_str()) {
        if existing.is_some() || plist_path.exists() {
            let _ = ctl.bootout(label);
        }
        if let Some(parent) = plist_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(plist_path, &desired) {
            tracing::warn!("could not write {}: {e}", plist_path.display());
            return false;
        }
    }
    let _ = ctl.bootstrap(plist_path);
    let started = ctl.kickstart(label);
    if !started {
        tracing::warn!("launchctl kickstart of {label} failed (next convergence tick retries)");
    }
    started
}

/// The macOS [`AgentSpawner`] — fauna-tui's directly, the desktop app's
/// through `fauna-ffi`'s `FfiLaunchdAgentSpawner`. Production resolves the
/// agent ([`macos_agent_candidates`]) and ensures the LaunchAgent; e2e
/// (`e2e = true`) direct-spawns the agent as a reaped child inheriting the
/// launch's isolated `HOME`, so no test ever touches the machine-global job
/// (the [`SystemdUserUnitSpawner::new`] shape on linux).
#[cfg(target_os = "macos")]
pub struct LaunchdAgentSpawner {
    e2e: bool,
    child: ChildSpawner,
}

#[cfg(target_os = "macos")]
impl LaunchdAgentSpawner {
    pub fn new(e2e: bool) -> Self {
        Self {
            e2e,
            child: ChildSpawner::new(),
        }
    }
}

#[cfg(target_os = "macos")]
impl AgentSpawner for LaunchdAgentSpawner {
    fn spawn_agent(&self) {
        if self.e2e {
            self.child.spawn();
            return;
        }
        let pinned = pinned_from_env();
        let is_pinned = pinned.is_some();
        let exe = std::env::current_exe().ok();
        let candidates = macos_agent_candidates(
            pinned,
            exe.as_deref().and_then(|e| e.parent()),
            std::path::Path::new(APP_BUNDLE_AGENT),
            std::path::Path::new(PKG_AGENT),
            std::env::var_os("PATH").as_deref(),
        );
        match resolve_macos_agent(&candidates, is_pinned) {
            Some(binary) => {
                ensure_launch_agent(
                    LAUNCH_AGENT_LABEL,
                    &binary,
                    &launchd_plist_path(LAUNCH_AGENT_LABEL),
                    &Launchctl,
                );
            }
            None => tracing::warn!(
                "no {AGENT_BIN_NAME} found in Fauna.app, beside this app, under \
                 /usr/local/bin or on PATH — cannot install the LaunchAgent \
                 (next convergence tick retries)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- the agent's pin replica (agent_pin_replica_dir_for) --

    #[test]
    fn the_pin_replica_is_the_trust_dir_inside_the_agents_data_dir() {
        let root = std::path::Path::new("agent-root");
        assert_eq!(
            agent_pin_replica_dir_for(Some(root.as_os_str())),
            Some(root.join("trust"))
        );
        assert_eq!(
            agent_pin_replica_dir_for(Some(root.as_os_str())),
            Some(agent_trust_dir(root)),
            "the app's replica and the agent's read dir are one derivation"
        );
    }

    #[test]
    fn no_agent_data_dir_means_no_replica() {
        assert_eq!(agent_pin_replica_dir_for(None), None);
        assert_eq!(agent_pin_replica_dir_for(Some(OsStr::new(""))), None);
    }

    /// Off windows the spawner forwards no `--data-dir` (the unix agent
    /// inherits the launch's isolated home), so no writer keeps a replica.
    #[cfg(not(windows))]
    #[test]
    fn no_replica_off_windows() {
        assert_eq!(spawned_agent_pin_replica_dir(), None);
    }

    // -- harness pin (AGENT_BIN_ENV) --

    #[test]
    fn pin_is_absent_when_unset_or_empty() {
        assert_eq!(pinned_agent_binary(None), None);
        assert_eq!(pinned_agent_binary(Some(OsStr::new(""))), None);
    }

    #[test]
    fn pin_is_taken_verbatim_even_when_the_target_is_missing() {
        // Verbatim + unconditional is the whole point: a pin that misses must
        // fail loudly at spawn, never fall through to the sibling/PATH probes
        // and silently drive the box's INSTALLED agent. That silent fallback is
        // what made the macOS and windows seats look bound while syncing
        // nothing (2026-07-24).
        let missing = "/nonexistent/target/debug/fauna-sync-agent";
        assert_eq!(
            pinned_agent_binary(Some(OsStr::new(missing))),
            Some(PathBuf::from(missing))
        );
    }

    #[test]
    fn pin_preserves_a_path_containing_spaces() {
        // Cargo target dirs are user-chosen; macOS home dirs routinely contain
        // spaces. No quoting/splitting here — the child spawn passes it as one
        // argv entry.
        let spaced = "/Users/dev/my target/debug/fauna-sync-agent";
        assert_eq!(
            pinned_agent_binary(Some(OsStr::new(spaced))),
            Some(PathBuf::from(spaced))
        );
    }

    // -- linger parsing (the headless-deployment reading) --

    #[test]
    fn linger_output_parses_the_two_definite_readings() {
        assert_eq!(parse_linger_output("yes"), LingerState::Enabled);
        assert_eq!(parse_linger_output("no"), LingerState::Disabled);
        // loginctl --value still emits a trailing newline.
        assert_eq!(parse_linger_output("yes\n"), LingerState::Enabled);
        assert_eq!(parse_linger_output("no\n"), LingerState::Disabled);
    }

    /// Anything we cannot read as a definite yes/no is Unavailable — never a
    /// guess. Painting "Disabled" for an unreadable box would nag a user who
    /// has no lingering concept at all; painting "Enabled" would silently
    /// promise their agent survives logout when it does not.
    #[test]
    fn an_unreadable_linger_output_is_unavailable_not_a_guess() {
        for raw in [
            "",
            "\n",
            "Failed to get user: No such process",
            "maybe",
            "1",
        ] {
            assert_eq!(
                parse_linger_output(raw),
                LingerState::Unavailable,
                "{raw:?} must not resolve to a definite reading"
            );
        }
    }

    // -- windows resolution order + harness arg forwarding (the C# lift) --

    /// The pin wins outright and is the ONLY candidate — never a first-choice
    /// that falls through to the probes. Falling through is what made a macOS
    /// and a windows seat look "bound" while driving the box's installed agent
    /// (2026-07-24), so the list length is the assertion that matters.
    #[cfg(windows)]
    #[test]
    fn a_pinned_agent_is_the_only_windows_candidate() {
        let pinned = PathBuf::from(r"C:\target\debug\fauna-sync-agent.exe");
        assert_eq!(
            windows_agent_candidates(
                Some(pinned.clone()),
                Some(std::path::Path::new(r"C:\Program Files\Fauna\app")),
                Some(OsStr::new(r"C:\Program Files")),
            ),
            vec![pinned]
        );
    }

    /// Unpinned: sibling first (the cargo dev layout and fauna-tui's install
    /// layout both co-locate the binaries), then one level up (the installed
    /// WinUI layout: app under `app\`, services at the install root), then
    /// `%ProgramFiles%\Fauna`.
    #[cfg(windows)]
    #[test]
    fn the_unpinned_windows_order_is_sibling_then_parent_then_program_files() {
        let exe_dir = std::path::Path::new(r"C:\Program Files\Fauna\app");
        assert_eq!(
            windows_agent_candidates(None, Some(exe_dir), Some(OsStr::new(r"C:\Program Files"))),
            vec![
                PathBuf::from(r"C:\Program Files\Fauna\app\fauna-sync-agent.exe"),
                PathBuf::from(r"C:\Program Files\Fauna\app\..\fauna-sync-agent.exe"),
                PathBuf::from(r"C:\Program Files\Fauna\fauna-sync-agent.exe"),
            ]
        );
    }

    /// A missing `current_exe` / `%ProgramFiles%` drops just its own probes —
    /// resolution degrades, it does not panic or invent a relative path.
    #[cfg(windows)]
    #[test]
    fn windows_candidates_skip_probes_whose_inputs_are_absent() {
        assert!(windows_agent_candidates(None, None, None).is_empty());
        assert_eq!(
            windows_agent_candidates(None, None, Some(OsStr::new(""))),
            Vec::<PathBuf>::new(),
            "a blank %ProgramFiles% must not become a relative Fauna\\ probe"
        );
    }

    /// Production forwards nothing: the agent derives its own per-SID pipe and
    /// `%LOCALAPPDATA%` data root.
    #[cfg(windows)]
    #[test]
    fn windows_args_are_empty_without_the_harness_envs() {
        assert!(windows_agent_args(None, None).is_empty());
        // Blank is not set — the same reading the connect side uses, or the
        // spawner would pass `--pipe-name \\.\pipe\` and the agent would serve
        // a name no client resolves.
        assert!(windows_agent_args(Some(OsStr::new("")), Some(OsStr::new(""))).is_empty());
    }

    /// Under a harness launch both isolations are forwarded, the pipe leaf
    /// translated to the full Win32 name the agent's `--pipe-name` expects.
    #[cfg(windows)]
    #[test]
    fn windows_args_forward_the_harness_pipe_and_data_dir() {
        let args = windows_agent_args(
            Some(OsStr::new("fauna-sync-e2e-tui-4711")),
            Some(OsStr::new(r"C:\tmp\launch\sync-agent")),
        );
        assert_eq!(
            args,
            vec![
                std::ffi::OsString::from("--pipe-name"),
                std::ffi::OsString::from(r"\\.\pipe\fauna-sync-e2e-tui-4711"),
                std::ffi::OsString::from("--data-dir"),
                std::ffi::OsString::from(r"C:\tmp\launch\sync-agent"),
            ]
        );
    }

    /// Each isolation stands alone — a launch may pin the pipe without moving
    /// the data dir, and vice versa.
    #[cfg(windows)]
    #[test]
    fn windows_args_forward_either_isolation_independently() {
        assert_eq!(
            windows_agent_args(Some(OsStr::new("leaf")), None),
            vec![
                std::ffi::OsString::from("--pipe-name"),
                std::ffi::OsString::from(r"\\.\pipe\leaf"),
            ]
        );
        assert_eq!(
            windows_agent_args(None, Some(OsStr::new(r"C:\d"))),
            vec![
                std::ffi::OsString::from("--data-dir"),
                std::ffi::OsString::from(r"C:\d"),
            ]
        );
    }

    // -- macos: the LaunchAgent arm (the Swift LaunchdSyncAgentSpawner lift) --

    #[cfg(target_os = "macos")]
    const BUNDLED: &str = "/Applications/Fauna.app/Contents/MacOS/fauna-sync-agent";

    /// The exact bytes, pinned whole: the `.pkg`'s sync postinstall writes this
    /// same text, so a drift here makes every `.pkg` box re-bootstrap its agent
    /// on the first tick — and a drift there is caught by
    /// `test_apple_identifier_pins.py`'s byte comparison against this shape.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plist_is_the_pkg_shape_byte_for_byte() {
        let plist =
            launchd_plist_contents(LAUNCH_AGENT_LABEL, std::path::Path::new(BUNDLED)).unwrap();
        assert_eq!(
            plist,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"\n\
             \x20 \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20   <key>Label</key>\n\
             \x20   <string>social.fauna.sync-agent</string>\n\
             \x20   <key>ProgramArguments</key>\n\
             \x20   <array>\n\
             \x20       <string>/Applications/Fauna.app/Contents/MacOS/fauna-sync-agent</string>\n\
             \x20   </array>\n\
             \x20   <key>RunAtLoad</key>\n\
             \x20   <true/>\n\
             \x20   <key>KeepAlive</key>\n\
             \x20   <true/>\n\
             </dict>\n\
             </plist>\n"
        );
    }

    /// launchd never rotates a stdout/stderr redirect (one reached 6.6 GB), and
    /// the agent writes its own size-capped log — so neither key is ever written.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plist_carries_no_launchd_log_redirect() {
        let plist =
            launchd_plist_contents(LAUNCH_AGENT_LABEL, std::path::Path::new(BUNDLED)).unwrap();
        assert!(!plist.contains("StandardOutPath"));
        assert!(!plist.contains("StandardErrorPath"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_space_and_ampersand_path_is_escaped_not_split() {
        let plist = launchd_plist_contents(
            LAUNCH_AGENT_LABEL,
            std::path::Path::new("/Users/dev/My Tools & <Bins>/fauna-sync-agent"),
        )
        .unwrap();
        assert!(plist.contains(
            "<string>/Users/dev/My Tools &amp; &lt;Bins&gt;/fauna-sync-agent</string>\n"
        ));
    }

    /// A control character is invalid XML 1.0 text and a non-UTF-8 name has no
    /// honest rendering: refuse, never sanitize into a plist naming a different
    /// file (the systemd arm's `UnitExec` rule).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_path_the_plist_cannot_carry_yields_no_plist() {
        use std::os::unix::ffi::OsStrExt;
        assert!(
            launchd_plist_contents(LAUNCH_AGENT_LABEL, std::path::Path::new("/tmp/x\u{1}y"))
                .is_none()
        );
        let non_utf8 = std::path::Path::new(OsStr::from_bytes(b"/tmp/\xff/fauna-sync-agent"));
        assert!(launchd_plist_contents(LAUNCH_AGENT_LABEL, non_utf8).is_none());
    }

    /// The archive's tui (`$PREFIX/bin`): the desktop app's bundled copy first —
    /// so a box with both apps keeps the bundle-attributed agent whichever wrote
    /// last — then tui's own sibling, then the `.pkg` copy, then PATH.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unbundled_writer_prefers_the_app_bundle_then_its_own_sibling() {
        let path_var = std::env::join_paths(["/opt/bin", "/Users/u/.local/bin"]).unwrap();
        assert_eq!(
            macos_agent_candidates(
                None,
                Some(std::path::Path::new("/Users/u/.local/bin")),
                std::path::Path::new(APP_BUNDLE_AGENT),
                std::path::Path::new(PKG_AGENT),
                Some(path_var.as_os_str()),
            ),
            vec![
                PathBuf::from(BUNDLED),
                PathBuf::from("/Users/u/.local/bin/fauna-sync-agent"),
                PathBuf::from("/usr/local/bin/fauna-sync-agent"),
                PathBuf::from("/opt/bin/fauna-sync-agent"),
            ],
            "the PATH entry equal to the sibling is not repeated"
        );
    }

    /// The desktop app writing from inside its own bundle (wherever the user put
    /// it): its own bundled copy IS the first candidate, not `/Applications`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bundled_writer_names_its_own_bundle_first() {
        let candidates = macos_agent_candidates(
            None,
            Some(std::path::Path::new(
                "/Users/u/Apps/Fauna.app/Contents/MacOS",
            )),
            std::path::Path::new(APP_BUNDLE_AGENT),
            std::path::Path::new(PKG_AGENT),
            None,
        );
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/Users/u/Apps/Fauna.app/Contents/MacOS/fauna-sync-agent"),
                PathBuf::from("/usr/local/bin/fauna-sync-agent"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_pinned_agent_is_the_only_macos_candidate() {
        let pinned = PathBuf::from("/nonexistent/target/debug/fauna-sync-agent");
        let candidates = macos_agent_candidates(
            Some(pinned.clone()),
            Some(std::path::Path::new("/Users/u/.local/bin")),
            std::path::Path::new(APP_BUNDLE_AGENT),
            std::path::Path::new(PKG_AGENT),
            None,
        );
        assert_eq!(candidates, vec![pinned.clone()]);
        // Verbatim even though it does not exist: a pin never falls through.
        assert_eq!(resolve_macos_agent(&candidates, true), Some(pinned));
    }

    /// The order over a real temp layout: each candidate wins only once every
    /// earlier one is absent or not executable.
    #[cfg(target_os = "macos")]
    #[test]
    fn resolution_walks_the_order_over_a_real_layout() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let mk = |rel: &str| {
            let p = root.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let bundled = root
            .path()
            .join("Applications/Fauna.app/Contents/MacOS/fauna-sync-agent");
        let prefix = root.path().join("prefix/bin");
        let sibling = prefix.join(AGENT_BIN_NAME);
        let pkg = root.path().join("usr/local/bin/fauna-sync-agent");
        let on_path = root.path().join("path/fauna-sync-agent");
        let path_var = std::env::join_paths([root.path().join("path")]).unwrap();
        let resolve = || {
            resolve_macos_agent(
                &macos_agent_candidates(
                    None,
                    Some(&prefix),
                    &bundled,
                    &pkg,
                    Some(path_var.as_os_str()),
                ),
                false,
            )
        };

        assert_eq!(resolve(), None, "nothing installed → nothing to launch");
        mk("path/fauna-sync-agent");
        assert_eq!(resolve(), Some(on_path.clone()));
        mk("usr/local/bin/fauna-sync-agent");
        assert_eq!(resolve(), Some(pkg.clone()));
        mk("prefix/bin/fauna-sync-agent");
        assert_eq!(resolve(), Some(sibling.clone()));
        // A non-executable bundled copy is skipped, an executable one wins.
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(&bundled, b"").unwrap();
        assert_eq!(resolve(), Some(prefix.join(AGENT_BIN_NAME)));
        mk("Applications/Fauna.app/Contents/MacOS/fauna-sync-agent");
        assert_eq!(resolve(), Some(bundled.clone()));
    }

    #[cfg(target_os = "macos")]
    #[derive(Default)]
    struct RecordingLaunchctl {
        calls: Mutex<Vec<String>>,
    }

    #[cfg(target_os = "macos")]
    impl RecordingLaunchctl {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    #[cfg(target_os = "macos")]
    impl LaunchctlControl for RecordingLaunchctl {
        fn bootstrap(&self, _plist: &std::path::Path) -> bool {
            self.calls.lock().unwrap().push("bootstrap".into());
            false
        }
        fn bootout(&self, label: &str) -> bool {
            self.calls.lock().unwrap().push(format!("bootout {label}"));
            true
        }
        fn kickstart(&self, label: &str) -> bool {
            self.calls
                .lock()
                .unwrap()
                .push(format!("kickstart {label}"));
            true
        }
    }

    /// Missing → written + loaded; current → left alone, re-kicked; a different
    /// target or an old redirect → booted out FIRST (launchd runs the loaded
    /// definition, not the file), rewritten, re-loaded.
    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_writes_then_keeps_then_heals_the_plist() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("LaunchAgents").join("probe.plist");
        let ctl = RecordingLaunchctl::default();
        let first = std::path::Path::new("/Users/u/.local/bin/fauna-sync-agent");

        assert!(ensure_launch_agent("probe", first, &plist, &ctl));
        assert_eq!(ctl.take(), ["bootstrap", "kickstart probe"]);
        let written = std::fs::read_to_string(&plist).unwrap();
        assert_eq!(
            Some(written.clone()),
            launchd_plist_contents("probe", first)
        );

        assert!(ensure_launch_agent("probe", first, &plist, &ctl));
        assert_eq!(
            ctl.take(),
            ["bootstrap", "kickstart probe"],
            "current: no bootout"
        );
        assert_eq!(std::fs::read_to_string(&plist).unwrap(), written);

        let moved = std::path::Path::new(BUNDLED);
        assert!(ensure_launch_agent("probe", moved, &plist, &ctl));
        assert_eq!(
            ctl.take(),
            ["bootout probe", "bootstrap", "kickstart probe"]
        );
        assert_eq!(
            Some(std::fs::read_to_string(&plist).unwrap()),
            launchd_plist_contents("probe", moved)
        );

        // An older `.pkg`'s redirect-carrying plist, same target: healed too.
        let with_redirect = launchd_plist_contents("probe", moved).unwrap().replace(
            "</dict>",
            "    <key>StandardOutPath</key>\n    <string>/tmp/a.log</string>\n</dict>",
        );
        std::fs::write(&plist, with_redirect).unwrap();
        assert!(ensure_launch_agent("probe", moved, &plist, &ctl));
        assert_eq!(
            ctl.take(),
            ["bootout probe", "bootstrap", "kickstart probe"]
        );
        assert!(
            !std::fs::read_to_string(&plist)
                .unwrap()
                .contains("StandardOutPath")
        );
    }

    /// A path the plist cannot carry writes nothing and drives nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_refuses_without_touching_launchd() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("probe.plist");
        let ctl = RecordingLaunchctl::default();
        assert!(!ensure_launch_agent(
            "probe",
            std::path::Path::new("/tmp/x\ny"),
            &plist,
            &ctl
        ));
        assert!(ctl.take().is_empty());
        assert!(!plist.exists());
    }

    /// The REAL launchd, with a throwaway label and a stub agent — never the
    /// real `social.fauna.sync-agent` job. Ignored by default: it mutates the
    /// logged-in user's `gui/<uid>` domain, so it runs only behind the
    /// real-session category's gates
    /// (`tests/e2e-unified/tests/real_session/test_sync_agent_launch_agent.py`,
    /// which passes the stub dir in `FAUNA_REAL_LAUNCHD_PROBE_DIR`).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "real launchd: run through the real_session pytest"]
    fn real_launchd_loads_heals_and_boots_out_the_probe_job() {
        use std::os::unix::fs::PermissionsExt;
        let dir = PathBuf::from(
            std::env::var_os("FAUNA_REAL_LAUNCHD_PROBE_DIR")
                .expect("run through test_sync_agent_launch_agent.py"),
        );
        let label = format!("social.fauna.sync-agent-probe-{}", std::process::id());
        let plist = dir.join(format!("{label}.plist"));
        let stub = |name: &str| {
            let p = dir.join(name).join(AGENT_BIN_NAME);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"#!/bin/sh\nexec /bin/sleep 300\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let print = || {
            let out = Command::new("/bin/launchctl")
                .args(["print", &format!("gui/{}/{label}", getuid())])
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let first = stub("one");
        let second = stub("two");

        let result = std::panic::catch_unwind(|| {
            assert!(ensure_launch_agent(&label, &first, &plist, &Launchctl));
            let (loaded, text) = print();
            assert!(loaded, "the probe job must be loaded after ensure");
            assert!(text.contains(&first.display().to_string()), "{text}");

            // Steady state: a second ensure is harmless (bootstrap fails, kick ok).
            assert!(ensure_launch_agent(&label, &first, &plist, &Launchctl));
            assert!(print().0);

            // Heal: the loaded definition follows the rewritten file.
            assert!(ensure_launch_agent(&label, &second, &plist, &Launchctl));
            let (loaded, text) = print();
            assert!(loaded);
            assert!(text.contains(&second.display().to_string()), "{text}");
            assert!(!text.contains(&first.display().to_string()), "{text}");

            assert!(Launchctl.bootout(&label));
            assert!(!print().0, "booted out → gone from gui/<uid>");
        });
        let _ = Launchctl.bootout(&label);
        let _ = std::fs::remove_file(&plist);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    // -- unit shape (the uninstall-inertness decision, the two-line guard) --

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn native_unit_execs_quoted_binary_with_matching_condition() {
        let exec =
            UnitExec::for_binary(PathBuf::from("/usr/bin/fauna-sync-agent")).expect("honest path");
        let unit = unit_contents(&exec);
        assert!(unit.contains("ExecStart=\"/usr/bin/fauna-sync-agent\"\n"));
        assert!(unit.contains("ConditionFileIsExecutable=/usr/bin/fauna-sync-agent\n"));
        // Re-checked on every auto-restart (Condition*= is not) — the anti-flap
        // guard, see `unit_contents`.
        assert!(unit.contains("ExecCondition=/usr/bin/test -x \"/usr/bin/fauna-sync-agent\"\n"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_space_containing_binary_path_stays_one_quoted_arg() {
        let exec = UnitExec::for_binary(PathBuf::from("/opt/My Apps/fauna-sync-agent"))
            .expect("honest path");
        let unit = unit_contents(&exec);
        assert!(unit.contains("ExecStart=\"/opt/My Apps/fauna-sync-agent\"\n"));
        assert!(
            unit.contains("ExecCondition=/usr/bin/test -x \"/opt/My Apps/fauna-sync-agent\"\n")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unit_keeps_the_ratified_lifecycle_shape() {
        // sync-agent.md § Packaging + lifecycle (linux): Restart=always,
        // WantedBy=default.target.
        let exec =
            UnitExec::for_binary(PathBuf::from("/usr/bin/fauna-sync-agent")).expect("honest path");
        let unit = unit_contents(&exec);
        assert!(unit.contains("Restart=always\n"));
        assert!(unit.contains("WantedBy=default.target\n"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_raw_exec_start_passes_through_new() {
        // The escape hatch the linux AppImage/Flatpak variants build on.
        let exec = UnitExec::new(
            "/usr/bin/flatpak run --command=fauna-sync-agent social.fauna.fauna".into(),
            PathBuf::from(
                "/var/lib/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent",
            ),
        )
        .expect("honest inputs");
        let unit = unit_contents(&exec);
        assert!(unit.contains(
            "ExecStart=/usr/bin/flatpak run --command=fauna-sync-agent social.fauna.fauna\n"
        ));
    }

    // -- unit-injection refusal --
    //
    // A systemd unit is a structured format whose record separator is the
    // newline, so an env-derived value carrying one does not corrupt its own
    // directive — it INVENTS directives, in whatever section it lands in. The
    // quoting `for_binary`/`unit_exec_for_appimage` already had defends the
    // ARGUMENT axis (spaces); these pin the DIRECTIVE axis. Refusal, not
    // sanitization: a stripped path would still be written, then compared
    // against the honest one by `ensure_unit_running`, i.e. a silently-wrong
    // unit — `ensure_unit_running` writes with `Restart=always` +
    // `WantedBy=default.target`, so a mangled or poisoned unit is a persistent,
    // self-restarting foothold that outlives the environment that produced it.

    /// Every line the honest template writes, so a test can assert that a
    /// hostile input added none of its own.
    #[cfg(all(unix, not(target_os = "macos")))]
    const HONEST_DIRECTIVES: &[&str] = &[
        "[Unit]",
        "Description=",
        "After=",
        "Wants=",
        "ConditionFileIsExecutable=",
        "[Service]",
        "Type=",
        "ExecCondition=",
        "ExecStart=",
        "Restart=",
        "RestartSec=",
        "Environment=",
        "[Install]",
        "WantedBy=",
    ];

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_newline_bearing_binary_path_yields_no_unit_at_all() {
        // The exact probe shape from the report: a pinned path whose
        // newline appends an ExecStartPre the caller never wrote.
        assert!(
            UnitExec::for_binary(PathBuf::from("/tmp/x\nExecStartPre=/tmp/evil")).is_none(),
            "a control character in the binary path must yield NO UnitExec — a \
             sanitized one would still be written and then compared as if honest"
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_newline_bearing_raw_exec_start_yields_no_unit_at_all() {
        // The AppImage-shaped caller (`UnitExec::new`), same attack.
        assert!(
            UnitExec::new(
                "\"/tmp/x\nExecStartPre=/tmp/evil\" --sync-agent".into(),
                PathBuf::from("/tmp/x"),
            )
            .is_none()
        );
        // …and via the condition path alone, which lands in [Unit].
        assert!(
            UnitExec::new(
                "\"/tmp/x\"".into(),
                PathBuf::from("/tmp/x\nExecStartPre=/tmp/evil"),
            )
            .is_none()
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn no_constructible_unit_carries_a_directive_the_caller_did_not_write() {
        // The general property, stated over the whole template rather than over
        // the one directive the probe happened to use: for every hostile input,
        // EITHER construction is refused, or the rendered unit contains no line
        // that opens a directive outside HONEST_DIRECTIVES. Carriage return and
        // NUL are covered alongside newline — systemd's parser splits on \r too.
        for hostile in [
            "/tmp/x\nExecStartPre=/tmp/evil",
            "/tmp/x\rExecStartPre=/tmp/evil",
            "/tmp/x\n[Service]\nExecStart=/tmp/evil",
            "/tmp/x\u{0}ExecStartPre=/tmp/evil",
        ] {
            for exec in [
                UnitExec::for_binary(PathBuf::from(hostile)),
                UnitExec::new(
                    format!("\"{hostile}\" --sync-agent"),
                    PathBuf::from("/tmp/x"),
                ),
            ]
            .into_iter()
            .flatten()
            {
                let unit = unit_contents(&exec);
                for line in unit.lines().map(str::trim_start) {
                    assert!(
                        line.is_empty() || HONEST_DIRECTIVES.iter().any(|d| line.starts_with(d)),
                        "hostile input {hostile:?} produced a unit line the caller never \
                         wrote: {line:?}\n--- full unit ---\n{unit}"
                    );
                }
            }
        }
    }

    // -- PATH fallback resolution (units need an absolute exec target for the
    //    Condition line to mean anything) --

    #[test]
    fn path_lookup_resolves_the_agent_to_an_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        // AGENT_BIN_NAME, not a literal: on windows the file the PATH walk must
        // find is `fauna-sync-agent.exe`.
        let agent = dir.path().join(AGENT_BIN_NAME);
        std::fs::write(&agent, b"#!/bin/sh\n").unwrap();
        let missing = PathBuf::from(if cfg!(windows) {
            r"C:\nonexistent"
        } else {
            "/nonexistent"
        });
        let path_var = std::env::join_paths([missing, dir.path().into()]).unwrap();
        assert_eq!(path_lookup(Some(path_var.as_os_str())), Some(agent));
        assert_eq!(path_lookup(None), None);
    }

    // -- child spawn (e2e + SandboxChild): the spawner-side belt against
    //    duplicate agents. The kernel-arbitrated InstanceLock in
    //    fauna-ipc::unix_transport is the correctness guard; this gate just
    //    avoids spawning a doomed duplicate every tick while a child is still
    //    coming up --

    /// A child that outlives the assertion, so the live-child gate is exercised
    /// rather than racing an exit. Per-OS because the gate matters on both — it
    /// is what stops a convergence tick spawning a sibling agent every 30 s
    /// while a slow one is still binding.
    fn long_lived_cmd() -> Command {
        #[cfg(unix)]
        {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        }
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/c", "ping", "-n", "30", "127.0.0.1"]);
            c.creation_flags(CREATE_NO_WINDOW);
            c
        }
    }

    /// A child that exits immediately, for the reap-then-respawn case.
    fn short_lived_cmd() -> Command {
        #[cfg(unix)]
        {
            Command::new("true")
        }
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/c", "exit"]);
            c.creation_flags(CREATE_NO_WINDOW);
            c
        }
    }

    fn kill_children(spawner: &ChildSpawner) {
        for c in spawner.children.lock().unwrap().iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    #[test]
    fn spawn_child_does_not_double_spawn_while_a_child_is_live() {
        let spawner = ChildSpawner::new();
        spawner.spawn_command(long_lived_cmd());
        spawner.spawn_command(long_lived_cmd());
        assert_eq!(
            spawner.children.lock().unwrap().len(),
            1,
            "a live (possibly not-yet-bound) child must gate further spawns"
        );
        kill_children(&spawner);
    }

    #[test]
    fn spawn_child_reaps_exited_children_and_respawns() {
        let spawner = ChildSpawner::new();
        spawner.spawn_command(short_lived_cmd());
        {
            // Deterministically get the first child to "exited" before the
            // second spawn attempt.
            let mut children = spawner.children.lock().unwrap();
            let _ = children[0].wait();
        }
        spawner.spawn_command(long_lived_cmd());
        {
            let mut children = spawner.children.lock().unwrap();
            assert_eq!(children.len(), 1, "exited child reaped, new one spawned");
            assert!(
                matches!(children[0].try_wait(), Ok(None)),
                "the survivor must be the live child, not the exited one"
            );
        }
        kill_children(&spawner);
    }
}
