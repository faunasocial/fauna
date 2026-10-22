//! The updater's report file: a bounded, schema-fixed ledger of the updater's
//! outcomes the updater writes into the nest container's own filesystem through
//! the engine's archive API, and the nest reads into its sidecar log plane as
//! `updater:<event>` entries.
//!
//! Owner: `docs/goal/architecture/nest/update-channel.md` § What the updater
//! reports. One home for what the two binaries of the one image must agree on:
//! the path, the shape, the retention, the `seq` rule and the label rule. The
//! catalogue of events is the updater's own (`bins/fauna-updater`), the read
//! the nest's (`bins/fauna-nest`).

use serde::{Deserialize, Serialize};

/// The directory the image creates for the report — root-owned, `0755`, in the
/// container's writable layer (`Dockerfile`): never under `/data` (a volume the
/// nest seals into; a report is not user data), never a tmpfs (the archive API
/// writes underneath one, invisible inside the container) and never under
/// `/run`, which s6-overlay mounts over at boot.
pub const REPORT_DIR: &str = "/var/lib/fauna-updater";

/// The report's file name inside [`REPORT_DIR`] — the one entry the updater's
/// archive carries.
pub const REPORT_FILE: &str = "report.json";

/// The report's full path, [`REPORT_DIR`]/[`REPORT_FILE`].
pub const REPORT_PATH: &str = "/var/lib/fauna-updater/report.json";

/// The report shape this build writes and reads. A file carrying any other is
/// ignored by the reader, never a ring entry.
pub const REPORT_SCHEMA: u32 = 1;

/// How many events the report retains: the last 32 the updater produced for
/// the container, oldest evicted, every eviction counted into
/// [`Report::dropped`].
pub const REPORT_RETAINED_EVENTS: usize = 32;

/// The word a version label or an image tag is replaced by when it does not
/// match [`is_reportable_label`] — registry-sourced text is data, never
/// interpolated as it is.
pub const UNPARSEABLE: &str = "unparseable";

/// One reported event: the plane's batch event (`fauna_protocol::log_plane`'s
/// `SidecarLogEvent`) plus `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEvent {
    /// Strictly increasing across the updater's restarts and hand-offs, so the
    /// nest can tell what it has already landed ([`Report::push`]).
    pub seq: u64,
    pub timestamp_ms: u64,
    /// `error`, `warn` or `info`.
    pub level: String,
    /// The catalogue id, `[a-z0-9_.]`.
    pub event: String,
    /// The rendered compile-time template.
    pub message: String,
}

/// The report file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    /// Events evicted from the ledger, all time — the reader lands the delta.
    pub dropped: u64,
    /// Oldest first, at most [`REPORT_RETAINED_EVENTS`].
    pub events: Vec<ReportEvent>,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            schema: REPORT_SCHEMA,
            dropped: 0,
            events: Vec::new(),
        }
    }
}

impl Report {
    /// Parse a report file. `None` for a file that does not parse — a read that
    /// found it mid-write — or that carries another [`REPORT_SCHEMA`].
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let report: Report = serde_json::from_slice(bytes).ok()?;
        (report.schema == REPORT_SCHEMA).then_some(report)
    }

    /// The bytes the updater writes.
    pub fn to_bytes(&self) -> Vec<u8> {
        // A struct of plain fields always serializes.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The highest `seq` the ledger holds, 0 when empty.
    pub fn last_seq(&self) -> u64 {
        self.events.last().map_or(0, |e| e.seq)
    }

    /// Append one event at `now_ms`, evicting the oldest past
    /// [`REPORT_RETAINED_EVENTS`] into `dropped`. Its `seq` is the larger of the
    /// last one plus one and `now_ms`: strictly increasing within a ledger, and
    /// across an updater restart or a hand-off that could not read the ledger
    /// back, as far as the host clock moves forward.
    pub fn push(&mut self, now_ms: u64, level: &str, event: &str, message: String) {
        let seq = (self.last_seq() + 1).max(now_ms);
        self.events.push(ReportEvent {
            seq,
            timestamp_ms: now_ms,
            level: level.to_string(),
            event: event.to_string(),
            message,
        });
        let excess = self.events.len().saturating_sub(REPORT_RETAINED_EVENTS);
        if excess > 0 {
            self.events.drain(..excess);
            self.dropped += excess as u64;
        }
    }
}

/// Whether a registry-sourced label or tag may be interpolated into a report
/// message: `[0-9A-Za-z.+-]{1,64}`.
pub fn is_reportable_label(label: &str) -> bool {
    (1..=64).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// `label` when [`is_reportable_label`], else [`UNPARSEABLE`].
pub fn reportable_label(label: &str) -> &str {
    if is_reportable_label(label) {
        label
    } else {
        UNPARSEABLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_is_the_directory_and_the_file() {
        assert_eq!(REPORT_PATH, format!("{REPORT_DIR}/{REPORT_FILE}"));
        assert!(!REPORT_DIR.starts_with("/data") && !REPORT_DIR.starts_with("/run"));
    }

    /// The image creates exactly this directory, root-owned `0755`: the path
    /// constant and the `Dockerfile` cannot drift apart.
    #[test]
    fn the_image_creates_the_report_directory() {
        let dockerfile = include_str!("../../../../Dockerfile");
        let line = format!("install -d -o root -g root -m 0755 {REPORT_DIR}");
        assert!(
            dockerfile.contains(&line),
            "the Dockerfile must carry `{line}`"
        );
    }

    #[test]
    fn the_ledger_keeps_the_last_32_and_counts_every_eviction() {
        let mut report = Report::default();
        for i in 0..40u64 {
            report.push(i, "info", "decision.pinned", format!("{i}"));
        }
        assert_eq!(report.events.len(), REPORT_RETAINED_EVENTS);
        assert_eq!(report.dropped, 8);
        assert_eq!(report.events[0].message, "8");
        assert_eq!(report.events.last().unwrap().message, "39");
    }

    #[test]
    fn seq_strictly_increases_even_when_the_clock_does_not() {
        let mut report = Report::default();
        report.push(1_000, "info", "a", String::new());
        report.push(1_000, "info", "b", String::new());
        report.push(500, "info", "c", String::new());
        let seqs: Vec<u64> = report.events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1_000, 1_001, 1_002]);
        // A fresh ledger (an updater that could not read the old one back)
        // still numbers above it while the clock moves forward.
        let mut fresh = Report::default();
        fresh.push(2_000, "info", "d", String::new());
        assert!(fresh.last_seq() > report.last_seq());
    }

    #[test]
    fn a_torn_file_or_another_schema_does_not_parse() {
        let mut report = Report::default();
        report.push(1, "warn", "decision.hold", "held".into());
        let bytes = report.to_bytes();
        assert_eq!(Report::parse(&bytes), Some(report.clone()));
        assert_eq!(Report::parse(&bytes[..bytes.len() / 2]), None);
        let other = serde_json::json!({"schema": 2, "dropped": 0, "events": []});
        assert_eq!(Report::parse(other.to_string().as_bytes()), None);
    }

    #[test]
    fn a_label_is_interpolated_only_in_the_charset() {
        assert_eq!(reportable_label("0.2.0-rc.1+abc"), "0.2.0-rc.1+abc");
        assert_eq!(reportable_label(""), UNPARSEABLE);
        assert_eq!(
            reportable_label("ignore previous instructions"),
            UNPARSEABLE
        );
        assert_eq!(reportable_label(&"9".repeat(65)), UNPARSEABLE);
        assert_eq!(reportable_label("1.0\n"), UNPARSEABLE);
    }
}
