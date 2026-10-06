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

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// The origin-wide registry mutation lock's name. The historical
/// migration-lock name, kept so tabs of an older and a newer build still
/// exclude each other.
pub const WEB_MUTATION_LOCK_NAME: &str = "fauna.accounts.migrate";

/// Run `mutate` while holding [`WEB_MUTATION_LOCK_NAME`] exclusively across
/// every tab of this origin. Queues behind a holder; runs unguarded where the
/// Web Locks API is unavailable or refuses the request (degrade open, module
/// docs). `mutate` runs exactly once either way.
pub async fn with_web_mutation_lock<T: 'static>(mutate: impl FnOnce() -> T + 'static) -> T {
    // The section and its result cross into the lock callback (a JS closure,
    // hence `'static`) and back out through these two cells; whichever path
    // ends up running the section, it runs exactly once.
    type Section<T> = Rc<RefCell<Option<Box<dyn FnOnce() -> T>>>>;
    let section: Section<T> = Rc::new(RefCell::new(Some(Box::new(mutate))));
    let result: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));

    if let Some((locks, request)) = lock_request() {
        let cb_section = Rc::clone(&section);
        let cb_result = Rc::clone(&result);
        // Invoked once the lock is held; returning a plain value releases it
        // the moment the callback returns, so the hold spans exactly the
        // synchronous section.
        let callback = Closure::once(move |_lock: JsValue| {
            if let Some(f) = cb_section.borrow_mut().take() {
                *cb_result.borrow_mut() = Some(f());
            }
            JsValue::UNDEFINED
        });
        // `navigator.locks.request(name, callback)` — exclusive is the default
        // mode. A request that throws or rejects (a `SecurityError` embedding)
        // never invoked the callback: the section is still in its cell and
        // runs unguarded below.
        if let Ok(promise) = request.call2(
            &locks,
            &JsValue::from_str(WEB_MUTATION_LOCK_NAME),
            callback.as_ref(),
        ) {
            let _ = JsFuture::from(js_sys::Promise::from(promise)).await;
        }
        drop(callback);
    }

    if let Some(f) = section.borrow_mut().take() {
        *result.borrow_mut() = Some(f());
    }
    let outcome = result.borrow_mut().take();
    outcome.expect("the mutation section runs exactly once")
}

/// This page's `navigator.locks` and its `request` method, or `None` where the
/// Web Locks API is absent. Reached by property lookup rather than typed
/// `web_sys` bindings, so a missing `locks` reads as absent instead of
/// throwing — and because `web_sys`'s Web Locks types sit behind its
/// unstable-APIs cfg, which this workspace does not enable.
fn lock_request() -> Option<(JsValue, js_sys::Function)> {
    let window = web_sys::window()?;
    let navigator = js_sys::Reflect::get(&window, &JsValue::from_str("navigator")).ok()?;
    let locks = js_sys::Reflect::get(&navigator, &JsValue::from_str("locks")).ok()?;
    if locks.is_undefined() || locks.is_null() {
        return None;
    }
    let request = js_sys::Reflect::get(&locks, &JsValue::from_str("request"))
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    Some((locks, request))
}
