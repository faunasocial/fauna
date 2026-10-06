//! Integration tests for the IndexManifest data shape.

use fauna_carv2::{Reader, Writer};
use fauna_cbor::{Cid, encode_canonical};
use fauna_index::{
    ContentKind, IndexError, IndexManifest, KindClass, KindManifest, MANIFEST_FORMAT_VERSION,
};
use std::collections::BTreeMap;
use std::io::Cursor;

#[test]
fn empty_manifest_round_trips_through_carv2() {
    let m = IndexManifest::empty(KindClass::Master, 7);
    let bytes = m.to_plaintext_bytes().expect("encode");
    let back = IndexManifest::from_plaintext_bytes(&bytes).expect("decode");
    assert_eq!(back, m);
    assert_eq!(back.format_version, MANIFEST_FORMAT_VERSION);
    assert_eq!(back.tokenizer_version, 7);
}

#[test]
fn populated_manifest_round_trips_through_carv2() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    m.kinds.push((
        ContentKind::Mail,
        KindManifest {
            next_seg_id: 5,
            live_segments: vec![1, 2, 4],
            tombstoned_segments: vec![3],
        },
    ));
    m.kinds.push((
        ContentKind::Calendar,
        KindManifest {
            next_seg_id: 1,
            live_segments: vec![],
            tombstoned_segments: vec![],
        },
    ));
    let bytes = m.to_plaintext_bytes().expect("encode");
    let back = IndexManifest::from_plaintext_bytes(&bytes).expect("decode");
    assert_eq!(back, m);
}

/// Encode an arbitrary manifest-shaped payload as a real CARv2 file — the bytes a
/// **future** build would write. The only way to prove a tolerant reader tolerates.
fn carv2_of(payload: &[u8]) -> Vec<u8> {
    let root = Cid::of_dag_cbor(payload);
    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[&root]).expect("writer");
        writer.write_block(&root, payload).expect("write block");
        writer.finalize().expect("finalize");
    }
    buf.into_inner()
}

/// A manifest a newer build grew **additively** — higher `format_version`, reader floor
/// untouched — must READ, not be rejected (I2 backward-compat, `version-compatibility.md`
/// § 2.2). The old exact-match `format_version != 1` reject failed exactly here: it
/// orphaned every future manifest, which is the intolerant-reader hazard this scheme
/// removes.
///
/// And the fields this build cannot name must survive: `unknown_future_field` lands in
/// `extra` and is re-emitted on the next rewrite, so an older build appending a segment
/// does not silently drop a newer build's data.
#[test]
fn newer_additive_manifest_reads_and_preserves_unknown_fields() {
    let mut payload: BTreeMap<String, fauna_cbor::Value> = BTreeMap::new();
    payload.insert("format_version".into(), fauna_cbor::Value::Integer(9));
    payload.insert("min_reader_version".into(), fauna_cbor::Value::Integer(2));
    payload.insert("class".into(), fauna_cbor::Value::String("master".into()));
    payload.insert("tokenizer_version".into(), fauna_cbor::Value::Integer(3));
    payload.insert("kinds".into(), fauna_cbor::Value::List(vec![]));
    payload.insert(
        "unknown_future_field".into(),
        fauna_cbor::Value::String("a field this build has never heard of".into()),
    );
    let bytes = carv2_of(&encode_canonical(&payload).expect("encode a future manifest"));

    let m = IndexManifest::from_plaintext_bytes(&bytes)
        .expect("a newer-ADDITIVE manifest must read (I2)");
    assert_eq!(m.format_version, 9);
    assert_eq!(m.tokenizer_version, 3);
    assert_eq!(
        m.extra.get("unknown_future_field"),
        Some(&fauna_cbor::Value::String(
            "a field this build has never heard of".into()
        )),
        "an unknown field must land in `extra`, not be dropped"
    );

    // ...and survive this build re-encoding the manifest (the rewrite that would
    // otherwise destroy it).
    let rewritten = m.to_plaintext_bytes().expect("re-encode");
    let back = IndexManifest::from_plaintext_bytes(&rewritten).expect("re-read");
    assert_eq!(back.extra, m.extra, "the rewrite must not drop `extra`");
    assert_eq!(
        back.format_version, 9,
        "the rewrite must not restamp a v9 manifest down to v1 (§ 2.2)"
    );
}

/// The other half: a newer build that raised the reader **floor** past us. Refuse with
/// the typed `Incompatible` — "intact, this build is too old" — never a generic decode
/// error a caller could mistake for "corrupt, recreate it".
#[test]
fn newer_breaking_manifest_refuses_with_the_typed_error() {
    // Relative to the shipped constant, so a format bump keeps this manifest
    // genuinely in the future (a hardcoded 4 went stale at the v3 bump).
    let breaking = fauna_index::CURRENT_INDEX_FORMAT_VERSION + 1;
    let mut payload: BTreeMap<String, fauna_cbor::Value> = BTreeMap::new();
    payload.insert(
        "format_version".into(),
        fauna_cbor::Value::Integer(breaking as i128),
    );
    payload.insert(
        "min_reader_version".into(),
        fauna_cbor::Value::Integer(breaking as i128),
    );
    payload.insert("tokenizer_version".into(), fauna_cbor::Value::Integer(1));
    payload.insert("kinds".into(), fauna_cbor::Value::List(vec![]));
    let bytes = carv2_of(&encode_canonical(&payload).expect("encode"));

    let err = IndexManifest::from_plaintext_bytes(&bytes)
        .expect_err("a newer-BREAKING manifest must refuse");
    assert!(
        matches!(
            err,
            IndexError::Incompatible {
                file_v,
                file_min,
                bin_v: fauna_index::CURRENT_INDEX_FORMAT_VERSION,
            } if file_v == breaking && file_min == breaking
        ),
        "must be the typed Incompatible, not SchemaMismatch: {err}"
    );
}

/// **The peek** — the piece that makes the refusal actionable. A future non-additive
/// change retypes `kinds`, so this build genuinely cannot decode the manifest; the
/// version pair must still come out of it, or "too old" and "corrupt" are
/// indistinguishable and the user gets an error they cannot act on.
#[test]
fn stamp_peeks_out_of_a_manifest_this_build_cannot_parse() {
    let mut payload: BTreeMap<String, fauna_cbor::Value> = BTreeMap::new();
    payload.insert("format_version".into(), fauna_cbor::Value::Integer(4));
    payload.insert("min_reader_version".into(), fauna_cbor::Value::Integer(4));
    payload.insert("tokenizer_version".into(), fauna_cbor::Value::Integer(1));
    payload.insert(
        "kinds".into(),
        fauna_cbor::Value::String("retyped by a future build".into()),
    );
    let bytes = carv2_of(&encode_canonical(&payload).expect("encode"));

    let stamp = IndexManifest::peek_stamp(&bytes)
        .expect("the stamp must peek out of a payload this build cannot decode");
    assert_eq!(stamp.format_version, 4);
    assert_eq!(stamp.min_reader_version, 4);
}

/// A manifest carrying no `min_reader_version` key — the shape a writer predating the
/// scheme produced — is refused, never read as a baseline. The pre-scheme "absent ⇒
/// baseline" default was retired by the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2, program 4).
#[test]
fn a_manifest_without_a_reader_floor_is_refused() {
    let mut payload: BTreeMap<String, fauna_cbor::Value> = BTreeMap::new();
    payload.insert("format_version".into(), fauna_cbor::Value::Integer(1));
    payload.insert("class".into(), fauna_cbor::Value::String("master".into()));
    payload.insert("tokenizer_version".into(), fauna_cbor::Value::Integer(2));
    payload.insert("kinds".into(), fauna_cbor::Value::List(vec![]));
    let bytes = carv2_of(&encode_canonical(&payload).expect("encode"));

    let err = IndexManifest::from_plaintext_bytes(&bytes)
        .expect_err("a manifest without a reader floor must be refused");
    assert!(
        matches!(err, IndexError::SchemaMismatch(_)),
        "an unstamped manifest is a decode refusal, got: {err}"
    );
    assert!(
        IndexManifest::peek_stamp(&bytes).is_err(),
        "the peek refuses it too"
    );
}

#[test]
fn plaintext_bytes_are_single_block_carv2() {
    // Confirm the plaintext layer is a standards-conformant single-block
    // CARv2 file: any CARv2 reader can parse it, there's exactly one block,
    // and the root CID is the BLAKE3 hash of the dag-cbor payload.
    let mut m = IndexManifest::empty(KindClass::MailCal, 3);
    m.kinds.push((
        ContentKind::Mail,
        KindManifest {
            next_seg_id: 2,
            live_segments: vec![1],
            tombstoned_segments: vec![],
        },
    ));
    let bytes = m.to_plaintext_bytes().expect("encode");

    let mut cursor = Cursor::new(&bytes);
    let mut reader = Reader::new(&mut cursor).expect("CARv2 reader");
    assert_eq!(reader.len(), 1, "manifest must be a single-block CARv2");

    let (cid, payload) = reader
        .iter()
        .next()
        .expect("one block")
        .expect("read block ok");
    // The root CID must be the canonical hash of the payload bytes.
    assert_eq!(
        cid,
        Cid::of_dag_cbor(&payload),
        "root cid must hash payload"
    );
    // The payload itself must round-trip back to the same manifest via the
    // dag-cbor decoder (so the CARv2 framing isn't accidentally lossy).
    let from_payload: IndexManifest =
        fauna_cbor::decode_strict(&payload).expect("dag-cbor decode payload");
    assert_eq!(from_payload, m);
}

use fauna_index::IndexMasterKey;

fn key(seed: u8) -> IndexMasterKey {
    IndexMasterKey::from_bytes([seed; 32])
}

#[test]
fn manifest_seal_open_round_trip() {
    let master = key(120);
    let mut m = IndexManifest::empty(KindClass::Master, 2);
    m.kinds.push((
        ContentKind::Post,
        KindManifest {
            next_seg_id: 11,
            live_segments: vec![1, 5, 9, 10],
            tombstoned_segments: vec![2, 3, 4, 6, 7, 8],
        },
    ));
    let sealed = m.to_sealed_bytes(&master).expect("seal");
    let opened = IndexManifest::from_sealed_bytes(&sealed, &master).expect("open");
    assert_eq!(opened, m);
}

#[test]
fn manifest_seal_open_round_trip_empty_kinds() {
    let master = key(121);
    let m = IndexManifest::empty(KindClass::Master, 0);
    let sealed = m.to_sealed_bytes(&master).expect("seal");
    let opened = IndexManifest::from_sealed_bytes(&sealed, &master).expect("open");
    assert_eq!(opened, m);
}

#[test]
fn manifest_open_with_wrong_master_fails() {
    let alice = key(122);
    let bob = key(123);
    let m = IndexManifest::empty(KindClass::Master, 0);
    let sealed = m.to_sealed_bytes(&alice).expect("seal");
    let err = IndexManifest::from_sealed_bytes(&sealed, &bob).expect_err("wrong master must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("decrypt master-direct body failed"),
        "unexpected: {msg}"
    );
}

#[test]
fn manifest_open_rejects_tampered_body() {
    let master = key(124);
    let m = IndexManifest::empty(KindClass::Master, 0);
    let mut sealed = m.to_sealed_bytes(&master).expect("seal");
    // Header is 32 bytes (FXMG + ver + alg + reserved + nonce), body starts at 32.
    let body_byte = 32 + 1;
    sealed[body_byte] ^= 0x10;
    let err =
        IndexManifest::from_sealed_bytes(&sealed, &master).expect_err("tampered body must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("decrypt master-direct body failed"),
        "unexpected: {msg}"
    );
}

use fauna_index::{INDEX_FOLDER, manifest_path, parse_segment_path, segment_path};

#[test]
fn index_folder_constant_matches_spec() {
    assert_eq!(INDEX_FOLDER, "__index");
}

#[test]
fn manifest_path_is_stable() {
    assert_eq!(manifest_path(), "__index/manifest.idx");
}

#[test]
fn segment_path_zero_pads_seq_to_eight_digits() {
    assert_eq!(
        segment_path(ContentKind::Mail, 1),
        "__index/mail/seg-00000001.idx"
    );
    assert_eq!(
        segment_path(ContentKind::Calendar, 42),
        "__index/calendar/seg-00000042.idx"
    );
    assert_eq!(
        segment_path(ContentKind::Media, 99_999_999),
        "__index/media/seg-99999999.idx"
    );
}

#[test]
fn segment_path_uses_lowercase_kind_names() {
    // Sanity: ContentKind::as_str returns lowercase already, but pin it here
    // so a future ContentKind change can't accidentally break the wire shape.
    for kind in ContentKind::ALL {
        let path = segment_path(*kind, 1);
        assert!(
            path.contains(kind.as_str()),
            "{path} should contain {}",
            kind.as_str()
        );
        assert_eq!(
            path.to_lowercase(),
            path,
            "segment path should be lowercase"
        );
    }
}

#[test]
fn parse_segment_path_round_trips() {
    for kind in ContentKind::ALL {
        for &seq in &[1u32, 2, 100, 12_345, 99_999_999] {
            let path = segment_path(*kind, seq);
            let (parsed_kind, parsed_seq) =
                parse_segment_path(&path).unwrap_or_else(|| panic!("failed to parse {path}"));
            assert_eq!(parsed_kind, *kind);
            assert_eq!(parsed_seq, seq);
        }
    }
}

#[test]
fn parse_segment_path_rejects_malformed() {
    assert!(parse_segment_path("__index/manifest.idx").is_none());
    assert!(
        parse_segment_path("__index/mail/seg-1.idx").is_none(),
        "must be 8-digit padded"
    );
    assert!(
        parse_segment_path("__index/mail/seg-00000001").is_none(),
        "must end in .idx"
    );
    assert!(parse_segment_path("__index/unknown/seg-00000001.idx").is_none());
    assert!(
        parse_segment_path("foo/mail/seg-00000001.idx").is_none(),
        "wrong folder prefix"
    );
    assert!(parse_segment_path("").is_none());
}

#[test]
fn append_segment_assigns_next_seg_id_and_appends_to_live() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    let id1 = m
        .append_segment(ContentKind::Mail)
        .expect("mail is mailcal-class");
    assert_eq!(id1, 1);
    let id2 = m
        .append_segment(ContentKind::Mail)
        .expect("mail is mailcal-class");
    assert_eq!(id2, 2);

    let kind = m.kind(ContentKind::Mail).expect("mail kind populated");
    assert_eq!(kind.next_seg_id, 3);
    assert_eq!(kind.live_segments, vec![1, 2]);
    assert!(kind.tombstoned_segments.is_empty());
}

#[test]
fn append_segment_creates_kind_entry_lazily() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    assert!(m.kind(ContentKind::Calendar).is_none());
    m.append_segment(ContentKind::Calendar)
        .expect("calendar is mailcal-class");
    assert!(m.kind(ContentKind::Calendar).is_some());
    // mail still absent — append_segment only touches the requested kind
    assert!(m.kind(ContentKind::Mail).is_none());
}

#[test]
fn tombstone_segment_moves_id_from_live_to_tombstoned() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    let _ = m.append_segment(ContentKind::Mail).expect("append"); // 1
    let id2 = m
        .append_segment(ContentKind::Mail)
        .expect("mail is mailcal-class"); // 2
    let _ = m.append_segment(ContentKind::Mail).expect("append"); // 3

    let removed = m.tombstone_segment(ContentKind::Mail, id2);
    assert!(
        removed,
        "tombstone_segment should report success on a live id"
    );

    let kind = m.kind(ContentKind::Mail).expect("mail kind");
    assert_eq!(kind.live_segments, vec![1, 3]);
    assert_eq!(kind.tombstoned_segments, vec![2]);
    // next_seg_id never decreases
    assert_eq!(kind.next_seg_id, 4);
}

#[test]
fn tombstone_segment_returns_false_for_unknown_id() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    let _ = m.append_segment(ContentKind::Mail).expect("append");
    let removed = m.tombstone_segment(ContentKind::Mail, 999);
    assert!(
        !removed,
        "tombstone_segment must report false for unknown ids"
    );

    let kind = m.kind(ContentKind::Mail).expect("mail kind");
    assert_eq!(kind.live_segments, vec![1]);
    assert!(kind.tombstoned_segments.is_empty());
}

#[test]
fn tombstone_segment_returns_false_for_unknown_kind() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    let removed = m.tombstone_segment(ContentKind::Calendar, 1);
    assert!(!removed);
    assert!(m.kind(ContentKind::Calendar).is_none());
}

#[test]
fn live_segments_stay_sorted_after_appends_and_tombstones() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    for _ in 0..5 {
        m.append_segment(ContentKind::Mail).expect("append");
    }
    m.tombstone_segment(ContentKind::Mail, 3);
    m.tombstone_segment(ContentKind::Mail, 1);
    let kind = m.kind(ContentKind::Mail).expect("mail kind");
    assert_eq!(kind.live_segments, vec![2, 4, 5], "live must remain sorted");
    assert_eq!(
        kind.tombstoned_segments,
        vec![1, 3],
        "tombstones sorted ascending"
    );
}

// ───────────────────────── v2 per-kind split (S1, 2026-08-02) ─────────────────────────

use fauna_index::{IndexSegmentKey, MAILCAL_MANIFEST_FILE_NAME, mailcal_manifest_path};

fn seg_key(seed: u8) -> IndexSegmentKey {
    IndexSegmentKey::from_bytes([seed; 32])
}

#[test]
fn mailcal_manifest_path_is_stable() {
    assert_eq!(MAILCAL_MANIFEST_FILE_NAME, "manifest-mailcal.idx");
    assert_eq!(mailcal_manifest_path(), "__index/manifest-mailcal.idx");
}

/// The wrong-class refusal — the split is enforced at the API, never trusted to
/// convention (`content-index.md` § Encryption posture).
#[test]
fn append_segment_refuses_a_wrong_class_kind() {
    let mut master_m = IndexManifest::empty(KindClass::Master, 1);
    let err = master_m
        .append_segment(ContentKind::Mail)
        .expect_err("mail must refuse in the master manifest");
    assert!(
        matches!(err, IndexError::WrongKindClass { .. }),
        "typed refusal expected: {err}"
    );
    assert!(
        master_m.kind(ContentKind::Mail).is_none(),
        "the refused kind must not have been created"
    );

    let mut mc_m = IndexManifest::empty(KindClass::MailCal, 1);
    let err = mc_m
        .append_segment(ContentKind::Post)
        .expect_err("post must refuse in the mail/calendar manifest");
    assert!(matches!(err, IndexError::WrongKindClass { .. }));
}

#[test]
fn mailcal_manifest_seal_open_round_trip_under_segment_key() {
    let key = seg_key(42);
    let mut m = IndexManifest::empty(KindClass::MailCal, 5);
    m.append_segment(ContentKind::Mail).expect("append");
    m.append_segment(ContentKind::Calendar).expect("append");
    let sealed = m.to_sealed_bytes_mailcal(&key).expect("seal");
    let opened = IndexManifest::from_sealed_bytes_mailcal(&sealed, &key).expect("open");
    assert_eq!(opened, m);
}

/// A manifest must refuse to seal under the other class's key — both directions.
#[test]
fn sealing_under_the_wrong_class_key_is_refused() {
    let mc = IndexManifest::empty(KindClass::MailCal, 1);
    let err = mc
        .to_sealed_bytes(&key(1))
        .expect_err("mailcal manifest must not seal under the master key");
    assert!(matches!(err, IndexError::SchemaMismatch(_)), "{err}");

    let master = IndexManifest::empty(KindClass::Master, 1);
    let err = master
        .to_sealed_bytes_mailcal(&seg_key(1))
        .expect_err("master manifest must not seal under the segment key");
    assert!(matches!(err, IndexError::SchemaMismatch(_)), "{err}");
}

/// Cross-key opens fail at the AEAD — a mailcal blob never opens with master-key
/// bytes, even when both keys carry identical raw bytes the class check still
/// refuses (belt-and-suspenders in `from_sealed_bytes`).
#[test]
fn mailcal_sealed_blob_does_not_open_as_a_master_manifest() {
    let mut mc = IndexManifest::empty(KindClass::MailCal, 1);
    mc.append_segment(ContentKind::Mail).expect("append");
    let sealed = mc.to_sealed_bytes_mailcal(&seg_key(9)).expect("seal");

    // Different key bytes: AEAD refusal.
    let err = IndexManifest::from_sealed_bytes(&sealed, &key(10))
        .expect_err("wrong key bytes must fail the AEAD");
    assert!(format!("{err}").contains("decrypt master-direct body failed"));

    // Same raw bytes smuggled into the master type: the class check refuses.
    let err = IndexManifest::from_sealed_bytes(&sealed, &key(9))
        .expect_err("same-bytes master open must refuse on class");
    assert!(
        format!("{err}").contains("wrong reader"),
        "the decoded-class check must fire: {err}"
    );
}

#[test]
fn ingest_cursor_max_merges_and_class_guards() {
    let mut m = IndexManifest::empty(KindClass::MailCal, 1);
    assert_eq!(m.ingest_cursor(ContentKind::Mail), None);

    assert!(m.merge_ingest_cursor(ContentKind::Mail, 40).expect("merge"));
    assert_eq!(m.ingest_cursor(ContentKind::Mail), Some(40));

    // A lower cursor never regresses the recorded one (max-merge).
    assert!(!m.merge_ingest_cursor(ContentKind::Mail, 12).expect("merge"));
    assert_eq!(m.ingest_cursor(ContentKind::Mail), Some(40));

    assert!(m.merge_ingest_cursor(ContentKind::Mail, 41).expect("merge"));
    assert_eq!(m.ingest_cursor(ContentKind::Mail), Some(41));

    // Class guard fires here too.
    let err = m
        .merge_ingest_cursor(ContentKind::Post, 1)
        .expect_err("post cursor must refuse in the mailcal manifest");
    assert!(matches!(err, IndexError::WrongKindClass { .. }));

    // And the cursors survive the CARv2 round trip.
    let bytes = m.to_plaintext_bytes().expect("encode");
    let back = IndexManifest::from_plaintext_bytes(&bytes).expect("decode");
    assert_eq!(back.ingest_cursor(ContentKind::Mail), Some(41));
}

/// The manifest `class` field round-trips through the wire — and a payload without
/// one (the pre-v2 shape) is refused rather than defaulted to Master: the default was
/// retired by the compat-remnant sweep (`version-compatibility.md` § Dimension 2,
/// program 4).
#[test]
fn class_field_round_trips_and_a_class_less_payload_is_refused() {
    let m = IndexManifest::empty(KindClass::MailCal, 1);
    let bytes = m.to_plaintext_bytes().expect("encode");
    let back = IndexManifest::from_plaintext_bytes(&bytes).expect("decode");
    assert_eq!(back.class, KindClass::MailCal);

    let mut payload: BTreeMap<String, fauna_cbor::Value> = BTreeMap::new();
    payload.insert(
        "format_version".into(),
        fauna_cbor::Value::Integer(fauna_index::CURRENT_INDEX_FORMAT_VERSION as i128),
    );
    payload.insert(
        "min_reader_version".into(),
        fauna_cbor::Value::Integer(fauna_index::MIN_READER_INDEX_FORMAT_VERSION as i128),
    );
    payload.insert("tokenizer_version".into(), fauna_cbor::Value::Integer(2));
    payload.insert("kinds".into(), fauna_cbor::Value::List(vec![]));
    let bytes = carv2_of(&encode_canonical(&payload).expect("encode"));
    assert!(
        matches!(
            IndexManifest::from_plaintext_bytes(&bytes),
            Err(IndexError::SchemaMismatch(_))
        ),
        "a class-less manifest must be refused"
    );
}
