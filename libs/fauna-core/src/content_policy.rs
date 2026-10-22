//! The **content-policy grammar** — one document, three publishers.
//!
//! Owner docs: `docs/goal/behavior/region-blocking.md` § The content plane →
//! *The policy document* (the grammar, its scorer kinds, the paragraph *One
//! grammar, three publishers*) and `docs/goal/behavior/nest-rules.md` § The
//! nest content policy (the nest as a publisher). A region's authority, a nest
//! admin and a guardian all publish the same document; what differs between
//! them is the [`Publisher`], which namespaces the factors a document's own
//! scorers produce and decides which scorer kinds the document may bundle.
//! [`crate::region_policy`] keeps its name as the region's assembler over this
//! module; the nest's and the guardian's assemblers live beside the engine in
//! [`crate::obligation`].
//!
//! Four properties this module is built around:
//!
//! 1. **Version first, structure second.** A document whose grammar `version`
//!    this build does not implement is **inert and says so** — never
//!    "malformed", because a newer grammar's perfectly good shapes are exactly
//!    what an older reader would misread as malformed.
//!    [`ContentPolicyDocument::status`] checks the version before it looks at a
//!    single rule, and [`decode_document`] reads the version before it decodes
//!    the rest, so a newer document whose shapes do not even decode here is
//!    inert too.
//! 2. **One structural validator, per publisher.** The per-publisher admissions
//!    — a `list` scorer is a region's instrument only — are one check in one
//!    place ([`Publisher::admits`]).
//! 3. **Structural validation only — no execution of a module.** This module
//!    validates that a bundled scorer is *well-formed* (`list` bytes through
//!    [`crate::scoring::validate_list_artifact`], `wasm` bounded, `keywords` and
//!    `prompt` UTF-8, `text-model` refused). It never runs a `wasm` module: the
//!    sandbox is `fauna-labeler`, which depends on `fauna-core`, so a call from
//!    here would be a dependency cycle. The one scorer it does run is the
//!    `keywords` kind ([`evaluate_keyword_scorers`]), because that matcher is
//!    already this crate's ([`crate::keyword::body_excludes_matches`]).
//! 4. **Inert per rule, as well as per document.** A rule whose scorer kind this
//!    build knows but cannot run — today every `prompt` rule, since no app has
//!    an on-device model runtime — leaves the rest of the document applied and
//!    reports itself [`RuleStatus::InertNoRuntime`] (`nest-rules.md` decision 1).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::content_category::ContentLabelEntry;
use crate::obligation::{AttestationLevel, ObligationAction, ObligationRule};
use crate::region_authority::RegionCode;

/// The grammar version this build implements.
///
/// Version 2 added the `keywords` and `prompt` scorer kinds and the publisher
/// dimension (`region-blocking.md` § The policy document, *One grammar, three
/// publishers*). A document at any other version is
/// [`PolicyStatus::InertUnimplementedVersion`] — inert, flagged on the
/// transparency surface, applying nothing. Inert is the fail direction because a
/// published rule can only ever *restrict*, and a blanket block over an
/// undecodable document would blank every item on the device.
pub const GRAMMAR_VERSION: u32 = 2;

/// The reason map's required entry — the text shown when the app's own language
/// has no entry of its own.
///
/// A reserved key rather than a language tag, so "the fallback" is a fact of the
/// document rather than a guess about which language a publisher considers
/// primary.
pub const REASON_DEFAULT_KEY: &str = "default";

/// The bound on an encoded content-policy document.
///
/// Larger than the feature plane's 64 KiB because a document may carry bundled
/// scorer artifacts; a region envelope's own 4 MiB total (`region-blocking.md`
/// § Publication and signing) remains the outer ceiling there, and this is the
/// inner one that refuses a payload which is obviously not a policy document.
pub const MAX_CONTENT_POLICY_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// The largest bundled scorer artifact this build accepts, structurally.
///
/// The *execution* bounds (fuel, memory) are the labeler crate's own constants,
/// applied where a module actually runs; this is only the "obviously not an
/// artifact" refusal that keeps an oversized blob out of the device store.
pub const MAX_SCORER_BYTES: usize = 2 * 1024 * 1024;

/// The longest single term a `keywords` scorer may carry, in characters.
///
/// A term is a word or a phrase a reader would recognise; anything longer is a
/// pasted body, not a rule.
pub const MAX_KEYWORD_TERM_CHARS: usize = 200;

/// The per-mille a `keywords` scorer's factor carries on a match — a keyword
/// match is a certainty, not a confidence (`nest-rules.md` § The keyword rule).
pub const KEYWORD_MATCH_PERMILLE: u16 = 1000;

/// Who published a document — the dimension the grammar gained at version 2.
///
/// It decides two things and nothing else: the namespace a document's own
/// scorers' factors live in ([`Self::factor_prefix`]), and which scorer kinds
/// the document may bundle ([`Self::admits`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Publisher {
    /// A region's administering authority, through the signed region envelope.
    Region(RegionCode),
    /// A nest admin, through the nest's signed self-description; the nest's
    /// domain.
    Nest(String),
    /// A guardian, through the family policy of one ward.
    Guardian,
}

impl Publisher {
    /// The prefix every factor one of this publisher's own scorers produces
    /// starts with: `region:<region>/`, `nest:<domain>/`, `guardian:`.
    pub fn factor_prefix(&self) -> String {
        match self {
            Self::Region(region) => format!("region:{region}/"),
            Self::Nest(domain) => format!("nest:{domain}/"),
            Self::Guardian => "guardian:".to_string(),
        }
    }

    /// Whether this publisher may bundle a scorer of `kind`.
    ///
    /// A content-id `list` is a government's instrument only (`nest-rules.md`
    /// decision 2): for an admin a list of ids is manual blocking without the
    /// notice, the appeal and the record; for a guardian, who never sees the
    /// content, there is nothing to list.
    pub fn admits(&self, kind: ScorerKind) -> bool {
        match kind {
            ScorerKind::List => matches!(self, Self::Region(_)),
            ScorerKind::Wasm | ScorerKind::Keywords | ScorerKind::Prompt => true,
        }
    }

    /// The publisher's kind as one word, for a defect sentence.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Region(_) => "region",
            Self::Nest(_) => "nest",
            Self::Guardian => "guardian",
        }
    }
}

/// The factor a bundled scorer named `scorer_name` produces for `publisher`.
pub fn scorer_factor(publisher: &Publisher, scorer_name: &str) -> String {
    format!("{}{scorer_name}", publisher.factor_prefix())
}

/// The two render verbs a rule may carry.
///
/// Deliberately **not** `#[serde(other)]`-tolerant: the engine has exactly two
/// client render verbs, and a third verb is a *grammar* change, which arrives as
/// a `version` bump and is handled by the inert path (property 1 in the module
/// docs). An unknown verb inside a version this build claims to implement is a
/// malformed document, not a silently-guessed one.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentVerdict {
    /// Render collapsed, with a reveal affordance.
    Collapse,
    /// Render blocked: no reveal, a placeholder naming the publisher.
    Block,
}

impl ContentVerdict {
    pub(crate) fn action(self) -> ObligationAction {
        match self {
            Self::Collapse => ObligationAction::Collapse,
            Self::Block => ObligationAction::Block,
        }
    }

    /// The wire string (`collapse` / `block`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collapse => "collapse",
            Self::Block => "block",
        }
    }
}

/// The kinds of bundled scorer a document may carry.
///
/// `text-model` is deliberately absent — it is a *training* artifact, and a
/// publisher's transparency obligation is better met by a scorer whose behaviour
/// a reader can follow in full (`region-blocking.md` § The policy document). It
/// therefore arrives as an unknown kind, which makes the document malformed: the
/// refusal that section asks for, expressed once in the type rather than as a
/// check every consumer must remember.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change — `Keywords` and `Prompt` came with version 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScorerKind {
    /// A content-id → per-mille map. Pure lookup, no execution. A region's
    /// instrument only ([`Publisher::admits`]).
    List,
    /// A deterministic, fuel- and memory-bounded module for the labeler sandbox
    /// — the labeler rule.
    Wasm,
    /// A UTF-8 list of terms, one per line, run through the shared
    /// deterministic keyword matcher ([`evaluate_keyword_scorers`]).
    Keywords,
    /// UTF-8 prompt text for an on-device language model. No app has a runtime
    /// for one yet, so every rule on a `prompt` scorer is
    /// [`RuleStatus::InertNoRuntime`] on this build.
    Prompt,
}

impl ScorerKind {
    /// Whether this build can produce the scorer's factor at all. A rule on a
    /// kind it cannot is inert by itself, never the whole document.
    pub fn runs_on_this_build(self) -> bool {
        !matches!(self, Self::Prompt)
    }
}

/// One bundled scorer artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundledScorer {
    /// The scorer's name within this document. The factor it produces is
    /// [`scorer_factor`]`(publisher, name)`.
    pub name: String,
    pub kind: ScorerKind,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    /// Forward-compatible catch-all (`transport.md`'s additive discipline).
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

impl BundledScorer {
    /// A `keywords` scorer's terms — its UTF-8 lines with blank ones dropped —
    /// or `None` for any other kind or for bytes that are not UTF-8.
    pub fn keyword_terms(&self) -> Option<Vec<String>> {
        if self.kind != ScorerKind::Keywords {
            return None;
        }
        let text = std::str::from_utf8(&self.bytes).ok()?;
        Some(split_keyword_terms(text))
    }

    /// A `prompt` scorer's prompt text, or `None` for any other kind or for
    /// bytes that are not UTF-8.
    pub fn prompt_text(&self) -> Option<&str> {
        if self.kind != ScorerKind::Prompt {
            return None;
        }
        std::str::from_utf8(&self.bytes).ok()
    }
}

/// The one way a `keywords` scorer's text becomes terms: one per line, trimmed,
/// blanks dropped. Shared by the validator, the matcher and the editor so the
/// three never disagree about what a term is.
pub fn split_keyword_terms(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

/// The bytes a `keywords` scorer carries for `terms` — the inverse of
/// [`split_keyword_terms`].
pub fn join_keyword_terms(terms: &[String]) -> Vec<u8> {
    terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// One verdict rule: a factor, a trigger, a verb, and the publisher's own
/// attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentRule {
    /// A label category exactly as the shared engine keys them (the canonical
    /// `nsfw` / `spam` / `phishing` / `commercial`), or a factor one of this
    /// document's bundled scorers produces, in the publisher's namespace
    /// ([`Publisher::factor_prefix`]).
    pub factor: String,
    /// The same per-mille trigger the engine's rules carry (`0..=1000`).
    pub min_permille: u16,
    pub verdict: ContentVerdict,
    /// A stable, publisher-assigned token. Machine-readable provenance for the
    /// transparency surface; never the user-facing text.
    pub reason_code: String,
    /// `{ lang → text }`, with [`REASON_DEFAULT_KEY`] required. Shown to the
    /// user **verbatim** under the app's own frame — the frame is the app's
    /// i18n, the reason is the publisher's, and an app never paraphrases it.
    pub reason: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// The content-policy document: one float-free dag-cbor document.
///
/// `Eq` is deliberately not derived — [`fauna_cbor::Value`] carries a float
/// variant. No float can actually arrive through [`decode_document`] (the
/// canonical validator refuses major-7 floats before serde runs), but the type
/// does not get to claim `Eq` on the strength of a caller's discipline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPolicyDocument {
    /// The grammar version. See [`GRAMMAR_VERSION`].
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<ContentRule>,
    #[serde(default)]
    pub scorers: Vec<BundledScorer>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

impl ContentPolicyDocument {
    /// A document at this build's grammar version with no rules — what a
    /// publisher that has written nothing publishes.
    pub fn empty() -> Self {
        Self {
            version: GRAMMAR_VERSION,
            rules: Vec::new(),
            scorers: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// What a carriage holds for bytes this build could not decode at all: a
    /// document at no grammar version (versions start at 1), so its
    /// [`Self::status`] is inert and says so — and a guardian's renders fail
    /// closed on it, as on a floor this build cannot name.
    pub fn unreadable() -> Self {
        Self {
            version: 0,
            ..Self::empty()
        }
    }
}

/// Why a document at a version this build *does* implement is nonetheless not
/// applicable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum PolicyDefect {
    #[error("rule {index} carries no reason for the required default key")]
    RuleWithoutReason { index: usize },
    #[error("rule {index} carries no reason_code")]
    RuleWithoutReasonCode { index: usize },
    #[error("rule {index} has min_permille {permille}, outside 0..=1000")]
    TriggerOutOfRange { index: usize, permille: u16 },
    #[error("rule {index} has a blank factor")]
    BlankFactor { index: usize },
    #[error(
        "rule {index} names factor {factor:?}, which no bundled scorer of this publisher produces"
    )]
    UnknownScorerFactor { index: usize, factor: String },
    #[error("scorer {index} has a blank name")]
    ScorerWithoutName { index: usize },
    #[error("scorers {first} and {second} share the name {name:?}")]
    DuplicateScorerName {
        first: usize,
        second: usize,
        name: String,
    },
    #[error("scorer {index} ({name:?}) is a {kind} scorer, which a {publisher} may not publish")]
    ScorerKindNotAdmitted {
        index: usize,
        name: String,
        kind: String,
        publisher: String,
    },
    #[error("scorer {index} ({name:?}) is a malformed list artifact: {detail}")]
    MalformedListScorer {
        index: usize,
        name: String,
        detail: String,
    },
    #[error("scorer {index} ({name:?}) is {len} bytes, over this build's structural bound")]
    ScorerTooLarge {
        index: usize,
        name: String,
        len: usize,
    },
    #[error("scorer {index} ({name:?}) carries no bytes")]
    EmptyScorer { index: usize, name: String },
    #[error("scorer {index} ({name:?}) is not UTF-8 text")]
    ScorerNotText { index: usize, name: String },
    #[error("keywords scorer {index} ({name:?}) carries no term")]
    KeywordsWithoutTerm { index: usize, name: String },
    #[error(
        "keywords scorer {index} ({name:?}) has a term of {chars} characters, over {MAX_KEYWORD_TERM_CHARS}"
    )]
    KeywordTermTooLong {
        index: usize,
        name: String,
        chars: usize,
    },
}

/// Whether a document applies, and if not, what the transparency surface says
/// instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyStatus {
    /// Every rule applies — save any [`RuleStatus::InertNoRuntime`] rule, which
    /// is inert by itself.
    Applied,
    /// This build does not implement the document's grammar version. Nothing
    /// applies, and the surface says so. **Checked before any structural
    /// rule**, so a newer grammar is never reported as malformed.
    InertUnimplementedVersion { version: u32 },
    /// The document is at a version this build implements but is structurally
    /// wrong. Nothing applies.
    Malformed(PolicyDefect),
}

impl PolicyStatus {
    pub fn is_applied(&self) -> bool {
        matches!(self, Self::Applied)
    }

    /// Whether the document was understood well enough to be *known* not to
    /// apply — the two "we are showing you nothing from this publisher, and
    /// here is why" arms the transparency surface renders.
    pub fn is_inert(&self) -> bool {
        !self.is_applied()
    }
}

/// Whether one rule of an applied document is enforced on this build
/// (module property 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleStatus {
    /// The rule is enforced.
    Applied,
    /// The rule names a scorer whose kind this build knows but cannot run —
    /// today a `prompt` scorer, with no on-device model runtime. The rest of the
    /// document applies; the transparency surface names this rule as *not
    /// applied on this device* (`nest-rules.md` decision 1).
    InertNoRuntime,
}

impl ContentPolicyDocument {
    /// Whether this document applies, evaluated for the publisher that
    /// published it.
    ///
    /// The publisher is needed because a rule may name a factor in its own
    /// namespace, a rule naming a scorer the document does not bundle is
    /// malformed rather than merely inoperative, and the scorer kinds a
    /// document may bundle depend on who published it.
    pub fn status(&self, publisher: &Publisher) -> PolicyStatus {
        // Version FIRST — see the module docs' property 1. Everything below this
        // line is only meaningful for a grammar this build actually implements.
        if self.version != GRAMMAR_VERSION {
            return PolicyStatus::InertUnimplementedVersion {
                version: self.version,
            };
        }
        if let Some(defect) = self.scorer_defect(publisher) {
            return PolicyStatus::Malformed(defect);
        }
        if let Some(defect) = self.rule_defect(publisher) {
            return PolicyStatus::Malformed(defect);
        }
        PolicyStatus::Applied
    }

    /// Whether rule `index` is enforced on this build (module property 4).
    ///
    /// Meaningful only for a document whose [`Self::status`] is applied; an
    /// index past the end answers [`RuleStatus::Applied`], which no caller
    /// reads.
    pub fn rule_status(&self, publisher: &Publisher, index: usize) -> RuleStatus {
        let Some(rule) = self.rules.get(index) else {
            return RuleStatus::Applied;
        };
        match self.scorer_for_factor(publisher, &rule.factor) {
            Some(scorer) if !scorer.kind.runs_on_this_build() => RuleStatus::InertNoRuntime,
            _ => RuleStatus::Applied,
        }
    }

    /// The bundled scorer that produces `factor` for `publisher`, if any.
    pub fn scorer_for_factor(&self, publisher: &Publisher, factor: &str) -> Option<&BundledScorer> {
        let name = factor.strip_prefix(&publisher.factor_prefix())?;
        self.scorers.iter().find(|s| s.name == name)
    }

    /// Structural validation of the bundled scorers. **Never executes a
    /// module** — module property 3.
    fn scorer_defect(&self, publisher: &Publisher) -> Option<PolicyDefect> {
        for (index, scorer) in self.scorers.iter().enumerate() {
            let name = || scorer.name.clone();
            if scorer.name.trim().is_empty() {
                return Some(PolicyDefect::ScorerWithoutName { index });
            }
            if let Some(first) = self.scorers[..index]
                .iter()
                .position(|earlier| earlier.name == scorer.name)
            {
                // Two scorers under one name would make the factor ambiguous,
                // and a factor that names two producers is not a factor.
                return Some(PolicyDefect::DuplicateScorerName {
                    first,
                    second: index,
                    name: name(),
                });
            }
            if !publisher.admits(scorer.kind) {
                return Some(PolicyDefect::ScorerKindNotAdmitted {
                    index,
                    name: name(),
                    kind: scorer_kind_name(scorer.kind).to_string(),
                    publisher: publisher.kind_name().to_string(),
                });
            }
            if scorer.bytes.is_empty() {
                return Some(PolicyDefect::EmptyScorer {
                    index,
                    name: name(),
                });
            }
            if scorer.bytes.len() > MAX_SCORER_BYTES {
                return Some(PolicyDefect::ScorerTooLarge {
                    index,
                    name: name(),
                    len: scorer.bytes.len(),
                });
            }
            match scorer.kind {
                // The built validator, reused rather than re-derived.
                ScorerKind::List => {
                    if let Err(e) = crate::scoring::validate_list_artifact(&scorer.bytes) {
                        return Some(PolicyDefect::MalformedListScorer {
                            index,
                            name: name(),
                            detail: e.to_string(),
                        });
                    }
                }
                // A module's *shape* is the sandbox's business, and the sandbox
                // is `fauna-labeler`, which depends on this crate. The bounds
                // above are the only structural claim this layer may make.
                ScorerKind::Wasm => {}
                ScorerKind::Keywords => {
                    let Ok(text) = std::str::from_utf8(&scorer.bytes) else {
                        return Some(PolicyDefect::ScorerNotText {
                            index,
                            name: name(),
                        });
                    };
                    let terms = split_keyword_terms(text);
                    if terms.is_empty() {
                        return Some(PolicyDefect::KeywordsWithoutTerm {
                            index,
                            name: name(),
                        });
                    }
                    if let Some(chars) = terms
                        .iter()
                        .map(|term| term.chars().count())
                        .find(|chars| *chars > MAX_KEYWORD_TERM_CHARS)
                    {
                        return Some(PolicyDefect::KeywordTermTooLong {
                            index,
                            name: name(),
                            chars,
                        });
                    }
                }
                ScorerKind::Prompt => {
                    if std::str::from_utf8(&scorer.bytes).is_err() {
                        return Some(PolicyDefect::ScorerNotText {
                            index,
                            name: name(),
                        });
                    }
                }
            }
        }
        None
    }

    /// Structural validation of the rules, for `publisher`.
    fn rule_defect(&self, publisher: &Publisher) -> Option<PolicyDefect> {
        let prefix = publisher.factor_prefix();
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.factor.trim().is_empty() {
                return Some(PolicyDefect::BlankFactor { index });
            }
            if let Some(name) = rule.factor.strip_prefix(&prefix) {
                // A rule may name one of this document's own scorers. One that
                // names a scorer the document does not bundle would silently
                // never fire, so it is malformed rather than inoperative — the
                // publisher meant to enforce something and this document
                // cannot.
                if !self.scorers.iter().any(|s| s.name == name) {
                    return Some(PolicyDefect::UnknownScorerFactor {
                        index,
                        factor: rule.factor.clone(),
                    });
                }
            }
            if rule.min_permille > 1000 {
                return Some(PolicyDefect::TriggerOutOfRange {
                    index,
                    permille: rule.min_permille,
                });
            }
            if rule.reason_code.trim().is_empty() {
                return Some(PolicyDefect::RuleWithoutReasonCode { index });
            }
            // The text the blocked reader is shown. A rule that would enforce
            // with nothing to show is refused, not enforced silently.
            let default_reason = rule.reason.get(REASON_DEFAULT_KEY);
            if default_reason.is_none_or(|text| text.trim().is_empty()) {
                return Some(PolicyDefect::RuleWithoutReason { index });
            }
        }
        None
    }
}

/// A scorer kind's wire name.
pub fn scorer_kind_name(kind: ScorerKind) -> &'static str {
    match kind {
        ScorerKind::List => "list",
        ScorerKind::Wasm => "wasm",
        ScorerKind::Keywords => "keywords",
        ScorerKind::Prompt => "prompt",
    }
}

/// Why [`decode_document`] could not produce a document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentDecodeError {
    #[error("the document is {0} bytes, over this build's bound")]
    TooLarge(usize),
    #[error("the document does not decode: {0}")]
    Decode(String),
}

/// Decode an encoded document — **version first** (module property 1).
///
/// A payload whose `version` this build does not implement comes back as a
/// document carrying that version and nothing else, whose [`status`] is
/// therefore inert-and-says-so — even when the rest of it would not decode here
/// at all, which is exactly what a newer grammar's new scorer kind does to an
/// older closed enum.
///
/// [`status`]: ContentPolicyDocument::status
///
/// # Errors
///
/// The payload is over [`MAX_CONTENT_POLICY_PAYLOAD_BYTES`], carries no
/// readable `version`, or is at this build's version and does not decode.
pub fn decode_document(payload: &[u8]) -> Result<ContentPolicyDocument, DocumentDecodeError> {
    #[derive(Deserialize)]
    struct VersionOnly {
        version: u32,
        #[serde(flatten)]
        _rest: BTreeMap<String, fauna_cbor::Value>,
    }
    if payload.len() > MAX_CONTENT_POLICY_PAYLOAD_BYTES {
        return Err(DocumentDecodeError::TooLarge(payload.len()));
    }
    let version = fauna_cbor::decode_strict::<VersionOnly>(payload)
        .map_err(|e| DocumentDecodeError::Decode(format!("{e:?}")))?
        .version;
    if version != GRAMMAR_VERSION {
        return Ok(ContentPolicyDocument {
            version,
            rules: Vec::new(),
            scorers: Vec::new(),
            extra: BTreeMap::new(),
        });
    }
    fauna_cbor::decode_strict(payload).map_err(|e| DocumentDecodeError::Decode(format!("{e:?}")))
}

/// Encode a document as the canonical dag-cbor every carriage signs and stores.
///
/// # Errors
///
/// The encoder refused the value (it carries a float in `extra`).
pub fn encode_document(document: &ContentPolicyDocument) -> Result<Vec<u8>, String> {
    fauna_cbor::encode_canonical(document).map_err(|e| e.to_string())
}

/// One published rule, ready for the fold: the engine rule plus the publisher's
/// own attribution, which the placeholder shows.
///
/// The attribution rides *with* the rule rather than in a parallel vector
/// because the placeholder must name the reason of the rule that actually
/// fired, and a parallel vector is one refactor away from naming a different
/// rule's reason. One shape for every publisher: a region's rule
/// ([`crate::region_policy::RegionRule`]) and a nest's
/// ([`crate::obligation::NestRule`]) are this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedRule {
    pub rule: ObligationRule,
    pub reason_code: String,
    pub reason: BTreeMap<String, String>,
}

impl PublishedRule {
    /// The reason text for `lang`, falling back to the document's required
    /// [`REASON_DEFAULT_KEY`] entry. Shown verbatim.
    pub fn reason_text(&self, lang: &str) -> &str {
        reason_text(&self.reason, lang)
    }
}

/// The entry of a `{ lang → text }` reason map for `lang`, falling back to the
/// required [`REASON_DEFAULT_KEY`] entry.
pub fn reason_text<'a>(reason: &'a BTreeMap<String, String>, lang: &str) -> &'a str {
    reason
        .get(lang)
        .or_else(|| reason.get(REASON_DEFAULT_KEY))
        .map(String::as_str)
        .unwrap_or_default()
}

/// What one document contributes to the fold, for any publisher: its status,
/// the rules this build enforces, and the indices of the rules it cannot
/// (module property 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledRules {
    pub status: PolicyStatus,
    /// Empty unless `status` is [`PolicyStatus::Applied`]; never holds an
    /// inert rule.
    pub rules: Vec<PublishedRule>,
    /// Indices into the document's `rules` that are
    /// [`RuleStatus::InertNoRuntime`] on this build.
    pub inert_rules: Vec<usize>,
}

/// Assemble one document into the rules the composed render call folds — the
/// one assembly every publisher's assembler wraps.
pub fn assemble_rules(document: &ContentPolicyDocument, publisher: &Publisher) -> AssembledRules {
    let status = document.status(publisher);
    if !status.is_applied() {
        return AssembledRules {
            status,
            rules: Vec::new(),
            inert_rules: Vec::new(),
        };
    }
    let mut rules = Vec::new();
    let mut inert_rules = Vec::new();
    for (index, r) in document.rules.iter().enumerate() {
        if document.rule_status(publisher, index) == RuleStatus::InertNoRuntime {
            inert_rules.push(index);
            continue;
        }
        rules.push(PublishedRule {
            rule: ObligationRule {
                category: r.factor.clone(),
                min_confidence_permille: r.min_permille,
                action: r.verdict.action(),
                // A published rule is one more `ObligationRule` and never a new
                // verb. Attestation is the retired server-ingest path's axis and
                // has no meaning at render, so it stays at the permissive floor
                // rather than inventing a second gate on a client-side fold.
                requires_attestation: AttestationLevel::Any,
            },
            reason_code: r.reason_code.clone(),
            reason: r.reason.clone(),
        });
    }
    AssembledRules {
        status,
        rules,
        inert_rules,
    }
}

/// Run a document's `keywords` scorers over an item's text: one label entry,
/// the scorer's factor at [`KEYWORD_MATCH_PERMILLE`], per scorer whose terms the
/// text contains (`nest-rules.md` § The keyword rule).
///
/// The match is the one shared deterministic matcher the muted-keyword path
/// uses ([`crate::keyword::body_excludes_matches`]), so a published keyword rule
/// and a member's own muted word agree on what "contains" means — while staying
/// two different things (theirs is private, this one is published with a
/// reason). The entries join the item's label list before the fold, exactly as a
/// `wasm` scorer's output does; the device runs this post-decrypt and the nest
/// over public plaintext on the publisher leg. A document that does not apply
/// contributes nothing, and a scorer no rule names is never run.
pub fn evaluate_keyword_scorers(
    document: &ContentPolicyDocument,
    publisher: &Publisher,
    text: &str,
) -> Vec<ContentLabelEntry> {
    if !document.status(publisher).is_applied() {
        return Vec::new();
    }
    document
        .scorers
        .iter()
        .filter_map(|scorer| {
            let terms = scorer.keyword_terms()?;
            let factor = scorer_factor(publisher, &scorer.name);
            if !document.rules.iter().any(|rule| rule.factor == factor) {
                return None;
            }
            crate::keyword::body_excludes_matches(&terms, text).then_some(ContentLabelEntry {
                category: factor,
                confidence_per_mille: KEYWORD_MATCH_PERMILLE,
            })
        })
        .collect()
}

/// The domain separator a nest's signature over its content policy opens with
/// (`nest-rules.md` § The nest content policy — *Nest state, written in the
/// app*), so the bytes a nest signs here can never be replayed as a signature
/// over anything else its identity key signs.
pub const NEST_POLICY_SIGNING_DOMAIN: &[u8] = b"fauna.nest.content_policy.v1\0";

/// The bytes a nest's content-policy signature covers: the domain separator,
/// then the canonical dag-cbor of `(document, sequence, issued_at)`.
///
/// One function, used by both [`sign_nest_policy`] and [`verify_nest_policy`],
/// so the two can never drift about what was signed. `document` is the
/// canonical encoding ([`encode_document`]) — the signature is over the exact
/// bytes the member stores, never over a re-encoding of a decoded value.
pub fn nest_policy_signing_input(
    document: &[u8],
    sequence: u64,
    issued_at: i64,
) -> Result<Vec<u8>, String> {
    #[derive(Serialize)]
    struct SigningInput<'a> {
        #[serde(with = "serde_bytes")]
        document: &'a [u8],
        sequence: u64,
        issued_at: i64,
    }
    let body = fauna_cbor::encode_canonical(&SigningInput {
        document,
        sequence,
        issued_at,
    })
    .map_err(|e| e.to_string())?;
    let mut input = NEST_POLICY_SIGNING_DOMAIN.to_vec();
    input.extend_from_slice(&body);
    Ok(input)
}

/// Sign a nest's content policy with the nest's identity key — the nest's own
/// act on each accepted `fauna.admin.content_policy.set`.
///
/// # Errors
///
/// The signing input does not encode.
pub fn sign_nest_policy(
    signing_key: &ed25519_dalek::SigningKey,
    document: &[u8],
    sequence: u64,
    issued_at: i64,
) -> Result<[u8; 64], String> {
    use ed25519_dalek::Signer;
    let input = nest_policy_signing_input(document, sequence, issued_at)?;
    Ok(signing_key.sign(&input).to_bytes())
}

/// Why a nest's signed content policy was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NestPolicyRejection {
    #[error("the nest's identity key is not a valid Ed25519 public key")]
    BadKey,
    #[error("the signature is not 64 bytes")]
    BadSignatureLength,
    #[error("the signature does not verify against the nest's identity key")]
    SignatureFailed,
    #[error("{0}")]
    Encode(String),
    #[error(transparent)]
    Decode(#[from] DocumentDecodeError),
}

/// Verify a nest's signed content policy against the nest's identity key and
/// decode the document — **the one door** to a nest's document, as
/// [`crate::region_authority::VerifiedArtifact::content_policy`] is a region's:
/// a document that was never verified is unrepresentable.
///
/// Decoding is version first ([`decode_document`]), so a newer grammar comes
/// back as an inert document rather than an error.
///
/// # Errors
///
/// The key or signature is malformed, the signature does not verify, or the
/// verified bytes do not decode.
pub fn verify_nest_policy(
    nest_key: &[u8; 32],
    document: &[u8],
    sequence: u64,
    issued_at: i64,
    signature: &[u8],
) -> Result<ContentPolicyDocument, NestPolicyRejection> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(nest_key)
        .map_err(|_| NestPolicyRejection::BadKey)?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| NestPolicyRejection::BadSignatureLength)?;
    let input = nest_policy_signing_input(document, sequence, issued_at)
        .map_err(NestPolicyRejection::Encode)?;
    key.verify_strict(&input, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| NestPolicyRejection::SignatureFailed)?;
    Ok(decode_document(document)?)
}

#[cfg(test)]
mod tests;
