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
//! Deliberately dependency-light (`wasm-bindgen` + `web-sys/console`, no
//! `fauna-log`): only `fauna-wasm` holds a log ring, via `installLogging`, so
//! the shared baseline every chunk can always reach is "mirror to the browser
//! console". A chunk with a richer sink layers it on via [`install_with`], and
//! a chunk's `tracing` reaches the ring through [`forward_tracing`].

// Empty rlib on native (the fauna-rpc-wasm pattern): the deps are declared in
// a wasm32 target table (see Cargo.toml — feature-unification leak guard), so
// a host compile of this crate (workspace-wide gates don't exclude it) must
// not reference them.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::fmt::Write;

use wasm_bindgen::JsValue;

thread_local! {
    /// Thread-local because a `js_sys::Function` is not `Send`, which a global
    /// subscriber must be. wasm32 is single-threaded, so this is one slot.
    static LOG_SINK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

/// Hand this chunk's `tracing` events to `sink(level, target, message)`, the
/// SPA's `logMessage` — each chunk is its own runtime with its own `tracing`
/// dispatcher, so without this a shared crate's log line running in a chunk
/// other than `fauna-wasm` goes nowhere (`observability.md` § Persistence &
/// privacy). A
/// plain [`tracing::Subscriber`] rather than `tracing-subscriber`: a line is all
/// this keeps, and the ring on the far side of `sink` does the rest. Filtered to
/// `info`, as `installLogging` is. A second call replaces the sink.
pub fn forward_tracing(sink: js_sys::Function) {
    LOG_SINK.with(|slot| *slot.borrow_mut() = Some(sink));
    let _ = tracing::subscriber::set_global_default(ForwardToSink);
}

struct ForwardToSink;

impl tracing::Subscriber for ForwardToSink {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut line = EventLine::default();
        event.record(&mut line);
        let meta = event.metadata();
        LOG_SINK.with(|slot| {
            if let Some(sink) = slot.borrow().as_ref() {
                let _ = sink.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(&meta.level().as_str().to_ascii_lowercase()),
                    &JsValue::from_str(meta.target()),
                    &JsValue::from_str(&line.0),
                );
            }
        });
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// The event's message, then its other fields as ` key=value`.
#[derive(Default)]
struct EventLine(String);

impl tracing::field::Visit for EventLine {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = if field.name() == "message" {
            write!(self.0, "{value:?}")
        } else {
            write!(self.0, " {}={value:?}", field.name())
        };
    }
}

/// One error-to-`JsValue` conversion, shared by every `fauna-wasm*` chunk —
/// the same "why one implementation instead of nine copies" rationale as
/// this crate's panic hook: every chunk already depends on this crate, so
/// the cost of sharing this is one call instead of one more local `fn`.
pub fn err_to_js<E: std::fmt::Display>(e: E) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Serialize `value` for the SPA the way its TS types declare it: every
/// `Option::None` crosses as `null` (not `undefined`) and every map — a
/// `LocalizedText`'s `args` above all — as a plain object (not a `Map`).
/// The default `serde_wasm_bindgen::to_value` does neither, so a face built on
/// it silently breaks `=== null` checks and `resolveLocalized`'s `args.x`
/// reads. Every `fauna-wasm*` chunk uses this; a site that really wants
/// `undefined` says so in a comment.
pub fn to_js<T: serde::Serialize + ?Sized>(
    value: &T,
) -> Result<JsValue, serde_wasm_bindgen::Error> {
    value.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
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
