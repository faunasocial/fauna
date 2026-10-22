//! The nest's read of the updater's report file (`nest/update-channel.md`
//! § What the updater reports → *The nest's read*): at boot and every
//! [`REPORT_POLL_INTERVAL`], the events above an in-memory high-water `seq`
//! become one sidecar-log-plane batch, admitted through the one gate every leg
//! shares under the fixed source `updater` ([`LogSource::Updater`]). The file
//! is attributed by where it is — written by the engine's socket holder into
//! this container's root-owned directory — never by anything it says.
//!
//! The mark starts at zero, so a nest start lands the retained history again,
//! on purpose: the ring is in memory. A file that does not parse (a read that
//! found it mid-write) or carries an unknown schema is ignored at debug and
//! retried on the next tick.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fauna_protocol::log_plane::{ReportLogEventsRequest, SidecarLogEvent};
use fauna_provisioning::update_channel::report::{REPORT_PATH, Report};

use crate::log_plane::{self, LogSource};
use crate::routes::AppState;

/// How often the report is read — a stat of one small file, polled rather than
/// watched (an inotify crate would be a dependency add for nothing).
pub const REPORT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// What the reader has landed so far.
#[derive(Debug, Default)]
pub struct Reader {
    /// The highest `seq` landed.
    mark: u64,
    /// The file's `dropped` when last read.
    dropped_seen: u64,
}

impl Reader {
    /// The batch a read of `bytes` lands: the events above the mark and the
    /// file's `dropped` less the last count seen (the whole count when it went
    /// down — a fresh ledger). `None` when there is nothing to land, or the
    /// file is torn or of another schema.
    pub fn read(&mut self, bytes: &[u8]) -> Option<ReportLogEventsRequest> {
        let Some(report) = Report::parse(bytes) else {
            tracing::debug!("updater report unreadable or of another schema; retried next tick");
            return None;
        };
        let dropped = report
            .dropped
            .checked_sub(self.dropped_seen)
            .unwrap_or(report.dropped);
        self.dropped_seen = report.dropped;
        let mark = self.mark;
        let events: Vec<SidecarLogEvent> = report
            .events
            .into_iter()
            .filter(|e| e.seq > mark)
            .map(|e| {
                self.mark = self.mark.max(e.seq);
                SidecarLogEvent {
                    timestamp_ms: e.timestamp_ms,
                    level: e.level,
                    event: e.event,
                    message: e.message,
                    extra: Default::default(),
                }
            })
            .collect();
        (!events.is_empty() || dropped > 0).then(|| ReportLogEventsRequest {
            events,
            dropped,
            extra: Default::default(),
        })
    }
}

/// Where the report is read: [`REPORT_PATH`]. An e2e build (`test-hooks`) reads
/// the file `FAUNA_E2E_UPDATER_REPORT_PATH` names instead — a tier_3 nest runs
/// outside any container — and, without one, the real path.
pub fn report_path() -> PathBuf {
    #[cfg(feature = "test-hooks")]
    if let Some(path) = std::env::var_os("FAUNA_E2E_UPDATER_REPORT_PATH") {
        return PathBuf::from(path);
    }
    PathBuf::from(REPORT_PATH)
}

/// One tick: read the file, land what is new. An absent file — no updater, an
/// updater that has reported nothing yet, a nest outside a container — is
/// nothing to land.
pub async fn read_once(reader: &mut Reader, path: &std::path::Path) {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return;
    };
    if let Some(batch) = reader.read(&bytes) {
        log_plane::admit(LogSource::Updater, &batch);
    }
}

/// The reader task, scoped to the serving generation.
pub fn spawn_updater_report_reader(state: Arc<AppState>) {
    let path = report_path();
    let reader = Arc::new(tokio::sync::Mutex::new(Reader::default()));
    state.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        REPORT_POLL_INTERVAL,
        false,
        move || {
            let reader = reader.clone();
            let path = path.clone();
            async move { read_once(&mut *reader.lock().await, &path).await }
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(seqs: &[u64], dropped: u64) -> Vec<u8> {
        let mut r = Report {
            dropped,
            ..Default::default()
        };
        for &seq in seqs {
            r.events
                .push(fauna_provisioning::update_channel::report::ReportEvent {
                    seq,
                    timestamp_ms: seq,
                    level: "warn".into(),
                    event: "decision.hold".into(),
                    message: format!("event {seq}"),
                });
        }
        r.to_bytes()
    }

    fn seqs(batch: &ReportLogEventsRequest) -> Vec<u64> {
        batch.events.iter().map(|e| e.timestamp_ms).collect()
    }

    #[test]
    fn only_the_events_above_the_high_water_mark_land() {
        let mut reader = Reader::default();
        let first = reader.read(&report(&[1, 2, 3], 0)).unwrap();
        assert_eq!(seqs(&first), [1, 2, 3]);
        assert_eq!(reader.read(&report(&[1, 2, 3], 0)), None);
        let next = reader.read(&report(&[2, 3, 4, 5], 0)).unwrap();
        assert_eq!(seqs(&next), [4, 5]);
    }

    #[test]
    fn a_start_lands_the_retained_history_again() {
        let file = report(&[7, 8], 0);
        assert_eq!(seqs(&Reader::default().read(&file).unwrap()), [7, 8]);
        // A restarted nest has a fresh reader: the same file lands again.
        assert_eq!(seqs(&Reader::default().read(&file).unwrap()), [7, 8]);
    }

    #[test]
    fn dropped_is_the_delta_of_the_files_count() {
        let mut reader = Reader::default();
        assert_eq!(reader.read(&report(&[1], 3)).unwrap().dropped, 3);
        let batch = reader.read(&report(&[1, 2], 5)).unwrap();
        assert_eq!((seqs(&batch), batch.dropped), (vec![2], 2));
        // A count that went down is a fresh ledger: all of it is new.
        assert_eq!(reader.read(&report(&[2], 1)).unwrap().dropped, 1);
    }

    #[test]
    fn a_torn_file_or_an_unknown_schema_lands_nothing_and_moves_no_mark() {
        let mut reader = Reader::default();
        let file = report(&[1, 2], 0);
        assert_eq!(reader.read(&file[..file.len() - 3]), None);
        let other = serde_json::json!({"schema": 99, "dropped": 0, "events": []});
        assert_eq!(reader.read(other.to_string().as_bytes()), None);
        assert_eq!(seqs(&reader.read(&file).unwrap()), [1, 2]);
    }

    #[test]
    fn an_unknown_event_code_is_passed_through_as_it_is() {
        let mut r = Report::default();
        r.push(
            1,
            "info",
            "apply.verify_refused",
            "a newer updater's event".into(),
        );
        let batch = Reader::default().read(&r.to_bytes()).unwrap();
        assert_eq!(batch.events[0].event, "apply.verify_refused");
    }
}
