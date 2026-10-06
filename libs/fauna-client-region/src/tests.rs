//! The plane's tier_1 set (`region-blocking.md` § What the build owes in tests),
//! over a synthetic registry and synthetic documents — never a real region.

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{RenderVerdict, render_verdict_composed};
use fauna_core::region_authority::{
    AuthorityKey, PAYLOAD_KIND_CONTENT_POLICY, PolicyArtifact, RegionCode, RegionEntry,
    RegionRegistry, sign_artifact,
};
use fauna_core::region_policy::{
    BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, GRAMMAR_VERSION,
    REASON_DEFAULT_KEY, ScorerKind, scorer_factor,
};
use fauna_core::scoring::build_list_artifact;
use fauna_protocol::region::RegionArtifactGetReply;

use crate::{DeclaredRegion, PolicyState, RegionPlane, RegionSource, fetch_chain};

const NOW: u64 = 1_800_000_000;
const REASON: &str = "Withheld under the Synthetic Act, section 7.";

fn code(s: &str) -> RegionCode {
    RegionCode::parse(s).unwrap()
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn entry(region: &str, authority: &str, seed: u8, parent: Option<&str>) -> RegionEntry {
    RegionEntry {
        region: code(region),
        authority_name: authority.into(),
        official_domain: "authority.example".into(),
        parent: parent.map(code),
        keys: vec![AuthorityKey {
            key_id: "k1".into(),
            public_key: key(seed).verifying_key().to_bytes().to_vec(),
            enrolled_at: 0,
            retired_at: None,
        }],
    }
}

fn registry() -> RegionRegistry {
    RegionRegistry {
        version: 1,
        regions: vec![
            entry("XZ", "Synthetic Authority", 42, None),
            entry("XZ-AB", "Synthetic Province", 43, Some("XZ")),
        ],
    }
}

fn declared(region: &str) -> Option<DeclaredRegion> {
    Some(DeclaredRegion {
        code: code(region),
        source: RegionSource::SystemLocale,
    })
}

fn rule(factor: &str, verdict: ContentVerdict) -> ContentRule {
    ContentRule {
        factor: factor.into(),
        min_permille: 500,
        verdict,
        reason_code: "SA-7".into(),
        reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), REASON.to_string())]),
        extra: Default::default(),
    }
}

fn document(rules: Vec<ContentRule>, scorers: Vec<BundledScorer>) -> ContentPolicyDocument {
    ContentPolicyDocument {
        version: GRAMMAR_VERSION,
        rules,
        scorers,
        extra: Default::default(),
    }
}

fn envelope_with_payload(
    region: &str,
    seed: u8,
    sequence: u64,
    payload: Vec<u8>,
) -> PolicyArtifact {
    sign_artifact(
        PolicyArtifact {
            region: code(region),
            key_id: "k1".into(),
            sequence,
            issued_at: NOW - 60,
            payload_kind: PAYLOAD_KIND_CONTENT_POLICY.to_string(),
            payload,
            sig: Vec::new(),
        },
        &key(seed),
    )
    .unwrap()
}

fn envelope(region: &str, seed: u8, sequence: u64, doc: &ContentPolicyDocument) -> PolicyArtifact {
    envelope_with_payload(
        region,
        seed,
        sequence,
        fauna_protocol::encode_canonical(doc).unwrap().to_vec(),
    )
}

fn reply(envelope: Option<PolicyArtifact>) -> RegionArtifactGetReply {
    RegionArtifactGetReply {
        envelope,
        last_checked_at: Some(NOW - 10),
        ..Default::default()
    }
}

fn nsfw_block() -> ContentPolicyDocument {
    document(vec![rule("nsfw", ContentVerdict::Block)], Vec::new())
}

fn nsfw(permille: u16) -> Vec<ContentLabelEntry> {
    vec![ContentLabelEntry {
        category: "nsfw".into(),
        confidence_per_mille: permille,
    }]
}

fn verdict(plane: &RegionPlane, labels: &[ContentLabelEntry]) -> RenderVerdict {
    render_verdict_composed(labels, None, None, &plane.rule_sets()).verdict
}

#[test]
fn a_fresh_device_holds_nothing_and_renders_unblocked() {
    let plane = RegionPlane::new(declared("XZ"), registry());
    assert!(plane.rule_sets().is_empty());
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Badge);
}

#[test]
fn a_relayed_document_binds_and_names_its_authority_and_reason() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    assert!(plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW
    ));
    let composed = render_verdict_composed(&nsfw(900), None, None, &plane.rule_sets());
    assert_eq!(composed.verdict, RenderVerdict::Block);
    let attribution = composed.region().expect("attributed to the region");
    assert_eq!(attribution.authority_name, "Synthetic Authority");
    assert_eq!(attribution.reason.get(REASON_DEFAULT_KEY).unwrap(), REASON);

    let view = plane.view(NOW);
    assert_eq!(view.policies.len(), 1);
    assert_eq!(view.policies[0].authority_name, "Synthetic Authority");
    assert_eq!(view.policies[0].sequence, 1);
    assert_eq!(view.policies[0].state, PolicyState::Applied);
    assert_eq!(view.last_checked_at, Some(NOW - 10));
    assert!(!view.stale);
}

#[test]
fn the_snapshot_restores_ahead_of_the_first_fetch() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    let bytes = plane.to_bytes();

    let mut relaunched = RegionPlane::new(declared("XZ"), registry());
    relaunched.load(Some(&bytes), NOW + 60);
    assert_eq!(verdict(&relaunched, &nsfw(900)), RenderVerdict::Block);
}

#[test]
fn no_document_and_a_refused_envelope_keep_the_last_known_good() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 2, &nsfw_block()))),
        NOW,
    );

    // The relay lost its cache: no document. The device keeps its block.
    plane.apply_reply(&code("XZ"), reply(None), NOW);
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);

    // A replayed older document that would relax the device is refused.
    let relaxed = document(Vec::new(), Vec::new());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &relaxed))),
        NOW,
    );
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);

    // A forged document (the wrong key) is refused.
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 99, 3, &relaxed))),
        NOW,
    );
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);

    // A newer genuine document replaces it.
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 3, &relaxed))),
        NOW,
    );
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Badge);
}

#[test]
fn the_replay_floor_survives_a_relaunch() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 5, &nsfw_block()))),
        NOW,
    );
    let bytes = plane.to_bytes();
    let mut relaunched = RegionPlane::new(declared("XZ"), registry());
    relaunched.load(Some(&bytes), NOW);
    let relaxed = document(Vec::new(), Vec::new());
    relaunched.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 4, &relaxed))),
        NOW,
    );
    assert_eq!(verdict(&relaunched, &nsfw(900)), RenderVerdict::Block);
}

#[test]
fn a_de_listed_authority_stops_binding_at_load() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    let bytes = plane.to_bytes();
    let mut relaunched = RegionPlane::new(declared("XZ"), RegionRegistry::default());
    relaunched.load(Some(&bytes), NOW);
    assert!(relaunched.rule_sets().is_empty());
    assert_eq!(verdict(&relaunched, &nsfw(900)), RenderVerdict::Badge);
}

#[test]
fn an_unreadable_snapshot_is_no_information() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.load(Some(b"not cbor at all"), NOW);
    assert!(plane.rule_sets().is_empty());
}

/// An unreadable record — here one a newer build wrote in a shape this one
/// cannot decode — is never written over: the plane still folds what it
/// fetches, but [`RegionPlane::to_bytes`] answers nothing to persist, and the
/// shared file write leaves the newer build's bytes exactly as they were
/// (`transport.md` § Rule 3 in full → *The store around the enum*).
#[test]
fn an_unreadable_record_is_never_written_over() {
    // `{"floors": 1}` — the floors this build reads as a list, reshaped.
    let newer: &[u8] = b"\xa1\x66floors\x01";
    let dir = std::env::temp_dir().join(format!(
        "fauna-client-region-unreadable-{}",
        std::process::id()
    ));
    let path = crate::store::record_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, newer).unwrap();

    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.load(crate::store::read_record(&path).as_deref(), NOW);
    assert!(plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    ));
    assert_eq!(
        verdict(&plane, &nsfw(900)),
        RenderVerdict::Block,
        "the fetched document still binds"
    );
    assert!(
        plane.to_bytes().is_empty(),
        "nothing may be written over it"
    );
    crate::store::write_record(&path, &plane.to_bytes()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), newer);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The open arm of `transport.md` § Rule 3 in full on the record's declared
/// source: a newer build's record naming a source this build lacks still
/// loads; a pending plane enforces its region (never relaxes), the view names
/// no source it cannot name, and the record re-encodes byte for byte until the
/// leaf answers.
#[test]
fn a_recorded_source_this_build_lacks_is_enforced_and_carried() {
    let mut first = RegionPlane::new(declared("XZ"), registry());
    first.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    // The newer writer: the same record, its declaration from a source this
    // build has never heard of.
    let mut value: fauna_protocol::Value =
        fauna_protocol::decode_strict(&first.to_bytes()).unwrap();
    let fauna_protocol::Value::Map(record) = &mut value else {
        panic!("the record is a map")
    };
    let Some(fauna_protocol::Value::Map(declared_map)) = record.get_mut("declared") else {
        panic!("the record carries its declaration")
    };
    declared_map.insert(
        "source".into(),
        fauna_protocol::Value::String("carrier_network".into()),
    );
    let newer = fauna_protocol::encode_canonical(&value).unwrap().to_vec();

    let mut pending = RegionPlane::new_pending(registry());
    pending.load(Some(&newer), NOW);
    assert_eq!(
        verdict(&pending, &nsfw(900)),
        RenderVerdict::Block,
        "the recorded region keeps binding"
    );
    assert_eq!(pending.chain(), vec![code("XZ")]);
    assert_eq!(pending.view(NOW).declared, None, "no source it cannot name");
    assert_eq!(pending.to_bytes(), newer, "the record re-encodes unchanged");

    // The leaf's answer is a fresh fact and replaces the carried declaration.
    assert!(pending.redeclare(declared("XZ")));
    assert_eq!(pending.declared(), declared("XZ").as_ref());
    assert_ne!(pending.to_bytes(), newer);
}

#[test]
fn an_unimplemented_grammar_version_is_inert_and_says_so() {
    let mut newer = nsfw_block();
    newer.version = GRAMMAR_VERSION + 1;
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(&code("XZ"), reply(Some(envelope("XZ", 42, 1, &newer))), NOW);
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Badge);
    assert_eq!(
        plane.view(NOW).policies[0].state,
        PolicyState::Inert {
            version: GRAMMAR_VERSION + 1
        }
    );
}

#[test]
fn a_newer_grammar_this_build_cannot_even_decode_is_inert_not_malformed() {
    #[derive(serde::Serialize)]
    struct Future {
        version: u32,
        rules: String,
    }
    let payload = fauna_protocol::encode_canonical(&Future {
        version: GRAMMAR_VERSION + 1,
        rules: "a shape this build does not know".into(),
    })
    .unwrap()
    .to_vec();
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope_with_payload("XZ", 42, 1, payload))),
        NOW,
    );
    assert_eq!(
        plane.view(NOW).policies[0].state,
        PolicyState::Inert {
            version: GRAMMAR_VERSION + 1
        }
    );
    assert!(plane.rule_sets()[0].rules.is_empty());
}

#[test]
fn every_policy_on_the_chain_binds_most_specific_first() {
    let province_collapse = document(vec![rule("spam", ContentVerdict::Collapse)], Vec::new());
    let mut plane = RegionPlane::new(declared("XZ-AB"), registry());
    assert_eq!(plane.chain(), vec![code("XZ-AB"), code("XZ")]);
    plane.apply_reply(
        &code("XZ-AB"),
        reply(Some(envelope("XZ-AB", 43, 1, &province_collapse))),
        NOW,
    );
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);
    let spam = vec![ContentLabelEntry {
        category: "spam".into(),
        confidence_per_mille: 900,
    }];
    assert_eq!(verdict(&plane, &spam), RenderVerdict::Collapse);
    let view = plane.view(NOW);
    assert_eq!(view.policies[0].authority_name, "Synthetic Province");
    assert_eq!(view.policies[1].authority_name, "Synthetic Authority");
}

#[test]
fn an_undeclared_or_unenrolled_region_applies_nothing() {
    let mut plane = RegionPlane::new(None, registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    assert!(plane.rule_sets().is_empty());
    let plane = RegionPlane::new(declared("QQ"), registry());
    assert!(plane.chain().is_empty());
}

#[test]
fn an_envelope_for_another_region_is_refused() {
    let mut plane = RegionPlane::new(declared("XZ-AB"), registry());
    plane.apply_reply(
        &code("XZ-AB"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    assert!(plane.view(NOW).policies.is_empty());
}

/// Correction (1) of the pickup's judge: the bundled scorers run through the
/// one shared runner, `fauna_labeler::region`, and their factor joins the
/// item's labels before the fold.
#[test]
fn a_bundled_list_scorer_names_the_exact_item() {
    let target = [7u8; 32];
    let other = [8u8; 32];
    let list = build_list_artifact(None, vec![(target, 1000)]).unwrap();
    let region = code("XZ");
    let doc = document(
        vec![rule(
            &scorer_factor(&region, "listed"),
            ContentVerdict::Block,
        )],
        vec![BundledScorer {
            name: "listed".into(),
            kind: ScorerKind::List,
            bytes: list,
            extra: Default::default(),
        }],
    );
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(&region, reply(Some(envelope("XZ", 42, 1, &doc))), NOW);
    assert!(plane.has_scorers());
    let input = crate::scorer_input(fauna_core::identity::ActorId([1; 32]), "hello", &[], false);

    let labels = plane.labels_for(Some(&target), &input);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].category, "region:XZ/listed");
    assert_eq!(verdict(&plane, &labels), RenderVerdict::Block);

    assert!(plane.labels_for(Some(&other), &input).is_empty());
    assert!(plane.labels_for(None, &input).is_empty());
}

#[test]
fn staleness_warns_and_never_relaxes() {
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    let mut stale_reply = reply(Some(envelope("XZ", 42, 1, &nsfw_block())));
    stale_reply.stale = true;
    plane.apply_reply(&code("XZ"), stale_reply, NOW);
    assert!(plane.view(NOW).stale);
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);

    // The device itself has not reached its nest for longer than the bound.
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    plane.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    let later = NOW + fauna_core::region_authority::STALE_AFTER_SECS + 1;
    assert!(plane.view(later).stale);
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);
}

#[test]
fn the_fetch_asks_the_relay_for_each_region_on_the_chain() {
    use fauna_client_testkit::{ScriptedRequester, block_on};
    let answer = fauna_protocol::encode_canonical(&RegionArtifactGetReply::default())
        .unwrap()
        .to_vec();
    let nest = ScriptedRequester::new([answer.clone(), answer]);
    let replies = block_on(fetch_chain(&nest, &[code("XZ-AB"), code("XZ")]));
    assert_eq!(replies.len(), 2);
    assert!(replies.iter().all(|(_, r)| r.is_ok()));
    assert_eq!(nest.kinds(), vec!["fauna.region.artifact.get"; 2]);
    let asked: fauna_protocol::region::RegionArtifactGetRequest =
        fauna_protocol::decode_strict(&nest.payloads()[0]).unwrap();
    assert_eq!(asked.region, code("XZ-AB"));
    assert_eq!(asked.payload_kind, PAYLOAD_KIND_CONTENT_POLICY);
}

// ── The shell-facing folds every app shares (C6: lifted out of tui's glue so
//    the six other shells paint rather than re-derive) ─────────────────────

#[test]
fn apply_replies_folds_the_answers_and_drops_a_failed_ask() {
    let mut plane = RegionPlane::new(declared("XZ-AB"), registry());
    let changed = plane.apply_replies(
        vec![
            (code("XZ-AB"), Err("the relay did not answer".to_string())),
            (
                code("XZ"),
                Ok(reply(Some(envelope("XZ", 42, 1, &nsfw_block())))),
            ),
        ],
        NOW,
    );
    assert!(changed);
    assert_eq!(verdict(&plane, &nsfw(900)), RenderVerdict::Block);

    // Every ask failed: nothing is written, nothing to persist.
    let mut untouched = RegionPlane::new(declared("XZ"), registry());
    assert!(!untouched.apply_replies(vec![(code("XZ"), Err("down".into()))], NOW));
    assert!(untouched.view(NOW).last_checked_at.is_none());
}

#[test]
fn a_refresh_is_due_first_and_then_on_the_shared_cadence() {
    use fauna_core::region_authority::REFRESH_INTERVAL_SECS;
    assert!(crate::refresh_due(None, NOW));
    assert!(!crate::refresh_due(Some(NOW), NOW + 1));
    assert!(crate::refresh_due(Some(NOW), NOW + REFRESH_INTERVAL_SECS));
}

#[test]
fn join_labels_borrows_when_no_scorer_is_in_force() {
    let plane = RegionPlane::new(declared("XZ"), registry());
    let labels = nsfw(900);
    let joined = plane.join_labels(Some(&[7u8; 32]), &labels, || {
        panic!("the scorer input is never built when no scorer is in force")
    });
    assert!(matches!(joined, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn the_placeholder_names_the_verb_the_authority_and_the_reason_in_the_app_language() {
    use crate::render::{RegionVerb, placeholder_for};
    let mut plane = RegionPlane::new(declared("XZ"), registry());
    let mut doc = nsfw_block();
    doc.rules[0]
        .reason
        .insert("en".to_string(), "In English.".to_string());
    plane.apply_reply(&code("XZ"), reply(Some(envelope("XZ", 42, 1, &doc))), NOW);

    let composed = render_verdict_composed(&nsfw(900), None, None, &plane.rule_sets());
    let p = placeholder_for(&composed, "en").expect("the region drove a block");
    assert_eq!(p.verb, RegionVerb::Block);
    assert_eq!(p.region.as_str(), "XZ");
    assert_eq!(p.authority_name, "Synthetic Authority");
    assert_eq!(p.reason, "In English.");
    // A language the authority did not write falls back to its default text.
    assert_eq!(placeholder_for(&composed, "nb").unwrap().reason, REASON);

    // A verdict no region drove paints no region placeholder.
    let unregioned = render_verdict_composed(&nsfw(900), None, None, &[]);
    assert!(placeholder_for(&unregioned, "en").is_none());
}

#[test]
fn a_bcp47_tag_declares_its_region_subtag_and_nothing_else() {
    use crate::source::declared_from_bcp47;
    let d = declared_from_bcp47("nb-NO").unwrap();
    assert_eq!(d.code, code("NO"));
    assert_eq!(d.source, RegionSource::BrowserLocale);
    // A script subtag is skipped; the region follows it.
    assert_eq!(declared_from_bcp47("zh-Hant-TW").unwrap().code, code("TW"));
    // BCP 47 subtags are case-insensitive: the canonical form is upper case.
    assert_eq!(declared_from_bcp47("en-us").unwrap().code, code("US"));
    // A numeric UN M.49 area is a region subtag too; whether anyone
    // administers it is the registry's question, not the parser's.
    assert_eq!(declared_from_bcp47("es-419").unwrap().code, code("419"));
    // No region subtag — never inferred from the language ("en" is not "US").
    for tag in ["en", "", "en-x-private", "de-1996", "zh-Hant", "sl-rozaj"] {
        assert_eq!(declared_from_bcp47(tag), None, "{tag:?}");
    }
}

#[test]
fn a_native_os_region_code_declares_with_its_source() {
    let d = DeclaredRegion::from_os_code("NO", RegionSource::SystemRegion).unwrap();
    assert_eq!(d.code, code("NO"));
    assert_eq!(d.source, RegionSource::SystemRegion);
    // The OS reports no region, or one that is not a region code.
    for raw in ["", "no", "N O"] {
        assert_eq!(
            DeclaredRegion::from_os_code(raw, RegionSource::SystemRegion),
            None,
            "{raw:?}"
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_device_record_file_round_trips_and_replaces_atomically() {
    let dir =
        std::env::temp_dir().join(format!("fauna-region-store-{}-{}", std::process::id(), NOW));
    let path = crate::store::record_path(&dir);
    assert!(path.ends_with("region/content-policy.cbor"));
    assert_eq!(crate::store::read_record(&path), None);
    crate::store::write_record(&path, b"first").unwrap();
    crate::store::write_record(&path, b"second").unwrap();
    assert_eq!(
        crate::store::read_record(&path).as_deref(),
        Some(&b"second"[..])
    );
    assert!(!path.with_extension("cbor.tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// An asynchronous leaf (a store build's storefront) has not answered at
/// launch: the pending plane stands on the declaration the record carries, so a
/// held document keeps binding from the first paint (§ Fail posture —
/// enforcement never moves on no information), and the leaf's answer replaces
/// it once it arrives.
#[test]
fn a_pending_plane_stands_on_the_recorded_declaration_until_the_leaf_answers() {
    let mut first = RegionPlane::new(declared("XZ"), registry());
    first.apply_reply(
        &code("XZ"),
        reply(Some(envelope("XZ", 42, 1, &nsfw_block()))),
        NOW,
    );
    let record = first.to_bytes();

    let mut pending = RegionPlane::new_pending(registry());
    pending.load(Some(&record), NOW);
    assert_eq!(pending.declared(), declared("XZ").as_ref());
    assert_eq!(verdict(&pending, &nsfw(900)), RenderVerdict::Block);

    // The leaf answers with the same region: nothing moves.
    assert!(!pending.redeclare(declared("XZ")));
    assert_eq!(verdict(&pending, &nsfw(900)), RenderVerdict::Block);

    // The leaf answers with another region: the held XZ document stops binding.
    let storefront = DeclaredRegion::from_storefront_alpha3(Some("NOR"));
    assert!(pending.redeclare(storefront.clone()));
    assert_eq!(pending.declared(), storefront.as_ref());
    assert_eq!(verdict(&pending, &nsfw(900)), RenderVerdict::Badge);
}

/// A device with no record declares nothing until its asynchronous leaf
/// answers — the fresh-subject residual — and a plane that is NOT pending
/// never takes the recorded declaration over its own leaf's.
#[test]
fn only_a_pending_plane_takes_the_recorded_declaration() {
    let mut fresh = RegionPlane::new_pending(registry());
    fresh.load(None, NOW);
    assert_eq!(fresh.declared(), None);

    let record = RegionPlane::new(declared("XZ"), registry()).to_bytes();
    let mut synchronous = RegionPlane::new(declared("XZ-AB"), registry());
    synchronous.load(Some(&record), NOW);
    assert_eq!(synchronous.declared(), declared("XZ-AB").as_ref());

    let mut undeclared = RegionPlane::new(None, registry());
    undeclared.load(Some(&record), NOW);
    assert_eq!(
        undeclared.declared(),
        None,
        "a synchronous leaf that declares nothing stays so"
    );
}
