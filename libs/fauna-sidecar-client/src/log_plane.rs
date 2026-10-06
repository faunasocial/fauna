//! Source-side half of the **sidecar log plane** for Rust sidecars — the queue,
//! the flush, and the disable-on-refusal rule, in one audited copy.
//!
//! Authority: `docs/goal/architecture/apps/observability.md` § The sidecar
//! log plane. Each sidecar declares its own **compile-time event catalogue**
//! (see its binary) and calls [`emit`] at deliberately chosen sites; this module
//! owns everything that is not catalogue-specific, so `fauna-iroh-relay` and
//! any later sidecar share it rather than each growing a copy.
//!
//! **Strictly best-effort, by construction.** [`emit`] never blocks, never
//! fails, and never allocates without bound: the queue is capped at
//! [`QUEUE_CAPACITY`] and drops *oldest*-first, counting each drop so nest can
//! surface the loss. A sidecar's real work must never wait on, or fail because
//! of, its logging.
//!
//! **The authoring contract this module cannot enforce.** An event's `message`
//! must be a compile-time-constant template whose interpolations are limited to
//! the bounded value classes (counts, sizes, durations, ports, protocol/error
//! codes, DNS domain names, Fauna service names). **Never** free-form
//! remote-controlled text, upstream error strings passed through verbatim, mail
//! addresses, actor IDs, or secrets. Nest sanitizes as defense-in-depth, but it
//! cannot un-leak an address you interpolated — the catalogue is where this is
//! enforced, by review.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use fauna_protocol::RpcDispatcher;
use fauna_protocol::log_plane::{
    KIND_SIDECAR_LOG_EVENTS, MAX_EVENTS_PER_BATCH, ReportLogEventsReply, ReportLogEventsRequest,
    SidecarLogEvent,
};

/// Maximum events held before a flush. Older events are dropped first (a recent
/// failure is worth more to an admin than a stale one) and counted.
pub const QUEUE_CAPACITY: usize = 256;

/// Level of a plane event. Deliberately narrower than `tracing`'s set — the
/// plane carries only what an admin acts on; debug/trace detail stays in the
/// sidecar's own stderr, which the container log stream already captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneLevel {
    Error,
    Warn,
    Info,
}

impl PlaneLevel {
    fn as_wire(self) -> &'static str {
        match self {
            PlaneLevel::Error => "error",
            PlaneLevel::Warn => "warn",
            PlaneLevel::Info => "info",
        }
    }
}

#[derive(Default)]
struct Queue {
    events: VecDeque<SidecarLogEvent>,
    /// Events dropped since the last accepted batch — reported to nest so
    /// source-side loss is visible rather than silent.
    dropped: u64,
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);

/// Set once the nest answers `fauna.sidecar.log_events` with an error: the
/// same image will keep refusing, so retrying is futile for this process's life
/// (the source silently disables rather than error-looping).
static DISABLED: AtomicBool = AtomicBool::new(false);

/// True once reporting has been disabled by a refusing nest. Sidecars may check this
/// to skip building a message, but calling [`emit`] while disabled is harmless.
pub fn is_disabled() -> bool {
    DISABLED.load(Ordering::Relaxed)
}

/// Queue one catalogued event. Never blocks, never fails.
///
/// `event` is a catalogue constant (`[a-z0-9_.-]`, ≤64 bytes — nest rejects
/// anything else); `message` is the rendered constant template. See the module
/// docs for what may and may not be interpolated.
pub fn emit(level: PlaneLevel, event: &'static str, message: String) {
    if is_disabled() {
        return;
    }
    let Ok(mut guard) = QUEUE.lock() else {
        return; // a poisoned log queue must not take the sidecar down
    };
    let queue = guard.get_or_insert_with(Queue::default);
    while queue.events.len() >= QUEUE_CAPACITY {
        queue.events.pop_front();
        queue.dropped += 1;
    }
    queue.events.push_back(SidecarLogEvent {
        timestamp_ms: now_millis(),
        level: level.as_wire().to_string(),
        event: event.to_string(),
        message,
        ..Default::default()
    });
}

/// How many events are queued right now (tests / diagnostics).
pub fn queued_len() -> usize {
    QUEUE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|q| q.events.len()))
        .unwrap_or(0)
}

/// Send everything queued over an **already-authenticated** sidecar dispatcher.
///
/// Call this right after a successful handshake: a sidecar whose channel is
/// short-lived (the relay dials, fetches, and drops) has no long-lived
/// dispatcher to flush over, so the natural moment is "whenever a channel
/// exists". A long-lived sidecar calls it on its own timer/size trigger too.
///
/// Never returns an error — a failed flush drops the batch (already counted) and
/// leaves the sidecar's real work untouched. An *error reply* is read as "this
/// nest does not know the kind" and disables reporting for the process.
pub async fn flush(dispatcher: &Arc<RpcDispatcher>) {
    if is_disabled() {
        return;
    }
    let Some(request) = take_batch() else {
        return;
    };
    match crate::request_typed::<_, ReportLogEventsReply>(
        dispatcher,
        KIND_SIDECAR_LOG_EVENTS,
        &request,
    )
    .await
    {
        Ok(_) => {}
        Err(crate::SidecarDialError::Rejected(code)) => {
            // A permanent refusal (the sidecar ships in the same image as its
            // nest, so this is never version skew). Stop reporting rather than
            // error-loop; the events are gone, which is the correct trade for a
            // best-effort plane talking to a nest that will not receive it.
            DISABLED.store(true, Ordering::Relaxed);
            tracing_disabled(&code);
        }
        Err(_) => {
            // Transport hiccup: drop the batch's EVENTS and stay enabled.
            // Re-queueing the events would let a flapping channel grow the queue
            // without bound — that trade is deliberate.
            //
            // But the *count* must survive. `take_batch` already zeroed the
            // pending `dropped`, so without this fold-back a failed flush loses
            // both the events and the fact that they existed, and nest's ring
            // shows an unbroken story with a silent hole in it. Folding the loss
            // back is bounded by construction (one integer, no queue growth), so
            // it costs nothing the re-queue would have cost.
            //
            // Counted: the events this batch was carrying, plus the drops it was
            // already reporting on behalf of earlier evictions.
            fold_back_dropped(request.dropped, request.events.len());
        }
    }
}

/// Return a failed flush's loss to the pending drop counter (see the `Err(_)`
/// arm above). Saturating rather than wrapping: a wrapped counter would report
/// a *small* number after a very long outage, which reads as healthy — the one
/// outcome worse than an unbounded-looking one.
fn fold_back_dropped(reported: u64, events: usize) {
    let lost = reported.saturating_add(events as u64);
    if lost == 0 {
        return;
    }
    if let Ok(mut guard) = QUEUE.lock() {
        // `get_or_insert_with`, not `as_mut`: a loss is worth recording even
        // when no queue has been allocated yet. The Go twin's `dropped` is a
        // package-level counter that is always present, so an `as_mut` here
        // would make the two halves disagree in exactly the case this whole
        // change exists to fix — one silently discarding what the other counts.
        let queue = guard.get_or_insert_with(Queue::default);
        queue.dropped = queue.dropped.saturating_add(lost);
    }
}

/// Drain up to one batch. `None` when there is nothing to report at all.
fn take_batch() -> Option<ReportLogEventsRequest> {
    let mut guard = QUEUE.lock().ok()?;
    let queue = guard.as_mut()?;
    if queue.events.is_empty() && queue.dropped == 0 {
        return None;
    }
    let take = queue.events.len().min(MAX_EVENTS_PER_BATCH);
    let events: Vec<_> = queue.events.drain(..take).collect();
    let dropped = std::mem::take(&mut queue.dropped);
    Some(ReportLogEventsRequest {
        events,
        dropped,
        ..Default::default()
    })
}

/// Note the disable on the sidecar's own stderr — this is exactly the kind of
/// thing the *local* log stream is for, and by definition it cannot ride the
/// plane.
fn tracing_disabled(code: &str) {
    eprintln!("[log-plane] nest rejected log_events ({code}); disabling plane reporting");
}

fn now_millis() -> u64 {
    fauna_core::data::Timestamp::now_millis_or_zero()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queue is process-global — serialize the tests that drive it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        DISABLED.store(false, Ordering::Relaxed);
        if let Ok(mut g) = QUEUE.lock() {
            *g = None;
        }
    }

    #[test]
    fn emit_queues_with_the_wire_level_and_a_timestamp() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        emit(PlaneLevel::Warn, "cert_fetch_failed", "code 42".into());
        let batch = take_batch().expect("a batch");
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].level, "warn");
        assert_eq!(batch.events[0].event, "cert_fetch_failed");
        assert_eq!(batch.events[0].message, "code 42");
        assert!(batch.events[0].timestamp_ms > 0);
        assert_eq!(batch.dropped, 0);
    }

    #[test]
    fn the_queue_is_bounded_and_drops_oldest_first_counting_them() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for i in 0..(QUEUE_CAPACITY + 10) {
            emit(PlaneLevel::Info, "tick", format!("{i}"));
        }
        assert_eq!(queued_len(), QUEUE_CAPACITY, "bounded");
        let batch = take_batch().expect("a batch");
        assert_eq!(batch.dropped, 10, "the 10 evicted are counted for nest");
        assert_eq!(
            batch.events[0].message, "10",
            "oldest dropped first — a recent failure outlives a stale one"
        );
    }

    #[test]
    fn a_batch_never_exceeds_the_wire_cap() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for i in 0..QUEUE_CAPACITY {
            emit(PlaneLevel::Info, "tick", format!("{i}"));
        }
        let batch = take_batch().expect("a batch");
        assert_eq!(batch.events.len(), MAX_EVENTS_PER_BATCH);
        assert_eq!(
            queued_len(),
            QUEUE_CAPACITY - MAX_EVENTS_PER_BATCH,
            "the remainder stays queued for the next flush"
        );
    }

    #[test]
    fn an_empty_queue_yields_no_batch() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(take_batch().is_none(), "nothing to say ⇒ no wire traffic");
    }

    #[test]
    fn a_drop_count_alone_is_still_worth_a_batch() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for i in 0..(QUEUE_CAPACITY + 5) {
            emit(PlaneLevel::Info, "tick", format!("{i}"));
        }
        // Drain the events but leave the drop count behind.
        while queued_len() > 0 {
            let _ = take_batch();
        }
        reset_events_only();
        emit_nothing_but_a_drop();
        let batch = take_batch().expect("a drop-only batch still reports");
        assert!(batch.events.is_empty());
        assert_eq!(batch.dropped, 3);
    }

    /// Helper for the drop-only case: clear events, keep a synthetic drop count.
    fn reset_events_only() {
        if let Ok(mut g) = QUEUE.lock()
            && let Some(q) = g.as_mut()
        {
            q.events.clear();
            q.dropped = 0;
        }
    }

    fn emit_nothing_but_a_drop() {
        if let Ok(mut g) = QUEUE.lock() {
            let q = g.get_or_insert_with(Queue::default);
            q.dropped = 3;
        }
    }

    /// A failed flush's loss must still be COUNTED, or nest's ring shows an
    /// unbroken story with a silent hole in it. The events themselves stay
    /// dropped — re-queueing is what would grow unboundedly on a flapping
    /// channel — so only the integer survives, which is why this is safe.
    ///
    /// Driven through `fold_back_dropped` rather than a real `flush`, because
    /// `flush` needs an authenticated dispatcher; this is the whole of what the
    /// transport-error arm does. The Go twin
    /// (`internal/logplane.TestAFailedFlushFoldsItsLossBackIntoTheDropCount`)
    /// covers the same property through its own `Flush` with a failing caller —
    /// keep the two in step.
    #[test]
    fn a_failed_flush_folds_its_loss_back_into_the_drop_count() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        emit(PlaneLevel::Info, "ready", "up".into());
        emit(PlaneLevel::Warn, "blip", "again".into());
        let batch = take_batch().expect("two events queued");
        assert_eq!(batch.events.len(), 2);
        assert_eq!(batch.dropped, 0);
        assert_eq!(queued_len(), 0, "take_batch drained the queue");

        // The flush fails at the transport: events gone, count preserved.
        fold_back_dropped(batch.dropped, batch.events.len());
        let next = take_batch().expect("a drop-only batch still reports");
        assert!(
            next.events.is_empty(),
            "the failed batch's events must NOT be resent"
        );
        assert_eq!(next.dropped, 2, "both lost events are counted for nest");
        reset();
    }

    /// A drop count already in flight when a flush fails is folded back
    /// ALONGSIDE the batch's events, never replaced by them.
    #[test]
    fn a_failed_flush_preserves_an_already_reported_drop_count() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        fold_back_dropped(5, 3);
        let batch = take_batch().expect("a drop-only batch still reports");
        assert_eq!(batch.dropped, 8, "5 already-reported + 3 lost events");
        reset();
    }

    /// Saturating, not wrapping: a wrapped counter reports a *small* number
    /// after a very long outage, which reads as healthy — worse than a large one.
    #[test]
    fn fold_back_saturates_rather_than_wrapping() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        fold_back_dropped(u64::MAX, 0);
        fold_back_dropped(10, 5);
        let batch = take_batch().expect("a drop-only batch still reports");
        assert_eq!(
            batch.dropped,
            u64::MAX,
            "saturated, not wrapped to a small number"
        );
        reset();
    }

    #[test]
    fn emit_is_a_no_op_once_disabled() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        DISABLED.store(true, Ordering::Relaxed);
        emit(PlaneLevel::Error, "boom", "x".into());
        assert_eq!(queued_len(), 0, "a refusing nest costs the sidecar nothing");
        reset();
    }
}
