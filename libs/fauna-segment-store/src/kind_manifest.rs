//! `KindManifest` — per-kind segment-id tracking inside an outer manifest.
//!
//! Lifted verbatim from `fauna-index` (originally
//! `libs/fauna-index/src/manifest.rs:23-44,127-160`). The outer manifest
//! (`IndexManifest` in fauna-index; the unified [`crate::Manifest`] for the
//! segment store) owns a list of `(KindEnum, KindManifest)` entries.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindManifest {
    /// Monotonic counter; the next segment file written for this kind
    /// takes `next_seg_id`. Never decreases.
    pub next_seg_id: u32,
    /// Segment ids currently active. Sorted ascending. Compaction
    /// replaces a sub-range here with a single new id.
    pub live_segments: Vec<u32>,
    /// Segment ids superseded by compaction, awaiting retention-window
    /// GC. Querying ignores these.
    pub tombstoned_segments: Vec<u32>,
}

impl Default for KindManifest {
    fn default() -> Self {
        Self::empty()
    }
}

impl KindManifest {
    pub fn empty() -> Self {
        Self {
            next_seg_id: 1,
            live_segments: Vec::new(),
            tombstoned_segments: Vec::new(),
        }
    }

    /// Reserve the next segment id, append it to `live_segments`,
    /// return the id.
    pub fn append_segment(&mut self) -> u32 {
        let id = self.next_seg_id;
        self.next_seg_id = self
            .next_seg_id
            .checked_add(1)
            .expect("u32 segment counter exhausted");
        self.live_segments.push(id);
        id
    }

    /// Move `seg_id` from `live_segments` to `tombstoned_segments`.
    /// Returns `true` if the move happened.
    pub fn tombstone_segment(&mut self, seg_id: u32) -> bool {
        let Some(pos) = self.live_segments.iter().position(|&id| id == seg_id) else {
            return false;
        };
        self.live_segments.remove(pos);
        let insert_at = self.tombstoned_segments.partition_point(|&id| id < seg_id);
        self.tombstoned_segments.insert(insert_at, seg_id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_starts_at_one() {
        let m = KindManifest::empty();
        assert_eq!(m.next_seg_id, 1);
        assert!(m.live_segments.is_empty());
        assert!(m.tombstoned_segments.is_empty());
    }

    #[test]
    fn append_returns_monotonic_ids() {
        let mut m = KindManifest::empty();
        assert_eq!(m.append_segment(), 1);
        assert_eq!(m.append_segment(), 2);
        assert_eq!(m.next_seg_id, 3);
        assert_eq!(m.live_segments, vec![1, 2]);
    }

    #[test]
    fn tombstone_moves_id_and_returns_true() {
        let mut m = KindManifest::empty();
        m.append_segment(); // 1
        m.append_segment(); // 2
        m.append_segment(); // 3
        assert!(m.tombstone_segment(2));
        assert_eq!(m.live_segments, vec![1, 3]);
        assert_eq!(m.tombstoned_segments, vec![2]);
    }

    #[test]
    fn tombstone_unknown_returns_false() {
        let mut m = KindManifest::empty();
        m.append_segment();
        assert!(!m.tombstone_segment(99));
    }

    #[test]
    fn dag_cbor_round_trips() {
        let mut m = KindManifest::empty();
        m.append_segment();
        m.append_segment();
        m.tombstone_segment(1);
        let bytes = fauna_cbor::encode_canonical(&m).expect("encode");
        let m2: KindManifest = fauna_cbor::decode_strict(&bytes).expect("decode");
        assert_eq!(m, m2);
    }
}
