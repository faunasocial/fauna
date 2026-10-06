//! The `wasm_admin_machine!` macro — shared by every `#[wasm_bindgen]` state-
//! machine wrapper in this crate (`src/mail_admin.rs`, `src/pairing.rs`, …).
//!
//! The wrappers are structurally identical (an `Rc<Machine>` + the same
//! `snapshot` / `hydrate` / `dispatch` surface); only the machine type, action
//! type, and build seam differ. Keeping the shape in one macro means adding a
//! method — or changing the dispatch contract — touches one place, not N.
//!
//! Expansion-site requirements (each invoking module must have these in scope —
//! see `mail_admin.rs` / `pairing.rs` for the canonical import block):
//! `std::rc::Rc`, `wasm_bindgen::prelude::*` (for `JsValue` + `#[wasm_bindgen]`),
//! `wasm_bindgen_futures::future_to_promise`, the crate's
//! `rpc::{err_to_js, from_js, to_js}`, and a `WsRpcClient as InnerClient` alias.

macro_rules! wasm_admin_machine {
    (
        $(#[$meta:meta])*
        $wrapper:ident, $machine:ty, $action:ty, $build:path $(,)?
    ) => {
        $(#[$meta])*
        #[wasm_bindgen]
        pub struct $wrapper {
            inner: Rc<$machine>,
        }

        impl $wrapper {
            /// Build over the SPA's browser WS-RPC client. Called by the
            /// `WsRpcClient` factory in `src/rpc.rs`.
            pub(crate) fn build(client: InnerClient) -> Self {
                Self {
                    inner: Rc::new($build(client)),
                }
            }
        }

        #[wasm_bindgen]
        impl $wrapper {
            /// The rendered snapshot as a plain JS object (sync).
            #[wasm_bindgen(js_name = snapshot)]
            pub fn snapshot(&self) -> Result<JsValue, JsValue> {
                to_js(&self.inner.snapshot())
            }

            /// Initial page load. Resolves `undefined`; read state via `snapshot()`.
            #[wasm_bindgen(js_name = hydrate)]
            pub fn hydrate(&self) -> js_sys::Promise {
                let m = self.inner.clone();
                future_to_promise(async move {
                    m.hydrate().await.map_err(err_to_js)?;
                    Ok(JsValue::UNDEFINED)
                })
            }

            /// Dispatch an action (a JS object decoded into the machine's action
            /// enum). Resolves `undefined` on success; read state via `snapshot()`.
            #[wasm_bindgen(js_name = dispatch)]
            pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
                let m = self.inner.clone();
                future_to_promise(async move {
                    let action: $action = from_js(action)?;
                    m.dispatch(action).await.map_err(err_to_js)?;
                    Ok(JsValue::UNDEFINED)
                })
            }
        }
    };
}
