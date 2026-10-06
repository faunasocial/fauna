//! WASM bindings for the Fauna connected-apps page — the page-level
//! `ConnectedAppsMachine` (the roster, the Requests tray, Connect an app,
//! per-client blocks; `docs/goal/ui/connected-apps.md`). The web route renders
//! off the shared machine via `$lib/wasm-connected-apps`, exactly as the
//! labeler-catalog route renders off `LabelerCatalogMachine` — one shared
//! surface, all 7 apps (priority #1/#2). Mirrors `fauna-wasm-labeler-catalog`.

#[cfg(target_arch = "wasm32")]
mod bindings {
    use std::sync::Arc;

    use wasm_bindgen::prelude::*;

    use fauna_client_connected_apps::{
        ConnectedAppsMachine as InnerMachine, ConnectedAppsObserver as InnerObserver,
    };

    #[wasm_bindgen]
    extern "C" {
        pub type JsConnectedAppsObserver;
        #[wasm_bindgen(method, js_name = onChanged)]
        fn on_changed(this: &JsConnectedAppsObserver);
    }

    struct ObserverShim(JsConnectedAppsObserver);
    // SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
    unsafe impl Send for ObserverShim {}
    unsafe impl Sync for ObserverShim {}
    impl InnerObserver for ObserverShim {
        fn on_changed(&self) {
            self.0.on_changed()
        }
    }

    #[wasm_bindgen]
    pub struct ConnectedAppsMachine(Arc<InnerMachine>);

    #[wasm_bindgen]
    impl ConnectedAppsMachine {
        /// Build the page machine over the SPA core chunk's socket, lent as
        /// `port` (a `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`), so web
        /// keeps one WebSocket per actor. Throws on an object that is not a
        /// port. State starts empty; call `refresh()` to populate it.
        #[wasm_bindgen(constructor)]
        pub fn new(
            observer: JsConnectedAppsObserver,
            port: fauna_rpc_wasm::JsRpcPort,
        ) -> Result<ConnectedAppsMachine, JsValue> {
            let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
            let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            // No mail machine yet: the mail app-password rows reach web with
            // its connected-apps leg, which decides how this chunk reaches the
            // session's mail-settings machine (another chunk's object cannot
            // cross into this one). Until then the roster has no mail rows.
            Ok(ConnectedAppsMachine(
                fauna_client_connected_apps::build_connected_apps_machine(client, observer, None),
            ))
        }

        /// The whole renderable page in one JSON object
        /// (`{ loaded, requests, principals, blocked, error }`).
        #[wasm_bindgen(js_name = snapshotJson)]
        pub fn snapshot_json(&self) -> String {
            serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
        }

        /// Re-read the roster, the pending requests and the blocks.
        #[wasm_bindgen(js_name = refresh)]
        pub fn refresh(&self) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.refresh().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Connect an app: look up the typed code.
        #[wasm_bindgen(js_name = submitCode)]
        pub fn submit_code(&self, code: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.submit_code(code).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// The same-device handoff: open the request whose PAR handle a
        /// `fauna://consent/<request_uri>` route carried.
        #[wasm_bindgen(js_name = openHandoff)]
        pub fn open_handoff(&self, request_uri: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.open_handoff(request_uri).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Approve or deny the pending request `consent_id_hex`.
        #[wasm_bindgen(js_name = resolveRequest)]
        pub fn resolve_request(&self, consent_id_hex: String, approved: bool) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.resolve_request(consent_id_hex, approved).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// "Never show requests from this app" for the request `consent_id_hex`.
        #[wasm_bindgen(js_name = blockRequest)]
        pub fn block_request(&self, consent_id_hex: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.block_request(consent_id_hex).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Lift the block on `client_id`.
        #[wasm_bindgen(js_name = unblock)]
        pub fn unblock(&self, client_id: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.unblock(client_id).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Revoke the roster row `key` — the machine picks the verb.
        #[wasm_bindgen(js_name = revoke)]
        pub fn revoke(&self, key: String) -> js_sys::Promise {
            let inner = Arc::clone(&self.0);
            wasm_bindgen_futures::future_to_promise(async move {
                inner.revoke(key).await;
                Ok(JsValue::UNDEFINED)
            })
        }
    }

    // Each wasm chunk is its own module with its own Rust runtime, so the panic
    // hook is installed per chunk (see the `fauna-wasm-panic-hook` crate doc).
    #[wasm_bindgen(start)]
    fn panic_hook_start() {
        fauna_wasm_panic_hook::install("fauna-wasm-connected-apps");
    }

    /// Test-only: deliberately panics, so an e2e can assert the hook above names
    /// this chunk. Compiled out of every non-`test-helpers` build.
    #[cfg(feature = "test-helpers")]
    #[wasm_bindgen(js_name = panicForTestOnly)]
    pub fn panic_for_test_only() {
        panic!("deliberate test panic");
    }
}
