//! End-to-end round trip exercising the public API only.

use fauna_cbor::Cid;
use fauna_segment_store::{
    FramedSegmentStore, KindManifest, PinSet, SegmentStats, compact, pick_compaction_inputs,
};
use std::collections::HashSet;
use tempfile::TempDir;

/// Helper: deterministic envelope bytes for index `i` (so the test's
/// expected CIDs are reproducible across reruns).
fn body_for(i: u32) -> Vec<u8> {
    format!("body-{i:03}").into_bytes()
}

#[test]
fn round_trip_across_buckets_then_compact() {
    let tmp = TempDir::new().expect("tmp");
    let mut store =
        FramedSegmentStore::new(tmp.path().join("st"), "mail", [3u8; 32]).expect("new store");
    let mut manifest = KindManifest::empty();

    // Bucket 2026-05: 4 records.
    let seg1 = manifest.append_segment();
    let mut bucket1_cids = Vec::new();
    for i in 0..4u32 {
        let body = body_for(i);
        let cid = Cid::of_dag_cbor(&body);
        bucket1_cids.push(cid);
        let seg_actual = store
            .append("2026-05", seg1, cid, &body, b"{}")
            .expect("append");
        assert_eq!(seg_actual, seg1);
    }

    // Bucket 2026-06: 2 records → triggers rotation.
    let seg2 = manifest.append_segment();
    let mut bucket2_cids = Vec::new();
    for i in 4..6u32 {
        let body = body_for(i);
        let cid = Cid::of_dag_cbor(&body);
        bucket2_cids.push(cid);
        store
            .append("2026-06", seg2, cid, &body, b"{}")
            .expect("append");
    }
    store.finalize_open().expect("finalize");

    // Verify both segments readable.
    let s1 = store.open_segment(seg1).expect("open 1");
    assert_eq!(s1.header.record_count, 4);
    let s2 = store.open_segment(seg2).expect("open 2");
    assert_eq!(s2.header.record_count, 2);

    // Simulate tombstones: msg-001 and msg-002 (in seg1).
    let tombstoned: HashSet<Cid> = [bucket1_cids[1], bucket1_cids[2]].iter().copied().collect();

    // pick_compaction_inputs decides seg1 is eligible.
    let stats = vec![SegmentStats {
        segment_id: seg1,
        record_count: 4,
        tombstone_count: 2,
    }];
    let plan = pick_compaction_inputs(&stats, "2026-05", 0.25, &PinSet::new()).expect("plan");
    assert_eq!(plan.inputs, vec![seg1]);

    // Compact.
    let new_seg = manifest.append_segment();
    let is_alive = |cid: &Cid| !tombstoned.contains(cid);
    let returned = compact(&mut store, plan, new_seg, is_alive).expect("compact");
    assert_eq!(returned, Some(new_seg));
    manifest.tombstone_segment(seg1);

    // Verify final state.
    assert!(manifest.live_segments.contains(&seg2));
    assert!(manifest.live_segments.contains(&new_seg));
    assert!(!manifest.live_segments.contains(&seg1));

    let s_new = store.open_segment(new_seg).expect("open new");
    assert_eq!(s_new.header.record_count, 2);
    let cids: HashSet<Cid> = s_new.iter_records().map(|r| r.cid).collect();
    assert!(cids.contains(&bucket1_cids[0]));
    assert!(cids.contains(&bucket1_cids[3]));
}

// --- Additional CARv2-era round trips per Task 3.2 acceptance ---

#[test]
fn empty_segment_create_finalize_open() {
    let tmp = TempDir::new().expect("tmp");
    let mut store = FramedSegmentStore::new(tmp.path().join("st"), "mail", [1u8; 32]).expect("new");

    // Zero appends, then finalize_open → no segment opened, returns None.
    assert_eq!(store.finalize_open().expect("finalize empty"), None);
    // No segment file exists yet.
    assert!(!store.segment_path(1).exists());
}

#[test]
fn single_record_round_trip() {
    let tmp = TempDir::new().expect("tmp");
    let mut store = FramedSegmentStore::new(tmp.path().join("st"), "mail", [2u8; 32]).expect("new");
    let body = b"only-record";
    let cid = Cid::of_dag_cbor(body);
    store
        .append("2026-05", 1, cid, body, b"floor")
        .expect("append");
    store.finalize_open().expect("finalize");

    let seg = store.open_segment(1).expect("open");
    assert_eq!(seg.header.record_count, 1);
    let read = seg.read_record(&cid).expect("read");
    assert_eq!(read.as_deref(), Some(body.as_slice()));
}

#[test]
fn many_records_round_trip() {
    let tmp = TempDir::new().expect("tmp");
    let mut store = FramedSegmentStore::new(tmp.path().join("st"), "mail", [4u8; 32]).expect("new");

    const N: u32 = 100;
    let mut expected: Vec<(Cid, Vec<u8>)> = Vec::with_capacity(N as usize);
    for i in 0..N {
        let body = format!("record-{i:04}").into_bytes();
        let cid = Cid::of_dag_cbor(&body);
        expected.push((cid, body.clone()));
        store.append("2026-05", 1, cid, &body, b"").expect("append");
    }
    store.finalize_open().expect("finalize");

    let seg = store.open_segment(1).expect("open");
    assert_eq!(seg.header.record_count, N);

    // iter_records preserves append order.
    let iter_cids: Vec<Cid> = seg.iter_records().map(|r| r.cid).collect();
    let expected_cids: Vec<Cid> = expected.iter().map(|(c, _)| *c).collect();
    assert_eq!(iter_cids, expected_cids, "iter must preserve append order");

    // Every record retrievable by CID.
    for (cid, body) in &expected {
        let got = seg.read_record(cid).expect("read");
        assert_eq!(got.as_deref(), Some(body.as_slice()));
    }
}

#[test]
fn read_unknown_cid_returns_ok_none() {
    let tmp = TempDir::new().expect("tmp");
    let mut store = FramedSegmentStore::new(tmp.path().join("st"), "mail", [5u8; 32]).expect("new");
    store
        .append("2026-05", 1, Cid::of_dag_cbor(b"present"), b"present", b"")
        .expect("append");
    store.finalize_open().expect("finalize");

    let seg = store.open_segment(1).expect("open");
    let missing = Cid::of_dag_cbor(b"absent");
    let res = seg.read_record(&missing).expect("must be Ok(None)");
    assert!(res.is_none(), "unknown cid must return Ok(None), not error");
}
