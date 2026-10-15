//! Web's cross-tab account-registry mutation lock — the Web Locks leg of
//! [`MutationLock`](crate::MutationLock) (`docs/goal/architecture/long-term-store.md`
//! § Multi-account evolution → *Cross-process mutation lock*, the web leg).
//!
//! Tabs are web's concurrent instances (`docs/goal/architecture/apps/account-scoping.md`
//! § Concurrent instances → *Web*): they share one origin's `localStorage`,
//! so every registry mutator's read-modify-write of `fauna/index` races its
//! siblings exactly as native processes race a file — and `localStorage`
//! offers no cross-tab transaction. The native answer is a file lock acquired
//! INSIDE every mutator ([`MutationLock`](crate::MutationLock)); that cannot
//! transfer verbatim, because the only cross-tab lock a browser offers, the
//! Web Locks API, is asynchronous while the shared mutators are synchronous.
//! So web's registry carries [`NoopMutationLock`](crate::NoopMutationLock)
//! and the lock lives one layer out: every wasm-facing mutator entry wraps
//! its synchronous mutation in [`with_web_mutation_lock`], which holds the
//! origin-wide exclusive lock [`WEB_MUTATION_LOCK_NAME`] for exactly the
//! synchronous section. Reads never take it.
//!
//! **Degrades open**, like the file lock on I/O failure: no lock manager (no
//! secure context, some embeddings), or a request the browser rejects, runs
//! the section unguarded — the pre-lock behaviour — rather than turning a
//! missing API into a switch or sign-out that cannot complete.
//!
//! **Not reentrant.** Web Locks queue a request from the holder's own tab
//! behind the holder, so a section must never reach another mutator through
//! this wrapper: a section is one synchronous shared-registry call.

#![cfg(target_arch = "wasm32")]

/// The origin-wide registry mutation lock's name. The historical
/// migration-lock name, kept so tabs of an older and a newer build still
/// exclude each other.
pub const WEB_MUTATION_LOCK_NAME: &str = "fauna.accounts.migrate";

/// Run `mutate` while holding [`WEB_MUTATION_LOCK_NAME`] exclusively across
/// every tab of this origin — one exclusive section over the shared Web Locks
/// primitive (`fauna_core::web_lock::with_exclusive`). Queues behind a holder;
/// runs unguarded where the Web Locks API is unavailable or refuses the request
/// (degrade open, module docs). `mutate` runs exactly once either way.
pub async fn with_web_mutation_lock<T: 'static>(mutate: impl FnOnce() -> T + 'static) -> T {
    fauna_core::web_lock::with_exclusive(WEB_MUTATION_LOCK_NAME, mutate).await
}
