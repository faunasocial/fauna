//! The attachment facet of the `label()` ABI — the shape a module that declared
//! `needs_attachment_bytes` reads, the encoder every position shares, and the
//! two ceilings (`content-moderation-and-ranking.md` § Tier-3 → *The attachment
//! facet*).

use fauna_core::data::Timestamp;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::scoring::{
    AlgorithmLabeler, LabelSource, LabelerAttachmentInput, LabelerInput,
    LabelerInputWithAttachments, LabelerOutput, LabelerPostInput, ScorerLimits,
    sign_labeler_metadata,
};
use fauna_labeler::{
    AttachmentFacetBudget, LABELER_ATTACHMENT_BYTES_MAX, LABELER_ATTACHMENT_FACET_MAX_BYTES,
    encode_attachment_facet, encode_labeler_input, encode_labeler_input_for, run_published_labeler,
    run_published_labeler_bare, run_published_labeler_with_attachments,
};

const MEOW_BYTES_LABELER_WAT: &str =
    include_str!("../../../tests/e2e-unified/fixtures/labeler/meow_bytes_labeler.wat");
const CAT_LABELER_WAT: &str =
    include_str!("../../../tests/e2e-unified/fixtures/labeler/cat_labeler.wat");

fn schema(needs_attachment_bytes: bool) -> LabelerInput {
    LabelerInput {
        needs_text: true,
        needs_hashtags: false,
        needs_media_metadata: true,
        needs_author: false,
        needs_attachment_bytes,
    }
}

fn signed_metadata(publisher: &ActorKeypair, wasm: &[u8], needs_attachment_bytes: bool) -> Vec<u8> {
    let meta = AlgorithmLabeler {
        algorithm_id: publisher.actor_id(),
        version: 1,
        wasm_hash: fauna_core::encoding::content_hash(wasm),
        wasm_size: wasm.len() as u64,
        input_schema: schema(needs_attachment_bytes),
        output_schema: LabelerOutput::default(),
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

/// An attachment-only item: no text, one image whose bytes carry the marker.
fn attachment_only_input() -> LabelerPostInput {
    LabelerPostInput {
        text: None,
        hashtags: Vec::new(),
        has_media: true,
        media_type: Some("image/png".into()),
        duration_ms: None,
        author: ActorId([7u8; 32]),
    }
}

fn png_with(marker: &[u8]) -> LabelerAttachmentInput {
    let mut bytes = b"\x89PNG\r\n\x1a\n....".to_vec();
    bytes.extend_from_slice(marker);
    bytes.extend_from_slice(b"....IEND");
    LabelerAttachmentInput {
        mime_type: "image/png".into(),
        size_bytes: bytes.len() as u64,
        bytes,
    }
}

#[test]
fn a_flagged_input_is_the_post_input_followed_by_the_facet() {
    // The ABI's one structural claim: BARE encodes a struct as its fields in
    // order, unframed, so `LabelerInputWithAttachments` IS the v1 bytes
    // followed by the facet — what lets a holder that already holds v1 bytes
    // append the facet (`run_published_labeler_bare`) and a module author
    // decode v1 and then the facet.
    let post = attachment_only_input();
    let attachments = vec![png_with(b"meow"), png_with(b"purr")];
    let whole = serde_bare::to_vec(&LabelerInputWithAttachments {
        post: post.clone(),
        attachments: attachments.clone(),
    })
    .unwrap();
    let mut concatenated = encode_labeler_input(&post).unwrap();
    concatenated.extend(encode_attachment_facet(&attachments).unwrap());
    assert_eq!(whole, concatenated);
    assert_eq!(
        encode_labeler_input_for(&schema(true), &post, &attachments).unwrap(),
        whole
    );
}

#[test]
fn a_module_that_did_not_declare_bytes_reads_the_v1_bytes_exactly_whatever_the_host_holds() {
    // Every module published before the flag keeps working unchanged: the
    // host may hold attachment bytes, and hands that module none of them.
    let post = attachment_only_input();
    let attachments = vec![png_with(b"meow")];
    assert_eq!(
        encode_labeler_input_for(&schema(false), &post, &attachments).unwrap(),
        encode_labeler_input(&post).unwrap()
    );
}

#[test]
fn a_flagged_module_at_a_position_with_no_bytes_reads_its_shape_with_an_empty_facet() {
    // The mail holder and the post positions hold no attachment bytes; a
    // flagged module there still reads the shape it declared — never a
    // silent v1 it would mis-decode.
    let post = attachment_only_input();
    let mut expected = encode_labeler_input(&post).unwrap();
    expected.extend(encode_attachment_facet(&[]).unwrap());
    assert_eq!(
        encode_labeler_input_for(&schema(true), &post, &[]).unwrap(),
        expected
    );
    assert_eq!(
        encode_attachment_facet(&[]).unwrap(),
        vec![0u8],
        "an empty facet is BARE's one-byte zero length"
    );
}

#[test]
fn the_meow_bytes_fixture_speaks_the_label_abi() {
    // The fixture's hand-written BARE bytes are a claim; this is the check.
    let expected = serde_bare::to_vec(&vec![fauna_core::scoring::Label {
        category: "nsfw".into(),
        confidence: 0.7,
        source: LabelSource::VisionModel,
    }])
    .unwrap();
    assert_eq!(expected.len(), 15);
    assert_eq!(
        expected,
        vec![
            0x01, 0x04, b'n', b's', b'f', b'w', 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0xe6, 0x3f,
            0x02
        ]
    );
}

#[test]
fn a_module_that_declared_bytes_labels_an_attachment_only_item_by_them() {
    let publisher = ActorKeypair::from_secret([3u8; 32]);
    let wasm = MEOW_BYTES_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm, true);
    let post = attachment_only_input();

    // The marker is in the bytes and nowhere else: a hit is a hit on the bytes.
    let hit = run_published_labeler_with_attachments(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &post,
        &[png_with(b"meow")],
    )
    .unwrap();
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].category, "nsfw");
    assert!((hit[0].confidence - 0.7).abs() < 1e-9);
    assert_eq!(hit[0].source, LabelSource::VisionModel);

    // The same item with the marker absent from the bytes labels nothing …
    let miss = run_published_labeler_with_attachments(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &post,
        &[png_with(b"purr")],
    )
    .unwrap();
    assert!(miss.is_empty());
    // … and so does the v1-only run: with no facet the module cannot see any
    // bytes, which is exactly what the flag exists to change.
    assert!(
        run_published_labeler(&metadata, wasm, &publisher.actor_id(), &post)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_holder_with_v1_bytes_in_hand_still_gives_a_flagged_module_its_facet() {
    // The FFI path: the holder encoded the v1 input on its side of the
    // boundary; the shared boundary appends the facet it holds (here, some)
    // because the metadata says the module reads it.
    let publisher = ActorKeypair::from_secret([4u8; 32]);
    let wasm = MEOW_BYTES_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm, true);
    let v1 = encode_labeler_input(&attachment_only_input()).unwrap();
    let hit = run_published_labeler_bare(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &v1,
        &[png_with(b"meow")],
    )
    .unwrap();
    assert_eq!(
        hit.len(),
        1,
        "the facet reached the module through the bare path"
    );
    let none =
        run_published_labeler_bare(&metadata, wasm, &publisher.actor_id(), &v1, &[]).unwrap();
    assert!(
        none.is_empty(),
        "an empty facet is still the declared shape"
    );
}

#[test]
fn an_unflagged_module_never_sees_the_bytes_a_position_holds() {
    // The cat fixture scans its whole input for "cat"; if the host handed it
    // the facet regardless of its declaration, an attachment carrying "cat"
    // would light it up. It must not: a module reads only what it declared.
    let publisher = ActorKeypair::from_secret([5u8; 32]);
    let wasm = CAT_LABELER_WAT.as_bytes();
    let metadata = signed_metadata(&publisher, wasm, false);
    let post = attachment_only_input();
    let out = run_published_labeler_with_attachments(
        &metadata,
        wasm,
        &publisher.actor_id(),
        &post,
        &[png_with(b"cat")],
    )
    .unwrap();
    assert!(
        out.is_empty(),
        "an unflagged module was handed bytes it never declared"
    );
}

#[test]
fn the_facet_budget_admits_by_opened_length_under_both_ceilings() {
    let mut budget = AttachmentFacetBudget::new();
    assert!(
        !budget.admit(LABELER_ATTACHMENT_BYTES_MAX + 1),
        "over the per-attachment ceiling"
    );
    assert_eq!(budget.handed_over(), 0, "a refusal spends nothing");
    assert!(
        budget.admit(LABELER_ATTACHMENT_BYTES_MAX),
        "exactly the ceiling fits"
    );
    // Fill to the total ceiling in maximal attachments, then one byte over.
    let mut admitted = 1;
    while budget.handed_over() + LABELER_ATTACHMENT_BYTES_MAX <= LABELER_ATTACHMENT_FACET_MAX_BYTES
    {
        assert!(budget.admit(LABELER_ATTACHMENT_BYTES_MAX));
        admitted += 1;
    }
    assert_eq!(
        budget.handed_over(),
        admitted * LABELER_ATTACHMENT_BYTES_MAX
    );
    let room = LABELER_ATTACHMENT_FACET_MAX_BYTES - budget.handed_over();
    assert!(!budget.admit(room + 1), "past the facet ceiling");
    assert!(budget.admit(room), "the last byte of the facet still fits");
    assert!(!budget.admit(1), "a full facet admits nothing more");
    assert_eq!(budget.handed_over(), LABELER_ATTACHMENT_FACET_MAX_BYTES);
    assert!(budget.admit(0), "an empty attachment always fits");
}
