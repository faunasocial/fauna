use std::collections::BTreeMap;

use fauna_core::content_policy::{
    BundledScorer, ContentRule, ContentVerdict, REASON_DEFAULT_KEY, ScorerKind, scorer_factor,
};
use fauna_core::obligation::{RenderVerdict, render_verdict_composed};

use super::*;

fn input(text: &str) -> LabelerPostInput {
    LabelerPostInput::from_card(fauna_core::identity::ActorId([7; 32]), text, &[], false)
}

/// A guardian document blocking at 700 on the factor of its scorer `name`.
fn document(name: &str, kind: ScorerKind, bytes: Vec<u8>) -> ContentPolicyDocument {
    let mut doc = ContentPolicyDocument::empty();
    doc.scorers.push(BundledScorer {
        name: name.to_string(),
        kind,
        bytes,
        extra: BTreeMap::new(),
    });
    doc.rules.push(ContentRule {
        factor: scorer_factor(&Publisher::Guardian, name),
        min_permille: 700,
        verdict: ContentVerdict::Block,
        reason_code: "rule-1".to_string(),
        reason: BTreeMap::from([(
            REASON_DEFAULT_KEY.to_string(),
            "Not for this account".to_string(),
        )]),
        extra: BTreeMap::new(),
    });
    doc
}

fn policy(document: ContentPolicyDocument) -> ContentPolicy {
    ContentPolicy {
        document: Some(document),
        ..Default::default()
    }
}

/// The verdict the guardian source alone gives the joined labels.
fn verdict(policy: &ContentPolicy, labels: &[ContentLabelEntry]) -> RenderVerdict {
    render_verdict_composed(labels, Some(policy), None, &[], &[]).verdict
}

/// A guardian's keyword rule binds on the ward's device: with no document the
/// input is never built; with one, a match blocks and a miss shows.
#[test]
fn a_guardian_keyword_rule_binds_through_the_join() {
    let mut rules = GuardianRules::default();
    let labels: Vec<ContentLabelEntry> = Vec::new();
    assert!(matches!(
        rules.join_labels(None, &labels, || unreachable!("no document, no input")),
        Cow::Borrowed(_)
    ));

    let policy = policy(document(
        "keywords",
        ScorerKind::Keywords,
        b"casino".to_vec(),
    ));
    rules.set_policy(Some(&policy));
    let quiet = rules.join_labels(None, &labels, || input("a quiet afternoon"));
    assert!(matches!(quiet, Cow::Borrowed(_)));
    assert_eq!(verdict(&policy, &quiet), RenderVerdict::Show);
    let joined = rules.join_labels(None, &labels, || input("the CASINO tonight"));
    assert_eq!(verdict(&policy, &joined), RenderVerdict::Block);

    // The guardian lifted supervision: nothing joins any more.
    rules.set_policy(None);
    assert!(matches!(
        rules.join_labels(None, &labels, || unreachable!("no document, no input")),
        Cow::Borrowed(_)
    ));
}

/// A guardian's labeler rule runs its `wasm` module on the ward's device: its
/// factor joins the item's labels and the guardian source blocks on it; the
/// same document handed over again keeps the compiled module, and a different
/// one recompiles — the answer kept for the item under the old document is
/// not reused.
#[cfg(feature = "scorers")]
#[test]
fn a_guardian_labeler_rule_runs_its_module_on_the_wards_device() {
    use fauna_core::obligation::VerdictSource;

    use crate::device::fixed_label_module;

    let item = [9u8; 32];
    let labels: Vec<ContentLabelEntry> = Vec::new();
    let mut rules = GuardianRules::default();
    let strict = policy(document(
        "model",
        ScorerKind::Wasm,
        fixed_label_module("gambling", 0.8),
    ));
    rules.set_policy(Some(&strict));
    assert!(!rules.device.is_prepared(), "compiled on first use");

    let joined = rules.join_labels(Some(&item), &labels, || input("anything"));
    assert_eq!(
        joined.as_ref(),
        [ContentLabelEntry {
            category: scorer_factor(&Publisher::Guardian, "model"),
            confidence_per_mille: 800,
        }]
    );
    let composed = render_verdict_composed(&joined, Some(&strict), None, &[], &[]);
    assert_eq!(composed.verdict, RenderVerdict::Block);
    assert!(
        matches!(
            &composed.source,
            Some(VerdictSource::Guardian(a)) if a.rule.is_some()
        ),
        "the document's labeler rule drove the block, with its reason"
    );

    // The next family status read hands the same document: nothing recompiles.
    rules.set_policy(Some(&strict.clone()));
    assert!(rules.device.is_prepared());

    // The guardian changes the module: recompiled, and below the rule's level.
    let lenient = policy(document(
        "model",
        ScorerKind::Wasm,
        fixed_label_module("gambling", 0.5),
    ));
    rules.set_policy(Some(&lenient));
    assert!(!rules.device.is_prepared());
    let joined = rules.join_labels(Some(&item), &labels, || input("anything"));
    assert_eq!(joined[0].confidence_per_mille, 500);
    assert!(matches!(
        verdict(&lenient, &joined),
        RenderVerdict::Show | RenderVerdict::Badge
    ));
}
