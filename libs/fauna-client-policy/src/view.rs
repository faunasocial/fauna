//! The editor's snapshot — everything a mounting paints, with every label
//! already chosen (`nest-rules.md` § The shared rule editor: the apps paint a
//! list, selects, inputs and a status line, and decide nothing).

use fauna_core::content_policy::REASON_DEFAULT_KEY;
use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

use crate::{LEVELS, OFFERED_KINDS, OFFERED_VERDICTS, RuleEditor, RuleKind, RuleVerdict};

/// Everything the `rule-editor` component paints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RuleEditorView {
    /// One per rule, in document order (`rule-editor-item`).
    pub rows: Vec<RuleRow>,
    /// The kinds `rule-editor-item-kind-select` and the add menu offer.
    pub kinds: Vec<KindOption>,
    /// `rule-editor-item-category-select`'s options.
    pub categories: Vec<CategoryOption>,
    /// The labelers the document bundles — `rule-editor-item-labeler-select`
    /// offers these beside the publisher's subscriptions the app lists.
    pub labelers: Vec<LabelerOption>,
    /// `rule-editor-item-verdict-select`'s options.
    pub verdicts: Vec<VerdictOption>,
    /// `rule-editor-item-level-select`'s options; a row's `level_index`
    /// indexes this list.
    pub levels: Vec<LevelOption>,
    /// What stops a save, as sentences (`rule-editor-status`). Empty when the
    /// document may be saved.
    pub defects: Vec<LocalizedText>,
    /// False for a document a newer build wrote: shown, never edited.
    pub editable: bool,
    /// Whether `rule-editor-save-button` is enabled.
    pub can_save: bool,
    /// The reason language every rule requires.
    pub reason_default_key: String,
}

/// One rule. Exactly one of `category` / `keywords` / `labeler` / `prompt` is
/// set, by `kind`; a `list` row sets none of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RuleRow {
    /// 1-based — the number every defect sentence names.
    pub number: u32,
    pub kind: RuleKind,
    pub kind_label: LocalizedText,
    /// A loaded `list` rule: shown, removable, never edited.
    pub read_only: bool,
    pub category: Option<String>,
    /// The terms, one per line.
    pub keywords: Option<String>,
    /// The labeler's name; empty until one is chosen.
    pub labeler: Option<String>,
    pub prompt: Option<String>,
    pub verdict: RuleVerdict,
    /// Into [`RuleEditorView::levels`]; `None` where a level means nothing (a
    /// keywords rule). A stored per-mille between two levels shows as the
    /// nearer and is kept until the user picks one.
    pub level_index: Option<u32>,
    /// The publisher's own reason texts, the required default first.
    pub reasons: Vec<ReasonEntry>,
    pub reason_code: String,
    /// "Not applied on this device" — a rule this build cannot enforce.
    pub inert: Option<LocalizedText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReasonEntry {
    pub lang: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct KindOption {
    pub kind: RuleKind,
    pub label: LocalizedText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CategoryOption {
    pub category: String,
    pub label: LocalizedText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerOption {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct VerdictOption {
    pub verdict: RuleVerdict,
    pub label: LocalizedText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LevelOption {
    pub label: LocalizedText,
    pub permille: u16,
}

pub(crate) fn view(editor: &RuleEditor) -> RuleEditorView {
    RuleEditorView {
        rows: (0..editor.document.rules.len())
            .map(|i| editor.row(i))
            .collect(),
        kinds: OFFERED_KINDS
            .iter()
            .map(|&kind| KindOption {
                kind,
                label: kind.label(),
            })
            .collect(),
        categories: editor.category_options(),
        labelers: editor.labeler_options(),
        verdicts: OFFERED_VERDICTS
            .iter()
            .map(|&verdict| VerdictOption {
                verdict,
                label: verdict.label(),
            })
            .collect(),
        levels: LEVELS
            .iter()
            .map(|level| LevelOption {
                label: LocalizedText::key(level.key),
                permille: level.permille,
            })
            .collect(),
        defects: editor.validate(),
        editable: editor.is_editable(),
        can_save: editor.can_save(),
        reason_default_key: REASON_DEFAULT_KEY.to_string(),
    }
}
