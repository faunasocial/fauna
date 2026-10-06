//! Nest-side admission for the **sidecar log plane** — the one gate both of
//! its wire legs share.
//!
//! Authority: `docs/goal/architecture/apps/observability.md` § The sidecar
//! log plane. A co-resident service (mail bridge, iroh-relay, …)
//! reports a small allowlisted set of admin-meaningful events; this module
//! decides what actually reaches the admin Logs surface.
//!
//! Three jobs, in order:
//!
//! 1. **Attribution** — the ring entry's target is `<source>:<event>`, with
//!    `<source>` taken from [`LogSource`], which the *caller* derives from the
//!    authenticated identity (bridge caller class / sidecar channel scope) and
//!    never from the payload. A source therefore cannot speak as another source
//!    or as the nest.
//! 2. **Sanitization** (defense-in-depth behind the source-side authoring
//!    contract) — control characters stripped, `message` capped, `event`
//!    constrained to `[a-z0-9_.-]` and capped, levels outside {error, warn,
//!    info} dropped-and-counted, batch length capped, future timestamps
//!    clamped.
//! 3. **Rate limiting** — a per-source token bucket ([`SUSTAINED_PER_MIN`] /
//!    [`BURST`], hard-coded per `principles.md` § One configuration surface,
//!    bucket 1). Overflow drops the event and synthesizes at most one nest-side
//!    `warn` per source per minute naming the drop count, so flooding is
//!    visible, bounded, and attributed.
//!
//! Admitted entries land in `fauna_log`'s **separate** remote ring, so no
//! amount of sidecar chatter can evict the nest's own history.

use std::sync::Mutex;

use fauna_log::{LogEntry, LogLevel};
use fauna_protocol::log_plane::{
    MAX_EVENT_BYTES, MAX_EVENTS_PER_BATCH, MAX_MESSAGE_BYTES, ReportLogEventsRequest,
};

/// Sustained admitted events per source per minute.
pub const SUSTAINED_PER_MIN: u32 = 30;

/// Burst capacity of a source's bucket (a source may spend this much at once —
/// e.g. a startup batch — then refills at [`SUSTAINED_PER_MIN`]).
pub const BURST: u32 = 60;

/// Minimum gap between two synthesized drop warnings for the same source.
const DROP_WARN_INTERVAL_MS: u64 = 60_000;

/// The fixed nest-side source id set. **Never parsed from a payload** — each
/// value is derived from an authenticated identity at the call site, which is
/// what makes `<source>:<event>` an attribution rather than a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    /// Mail bridge, SMTP side.
    Mta,
    /// Mail bridge, IMAP/CalDAV side.
    Mda,
    /// The out-of-process ATProto PDS bridge.
    Atproto,
    /// A generic content-processing bridge (scorer / FTS-indexer).
    ContentProcessor,
    /// The iroh-relay sidecar (internal duplex).
    Relay,
}

impl LogSource {
    /// The `<source>` half of the ring target. Stable wire-visible strings —
    /// an admin reads these in the Logs page and they appear in support
    /// transcripts, so treat a rename as a user-visible change.
    pub fn as_str(self) -> &'static str {
        match self {
            LogSource::Mta => "mta",
            LogSource::Mda => "mda",
            LogSource::Atproto => "atproto",
            LogSource::ContentProcessor => "content_processor",
            LogSource::Relay => "relay",
        }
    }

    /// Dense index into the per-source bucket table.
    fn index(self) -> usize {
        match self {
            LogSource::Mta => 0,
            LogSource::Mda => 1,
            LogSource::Atproto => 2,
            LogSource::ContentProcessor => 3,
            LogSource::Relay => 4,
        }
    }
}

const SOURCE_COUNT: usize = 5;

/// Per-source rate-limit state. `tokens` is a plain float — this is process
/// state, not wire data (the DAG-CBOR float ban applies to the wire only).
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_refill_ms: u64,
    /// Drops (admission + source-reported) accumulated since the last warning.
    pending_drops: u64,
    /// When the last drop warning was synthesized; `None` = never.
    last_warn_ms: Option<u64>,
}

impl Bucket {
    const fn new() -> Self {
        Self {
            tokens: BURST as f64,
            last_refill_ms: 0,
            pending_drops: 0,
            last_warn_ms: None,
        }
    }
}

static BUCKETS: Mutex<[Bucket; SOURCE_COUNT]> = Mutex::new([Bucket::new(); SOURCE_COUNT]);

/// Outcome of admitting one batch — returned for tests and callers that want to
/// log a summary. Callers must NOT surface it to the source: the plane is
/// strictly best-effort and never back-pressures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmitOutcome {
    /// Events pushed to the remote ring.
    pub accepted: u64,
    /// Events refused here: malformed, over-capacity batch, or rate-limited.
    /// Does not include the source's own `dropped` count.
    pub rejected: u64,
}

/// Admit one reported batch from `source`. Never fails: an unusable event is
/// dropped and counted, never an error the source could act on.
pub fn admit(source: LogSource, req: &ReportLogEventsRequest) -> AdmitOutcome {
    admit_at(source, req, fauna_core::data::Timestamp::now_millis())
}

/// [`admit`] with an injected clock — the testable core.
pub fn admit_at(source: LogSource, req: &ReportLogEventsRequest, now_ms: u64) -> AdmitOutcome {
    let mut outcome = AdmitOutcome::default();

    // Batch cap: the excess is dropped, and counted like any other drop.
    let (events, over_batch) = if req.events.len() > MAX_EVENTS_PER_BATCH {
        (
            &req.events[..MAX_EVENTS_PER_BATCH],
            (req.events.len() - MAX_EVENTS_PER_BATCH) as u64,
        )
    } else {
        (&req.events[..], 0)
    };
    outcome.rejected += over_batch;

    let mut admitted = Vec::new();
    let mut local_drops = over_batch;

    for ev in events {
        let Some((level, event, message)) = sanitize(ev) else {
            outcome.rejected += 1;
            local_drops += 1;
            continue;
        };
        // A source's clock is the same host clock, so a future stamp is either
        // nonsense or an attempt to pin itself to the top of the merged view.
        let timestamp_ms = ev.timestamp_ms.min(now_ms);
        admitted.push(LogEntry {
            timestamp_ms,
            level,
            target: format!("{}:{}", source.as_str(), event),
            message,
        });
    }

    // Rate limit + drop bookkeeping under one lock; emit the warning after
    // releasing it (the tracing call re-enters `fauna_log`).
    let warn_drops = {
        let mut buckets = match BUCKETS.lock() {
            Ok(b) => b,
            Err(poisoned) => poisoned.into_inner(),
        };
        let bucket = &mut buckets[source.index()];
        refill(bucket, now_ms);

        let mut allowed = Vec::with_capacity(admitted.len());
        for entry in admitted {
            if bucket.tokens >= 1.0 {
                bucket.tokens -= 1.0;
                allowed.push(entry);
            } else {
                outcome.rejected += 1;
                local_drops += 1;
            }
        }

        // The source's own overflow count rides the same accounting, so
        // source-side loss is visible rather than silent.
        bucket.pending_drops += local_drops + req.dropped;

        let due = bucket.pending_drops > 0
            && bucket
                .last_warn_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= DROP_WARN_INTERVAL_MS);
        let warn_drops = if due {
            bucket.last_warn_ms = Some(now_ms);
            Some(std::mem::take(&mut bucket.pending_drops))
        } else {
            None
        };

        for entry in allowed {
            outcome.accepted += 1;
            fauna_log::push_remote_entry(entry);
        }
        warn_drops
    };

    if let Some(dropped) = warn_drops {
        // A *nest* observation about a misbehaving source, not a plane entry:
        // it goes through tracing into the nest's own ring (and stderr), where
        // the flood that caused it can never evict it.
        tracing::warn!(
            target: "fauna_nest::log_plane",
            source = source.as_str(),
            dropped,
            "sidecar log plane: dropped {dropped} event(s) from {}",
            source.as_str()
        );
    }

    outcome
}

fn refill(bucket: &mut Bucket, now_ms: u64) {
    if bucket.last_refill_ms == 0 {
        bucket.last_refill_ms = now_ms;
        return;
    }
    let elapsed_ms = now_ms.saturating_sub(bucket.last_refill_ms);
    if elapsed_ms == 0 {
        return;
    }
    let gained = elapsed_ms as f64 * (SUSTAINED_PER_MIN as f64 / 60_000.0);
    bucket.tokens = (bucket.tokens + gained).min(BURST as f64);
    bucket.last_refill_ms = now_ms;
}

/// Sanitize one reported event into `(level, event, message)`, or `None` if it
/// is unusable (unknown level, empty/invalid event id).
fn sanitize(ev: &fauna_protocol::log_plane::SidecarLogEvent) -> Option<(LogLevel, String, String)> {
    let level = match ev.level.as_str() {
        "error" => LogLevel::Error,
        "warn" => LogLevel::Warn,
        "info" => LogLevel::Info,
        // Debug/trace stay in the source's own stderr; anything else is junk.
        _ => return None,
    };

    // The event id is a catalogue constant, so a charset violation means a
    // broken or hostile source — refuse rather than silently rewrite it. Length
    // is merely capped (a real catalogue never approaches the cap).
    let event =
        fauna_core::encoding::truncate_to_char_boundary(&ev.event, MAX_EVENT_BYTES).to_string();
    if event.is_empty()
        || !event
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
    {
        return None;
    }

    // Control characters would let a report forge line structure in a log view;
    // strip before capping so the cap counts real content. (The ring's own
    // ingest re-applies the same strip at the storage boundary — this call is
    // the one that orders it ahead of the byte cap.)
    let stripped = fauna_log::strip_control_chars(&ev.message);
    let message =
        fauna_core::encoding::truncate_to_char_boundary(&stripped, MAX_MESSAGE_BYTES).to_string();

    Some((level, event, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::log_plane::SidecarLogEvent;

    /// `fauna_log`'s rings are process-global, as are the buckets — serialize.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        fauna_log::clear();
        let mut buckets = match BUCKETS.lock() {
            Ok(b) => b,
            Err(p) => p.into_inner(),
        };
        *buckets = [Bucket::new(); SOURCE_COUNT];
    }

    fn ev(event: &str, message: &str) -> SidecarLogEvent {
        SidecarLogEvent {
            timestamp_ms: 1_000,
            level: "warn".into(),
            event: event.into(),
            message: message.into(),
            ..Default::default()
        }
    }

    /// The synthesized drop warning is a real `tracing` event, so a test that
    /// asserts on it must install the same `RingLayer` the nest installs.
    fn with_ring_subscriber<T>(f: impl FnOnce() -> T) -> T {
        use tracing_subscriber::layer::SubscriberExt as _;
        let subscriber = tracing_subscriber::registry().with(fauna_log::RingLayer);
        tracing::subscriber::with_default(subscriber, f)
    }

    fn batch(events: Vec<SidecarLogEvent>) -> ReportLogEventsRequest {
        ReportLogEventsRequest {
            events,
            dropped: 0,
            ..Default::default()
        }
    }

    #[test]
    fn target_is_source_colon_event_and_source_comes_from_the_caller() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let out = admit_at(
            LogSource::Mta,
            &batch(vec![ev("tls.handshake_failed", "boom")]),
            5_000,
        );
        assert_eq!(out.accepted, 1);
        let ring = fauna_log::snapshot_remote();
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].target, "mta:tls.handshake_failed");
        assert_eq!(ring[0].message, "boom");
        assert_eq!(ring[0].level, LogLevel::Warn);
    }

    #[test]
    fn a_source_cannot_impersonate_the_nest_or_another_source() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // Whatever the payload says, the target is prefixed with the caller's
        // own authenticated source id — impersonation is unrepresentable.
        let out = admit_at(
            LogSource::Relay,
            &batch(vec![ev("nest", "I am the nest")]),
            5_000,
        );
        assert_eq!(out.accepted, 1);
        let ring = fauna_log::snapshot_remote();
        assert_eq!(ring[0].target, "relay:nest");
        assert!(!ring[0].target.starts_with("fauna_nest"));
    }

    #[test]
    fn control_characters_are_stripped_from_the_message() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let out = admit_at(
            LogSource::Mda,
            &batch(vec![ev(
                "bind.failed",
                "line one\nERROR forged line\r\n\ttab",
            )]),
            5_000,
        );
        assert_eq!(out.accepted, 1);
        let msg = &fauna_log::snapshot_remote()[0].message;
        assert!(!msg.contains('\n'), "no newline survives: {msg:?}");
        assert!(!msg.contains('\r'));
        assert!(!msg.contains('\t'));
        assert_eq!(msg, "line oneERROR forged linetab");
    }

    #[test]
    fn an_over_long_message_is_capped() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let long = "x".repeat(MAX_MESSAGE_BYTES * 3);
        admit_at(LogSource::Mta, &batch(vec![ev("flood", &long)]), 5_000);
        assert_eq!(
            fauna_log::snapshot_remote()[0].message.len(),
            MAX_MESSAGE_BYTES
        );
    }

    #[test]
    fn an_invalid_event_id_is_dropped_and_counted() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for bad in ["", "Has.Upper", "has space", "has/slash", "emoji🙂"] {
            reset();
            let out = admit_at(LogSource::Mta, &batch(vec![ev(bad, "m")]), 5_000);
            assert_eq!(out.accepted, 0, "{bad:?} must not be admitted");
            assert_eq!(out.rejected, 1);
            assert!(fauna_log::snapshot_remote().is_empty());
        }
    }

    #[test]
    fn a_level_outside_the_plane_set_is_dropped_and_counted() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        for bad in ["debug", "trace", "fatal", "WARN", ""] {
            reset();
            let mut e = ev("x", "m");
            e.level = bad.into();
            let out = admit_at(LogSource::Mta, &batch(vec![e]), 5_000);
            assert_eq!(out.accepted, 0, "level {bad:?} must not be admitted");
            assert_eq!(out.rejected, 1);
        }
    }

    #[test]
    fn an_over_length_batch_is_capped_and_the_excess_counted() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let events = (0..MAX_EVENTS_PER_BATCH + 10)
            .map(|i| ev("x", &format!("m{i}")))
            .collect();
        let out = admit_at(LogSource::Mta, &batch(events), 5_000);
        assert!(out.rejected >= 10, "the 10 over the cap are rejected");
        // Burst is smaller than the batch cap, so acceptance is bounded by it.
        assert_eq!(out.accepted, BURST as u64);
    }

    #[test]
    fn a_future_timestamp_is_clamped_to_now() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let mut e = ev("x", "m");
        e.timestamp_ms = u64::MAX;
        admit_at(LogSource::Mta, &batch(vec![e]), 5_000);
        assert_eq!(
            fauna_log::snapshot_remote()[0].timestamp_ms,
            5_000,
            "a source cannot pin itself above the nest's newest entry"
        );
    }

    #[test]
    fn the_bucket_bounds_a_flood_and_refills_at_the_sustained_rate() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // Burst first: exactly BURST accepted out of a much larger flood.
        let mut accepted = 0;
        for _ in 0..10 {
            let events = (0..MAX_EVENTS_PER_BATCH).map(|_| ev("x", "m")).collect();
            accepted += admit_at(LogSource::Mta, &batch(events), 1_000).accepted;
        }
        assert_eq!(
            accepted, BURST as u64,
            "burst is the hard ceiling at one instant"
        );

        // One minute later the bucket has refilled by the sustained rate.
        let events = (0..MAX_EVENTS_PER_BATCH).map(|_| ev("x", "m")).collect();
        let out = admit_at(LogSource::Mta, &batch(events), 61_000);
        assert_eq!(out.accepted, SUSTAINED_PER_MIN as u64);
    }

    #[test]
    fn one_sources_flood_does_not_consume_anothers_budget() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let events: Vec<_> = (0..MAX_EVENTS_PER_BATCH).map(|_| ev("x", "m")).collect();
        for _ in 0..5 {
            admit_at(LogSource::Mta, &batch(events.clone()), 1_000);
        }
        let out = admit_at(
            LogSource::Relay,
            &batch(vec![ev("up", "relay started")]),
            1_000,
        );
        assert_eq!(out.accepted, 1, "buckets are per source");
    }

    #[test]
    fn the_drop_warning_is_synthesized_at_most_once_per_source_per_minute() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let flood: Vec<_> = (0..MAX_EVENTS_PER_BATCH).map(|_| ev("x", "m")).collect();
        // Drive several flooding batches inside the same minute.
        with_ring_subscriber(|| {
            for _ in 0..5 {
                admit_at(LogSource::Mta, &batch(flood.clone()), 1_000);
            }
        });
        // The warning lands in the NEST ring (a nest observation), so it cannot
        // be evicted by the very flood it reports.
        let nest_warns: Vec<_> = fauna_log::snapshot()
            .into_iter()
            .filter(|e| e.message.contains("sidecar log plane"))
            .collect();
        assert_eq!(
            nest_warns.len(),
            1,
            "at most one warning per source per minute"
        );
        assert_eq!(nest_warns[0].level, LogLevel::Warn);
        assert!(
            nest_warns[0].message.contains("mta"),
            "the warning is attributed"
        );

        // A minute later, a further drop earns a second warning.
        with_ring_subscriber(|| {
            admit_at(LogSource::Mta, &batch(flood.clone()), 62_000);
        });
        let nest_warns: Vec<_> = fauna_log::snapshot()
            .into_iter()
            .filter(|e| e.message.contains("sidecar log plane"))
            .collect();
        assert_eq!(nest_warns.len(), 2);
    }

    #[test]
    fn source_reported_drops_are_counted_into_the_warning() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let req = ReportLogEventsRequest {
            events: vec![ev("x", "m")],
            dropped: 41,
            ..Default::default()
        };
        let out = with_ring_subscriber(|| admit_at(LogSource::Relay, &req, 1_000));
        assert_eq!(out.accepted, 1);
        let warns: Vec<_> = fauna_log::snapshot()
            .into_iter()
            .filter(|e| e.message.contains("sidecar log plane"))
            .collect();
        assert_eq!(warns.len(), 1, "source-side loss is surfaced, not silent");
        assert!(
            warns[0].message.contains("41"),
            "the source's own overflow count rides the same accounting: {:?}",
            warns[0].message
        );
    }

    #[test]
    fn an_empty_batch_carrying_only_a_drop_count_is_accepted() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let req = ReportLogEventsRequest {
            events: vec![],
            dropped: 5,
            ..Default::default()
        };
        let out = admit_at(LogSource::Mda, &req, 1_000);
        assert_eq!(
            out,
            AdmitOutcome {
                accepted: 0,
                rejected: 0
            }
        );
    }
}
