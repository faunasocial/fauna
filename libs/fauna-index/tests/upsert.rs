//! `Index::add_doc` upsert behavior — re-adding the same content_id replaces
//! the prior doc rather than duplicating it.

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

#[test]
fn re_adding_same_content_id_replaces_prior_doc() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(doc(b"id-1", "first version", 1_000)).unwrap();
    idx.commit().unwrap();

    // Confirm the first version is queryable.
    let hits = idx.query("first", &[ContentKind::Mail], None, 10).unwrap();
    assert_eq!(hits.len(), 1, "first version present");

    // Re-add with the same content_id but different body.
    idx.add_doc(doc(b"id-1", "second version", 2_000)).unwrap();
    idx.commit().unwrap();

    // The old text is no longer queryable.
    let hits_old = idx.query("first", &[ContentKind::Mail], None, 10).unwrap();
    assert!(hits_old.is_empty(), "old version was deleted by upsert");

    // The new text is queryable, and there is exactly one doc with this id.
    let hits_new = idx.query("second", &[ContentKind::Mail], None, 10).unwrap();
    assert_eq!(hits_new.len(), 1, "new version present");
    assert_eq!(hits_new[0].content_id, ContentId(b"id-1".to_vec()));
    assert_eq!(hits_new[0].timestamp_ns, 2_000);
}

#[test]
fn distinct_content_ids_coexist() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(doc(b"id-1", "alpha bravo", 1_000)).unwrap();
    idx.add_doc(doc(b"id-2", "alpha charlie", 2_000)).unwrap();
    idx.commit().unwrap();

    let hits = idx.query("alpha", &[ContentKind::Mail], None, 10).unwrap();
    assert_eq!(hits.len(), 2, "both distinct ids should hit");
}

#[test]
fn upsert_is_idempotent_under_repeated_replay() {
    let mut idx = Index::create_in_ram().unwrap();
    for _ in 0..5 {
        idx.add_doc(doc(b"id-1", "same body each time", 1_000))
            .unwrap();
    }
    idx.commit().unwrap();
    let hits = idx.query("body", &[ContentKind::Mail], None, 10).unwrap();
    assert_eq!(hits.len(), 1, "5 replays must collapse to 1 doc");
}
