//! Test-only tracing capture for this crate's native test suite.
//!
//! A crate-local copy of `fauna_client_mls_sync::test_tracing`'s
//! `capture_tracing_at_info` (its own doc: equivalents stay crate-local by
//! design, not a shared test-support crate — the same reasoning that keeps
//! `fauna_onboarding_machine::machine` and `fauna_provisioning::progress`'s
//! own copies from becoming one).

use std::sync::{Arc, Mutex};

use tracing_subscriber::prelude::*;

/// Records every field on a captured event, not only the literal `message` —
/// a caller that needs to prove an attacker-controlled value carried on a
/// non-message field (e.g. `error = %e`) actually reached this process can
/// `.contains()` the full line, rather than being blind to anything the
/// format string itself didn't interpolate.
#[derive(Default)]
struct AllFields {
    message: String,
    rest: String,
}

impl tracing::field::Visit for AllFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.rest.push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}

struct CaptureLayer(Arc<Mutex<Vec<String>>>);

impl<S> tracing_subscriber::Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = AllFields::default();
        event.record(&mut visitor);
        self.0.lock().unwrap().push(format!(
            "[{}][{}] {}{}",
            event.metadata().level(),
            event.metadata().target(),
            visitor.message,
            visitor.rest,
        ));
    }
}

/// A fresh capturing subscriber filtered at `info` — the filter `fauna-log`'s
/// ring really runs (`EnvFilter::new("info")`), so a line captured here is a
/// line that reaches the ring in production — plus the buffer it writes into.
fn capturing_subscriber() -> (
    impl tracing::Subscriber + Send + Sync + 'static,
    Arc<Mutex<Vec<String>>>,
) {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(CaptureLayer(Arc::clone(&captured)));
    (subscriber, captured)
}

/// Run `f` under a capturing `tracing` subscriber filtered at `info`. Returns
/// `f`'s value and the captured lines as `"[LEVEL][target] message field=value …"`
/// — the message text first, unchanged in position from before this captured
/// other fields too, so existing byte-for-byte assertions keep matching; any
/// non-message field is appended after.
///
/// exposure check: this crate's two
/// call sites (`the_drain_side_apply_reports_*`) are the ONLY callers of
/// `SessionInboxApply::apply_welcome` — and hence of
/// `report_welcome_ingest_failure`'s `tracing::{debug,info,error}!` callsites
/// — in this test binary; no sibling test can reach them with no subscriber
/// installed. Demonstrated unexposed; the thread-local `with_default` here
/// is not a defect.
pub(crate) fn capture_tracing_at_info<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let (subscriber, captured) = capturing_subscriber();
    let out = tracing::subscriber::with_default(subscriber, f);
    let lines = captured.lock().unwrap().clone();
    (out, lines)
}

/// `f`'s async twin, for a witness that must drive a REAL suspension point (a
/// loopback socket, genuine I/O) rather than
/// [`fauna_client_testkit::block_on`]'s single-poll executor, which panics on
/// the first `Pending` and so cannot wrap real I/O. Holds the subscriber for the whole `.await`;
/// valid only on a single OS thread — every caller must run under
/// `#[tokio::test]`'s default `current_thread` flavor, never
/// `flavor = "multi_thread"`, which could resume `f` on a different thread
/// than the one holding this thread-local guard.
pub(crate) async fn capture_tracing_at_info_async<F: std::future::Future>(
    f: F,
) -> (F::Output, Vec<String>) {
    let (subscriber, captured) = capturing_subscriber();
    let guard = tracing::subscriber::set_default(subscriber);
    let out = f.await;
    drop(guard);
    let lines = captured.lock().unwrap().clone();
    (out, lines)
}
