//! The admin Logs sub-page (`admin-logs`) — the **nest's** `fauna-log` ring,
//! fetched over `fauna.admin.logs` (`observability.md` § 3 Surfaces).
//!
//! The distinction from `crate::settings::logs` is the *source*, not the widget:
//! that page renders this device's local ring, this one renders the nest's,
//! fetched once per entry into the page. Everything downstream is deliberately
//! identical — the same `log-level-filter` / `log-copy-button` / `log-entry` ids
//! and the same shared `fauna_log::format` row rendering — because
//! `observability.md:176-179` requires the admin surface to render "with the same
//! widget as the client Logs page", which is what lets the e2e's shared
//! `LogsActions` accessors drive both.
//!
//! Two element-set differences from the Settings page, both from ui.yaml
//! (`:984`): there is **no `log-clear-button`** (there is no admin RPC to wipe
//! the nest ring — `ui.yaml:995`), and the landmark is `admin-logs-heading`
//! rather than a shared `page-heading`.
//!
//! The shell (`super`) owns the client, op and fold; this file is paint only.

use fauna_i18n::strings::{admin as t, logs};
use fauna_ui_ids as ids;

use super::{Action, AdminState};
use crate::element::{Element, Gesture, SelectTarget};
use crate::pages::Page;
use crate::settings::logs::{local_offset_secs, log_filter_labels};

/// The nest entries the active filter selects, newest-first — the order the page
/// paints. The source is the **fetched** snapshot on [`AdminState`], never
/// `fauna_log::snapshot*` (that is this device's own ring, which is what the
/// Settings page shows); `filter_entries` is the shared narrowing both use.
pub(super) fn filtered_nest_entries(state: &AdminState) -> Vec<fauna_log::LogEntry> {
    let min = fauna_log::format::level_for_index(state.log_filter);
    let mut entries = fauna_log::format::filter_entries(&state.logs, min);
    // The wire delivers oldest-first (`AdminLogsReply` doc); the view is
    // newest-first, the same inversion `settings::logs` does at paint time.
    entries.reverse();
    entries
}

pub(super) fn logs_elements(state: &AdminState) -> Vec<Element> {
    let labels = log_filter_labels();
    let current_label = labels
        .get(state.log_filter as usize)
        .copied()
        .unwrap_or(logs::FILTER_ALL);

    let mut els = vec![
        Element::label(ids::ADMIN_LOGS_HEADING, t::logs_page::TITLE),
        Element::chrome(t::logs_page::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        // Round-trips the display label, the DropDown contract the shared
        // `LogsActions::set_level("Error")` drives.
        Element::select(
            ids::LOG_LEVEL_FILTER,
            current_label,
            SelectTarget::AdminLogLevel,
            labels.iter().map(|l| l.to_string()).collect(),
        )
        .labelled(logs::FILTER_LABEL),
        Element::gesture_button(
            ids::LOG_COPY_BUTTON,
            logs::COPY_BUTTON,
            true,
            Gesture::Admin(Action::CopyLogs),
        ),
    ];

    let entries = filtered_nest_entries(state);
    if entries.is_empty() {
        // Untagged placeholder chrome — the page still resolves
        // (`admin-logs-heading` is present) but paints no `log-entry`, so
        // `count("log-entry") == 0` rather than one blank row.
        els.push(Element::chrome(t::logs_page::EMPTY));
    } else {
        let offset = local_offset_secs();
        for entry in &entries {
            els.push(Element::label(
                ids::LOG_ENTRY,
                fauna_log::format::format_line(entry, offset),
            ));
        }
    }

    els
}

/// This page's read failure — read by `App::screen_error_text`, not painted
/// here (`crate::admin::page_error` carries the why).
pub(super) fn page_error(state: &AdminState) -> Option<String> {
    state
        .logs_error
        .as_deref()
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::AdminPage;
    use fauna_log::{LogEntry, LogLevel};

    fn entry(level: LogLevel, timestamp_ms: u64, message: &str) -> LogEntry {
        LogEntry {
            timestamp_ms,
            level,
            target: "fauna_nest::serve".to_string(),
            message: message.to_string(),
        }
    }

    /// Oldest-first on the wire, three levels — the shape `fauna.admin.logs`
    /// delivers.
    fn seeded() -> Vec<LogEntry> {
        vec![
            entry(LogLevel::Info, 1_000, "listening"),
            entry(LogLevel::Warn, 2_000, "slow reply"),
            entry(LogLevel::Error, 3_000, "handler failed"),
        ]
    }

    fn app_with(entries: Vec<LogEntry>) -> crate::app::App {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Logs;
        app.admin.logs = entries;
        app
    }

    fn tagged(els: &[Element]) -> Vec<&str> {
        els.iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect()
    }

    /// The page's chrome is exactly ui.yaml's `admin-logs` element set — note
    /// **no `log-clear-button`** (the nest ring has no wipe RPC) and no
    /// `page-heading` (the landmark is `admin-logs-heading`).
    #[test]
    fn paints_the_ui_yaml_element_set_and_one_row_per_entry() {
        let app = app_with(seeded());
        let els = logs_elements(&app.admin);
        assert_eq!(
            tagged(&els),
            vec![
                "admin-logs-heading",
                "admin-nav-back",
                "log-level-filter",
                "log-copy-button",
                "log-entry",
                "log-entry",
                "log-entry",
            ],
            "a clean page paints no error-message and no log-clear-button"
        );
    }

    /// Newest-first: the wire is oldest-first, the view inverts it. Asserted on
    /// the rendered text so a silent re-ordering can't pass.
    #[test]
    fn rows_render_newest_first() {
        let app = app_with(seeded());
        let els = logs_elements(&app.admin);
        let rows: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "log-entry")
            .map(|e| e.text.as_str())
            .collect();
        assert!(rows[0].contains("handler failed"), "got {rows:?}");
        assert!(rows[2].contains("listening"), "got {rows:?}");
    }

    /// The filter narrows the **fetched nest source**, not this device's ring —
    /// `Error` is a strict subset of `All`, the e2e's assertion.
    #[test]
    fn level_filter_narrows_the_fetched_source() {
        let mut app = app_with(seeded());
        let n_all = logs_elements(&app.admin)
            .iter()
            .filter(|e| e.id == "log-entry")
            .count();
        assert_eq!(n_all, 3);

        // Index 1 = "Error" (`log_filter_labels` order).
        app.admin.log_filter = 1;
        let rows: Vec<String> = logs_elements(&app.admin)
            .iter()
            .filter(|e| e.id == "log-entry")
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(rows.len(), 1, "only the Error row survives, got {rows:?}");
        assert!(rows[0].contains("handler failed"));

        // Back to All restores the full set (a pure client-side narrowing over
        // the held source — no refetch).
        app.admin.log_filter = 0;
        assert_eq!(
            logs_elements(&app.admin)
                .iter()
                .filter(|e| e.id == "log-entry")
                .count(),
            n_all
        );
    }

    /// An empty nest ring paints the placeholder chrome, never a tagged blank
    /// row — `count("log-entry") == 0` is what the e2e's poll waits out.
    #[test]
    fn empty_ring_paints_no_log_entry() {
        let app = app_with(Vec::new());
        let els = logs_elements(&app.admin);
        assert!(!els.iter().any(|e| e.id == "log-entry"));
        assert!(els.iter().any(|e| e.id == "admin-logs-heading"));
    }

    /// A failed `fauna.admin.logs` read bridges onto the page's `error-message`
    /// (rule 2), leaving whatever rows were last painted.
    #[test]
    fn read_failure_reaches_the_screens_error_line() {
        let mut app = app_with(Vec::new());
        app.session = Some(crate::app::tests::test_session());
        app.page = crate::pages::Page::Admin;
        app.admin.sub = AdminPage::Logs;
        app.admin.logs_error = Some("load nest logs: timeout".to_string());
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("load nest logs: timeout"),
            "the page error must reach the ONE funnel the paint, the registry and \
             `messages.error` all read"
        );
        assert!(
            !logs_elements(&app.admin)
                .iter()
                .any(|e| e.id == "error-message"),
            "and must NOT be a second, page-pushed copy of the id"
        );
    }

    /// The security pin for the admin-log ANSI plane (reviews/2026-07-29 § 4):
    /// a message carrying a real `\x1b`/`\x07` is driven through
    /// the **production path** — a genuine `tracing` emit into the real ring
    /// ingest, then the snapshot, then `format_line` — and no control
    /// character survives to the rendered `log-entry` element text. The ring
    /// entries are written by the real recorder, never hand-constructed, so
    /// disabling the ingest strip (`fauna-log`'s `sanitize_entry`) turns this
    /// red. The one leg not driven is the wire transit (`AdminLogEntry` serde
    /// mirror → `log_entry_from_wire`), which is byte-preserving on
    /// message/target — so ring entries ARE what `AdminState.logs` holds.
    #[test]
    fn a_ring_written_escape_never_reaches_the_rendered_element_text() {
        use tracing_subscriber::prelude::*;

        // Thread-scoped subscriber: parallel tests in this binary never share
        // it, and the global ring is filtered by marker below, so a concurrent
        // ring writer can't confuse the assertion.
        let subscriber = tracing_subscriber::registry().with(fauna_log::RingLayer);
        tracing::subscriber::with_default(subscriber, || {
            // The proven reachable shape (media_proxy_routes.rs:122): an
            // attacker-chosen URL as a structured field on the rejection warn.
            tracing::warn!(
                url = %"https://x/\x1b[31mRED\x1b]0;title\x07",
                "media proxy: URL rejected"
            );
        });

        let entries: Vec<LogEntry> = fauna_log::snapshot()
            .into_iter()
            .filter(|e| e.message.contains("media proxy: URL rejected"))
            .collect();
        assert!(!entries.is_empty(), "the emitted event reached the ring");

        let app = app_with(entries);
        let rows: Vec<String> = logs_elements(&app.admin)
            .iter()
            .filter(|e| e.id == "log-entry")
            .map(|e| e.text.clone())
            .collect();
        assert!(!rows.is_empty());
        for row in &rows {
            assert!(
                !row.chars().any(|c| c.is_control()),
                "control char survived to the rendered element text: {row:?}"
            );
        }
        // The strip removes the escape bytes, never the content around them.
        assert!(rows[0].contains("RED"), "got {:?}", rows[0]);
    }

    /// An out-of-range filter index reads back as "All" rather than panicking on
    /// the label lookup — the stray-`select` floor `settings::logs` also holds.
    #[test]
    fn out_of_range_filter_reads_back_as_all() {
        let mut app = app_with(seeded());
        app.admin.log_filter = 99;
        let els = logs_elements(&app.admin);
        let picker = els
            .iter()
            .find(|e| e.id == "log-level-filter")
            .expect("picker painted");
        assert_eq!(picker.text, logs::FILTER_ALL);
    }
}
