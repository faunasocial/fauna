//! One panic hook implementation, shared by every `fauna-wasm*` chunk.
//!
//! **Why this crate exists instead of nine copies of the same few lines.**
//! Each wasm chunk (`libs/fauna-wasm`, `libs/fauna-wasm-onboarding`, …) is its
//! own `.wasm` binary with its own Rust runtime — `std::panic::set_hook` in
//! one module has zero effect on any other. wasm has no unwinding, so an
//! uninstalled panic inside any exported fn (especially a
//! `future_to_promise` task) aborts mid-poll: the task dies, its JS promise
//! never settles, and the only witness is a bare `RuntimeError: unreachable`
//! — no message, no panic site, no chunk name. That silent-hang shape
//! survived repeated diagnosis on the core chunk before `fauna-wasm`'s own
//! hook named it (`libs/fauna-wasm/src/logs.rs::install_panic_hook`); this crate generalizes that fix so every chunk gets it for
//! the cost of one dependency line + one call.
//!
//! Deliberately dependency-light (`wasm-bindgen` + `web-sys/console` only —
//! no `tracing`/`fauna-log`): most chunks have no tracing subscriber of their
//! own (only `fauna-wasm` does, via `installLogging`), so the shared
//! baseline every chunk can always reach is "mirror to the browser console".
//! A chunk with a richer sink layers it on via [`install_with`].

// Empty rlib on native (the fauna-rpc-wasm pattern): the deps are declared in
// a wasm32 target table (see Cargo.toml — feature-unification leak guard), so
// a host compile of this crate (workspace-wide gates don't exclude it) must
// not reference them.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsValue;

/// One error-to-`JsValue` conversion, shared by every `fauna-wasm*` chunk —
/// the same "why one implementation instead of nine copies" rationale as
/// this crate's panic hook: every chunk already depends on this crate, so
/// the cost of sharing this is one call instead of one more local `fn`.
pub fn err_to_js<E: std::fmt::Display>(e: E) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Install a panic hook that mirrors every panic to the browser console,
/// naming `chunk` in the message — the only way to tell which wasm module
/// panicked from the console alone, since each chunk is a separate runtime —
/// then chains to whatever hook was previously installed (never silently
/// swallows the default hook's own output).
pub fn install(chunk: &'static str) {
    install_with(chunk, |_message| {});
}

/// Like [`install`], plus `extra_sink`, called with the formatted message —
/// the seam `fauna-wasm` uses to also feed its `tracing`-backed log ring.
/// `Send + Sync` is required by `std::panic::set_hook`'s own signature, not
/// because a hook can really cross a thread (wasm32 is single-threaded) —
/// callers pass plain closures/fn items with no non-`Send` captures, so the
/// bound costs nothing in practice.
pub fn install_with(chunk: &'static str, extra_sink: impl Fn(&str) + Send + Sync + 'static) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = format!("fauna wasm panic [{chunk}]: {info}");
        web_sys::console::error_1(&JsValue::from_str(&message));
        extra_sink(&message);
        previous(info);
    }));
}
