//! Device-local application settings — the linux twin of windows'
//! `AppSettingsStore` (`docs/goal/architecture/apps/windows.md` § App
//! Lifecycle, ratified 2026-07-16).
//!
//! Deliberately **not** nest state: every value here is a per-device UI choice
//! that means nothing on another machine (whether *this* desktop keeps Fauna
//! resident in *this* session's tray), so it never round-trips through
//! the synced account plane. It is still a genuine
//! user choice made in the client UI, per `principles.md` — not a knob anyone
//! hand-edits.
//!
//! **The absent-vs-explicitly-false distinction is the whole point.** A default
//! that is merely "ON" would silently override a user who turned the setting
//! off, which is why `close_to_tray` is written only by a real toggle and read
//! back through `#[serde(default)]`: a key missing from the JSON takes the
//! default, while a key present as `false` wins over it. That is the same
//! semantic windows gets from a C# record's `init` default, and it is what lets
//! the default flip to ON without clobbering an existing opt-out.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Close-to-tray defaults **ON**: the app hosts the in-process
/// `fauna-sync-engine`, so closing a *window* must not silently stop file sync
/// — a deliberate stop is the tray's Quit. (`apps/windows.md` § App
/// Lifecycle → *Window close*; linux additionally gates the hide itself on a
/// live tray host, `tray::should_hide_to_tray`.)
fn default_close_to_tray() -> bool {
    true
}

/// The on-disk shape of `~/.config/fauna/app-settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettingsData {
    #[serde(default = "default_close_to_tray")]
    pub close_to_tray: bool,
    /// The tri-state auto-start choice (`apps/linux.md` § Auto-start at
    /// sign-in). `None` = never chosen ⇒ register by default; `Some(false)` =
    /// an explicit opt-out ⇒ never re-register; `Some(true)` = an explicit
    /// opt-in. **Deliberately not a bool**: the presence of the `.desktop`
    /// file cannot distinguish *never chosen* from *deliberately off*, and
    /// re-registering over a deliberate opt-out at the next login would
    /// override a user's explicit choice.
    ///
    /// `#[serde(default)]` is belt-and-braces, not load-bearing: serde already
    /// maps a *missing* `Option<T>` to `None`, so a file written before this
    /// field existed (`{"close_to_tray": false}`) keeps loading either way —
    /// measured, not assumed. It stays because it states the intent locally
    /// and keeps the field safe if the type ever stops being an `Option`, at
    /// which point a missing key *would* be a hard deserialization error that
    /// degrades the whole file to the ON defaults and eats a real user's
    /// opt-out. `a_file_written_before_autostart_existed_keeps_its_close_to_tray_opt_out`
    /// pins the behaviour whichever way that goes.
    #[serde(default)]
    pub autostart_choice: Option<bool>,
}

impl Default for AppSettingsData {
    fn default() -> Self {
        Self {
            close_to_tray: default_close_to_tray(),
            autostart_choice: None,
        }
    }
}

/// Read/modify/write access to the device-local settings file.
///
/// Every operation is best-effort and infallible by design: a settings file we
/// cannot read or write must never block the app from starting, so failures
/// degrade to the defaults rather than propagate.
pub struct AppSettingsStore {
    path: PathBuf,
}

impl AppSettingsStore {
    /// The real store at `$XDG_CONFIG_HOME/fauna/app-settings.json` (or
    /// `~/.config/fauna/...`). `None` only when neither `XDG_CONFIG_HOME` nor
    /// `HOME` is set, in which case callers fall back to the defaults.
    pub fn new() -> Option<Self> {
        let mut path = crate::window_state::dirs_config()?;
        path.push("fauna");
        path.push("app-settings.json");
        Some(Self { path })
    }

    /// A store at an explicit path (tests).
    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    fn read(&self) -> AppSettingsData {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return AppSettingsData::default();
        };
        // A corrupt/truncated file reads as defaults rather than wedging the
        // app — the next write heals it.
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    fn write(&self, data: &AppSettingsData) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_vec_pretty(data) else {
            return;
        };
        let _ = std::fs::write(&self.path, json);
    }

    /// Whether closing the window should keep Fauna resident in the tray.
    /// Absent from the file ⇒ the ON default; present ⇒ the user's choice.
    pub fn close_to_tray(&self) -> bool {
        self.read().close_to_tray
    }

    /// Persist an explicit close-to-tray choice. Only a genuine user toggle
    /// calls this — never a programmatic/display refresh, which would convert
    /// "never chosen" into a fake explicit choice.
    pub fn set_close_to_tray(&self, value: bool) {
        let mut data = self.read();
        data.close_to_tray = value;
        self.write(&data);
    }

    /// The tri-state auto-start choice. `None` ⇒ the user never chose, so the
    /// post-auth hook registers by default (`crate::autostart::should_register`).
    pub fn autostart_choice(&self) -> Option<bool> {
        self.read().autostart_choice
    }

    /// Persist an explicit auto-start choice. As with close-to-tray, only a
    /// genuine user toggle calls this — a programmatic `set_active` during a
    /// page build must be signal-blocked, or it would record a fake explicit
    /// choice and defeat the default.
    pub fn set_autostart_choice(&self, value: bool) {
        let mut data = self.read();
        data.autostart_choice = Some(value);
        self.write(&data);
    }
}

/// Load the persisted close-to-tray preference into `tray::CLOSE_TO_TRAY`.
///
/// Call once at startup, before the main window exists. Without this the
/// setting was in-memory only and silently reset to OFF on every relaunch.
pub fn apply_saved_close_to_tray() {
    let value = AppSettingsStore::new()
        .map(|s| s.close_to_tray())
        .unwrap_or_else(default_close_to_tray);
    crate::tray::CLOSE_TO_TRAY.store(value, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &tempfile::TempDir) -> AppSettingsStore {
        AppSettingsStore::at(dir.path().join("fauna").join("app-settings.json"))
    }

    /// A fresh install has no file — close-to-tray is ON (shape A's default).
    #[test]
    fn close_to_tray_defaults_on_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store_in(&dir).close_to_tray());
    }

    /// The property the whole store exists for: an explicit OFF survives a
    /// restart. Before this store existed, close-to-tray lived in a bare
    /// `AtomicBool` and reset on every launch.
    #[test]
    fn close_to_tray_explicit_off_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        store_in(&dir).set_close_to_tray(false);
        // A *separate* store instance == a fresh process reading the file.
        assert!(!store_in(&dir).close_to_tray());
    }

    #[test]
    fn close_to_tray_explicit_on_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        store_in(&dir).set_close_to_tray(true);
        assert!(store_in(&dir).close_to_tray());
    }

    /// A file written before close-to-tray was persisted at all (or by a
    /// future version that drops the key) must take the new ON default, not
    /// `bool::default()` == false.
    #[test]
    fn close_to_tray_field_absent_from_json_reads_new_default_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fauna").join("app-settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        assert!(AppSettingsStore::at(path).close_to_tray());
    }

    /// An explicit `false` in the file beats the ON default — this is what
    /// stops the flip from overriding a user who already opted out.
    #[test]
    fn close_to_tray_explicit_false_in_json_beats_the_on_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fauna").join("app-settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"close_to_tray": false}"#).unwrap();
        assert!(!AppSettingsStore::at(path).close_to_tray());
    }

    #[test]
    fn close_to_tray_corrupt_file_defaults_on_and_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fauna").join("app-settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json at all {{{").unwrap();
        assert!(AppSettingsStore::at(path).close_to_tray());
    }

    /// Repeated writes round-trip through the file rather than accumulating a
    /// stale in-memory view — `set_*` is read-modify-write, which is also what
    /// keeps a sibling key intact once the file grows (autostart's tri-state
    /// choice lands here next).
    #[test]
    fn repeated_writes_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.set_close_to_tray(false);
        assert!(!store.close_to_tray());
        store.set_close_to_tray(true);
        assert!(store.close_to_tray());
    }

    // --- autostart_choice (tri-state; `apps/linux.md` § Auto-start at sign-in) ---

    /// A fresh install has never chosen — the hook registers by default.
    #[test]
    fn autostart_choice_is_unset_on_a_fresh_install() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(store_in(&dir).autostart_choice(), None);
    }

    /// The property the tri-state exists for: an explicit opt-out survives a
    /// restart and is therefore distinguishable from "never chosen", so the
    /// post-auth hook never re-registers over it.
    #[test]
    fn autostart_explicit_off_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        store_in(&dir).set_autostart_choice(false);
        assert_eq!(store_in(&dir).autostart_choice(), Some(false));
    }

    #[test]
    fn autostart_explicit_on_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        store_in(&dir).set_autostart_choice(true);
        assert_eq!(store_in(&dir).autostart_choice(), Some(true));
    }

    /// **The migration-safety property.** A file written by the binary that
    /// shipped before this field existed carries only `close_to_tray`. It must
    /// still load — with the user's close-to-tray opt-out intact and autostart
    /// merely *unset*. If the grown struct failed to deserialize, `read()`
    /// would degrade to `Default` and silently revert a real user's explicit
    /// OFF back to the ON default.
    #[test]
    fn a_file_written_before_autostart_existed_keeps_its_close_to_tray_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fauna").join("app-settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"close_to_tray": false}"#).unwrap();
        let store = AppSettingsStore::at(path);
        assert!(
            !store.close_to_tray(),
            "the opt-out must survive the field-add"
        );
        assert_eq!(store.autostart_choice(), None, "autostart is merely unset");
    }

    /// The two keys are independent: writing one must not clobber the other.
    /// `set_*` is read-modify-write, and this is what pins it as the struct
    /// grows.
    #[test]
    fn writing_one_key_preserves_its_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(&dir);
        store.set_close_to_tray(false);
        store.set_autostart_choice(false);
        assert!(!store.close_to_tray());
        assert_eq!(store.autostart_choice(), Some(false));

        // And in the other order, from a separate instance (a fresh process).
        store_in(&dir).set_close_to_tray(true);
        assert_eq!(
            store_in(&dir).autostart_choice(),
            Some(false),
            "an autostart opt-out must survive a close-to-tray write"
        );
    }
}
