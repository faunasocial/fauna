//! `#[wasm_bindgen]` face of the member's **nest-rules book**
//! (`fauna_client_policy::NestRulesBook`; `docs/goal/behavior/nest-rules.md`
//! § The nest content policy → *Carriage*, § Acceptance at joining, and a
//! change of rules) — the web twin of the UniFFI `FfiNestRulesBook`
//! (`libs/fauna-ffi/src/nest_rules.rs`). The SPA hands each `fauna.nest.info`
//! reply's nest id, domain and `moderation.content_policy` to
//! [`WasmNestRulesBook::read`], passes [`WasmNestRulesBook::rule_sets_for`] to
//! `contentRenderComposedWithNest`, and paints [`WasmNestRulesBook::summary`].

use std::cell::RefCell;
use std::rc::Rc;

use fauna_client_policy::{NestRulesBook, NestRulesRead, SignedNestRules, StoredNestRules};
use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};
use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::rpc::{WsRpcClient, from_js, to_js};

/// The book, held in wasm — one per app, over every nest it is signed into.
#[wasm_bindgen]
pub struct WasmNestRulesBook {
    book: Rc<RefCell<NestRulesBook>>,
}

impl WasmNestRulesBook {
    /// The shared book, for the composed render in `region.rs`.
    pub(crate) fn book(&self) -> Rc<RefCell<NestRulesBook>> {
        Rc::clone(&self.book)
    }
}

impl Default for WasmNestRulesBook {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl WasmNestRulesBook {
    /// An empty book.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            book: Rc::new(RefCell::new(NestRulesBook::default())),
        }
    }

    /// The book rebuilt from `stored()`'s records, each verified again.
    pub fn restore(records: JsValue) -> Result<WasmNestRulesBook, JsValue> {
        let records: Vec<StoredNestRules> = from_js(records)?;
        Ok(Self {
            book: Rc::new(RefCell::new(NestRulesBook::restore(records))),
        })
    }

    /// Read one self-description. `document`/`signature` absent → the nest
    /// published no policy. `sequence`/`issuedAt` are numbers (no `bigint`).
    /// Returns `{ kind: "no_rules" | "first" | "unchanged" | "changed" |
    /// "refused", sequence?, domain?, reason? }`; on `changed` show the notice
    /// naming `domain` (`nest_rules.changed_notice`); `reason` is for the log.
    pub fn read(
        &self,
        nest_id: String,
        domain: String,
        document: Option<Vec<u8>>,
        sequence: f64,
        issued_at: f64,
        signature: Option<Vec<u8>>,
    ) -> Result<JsValue, JsValue> {
        let signed = match (&document, &signature) {
            (Some(document), Some(signature)) => Some(SignedNestRules {
                document,
                // Wire integers far below 2^53: a cast, not a parse.
                sequence: sequence as u64,
                issued_at: issued_at as i64,
                signature,
            }),
            _ => None,
        };
        let read = self.book.borrow_mut().read(&nest_id, &domain, signed);
        to_js(&read_json(read))
    }

    /// Read the session nest's self-description (`fauna.nest.info`, asked
    /// afresh — the web client keeps no memo) and fold it: the one read the SPA
    /// makes at sign-in and again on every `nest_info` push and reconnect.
    /// Resolves `{ nestId, domain, read }` with `read` as [`Self::read`]'s
    /// answer — show the changed notice on `kind: "changed"`.
    pub fn refresh(&self, client: &WsRpcClient) -> Promise {
        let book = Rc::clone(&self.book);
        let nest = client.inner_client();
        future_to_promise(async move {
            let info: NestInfoReply = nest
                .request("fauna.nest.info", NestInfoRequest::default())
                .await
                .map_err(crate::rpc::err_to_js)?;
            let signed = info
                .moderation
                .content_policy
                .as_ref()
                .map(|p| SignedNestRules {
                    document: &p.document,
                    sequence: p.sequence,
                    issued_at: p.issued_at,
                    signature: &p.signature,
                });
            let read = book.borrow_mut().read(&info.nest_id, &info.domain, signed);
            to_js(&serde_json::json!({
                "nestId": info.nest_id,
                "domain": info.domain,
                "read": read_json(read),
            }))
        })
    }

    /// The rule sets to fold for an item read through the nest `nestId` names.
    #[wasm_bindgen(js_name = ruleSetsFor)]
    pub fn rule_sets_for(&self, nest_id: String) -> Result<JsValue, JsValue> {
        to_js(&self.book.borrow().rule_sets_for(&nest_id))
    }

    /// An item's labels (`ContentLabelEntry[]`) with the factors the nest's
    /// bundled scorers produce over it joined — the `labels` to pass to
    /// `contentRenderComposedWithNest` for an item read through the nest
    /// `nestId` names. The scorer input is the region plane's: `contentIdHex`
    /// (`null` for an item without one), `authorHex` (`null` → the zero id),
    /// the plaintext, hashtags and whether it carries media. The keyword rules
    /// match the text; the `wasm` labelers run over the whole input.
    #[wasm_bindgen(js_name = joinLabels)]
    #[allow(clippy::too_many_arguments)]
    pub fn join_labels(
        &self,
        nest_id: String,
        labels: JsValue,
        content_id_hex: Option<String>,
        author_hex: Option<String>,
        text: String,
        hashtags: Vec<String>,
        has_media: bool,
    ) -> Result<JsValue, JsValue> {
        let labels: Vec<fauna_core::content_category::ContentLabelEntry> = from_js(labels)?;
        let id = content_id_hex
            .as_deref()
            .and_then(|h| fauna_core::hex32::decode(h).ok());
        let book = self.book.borrow();
        let joined = book.join_labels(&nest_id, id.as_ref(), &labels, || {
            let author = author_hex
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(fauna_core::identity::ActorId)
                .unwrap_or(fauna_core::identity::ActorId([0; 32]));
            fauna_core::scoring::LabelerPostInput::from_card(author, &text, &hashtags, has_media)
        });
        to_js(&joined.into_owned())
    }

    /// The rules in force for the nest as the joining and settings surfaces
    /// paint them in `lang` (`NestRulesSummary`); `null` when it published none.
    pub fn summary(&self, nest_id: String, lang: String) -> Result<JsValue, JsValue> {
        let summary = self.book.borrow().get(&nest_id).map(|a| a.summary(&lang));
        to_js(&summary)
    }

    /// Drop a nest's rules — the app signed out of it.
    pub fn forget(&self, nest_id: String) {
        self.book.borrow_mut().forget(&nest_id);
    }

    /// Every accepted record, for the SPA to persist.
    pub fn stored(&self) -> Result<JsValue, JsValue> {
        to_js(&self.book.borrow().stored())
    }
}

/// One read's answer, as `read` and `refresh` resolve it.
fn read_json(read: NestRulesRead) -> serde_json::Value {
    match read {
        NestRulesRead::NoRules => serde_json::json!({ "kind": "no_rules" }),
        NestRulesRead::First { sequence } => {
            serde_json::json!({ "kind": "first", "sequence": sequence })
        }
        NestRulesRead::Unchanged => serde_json::json!({ "kind": "unchanged" }),
        NestRulesRead::Changed { domain, sequence } => {
            serde_json::json!({ "kind": "changed", "domain": domain, "sequence": sequence })
        }
        NestRulesRead::Refused(refusal) => {
            serde_json::json!({ "kind": "refused", "reason": format!("{refusal:?}") })
        }
    }
}
