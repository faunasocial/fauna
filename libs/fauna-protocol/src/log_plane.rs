//! The sidecar log plane — wire types shared by both of its legs.
//!
//! A co-resident service (mail bridge, iroh-relay, …) reports a
//! small allowlisted set of admin-meaningful events to nest, which lands them
//! in the admin Logs surface beside its own entries. Authority:
//! `docs/goal/architecture/apps/observability.md` § The sidecar log plane.
//!
//! **Two transports, one payload.** Which kind a source uses depends only on
//! which authenticated channel it already holds:
//!
//! * enrolled bridges (mail MTA/MDA, atproto-PDS, content-processor) →
//!   [`KIND_BRIDGES_REPORT_LOG_EVENTS`], caller-class-gated like every other
//!   `fauna.bridges.*` kind (`architecture/apps/bridges.md` § Bridge-kind
//!   catalogue);
//! * token-handshake sidecars (iroh-relay) →
//!   [`KIND_SIDECAR_LOG_EVENTS`], originated up the internal duplex the
//!   `fauna.sidecar.hello` handshake established (`sidecar::KIND_SIDECAR_HELLO`).
//!
//! **The source identity is deliberately absent from the payload.** Nest
//! derives it from the authenticated identity (bridge role / sidecar channel
//! scope) and stamps the ring entry's target as `<source>:<event>`, so a source
//! can neither speak as another source nor as the nest.
//!
//! **Additive discipline** (`version-compatibility.md`): an old nest answers
//! an unknown kind with an error and the source silently disables reporting for
//! that session; a new nest with an old source simply sees no events. Both
//! payloads carry `extra` so fields may be added without a compat break.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

/// Enrolled-bridge leg: a batch of reported events, bridge → nest.
pub const KIND_BRIDGES_REPORT_LOG_EVENTS: &str = "fauna.bridges.report_log_events";

/// Token-handshake sidecar leg: the same batch, sidecar → nest, up the internal
/// duplex (beside `sidecar::KIND_SIDECAR_HELLO` / the `fauna.relay.*` kinds).
pub const KIND_SIDECAR_LOG_EVENTS: &str = "fauna.sidecar.log_events";

/// Maximum events nest accepts in one batch; a source flushing more than this
/// has its batch truncated at admission (and the excess counted as dropped).
/// Hard-coded — no human chooses it (`principles.md` § One configuration
/// surface, bucket 1).
pub const MAX_EVENTS_PER_BATCH: usize = 128;

/// Maximum accepted `message` length in bytes, after control-character
/// stripping. Longer messages are truncated, never rejected.
pub const MAX_MESSAGE_BYTES: usize = 512;

/// Maximum accepted `event` identifier length in bytes.
pub const MAX_EVENT_BYTES: usize = 64;

/// One reported event.
///
/// `message` is a **compile-time-constant template** at the source, with
/// interpolation restricted to the bounded value classes (counts, sizes,
/// durations, ports, protocol/error codes, DNS domain names, Fauna service
/// names) — never free-form remote-controlled text, addresses, actor IDs, or
/// secrets. That is an authoring contract nest cannot verify, so admission
/// additionally sanitizes (see `log_plane` on the nest side).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidecarLogEvent {
    /// Milliseconds since the Unix epoch, stamped at the source.
    pub timestamp_ms: u64,
    /// One of `"error"`, `"warn"`, `"info"` — anything else is dropped and
    /// counted at admission. Debug/trace detail stays in the source's own
    /// stderr; the plane is for events an admin acts on.
    pub level: String,
    /// Stable catalogue identifier, `[a-z0-9_.-]`, e.g. `tls.handshake_failed`.
    /// Becomes the second half of the ring entry's `<source>:<event>` target.
    pub event: String,
    /// The rendered constant template.
    pub message: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// A batch of events, source → nest. Identical on both legs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportLogEventsRequest {
    pub events: Vec<SidecarLogEvent>,
    /// How many events the *source* dropped from its own bounded queue since
    /// the last accepted batch (overflow it could not send). Nest surfaces this
    /// the same way it surfaces its own admission drops, so source-side loss is
    /// visible rather than silent.
    pub dropped: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// Reply. Empty by design — the plane is strictly best-effort and must never
/// back-pressure the source's real work, so nest reports nothing a source would
/// act on. Success is the Reply's `ok = true`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReportLogEventsReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn event() -> SidecarLogEvent {
        SidecarLogEvent {
            timestamp_ms: 1_750_000_000_000,
            level: "warn".into(),
            event: "tls.handshake_failed".into(),
            message: "TLS handshake failed for example.com (code 42)".into(),
            ..Default::default()
        }
    }

    #[test]
    fn report_log_events_request_round_trips() {
        let req = ReportLogEventsRequest {
            events: vec![event()],
            dropped: 7,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: ReportLogEventsRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn reply_round_trips() {
        let bytes = encode_canonical(&ReportLogEventsReply::default()).unwrap();
        let back: ReportLogEventsReply = decode(&bytes).unwrap();
        assert_eq!(back, ReportLogEventsReply::default());
    }

    #[test]
    fn an_empty_batch_is_representable() {
        // A source may flush a batch carrying only a `dropped` count (its queue
        // overflowed and then drained) — nest must decode that, not reject it.
        let req = ReportLogEventsRequest {
            events: vec![],
            dropped: 3,
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: ReportLogEventsRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
        assert!(back.events.is_empty());
    }

    #[test]
    fn an_older_encoding_without_extra_still_decodes() {
        // Additive discipline: a source built before any future field-add sends
        // exactly the fields below, and a newer nest must decode it unchanged.
        #[derive(Serialize)]
        struct OldEvent {
            timestamp_ms: u64,
            level: String,
            event: String,
            message: String,
        }
        #[derive(Serialize)]
        struct OldRequest {
            events: Vec<OldEvent>,
            dropped: u64,
        }
        let old = OldRequest {
            events: vec![OldEvent {
                timestamp_ms: 1,
                level: "info".into(),
                event: "startup".into(),
                message: "started".into(),
            }],
            dropped: 0,
        };
        let bytes = encode_canonical(&old).unwrap();
        let back: ReportLogEventsRequest = decode(&bytes).unwrap();
        assert_eq!(back.events.len(), 1);
        assert_eq!(back.events[0].event, "startup");
        assert!(back.events[0].extra.is_empty());
    }

    #[test]
    fn the_two_kinds_are_the_ratified_strings() {
        // These strings are the wire contract (observability.md § Wire — two
        // legs); a rename is a compat break, not a refactor.
        assert_eq!(
            KIND_BRIDGES_REPORT_LOG_EVENTS,
            "fauna.bridges.report_log_events"
        );
        assert_eq!(KIND_SIDECAR_LOG_EVENTS, "fauna.sidecar.log_events");
    }
}
