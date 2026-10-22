//! The **shared rule editor** model — one editor for the three publishers of
//! the content-policy grammar: a nest admin, a guardian and a region's
//! authority.
//!
//! Owner doc: `docs/goal/behavior/nest-rules.md` § The shared rule editor. The
//! grammar it edits, and its one structural validator, are
//! [`fauna_core::content_policy`]'s; this crate adds no second validator. It
//! loads a document for a [`Publisher`], edits it in place (so every field a
//! newer build added — each `extra` — survives the round trip), validates it
//! with [`ContentPolicyDocument::status`] alone, and hands the document back to
//! be carried. Every rendering decision is here — the kinds offered, the level
//! scale, the defect sentences, the "not applied on this device" wording — so
//! each app paints [`RuleEditorView`] and forwards the user's choices (priority
//! #2).
//!
//! What the editor never does (`nest-rules.md` § The shared rule editor): show
//! content, accept a content id, or offer a verdict the engine lacks. The `list`
//! kind is offered to no publisher; a loaded region document's list rule is
//! kept, shown read-only and removable.
//!
//! Each non-category rule names a scorer the document bundles: the editor
//! mints one when the rule is added (its bytes empty until the user writes the
//! subject, which [`ContentPolicyDocument::status`] reports as the defect it
//! is), keeps a keywords or prompt scorer to one rule, lets several rules share
//! a labeler, and drops a scorer an edit left no rule naming.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_policy");

use std::collections::BTreeMap;

use fauna_core::content_category::content_label_style;
use fauna_core::content_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION,
    MAX_KEYWORD_TERM_CHARS, PolicyDefect, PolicyStatus, Publisher, REASON_DEFAULT_KEY, RuleStatus,
    ScorerKind, encode_document, join_keyword_terms, scorer_factor, split_keyword_terms,
};
use fauna_core::localized::LocalizedText;
use fauna_core::obligation::{
    ContentFloor, ContentPolicy, GUARDIAN_FLOOR_CATEGORIES, GUARDIAN_FLOOR_TRIGGER_PERMILLE,
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "scorers")]
mod device;
pub mod guardian_rules;
pub mod nest_rules;
mod view;

pub use guardian_rules::GuardianRules;
pub use nest_rules::{
    AcceptedNestRules, NestRuleLine, NestRulesBook, NestRulesNotApplied, NestRulesRead,
    NestRulesRefusal, NestRulesSummary, SignedNestRules, StoredNestRules,
};
pub use view::{
    CategoryOption, KindOption, LabelerOption, LevelOption, ReasonEntry, RuleEditorView, RuleRow,
    VerdictOption,
};

/// What a rule tests. The editor's own vocabulary over the grammar: a rule is
/// a **category** rule when its factor is a label category, and otherwise the
/// kind of the bundled scorer it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RuleKind {
    /// One of the canonical categories (`nsfw`, `spam`, `phishing`,
    /// `commercial`), as the device's labelers produce them.
    Category,
    /// A list of terms, matched by the shared keyword matcher.
    Keywords,
    /// A labeler the publisher bundles as a `wasm` scorer.
    Labeler,
    /// A prompt for an on-device language model — inert on every build today.
    Prompt,
    /// A content-id list. Never offered; a loaded region rule is read-only.
    List,
}

/// The kinds the editor offers, in the order it offers them, to every
/// publisher (`nest-rules.md` § The shared rule editor: never `list`).
pub const OFFERED_KINDS: [RuleKind; 4] = [
    RuleKind::Category,
    RuleKind::Keywords,
    RuleKind::Labeler,
    RuleKind::Prompt,
];

impl RuleKind {
    fn label(self) -> LocalizedText {
        LocalizedText::key(match self {
            Self::Category => "rule_editor.kind.category",
            Self::Keywords => "rule_editor.kind.keywords",
            Self::Labeler => "rule_editor.kind.labeler",
            Self::Prompt => "rule_editor.kind.prompt",
            Self::List => "rule_editor.kind.list",
        })
    }

    fn scorer_kind(self) -> Option<ScorerKind> {
        match self {
            Self::Category => None,
            Self::Keywords => Some(ScorerKind::Keywords),
            Self::Labeler => Some(ScorerKind::Wasm),
            Self::Prompt => Some(ScorerKind::Prompt),
            Self::List => Some(ScorerKind::List),
        }
    }

    fn of_scorer(kind: ScorerKind) -> Self {
        match kind {
            ScorerKind::List => Self::List,
            ScorerKind::Wasm => Self::Labeler,
            ScorerKind::Keywords => Self::Keywords,
            ScorerKind::Prompt => Self::Prompt,
        }
    }

    /// Whether the rule's level means anything: a keyword match always scores
    /// [`fauna_core::content_policy::KEYWORD_MATCH_PERMILLE`], so any level
    /// fires on it.
    fn has_level(self) -> bool {
        !matches!(self, Self::Keywords)
    }
}

/// The two verdicts a rule may carry — the engine's two render verbs, and no
/// other (`nest-rules.md` § The shared rule editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RuleVerdict {
    Block,
    Collapse,
}

/// The verdicts the editor offers, strictest first.
pub const OFFERED_VERDICTS: [RuleVerdict; 2] = [RuleVerdict::Block, RuleVerdict::Collapse];

impl RuleVerdict {
    fn label(self) -> LocalizedText {
        LocalizedText::key(match self {
            Self::Block => "rule_editor.verdict.block",
            Self::Collapse => "rule_editor.verdict.collapse",
        })
    }
}

impl From<ContentVerdict> for RuleVerdict {
    fn from(v: ContentVerdict) -> Self {
        match v {
            ContentVerdict::Block => Self::Block,
            ContentVerdict::Collapse => Self::Collapse,
        }
    }
}

impl From<RuleVerdict> for ContentVerdict {
    fn from(v: RuleVerdict) -> Self {
        match v {
            RuleVerdict::Block => Self::Block,
            RuleVerdict::Collapse => Self::Collapse,
        }
    }
}

/// One step of the level scale: the label key and the per-mille trigger it
/// stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub key: &'static str,
    pub permille: u16,
}

/// The level scale, most sensitive first. A rule fires when a label's
/// confidence reaches its per-mille, so a lower per-mille acts on less
/// certain labels. The middle step is the shared guardian floor trigger.
pub const LEVELS: [Level; 5] = [
    Level {
        key: "rule_editor.level.very_strict",
        permille: 200,
    },
    Level {
        key: "rule_editor.level.strict",
        permille: 350,
    },
    Level {
        key: "rule_editor.level.standard",
        permille: GUARDIAN_FLOOR_TRIGGER_PERMILLE,
    },
    Level {
        key: "rule_editor.level.lenient",
        permille: 700,
    },
    Level {
        key: "rule_editor.level.very_lenient",
        permille: 900,
    },
];

/// The level a new rule starts at.
pub const DEFAULT_LEVEL_INDEX: usize = 2;

const _: () = assert!(LEVELS[DEFAULT_LEVEL_INDEX].permille == GUARDIAN_FLOOR_TRIGGER_PERMILLE);

/// The level a per-mille is shown as. A per-mille between two steps shows as
/// the nearer, and a tie as the stricter.
pub fn nearest_level(permille: u16) -> usize {
    (0..LEVELS.len())
        .min_by_key(|&i| LEVELS[i].permille.abs_diff(permille))
        .unwrap_or(DEFAULT_LEVEL_INDEX)
}

/// The editor: a working document, the publisher it is validated for, and
/// whether this build may write it.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleEditor {
    publisher: Publisher,
    document: ContentPolicyDocument,
}

impl RuleEditor {
    /// Open the editor over `document` as `publisher` publishes it. An absent
    /// document is [`ContentPolicyDocument::empty`].
    ///
    /// A document at a grammar version this build does not implement is shown
    /// and never edited: it may carry rules this build cannot even decode, and
    /// writing it back at this build's version would destroy them.
    pub fn load(document: Option<ContentPolicyDocument>, publisher: Publisher) -> Self {
        Self {
            publisher,
            document: document.unwrap_or_else(ContentPolicyDocument::empty),
        }
    }

    /// Open the guardian's editor over a ward's content policy: its rule
    /// document, with every floor still set shown as a category rule beside it
    /// (`nest-rules.md` § Implementation status today — a set floor keeps
    /// binding, and storing a document clears the floors, so the document the
    /// editor writes must carry them). A floor has no reason of its own; the
    /// guardian writes one before the document can be saved.
    pub fn load_guardian(policy: Option<&ContentPolicy>) -> Self {
        let mut editor = Self::load(policy.and_then(|p| p.document.clone()), Publisher::Guardian);
        let Some(policy) = policy else {
            return editor;
        };
        if !editor.is_editable() {
            return editor;
        }
        for category in GUARDIAN_FLOOR_CATEGORIES {
            let verdict = match policy.floor_for(category) {
                ContentFloor::Inherit => continue,
                ContentFloor::Collapse => ContentVerdict::Collapse,
                // An unknown floor renders blocked, so it is shown as what binds.
                ContentFloor::Block | ContentFloor::Unknown => ContentVerdict::Block,
            };
            let reason_code = editor.mint_reason_code();
            editor.document.rules.push(ContentRule {
                factor: category.to_string(),
                min_permille: GUARDIAN_FLOOR_TRIGGER_PERMILLE,
                verdict,
                reason_code,
                reason: BTreeMap::new(),
                extra: BTreeMap::new(),
            });
        }
        editor
    }

    pub fn publisher(&self) -> &Publisher {
        &self.publisher
    }

    /// Whether this build may write the document — false for a newer grammar.
    pub fn is_editable(&self) -> bool {
        self.document.version == GRAMMAR_VERSION
    }

    /// The document as edited, every loaded field kept.
    pub fn document(&self) -> ContentPolicyDocument {
        self.document.clone()
    }

    /// The document as the canonical dag-cbor every carriage takes.
    ///
    /// # Errors
    ///
    /// A loaded catch-all field carries a float, which the canonical encoder
    /// refuses.
    pub fn encoded(&self) -> Result<Vec<u8>, String> {
        encode_document(&self.document)
    }

    /// The kind of rule `index`, or `None` past the end.
    pub fn kind_of(&self, index: usize) -> Option<RuleKind> {
        let rule = self.document.rules.get(index)?;
        Some(
            self.document
                .scorer_for_factor(&self.publisher, &rule.factor)
                .map_or(RuleKind::Category, |s| RuleKind::of_scorer(s.kind)),
        )
    }

    /// Append a rule of `kind` at the default level, verdict `block`, a fresh
    /// reason code and no reason; its index. `None` for the `list` kind, or a
    /// document this build may not write.
    pub fn add_rule(&mut self, kind: RuleKind) -> Option<usize> {
        if !self.is_editable() || !OFFERED_KINDS.contains(&kind) {
            return None;
        }
        let reason_code = self.mint_reason_code();
        self.document.rules.push(ContentRule {
            factor: GUARDIAN_FLOOR_CATEGORIES[0].to_string(),
            min_permille: LEVELS[DEFAULT_LEVEL_INDEX].permille,
            verdict: ContentVerdict::Block,
            reason_code,
            reason: BTreeMap::new(),
            extra: BTreeMap::new(),
        });
        let index = self.document.rules.len() - 1;
        if kind != RuleKind::Category {
            self.point_at_new_scorer(index, kind, kind_prefix(kind), Vec::new());
        }
        Some(index)
    }

    /// Remove rule `index`, and its scorer when no other rule names it. A
    /// loaded `list` rule is removable like any other.
    pub fn remove_rule(&mut self, index: usize) -> bool {
        if !self.is_editable() || index >= self.document.rules.len() {
            return false;
        }
        let rule = self.document.rules.remove(index);
        self.release_scorer(&rule.factor);
        true
    }

    /// Change rule `index` to `kind`, with that kind's empty subject. Its
    /// verdict, level, reason code and reasons are kept.
    pub fn set_kind(&mut self, index: usize, kind: RuleKind) -> bool {
        if !OFFERED_KINDS.contains(&kind) || !self.can_edit(index) {
            return false;
        }
        if self.kind_of(index) == Some(kind) {
            return true;
        }
        let old = self.document.rules[index].factor.clone();
        if kind == RuleKind::Category {
            self.document.rules[index].factor = GUARDIAN_FLOOR_CATEGORIES[0].to_string();
        } else {
            self.point_at_new_scorer(index, kind, kind_prefix(kind), Vec::new());
        }
        self.release_scorer(&old);
        true
    }

    /// Set a category rule's category (any label category the device's
    /// labelers produce; the canonical four are the ones offered).
    pub fn set_category(&mut self, index: usize, category: &str) -> bool {
        if self.kind_of(index) != Some(RuleKind::Category) || !self.can_edit(index) {
            return false;
        }
        let category = category.trim();
        if category.is_empty() || category.starts_with(&self.publisher.factor_prefix()) {
            return false;
        }
        self.document.rules[index].factor = category.to_string();
        true
    }

    /// Set a keywords rule's terms from `text`, one term per line.
    pub fn set_keywords_text(&mut self, index: usize, text: &str) -> bool {
        self.set_keywords(index, &split_keyword_terms(text))
    }

    /// Set a keywords rule's terms from a single-line input: commas separate
    /// terms as line breaks do ([`keywords_inline`] is the form it paints). A
    /// term containing a comma needs the multi-line form.
    pub fn set_keywords_inline(&mut self, index: usize, text: &str) -> bool {
        self.set_keywords_text(index, &text.replace(',', "\n"))
    }

    /// Set a keywords rule's terms.
    pub fn set_keywords(&mut self, index: usize, terms: &[String]) -> bool {
        self.set_own_scorer_bytes(index, RuleKind::Keywords, join_keyword_terms(terms))
    }

    /// Set a prompt rule's prompt text.
    pub fn set_prompt(&mut self, index: usize, text: &str) -> bool {
        self.set_own_scorer_bytes(index, RuleKind::Prompt, text.as_bytes().to_vec())
    }

    /// Point a labeler rule at the labeler `name`, whose module `bytes` the
    /// app supplies from the publisher's subscriptions. A labeler the document
    /// already bundles under that name is shared (and takes the new bytes).
    pub fn set_labeler(&mut self, index: usize, name: &str, bytes: Vec<u8>) -> bool {
        if self.kind_of(index) != Some(RuleKind::Labeler) || !self.can_edit(index) {
            return false;
        }
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let old = self.document.rules[index].factor.clone();
        if let Some(existing) = self
            .document
            .scorers
            .iter_mut()
            .find(|s| s.name == name && s.kind == ScorerKind::Wasm)
        {
            existing.bytes = bytes;
            self.document.rules[index].factor = scorer_factor(&self.publisher, name);
        } else {
            self.point_at_new_scorer(index, RuleKind::Labeler, name, bytes);
        }
        if old != self.document.rules[index].factor {
            self.release_scorer(&old);
        }
        true
    }

    pub fn set_verdict(&mut self, index: usize, verdict: RuleVerdict) -> bool {
        if !self.can_edit(index) {
            return false;
        }
        self.document.rules[index].verdict = verdict.into();
        true
    }

    /// Set rule `index` to level `level` of [`LEVELS`].
    pub fn set_level(&mut self, index: usize, level: usize) -> bool {
        let Some(step) = LEVELS.get(level) else {
            return false;
        };
        if !self.can_edit(index) {
            return false;
        }
        self.document.rules[index].min_permille = step.permille;
        true
    }

    /// Write rule `index`'s reason in `lang` ([`REASON_DEFAULT_KEY`] for the
    /// required default). Empty text removes the language.
    pub fn set_reason(&mut self, index: usize, lang: &str, text: &str) -> bool {
        let lang = lang.trim();
        if lang.is_empty() || !self.can_edit(index) {
            return false;
        }
        let reason = &mut self.document.rules[index].reason;
        if text.trim().is_empty() {
            reason.remove(lang);
        } else {
            reason.insert(lang.to_string(), text.to_string());
        }
        true
    }

    /// The document's defects, as sentences: the one structural validator
    /// ([`ContentPolicyDocument::status`]) for this publisher, and nothing
    /// else. Empty when the document may be saved.
    pub fn validate(&self) -> Vec<LocalizedText> {
        match self.document.status(&self.publisher) {
            PolicyStatus::Applied => Vec::new(),
            PolicyStatus::InertUnimplementedVersion { .. } => {
                vec![LocalizedText::key("rule_editor.newer_version")]
            }
            PolicyStatus::Malformed(defect) => vec![self.defect_sentence(&defect)],
        }
    }

    /// Whether the document may be saved: this build may write it and the
    /// validator finds nothing.
    pub fn can_save(&self) -> bool {
        self.is_editable() && self.document.status(&self.publisher).is_applied()
    }

    /// Everything the editor paints.
    pub fn view(&self) -> RuleEditorView {
        view::view(self)
    }

    // ── helpers ──────────────────────────────────────────────────────────

    /// Whether rule `index` exists, is not a read-only `list` rule, and this
    /// build may write the document.
    fn can_edit(&self, index: usize) -> bool {
        self.is_editable()
            && self
                .kind_of(index)
                .is_some_and(|kind| kind != RuleKind::List)
    }

    /// The next `rule-<n>`, above every such code the document already
    /// carries, so a new rule never takes a code a loaded rule holds.
    fn mint_reason_code(&self) -> String {
        let next = self
            .document
            .rules
            .iter()
            .filter_map(|r| r.reason_code.strip_prefix("rule-")?.parse::<u32>().ok())
            .max()
            .map_or(1, |n| n.saturating_add(1));
        format!("rule-{next}")
    }

    /// `base`, or `base-<n>` for the first `n` no bundled scorer is named.
    fn fresh_scorer_name(&self, base: &str) -> String {
        let taken = |name: &str| self.document.scorers.iter().any(|s| s.name == name);
        if !taken(base) {
            return base.to_string();
        }
        (2u32..)
            .map(|n| format!("{base}-{n}"))
            .find(|name| !taken(name))
            .expect("an unbounded range finds a free name")
    }

    /// Bundle a new scorer of `kind` under a name derived from `base` and point
    /// rule `index` at it.
    fn point_at_new_scorer(&mut self, index: usize, kind: RuleKind, base: &str, bytes: Vec<u8>) {
        let scorer_kind = kind.scorer_kind().expect("a scorer-backed kind");
        let name = self.fresh_scorer_name(base);
        self.document.rules[index].factor = scorer_factor(&self.publisher, &name);
        self.document.scorers.push(BundledScorer {
            name,
            kind: scorer_kind,
            bytes,
            extra: BTreeMap::new(),
        });
    }

    /// Set the bytes of rule `index`'s own scorer, first giving it a scorer of
    /// its own when a loaded document shared one between rules — a keyword or
    /// prompt edit to one rule never rewrites another.
    fn set_own_scorer_bytes(&mut self, index: usize, kind: RuleKind, bytes: Vec<u8>) -> bool {
        if self.kind_of(index) != Some(kind) || !self.can_edit(index) {
            return false;
        }
        let factor = self.document.rules[index].factor.clone();
        let shared = self
            .document
            .rules
            .iter()
            .enumerate()
            .any(|(i, r)| i != index && r.factor == factor);
        if shared {
            self.point_at_new_scorer(index, kind, kind_prefix(kind), bytes);
            return true;
        }
        let name = factor
            .strip_prefix(&self.publisher.factor_prefix())
            .expect("a scorer-backed rule names its own scorer")
            .to_string();
        if let Some(scorer) = self.document.scorers.iter_mut().find(|s| s.name == name) {
            scorer.bytes = bytes;
        }
        true
    }

    /// Drop the scorer `factor` names when no rule names it any more.
    fn release_scorer(&mut self, factor: &str) {
        let Some(name) = factor.strip_prefix(&self.publisher.factor_prefix()) else {
            return;
        };
        if self.document.rules.iter().any(|r| r.factor == factor) {
            return;
        }
        self.document.scorers.retain(|s| s.name != name);
    }

    /// The 1-based number of the first rule naming scorer `index`.
    fn rule_of_scorer(&self, index: usize) -> Option<usize> {
        let scorer = self.document.scorers.get(index)?;
        let factor = scorer_factor(&self.publisher, &scorer.name);
        self.document
            .rules
            .iter()
            .position(|r| r.factor == factor)
            .map(|i| i + 1)
    }

    fn defect_sentence(&self, defect: &PolicyDefect) -> LocalizedText {
        let rule =
            |key: &str, index: usize| LocalizedText::key_arg(key, "rule", (index + 1).to_string());
        // A scorer defect is the defect of the rule that names the scorer; one
        // no rule names is said of the scorer itself.
        let scorer = |key: &str, index: usize, name: &str| match self.rule_of_scorer(index) {
            Some(number) => LocalizedText::key_arg(key, "rule", number.to_string()),
            None => LocalizedText::key_arg("rule_editor.defect.unused_scorer", "name", name),
        };
        match defect {
            PolicyDefect::RuleWithoutReason { index } => {
                rule("rule_editor.defect.no_reason", *index)
            }
            PolicyDefect::RuleWithoutReasonCode { index } => {
                rule("rule_editor.defect.no_reason_code", *index)
            }
            PolicyDefect::TriggerOutOfRange { index, .. } => {
                rule("rule_editor.defect.level_out_of_range", *index)
            }
            PolicyDefect::BlankFactor { index } => rule("rule_editor.defect.no_subject", *index),
            PolicyDefect::UnknownScorerFactor { index, .. } => {
                rule("rule_editor.defect.missing_scorer", *index)
            }
            PolicyDefect::ScorerWithoutName { index } => {
                scorer("rule_editor.defect.malformed_scorer", *index, "")
            }
            PolicyDefect::DuplicateScorerName { second, name, .. } => {
                scorer("rule_editor.defect.malformed_scorer", *second, name)
            }
            PolicyDefect::ScorerKindNotAdmitted { index, name, .. } => {
                scorer("rule_editor.defect.list_not_admitted", *index, name)
            }
            PolicyDefect::MalformedListScorer { index, name, .. }
            | PolicyDefect::ScorerNotText { index, name } => {
                scorer("rule_editor.defect.malformed_scorer", *index, name)
            }
            PolicyDefect::ScorerTooLarge { index, name, .. } => {
                scorer("rule_editor.defect.scorer_too_large", *index, name)
            }
            PolicyDefect::EmptyScorer { index, name } => {
                let key = match self.document.scorers.get(*index).map(|s| s.kind) {
                    Some(ScorerKind::Keywords) => "rule_editor.defect.no_keyword",
                    Some(ScorerKind::Prompt) => "rule_editor.defect.no_prompt",
                    Some(ScorerKind::Wasm) => "rule_editor.defect.no_labeler",
                    Some(ScorerKind::List) | None => "rule_editor.defect.malformed_scorer",
                };
                scorer(key, *index, name)
            }
            PolicyDefect::KeywordsWithoutTerm { index, name } => {
                scorer("rule_editor.defect.no_keyword", *index, name)
            }
            PolicyDefect::KeywordTermTooLong { index, name, .. } => {
                let mut text = scorer("rule_editor.defect.keyword_too_long", *index, name);
                text.args
                    .insert("max".to_string(), MAX_KEYWORD_TERM_CHARS.to_string());
                text
            }
        }
    }

    fn row(&self, index: usize) -> RuleRow {
        let rule = &self.document.rules[index];
        let kind = self.kind_of(index).expect("an existing rule");
        let scorer = self
            .document
            .scorer_for_factor(&self.publisher, &rule.factor);
        let text = || scorer.and_then(|s| std::str::from_utf8(&s.bytes).ok().map(str::to_string));
        let mut reasons: Vec<ReasonEntry> = rule
            .reason
            .iter()
            .map(|(lang, text)| ReasonEntry {
                lang: lang.clone(),
                text: text.clone(),
            })
            .collect();
        // The required default first; the rest in language order.
        reasons.sort_by_key(|r| r.lang != REASON_DEFAULT_KEY);
        RuleRow {
            number: (index + 1) as u32,
            kind,
            kind_label: kind.label(),
            read_only: kind == RuleKind::List,
            category: (kind == RuleKind::Category).then(|| rule.factor.clone()),
            keywords: (kind == RuleKind::Keywords).then(|| {
                scorer
                    .and_then(|s| s.keyword_terms())
                    .unwrap_or_default()
                    .join("\n")
            }),
            labeler: (kind == RuleKind::Labeler)
                .then(|| scorer.map(|s| s.name.clone()).unwrap_or_default()),
            prompt: (kind == RuleKind::Prompt).then(|| text().unwrap_or_default()),
            verdict: rule.verdict.into(),
            level_index: kind
                .has_level()
                .then(|| nearest_level(rule.min_permille) as u32),
            reasons,
            reason_code: rule.reason_code.clone(),
            inert: (self.document.rule_status(&self.publisher, index)
                == RuleStatus::InertNoRuntime)
                .then(|| LocalizedText::key("rule_editor.prompt_not_applied")),
        }
    }

    fn category_options(&self) -> Vec<CategoryOption> {
        let mut categories: Vec<String> = GUARDIAN_FLOOR_CATEGORIES
            .iter()
            .map(|c| c.to_string())
            .collect();
        // A loaded rule on another label category stays choosable.
        for index in 0..self.document.rules.len() {
            if self.kind_of(index) == Some(RuleKind::Category) {
                let factor = &self.document.rules[index].factor;
                if !categories.contains(factor) {
                    categories.push(factor.clone());
                }
            }
        }
        categories
            .into_iter()
            .map(|category| CategoryOption {
                label: content_label_style(&category).label,
                category,
            })
            .collect()
    }

    /// What `rule-editor-item-labeler-select` offers: the labelers the
    /// document already bundles, then each of the publisher's subscribed
    /// labelers it does not (`subscribed`: their hex ids, as the mounting lists
    /// them — `fauna_client_labelers::LabelersClient::rule_labelers`). A
    /// subscribed labeler is named by [`labeler_scorer_name`], so choosing it
    /// again later finds the scorer it bundled.
    pub fn labeler_choices(&self, subscribed: &[String]) -> Vec<LabelerChoice> {
        let mut choices: Vec<LabelerChoice> = self
            .labeler_options()
            .into_iter()
            .map(|o| LabelerChoice {
                name: o.name,
                labeler_id: None,
            })
            .collect();
        for id in subscribed {
            let name = labeler_scorer_name(id);
            if !choices.iter().any(|c| c.name == name) {
                choices.push(LabelerChoice {
                    name,
                    labeler_id: Some(id.clone()),
                });
            }
        }
        choices
    }

    /// Point a labeler rule at a labeler the document already bundles, sharing
    /// its module. `false` when no bundled labeler has that name.
    pub fn choose_bundled_labeler(&mut self, index: usize, name: &str) -> bool {
        let Some(bytes) = self
            .document
            .scorers
            .iter()
            .find(|s| s.name == name && s.kind == ScorerKind::Wasm && !s.bytes.is_empty())
            .map(|s| s.bytes.clone())
        else {
            return false;
        };
        self.set_labeler(index, name, bytes)
    }

    fn labeler_options(&self) -> Vec<LabelerOption> {
        self.document
            .scorers
            .iter()
            .filter(|s| s.kind == ScorerKind::Wasm && !s.bytes.is_empty())
            .map(|s| LabelerOption {
                name: s.name.clone(),
            })
            .collect()
    }
}

/// A keywords rule's terms ([`RuleRow::keywords`], one per line) as a
/// single-line input shows them — the form [`RuleEditor::set_keywords_inline`]
/// reads back.
pub fn keywords_inline(terms: &str) -> String {
    split_keyword_terms(terms).join(", ")
}

/// One option of the labeler picker ([`RuleEditor::labeler_choices`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LabelerChoice {
    /// The scorer name the option paints and the rule names.
    pub name: String,
    /// `None` for a labeler the document bundles
    /// ([`RuleEditor::choose_bundled_labeler`]); the hex labeler id of a
    /// subscribed one, whose module the mounting fetches
    /// (`fauna_client_labelers::LabelersClient::rule_module`) and hands to
    /// [`RuleEditor::set_labeler`] under [`Self::name`].
    pub labeler_id: Option<String>,
}

/// The scorer name a subscribed labeler is bundled under: `labeler-` and the
/// first twelve hex digits of its id — stable, so the same labeler chosen for
/// two rules is one scorer.
pub fn labeler_scorer_name(labeler_id_hex: &str) -> String {
    let prefix: String = labeler_id_hex
        .chars()
        .take(12)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    format!("labeler-{prefix}")
}

/// The scorer name a new rule of `kind` starts from.
fn kind_prefix(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Keywords => "keywords",
        RuleKind::Labeler => "labeler",
        RuleKind::Prompt => "prompt",
        RuleKind::Category | RuleKind::List => "scorer",
    }
}

#[cfg(test)]
mod tests;
