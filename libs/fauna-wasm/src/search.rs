//! [`WasmSearchManager`] — the web twin of the native `FfiSearchManager`
//! UniFFI façade: a thin `wasm_bindgen` wrapper over the shared stateful
//! `fauna_client_search::SearchManager<WsRpcClient>`, so the Svelte Search
//! page renders entirely from `snapshot()` and forwards gestures to the async
//! manager methods (`docs/goal/ui/search.md` § State & data shape, ratified
//! 2026-08-02).
//!
//! Wasm-only, like [`crate::feed::WasmFeedManager`]: `fauna-rpc-wasm` (the
//! `Rc`-based browser transport) only exists on wasm, so the whole file is
//! gated at the `mod` site in `lib.rs`, matching `crate::rpc`.
//!
//! **The local arm (backend 2) never registers here.** The sealed tantivy
//! index cannot run in a browser (`content-index.md` § Where queries run), so
//! `fauna_client_search::local::LocalSearchIndex` depends on neither tantivy
//! nor an index crate — this crate's WASM graph structurally cannot contain
//! one. `has_local_index()` on a wasm manager is permanently `false`, so it
//! is not exported: nothing on web would ever read `true` from it
//! (`search.md` § Implementation status today — *the local arm is off on
//! wasm, structurally; do not "fix" it*).
//!
//! Like [`crate::feed::WasmFeedManager`], no foreign `SearchSnapshotObserver`
//! callback crosses into JS: the browser owns the loop, so the page `await`s
//! each async manager method then calls `snapshot()` again — the
//! snapshot-after-call reactivity contract.

use std::rc::Rc;

use fauna_client_search::SearchManager;
use fauna_rpc_wasm::WsRpcClient;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

/// The shared, stateful Search page exposed to the Svelte SPA — the web twin
/// of the native `FfiSearchManager`. Holds `Rc<SearchManager<WsRpcClient>>` so
/// each `future_to_promise` body owns a cheap clone across its `await`.
#[wasm_bindgen]
pub struct WasmSearchManager {
    manager: Rc<SearchManager<WsRpcClient>>,
}

impl WasmSearchManager {
    /// Build over the browser WS-RPC `client`. Plain (non-`#[wasm_bindgen]`)
    /// constructor — `WsRpcClient` is the inner transport, not a JS type, so
    /// the JS entry point is the `WsRpcClient::searchManager` factory which
    /// owns it (mirrors `WasmFeedManager::with_client`). Unlike the Feed
    /// manager this needs no actor secret — searching signs nothing. The
    /// snapshot starts `SearchSnapshot::default()` (the pre-search state) and
    /// stays there until the page fires `runQuery`.
    pub fn with_client(client: WsRpcClient) -> WasmSearchManager {
        Self {
            manager: Rc::new(SearchManager::new(client)),
        }
    }
}

#[wasm_bindgen]
impl WasmSearchManager {
    /// The current `SearchSnapshot` as a plain JS object — the SPA renders
    /// the query bar, cancel button, and results/no-results pair entirely
    /// from it. `json_compatible` so numbers stay numbers and unit enums
    /// (`source`) serialize to strings (matches `snapshot()` on the feed and
    /// conversations managers).
    #[wasm_bindgen(js_name = "snapshot")]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        crate::rpc::to_js(&self.manager.snapshot())
    }

    /// `search-cancel-button` — reset the page to its pre-search state.
    ///
    /// Synchronous, but **not** merely a local clear: it bumps the manager's
    /// query generation, so a query still in flight lands on a cancelled page
    /// and is dropped rather than repopulating results the user just
    /// dismissed.
    #[wasm_bindgen(js_name = "cancel")]
    pub fn cancel(&self) {
        self.manager.cancel();
    }

    /// Fire a search: `search-submit-button`, and `search-type-filter` when
    /// the filter changes. `query` is the page's **live** query-field buffer
    /// (the field stays plain local state — the page is submit-driven, no
    /// debounce). A blank query is a no-op that leaves the page untouched.
    /// `type_filter` is `TYPE_FILTER_ALL` or a nest `content_type`; the
    /// manager applies it to both arms through one shared mapping. Resolves
    /// `undefined`; the page re-reads `snapshot()` after.
    #[wasm_bindgen(js_name = "runQuery")]
    pub fn run_query(&self, query: String, type_filter: String) -> js_sys::Promise {
        let m = self.manager.clone();
        future_to_promise(async move {
            m.run_query(&query, &type_filter).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// `search-load-more-button` — re-fire the **last fired** query (not the
    /// live buffer) with a grown page. A no-op when nothing has been
    /// searched. Resolves `undefined`; the page re-reads `snapshot()` after.
    #[wasm_bindgen(js_name = "loadMore")]
    pub fn load_more(&self) -> js_sys::Promise {
        let m = self.manager.clone();
        future_to_promise(async move {
            m.load_more().await;
            Ok(JsValue::UNDEFINED)
        })
    }
}
