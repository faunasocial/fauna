//! The Progress step's elapsed time and estimate (`archive-import.md` § The
//! wizard and its machine, step 5: "elapsed + ETA").
//!
//! The clock measures the run's ACTIVE time as this app has watched it: it
//! runs while the run is `Running`, stops on a pause, cancel, error or
//! completion, and resumes with the run. It is process-local on purpose — a
//! restart-resume (possibly on another device) starts it afresh, and the
//! estimate's rate counts only the records this process watched settle, so a
//! resumed run never divides a fresh clock by records an earlier process
//! settled.

/// Microsecond instants in, whole seconds out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RunClock {
    /// Active time banked by earlier running stretches.
    active_micros: u64,
    /// When the current running stretch began; `None` while stopped.
    running_since: Option<u64>,
    /// Records settled (imported + skipped) when this clock first started.
    settled_at_start: Option<u64>,
}

impl RunClock {
    /// The run is (again) running at `now`, with `settled` records already
    /// settled. A clock already running keeps its stretch.
    pub(crate) fn start(&mut self, now: u64, settled: u64) {
        self.settled_at_start.get_or_insert(settled);
        self.running_since.get_or_insert(now);
    }

    /// The run stopped at `now` — the stretch is banked.
    pub(crate) fn stop(&mut self, now: u64) {
        if let Some(since) = self.running_since.take() {
            self.active_micros += now.saturating_sub(since);
        }
    }

    /// Whether a running stretch is open.
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.running_since.is_some()
    }

    /// A fresh run, or no run at all.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// `(elapsed_secs, eta_secs)` at `now`. The estimate is the watched rate
    /// applied to what is left; `None` until at least one record has settled
    /// under this clock, `Some(0)` once nothing is left.
    pub(crate) fn timing(&self, now: u64, settled: u64, total: u64) -> (u64, Option<u64>) {
        let elapsed = self.active_micros
            + self
                .running_since
                .map_or(0, |since| now.saturating_sub(since));
        let remaining = total.saturating_sub(settled);
        let watched = self
            .settled_at_start
            .map_or(0, |start| settled.saturating_sub(start));
        let eta = if remaining == 0 && self.settled_at_start.is_some() {
            Some(0)
        } else if watched == 0 {
            None
        } else {
            // u128: a long run's micros × a large remainder can pass u64.
            Some((elapsed as u128 * remaining as u128 / watched as u128) as u64)
        };
        (elapsed / 1_000_000, eta.map(|micros| micros / 1_000_000))
    }
}

#[cfg(test)]
mod tests {
    use super::RunClock;

    const S: u64 = 1_000_000;

    #[test]
    fn an_unstarted_clock_reads_zero_and_no_estimate() {
        assert_eq!(RunClock::default().timing(50 * S, 0, 10), (0, None));
    }

    #[test]
    fn the_estimate_is_the_watched_rate_over_what_is_left() {
        let mut c = RunClock::default();
        c.start(100 * S, 0);
        assert_eq!(
            c.timing(110 * S, 0, 10),
            (10, None),
            "no record settled yet: no estimate"
        );
        // 2 records in 20 s → 10 s each; 8 left → 80 s.
        assert_eq!(c.timing(120 * S, 2, 10), (20, Some(80)));
        assert_eq!(c.timing(200 * S, 10, 10), (100, Some(0)));
    }

    #[test]
    fn a_pause_stops_the_clock_and_a_resume_continues_it() {
        let mut c = RunClock::default();
        c.start(0, 0);
        c.stop(30 * S);
        // Paused for an hour: elapsed stays at the active 30 s.
        assert_eq!(c.timing(3_630 * S, 3, 6), (30, Some(30)));
        c.start(3_630 * S, 3);
        assert_eq!(c.timing(3_660 * S, 6, 6), (60, Some(0)));
    }

    #[test]
    fn a_resumed_process_rates_only_what_it_watched() {
        // A restart-resume: 50 records settled by an earlier process.
        let mut c = RunClock::default();
        c.start(0, 50);
        assert_eq!(c.timing(5 * S, 50, 100), (5, None));
        // 5 watched in 10 s → 2 s each; 45 left → 90 s.
        assert_eq!(c.timing(10 * S, 55, 100), (10, Some(90)));
    }

    #[test]
    fn reset_forgets_the_run() {
        let mut c = RunClock::default();
        c.start(0, 0);
        c.stop(10 * S);
        c.reset();
        assert_eq!(c, RunClock::default());
    }
}
