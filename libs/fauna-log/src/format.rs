//! Shared presentation logic for the two Fauna **Logs** surfaces — the
//! *Settings → Logs* page (the client's own ring) and the *admin Logs* page
//! (the nest's ring fetched over `fauna.admin.logs`). Both render the same
//! [`LogEntry`] list with the same severity filter and the same one-line
//! `log-entry` form, so the bug-prone parts (the filter↔level map, the time
//! format, the line/subtitle form, the newest-first copy payload) live **here
//! once** and every app consumes them — Linux natively, the native apps
//! over UniFFI (`fauna-ffi`), the web over WASM (`fauna-wasm`). This replaces
//! the five per-app re-implementations that used to each be a "twin of
//! `logs_view.rs`" (priority #2/#3; `observability.md` § Surfaces).
//!
//! ## Time is pure: the caller supplies the local offset
//!
//! [`format_time`] takes the local UTC offset in seconds rather than reading the
//! ambient timezone, so this module is **pure, dependency-free, and compiles for
//! wasm** (no `chrono`/`time`). It's the same "the shell passes the environmental
//! input" contract `fauna_core::format::relative_time(now_ms, then_ms)` uses for
//! the clock. Each shell hands its current local offset
//! (`DateTimeOffset.Now.Offset`, `glib::DateTime`, `new Date().getTimezoneOffset()`,
//! …); a Logs ring spans minutes, so a single "now" offset matches every entry's
//! wall-clock display in all but the (ignored) across-a-DST-boundary case.
//!
//! ## Redaction (`observability.md` § Persistence & privacy)
//!
//! This layer renders whatever `tracing` captured; call sites are forbidden from
//! logging message plaintext or secrets. The rule is upheld at the call sites,
//! not here.

use crate::{LogEntry, LogLevel};

/// The number of severity-filter options, in order: index `0` = "All" (every
/// entry); `1..=5` map to a [`LogLevel`] threshold (see [`level_for_index`]).
/// Shared so every Logs surface offers the identical set
/// ("All" / Error / Warn / Info / Debug / Trace).
pub const FILTER_OPTION_COUNT: u32 = 6;

/// Map a severity-filter index to the threshold it selects. Index `0` ("All")
/// and any out-of-range index → `None` (no threshold → every entry).
pub fn level_for_index(index: u32) -> Option<LogLevel> {
    match index {
        1 => Some(LogLevel::Error),
        2 => Some(LogLevel::Warn),
        3 => Some(LogLevel::Info),
        4 => Some(LogLevel::Debug),
        5 => Some(LogLevel::Trace),
        _ => None,
    }
}

/// Entries at or above `min` in severity (`Error` most severe), order preserved
/// — the in-memory equivalent of [`crate::snapshot_at_least`], for sources that
/// are not the local ring (the admin view's fetched `Vec`). `None` ⇒ all.
pub fn filter_entries(entries: &[LogEntry], min: Option<LogLevel>) -> Vec<LogEntry> {
    match min {
        // `Error` is most severe (discriminant 0), so "at or above `min`" is
        // `level <= min` — mirrors `snapshot_at_least`.
        Some(min) => entries.iter().filter(|e| e.level <= min).cloned().collect(),
        None => entries.to_vec(),
    }
}

/// Local wall-clock `HH:MM:SS` for a Unix-millis timestamp, given the caller's
/// local UTC offset in seconds. Pure integer arithmetic — `rem_euclid` wraps a
/// negative offset across the day boundary correctly.
pub fn format_time(timestamp_ms: u64, tz_offset_secs: i32) -> String {
    let local_secs = (timestamp_ms / 1000) as i64 + tz_offset_secs as i64;
    // `rem_euclid` keeps the seconds-of-day in `0..86_400` for a negative offset
    // (which would otherwise wrap to a previous day).
    let secs_of_day = local_secs.rem_euclid(86_400);
    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// `LEVEL · HH:MM:SS · target · message` — the copy / indexed `log-entry`
/// one-line form.
pub fn format_line(entry: &LogEntry, tz_offset_secs: i32) -> String {
    format!(
        "{} · {} · {} · {}",
        entry.level.as_str(),
        format_time(entry.timestamp_ms, tz_offset_secs),
        entry.target,
        entry.message
    )
}

/// `LEVEL · HH:MM:SS · target` — the row's secondary line (the message is the
/// row title; this is the subtitle beneath it).
pub fn subtitle(entry: &LogEntry, tz_offset_secs: i32) -> String {
    format!(
        "{} · {} · {}",
        entry.level.as_str(),
        format_time(entry.timestamp_ms, tz_offset_secs),
        entry.target
    )
}

/// The entries joined **newest-first** into one block — the copy payload. Input
/// is oldest-first as the ring returns it.
pub fn rendered_text(entries: &[LogEntry], tz_offset_secs: i32) -> String {
    entries
        .iter()
        .rev()
        .map(|e| format_line(e, tz_offset_secs))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One rendered `log-entry` row. [`line`](LogRow::line) is the one-line copy /
/// marker form; [`message`](LogRow::message) is the row title;
/// [`subtitle`](LogRow::subtitle) (`LEVEL · time · target`) is the secondary
/// line. The shared row shape every Logs list binds (Linux's
/// title-over-subtitle row, the WinUI/SwiftUI/Compose/Svelte two-line row).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LogRow {
    /// The one-line `LEVEL · time · target · message` form (copy / e2e marker).
    pub line: String,
    /// The row title — the raw `message` (no formatting).
    pub message: String,
    /// The secondary line `LEVEL · time · target`.
    pub subtitle: String,
}

/// Build the [`LogRow`] list for display, **newest-first** (input is oldest-first
/// as the ring returns it) — the one place every Logs surface maps entries to
/// rows, so the row shape stays identical across clients.
pub fn rows(entries: &[LogEntry], tz_offset_secs: i32) -> Vec<LogRow> {
    entries
        .iter()
        .rev()
        .map(|e| LogRow {
            line: format_line(e, tz_offset_secs),
            message: e.message.clone(),
            subtitle: subtitle(e, tz_offset_secs),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: u64, level: LogLevel, target: &str, message: &str) -> LogEntry {
        LogEntry {
            timestamp_ms: ts,
            level,
            target: target.into(),
            message: message.into(),
        }
    }

    #[test]
    fn level_for_index_maps_severities() {
        assert_eq!(level_for_index(0), None); // "All"
        assert_eq!(level_for_index(1), Some(LogLevel::Error));
        assert_eq!(level_for_index(2), Some(LogLevel::Warn));
        assert_eq!(level_for_index(3), Some(LogLevel::Info));
        assert_eq!(level_for_index(4), Some(LogLevel::Debug));
        assert_eq!(level_for_index(5), Some(LogLevel::Trace));
        assert_eq!(level_for_index(99), None); // out of range → no threshold
    }

    #[test]
    fn filter_entries_none_keeps_all() {
        let entries = vec![
            entry(1, LogLevel::Error, "t", "e"),
            entry(2, LogLevel::Info, "t", "i"),
        ];
        assert_eq!(filter_entries(&entries, None).len(), 2);
    }

    #[test]
    fn filter_entries_keeps_at_or_above_threshold_preserving_order() {
        let entries = vec![
            entry(1, LogLevel::Error, "t", "e"),
            entry(2, LogLevel::Warn, "t", "w"),
            entry(3, LogLevel::Info, "t", "i"),
            entry(4, LogLevel::Debug, "t", "d"),
            entry(5, LogLevel::Trace, "t", "tr"),
        ];
        let info_and_up = filter_entries(&entries, Some(LogLevel::Info));
        assert_eq!(info_and_up.len(), 3); // Error + Warn + Info
        assert_eq!(info_and_up[0].message, "e"); // order preserved (oldest-first)
        assert_eq!(info_and_up[2].message, "i");
        assert!(info_and_up.iter().all(|e| e.level <= LogLevel::Info));
    }

    #[test]
    fn format_time_is_local_wall_clock_with_offset() {
        assert_eq!(format_time(0, 0), "00:00:00");
        assert_eq!(format_time(3_661_000, 0), "01:01:01"); // 1h 1m 1s past epoch
        assert_eq!(format_time(0, 3600), "01:00:00"); // +1h
        assert_eq!(format_time(0, 19_800), "05:30:00"); // +5:30 (India)
        assert_eq!(format_time(0, -3600), "23:00:00"); // -1h wraps to prev day
    }

    #[test]
    fn format_line_is_level_time_target_message() {
        let line = format_line(&entry(3_661_000, LogLevel::Warn, "fauna::sync", "hi"), 0);
        assert_eq!(line, "WARN · 01:01:01 · fauna::sync · hi");
    }

    #[test]
    fn subtitle_omits_the_message() {
        let s = subtitle(&entry(3_661_000, LogLevel::Error, "fauna::net", "boom"), 0);
        assert_eq!(s, "ERROR · 01:01:01 · fauna::net");
    }

    #[test]
    fn rendered_text_is_newest_first() {
        // Input oldest-first; copy payload newest-first.
        let entries = vec![
            entry(1, LogLevel::Info, "t", "older"),
            entry(2, LogLevel::Info, "t", "newer"),
        ];
        let text = rendered_text(&entries, 0);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("newer"), "got {:?}", lines[0]);
        assert!(lines[1].contains("older"), "got {:?}", lines[1]);
    }

    #[test]
    fn rows_are_newest_first_with_line_message_and_subtitle() {
        let entries = vec![
            entry(3_661_000, LogLevel::Info, "fauna::a", "older"),
            entry(3_662_000, LogLevel::Warn, "fauna::b", "newer"),
        ];
        let rows = rows(&entries, 0);
        assert_eq!(rows.len(), 2);
        // newest-first
        assert_eq!(rows[0].message, "newer");
        assert_eq!(rows[0].line, "WARN · 01:01:02 · fauna::b · newer");
        assert_eq!(rows[0].subtitle, "WARN · 01:01:02 · fauna::b");
        assert_eq!(rows[1].message, "older");
        assert_eq!(rows[1].line, "INFO · 01:01:01 · fauna::a · older");
        assert_eq!(rows[1].subtitle, "INFO · 01:01:01 · fauna::a");
    }
}
