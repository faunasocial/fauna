//! The one Web Locks request primitive every web lock in shared Rust rides —
//! the browser's cross-tab lock manager (`navigator.locks`), reached from the
//! three legs that take a web lock: the account registry's mutation lock
//! (`fauna_client_accounts::with_web_mutation_lock`), the account store's two
//! roles (`fauna_account_store::locks`, web's leg) and the conversations-engine
//! role (`fauna_mls::engine_role`, web's leg). Lowest crate all three share,
//! hence here (`account-runtime.md` § Multi-instance concurrency → *Election
//! mechanics*: every web lock follows one scheme, `fauna.<owner>.<role>/<key>`).
//!
//! Two request shapes, the only two the fleet asks for:
//!
//! - [`WebLockHold::try_take`] — an `ifAvailable` **try**, never a wait: a
//!   role. Answers [`WebLockAsk::Held`] (an RAII hold, released when dropped or
//!   [`WebLockHold::release`]d, and by the browser when the tab goes away),
//!   [`WebLockAsk::Refused`] (another holder — another tab, or another hold in
//!   this one: a Web Lock does **not** re-enter), or [`WebLockAsk::Degraded`]
//!   (no lock manager in this context, or the browser refused the request).
//!   What `Degraded` means is the CALLER's: a role that guards
//!   user-irrecoverable state fails closed on it, a probe degrades open.
//! - [`with_exclusive`] — an exclusive section that **queues** behind a holder
//!   and holds for exactly one synchronous closure; degrades open (the section
//!   runs unguarded where no lock manager answers).
//!
//! The API is reached by property lookup, not typed `web_sys` bindings — its
//! types sit behind `web_sys`'s unstable-APIs cfg, which this workspace does not
//! enable — and from the global scope, so a worker finds it as a window does.

#![cfg(all(target_arch = "wasm32", feature = "js"))]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Object, Promise, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// How the lock manager answered one `ifAvailable` request.
#[derive(Debug)]
pub enum WebLockAsk {
    /// This context now holds the lock, for as long as the hold lives.
    Held(WebLockHold),
    /// Another holder has it.
    Refused,
    /// The lock could not be asked for. The caller owns what this means.
    Degraded(std::io::Error),
}

/// One held Web Lock; RAII — dropping releases.
pub struct WebLockHold {
    /// Resolving it resolves the promise the lock callback returned, which is
    /// what the lock manager holds the lock for.
    release: Option<Function>,
    /// The request's own promise: it settles once the lock is released.
    released: Option<JsFuture>,
    /// The lock callback, alive for as long as the hold.
    _callback: Closure<dyn FnMut(JsValue) -> JsValue>,
}

impl std::fmt::Debug for WebLockHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebLockHold").finish_non_exhaustive()
    }
}

impl WebLockHold {
    /// Ask for the lock named `name`, never waiting on a holder.
    pub async fn try_take(name: &str) -> WebLockAsk {
        let Some((locks, request)) = lock_request() else {
            return WebLockAsk::Degraded(std::io::Error::other(
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
                return WebLockAsk::Degraded(std::io::Error::other(format!(
                    "Web Locks request refused: {e:?}"
                )));
            }
        };
        let released = JsFuture::from(Promise::resolve(&requested));

        match JsFuture::from(decided).await {
            Ok(v) if v.as_bool() == Some(true) => WebLockAsk::Held(WebLockHold {
                release: release_slot,
                released: Some(released),
                _callback: callback,
            }),
            Ok(_) => WebLockAsk::Refused,
            Err(e) => WebLockAsk::Degraded(std::io::Error::other(format!(
                "Web Locks decision failed: {e:?}"
            ))),
        }
    }

    /// Let the lock go and wait until the lock manager has released it — so a
    /// caller (or a test) that hands the lock on sees the next try succeed.
    /// Dropping releases too, without the wait.
    pub async fn release(mut self) {
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

impl Drop for WebLockHold {
    fn drop(&mut self) {
        self.let_go();
    }
}

/// Run `section` while holding the lock named `name` exclusively across every
/// tab of this origin. Queues behind a holder; runs unguarded where the Web
/// Locks API is unavailable or refuses the request (degrade open). `section`
/// runs exactly once either way, and the hold spans exactly that synchronous
/// call. **Not reentrant**: `section` must never ask for `name` again.
pub async fn with_exclusive<T: 'static>(name: &str, section: impl FnOnce() -> T + 'static) -> T {
    // The section and its result cross into the lock callback (a JS closure,
    // hence `'static`) and back out through these two cells; whichever path
    // ends up running the section, it runs exactly once.
    type Section<T> = Rc<RefCell<Option<Box<dyn FnOnce() -> T>>>>;
    let section: Section<T> = Rc::new(RefCell::new(Some(Box::new(section))));
    let result: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));

    if let Some((locks, request)) = lock_request() {
        let cb_section = Rc::clone(&section);
        let cb_result = Rc::clone(&result);
        // Invoked once the lock is held; returning a plain value releases it
        // the moment the callback returns.
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
        if let Ok(promise) = request.call2(&locks, &JsValue::from_str(name), callback.as_ref()) {
            let _ = JsFuture::from(Promise::from(promise)).await;
        }
        drop(callback);
    }

    if let Some(f) = section.borrow_mut().take() {
        *result.borrow_mut() = Some(f());
    }
    let outcome = result.borrow_mut().take();
    outcome.expect("the exclusive section runs exactly once")
}

/// This context's `navigator.locks` and its `request` method, or `None` where
/// the Web Locks API is absent (module docs: property lookup, global scope).
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
