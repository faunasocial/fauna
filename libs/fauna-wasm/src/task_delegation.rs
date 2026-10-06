//! The Task-delegation Settings sub-page's wasm surface — the web twin of the
//! native `FfiTaskDelegationView` (`fauna-ffi`'s `task-delegation` feature), over
//! the same shared `fauna_client_delegation::TaskDelegationView` the four native
//! apps render (`docs/goal/behavior/participants.md` § Task delegation;
//! ui.yaml page `task-delegation`).
//!
//! **No type mirrors.** `TaskDelegationRow` / `PinOption` / `RunnerStatus` are
//! already `Serialize`/`Deserialize` in `fauna-core`, so they cross to JS through
//! `serde_wasm_bindgen` (the `feed.rs` pattern) rather than through a hand-kept
//! wasm mirror. The SPA renders `row.pin_options` verbatim: the rule deciding
//! *which* options a picker may offer is a correctness surface and lives once, in
//! shared Rust, for all seven apps (participants.md § The assignment picker).
//!
//! **One store path, shared.** `load` and `setAssignment` go through the
//! `preference_surfaces` the native apps call: the pins ride this tab's
//! account store (`crate::account_runtime::handle_source()`, waited for when
//! a call arrives before the runtime is up); the live lease stays a nest read
//! (`config-dissolution.md` § The `__config` dissolution schedule → *The
//! closure order*, steps (2) and (5)).

use std::collections::HashMap;
use std::rc::Rc;

use fauna_account_plane::preference_surfaces;
use fauna_client_delegation::{DelegationClient, TaskDelegationView};
use fauna_core::data::ParticipantRef;
use fauna_core::delegation::{HeavyTaskCapability, PinOption, RunnerStatus};
use fauna_rpc_wasm::WsRpcClient;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{err_to_js, from_js, to_js};

/// A browser tab **runs no heavy task kind at all**: the web SPA drives no lease
/// driver, and it is the one app structurally barred from the content-index
/// builder too (tantivy cannot run in a browser — `content-index.md` § Where
/// queries run). A pin collapses the candidate set to exactly the pinned
/// participant, so pinning one to a browser would make the kind wait *forever*
/// (participants.md § The assignment picker).
///
/// Hard-coded here rather than taken as a JS argument, so the SPA cannot strand a
/// task kind by passing the wrong value. The native surfaces *do* take the
/// capability as a parameter, because which kinds a native app runs differs
/// per app — but wasm has exactly one consumer, and it runs none of them.
fn web_capability() -> HeavyTaskCapability {
    HeavyTaskCapability::viewer_only()
}

/// The Task-delegation surface for the logged-in actor in this browser.
///
/// Holds `Rc<TaskDelegationView<WsRpcClient>>` so each `future_to_promise` body
/// owns a cheap clone across its `await` (mirrors `WasmFeedManager`).
/// Label a runner status for display (priority #2 — the one shared decision in
/// `fauna_core::delegation`, mirroring `fauna-ffi`'s
/// `task_delegation_runner_label` for the four native apps). `runner` is a
/// `RunnerStatus` exactly as it came out of [`WasmTaskDelegationView::load`]'s
/// `runner` field; `labels` is the SPA's device-id → display-name roster (a
/// plain JS object). Resolves to a `LocalizedText` `{ key, args }` for the SPA
/// to resolve through `L()`.
#[wasm_bindgen(js_name = taskDelegationRunnerLabel)]
pub fn task_delegation_runner_label(runner: JsValue, labels: JsValue) -> Result<JsValue, JsValue> {
    let runner: RunnerStatus = from_js(runner)?;
    let labels: HashMap<String, String> = from_js(labels)?;
    to_js(&fauna_core::delegation::runner_label(&runner, &labels))
}

/// Label one assignment-picker option for display. See
/// [`task_delegation_runner_label`] for the shape (same roster input, same
/// `LocalizedText` output, same shared decision in `fauna_core::delegation`).
#[wasm_bindgen(js_name = taskDelegationOptionLabel)]
pub fn task_delegation_option_label(option: JsValue, labels: JsValue) -> Result<JsValue, JsValue> {
    let option: PinOption = from_js(option)?;
    let labels: HashMap<String, String> = from_js(labels)?;
    to_js(&fauna_core::delegation::option_label(&option, &labels))
}

#[wasm_bindgen]
pub struct WasmTaskDelegationView {
    view: Rc<TaskDelegationView<WsRpcClient>>,
}

impl WasmTaskDelegationView {
    /// Build over the browser WS-RPC `client` and this browser's
    /// `device_id_hex`. The pins rest sealed on `fauna.state.delegation` in
    /// this tab's account store, so no key crosses the boundary.
    ///
    /// Plain (non-`#[wasm_bindgen]`) constructor: `WsRpcClient` is the inner
    /// transport, not a JS type, so the JS entry point is the
    /// `WsRpcClient::taskDelegationViewForDevice` factory that owns it (mirrors
    /// `WasmFeedManager::with_client`).
    ///
    /// `device_id_hex` is only ever compared for equality — against the lease
    /// holder (is this device the runner?) and against the pinned participant (is
    /// the pin me?). A [`web_capability`] client answers "no" to both by
    /// construction, so the id serves to *distinguish* this browser from the
    /// participants it renders, never to claim a lease.
    pub fn with_client(client: WsRpcClient, device_id_hex: String) -> WasmTaskDelegationView {
        Self {
            view: Rc::new(TaskDelegationView::new(
                DelegationClient::new(client),
                ParticipantRef::Device {
                    device_id: device_id_hex,
                },
                web_capability(),
            )),
        }
    }
}

#[wasm_bindgen]
impl WasmTaskDelegationView {
    /// Read the surface: one row per live task kind (`LIVE_TASK_KINDS`), each
    /// composing the user's pin (`fauna.state.delegation`) with the live advisory
    /// lease (`fauna.delegation.observe`) into a runner + an assignment + the
    /// options the picker may offer.
    ///
    /// Resolves to a JS array of `TaskDelegationRow`. `json_compatible` so
    /// `LocalizedText.args` arrives as a plain object (not a JS `Map`) and the
    /// unit enum arms (`"Automatic"`, `"Waiting"`) serialize to strings — the same
    /// serializer `WasmFeedManager::snapshot` uses.
    #[wasm_bindgen(js_name = "load")]
    pub fn load(&self) -> js_sys::Promise {
        let view = self.view.clone();
        future_to_promise(async move {
            let store = crate::account_runtime::handle_source();
            let rows = preference_surfaces::load_task_delegation_rows(&store, &view)
                .await
                .map_err(preference_surfaces::delegation_failure)
                .map_err(err_to_js)?;
            to_js(&rows)
        })
    }

    /// Write the user's assignment for `task_kind` — the picker's `onchange`.
    ///
    /// `option` is a `PinOption` exactly as it came out of [`Self::load`]'s
    /// `pin_options` (the SPA passes the selected element straight back, never a
    /// re-derived one). The write rides `put_preference`'s read-modify-write on the account store, so a
    /// sibling device editing the pins concurrently is merged rather than
    /// clobbered; and a `ThisDevice` pin is refused before the first request,
    /// since this client can never run the kind.
    #[wasm_bindgen(js_name = "setAssignment")]
    pub fn set_assignment(&self, task_kind: String, option: JsValue) -> js_sys::Promise {
        let view = self.view.clone();
        let option: Result<PinOption, _> = serde_wasm_bindgen::from_value(option);
        future_to_promise(async move {
            let option = option
                .map_err(|e| JsValue::from_str(&format!("invalid assignment option: {e}")))?;
            let store = crate::account_runtime::handle_source();
            preference_surfaces::set_task_assignment(&store, &view, &task_kind, &option)
                .await
                .map_err(preference_surfaces::delegation_failure)
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}
