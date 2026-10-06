//! The region **content-policy** document — the payload of the content plane's
//! signed artifact, and its fold into the shared render-verdict engine.
//!
//! Owner doc: `docs/goal/behavior/region-blocking.md` § The content plane →
//! *The policy document*, *Where it composes — the render seam*, *Regions
//! compose along the registry's parent chain*. The engine that consumes what
//! this module assembles is [`crate::obligation`] (owner:
//! `docs/goal/behavior/family-client-enforcement.md` § Content policy, whose
//! last bullet records only *that* the engine takes this third source).
//!
//! Three properties this module is built around:
//!
//! 1. **The document is reachable only through a verified artifact.** There is
//!    no public `decode` here:
//!    [`crate::region_authority::VerifiedArtifact::content_policy`] is the one
//!    door, exactly as `feature_policies()` is the sibling plane's — the same
//!    "forgetting is not representable" shape.
//! 2. **Version first, structure second.** A document whose grammar `version`
//!    this build does not implement is **inert and says so** — never
//!    "malformed", because a newer grammar's perfectly good shapes are exactly
//!    what an older reader would misread as malformed.
//!    [`ContentPolicyDocument::status`] checks the version before it looks at a
//!    single rule.
//! 3. **Structural validation only — no execution.** This module validates that
//!    a bundled scorer is *well-formed* (`list` bytes through
//!    [`crate::scoring::validate_list_artifact`], `wasm` bounded, `text-model`
//!    refused). It never *runs* one: the wasm sandbox is `fauna-labeler`, which
//!    depends on `fauna-core`, so a call from here would be a dependency cycle.
//!    Scorer **execution** belongs to the app- and nest-side slices that already
//!    link the labeler.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::obligation::{AttestationLevel, ObligationAction, ObligationRule};
use crate::region_authority::RegionCode;

/// The grammar version this build implements.
///
/// A document at any other version is [`PolicyStatus::InertUnimplementedVersion`]
/// — inert, flagged on the transparency surface, applying nothing. Inert is the
/// fail direction because a region rule can only ever *restrict*, and a blanket
/// block over an undecodable document would blank every item on the device
/// (§ The policy document).
pub const GRAMMAR_VERSION: u32 = 1;

/// The reason map's required entry — the text shown when the app's own language
/// has no entry of its own.
///
/// A reserved key rather than a language tag, so "the fallback" is a fact of the
/// document rather than a guess about which language an authority considers
/// primary.
pub const REASON_DEFAULT_KEY: &str = "default";

/// This plane's bound on a decoded content-policy payload.
///
/// Larger than the feature plane's 64 KiB because a document may carry bundled
/// scorer artifacts; the envelope's own 4 MiB total (§ Publication and signing)
/// remains the outer ceiling, and this is the inner one that refuses a payload
/// which is obviously not a policy document.
pub const MAX_CONTENT_POLICY_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// The largest bundled scorer artifact this build accepts, structurally.
///
/// The *execution* bounds (fuel, memory) are the labeler crate's own constants,
/// applied where a module actually runs; this is only the "obviously not an
/// artifact" refusal that keeps an oversized blob out of the device store.
pub const MAX_SCORER_BYTES: usize = 2 * 1024 * 1024;

/// The two render verbs a region rule may carry.
///
/// Deliberately **not** `#[serde(other)]`-tolerant: the engine has exactly two
/// client render verbs, and a third verb is a *grammar* change, which arrives as
/// a `version` bump and is handled by the inert path (property 2 in the module
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
    /// Render blocked: no reveal, a placeholder naming the authority.
    Block,
}

impl ContentVerdict {
    fn action(self) -> ObligationAction {
        match self {
            Self::Collapse => ObligationAction::Collapse,
            Self::Block => ObligationAction::Block,
        }
    }
}

/// The kinds of bundled scorer artifact a document may carry.
///
/// `text-model` is deliberately absent — it is a *training* artifact, and an
/// authority's transparency obligation is better met by a list or a module whose
/// behaviour experts can read in full (§ The policy document). It therefore
/// arrives as an unknown kind, which makes the document malformed: the refusal
/// that section asks for, expressed once in the type rather than as a check
/// every consumer must remember.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScorerKind {
    /// A content-id → per-mille map. Pure lookup, no execution.
    List,
    /// A deterministic, fuel- and memory-bounded module for the labeler sandbox.
    Wasm,
}

/// One bundled scorer artifact, reusing the tier-3 artifact kinds as built.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundledScorer {
    /// The scorer's name within this document. The factor it produces is
    /// [`scorer_factor`]`(region, name)`.
    pub name: String,
    pub kind: ScorerKind,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    /// Forward-compatible catch-all (`transport.md`'s additive discipline).
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// One verdict rule: a factor, a trigger, a verb, and the authority's own
/// attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentRule {
    /// A label category exactly as the shared engine keys them (the canonical
    /// `nsfw` / `spam` / `phishing` / `commercial`), or a factor one of this
    /// document's bundled scorers produces, namespaced `region:<region>/<name>`.
    pub factor: String,
    /// The same per-mille trigger the engine's rules carry (`0..=1000`).
    pub min_permille: u16,
    pub verdict: ContentVerdict,
    /// A stable, authority-assigned token. Machine-readable provenance for the
    /// transparency surface; never the user-facing text.
    pub reason_code: String,
    /// `{ lang → text }`, with [`REASON_DEFAULT_KEY`] required. Shown to the
    /// user **verbatim** under the app's own frame — the frame is the app's
    /// i18n, the reason is the authority's, and an app never paraphrases an
    /// authority (invariant 1).
    pub reason: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, fauna_cbor::Value>,
}

/// The content-policy payload: one float-free dag-cbor document.
///
/// `Eq` is deliberately not derived — [`fauna_cbor::Value`] carries a float
/// variant. No float can
/// actually arrive through
/// [`crate::region_authority::VerifiedArtifact::content_policy`] (the canonical
/// validator refuses major-7 floats before serde runs), but the type does not
/// get to claim `Eq` on the strength of a caller's discipline.
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
        "rule {index} names factor {factor:?}, which no bundled scorer of this region produces"
    )]
    UnknownRegionFactor { index: usize, factor: String },
    #[error("scorer {index} has a blank name")]
    ScorerWithoutName { index: usize },
    #[error("scorers {first} and {second} share the name {name:?}")]
    DuplicateScorerName {
        first: usize,
        second: usize,
        name: String,
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
}

/// Whether a verified document applies, and if not, what the transparency
/// surface says instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyStatus {
    /// Every rule applies.
    Applied,
    /// This build does not implement the document's grammar version. Nothing
    /// applies, and the surface says so (§ The policy document). **Checked
    /// before any structural rule**, so a newer grammar is never reported as
    /// malformed.
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
    /// apply — the two "we are showing you nothing from this authority, and
    /// here is why" arms the transparency surface renders.
    pub fn is_inert(&self) -> bool {
        !self.is_applied()
    }
}

/// One region rule, ready for the fold: the engine rule plus the authority's own
/// attribution, which the placeholder shows.
///
/// The attribution rides *with* the rule rather than in a parallel vector
/// because the placeholder must name the reason of the rule that actually
/// fired, and a parallel vector is one refactor away from naming a different
/// rule's reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionRule {
    pub rule: ObligationRule,
    pub reason_code: String,
    pub reason: BTreeMap<String, String>,
}

impl RegionRule {
    /// The reason text for `lang`, falling back to the document's required
    /// [`REASON_DEFAULT_KEY`] entry. Shown verbatim.
    pub fn reason_text(&self, lang: &str) -> &str {
        self.reason
            .get(lang)
            .or_else(|| self.reason.get(REASON_DEFAULT_KEY))
            .map(String::as_str)
            .unwrap_or_default()
    }
}

/// Everything one region on the chain contributes to a render decision.
///
/// Carries `status` alongside the rules so a caller holds, in one value, both
/// what to enforce and what the transparency surface must say — an inert or
/// malformed document is never silently indistinguishable from a well-formed
/// document that happens to have no rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionRuleSet {
    pub region: RegionCode,
    /// As the registry names the administering authority — never as the
    /// artifact names itself.
    pub authority_name: String,
    pub status: PolicyStatus,
    /// Empty unless `status` is [`PolicyStatus::Applied`].
    pub rules: Vec<RegionRule>,
}

/// The `region:<region>/<name>` factor a bundled scorer's output carries.
pub fn scorer_factor(region: &RegionCode, scorer_name: &str) -> String {
    format!("region:{region}/{scorer_name}")
}

/// The namespace prefix a factor must be in to be one of `region`'s own scorers.
fn region_factor_prefix(region: &RegionCode) -> String {
    format!("region:{region}/")
}

impl ContentPolicyDocument {
    /// Whether this document applies, evaluated in the context of the region
    /// whose artifact carried it.
    ///
    /// The region is needed because a rule may name a factor in this region's
    /// own `region:<region>/<name>` namespace, and a rule naming a scorer the
    /// document does not bundle is malformed rather than merely inoperative.
    pub fn status(&self, region: &RegionCode) -> PolicyStatus {
        // Version FIRST — see the module docs' property 2. Everything below this
        // line is only meaningful for a grammar this build actually implements.
        if self.version != GRAMMAR_VERSION {
            return PolicyStatus::InertUnimplementedVersion {
                version: self.version,
            };
        }
        if let Some(defect) = self.scorer_defect() {
            return PolicyStatus::Malformed(defect);
        }
        if let Some(defect) = self.rule_defect(region) {
            return PolicyStatus::Malformed(defect);
        }
        PolicyStatus::Applied
    }

    /// Structural validation of the bundled scorers. **Never executes one** —
    /// module property 3.
    fn scorer_defect(&self) -> Option<PolicyDefect> {
        for (index, scorer) in self.scorers.iter().enumerate() {
            if scorer.name.trim().is_empty() {
                return Some(PolicyDefect::ScorerWithoutName { index });
            }
            if let Some(first) = self.scorers[..index]
                .iter()
                .position(|earlier| earlier.name == scorer.name)
            {
                // Two scorers under one name would make `region:<r>/<name>`
                // ambiguous, and a factor that names two producers is not a
                // factor.
                return Some(PolicyDefect::DuplicateScorerName {
                    first,
                    second: index,
                    name: scorer.name.clone(),
                });
            }
            if scorer.bytes.is_empty() {
                return Some(PolicyDefect::EmptyScorer {
                    index,
                    name: scorer.name.clone(),
                });
            }
            if scorer.bytes.len() > MAX_SCORER_BYTES {
                return Some(PolicyDefect::ScorerTooLarge {
                    index,
                    name: scorer.name.clone(),
                    len: scorer.bytes.len(),
                });
            }
            match scorer.kind {
                // The built validator, reused rather than re-derived.
                ScorerKind::List => {
                    if let Err(e) = crate::scoring::validate_list_artifact(&scorer.bytes) {
                        return Some(PolicyDefect::MalformedListScorer {
                            index,
                            name: scorer.name.clone(),
                            detail: e.to_string(),
                        });
                    }
                }
                // A module's *shape* is the sandbox's business, and the sandbox
                // is `fauna-labeler`, which depends on this crate. The bounds
                // above are the only structural claim this layer may make.
                ScorerKind::Wasm => {}
            }
        }
        None
    }

    /// Structural validation of the rules, in the context of `region`.
    fn rule_defect(&self, region: &RegionCode) -> Option<PolicyDefect> {
        let prefix = region_factor_prefix(region);
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.factor.trim().is_empty() {
                return Some(PolicyDefect::BlankFactor { index });
            }
            if let Some(name) = rule.factor.strip_prefix(&prefix) {
                // A rule may name one of this document's own scorers. One that
                // names a scorer the document does not bundle would silently
                // never fire, so it is malformed rather than inoperative — the
                // authority meant to enforce something and this document
                // cannot.
                if !self.scorers.iter().any(|s| s.name == name) {
                    return Some(PolicyDefect::UnknownRegionFactor {
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
            // Invariant 1's text. A rule that would enforce with nothing to show
            // the user is refused, not enforced silently.
            let default_reason = rule.reason.get(REASON_DEFAULT_KEY);
            if default_reason.is_none_or(|text| text.trim().is_empty()) {
                return Some(PolicyDefect::RuleWithoutReason { index });
            }
        }
        None
    }
}

/// Assemble one region's published document into the rule set the composed
/// render call folds.
///
/// Returns an empty rule list for any document that does not apply, with
/// `status` saying which of the two reasons it was.
pub fn rules_from_region_policy(
    document: &ContentPolicyDocument,
    region: &RegionCode,
    authority_name: &str,
) -> RegionRuleSet {
    let status = document.status(region);
    let rules = if status.is_applied() {
        document
            .rules
            .iter()
            .map(|r| RegionRule {
                rule: ObligationRule {
                    category: r.factor.clone(),
                    min_confidence_permille: r.min_permille,
                    action: r.verdict.action(),
                    // A region rule is one more `ObligationRule` and never a new
                    // verb (§ Where it composes). Attestation is the retired
                    // server-ingest path's axis and has no meaning at render, so
                    // it stays at the permissive floor rather than inventing a
                    // second gate on a client-side fold.
                    requires_attestation: AttestationLevel::Any,
                },
                reason_code: r.reason_code.clone(),
                reason: r.reason.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    RegionRuleSet {
        region: region.clone(),
        authority_name: authority_name.to_string(),
        status,
        rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(code: &str) -> RegionCode {
        RegionCode::parse(code).expect("test region code")
    }

    fn reason(text: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), text.to_string())])
    }

    fn rule(factor: &str, permille: u16, verdict: ContentVerdict) -> ContentRule {
        ContentRule {
            factor: factor.to_string(),
            min_permille: permille,
            verdict,
            reason_code: "AUTH-1".into(),
            reason: reason("Restricted under local law."),
            extra: BTreeMap::new(),
        }
    }

    /// A well-formed `list` scorer artifact, encoded exactly as
    /// [`crate::scoring::validate_list_artifact`] expects to read it.
    fn list_artifact_bytes() -> Vec<u8> {
        let artifact = crate::scoring::LabelerListArtifact {
            entries: vec![crate::scoring::ListEntry {
                content_id: serde_bytes::ByteBuf::from(vec![7u8; 32]),
                score: 900,
            }],
            name: Some("violence".into()),
        };
        crate::encoding::canonical_encode(&artifact).expect("encode list artifact")
    }

    fn doc(rules: Vec<ContentRule>) -> ContentPolicyDocument {
        ContentPolicyDocument {
            version: GRAMMAR_VERSION,
            rules,
            scorers: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn a_rule_without_a_reason_is_malformed() {
        // § The policy document: "A rule missing its reason is a malformed
        // document." The reason is what invariant 1 shows the user, so a rule
        // that would enforce without one is refused rather than enforced
        // silently.
        let mut d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)]);
        d.rules[0].reason = BTreeMap::new();
        assert_eq!(
            d.status(&region("NO")),
            PolicyStatus::Malformed(PolicyDefect::RuleWithoutReason { index: 0 })
        );

        // A reason map that has *some* language but not the required default
        // entry is the same defect: an app whose language is missing would have
        // nothing to show.
        let mut d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)]);
        d.rules[0].reason = BTreeMap::from([("nb".to_string(), "Ulovlig".to_string())]);
        assert_eq!(
            d.status(&region("NO")),
            PolicyStatus::Malformed(PolicyDefect::RuleWithoutReason { index: 0 })
        );
    }

    #[test]
    fn a_malformed_document_enforces_nothing() {
        let mut d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)]);
        d.rules[0].reason_code = String::new();
        let set = rules_from_region_policy(&d, &region("NO"), "Authority");
        assert!(
            set.rules.is_empty(),
            "a malformed document enforces nothing"
        );
        assert_eq!(
            set.status,
            PolicyStatus::Malformed(PolicyDefect::RuleWithoutReasonCode { index: 0 })
        );
    }

    #[test]
    fn an_out_of_range_trigger_is_malformed() {
        let d = doc(vec![rule("nsfw", 1001, ContentVerdict::Block)]);
        assert_eq!(
            d.status(&region("NO")),
            PolicyStatus::Malformed(PolicyDefect::TriggerOutOfRange {
                index: 0,
                permille: 1001
            })
        );
    }

    #[test]
    fn an_unknown_version_is_inert_and_reports_itself_inert() {
        // § The policy document: a consumer meeting a version it does not
        // implement treats the document as inert and says so — never silently
        // mis-applies a document it half-understands.
        let mut d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)]);
        d.version = GRAMMAR_VERSION + 7;
        let set = rules_from_region_policy(&d, &region("NO"), "Authority");
        assert!(set.rules.is_empty(), "an inert document enforces nothing");
        assert_eq!(
            set.status,
            PolicyStatus::InertUnimplementedVersion {
                version: GRAMMAR_VERSION + 7
            }
        );
        assert!(set.status.is_inert());
    }

    #[test]
    fn an_unimplemented_version_is_inert_even_when_this_build_reads_it_as_malformed() {
        // Version FIRST, structure second (module property 2). A newer grammar's
        // perfectly good shapes are exactly what an older reader misreads as
        // malformed, so reporting "malformed" here would tell the user the
        // authority published a broken document when it published a newer one.
        let mut d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)]);
        d.version = GRAMMAR_VERSION + 1;
        d.rules[0].reason = BTreeMap::new();
        assert_eq!(
            d.status(&region("NO")),
            PolicyStatus::InertUnimplementedVersion {
                version: GRAMMAR_VERSION + 1
            }
        );
    }

    #[test]
    fn an_applied_document_becomes_engine_rules_carrying_their_reason() {
        let d = doc(vec![
            rule("nsfw", 800, ContentVerdict::Block),
            rule("spam", 400, ContentVerdict::Collapse),
        ]);
        let set = rules_from_region_policy(&d, &region("NO"), "Norwegian Media Authority");
        assert_eq!(set.status, PolicyStatus::Applied);
        assert_eq!(set.authority_name, "Norwegian Media Authority");
        assert_eq!(set.rules.len(), 2);
        assert_eq!(set.rules[0].rule.category, "nsfw");
        assert_eq!(set.rules[0].rule.min_confidence_permille, 800);
        assert_eq!(set.rules[0].rule.action, ObligationAction::Block);
        assert_eq!(set.rules[1].rule.action, ObligationAction::Collapse);
        // The authority's text rides with the rule, and an app language with no
        // entry of its own falls back to the required default.
        assert_eq!(
            set.rules[0].reason_text("nb"),
            "Restricted under local law."
        );
    }

    #[test]
    fn a_region_factor_must_name_a_scorer_the_document_bundles() {
        let d = doc(vec![rule("region:NO/violence", 500, ContentVerdict::Block)]);
        assert_eq!(
            d.status(&region("NO")),
            PolicyStatus::Malformed(PolicyDefect::UnknownRegionFactor {
                index: 0,
                factor: "region:NO/violence".into()
            })
        );
    }

    #[test]
    fn a_bundled_list_scorer_is_validated_structurally_and_names_its_factor() {
        // Property 3: `list` bytes go through the built validator; nothing is
        // ever executed here.
        let bytes = list_artifact_bytes();
        let d = ContentPolicyDocument {
            version: GRAMMAR_VERSION,
            rules: vec![rule("region:NO/violence", 500, ContentVerdict::Block)],
            scorers: vec![BundledScorer {
                name: "violence".into(),
                kind: ScorerKind::List,
                bytes,
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        assert_eq!(d.status(&region("NO")), PolicyStatus::Applied);
        assert_eq!(
            scorer_factor(&region("NO"), "violence"),
            "region:NO/violence"
        );

        // Garbage bytes under the `list` kind are a malformed document.
        let mut bad = d.clone();
        bad.scorers[0].bytes = vec![0xff, 0xff, 0xff];
        assert!(matches!(
            bad.status(&region("NO")),
            PolicyStatus::Malformed(PolicyDefect::MalformedListScorer { index: 0, .. })
        ));
    }

    #[test]
    fn a_text_model_scorer_kind_does_not_decode_at_v1() {
        // § The policy document: `text-model` is not admitted at v1. It is not a
        // `ScorerKind` variant at all, so a document carrying one fails to
        // decode rather than needing a check every consumer must remember.
        let json = r#"{"name":"m","kind":"text-model","bytes":[]}"#;
        assert!(serde_json::from_str::<BundledScorer>(json).is_err());
    }

    #[test]
    fn a_scorer_over_the_structural_bound_is_malformed() {
        let d = ContentPolicyDocument {
            version: GRAMMAR_VERSION,
            rules: Vec::new(),
            scorers: vec![BundledScorer {
                name: "big".into(),
                kind: ScorerKind::Wasm,
                bytes: vec![0u8; MAX_SCORER_BYTES + 1],
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        assert!(matches!(
            d.status(&region("NO")),
            PolicyStatus::Malformed(PolicyDefect::ScorerTooLarge { index: 0, .. })
        ));
    }

    #[test]
    fn an_unknown_verdict_verb_does_not_decode() {
        // The engine has exactly two client render verbs; a third arrives as a
        // grammar version bump, not as a new string inside v1.
        let json = r#"{"factor":"nsfw","min_permille":500,"verdict":"quarantine","reason_code":"X","reason":{"default":"t"}}"#;
        assert!(serde_json::from_str::<ContentRule>(json).is_err());
    }

    #[test]
    fn an_unknown_document_field_round_trips_through_extra() {
        // transport.md's additive discipline: a field a newer publisher added is
        // captured, not dropped, so a re-encode by an older reader preserves it.
        let json = r#"{"version":1,"rules":[],"scorers":[],"published_by":"a newer field"}"#;
        let d: ContentPolicyDocument = serde_json::from_str(json).expect("decodes");
        assert_eq!(d.extra.len(), 1);
        assert!(d.extra.contains_key("published_by"));
    }
}
