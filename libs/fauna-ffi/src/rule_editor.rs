//! UniFFI face of the **shared rule editor** (`fauna_client_policy`;
//! `docs/goal/behavior/nest-rules.md` § The shared rule editor): the working
//! document lives HERE, in shared Rust, and each mounting — the admin's on
//! `admin-nest`, the guardian's on `family`, the authority's — forwards the
//! user's choices and paints [`FfiRuleEditor::view`]. One editor, one face; the
//! web twin is `fauna-wasm`'s `WasmRuleEditor`.
//!
//! Documents cross as their canonical dag-cbor, the bytes every carriage of
//! the grammar takes. Saving is the mounting's own write
//! (`fauna.admin.content_policy.set`, the family policy update, the
//! authority's signing path); the editor only says whether a save may go.
//!
//! Gated `rule-editor`: the view embeds bare `LocalizedText`, which
//! `uniffi-bindgen-go` cannot emit, and the Go mail-bridge edits no rules.

use std::sync::{Arc, Mutex};

use fauna_client_policy::{RuleEditor, RuleEditorView, RuleKind, RuleVerdict};
use fauna_core::content_policy::{ContentPolicyDocument, Publisher, decode_document};
use fauna_core::localized::LocalizedText;
use fauna_core::obligation::ContentPolicy;
use fauna_core::region_authority::RegionCode;

use crate::FfiAdminClient;
use crate::family::FfiContentPolicy;
use crate::{FfiError, general_err};

/// The shared rule editor, held across the boundary. Open with one of the
/// three constructors — one per publisher.
#[derive(uniffi::Object)]
pub struct FfiRuleEditor {
    editor: Mutex<RuleEditor>,
}

// Helpers in a PLAIN impl: a `#[uniffi::export]` block exports its private
// methods too.
impl FfiRuleEditor {
    fn with<T>(&self, f: impl FnOnce(&mut RuleEditor) -> T) -> T {
        f(&mut self.editor.lock().expect("rule editor lock"))
    }

    fn open(document: Option<Vec<u8>>, publisher: Publisher) -> Result<Arc<Self>, FfiError> {
        let document = document.map(|bytes| decode(&bytes)).transpose()?;
        Ok(Arc::new(Self {
            editor: Mutex::new(RuleEditor::load(document, publisher)),
        }))
    }
}

/// A carried document back from its bytes — in full first, so a newer
/// build's document is shown whole, then version first.
pub(crate) fn decode(bytes: &[u8]) -> Result<ContentPolicyDocument, FfiError> {
    fauna_protocol::decode_strict::<ContentPolicyDocument>(bytes)
        .or_else(|_| decode_document(bytes))
        .map_err(|e| general_err(format!("the rule document does not decode: {e}")))
}

#[uniffi::export]
impl FfiRuleEditor {
    /// The admin's editor over the nest `domain`'s document
    /// (`AdminContentPolicyGetReply`'s, as canonical bytes); `None` when the
    /// nest publishes none.
    #[uniffi::constructor]
    pub fn for_nest(domain: String, document: Option<Vec<u8>>) -> Result<Arc<Self>, FfiError> {
        Self::open(document, Publisher::Nest(domain))
    }

    /// The guardian's editor over a ward's content policy: its rule document
    /// with every floor still set shown as a rule beside it. `None` for a
    /// ward with no content policy.
    #[uniffi::constructor]
    pub fn for_guardian(policy: Option<FfiContentPolicy>) -> Arc<Self> {
        let policy: Option<ContentPolicy> = policy.map(Into::into);
        Arc::new(Self {
            editor: Mutex::new(RuleEditor::load_guardian(policy.as_ref())),
        })
    }

    /// The authority's editor over region `region`'s policy document.
    #[uniffi::constructor]
    pub fn for_region(region: String, document: Option<Vec<u8>>) -> Result<Arc<Self>, FfiError> {
        let region = RegionCode::parse(region)
            .map_err(|e| general_err(format!("not a region code: {e}")))?;
        Self::open(document, Publisher::Region(region))
    }

    /// Everything the editor paints.
    pub fn view(&self) -> RuleEditorView {
        self.with(|e| e.view())
    }

    /// `rule-editor-add-button` — the new rule's index, or `None` when the
    /// kind is not offered or the document is not editable here.
    pub fn add_rule(&self, kind: RuleKind) -> Option<u32> {
        self.with(|e| e.add_rule(kind)).map(|i| i as u32)
    }

    /// `rule-editor-item-remove-button[index]`.
    pub fn remove_rule(&self, index: u32) -> bool {
        self.with(|e| e.remove_rule(index as usize))
    }

    /// `rule-editor-item-kind-select[index]`.
    pub fn set_kind(&self, index: u32, kind: RuleKind) -> bool {
        self.with(|e| e.set_kind(index as usize, kind))
    }

    /// `rule-editor-item-category-select[index]`.
    pub fn set_category(&self, index: u32, category: String) -> bool {
        self.with(|e| e.set_category(index as usize, &category))
    }

    /// `rule-editor-item-keywords-input[index]` — the terms, one per line.
    pub fn set_keywords(&self, index: u32, text: String) -> bool {
        self.with(|e| e.set_keywords_text(index as usize, &text))
    }

    /// `rule-editor-item-keywords-input[index]` as a single-line input hands
    /// it: commas separate terms as line breaks do.
    pub fn set_keywords_inline(&self, index: u32, text: String) -> bool {
        self.with(|e| e.set_keywords_inline(index as usize, &text))
    }

    /// `rule-editor-item-labeler-select[index]` — the labeler's name and the
    /// module bytes from the publisher's subscription.
    pub fn set_labeler(&self, index: u32, name: String, module: Vec<u8>) -> bool {
        self.with(|e| e.set_labeler(index as usize, &name, module))
    }

    /// `rule-editor-item-labeler-select`'s options: the bundled labelers, then
    /// the publisher's subscribed ones (`subscribed`: hex ids). A choice with
    /// a `labeler_id` is fetched and handed to [`Self::set_labeler`] under its
    /// name; one without is passed to [`Self::choose_bundled_labeler`].
    pub fn labeler_choices(
        &self,
        subscribed: Vec<String>,
    ) -> Vec<fauna_client_policy::LabelerChoice> {
        self.with(|e| e.labeler_choices(&subscribed))
    }

    /// Point rule `index` at a labeler the document already bundles.
    pub fn choose_bundled_labeler(&self, index: u32, name: String) -> bool {
        self.with(|e| e.choose_bundled_labeler(index as usize, &name))
    }

    /// `rule-editor-item-prompt-input[index]`.
    pub fn set_prompt(&self, index: u32, text: String) -> bool {
        self.with(|e| e.set_prompt(index as usize, &text))
    }

    /// `rule-editor-item-verdict-select[index]`.
    pub fn set_verdict(&self, index: u32, verdict: RuleVerdict) -> bool {
        self.with(|e| e.set_verdict(index as usize, verdict))
    }

    /// `rule-editor-item-level-select[index]` — an index into the view's
    /// `levels`.
    pub fn set_level(&self, index: u32, level: u32) -> bool {
        self.with(|e| e.set_level(index as usize, level as usize))
    }

    /// `rule-editor-item-reason-input[index]` — the reason in `lang` (the
    /// view's `reason_default_key` for the required one); empty removes it.
    pub fn set_reason(&self, index: u32, lang: String, text: String) -> bool {
        self.with(|e| e.set_reason(index as usize, &lang, &text))
    }

    /// The defects stopping a save, as sentences (`rule-editor-status`).
    pub fn validate(&self) -> Vec<LocalizedText> {
        self.with(|e| e.validate())
    }

    /// The document to save, as canonical dag-cbor — the admin's
    /// `fauna.admin.content_policy.set`, the guardian's
    /// `FfiContentPolicy.document`. Refused while the view's `can_save` is
    /// false, so no mounting can write a document the validator refuses.
    pub fn document_bytes(&self) -> Result<Vec<u8>, FfiError> {
        self.with(|e| {
            if !e.can_save() {
                return Err(general_err("the rules cannot be saved yet"));
            }
            e.encoded().map_err(general_err)
        })
    }
}

/// The nest's rules as the admin's mounting reads and writes them
/// (`fauna.admin.content_policy.get` / `.set`): the document as canonical
/// bytes for [`FfiRuleEditor::for_nest`], with the nest's sequence and time.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAdminContentPolicy {
    /// `None` on a fresh install, or on a refused write.
    pub document: Option<Vec<u8>>,
    pub sequence: u64,
    pub issued_at: i64,
    /// A write the nest refused: the shared validator's sentence. Always
    /// `None` on a read.
    pub defect: Option<String>,
}

impl FfiAdminContentPolicy {
    fn of(policy: Option<fauna_protocol::content_policy::SignedContentPolicy>) -> Self {
        match policy {
            Some(p) => Self {
                document: Some(p.document),
                sequence: p.sequence,
                issued_at: p.issued_at,
                defect: None,
            },
            None => Self {
                document: None,
                sequence: 0,
                issued_at: 0,
                defect: None,
            },
        }
    }
}

// The admin mounting's two kinds, in their own gated block so the ungated
// admin face stays free of the editor's types.
#[fauna_uniffi_async::export]
impl FfiAdminClient {
    /// `fauna.admin.content_policy.get` — the nest's rules as last accepted.
    pub async fn content_policy(&self) -> Result<FfiAdminContentPolicy, FfiError> {
        let reply = self
            .client()
            .content_policy()
            .await
            .map_err(crate::stringify)?;
        Ok(FfiAdminContentPolicy::of(reply.policy))
    }

    /// `fauna.admin.content_policy.set` — replace the nest's whole rules with
    /// [`FfiRuleEditor::document_bytes`]. A document the nest refuses comes
    /// back as `defect`, not an error.
    pub async fn set_content_policy(
        &self,
        document: Vec<u8>,
    ) -> Result<FfiAdminContentPolicy, FfiError> {
        let document = decode(&document)?;
        let reply = self
            .client()
            .set_content_policy(document)
            .await
            .map_err(crate::stringify)?;
        let mut out = FfiAdminContentPolicy::of(reply.policy);
        out.defect = reply.defect;
        Ok(out)
    }
}

/// The labeler picker's reads (`nest-rules.md` § The shared rule editor: a
/// labeler rule names one of the publisher's subscriptions), over the same
/// authenticated connection as the labeler catalog.
#[derive(uniffi::Object)]
pub struct FfiRuleLabelersClient {
    nest: Arc<fauna_client::NestClient>,
}

/// A row's `keywords` (one term per line) as a single-line input shows them,
/// comma-separated — the form [`FfiRuleEditor::set_keywords_inline`] reads back.
#[uniffi::export]
pub fn rule_keywords_inline(terms: String) -> String {
    fauna_client_policy::keywords_inline(&terms)
}

/// Build the picker's reads over `nest`'s connection.
#[uniffi::export]
pub fn build_rule_labelers_client(nest: Arc<crate::FfiNestClient>) -> Arc<FfiRuleLabelersClient> {
    Arc::new(FfiRuleLabelersClient {
        nest: nest.nest_arc(),
    })
}

#[fauna_uniffi_async::export]
impl FfiRuleLabelersClient {
    /// The caller's subscribed `wasm` labelers over posts, as hex ids — the
    /// `subscribed` argument of [`FfiRuleEditor::labeler_choices`].
    pub async fn rule_labelers(&self) -> Result<Vec<String>, FfiError> {
        fauna_client_labelers::LabelersClient::new(Arc::clone(&self.nest))
            .rule_labelers()
            .await
            .map_err(crate::stringify)
    }

    /// One labeler's module for a rule to bundle (hand it to
    /// [`FfiRuleEditor::set_labeler`] under the choice's name), or `None` when
    /// it is not a `wasm` module over posts.
    pub async fn rule_module(&self, labeler_id: Vec<u8>) -> Result<Option<Vec<u8>>, FfiError> {
        fauna_client_labelers::LabelersClient::new(Arc::clone(&self.nest))
            .rule_module(labeler_id)
            .await
            .map_err(crate::stringify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guardian_edit_crosses_back_as_the_policy_document() {
        let editor = FfiRuleEditor::for_guardian(None);
        let i = editor.add_rule(RuleKind::Keywords).unwrap();
        assert!(editor.document_bytes().is_err(), "an empty keyword rule");
        editor.set_keywords(i, "casino".into());
        editor.set_reason(i, "default".into(), "Gambling".into());
        let bytes = editor.document_bytes().unwrap();

        let policy = FfiContentPolicy {
            nsfw: "inherit".into(),
            spam: "inherit".into(),
            phishing: "inherit".into(),
            commercial: "inherit".into(),
            document: Some(bytes.clone()),
        };
        let shared: ContentPolicy = policy.clone().into();
        assert_eq!(shared.document.as_ref().unwrap().rules.len(), 1);
        let back: FfiContentPolicy = shared.into();
        assert_eq!(back.document, Some(bytes));

        let reopened = FfiRuleEditor::for_guardian(Some(policy));
        assert_eq!(reopened.view().rows[0].keywords.as_deref(), Some("casino"));
    }

    #[test]
    fn unreadable_guardian_bytes_fail_closed() {
        let policy: ContentPolicy = FfiContentPolicy {
            nsfw: "inherit".into(),
            spam: "inherit".into(),
            phishing: "inherit".into(),
            commercial: "inherit".into(),
            document: Some(vec![0xff]),
        }
        .into();
        let rules = fauna_core::obligation::rules_from_guardian_policy(&policy);
        assert_eq!(rules.len(), 4);
        assert!(
            rules
                .iter()
                .all(|r| r.action == fauna_core::obligation::ObligationAction::Block)
        );
    }

    #[test]
    fn a_nest_editor_refuses_bytes_that_are_not_a_document() {
        assert!(FfiRuleEditor::for_nest("example.test".into(), Some(vec![0xff])).is_err());
        let editor = FfiRuleEditor::for_nest("example.test".into(), None).unwrap();
        assert_eq!(editor.add_rule(RuleKind::List), None);
    }
}
