use fauna_index::{ContentId, ContentKind, FieldKind, Index, IndexedDoc, IndexedField};

fn body_doc(id: &[u8], kind: ContentKind, ts: i64, body: &str) -> IndexedDoc {
    IndexedDoc {
        kind,
        content_id: ContentId(id.to_vec()),
        timestamp_ns: ts,
        sender_actor_id: None,
        secondary_id: None,
        fields: vec![IndexedField {
            kind: FieldKind::Body,
            text: body.to_string(),
        }],
    }
}

#[test]
fn add_then_query_returns_matching_doc() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"id-1", ContentKind::Mail, 1_000, "hello world"))
        .unwrap();
    idx.add_doc(body_doc(
        b"id-2",
        ContentKind::Mail,
        2_000,
        "the quick brown fox",
    ))
    .unwrap();
    idx.commit().unwrap();

    let hits = idx.query("hello", &[ContentKind::Mail], None, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content_id, ContentId(b"id-1".to_vec()));
}

#[test]
fn unrelated_kind_filter_excludes_other_kinds() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"m1", ContentKind::Mail, 1_000, "hello"))
        .unwrap();
    idx.add_doc(body_doc(b"p1", ContentKind::Post, 2_000, "hello"))
        .unwrap();
    idx.commit().unwrap();

    let hits = idx.query("hello", &[ContentKind::Post], None, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, ContentKind::Post);
}

#[test]
fn empty_query_returns_no_results() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"m1", ContentKind::Mail, 1_000, "hello"))
        .unwrap();
    idx.commit().unwrap();

    let hits = idx.query("", ContentKind::ALL, None, 10).unwrap();
    assert_eq!(hits.len(), 0, "empty query produces no hits");
}

#[test]
fn phrase_query_matches_only_when_terms_are_adjacent() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(
        b"adj",
        ContentKind::Mail,
        1_000,
        "alice rust meeting",
    ))
    .unwrap();
    idx.add_doc(body_doc(
        b"split",
        ContentKind::Mail,
        2_000,
        "alice talked about rust today",
    ))
    .unwrap();
    idx.commit().unwrap();

    let hits = idx
        .query("\"alice rust\"", &[ContentKind::Mail], None, 10)
        .unwrap();
    let ids: Vec<&[u8]> = hits.iter().map(|h| h.content_id.0.as_slice()).collect();
    assert_eq!(
        ids,
        vec![&b"adj"[..]],
        "phrase query matches adjacency only"
    );
}

#[test]
fn bm25_ranks_rare_terms_above_common_ones() {
    let mut idx = Index::create_in_ram().unwrap();
    // "the" is in every doc, "quokka" is in only one — BM25 should rank the
    // doc containing both above the doc that only contains the common term.
    for i in 0..20 {
        let id = format!("filler-{}", i).into_bytes();
        idx.add_doc(body_doc(
            &id,
            ContentKind::Mail,
            i,
            "the quick brown fox jumps over the lazy dog",
        ))
        .unwrap();
    }
    idx.add_doc(body_doc(
        b"quokka",
        ContentKind::Mail,
        100,
        "the quokka is a small marsupial",
    ))
    .unwrap();
    idx.commit().unwrap();

    let hits = idx
        .query("the quokka", &[ContentKind::Mail], None, 5)
        .unwrap();
    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].content_id.0,
        b"quokka".to_vec(),
        "doc with rare term should rank first"
    );
}

#[test]
fn time_range_filter_excludes_out_of_range_docs() {
    use fauna_index::TimeRange;
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"old", ContentKind::Mail, 1_000, "hello"))
        .unwrap();
    idx.add_doc(body_doc(b"new", ContentKind::Mail, 5_000, "hello"))
        .unwrap();
    idx.commit().unwrap();

    let hits = idx
        .query(
            "hello",
            &[ContentKind::Mail],
            Some(TimeRange {
                start_ns: 4_000,
                end_ns: 6_000,
            }),
            10,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content_id.0, b"new".to_vec());
}

#[test]
fn seal_to_bytes_then_open_from_bytes_round_trips_query_results() {
    use fauna_index::Index;

    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(body_doc(b"a", ContentKind::Mail, 1_000, "alice rust"))
        .unwrap();
    idx.add_doc(body_doc(b"b", ContentKind::Mail, 2_000, "bob python"))
        .unwrap();
    idx.commit().unwrap();

    let bytes = idx.seal_to_bytes().unwrap();
    assert!(!bytes.is_empty(), "sealed bytes must not be empty");

    let opened = Index::open_from_bytes(&bytes).unwrap();
    let hits = opened
        .query("alice", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content_id.0, b"a".to_vec());
}

#[test]
fn open_from_bytes_rejects_garbage() {
    use fauna_index::Index;
    let result = Index::open_from_bytes(b"not a valid sealed segment");
    assert!(result.is_err(), "open_from_bytes must reject garbage input");
}

// The `open_from_bytes_rejects_garbage` test above only proves *some* error
// comes back for *some* garbage. The wire parser (`parse_wire_format`, private
// to the crate, reached only through `Index::open_from_bytes`/
// `open_multi_segment`) has 8 distinct validation branches over
// attacker-influenceable sealed-segment bytes; each below pins the SPECIFIC
// branch that fires (via the `IndexError::SchemaMismatch` message), not just
// "it errored". Format: `[u32 BE version][u32 BE count]` then, per entry,
// `[u32 BE name_len][name][u64 BE body_len][body]`.
const WIRE_FORMAT_VERSION: u32 = 1;

fn wire_header(version: u32, count: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out
}

/// `Index` has no `Debug` impl (so `.expect_err()`/`.unwrap_err()` don't
/// compile against it — both require the `Ok` side to be `Debug` too);
/// panics with `must reject` on an unexpected `Ok`.
fn open_err(bytes: &[u8]) -> fauna_index::IndexError {
    match Index::open_from_bytes(bytes) {
        Ok(_) => panic!("must reject"),
        Err(e) => e,
    }
}

#[test]
fn open_from_bytes_rejects_too_short_input() {
    // Fewer than the 8-byte header (version + count) itself.
    let err = open_err(&[1, 2, 3]);
    assert!(err.to_string().contains("too short"), "wrong branch: {err}");
}

#[test]
fn open_from_bytes_rejects_an_unsupported_version() {
    let bytes = wire_header(WIRE_FORMAT_VERSION + 1, 0);
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("unsupported format version"),
        "wrong branch: {err}"
    );
}

#[test]
fn open_from_bytes_rejects_a_truncated_name_length() {
    // Header claims 1 entry, then ends before that entry's 4-byte name_len.
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    bytes.extend_from_slice(&[0, 0]); // 2 of the 4 name_len bytes
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("truncated (name length)"),
        "wrong branch: {err}"
    );
}

#[test]
fn open_from_bytes_rejects_a_truncated_name() {
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    bytes.extend_from_slice(&10u32.to_be_bytes()); // name_len = 10
    bytes.extend_from_slice(b"short"); // only 5 bytes actually present
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("truncated (name)"),
        "wrong branch: {err}"
    );
}

#[test]
fn open_from_bytes_rejects_a_non_utf8_name() {
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    let garbage_name = [0xFFu8, 0xFE, 0xFD];
    bytes.extend_from_slice(&(garbage_name.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&garbage_name);
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("non-utf8 file name"),
        "wrong branch: {err}"
    );
}

#[test]
fn open_from_bytes_rejects_a_truncated_body_length() {
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    bytes.extend_from_slice(&4u32.to_be_bytes()); // name_len = 4
    bytes.extend_from_slice(b"meta"); // name present in full
    bytes.extend_from_slice(&[0, 0, 0]); // 3 of the 8 body_len bytes
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("truncated (body length)"),
        "wrong branch: {err}"
    );
}

#[test]
fn open_from_bytes_rejects_a_truncated_body() {
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    bytes.extend_from_slice(&4u32.to_be_bytes());
    bytes.extend_from_slice(b"meta");
    bytes.extend_from_slice(&100u64.to_be_bytes()); // body_len = 100
    bytes.extend_from_slice(b"only a few bytes"); // far fewer than 100
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("truncated (body)"),
        "wrong branch: {err}"
    );
}

#[test]
fn open_from_bytes_rejects_trailing_bytes() {
    // One fully valid, self-consistent entry, then extra unconsumed bytes.
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    bytes.extend_from_slice(&4u32.to_be_bytes());
    bytes.extend_from_slice(b"meta");
    bytes.extend_from_slice(&3u64.to_be_bytes());
    bytes.extend_from_slice(b"abc");
    bytes.push(0xFF); // trailing byte the entry count never accounted for
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("trailing bytes"),
        "wrong branch: {err}"
    );
}

/// Pins the doc comment's overflow-safety claim on `parse_wire_format`: a
/// tampered `body_len` near `u64::MAX` must fail cleanly via the `checked_sub`
/// remaining-bytes guard (`IndexError::SchemaMismatch`), never panic/overflow
/// a naive `cursor + body_len > bytes.len()` comparison. This is the one
/// invariant the module doc calls out explicitly (wasm32-safety) and the crate
/// had zero coverage proving it holds before this test.
#[test]
fn open_from_bytes_rejects_a_near_max_body_length_without_overflow_panic() {
    let mut bytes = wire_header(WIRE_FORMAT_VERSION, 1);
    bytes.extend_from_slice(&4u32.to_be_bytes());
    bytes.extend_from_slice(b"meta");
    bytes.extend_from_slice(&(u64::MAX - 1).to_be_bytes()); // tampered body_len
    let err = open_err(&bytes);
    assert!(
        err.to_string().contains("truncated (body)"),
        "wrong branch: {err}"
    );
}

#[test]
fn seal_to_bytes_is_deterministic_for_identical_inputs() {
    let build = || {
        let mut idx = Index::create_in_ram().unwrap();
        for (i, body) in ["alice rust", "bob python", "carol go"].iter().enumerate() {
            idx.add_doc(body_doc(
                format!("doc-{i}").as_bytes(),
                ContentKind::Mail,
                (i as i64) * 1_000,
                body,
            ))
            .unwrap();
        }
        idx.seal_to_bytes().unwrap()
    };

    let a = build();
    let b = build();
    assert_eq!(
        a, b,
        "two indexes built from identical input must seal to identical bytes"
    );
}

#[test]
fn merge_segments_is_deterministic_for_identical_inputs() {
    // Two distinct single-segment seals (so `open_multi_segment` must remap
    // their colliding canonical ids), merged into one — twice. The merge path
    // is the other producer of sealed bytes, so it gets its own determinism
    // check.
    let seal = |docs: &[(&[u8], &str)]| {
        let mut idx = Index::create_in_ram().unwrap();
        for (i, (id, body)) in docs.iter().enumerate() {
            idx.add_doc(body_doc(id, ContentKind::Mail, (i as i64) * 1_000, body))
                .unwrap();
        }
        idx.seal_to_bytes().unwrap()
    };
    let a = seal(&[(b"x", "alice rust"), (b"y", "bob python")]);
    let b = seal(&[(b"z", "carol go")]);

    let merged1 = Index::merge_segments(&[a.clone(), b.clone()]).unwrap();
    let merged2 = Index::merge_segments(&[a, b]).unwrap();
    assert_eq!(
        merged1, merged2,
        "merging the same inputs twice must produce identical bytes"
    );

    // ...and the merged index still answers queries across all inputs.
    let opened = Index::open_from_bytes(&merged1).unwrap();
    for (q, id) in [("alice", b"x".as_slice()), ("carol", b"z".as_slice())] {
        let hits = opened.query(q, &[ContentKind::Mail], None, 10).unwrap();
        assert_eq!(hits.len(), 1, "query {q:?} should hit exactly one doc");
        assert_eq!(hits[0].content_id.0, id.to_vec());
    }
}

/// NFKC-changing input produces
/// raw-input token offsets through the Tantivy adapter, so snippet rendering
/// can slice the source text (the S1 raw-offset fix, 2026-08-02).
#[test]
fn adapter_offsets_slice_the_raw_input_for_nfkc_changing_text() {
    use fauna_index::tokenizer_adapter::FaunaTokenizer;
    use tantivy::tokenizer::{TokenStream, Tokenizer};

    let raw = "of\u{FB01}cial \u{FF21}\u{FF22} cafe\u{301} report";
    let mut tok = FaunaTokenizer::new();
    let mut stream = tok.token_stream(raw);
    let mut spans = Vec::new();
    while stream.advance() {
        let t = stream.token();
        // Every offset must be a valid char boundary of the RAW string —
        // slicing must not panic — and the slice must NFKC-fold back to the
        // token text.
        let slice = &raw[t.offset_from..t.offset_to];
        spans.push((t.text.clone(), slice.to_string()));
    }
    let texts: Vec<&str> = spans.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(texts, vec!["official", "ab", "café", "report"]);
    assert_eq!(spans[0].1, "of\u{FB01}cial");
    assert_eq!(spans[1].1, "\u{FF21}\u{FF22}");
    assert_eq!(spans[2].1, "cafe\u{301}");
    assert_eq!(spans[3].1, "report");
}
