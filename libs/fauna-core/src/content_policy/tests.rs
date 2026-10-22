use super::*;

fn region(code: &str) -> RegionCode {
    RegionCode::parse(code).expect("test region code")
}

fn nest() -> Publisher {
    Publisher::Nest("example.org".into())
}

fn reason(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), text.to_string())])
}

fn rule(factor: &str, permille: u16, verdict: ContentVerdict) -> ContentRule {
    ContentRule {
        factor: factor.to_string(),
        min_permille: permille,
        verdict,
        reason_code: "R-1".into(),
        reason: reason("Not on this nest."),
        extra: BTreeMap::new(),
    }
}

fn scorer(name: &str, kind: ScorerKind, bytes: &[u8]) -> BundledScorer {
    BundledScorer {
        name: name.into(),
        kind,
        bytes: bytes.to_vec(),
        extra: BTreeMap::new(),
    }
}

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

fn doc(rules: Vec<ContentRule>, scorers: Vec<BundledScorer>) -> ContentPolicyDocument {
    ContentPolicyDocument {
        version: GRAMMAR_VERSION,
        rules,
        scorers,
        extra: BTreeMap::new(),
    }
}

#[test]
fn the_factor_namespace_is_the_publishers() {
    assert_eq!(
        scorer_factor(&Publisher::Region(region("NO")), "v"),
        "region:NO/v"
    );
    assert_eq!(scorer_factor(&nest(), "v"), "nest:example.org/v");
    assert_eq!(scorer_factor(&Publisher::Guardian, "v"), "guardian:v");
}

#[test]
fn a_list_scorer_is_admitted_for_a_region_only() {
    // nest-rules.md decision 2: content-id lists are a government's instrument
    // only; a nest or guardian document bundling one is malformed.
    let d = doc(
        vec![rule("region:NO/violence", 500, ContentVerdict::Block)],
        vec![scorer("violence", ScorerKind::List, &list_artifact_bytes())],
    );
    assert_eq!(
        d.status(&Publisher::Region(region("NO"))),
        PolicyStatus::Applied
    );
    for publisher in [nest(), Publisher::Guardian] {
        let mut d = d.clone();
        d.rules[0].factor = scorer_factor(&publisher, "violence");
        assert_eq!(
            d.status(&publisher),
            PolicyStatus::Malformed(PolicyDefect::ScorerKindNotAdmitted {
                index: 0,
                name: "violence".into(),
                kind: "list".into(),
                publisher: publisher.kind_name().into(),
            }),
            "{publisher:?} may not bundle a list"
        );
    }
}

#[test]
fn a_document_at_another_version_is_inert_even_when_it_would_not_decode() {
    // A v3 document carrying a scorer kind this build has never heard of — the
    // closed enum cannot decode it, and the version peek reads it as inert, never
    // as malformed (module property 1).
    #[derive(Serialize)]
    struct Future {
        version: u32,
        scorers: Vec<BTreeMap<String, String>>,
    }
    let bytes = fauna_cbor::encode_canonical(&Future {
        version: GRAMMAR_VERSION + 1,
        scorers: vec![BTreeMap::from([
            ("name".to_string(), "m".to_string()),
            ("kind".to_string(), "text-model".to_string()),
        ])],
    })
    .unwrap();
    let decoded = decode_document(&bytes).expect("a newer version decodes as inert");
    assert_eq!(
        decoded.status(&nest()),
        PolicyStatus::InertUnimplementedVersion {
            version: GRAMMAR_VERSION + 1
        }
    );
    // A v1 document (the grammar before the publisher dimension) is inert on this
    // build in the same way.
    let mut v1 = doc(vec![rule("nsfw", 800, ContentVerdict::Block)], Vec::new());
    v1.version = 1;
    let assembled = assemble_rules(&v1, &Publisher::Region(region("NO")));
    assert!(assembled.rules.is_empty());
    assert_eq!(
        assembled.status,
        PolicyStatus::InertUnimplementedVersion { version: 1 }
    );
}

#[test]
fn decode_round_trips_a_document_at_this_version() {
    let d = doc(
        vec![rule(
            "nest:example.org/words",
            1000,
            ContentVerdict::Collapse,
        )],
        vec![scorer("words", ScorerKind::Keywords, b"casino\nfree money")],
    );
    let bytes = encode_document(&d).unwrap();
    assert_eq!(decode_document(&bytes).unwrap(), d);
    // At this version, a shape this build refuses is a decode error, not inert.
    let mut bad = d.clone();
    bad.extra.clear();
    let mut bytes = encode_document(&bad).unwrap();
    bytes.truncate(bytes.len() - 1);
    assert!(decode_document(&bytes).is_err());
}

#[test]
fn a_prompt_rule_is_inert_by_itself_and_the_rest_applies() {
    // nest-rules.md decision 1: a device with no runtime applies every other rule
    // and names the prompt rule as not applied on this device.
    let d = doc(
        vec![
            rule("nsfw", 800, ContentVerdict::Block),
            rule("nest:example.org/ask", 500, ContentVerdict::Collapse),
            rule("spam", 400, ContentVerdict::Collapse),
        ],
        vec![scorer(
            "ask",
            ScorerKind::Prompt,
            "Is this a scam?".as_bytes(),
        )],
    );
    assert_eq!(d.status(&nest()), PolicyStatus::Applied);
    assert_eq!(d.rule_status(&nest(), 0), RuleStatus::Applied);
    assert_eq!(d.rule_status(&nest(), 1), RuleStatus::InertNoRuntime);
    let assembled = assemble_rules(&d, &nest());
    assert_eq!(assembled.inert_rules, vec![1]);
    let factors: Vec<_> = assembled
        .rules
        .iter()
        .map(|r| r.rule.category.as_str())
        .collect();
    assert_eq!(factors, ["nsfw", "spam"]);
}

#[test]
fn a_keywords_scorer_is_validated_as_text_with_terms() {
    let with = |bytes: &[u8]| {
        doc(
            vec![rule("nest:example.org/w", 1000, ContentVerdict::Block)],
            vec![scorer("w", ScorerKind::Keywords, bytes)],
        )
        .status(&nest())
    };
    assert_eq!(with(b"casino\n  \nbet now"), PolicyStatus::Applied);
    assert_eq!(
        with(&[0xff, 0xfe]),
        PolicyStatus::Malformed(PolicyDefect::ScorerNotText {
            index: 0,
            name: "w".into()
        })
    );
    assert_eq!(
        with(b"  \n \n"),
        PolicyStatus::Malformed(PolicyDefect::KeywordsWithoutTerm {
            index: 0,
            name: "w".into()
        })
    );
    let long = "x".repeat(MAX_KEYWORD_TERM_CHARS + 1);
    assert_eq!(
        with(long.as_bytes()),
        PolicyStatus::Malformed(PolicyDefect::KeywordTermTooLong {
            index: 0,
            name: "w".into(),
            chars: MAX_KEYWORD_TERM_CHARS + 1
        })
    );
}

#[test]
fn the_keyword_scorer_agrees_with_the_muted_keyword_matcher() {
    // nest-rules.md § The keyword rule: the one shared deterministic matcher, so
    // a published keyword rule and a member's muted word agree on "contains".
    let terms = ["Casino", "free money", "  "];
    let d = doc(
        vec![rule("nest:example.org/w", 1000, ContentVerdict::Block)],
        vec![scorer(
            "w",
            ScorerKind::Keywords,
            terms.join("\n").as_bytes(),
        )],
    );
    let as_strings: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
    for text in [
        "Visit the CASINO tonight",
        "get FREE MONEY",
        "free  money",
        "nothing here",
        "",
        "Ça sino",
    ] {
        let matched = !evaluate_keyword_scorers(&d, &nest(), text).is_empty();
        assert_eq!(
            matched,
            crate::keyword::body_excludes_matches(&as_strings, text),
            "{text:?}"
        );
    }
    assert_eq!(
        evaluate_keyword_scorers(&d, &nest(), "casino"),
        vec![ContentLabelEntry {
            category: "nest:example.org/w".into(),
            confidence_per_mille: KEYWORD_MATCH_PERMILLE,
        }]
    );
}

#[test]
fn a_keyword_scorer_no_rule_names_and_a_malformed_document_produce_nothing() {
    let d = doc(
        vec![rule("nsfw", 800, ContentVerdict::Block)],
        vec![scorer("w", ScorerKind::Keywords, b"casino")],
    );
    assert!(evaluate_keyword_scorers(&d, &nest(), "casino").is_empty());
    let mut bad = doc(
        vec![rule("nest:example.org/w", 1000, ContentVerdict::Block)],
        vec![scorer("w", ScorerKind::Keywords, b"casino")],
    );
    bad.rules[0].reason = BTreeMap::new();
    assert!(evaluate_keyword_scorers(&bad, &nest(), "casino").is_empty());
}

#[test]
fn a_rule_naming_an_unbundled_scorer_of_its_publisher_is_malformed() {
    let d = doc(
        vec![rule("guardian:words", 1000, ContentVerdict::Block)],
        Vec::new(),
    );
    assert_eq!(
        d.status(&Publisher::Guardian),
        PolicyStatus::Malformed(PolicyDefect::UnknownScorerFactor {
            index: 0,
            factor: "guardian:words".into()
        })
    );
    // Another publisher's namespace is just a category to this document — it is
    // not this publisher's scorer, and no label will carry it.
    assert_eq!(d.status(&nest()), PolicyStatus::Applied);
}

#[test]
fn keyword_terms_round_trip_through_the_scorer_bytes() {
    let terms = vec!["one".to_string(), "two words".to_string()];
    let bytes = join_keyword_terms(&terms);
    assert_eq!(
        split_keyword_terms(std::str::from_utf8(&bytes).unwrap()),
        terms
    );
}

#[test]
fn a_signed_nest_policy_verifies_and_refuses_a_tampered_byte() {
    let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let public = key.verifying_key().to_bytes();
    let d = doc(vec![rule("nsfw", 800, ContentVerdict::Block)], Vec::new());
    let bytes = encode_document(&d).unwrap();
    let sig = sign_nest_policy(&key, &bytes, 3, 1_700_000_000).unwrap();

    assert_eq!(
        verify_nest_policy(&public, &bytes, 3, 1_700_000_000, &sig).unwrap(),
        d
    );
    // Every signed field is covered: the document, the sequence, the stamp.
    let mut tampered = bytes.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    assert_eq!(
        verify_nest_policy(&public, &tampered, 3, 1_700_000_000, &sig),
        Err(NestPolicyRejection::SignatureFailed)
    );
    assert_eq!(
        verify_nest_policy(&public, &bytes, 4, 1_700_000_000, &sig),
        Err(NestPolicyRejection::SignatureFailed)
    );
    assert_eq!(
        verify_nest_policy(&public, &bytes, 3, 1_700_000_001, &sig),
        Err(NestPolicyRejection::SignatureFailed)
    );
    let other = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32])
        .verifying_key()
        .to_bytes();
    assert_eq!(
        verify_nest_policy(&other, &bytes, 3, 1_700_000_000, &sig),
        Err(NestPolicyRejection::SignatureFailed)
    );
    // The signing input opens with the domain separator.
    assert!(
        nest_policy_signing_input(&bytes, 3, 1_700_000_000)
            .unwrap()
            .starts_with(NEST_POLICY_SIGNING_DOMAIN)
    );
}
