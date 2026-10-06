//! Shared scaffolding behind this crate's periodic sweeper tasks.
//!
//! Every sweeper is the same shape — build an interval, optionally skip the
//! immediate first tick, then await an opaque callback forever — with only the
//! callback body (what to sweep, cutoff computation, result logging) differing
//! per site. [`spawn_periodic_sweeper`] is that shared shape; each call site
//! stays doc-commented with its own cadence and skip-vs-no-skip rationale,
//! since those are load-bearing per site (a sweep whose first tick doubles as
//! an at-boot reconcile must NOT skip it; one that would sweep empty/cold
//! state at startup must).

use std::future::Future;
use std::time::Duration;

/// Spawns a tokio task that ticks every `interval` and awaits `tick_once` on
/// each tick, forever. When `skip_first_tick` is `true`, the immediate first
/// tick that `tokio::time::interval` otherwise fires right away is consumed
/// silently before the loop starts.
pub(crate) fn spawn_periodic_sweeper<F, Fut>(
    interval: Duration,
    skip_first_tick: bool,
    mut tick_once: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    // spawn-ok(returns-handle-for-scope): the primitive returns its handle; every boot-time caller adopts it via `AppState::scope_handle`
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        if skip_first_tick {
            ticker.tick().await; // skip immediate
        }
        loop {
            ticker.tick().await;
            tick_once().await;
        }
    })
}
