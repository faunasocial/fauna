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
        // One line each, ellipsized: a ring of up to `RING_CAPACITY` rows of
        // wrapping kilobyte messages cost the UI thread a height-for-width
        // measure of every wrapped label at each render, a stall long enough to
        // miss the page's own deadlines. The full line rides the `log-entry`
        // marker below and the Copy button.
        .title_lines(1)
        .subtitle_lines(1)
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

/// The `logs.empty` key — the Settings → Logs page's own empty line, the
/// reason its copy and clear are blocked on an empty ring.
pub const DEVICE_EMPTY_KEY: &str = "logs.empty";
/// The `admin.logs_page.empty` key — the admin Logs page's own empty line.
pub const ADMIN_EMPTY_KEY: &str = "admin.logs_page.empty";

/// Paint a Logs surface's ring actions (`log-copy-button`, and on the device
/// page `log-clear-button`) from the shared
/// [`fauna_log::format::ring_action_possibility`] over the `shown` entries the
/// active filter paints. The reason is the page's own empty line, already on
/// the page as the list placeholder, so no second reason line is painted — it
/// rides the buttons' tooltips (the tui twin: `settings::logs::logs_elements`).
pub fn paint_ring_actions(buttons: &[&gtk::Button], shown: usize, empty_key: &str) {
    let answer = fauna_log::format::ring_action_possibility(shown, empty_key);
    for button in buttons {
        crate::possible::paint(*button, None, answer.clone());
    }
}

/// The press half of a ring action: asks the same answer the buttons were
/// painted from and, when it is not possible, puts its reason on the page's
/// `error_line` and refuses (`true`).
pub fn ring_action_refused(shown: usize, empty_key: &str, error_line: &gtk::Label) -> bool {
    crate::possible::refused_at_press(
        &fauna_log::format::ring_action_possibility(shown, empty_key),
        error_line,
    )
}

/// `log-copy-button`'s press over the `entries` the filter shows: refused with
/// the empty line when there are none (never an empty string on the
/// clipboard), else the newest-first payload written through `write` and its
/// outcome recorded — "Copied to clipboard" + the `copied` attr, or
/// `common.copy_failed` on `error_line` (`clipboard::record_copy_outcome`).
/// `write` is [`crate::clipboard::try_copy_text`] in production.
pub fn copy_shown(
    btn: &gtk::Button,
    error_line: &gtk::Label,
    entries: &[LogEntry],
    empty_key: &str,
    write: impl FnOnce(&str) -> Result<(), String>,
) {
    if ring_action_refused(entries.len(), empty_key, error_line) {
        return;
    }
    // An earlier refusal's empty line no longer holds: the ring has entries.
    if crate::possible::reason_text(&fauna_log::format::ring_action_possibility(0, empty_key))
        .is_some_and(|empty| error_line.text().as_str() == empty)
    {
        error_line.set_text("");
        error_line.set_visible(false);
    }
    let text = fauna_log::format::rendered_text(entries, local_offset_secs());
    let outcome = write(&text);
    crate::clipboard::record_copy_outcome(btn, error_line, S::COPY_BUTTON, &text, outcome);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_log::LogLevel;

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            timestamp_ms: 1_000,
            level: LogLevel::Error,
            target: "fauna_test".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn an_empty_ring_blocks_copy_and_clear_with_the_empty_line() {
        crate::testid::run_on_gtk_thread(|| {
            let copy = gtk::Button::with_label(S::COPY_BUTTON);
            let clear = gtk::Button::with_label(S::CLEAR_BUTTON);
            paint_ring_actions(&[&copy, &clear], 0, DEVICE_EMPTY_KEY);
            for b in [&copy, &clear] {
                assert!(b.is_visible() && !b.is_sensitive());
                assert_eq!(b.tooltip_text().as_deref(), Some(S::EMPTY));
            }
            paint_ring_actions(&[&copy, &clear], 2, DEVICE_EMPTY_KEY);
            assert!(copy.is_sensitive() && clear.is_sensitive());

            // The admin page's own empty line.
            paint_ring_actions(&[&copy], 0, ADMIN_EMPTY_KEY);
            assert_eq!(
                copy.tooltip_text().as_deref(),
                Some(crate::i18n::strings::admin::logs_page::EMPTY)
            );

            // A press on an empty ring is refused with the reason, and writes nothing.
            let error = gtk::Label::new(None);
            error.set_visible(false);
            let mut wrote = false;
            copy_shown(&copy, &error, &[], DEVICE_EMPTY_KEY, |_| {
                wrote = true;
                Ok(())
            });
            assert!(!wrote, "nothing shown, nothing copied");
            assert!(error.is_visible());
            assert_eq!(error.text().as_str(), S::EMPTY);
            assert!(ring_action_refused(0, DEVICE_EMPTY_KEY, &error));
            assert!(!ring_action_refused(1, DEVICE_EMPTY_KEY, &error));
        });
    }

    #[test]
    fn a_copy_that_succeeds_is_confirmed_on_the_button() {
        crate::testid::run_on_gtk_thread(|| {
            let copy = gtk::Button::with_label(S::COPY_BUTTON);
            let error = gtk::Label::new(None);
            error.set_visible(false);
            let mut written = String::new();
            copy_shown(
                &copy,
                &error,
                &[entry("handler failed")],
                DEVICE_EMPTY_KEY,
                |t| {
                    written = t.to_string();
                    Ok(())
                },
            );
            assert!(written.contains("handler failed"));
            assert_eq!(
                crate::testid::test_attr_text(&copy, "copied").as_deref(),
                Some(written.as_str())
            );
            assert_eq!(
                copy.label().as_deref(),
                Some(crate::i18n::strings::settings::account_page::COPIED_CLIPBOARD)
            );
            assert!(!error.is_visible());
        });
    }

    #[test]
    fn a_copy_that_fails_says_so_on_the_error_line() {
        crate::testid::run_on_gtk_thread(|| {
            let copy = gtk::Button::with_label(S::COPY_BUTTON);
            let error = gtk::Label::new(None);
            error.set_visible(false);
            copy_shown(&copy, &error, &[entry("x")], DEVICE_EMPTY_KEY, |_| {
                Err("closed".to_string())
            });
            assert!(error.is_visible());
            assert_eq!(
                error.text().as_str(),
                crate::clipboard::copy_failed_text("closed")
            );
            assert_eq!(
                crate::testid::test_attr_text(&copy, "copied").as_deref(),
                Some("")
            );
            assert_eq!(copy.label().as_deref(), Some(S::COPY_BUTTON));

            // A later copy that succeeds clears its own failure.
            copy_shown(&copy, &error, &[entry("x")], DEVICE_EMPTY_KEY, |_| Ok(()));
            assert!(!error.is_visible());
        });
    }
}
