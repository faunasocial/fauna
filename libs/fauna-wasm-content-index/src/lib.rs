//! WASM bindings for fauna-index. **ON HOLD — does not run in a browser.**
//!
//! This crate compiles to `wasm32-unknown-unknown` (after `fauna-index` drops
//! tantivy's `mmap` feature and we add a `uuid` randomness source — both done),
//! but tantivy spawns background threads (a segment updater + indexing workers)
//! and `std::thread::spawn` is unsupported on `wasm32-unknown-unknown`, so
//! `Index::create_in_ram()` panics at runtime: `tantivy error: System error
//! 'Failed to spawn segment updater thread'`. Verified on Linux via
//! `wasm-pack test --headless --chrome` (2026-05-11).
//!
//! Consequence: the web app does NOT run a local Tantivy index. Web (and
//! iOS) search queries go through WS-RPC against the user's plaintext-mode nest
//! (or the MDA bridge during a session) — design tracked internally
//! (§ Open items and Plan 6). This crate is kept in-tree (it still builds as a
//! host-target lib so `cargo build --workspace` is unaffected) but is removed
//! from the `just wasm` pipeline. Re-attempt path (wasm-threads / upstream
//! tantivy thread-free mode) is tracked internally.
//!
//! Wraps a single in-memory `Index` instance held in a thread-local `RefCell`
//! (WASM is single-threaded — no Mutex needed). The web app constructs the
//! handle once at session start, accumulates docs through the day's session,
//! and queries against the live in-RAM index.
//!
//! Surface mirrors the UniFFI handle: createInRam / addDoc / commit / query.
//! `IndexedDoc`, `QueryHit`, `TimeRange` are passed through `serde_json` so we
//! don't have to define `#[wasm_bindgen]` getters per field. Errors land as JS
//! `Error` instances via `JsValue::from_str`.

use std::cell::RefCell;

use fauna_index::{ContentKind, Index, IndexedDoc, QueryHit, TimeRange};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

thread_local! {
    /// At most one `Index` per JS realm (worker / main thread). The web app
    /// constructs it on Search-page first-load (Plan 6) and reuses across
    /// queries.
    static INDEX: RefCell<Option<Index>> = const { RefCell::new(None) };
}

#[derive(Serialize, Deserialize)]
struct QueryArgs {
    query: String,
    kinds: Vec<ContentKind>,
    range: Option<TimeRange>,
    limit: u32,
}

/// Construct a fresh in-memory index. Replaces any prior instance held in this
/// JS realm.
#[wasm_bindgen]
pub fn content_index_create_in_ram() -> Result<(), JsValue> {
    let idx = Index::create_in_ram().map_err(|e| JsValue::from_str(&e.to_string()))?;
    INDEX.with(|cell| {
        *cell.borrow_mut() = Some(idx);
    });
    Ok(())
}

/// Add one document. `doc_json` is the JSON-encoded `IndexedDoc` shape (matches
/// the snake_case serde derives on the Rust struct).
#[wasm_bindgen]
pub fn content_index_add_doc(doc_json: &str) -> Result<(), JsValue> {
    let doc: IndexedDoc =
        serde_json::from_str(doc_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    INDEX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let idx = borrow
            .as_mut()
            .ok_or_else(|| JsValue::from_str("content index not initialised"))?;
        idx.add_doc(doc)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    })
}

/// Commit pending writes.
#[wasm_bindgen]
pub fn content_index_commit() -> Result<(), JsValue> {
    INDEX.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let idx = borrow
            .as_mut()
            .ok_or_else(|| JsValue::from_str("content index not initialised"))?;
        idx.commit().map_err(|e| JsValue::from_str(&e.to_string()))
    })
}

/// Run a free-text query. Returns a JSON array of `QueryHit`.
#[wasm_bindgen]
pub fn content_index_query(args_json: &str) -> Result<String, JsValue> {
    let args: QueryArgs =
        serde_json::from_str(args_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    INDEX.with(|cell| {
        let borrow = cell.borrow();
        let idx = borrow
            .as_ref()
            .ok_or_else(|| JsValue::from_str("content index not initialised"))?;
        let hits: Vec<QueryHit> = idx
            .query(&args.query, &args.kinds, args.range, args.limit as usize)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_json::to_string(&hits).map_err(|e| JsValue::from_str(&e.to_string()))
    })
}

// ── Panic hook ───────────────────────────────────────────────────────────
//
// Each wasm chunk is its own module with its own Rust runtime, so a hook
// installed in one chunk covers none of the others (see the
// `fauna-wasm-panic-hook` crate doc comment). `#[wasm_bindgen(start)]` runs
// automatically the moment this chunk's module is instantiated. This chunk is
// currently ON HOLD (not built into the SPA's `static/` — see the module doc
// comment), but the hook is installed anyway so it is not a landmine for
// whoever revives it.
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-content-index");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}
