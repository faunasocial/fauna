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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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

#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
pub struct SystemctlUser;

#[cfg(unix)]
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

#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
pub struct LoginctlUser;

#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
pub struct SystemdUserUnitSpawner {
    e2e: bool,
    child: ChildSpawner,
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // -- unit shape (the uninstall-inertness decision, the two-line guard) --

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
