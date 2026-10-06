//! `PinSet` — the union of segment IDs that `pick_compaction_inputs`
//! must filter out of its candidate set.
//!
//! The set's composition is the caller's choice; the crate exposes
//! only the union primitive (`extend_from_manifest`). Typical
//! composition for a compaction worker:
//!
//! - The active manifest's `tombstoned_segments` (segments compaction
//!   already rewrote in an earlier pass; re-feeding them would be
//!   wasteful).
//! - Every pinned snapshot's `live_segments` + `tombstoned_segments`
//!   (snapshots pin the bytes of everything their manifest references).
//!
//! The active manifest's `live_segments` are intentionally *not* in
//! the pin set — those are the compaction candidates. After
//! compaction they move from `live` → `tombstoned` and become pinned
//! via the active manifest's tombstoned list for the 14 d retention
//! window before fauna-sync GC reclaims their chunks.
//!
//! (The broader "GC pin set" (design tracked internally, § D8) — the set used
//! to gate *physical* deletion via fauna-sync GC — is a separate
//! concept that *does* include the active manifest's live segments.
//! The compaction-input-eligibility set here is strictly narrower.)

use crate::KindManifest;
use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
pub struct PinSet {
    pinned: BTreeSet<u32>,
}

impl PinSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend_from_manifest(&mut self, manifest: &KindManifest) {
        self.pinned.extend(&manifest.live_segments);
        self.pinned.extend(&manifest.tombstoned_segments);
    }

    pub fn contains(&self, segment_id: u32) -> bool {
        self.pinned.contains(&segment_id)
    }

    pub fn len(&self) -> usize {
        self.pinned.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_contains_nothing() {
        let p = PinSet::new();
        assert!(p.is_empty());
        assert!(!p.contains(1));
    }

    #[test]
    fn extend_picks_up_live_and_tombstoned() {
        let mut m = KindManifest::empty();
        m.append_segment(); // 1
        m.append_segment(); // 2
        m.append_segment(); // 3
        m.tombstone_segment(2);

        let mut p = PinSet::new();
        p.extend_from_manifest(&m);
        assert_eq!(p.len(), 3);
        for id in [1, 2, 3] {
            assert!(p.contains(id), "should contain {id}");
        }
        assert!(!p.contains(4));
    }

    #[test]
    fn extend_from_multiple_manifests_unions() {
        let mut active = KindManifest::empty();
        active.append_segment(); // 1
        active.append_segment(); // 2

        let mut pinned_snap = KindManifest::empty();
        pinned_snap.append_segment(); // 1 (independent counter)
        pinned_snap.append_segment(); // 2

        let mut p = PinSet::new();
        p.extend_from_manifest(&active);
        p.extend_from_manifest(&pinned_snap);
        // BTreeSet dedupes; ids overlap.
        assert_eq!(p.len(), 2);
    }
}
