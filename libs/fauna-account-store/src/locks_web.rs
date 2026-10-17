//! The store's advisory locks — **web's leg** (mounted as `crate::locks` on
//! wasm32; the native leg is `locks.rs`, whose module docs own the model).
//!
//! Of the native leg's four locks, web needs the two roles:
//!
//! - **[`EngineLock`] — the engine-singleton role**, which the charter gives
//!   web as "the Web Locks API keyed by store name (web, among tabs)"
//!   (`docs/goal/architecture/account-runtime.md` § Multi-instance
//!   concurrency → *Election mechanics*): a **try**-acquire, held for the
//!   role's lifetime, released by the browser when the holding tab closes or
//!   crashes — the kernel-arbitrated shape, with the lock manager as kernel.
//! - **[`SeedLegLock`] — the seed-leg role** (same section → *The seed-leg
//!   role*, part 1): the same request under a second Web Locks name. Every
//!   tab holds the seed, so the tab that wins the engine role ordinarily wins
//!   this one right behind it.
//! - The **migration** section needs no lock here: IndexedDB's own
//!   `versionchange` transaction runs exclusively across every connection of
//!   the origin (`crate::indexeddb::IndexedDbBackend::open`).
//! - The **serving** lock (presence, asked by an erase) has no web leg here,
//!   by ruling: web's erase asks the per-account conversations-engine role
//!   Web Lock (`fauna_mls::engine_role`, taken in the wasm engine-construction
//!   path), `ifAvailable`, never a wait (`apps/account-scoping.md`
//!   § Concurrent instances → *Web owes the same refusal through the lock it
//!   already has*). That lock and [`EngineLock`]
//!   are two roles under two names, held side by side in the one tab that
//!   hosts the runtime — BY RULING, not by accident (`account-runtime.md`
//!   § Multi-instance concurrency → *Election mechanics*, 2026-10-01): it is
//!   the conversations-engine role (`mls_state.db.lock` natively,
//!   a different section held by a different process in steady state), a
//!   Web Lock does not re-enter, and the erase needs only the role whose
//!   holder hosts the runtime. Every web lock follows one naming scheme,
//!   `fauna.<owner>.<role>/<key>` — the two below,
//!   `fauna.mls.conversations-engine/<actor>`, and the keyless
//!   `fauna.accounts.migrate`.
//!
//! Both are one request over the shared Web Locks primitive
//! (`fauna_core::web_lock`, which owns how the API is reached): a missing API
//! or a refused request reports `Degraded` and the caller owns what that means
//! (the native leg's I/O-failure posture). **Not reentrant**: a second acquire
//! from the holding tab queues behind — here, is `Refused` by — its own hold.

use fauna_core::web_lock::{WebLockAsk, WebLockHold};

/// The Web Locks name of `store_name`'s engine-singleton role — one lock per
/// store, the web analogue of `<store dir>/engine.lock`.
pub fn engine_lock_name(store_name: &str) -> String {
    format!("fauna.account-store.engine/{store_name}")
}

/// The Web Locks name of `store_name`'s seed-leg role — the web analogue of
/// `<store dir>/seed-legs.lock`.
pub fn seed_legs_lock_name(store_name: &str) -> String {
    format!("fauna.account-store.seed-legs/{store_name}")
}

/// Result of [`EngineLock::try_acquire`] — the native leg's outcome, arm for
/// arm.
#[derive(Debug)]
pub enum EngineLockOutcome {
    /// This tab now holds the engine-singleton role; it releases when the
    /// value drops (or [`EngineLock::release`]s), or when the tab goes away.
    Held(EngineLock),
    /// Another holder has the role — another tab, or another runtime in this
    /// one. Run as a plain reader/writer and re-try on the backstop cadence.
    Refused,
    /// The lock could not be asked for (no Web Locks API in this context, or
    /// the browser refused the request). The caller owns what this means.
    Degraded(std::io::Error),
}

/// A held engine-singleton role; RAII — dropping releases.
pub struct EngineLock(WebLockHold);

impl std::fmt::Debug for EngineLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineLock").finish_non_exhaustive()
    }
}

impl EngineLock {
    /// Try to become the engine singleton for the store named `store_name`.
    /// Never waits on a holder: `ifAvailable` answers at once.
    pub async fn try_acquire(store_name: &str) -> EngineLockOutcome {
        match WebLockHold::try_take(&engine_lock_name(store_name)).await {
            WebLockAsk::Held(hold) => EngineLockOutcome::Held(EngineLock(hold)),
            WebLockAsk::Refused => EngineLockOutcome::Refused,
            WebLockAsk::Degraded(e) => EngineLockOutcome::Degraded(e),
        }
    }

    /// Release the role and wait until the lock manager has let it go — so a
    /// caller (or a test) that hands the role on sees the next acquire
    /// succeed. Dropping releases too, without the wait.
    pub async fn release(self) {
        self.0.release().await;
    }
}

/// Result of [`SeedLegLock::try_acquire`] — [`EngineLockOutcome`]'s arms, for
/// the store's other role.
#[derive(Debug)]
pub enum SeedLegLockOutcome {
    /// This tab now holds the seed-leg role; it releases when the value drops
    /// (or [`SeedLegLock::release`]s), or when the tab goes away.
    Held(SeedLegLock),
    /// Another holder has the role — another tab, or another runtime in this
    /// one. Run no seed-only leg and re-try on the backstop cadence.
    Refused,
    /// The lock could not be asked for. The caller owns what this means.
    Degraded(std::io::Error),
}

/// A held seed-leg role; RAII — dropping releases.
pub struct SeedLegLock(WebLockHold);

impl std::fmt::Debug for SeedLegLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedLegLock").finish_non_exhaustive()
    }
}

impl SeedLegLock {
    /// Try to become the seed-leg holder for the store named `store_name`
    /// (`account-runtime.md` § Multi-instance concurrency → *The seed-leg
    /// role*). Never waits on a holder.
    pub async fn try_acquire(store_name: &str) -> SeedLegLockOutcome {
        match WebLockHold::try_take(&seed_legs_lock_name(store_name)).await {
            WebLockAsk::Held(hold) => SeedLegLockOutcome::Held(SeedLegLock(hold)),
            WebLockAsk::Refused => SeedLegLockOutcome::Refused,
            WebLockAsk::Degraded(e) => SeedLegLockOutcome::Degraded(e),
        }
    }

    /// Release the role and wait until the lock manager has let it go.
    /// Dropping releases too, without the wait.
    pub async fn release(self) {
        self.0.release().await;
    }
}
