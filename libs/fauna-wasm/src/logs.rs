//! Web log-ring exposure — the browser twin of the native `fauna-log`
//! subscriber install + snapshot reads (`fauna-ffi/src/logs.rs` / the Linux
//! `client::install_logging`).
//!
//! Web has no filesystem, so the install is **ring-only**: the shared
//! [`fauna_log::RingLayer`] (powers the Settings → Logs page) plus the browser
//! console — no rolling file (observability.md § Persistence & privacy — Web).
//! The owned `fauna_log::LogEntry` doesn't cross to JS directly (JS is untyped);
//! reads return a plain `{ timestamp_ms, level, target, message }` array, the
//! same shape the SPA's admin Logs view gets from `adminLogs` (rpc.rs), so one
//! TS component renders both.
//!
//! The whole module is `#[cfg(target_arch = "wasm32")]` (gated at the `mod` site
//! in `lib.rs`) — `web-sys` / the subscriber install only exist on wasm.

use serde::{Deserialize, Serialize};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use wasm_bindgen::prelude::*;

/// JS-facing log line. `timestamp_ms` is an `f64` (JS number, not BigInt);
/// `level` is the lowercase severity string the `log-level-filter` keys off.
/// `Deserialize` so the SPA can hand an entry array *back* to the shared
/// formatter exports below (`logFormatLine` / `logRows` / …).
#[derive(Serialize, Deserialize)]
pub(crate) struct JsLogEntry {
    pub timestamp_ms: f64,
    /// `"error" | "warn" | "info" | "debug" | "trace"`.
    pub level: String,
    pub target: String,
    pub message: String,
}

/// JS-facing rendered row — the shared [`fauna_log::format::LogRow`] over the
/// boundary (the SPA's `LogsView` binds `line` / `message` / `subtitle`).
#[derive(Serialize)]
pub(crate) struct JsLogRow {
    /// The one-line `LEVEL · time · target · message` copy / marker form.
    pub line: String,
    /// The row title — the raw message.
    pub message: String,
    /// The secondary line `LEVEL · time · target`.
    pub subtitle: String,
}

/// Lowercase severity string shared by the client ring read and the admin view.
pub(crate) fn level_str(level: fauna_log::LogLevel) -> &'static str {
    use fauna_log::LogLevel::*;
    match level {
        Error => "error",
        Warn => "warn",
        Info => "info",
        Debug => "debug",
        Trace => "trace",
    }
}

pub(crate) fn js_entries(entries: &[fauna_log::LogEntry]) -> Vec<JsLogEntry> {
    entries
        .iter()
        .map(|e| JsLogEntry {
            timestamp_ms: e.timestamp_ms as f64,
            level: level_str(e.level).to_string(),
            target: e.target.clone(),
            message: e.message.clone(),
        })
        .collect()
}

/// Parse a `log-level-filter` value into a severity threshold. `"all"` / unknown
/// ⇒ `Trace` (everything passes `snapshot_at_least`).
fn parse_level(s: &str) -> fauna_log::LogLevel {
    use fauna_log::LogLevel;
    match s {
        "error" => LogLevel::Error,
        "warn" => LogLevel::Warn,
        "info" => LogLevel::Info,
        "debug" => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

/// Parse a `logMessage` severity string into a level. Unlike [`parse_level`]
/// (a *filter threshold*, so unknown ⇒ everything), an emit with an unknown
/// level defaults to `Info`.
fn emit_level(s: &str) -> fauna_log::LogLevel {
    use fauna_log::LogLevel;
    match s {
        "error" => LogLevel::Error,
        "warn" => LogLevel::Warn,
        "debug" => LogLevel::Debug,
        "trace" => LogLevel::Trace,
        _ => LogLevel::Info,
    }
}

/// Install the web app's ring-only `tracing` subscriber: the shared
/// [`fauna_log::RingLayer`] (Settings → Logs source) plus a browser-console
/// layer, filtered to `info` (matching the native default — no `RUST_LOG` in a
/// browser). Call once at app start. Idempotent — a second call is a no-op.
#[wasm_bindgen(js_name = installLogging)]
pub fn install_logging() {
    use tracing_subscriber::prelude::*;
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(fauna_log::RingLayer)
        .with(ConsoleLayer)
        .try_init();
    install_panic_hook();
    // Seed one lifecycle line so the Settings → Logs page is never empty on
    // first open. No secrets (redaction rule — observability.md).
    tracing::info!(target: "fauna_web", "fauna web client logging initialised");
}

/// Name a Rust panic on web instead of letting it vanish into an unnamed trap.
///
/// **Why this is load-bearing, not cosmetic.** wasm has no unwinding, so a panic
/// inside a `future_to_promise` task aborts mid-poll: the task dies, and its JS
/// promise **never settles** — no resolve, no reject. An `await` on it hangs
/// forever, and any timeout *inside* that task (e.g. `dispatch_typed`'s 30 s
/// `TimeoutFuture` race) dies with it, so "the request is bounded" stops being
/// true exactly when it matters. Without a hook the only witness is a bare
/// `RuntimeError: unreachable` — no message, no panic site — which is how a
/// silent-hang class survives repeated diagnosis (testing.md § point 6:
/// failures must diagnose themselves).
///
/// The console mirror + chunk-naming + previous-hook chaining live in the
/// shared [`fauna_wasm_panic_hook`] crate (every other wasm chunk installs the
/// same hook at its own init — see that crate's doc comment). This chunk
/// layers on a second sink via `install_with`: `tracing::error!` so the panic
/// also lands in the shared log ring the user's own Settings → Logs page reads
/// (observability.md — a panic is the one event most worth having there).
/// Ships in production too: it adds no automation surface (nothing reachable
/// from JS), only a message on a path that was already fatal.
///
/// Idempotent in effect — [`install_logging`] is the single caller and is itself
/// called once at app start.
fn install_panic_hook() {
    fauna_wasm_panic_hook::install_with("fauna-wasm", |message| {
        tracing::error!(target: "fauna_web", "{message}");
    });
}

/// Every retained log line, oldest first (`fauna_log::snapshot`) — the "All"
/// view's source. Returns a JS array of `{ timestamp_ms, level, target, message }`.
#[wasm_bindgen(js_name = logSnapshot)]
pub fn log_snapshot() -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(&js_entries(&fauna_log::snapshot())).map_err(crate::rpc::err_to_js)
}

/// Log lines at or above `min_level` (`"error"|"warn"|"info"|"debug"|"trace"`;
/// `"all"`/unknown ⇒ everything), oldest first — the level filter's source
/// (`fauna_log::snapshot_at_least`).
#[wasm_bindgen(js_name = logSnapshotAtLeast)]
pub fn log_snapshot_at_least(min_level: &str) -> Result<JsValue, JsValue> {
    let entries = fauna_log::snapshot_at_least(parse_level(min_level));
    serde_wasm_bindgen::to_value(&js_entries(&entries)).map_err(crate::rpc::err_to_js)
}

/// Drop all retained entries (the page's "Clear" affordance — `fauna_log::clear`).
#[wasm_bindgen(js_name = logClear)]
pub fn log_clear() {
    fauna_log::clear();
}

/// Record one log line from the web app **shell** (TypeScript) into the
/// shared ring — the wasm twin of the native `log_message` (UniFFI). Every
/// message the SPA would otherwise drop must come through here:
///
/// * a message **displayed** to the user (the `MessageBanner` error/warn/info),
/// * a **`console.*`** print,
/// * a **meaningful swallowed error** (an empty `.catch` / `catch {}`).
///
/// `level` is `"error"|"warn"|"info"|"debug"|"trace"` (unknown ⇒ `info`),
/// `target` is the source shown in the Logs view's target column (e.g.
/// `"fauna_web::conversations"`). The event flows through the same ring +
/// console subscriber `installLogging` set up. Redaction rule
/// (observability.md): pass error metadata / the message string, never
/// plaintext bodies or secrets.
#[wasm_bindgen(js_name = logMessage)]
pub fn log_message(level: &str, target: &str, message: &str) {
    fauna_log::log_message(emit_level(level), target, message);
}

// ── Shared Logs presentation (`fauna_log::format`) over the boundary ─────────
//
// The SPA's `LogsView` renders through these instead of re-implementing the
// severity filter / line+row form in TypeScript (the web "twin of
// `logs_view.rs`" — observability.md § Surfaces). The SPA hands its own
// `{ timestamp_ms, level, target, message }` array straight back; `tzOffsetSecs`
// is the browser's current local offset (`-new Date().getTimezoneOffset() * 60`),
// so the shared formatter renders local `HH:MM:SS` with no timezone lib.

/// Reconstruct shared `fauna_log::LogEntry`s from the JS array shape. `level`
/// reuses [`emit_level`]'s exact lowercase mapping (unknown ⇒ `info`).
fn from_js_entries(entries: Vec<JsLogEntry>) -> Vec<fauna_log::LogEntry> {
    entries
        .into_iter()
        .map(|e| fauna_log::LogEntry {
            timestamp_ms: e.timestamp_ms as u64,
            level: emit_level(&e.level),
            target: e.target,
            message: e.message,
        })
        .collect()
}

/// `entries` narrowed to those at or above `min_level` in severity, order
/// preserved (`"all"`/unknown ⇒ everything) — the in-memory filter for the
/// admin Logs view's fetched list (`fauna_log::format::filter_entries`).
#[wasm_bindgen(js_name = logFilterEntries)]
pub fn log_filter_entries(entries: JsValue, min_level: &str) -> Result<JsValue, JsValue> {
    let parsed: Vec<JsLogEntry> =
        serde_wasm_bindgen::from_value(entries).map_err(crate::rpc::err_to_js)?;
    let native = from_js_entries(parsed);
    // `parse_level` maps "all"/unknown → Trace, the least-severe threshold, so
    // `Some(Trace)` keeps everything — matching the native index-0 "All".
    let filtered = fauna_log::format::filter_entries(&native, Some(parse_level(min_level)));
    serde_wasm_bindgen::to_value(&js_entries(&filtered)).map_err(crate::rpc::err_to_js)
}

/// The one-line `LEVEL · HH:MM:SS · target · message` form for one entry
/// (`fauna_log::format::format_line`).
#[wasm_bindgen(js_name = logFormatLine)]
pub fn log_format_line(entry: JsValue, tz_offset_secs: i32) -> Result<String, JsValue> {
    let e: JsLogEntry = serde_wasm_bindgen::from_value(entry).map_err(crate::rpc::err_to_js)?;
    let native = from_js_entries(vec![e]);
    Ok(fauna_log::format::format_line(&native[0], tz_offset_secs))
}

/// The entries joined **newest-first** into one block — the copy payload (input
/// oldest-first; `fauna_log::format::rendered_text`).
#[wasm_bindgen(js_name = logRenderedText)]
pub fn log_rendered_text(entries: JsValue, tz_offset_secs: i32) -> Result<String, JsValue> {
    let parsed: Vec<JsLogEntry> =
        serde_wasm_bindgen::from_value(entries).map_err(crate::rpc::err_to_js)?;
    Ok(fauna_log::format::rendered_text(
        &from_js_entries(parsed),
        tz_offset_secs,
    ))
}

/// The rendered `log-entry` rows **newest-first** — `{ line, message, subtitle }`
/// each (`fauna_log::format::rows`).
#[wasm_bindgen(js_name = logRows)]
pub fn log_rows(entries: JsValue, tz_offset_secs: i32) -> Result<JsValue, JsValue> {
    let parsed: Vec<JsLogEntry> =
        serde_wasm_bindgen::from_value(entries).map_err(crate::rpc::err_to_js)?;
    let rows: Vec<JsLogRow> = fauna_log::format::rows(&from_js_entries(parsed), tz_offset_secs)
        .into_iter()
        .map(|r| JsLogRow {
            line: r.line,
            message: r.message,
            subtitle: r.subtitle,
        })
        .collect();
    serde_wasm_bindgen::to_value(&rows).map_err(crate::rpc::err_to_js)
}

// ── Browser-console layer ───────────────────────────────────────────────────

/// A `tracing` layer that mirrors each event to the browser console, routed by
/// severity (`console.error` / `.warn` / `.info` / `.debug`). The ring is the
/// durable surface; this is the developer-console convenience the goal doc calls
/// for ("plus the browser console"). No timestamp — the console stamps its own.
struct ConsoleLayer;

impl<S> Layer<S> for ConsoleLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Same fold the ring uses, from its one owner — including the
        // `logMessage` shim's `log_target` lift. Console and ring must agree on
        // what a line says; a private copy here is a second place that rule can
        // drift (`fauna_log::record_event`).
        let recorded = fauna_log::record_event(event);
        let line = format!(
            "[{}] {}: {}",
            meta.level(),
            recorded.target,
            recorded.message
        );
        let js = JsValue::from_str(&line);
        match *meta.level() {
            tracing::Level::ERROR => web_sys::console::error_1(&js),
            tracing::Level::WARN => web_sys::console::warn_1(&js),
            tracing::Level::INFO => web_sys::console::info_1(&js),
            tracing::Level::DEBUG | tracing::Level::TRACE => web_sys::console::debug_1(&js),
        }
    }
}
