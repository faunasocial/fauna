//! Shared in-process log capture for Fauna clients and the nest.
//!
//! Everything in Fauna logs through [`tracing`]. This crate adds the two
//! pieces every surface needs on top of that:
//!
//! 1. [`RingLayer`] — a `tracing_subscriber::Layer` that copies recent events
//!    into a bounded in-memory ring buffer. [`snapshot`] hands that buffer to
//!    the **Settings → Logs** sub-page (and, on the nest, to the admin
//!    `fauna.admin.logs` RPC). Pure Rust, so it compiles for native *and* the
//!    web app.
//! 2. [`init`] (native only) — installs the process-global subscriber: the
//!    ring layer **plus** a daily-rolling file under the data dir (persistence)
//!    **plus** stderr, all filtered by `RUST_LOG` (default `info`).
//!
//! ## Redaction rule (authoring, not enforced here)
//!
//! NEVER log message plaintext, secret material, or other sensitive user
//! content — Fauna is encrypted-by-default and these entries land in an
//! on-disk file and a user-visible Settings page. Log levels, targets,
//! operation names, and *error metadata* only. See
//! `docs/goal/architecture/apps/observability.md`.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

// UniFFI scaffolding for the `LogEntry` / `LogLevel` derives below — only when
// the optional `uniffi` feature is on (the `fauna-ffi` build). Names the
// namespace `fauna_log`, so the generated bindings expose these owned types to
// Apple / Windows / Android unchanged. Mirrors `fauna_core` / `fauna_mail`.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_log");

/// Shared presentation logic (severity filter, time/line/row formatting) for the
/// Logs surfaces — the single home every app renders through, replacing the
/// per-app "twin of `logs_view.rs`" re-implementations (priority #2;
/// `observability.md` § Surfaces). Reach it as `fauna_log::format::*`.
pub mod format;

#[cfg(not(target_arch = "wasm32"))]
pub mod rolling;

/// Maximum number of entries retained in the in-memory ring. Older entries are
/// evicted FIFO. ~2000 lines covers the Settings Logs view while staying
/// bounded (a few hundred KB); the on-disk rolling file is the long tail.
pub const RING_CAPACITY: usize = 2000;

/// Maximum number of entries retained in the **remote** ring — the separate
/// buffer the sidecar log plane's entries land in (`observability.md` § The
/// sidecar log plane → *The remote ring*). Deliberately smaller than
/// [`RING_CAPACITY`], and deliberately a *second* ring: a chatty or hostile
/// co-resident service can then never evict the nest's own history, no matter
/// how much it emits.
pub const REMOTE_RING_CAPACITY: usize = 1000;

/// Severity of a captured line. Mirrors [`tracing::Level`] but is a plain owned
/// enum so it crosses the UniFFI / WASM boundary to every app unchanged.
///
/// Discriminants run most-severe → least-severe (`Error = 0`), which
/// [`snapshot_at_least`] relies on for threshold filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl LogLevel {
    fn from_tracing(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::ERROR => LogLevel::Error,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::TRACE => LogLevel::Trace,
        }
    }

    /// Uppercase label for display (`"ERROR"`, `"WARN"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        }
    }
}

/// One captured log line. Snapshots are returned oldest-first (display order).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LogEntry {
    /// Milliseconds since the Unix epoch when the event was recorded.
    pub timestamp_ms: u64,
    pub level: LogLevel,
    /// The `tracing` target (module path), e.g. `fauna_linux::sync`.
    pub target: String,
    /// The event's `message` followed by any structured fields.
    pub message: String,
}

// A `static Mutex<VecDeque>` (no lazy init) — `Mutex::new`/`VecDeque::new` are
// const, so the ring is ready before `main` with no `OnceLock` ceremony.
static RING: Mutex<VecDeque<LogEntry>> = Mutex::new(VecDeque::new());

/// The sidecar log plane's ring — see [`push_remote_entry`]. Separate from
/// [`RING`] on purpose (the eviction-isolation property).
static REMOTE_RING: Mutex<VecDeque<LogEntry>> = Mutex::new(VecDeque::new());

/// The project's one control-character strip, re-exported so the log plane's
/// callers (ring ingest below, and the nest's `log_plane::admit`, which calls it
/// ahead of its byte cap so the cap counts real content) keep one import path.
///
/// **It moved to `fauna_core::control_chars`** when a second boundary needed it:
/// the tui render funnel strips remote-authored content the same way
/// (`tui.md` § Rendering), and a *content* boundary must not depend on the log
/// crate — while a second copy of the filter would break the property that makes
/// either fix trustworthy, namely that there is exactly one strip and every
/// boundary calls it. Rationale and the `\n`-is-structure caveat live with the
/// function; the log-plane reasoning is `observability.md` § Ring-ingest
/// sanitization.
pub use fauna_core::control_chars::strip_control_chars;

/// Apply [`strip_control_chars`] to the fields a ring consumer renders. Runs at
/// both rings' storage boundary, so the invariant — no retained entry carries a
/// control character — holds for every current and future ingest path
/// structurally, rather than per-caller.
fn sanitize_entry(entry: &mut LogEntry) {
    if let Cow::Owned(clean) = strip_control_chars(&entry.message) {
        entry.message = clean;
    }
    if let Cow::Owned(clean) = strip_control_chars(&entry.target) {
        entry.target = clean;
    }
}

fn push(mut entry: LogEntry) {
    sanitize_entry(&mut entry);
    if let Ok(mut ring) = RING.lock() {
        while ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
    }
}

/// Record one entry reported by a **co-resident service** (mail bridge, relay,
/// algorithm, …) into the remote ring — the nest side of the sidecar log plane
/// (`observability.md` § The sidecar log plane).
///
/// Pushed as *data*, never re-emitted as a synthetic `tracing` event: a remote
/// entry already carries its own timestamp, level and `<source>:<event>`
/// target, and the source's own stderr is already in the container log stream —
/// re-emitting would double every line in `docker logs`.
///
/// The caller is responsible for admission (rate limiting, byte caps, deriving
/// `target` from the *authenticated* identity rather than the payload) — on
/// the nest that is `log_plane::admit`. The control-character strip alone is
/// also enforced here at the storage boundary ([`sanitize_entry`]), so no
/// future caller can re-open the terminal-injection surface admission closes.
pub fn push_remote_entry(mut entry: LogEntry) {
    sanitize_entry(&mut entry);
    if let Ok(mut ring) = REMOTE_RING.lock() {
        while ring.len() >= REMOTE_RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
    }
}

/// Every retained entry, oldest first.
pub fn snapshot() -> Vec<LogEntry> {
    RING.lock()
        .map(|r| r.iter().cloned().collect())
        .unwrap_or_default()
}

/// Entries at or above `min_level` in severity (`Error` is most severe),
/// oldest first. Powers the Settings Logs level filter.
pub fn snapshot_at_least(min_level: LogLevel) -> Vec<LogEntry> {
    RING.lock()
        .map(|r| r.iter().filter(|e| e.level <= min_level).cloned().collect())
        .unwrap_or_default()
}

/// The Settings Logs level-filter picker's entry set: `filter_index` is the
/// picker's selected index ([`format::level_for_index`]); an index outside
/// the level table means "no filter" — every retained entry. Both apps' logs
/// pages hand-copied this dispatch.
pub fn snapshot_filtered(filter_index: u32) -> Vec<LogEntry> {
    match format::level_for_index(filter_index) {
        Some(level) => snapshot_at_least(level),
        None => snapshot(),
    }
}

/// Every retained **remote** entry (sidecar log plane), oldest first.
pub fn snapshot_remote() -> Vec<LogEntry> {
    REMOTE_RING
        .lock()
        .map(|r| r.iter().cloned().collect())
        .unwrap_or_default()
}

/// Both rings merged into one `timestamp_ms`-ordered sequence, oldest first —
/// what the nest's `fauna.admin.logs` serves so plane entries appear inline
/// with the nest's own (`observability.md` § The sidecar log plane). On a
/// client the remote ring is always empty, so this equals [`snapshot`].
///
/// The merge is stable: entries sharing a timestamp keep nest-before-remote
/// order, so a flood can never interleave itself *into* the middle of a
/// same-millisecond run of nest entries.
pub fn snapshot_merged() -> Vec<LogEntry> {
    let mut merged = snapshot();
    let remote = snapshot_remote();
    if remote.is_empty() {
        return merged;
    }
    merged.extend(remote);
    // Both inputs are already sorted, and `sort_by_key` is stable.
    merged.sort_by_key(|e| e.timestamp_ms);
    merged
}

/// Drop all retained entries — both rings (the Settings Logs "Clear"
/// affordance drops everything the surface shows, and the nest surface shows
/// the merged view). Does not touch the on-disk file.
pub fn clear() {
    if let Ok(mut ring) = RING.lock() {
        ring.clear();
    }
    if let Ok(mut ring) = REMOTE_RING.lock() {
        ring.clear();
    }
}

/// The structured field a caller may set to override the entry's `target`
/// column. The FFI / WASM [`log_message`] shim uses it to carry a shell-supplied
/// runtime target (e.g. `"fauna_web::sync"`, `"FaunaApp.Onboarding"`) that
/// `tracing`'s compile-time static metadata `target` cannot — the event's
/// metadata target stays the fixed callsite (`fauna_client`), but the ring (and
/// the Logs UI) shows the real source. Lifted out by [`record_event`] — the one
/// fold both the ring and the wasm console layer render through; never appended
/// to the message text.
pub const TARGET_OVERRIDE_FIELD: &str = "log_target";

/// One `tracing` event folded into the `(target, message)` pair every log
/// surface renders. Produced by [`record_event`], which owns the fold.
pub struct RecordedEvent {
    /// The event's effective target: the caller-supplied
    /// [`TARGET_OVERRIDE_FIELD`] when the event carries one, else the
    /// callsite's static metadata target.
    pub target: String,
    /// The `message` field first, then every other field as ` name=value`.
    /// Never carries the override field itself.
    pub message: String,
}

/// Fold one `tracing` event into the target + message a log surface shows.
///
/// **This is the single owner of that fold.** Both surfaces that render an
/// event go through it: the ring ([`RingLayer`], which feeds Settings → Logs
/// and the admin `fauna.admin.logs` RPC) and the web app's browser-console
/// layer (`fauna-wasm`'s `ConsoleLayer`). A second spelling is a second place
/// the [`TARGET_OVERRIDE_FIELD`] rule can be lost — and losing it is visible
/// twice over: the override leaks into the message text as
/// `log_target=fauna_web::sync`, *and* every shell-bridged line is mislabelled
/// with the fixed callsite target `fauna_client` instead of its real source.
pub fn record_event(event: &tracing::Event<'_>) -> RecordedEvent {
    let mut visitor = MessageVisitor::default();
    event.record(&mut visitor);
    RecordedEvent {
        target: visitor
            .target_override
            .unwrap_or_else(|| event.metadata().target().to_string()),
        message: visitor.message,
    }
}

/// Concatenates an event's `message` field and any other fields into one line,
/// pulling out the [`TARGET_OVERRIDE_FIELD`] (if any) as a separate target.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    target_override: Option<String>,
}

impl MessageVisitor {
    fn append_field(&mut self, name: &str, rendered: &str) {
        if !self.message.is_empty() {
            self.message.push(' ');
        }
        let _ = write!(self.message, "{name}={rendered}");
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else if field.name() == TARGET_OVERRIDE_FIELD {
            self.target_override = Some(value.to_string());
        } else {
            self.append_field(field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // The `message` field is recorded as `format_args!(..)`, whose Debug is
        // its Display — the plain formatted string, no surrounding quotes.
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else if field.name() == TARGET_OVERRIDE_FIELD {
            self.target_override = Some(format!("{value:?}"));
        } else {
            let rendered = format!("{value:?}");
            self.append_field(field.name(), &rendered);
        }
    }
}

/// Emit one log line on behalf of a caller that can't use the `tracing` macros
/// directly — the non-Rust client shells (web TypeScript over WASM, Swift /
/// Kotlin / C# over UniFFI), whose displayed messages, console prints, and
/// meaningful swallowed errors must land in the **same** ring as Rust code.
///
/// Both `level` and `target` are runtime values, which the `tracing` macros
/// (static metadata) can't take, so this dispatches on `level` and threads the
/// caller's `target` through the [`TARGET_OVERRIDE_FIELD`] field. The event
/// flows through the normal subscriber — `RingLayer`, the file/console layer,
/// the `EnvFilter` — so shell messages persist and filter exactly like Rust
/// ones. The fixed metadata target is `fauna_client` (the EnvFilter knob for
/// shell-bridged lines).
///
/// Redaction rule still applies (observability.md): callers pass error
/// *metadata* / localized message strings, never plaintext bodies or secrets.
pub fn log_message(level: LogLevel, target: &str, message: &str) {
    // The override key is the plain identifier `log_target` — a dotted field
    // name (`log.target`) is ambiguous to the `tracing` macro parser.
    match level {
        LogLevel::Error => {
            tracing::error!(target: "fauna_client", log_target = target, "{message}")
        }
        LogLevel::Warn => {
            tracing::warn!(target: "fauna_client", log_target = target, "{message}")
        }
        LogLevel::Info => {
            tracing::info!(target: "fauna_client", log_target = target, "{message}")
        }
        LogLevel::Debug => {
            tracing::debug!(target: "fauna_client", log_target = target, "{message}")
        }
        LogLevel::Trace => {
            tracing::trace!(target: "fauna_client", log_target = target, "{message}")
        }
    }
}

/// A `tracing` layer that copies each event into the in-memory ring buffer.
///
/// Add it to a subscriber alongside whatever else you want (a file/stderr
/// `fmt` layer, an `EnvFilter`). On native, [`init`] does this for you.
pub struct RingLayer;

impl<S> Layer<S> for RingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        // The target/message fold — including the `log_message` shim's
        // override-field lift — belongs to `record_event`, not to this layer.
        let recorded = record_event(event);
        push(LogEntry {
            timestamp_ms: fauna_core::data::Timestamp::now_millis(),
            level: LogLevel::from_tracing(event.metadata().level()),
            target: recorded.target,
            message: recorded.message,
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::RingLayer;
    use super::rolling::CappedRollingFile;
    use std::io::IsTerminal as _;
    use std::path::Path;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::Layer;
    use tracing_subscriber::prelude::*;

    /// Keeps the non-blocking file writer thread alive. Hold it for the whole
    /// process (e.g. `std::mem::forget` it, or stash it in long-lived state);
    /// dropping it flushes and stops the writer. `None` inside when file
    /// logging was degraded away (unwritable log dir) — ring + stderr still run.
    pub struct LogGuard(#[allow(dead_code)] Option<tracing_appender::non_blocking::WorkerGuard>);

    /// Install the process-global tracing subscriber for a native app/nest:
    ///
    /// * the in-memory [`RingLayer`] (powers Settings → Logs / the admin RPC),
    /// * a daily-named, size-capped file `<data_dir>/logs/fauna.log.<date>`
    ///   whose directory stays under [`crate::rolling::MAX_TOTAL_BYTES`]
    ///   (persistence — [`crate::rolling`]),
    /// * stderr (developer console; captured by journald/systemd in the field),
    ///   coloured only when stderr is a terminal,
    ///
    /// all filtered by `RUST_LOG` (default `info` when unset, matching the
    /// server fleet), plus a panic hook that records a panic as an `error` event
    /// (so it reaches the file even when nothing keeps the process's stderr)
    /// before handing it to the previous hook. Returns a [`LogGuard`] the caller
    /// MUST keep alive so the file writer keeps running.
    ///
    /// Idempotent: a second call (or a process that already set a global
    /// subscriber) is a no-op returning `None`.
    pub fn init(data_dir: &Path) -> Option<LogGuard> {
        init_with_stderr(data_dir, true)
    }

    /// Like [`init`], but the caller chooses whether the **stderr** layer is
    /// installed. A full-screen terminal client (`fauna-tui`) owns the alternate
    /// screen: log lines painted to stderr over it corrupt the display, so the
    /// tui passes `stderr = false` when its stderr is the terminal (and `true`
    /// under the e2e driver / any `2>file` launch, where stderr is a plain file
    /// and the one debugging surface a full-screen client has). The in-memory
    /// [`RingLayer`] and the on-disk rolling file are installed **regardless** —
    /// the ring is what powers Settings → Logs, so it must always be present.
    /// Every non-tui caller keeps the stderr layer via [`init`].
    pub fn init_with_stderr(data_dir: &Path, stderr: bool) -> Option<LogGuard> {
        let (file_layer, guard) = match file_layer(&data_dir.join("logs")) {
            Some((layer, guard)) => (Some(layer), guard),
            None => (None, LogGuard(None)),
        };

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        // `Option<Layer>` is itself a `Layer` (a `None` is a no-op), so the
        // stderr layer is included or elided by one flag without a type change
        // — and the file layer degrades away the same way (`try_file_appender`).
        // Escape codes belong on a terminal only: a launchd/systemd/redirected
        // stderr is a file, where they are noise in every line.
        let stderr_layer = stderr.then(|| {
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr)
        });

        let registered = tracing_subscriber::registry()
            .with(filter)
            .with(RingLayer)
            .with(file_layer)
            .with(stderr_layer)
            .try_init();

        registered.ok().map(|()| {
            install_panic_hook();
            guard
        })
    }

    /// The size-capped file sink alone, as a layer [`init`] composes into its
    /// subscriber. Writes `fauna.log.<date>` into `log_dir` itself
    /// (not a `logs/` child — [`init`] passes `<data_dir>/logs`), bounded as
    /// [`crate::rolling`] says. `None` when the directory cannot be written
    /// (degraded, never a panic — see `try_file_appender`). The [`LogGuard`]
    /// must outlive every event the layer should persist.
    fn file_layer<S>(log_dir: &Path) -> Option<(Box<dyn Layer<S> + Send + Sync>, LogGuard)>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        let appender = try_file_appender(log_dir)?;
        let (file_writer, file_guard) = tracing_appender::non_blocking(appender);
        let layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file_writer);
        Some((Box::new(layer), LogGuard(Some(file_guard))))
    }

    /// Record every panic through `tracing` (→ ring + file) before the previous
    /// hook prints it to stderr as usual. Installed once, by the call that set
    /// the global subscriber.
    fn install_panic_hook() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            tracing::error!(
                target: "panic",
                thread = thread.name().unwrap_or("<unnamed>"),
                "{info}"
            );
            previous(info);
        }));
    }

    /// Open the size-capped log file FALLIBLY. A data root the process cannot
    /// write — an unmounted disk, or a macOS TCC deny on the app-group
    /// container — must degrade file logging away (the memory ring + stderr
    /// still install), never panic: a panicking log-init crash-loops every
    /// KeepAlive'd launchd agent (measured on macOS 2026-08-23 with the
    /// previous `tracing_appender::rolling::daily` sink;
    /// `sync-agent.md` § Implementation status today → A4 item 2).
    fn try_file_appender(log_dir: &Path) -> Option<CappedRollingFile> {
        match CappedRollingFile::open(log_dir) {
            Ok(appender) => Some(appender),
            Err(e) => {
                // tracing is not up yet — stderr is the only reporting surface
                // (a terminal, journald, or nowhere for a launchd agent, whose
                // plist keeps no stderr file; the memory ring still records
                // everything after this line).
                eprintln!(
                    "fauna-log: file logging disabled ({e}); \
                     continuing with memory ring + stderr only (log dir {log_dir:?})"
                );
                None
            }
        }
    }

    #[cfg(all(test, unix))]
    mod degrade_tests {
        use super::try_file_appender;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn unwritable_log_dir_degrades_instead_of_panicking() {
            // A read-only parent makes both create_dir_all and the appender's
            // own file creation fail — the TCC-deny shape, minus macOS.
            let parent =
                std::env::temp_dir().join(format!("fauna-log-degrade-test-{}", std::process::id()));
            std::fs::create_dir_all(&parent).unwrap();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

            let result = try_file_appender(&parent.join("logs"));

            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = std::fs::remove_dir_all(&parent);
            assert!(
                result.is_none(),
                "an unwritable log dir must degrade to None, not panic"
            );
        }

        /// `file_layer` composed into a subscriber writes the
        /// bounded `fauna.log.<date>` straight into the directory it is given.
        #[test]
        fn file_layer_writes_into_the_given_dir() {
            use tracing_subscriber::prelude::*;
            let dir = std::env::temp_dir()
                .join(format!("fauna-log-file-layer-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let (layer, guard) = super::file_layer(&dir).expect("writable dir");
            tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
                tracing::info!("file-layer-marker");
            });
            drop(guard); // flushes the non-blocking writer

            let names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            let text: String = names
                .iter()
                .map(|n| std::fs::read_to_string(dir.join(n)).unwrap())
                .collect();
            let _ = std::fs::remove_dir_all(&dir);
            assert!(
                names.iter().all(|n| n.starts_with("fauna.log.")),
                "{names:?}"
            );
            assert!(text.contains("file-layer-marker"), "{text:?}");
        }

        #[test]
        fn writable_log_dir_builds_the_appender() {
            let dir = std::env::temp_dir()
                .join(format!("fauna-log-appender-test-{}", std::process::id()));
            let result = try_file_appender(&dir.join("logs"));
            let _ = std::fs::remove_dir_all(&dir);
            assert!(result.is_some());
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{LogGuard, init, init_with_stderr};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing_subscriber::prelude::*;

    // The ring is a process-global; serialize the tests that mutate it so
    // parallel execution doesn't interleave their events.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    // exposure check: every
    // `tracing::{error,warn,info,debug}!` and `log_message(...)` call in this
    // module's tests runs inside `with_ring_subscriber` or this module's own
    // inline `with_default` (`record_event_owns_the_target_override_lift`) —
    // there is no call to these callsites anywhere in this binary with no
    // subscriber installed, so the first-ever hit always sees a real
    // (unfiltered) layer and the process-global interest cache latches
    // `always()`, not `never()`. Demonstrated unexposed; the thread-local
    // `with_default` here is not a defect.
    fn with_ring_subscriber(f: impl FnOnce()) {
        let subscriber = tracing_subscriber::registry().with(RingLayer);
        tracing::subscriber::with_default(subscriber, f);
    }

    #[test]
    fn captures_message_level_and_target() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        with_ring_subscriber(|| {
            tracing::error!("boom {}", 42);
            tracing::info!(target: "my::target", "hello world");
        });

        let snap = snapshot();
        assert_eq!(snap.len(), 2, "both events captured");
        assert_eq!(snap[0].level, LogLevel::Error);
        assert!(
            snap[0].message.contains("boom 42"),
            "got {:?}",
            snap[0].message
        );
        assert_eq!(snap[1].level, LogLevel::Info);
        assert_eq!(snap[1].target, "my::target");
        assert!(snap[1].message.contains("hello world"));
        assert!(snap[1].timestamp_ms > 0);
    }

    #[test]
    fn captures_structured_fields_after_message() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        with_ring_subscriber(|| {
            tracing::warn!(peer = "alice", count = 3, "rename failed");
        });
        let snap = snapshot();
        assert_eq!(snap.len(), 1);
        let m = &snap[0].message;
        assert!(m.contains("rename failed"), "got {m:?}");
        assert!(m.contains("peer=") && m.contains("alice"), "got {m:?}");
        assert!(m.contains("count=3"), "got {m:?}");
    }

    /// The `(target, message)` fold has one owner, [`record_event`], and this
    /// pins it *there* rather than through the ring — the ring is only one of
    /// its two callers. The other is the web app's browser-console layer
    /// (`fauna-wasm`'s `ConsoleLayer`), which never touches the ring, so no
    /// ring assertion can cover it; before the two were converged that layer
    /// carried its own copy of this rule with no test at all.
    ///
    /// Both distinctions a copy quietly loses are asserted: the override is
    /// pulled *out* of the field stream into the target column, and it is not
    /// also appended to the visible message text.
    #[test]
    fn record_event_owns_the_target_override_lift() {
        // A capture layer, not the ring: `record_event` is the seam under test.
        struct CaptureLayer(Arc<Mutex<Vec<RecordedEvent>>>);
        impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
            fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
                self.0.lock().unwrap().push(record_event(event));
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureLayer(Arc::clone(&captured)));
        tracing::subscriber::with_default(subscriber, || {
            // The shell-bridged shape: fixed callsite target + a runtime one.
            log_message(LogLevel::Warn, "fauna_web::sync", "upload failed");
            // No override: the callsite's static metadata target stands.
            tracing::info!(target: "my::target", peer = "alice", "rename failed");
        });

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 2, "both events folded");

        assert_eq!(
            events[0].target, "fauna_web::sync",
            "the override becomes the target, not the fixed callsite `fauna_client`"
        );
        assert_eq!(
            events[0].message, "upload failed",
            "the override is lifted out, never appended as `log_target=…`"
        );

        assert_eq!(
            events[1].target, "my::target",
            "no override: the static metadata target stands"
        );
        assert!(
            events[1].message.contains("rename failed")
                && events[1].message.contains("peer=")
                && events[1].message.contains("alice"),
            "other fields still append after the message: {:?}",
            events[1].message
        );
    }

    #[test]
    fn evicts_oldest_past_capacity() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        with_ring_subscriber(|| {
            for i in 0..(RING_CAPACITY + 50) {
                tracing::info!("entry {i}");
            }
        });
        let snap = snapshot();
        assert_eq!(snap.len(), RING_CAPACITY, "ring stays bounded");
        // Oldest 50 were evicted; the newest entry is the last one logged.
        assert!(
            snap.last()
                .unwrap()
                .message
                .contains(&format!("entry {}", RING_CAPACITY + 49))
        );
        assert!(
            snap.first()
                .unwrap()
                .message
                .contains(&format!("entry {}", 50))
        );
    }

    #[test]
    fn log_message_emits_with_caller_target_and_level() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        with_ring_subscriber(|| {
            // The FFI / WASM `log_message` shim path: a shell hands us a runtime
            // level + a runtime target (which `tracing`'s static metadata can't
            // carry), and we surface both in the ring.
            log_message(LogLevel::Error, "fauna_web::sync", "upload failed");
            log_message(LogLevel::Info, "FaunaApp.Onboarding", "saved identity");
        });
        let snap = snapshot();
        assert_eq!(snap.len(), 2, "both shell messages captured");
        assert_eq!(snap[0].level, LogLevel::Error);
        assert_eq!(
            snap[0].target, "fauna_web::sync",
            "caller target preserved in the column, not the fixed callsite target"
        );
        assert_eq!(snap[0].message, "upload failed", "message stays clean");
        assert_eq!(snap[1].level, LogLevel::Info);
        assert_eq!(snap[1].target, "FaunaApp.Onboarding");
        assert_eq!(snap[1].message, "saved identity");
    }

    /// Ring ingest strips control characters — message, structured fields, and
    /// the shell-supplied target override alike. Ring text is rendered into
    /// terminals (the tui admin-logs / settings-logs pages), and log writes are
    /// user-influenceable (a rejected URL on the media-proxy warn path carries
    /// the client's raw query string), so an unstripped `\x1b`/`\x07` is a
    /// terminal-escape injection into an admin's screen. Mirrors the sidecar
    /// plane's admission strip (`log_plane.rs`).
    #[test]
    fn ring_ingest_strips_control_characters() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        with_ring_subscriber(|| {
            // The media-proxy shape: an attacker-chosen value as a structured
            // field on a warn — plus a raw ESC/BEL/newline in a message body
            // and in a shell `log_message` target override.
            tracing::warn!(
                url = %"https://x/\x1b[31mRED\x1b]0;title\x07",
                "media proxy: URL rejected"
            );
            tracing::error!("line one\nforged ERROR line\ttabbed");
            log_message(
                LogLevel::Info,
                "fauna_web::sync\x1b[2J",
                "saved\x08\x08\x08wiped",
            );
        });

        let snap = snapshot();
        assert_eq!(snap.len(), 3);
        for entry in &snap {
            assert!(
                !entry.message.chars().any(|c| c.is_control()),
                "control char survived to the ring message: {:?}",
                entry.message
            );
            assert!(
                !entry.target.chars().any(|c| c.is_control()),
                "control char survived to the ring target: {:?}",
                entry.target
            );
        }
        // The strip removes the control characters, never the content.
        assert!(snap[0].message.contains("RED"), "got {:?}", snap[0].message);
        assert!(snap[0].message.contains("media proxy: URL rejected"));
        assert!(snap[1].message.contains("forged ERROR line"));
        assert_eq!(snap[2].target, "fauna_web::sync[2J");
        assert_eq!(snap[2].message, "savedwiped");
    }

    #[test]
    fn snapshot_at_least_filters_by_severity() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        with_ring_subscriber(|| {
            tracing::error!("e");
            tracing::warn!("w");
            tracing::info!("i");
            tracing::debug!("d"); // dropped by default? no — no filter here, so captured
        });
        // No EnvFilter in this subscriber, so all four are captured.
        assert_eq!(snapshot().len(), 4);
        // At least WARN → Error + Warn only.
        let warn_plus = snapshot_at_least(LogLevel::Warn);
        assert_eq!(warn_plus.len(), 2);
        assert!(warn_plus.iter().all(|e| e.level <= LogLevel::Warn));
    }

    // ── The remote ring (sidecar log plane) ──────────────────────────────────

    fn remote(timestamp_ms: u64, message: &str) -> LogEntry {
        LogEntry {
            timestamp_ms,
            level: LogLevel::Info,
            target: "mta:startup".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn remote_ring_stays_bounded_and_evicts_oldest() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        for i in 0..(REMOTE_RING_CAPACITY + 50) {
            push_remote_entry(remote(1000 + i as u64, &format!("remote {i}")));
        }
        let snap = snapshot_remote();
        assert_eq!(
            snap.len(),
            REMOTE_RING_CAPACITY,
            "remote ring stays bounded"
        );
        assert!(
            snap.first().unwrap().message == format!("remote {}", 50),
            "oldest 50 evicted FIFO, got {:?}",
            snap.first().unwrap().message
        );
        assert!(
            snap.last().unwrap().message == format!("remote {}", REMOTE_RING_CAPACITY + 49),
            "newest retained"
        );
    }

    /// The storage primitive enforces the control-character strip for the
    /// remote ring too — `log_plane::admit` sanitizes first (its byte cap must
    /// count real content), but the invariant "no ring entry carries a control
    /// character" belongs to the rings themselves, not to each caller.
    #[test]
    fn remote_ring_ingest_strips_control_characters() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        push_remote_entry(LogEntry {
            timestamp_ms: 1_000,
            level: LogLevel::Warn,
            target: "mta:tls.handshake_failed\x1b[31m".to_string(),
            message: "handshake\x1b]0;owned\x07 failed\n".to_string(),
        });
        let snap = snapshot_remote();
        assert_eq!(snap.len(), 1);
        assert!(
            !snap[0].message.chars().any(|c| c.is_control())
                && !snap[0].target.chars().any(|c| c.is_control()),
            "control char survived remote ingest: {:?} / {:?}",
            snap[0].target,
            snap[0].message
        );
        assert!(snap[0].message.contains("handshake"));
        assert!(snap[0].message.contains("failed"));
    }

    #[test]
    fn merged_snapshot_is_timestamp_ordered_across_both_rings() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        // Interleave: nest entries land via the subscriber (their timestamps are
        // "now"), remote entries carry their own — so assert on an explicit set
        // pushed directly into each ring rather than on wall-clock ordering.
        push_remote_entry(remote(200, "b remote"));
        push_remote_entry(remote(400, "d remote"));
        push(LogEntry {
            timestamp_ms: 100,
            level: LogLevel::Warn,
            target: "fauna_nest".into(),
            message: "a nest".into(),
        });
        push(LogEntry {
            timestamp_ms: 300,
            level: LogLevel::Warn,
            target: "fauna_nest".into(),
            message: "c nest".into(),
        });

        let merged: Vec<String> = snapshot_merged().into_iter().map(|e| e.message).collect();
        assert_eq!(
            merged,
            vec!["a nest", "b remote", "c nest", "d remote"],
            "merged oldest-first by timestamp_ms across both rings"
        );
    }

    #[test]
    fn a_remote_flood_can_never_evict_the_nests_own_history() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        // The security property of the split ring: the nest's own entry survives
        // a flood far larger than either ring's capacity.
        push(LogEntry {
            timestamp_ms: 1,
            level: LogLevel::Error,
            target: "fauna_nest".into(),
            message: "the nest's own history".into(),
        });
        for i in 0..(REMOTE_RING_CAPACITY * 3) {
            push_remote_entry(remote(1000 + i as u64, &format!("flood {i}")));
        }
        assert_eq!(snapshot().len(), 1, "nest ring untouched by the flood");
        assert_eq!(snapshot()[0].message, "the nest's own history");
        assert_eq!(
            snapshot_remote().len(),
            REMOTE_RING_CAPACITY,
            "the flood is bounded to the remote ring's own capacity"
        );
    }

    #[test]
    fn clear_drops_both_rings() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        push_remote_entry(remote(1, "remote"));
        push(LogEntry {
            timestamp_ms: 2,
            level: LogLevel::Info,
            target: "fauna_nest".into(),
            message: "local".into(),
        });
        assert_eq!(snapshot_merged().len(), 2);
        clear();
        assert!(
            snapshot_merged().is_empty(),
            "the Clear affordance drops everything the surface shows"
        );
    }
}
