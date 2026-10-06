use crate::i18n::strings::common;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

static TRAY_STATE: OnceLock<TrayState> = OnceLock::new();

/// When set to `true` by the tray thread, the GTK poll loop will raise the
/// main window on the next tick.
pub static TRAY_RAISE: AtomicBool = AtomicBool::new(false);

/// The most recent xdg-activation token handed to us by the Wayland tray host
/// (GNOME's appindicator extension) via the StatusNotifierItem
/// `ProvideXdgActivationToken` call. On Wayland the compositor (mutter) refuses
/// to raise a window unless the activation request carries a token minted from a
/// real input event — and a tray click lands on the shell's surface, not ours, so
/// only the host can mint it. The host calls `ProvideXdgActivationToken` (handled
/// in our patched `ksni`, see libs/ksni/PATCH.md) *just before* the "Open Fauna"
/// menu activation, so by the time `TRAY_RAISE` is observed this holds the fresh
/// token. `show_window` feeds it to `gtk::Window::set_startup_id` before
/// `present()`; without it mutter denies the raise and posts the passive
/// "Fauna is ready" notification instead. Written on the ksni D-Bus thread, read
/// on the GTK main thread — hence the mutex.
static ACTIVATION_TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// Store the latest xdg-activation token from the tray host. Called on the ksni
/// D-Bus thread.
pub fn set_activation_token(token: String) {
    *ACTIVATION_TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
}

/// Take (consume) the pending xdg-activation token, if any. Called on the GTK
/// main thread right before presenting the window.
pub fn take_activation_token() -> Option<String> {
    ACTIVATION_TOKEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

/// When set to `true` together with `TRAY_RAISE`, the GTK poll loop will
/// additionally open the compose dialog after raising the window.
pub static TRAY_COMPOSE: AtomicBool = AtomicBool::new(false);

/// Controls close-to-tray behaviour. When `true`, closing the window hides
/// it instead of quitting the application. Toggled from the General
/// preferences page.
///
/// **This is a cache of the persisted choice, not the source of truth** —
/// `app_settings::apply_saved_close_to_tray()` loads it at startup and the
/// General page writes both on a user toggle. The `false` here is only the
/// pre-load value; the *product* default is ON (`app_settings`), because the
/// app hosts the in-process sync engine and closing a window must not silently
/// stop sync (`apps/windows.md` § App Lifecycle, shape A). Hiding stays
/// gated on a live tray host — see `should_hide_to_tray`.
pub static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(false);

/// Whether a system-tray host (`org.kde.StatusNotifierWatcher`) is currently
/// present on the session bus. Updated dynamically by the ksni `watcher_online`
/// / `watcher_offine` hooks (a host can appear/disappear at runtime — e.g.
/// enabling/disabling GNOME's appindicator extension). Defaults to `false`: we
/// only ever hide-to-tray once a host is *confirmed*, so a window can never be
/// stranded with no affordance to restore it (stock GNOME ships no tray host).
pub static TRAY_HOST_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether the tray host question has been *answered* yet — set by both the
/// `watcher_online` and `watcher_offine` hooks, so it distinguishes "no host"
/// from "we have not heard back from ksni yet".
///
/// `TRAY_HOST_AVAILABLE` alone cannot: it defaults `false` and is only stored
/// asynchronously once ksni's `RegisterStatusNotifierItem` returns, so reading
/// it at launch would say "no host" on every start, whatever the truth. The
/// `--autostart` presentation decision waits for *this* flag instead
/// (`crate::autostart`, `apps/linux.md` § Auto-start at sign-in).
///
/// ⚠ It is **not** guaranteed to be set: ksni only calls the hooks on `Ok` and
/// on `ServiceUnknown`, so a different D-Bus error answers neither way. Any
/// waiter must therefore be bounded and fail safe to *visible* rather than
/// wait forever (`libs/ksni/src/service.rs`).
pub static TRAY_HOST_ANSWERED: AtomicBool = AtomicBool::new(false);

/// Whether closing the window should hide it to the tray rather than quit.
/// True only when the user enabled close-to-tray AND a tray host is present to
/// restore the window from — otherwise hiding would strand the window with no
/// way to bring it back (out-of-the-box trap on stock GNOME, which has no
/// `StatusNotifierWatcher`). The X button, Ctrl+W, and Escape all route through
/// this.
pub fn should_hide_to_tray() -> bool {
    CLOSE_TO_TRAY.load(Ordering::SeqCst) && TRAY_HOST_AVAILABLE.load(Ordering::SeqCst)
}

/// Whether a system-tray host is currently present. Used by the explicit
/// "hide to tray" shortcut (Ctrl+Shift+H) and the General preferences page to
/// avoid offering close-to-tray when there is nowhere to restore from.
pub fn tray_host_available() -> bool {
    TRAY_HOST_AVAILABLE.load(Ordering::SeqCst)
}

/// One-shot bypass for the close-to-tray handler. The sign-out flow sets
/// this before closing the main window so close() actually destroys the
/// window — and drops the widget tree's strong refs to the FaunaClient —
/// instead of just hiding it (which leaves the runtime alive and silently
/// hammering background tasks). The close-request handler swaps it back to
/// false on use, preserving the user's saved CLOSE_TO_TRAY preference.
pub static SIGNING_OUT: AtomicBool = AtomicBool::new(false);

/// Set to `true` while the main application window has keyboard focus.
/// Notifications are suppressed while this is `true` to avoid interrupting
/// the user when they are already looking at the app.
pub static WINDOW_FOCUSED: AtomicBool = AtomicBool::new(false);

/// When `true`, notifications include a freedesktop sound hint so the
/// notification daemon plays the standard "message-new-instant" sound.
/// Toggled from the General preferences page.
pub static NOTIFICATION_SOUND: AtomicBool = AtomicBool::new(true);

/// Return a reference to the global `TrayState`.
///
/// Panics if called before `start_tray()`.
pub fn tray_state() -> &'static TrayState {
    TRAY_STATE.get().expect("tray not started")
}

/// Shared tray state that the main application can update.
pub struct TrayState {
    pub unread_count: Arc<Mutex<u32>>,
    pub connected: Arc<Mutex<bool>>,
}

struct FaunaTray {
    unread_count: Arc<Mutex<u32>>,
    connected: Arc<Mutex<bool>>,
}

impl ksni::Tray for FaunaTray {
    fn id(&self) -> String {
        "fauna-desktop".into()
    }

    /// The tray host (GNOME's appindicator extension) calls this with a fresh
    /// xdg-activation token just before dispatching the "Open Fauna" menu
    /// activation. Stash it so `show_window` can hand it to GTK and have mutter
    /// honour the window raise. See `ACTIVATION_TOKEN` and libs/ksni/PATCH.md.
    fn provide_xdg_activation_token(&mut self, token: String) {
        set_activation_token(token);
    }

    /// A `org.kde.StatusNotifierWatcher` is now present — close-to-tray has
    /// somewhere to restore the window from, so honour the user's setting.
    fn watcher_online(&self) {
        tracing::info!("[tray] StatusNotifierWatcher online — close-to-tray enabled");
        TRAY_HOST_AVAILABLE.store(true, Ordering::SeqCst);
        TRAY_HOST_ANSWERED.store(true, Ordering::SeqCst);
    }

    /// No tray host on the bus (none at startup, or one disappeared). Mark it
    /// unavailable so `should_hide_to_tray` falls back to quit-on-close instead
    /// of hiding into a tray that isn't there. Return `true` to keep the tray
    /// service running so it re-registers if a host appears later.
    fn watcher_offine(&self) -> bool {
        tracing::info!("[tray] no StatusNotifierWatcher — close-to-tray will quit instead of hide");
        TRAY_HOST_AVAILABLE.store(false, Ordering::SeqCst);
        TRAY_HOST_ANSWERED.store(true, Ordering::SeqCst);
        true
    }

    fn title(&self) -> String {
        "Fauna".into()
    }

    fn icon_name(&self) -> String {
        let connected = *self.connected.lock().unwrap_or_else(|e| e.into_inner());
        if connected {
            "network-idle-symbolic".into()
        } else {
            "network-offline-symbolic".into()
        }
    }

    fn attention_icon_name(&self) -> String {
        "mail-unread-symbolic".into()
    }

    fn status(&self) -> ksni::Status {
        let unread = *self.unread_count.lock().unwrap_or_else(|e| e.into_inner());
        if unread > 0 {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let unread = *self.unread_count.lock().unwrap_or_else(|e| e.into_inner());
        let connected = *self.connected.lock().unwrap_or_else(|e| e.into_inner());

        let description = if !connected {
            format!("Fauna \u{2014} {}", common::DISCONNECTED)
        } else if unread > 0 {
            format!(
                "Fauna \u{2014} {}",
                common::unread_count(&unread.to_string())
            )
        } else {
            format!("Fauna \u{2014} {}", common::CONNECTED)
        };

        ksni::ToolTip {
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
            title: "Fauna".into(),
            description,
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: format!("{} Fauna", common::OPEN),
                activate: Box::new(|_| {
                    TRAY_RAISE.store(true, Ordering::SeqCst);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: format!(
                    "{}\u{2026}",
                    crate::i18n::strings::conversations::compose::TITLE
                ),
                activate: Box::new(|_| {
                    TRAY_COMPOSE.store(true, Ordering::SeqCst);
                    TRAY_RAISE.store(true, Ordering::SeqCst);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: common::QUIT.into(),
                activate: Box::new(|_| {
                    std::process::exit(0);
                }),
                ..Default::default()
            }),
        ]
    }
}

/// Start the system tray icon in a background thread.
///
/// Initialises the global `TRAY_STATE` and returns a reference to it.
/// Must be called exactly once, before any call to `tray_state()`.
pub fn start_tray() -> &'static TrayState {
    let unread_count = Arc::new(Mutex::new(0u32));
    let connected = Arc::new(Mutex::new(false));

    let tray = FaunaTray {
        unread_count: Arc::clone(&unread_count),
        connected: Arc::clone(&connected),
    };

    // ksni::TrayService runs its own event loop on a dedicated thread. Inside a
    // Flatpak sandbox (FLATPAK_ID set) the D-Bus proxy refuses the per-item
    // well-known name (`org.kde.StatusNotifierItem-<pid>-<n>` cannot be granted
    // via finish-args), so register via the unique bus name instead — the
    // sandbox path ksni ships for exactly this; the watcher accepts both forms.
    let service = ksni::TrayService::new(tray);
    if std::env::var_os("FLATPAK_ID").is_some() {
        service.spawn_without_dbus_name();
    } else {
        service.spawn();
    }

    TRAY_STATE
        .set(TrayState {
            unread_count,
            connected,
        })
        .unwrap_or_else(|_| panic!("start_tray called more than once"));

    TRAY_STATE.get().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `should_hide_to_tray` must hide ONLY when close-to-tray is on AND a tray
    /// host is present. The three other combinations quit-on-close, so a window
    /// can never be hidden with no affordance to restore it (Track 4 trap). All
    /// combinations live in one test because `CLOSE_TO_TRAY`/`TRAY_HOST_AVAILABLE`
    /// are process-global; we restore them before returning.
    #[test]
    fn hide_to_tray_requires_both_setting_and_host() {
        let saved_close = CLOSE_TO_TRAY.load(Ordering::SeqCst);
        let saved_host = TRAY_HOST_AVAILABLE.load(Ordering::SeqCst);

        for (close, host, expected) in [
            (false, false, false),
            (false, true, false),
            (true, false, false), // the trap: enabled but no host → must NOT hide
            (true, true, true),
        ] {
            CLOSE_TO_TRAY.store(close, Ordering::SeqCst);
            TRAY_HOST_AVAILABLE.store(host, Ordering::SeqCst);
            assert_eq!(
                should_hide_to_tray(),
                expected,
                "close_to_tray={close}, host_available={host}"
            );
            assert_eq!(tray_host_available(), host);
        }

        CLOSE_TO_TRAY.store(saved_close, Ordering::SeqCst);
        TRAY_HOST_AVAILABLE.store(saved_host, Ordering::SeqCst);
    }
}
