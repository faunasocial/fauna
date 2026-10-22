//! `#[wasm_bindgen]` face of the **shared rule editor** (`fauna_client_policy`;
//! `docs/goal/behavior/nest-rules.md` § The shared rule editor) — the web twin
//! of the UniFFI `FfiRuleEditor` (`libs/fauna-ffi/src/rule_editor.rs`). The
//! working document lives in shared Rust; the SPA paints
//! [`WasmRuleEditor::view`] (`RuleEditorView`) and forwards the user's choices.
//!
//! A nest's or a region's document crosses as canonical dag-cbor bytes; the
//! guardian's rides the ward's content policy exactly as `familyStatus`
//! returns it and `familyPolicyUpdate` takes it, so the editor reads and
//! writes the object the family page already holds.

use std::cell::RefCell;

use fauna_client_policy::{RuleEditor, RuleKind, RuleVerdict};
use fauna_core::content_policy::{ContentPolicyDocument, Publisher, decode_document};
use fauna_core::obligation::ContentPolicy;
use fauna_core::region_authority::RegionCode;
use wasm_bindgen::prelude::*;

use crate::rpc::{from_js, to_js};

/// The shared rule editor, held in wasm.
#[wasm_bindgen]
pub struct WasmRuleEditor {
    editor: RefCell<RuleEditor>,
}

pub(crate) fn decode(bytes: &[u8]) -> Result<ContentPolicyDocument, JsValue> {
    fauna_protocol::decode_strict::<ContentPolicyDocument>(bytes)
        .or_else(|_| decode_document(bytes))
        .map_err(|e| JsValue::from_str(&format!("the rule document does not decode: {e}")))
}

fn open(document: Option<Vec<u8>>, publisher: Publisher) -> Result<WasmRuleEditor, JsValue> {
    let document = document.as_deref().map(decode).transpose()?;
    Ok(WasmRuleEditor {
        editor: RefCell::new(RuleEditor::load(document, publisher)),
    })
}

fn kind_of(kind: JsValue) -> Result<RuleKind, JsValue> {
    from_js(kind)
}

fn verdict_of(verdict: JsValue) -> Result<RuleVerdict, JsValue> {
    from_js(verdict)
}

#[wasm_bindgen]
impl WasmRuleEditor {
    /// The admin's editor over the nest `domain`'s document bytes; `null` when
    /// the nest publishes none.
    #[wasm_bindgen(js_name = forNest)]
    pub fn for_nest(domain: String, document: Option<Vec<u8>>) -> Result<WasmRuleEditor, JsValue> {
        open(document, Publisher::Nest(domain))
    }

    /// The guardian's editor over a ward's content policy (the `familyStatus`
    /// object, or `null`): its rule document with every floor still set shown
    /// as a rule beside it.
    #[wasm_bindgen(js_name = forGuardian)]
    pub fn for_guardian(content_policy: JsValue) -> Result<WasmRuleEditor, JsValue> {
        let policy: Option<ContentPolicy> =
            if content_policy.is_null() || content_policy.is_undefined() {
                None
            } else {
                Some(from_js(content_policy)?)
            };
        Ok(WasmRuleEditor {
            editor: RefCell::new(RuleEditor::load_guardian(policy.as_ref())),
        })
    }

    /// The authority's editor over region `region`'s policy document bytes.
    #[wasm_bindgen(js_name = forRegion)]
    pub fn for_region(
        region: String,
        document: Option<Vec<u8>>,
    ) -> Result<WasmRuleEditor, JsValue> {
        let region = RegionCode::parse(region)
            .map_err(|e| JsValue::from_str(&format!("not a region code: {e}")))?;
        open(document, Publisher::Region(region))
    }

    /// Everything the editor paints (`RuleEditorView`).
    pub fn view(&self) -> Result<JsValue, JsValue> {
        to_js(&self.editor.borrow().view())
    }

    /// `rule-editor-add-button` — `kind` is `"category"` | `"keywords"` |
    /// `"labeler"` | `"prompt"`; the new rule's index, or `undefined`.
    #[wasm_bindgen(js_name = addRule)]
    pub fn add_rule(&self, kind: JsValue) -> Result<Option<u32>, JsValue> {
        let kind = kind_of(kind)?;
        Ok(self.editor.borrow_mut().add_rule(kind).map(|i| i as u32))
    }

    #[wasm_bindgen(js_name = removeRule)]
    pub fn remove_rule(&self, index: u32) -> bool {
        self.editor.borrow_mut().remove_rule(index as usize)
    }

    #[wasm_bindgen(js_name = setKind)]
    pub fn set_kind(&self, index: u32, kind: JsValue) -> Result<bool, JsValue> {
        let kind = kind_of(kind)?;
        Ok(self.editor.borrow_mut().set_kind(index as usize, kind))
    }

    #[wasm_bindgen(js_name = setCategory)]
    pub fn set_category(&self, index: u32, category: String) -> bool {
        self.editor
            .borrow_mut()
            .set_category(index as usize, &category)
    }

    /// The terms, one per line.
    #[wasm_bindgen(js_name = setKeywords)]
    pub fn set_keywords(&self, index: u32, text: String) -> bool {
        self.editor
            .borrow_mut()
            .set_keywords_text(index as usize, &text)
    }

    /// The terms as a single-line input hands them: commas separate terms as
    /// line breaks do, so a one-line input decides nothing itself.
    #[wasm_bindgen(js_name = setKeywordsInline)]
    pub fn set_keywords_inline(&self, index: u32, text: String) -> bool {
        self.editor
            .borrow_mut()
            .set_keywords_inline(index as usize, &text)
    }

    /// A row's `keywords` (one term per line) as the single-line input shows
    /// them, comma-separated.
    #[wasm_bindgen(js_name = keywordsInline)]
    pub fn keywords_inline(terms: String) -> String {
        fauna_client_policy::keywords_inline(&terms)
    }

    /// The labeler's name and the module bytes from the publisher's
    /// subscription.
    #[wasm_bindgen(js_name = setLabeler)]
    pub fn set_labeler(&self, index: u32, name: String, module: Vec<u8>) -> bool {
        self.editor
            .borrow_mut()
            .set_labeler(index as usize, &name, module)
    }

    /// The labeler picker's options (`LabelerChoice[]`): the bundled labelers,
    /// then the publisher's subscribed ones (`subscribed`: hex ids). A choice
    /// with a `labeler_id` is fetched and passed to `setLabeler` under its
    /// name; one without to `chooseBundledLabeler`.
    #[wasm_bindgen(js_name = labelerChoices)]
    pub fn labeler_choices(&self, subscribed: Vec<String>) -> Result<JsValue, JsValue> {
        to_js(&self.editor.borrow().labeler_choices(&subscribed))
    }

    #[wasm_bindgen(js_name = chooseBundledLabeler)]
    pub fn choose_bundled_labeler(&self, index: u32, name: String) -> bool {
        self.editor
            .borrow_mut()
            .choose_bundled_labeler(index as usize, &name)
    }

    #[wasm_bindgen(js_name = setPrompt)]
    pub fn set_prompt(&self, index: u32, text: String) -> bool {
        self.editor.borrow_mut().set_prompt(index as usize, &text)
    }

    /// `verdict` is `"block"` | `"collapse"`.
    #[wasm_bindgen(js_name = setVerdict)]
    pub fn set_verdict(&self, index: u32, verdict: JsValue) -> Result<bool, JsValue> {
        let verdict = verdict_of(verdict)?;
        Ok(self
            .editor
            .borrow_mut()
            .set_verdict(index as usize, verdict))
    }

    /// An index into the view's `levels`.
    #[wasm_bindgen(js_name = setLevel)]
    pub fn set_level(&self, index: u32, level: u32) -> bool {
        self.editor
            .borrow_mut()
            .set_level(index as usize, level as usize)
    }

    /// The reason in `lang` (the view's `reason_default_key` for the required
    /// one); empty text removes it.
    #[wasm_bindgen(js_name = setReason)]
    pub fn set_reason(&self, index: u32, lang: String, text: String) -> bool {
        self.editor
            .borrow_mut()
            .set_reason(index as usize, &lang, &text)
    }

    /// The defects stopping a save, as `LocalizedText[]`.
    pub fn validate(&self) -> Result<JsValue, JsValue> {
        to_js(&self.editor.borrow().validate())
    }

    /// The document to save as canonical dag-cbor (the admin's
    /// `fauna.admin.content_policy.set`). Throws while `can_save` is false.
    #[wasm_bindgen(js_name = documentBytes)]
    pub fn document_bytes(&self) -> Result<Vec<u8>, JsValue> {
        let editor = self.editor.borrow();
        if !editor.can_save() {
            return Err(JsValue::from_str("the rules cannot be saved yet"));
        }
        editor.encoded().map_err(|e| JsValue::from_str(&e))
    }

    /// The document to save in the shape the ward's content policy carries it
    /// (`ContentPolicy.document` for `familyPolicyUpdate`). Throws while
    /// `can_save` is false.
    pub fn document(&self) -> Result<JsValue, JsValue> {
        let editor = self.editor.borrow();
        if !editor.can_save() {
            return Err(JsValue::from_str("the rules cannot be saved yet"));
        }
        to_js(&editor.document())
    }
}
