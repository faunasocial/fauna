//! GTK glue for the two Fauna log surfaces — the **Settings → Logs** sub-page
//! (the client's own ring, `settings/logs.rs`) and the **admin Logs** view (the
//! nest's ring fetched over `fauna.admin.logs`, `views/admin.rs`).
//!
//! The *pure* presentation logic (the filter↔level map, the time / line /
//! subtitle form, the newest-first copy payload) lives in shared, wasm-safe
//! [`fauna_log::format`] so every app renders identically (priority #2/#3;
//! `observability.md` § 3 Surfaces). This module is the thin GTK layer the two
//! Linux surfaces share on top of it: the `log-entry` row widget, the value
//! marker, the list-render, and the severity-filter *labels* (which need i18n,
//! so they can't be pure). Both surfaces render the same `fauna_log::LogEntry`
//! list and differ only in *source* — the local process-global ring vs. a
//! fetched `Vec`.
//!
//! **Redaction rule** (`observability.md` § Persistence & privacy): this layer
//! only renders what `tracing` captured; call sites are forbidden from logging
//! message plaintext or secrets. The rule is upheld at the call sites, not here.

use std::cell::RefCell;

use adw::prelude::*;

use fauna_log::LogEntry;

use crate::i18n::strings::logs as S;

/// The severity filter DropDown options, in order. Index 0 = "All" (everything);
/// indices 1..=5 map to a `LogLevel` threshold (see
/// [`fauna_log::format::level_for_index`]). Shared so both Logs surfaces offer
/// the identical severity set. Stays Linux-side because it needs i18n (not
/// pure); the index→level mapping itself lives in `fauna_log::format`.
pub fn filter_labels() -> [&'static str; 6] {
    [
        S::FILTER_ALL,
        S::LEVEL_ERROR,
        S::LEVEL_WARN,
        S::LEVEL_INFO,
        S::LEVEL_DEBUG,
        S::LEVEL_TRACE,
    ]
}

/// The local UTC offset in **seconds** — the environmental input the shared,
/// pure `fauna_log::format` time fns take (they don't read the ambient
/// timezone). A Logs ring spans minutes, so a single "now" offset matches
/// every entry's wall-clock display except across a DST boundary (ignored —
/// same contract the shared module documents).
///
/// Delegates to [`fauna_core::caltime::local_utc_offset_seconds`], the tree's
/// **one** device-offset derivation — this door used to
/// source glib's local clock directly, independently of tui's chrono-based
/// twin and of `fauna_core::screen_time`'s own third derivation; glib and
/// chrono can disagree about a zone the OS reports mid-transition, so the two
/// were a genuine divergence surface, not just a duplicated expression.
pub fn local_offset_secs() -> i32 {
    fauna_core::caltime::local_utc_offset_seconds()
}

/// Build one `log-entry` row. The visible row shows the message (title) over a
/// `LEVEL · time · target` subtitle (`fauna_log::format::subtitle`); a 1px
/// `log-entry` marker carries the full one-line form
/// (`fauna_log::format::format_line`) for the agent. `use_markup(false)` so a
/// `<`/`&` in a message can't be mis-parsed as Pango markup. `tz_offset_secs`
/// is the caller's local offset (see [`local_offset_secs`]).
pub fn build_entry_row(entry: &LogEntry, tz_offset_secs: i32) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&entry.message)
        .subtitle(fauna_log::format::subtitle(entry, tz_offset_secs))
        .build();
    row.set_use_markup(false);
    row.add_prefix(&crate::settings::value_marker(
        "log-entry",
        &fauna_log::format::format_line(entry, tz_offset_secs),
    ));
    row
}

/// Rebuild `list_group`'s `log-entry` rows from `entries` **newest-first**,
/// tracking the live rows in `rows` (so the next call can remove them) and
/// toggling `placeholder` for the empty case. The shared list-render both Logs
/// surfaces call once they've selected their filtered entries.
pub fn populate(
    list_group: &adw::PreferencesGroup,
    placeholder: &adw::ActionRow,
    rows: &RefCell<Vec<adw::ActionRow>>,
    entries: &[LogEntry],
) {
    let off = local_offset_secs();
    let mut rows = rows.borrow_mut();
    for row in rows.drain(..) {
        list_group.remove(&row);
    }
    for entry in entries.iter().rev() {
        let row = build_entry_row(entry, off);
        list_group.add(&row);
        rows.push(row);
    }
    placeholder.set_visible(entries.is_empty());
}
