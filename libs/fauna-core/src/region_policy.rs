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
//! Since grammar version 2 the document, its validator and its scorer kinds are
//! the publisher-aware [`crate::content_policy`], which a nest and a guardian
//! publish in too (`docs/goal/behavior/nest-rules.md`); this module keeps its
//! name as the **region's assembler** over it, and re-exports the document types
//! so the region plane's callers read on unchanged. The properties below are the
//! region's view of the shared module's.
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

use serde::{Deserialize, Serialize};

use crate::region_authority::RegionCode;

pub use crate::content_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION,
    MAX_CONTENT_POLICY_PAYLOAD_BYTES, MAX_SCORER_BYTES, PolicyDefect, PolicyStatus, Publisher,
    REASON_DEFAULT_KEY, ScorerKind,
};

/// One region rule, ready for the fold — the one published-rule shape every
/// publisher shares ([`crate::content_policy::PublishedRule`]).
pub type RegionRule = crate::content_policy::PublishedRule;

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
    /// Empty unless `status` is [`PolicyStatus::Applied`]; never holds a rule
    /// this build cannot enforce.
    pub rules: Vec<RegionRule>,
    /// Indices into the document's rules that are inert on this build — a
    /// `prompt` rule with no on-device runtime
    /// ([`crate::content_policy::RuleStatus::InertNoRuntime`]), which the
    /// transparency surface names as not applied on this device.
    #[serde(default)]
    pub inert_rules: Vec<usize>,
}

/// The `region:<region>/<name>` factor a bundled scorer's output carries.
pub fn scorer_factor(region: &RegionCode, scorer_name: &str) -> String {
    crate::content_policy::scorer_factor(&Publisher::Region(region.clone()), scorer_name)
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
    let assembled =
        crate::content_policy::assemble_rules(document, &Publisher::Region(region.clone()));
    RegionRuleSet {
        region: region.clone(),
        authority_name: authority_name.to_string(),
        status: assembled.status,
        rules: assembled.rules,
        inert_rules: assembled.inert_rules,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::obligation::ObligationAction;

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
            d.status(&Publisher::Region(region("NO"))),
            PolicyStatus::Malformed(PolicyDefect::RuleWithoutReason { index: 0 })
        );

        // A reason map that has *some* language but not the required default
        // entry is the same defect: an app whose language is missing would have
        // nothing to show.
        let mut d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)]);
        d.rules[0].reason = BTreeMap::from([("nb".to_string(), "Ulovlig".to_string())]);
        assert_eq!(
            d.status(&Publisher::Region(region("NO"))),
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
            d.status(&Publisher::Region(region("NO"))),
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
            d.status(&Publisher::Region(region("NO"))),
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
            d.status(&Publisher::Region(region("NO"))),
            PolicyStatus::Malformed(PolicyDefect::UnknownScorerFactor {
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
        assert_eq!(
            d.status(&Publisher::Region(region("NO"))),
            PolicyStatus::Applied
        );
        assert_eq!(
            scorer_factor(&region("NO"), "violence"),
            "region:NO/violence"
        );

        // Garbage bytes under the `list` kind are a malformed document.
        let mut bad = d.clone();
        bad.scorers[0].bytes = vec![0xff, 0xff, 0xff];
        assert!(matches!(
            bad.status(&Publisher::Region(region("NO"))),
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
            d.status(&Publisher::Region(region("NO"))),
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
        let json = r#"{"version":2,"rules":[],"scorers":[],"published_by":"a newer field"}"#;
        let d: ContentPolicyDocument = serde_json::from_str(json).expect("decodes");
        assert_eq!(d.extra.len(), 1);
        assert!(d.extra.contains_key("published_by"));
    }
}
