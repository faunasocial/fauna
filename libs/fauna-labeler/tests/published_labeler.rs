//! The shared execution boundary for a **published** `wasm` labeler —
//! [`fauna_labeler::run_published_labeler`], the one path every position that
//! runs one goes through (the capability-holder content-processor through the
//! FFI, and a community room's home nest as that room's capability-holder).
//!
//! Driven over the committed tier_3 fixture, so the path is exercised against
//! the same bytes the drain e2e publishes.

use fauna_core::data::Timestamp;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::scoring::{
    AlgorithmLabeler, LabelSource, LabelerInput, LabelerOutput, LabelerPostInput, ScorerLimits,
    sign_labeler_metadata,
};
use fauna_labeler::{
    LABELER_FEED_MAX_CPU_MICROSECONDS, LabelerAbiNewer, PreparedLabeler, run_published_labeler,
};

const CAT_LABELER_WAT: &str =
    include_str!("../../../tests/e2e-unified/fixtures/labeler/cat_labeler.wat");

fn signed_metadata(publisher: &ActorKeypair, wasm: &[u8]) -> Vec<u8> {
    signed_metadata_at(publisher, wasm, LabelerOutput::default())
}

fn signed_metadata_at(
    publisher: &ActorKeypair,
    wasm: &[u8],
    output_schema: LabelerOutput,
) -> Vec<u8> {
    let meta = AlgorithmLabeler {
        algorithm_id: publisher.actor_id(),
        version: 1,
        wasm_hash: fauna_core::encoding::content_hash(wasm),
        wasm_size: wasm.len() as u64,
        input_schema: LabelerInput {
            needs_text: true,
            needs_hashtags: false,
            needs_media_metadata: false,
            needs_author: false,
            needs_attachment_bytes: false,
        },
        output_schema,
        resource_limits: ScorerLimits {
            max_memory_bytes: 16 * 1024 * 1024,
            max_cpu_microseconds: 100_000,
        },
        updated_at: Timestamp(1),
        signature: Vec::new(),
    };
    let signed = sign_labeler_metadata(publisher.signing_key(), meta).unwrap();
    fauna_core::encoding::canonical_encode(&signed).unwrap()
}

fn text_input(text: &str) -> LabelerPostInput {
    LabelerPostInput {
        text: Some(text.to_string()),
        hashtags: Vec::new(),
        has_media: false,
        media_type: None,
        duration_ms: None,
        author: ActorId([7u8; 32]),
    }
}

#[test]
fn a_published_labeler_runs_over_a_typed_input_and_returns_its_labels() {
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm);

    let hit = run_published_labeler(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &text_input("my cat is asleep"),
    )
    .unwrap();
    assert_eq!(hit.len(), 1, "the fixture emits exactly one label on a hit");
    assert_eq!(hit[0].category, "cat");
    assert!((hit[0].confidence - 0.9).abs() < 1e-9);
    assert_eq!(hit[0].source, LabelSource::TextAnalysis);

    let miss = run_published_labeler(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &text_input("the weather is nice"),
    )
    .unwrap();
    assert!(
        miss.is_empty(),
        "an empty output is 'detected nothing', not an error"
    );
}

#[test]
fn a_module_that_is_not_the_one_its_publisher_signed_never_runs() {
    // The pre-instantiation re-verify (security review B1) is part of the one
    // boundary, so no caller can run a module the registry swapped.
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm);
    let mut swapped = wasm.to_vec();
    let last = swapped.len() - 2;
    swapped[last] ^= 0x01;

    let err = run_published_labeler(
        &metadata,
        &swapped,
        &publisher.actor_id(),
        &text_input("cat"),
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("verify"),
        "expected the verify refusal, got: {err:#}"
    );
}

#[test]
fn a_module_declaring_a_newer_label_revision_is_refused_as_newer_before_it_is_compiled() {
    // The output stamp is read after the signature verify and before the
    // module is compiled — so before its positional BARE output could be
    // decoded. Over bytes that are not wasm at all, a refusal that is neither
    // a compile error nor a decode error proves the check runs before both.
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let not_wasm = b"not a wasm module".as_slice();
    let metadata = signed_metadata_at(&publisher, not_wasm, LabelerOutput { label_abi: 2 });

    let err = run_published_labeler(
        &metadata,
        not_wasm,
        &publisher.actor_id(),
        &text_input("cat"),
    )
    .unwrap_err();
    assert_eq!(
        err.downcast_ref::<LabelerAbiNewer>(),
        Some(&LabelerAbiNewer {
            declared: 2,
            supported: fauna_core::scoring::LABEL_ABI_CURRENT,
        }),
        "expected the typed newer-labeler refusal, got: {err:#}"
    );
}

#[test]
fn a_module_declaring_revision_one_explicitly_still_labels() {
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let metadata = signed_metadata_at(&publisher, wasm, LabelerOutput { label_abi: 1 });

    let hit = run_published_labeler(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &text_input("my cat is asleep"),
    )
    .unwrap();
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].category, "cat");
}

/// The cat fixture signed with a declared CPU budget of `cpu_microseconds`.
fn signed_metadata_with_cpu(
    publisher: &ActorKeypair,
    wasm: &[u8],
    cpu_microseconds: u64,
) -> Vec<u8> {
    let mut meta: AlgorithmLabeler =
        fauna_core::encoding::canonical_decode(&signed_metadata(publisher, wasm)).unwrap();
    meta.resource_limits.max_cpu_microseconds = cpu_microseconds;
    meta.signature = Vec::new();
    let signed = sign_labeler_metadata(publisher.signing_key(), meta).unwrap();
    fauna_core::encoding::canonical_encode(&signed).unwrap()
}

#[test]
fn a_prepared_labeler_is_verified_once_and_runs_over_many_items() {
    // The prepare/run split (`content-moderation-and-ranking.md` § Tier-3 →
    // *Where it runs*, invariant (a)): every guard runs in `prepare`, and `run`
    // takes neither the metadata nor the module bytes — so a position looping
    // one labeler over many items cannot skip a guard, and pays for none twice.
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm);
    let prepared = PreparedLabeler::prepare(&metadata, wasm, &publisher.actor_id()).unwrap();
    assert_eq!(prepared.metadata().version, 1);

    let encode = |text: &str| fauna_labeler::encode_labeler_input(&text_input(text)).unwrap();
    let hit = prepared.run(&encode("my cat is asleep"), &[]).unwrap();
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].category, "cat");
    let miss = prepared.run(&encode("the weather is nice"), &[]).unwrap();
    assert!(miss.is_empty());
    let again = prepared.run(&encode("another cat"), &[]).unwrap();
    assert_eq!(
        again.len(),
        1,
        "the compiled module answers every run alike"
    );
    assert_eq!(again[0].category, "cat");
}

#[test]
fn prepare_refuses_what_the_one_boundary_refuses() {
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm);
    let other = ActorKeypair::from_secret([4u8; 32]);
    let err = PreparedLabeler::prepare(&metadata, wasm, &other.actor_id())
        .err()
        .expect("a module prepared under another labeler's id is refused");
    assert!(format!("{err:#}").contains("verify"), "got: {err:#}");

    let newer = signed_metadata_at(&publisher, wasm, LabelerOutput { label_abi: 2 });
    let err = PreparedLabeler::prepare_for_feed(&newer, wasm, &publisher.actor_id())
        .err()
        .expect("a newer label revision is refused at prepare");
    assert!(
        err.downcast_ref::<LabelerAbiNewer>().is_some(),
        "got: {err:#}"
    );
}

#[test]
fn the_feed_position_clamps_the_cpu_budget_to_its_own_ceiling() {
    // Invariant (d): an interactive position bounds a module tighter than a
    // background holder does; a publisher declaring less keeps its lower limit.
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let generous = signed_metadata_with_cpu(&publisher, wasm, 900_000);

    let feed = PreparedLabeler::prepare_for_feed(&generous, wasm, &publisher.actor_id()).unwrap();
    assert_eq!(
        feed.cpu_limit_microseconds(),
        LABELER_FEED_MAX_CPU_MICROSECONDS
    );
    let holder = PreparedLabeler::prepare(&generous, wasm, &publisher.actor_id()).unwrap();
    assert_eq!(holder.cpu_limit_microseconds(), 900_000);

    let modest = signed_metadata_with_cpu(&publisher, wasm, 20_000);
    let feed = PreparedLabeler::prepare_for_feed(&modest, wasm, &publisher.actor_id()).unwrap();
    assert_eq!(feed.cpu_limit_microseconds(), 20_000);
}
