//! Smoke test that the Linux desktop build links and uses `fauna_index`.
//!
//! `apps/fauna-linux` does not ship a public library — the binary entry is
//! `src/main.rs`. We expose this module through `mod content_index_smoke_test;`
//! in `main.rs` (compiled out of release with `#[cfg(test)]`) so
//! `cargo test -p fauna-linux` exercises the dep.
//!
//! No production call site yet; Plan 5 wires real ingest paths.

#[cfg(test)]
mod tests {
    use fauna_index::{ContentId, ContentKind, FieldKind, Index, IndexedDoc, IndexedField};

    #[test]
    fn add_then_query_round_trip() {
        let mut idx = Index::create_in_ram().unwrap();
        idx.add_doc(IndexedDoc {
            kind: ContentKind::Mail,
            content_id: ContentId(b"linux-1".to_vec()),
            timestamp_ns: 1_000,
            sender_actor_id: None,
            secondary_id: None,
            fields: vec![IndexedField {
                kind: FieldKind::Body,
                text: "hello from linux".to_string(),
            }],
        })
        .unwrap();
        idx.commit().unwrap();

        let hits = idx.query("linux", &[ContentKind::Mail], None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content_id.0, b"linux-1".to_vec());
    }
}
