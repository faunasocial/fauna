//! What ends a slow background loop's wait early — the e2e seam behind the
//! agent's `run_now`-style pokes (convention 14), so a witness drives the
//! production loop's own next pass instead of waiting out its clock.
//!
//! tui and linux each hand-copied this type (found by the dev-fleet
//! near-duplicate-function scanner); it lives here once. The apps' loops that
//! carry one: the critical-alert re-sweep and the certificate auto-renew tick.
//!
//! Empty in a production build (convention 15: the automation surface is
//! compiled out), so a loop there runs on its clock alone. The seam is live
//! under `debug_assertions` or this crate's `e2e-agent` feature, which both
//! apps forward from their own. One per loop instance: the owner mints a fresh
//! one each time it spawns the loop, so a departed session's loop — which
//! lingers until its next wake — holds a handle nothing fires any more.

/// A poke that ends one loop's current wait. See the module docs.
#[derive(Clone, Default)]
pub struct LoopWake {
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl LoopWake {
    /// End the loop's current wait now. A wake that lands while a pass is
    /// running is kept (`notify_one` stores the permit) and ends the very next
    /// wait, so a test never loses its poke to a busy loop.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// The underlying `Notify`, for a loop that takes its wake as a closure
    /// (`run_alert_sweep_loop_wakeable`) rather than calling [`Self::sleep`].
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub fn notify(&self) -> std::sync::Arc<tokio::sync::Notify> {
        std::sync::Arc::clone(&self.notify)
    }

    /// Sleep `period` — or, in a test-capable build, until [`Self::wake`] ends
    /// the wait first. The loop's body is the production one either way; only
    /// its wait is raceable.
    pub async fn sleep(&self, period: std::time::Duration) {
        #[cfg(any(debug_assertions, feature = "e2e-agent"))]
        tokio::select! {
            () = tokio::time::sleep(period) => {}
            () = self.notify.notified() => {}
        }
        #[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
        tokio::time::sleep(period).await;
    }

    /// Test-only: whether a wake is pending for this handle's loop, consuming
    /// it. A zero timeout polls the `Notified` once before its deadline, so a
    /// stored permit answers `true` without any wall clock.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub async fn take_pending_wake(&self) -> bool {
        tokio::time::timeout(std::time::Duration::ZERO, self.notify.notified())
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wake ends the wait long before its clock would, and a wake that lands
    /// before the wait begins is kept for it.
    #[tokio::test]
    async fn a_wake_ends_the_wait_and_an_early_one_is_kept() {
        let wake = LoopWake::default();
        let six_hours = std::time::Duration::from_secs(6 * 60 * 60);
        wake.wake();
        tokio::time::timeout(std::time::Duration::from_secs(5), wake.sleep(six_hours))
            .await
            .expect("a stored wake ends the next wait");
        let waiter = wake.clone();
        let task = tokio::spawn(async move { waiter.sleep(six_hours).await });
        tokio::task::yield_now().await;
        wake.wake();
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("a wake mid-wait ends it")
            .expect("task");
    }
}
