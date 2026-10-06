//! A generic burst-absorbing wait: coalesce several rapid [`tokio::sync::Notify`]
//! pulses into one pass instead of one per pulse.
//!
//! `fauna-client-index` and `fauna-sync-engine` each hand-rolled this exact
//! loop identically (found by the dev-fleet near-duplicate-function
//! scanner's cross-crate pass, 0.677 similarity) — the debounce shape
//! carries no domain logic, only a caller-supplied window.

use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

/// Wait up to `debounce` from the moment this is called, or until `cancel`
/// fires. A pulse arriving before the deadline keeps the wait going rather
/// than returning immediately — but the deadline itself is fixed at entry
/// and does not move: this bounds the return to within `debounce` of being
/// called whether or not pulses keep arriving, it does not wait out a full
/// `debounce` of *quiet*. (Ported unchanged from the two hand-duplicated
/// copies this replaces — preserving exact behavior, not re-deriving intent
/// from the "until … passes without one" phrasing either copy's own caller
/// used to describe it.)
pub async fn absorb_burst(notify: &Notify, cancel: &CancellationToken, debounce: Duration) {
    let deadline = Instant::now() + debounce;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            res = timeout(wait, notify.notified()) => {
                if res.is_err() {
                    break;
                }
                // A pulse arrived inside the window — extend it.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn returns_once_the_window_elapses_with_no_pulse() {
        let notify = Notify::new();
        let cancel = CancellationToken::new();
        let start = Instant::now();
        absorb_burst(&notify, &cancel, Duration::from_millis(50)).await;
        assert!(Instant::now() - start >= Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn a_pulse_inside_the_window_does_not_return_early() {
        let notify = std::sync::Arc::new(Notify::new());
        let cancel = CancellationToken::new();
        let debounce = Duration::from_millis(50);

        // Pulse once, 30ms in — well inside the window.
        tokio::spawn({
            let notify = notify.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                notify.notify_one();
            }
        });

        let start = Instant::now();
        absorb_burst(&notify, &cancel, debounce).await;
        let elapsed = Instant::now() - start;

        // The pulse re-arms a wait for whatever is left of the SAME fixed
        // deadline (see the fn doc) — it must not cause an early return, and
        // since the deadline never moves it must not push the return past
        // the original window either.
        assert!(
            elapsed >= debounce,
            "elapsed {elapsed:?} returned before the window closed"
        );
        assert!(
            elapsed < debounce + Duration::from_millis(20),
            "elapsed {elapsed:?} unexpectedly extended past the fixed deadline"
        );
    }

    #[tokio::test]
    async fn cancellation_returns_immediately() {
        let notify = Notify::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let start = Instant::now();
        absorb_burst(&notify, &cancel, Duration::from_secs(60)).await;
        assert!(Instant::now() - start < Duration::from_millis(50));
    }
}
