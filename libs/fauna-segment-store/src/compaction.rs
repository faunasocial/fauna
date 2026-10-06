//! Compaction planning + execution.
//!
//! Tombstone state lives in the *caller's* SQLite mirror (see spec D3) —
//! this crate doesn't track it. `pick_compaction_inputs` takes a slice
//! of (segment_id, record_count, tombstone_count) and a threshold;
//! `compact` (Task 8) takes an is_alive callback and rewrites live records
//! into a fresh segment.

use crate::PinSet;
use fauna_cbor::Cid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPlan {
    pub inputs: Vec<u32>,
    pub bucket: String,
}

/// Per-segment stat fed in by the caller from its SQLite mirror.
#[derive(Debug, Clone, Copy)]
pub struct SegmentStats {
    pub segment_id: u32,
    pub record_count: u32,
    pub tombstone_count: u32,
}

/// Pick segments whose tombstone fraction is at or above
/// `min_tombstone_fraction` and that are not pinned. All chosen
/// segments must share a bucket (rotation invariant — compaction
/// rewrites within one month bucket at a time).
///
/// Returns None if no segment qualifies. The caller is expected to
/// iterate per-bucket if it wants to drive multi-bucket compaction.
pub fn pick_compaction_inputs(
    segments_in_bucket: &[SegmentStats],
    bucket: &str,
    min_tombstone_fraction: f32,
    pin_set: &PinSet,
) -> Option<CompactionPlan> {
    let mut inputs: Vec<u32> = segments_in_bucket
        .iter()
        .filter(|s| {
            if pin_set.contains(s.segment_id) {
                return false;
            }
            if s.record_count == 0 {
                return false;
            }
            let frac = s.tombstone_count as f32 / s.record_count as f32;
            frac >= min_tombstone_fraction
        })
        .map(|s| s.segment_id)
        .collect();
    if inputs.is_empty() {
        return None;
    }
    inputs.sort();
    Some(CompactionPlan {
        inputs,
        bucket: bucket.to_string(),
    })
}

/// Execute a compaction plan: rewrite live records from the input
/// segments into a single new segment. Caller passes:
///
/// - `store` — the segment store backing the kind.
/// - `plan` — input segment ids + bucket (must match across inputs).
/// - `new_segment_id` — typically the outer manifest's
///   `next_seg_id` after `append_segment`.
/// - `is_alive` — closure returning true if a record is still live;
///   the caller looks this up in its SQLite tombstone mirror.
///
/// Returns `Ok(Some(new_segment_id))` on success when at least one
/// record survived. The new segment file is written; caller commits
/// the plan by tombstoning each `plan.inputs[i]` and adding
/// `new_segment_id` to live_segments.
///
/// Returns `Ok(None)` when every record in the input segments was
/// tombstoned. No new segment was written; the supplied
/// `new_segment_id` is unused. Caller should still tombstone each
/// `plan.inputs[i]` but should NOT add `new_segment_id` to
/// live_segments (no file was created).
///
/// On error, any partially-written open segment is dropped (a
/// best-effort `finalize_open` is performed) so the store is in a
/// consistent state to retry. The new segment file may exist on disk
/// but is not referenced by the manifest until the caller commits
/// the plan.
pub fn compact<F>(
    store: &mut crate::FramedSegmentStore,
    plan: CompactionPlan,
    new_segment_id: u32,
    is_alive: F,
) -> Result<Option<u32>, crate::SegmentStoreError>
where
    F: Fn(&Cid) -> bool,
{
    // Snapshot live records from each input before opening the writer.
    #[derive(Debug)]
    struct LiveRecord {
        cid: Cid,
        bytes: Vec<u8>,
        floor_metadata: Vec<u8>,
    }
    let mut survivors: Vec<LiveRecord> = Vec::new();
    for &input_id in &plan.inputs {
        let input = store.open_segment(input_id)?;
        // Two-pass: collect references to live entries, then bulk-read
        // their payloads with a single file handle (one open per input).
        let live_entries: Vec<&crate::RecordEntry> =
            input.iter_records().filter(|e| is_alive(&e.cid)).collect();
        let bytes_for = input.read_records_bulk(&live_entries)?;
        for (entry, bytes_opt) in live_entries.iter().zip(bytes_for) {
            let bytes = bytes_opt.ok_or_else(|| {
                crate::SegmentStoreError::InvalidSegment(format!(
                    "compaction: record {} present in sidecar but absent from carv2 data \
                     section (segment {} corrupt?)",
                    entry.cid, input_id
                ))
            })?;
            survivors.push(LiveRecord {
                cid: entry.cid,
                bytes,
                floor_metadata: entry.floor_metadata.clone(),
            });
        }
    }

    // No survivors — nothing to write; caller tombstones inputs only.
    if survivors.is_empty() {
        return Ok(None);
    }

    // Write a new segment. We call `append` per record — it opens the
    // segment lazily on the first call. If anything errors mid-write,
    // perform a best-effort finalize_open() to drop the partial segment
    // so the store is not left wedged with a non-None self.open.
    let write_result = (|| -> Result<(), crate::SegmentStoreError> {
        for r in &survivors {
            store.append(
                &plan.bucket,
                new_segment_id,
                r.cid,
                &r.bytes,
                &r.floor_metadata,
            )?;
        }
        store.finalize_open()?;
        Ok(())
    })();
    if write_result.is_err() {
        // Best-effort cleanup: drop the partial segment so the store
        // isn't wedged. Errors here are ignored — the original write
        // error is what the caller cares about.
        let _ = store.finalize_open();
    }
    write_result?;

    Ok(Some(new_segment_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KindManifest;

    fn pin_set(live: &[u32]) -> PinSet {
        let mut p = PinSet::new();
        let mut m = KindManifest::empty();
        for _ in 0..*live.iter().max().unwrap_or(&0) {
            m.append_segment();
        }
        // Keep only those in `live` as live; tombstone the rest. Since
        // PinSet covers live+tombstoned, this just exercises the union.
        let live_set: std::collections::BTreeSet<u32> = live.iter().copied().collect();
        let to_tombstone: Vec<u32> = m
            .live_segments
            .iter()
            .copied()
            .filter(|id| !live_set.contains(id))
            .collect();
        for id in to_tombstone {
            m.tombstone_segment(id);
        }
        p.extend_from_manifest(&m);
        p
    }

    #[test]
    fn skips_below_threshold() {
        let segs = vec![SegmentStats {
            segment_id: 1,
            record_count: 100,
            tombstone_count: 10, // 10%
        }];
        assert_eq!(
            pick_compaction_inputs(&segs, "2026-05", 0.25, &PinSet::new()),
            None
        );
    }

    #[test]
    fn picks_at_threshold() {
        let segs = vec![SegmentStats {
            segment_id: 1,
            record_count: 100,
            tombstone_count: 25, // exactly 25%
        }];
        let plan = pick_compaction_inputs(&segs, "2026-05", 0.25, &PinSet::new()).expect("plan");
        assert_eq!(plan.inputs, vec![1]);
        assert_eq!(plan.bucket, "2026-05");
    }

    #[test]
    fn skips_pinned() {
        let segs = vec![SegmentStats {
            segment_id: 1,
            record_count: 100,
            tombstone_count: 80,
        }];
        let pinned = pin_set(&[1]);
        assert_eq!(
            pick_compaction_inputs(&segs, "2026-05", 0.25, &pinned),
            None,
            "pinned segment must not be selected even with 80% tombstones"
        );
    }

    #[test]
    fn picks_multiple_and_sorts() {
        let segs = vec![
            SegmentStats {
                segment_id: 3,
                record_count: 10,
                tombstone_count: 5,
            },
            SegmentStats {
                segment_id: 1,
                record_count: 10,
                tombstone_count: 6,
            },
            SegmentStats {
                segment_id: 2,
                record_count: 10,
                tombstone_count: 1, // below threshold
            },
        ];
        let plan = pick_compaction_inputs(&segs, "2026-05", 0.25, &PinSet::new()).expect("plan");
        assert_eq!(plan.inputs, vec![1, 3]);
    }

    use tempfile::TempDir;

    #[test]
    fn compact_drops_tombstoned_records() {
        use crate::FramedSegmentStore;
        let tmp = TempDir::new().expect("tmp");
        let mut s = FramedSegmentStore::new(tmp.path().join("st"), "mail", [0u8; 32]).expect("new");
        // Seg 1: 3 records, alpha tombstoned.
        let bodies: [&[u8]; 5] = [b"alpha", b"beta", b"gamma", b"delta", b"epsilon"];
        let cids: Vec<Cid> = bodies.iter().map(|b| Cid::of_dag_cbor(b)).collect();
        s.append("2026-05", 1, cids[0], bodies[0], b"").expect("a");
        s.append("2026-05", 1, cids[1], bodies[1], b"").expect("b");
        s.append("2026-05", 1, cids[2], bodies[2], b"").expect("c");
        s.finalize_open().expect("finalize 1");
        // Seg 2: 2 records, delta tombstoned.
        s.append("2026-05", 2, cids[3], bodies[3], b"").expect("d");
        s.append("2026-05", 2, cids[4], bodies[4], b"").expect("e");
        s.finalize_open().expect("finalize 2");

        let tombstoned: std::collections::HashSet<Cid> =
            [cids[0], cids[3]].iter().copied().collect();
        let is_alive = |cid: &Cid| !tombstoned.contains(cid);

        let plan = CompactionPlan {
            inputs: vec![1, 2],
            bucket: "2026-05".to_string(),
        };
        let new_seg_id = compact(&mut s, plan, 3, is_alive).expect("compact");
        assert_eq!(new_seg_id, Some(3));

        let seg = s.open_segment(3).expect("open new");
        assert_eq!(seg.header.record_count, 3);
        let ids: std::collections::HashSet<Cid> = seg.iter_records().map(|r| r.cid).collect();
        assert!(ids.contains(&cids[1]));
        assert!(ids.contains(&cids[2]));
        assert!(ids.contains(&cids[4]));
        assert!(!ids.contains(&cids[0]));
        assert!(!ids.contains(&cids[3]));
    }

    #[test]
    fn compact_all_tombstoned_returns_none() {
        use crate::FramedSegmentStore;
        let tmp = TempDir::new().expect("tmp");
        let mut s = FramedSegmentStore::new(tmp.path().join("st"), "mail", [0u8; 32]).expect("new");
        s.append("2026-05", 1, Cid::of_dag_cbor(b"a"), b"a", b"")
            .expect("a");
        s.append("2026-05", 1, Cid::of_dag_cbor(b"b"), b"b", b"")
            .expect("b");
        s.finalize_open().expect("finalize");

        // Everything tombstoned.
        let is_alive = |_: &Cid| false;

        let plan = CompactionPlan {
            inputs: vec![1],
            bucket: "2026-05".to_string(),
        };
        let result = compact(&mut s, plan, 2, is_alive).expect("compact");
        assert_eq!(result, None, "no survivors → no new segment");
        // Segment 2 file should not exist on disk.
        assert!(!s.segment_path(2).exists());
    }
}
