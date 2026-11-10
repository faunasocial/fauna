//! Which launch is the current one — the linux twin of tui's `launch::deliver`
//! / `launch::retire_in_flight` (`onboarding.md` § App-launch routing → *A
//! verdict renders only while its own launch is still the current one*;
//! multiple-accounts outcome 17).
//!
//! A launch verdict is decided from a store read taken when the launch started
//! and rendered a round trip later. Every asynchronous launch drive (the cold
//! start, Retry's fresh machine, the account switcher's re-launch, the Trust
//! button's re-challenge) [`begin`]s its machine here before it awaits, and its
//! GTK-thread callback asks [`deliver`] before rendering anything. A newer
//! launch replaces the held machine; the e2e session patch, which builds a
//! session without a launch, clears it ([`retire_in_flight`]). A verdict from a
//! machine that is no longer held is dropped whole.
//!
//! It is an OWNERSHIP test, never an "is a session present?" one: the held
//! launch's verdict still routes in full over a live session — the
//! `IdentityChanged` teardown depends on it.
//!
//! The held machine is the IN-FLIGHT one only: [`deliver`] takes it when the
//! verdict routes, so a settled launch is never retired (its machine lives on
//! as the session's bearer source, held by the shell, not here).

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use fauna_launch_machine::LaunchMachine;

thread_local! {
    /// The launch whose verdict the app is waiting for. GTK-thread only: every
    /// caller is a GTK callback or the test agent's command handler. Weak, so a
    /// launch window torn down without a verdict never keeps its machine alive.
    static IN_FLIGHT: RefCell<Option<Weak<LaunchMachine>>> = const { RefCell::new(None) };
}

/// The two counters `fauna_e2e_agent::LAUNCH_VERDICTS_KEY` publishes.
static ROUTED: AtomicU64 = AtomicU64::new(0);
static SUPERSEDED: AtomicU64 = AtomicU64::new(0);

/// `machine` is now the launch in flight: any earlier one still waiting is
/// superseded, and its verdict will be dropped when it lands.
pub fn begin(machine: &Arc<LaunchMachine>) {
    IN_FLIGHT.with(|held| *held.borrow_mut() = Some(Arc::downgrade(machine)));
}

/// May `machine`'s delivered verdict be rendered? `true` only while it is still
/// the launch in flight — which it then stops being (the verdict settles it).
/// Counts the verdict either way, before anything is rendered or dropped.
pub fn deliver(machine: &Arc<LaunchMachine>) -> bool {
    let current = IN_FLIGHT.with(|held| {
        let mut held = held.borrow_mut();
        let ours = held
            .as_ref()
            .is_some_and(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(machine)));
        if ours {
            *held = None;
        }
        ours
    });
    if current {
        ROUTED.fetch_add(1, Ordering::SeqCst);
    } else {
        SUPERSEDED.fetch_add(1, Ordering::SeqCst);
        tracing::info!("[launch] verdict superseded before it rendered — dropping it");
    }
    current
}

/// Retire a launch still in flight because a newer session has just been built
/// without it (the e2e session patch). A settled launch holds nothing here, so
/// this is a no-op for it.
pub fn retire_in_flight() {
    IN_FLIGHT.with(|held| {
        if held.borrow_mut().take().is_some() {
            tracing::info!("[launch] a newer session retired the launch still in flight");
        }
    });
}

/// `fauna_e2e_agent::LAUNCH_VERDICTS_KEY`'s value.
pub fn verdicts_json() -> serde_json::Value {
    fauna_e2e_agent::launch_verdicts_json(
        ROUTED.load(Ordering::SeqCst),
        SUPERSEDED.load(Ordering::SeqCst),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Arc<LaunchMachine> {
        LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
        )
    }

    fn counts() -> (u64, u64) {
        (
            ROUTED.load(Ordering::SeqCst),
            SUPERSEDED.load(Ordering::SeqCst),
        )
    }

    /// Red-verify by making `deliver` return `true` unconditionally: the
    /// replaced and the retired launch's verdicts both read as current.
    #[test]
    fn a_replaced_or_retired_launch_is_dropped_and_the_held_one_routes_once() {
        // The counters are process-wide; every assert below is on a delta.
        let (r0, s0) = counts();

        let stale = machine();
        begin(&stale);
        let newer = machine();
        begin(&newer);
        assert!(!deliver(&stale), "a replaced launch's verdict is dropped");
        assert!(deliver(&newer), "the held launch's verdict routes");
        assert!(!deliver(&newer), "a settled launch is held no longer");

        let retired = machine();
        begin(&retired);
        retire_in_flight();
        assert!(!deliver(&retired), "a retired launch's verdict is dropped");

        let (r1, s1) = counts();
        assert_eq!((r1 - r0, s1 - s0), (1, 3));
    }
}
