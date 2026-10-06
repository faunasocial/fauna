use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted window geometry and UI state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub width: i32,
    pub height: i32,
    pub sidebar_item: String,
    #[serde(default = "default_calendar_mode")]
    pub calendar_view_mode: String,
}

pub fn default_calendar_mode() -> String {
    // Agenda is the cross-app default events view (web/windows/macos/ios/android
    // all default to agenda); linux matches them. See goal/ui/events.md.
    "agenda".into()
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1100,
            height: 700,
            sidebar_item: "conversations".into(),
            calendar_view_mode: default_calendar_mode(),
        }
    }
}

/// Resolve the view to open on launch from the persisted `sidebar_item`.
///
/// Never launch *into* a shell you "step into" (Settings / Admin) — those are
/// modes you explicitly enter and exit (mirroring the nav-back affordances), so
/// fall back to the primary view (Conversations). Any other (content) view is
/// restored as-is.
pub fn initial_sidebar_item(saved: &str) -> String {
    match saved {
        "settings" | "admin" => "conversations".to_string(),
        other => other.to_string(),
    }
}

/// Return the path to `~/.config/fauna/window-state.json`.
fn state_path() -> Option<PathBuf> {
    let mut path = dirs_config()?;
    path.push("fauna");
    path.push("window-state.json");
    Some(path)
}

/// Resolve the XDG config home directory (`$XDG_CONFIG_HOME` or `~/.config`).
///
/// The one resolver for every device-local file the client writes
/// (`window-state.json`, `theme.json`, `app-settings.json`) — the e2e driver
/// isolates `XDG_CONFIG_HOME` per launch, so anything routed through here is
/// automatically test-isolated.
pub(crate) fn dirs_config() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config"))
}

/// Load window state from disk. Returns `None` if the file does not exist or
/// cannot be parsed — callers should fall back to `WindowState::default()`.
pub fn load_window_state() -> Option<WindowState> {
    let path = state_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Save window state to disk. Silently ignores errors (non-fatal).
pub fn save_window_state(state: &WindowState) {
    let Some(path) = state_path() else { return };

    // Ensure the parent directory exists.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(json) = serde_json::to_string(state) {
        let _ = std::fs::write(&path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::initial_sidebar_item;

    #[test]
    fn shells_never_open_on_launch() {
        // The reported bug: relaunching into the Settings shell. Shells map to
        // the primary view instead.
        assert_eq!(initial_sidebar_item("settings"), "conversations");
        assert_eq!(initial_sidebar_item("admin"), "conversations");
    }

    #[test]
    fn content_views_are_restored() {
        for view in [
            "conversations",
            "feed",
            "contacts",
            "events",
            "media",
            "backups",
        ] {
            assert_eq!(initial_sidebar_item(view), view);
        }
    }
}
