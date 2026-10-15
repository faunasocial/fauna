//! Elapsed time and time remaining for the two mail-portability wizards — the
//! "elapsed + ETA" of `mail-export.md` § Wizard steps step 4 and
//! `mailbox-migration.md` § Wizard steps step 5, and the "time elapsed" both
//! Done steps report (`mail-export.md` step 5, `mailbox-migration.md` step 6).
//!
//! One computation for both wizards and all seven apps (priority #2): each app
//! renders the two numbers through `fauna_core::format::duration_secs` and its
//! own summary template, and never re-derives either from the session's
//! timestamps.
//!
//! Both clocks are the nest's: `started_at` and `last_progress_at` are the
//! session row's epoch seconds (`import_sessions` / `export_sessions`), so a
//! second device, or the same app after a restart, reports the same elapsed
//! time as the one that started the run. Only a RUNNING session reads the
//! client's own clock, because its elapsed time is still growing.

use serde::{Deserialize, Serialize};

/// What a progress or done summary says about time.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TransferTiming {
    /// Seconds since the session started. `None` when no session is known.
    pub elapsed_secs: Option<u64>,
    /// The estimated seconds left, extrapolated from the rate so far. `None`
    /// while nothing has been processed (there is no rate yet), once nothing
    /// is left, and for any session that is not running — a paused, finished
    /// or failed run is not getting closer to done.
    pub remaining_secs: Option<u64>,
}

/// The [`TransferTiming`] of one session.
///
/// - `started_at` / `last_progress_at`: the session row's epoch seconds; a
///   `started_at` of `0` or less means no session, and answers nothing.
/// - `running`: the session's state is `running`. A running session's
///   elapsed time runs to `now_secs`; any other state's stops at
///   `last_progress_at`, the row's stamp of its last transition (pause,
///   completion, cancel, failure).
/// - `processed` / `total`: every message the run has dealt with (imported or
///   exported, skipped and failed alike — each consumed its share of the
///   time) against the session's total.
///
/// A clock that runs backwards answers `0`, never an underflow.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn transfer_timing(
    started_at: i64,
    last_progress_at: i64,
    running: bool,
    processed: u64,
    total: u64,
    now_secs: i64,
) -> TransferTiming {
    if started_at <= 0 {
        return TransferTiming::default();
    }
    let end = if running { now_secs } else { last_progress_at };
    let elapsed = u64::try_from(end.saturating_sub(started_at)).unwrap_or(0);
    let remaining = (running && processed > 0 && total > processed).then(|| {
        // u128 so a long import's elapsed × remaining-count cannot overflow.
        let left = u128::from(elapsed) * u128::from(total - processed) / u128::from(processed);
        u64::try_from(left).unwrap_or(u64::MAX)
    });
    TransferTiming {
        elapsed_secs: Some(elapsed),
        remaining_secs: remaining,
    }
}

/// The rate the export confirm step's wall-clock estimate assumes, in sealed
/// bytes per second (`mail-export.md` § Wizard steps step 3). **An estimate,
/// not a measurement**: each byte crosses the wire down as a sealed record, is
/// opened and re-serialized on the client, and crosses back up as a sealed
/// chunk, so the real rate follows the link and the device. 1 MiB/s is a
/// deliberately unhurried round trip, so the figure tends to over-promise the
/// wait rather than the speed; once the run starts, the progress step's
/// estimate ([`transfer_timing`]) replaces it with the rate actually seen.
pub const EXPORT_ESTIMATE_BYTES_PER_SEC: u64 = 1024 * 1024;

/// The export confirm step's estimated wall-clock seconds for `sealed_bytes`
/// at [`EXPORT_ESTIMATE_BYTES_PER_SEC`], rounded up — any non-empty export
/// takes at least a second. One computation for all seven apps (priority #2):
/// each renders it through `fauna_core::format::duration_secs`.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn export_time_estimate_secs(sealed_bytes: u64) -> u64 {
    sealed_bytes.div_ceil(EXPORT_ESTIMATE_BYTES_PER_SEC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_export_estimate_is_bytes_over_the_named_rate_rounded_up() {
        assert_eq!(export_time_estimate_secs(0), 0);
        assert_eq!(export_time_estimate_secs(1), 1);
        assert_eq!(export_time_estimate_secs(EXPORT_ESTIMATE_BYTES_PER_SEC), 1);
        // 3 GiB at 1 MiB/s is 3 072 s, about 51 minutes.
        assert_eq!(export_time_estimate_secs(3 << 30), 3_072);
        assert_eq!(
            export_time_estimate_secs(u64::MAX),
            u64::MAX / EXPORT_ESTIMATE_BYTES_PER_SEC + 1
        );
    }

    #[test]
    fn no_session_answers_nothing() {
        assert_eq!(
            transfer_timing(0, 0, true, 5, 10, 1_000),
            TransferTiming::default()
        );
    }

    #[test]
    fn a_running_session_counts_to_now_and_extrapolates_the_rest() {
        // 25 of 100 in 60 s → 75 left at the same rate is 180 s.
        let t = transfer_timing(1_000, 1_030, true, 25, 100, 1_060);
        assert_eq!(t.elapsed_secs, Some(60));
        assert_eq!(t.remaining_secs, Some(180));
    }

    #[test]
    fn nothing_processed_yet_has_no_estimate() {
        let t = transfer_timing(1_000, 1_000, true, 0, 100, 1_010);
        assert_eq!(t.elapsed_secs, Some(10));
        assert_eq!(t.remaining_secs, None);
    }

    #[test]
    fn a_stopped_session_freezes_at_its_last_transition() {
        // Finished at 1_090; read an hour later it still says 90 s.
        let t = transfer_timing(1_000, 1_090, false, 100, 100, 4_600);
        assert_eq!(t.elapsed_secs, Some(90));
        assert_eq!(t.remaining_secs, None);
        // Paused part-way: no estimate either — it is not moving.
        let t = transfer_timing(1_000, 1_040, false, 40, 100, 4_600);
        assert_eq!(t.elapsed_secs, Some(40));
        assert_eq!(t.remaining_secs, None);
    }

    #[test]
    fn a_backwards_clock_reads_zero() {
        let t = transfer_timing(1_000, 900, true, 1, 2, 900);
        assert_eq!(t.elapsed_secs, Some(0));
        assert_eq!(t.remaining_secs, Some(0));
    }
}
