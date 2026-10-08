//! A process-wide, test-only "interleave here" fence.
//!
//! Extracted from two byte-identical hand-copies —
//! `fauna_sync_engine::principal_succession::reauthor_window` and
//! `fauna_account_store::sqlite::pair_window` — whose own doc comments
//! already named each other as the precedent for the other, but never shared
//! the actual implementation. A test installs a closure that runs at a fixed
//! point inside production code (`ProcessHook::fire`), so an interleaving
//! property that would otherwise need real concurrency (a fence landing
//! *between* two specific steps) can be probed deterministically from a
//! single thread instead.
//!
//! The hook runs only for fires on the **installing thread**. libtest runs a
//! binary's tests on parallel threads, and every other test passing the same
//! seam fires it too; run from there, the hook staged its race at an
//! arbitrary point of the installer's own pass (a writer fence landing
//! mid-walk instead of after it — the one-in-forty red that pinned this).
//! Every probe is a current-thread `#[tokio::test]`, so the code under test
//! fires on the installer's thread by construction.
//!
//! `#[cfg(any(test, feature = "test-helpers"))]`-gated, same posture as
//! [`crate::authoritative_dns::spawn_responder`] and
//! [`crate::data::fixture_tier_period_keys`] — reaches a dev-dependency
//! consumer's own tests via `fauna-core = { ..., features = ["test-helpers"]
//! }`, never a release artifact (`e2e-conventions.md` convention 15).

#![cfg(any(test, feature = "test-helpers"))]

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;

/// A closure a test installs to run at one [`ProcessHook::fire`] site.
pub type Hook = Arc<dyn Fn() + Send + Sync>;

/// One process-wide hook slot. Declare one `static` per fence — each carries
/// its own independent slot and install-serialization lock, so two unrelated
/// fences (e.g. one per crate, or one per interleaving seam within a crate)
/// never contend with each other.
pub struct ProcessHook {
    /// The hook and the thread that installed it — the only thread it runs on.
    slot: OnceLock<Mutex<Option<(ThreadId, Hook)>>>,
    /// Serializes installers: the slot is process-wide, and every exerciser
    /// in the same test binary runs the clear on drop.
    install_lock: Mutex<()>,
}

impl ProcessHook {
    pub const fn new() -> Self {
        Self {
            slot: OnceLock::new(),
            install_lock: Mutex::new(()),
        }
    }

    fn slot(&self) -> &Mutex<Option<(ThreadId, Hook)>> {
        self.slot.get_or_init(|| Mutex::new(None))
    }

    /// Installed for as long as the returned value lives; clears on drop, so
    /// a panicking test cannot leave the hook armed for the next one. Takes
    /// `&'static self` — every call site is a `static ProcessHook`. Runs only
    /// for fires on the calling thread (module doc).
    pub fn install(&'static self, hook: Hook) -> Installed {
        let guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());
        *self.slot().lock().unwrap_or_else(|e| e.into_inner()) =
            Some((std::thread::current().id(), hook));
        Installed {
            owner: self,
            _guard: guard,
        }
    }

    /// A no-op off the installing thread. Cloned out of the lock before
    /// calling: a hook that touches the store must not be holding this mutex
    /// while it does.
    pub fn fire(&self) {
        let hook = match &*self.slot().lock().unwrap_or_else(|e| e.into_inner()) {
            Some((installer, hook)) if *installer == std::thread::current().id() => {
                Arc::clone(hook)
            }
            _ => return,
        };
        hook();
    }
}

impl Default for ProcessHook {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard returned by [`ProcessHook::install`] — clears the hook on drop.
pub struct Installed {
    owner: &'static ProcessHook,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for Installed {
    fn drop(&mut self) {
        *self.owner.slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static HOOK: ProcessHook = ProcessHook::new();

    #[test]
    fn fire_without_install_is_a_silent_no_op() {
        HOOK.fire();
    }

    #[test]
    fn installed_hook_fires_until_dropped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let installed = HOOK.install(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        HOOK.fire();
        HOOK.fire();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        drop(installed);
        HOOK.fire();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "fire after drop must not call the cleared hook"
        );
    }

    /// A fire from any thread but the installer's is another test's code
    /// passing the same seam — the libtest harness runs tests on parallel
    /// threads — and must not run this test's hook.
    #[test]
    fn a_fire_from_another_thread_does_not_run_the_hook() {
        static SCOPED: ProcessHook = ProcessHook::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let _installed = SCOPED.install(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        std::thread::spawn(|| SCOPED.fire()).join().unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "a foreign thread's fire ran the installer's hook"
        );
        SCOPED.fire();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
