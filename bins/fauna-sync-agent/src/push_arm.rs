//! The agent's `ws-device` notification arm — the desktop's push wake stand-in
//! (`sync-agent.md` § Scope per platform; owner `common.md` § Push
//! Notifications → *Transports*, the `ws-device` ruling).
//!
//! The nest delivers a present `ws-device` row as a `fauna.push.notification`
//! frame to every connection that announced the row's device id — the agent's
//! own account connection, and the app's if it is open. Exactly one process
//! posts the banner: **the agent posts only while no app is attached** over the
//! local IPC seam; an attached app owns the machine's banners under its own
//! focus rule and ignores the frame. "Attached" is a connection lease
//! ([`AttachedApps`]): an app sends `RequestMethod::AttachApp` on a connection it
//! holds for its whole life, and the lease ends when that connection closes,
//! a crash included.
//!
//! The subscription is the APP's, never the agent's: the agent only announces
//! presence and posts. **Both ride the capability connection, never the
//! account-store mount** (`common.md` § Push Notifications → *Transports*,
//! ruled 2026-10-09): the retained capability client is built announcing the
//! capability's `device_id` as a stand-in
//! ([`SyncServiceState::nest_rpc_client`]), and [`host`] arms on that same
//! client from provision until un-provision or a refused renewal — whether or
//! not the seedless account store has assembled, which waits on a signed-in
//! enrollment.
//! A machine with no desktop session has no sink; the agent says so on
//! `ServiceStatusInfo::notification_sink` and the app's control renders its
//! failure line.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use std::time::Duration;

use fauna_protocol::PushEvent;
use fauna_protocol::push_events::PushNotificationPayload;
use tokio::sync::{broadcast, watch};

use crate::state::{IDLE_RECHECK_SECS, SyncServiceState};

/// The first and the longest wait between two failed attempts to obtain the
/// capability connection (a nest unreachable at boot).
const ARM_BACKOFF_START_SECS: u64 = 5;
const ARM_BACKOFF_MAX_SECS: u64 = 300;

/// How many apps hold an attachment right now.
#[derive(Default)]
pub struct AttachedApps(AtomicUsize);

impl AttachedApps {
    /// Start one app's lease; it ends when the returned guard drops.
    pub fn attach(self: &Arc<Self>) -> AppAttachment {
        self.0.fetch_add(1, Ordering::SeqCst);
        AppAttachment(Arc::clone(self))
    }

    /// Is any app open on this machine?
    pub fn any(&self) -> bool {
        self.count() > 0
    }

    /// How many apps hold a lease right now.
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// What [`run`] decided, one count per outcome. Read only by the test-only
/// `GetPushArmTally` request (`fauna_ipc::sync::RequestMethod::GetPushArmTally`),
/// because a frame withheld for an attached app leaves nothing else behind:
/// the count is how an end-to-end test knows the arm has decided a frame
/// before it asserts that nothing was posted (`testing.md` convention 14).
#[derive(Default)]
pub struct ArmTally {
    withheld: AtomicU64,
    no_sink: AtomicU64,
    posted: AtomicU64,
    failed: AtomicU64,
}

impl ArmTally {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::SeqCst);
    }

    /// The counts so far, beside the attachments held now.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn snapshot(&self, attached: &AttachedApps) -> fauna_ipc::sync::PushArmTally {
        fauna_ipc::sync::PushArmTally {
            attached_apps: attached.count() as u64,
            withheld: self.withheld.load(Ordering::SeqCst),
            no_sink: self.no_sink.load(Ordering::SeqCst),
            posted: self.posted.load(Ordering::SeqCst),
            failed: self.failed.load(Ordering::SeqCst),
        }
    }
}

/// One app's attachment lease — held for its IPC connection's lifetime
/// (`fauna_ipc::conn_scope::hold_for_connection`).
pub struct AppAttachment(Arc<AttachedApps>);

impl Drop for AppAttachment {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where the agent posts a banner.
pub trait NotificationSink: Send + Sync {
    /// Whether a banner can be posted here, without blocking: `None` until
    /// [`Self::probe`] has answered.
    fn availability(&self) -> Option<bool>;
    /// Answer [`Self::availability`] — may block (a session-bus round trip);
    /// called once at boot, off the async runtime, and again by a sink whose
    /// answer an adopted identity changes.
    fn probe(&self);
    /// Post one banner. Blocking.
    fn post(&self, notification: &PushNotificationPayload) -> Result<(), String>;
    /// An attaching app named the identity its banners are posted under
    /// (`RequestMethod::AttachApp`'s `notification_identity` — windows: the
    /// app's AUMID). Only a sink that posts under an app's identity keeps it;
    /// the rest ignore it. Blocking (it re-probes).
    fn adopt_identity(&self, _identity: &str) {}
    /// An attaching app named itself (`RequestMethod::AttachApp`'s `app`) —
    /// the app a tap on a banner launches. Only a sink that launches an app by
    /// name keeps it (linux, its `tap` module); windows' toast activates the app its
    /// identity names. Blocking (a small file write).
    fn adopt_app(&self, _app: &str) {}
}

/// The sink this platform's agent posts through. linux: the freedesktop
/// notification daemon through `notify-rust`, present when a desktop session
/// is (the `keyring_probe` selector tui already trusts — never an env knob),
/// a tap launching the app that last attached, kept in `base_dir` (its `tap` module).
/// windows: the WinRT toast under the identity the app attached with, kept in
/// `base_dir` across the agent's restarts ([`toast`]). macOS: no sink, a
/// declared absence (`common.md` § Push Notifications → *Transports*: macOS
/// refuses `notify-rust`'s legacy-client post while reporting success), so the
/// agent honestly reports no sink.
pub fn platform_sink(base_dir: &std::path::Path) -> Arc<dyn NotificationSink> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(DesktopSink::new(base_dir.join(tap::APP_FILE)))
    }
    #[cfg(windows)]
    {
        Arc::new(toast::ToastSink::new(base_dir.join(toast::IDENTITY_FILE)))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = base_dir;
        Arc::new(NoSink)
    }
}

#[cfg(target_os = "linux")]
struct DesktopSink {
    present: std::sync::OnceLock<bool>,
    /// Where the last attached app's name is kept across the agent's restarts.
    app_file: std::path::PathBuf,
    app: std::sync::Mutex<Option<String>>,
}

#[cfg(target_os = "linux")]
impl DesktopSink {
    fn new(app_file: std::path::PathBuf) -> Self {
        let app = std::fs::read_to_string(&app_file)
            .ok()
            .and_then(|kept| tap::valid_app(kept.trim()).map(str::to_owned));
        Self {
            present: std::sync::OnceLock::new(),
            app_file,
            app: std::sync::Mutex::new(app),
        }
    }

    fn app(&self) -> Option<String> {
        self.app.lock().ok()?.clone()
    }
}

#[cfg(target_os = "linux")]
impl NotificationSink for DesktopSink {
    fn availability(&self) -> Option<bool> {
        self.present.get().copied()
    }

    fn probe(&self) {
        self.present
            .get_or_init(fauna_credential_store::keyring_probe);
    }

    fn post(&self, n: &PushNotificationPayload) -> Result<(), String> {
        let launch = tap::launch_argv(
            self.app().as_deref(),
            &n.url,
            &tap::which,
            &tap::desktop_entry,
        );
        let mut notification = notify_rust::Notification::new();
        notification
            .summary(&n.title)
            .body(&n.body)
            .appname("Fauna");
        // A banner offers the default action only when a tap has something to
        // launch; one with nothing to run is a plain banner.
        if launch.is_some() {
            notification.action("default", "Open");
        }
        let handle = notification.show().map_err(|e| e.to_string())?;
        if let Some(argv) = launch {
            // `wait_for_action` blocks until the banner is tapped or closed —
            // on a desktop that keeps banners in its tray, until the user
            // dismisses it — so each tappable banner holds one small thread.
            let spawned = std::thread::Builder::new()
                .name("fauna-push-tap".into())
                .spawn(move || {
                    handle.wait_for_action(|action| {
                        if action == "default" {
                            tap::launch(&argv);
                        }
                    })
                });
            if let Err(e) = spawned {
                tracing::warn!("push arm: the banner's tap is not watched: {e}");
            }
        }
        Ok(())
    }

    fn adopt_app(&self, app: &str) {
        let Some(app) = tap::valid_app(app) else {
            tracing::warn!("push arm: refused an unusable app name");
            return;
        };
        let changed = match self.app.lock() {
            Ok(mut held) if held.as_deref() != Some(app) => {
                *held = Some(app.to_owned());
                true
            }
            _ => false,
        };
        if changed {
            if let Some(dir) = self.app_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&self.app_file, app) {
                tracing::warn!("push arm: the attached app was not kept: {e}");
            }
        }
    }
}

/// What a tap on a linux agent banner runs (`common.md` § Push Notifications
/// → *Transports*: "a tap opens the app, at the frame's `url` where the
/// platform's launch path allows"; the mapping is stated in `apps/linux.md`
/// § Desktop Notifications). The app launched is the one that last attached
/// (`RequestMethod::AttachApp`'s `app`), kept under the agent's flat base so a
/// banner after an agent restart still knows it:
///
/// - `"linux"` → the installed desktop entry `<LINUX_APP_ID>.desktop`, through
///   `gtk-launch` (which resolves the entry the way the desktop's launcher
///   does, Flatpak and Snap exports included), else `gio launch` of the entry
///   file found in the XDG data dirs; neither → no action.
/// - `"tui"` → `x-terminal-emulator -e <fauna-tui>`, the binary beside the
///   agent's own (the archive installs the pair together) or on `PATH`;
///   either missing → no action.
/// - anything else, or no app ever attached → no action.
///
/// The frame's `url` rides along only when it is a `fauna://` in-app route
/// (`fauna_core::app_route` — the one argument both apps take on their command
/// line), re-minted from the parse so remote text never reaches an argv
/// as-is. The nest's frames carry web paths today (`/app/contacts`), so a tap
/// opens the app at its start page.
#[cfg(target_os = "linux")]
pub(crate) mod tap {
    use std::path::{Path, PathBuf};

    use fauna_core::app_route::AppRoute;
    use fauna_core::platform_ids::LINUX_APP_ID;

    /// The file under the agent's flat base holding the last attached app.
    pub(super) const APP_FILE: &str = "attached-app";
    /// The longest app name kept: the names are short words (`"tui"`).
    const MAX_APP: usize = 32;
    const TUI_BINARY: &str = "fauna-tui";

    pub(super) fn valid_app(app: &str) -> Option<&str> {
        let ok = !app.is_empty()
            && app.len() <= MAX_APP
            && app
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        ok.then_some(app)
    }

    /// The launch a tap runs, or `None` for no action. `which` finds a
    /// command (an absolute path) and `entry` the desktop entry file — the
    /// host's answers, passed in so the mapping is asserted headlessly.
    pub(crate) fn launch_argv(
        app: Option<&str>,
        url: &str,
        which: &dyn Fn(&str) -> Option<PathBuf>,
        entry: &dyn Fn() -> Option<PathBuf>,
    ) -> Option<Vec<String>> {
        let mut argv: Vec<String> = match app? {
            "linux" => {
                let desktop_id = format!("{LINUX_APP_ID}.desktop");
                if let Some(gtk_launch) = which("gtk-launch") {
                    vec![path_arg(&gtk_launch)?, desktop_id]
                } else {
                    let gio = which("gio")?;
                    vec![path_arg(&gio)?, "launch".into(), path_arg(&entry()?)?]
                }
            }
            "tui" => {
                let terminal = which("x-terminal-emulator")?;
                let tui = which(TUI_BINARY)?;
                vec![path_arg(&terminal)?, "-e".into(), path_arg(&tui)?]
            }
            _ => return None,
        };
        argv.extend(AppRoute::parse(url).map(|route| route.to_uri()));
        Some(argv)
    }

    fn path_arg(path: &Path) -> Option<String> {
        path.to_str().map(str::to_owned)
    }

    /// The host's `which`: `fauna-tui` beside the agent's own binary first,
    /// then every command on `PATH`.
    pub(crate) fn which(command: &str) -> Option<PathBuf> {
        let beside = (command == TUI_BINARY)
            .then(|| std::env::current_exe().ok())
            .flatten()
            .map(|exe| exe.with_file_name(TUI_BINARY));
        let path = std::env::var_os("PATH").unwrap_or_default();
        beside
            .into_iter()
            .chain(std::env::split_paths(&path).map(|dir| dir.join(command)))
            .find(|candidate| is_executable(candidate))
    }

    fn is_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    /// The installed desktop entry file, searched the XDG way
    /// (`$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`, each under `applications/`).
    pub(crate) fn desktop_entry() -> Option<PathBuf> {
        let home = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        let dirs = std::env::var_os("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        home.into_iter()
            .chain(std::env::split_paths(&dirs))
            .map(|dir| {
                dir.join("applications")
                    .join(format!("{LINUX_APP_ID}.desktop"))
            })
            .find(|file| file.is_file())
    }

    /// Open the linux app at `route` the way a tap does — the second caller
    /// is the `--share-target --open` helper KDE Dolphin's service menu runs
    /// (`apps/linux.md` § The file manager's Share entry (GNOME Files) → *The
    /// KDE Dolphin leg*): one opener for both. `false` when the host has no
    /// launcher.
    pub(crate) fn open_linux_app(route: &str) -> bool {
        match launch_argv(Some("linux"), route, &which, &desktop_entry) {
            Some(argv) => {
                launch(&argv);
                true
            }
            None => false,
        }
    }

    /// Run a tap's launch, detached from the agent: the launched app outlives
    /// it, and the child is reaped on this (the tap's own) thread.
    pub(crate) fn launch(argv: &[String]) {
        let Some((program, args)) = argv.split_first() else {
            return;
        };
        let spawned = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let _ = child.wait();
            }
            Err(e) => tracing::warn!("push arm: a tapped banner could not launch {program}: {e}"),
        }
    }
}

/// The windows arm: a WinRT toast posted under the app's own toast identity
/// (`common.md` § Push Notifications → *Transports*: the toast attributed to
/// the app, through the agent's existing `windows` crate). The agent has no
/// notification identity of its own; the app names its AUMID when it attaches
/// — the registration it already carries for its own toasts — so an agent
/// toast shows under the app's name and icon, groups with the app's own
/// toasts, and activates the app on a tap. The last identity named is kept in
/// a file under the agent's flat base, so a toast still finds it after the
/// agent restarts while the app is closed. No identity yet → no sink.
#[cfg(windows)]
mod toast {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use fauna_protocol::push_events::PushNotificationPayload;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{
        NotificationSetting, ToastNotification, ToastNotificationManager,
    };
    use windows::core::HSTRING;

    use super::NotificationSink;

    /// The file under the agent's flat base holding the adopted identity.
    pub(super) const IDENTITY_FILE: &str = "notification-identity";
    /// The longest identity adopted. Windows caps an AUMID at 128 characters;
    /// the margin admits nothing a real one needs and refuses an unbounded
    /// string from a local client.
    const MAX_IDENTITY: usize = 256;

    pub(super) struct ToastSink {
        identity_file: PathBuf,
        identity: Mutex<Option<String>>,
        present: Mutex<Option<bool>>,
    }

    impl ToastSink {
        pub(super) fn new(identity_file: PathBuf) -> Self {
            let identity = std::fs::read_to_string(&identity_file)
                .ok()
                .and_then(|kept| valid(kept.trim()).map(str::to_owned));
            Self {
                identity_file,
                identity: Mutex::new(identity),
                present: Mutex::new(None),
            }
        }

        fn identity(&self) -> Option<String> {
            self.identity.lock().ok()?.clone()
        }
    }

    fn valid(identity: &str) -> Option<&str> {
        let ok = !identity.is_empty()
            && identity.chars().count() <= MAX_IDENTITY
            && !identity.chars().any(char::is_control);
        ok.then_some(identity)
    }

    /// `ToastNotifier.Setting`'s answer for an identity the notification
    /// platform has not met yet (`HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`).
    const NOT_MET_YET: windows::core::HRESULT = windows::core::HRESULT(0x8007_0490_u32 as i32);

    /// Whether toasts for `aumid` show for this user in this session — the
    /// notification platform's own answer (`ToastNotifier.Setting`): enabled,
    /// or turned off by the user or by policy. One case is not an answer
    /// (measured on Windows 2026-10-08): an unpackaged app's identity the
    /// platform meets only at its first toast reads "not found" until then,
    /// so there the documented unpackaged registration decides
    /// (`HKCU\Software\Classes\AppUserModelId\<aumid>` — the key
    /// `AppNotificationManager.Register()` writes). A packaged identity reads
    /// enabled from its registration on, posted from this unpackaged process
    /// or not (measured the same day against the sparse identity package).
    pub(super) fn enabled(aumid: &str) -> bool {
        let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))
            .and_then(|notifier| notifier.Setting());
        match setting {
            Ok(setting) => setting == NotificationSetting::Enabled,
            Err(e) if e.code() == NOT_MET_YET => registered_unpackaged(aumid),
            Err(_) => false,
        }
    }

    /// The documented registration of an unpackaged app's toast identity.
    fn registered_unpackaged(aumid: &str) -> bool {
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegOpenKeyExW,
        };
        let path = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{aumid}"));
        let mut key = HKEY::default();
        // SAFETY: `path` is a live null-terminated wide string and `key` a local
        // out-slot; the key is closed below whenever the open succeeded.
        let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &path, None, KEY_READ, &mut key) };
        if opened.is_ok() {
            // SAFETY: `key` was just opened by the call above.
            let _ = unsafe { RegCloseKey(key) };
            true
        } else {
            false
        }
    }

    /// The toast's XML: the frame's title and body as the two text lines.
    pub(super) fn toast_xml(title: &str, body: &str) -> String {
        format!(
            "<toast><visual><binding template=\"ToastGeneric\">\
             <text>{}</text><text>{}</text>\
             </binding></visual></toast>",
            escape(title),
            escape(body)
        )
    }

    fn escape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                // XML 1.0 admits no other control characters; a frame is
                // remote-authored text.
                c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => out.push(' '),
                c => out.push(c),
            }
        }
        out
    }

    /// Post one toast under `aumid`. `label` (tag, group) names it in the
    /// notification centre; `popup: false` delivers it there without the
    /// on-screen banner — what the tests use, so a run paints nothing.
    pub(super) fn show(
        aumid: &str,
        n: &PushNotificationPayload,
        label: Option<(&str, &str)>,
        popup: bool,
    ) -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(toast_xml(&n.title, &n.body)))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        if let Some((tag, group)) = label {
            toast.SetTag(&HSTRING::from(tag))?;
            toast.SetGroup(&HSTRING::from(group))?;
        }
        if !popup {
            toast.SetSuppressPopup(true)?;
        }
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?.Show(&toast)
    }

    impl NotificationSink for ToastSink {
        fn availability(&self) -> Option<bool> {
            *self.present.lock().ok()?
        }

        fn probe(&self) {
            let present = self.identity().is_some_and(|id| enabled(&id));
            if let Ok(mut held) = self.present.lock() {
                *held = Some(present);
            }
        }

        fn post(&self, n: &PushNotificationPayload) -> Result<(), String> {
            let aumid = self
                .identity()
                .ok_or("no app has named its notification identity")?;
            show(&aumid, n, None, true).map_err(|e| e.to_string())
        }

        fn adopt_identity(&self, identity: &str) {
            let Some(identity) = valid(identity) else {
                tracing::warn!("push arm: refused an unusable notification identity");
                return;
            };
            let changed = match self.identity.lock() {
                Ok(mut held) if held.as_deref() != Some(identity) => {
                    *held = Some(identity.to_owned());
                    true
                }
                _ => false,
            };
            if changed {
                if let Some(dir) = self.identity_file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(&self.identity_file, identity) {
                    tracing::warn!("push arm: the notification identity was not kept: {e}");
                }
            }
            self.probe();
        }
    }
}

/// A platform whose agent arm is not built: never a sink.
#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
struct NoSink;

impl NotificationSink for NoSink {
    fn availability(&self) -> Option<bool> {
        Some(false)
    }
    fn probe(&self) {}
    fn post(&self, _: &PushNotificationPayload) -> Result<(), String> {
        Err("no notification sink on this platform".into())
    }
}

/// Post every `fauna.push.notification` frame on `pushes` while no app is
/// attached, until the push stream closes (the connection's client is gone).
/// Each decided frame is counted in `tally`.
pub async fn run(
    mut pushes: broadcast::Receiver<PushEvent>,
    attached: Arc<AttachedApps>,
    sink: Arc<dyn NotificationSink>,
    tally: Arc<ArmTally>,
) {
    loop {
        let notification = match pushes.recv().await {
            Ok(PushEvent::PushNotification(n)) => n,
            Ok(_) => continue,
            // A burst we fell behind on: the banners are best-effort.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if attached.any() {
            // The open app received the same frame and owns the banner.
            ArmTally::bump(&tally.withheld);
            continue;
        }
        if sink.availability() != Some(true) {
            ArmTally::bump(&tally.no_sink);
            continue;
        }
        let sink = Arc::clone(&sink);
        let posted = tokio::task::spawn_blocking(move || sink.post(&notification)).await;
        match posted {
            Ok(Ok(())) => ArmTally::bump(&tally.posted),
            Ok(Err(e)) => {
                ArmTally::bump(&tally.failed);
                tracing::warn!("push arm: the banner was not posted: {e}");
            }
            Err(e) => {
                ArmTally::bump(&tally.failed);
                tracing::warn!("push arm: the post task failed: {e}");
            }
        }
    }
}

/// Wake [`host`] to re-read the capability now — called wherever the slot it
/// keys on changes (provision, `RefreshBearer`, un-provision, a refused
/// renewal), beside `crate::account_host::recheck_now`.
pub(crate) fn recheck_now(state: &SyncServiceState) {
    state.push_arm_wake.notify_one();
}

/// The push arm's host task: spawned by `run_agent` beside the renewal,
/// custodian and account hosts, and returns when `shutdown` flips.
///
/// While a capability is provisioned with a live bearer it holds one arm on
/// the retained capability connection ([`arm_stint`]); unprovisioned or
/// refused, it idles at one capability read per [`IDLE_RECHECK_SECS`]. Never
/// gated on the account-store mount or on a platform: on a machine with no
/// notification sink the arm counts `no_sink` and posts nothing.
pub async fn host(state: Arc<SyncServiceState>, mut shutdown: watch::Receiver<bool>) {
    let mut backoff = fauna_protocol::reconnect::Backoff::new(
        Duration::from_secs(ARM_BACKOFF_START_SECS),
        Duration::from_secs(ARM_BACKOFF_MAX_SECS),
    );
    loop {
        // Re-read at the top of every pass: the stint's own select may have
        // consumed the shutdown transition (`account_host::run`'s reasoning).
        if *shutdown.borrow() {
            return;
        }
        let delay = match arm_stint(&state, &mut shutdown).await {
            Stint::Ended => {
                backoff.reset();
                Duration::from_secs(1)
            }
            Stint::Idle => Duration::from_secs(IDLE_RECHECK_SECS),
            Stint::Unavailable(e) => {
                tracing::debug!(
                    error = %e,
                    retry_in_secs = backoff.ceiling().as_secs(),
                    "push arm: capability connection unavailable; will retry"
                );
                let d = backoff.ceiling();
                backoff.grow();
                d
            }
            Stint::Shutdown => return,
        };
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = state.push_arm_wake.notified() => {}
            _ = shutdown.changed() => {}
        }
    }
}

/// How one [`arm_stint`] ended.
enum Stint {
    /// The arm ran and its connection went (or was replaced): re-arm at once.
    Ended,
    /// Nothing to arm on: no capability, or its renewal stands refused.
    Idle,
    /// The capability connection could not be had: retry on the backoff.
    Unavailable(String),
    Shutdown,
}

/// Whether the slot holds a capability with a bearer to ride — provisioned,
/// and its renewal not refused (the account host's `host_inputs` shape, minus
/// any enrollment).
async fn capability_armed(state: &SyncServiceState) -> bool {
    state
        .capability
        .read()
        .await
        .as_ref()
        .is_some_and(|cap| !crate::bearer::renewal_refused(cap))
}

/// Arm on the retained capability connection and hold the arm while that
/// connection is still the retained one.
///
/// The arm keeps the client's push stream, never the client: holding the
/// `Arc` would keep the broadcast — and the socket — open for ever, so the arm
/// ends by itself when the last holder drops the client ([`run`] returns on
/// `Closed`). And it re-checks the slot at every wake and every
/// [`IDLE_RECHECK_SECS`], ending the moment the capability is gone or refused
/// or the retained client is another one (a re-provision under a new
/// `device_id`, a stopped supervisor replaced) — another holder of the old
/// client, such as a mount still coming down, never keeps the arm on it.
async fn arm_stint(state: &Arc<SyncServiceState>, shutdown: &mut watch::Receiver<bool>) -> Stint {
    if !capability_armed(state).await {
        return Stint::Idle;
    }
    let client = match state.nest_rpc_client().await {
        Ok(client) => client,
        Err(e) => return Stint::Unavailable(e),
    };
    let pushes = client.subscribe_pushes();
    let armed_on = Arc::downgrade(&client);
    drop(client);

    let arm = run(
        pushes,
        Arc::clone(&state.attached_apps),
        Arc::clone(&state.notification_sink),
        Arc::clone(&state.push_tally),
    );
    tokio::pin!(arm);
    loop {
        tokio::select! {
            () = &mut arm => return Stint::Ended,
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return Stint::Shutdown;
                }
            }
            () = state.push_arm_wake.notified() => {
                if !still_retained(state, &armed_on).await {
                    return Stint::Ended;
                }
            }
            () = tokio::time::sleep(Duration::from_secs(IDLE_RECHECK_SECS)) => {
                if !still_retained(state, &armed_on).await {
                    return Stint::Ended;
                }
            }
        }
    }
}

/// Whether `armed_on` is still the retained capability client for a live
/// capability. Resolving it rebuilds a client whose identity moved or whose
/// supervisor stopped — which is the arm's cue to move onto the new one.
async fn still_retained(
    state: &SyncServiceState,
    armed_on: &std::sync::Weak<fauna_client::NestClient>,
) -> bool {
    if !capability_armed(state).await {
        return false;
    }
    match state.nest_rpc_client().await {
        Ok(current) => std::ptr::eq(armed_on.as_ptr(), Arc::as_ptr(&current)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSink(Mutex<Vec<String>>);
    impl NotificationSink for FakeSink {
        fn availability(&self) -> Option<bool> {
            Some(true)
        }
        fn probe(&self) {}
        fn post(&self, n: &PushNotificationPayload) -> Result<(), String> {
            self.0.lock().unwrap().push(n.title.clone());
            Ok(())
        }
    }

    /// A sink with nowhere to post.
    struct AbsentSink;
    impl NotificationSink for AbsentSink {
        fn availability(&self) -> Option<bool> {
            Some(false)
        }
        fn probe(&self) {}
        fn post(&self, _: &PushNotificationPayload) -> Result<(), String> {
            unreachable!("the arm never posts to an absent sink")
        }
    }

    /// A sink the platform refuses every post on.
    struct RefusingSink;
    impl NotificationSink for RefusingSink {
        fn availability(&self) -> Option<bool> {
            Some(true)
        }
        fn probe(&self) {}
        fn post(&self, _: &PushNotificationPayload) -> Result<(), String> {
            Err("refused".into())
        }
    }

    fn frame(title: &str) -> PushEvent {
        PushEvent::PushNotification(PushNotificationPayload {
            title: title.into(),
            body: "body".into(),
            url: "/app/inbox".into(),
            extra: Default::default(),
        })
    }

    /// Feed `events` through the arm and return its tally. Closing the channel
    /// ends the arm, so the run is deterministic: no settle sleep.
    async fn tally_of(
        events: Vec<PushEvent>,
        attached: Arc<AttachedApps>,
        sink: Arc<dyn NotificationSink>,
    ) -> fauna_ipc::sync::PushArmTally {
        let (tx, rx) = broadcast::channel(16);
        for e in events {
            tx.send(e).unwrap();
        }
        drop(tx);
        let tally = Arc::<ArmTally>::default();
        run(rx, Arc::clone(&attached), sink, Arc::clone(&tally)).await;
        tally.snapshot(&attached)
    }

    /// Feed `events` through the arm and return what it posted.
    async fn posted(
        events: Vec<PushEvent>,
        attached: Arc<AttachedApps>,
        sink: Arc<FakeSink>,
    ) -> Vec<String> {
        tally_of(
            events,
            attached,
            Arc::clone(&sink) as Arc<dyn NotificationSink>,
        )
        .await;
        sink.0.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn with_no_app_attached_a_frame_is_posted_once() {
        let posts = posted(vec![frame("New message")], Arc::default(), Arc::default()).await;
        assert_eq!(posts, vec!["New message".to_string()]);
    }

    #[tokio::test]
    async fn while_an_app_is_attached_nothing_is_posted() {
        let attached = Arc::<AttachedApps>::default();
        let _lease = attached.attach();
        let posts = posted(vec![frame("New message")], attached, Arc::default()).await;
        assert!(posts.is_empty(), "the open app owns the banner: {posts:?}");
    }

    #[tokio::test]
    async fn once_the_app_detaches_the_agent_posts_again() {
        let attached = Arc::<AttachedApps>::default();
        drop(attached.attach());
        assert!(!attached.any());
        let posts = posted(vec![frame("Group invite")], attached, Arc::default()).await;
        assert_eq!(posts, vec!["Group invite".to_string()]);
    }

    /// Every heard frame lands in exactly one outcome count — what the
    /// end-to-end witness reads to know a withheld frame was decided.
    #[tokio::test]
    async fn the_tally_counts_each_decision_once_by_outcome() {
        let posted_twice = tally_of(
            vec![frame("a"), frame("b")],
            Arc::default(),
            Arc::new(FakeSink::default()),
        )
        .await;
        assert_eq!(
            posted_twice,
            fauna_ipc::sync::PushArmTally {
                posted: 2,
                ..Default::default()
            }
        );

        let attached = Arc::<AttachedApps>::default();
        let _lease = attached.attach();
        let withheld = tally_of(
            vec![frame("a")],
            Arc::clone(&attached),
            Arc::new(FakeSink::default()),
        )
        .await;
        assert_eq!(
            withheld,
            fauna_ipc::sync::PushArmTally {
                attached_apps: 1,
                withheld: 1,
                ..Default::default()
            }
        );

        let no_sink = tally_of(vec![frame("a")], Arc::default(), Arc::new(AbsentSink)).await;
        assert_eq!(
            no_sink,
            fauna_ipc::sync::PushArmTally {
                no_sink: 1,
                ..Default::default()
            }
        );

        let failed = tally_of(vec![frame("a")], Arc::default(), Arc::new(RefusingSink)).await;
        assert_eq!(
            failed,
            fauna_ipc::sync::PushArmTally {
                failed: 1,
                ..Default::default()
            }
        );
    }
}

/// The windows sink against the REAL notification platform: what the health
/// reply reports, and that a post lands under the app's identity. The identity
/// is registered per run the way an unpackaged app's toast registration is
/// (`HKCU\Software\Classes\AppUserModelId\<aumid>`) and removed afterwards;
/// the toast is delivered without its on-screen banner and removed from the
/// notification centre, so a run paints nothing.
/// tier_1: what a tap on a linux agent banner launches — the argv asserted at
/// the seam, against a stated host (which commands exist, where the desktop
/// entry is). That the launched window *looks* right is the human's last inch;
/// that the banner reaches the daemon is the linux end-to-end witness's.
#[cfg(all(test, target_os = "linux"))]
mod tap_tests {
    use std::path::PathBuf;

    use super::{DesktopSink, NotificationSink, tap};

    fn host(present: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |command| {
            present
                .contains(&command)
                .then(|| PathBuf::from(format!("/opt/bin/{command}")))
        }
    }

    fn entry() -> Option<PathBuf> {
        Some(PathBuf::from(
            "/usr/share/applications/social.fauna.fauna.desktop",
        ))
    }

    fn no_entry() -> Option<PathBuf> {
        None
    }

    fn argv(app: Option<&str>, url: &str, present: &'static [&'static str]) -> Option<Vec<String>> {
        tap::launch_argv(app, url, &host(present), &entry)
    }

    #[test]
    fn the_linux_app_launches_through_its_desktop_entry() {
        assert_eq!(
            argv(Some("linux"), "/app/contacts", &["gtk-launch", "gio"]).unwrap(),
            ["/opt/bin/gtk-launch", "social.fauna.fauna.desktop"]
        );
        // No gtk-launch: gio launches the entry file the XDG search found.
        assert_eq!(
            argv(Some("linux"), "/app", &["gio"]).unwrap(),
            [
                "/opt/bin/gio",
                "launch",
                "/usr/share/applications/social.fauna.fauna.desktop"
            ]
        );
        assert_eq!(
            tap::launch_argv(Some("linux"), "/app", &host(&["gio"]), &no_entry),
            None,
            "gio with no installed entry has nothing to launch"
        );
        assert_eq!(argv(Some("linux"), "/app", &[]), None);
    }

    #[test]
    fn tui_launches_in_the_users_terminal_where_both_are_present() {
        assert_eq!(
            argv(Some("tui"), "/app", &["x-terminal-emulator", "fauna-tui"]).unwrap(),
            ["/opt/bin/x-terminal-emulator", "-e", "/opt/bin/fauna-tui"]
        );
        assert_eq!(
            argv(Some("tui"), "/app", &["fauna-tui"]),
            None,
            "no terminal"
        );
        assert_eq!(
            argv(Some("tui"), "/app", &["x-terminal-emulator"]),
            None,
            "no tui"
        );
    }

    #[test]
    fn no_attached_app_or_an_unknown_one_launches_nothing() {
        let all = &["gtk-launch", "gio", "x-terminal-emulator", "fauna-tui"];
        assert_eq!(argv(None, "/app", all), None);
        assert_eq!(argv(Some("windows"), "/app", all), None);
    }

    #[test]
    fn only_a_fauna_route_rides_along_and_it_is_re_minted() {
        // The parse's lenient spelling goes out in the canonical one.
        let route = " FAUNA://Folder-Share/?folder=7 ";
        let launched = argv(Some("tui"), route, &["x-terminal-emulator", "fauna-tui"]).unwrap();
        assert_eq!(launched.last().unwrap(), "fauna://folder-share?folder=7");
        assert_eq!(launched.len(), 4);
        let launched = argv(Some("linux"), route, &["gtk-launch"]).unwrap();
        assert_eq!(launched.len(), 3);
        // A web path, or remote text that is no route, never reaches the argv.
        for url in [
            "/app/contacts",
            "https://example.org",
            "--help",
            "fauna://evil; rm -rf ~",
        ] {
            assert_eq!(
                argv(Some("linux"), url, &["gtk-launch"]).unwrap().len(),
                2,
                "{url}"
            );
        }
    }

    #[test]
    fn the_attached_app_is_kept_across_a_restart_and_a_bad_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(tap::APP_FILE);
        let sink = DesktopSink::new(file.clone());
        assert_eq!(sink.app(), None);
        sink.adopt_app("tui");
        assert_eq!(sink.app().as_deref(), Some("tui"));
        sink.adopt_app("linux\n--evil");
        assert_eq!(
            sink.app().as_deref(),
            Some("tui"),
            "an unusable name is refused"
        );
        sink.adopt_app("linux");
        assert_eq!(
            DesktopSink::new(file.clone()).app().as_deref(),
            Some("linux")
        );
        std::fs::write(&file, "../x").unwrap();
        assert_eq!(
            DesktopSink::new(file).app(),
            None,
            "a tampered file keeps nothing"
        );
    }
}

#[cfg(all(test, windows))]
mod toast_tests {
    use super::toast::{self, ToastSink};
    use super::*;
    use windows::UI::Notifications::ToastNotificationManager;
    use windows::core::HSTRING;

    const KEY: &str = r"HKCU\Software\Classes\AppUserModelId";

    /// A per-run AUMID with a toast registration, unregistered on drop.
    struct RegisteredIdentity(String);

    impl RegisteredIdentity {
        fn new(tag: &str) -> Self {
            let id = format!("Fauna.AgentToastTest.{tag}.{}", std::process::id());
            let status = std::process::Command::new("reg")
                .args(["add", &format!(r"{KEY}\{id}"), "/v", "DisplayName"])
                .args(["/t", "REG_SZ", "/d", "Fauna agent test", "/f"])
                .output()
                .expect("reg.exe runs");
            assert!(status.status.success(), "registering {id}: {status:?}");
            Self(id)
        }
    }

    impl Drop for RegisteredIdentity {
        fn drop(&mut self) {
            let _ = std::process::Command::new("reg")
                .args(["delete", &format!(r"{KEY}\{}", self.0), "/f"])
                .output();
        }
    }

    fn sink_in(dir: &tempfile::TempDir) -> ToastSink {
        ToastSink::new(dir.path().join(toast::IDENTITY_FILE))
    }

    #[test]
    fn with_no_app_identity_there_is_no_sink() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink_in(&dir);
        sink.probe();
        assert_eq!(sink.availability(), Some(false));
        assert!(sink.post(&payload("New message")).is_err());
    }

    #[test]
    fn an_identity_with_no_toast_registration_is_no_sink() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink_in(&dir);
        sink.adopt_identity(&format!(
            "Fauna.AgentToastTest.unregistered.{}",
            std::process::id()
        ));
        assert_eq!(sink.availability(), Some(false));
    }

    #[test]
    fn an_unusable_identity_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sink = sink_in(&dir);
        sink.probe();
        sink.adopt_identity("bad\nid");
        sink.adopt_identity(&"x".repeat(300));
        assert!(!dir.path().join(toast::IDENTITY_FILE).exists());
        assert_eq!(sink.availability(), Some(false));
    }

    /// The arm's whole windows path: the app's identity adopted → the sink
    /// reports available → the identity survives an agent restart → a toast
    /// posted under it is in that identity's notification-centre history.
    #[test]
    fn the_apps_identity_is_a_sink_kept_across_a_restart_and_a_toast_lands_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = RegisteredIdentity::new("posts");
        let sink = sink_in(&dir);
        sink.adopt_identity(&app.0);
        assert_eq!(
            sink.availability(),
            Some(true),
            "the registered identity is a sink"
        );

        // A restarted agent reads the kept identity before any app attaches.
        let restarted = sink_in(&dir);
        restarted.probe();
        assert_eq!(
            restarted.availability(),
            Some(true),
            "the identity was kept"
        );

        let (tag, group) = ("push-arm", "fauna-agent-test");
        toast::show(&app.0, &payload("New message"), Some((tag, group)), false)
            .expect("the toast posts under the app's identity");
        let history = ToastNotificationManager::History().unwrap();
        let tags: Vec<String> = history
            .GetHistoryWithId(&HSTRING::from(&app.0))
            .unwrap()
            .into_iter()
            .filter_map(|t| t.Tag().ok().map(|s| s.to_string()))
            .collect();
        let _ = history.RemoveGroupedTagWithId(
            &HSTRING::from(tag),
            &HSTRING::from(group),
            &HSTRING::from(&app.0),
        );
        assert!(
            tags.iter().any(|t| t == tag),
            "history for {}: {tags:?}",
            app.0
        );
    }

    #[test]
    fn remote_text_cannot_break_out_of_the_toast_xml() {
        let xml = toast::toast_xml("a<b>&\"c'", "</text><image src=\"x\"/>\u{1b}");
        assert!(
            xml.contains("<text>a&lt;b&gt;&amp;&quot;c&apos;</text>"),
            "{xml}"
        );
        assert!(!xml.contains("<image"), "{xml}");
        assert!(!xml.contains('\u{1b}'), "{xml}");
    }

    fn payload(title: &str) -> PushNotificationPayload {
        PushNotificationPayload {
            title: title.into(),
            body: "body".into(),
            url: "/app/inbox".into(),
            extra: Default::default(),
        }
    }
}

/// [`host`]: the announce and the arm ride the capability connection, from
/// provision until un-provision or a refused renewal, with no enrollment and
/// no account-store mount (`common.md` § Push Notifications → *Transports*,
/// ruled 2026-10-09). The seam is the real retained client
/// (`SyncServiceState::nest_rpc_client`) over an unreachable nest: what it
/// announces is read off `NestClient::push_presence`, and whether the arm is
/// listening off its push stream's receiver count — no fake `NestClient`
/// exists, and none is needed for either fact.
#[cfg(test)]
mod host_tests {
    use super::*;

    /// Port 1 on loopback: the client builds and its supervisor dials, never
    /// reaching a nest — what an unenrolled machine's agent looks like to the
    /// arm until the nest answers.
    const UNREACHABLE_NEST: &str = "http://127.0.0.1:1";

    fn capability(device_id: &str) -> fauna_ipc::sync::SyncCapability {
        fauna_ipc::sync::SyncCapability::new(
            vec![1u8; 32],
            vec![7u8; 32],
            UNREACHABLE_NEST.into(),
            device_id.into(),
            fauna_ipc::sync::BearerToken::new("tok".into(), u64::MAX),
        )
    }

    /// A provisioned agent with no credential store — so no enrollment, and no
    /// account host running: nothing but the capability.
    async fn provisioned(device_id: &str) -> (Arc<SyncServiceState>, watch::Sender<bool>) {
        let (shutdown_tx, _rx) = watch::channel(false);
        let (event_tx, _) = broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx.clone(),
            event_tx,
            crate::config::SyncPaths::new(None).expect("this box has an absolute home"),
        );
        *state.capability.write().await = Some(capability(device_id));
        tokio::spawn(host(Arc::clone(&state), shutdown_tx.subscribe()));
        (state, shutdown_tx)
    }

    /// The retained capability client, if one is held.
    async fn retained(state: &SyncServiceState) -> Option<Arc<fauna_client::NestClient>> {
        // Through the slot, never `nest_rpc_client`: reading must not build.
        state
            .nest_rpc
            .lock()
            .await
            .as_ref()
            .map(|r| Arc::clone(r.client_for_test()))
    }

    /// Poll a predicate across scheduler passes, bounded (convention 14: a
    /// state poll, never a settle sleep). Panics with `what` on the bound.
    async fn eventually<F, Fut>(what: &str, mut pred: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let polled = tokio::time::timeout(Duration::from_secs(10), async {
            while !pred().await {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(polled.is_ok(), "never: {what}");
    }

    /// The armed client: retained, announcing `device_id` as a stand-in, and
    /// listened on by exactly the arm.
    async fn armed_on(state: &SyncServiceState, device_id: &str) -> Arc<fauna_client::NestClient> {
        eventually(
            "the arm listens on the retained capability client",
            || async {
                retained(state).await.is_some_and(|c| {
                    c.push_receiver_count() == 1
                        && c.push_presence().is_some_and(|p| p.device_id == device_id)
                })
            },
        )
        .await;
        let client = retained(state).await.expect("retained");
        let presence = client.push_presence().expect("announces");
        assert!(
            presence.stand_in,
            "the agent announces as the wake stand-in"
        );
        client
    }

    async fn arm_ends_on(client: &fauna_client::NestClient) {
        eventually("the arm stops listening", || async {
            client.push_receiver_count() == 0
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_provisioned_unenrolled_agent_announces_and_arms() {
        let (state, shutdown) = provisioned("dev-1").await;
        armed_on(&state, "dev-1").await;
        shutdown.send(true).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_renewal_ends_the_arm_and_closes_the_connection() {
        let (state, shutdown) = provisioned("dev-1").await;
        let client = armed_on(&state, "dev-1").await;

        crate::renewal::refuse_renewal(
            &state,
            &crate::renewal::SlotKey {
                nest_url: UNREACHABLE_NEST.into(),
                actor_id: [7u8; 32],
            },
            crate::credentials::RefusalRecord {
                nest_url: UNREACHABLE_NEST.into(),
                principal_key: hex::encode([0xc3u8; 32]),
                grant_registered: false,
            },
        )
        .await;

        assert!(
            retained(&state).await.is_none(),
            "the refusal drops the open capability connection, and its announce with it"
        );
        arm_ends_on(&client).await;
        // And nothing re-arms while the refusal stands: the host idles.
        recheck_now(&state);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            retained(&state).await.is_none(),
            "a refused credential dials nothing"
        );

        // An app's `RefreshBearer` re-arms it: a fresh connection, announced.
        state
            .capability
            .write()
            .await
            .as_mut()
            .expect("the capability stays")
            .bearer = fauna_ipc::sync::BearerToken::new("tok-2".into(), u64::MAX);
        recheck_now(&state);
        let rearmed = armed_on(&state, "dev-1").await;
        assert!(!Arc::ptr_eq(&rearmed, &client));
        shutdown.send(true).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unprovision_ends_the_arm() {
        let (state, shutdown) = provisioned("dev-1").await;
        let client = armed_on(&state, "dev-1").await;

        assert!(crate::pipe_server::unprovision_now(&state).await);

        arm_ends_on(&client).await;
        assert!(retained(&state).await.is_none());
        shutdown.send(true).unwrap();
    }

    /// The same actor re-provisioned under a new device id: the retained
    /// client is rebuilt announcing the new id and the arm moves onto it —
    /// even while something else still holds the old client (here, the test).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reprovision_under_a_new_device_id_re_announces() {
        let (state, shutdown) = provisioned("dev-1").await;
        let old = armed_on(&state, "dev-1").await;

        *state.capability.write().await = Some(capability("dev-2"));
        recheck_now(&state);

        let new = armed_on(&state, "dev-2").await;
        assert!(!Arc::ptr_eq(&new, &old));
        arm_ends_on(&old).await;
        shutdown.send(true).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_host_returns_on_shutdown() {
        let (shutdown_tx, _rx) = watch::channel(false);
        let (event_tx, _) = broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx.clone(),
            event_tx,
            crate::config::SyncPaths::new(None).expect("this box has an absolute home"),
        );
        *state.capability.write().await = Some(capability("dev-1"));
        let task = tokio::spawn(host(Arc::clone(&state), shutdown_tx.subscribe()));
        armed_on(&state, "dev-1").await;
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the host exits on shutdown while armed")
            .unwrap();
    }
}
