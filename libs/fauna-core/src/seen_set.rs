//! The fleet seen-set's per-scope value shape and its **join** — the plane's
//! first union-CRDT kind (`fauna.state.seen-set`, registered in
//! `fauna_protocol::merge_policy`).
//!
//! Authority: `docs/goal/architecture/account-replica-posture.md` § The
//! replica boundary (R1 (account-data-plane.md § The ratified decisions) — the seen-set materializes the boundary; grow-only as a
//! *set*), its A4 amendment (compactable as *bytes*: references coalesce
//! into per-scope watermarks once the referenced items are locally indexed),
//! and transition (4)'s *The budget* ruling (bounded by construction, § The
//! budget below). Product-level read markers are **not** this structure —
//! they register as their own class-2 kind with its own retention policy.
//!
//! # The representation (the W2.5 (account-data-plane.md § Workstreams) item-2 ruling)
//!
//! One entry per **referenced scope**: the entry's logical key is the scope
//! whose observations it records, and its value is a [`SeenScopeSet`]. A
//! reference is recorded as the observed item's **scope-feed coordinate**
//! `(writer, seq)` — the same pair the frontier plane accounts, globally
//! unique, immutable once published, and durable at the nest (compat
//! obligation 4: feed history is never deleted). Recording coordinates rather
//! than CIDs / kind-scoped keys is what makes A4's compaction a **pure join
//! law** instead of a local-index-dependent transform: a per-writer watermark
//! (`seq_at_or_below_is_seen`) covers a coordinate by arithmetic alone, so two
//! replicas converge on identical canonical bytes without consulting any
//! index. Resolving a coordinate back to the item it denotes (CID or
//! kind-scoped key) is the local record index's job, at the projection layer.
//! One item observed by two writers yields two coordinates; set semantics
//! absorb the redundancy ("is X seen?" is "is any coordinate of X a member?").
//!
//! # The join, and why elision is inside it
//!
//! `join = pointwise-max watermarks ∪ ref-union, minus watermark-covered
//! refs`. The elision must live **in the join**: if compaction were only a
//! writer-side rewrite, a compacted replica joining an uncompacted peer would
//! resurrect every elided reference from the peer's copy, and the two would
//! oscillate. With elision in the join, raising a watermark shrinks the bytes
//! *convergently* — represented membership never shrinks (grow-only as a
//! set), the encoding does (compactable as bytes).
//!
//! # The budget — bounded by construction, inside the join too
//!
//! Elision alone bounds nothing for a **browse** scope: no producer ever
//! earns one a watermark (a render earns one item, never a prefix), so its
//! entry grew with every non-contiguous read until it outgrew the plane's
//! per-entry cap — at the writer door on one device, and at the walk's merge
//! door for two, where a union of two under-cap entries sealed past the cap
//! and the merged row no nest accepts stalled `publish_pending`. So every
//! value this module produces is **within [`SEEN_SET_ELEMENT_BUDGET`]**:
//! each writer named in the value gets an equal share of the budget for
//! itemized refs (the budget minus one watermark slot per writer, split
//! evenly), and past its share a writer's **oldest** refs fold into its
//! watermark ([`SeenScopeSet::bound`]). The fold is the one sanctioned
//! **upward** over-approximation — the folded prefix counts its unobserved
//! gaps as seen — chosen because the alternatives lose more: refusing the
//! merge breaks grow-only-union convergence and leaves the producer dark past
//! the cap (an *under*-approximated boundary), and sharding a scope over
//! several entries is exactly the forever-growing itemized log A4 rules out.
//!
//! The fold is applied by every mutator and by `join` as a **closure
//! operator** — extensive (membership only grows), monotone (a value below
//! another folds to a value below the other's fold) and idempotent — which is
//! what keeps the bounded merge a *join*: `bound(a ∪ b)` is commutative,
//! associative and idempotent whether or not the budget binds, so replicas
//! that merge in different orders still meet on identical bytes. A value a
//! hostile or buggy writer merged unbounded is simply below its own fold, and converges
//! with a bounded peer on the next exchange. The one axis the budget leaves
//! open is the writer population of a scope (one watermark per writer, never
//! dropped); it is bounded elsewhere, and the walk's merge door sizes every
//! merged value as the backstop.
//!
//! # Evolution posture
//!
//! The value decodes with `deny_unknown_fields`, deliberately: a CRDT kind
//! re-encodes what it decodes, so a tolerant reader meeting a newer field
//! would silently *strip* it from its merged output — worse than the loud
//! `unmergeable`/`BadValue` skip the walk already reports. A richer seen-set
//! shape therefore registers as a sibling kind rather than growing this one.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The per-scope **element budget**: the most elements — watermarks plus
/// itemized refs — one normal-form [`SeenScopeSet`] holds. Every mutator and
/// the join fold anything past it into watermarks ([`SeenScopeSet::bound`]),
/// which is what bounds a browse scope's entry by construction (module docs
/// § The budget).
///
/// Sized against the plane's per-entry byte cap, not the other way round: an
/// element encodes to at most 55 bytes — a two-field map of a u64 seq (9 at
/// its widest) and a 32-byte writer id, a 34-byte CBOR byte string like every
/// fixed-width id (`docs/goal/architecture/serialization.md` § Canonical IPLD
/// dag-cbor, "Fixed-size byte arrays") — so a full budget is about 55 KB of
/// value under the 64 KiB every nest enforces on a sealed entry, with
/// headroom for the envelope and any plausible scope key. The tie is
/// asserted where the cap lives (`fauna_protocol::merge_policy`, the
/// seen-set arm's tests). (The budget was 640 while the writer encoded as a
/// 32-integer array of up to 66 bytes; the byte-string writer of 2026-09-29
/// re-sized it to this figure at the same headroom.)
pub const SEEN_SET_ELEMENT_BUDGET: usize = 1000;

/// A per-writer high-water mark: every item this writer journaled in the
/// entry's scope at or below `seq` is a member of the seen-set.
///
/// A watermark asserts observation of the writer's whole prefix, so only a
/// producer that has actually observed everything at-or-below may raise it
/// (delivery-class scopes, where R1 puts items in-set at delivery, are the
/// natural fit; browse scopes stay itemized until a prefix is truly seen) —
/// with one sanctioned exception, the budget fold (module docs § The budget),
/// which raises a writer's watermark over its oldest itemized refs once they
/// outgrow the writer's share of the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeenWatermark {
    /// The observed scope's writer (32-byte writer id — the feed's
    /// `origin_writer`).
    #[serde(with = "serde_bytes")]
    pub writer: [u8; 32],
    /// The high-water `writer_seq`, inclusive. Always ≥ 1 in normal form (a
    /// zero watermark asserts nothing and is represented by absence).
    pub seq: u64,
}

/// One itemized member: the scope-feed coordinate of an observed item, kept
/// only while it is above its writer's watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeenRef {
    /// The observed scope's writer (the feed's `origin_writer`).
    #[serde(with = "serde_bytes")]
    pub writer: [u8; 32],
    /// The observed row's `writer_seq` (the feed's `origin_seq`).
    pub seq: u64,
}

/// The seen-set for one referenced scope — the value of one
/// `fauna.state.seen-set` entry.
///
/// **Normal form** (what canonical bytes require, maintained by every
/// constructor and by [`Self::join`], asserted by [`Self::is_normal_form`]):
/// `watermarks` strictly ascending by writer with all seqs ≥ 1; `refs`
/// strictly ascending `(writer, seq)` with no member covered by its writer's
/// watermark; and **within the budget** — no writer itemizes more refs than
/// its share ([`Self::within_budget`]). Two values representing the same
/// membership therefore encode to identical bytes, which is what lets the
/// merge seam's byte-equality echo-stop terminate
/// (`fauna_protocol::merge_policy`, the `KeepCurrent` arm).
///
/// Fields are `pub` because the Secret-free pin destructures them
/// exhaustively (`fauna_protocol::secret_free`); mutate through
/// [`Self::insert_ref`] / [`Self::raise_watermark`], which keep normal form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeenScopeSet {
    /// Per-writer prefix coverage, sorted by writer, at most one per writer.
    pub watermarks: Vec<SeenWatermark>,
    /// Members above their writer's watermark, sorted, deduplicated.
    pub refs: Vec<SeenRef>,
}

impl SeenScopeSet {
    /// The empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `(writer, seq)` is a member — covered by a watermark or
    /// itemized.
    pub fn contains(&self, writer: &[u8; 32], seq: u64) -> bool {
        self.watermark_of(writer) >= seq
            || self
                .refs
                .binary_search(&SeenRef {
                    writer: *writer,
                    seq,
                })
                .is_ok()
    }

    /// This writer's watermark, 0 when absent.
    pub fn watermark_of(&self, writer: &[u8; 32]) -> u64 {
        self.watermarks
            .binary_search_by(|w| w.writer.cmp(writer))
            .map(|i| self.watermarks[i].seq)
            .unwrap_or(0)
    }

    /// Record one observation. Returns whether membership grew (`false` when
    /// the coordinate was already represented). Past the writer's share of
    /// the budget the fold applies at once, so the new ref may land in the
    /// watermark rather than the itemized list — a member either way.
    pub fn insert_ref(&mut self, writer: [u8; 32], seq: u64) -> bool {
        if !self.insert_ref_unbounded(writer, seq) {
            return false;
        }
        self.bound();
        true
    }

    /// Raise (never lower) one writer's watermark, eliding the refs it now
    /// covers. Returns whether anything changed.
    ///
    /// The caller asserts the prefix property: everything at-or-below `seq`
    /// for this writer has been observed. This structure cannot check that —
    /// it records membership, not item existence.
    pub fn raise_watermark(&mut self, writer: [u8; 32], seq: u64) -> bool {
        if !self.raise_watermark_unbounded(writer, seq) {
            return false;
        }
        // A watermark for a writer with no refs is a new present writer, which
        // shrinks every other writer's share.
        self.bound();
        true
    }

    /// [`Self::insert_ref`] without the fold — the join's building block, so
    /// a union folds once at the end rather than per element.
    fn insert_ref_unbounded(&mut self, writer: [u8; 32], seq: u64) -> bool {
        if seq == 0 || self.contains(&writer, seq) {
            return false;
        }
        let r = SeenRef { writer, seq };
        let at = self.refs.binary_search(&r).unwrap_err();
        self.refs.insert(at, r);
        true
    }

    /// [`Self::raise_watermark`] without the fold (see
    /// [`Self::insert_ref_unbounded`]).
    fn raise_watermark_unbounded(&mut self, writer: [u8; 32], seq: u64) -> bool {
        if seq == 0 || self.watermark_of(&writer) >= seq {
            return false;
        }
        match self.watermarks.binary_search_by(|w| w.writer.cmp(&writer)) {
            Ok(i) => self.watermarks[i].seq = seq,
            Err(i) => self.watermarks.insert(i, SeenWatermark { writer, seq }),
        }
        self.refs.retain(|r| r.writer != writer || r.seq > seq);
        true
    }

    /// The join: pointwise-max watermarks, ref union, watermark-covered refs
    /// elided, then the budget fold. Commutative, associative, idempotent,
    /// and total — the fold being a closure operator is what keeps the
    /// laws when the budget binds (module docs § The budget) — and it
    /// normalizes, so a non-normal-form input (a hostile or buggy writer's
    /// bytes, merged without the budget) joins into normal form
    /// rather than propagating.
    pub fn join(&self, other: &Self) -> Self {
        let mut out = Self::new();
        // Pointwise max over both watermark lists (raise_watermark keeps the
        // max and the sort order; duplicate writers inside ONE input collapse
        // to their max the same way).
        for w in self.watermarks.iter().chain(&other.watermarks) {
            out.raise_watermark_unbounded(w.writer, w.seq);
        }
        // Union of refs, minus covered ones (insert_ref elides + dedupes).
        for r in self.refs.iter().chain(&other.refs) {
            out.insert_ref_unbounded(r.writer, r.seq);
        }
        out.bound();
        out
    }

    /// The budget fold (module docs § The budget): for every writer itemizing
    /// more refs than its share, raise its watermark to the seq of its
    /// `(share + 1)`-th newest ref, eliding that ref and every older one, so
    /// exactly `share` refs remain. A closure operator over the lattice —
    /// extensive, monotone, idempotent — proven by the tests below.
    ///
    /// Assumes normal form but for the budget (sorted, elided refs), which is
    /// what every caller hands it.
    fn bound(&mut self) {
        let share = Self::per_writer_ref_share(self.present_writers());
        let mut raises = Vec::new();
        let mut start = 0;
        while start < self.refs.len() {
            let writer = self.refs[start].writer;
            let run = self.refs[start..]
                .iter()
                .take_while(|r| r.writer == writer)
                .count();
            if run > share {
                // The refs of one writer are ascending: the (share + 1)-th
                // newest sits `share + 1` from the run's end.
                raises.push((writer, self.refs[start + run - share - 1].seq));
            }
            start += run;
        }
        for (writer, seq) in raises {
            // A raise never adds a writer (the writer already had refs), so
            // the shares computed above stay the shares.
            self.raise_watermark_unbounded(writer, seq);
        }
    }

    /// Distinct writers named anywhere in the value — by a watermark, a ref,
    /// or both. Each holds one watermark slot of the budget.
    fn present_writers(&self) -> usize {
        self.watermarks
            .iter()
            .map(|w| w.writer)
            .chain(self.refs.iter().map(|r| r.writer))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Each present writer's share of the budget for itemized refs: the
    /// budget minus one watermark slot per writer, split evenly. Zero once the
    /// writers alone fill the budget — every ref then folds into its
    /// watermark, and the writer population is the one axis left open.
    fn per_writer_ref_share(writers: usize) -> usize {
        if writers == 0 {
            return SEEN_SET_ELEMENT_BUDGET;
        }
        SEEN_SET_ELEMENT_BUDGET.saturating_sub(writers) / writers
    }

    /// Watermarks plus itemized refs — what the budget counts.
    pub fn element_count(&self) -> usize {
        self.watermarks.len() + self.refs.len()
    }

    /// Whether no writer itemizes more refs than its share of the budget
    /// (module docs § The budget). Part of normal form; `join` and the
    /// mutators always produce it, a decoded peer value may not carry it.
    pub fn within_budget(&self) -> bool {
        let share = Self::per_writer_ref_share(self.present_writers());
        let mut start = 0;
        while start < self.refs.len() {
            let writer = self.refs[start].writer;
            let run = self.refs[start..]
                .iter()
                .take_while(|r| r.writer == writer)
                .count();
            if run > share {
                return false;
            }
            start += run;
        }
        true
    }

    /// Whether this value is in normal form (see the type docs). `join` and
    /// the mutators always produce it; a decoded peer value may not carry it.
    pub fn is_normal_form(&self) -> bool {
        self.watermarks
            .windows(2)
            .all(|w| w[0].writer < w[1].writer)
            && self.watermarks.iter().all(|w| w.seq >= 1)
            && self.refs.windows(2).all(|r| r[0] < r[1])
            && self
                .refs
                .iter()
                .all(|r| r.seq >= 1 && self.watermark_of(&r.writer) < r.seq)
            && self.within_budget()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn set(watermarks: &[(u8, u64)], refs: &[(u8, u64)]) -> SeenScopeSet {
        let mut s = SeenScopeSet::new();
        for (writer, seq) in watermarks {
            s.raise_watermark(w(*writer), *seq);
        }
        for (writer, seq) in refs {
            s.insert_ref(w(*writer), *seq);
        }
        s
    }

    #[test]
    fn membership_is_watermark_or_itemized() {
        let s = set(&[(1, 5)], &[(1, 9), (2, 3)]);
        for seq in 1..=5 {
            assert!(s.contains(&w(1), seq), "covered by the watermark: {seq}");
        }
        assert!(!s.contains(&w(1), 6));
        assert!(s.contains(&w(1), 9));
        assert!(s.contains(&w(2), 3));
        assert!(
            !s.contains(&w(2), 2),
            "a watermark for writer 1 says nothing about writer 2"
        );
    }

    #[test]
    fn insert_is_idempotent_and_refuses_covered_or_zero() {
        let mut s = set(&[(1, 5)], &[]);
        assert!(!s.insert_ref(w(1), 3), "already covered");
        assert!(!s.insert_ref(w(1), 0), "seq 0 is not a coordinate");
        assert!(s.insert_ref(w(1), 7));
        assert!(!s.insert_ref(w(1), 7), "second insert is a no-op");
        assert!(s.is_normal_form());
    }

    #[test]
    fn raising_a_watermark_elides_covered_refs_and_never_lowers() {
        let mut s = set(&[], &[(1, 2), (1, 8), (2, 4)]);
        assert!(s.raise_watermark(w(1), 5));
        assert_eq!(
            s.refs,
            vec![
                SeenRef {
                    writer: w(1),
                    seq: 8
                },
                SeenRef {
                    writer: w(2),
                    seq: 4
                },
            ]
        );
        assert!(!s.raise_watermark(w(1), 4), "lowering is refused");
        assert_eq!(s.watermark_of(&w(1)), 5);
        // Membership only grew: the elided refs are still members.
        assert!(s.contains(&w(1), 2));
        assert!(s.is_normal_form());
    }

    /// The join laws, on shapes chosen to catch each mutation of the join:
    /// max-vs-min on watermarks, union-vs-intersection on refs, and the
    /// elision arm.
    #[test]
    fn join_is_commutative_associative_idempotent() {
        let a = set(&[(1, 5)], &[(2, 9), (3, 1)]);
        let b = set(&[(1, 3), (2, 6)], &[(1, 8), (2, 9)]);
        let c = set(&[(3, 2)], &[(1, 8), (4, 4)]);

        assert_eq!(a.join(&b), b.join(&a), "commutative");
        assert_eq!(a.join(&b).join(&c), a.join(&b.join(&c)), "associative");
        assert_eq!(a.join(&a), a, "idempotent");
        assert_eq!(a.join(&SeenScopeSet::new()), a, "empty is the identity");

        let j = a.join(&b);
        assert_eq!(j.watermark_of(&w(1)), 5, "pointwise max, not min");
        assert_eq!(j.watermark_of(&w(2)), 6);
        assert!(
            j.contains(&w(2), 9),
            "kept: above writer 2's watermark? no — covered"
        );
        assert!(j.contains(&w(1), 8), "the union kept b's itemized ref");
        assert!(j.is_normal_form());
    }

    /// A4's whole point, as a convergence property: one side compacts (raises
    /// a watermark), the other still carries the itemized refs — the join
    /// elides them on both sides identically, so canonical bytes agree and
    /// shrink.
    #[test]
    fn compaction_converges_and_shrinks_the_bytes() {
        let itemized = set(&[], &[(1, 1), (1, 2), (1, 3), (1, 4)]);
        let compacted = set(&[(1, 4)], &[]);

        let ab = itemized.join(&compacted);
        let ba = compacted.join(&itemized);
        assert_eq!(ab, ba);
        assert_eq!(ab, compacted, "the watermark absorbed every itemized ref");

        let big = crate::encoding::canonical_encode(&itemized).unwrap();
        let small = crate::encoding::canonical_encode(&ab).unwrap();
        assert!(
            small.len() < big.len(),
            "compaction must shrink the encoding ({} !< {})",
            small.len(),
            big.len()
        );
        // And membership never shrank.
        for seq in 1..=4 {
            assert!(ab.contains(&w(1), seq));
        }
    }

    /// The join normalizes hostile input: duplicate refs, an unsorted list, a
    /// covered ref, and a zero watermark all come out normal-form.
    #[test]
    fn join_normalizes_non_normal_input() {
        let hostile = SeenScopeSet {
            watermarks: vec![
                SeenWatermark {
                    writer: w(2),
                    seq: 3,
                },
                SeenWatermark {
                    writer: w(1),
                    seq: 0,
                },
            ],
            refs: vec![
                SeenRef {
                    writer: w(2),
                    seq: 2,
                }, // covered
                SeenRef {
                    writer: w(1),
                    seq: 7,
                },
                SeenRef {
                    writer: w(1),
                    seq: 7,
                }, // duplicate
            ],
        };
        assert!(!hostile.is_normal_form());
        let j = hostile.join(&SeenScopeSet::new());
        assert!(j.is_normal_form());
        assert_eq!(j.watermark_of(&w(1)), 0, "a zero watermark asserts nothing");
        assert!(j.contains(&w(1), 7));
        assert!(j.contains(&w(2), 2));
        assert_eq!(j.refs.len(), 1);
    }

    /// Same membership ⇒ same canonical bytes — the property the merge seam's
    /// byte-equality echo-stop rests on.
    #[test]
    fn normal_form_encodes_canonically() {
        let via_refs_then_watermark = {
            let mut s = set(&[], &[(1, 1), (1, 2), (2, 5)]);
            s.raise_watermark(w(1), 2);
            s
        };
        let direct = set(&[(1, 2)], &[(2, 5)]);
        assert_eq!(via_refs_then_watermark, direct);
        assert_eq!(
            crate::encoding::canonical_encode(&via_refs_then_watermark).unwrap(),
            crate::encoding::canonical_encode(&direct).unwrap()
        );
    }

    #[test]
    fn canonical_round_trip() {
        let s = set(&[(1, 5), (9, 2)], &[(1, 8), (2, 3)]);
        let bytes = crate::encoding::canonical_encode(&s).unwrap();
        let back: SeenScopeSet = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, s);
    }

    // ── § The budget ────────────────────────────────────────────────────────

    const B: u64 = SEEN_SET_ELEMENT_BUDGET as u64;

    /// A writer id past the `u8` range the fixtures above use.
    fn wid(n: u16) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[..2].copy_from_slice(&n.to_be_bytes());
        id
    }

    /// A value built through the UNBOUNDED mutators, so the budget can be
    /// exceeded on purpose — what a hostile or buggy
    /// writer hands the join.
    fn raw(watermarks: &[([u8; 32], u64)], refs: &[([u8; 32], u64)]) -> SeenScopeSet {
        let mut s = SeenScopeSet::new();
        for (writer, seq) in watermarks {
            s.raise_watermark_unbounded(*writer, *seq);
        }
        for (writer, seq) in refs {
            s.insert_ref_unbounded(*writer, *seq);
        }
        s
    }

    fn run(writer: [u8; 32], seqs: impl IntoIterator<Item = u64>) -> Vec<([u8; 32], u64)> {
        seqs.into_iter().map(|seq| (writer, seq)).collect()
    }

    /// The fold alone: the join with the empty set.
    fn fold(s: &SeenScopeSet) -> SeenScopeSet {
        s.join(&SeenScopeSet::new())
    }

    /// The lattice order the fold is monotone over: watermarks pointwise, and
    /// every ref of `a` a member of `b`.
    fn le(a: &SeenScopeSet, b: &SeenScopeSet) -> bool {
        a.watermarks
            .iter()
            .all(|w| b.watermark_of(&w.writer) >= w.seq)
            && a.refs.iter().all(|r| b.contains(&r.writer, r.seq))
    }

    /// Past its share, a writer's OLDEST itemized refs fold into its
    /// watermark: the bytes stay bounded, membership only grows — and the
    /// fold is the documented upward over-approximation, so an unobserved
    /// gap below the raised watermark now counts as seen.
    #[test]
    fn the_budget_folds_a_writers_oldest_refs_into_its_watermark() {
        let mut s = SeenScopeSet::new();
        // B + 1 odd seqs — every insert leaves a gap below it.
        for seq in (1..=2 * (B + 1)).step_by(2) {
            assert!(s.insert_ref(w(1), seq));
        }
        // One writer: its share is the budget minus its own watermark slot,
        // so two refs (seqs 1 and 3) folded.
        assert_eq!(s.refs.len(), SEEN_SET_ELEMENT_BUDGET - 1);
        assert_eq!(s.watermark_of(&w(1)), 3);
        assert_eq!(s.element_count(), SEEN_SET_ELEMENT_BUDGET);
        assert!(s.is_normal_form());
        for seq in (1..=2 * (B + 1)).step_by(2) {
            assert!(s.contains(&w(1), seq), "membership never shrinks: {seq}");
        }
        assert!(
            s.contains(&w(1), 2),
            "the fold over-approximates upward: the gap under the raised watermark is in-set"
        );
        assert!(
            !s.contains(&w(1), 4),
            "above the watermark, only the itemized refs are members"
        );
    }

    /// The budget is per scope: present writers split it evenly, one
    /// watermark slot each, and a writer that appears by watermark alone
    /// still takes a slot (and shrinks every other writer's share).
    #[test]
    fn the_budget_is_shared_between_present_writers() {
        let mut s = SeenScopeSet::new();
        for seq in 1..=600 {
            s.insert_ref(w(1), seq);
            s.insert_ref(w(2), seq);
        }
        let share = (SEEN_SET_ELEMENT_BUDGET - 2) / 2;
        for writer in [w(1), w(2)] {
            assert_eq!(s.refs.iter().filter(|r| r.writer == writer).count(), share);
            assert_eq!(s.watermark_of(&writer), 600 - share as u64);
        }
        assert_eq!(s.element_count(), SEEN_SET_ELEMENT_BUDGET);

        assert!(s.raise_watermark(w(3), 7));
        let share = (SEEN_SET_ELEMENT_BUDGET - 3) / 3;
        for writer in [w(1), w(2)] {
            assert_eq!(s.refs.iter().filter(|r| r.writer == writer).count(), share);
            assert_eq!(s.watermark_of(&writer), 600 - share as u64);
        }
        assert!(s.element_count() <= SEEN_SET_ELEMENT_BUDGET);
        assert!(s.is_normal_form());
    }

    /// The join laws still hold when the budget binds — every grouping and
    /// order of three values whose union outgrows it meets on identical
    /// bytes. Includes the shape a non-closure fold (drop the globally
    /// oldest refs) diverges on: one side folded early, the other side's
    /// watermark then eliding the very refs the early fold had ranked.
    #[test]
    fn the_fold_keeps_the_join_a_join_when_the_budget_binds() {
        let a = raw(&[], &run(w(1), 1..=700));
        let b = raw(&[], &run(w(1), 701..=1400));
        let c = raw(&[(w(1), 1023)], &run(w(2), 1..=300));

        let abc = a.join(&b).join(&c);
        assert_eq!(abc, a.join(&b.join(&c)), "associative");
        assert_eq!(abc, a.join(&c).join(&b), "associative, other grouping");
        assert_eq!(abc, c.join(&b).join(&a), "commutative");
        assert_eq!(abc, abc.join(&abc), "idempotent");
        assert_eq!(
            abc,
            fold(&abc),
            "closed: folding a joined value is the identity"
        );
        assert!(abc.is_normal_form());
        assert!(abc.element_count() <= SEEN_SET_ELEMENT_BUDGET);
        for seq in 1..=1400 {
            assert!(abc.contains(&w(1), seq));
        }
        for seq in 1..=300 {
            assert!(abc.contains(&w(2), seq));
        }

        // The divergence shape: x folds (writer 1 past its share, writer 2's
        // lone ref intact); y's watermark then elides everything of writer 1.
        let x = raw(&[], &[run(w(1), 1..=B), vec![(w(2), 1)]].concat());
        let y = raw(&[(w(1), B)], &[]);
        assert_eq!(x.join(&y), fold(&x).join(&y));
        assert_eq!(x.join(&y), y.join(&fold(&x)));
    }

    /// The fold is a closure operator over the lattice — extensive,
    /// idempotent, monotone — on deterministic pseudo-random pairs `x ≤ y`
    /// sized so the budget binds on some and not on others. Monotonicity is
    /// the property that makes `bound(a ∪ b)` associative.
    #[test]
    fn the_fold_is_a_closure_over_the_lattice() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |bound: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % bound
        };
        for _ in 0..60 {
            let writers = 1 + next(3) as usize;
            let mut x = SeenScopeSet::new();
            for writer in 0..writers {
                let refs = 200 + next(1100);
                for _ in 0..refs {
                    x.insert_ref_unbounded(wid(writer as u16), 1 + next(3000));
                }
                if next(2) == 1 {
                    x.raise_watermark_unbounded(wid(writer as u16), 1 + next(500));
                }
            }
            // y: x plus more — extra refs, an extra writer sometimes, a raise.
            let mut y = x.clone();
            for _ in 0..next(400) {
                y.insert_ref_unbounded(wid(next(writers as u64 + 1) as u16), 1 + next(3000));
            }
            if next(2) == 1 {
                y.raise_watermark_unbounded(wid(next(writers as u64) as u16), 1 + next(1500));
            }
            assert!(le(&x, &y), "fixture: x ≤ y");

            let fx = fold(&x);
            let fy = fold(&y);
            assert!(le(&x, &fx), "extensive");
            assert_eq!(fold(&fx), fx, "idempotent");
            assert!(le(&fx, &fy), "monotone");
            assert!(fx.is_normal_form() && fy.is_normal_form());
            assert!(fx.element_count() <= SEEN_SET_ELEMENT_BUDGET);
        }
    }

    /// A value past the budget — from a hostile or buggy writer — is not normal form, and joins into it.
    #[test]
    fn the_join_bounds_an_over_budget_peer_value() {
        let unbounded = raw(&[], &run(w(1), 1..=2000));
        assert!(!unbounded.within_budget());
        assert!(!unbounded.is_normal_form());
        let j = fold(&unbounded);
        assert!(j.is_normal_form());
        assert_eq!(j.element_count(), SEEN_SET_ELEMENT_BUDGET);
        assert_eq!(j.watermark_of(&w(1)), 2000 - (B - 1));
        for seq in 1..=2000 {
            assert!(j.contains(&w(1), seq));
        }
        // And the over-budget value is simply below its fold, so the pair
        // converges on the folded bytes at the next exchange.
        assert!(le(&unbounded, &j));
        assert_eq!(unbounded.join(&j), j);
    }

    /// The one axis the budget leaves open: the writer population. Past the
    /// budget in writers alone every ref folds into its watermark, and the
    /// value is one watermark per writer — bounded by the population, which
    /// the plane bounds elsewhere.
    #[test]
    fn a_writer_population_past_the_budget_folds_every_ref() {
        let mut s = SeenScopeSet::new();
        for n in 0..1100u16 {
            assert!(s.insert_ref(wid(n), 5));
        }
        assert!(s.refs.is_empty());
        assert_eq!(s.watermarks.len(), 1100);
        assert!(s.is_normal_form(), "a zero share is within budget");
        assert!(s.contains(&wid(7), 5) && s.contains(&wid(7), 3));
    }
}
