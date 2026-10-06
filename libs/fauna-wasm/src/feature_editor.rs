//! `#[wasm_bindgen]` face over the feature plane's **authoring** half
//! (`fauna_client_features::editor`; `docs/goal/architecture/dynamic-features.md`
//! § Authoring surfaces) — the web twin of the UniFFI `FfiPolicyEditor`
//! (`libs/fauna-ffi/src/features.rs`). One shared editor, exposed once at each
//! boundary (priority #2): the draft, the view-model, the parser and the
//! tier-to-kind choice all live in shared Rust, and the SPA paints
//! [`WasmPolicyEditor::view`] and forwards keystrokes.

use std::cell::RefCell;
use std::rc::Rc;

use fauna_client_features::{AuthoringTier, FeaturesClient, PolicyEditor, SaveError};
use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{WsRpcClient, err_to_js, to_js};

fn tier_of(key: &str) -> Result<AuthoringTier, JsValue> {
    AuthoringTier::from_key(key).ok_or_else(|| {
        JsValue::from_str(&format!(
            "unknown authoring tier {key:?} (expected \"admin\" or \"self\")"
        ))
    })
}

fn guardian_tier(ward: &[u8]) -> Result<AuthoringTier, JsValue> {
    let ward: [u8; 32] = ward.try_into().map_err(|_| {
        JsValue::from_str(&format!("a ward actor id is 32 bytes, got {}", ward.len()))
    })?;
    Ok(AuthoringTier::Guardian { ward })
}

/// The shared feature-policy editor, held in wasm. Open with
/// [`WasmPolicyEditor::open`]; the host page keeps the instance while
/// `feature-policy-editor` is open.
#[wasm_bindgen]
pub struct WasmPolicyEditor {
    editor: Rc<RefCell<PolicyEditor>>,
}

/// A write's verdict, as the SPA reads it: `{ status }` when the write landed
/// (the editor has already re-seeded from a fresh read), `{ invalid }` when the
/// shared parser refused a cell and nothing was dispatched. A nest refusal
/// rejects the promise instead.
#[derive(serde::Serialize)]
struct Verdict {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<fauna_core::localized::LocalizedText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalid: Option<fauna_core::localized::LocalizedText>,
}

#[wasm_bindgen]
impl WasmPolicyEditor {
    /// One tier's authored documents as host rows (`AuthoredRow[]`; `tier`:
    /// `"admin"` | `"self"`) — the authored-document read joined with the
    /// nest's capability set, unsupported members dropped.
    #[wasm_bindgen(js_name = authoredRows)]
    pub fn authored_rows(client: &WsRpcClient, tier: String) -> Promise {
        let nest = client.inner_client();
        future_to_promise(async move {
            let tier = tier_of(&tier)?;
            let surface = FeaturesClient::new(nest)
                .authored_surface(tier)
                .await
                .map_err(err_to_js)?;
            to_js(&surface.rows())
        })
    }

    /// Open the editor over one member at `tier`, seeded from the tier's
    /// AUTHORED document — resolves to a `WasmPolicyEditor`.
    pub fn open(client: &WsRpcClient, tier: String, feature: String) -> Promise {
        let nest = client.inner_client();
        future_to_promise(async move {
            let tier = tier_of(&tier)?;
            let surface = FeaturesClient::new(nest)
                .authored_surface(tier)
                .await
                .map_err(err_to_js)?;
            let editor = surface.editor(tier, &feature).ok_or_else(|| {
                JsValue::from_str(&format!("this nest does not carry the feature {feature:?}"))
            })?;
            Ok(JsValue::from(WasmPolicyEditor {
                editor: Rc::new(RefCell::new(editor)),
            }))
        })
    }

    /// The guardian host's rows for one ward (`AuthoredRow[]`) — the ward's
    /// `fauna.family.status` entry mapped to the rows the other two tiers
    /// read. `ward` is the ward's actor id, as `familyPolicyUpdate` takes it.
    #[wasm_bindgen(js_name = authoredRowsForWard)]
    pub fn authored_rows_for_ward(client: &WsRpcClient, ward: Vec<u8>) -> Promise {
        let nest = client.inner_client();
        future_to_promise(async move {
            let tier = guardian_tier(&ward)?;
            let surface = FeaturesClient::new(nest)
                .authored_surface(tier)
                .await
                .map_err(err_to_js)?;
            to_js(&surface.rows())
        })
    }

    /// Open the editor over one member at the guardian tier for `ward` — the
    /// ward-keyed twin of [`Self::open`] (the guardian tier has no bare key).
    #[wasm_bindgen(js_name = openForWard)]
    pub fn open_for_ward(client: &WsRpcClient, ward: Vec<u8>, feature: String) -> Promise {
        let nest = client.inner_client();
        future_to_promise(async move {
            let tier = guardian_tier(&ward)?;
            let surface = FeaturesClient::new(nest)
                .authored_surface(tier)
                .await
                .map_err(err_to_js)?;
            let editor = surface.editor(tier, &feature).ok_or_else(|| {
                JsValue::from_str(&format!(
                    "no editable limit for the feature {feature:?} on this ward"
                ))
            })?;
            Ok(JsValue::from(WasmPolicyEditor {
                editor: Rc::new(RefCell::new(editor)),
            }))
        })
    }

    /// Everything the editor paints (`PolicyEditorView`).
    pub fn view(&self) -> Result<JsValue, JsValue> {
        to_js(&self.editor.borrow().view())
    }

    /// `feature-policy-editor-on-radio` (`true`) / `-off-radio` (`false`).
    #[wasm_bindgen(js_name = setOn)]
    pub fn set_on(&self, on: bool) {
        self.editor.borrow_mut().set_on(on);
    }

    /// `feature-policy-editor-cell-input[index]` — one keystroke's new text.
    #[wasm_bindgen(js_name = setCell)]
    pub fn set_cell(&self, index: u32, text: String) {
        self.editor.borrow_mut().set_cell(index as usize, text);
    }

    /// `feature-policy-editor-save-button` — parse, write, re-read, re-seed.
    pub fn save(&self, client: &WsRpcClient) -> Promise {
        self.write(client, false)
    }

    /// `feature-policy-editor-remove-button` — the absent policy, re-read.
    pub fn remove(&self, client: &WsRpcClient) -> Promise {
        self.write(client, true)
    }
}

// Not exported: the shared body of save/remove.
impl WasmPolicyEditor {
    fn write(&self, client: &WsRpcClient, remove: bool) -> Promise {
        let nest = client.inner_client();
        let cell = Rc::clone(&self.editor);
        future_to_promise(async move {
            let editor = cell.borrow().clone();
            let features = FeaturesClient::new(nest);
            let result = if remove {
                features.remove(&editor).await
            } else {
                features.save(&editor).await
            };
            let verdict = match result {
                Ok(saved) => {
                    let next = editor.reseeded(&saved.reply);
                    if let Some(next) = next {
                        *cell.borrow_mut() = next;
                    }
                    Verdict {
                        status: Some(saved.status),
                        invalid: None,
                    }
                }
                Err(SaveError::Invalid(reason)) => Verdict {
                    status: None,
                    invalid: Some(reason),
                },
                Err(SaveError::Rpc(e)) => return Err(err_to_js(e)),
            };
            to_js(&verdict)
        })
    }
}
