//! The Settings → Logs sub-page (split out of `settings/mod.rs`).

use fauna_i18n::strings::logs;
use fauna_ui_ids as ids;

use super::{Action, SettingsState};
use crate::element::{Element, Gesture, SelectTarget};

/// The severity-filter option labels, in index order — `["All", "Error",
/// "Warn", "Info", "Debug", "Trace"]` (`0` = All). Same set every Logs surface
/// offers (`fauna_log::format::FILTER_OPTION_COUNT`); mirrors linux
/// `logs_view::filter_labels`. `get_text`/`select` round-trip the *label* (the
/// DropDown contract — no token/id split, unlike the external-media picker).
pub(crate) fn log_filter_labels() -> [&'static str; 6] {
    [
        logs::FILTER_ALL,
        logs::LEVEL_ERROR,
        logs::LEVEL_WARN,
        logs::LEVEL_INFO,
        logs::LEVEL_DEBUG,
        logs::LEVEL_TRACE,
    ]
}

/// The filter index a display label selects (the reverse of
/// [`log_filter_labels`]); an unrecognized label → `0` (All), so a stray
/// `select` value never panics.
pub(crate) fn log_filter_index_for_label(label: &str) -> u32 {
    log_filter_labels()
        .iter()
        .position(|l| *l == label)
        .map(|i| i as u32)
        .unwrap_or(0)
}

/// The ring entries the active filter selects — `snapshot_at_least(level)` for a
/// threshold, `snapshot()` for "All" (oldest-first, as the ring returns them;
/// the render reverses for newest-first display).
pub(super) fn filtered_log_entries(filter: u32) -> Vec<fauna_log::LogEntry> {
    fauna_log::snapshot_filtered(filter)
}

/// The local UTC offset in seconds — the tui's own device-offset door, and the
/// twin of linux's `logs_view::local_offset_secs` (same shape in both apps:
/// the offset lives in the logs module and every other surface calls it).
///
/// Used for the wall-clock column here (`fauna_log::format` is pure and takes
/// the offset — the shell supplies the environmental input) and by the
/// conversations timestamp, admin logs and settings.
///
/// Delegates to [`fauna_core::caltime::local_utc_offset_seconds`], the tree's
/// **one** device-offset derivation — this door used to
/// hand-roll its own chrono read, independently of linux's glib-based twin
/// and of `fauna_core::screen_time`'s own third derivation.
pub(crate) fn local_offset_secs() -> i32 {
    fauna_core::caltime::local_utc_offset_seconds()
}

/// The Settings → Logs page (`observability.md` § 3 Surfaces): the `settings-logs`
/// landmark, the `log-level-filter` severity picker, `log-copy-button` /
/// `log-clear-button`, and the captured ring painted **newest-first** as flat
/// indexed `log-entry` rows (`LEVEL · time · target · message`, the shared
/// one-line form). An empty ring paints the placeholder chrome instead (no
/// `log-entry` id, so `count("log-entry") == 0`). The page's `error-message` is
/// registered globally by [`crate::ui::register_frame`], so it is not painted
/// here (the `tui-settings` precedent).
pub(super) fn logs_elements(state: &SettingsState) -> Vec<Element> {
    let filter = state.log_filter;
    let labels = log_filter_labels();
    let current_label = labels
        .get(filter as usize)
        .copied()
        .unwrap_or(logs::FILTER_ALL);
    let mut els = vec![
        // settings-logs — the page landmark (Rule 1: every page has one).
        Element::label(ids::SETTINGS_LOGS, logs::TITLE),
        // log-level-filter — get_text/select round-trip the display label; the
        // keyboard path cycles through `options` (the six labels).
        Element::select(
            ids::LOG_LEVEL_FILTER,
            current_label,
            SelectTarget::LogLevel,
            labels.iter().map(|l| l.to_string()).collect(),
        ),
        Element::gesture_button(
            ids::LOG_COPY_BUTTON,
            logs::COPY_BUTTON,
            true,
            Gesture::Settings(Action::CopyLogs),
        ),
        Element::gesture_button(
            ids::LOG_CLEAR_BUTTON,
            logs::CLEAR_BUTTON,
            true,
            Gesture::Settings(Action::ClearLogs),
        ),
    ];

    let entries = filtered_log_entries(filter);
    if entries.is_empty() {
        // Untagged placeholder chrome — the page still resolves (`settings-logs`
        // is present) but paints no `log-entry`.
        els.push(Element::chrome(logs::EMPTY));
    } else {
        let offset = local_offset_secs();
        // Newest-first: the ring returns oldest-first, so walk it in reverse —
        // the `fauna_log::format::rows` display order, one flat `log-entry` each.
        for entry in entries.iter().rev() {
            els.push(Element::label(
                ids::LOG_ENTRY,
                fauna_log::format::format_line(entry, offset),
            ));
        }
    }
    // `settings-nav-back` — Esc already returns to the Settings hub
    // (`Action::NavBack => state.sub = SubPage::Root`), but a live user report
    // found it was the ONLY way out on Folders, undiscoverable (user-approved
    // 2026-08-03; matches `account.rs`/`folders.rs`'s existing pattern).
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}
