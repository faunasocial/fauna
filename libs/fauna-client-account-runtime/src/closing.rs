//! The **stores still closing**: every stop of an account runtime that
//! outlived its budget, kept running until it ends and counted until it does —
//! and the one wait an erase takes behind them (`apps/account-scoping.md`
//! § Erasure follows scope, the ⚠ *An OPEN store is an unerasable store* note).
//!
//! # Why a lapsed stop is handed over and never dropped
//!
//! A stop's budget bounds how long its CALLER waits — a sign-out returns, a
//! switch builds the next client — never how long the store stays open. Up to
//! 2026-11 a lapse dropped the stop's future and nothing else knew the store
//! was still closing: the windows barrier counted that teardown finished at
//! the lapse, the remove-account erase ran 3 ms later over the open store,
//! and windows refused it (`os error 32`). Measured `--app windows` on Windows,
//! 2026-11-04, in one full-file run out of five. The same drop sat in the remove-account's own retirement runtime.
//!
//! So a stop that lapses goes on in the background, here, and an erase waits
//! for this set to drain under its own named budget
//! ([`CLOSING_STORES_BUDGET`]) — erasing anyway, loudly, if that lapses too.
//! The caller's budget still holds; what changed is that the store's close
//! is a fact an erase can wait on rather than one it guesses.

use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

/// How long an erase waits for the stores still closing before erasing
/// anyway — a bound, not a clock: the set is empty on every healthy stop, and
/// a lapsed one is a store whose own stop already spent its budget
/// ([`crate::ACCOUNT_RUNTIME_STOP_BUDGET`]) draining a pass. The windows
/// barrier's own budget (`AccountRuntimeStops.StopBudget`, 30 s) is sized over
/// a teardown's leave-flush, that stop and this wait.
pub const CLOSING_STORES_BUDGET: Duration = Duration::from_secs(15);

/// A set of stops still closing their stores. Cheap to clone; every clone is
/// the same set. A process has one ([`closing_stores`]); tests build their own.
#[derive(Clone)]
pub struct ClosingStores(Arc<tokio::sync::watch::Sender<usize>>);

impl Default for ClosingStores {
    fn default() -> Self {
        Self::new()
    }
}

/// Counts one stop out of the set when dropped — at the stop's end, and at
/// its unwinding if it panics, so a crashed stop cannot hold every later
/// erase for the whole budget.
struct Counted(Arc<tokio::sync::watch::Sender<usize>>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

impl ClosingStores {
    /// An empty set.
    pub fn new() -> Self {
        Self(Arc::new(tokio::sync::watch::Sender::new(0)))
    }

    /// How many stops are still closing right now.
    pub fn len(&self) -> usize {
        *self.0.borrow()
    }

    /// Is nothing closing?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Finish `rest` — the remainder of a stop that outlived its budget — in
    /// the background, counted in this set until it ends. Counted before this
    /// returns, so an erase that follows the lapse can never miss it.
    ///
    /// Requires a tokio context, as every native stop has.
    pub fn hand_over(&self, rest: impl Future<Output = ()> + Send + 'static) {
        self.0.send_modify(|n| *n += 1);
        let counted = Counted(Arc::clone(&self.0));
        tokio::spawn(async move {
            let _counted = counted;
            rest.await;
        });
    }

    /// Wait until no stop is still closing, or until `budget` lapses. `true`
    /// when nothing is left — the scope is erasable — and `false` on a lapse,
    /// which this reports itself. `erase` names the erase about to run, for
    /// the log lines. The caller erases either way.
    pub async fn closed(&self, budget: Duration, erase: &str) -> bool {
        let mut rx = self.0.subscribe();
        let waiting = *rx.borrow_and_update();
        if waiting == 0 {
            return true;
        }
        let started = Instant::now();
        match tokio::time::timeout(budget, rx.wait_for(|n| *n == 0)).await {
            Ok(_) => {
                tracing::info!(
                    "[account-runtime] the {erase} waited {} ms for {waiting} lapsed stop(s) to \
                     close their stores; the scope is erasable",
                    started.elapsed().as_millis()
                );
                true
            }
            Err(_) => {
                tracing::warn!(
                    "[account-runtime] {} lapsed stop(s) still closing their stores after {}s — \
                     the {erase} erases anyway; a still-open store fails its remove_dir_all on \
                     windows (os error 32)",
                    self.len(),
                    budget.as_secs()
                );
                false
            }
        }
    }
}

/// This process's stores still closing — the one set every native host's
/// stops hand a lapse to and every erase waits behind.
pub fn closing_stores() -> &'static ClosingStores {
    static SET: LazyLock<ClosingStores> = LazyLock::new(ClosingStores::new);
    &SET
}

/// Run `stop` for up to `budget`, and on a lapse hand the rest of it to
/// `closing` rather than drop it. `Some(output)` when it finished in time,
/// `None` on a lapse — after which `then_late` receives the output when it
/// finally comes, inside the handed-over remainder (the place for whatever
/// must still close what the stop produced late).
pub async fn within_or_hand_over<F, L, G>(
    closing: &ClosingStores,
    budget: Duration,
    stop: F,
    then_late: L,
) -> Option<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send,
    L: FnOnce(F::Output) -> G + Send + 'static,
    G: Future<Output = ()> + Send,
{
    let mut stop = Box::pin(stop);
    match tokio::time::timeout(budget, &mut stop).await {
        Ok(output) => Some(output),
        Err(_) => {
            closing.hand_over(async move {
                let output = stop.await;
                then_late(output).await;
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty set is closed at once — the common erase, where a wait would
    /// be pure latency.
    #[tokio::test]
    async fn an_empty_set_is_closed_at_once() {
        let closing = ClosingStores::new();
        assert!(closing.closed(Duration::from_secs(3600), "test").await);
    }

    /// A handed-over stop is counted until it ends, and the wait resolves
    /// exactly then. Causal, not timed (e2e convention 14): the stop ends only
    /// when the test releases it, and the budget is far beyond any machine.
    #[tokio::test]
    async fn a_handed_over_stop_holds_the_wait_until_it_ends() {
        let closing = ClosingStores::new();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        closing.hand_over(async move {
            let _ = released.await;
        });
        assert_eq!(closing.len(), 1, "counted before hand_over returns");

        let waiter = {
            let closing = closing.clone();
            tokio::spawn(async move { closing.closed(Duration::from_secs(3600), "test").await })
        };
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "an erase that goes on while the store still closes is the whole defect"
        );

        release.send(()).unwrap();
        assert!(waiter.await.unwrap(), "the store closed inside the budget");
        assert!(closing.is_empty());
    }

    /// The wait is bounded and says so: a stop that never ends cannot hold
    /// an erase forever. Latency-independent in the direction that matters —
    /// the stop is never released, so the wait can only time out.
    #[tokio::test]
    async fn the_wait_lapses_rather_than_holding_the_erase() {
        let closing = ClosingStores::new();
        let (_hold, held) = tokio::sync::oneshot::channel::<()>();
        closing.hand_over(async move {
            let _ = held.await;
        });
        assert!(!closing.closed(Duration::from_millis(10), "test").await);
        assert_eq!(closing.len(), 1, "a lapse leaves the stop counted");
    }

    /// A stop that panics has stopped holding anything; it must leave the set.
    #[tokio::test]
    async fn a_panicking_stop_leaves_the_set() {
        let closing = ClosingStores::new();
        closing.hand_over(async { panic!("a stop that crashed") });
        assert!(closing.closed(Duration::from_secs(3600), "test").await);
    }

    /// In time: the output comes back and nothing is handed over.
    #[tokio::test]
    async fn a_stop_in_time_hands_nothing_over() {
        let closing = ClosingStores::new();
        let out = within_or_hand_over(
            &closing,
            Duration::from_secs(3600),
            async { 7 },
            |_| async {},
        )
        .await;
        assert_eq!(out, Some(7));
        assert!(closing.is_empty());
    }

    /// Past the budget: the stop is NOT dropped — it goes on, counted, and
    /// the late output reaches `then_late` before the set drains.
    #[tokio::test]
    async fn a_lapsed_stop_goes_on_and_its_late_output_is_closed() {
        let closing = ClosingStores::new();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (late_tx, late_rx) = tokio::sync::oneshot::channel::<u32>();
        let out = within_or_hand_over(
            &closing,
            Duration::from_millis(10),
            async move {
                let _ = released.await;
                42u32
            },
            move |late| async move {
                let _ = late_tx.send(late);
            },
        )
        .await;
        assert_eq!(out, None, "the caller's budget still holds");
        assert_eq!(closing.len(), 1, "the rest of the stop is counted");

        release.send(()).unwrap();
        assert!(closing.closed(Duration::from_secs(3600), "test").await);
        assert_eq!(late_rx.await.unwrap(), 42, "the late output was closed");
    }
}
