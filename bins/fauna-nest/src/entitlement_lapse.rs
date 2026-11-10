//! The ONE entitlement-lapse sweep: the time-driven end of a paid window.
//!
//! A paid window (`subscribers.valid_until`) expires silently — no event fires
//! when the clock passes it — so, exactly like the trend re-decay
//! (`trend_sweeper`), it needs a periodic reconcile. Every hour this worker
//! runs two halves, and its first tick doubles as the at-boot reconcile:
//!
//! 1. **The membership reconcile** — every expired member still sitting at their
//!    membership `admin_tier` flips down to the link's `lapse_tier`
//!    (monetization.md § Pillar 4 Rail C step 3 — a reversible quota downgrade,
//!    never suspension, never data loss).
//! 2. **The content-tier removal** — every content-tier roster row whose window
//!    ended more than [`PAID_WINDOW_LAPSE_GRACE_SECS`] ago gets the leave's own
//!    `kind='unsubscribe'` request row, the roster row's home copied onto it, so
//!    the author's drain rotates the buyer out and a remote buyer's nest reads
//!    `removed` (monetization.md § Pillar 3 → *Expiry & revocation*, the
//!    2026-11-09 ruling). The gates refuse from `valid_until` itself meanwhile;
//!    the grace only delays the key rotation, so a renewal arriving inside it
//!    re-stamps the standing row in place (`payment_core`).
//!
//! Renewals and refunds reconcile the affected buyer promptly at webhook
//! ingress (`payment_routes`, `payment_core::void_entitlement_window`); this
//! worker owns the time-driven lapse the ingress path can't see.

use std::sync::Arc;

use crate::db::now_epoch_secs;
use crate::routes::AppState;

/// Sweep cadence — hard-coded (bucket 1 of the one-configuration-surface
/// invariant: no human chooses it; monetization.md § Pillar 4 Rail C step 3,
/// "a periodic sweep whose cadence is a hard-coded constant"). Hourly: a lapsed
/// member's over-cap *new* writes are the only thing the downgrade gates (reads,
/// exports, and existing data stay fully available), and a lapsed content
/// window is already refused at the gates, so hour-granularity latency on
/// either half is immaterial.
const LAPSE_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// How long past its end a content-tier paid window stands before the sweep
/// queues the buyer's removal — hard-coded (bucket 1: a tolerance for the
/// payment provider's renewal latency and dunning retries, not a preference;
/// monetization.md § Pillar 3 → *Expiry & revocation*). Three days covers a
/// recurring charge's first retries without leaving a lapsed buyer holding the
/// period key for long.
pub const PAID_WINDOW_LAPSE_GRACE_SECS: i64 = 3 * 24 * 60 * 60;

/// What one sweep tick did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LapseSwept {
    /// Members the membership reconcile downgraded.
    pub downgraded: u64,
    /// Content-tier removals queued for the authors' drains.
    pub removals_queued: u64,
}

/// Run one tick of the sweep: both halves, in order, against the wall clock.
/// The periodic worker and the `test-hooks` poke run exactly this.
pub async fn sweep_once(state: &AppState) -> anyhow::Result<LapseSwept> {
    let downgraded = state.db.reconcile_lapsed_memberships(None).await?;
    let removals_queued = state
        .db
        .queue_lapsed_paid_window_removals(now_epoch_secs() - PAID_WINDOW_LAPSE_GRACE_SECS)
        .await?;
    Ok(LapseSwept {
        downgraded,
        removals_queued,
    })
}

/// Spawn the periodic entitlement-lapse sweep via the shared
/// [`crate::sweeper::spawn_periodic_sweeper`] primitive: a bare interval loop
/// whose first tick fires immediately (the boot reconcile — the second of the
/// three trigger points, the third being webhook ingress), torn down at
/// process exit.
pub fn spawn_entitlement_lapse_sweeper(state: Arc<AppState>) {
    let scope = state.clone();
    scope.scope_handle(crate::sweeper::spawn_periodic_sweeper(
        LAPSE_SWEEP_INTERVAL,
        false,
        move || {
            let state = state.clone();
            async move {
                match sweep_once(&state).await {
                    Ok(swept) => {
                        if swept.downgraded > 0 {
                            tracing::info!(
                                "entitlement lapse sweep: downgraded {} member(s)",
                                swept.downgraded
                            );
                        }
                        if swept.removals_queued > 0 {
                            tracing::info!(
                                "entitlement lapse sweep: queued {} lapsed subscriber removal(s)",
                                swept.removals_queued
                            );
                        }
                    }
                    Err(e) => tracing::error!("entitlement lapse sweep error: {e:#}"),
                }
            }
        },
    ));
}
