use std::collections::BTreeMap;

use fauna_core::content_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, REASON_DEFAULT_KEY,
    ScorerKind, encode_document, scorer_factor, sign_nest_policy,
};
use fauna_core::identity::ActorKeypair;
use fauna_core::localized::LocalizedText;

use super::*;

const DOMAIN: &str = "example.test";

struct Nest {
    keypair: ActorKeypair,
}

/// A signed record, owned, for [`SignedNestRules`] to borrow.
struct Record {
    document: Vec<u8>,
    sequence: u64,
    issued_at: i64,
    signature: Vec<u8>,
}

impl Record {
    fn signed(&self) -> SignedNestRules<'_> {
        SignedNestRules {
            document: &self.document,
            sequence: self.sequence,
            issued_at: self.issued_at,
            signature: &self.signature,
        }
    }
}

impl Nest {
    fn new(seed: u8) -> Self {
        Self {
            keypair: ActorKeypair::from_secret([seed; 32]),
        }
    }

    fn id(&self) -> String {
        self.keypair.actor_id_hex()
    }

    fn sign(&self, document: &ContentPolicyDocument, sequence: u64) -> Record {
        let document = encode_document(document).unwrap();
        let issued_at = 1_700_000_000 + i64::try_from(sequence).unwrap();
        let signature =
            sign_nest_policy(self.keypair.signing_key(), &document, sequence, issued_at).unwrap();
        Record {
            document,
            sequence,
            issued_at,
            signature: signature.to_vec(),
        }
    }
}

fn reason(default: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), default.to_string())])
}

/// A keywords rule blocking `casino`, and a category rule hiding spam.
fn document() -> ContentPolicyDocument {
    let publisher = Publisher::Nest(DOMAIN.to_string());
    let mut doc = ContentPolicyDocument::empty();
    doc.scorers.push(BundledScorer {
        name: "keywords".to_string(),
        kind: ScorerKind::Keywords,
        bytes: b"casino".to_vec(),
        extra: BTreeMap::new(),
    });
    doc.rules.push(ContentRule {
        factor: scorer_factor(&publisher, "keywords"),
        min_permille: 500,
        verdict: ContentVerdict::Block,
        reason_code: "rule-1".to_string(),
        reason: BTreeMap::from([
            (REASON_DEFAULT_KEY.to_string(), "No gambling".to_string()),
            ("nb".to_string(), "Ingen pengespill".to_string()),
        ]),
        extra: BTreeMap::new(),
    });
    doc.rules.push(ContentRule {
        factor: "spam".to_string(),
        min_permille: 350,
        verdict: ContentVerdict::Collapse,
        reason_code: "rule-2".to_string(),
        reason: reason("No spam"),
        extra: BTreeMap::new(),
    });
    doc
}

fn one_rule() -> ContentPolicyDocument {
    let mut doc = document();
    doc.rules.truncate(1);
    doc
}

#[test]
fn no_document_and_none_known_is_no_rules() {
    let nest = Nest::new(1);
    let mut book = NestRulesBook::default();
    assert_eq!(book.read(&nest.id(), DOMAIN, None), NestRulesRead::NoRules);
    assert!(book.rule_sets_for(&nest.id()).is_empty());
}

#[test]
fn first_read_accepts_and_assembles_the_rule_set() {
    let nest = Nest::new(1);
    let record = nest.sign(&document(), 1);
    let mut book = NestRulesBook::default();

    assert_eq!(
        book.read(&nest.id(), DOMAIN, Some(record.signed())),
        NestRulesRead::First { sequence: 1 }
    );
    let sets = book.rule_sets_for(&nest.id());
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].domain, DOMAIN);
    assert!(sets[0].status.is_applied());
    assert_eq!(sets[0].rules.len(), 2);
    // The id is matched whatever its case.
    assert!(book.get(&nest.id().to_uppercase()).is_some());
}

#[test]
fn a_repeat_is_unchanged_and_a_newer_sequence_is_a_change() {
    let nest = Nest::new(1);
    let mut book = NestRulesBook::default();
    let first = nest.sign(&one_rule(), 1);
    book.read(&nest.id(), DOMAIN, Some(first.signed()));

    assert_eq!(
        book.read(&nest.id(), DOMAIN, Some(first.signed())),
        NestRulesRead::Unchanged
    );

    let second = nest.sign(&document(), 2);
    assert_eq!(
        book.read(&nest.id(), DOMAIN, Some(second.signed())),
        NestRulesRead::Changed {
            domain: DOMAIN.to_string(),
            sequence: 2
        }
    );
    assert_eq!(book.rule_sets_for(&nest.id())[0].rules.len(), 2);
}

#[test]
fn refusals_leave_the_last_known_rules_in_force() {
    let nest = Nest::new(1);
    let mut book = NestRulesBook::default();
    let first = nest.sign(&one_rule(), 1);
    let second = nest.sign(&document(), 2);
    book.read(&nest.id(), DOMAIN, Some(second.signed()));

    // A replay of an older sequence.
    assert_eq!(
        book.read(&nest.id(), DOMAIN, Some(first.signed())),
        NestRulesRead::Refused(NestRulesRefusal::Replay {
            known: 2,
            offered: 1
        })
    );
    // The document left out.
    assert_eq!(
        book.read(&nest.id(), DOMAIN, None),
        NestRulesRead::Refused(NestRulesRefusal::Missing { known: 2 })
    );
    // The sequence in force over different bytes.
    let mut conflicting = nest.sign(&one_rule(), 2);
    conflicting.issued_at = second.issued_at;
    assert_eq!(
        book.read(&nest.id(), DOMAIN, Some(conflicting.signed())),
        NestRulesRead::Refused(NestRulesRefusal::Conflict { sequence: 2 })
    );
    // A newer sequence another key signed.
    let forged = Nest::new(2).sign(&one_rule(), 3);
    assert!(matches!(
        book.read(&nest.id(), DOMAIN, Some(forged.signed())),
        NestRulesRead::Refused(NestRulesRefusal::Rejected(
            NestPolicyRejection::SignatureFailed
        ))
    ));

    let kept = book.get(&nest.id()).unwrap();
    assert_eq!(kept.sequence, 2);
    assert_eq!(kept.rule_set.rules.len(), 2);
}

#[test]
fn a_bad_nest_id_is_refused() {
    let record = Nest::new(1).sign(&document(), 1);
    let mut book = NestRulesBook::default();
    assert_eq!(
        book.read("not-hex", DOMAIN, Some(record.signed())),
        NestRulesRead::Refused(NestRulesRefusal::BadNestId)
    );
}

#[test]
fn rules_are_kept_per_nest() {
    let a = Nest::new(1);
    let b = Nest::new(2);
    let mut book = NestRulesBook::default();
    book.read(&a.id(), "a.test", Some(a.sign(&one_rule(), 1).signed()));

    assert!(book.rule_sets_for(&b.id()).is_empty());
    assert_eq!(book.rule_sets_for(&a.id())[0].domain, "a.test");

    book.forget(&a.id());
    assert!(book.rule_sets_for(&a.id()).is_empty());
}

#[test]
fn stored_records_restore_verified_and_a_tampered_one_is_dropped() {
    let a = Nest::new(1);
    let b = Nest::new(2);
    let mut book = NestRulesBook::default();
    book.read(&a.id(), "a.test", Some(a.sign(&document(), 4).signed()));
    book.read(&b.id(), "b.test", Some(b.sign(&one_rule(), 1).signed()));

    let mut stored = book.stored();
    assert_eq!(NestRulesBook::restore(stored.clone()), book);

    let tampered = stored.iter_mut().find(|r| r.domain == "b.test").unwrap();
    tampered.sequence = 9;
    let restored = NestRulesBook::restore(stored);
    assert_eq!(restored.get(&a.id()).unwrap().sequence, 4);
    assert!(restored.get(&b.id()).is_none());

    // A restored book still tells a change from a repeat.
    let mut restored = restored;
    assert_eq!(
        restored.read(&a.id(), "a.test", Some(a.sign(&document(), 5).signed())),
        NestRulesRead::Changed {
            domain: "a.test".to_string(),
            sequence: 5
        }
    );
}

#[test]
fn the_summary_names_verdict_level_and_reason_in_the_members_language() {
    let nest = Nest::new(1);
    let mut book = NestRulesBook::default();
    book.read(&nest.id(), DOMAIN, Some(nest.sign(&document(), 3).signed()));
    let accepted = book.get(&nest.id()).unwrap();

    let nb = accepted.summary("nb");
    assert_eq!(nb.domain, DOMAIN);
    assert_eq!(nb.sequence, 3);
    assert_eq!(nb.issued_at, 1_700_000_003);
    assert_eq!(nb.not_applied, None);
    assert_eq!(
        nb.rules,
        vec![
            NestRuleLine {
                verdict: LocalizedText::key("rule_editor.verdict.block"),
                // A keyword match always scores 1000: no level to show.
                level: None,
                reason: "Ingen pengespill".to_string(),
                inert: None,
            },
            NestRuleLine {
                verdict: LocalizedText::key("rule_editor.verdict.collapse"),
                level: Some(LocalizedText::key("rule_editor.level.strict")),
                // No Norwegian text: the default.
                reason: "No spam".to_string(),
                inert: None,
            },
        ]
    );
    assert_eq!(accepted.summary("en").rules[0].reason, "No gambling");
}

/// A labeler rule is named *not applied on this device* exactly where the
/// build runs no labeler — without the `scorers` feature it adds no factor.
#[test]
fn a_labeler_rule_is_inert_where_the_build_runs_no_labeler() {
    let publisher = Publisher::Nest(DOMAIN.to_string());
    let mut doc = ContentPolicyDocument::empty();
    doc.scorers.push(BundledScorer {
        name: "model".to_string(),
        kind: ScorerKind::Wasm,
        bytes: b"\0asm".to_vec(),
        extra: BTreeMap::new(),
    });
    doc.rules.push(ContentRule {
        factor: scorer_factor(&publisher, "model"),
        min_permille: 700,
        verdict: ContentVerdict::Block,
        reason_code: "rule-1".to_string(),
        reason: reason("No gambling"),
        extra: BTreeMap::new(),
    });
    let inert = summary(DOMAIN, 1, 0, &doc, "en").rules[0].inert.clone();
    let expected =
        (!cfg!(feature = "scorers")).then(|| LocalizedText::key("rule_editor.prompt_not_applied"));
    assert_eq!(inert, expected);
}

#[test]
fn an_empty_document_summarises_to_no_rules() {
    let s = summary(DOMAIN, 1, 0, &ContentPolicyDocument::empty(), "en");
    assert!(s.rules.is_empty());
    assert_eq!(s.not_applied, None);
}

#[test]
fn a_document_this_build_cannot_apply_says_why() {
    let mut newer = document();
    newer.version += 1;
    assert_eq!(
        summary(DOMAIN, 1, 0, &newer, "en").not_applied,
        Some(NestRulesNotApplied::NewerVersion)
    );

    let mut malformed = document();
    malformed.rules[1].reason = BTreeMap::new();
    assert_eq!(
        summary(DOMAIN, 1, 0, &malformed, "en").not_applied,
        Some(NestRulesNotApplied::Malformed)
    );
}

#[test]
fn a_keyword_rule_joins_its_factor_to_the_labels_of_a_matching_body() {
    use fauna_core::content_category::ContentLabelEntry;
    use fauna_core::obligation::{RenderVerdict, render_verdict_composed};

    let nest = Nest::new(1);
    let record = nest.sign(&document(), 1);
    let mut book = NestRulesBook::default();
    book.read(&nest.id(), DOMAIN, Some(record.signed()));
    let labels = vec![ContentLabelEntry {
        category: "trusted".to_string(),
        confidence_per_mille: 900,
    }];

    // A body the keyword rule does not match borrows the labels untouched.
    let plain = book.join_labels(&nest.id(), None, &labels, || input("a quiet afternoon"));
    assert!(matches!(plain, std::borrow::Cow::Borrowed(_)));

    // A matching body gains the rule's factor, and the fold blocks it,
    // attributed to the nest with the admin's reason.
    let joined = book.join_labels(&nest.id(), None, &labels, || {
        input("Visit the CASINO tonight")
    });
    assert_eq!(joined.len(), 2);
    let sets = book.rule_sets_for(&nest.id());
    let verdict = render_verdict_composed(&joined, None, None, &[], &sets);
    assert_eq!(verdict.verdict, RenderVerdict::Block);
    let source = verdict.nest().expect("the nest drove the block");
    assert_eq!(source.domain, DOMAIN);
    assert_eq!(source.reason_code, "rule-1");

    // A nest with no rules on the device joins nothing, and never builds the
    // scorer input.
    let other = Nest::new(2);
    assert!(matches!(
        book.join_labels(&other.id(), None, &labels, || unreachable!()),
        std::borrow::Cow::Borrowed(_)
    ));
}

fn input(text: &str) -> fauna_core::scoring::LabelerPostInput {
    fauna_core::scoring::LabelerPostInput::from_card(
        fauna_core::identity::ActorId([7; 32]),
        text,
        &[],
        false,
    )
}

/// A nest document's `wasm` labeler runs on the device through the region
/// plane's runner: its factor joins the item's labels, the fold blocks on it
/// attributed to the nest, and a new sequence recompiles.
#[cfg(feature = "scorers")]
#[test]
fn a_labeler_rule_runs_its_module_on_the_device() {
    use fauna_core::content_category::ContentLabelEntry;
    use fauna_core::obligation::{RenderVerdict, render_verdict_composed};

    use crate::device::fixed_label_module;

    let publisher = Publisher::Nest(DOMAIN.to_string());
    let mut doc = ContentPolicyDocument::empty();
    doc.scorers.push(BundledScorer {
        name: "model".to_string(),
        kind: ScorerKind::Wasm,
        bytes: fixed_label_module("gambling", 0.8),
        extra: BTreeMap::new(),
    });
    doc.rules.push(ContentRule {
        factor: scorer_factor(&publisher, "model"),
        min_permille: 700,
        verdict: ContentVerdict::Block,
        reason_code: "rule-1".to_string(),
        reason: reason("No gambling"),
        extra: BTreeMap::new(),
    });
    let nest = Nest::new(1);
    let record = nest.sign(&doc, 1);
    let mut book = NestRulesBook::default();
    book.read(&nest.id(), DOMAIN, Some(record.signed()));

    let labels: Vec<ContentLabelEntry> = Vec::new();
    let joined = book.join_labels(&nest.id(), Some(&[9; 32]), &labels, || input("anything"));
    assert_eq!(
        joined.as_ref(),
        [ContentLabelEntry {
            category: scorer_factor(&publisher, "model"),
            confidence_per_mille: 800,
        }]
    );
    let sets = book.rule_sets_for(&nest.id());
    let verdict = render_verdict_composed(&joined, None, None, &[], &sets);
    assert_eq!(verdict.verdict, RenderVerdict::Block);
    assert_eq!(
        verdict.nest().expect("the nest drove it").reason_code,
        "rule-1"
    );

    // A second sequence below the rule's level: the module is recompiled, and
    // the answer kept for the item under the old sequence is not reused.
    doc.scorers[0].bytes = fixed_label_module("gambling", 0.5);
    let record = nest.sign(&doc, 2);
    book.read(&nest.id(), DOMAIN, Some(record.signed()));
    let joined = book.join_labels(&nest.id(), Some(&[9; 32]), &labels, || input("anything"));
    assert_eq!(joined[0].confidence_per_mille, 500);
    let sets = book.rule_sets_for(&nest.id());
    assert!(matches!(
        render_verdict_composed(&joined, None, None, &[], &sets).verdict,
        RenderVerdict::Show | RenderVerdict::Badge
    ));
}
