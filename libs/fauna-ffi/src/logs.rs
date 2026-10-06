//! UniFFI façade for the shared `fauna-log` ring (the Apple / Windows / Android
//! twin of the Linux `client::install_logging` + `fauna_log::{snapshot,clear}`
//! calls the Settings → Logs page makes natively).
//!
//! The owned `LogEntry` / `LogLevel` cross the boundary unchanged (they derive
//! `uniffi::Record` / `uniffi::Enum` behind `fauna-log`'s `uniffi` feature), so
//! the native apps render the ring with the same types Linux uses — no FFI
//! mirror. See `docs/goal/architecture/apps/observability.md` § Surfaces and
//! § Persistence & privacy.

use std::path::Path;

use fauna_log::format::LogRow;
use fauna_log::{LogEntry, LogLevel};

/// Install the native process-global `tracing` subscriber for a GUI app:
/// the in-memory ring (read by the Settings → Logs page below), a daily-rolling
/// file under `<data_dir>/logs/`, and stderr — all filtered by `RUST_LOG`
/// (default `info`). Call **once**, as the first thing the app does, before
/// anything emits a `tracing` event. Idempotent (a second call is a no-op).
///
/// `data_dir` is the client's own per-platform data root (the caller passes its
/// app-support / files dir, the analog of Linux's `~/.config/fauna`); the writer
/// guard is leaked internally so the file writer flushes for the whole process,
/// exactly like the Linux `install_logging`.
#[uniffi::export]
pub fn install_logging(data_dir: String) {
    if let Some(guard) = fauna_log::init(Path::new(&data_dir)) {
        // The guard must outlive the process so the non-blocking file writer
        // keeps flushing; we never reclaim it (same as Linux `install_logging`).
        std::mem::forget(guard);
    }
    // Seed one lifecycle line so the Settings → Logs page is never empty on
    // first open and each launch stamps the on-disk file. No secrets / paths
    // (redaction rule — observability.md § Persistence & privacy).
    tracing::info!(target: "fauna_ffi", "fauna client logging initialised");
}

/// Every retained log line, oldest first — the source for the Settings → Logs
/// page's "All" view (`fauna_log::snapshot`).
#[uniffi::export]
pub fn log_snapshot() -> Vec<LogEntry> {
    fauna_log::snapshot()
}

/// Log lines at or above `min_level` in severity (`Error` is most severe),
/// oldest first — the source for the page's level filter
/// (`fauna_log::snapshot_at_least`).
#[uniffi::export]
pub fn log_snapshot_at_least(min_level: LogLevel) -> Vec<LogEntry> {
    fauna_log::snapshot_at_least(min_level)
}

/// Drop all retained entries (the page's "Clear" affordance). Leaves the
/// on-disk rolling file untouched (`fauna_log::clear`).
#[uniffi::export]
pub fn log_clear() {
    fauna_log::clear();
}

/// Record one log line from the native app **shell** (Swift / Kotlin / C#)
/// into the shared ring — the UniFFI twin of the wasm `logMessage`. Every
/// message the shell would otherwise drop into the void must come through here:
///
/// * a message **displayed** to the user (an error banner, an alert, a toast),
/// * a native **console** print (`print`/`NSLog`, `Log.*`, `Debug.WriteLine`),
/// * a **meaningful swallowed error** (`try?`, an empty `catch`).
///
/// `level` picks the severity, `target` is the shell source shown in the Logs
/// view's target column (e.g. `"FaunaApp.Onboarding"`, `"P2PManager"`), and the
/// event flows through the same subscriber `install_logging` set up (ring + file
/// + stderr). Redaction rule (observability.md): pass error metadata / the
/// localized message string, never plaintext bodies or secrets.
#[uniffi::export]
pub fn log_message(level: LogLevel, target: String, message: String) {
    fauna_log::log_message(level, &target, &message);
}

// ---------------------------------------------------------------------------
// Shared Logs presentation (`fauna_log::format`) over the boundary
//
// The native apps' Logs surfaces render through these instead of each
// re-implementing the severity filter / line+row form (the per-app "twin of
// `logs_view.rs`" — observability.md § Surfaces). `tz_offset_secs` is the
// shell's current local UTC offset in seconds (it owns the platform clock); the
// same "caller passes the environmental input" contract as `relative_time`.
// ---------------------------------------------------------------------------

/// Map a severity-filter index to its threshold (`0`/"All"/out-of-range → none).
/// The canonical filter↔level map every app's filter dropdown uses.
#[uniffi::export]
pub fn log_level_for_index(index: u32) -> Option<LogLevel> {
    fauna_log::format::level_for_index(index)
}

/// `entries` narrowed to those at or above `min` in severity, order preserved
/// (`None` ⇒ all) — for a fetched source like the admin view's `Vec`, filtered
/// in memory rather than re-snapshotting the ring.
#[uniffi::export]
pub fn log_filter_entries(entries: Vec<LogEntry>, min: Option<LogLevel>) -> Vec<LogEntry> {
    fauna_log::format::filter_entries(&entries, min)
}

/// The one-line `LEVEL · HH:MM:SS · target · message` form for one entry.
#[uniffi::export]
pub fn log_format_line(entry: LogEntry, tz_offset_secs: i32) -> String {
    fauna_log::format::format_line(&entry, tz_offset_secs)
}

/// The entries joined **newest-first** into one block — the copy payload (input
/// oldest-first as the ring returns it).
#[uniffi::export]
pub fn log_rendered_text(entries: Vec<LogEntry>, tz_offset_secs: i32) -> String {
    fauna_log::format::rendered_text(&entries, tz_offset_secs)
}

/// The rendered `log-entry` rows **newest-first** — the shared row shape
/// (one-line form + message title + `LEVEL · time · target` subtitle).
#[uniffi::export]
pub fn log_rows(entries: Vec<LogEntry>, tz_offset_secs: i32) -> Vec<LogRow> {
    fauna_log::format::rows(&entries, tz_offset_secs)
}
