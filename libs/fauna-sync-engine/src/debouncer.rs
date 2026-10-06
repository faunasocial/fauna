//! Timer-based event debouncing for filesystem events.
//!
//! Aggregates rapid events (e.g., VS Code saves) into a single action
//! after a configurable quiet period.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Debounces events by path. An event is "ready" only after `delay` has
/// passed since the last `touch()` for that path.
pub struct EventDebouncer {
    delay: Duration,
    pending: Mutex<HashMap<String, Instant>>,
}

impl EventDebouncer {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Record an event for the given path. Resets the timer if already pending.
    pub fn touch(&self, path: &str) {
        self.pending
            .lock()
            .unwrap()
            .insert(path.to_string(), Instant::now());
    }

    /// Returns true if `path` has been pending longer than the delay.
    pub fn is_ready(&self, path: &str) -> bool {
        let pending = self.pending.lock().unwrap();
        match pending.get(path) {
            Some(last) => last.elapsed() >= self.delay,
            None => false,
        }
    }

    /// Drain all paths whose timers have expired. Returns the paths.
    pub fn drain_ready(&self) -> Vec<String> {
        let mut pending = self.pending.lock().unwrap();
        let ready: Vec<String> = pending
            .iter()
            .filter(|(_, last)| last.elapsed() >= self.delay)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &ready {
            pending.remove(path);
        }
        ready
    }

    /// Remove a path from pending (e.g., on delete event).
    pub fn cancel(&self, path: &str) {
        self.pending.lock().unwrap().remove(path);
    }

    /// Duration until the next pending path becomes ready. Returns None if empty.
    pub fn next_deadline(&self) -> Option<Duration> {
        let pending = self.pending.lock().unwrap();
        pending
            .values()
            .map(|last| {
                let elapsed = last.elapsed();
                if elapsed >= self.delay {
                    Duration::ZERO
                } else {
                    self.delay - elapsed
                }
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn debouncer_touch_and_drain() {
        let debouncer = EventDebouncer::new(Duration::from_millis(50));
        debouncer.touch("a.txt");
        debouncer.touch("b.txt");

        // Not ready yet
        assert!(debouncer.drain_ready().is_empty());

        // Wait for delay
        std::thread::sleep(Duration::from_millis(60));

        let ready = debouncer.drain_ready();
        assert_eq!(ready.len(), 2);
        assert!(ready.contains(&"a.txt".to_string()));
        assert!(ready.contains(&"b.txt".to_string()));

        // Drained — no more ready
        assert!(debouncer.drain_ready().is_empty());
    }

    #[test]
    fn debouncer_cancel_removes_pending() {
        let debouncer = EventDebouncer::new(Duration::from_millis(50));
        debouncer.touch("a.txt");
        debouncer.cancel("a.txt");

        std::thread::sleep(Duration::from_millis(60));
        assert!(debouncer.drain_ready().is_empty());
    }

    #[test]
    fn debouncer_touch_resets_timer() {
        let debouncer = EventDebouncer::new(Duration::from_millis(100));
        debouncer.touch("a.txt");

        std::thread::sleep(Duration::from_millis(60));
        // Re-touch to reset timer
        debouncer.touch("a.txt");

        std::thread::sleep(Duration::from_millis(60));
        // Should not be ready yet (only 60ms since last touch)
        assert!(debouncer.drain_ready().is_empty());

        std::thread::sleep(Duration::from_millis(50));
        // Now it should be ready
        let ready = debouncer.drain_ready();
        assert_eq!(ready.len(), 1);
    }
}
