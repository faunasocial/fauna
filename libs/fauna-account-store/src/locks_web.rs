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
//!   by ruling: web's erase asks the per-account MLS-engine role Web Lock the
//!   SPA already holds (`$lib/webLocks`), `ifAvailable`, never a wait
//!   (`apps/account-scoping.md` § Concurrent instances → *Web owes the same
//!   refusal through the lock it already has*). That lock and [`EngineLock`]
//!   are two roles under two names, held side by side in the one tab that
//!   hosts the runtime — BY RULING, not by accident (`account-runtime.md`
//!   § Multi-instance concurrency → *Election mechanics*, 2026-10-01): the
//!   SPA's is the conversations-engine role (`mls_state.db.lock` natively,
//!   a different section held by a different process in steady state), a
//!   Web Lock does not re-enter, and the erase needs only the role whose
//!   holder hosts the runtime. Every web lock follows one naming scheme,
//!   `fauna.<owner>.<role>/<key>` — the two below, the SPA's
//!   `fauna.mls.conversations-engine/<actor>`, and the keyless
//!   `fauna.accounts.migrate`.
//!
//! The Web Locks API is reached by property lookup, not typed `web_sys`
//! bindings — its types sit behind `web_sys`'s unstable-APIs cfg, which this
//! workspace does not enable (the shipped precedent:
//! `fauna_client_accounts::web_mutation_lock`). As there, a missing API or a
//! refused request reports `Degraded` and the caller owns what that means (the
//! native leg's I/O-failure posture). **Not reentrant**: a second acquire from
//! the holding tab queues behind — here, is `Refused` by — its own hold.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Object, Promise, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

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
pub struct EngineLock(Hold);

impl std::fmt::Debug for EngineLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineLock").finish_non_exhaustive()
    }
}

impl EngineLock {
    /// Try to become the engine singleton for the store named `store_name`.
    /// Never waits on a holder: `ifAvailable` answers at once.
    pub async fn try_acquire(store_name: &str) -> EngineLockOutcome {
        match Hold::try_take(&engine_lock_name(store_name)).await {
            Asked::Held(hold) => EngineLockOutcome::Held(EngineLock(hold)),
            Asked::Refused => EngineLockOutcome::Refused,
            Asked::Degraded(e) => EngineLockOutcome::Degraded(e),
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
pub struct SeedLegLock(Hold);

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
        match Hold::try_take(&seed_legs_lock_name(store_name)).await {
            Asked::Held(hold) => SeedLegLockOutcome::Held(SeedLegLock(hold)),
            Asked::Refused => SeedLegLockOutcome::Refused,
            Asked::Degraded(e) => SeedLegLockOutcome::Degraded(e),
        }
    }

    /// Release the role and wait until the lock manager has let it go.
    /// Dropping releases too, without the wait.
    pub async fn release(self) {
        self.0.release().await;
    }
}

/// One held Web Lock — what both role locks are.
struct Hold {
    /// Resolving it resolves the promise the lock callback returned, which is
    /// what the lock manager holds the lock for.
    release: Option<Function>,
    /// The request's own promise: it settles once the lock is released.
    released: Option<JsFuture>,
    /// The lock callback, alive for as long as the hold.
    _callback: Closure<dyn FnMut(JsValue) -> JsValue>,
}

/// How the lock manager answered one `ifAvailable` request.
enum Asked {
    Held(Hold),
    Refused,
    Degraded(std::io::Error),
}

impl Hold {
    /// Ask for the lock named `name`, never waiting on a holder.
    async fn try_take(name: &str) -> Asked {
        let Some((locks, request)) = lock_request() else {
            return Asked::Degraded(std::io::Error::other(
                "the Web Locks API is unavailable in this context",
            ));
        };

        // `hold` is what the callback hands the lock manager on a grant; the
        // lock stays held until it resolves. `decided` carries the grant or
        // refusal back out of the callback.
        let mut release_slot = None;
        let hold = Promise::new(&mut |resolve, _| release_slot = Some(resolve));
        let decision: Rc<RefCell<Option<Function>>> = Rc::new(RefCell::new(None));
        let decided = Promise::new(&mut |resolve, _| *decision.borrow_mut() = Some(resolve));
        let callback = Closure::<dyn FnMut(JsValue) -> JsValue>::new({
            let decision = Rc::clone(&decision);
            move |lock: JsValue| {
                let granted = !lock.is_null() && !lock.is_undefined();
                if let Some(tell) = decision.borrow_mut().take() {
                    let _ = tell.call1(&JsValue::NULL, &JsValue::from_bool(granted));
                }
                if granted {
                    hold.clone().into()
                } else {
                    JsValue::UNDEFINED
                }
            }
        });

        let options = Object::new();
        let _ = Reflect::set(&options, &JsValue::from_str("ifAvailable"), &JsValue::TRUE);
        let requested = match request.call3(
            &locks,
            &JsValue::from_str(name),
            &options,
            callback.as_ref(),
        ) {
            Ok(p) => p,
            Err(e) => {
                return Asked::Degraded(std::io::Error::other(format!(
                    "Web Locks request refused: {e:?}"
                )));
            }
        };
        let released = JsFuture::from(Promise::resolve(&requested));

        match JsFuture::from(decided).await {
            Ok(v) if v.as_bool() == Some(true) => Asked::Held(Hold {
                release: release_slot,
                released: Some(released),
                _callback: callback,
            }),
            Ok(_) => Asked::Refused,
            Err(e) => Asked::Degraded(std::io::Error::other(format!(
                "Web Locks decision failed: {e:?}"
            ))),
        }
    }

    /// Let the lock go and wait until the lock manager has released it.
    async fn release(mut self) {
        self.let_go();
        if let Some(released) = self.released.take() {
            let _ = released.await;
        }
    }

    fn let_go(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.let_go();
    }
}

/// This context's `navigator.locks` and its `request` method, or `None` where
/// the Web Locks API is absent — by property lookup (module docs), from the
/// global scope so a worker finds it as a window does.
fn lock_request() -> Option<(JsValue, Function)> {
    let navigator = Reflect::get(&js_sys::global(), &JsValue::from_str("navigator")).ok()?;
    let locks = Reflect::get(&navigator, &JsValue::from_str("locks")).ok()?;
    if locks.is_undefined() || locks.is_null() {
        return None;
    }
    let request = Reflect::get(&locks, &JsValue::from_str("request"))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    Some((locks, request))
}
