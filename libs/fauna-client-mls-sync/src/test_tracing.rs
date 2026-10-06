//! Test-only tracing capture shared by this crate's native test suites.
//!
//! The provider CAS merge's two-writer warning is a user-visible line — it
//! reaches `fauna-log`'s ring and thence every app's Settings → Logs page — so
//! the tests that assert it (`store`) and the tests that assert its *absence*
//! (`orchestration`) both need the same capture, filtered at the ring's real
//! default. One copy here; the equivalents in `fauna_onboarding_machine::machine`
//! and `fauna_provisioning::progress` stay crate-local by the same reasoning
//! that keeps this one from becoming a cross-crate test dependency — but,
//! per `fauna_provisioning::progress::tests::install_narration_capture`'s doc
//! comment, "crate-local" no longer means "trivial": this capture is exposed
//! by the exact defect that row fixed, and carries the same **process-global, once-per-binary**
//! subscriber for the same reason — see [`install_capture`] below.

use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;

static CAPTURED: OnceLock<Mutex<Vec<(ThreadId, String)>>> = OnceLock::new();

/// Install the capturing subscriber for the whole test binary, once.
///
/// ⚠ Deliberately a **process-global** subscriber (`set_global_default`), not
/// a thread-local `tracing::subscriber::with_default` — `tracing` caches each
/// callsite's interest process-globally on first use, so whichever thread
/// reaches a callsite first decides whether it is enabled for everyone.
/// `store::tests::the_conflict_winner_persists_past_the_losers_next_flush_unreported`
/// calls `save_provider_cas` with a genuinely conflicting base *without* this
/// capture active (it only needs the WARN observed on a later call in the same
/// test), so it reaches `store.rs`'s two-writer `tracing::warn!` with no
/// subscriber at all; any sibling test racing a capturing test for that same
/// callsite can then lose the line. This is exactly `fauna_provisioning::
/// progress`'s defect, generalized — same
/// fix: one subscriber for the binary, events tagged with the emitting
/// thread, each caller reading back only its own thread's lines.
fn install_capture() {
    use tracing_subscriber::prelude::*;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(CaptureLayer);
        // Nothing else in this binary sets a global default; if that ever
        // changes, a capturing test fails with an empty capture rather than
        // silently asserting against someone else's subscriber.
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
        CAPTURED
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .push((std::thread::current().id(), line));
    }
}

/// Run `f` under the process-global capturing subscriber, filtered at
/// `info` — the filter `fauna-log`'s ring really runs (`EnvFilter::new(
/// "info")`), so a line captured here is a line that reaches the ring in
/// production. Returns `f`'s value and the lines emitted by the CALLING
/// thread while `f` ran, as `"[LEVEL][target] message"`.
///
/// Filtering by thread (rather than draining a shared buffer) is what keeps
/// one test's lines out of another's assertions when the global subscriber
/// is shared by the whole binary — an existence assert over an undrained
/// shared buffer would otherwise pass on a sibling's line even if `f` itself
/// produced nothing.
pub(crate) fn capture_tracing_at_info<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    install_capture();
    let me = std::thread::current().id();
    let start = CAPTURED.get_or_init(Default::default).lock().unwrap().len();
    let out = f();
    let lines = CAPTURED
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .iter()
        .skip(start)
        .filter(|(thread, _)| *thread == me)
        .map(|(_, line)| line.clone())
        .collect();
    (out, lines)
}
