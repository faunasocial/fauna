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
//! presence (`NestClient::set_push_presence`, in the account host) and posts.
//! A machine with no desktop session has no sink; the agent says so on
//! `ServiceStatusInfo::notification_sink` and the app's control renders its
//! failure line.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use fauna_protocol::PushEvent;
use fauna_protocol::push_events::PushNotificationPayload;
use tokio::sync::broadcast;

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
        self.0.load(Ordering::SeqCst) > 0
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
}

/// The sink this platform's agent posts through. linux: the freedesktop
/// notification daemon through `notify-rust`, present when a desktop session
/// is (the `keyring_probe` selector tui already trusts — never an env knob).
/// windows: the WinRT toast under the identity the app attached with, kept in
/// `base_dir` across the agent's restarts ([`toast`]). macOS: the arm is
/// unbuilt (the mac row's), so the agent honestly reports no sink.
pub fn platform_sink(base_dir: &std::path::Path) -> Arc<dyn NotificationSink> {
    #[cfg(target_os = "linux")]
    {
        let _ = base_dir;
        Arc::new(DesktopSink::default())
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
#[derive(Default)]
struct DesktopSink {
    present: std::sync::OnceLock<bool>,
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
        notify_rust::Notification::new()
            .summary(&n.title)
            .body(&n.body)
            .appname("Fauna")
            .show()
            .map(|_| ())
            .map_err(|e| e.to_string())
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
pub async fn run(
    mut pushes: broadcast::Receiver<PushEvent>,
    attached: Arc<AttachedApps>,
    sink: Arc<dyn NotificationSink>,
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
            continue;
        }
        if sink.availability() != Some(true) {
            continue;
        }
        let sink = Arc::clone(&sink);
        let posted = tokio::task::spawn_blocking(move || sink.post(&notification)).await;
        match posted {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("push arm: the banner was not posted: {e}"),
            Err(e) => tracing::warn!("push arm: the post task failed: {e}"),
        }
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

    fn frame(title: &str) -> PushEvent {
        PushEvent::PushNotification(PushNotificationPayload {
            title: title.into(),
            body: "body".into(),
            url: "/app/inbox".into(),
            extra: Default::default(),
        })
    }

    /// Feed `events` through the arm and return what it posted. Closing the
    /// channel ends the arm, so the run is deterministic: no settle sleep.
    async fn posted(
        events: Vec<PushEvent>,
        attached: Arc<AttachedApps>,
        sink: Arc<FakeSink>,
    ) -> Vec<String> {
        let (tx, rx) = broadcast::channel(16);
        for e in events {
            tx.send(e).unwrap();
        }
        drop(tx);
        run(rx, attached, Arc::clone(&sink) as Arc<dyn NotificationSink>).await;
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
}

/// The windows sink against the REAL notification platform: what the health
/// reply reports, and that a post lands under the app's identity. The identity
/// is registered per run the way an unpackaged app's toast registration is
/// (`HKCU\Software\Classes\AppUserModelId\<aumid>`) and removed afterwards;
/// the toast is delivered without its on-screen banner and removed from the
/// notification centre, so a run paints nothing.
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
