//! What `add_doc`'s upsert does and does **not** reach once a segment is
//! sealed — the substrate question every *mutable* content kind rests on.
//!
//! `upsert.rs` pins the within-index guarantee: re-adding a content id replaces
//! the prior doc, so the old text stops matching. That guarantee is scoped to
//! one writer. A builder that has already published segment N cannot issue a
//! `delete_term` into it — the bytes are sealed, and `open_multi_segment` copies
//! each input in as-is. So for a kind whose content *changes* (drafts), the
//! superseded version stays queryable from its old segment, and a query for the
//! old text hits it with nothing to dedup it against.
//!
//! These tests state that limit as a fact rather than leaving each later reader
//! to rediscover it: an arm over a mutable kind must supersede at a layer the
//! index does not provide by itself.

use fauna_index::{ContentId, ContentKind, FieldKind, Index, IndexedDoc, IndexedField};

fn doc(id: &[u8], body: &str, ts: i64) -> IndexedDoc {
    IndexedDoc {
        kind: ContentKind::Draft,
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

/// One sealed segment holding exactly one doc — a flush, in miniature.
fn sealed(id: &[u8], body: &str, ts: i64) -> Vec<u8> {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(doc(id, body, ts)).unwrap();
    idx.commit().unwrap();
    idx.seal_to_bytes().unwrap()
}

/// The load-bearing fact: a later segment does **not** supersede an earlier
/// one's copy of the same content id.
#[test]
fn a_later_segment_does_not_supersede_an_earlier_segments_copy() {
    let first = sealed(b"draft-1", "hello world", 1_000);
    let second = sealed(b"draft-1", "goodbye world", 2_000);

    let combined = Index::open_multi_segment(&[first, second]).unwrap();

    // The new text is findable — that much a naive re-stage does buy.
    let hits_new = combined
        .query("goodbye", &[ContentKind::Draft], None, 10)
        .unwrap();
    assert_eq!(hits_new.len(), 1, "the current version is queryable");

    // ...and so is the OLD text, from the segment that was already sealed when
    // the replacement was written. This is the failure a mutable kind must
    // answer for: the user searches a phrase they deleted and still finds the
    // draft.
    let hits_old = combined
        .query("hello", &[ContentKind::Draft], None, 10)
        .unwrap();
    assert_eq!(
        hits_old.len(),
        1,
        "the superseded version stays queryable across a segment boundary — \
         `delete_term` never reached the sealed segment"
    );

    // And both carry the same content id, so nothing downstream can tell them
    // apart by identity alone: the stale hit is not a *duplicate* to be deduped
    // away, it is the sole answer to that query.
    assert_eq!(hits_old[0].content_id, ContentId(b"draft-1".to_vec()));
    assert_eq!(hits_new[0].content_id, ContentId(b"draft-1".to_vec()));
}

/// Compaction does not clean it up either, so "the fold will fix it" is not an
/// answer an arm may lean on.
#[test]
fn merging_the_two_segments_keeps_both_versions() {
    let first = sealed(b"draft-1", "hello world", 1_000);
    let second = sealed(b"draft-1", "goodbye world", 2_000);

    let merged = Index::merge_segments(&[first, second]).unwrap();
    let idx = Index::open_multi_segment(&[merged]).unwrap();

    let hits_old = idx.query("hello", &[ContentKind::Draft], None, 10).unwrap();
    assert_eq!(
        hits_old.len(),
        1,
        "a fold concatenates its inputs; neither input carried a delete for the \
         other, so the superseded doc survives compaction"
    );

    // Two live docs under one content id. `content_ids` reports one entry per
    // live doc (its own docs say callers collect into a set), so the duplication
    // is *visible* here but collapses the moment a resumed builder seeds its
    // guard — which is why re-staging alone can never repair it.
    let all: Vec<_> = idx.content_ids().unwrap();
    assert_eq!(
        all.len(),
        2,
        "both live docs are reported, one per copy of the id"
    );
}

/// The other side of the limit, and the affordance every arm over a replaceable
/// content id rests on: a delete issued **on the view that copied the segment
/// in** *does* reach it.
///
/// The two tests above are about a writer that has already published segment N
/// and is writing segment N+1 — it cannot reach backwards. `rebuild_with` is
/// the opposite construction: the old segment is an *input* to the writer doing
/// the replacing, so `add_doc`'s own `delete_term` lands on it. That is why
/// retirement is expressible at all without a whole-corpus republish, and why
/// "an arm over a mutable kind must supersede at a layer the index does not
/// provide by itself" means *this* layer rather than an impossibility.
#[test]
fn a_rebuild_over_the_old_segment_retires_the_superseded_copy() {
    let first = sealed(b"draft-1", "hello world", 1_000);

    let rebuilt =
        Index::rebuild_with(&[first], vec![doc(b"draft-1", "goodbye world", 2_000)]).unwrap();
    let idx = Index::open_multi_segment(&[rebuilt]).unwrap();

    let hits_new = idx
        .query("goodbye", &[ContentKind::Draft], None, 10)
        .unwrap();
    assert_eq!(hits_new.len(), 1, "the replacement is queryable");

    let hits_old = idx.query("hello", &[ContentKind::Draft], None, 10).unwrap();
    assert!(
        hits_old.is_empty(),
        "and the superseded text stops matching — the delete reached the \
         copied-in segment, which is exactly what a later segment's write \
         cannot do"
    );

    assert_eq!(
        idx.content_ids().unwrap().len(),
        1,
        "one live doc under the id, not the two a fold leaves behind"
    );
}

/// The rewrite touches only the doc it replaces: a segment is rebuilt, so every
/// *other* doc it held has to come through unharmed.
#[test]
fn a_rebuild_keeps_the_segments_other_docs() {
    let mut idx = Index::create_in_ram().unwrap();
    idx.add_doc(doc(b"draft-1", "hello world", 1_000)).unwrap();
    idx.add_doc(doc(b"draft-2", "unrelated note", 1_001))
        .unwrap();
    idx.commit().unwrap();
    let first = idx.seal_to_bytes().unwrap();

    let rebuilt =
        Index::rebuild_with(&[first], vec![doc(b"draft-1", "goodbye world", 2_000)]).unwrap();
    let out = Index::open_multi_segment(&[rebuilt]).unwrap();

    assert_eq!(
        out.query("unrelated", &[ContentKind::Draft], None, 10)
            .unwrap()
            .len(),
        1,
        "the bystander doc survives the rewrite"
    );
    assert!(
        out.query("hello", &[ContentKind::Draft], None, 10)
            .unwrap()
            .is_empty(),
        "and only the replaced doc is retired"
    );
    assert_eq!(out.content_ids().unwrap().len(), 2);
}

/// With no replacements it is exactly `merge_segments`, so a caller can reach
/// for one entry point without branching on whether anything is being replaced.
#[test]
fn a_rebuild_with_no_replacements_is_a_plain_fold() {
    let first = sealed(b"draft-1", "hello world", 1_000);
    let second = sealed(b"draft-2", "second segment", 2_000);

    let rebuilt = Index::rebuild_with(&[first, second], Vec::new()).unwrap();
    let idx = Index::open_multi_segment(&[rebuilt]).unwrap();

    assert_eq!(idx.content_ids().unwrap().len(), 2, "both docs survive");
    assert_eq!(
        idx.query("second", &[ContentKind::Draft], None, 10)
            .unwrap()
            .len(),
        1
    );
}
