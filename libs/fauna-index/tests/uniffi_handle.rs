//! Integration test for the UniFFI `IndexHandle` wrapper.
//!
//! Exercises the same round-trip the bare `Index` already covers, but through
//! the lock-protected handle that UniFFI exposes to clients.

#![cfg(feature = "uniffi")]

use fauna_index::{
    ContentId, ContentKind, FieldKind, IndexHandle, IndexedDoc, IndexedField, TimeRange,
};
use std::sync::Arc;

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
fn handle_round_trip() {
    let handle: Arc<IndexHandle> = IndexHandle::create_in_ram().unwrap();
    handle
        .add_doc(body_doc(b"a", ContentKind::Mail, 1_000, "hello world"))
        .unwrap();
    handle
        .add_doc(body_doc(
            b"b",
            ContentKind::Mail,
            2_000,
            "the quick brown fox",
        ))
        .unwrap();
    handle.commit().unwrap();

    let hits = handle
        .query("hello".to_string(), vec![ContentKind::Mail], None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].content_id, ContentId(b"a".to_vec()));
}

#[test]
fn handle_kind_filter() {
    let handle = IndexHandle::create_in_ram().unwrap();
    handle
        .add_doc(body_doc(b"m", ContentKind::Mail, 1_000, "hello"))
        .unwrap();
    handle
        .add_doc(body_doc(b"p", ContentKind::Post, 2_000, "hello"))
        .unwrap();
    handle.commit().unwrap();

    let hits = handle
        .query("hello".to_string(), vec![ContentKind::Post], None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, ContentKind::Post);
}

#[test]
fn handle_time_range_filter() {
    let handle = IndexHandle::create_in_ram().unwrap();
    handle
        .add_doc(body_doc(b"old", ContentKind::Mail, 1_000, "hello"))
        .unwrap();
    handle
        .add_doc(body_doc(b"new", ContentKind::Mail, 5_000, "hello"))
        .unwrap();
    handle.commit().unwrap();

    let hits = handle
        .query(
            "hello".to_string(),
            vec![ContentKind::Mail],
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
fn handle_empty_query_yields_no_hits() {
    let handle = IndexHandle::create_in_ram().unwrap();
    handle
        .add_doc(body_doc(b"m", ContentKind::Mail, 1_000, "hello"))
        .unwrap();
    handle.commit().unwrap();

    let hits = handle
        .query("".to_string(), vec![ContentKind::Mail], None, 10)
        .unwrap();
    assert!(hits.is_empty());
}
