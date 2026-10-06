//! The launcher badge — linux's home-screen widget
//! (`docs/goal/architecture/apps/linux.md` § Home-screen widget; the cross-app
//! promise is `common.md` § Home-screen widget).
//!
//! Linux has no one widget host, so the glanceable unread count goes where the
//! most stock desktops already look for one: the **launcher badge**, the
//! `com.canonical.Unity.LauncherEntry` `Update` broadcast on the session bus
//! that Ubuntu's dock, KDE Plasma's task manager, elementary's dock and
//! Dash-to-Dock paint as a number on the app's launcher icon. It is a
//! fire-and-forget broadcast: no host is discovered or required, a desktop
//! with nothing listening simply shows nothing (an upstream stock GNOME — the
//! same shape as the tray-host gate, `linux.md` § System Tray).
//!
//! The number is the tray's number: the shared
//! `fauna_conversations::ConversationsManager::unread_total` (`sum_unread`
//! over every thread — the per-thread `unread_count` the conversations list
//! renders, summed) — so the badge can never show a count the app would not
//! (`ui/conversations.md` § State & data shape → *When a thread is read* owns
//! what "unread" means). `main.rs`'s tray-toast loop reads it once per
//! snapshot tick and hands it to both surfaces.
//!
//! **Main-thread only.** The loop runs on the GLib main context, and
//! `gio::DBusConnection` is not `Send`, so the connection and the last
//! published value live in thread-locals; a call from another thread would
//! open a second connection and re-publish, which is why there is none.

use std::cell::{Cell, RefCell};

use glib::prelude::*;

/// The LauncherEntry interface every badge-painting dock listens on.
const LAUNCHER_ENTRY_IFACE: &str = "com.canonical.Unity.LauncherEntry";

/// The snapcraft `apps:` key — the second half of snapd's desktop-entry id.
const SNAP_APP_NAME: &str = "fauna";

/// The `application://<desktop-id>` URI a dock keys the badge on.
///
/// The desktop id is the installed desktop entry's basename, which differs
/// per channel: native installs (deb, AppImage, `install.sh`) and Flatpak
/// install `<APP_ID>.desktop` (`packaging_identity_test.rs` pins the one
/// spelling; `FLATPAK_ID` *is* `APP_ID` inside the sandbox and is read rather
/// than assumed so a renamed Flatpak still badges its own icon), while snapd
/// rewrites every snap's entries to `<instance>_<app>.desktop`. A badge keyed
/// on a desktop id the dock does not know is simply not painted, so the id
/// must be the one the launcher was clicked through.
pub fn launcher_app_uri(flatpak_id: Option<&str>, snap_instance_name: Option<&str>) -> String {
    let desktop_id = match (flatpak_id, snap_instance_name) {
        (Some(id), _) if !id.is_empty() => format!("{id}.desktop"),
        (_, Some(snap)) if !snap.is_empty() => format!("{snap}_{SNAP_APP_NAME}.desktop"),
        _ => format!("{}.desktop", crate::APP_ID),
    };
    format!("application://{desktop_id}")
}

/// The object path the `Update` signal is emitted from.
///
/// libunity emits from `/com/canonical/unity/launcherentry/<hash of the app
/// uri>`, and snapd's `unity7` interface admits exactly that shape
/// (`path=/com/canonical/unity/launcherentry/[0-9]*`), so the path is a
/// decimal hash of the URI — stable per desktop id, digits only. Docks key on
/// the `app_uri` argument, never the path, so the hash function is free.
pub fn launcher_object_path(app_uri: &str) -> String {
    // FNV-1a, 32-bit: tiny, dependency-free, and only ever a stable label.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in app_uri.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("/com/canonical/unity/launcherentry/{hash}")
}

/// The `Update` signal's arguments for `count`: `(s app_uri, a{sv} props)`
/// with `count` as an int64 and `count-visible` true only above zero, which
/// is what every LauncherEntry consumer reads (a visible zero would paint a
/// "0" badge).
fn update_parameters(app_uri: &str, count: u32) -> glib::Variant {
    let props = glib::VariantDict::new(None);
    props.insert_value("count", &i64::from(count).to_variant());
    props.insert_value("count-visible", &(count > 0).to_variant());
    glib::Variant::tuple_from_iter([app_uri.to_variant(), props.end()])
}

thread_local! {
    /// The session-bus connection, opened on first publish. `Err` once the
    /// bus could not be reached: without a session bus there is no dock to
    /// badge either, so one warning and then silence.
    static CONNECTION: RefCell<Option<Result<gio::DBusConnection, ()>>> = const { RefCell::new(None) };
    /// The last count handed to the bus — a snapshot tick that did not move
    /// the total emits nothing. `None` until the first publish, so a process
    /// start always emits (clearing whatever a previous run left painted).
    static LAST_PUBLISHED: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Publish the total unread count to the launcher badge, if it changed.
///
/// Called from the tray-toast loop on every snapshot tick with the same
/// number the tray tooltip shows. Main-thread only (module doc).
pub fn publish_unread(count: u32) {
    if LAST_PUBLISHED.with(|last| last.get() == Some(count)) {
        return;
    }
    let Some(conn) = connection() else {
        return;
    };
    let app_uri = launcher_app_uri(
        std::env::var("FLATPAK_ID").ok().as_deref(),
        std::env::var("SNAP_INSTANCE_NAME").ok().as_deref(),
    );
    let path = launcher_object_path(&app_uri);
    match conn.emit_signal(
        None,
        &path,
        LAUNCHER_ENTRY_IFACE,
        "Update",
        Some(&update_parameters(&app_uri, count)),
    ) {
        Ok(()) => {
            tracing::debug!("[launcher-badge] published unread={count} for {app_uri}");
            LAST_PUBLISHED.with(|last| last.set(Some(count)));
        }
        Err(e) => tracing::warn!("[launcher-badge] LauncherEntry.Update failed: {e}"),
    }
}

fn connection() -> Option<gio::DBusConnection> {
    CONNECTION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| {
                    tracing::warn!(
                        "[launcher-badge] no session bus — the launcher badge is off: {e}"
                    );
                }),
            );
        }
        slot.as_ref().and_then(|r| r.as_ref().ok().cloned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_and_flatpak_key_on_the_app_id_desktop_entry() {
        assert_eq!(
            launcher_app_uri(None, None),
            "application://social.fauna.fauna.desktop"
        );
        // Inside the sandbox FLATPAK_ID is APP_ID; read, not assumed.
        assert_eq!(
            launcher_app_uri(Some("social.fauna.fauna"), None),
            "application://social.fauna.fauna.desktop"
        );
    }

    #[test]
    fn snap_keys_on_snapds_rewritten_entry() {
        assert_eq!(
            launcher_app_uri(None, Some("fauna")),
            "application://fauna_fauna.desktop"
        );
        // A parallel-installed instance carries its instance key.
        assert_eq!(
            launcher_app_uri(None, Some("fauna_beta")),
            "application://fauna_beta_fauna.desktop"
        );
    }

    #[test]
    fn empty_env_values_fall_through_to_the_native_id() {
        assert_eq!(
            launcher_app_uri(Some(""), Some("")),
            "application://social.fauna.fauna.desktop"
        );
    }

    #[test]
    fn object_path_is_the_snapd_admitted_shape_and_stable() {
        let uri = launcher_app_uri(None, None);
        let path = launcher_object_path(&uri);
        let digits = path
            .strip_prefix("/com/canonical/unity/launcherentry/")
            .expect("libunity's path prefix");
        assert!(!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(path, launcher_object_path(&uri), "same uri, same path");
        assert_ne!(path, launcher_object_path("application://other.desktop"));
    }

    #[test]
    fn update_parameters_carry_count_and_visibility() {
        let params = update_parameters("application://x.desktop", 3);
        assert_eq!(params.type_().as_str(), "(sa{sv})");
        assert_eq!(
            params.child_value(0).get::<String>().unwrap(),
            "application://x.desktop"
        );
        let props = params.child_value(1);
        let dict = glib::VariantDict::new(Some(&props));
        assert_eq!(
            dict.lookup_value("count", None).unwrap().get::<i64>(),
            Some(3)
        );
        assert_eq!(
            dict.lookup_value("count-visible", None)
                .unwrap()
                .get::<bool>(),
            Some(true)
        );

        let zero = glib::VariantDict::new(Some(
            &update_parameters("application://x.desktop", 0).child_value(1),
        ));
        assert_eq!(
            zero.lookup_value("count", None).unwrap().get::<i64>(),
            Some(0)
        );
        assert_eq!(
            zero.lookup_value("count-visible", None)
                .unwrap()
                .get::<bool>(),
            Some(false)
        );
    }
}
