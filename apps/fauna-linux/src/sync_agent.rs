//! The GTK client's control surface for the external per-user
//! **`fauna-sync-agent`** — the A3 linux cutover (`sync-agent.md` § Control
//! plane split + § Packaging + lifecycle (linux) + § Credential model; plan
//! D3/D7-linux).
//!
//! The app no longer hosts resident sync engines ([`crate::sync::SyncDriver`]
//! is retired): at post-auth it [`install`]s this module, which
//!
//! 1. ensures the agent runs — production: a **systemd user unit**
//!    `fauna-sync-agent.service` self-installed at first post-auth (the
//!    autostart-`.desktop` shape, [`SystemdAgentSpawner`]; per-channel exec
//!    forms + the sandboxed-channel child fallback: [`LaunchChannel`] and
//!    `installers/linux-desktop.md` § Installation Files); e2e: a
//!    direct-spawned child process inheriting the launch's isolated
//!    `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR` world (testing.md § point 10 — the
//!    machine-global systemd/user-unit surface is never touched under e2e, but
//!    the *real* agent binary still runs, so sync e2e exercises the production
//!    path);
//! 2. runs the shared provisioning convergence loop
//!    (`fauna_client_sync::agent::SyncAgentProvisioner` — the linux GTK client
//!    links the Rust client directly, no FFI hop), which mints + registers the
//!    `RenewBearer` device grant and keeps the agent provisioned;
//! 3. reroutes the folder-binding UI over the agent socket
//!    (`bind_location`/`unbind_location`/`list_locations`) through the shared
//!    [`LocationBindingsModel`] — optimistic rows, union reconcile re-driven on
//!    every agent-reachable edge (never only once at attach);
//! 4. subscribes to the agent's pushed events (the shared
//!    [`fauna_ipc::events::spawn_event_listener`] loop) and re-surfaces
//!    per-file completed syncs as a desktop notification — the consumer
//!    retired with the in-app `SyncDriver`, resurfaced over the socket
//!    instead of an in-process channel.
//!
//! Sign-out / account-switch / factory-reset / e2e-reset call [`teardown`],
//! which unprovisions (the agent deletes its persisted capability and stops
//! engines). A plain app quit does **not** — always-running sync is the whole
//! point of the agent.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::ToVariant;

use fauna_client_sync::agent::PendingBind;
use fauna_client_sync::agent::{
    AgentCapabilityInputs, AgentSpawner, BindingRow, BindingState, LocationBindingsModel,
    ModeToggle, ReachabilityObserver, SyncAgentProvisioner,
};
// The channel-agnostic spawner half (`sync-agent.md` § Packaging + lifecycle),
// lifted to shared Rust so fauna-tui reuses it. This module keeps only the
// linux-packaging-specific arms: `LaunchChannel` detection, the Flatpak
// host-unit seam, and the session-bus [`UnitControl`] impl.
use fauna_client_sync::agent_spawner::{
    self, ChildSpawner, SystemctlUser, UNIT_NAME, UnitControl, UnitExec,
};
use fauna_sync_engine::share_glue::AgentBearerSource;

use crate::client::FaunaClient;
use crate::sync::LocationBinding;

/// The concrete provisioner instantiation the linux app drives.
pub type LinuxAgentProvisioner =
    SyncAgentProvisioner<std::sync::Arc<fauna_client::NestClient>, AgentBearerSource>;

/// The device label every linux-registered sync device shows in the nest's
/// device list — moved here from the retired in-app `LinuxSyncSpec`.
const DEVICE_LABEL: &str = "fauna-linux";

thread_local! {
    /// The live agent surface, installed at post-auth on the GTK main thread.
    static AGENT: RefCell<Option<AgentUi>> = const { RefCell::new(None) };
}

struct AgentUi {
    provisioner: Arc<LinuxAgentProvisioner>,
    /// The shared optimistic-UI ⇄ agent-truth reconcile model
    /// (`fauna_client_sync::agent::LocationBindingsModel`). GTK-main-thread only.
    model: Rc<RefCell<LocationBindingsModel>>,
    /// The last repaint pushed to the Folders page, so a reconcile only repaints
    /// when the rendered list actually changed. A no-change repaint is not just
    /// wasted work — rebuilding the folder list destroys live widgets
    /// mid-interaction (an e2e `select` on a conflict-policy row 404'd exactly
    /// this way).
    ///
    /// The WHOLE row, not a `(path, folder)` projection: every field the rows
    /// carry is rendered somewhere (`access_revoked` →
    /// `folder-access-revoked-warning`, `deletes_held` →
    /// `folder-location-deletes-held`), so a projection is a list of the changes
    /// that silently fail to repaint. Growing the row is then automatically
    /// covered instead of quietly widening that list.
    last_rendered: RefCell<Vec<BindingRow>>,
    rt: tokio::runtime::Handle,
    /// Signals the background event-listener thread (per-file completed-sync
    /// notifications) to stop. Best-effort: the thread may be parked in a
    /// blocking `recv_event()` and only notices on its next wake (an event or
    /// a socket error), matching the rest of this module's teardown style.
    event_listener_stop: Arc<std::sync::atomic::AtomicBool>,
}

// ---------------------------------------------------------------------------
// Spawner: systemd user unit (production) / direct child spawn (e2e).
// ---------------------------------------------------------------------------

/// The systemd **user**-unit self-install + kick, on `autostart.rs`'s
/// post-auth-ensure model (`sync-agent.md` § Packaging + lifecycle (linux)).
/// Under e2e ([`crate::e2e_mode_enabled`]) it never touches systemd — it
/// direct-spawns the agent binary as a child that inherits this launch's
/// isolated XDG world, so the per-launch socket + data dirs hold by
/// construction.
pub struct SystemdAgentSpawner {
    e2e: bool,
    /// The reaping direct-child spawner (shared `agent_spawner::ChildSpawner`) —
    /// the e2e path and the production Flatpak/Snap `SandboxChild` residency.
    child: ChildSpawner,
}

/// The install channel this app process is running from — it decides how the
/// spawner keeps the agent alive (`installers/linux-desktop.md` § Installation
/// Files: the five channels reduce to these three lifecycle shapes).
#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchChannel {
    /// Native install (install.sh, deb, dev tree): the user unit execs the
    /// agent binary installed beside the app.
    NativeUnit,
    /// AppImage: the per-run FUSE mount path is throwaway, so the unit execs
    /// the stable `.AppImage` file itself with `--sync-agent` (AppRun
    /// dispatches to the bundled agent). Moving the file self-heals on the
    /// next app launch — the desired unit content changes, so the ensure path
    /// rewrites it.
    AppImageUnit { appimage: PathBuf },
    /// Flatpak: the always-on seam (`linux-desktop.md` § Flatpak). The sandbox
    /// writes a **host** systemd user unit exec'ing
    /// `flatpak run --command=fauna-sync-agent <app-id>` through the
    /// `xdg-config/systemd/user:create` filesystem grant, and enables/starts
    /// it over the user manager's D-Bus API (`org.freedesktop.systemd1` talk
    /// grant — `systemctl` binary calls don't cross the sandbox). The agent
    /// then runs in its own sandbox instance of the same app-id, reached over
    /// the instance-shared `$XDG_RUNTIME_DIR/app/<app-id>` socket
    /// (`fauna_ipc::unix_transport::default_socket_path`). If any step fails
    /// (grants missing from the install's overrides, no `.flatpak-info`
    /// app-path), the spawner falls back to the pre-seam interim: the bundled
    /// agent as a direct app child.
    FlatpakUnit { app_id: String },
    /// Snap: strict confinement offers no user-unit control surface, so the
    /// bundled agent runs as a direct app child — app-lifetime residency, the
    /// documented interim until snapd stabilizes `daemon-scope: user`
    /// (`linux-desktop.md` § Snap). Also the [`LaunchChannel::FlatpakUnit`]
    /// fallback when the host-unit seam fails.
    SandboxChild,
}

impl LaunchChannel {
    fn detect() -> Self {
        Self::from_env(
            std::env::var_os("FLATPAK_ID"),
            std::env::var_os("SNAP"),
            std::env::var_os("APPIMAGE"),
        )
    }

    fn from_env(
        flatpak_id: Option<std::ffi::OsString>,
        snap: Option<std::ffi::OsString>,
        appimage: Option<std::ffi::OsString>,
    ) -> Self {
        if let Some(id) = flatpak_id.filter(|id| !id.is_empty()) {
            return Self::FlatpakUnit {
                app_id: id.to_string_lossy().into_owned(),
            };
        }
        if snap.as_deref().is_some_and(|s| !s.is_empty()) {
            return Self::SandboxChild;
        }
        if let Some(path) = appimage.filter(|p| !p.is_empty()) {
            return Self::AppImageUnit {
                appimage: PathBuf::from(path),
            };
        }
        Self::NativeUnit
    }
}

// The generic `UnitExec` (`{exec_start, condition}` + the native `for_binary`
// constructor) is shared Rust (`agent_spawner::UnitExec`). Only the two
// linux-packaging-specific constructors stay here.

/// The AppImage user unit: exec the stable `.AppImage` file with `--sync-agent`
/// (AppRun dispatches to the bundled agent). Quoted — AppImage paths are
/// user-chosen and may contain spaces. `None` when `$APPIMAGE` carries a
/// control character (`UnitExec`'s directive-axis refusal — the quotes here
/// only defend the argument axis).
fn unit_exec_for_appimage(appimage: PathBuf) -> Option<UnitExec> {
    UnitExec::new(format!("\"{}\" --sync-agent", appimage.display()), appimage)
}

/// The Flatpak host unit: `flatpak run` re-enters a fresh sandbox instance of
/// the same app-id, whose `--command` runs the bundled agent. `condition` is
/// the *deployed* agent binary under the installation's stable `current/active`
/// symlink (derived from `.flatpak-info` — see [`flatpak_condition_target`]),
/// so `flatpak uninstall` condition-skips the unit exactly like a removed
/// native binary — `/usr/bin/flatpak` itself survives an uninstall and would
/// Restart-flap.
/// `app_id` is **quoted**: it comes from `$FLATPAK_ID`, and unquoted it splits
/// on a bare space into extra `flatpak run` arguments — an argument-axis bug
/// independent of the newline refusal `UnitExec::new` adds on the directive
/// axis. `None` when the id carries a control character.
fn unit_exec_for_flatpak(app_id: &str, condition: PathBuf) -> Option<UnitExec> {
    UnitExec::new(
        format!("/usr/bin/flatpak run --command=fauna-sync-agent \"{app_id}\""),
        condition,
    )
}

/// The host path of this sandbox's deployed app files, read from
/// `/.flatpak-info` (`[Instance] app-path=` — flatpak writes it into every
/// sandbox). This is a *per-commit* deploy dir; [`flatpak_condition_target`]
/// rewrites it to the installation's stable `current/active` link.
fn flatpak_info_app_path(info: &str) -> Option<PathBuf> {
    let mut in_instance = false;
    for line in info.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[') {
            in_instance = section.trim_end_matches(']') == "Instance";
            continue;
        }
        if in_instance && let Some(value) = line.strip_prefix("app-path=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

/// The `ConditionFileIsExecutable` target for the Flatpak host unit: the
/// deployed agent binary under the installation's **stable** deploy link,
/// `<installation>/app/<app-id>/current/active/files/bin/fauna-sync-agent`.
///
/// Derived by truncating the per-commit `app-path`
/// (`<installation>/app/<app-id>/<arch>/<branch>/<commit>/files`) at its
/// `app/<app-id>` segment — works for both system (`/var/lib/flatpak`) and
/// user (`~/.local/share/flatpak`) installations without knowing which one
/// this is. The per-commit path itself would go stale on every `flatpak
/// update`, condition-skipping a healthy install until the next app launch
/// healed the unit; `current/active` survives updates and disappears on
/// uninstall — exactly the semantics the condition line needs.
fn flatpak_condition_target(app_path: &Path, app_id: &str) -> Option<PathBuf> {
    let mut components = app_path.components().peekable();
    let mut prefix = PathBuf::new();
    while let Some(component) = components.next() {
        prefix.push(component);
        // Match the `app/<app-id>` segment pair; a parent dir literally named
        // `app` without our id right after it just keeps scanning deeper.
        if component.as_os_str() == "app"
            && components.peek().is_some_and(|id| id.as_os_str() == app_id)
        {
            return Some(
                prefix
                    .join(app_id)
                    .join("current/active/files/bin/fauna-sync-agent"),
            );
        }
    }
    None
}

/// The Flatpak channel's [`UnitControl`]: the `systemctl` binary can't cross
/// the sandbox, so the reload/enable/start verbs go over the session bus to
/// `org.freedesktop.systemd1` — the manifest's `--talk-name` grant. Every
/// failure reports `false` so the caller can fall back to app-child residency.
/// (Native/AppImage use the shared [`SystemctlUser`] instead.)
struct SessionDbus;

impl UnitControl for SessionDbus {
    fn reload_enable_start(&self) -> bool {
        Self::dbus_manager_call("Reload", None)
            && Self::dbus_manager_call(
                "EnableUnitFiles",
                // (unit names, runtime-only=false, force=true) — the
                // `systemctl enable --force` shape, replacing any dangling
                // symlink from a prior install.
                Some(&(vec![UNIT_NAME.to_string()], false, true).to_variant()),
            )
            && Self::start_unit_over_dbus()
    }

    fn start(&self) -> bool {
        Self::start_unit_over_dbus()
    }
}

impl SessionDbus {
    fn start_unit_over_dbus() -> bool {
        Self::dbus_manager_call("StartUnit", Some(&(UNIT_NAME, "replace").to_variant()))
    }

    /// One `org.freedesktop.systemd1.Manager` method call on the session bus,
    /// `false` (with a warn) on any failure — including the no-grant case,
    /// where the sandbox's D-Bus proxy rejects the destination.
    fn dbus_manager_call(method: &str, params: Option<&glib::Variant>) -> bool {
        let conn = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!("session bus unavailable for systemd1 {method}: {e}");
                return false;
            }
        };
        match conn.call_sync(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            method,
            params,
            None,
            gio::DBusCallFlags::NONE,
            10_000,
            gio::Cancellable::NONE,
        ) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("systemd1 {method} over the session bus failed: {e}");
                false
            }
        }
    }
}

impl SystemdAgentSpawner {
    pub fn new() -> Self {
        Self {
            e2e: crate::e2e_mode_enabled(),
            child: ChildSpawner::new(),
        }
    }

    /// The **host** unit path as seen from inside the Flatpak sandbox:
    /// `$HOME/.config/systemd/user/fauna-sync-agent.service`. Deliberately not
    /// [`agent_spawner::unit_path`] — the sandbox redirects `XDG_CONFIG_HOME` to
    /// `~/.var/app/<app-id>/config` (app-private, invisible to the host's
    /// systemd), while the `xdg-config/systemd/user:create` grant mounts the
    /// host's real `~/.config/systemd/user` at its host-side path, which under
    /// the (default) host config root is `$HOME/.config/systemd/user`.
    /// `$HOME` inside the sandbox is the real host home.
    fn flatpak_host_unit_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("systemd")
                .join("user")
                .join(UNIT_NAME),
        )
    }

    /// The Flatpak always-on seam (`linux-desktop.md` § Flatpak): write/heal
    /// the **host** user unit through the `xdg-config/systemd/user:create`
    /// grant and drive it over the session bus. Every failure mode — no
    /// `$HOME`, no readable `.flatpak-info` `app-path`, unit write refused
    /// (grant missing), D-Bus rejected (talk grant missing) — falls back to
    /// the pre-seam interim: the bundled agent as a direct app child, so a
    /// grant-less install still syncs while the app runs.
    fn ensure_flatpak_unit(&self, app_id: &str) {
        let ensured = (|| {
            let unit_path = Self::flatpak_host_unit_path()?;
            let info = std::fs::read_to_string("/.flatpak-info").ok()?;
            let app_path = flatpak_info_app_path(&info)?;
            let condition = flatpak_condition_target(&app_path, app_id)?;
            // A refused (control-char-bearing) FLATPAK_ID takes the same
            // fallback as a missing grant: no unit, app-child residency.
            let exec = unit_exec_for_flatpak(app_id, condition)?;
            Some(agent_spawner::ensure_unit_running(
                &exec,
                &unit_path,
                &SessionDbus,
            ))
        })()
        .unwrap_or(false);
        if !ensured {
            tracing::warn!(
                "flatpak host-unit seam unavailable (missing sandbox grants, or no \
                 .flatpak-info app-path) — falling back to app-child agent residency"
            );
            self.child.spawn();
        }
    }
}

impl Default for SystemdAgentSpawner {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentSpawner for SystemdAgentSpawner {
    fn spawn_agent(&self) {
        if self.e2e {
            self.child.spawn();
            return;
        }
        match LaunchChannel::detect() {
            LaunchChannel::SandboxChild => self.child.spawn(),
            LaunchChannel::FlatpakUnit { app_id } => self.ensure_flatpak_unit(&app_id),
            LaunchChannel::AppImageUnit { appimage } => {
                // Refusal already logged by UnitExec; no unit is written and
                // the convergence loop retries next tick.
                if let Some(exec) = unit_exec_for_appimage(appimage) {
                    agent_spawner::ensure_unit_running(
                        &exec,
                        &agent_spawner::unit_path(),
                        &SystemctlUser,
                    );
                }
            }
            LaunchChannel::NativeUnit => {
                match agent_spawner::agent_binary_absolute().and_then(UnitExec::for_binary) {
                    Some(exec) => {
                        agent_spawner::ensure_unit_running(
                            &exec,
                            &agent_spawner::unit_path(),
                            &SystemctlUser,
                        );
                    }
                    None => tracing::warn!(
                        "fauna-sync-agent not found beside the app or on PATH — \
                         cannot install the user unit (next convergence tick retries)"
                    ),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Bearer source: the AuthClient's shared bearer cache.
// ---------------------------------------------------------------------------

/// Forwards the convergence loop's agent-reachable edge (a tokio thread) onto
/// the GTK main loop as a [`reconcile`] — the re-fire that closes the
/// attach-time race (the 2026-07-19 A4 review finding; linux inherits it
/// identically).
struct GlibReachabilityObserver;

impl ReachabilityObserver for GlibReachabilityObserver {
    fn on_agent_reachable(&self) {
        glib::idle_add_once(reconcile);
    }
}

// ---------------------------------------------------------------------------
// Event listener: per-file completed-sync desktop notification
// (sync-agent.md § Implementation status — the consumer retired with the
// in-app `SyncDriver` at the A3 cutover, resurfaced from the agent's pushed
// events over the socket). Filter + blocking loop + self-healing reconnect
// are the shared `fauna_ipc::events` module (FaunaKit runs the same loop via
// its UniFFI wrapper); only the GTK notification hop lives here.
// ---------------------------------------------------------------------------

/// Spawn the shared event listener with the GTK notification surface: every
/// completed-sync basename hops to the main thread and posts the desktop
/// notification. Spawned once at [`install`]; stopped at [`teardown`].
fn spawn_event_listener() -> Arc<std::sync::atomic::AtomicBool> {
    match fauna_ipc::events::spawn_event_listener(|filename| {
        glib::idle_add_once(move || crate::notifications::notify_sync_complete(&filename));
    }) {
        Ok(stop) => stop,
        Err(e) => {
            tracing::error!("sync-events listener thread failed to spawn: {e}");
            // Already-set flag: teardown's stop() is a no-op on the dead listener.
            Arc::new(std::sync::atomic::AtomicBool::new(true))
        }
    }
}

// ---------------------------------------------------------------------------
// Install / teardown (post-auth hook + the four teardown paths).
// ---------------------------------------------------------------------------

/// Post-auth hook: build + start the provisioner (registers the `RenewBearer`
/// grant, then converges: probe → spawn-if-absent → `RefreshBearer` → full
/// `ProvisionCapability`) and start the folder model empty (every row arrives
/// by the agent reconcile's adoption or a user add). GTK main thread.
pub fn install(fauna_client: &Rc<FaunaClient>) {
    let device_id = match crate::sync::device_id() {
        Ok(id) => fauna_core::hex32::encode(&id),
        Err(e) => {
            tracing::error!("sync agent: could not establish device id: {e}");
            return;
        }
    };
    let seed = fauna_client.secret_bytes();
    let backup_key = fauna_core::crypto::BackupKey::derive(&seed)
        .to_bytes()
        .to_vec();
    // `FaunaClient::predecessor_backup_keys()` — the SAME cached resolution
    // `client.rs::label_custody` reads, not a second independent walk of
    // `AccountRegistry::predecessor_backup_keys` (`sync-agent.md` § Credential
    // model → *Retired owner keys after an identity succession*; two walks could observe different registry states).
    // Mirrors tui's post-auth hook.
    let predecessor_backup_keys = fauna_client
        .predecessor_backup_keys()
        .iter()
        .map(|k| k.to_bytes().to_vec())
        .collect();
    // The same walk's ATTESTED actor ids — the agent's `prior` for the
    // runtime it hosts app-dead (`account-data-taxonomy.md` § The generation
    // machinery → *The source of `prior`*); the cached resolution
    // `account_runtime::install` reads too, so this process and the agent
    // hand the fleet view one list.
    let predecessor_actor_ids = fauna_client
        .attested_predecessors()
        .actor_ids()
        .iter()
        .map(|id| id.0.to_vec())
        .collect();
    let nest = std::sync::Arc::clone(fauna_client.nest_rpc());
    let nest_url = nest.nest_url();
    let bearer_source = Arc::new(AgentBearerSource(fauna_client.build_sync_auth().bearer()));

    let provisioner = match SyncAgentProvisioner::new(
        nest,
        AgentCapabilityInputs {
            identity_secret: seed.to_vec(),
            backup_key,
            predecessor_backup_keys,
            predecessor_actor_ids,
            // The same walk's keys paired with their identities — the
            // per-signer bound's input on the agent (ruling (8)(c)).
            predecessor_keys_by_actor: fauna_client
                .predecessor_chain()
                .iter()
                .map(|(id, key)| (id.0.to_vec(), key.to_bytes().to_vec()))
                .collect(),
            device_id,
            device_label: DEVICE_LABEL.to_string(),
            nest_url,
        },
        Arc::new(SystemdAgentSpawner::new()),
        bearer_source,
        Some(Arc::new(GlibReachabilityObserver)),
    ) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            tracing::error!("sync agent provisioner build failed: {e}");
            return;
        }
    };
    // The devices page's participation switch asks the agent — the usual
    // engine holder — for the pass that reads the row (`p2p.md` § Per-device
    // participation, (c)); weak, so it lapses with this session's provisioner.
    {
        use fauna_client_account_runtime::p2p_participation::{
            EngineHolderNudge, EngineHolderNudgeSlot,
        };
        let holder_nudge: Arc<dyn EngineHolderNudge> = provisioner.clone();
        EngineHolderNudgeSlot::seat().publish(&holder_nudge);
    }

    // Every row arrives by the agent reconcile's adoption or a user add.
    let model = Rc::new(RefCell::new(LocationBindingsModel::default()));

    let rt = fauna_client.runtime_handle();
    {
        let provisioner = Arc::clone(&provisioner);
        rt.spawn(async move {
            if let Err(e) = provisioner.start().await {
                tracing::error!("sync agent provisioner start failed: {e}");
            }
        });
    }

    let last_rendered = RefCell::new(model.borrow().rendered());
    let event_listener_stop = spawn_event_listener();

    AGENT.with(|cell| {
        *cell.borrow_mut() = Some(AgentUi {
            provisioner,
            model,
            last_rendered,
            rt,
            event_listener_stop,
        })
    });
}

/// Snapshot the provisioner + its tokio runtime handle, if installed — the
/// same pair `reconcile()` reads. `app.rs`'s `sync-agent-status` poll uses
/// this to reach `get_service_status()` without owning the agent surface
/// itself (sync_agent.rs owns the provisioner; app.rs owns the sidebar's
/// `WidgetHandles` the poll result renders into).
pub fn provisioner_and_runtime() -> Option<(Arc<LinuxAgentProvisioner>, tokio::runtime::Handle)> {
    AGENT.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|a| (Arc::clone(&a.provisioner), a.rt.clone()))
    })
}

/// An agent un-provision already under way: the tokio runtime it was spawned
/// on, and the wait for its reply — what [`teardown`] hands the account
/// runtime's stop to sequence the erase behind.
pub struct Unprovision {
    pub rt: tokio::runtime::Handle,
    pub reply: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
}

/// Teardown (sign-out / account-switch / factory-reset / e2e-reset): stop the
/// convergence loop and push `UnprovisionCapability` (the agent deletes its
/// persisted capability and stops engines). A plain app quit deliberately does
/// NOT come here — the agent keeps syncing app-dead.
///
/// **Starts the un-provision and hands back the wait for its reply — it never
/// blocks.** The reply is the agent's receipt that its own mount of the account
/// store is down (`sync-agent.md` § Control plane split), and every caller
/// erases or re-scopes that store next, so the erase must wait for it — but
/// the GTK thread must not: an agent slow to answer froze the window for the
/// pipe client's whole request ceiling. The one caller,
/// `account_runtime::teardown`, awaits the wait on the same spawned stop as the
/// account store's, ahead of it (tui's `session::sign_out` order), and the
/// continuation that erases runs once both have finished. A dead agent is
/// success, and so is a runtime that drops the task: the wait ends either way.
/// `None` when no agent was installed.
pub fn teardown() -> Option<Unprovision> {
    let agent = AGENT.with(|cell| cell.borrow_mut().take())?;
    agent
        .event_listener_stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let provisioner = Arc::clone(&agent.provisioner);
    let task = agent.rt.spawn(async move {
        let _ = provisioner.unprovision().await;
    });
    Some(Unprovision {
        rt: agent.rt,
        reply: Box::pin(async move {
            let _ = task.await;
        }),
    })
}

// ---------------------------------------------------------------------------
// Folder-binding surface (the UI + e2e seam entry points; GTK main thread).
// ---------------------------------------------------------------------------

/// The bindings the UI renders right now — the model's union view (confirmed +
/// pending-bind rows; optimistic adds included). Empty before [`install`]
/// (pre-auth builds): the agent is the one record of this device's bindings.
pub fn current_locations() -> Vec<LocationBinding> {
    AGENT.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|agent| {
                agent
                    .model
                    .borrow()
                    .rendered()
                    .into_iter()
                    .map(|row| LocationBinding {
                        path: PathBuf::from(row.path),
                        folder: row.folder,
                        folder_id: row.folder_id,
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Whether the agent has **parked** every-or-any binding of `folder` because
/// the nest that owns the set refused this actor's write grant (D4,
/// `file-sync.md` § Multi-writer shared sets → the `folder-access-revoked-warning`
/// the Folders page renders).
///
/// Read from the same [`LocationBindingsModel`] rows [`current_locations`] renders,
/// so it is exactly as fresh: the reconcile that learns the park from the agent's
/// `ListLocations` also fires [`rerender_folders_page`], which rebuilds the
/// row and re-asks this. `false` before `install` (no agent, nothing parked) and
/// for a set with no local binding at all.
///
/// Deliberately *any* rather than *all*: a set is parked as a unit — the grant is
/// on the set, not the folder — so one parked binding means the set's engine is
/// stopped, and the warning belongs on the set's row.
pub fn is_access_revoked(folder: &str) -> bool {
    AGENT.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|agent| {
                agent
                    .model
                    .borrow()
                    .rendered()
                    .iter()
                    .any(|row| row.folder == folder && row.access_revoked)
            })
            .unwrap_or(false)
    })
}

/// How many deletes the **mass-delete floor** is holding on `folder` — the count
/// `folder-location-deletes-held` renders and
/// `folder-location-apply-deletes-button` offers to propagate
/// (`delete-propagation.md` § A wholesale-vanished folder is infrastructure
/// failure). `0` — the overwhelmingly common reading, and the reading before
/// [`install`] — means render neither.
///
/// Read off the same [`LocationBindingsModel`] rows [`current_locations`]
/// renders, kept current by [`refresh_engine_holds`] on the 10 s status tick.
/// The hold is per SET (one engine per bound set), so like [`is_access_revoked`]
/// this is keyed by set name rather than by folder path.
pub fn deletes_held_for(folder: &str) -> u64 {
    AGENT.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|agent| {
                agent
                    .model
                    .borrow()
                    .rendered()
                    .iter()
                    .find(|row| row.folder == folder)
                    .map(|row| row.deletes_held)
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    })
}

/// How the bound row at `path` renders `folder-location-mode-toggle` — the
/// shared rule ([`LocationBindingsModel::mode_toggle`]: the host's on-demand
/// surface from the status poll, the row's mode and its own mount failure from
/// the agent's `ListLocations`). `None` renders no switch: before [`install`],
/// and for a row the model does not hold (an e2e-injected render fixture).
pub fn mode_toggle_for(path: &Path) -> Option<ModeToggle> {
    let path = path.display().to_string();
    AGENT.with(|cell| {
        let borrow = cell.borrow();
        let model = borrow.as_ref()?.model.borrow();
        let row = model.rendered().into_iter().find(|row| row.path == path)?;
        model.mode_toggle(&row)
    })
}

/// Keep the host's on-demand surface from a `GetServiceStatus` reply (the
/// `sync-agent-status` poll's, `app.rs`) and repaint the bound rows when it
/// changed — every row's switch reads it ([`mode_toggle_for`]).
pub fn fold_service_status(status: &fauna_ipc::sync::ServiceStatusInfo) {
    let changed = AGENT.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|agent| agent.model.borrow_mut().fold_service_status(status))
    });
    if changed {
        rerender_folders_page();
    }
}

/// `folder-location-mode-toggle` — set the bound folder at `path` on-demand or
/// always-resident over the agent's `SetLocationSyncMode` (the verb windows'
/// `LocationBindingsController.SetModeAsync` and tui's `set_location_mode`
/// send; the agent persists `LocationConfig.mode` and re-drives its engines,
/// so the FUSE root actually mounts or unmounts). **Not optimistic:** the row
/// repaints from the `ListLocations` read that follows, so a refused flip
/// leaves the switch showing the agent's truth.
///
/// A row whose bind push has not landed yet is re-pushed first (binding is
/// idempotent): a switch clicked right after an add would otherwise address a
/// path the agent does not know.
pub fn set_location_mode(path: String, on_demand: bool) {
    let Some((provisioner, rt)) = provisioner_and_runtime() else {
        return;
    };
    let pending = AGENT.with(|cell| {
        cell.borrow().as_ref().and_then(|agent| {
            agent
                .model
                .borrow()
                .rendered()
                .into_iter()
                .find(|row| row.path == path && row.state == BindingState::PendingBind)
        })
    });
    let mode = if on_demand { "on-demand" } else { "always" }.to_string();
    crate::async_helper::spawn_with_snapshot(
        &rt,
        move || async move {
            if let Some(row) = pending
                && let Err(e) = provisioner
                    .bind_location(row.path, row.folder, row.folder_id)
                    .await
            {
                tracing::warn!("bind_location before a mode flip failed: {e}");
            }
            if let Err(e) = provisioner.set_location_sync_mode(path, mode).await {
                tracing::warn!("set_location_sync_mode failed (the mode stands): {e}");
            }
            provisioner.list_locations().await
        },
        move |result| {
            if let Ok(agent_rows) = result {
                fold_agent_rows(&agent_rows);
            }
            // Always, changed or not: the click moved the switch, and a flip the
            // agent refused must snap it back to the mode that stands.
            rerender_folders_page();
        },
    );
}

/// Reconcile the model against `agent_rows` (the agent's `ListLocations`
/// truth), push the union's outstanding binds/unbinds, and say whether the
/// rendered rows changed.
fn fold_agent_rows(agent_rows: &[fauna_ipc::sync::LocationInfo]) -> bool {
    AGENT.with(|cell| {
        let borrow = cell.borrow();
        let Some(agent) = borrow.as_ref() else {
            return false;
        };
        let actions = agent.model.borrow_mut().reconcile(agent_rows);
        for pending in actions.to_bind {
            push_bind(agent, pending);
        }
        for path in actions.to_unbind {
            push_unbind(agent, path);
        }
        // Repaint only when the rendered list actually changed — a
        // no-change rebuild destroys live widgets mid-interaction (see
        // `AgentUi::last_rendered`).
        let rendered = agent.model.borrow().rendered();
        let changed = *agent.last_rendered.borrow() != rendered;
        *agent.last_rendered.borrow_mut() = rendered;
        changed
    })
}

/// Fold the agent's `ListEngines` roster onto the model, so the mass-delete
/// floor's per-set hold reaches the Folders page — and its `ListLocations` park
/// (`access_revoked`), so a demoted writer's row says it stopped syncing.
/// Driven from `app.rs`'s 10 s `sync-agent-status` tick, beside
/// `get_service_status`.
///
/// Both need a *watch*, not a reconcile hook: each is derived inside the agent
/// (the hold on its own rescan cadence, the park when the owning nest refuses a
/// write), so no user gesture and no reachability edge produces it —
/// [`reconcile`] alone would leave a folder that emptied five minutes ago still
/// painting as healthy, and a refused one still painting as syncing. Repaints
/// only on an actual change, same as [`reconcile`] and for the same
/// widget-destroying reason.
pub fn refresh_engine_holds() {
    let Some((provisioner, rt)) = provisioner_and_runtime() else {
        return;
    };
    crate::async_helper::spawn_with_snapshot(
        &rt,
        move || async move {
            (
                provisioner.list_engines().await,
                provisioner.list_locations().await,
            )
        },
        move |(engines, locations)| {
            // An unreachable agent yields no rows, which folds to "no live
            // engine stands behind a count" — the honest reading, and exactly
            // the derived-never-stored contract.
            let engines = engines.unwrap_or_default();
            let changed = AGENT.with(|cell| {
                let borrow = cell.borrow();
                let Some(agent) = borrow.as_ref() else {
                    return false;
                };
                agent.model.borrow_mut().fold_engine_holds(&engines);
                // The binding PARK is the same kind of level — derived inside
                // the agent when the owning nest refuses a write, so the
                // mutation-driven `reconcile` never saw a demoted writer's
                // park and `is_access_revoked` stayed false. An unreachable
                // agent folds nothing (the rows keep their last-known park).
                if let Ok(locations) = &locations {
                    agent.model.borrow_mut().fold_parks(locations);
                }
                let rendered = agent.model.borrow().rendered();
                let changed = *agent.last_rendered.borrow() != rendered;
                *agent.last_rendered.borrow_mut() = rendered;
                changed
            });
            if changed {
                rerender_folders_page();
            }
        },
    );
}

/// The user confirmed the mass-delete floor's hold on `folder`: propagate the
/// held deletions to the nest (`RequestMethod::ApplyHeldDeletes` — the
/// `folder-location-apply-deletes-button` gesture). Propagation of a held set is
/// an **explicit user action**, never automatic.
///
/// Deliberately non-optimistic: the agent re-derives what is actually missing at
/// click time, so the only honest count is the one its reply carries back
/// (`remaining_held`). A failure leaves the hold standing — nothing was recorded
/// and there is no client-side state to unwind.
pub fn apply_held_deletes(folder: String) {
    let Some((provisioner, rt)) = provisioner_and_runtime() else {
        return;
    };
    let set = folder.clone();
    crate::async_helper::spawn_with_snapshot(
        &rt,
        move || async move { provisioner.apply_held_deletes(folder).await },
        move |result| {
            let info = match result {
                Ok(info) => info,
                Err(e) => {
                    tracing::warn!("apply_held_deletes failed (the hold stands): {e}");
                    return;
                }
            };
            let changed = AGENT.with(|cell| {
                let borrow = cell.borrow();
                let Some(agent) = borrow.as_ref() else {
                    return false;
                };
                agent
                    .model
                    .borrow_mut()
                    .set_engine_hold(&set, info.remaining_held);
                let rendered = agent.model.borrow().rendered();
                let changed = *agent.last_rendered.borrow() != rendered;
                *agent.last_rendered.borrow_mut() = rendered;
                changed
            });
            if changed {
                rerender_folders_page();
            }
        },
    );
}

/// Optimistic add: record the row (it renders immediately), THEN push the bind to
/// the agent. A failed push keeps the row rendered and re-pushes on the next
/// reconcile (union semantics); a successful one confirms + pokes the loop. The
/// set's content keys are the agent's to resolve: the bind's reconcile re-reads
/// its holder's custody (`on-demand-files.md` § Shared sets on a capability host
/// → *One mechanism*).
pub fn add_binding(mapping: LocationBinding) {
    AGENT.with(|cell| {
        let borrow = cell.borrow();
        let Some(agent) = borrow.as_ref() else {
            return;
        };
        // A binding is keyed by the set's ref alone — a `LocationBinding` cannot
        // be built without one, and the UI refuses a row with none upstream.
        let pending = agent.model.borrow_mut().add(
            mapping.path.display().to_string(),
            mapping.folder.clone(),
            mapping.folder_id.clone(),
        );
        push_bind(agent, pending);
    });
}

/// Optimistic remove (by folder — the UI's row key): the row leaves the
/// rendered list immediately; `RemoveLocation` pushes now and re-pushes on
/// reconcile until the agent no longer lists it.
pub fn remove_binding(folder: &str) {
    AGENT.with(|cell| {
        let borrow = cell.borrow();
        let Some(agent) = borrow.as_ref() else {
            return;
        };
        let paths = agent.model.borrow_mut().remove_by_set(folder);
        for path in paths {
            push_unbind(agent, path);
        }
    });
}

/// Nudge the sync agent's resident engine for `folder` to pull remote changes
/// now — the `PushEvent::SyncChanged` handler calls this (`file-sync.md`
/// § Remote-change nudge). Fire-and-forget on the provisioner's runtime (off the
/// GTK main thread); a failed or dropped nudge just means the next rescan tick
/// catches up. A no-op before [`install`] (no agent attached).
/// `folder_hash` is the push's hash address, relayed as received (a sealed
/// set's nudge carries no name).
pub fn pull_set_now(folder: String, folder_hash: Option<Vec<u8>>) {
    AGENT.with(|cell| {
        let borrow = cell.borrow();
        let Some(agent) = borrow.as_ref() else {
            return;
        };
        let provisioner = Arc::clone(&agent.provisioner);
        agent.rt.spawn(async move {
            if let Err(e) = provisioner.pull_folder_now(folder, folder_hash).await {
                tracing::debug!("remote-change pull-now nudge failed (tick will catch up): {e}");
            }
        });
    });
}

/// Whether the agent is currently serving ≥1 sync engine — the e2e state-JSON
/// `data.sync.running` read.
///
/// The derivation itself is shared
/// ([`fauna_client_sync::agent::any_engine_serving_cached`], lifted out of here
/// 2026-07-29 when fauna-tui needed the same reading — priority #2); this stays
/// as the named local call site the state report reads better through.
///
/// ⚠ **The `_cached` spelling is required here, not preferred.** This is read by
/// `main.rs::sync_state_json`, i.e. the test agent's state provider — the ack
/// path — and the blocking spelling costs a full request timeout against an
/// agent that connects but does not answer. linux escaped that on windows' terms
/// only by luck: a *missing* unix socket fails `connect()` instantly, while a
/// wedged-but-listening agent (exactly what a sync test is most likely to
/// create) would have burned the ceiling here too. See
/// `e2e-conventions.md` § convention 14 build-out → the windows leg.
pub fn sync_running() -> bool {
    fauna_client_sync::agent::any_engine_serving_cached()
}

/// Reconcile the model against the agent's `ListLocations` truth and push the
/// union's outstanding binds/unbinds. Re-driven on every agent-reachable edge
/// (via [`GlibReachabilityObserver`]) — never only once at attach. Re-renders
/// the Folders page when it is built.
pub fn reconcile() {
    let Some((provisioner, rt)) = AGENT.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|a| (Arc::clone(&a.provisioner), a.rt.clone()))
    }) else {
        return;
    };
    let list_provisioner = Arc::clone(&provisioner);
    crate::async_helper::spawn_with_snapshot(
        &rt,
        move || async move { list_provisioner.list_locations().await },
        move |result| {
            let Ok(agent_rows) = result else {
                return; // unreachable — the next edge re-drives us
            };
            if fold_agent_rows(&agent_rows) {
                rerender_folders_page();
            }
        },
    );
}

/// Push one bind; confirm into the model on success (a failure just leaves the
/// row pending for the next reconcile).
fn push_bind(agent: &AgentUi, pending: PendingBind) {
    let provisioner = Arc::clone(&agent.provisioner);
    let done_path = pending.path.clone();
    crate::async_helper::spawn_with_snapshot(
        &agent.rt,
        move || async move {
            provisioner
                .bind_location(pending.path, pending.folder, pending.folder_id)
                .await
        },
        move |result| match result {
            Ok(()) => AGENT.with(|cell| {
                if let Some(agent) = cell.borrow().as_ref() {
                    agent.model.borrow_mut().confirm_bind(&done_path);
                }
            }),
            Err(e) => tracing::warn!("bind_location failed (reconcile will retry): {e}"),
        },
    );
}

/// Push one unbind; drop the pending row on success.
fn push_unbind(agent: &AgentUi, path: String) {
    let provisioner = Arc::clone(&agent.provisioner);
    let done_path = path.clone();
    crate::async_helper::spawn_with_snapshot(
        &agent.rt,
        move || async move { provisioner.unbind_location(path).await },
        move |result| match result {
            Ok(()) => AGENT.with(|cell| {
                if let Some(agent) = cell.borrow().as_ref() {
                    agent.model.borrow_mut().confirm_unbind(&done_path);
                }
            }),
            Err(e) => tracing::warn!("unbind_location failed (reconcile will retry): {e}"),
        },
    );
}

/// Repaint the Folders page's nested binding rows from the model (a no-op
/// until the page registers its re-render callback).
fn rerender_folders_page() {
    crate::views::devices_folders::rerender_bindings(current_locations());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env(v: &str) -> Option<OsString> {
        Some(OsString::from(v))
    }

    /// Every line `folder-location-mode-toggle` can render resolves in the
    /// string table — the shared notice names a key, and a key with no string
    /// would render the switch disabled with no reason at all.
    #[test]
    fn every_on_demand_notice_has_its_line() {
        for notice in fauna_client_sync::agent::OnDemandNotice::ALL {
            assert!(
                crate::i18n::strings::lookup(notice.i18n_key()).is_some(),
                "{notice:?} names {} — no such string",
                notice.i18n_key()
            );
        }
    }

    // -- channel detection (linux-desktop.md § Installation Files: the five
    //    channels reduce to three lifecycle shapes) --

    #[test]
    fn channel_is_native_unit_without_sandbox_or_appimage_env() {
        assert_eq!(
            LaunchChannel::from_env(None, None, None),
            LaunchChannel::NativeUnit
        );
    }

    #[test]
    fn channel_is_flatpak_unit_under_flatpak_and_sandbox_child_under_snap() {
        // Flatpak: the always-on host-unit seam, carrying the app-id the unit's
        // `flatpak run` line needs.
        assert_eq!(
            LaunchChannel::from_env(env("social.fauna.fauna"), None, None),
            LaunchChannel::FlatpakUnit {
                app_id: "social.fauna.fauna".into()
            }
        );
        // Snap: still the app-child interim (no user-unit surface until snapd
        // stabilizes daemon-scope: user — linux-desktop.md § Snap).
        assert_eq!(
            LaunchChannel::from_env(None, env("/snap/fauna/1"), None),
            LaunchChannel::SandboxChild
        );
        // Sandbox wins over an (implausible) simultaneous APPIMAGE var — the
        // sandbox boundary is the harder constraint.
        assert_eq!(
            LaunchChannel::from_env(env("social.fauna.fauna"), None, env("/x/Fauna.AppImage")),
            LaunchChannel::FlatpakUnit {
                app_id: "social.fauna.fauna".into()
            }
        );
    }

    // -- the Flatpak host-unit seam's pure derivations --

    #[test]
    fn flatpak_info_app_path_reads_the_instance_section() {
        let info = "[Application]\n\
                    name=social.fauna.fauna\n\
                    runtime=runtime/org.gnome.Platform/aarch64/50\n\
                    \n\
                    [Instance]\n\
                    instance-id=12345\n\
                    app-path=/var/lib/flatpak/app/social.fauna.fauna/aarch64/master/deadbeef/files\n\
                    branch=master\n";
        assert_eq!(
            flatpak_info_app_path(info),
            Some(PathBuf::from(
                "/var/lib/flatpak/app/social.fauna.fauna/aarch64/master/deadbeef/files"
            ))
        );
        // An app-path outside [Instance] must not match.
        assert_eq!(flatpak_info_app_path("[Application]\napp-path=/x\n"), None);
    }

    /// The condition target must be the *stable* `current/active` deploy link —
    /// the per-commit deploy dir from `app-path` goes stale on every
    /// `flatpak update`, which would condition-skip a healthy install until the
    /// next app launch healed the unit.
    #[test]
    fn flatpak_condition_target_uses_the_stable_deploy_link() {
        // System installation.
        assert_eq!(
            flatpak_condition_target(
                Path::new("/var/lib/flatpak/app/social.fauna.fauna/aarch64/master/deadbeef/files"),
                "social.fauna.fauna"
            ),
            Some(PathBuf::from(
                "/var/lib/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent"
            ))
        );
        // User installation.
        assert_eq!(
            flatpak_condition_target(
                Path::new(
                    "/home/u/.local/share/flatpak/app/social.fauna.fauna/aarch64/master/abc/files"
                ),
                "social.fauna.fauna"
            ),
            Some(PathBuf::from(
                "/home/u/.local/share/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent"
            ))
        );
        // A parent dir literally named `app` without our id right after it
        // keeps scanning to the real segment pair.
        assert_eq!(
            flatpak_condition_target(
                Path::new("/app/flatpak/app/social.fauna.fauna/aarch64/master/abc/files"),
                "social.fauna.fauna"
            ),
            Some(PathBuf::from(
                "/app/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent"
            ))
        );
        // No app/<id> pair → no condition target (caller falls back to the
        // app-child interim).
        assert_eq!(
            flatpak_condition_target(Path::new("/somewhere/else/files"), "social.fauna.fauna"),
            None
        );
    }

    /// The unit the Flatpak channel writes: `flatpak run` re-enters the
    /// sandbox, and the condition line points at the deployed agent binary so
    /// `flatpak uninstall` condition-skips instead of Restart-flapping
    /// (`/usr/bin/flatpak` itself survives an uninstall).
    #[test]
    fn flatpak_unit_contents_exec_flatpak_run_with_deploy_condition() {
        let exec = unit_exec_for_flatpak(
            "social.fauna.fauna",
            PathBuf::from(
                "/var/lib/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent",
            ),
        )
        .expect("honest inputs");
        let unit = agent_spawner::unit_contents(&exec);
        assert!(unit.contains(
            "ExecStart=/usr/bin/flatpak run --command=fauna-sync-agent \"social.fauna.fauna\"\n"
        ));
        assert!(unit.contains(
            "ConditionFileIsExecutable=/var/lib/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent\n"
        ));
        // Re-checked on every Restart=always auto-restart (Condition*= is not),
        // so `flatpak uninstall` of the running agent condition-skips instead
        // of Restart-flapping forever — see `unit_contents`.
        assert!(unit.contains(
            "ExecCondition=/usr/bin/test -x \"/var/lib/flatpak/app/social.fauna.fauna/current/active/files/bin/fauna-sync-agent\"\n"
        ));
    }

    #[test]
    fn channel_is_appimage_unit_with_the_appimage_path() {
        assert_eq!(
            LaunchChannel::from_env(None, None, env("/home/u/Apps/Fauna.AppImage")),
            LaunchChannel::AppImageUnit {
                appimage: PathBuf::from("/home/u/Apps/Fauna.AppImage")
            }
        );
    }

    #[test]
    fn empty_env_values_do_not_select_a_channel() {
        assert_eq!(
            LaunchChannel::from_env(env(""), env(""), env("")),
            LaunchChannel::NativeUnit
        );
    }

    // -- unit shape, linux-specific: the AppImage exec form. (The generic
    //    `for_binary`/`unit_contents`/`path_lookup`/child-spawn tests moved to
    //    `fauna_client_sync::agent_spawner` with the lift.) --

    #[test]
    fn appimage_unit_execs_the_appimage_with_the_sync_agent_arg() {
        let exec = unit_exec_for_appimage(PathBuf::from("/home/u/My Apps/Fauna.AppImage"))
            .expect("honest path");
        let unit = agent_spawner::unit_contents(&exec);
        // Quoted — AppImage paths are user-chosen and may contain spaces.
        assert!(unit.contains("ExecStart=\"/home/u/My Apps/Fauna.AppImage\" --sync-agent\n"));
        assert!(unit.contains("ConditionFileIsExecutable=/home/u/My Apps/Fauna.AppImage\n"));
        // ExecCondition quotes the same space-containing path (systemd double-quote
        // parsing preserves it as one arg) so the anti-flap guard tracks it too.
        assert!(
            unit.contains("ExecCondition=/usr/bin/test -x \"/home/u/My Apps/Fauna.AppImage\"\n")
        );
    }

    // The event-listener mechanism tests (the Synced filter + the
    // fake-event-over-a-real-socket loop test) moved to `fauna_ipc::events`
    // with the loop itself; only the GTK notification hop remains here.
}
