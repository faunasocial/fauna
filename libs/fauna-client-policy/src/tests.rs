use std::collections::BTreeMap;

use fauna_core::content_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION, Publisher,
    REASON_DEFAULT_KEY, ScorerKind, decode_document, encode_document,
};
use fauna_core::obligation::{ContentFloor, ContentPolicy, GUARDIAN_FLOOR_TRIGGER_PERMILLE};
use fauna_core::region_authority::RegionCode;

use super::*;

fn nest() -> Publisher {
    Publisher::Nest("example.test".to_string())
}

fn region() -> Publisher {
    Publisher::Region(RegionCode::parse("NO").expect("a region code"))
}

fn publishers() -> [Publisher; 3] {
    [region(), nest(), Publisher::Guardian]
}

fn keys(texts: &[fauna_core::localized::LocalizedText]) -> Vec<&str> {
    texts.iter().map(|t| t.key.as_str()).collect()
}

/// One rule of every offered kind, each with its subject and reason written.
fn every_kind(editor: &mut RuleEditor) {
    let c = editor.add_rule(RuleKind::Category).unwrap();
    assert!(editor.set_category(c, "spam"));
    assert!(editor.set_reason(c, REASON_DEFAULT_KEY, "No spam here"));
    assert!(editor.set_reason(c, "nb", "Ikke søppel her"));
    assert!(editor.set_level(c, 3));

    let k = editor.add_rule(RuleKind::Keywords).unwrap();
    assert!(editor.set_keywords_text(k, "  casino \n\nlottery\n"));
    assert!(editor.set_verdict(k, RuleVerdict::Collapse));
    assert!(editor.set_reason(k, REASON_DEFAULT_KEY, "Gambling"));

    let l = editor.add_rule(RuleKind::Labeler).unwrap();
    assert!(editor.set_labeler(l, "acme", b"\0asm-module".to_vec()));
    assert!(editor.set_reason(l, REASON_DEFAULT_KEY, "Acme says so"));

    let p = editor.add_rule(RuleKind::Prompt).unwrap();
    assert!(editor.set_prompt(p, "Is this cruel to animals?"));
    assert!(editor.set_reason(p, REASON_DEFAULT_KEY, "Cruelty"));
}

#[test]
fn every_kind_round_trips_for_every_publisher() {
    for publisher in publishers() {
        let mut editor = RuleEditor::load(None, publisher.clone());
        every_kind(&mut editor);
        assert_eq!(editor.validate(), Vec::new(), "{publisher:?}");
        assert!(editor.can_save());

        let document = editor.document();
        assert_eq!(document.version, GRAMMAR_VERSION);
        assert!(document.status(&publisher).is_applied());
        let bytes = encode_document(&document).unwrap();
        let decoded = decode_document(&bytes).unwrap();
        assert_eq!(decoded, document);

        let reloaded = RuleEditor::load(Some(decoded), publisher.clone());
        assert_eq!(reloaded.document(), document);
        let kinds: Vec<_> = reloaded.view().rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                RuleKind::Category,
                RuleKind::Keywords,
                RuleKind::Labeler,
                RuleKind::Prompt
            ]
        );
        let view = reloaded.view();
        assert_eq!(view.rows[0].category.as_deref(), Some("spam"));
        assert_eq!(view.rows[0].level_index, Some(3));
        assert_eq!(view.rows[1].keywords.as_deref(), Some("casino\nlottery"));
        assert_eq!(view.rows[1].verdict, RuleVerdict::Collapse);
        assert_eq!(
            view.rows[1].level_index, None,
            "a keyword match has no level"
        );
        assert_eq!(view.rows[2].labeler.as_deref(), Some("acme"));
        assert_eq!(
            view.rows[3].prompt.as_deref(),
            Some("Is this cruel to animals?")
        );
        assert_eq!(view.rows[0].reasons[0].lang, REASON_DEFAULT_KEY);
        assert_eq!(view.rows[0].reasons[1].text, "Ikke søppel her");
    }
}

#[test]
fn the_list_kind_is_offered_to_no_publisher() {
    for publisher in publishers() {
        let mut editor = RuleEditor::load(None, publisher);
        let offered: Vec<_> = editor.view().kinds.iter().map(|k| k.kind).collect();
        assert_eq!(offered, OFFERED_KINDS);
        assert_eq!(editor.add_rule(RuleKind::List), None);
        assert!(editor.document().rules.is_empty());
    }
}

fn list_document(publisher: &Publisher) -> ContentPolicyDocument {
    let mut document = ContentPolicyDocument::empty();
    document.scorers.push(BundledScorer {
        name: "ids".into(),
        kind: ScorerKind::List,
        bytes: b"not-a-list".to_vec(),
        extra: BTreeMap::new(),
    });
    document.rules.push(ContentRule {
        factor: format!("{}ids", publisher.factor_prefix()),
        min_permille: 500,
        verdict: ContentVerdict::Block,
        reason_code: "ids".into(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.into(), "Listed".into())]),
        extra: BTreeMap::new(),
    });
    document
}

#[test]
fn a_loaded_list_rule_is_read_only_refused_on_a_nest_and_removable() {
    let mut editor = RuleEditor::load(Some(list_document(&nest())), nest());
    let row = &editor.view().rows[0];
    assert_eq!(row.kind, RuleKind::List);
    assert!(row.read_only);
    assert!(!editor.set_verdict(0, RuleVerdict::Collapse));
    assert!(!editor.set_kind(0, RuleKind::Category));
    assert_eq!(
        keys(&editor.validate()),
        ["rule_editor.defect.list_not_admitted"]
    );
    assert_eq!(editor.validate()[0].args["rule"], "1");
    assert!(!editor.can_save());

    assert!(editor.remove_rule(0));
    assert!(editor.document().rules.is_empty());
    assert!(
        editor.document().scorers.is_empty(),
        "the unreferenced scorer goes with its rule"
    );
    assert!(editor.can_save());
}

#[test]
fn an_empty_keywords_rule_is_refused() {
    let mut editor = RuleEditor::load(None, nest());
    let k = editor.add_rule(RuleKind::Keywords).unwrap();
    editor.set_reason(k, REASON_DEFAULT_KEY, "Words");
    assert_eq!(keys(&editor.validate()), ["rule_editor.defect.no_keyword"]);
    editor.set_keywords_text(k, " \n ");
    assert_eq!(keys(&editor.validate()), ["rule_editor.defect.no_keyword"]);
    assert!(!editor.can_save());
}

#[test]
fn a_missing_default_reason_is_refused() {
    let mut editor = RuleEditor::load(None, Publisher::Guardian);
    editor.add_rule(RuleKind::Category).unwrap();
    let r = editor.add_rule(RuleKind::Category).unwrap();
    editor.set_reason(0, REASON_DEFAULT_KEY, "First");
    editor.set_reason(r, "nb", "Bare norsk");
    let defects = editor.validate();
    assert_eq!(keys(&defects), ["rule_editor.defect.no_reason"]);
    assert_eq!(defects[0].args["rule"], "2", "1-based rule number");
    assert!(!editor.can_save());
}

#[test]
fn a_loaded_off_scale_permille_is_kept_until_changed() {
    let mut document = ContentPolicyDocument::empty();
    document.rules.push(ContentRule {
        factor: "nsfw".into(),
        min_permille: 610,
        verdict: ContentVerdict::Collapse,
        reason_code: "custom".into(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.into(), "Adult".into())]),
        extra: BTreeMap::new(),
    });
    let mut editor = RuleEditor::load(Some(document), Publisher::Guardian);
    let shown = editor.view().rows[0].level_index.unwrap() as usize;
    assert_eq!(shown, 3, "610 is nearer 700 than 500");
    assert_eq!(nearest_level(425), 1, "a tie shows as the stricter level");
    assert_ne!(LEVELS[shown].permille, 610);

    // Unrelated edits leave it alone.
    editor.set_verdict(0, RuleVerdict::Block);
    editor.set_reason(0, "nb", "Voksen");
    assert_eq!(editor.document().rules[0].min_permille, 610);
    assert_eq!(editor.document().rules[0].reason_code, "custom");

    editor.set_level(0, shown);
    assert_eq!(
        editor.document().rules[0].min_permille,
        LEVELS[shown].permille
    );
}

#[test]
fn a_prompt_rule_is_flagged_not_applied_on_this_device() {
    let mut editor = RuleEditor::load(None, nest());
    every_kind(&mut editor);
    let rows = editor.view().rows;
    let inert: Vec<_> = rows.iter().map(|r| r.inert.is_some()).collect();
    assert_eq!(inert, [false, false, false, true]);
    assert_eq!(
        rows[3].inert.as_ref().unwrap().key,
        "rule_editor.prompt_not_applied"
    );
}

#[test]
fn the_default_level_is_the_shared_floor_trigger() {
    assert_eq!(
        LEVELS[DEFAULT_LEVEL_INDEX].permille,
        GUARDIAN_FLOOR_TRIGGER_PERMILLE
    );
    let mut editor = RuleEditor::load(None, nest());
    let c = editor.add_rule(RuleKind::Category).unwrap();
    assert_eq!(
        editor.document().rules[c].min_permille,
        GUARDIAN_FLOOR_TRIGGER_PERMILLE
    );
    assert_eq!(
        editor.view().rows[c].level_index,
        Some(DEFAULT_LEVEL_INDEX as u32)
    );
}

#[test]
fn reason_codes_are_minted_per_new_rule_and_kept_on_load() {
    let mut editor = RuleEditor::load(Some(list_document(&region())), region());
    let a = editor.add_rule(RuleKind::Category).unwrap();
    let b = editor.add_rule(RuleKind::Keywords).unwrap();
    let doc = editor.document();
    assert_eq!(doc.rules[0].reason_code, "ids");
    assert_ne!(doc.rules[a].reason_code, doc.rules[b].reason_code);
    assert!(!doc.rules[a].reason_code.is_empty());
    let reloaded = RuleEditor::load(Some(doc.clone()), region());
    assert_eq!(
        reloaded.document().rules[b].reason_code,
        doc.rules[b].reason_code
    );
}

#[test]
fn loaded_extra_fields_survive_every_edit() {
    let extra = || BTreeMap::from([("future".to_string(), fauna_cbor::Value::Integer(7))]);
    let mut document = ContentPolicyDocument::empty();
    document.extra = extra();
    document.rules.push(ContentRule {
        factor: "spam".into(),
        min_permille: 500,
        verdict: ContentVerdict::Block,
        reason_code: "spam".into(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.into(), "Spam".into())]),
        extra: extra(),
    });
    let mut editor = RuleEditor::load(Some(document), nest());
    editor.set_category(0, "phishing");
    editor.set_level(0, 0);
    every_kind(&mut editor);
    let out = editor.document();
    assert_eq!(out.extra, extra());
    assert_eq!(out.rules[0].extra, extra());
}

#[test]
fn changing_kind_swaps_the_scorer_and_drops_the_old_one() {
    let mut editor = RuleEditor::load(None, nest());
    let r = editor.add_rule(RuleKind::Keywords).unwrap();
    editor.set_keywords_text(r, "casino");
    assert_eq!(editor.document().scorers.len(), 1);
    assert!(editor.set_kind(r, RuleKind::Category));
    assert!(editor.document().scorers.is_empty());
    assert_eq!(editor.view().rows[r].category.as_deref(), Some("nsfw"));
}

#[test]
fn the_labeler_picker_offers_the_bundled_labelers_then_the_publishers_subscriptions() {
    let id = "ABCDEF0123456789".repeat(4);
    let name = labeler_scorer_name(&id);
    assert_eq!(name, "labeler-abcdef012345");

    let mut editor = RuleEditor::load(None, nest());
    let a = editor.add_rule(RuleKind::Labeler).unwrap();
    // Nothing bundled yet: every subscription is offered, with its id.
    assert_eq!(
        editor.labeler_choices(std::slice::from_ref(&id)),
        [LabelerChoice {
            name: name.clone(),
            labeler_id: Some(id.clone()),
        }]
    );
    // The mounting fetched the module and bundled it under the choice's name.
    assert!(editor.set_labeler(a, &name, b"module".to_vec()));
    // Now it is bundled: offered once, as bundled, so a second rule shares it.
    assert_eq!(
        editor.labeler_choices(std::slice::from_ref(&id)),
        [LabelerChoice {
            name: name.clone(),
            labeler_id: None,
        }]
    );
    let b = editor.add_rule(RuleKind::Labeler).unwrap();
    assert!(editor.choose_bundled_labeler(b, &name));
    assert!(!editor.choose_bundled_labeler(b, "no-such-labeler"));
    let doc = editor.document();
    assert_eq!(doc.rules[a].factor, doc.rules[b].factor);
    assert_eq!(doc.scorers.iter().filter(|s| s.name == name).count(), 1);
}

#[test]
fn two_rules_on_one_labeler_share_its_scorer() {
    let mut editor = RuleEditor::load(None, nest());
    let a = editor.add_rule(RuleKind::Labeler).unwrap();
    let b = editor.add_rule(RuleKind::Labeler).unwrap();
    editor.set_labeler(a, "acme", b"m1".to_vec());
    editor.set_labeler(b, "acme", b"m1".to_vec());
    let doc = editor.document();
    assert_eq!(doc.scorers.len(), 1);
    assert_eq!(doc.rules[a].factor, doc.rules[b].factor);
    let labelers: Vec<_> = editor
        .view()
        .labelers
        .iter()
        .map(|l| l.name.clone())
        .collect();
    assert_eq!(labelers, ["acme"]);
    editor.remove_rule(a);
    assert_eq!(
        editor.document().scorers.len(),
        1,
        "still used by the other rule"
    );
}

#[test]
fn only_block_and_collapse_are_offered() {
    let editor = RuleEditor::load(None, Publisher::Guardian);
    let verdicts: Vec<_> = editor.view().verdicts.iter().map(|v| v.verdict).collect();
    assert_eq!(verdicts, [RuleVerdict::Block, RuleVerdict::Collapse]);
}

#[test]
fn a_newer_grammar_is_shown_and_never_overwritten() {
    let newer = ContentPolicyDocument {
        version: GRAMMAR_VERSION + 1,
        ..ContentPolicyDocument::empty()
    };
    let mut editor = RuleEditor::load(Some(newer.clone()), nest());
    assert!(!editor.view().editable);
    assert_eq!(editor.add_rule(RuleKind::Category), None);
    assert_eq!(keys(&editor.validate()), ["rule_editor.newer_version"]);
    assert!(!editor.can_save());
    assert_eq!(editor.document(), newer);
}

#[test]
fn a_set_guardian_floor_shows_as_a_rule() {
    let policy = ContentPolicy {
        nsfw: ContentFloor::Block,
        spam: ContentFloor::Collapse,
        phishing: ContentFloor::Inherit,
        commercial: ContentFloor::Unknown,
        document: None,
    };
    let editor = RuleEditor::load_guardian(Some(&policy));
    let doc = editor.document();
    let rules: Vec<_> = doc
        .rules
        .iter()
        .map(|r| (r.factor.as_str(), r.verdict, r.min_permille))
        .collect();
    assert_eq!(
        rules,
        [
            (
                "nsfw",
                ContentVerdict::Block,
                GUARDIAN_FLOOR_TRIGGER_PERMILLE
            ),
            (
                "spam",
                ContentVerdict::Collapse,
                GUARDIAN_FLOOR_TRIGGER_PERMILLE
            ),
            // An unknown floor renders blocked, so it shows as a block rule.
            (
                "commercial",
                ContentVerdict::Block,
                GUARDIAN_FLOOR_TRIGGER_PERMILLE
            ),
        ]
    );
    // A floor carries no reason of its own: the guardian writes one before saving.
    assert_eq!(keys(&editor.validate()), ["rule_editor.defect.no_reason"]);

    let none = RuleEditor::load_guardian(None);
    assert!(none.document().rules.is_empty());
    assert!(none.can_save());
}

#[test]
fn the_guardian_document_and_its_floors_load_together() {
    let mut document = ContentPolicyDocument::empty();
    document.rules.push(ContentRule {
        factor: "phishing".into(),
        min_permille: 200,
        verdict: ContentVerdict::Block,
        reason_code: "rule-1".into(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.into(), "Scams".into())]),
        extra: BTreeMap::new(),
    });
    let policy = ContentPolicy {
        nsfw: ContentFloor::Collapse,
        document: Some(document),
        ..ContentPolicy::default()
    };
    let doc = RuleEditor::load_guardian(Some(&policy)).document();
    let factors: Vec<_> = doc.rules.iter().map(|r| r.factor.as_str()).collect();
    assert_eq!(factors, ["phishing", "nsfw"]);
    assert_ne!(doc.rules[0].reason_code, doc.rules[1].reason_code);
}

#[test]
fn every_key_the_editor_emits_is_a_string() {
    let mut editor = RuleEditor::load(Some(list_document(&nest())), nest());
    every_kind(&mut editor);
    let view = editor.view();
    let mut texts: Vec<fauna_core::localized::LocalizedText> = Vec::new();
    texts.extend(view.kinds.iter().map(|k| k.label.clone()));
    texts.extend(view.verdicts.iter().map(|v| v.label.clone()));
    texts.extend(view.levels.iter().map(|l| l.label.clone()));
    texts.extend(view.categories.iter().map(|c| c.label.clone()));
    texts.extend(view.rows.iter().map(|r| r.kind_label.clone()));
    texts.extend(view.rows.iter().filter_map(|r| r.inert.clone()));
    texts.extend(view.defects.iter().cloned());
    texts.push(fauna_core::localized::LocalizedText::key(
        "rule_editor.newer_version",
    ));
    for key in [
        "no_reason",
        "no_reason_code",
        "level_out_of_range",
        "no_subject",
        "missing_scorer",
        "malformed_scorer",
        "list_not_admitted",
        "scorer_too_large",
        "no_keyword",
        "keyword_too_long",
        "no_prompt",
        "no_labeler",
        "unused_scorer",
    ] {
        texts.push(fauna_core::localized::LocalizedText::key(format!(
            "rule_editor.defect.{key}"
        )));
    }
    assert!(
        !view.defects.is_empty(),
        "the list rule's defect is among them"
    );
    for text in texts {
        assert!(
            fauna_i18n::strings::lookup(&text.key).is_some(),
            "{} is not in en.yaml",
            text.key
        );
    }
}

#[test]
fn the_inline_keywords_form_round_trips_through_a_single_line_input() {
    let mut editor = RuleEditor::load(None, nest());
    let k = editor.add_rule(RuleKind::Keywords).unwrap();
    // Commas and line breaks both separate terms; blanks are dropped.
    assert!(editor.set_keywords_inline(k, " casino, lottery ,,\npoker "));
    let row = &editor.view().rows[k];
    assert_eq!(row.keywords.as_deref(), Some("casino\nlottery\npoker"));
    assert_eq!(
        keywords_inline(row.keywords.as_deref().unwrap()),
        "casino, lottery, poker"
    );
    // The inline setter is a keywords-rule setter like the others.
    let c = editor.add_rule(RuleKind::Category).unwrap();
    assert!(!editor.set_keywords_inline(c, "casino"));
}
