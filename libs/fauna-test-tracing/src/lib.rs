//! Test-only tracing capture shared by the crates' native test suites.
//!
//! A user-visible log line reaches `fauna-log`'s ring (and every app's
//! Settings → Logs page) filtered at `info`; tests asserting a line, or its
//! absence, need the same filter. One owner here.
//!
//! ⚠ The subscriber is **process-global** (`set_global_default`, once per test
//! binary), never a thread-local `with_default`: `tracing` caches each
//! callsite's interest process-globally on first use, so whichever thread
//! reaches a callsite first decides whether it is enabled for everyone, and a
//! sibling test with no subscriber can make a capturing test lose its line.
//! Events are tagged with the emitting thread and each caller reads back only
//! its own thread's lines.

use std::sync::{Mutex, Once, OnceLock};
use std::thread::ThreadId;

static CAPTURED: OnceLock<Mutex<Vec<(ThreadId, String)>>> = OnceLock::new();

fn captured() -> &'static Mutex<Vec<(ThreadId, String)>> {
    CAPTURED.get_or_init(Default::default)
}

/// Install the capturing subscriber for the whole test binary, once.
/// Nothing else in the binary may set a global default; if it does, a
/// capturing test fails with an empty capture rather than silently asserting
/// against someone else's subscriber.
pub fn install_capture() {
    use tracing_subscriber::prelude::*;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(CaptureLayer);
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

#[derive(Default)]
struct MessageOnly(String);

impl tracing::field::Visit for MessageOnly {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

struct CaptureLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageOnly::default();
        event.record(&mut visitor);
        let line = format!(
            "[{}][{}] {}",
            event.metadata().level(),
            event.metadata().target(),
            visitor.0
        );
        captured()
            .lock()
            .unwrap()
            .push((std::thread::current().id(), line));
    }
}

/// Every line the calling thread has emitted so far, as
/// `"[LEVEL][target] message"`.
pub fn captured_lines_for_this_thread() -> Vec<String> {
    let me = std::thread::current().id();
    captured()
        .lock()
        .unwrap()
        .iter()
        .filter(|(thread, _)| *thread == me)
        .map(|(_, line)| line.clone())
        .collect()
}

/// Run `f` under the global capture and return its value plus the lines the
/// CALLING thread emitted while it ran.
pub fn capture_tracing_at_info<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    install_capture();
    let me = std::thread::current().id();
    let start = captured().lock().unwrap().len();
    let out = f();
    let lines = captured()
        .lock()
        .unwrap()
        .iter()
        .skip(start)
        .filter(|(thread, _)| *thread == me)
        .map(|(_, line)| line.clone())
        .collect();
    (out, lines)
}
