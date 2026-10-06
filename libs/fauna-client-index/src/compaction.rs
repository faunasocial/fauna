//! **Fold-at-flush compaction** — the ruled v1 mechanism
//! (`content-index.md` § Where the index is built, ruled 2026-08-02).
//!
//! A flush that would append segment N may instead merge its staged docs with
//! small live segments the builder's manifest view lists, publishing **one**
//! merged segment and a manifest that tombstones the folded ones. That is the
//! whole mechanism: same segment-then-manifest publish order, same wholesale
//! manifest rewrite, same concurrency envelope as an ordinary flush — so it
//! adds **no new race class**. A lost manifest race degrades to an unreferenced
//! segment (harmless, re-created) or an un-taken compaction (retried at the next
//! threshold crossing), which are exactly the outcomes the flush path already
//! designs for.
//!
//! This module is deliberately **pure**: it decides *which* segments to fold
//! from sizes and ids alone, with no rail access and no key material. That is
//! what lets the policy be unit-tested exhaustively while the I/O half stays in
//! [`crate::index_builder`].

/// Live-segment count past which a kind becomes compaction-eligible.
///
/// **Not a configuration surface** — no user or admin would ever choose this
/// (product invariants § the only configuration surface is the apps), so it is
/// a Rust constant, ratified in `content-index.md` § Where the index is built.
///
/// Read it as *strictly past*: 16 live segments is fine, the 17th makes the
/// kind eligible. That matches the ratified wording ("past 16 live segments")
/// and the long-standing `Index::merge_segments` doc comment (">16").
pub const COMPACTION_LIVE_SEGMENT_THRESHOLD: usize = 16;

/// Tombstoned-doc fraction past which a *single* segment is worth folding on
/// its own account, ratified alongside the count threshold.
///
/// **Inert for the mail slice in v1, by construction — and that is a property
/// of mail, not an unimplemented piece.** A tombstoned doc is one superseded by
/// a newer version of the same content id, and mail content ids are the
/// immutable producer-owned RFC `Message-ID`: the stage-time re-index guard
/// (`content-index.md` § Ingest triggers, v1) drops an already-known id rather
/// than re-staging it, so a mail doc is written once and never superseded. The
/// fraction is therefore always 0 % for the only kind this builder writes, and
/// no amount of ingest moves it.
///
/// It becomes measurable — and this constant load-bearing — with **S4's
/// master-key builder**, which indexes genuinely mutable kinds (drafts being
/// the named case the guard is explicitly *wrong* for). That is also the slice
/// that must add the per-segment doc accounting to measure it: `KindManifest`
/// records `next_seg_id` / `live_segments` / `tombstoned_segments` and no doc
/// counts at all, so there is nothing to compute a fraction from today.
pub const COMPACTION_TOMBSTONED_DOC_FRACTION: f64 = 0.25;

/// Minimum number of live segments a fold must consume to be worth taking.
///
/// Folding a *single* live segment with the staged batch removes one live
/// segment and adds one back, so the live count does not move and the fold
/// bought nothing but a rewrite. Two is the smallest input set that actually
/// shrinks `live_segments`.
const MIN_FOLD_INPUTS: usize = 2;

/// One live segment as the planner sees it: its id and the size of its sealed
/// blob on the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FoldCandidate {
    pub seg_id: u32,
    pub sealed_bytes: u64,
}

/// The chosen fold: the live segment ids whose plaintext is merged with the
/// staged batch into one new segment, and which the manifest then tombstones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoldPlan {
    /// Ascending, so the fold reads and tombstones in a deterministic order.
    pub inputs: Vec<u32>,
}

/// Decide whether this flush folds, and over which segments.
///
/// `staged_sealed_bytes` is the size of the segment this flush *would* have
/// appended (zero when the flush is manifest-only); `ceiling` is
/// [`crate::MAX_SEGMENT_BYTES`], which the merged output must respect exactly
/// as an appended segment does — the `__index` rail carries one whole blob per
/// path with no chunk-manifest form, so an over-ceiling merge would have
/// nowhere to go.
///
/// Returns `None` when the kind is not eligible, or when no admissible input
/// set exists (every live segment is too large to fold under the ceiling). The
/// second case is a normal, designed outcome — the "un-taken compaction" the
/// ruling names — not an error: those segments are not *small*, and the ruling
/// folds small ones.
pub(crate) fn plan_fold(
    live: &[FoldCandidate],
    staged_sealed_bytes: u64,
    ceiling: u64,
) -> Option<FoldPlan> {
    if live.len() <= COMPACTION_LIVE_SEGMENT_THRESHOLD {
        return None;
    }

    // Smallest first: the ruling folds *small* live segments, and taking the
    // smallest maximises how many fit under the one-blob ceiling. Ties break on
    // segment id so the plan is deterministic for identical input.
    let mut by_size: Vec<FoldCandidate> = live.to_vec();
    by_size.sort_by_key(|c| (c.sealed_bytes, c.seg_id));

    let mut budget = ceiling.checked_sub(staged_sealed_bytes)?;
    let mut inputs: Vec<u32> = Vec::new();
    for candidate in by_size {
        let Some(remaining) = budget.checked_sub(candidate.sealed_bytes) else {
            // Sorted ascending, so nothing after this fits either.
            break;
        };
        budget = remaining;
        inputs.push(candidate.seg_id);
    }

    if inputs.len() < MIN_FOLD_INPUTS {
        return None;
    }
    inputs.sort_unstable();
    Some(FoldPlan { inputs })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CEILING: u64 = 8 * 1024 * 1024;

    fn live(sizes: &[(u32, u64)]) -> Vec<FoldCandidate> {
        sizes
            .iter()
            .map(|&(seg_id, sealed_bytes)| FoldCandidate {
                seg_id,
                sealed_bytes,
            })
            .collect()
    }

    fn uniform(count: u32, each: u64) -> Vec<FoldCandidate> {
        live(&(1..=count).map(|i| (i, each)).collect::<Vec<_>>())
    }

    #[test]
    fn a_kind_at_or_below_the_threshold_does_not_fold() {
        // The ratified threshold is *past* 16 — 16 itself is still fine.
        assert_eq!(plan_fold(&uniform(16, 1024), 0, CEILING), None);
        assert_eq!(plan_fold(&uniform(1, 1024), 0, CEILING), None);
        assert_eq!(plan_fold(&[], 0, CEILING), None);
    }

    #[test]
    fn the_seventeenth_live_segment_makes_the_kind_eligible() {
        let plan = plan_fold(&uniform(17, 1024), 0, CEILING).expect("eligible past 16");
        assert_eq!(plan.inputs.len(), 17, "all seventeen fit well under 8 MiB");
    }

    #[test]
    fn a_fold_shrinks_the_live_count() {
        // The property that makes compaction worth taking at all: inputs
        // collapse to one segment, so live_segments strictly shrinks.
        let plan = plan_fold(&uniform(20, 4096), 0, CEILING).unwrap();
        assert!(
            plan.inputs.len() >= MIN_FOLD_INPUTS,
            "a plan that consumes fewer than two segments cannot shrink the live count"
        );
        let after = 20 - plan.inputs.len() + 1;
        assert!(
            after < 20,
            "live count must strictly decrease: {after} vs 20"
        );
    }

    #[test]
    fn the_merged_output_stays_under_the_one_blob_ceiling() {
        // Sealed sizes are the planner's proxy for the merged size, so the
        // chosen inputs plus the staged batch must fit the ceiling — an
        // over-ceiling merge has nowhere to go on the `__index` rail.
        let sizes: Vec<(u32, u64)> = (1..=20).map(|i| (i, 1024 * 1024)).collect();
        let staged = 2 * 1024 * 1024;
        let plan = plan_fold(&live(&sizes), staged, CEILING).unwrap();
        let folded: u64 = plan.inputs.len() as u64 * 1024 * 1024;
        assert!(
            folded + staged <= CEILING,
            "fold of {} MiB + staged {} MiB exceeds the ceiling",
            folded / 1024 / 1024,
            staged / 1024 / 1024
        );
    }

    #[test]
    fn it_prefers_the_smallest_segments() {
        // "Small live segments" is the ruled input set, so with a budget that
        // cannot take everything, no excluded segment may be smaller than an
        // included one. Asserted as that invariant rather than a hand-computed
        // id list: the list moves with the ceiling arithmetic, the invariant is
        // the actual ruled property.
        let mut sizes: Vec<(u32, u64)> = (1..=16).map(|i| (i, 4 * 1024 * 1024)).collect();
        sizes.push((17, 1024));
        sizes.push((18, 2048));
        let candidates = live(&sizes);
        let plan = plan_fold(&candidates, 0, CEILING).unwrap();

        assert!(
            plan.inputs.contains(&17) && plan.inputs.contains(&18),
            "the two smallest segments must always be folded: {:?}",
            plan.inputs
        );
        assert!(
            plan.inputs.len() < sizes.len(),
            "this budget cannot take every segment, or the test proves nothing"
        );
        let largest_taken = candidates
            .iter()
            .filter(|c| plan.inputs.contains(&c.seg_id))
            .map(|c| c.sealed_bytes)
            .max()
            .unwrap();
        let smallest_left = candidates
            .iter()
            .filter(|c| !plan.inputs.contains(&c.seg_id))
            .map(|c| c.sealed_bytes)
            .min()
            .unwrap();
        assert!(
            largest_taken <= smallest_left,
            "a larger segment ({largest_taken}) was folded while a smaller one \
             ({smallest_left}) was left behind"
        );
    }

    #[test]
    fn segments_too_large_to_fold_yield_an_un_taken_compaction() {
        // A designed outcome, not an error: nothing here is *small*.
        let sizes: Vec<(u32, u64)> = (1..=20).map(|i| (i, 5 * 1024 * 1024)).collect();
        assert_eq!(plan_fold(&live(&sizes), 0, CEILING), None);
    }

    #[test]
    fn a_staged_batch_filling_the_ceiling_defers_the_fold() {
        // No room left to fold anything into: the append path takes this flush
        // and the threshold is re-crossed on the next one.
        assert_eq!(plan_fold(&uniform(20, 1024), CEILING, CEILING), None);
        // And an over-ceiling staged batch must not underflow the budget.
        assert_eq!(plan_fold(&uniform(20, 1024), CEILING + 1, CEILING), None);
    }

    #[test]
    fn the_plan_is_deterministic_and_ascending() {
        // Two builders with the same manifest view must choose the same fold,
        // and the ids come back sorted so reads and tombstones are ordered.
        let sizes: Vec<(u32, u64)> = (1..=20).rev().map(|i| (i, 1024 + i as u64)).collect();
        let a = plan_fold(&live(&sizes), 0, CEILING).unwrap();
        let b = plan_fold(&live(&sizes), 0, CEILING).unwrap();
        assert_eq!(a, b);
        let mut sorted = a.inputs.clone();
        sorted.sort_unstable();
        assert_eq!(a.inputs, sorted, "inputs must be ascending");
    }
}
