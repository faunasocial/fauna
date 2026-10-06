//! Tests for `Index::open_multi_segment` — combining N sealed segments into
//! one query view — and `Index::merge_segments` — collapsing N segments into
//! one for D8 compaction.

use fauna_index::{ContentId, ContentKind, FieldKind, Index, IndexedDoc, IndexedField};

fn doc(id: &[u8], body: &str, ts: i64) -> IndexedDoc {
    IndexedDoc {
        kind: ContentKind::Mail,
        content_id: ContentId(id.to_vec()),
        timestamp_ns: ts,
        sender_actor_id: None,
        secondary_id: None,
        fields: vec![IndexedField {
            kind: FieldKind::Body,
            text: body.into(),
        }],
    }
}

fn one_doc_segment(id: &[u8], body: &str, ts: i64) -> Vec<u8> {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(doc(id, body, ts)).unwrap();
    idx.commit().unwrap();
    idx.seal_to_bytes().unwrap()
}

#[test]
fn open_multi_segment_unions_hits_across_inputs() {
    let segs = vec![
        one_doc_segment(b"a", "alpha bravo", 1_000),
        one_doc_segment(b"b", "alpha charlie", 2_000),
        one_doc_segment(b"c", "alpha delta", 3_000),
    ];
    let combined = Index::open_multi_segment(&segs).unwrap();
    let hits = combined
        .query("alpha", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert_eq!(hits.len(), 3, "multi-segment query unions hits");
    let mut ids: Vec<&[u8]> = hits.iter().map(|h| h.content_id.0.as_slice()).collect();
    ids.sort();
    assert_eq!(ids, vec![&b"a"[..], &b"b"[..], &b"c"[..]]);
}

#[test]
fn open_multi_segment_with_empty_input_creates_empty_index() {
    let combined = Index::open_multi_segment(&[]).unwrap();
    let hits = combined
        .query("anything", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert!(hits.is_empty(), "empty multi-segment has no hits");
}

#[test]
fn open_multi_segment_with_single_input_round_trips() {
    let seg = one_doc_segment(b"only", "hello world", 1_000);
    let combined = Index::open_multi_segment(&[seg]).unwrap();
    let hits = combined
        .query("hello", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content_id, ContentId(b"only".to_vec()));
}

#[test]
fn open_multi_segment_rejects_garbage_input() {
    let garbage = vec![b"not a valid sealed segment".to_vec()];
    let result = Index::open_multi_segment(&garbage);
    assert!(
        result.is_err(),
        "garbage bytes must produce an error, not silently succeed"
    );
}

#[test]
fn open_multi_segment_rejects_schema_mismatch_across_inputs() {
    // Two segments with different schemas. We can't easily construct two
    // different Index types from this crate (the schema is fixed by
    // build_schema), so instead patch one segment's meta.json to claim a
    // different schema. The function should reject the combined open.
    let seg_a = one_doc_segment(b"a", "alpha", 1_000);
    let seg_b_orig = one_doc_segment(b"b", "bravo", 2_000);

    // Reparse seg_b's wire format, swap a field name in its meta.json,
    // re-encode. This produces a sealed segment whose meta.json claims a
    // different schema from seg_a.
    let entries_b = parse_seg_for_test(&seg_b_orig);
    let mut patched_entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(entries_b.len());
    for (name, bytes) in entries_b {
        if name == "meta.json" {
            // Patch: rename the "title" field to "title_swapped" in the
            // schema. This is the cheapest way to produce a different schema.
            let patched = String::from_utf8(bytes)
                .unwrap()
                .replace("\"title\"", "\"title_swapped\"");
            patched_entries.push((name, patched.into_bytes()));
        } else {
            patched_entries.push((name, bytes));
        }
    }
    let seg_b_patched = reencode_seg_for_test(&patched_entries);

    let result = Index::open_multi_segment(&[seg_a, seg_b_patched]);
    assert!(result.is_err(), "schema mismatch across inputs must error");
}

// Test-only helpers for re-parsing and re-encoding sealed bytes. They
// shadow the same wire-format logic as the production parse_wire_format
// — kept local to the test file rather than exposing a test-only public
// API on Index.
fn parse_seg_for_test(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    const FORMAT_VERSION: u32 = 1;
    let mut cursor = 0usize;
    let version = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
    assert_eq!(
        version, FORMAT_VERSION,
        "unexpected format version in test helper"
    );
    cursor += 4;
    let count = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        let name = std::str::from_utf8(&bytes[cursor..cursor + name_len])
            .unwrap()
            .to_string();
        cursor += name_len;
        let body_len = u64::from_be_bytes(bytes[cursor..cursor + 8].try_into().unwrap()) as usize;
        cursor += 8;
        let body = bytes[cursor..cursor + body_len].to_vec();
        cursor += body_len;
        out.push((name, body));
    }
    out
}

fn reencode_seg_for_test(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    const FORMAT_VERSION: u32 = 1;
    let mut out = Vec::new();
    out.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (name, body) in entries {
        out.extend_from_slice(&(name.len() as u32).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(body.len() as u64).to_be_bytes());
        out.extend_from_slice(body);
    }
    out
}

#[test]
fn merge_segments_collapses_inputs_into_one_queryable_blob() {
    let segs = vec![
        one_doc_segment(b"a", "alpha", 1_000),
        one_doc_segment(b"b", "bravo", 2_000),
        one_doc_segment(b"c", "charlie", 3_000),
    ];
    let merged_bytes = Index::merge_segments(&segs).unwrap();
    assert!(!merged_bytes.is_empty());

    let merged = Index::open_from_bytes(&merged_bytes).unwrap();
    for (term, expected_id) in [
        ("alpha", &b"a"[..]),
        ("bravo", &b"b"[..]),
        ("charlie", &b"c"[..]),
    ] {
        let hits = merged.query(term, &[ContentKind::Mail], None, 10).unwrap();
        assert_eq!(hits.len(), 1, "term `{term}` should hit");
        assert_eq!(
            hits[0].content_id.0, expected_id,
            "term `{term}` content_id"
        );
    }
}

#[test]
fn merge_segments_preserves_per_doc_metadata() {
    let segs = vec![
        one_doc_segment(b"old", "hello", 1_000),
        one_doc_segment(b"new", "hello", 5_000),
    ];
    let merged_bytes = Index::merge_segments(&segs).unwrap();
    let merged = Index::open_from_bytes(&merged_bytes).unwrap();

    let hits = merged
        .query("hello", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert_eq!(hits.len(), 2);
    let mut by_id: std::collections::HashMap<&[u8], i64> = std::collections::HashMap::new();
    for h in &hits {
        by_id.insert(h.content_id.0.as_slice(), h.timestamp_ns);
    }
    assert_eq!(by_id.get(&b"old"[..]), Some(&1_000));
    assert_eq!(by_id.get(&b"new"[..]), Some(&5_000));
}

#[test]
fn merge_segments_with_zero_inputs_returns_empty_index_bytes() {
    let merged_bytes = Index::merge_segments(&[]).unwrap();
    let merged = Index::open_from_bytes(&merged_bytes).unwrap();
    let hits = merged
        .query("anything", &[ContentKind::Mail], None, 10)
        .unwrap();
    assert!(hits.is_empty());
}
