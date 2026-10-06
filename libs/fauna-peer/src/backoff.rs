use std::collections::VecDeque;
use std::time::Duration;

/// Maximum number of outcomes tracked in the rolling window.
const WINDOW_SIZE: usize = 20;

/// Returns the probe interval for a given backoff level.
///
/// - 0 (active): 30 seconds
/// - 1 (warm): 5 minutes
/// - 2 (cool): 30 minutes
/// - 3 (cold): 4 hours
/// - 4+ (dormant): Duration::MAX (effectively never probe)
pub fn probe_interval(level: u8) -> Duration {
    match level {
        0 => Duration::from_secs(30),
        1 => Duration::from_secs(300),
        2 => Duration::from_secs(1800),
        3 => Duration::from_secs(14400),
        _ => Duration::MAX,
    }
}

/// Maps a success rate (0.0 to 1.0) to a backoff level.
///
/// - > 0.80 -> 0 (active)
/// - 0.40 to 0.80 -> 1 (warm)
/// - 0.10 to 0.40 -> 2 (cool)
/// - < 0.10 -> 3 (cold)
pub fn compute_backoff_level(success_rate: f32) -> u8 {
    if success_rate > 0.80 {
        0
    } else if success_rate >= 0.40 {
        1
    } else if success_rate >= 0.10 {
        2
    } else {
        3
    }
}

/// Tracks rolling success/failure outcomes and computes adaptive backoff.
pub struct BackoffState {
    /// Rolling window of last 20 probe outcomes (true = success).
    pub outcomes: VecDeque<bool>,
    /// Current backoff level (0=active, 1=warm, 2=cool, 3=cold, 4=dormant).
    pub backoff_level: u8,
}

impl BackoffState {
    /// Creates a new empty backoff state at level 0.
    pub fn new() -> Self {
        Self {
            outcomes: VecDeque::with_capacity(WINDOW_SIZE),
            backoff_level: 0,
        }
    }

    /// Records a successful probe. Pushes `true` into the rolling window
    /// and unconditionally resets the backoff level to 0.
    pub fn record_success(&mut self) {
        self.push_outcome(true);
        self.backoff_level = 0;
    }

    /// Records a failed probe. Pushes `false` into the rolling window
    /// and recomputes the backoff level from the current success rate.
    pub fn record_failure(&mut self) {
        self.push_outcome(false);
        self.backoff_level = compute_backoff_level(self.success_rate());
    }

    /// Returns the fraction of successes in the rolling window.
    /// Returns 0.0 if the window is empty.
    pub fn success_rate(&self) -> f32 {
        if self.outcomes.is_empty() {
            return 0.0;
        }
        let successes = self.outcomes.iter().filter(|&&ok| ok).count();
        successes as f32 / self.outcomes.len() as f32
    }

    fn push_outcome(&mut self, outcome: bool) {
        if self.outcomes.len() >= WINDOW_SIZE {
            self.outcomes.pop_front();
        }
        self.outcomes.push_back(outcome);
    }
}

impl Default for BackoffState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn initial_interval_is_30s() {
        assert_eq!(probe_interval(0), Duration::from_secs(30));
    }

    #[test]
    fn backoff_levels() {
        assert_eq!(probe_interval(0), Duration::from_secs(30));
        assert_eq!(probe_interval(1), Duration::from_secs(300));
        assert_eq!(probe_interval(2), Duration::from_secs(1800));
        assert_eq!(probe_interval(3), Duration::from_secs(14400));
        assert_eq!(probe_interval(4), Duration::MAX); // dormant
    }

    #[test]
    fn success_resets_to_level_0() {
        let mut state = BackoffState::new();
        state.record_failure();
        state.record_failure();
        assert!(state.backoff_level > 0);
        state.record_success();
        assert_eq!(state.backoff_level, 0);
    }

    #[test]
    fn success_rate_rolling_window() {
        let mut state = BackoffState::new();
        // 15 successes, 5 failures = 75% success rate
        for _ in 0..15 {
            state.record_success();
        }
        for _ in 0..5 {
            state.record_failure();
        }
        assert!((state.success_rate() - 0.75).abs() < 0.01);
    }

    #[test]
    fn backoff_level_from_success_rate() {
        assert_eq!(compute_backoff_level(0.85), 0); // active
        assert_eq!(compute_backoff_level(0.50), 1); // warm
        assert_eq!(compute_backoff_level(0.25), 2); // cool
        assert_eq!(compute_backoff_level(0.05), 3); // cold
    }
}
