//! Background sweep that re-decays live `trending` rows on wall-clock.
//!
//! The `trending` factor is a *decayed* velocity, so its value drifts with time
//! even when no new engagement lands — unlike the transition-driven `engagement`
//! scalar, it needs a periodic recompute. Every ~15 min this worker re-scores
//! every live trend row against the current clock and withdraws those fallen to
//! 0‰ (`trending.md` § Local velocity: "a periodic sweep that re-decays live
//! rows and withdraws rows fallen to 0‰"). New acts *between* sweeps are handled
//! promptly by the transition hook in `db::engagement::insert` /
//! `delete_engagement_event`; this worker owns only the wall-clock decay.

use std::sync::Arc;

use crate::routes::AppState;

/// Sweep cadence — hard-coded (bucket 1: no human chooses it; `trending.md`
/// § Local velocity, ~15 min). Far below the 6 h decay half-life, so a trend
/// re-decays smoothly rather than in visible steps.
const TREND_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Spawn the periodic trend re-decay sweep via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive: a bare interval loop
/// whose first tick fires immediately, torn down when the process exits.
pub fn spawn_trend_sweeper(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        TREND_SWEEP_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                let now_us = fauna_core::data::Timestamp::now().as_i64();
                match state.db.sweep_trend_scores(now_us).await {
                    Ok(n) if n > 0 => tracing::debug!("trend sweep: re-decayed {n} live rows"),
                    Ok(_) => {}
                    Err(e) => tracing::error!("trend sweep error: {e}"),
                }
            }
        },
    ));
}
