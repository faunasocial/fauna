//! Background sweep that downgrades lapsed memberships on wall-clock.
//!
//! A membership's paid window (`subscribers.valid_until`) expires silently — no
//! event fires when the clock passes it — so, exactly like the trend re-decay
//! (`trend_sweeper`), it needs a periodic reconcile: every hour this worker
//! flips every expired member still sitting at their membership `admin_tier`
//! down to the link's `lapse_tier` (monetization.md § Pillar 4 Rail C step 3 — a
//! reversible quota downgrade, never suspension, never data loss). Renewals and
//! refunds reconcile the affected buyer promptly at webhook ingress
//! (`payment_routes`); this worker owns the time-driven lapse the ingress path
//! can't see, and its first tick doubles as the at-boot reconcile.

use std::sync::Arc;

use crate::routes::AppState;

/// Sweep cadence — hard-coded (bucket 1 of the one-configuration-surface
/// invariant: no human chooses it; monetization.md § Pillar 4 Rail C step 3,
/// "a periodic sweep whose cadence is a hard-coded constant"). Hourly: a lapsed
/// member's over-cap *new* writes are the only thing the downgrade gates (reads,
/// exports, and existing data stay fully available), so hour-granularity latency
/// on the downgrade is immaterial.
const LAPSE_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Spawn the periodic membership-lapse reconcile via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive: a bare interval loop
/// whose first tick fires immediately (the boot reconcile — the second of the
/// three trigger points, the third being webhook ingress), torn down at
/// process exit.
pub fn spawn_membership_lapse_sweeper(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        LAPSE_SWEEP_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                match state.db.reconcile_lapsed_memberships(None).await {
                    Ok(n) if n > 0 => {
                        tracing::info!("membership lapse sweep: downgraded {n} member(s)")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!("membership lapse sweep error: {e}"),
                }
            }
        },
    ));
}
