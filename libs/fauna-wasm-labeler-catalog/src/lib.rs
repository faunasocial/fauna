//! WASM bindings for the Fauna community-labeler-catalog page — the
//! page-level `LabelerCatalogMachine` (browse / inspect-before-subscribe /
//! (un)subscribe). The web labeler-catalog route (and the personalization
//! home's subscribed-labelers facet, which reads the same snapshot) renders
//! off the shared `LabelerCatalogMachine` via `$lib/wasm-labeler-catalog`,
//! exactly as the Devices route renders off `DevicesMachine` via
//! `$lib/wasm-folders` — one shared surface, all 7 apps (priority #1/#2;
//! `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3).
//! Mirrors the page-level wrapper half of `fauna-wasm-folders` /
//! `fauna-wasm-media`.

use std::sync::Arc;

use wasm_bindgen::prelude::*;

use fauna_core::identity::ActorKeypair;
use fauna_labeler_catalog_machine::{
    LabelerCatalogMachine as InnerMachine, LabelerCatalogObserver as InnerObserver,
};

#[wasm_bindgen]
extern "C" {
    pub type JsLabelerCatalogObserver;
    #[wasm_bindgen(method, js_name = onChanged)]
    fn on_changed(this: &JsLabelerCatalogObserver);
}

struct ObserverShim(JsLabelerCatalogObserver);
// SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
unsafe impl Send for ObserverShim {}
unsafe impl Sync for ObserverShim {}
impl InnerObserver for ObserverShim {
    fn on_changed(&self) {
        self.0.on_changed()
    }
}

#[wasm_bindgen]
pub struct LabelerCatalogMachine(Arc<InnerMachine>);

#[wasm_bindgen]
impl LabelerCatalogMachine {
    /// Build the page machine over the SPA core chunk's socket, lent as
    /// `port` (a `SharedRpcPort` — `$lib/rpc`'s `sharedRpcPort`; the owner's
    /// `requestRaw` runs every request this machine makes, so web keeps one
    /// WebSocket per actor). Throws on an object that is not a port. `secret`
    /// is the actor's 32-byte ed25519 seed: the machine is built **with its
    /// grant seams**, so subscribing a `wasm` mail labeler mints the
    /// per-labeler grant and unsubscribing revokes it
    /// (`content-moderation-and-ranking.md` § Tier-3 → *Subscribing = minting
    /// a capability*) — the keypair signs the grant-log events, which record
    /// through the account's succession ledger (`fauna.state.succession-ledger`,
    /// the succession-ledger seam), and mints it from the account's mail
    /// custody (`fauna.state.mail`, the MSEK, the mail seam). Both are read
    /// through `account` — the tab's account port (`$lib/account-runtime`'s
    /// `sharedAccountPort`, minted for this secret's account), which reaches
    /// the core chunk's runtime. Throws on a secret that is not 32 bytes, or
    /// on an object that is not a port.
    /// State starts empty; call `refresh()` to populate it.
    #[wasm_bindgen(constructor)]
    pub fn new(
        observer: JsLabelerCatalogObserver,
        port: fauna_rpc_wasm::JsRpcPort,
        secret: Vec<u8>,
        account: fauna_account_port::JsAccountPort,
    ) -> Result<LabelerCatalogMachine, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        let observer: Arc<dyn InnerObserver> = Arc::new(ObserverShim(observer));
        let client = fauna_rpc_wasm::WsRpcClient::over_port(port.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // Neither the port nor `JsAccountTransport` is `Clone`: each seam's
        // forwarder wraps its own handle on the one JS port object.
        let ledger = fauna_client_config::succession_ledger_port::from_js_port(
            account.clone().unchecked_into(),
            keypair.actor_id(),
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mail = fauna_account_port::JsAccountTransport::new(account.into())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(LabelerCatalogMachine(
            fauna_labeler_catalog_machine::build_labeler_catalog_machine_with_grants(
                client,
                keypair,
                ledger,
                Arc::new(fauna_client_config::mail_port::PortMailStore::new(mail)),
                observer,
            ),
        ))
    }

    /// The whole renderable labeler-catalog page in one JSON object
    /// (`{ entries, inspecting, error, loaded }`). The personalization home's
    /// subscribed-labelers facet is the same `entries`, client-filtered to
    /// `subscribed == true`.
    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(&self.0.snapshot()).unwrap_or_default()
    }

    /// Re-read the full catalog (`fauna.labelers.list`). Resolves when done
    /// (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = refresh)]
    pub fn refresh(&self) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.refresh().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Open the inspect-before-subscribe panel for the catalog row at `index`.
    /// Resolves when done (read `snapshotJson().inspecting`).
    #[wasm_bindgen(js_name = inspect)]
    pub fn inspect(&self, index: u32) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.inspect(index).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Close the inspect panel (`labeler-inspect-close-button`).
    #[wasm_bindgen(js_name = closeInspect)]
    pub fn close_inspect(&self) {
        self.0.close_inspect()
    }

    /// Subscribe to the catalog row at `index`, then refresh. Resolves when
    /// done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = subscribe)]
    pub fn subscribe(&self, index: u32) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.subscribe(index).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Unsubscribe from the catalog row at `index`, then refresh. Resolves
    /// when done (read `snapshotJson` for the result / `error`).
    #[wasm_bindgen(js_name = unsubscribe)]
    pub fn unsubscribe(&self, index: u32) -> js_sys::Promise {
        let inner = Arc::clone(&self.0);
        wasm_bindgen_futures::future_to_promise(async move {
            inner.unsubscribe(index).await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

// ── Panic hook ───────────────────────────────────────────────────────────
//
// Each wasm chunk is its own module with its own Rust runtime, so a hook
// installed in one chunk covers none of the others (see the
// `fauna-wasm-panic-hook` crate doc comment). `#[wasm_bindgen(start)]` runs
// automatically the moment this chunk's module is instantiated — no SPA-side
// call site to add or remember, unlike `fauna-wasm`'s explicit `installLogging`.
#[wasm_bindgen(start)]
fn panic_hook_start() {
    fauna_wasm_panic_hook::install("fauna-wasm-labeler-catalog");
}

/// Test-only: deliberately panics, so an e2e can assert the hook above really
/// names this chunk in the browser console — a headless witness, not a
/// review-only claim. Compiled out of every non-`test-helpers` build.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = panicForTestOnly)]
pub fn panic_for_test_only() {
    panic!("deliberate test panic");
}
