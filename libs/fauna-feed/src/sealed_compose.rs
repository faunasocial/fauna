//! The client-side **sealed-factor compose seam** for the feed
//! (`docs/goal/behavior/topic-factors.md` § Scoring — the owner doc).
//!
//! A sealed tier-1 factor (a trained `topic:*` model, the `muted-keywords`
//! penalty) writes no nest-side `content_scores` rows, so its nest composition
//! term is always `COALESCE(…, 0) = 0` — the *zero-term seam*, pinned by
//! `bins/fauna-nest/tests/composed_feed.rs`. The nest therefore hands the client
//! a composed key (`FeedPostItem.score`) that is **missing** exactly those
//! factors' contributions, and the client adds them back post-decrypt:
//!
//! ```text
//! final = FeedPostItem.score + Σ sealed contributions
//! ```
//!
//! Additivity is why the client needs no per-factor breakdown of the nest's key.
//!
//! # Units — the one place this is easy to get wrong
//!
//! Three fixed-point scales meet here, so the derivation is spelled out rather
//! than asserted:
//!
//! * A **bus factor value** (`content_scores.score`, `TopicModel::score`) is an
//!   integer **per-mille**: `1000` = 1.0.
//! * A **composition weight** (`CompositionEntry::weight_permille`) is an
//!   integer **per-mille**: `1000` = 1.0×.
//! * The **wire score** (`FeedPostItem.score`) is integer **micro-units**: the
//!   nest computes an `f64` ordering key and ships `(key * 1e6).round() as i64`
//!   (`bins/fauna-nest/src/feed_handlers.rs::score_to_micro`).
//!
//! The nest's SQL term for a bus factor is
//! `weight_permille * (content_scores.score / 1000.0)`
//! (`bins/fauna-nest/src/db/feeds.rs::query_feed_scored`). So the same factor,
//! composed client-side, must contribute — in wire micro-units —
//!
//! ```text
//! round( w * (v / 1000.0) * 1e6 )  ==  w * v * 1000      (exact, in integers)
//! ```
//!
//! which is [`sealed_contribution_micro`]. The invariant this pins is
//! **nest-equivalence**: a sealed factor contributes *exactly* what the nest
//! would have contributed had it been able to read that factor's bus row. That
//! is what makes the seal a privacy boundary rather than a scoring change, and
//! it is what `sealed_contribution_equals_the_nest_term_it_replaces` tests.
//!
//! # Page-boundary honesty (accepted limit)
//!
//! The nest paginates on the transparent key, so sealed contributions re-rank
//! only the **loaded window**. Exact cross-page ordering would require handing
//! the factor to the nest, which the tier-1 seal forbids. A strong negative (a
//! mute) therefore sinks-and-collapses within the window rather than
//! pre-filtering pages — the same privacy-over-exactness trade the conversation
//! collapse already made (owner doc § Scoring).

use std::collections::HashMap;

use crate::snapshot::PostSummary;

/// Wire micro-units per (per-mille weight × per-mille value): `1e6 / 1000`.
/// See the module-level units derivation — named so the call site reads as a
/// unit conversion rather than a magic constant.
const MICRO_PER_PERMILLE_PRODUCT: i64 = 1_000;

/// One sealed tier-1 factor's contribution to one post, in **wire micro-units**
/// (the scale of `FeedPostItem.score`).
///
/// `weight_permille` is the factor's weight in the feed's effective composition
/// (its own composition plus the user's global factor set); `factor_value_permille`
/// is the sealed scorer's output — `TopicModel::score`'s damped per-mille, or
/// [`fauna_core::scoring::MUTED_KEYWORDS_PENALTY`] (−1000) for a muted match.
///
/// Saturating, not wrapping: composition weights are unrestricted `i64` (an
/// extreme negative is the *filter* verb — owner doc § Scoring), and a
/// pathological weight must sink a post, never wrap it to the top of the feed.
pub fn sealed_contribution_micro(weight_permille: i64, factor_value_permille: i64) -> i64 {
    weight_permille
        .saturating_mul(factor_value_permille)
        .saturating_mul(MICRO_PER_PERMILLE_PRODUCT)
}

/// A post's adjusted ordering key: the nest-served composed key plus every
/// sealed contribution, all in wire micro-units. `terms` are
/// `(weight_permille, factor_value_permille)` pairs.
///
/// `base_score_micro` is `FeedPostItem.score`. It is `0` for a post the nest
/// returned without a score (a chronological page) — which is why the manager
/// re-ranks only feeds it fetched with `order=score`. Adding sealed terms to a
/// uniformly-zero base would silently turn a chronological feed into a
/// sealed-factor-ordered one.
pub fn adjusted_score_micro(base_score_micro: i64, terms: &[(i64, i64)]) -> i64 {
    terms
        .iter()
        .fold(base_score_micro, |acc, &(weight, value)| {
            acc.saturating_add(sealed_contribution_micro(weight, value))
        })
}

/// Re-rank the **loaded window** in place by descending adjusted key.
///
/// Stable, so posts with equal adjusted keys keep their relative nest order:
/// the nest's `ORDER BY …, c.created_at DESC` tiebreak survives, and — the
/// property that matters — a factor shifting every post by the same constant
/// (an untrained model returns a flat `500` per-mille) leaves the order
/// **exactly** as the nest sent it. That is what makes an untrained topic factor
/// a no-op rather than a reshuffle (owner doc § The model → cold-model damping).
///
/// **All-or-nothing.** If any loaded post is missing from `adjusted`, the window
/// is left in nest order untouched. A partial sort would interleave scored and
/// unscored posts on incomparable keys — worse than not re-ranking, and it would
/// require a comparator that is not a total order.
pub fn rerank_loaded_window(posts: &mut [PostSummary], adjusted: &HashMap<String, i64>) {
    if posts.iter().any(|p| !adjusted.contains_key(&p.post_id)) {
        return;
    }
    posts.sort_by(|a, b| adjusted[&b.post_id].cmp(&adjusted[&a.post_id]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestPostSpec;

    fn post(id: &str) -> PostSummary {
        TestPostSpec {
            post_id: id.to_string(),
            ..Default::default()
        }
        .into_summary()
    }

    /// The load-bearing invariant: a sealed factor contributes *exactly* what
    /// the nest's SQL would have contributed for an equivalent `content_scores`
    /// row. Computed here the way the nest does — in `f64`, then scaled to
    /// micro-units — and compared against our integer path.
    ///
    /// If this drifts, sealed and transparent factors sit on different scales
    /// and every composed feed silently mis-ranks.
    #[test]
    fn sealed_contribution_equals_the_nest_term_it_replaces() {
        let cases = [
            (1000, 1000),  // 1.0x weight, P = 1.0
            (1000, 500),   // 1.0x weight, neutral
            (2500, 750),   // 2.5x weight
            (-1000, 400),  // negative weight (demote)
            (1000, -1000), // the muted-keywords penalty
            (0, 1000),     // zero weight contributes nothing
        ];
        for (w, v) in cases {
            // The nest: ordering-key term `w * (v / 1000.0)`, shipped as
            // `(key * 1e6).round() as i64` (feed_handlers.rs::score_to_micro).
            let nest_micro = ((w as f64 * (v as f64 / 1000.0)) * 1e6).round() as i64;
            assert_eq!(
                sealed_contribution_micro(w, v),
                nest_micro,
                "sealed contribution diverged from the nest term for (w={w}, v={v})"
            );
        }
    }

    #[test]
    fn an_untrained_model_has_zero_ordering_effect() {
        // Owner doc § The model: cold damping returns a constant 500 per-mille,
        // and "a constant 500 shifts every item equally, so an untrained model
        // has zero ordering effect".
        let mut posts = vec![post("a"), post("b"), post("c")];
        let bases = [("a", 3_000_000i64), ("b", 2_000_000), ("c", 1_000_000)];
        let adjusted: HashMap<String, i64> = bases
            .iter()
            .map(|&(id, b)| (id.to_string(), adjusted_score_micro(b, &[(1000, 500)])))
            .collect();

        rerank_loaded_window(&mut posts, &adjusted);

        let order: Vec<&str> = posts.iter().map(|p| p.post_id.as_str()).collect();
        assert_eq!(
            order,
            vec!["a", "b", "c"],
            "a flat factor reshuffled the feed"
        );
    }

    #[test]
    fn a_trained_factor_promotes_a_matching_post_over_a_higher_engagement_one() {
        // "b" has the lower nest key but scores 1.0 on the trained topic; "a"
        // leads on engagement but scores 0. At a 1.0x topic weight the topic
        // term (1e9) dominates the engagement gap (1e6) and "b" rises.
        let mut posts = vec![post("a"), post("b")];
        let adjusted: HashMap<String, i64> = [
            (
                "a".to_string(),
                adjusted_score_micro(3_000_000, &[(1000, 0)]),
            ),
            (
                "b".to_string(),
                adjusted_score_micro(1_000_000, &[(1000, 1000)]),
            ),
        ]
        .into_iter()
        .collect();

        rerank_loaded_window(&mut posts, &adjusted);

        let order: Vec<&str> = posts.iter().map(|p| p.post_id.as_str()).collect();
        assert_eq!(order, vec!["b", "a"]);
    }

    #[test]
    fn a_muted_match_sinks_the_post_below_every_unmuted_one() {
        // -1000 per-mille at unit weight = -1e9 micro, dominating any realistic
        // engagement key (owner doc § Scoring: "sinks-and-collapses").
        let mut posts = vec![post("muted"), post("plain")];
        let adjusted: HashMap<String, i64> = [
            (
                "muted".to_string(),
                adjusted_score_micro(
                    5_000_000, // a very high-engagement post
                    &[(1000, fauna_core::scoring::MUTED_KEYWORDS_PENALTY)],
                ),
            ),
            ("plain".to_string(), adjusted_score_micro(0, &[])),
        ]
        .into_iter()
        .collect();

        rerank_loaded_window(&mut posts, &adjusted);

        let order: Vec<&str> = posts.iter().map(|p| p.post_id.as_str()).collect();
        assert_eq!(order, vec!["plain", "muted"]);
    }

    #[test]
    fn an_incompletely_keyed_window_keeps_the_nest_order() {
        let mut posts = vec![post("a"), post("b")];
        // Only "b" is keyed, and keyed to sort first — but "a" is unkeyed, so
        // the whole window must be left alone rather than partially sorted.
        let adjusted: HashMap<String, i64> = [("b".to_string(), i64::MAX)].into_iter().collect();

        rerank_loaded_window(&mut posts, &adjusted);

        let order: Vec<&str> = posts.iter().map(|p| p.post_id.as_str()).collect();
        assert_eq!(order, vec!["a", "b"]);
    }

    #[test]
    fn extreme_weights_saturate_rather_than_wrap() {
        // A pathological weight must sink a post, never wrap it to the top.
        assert_eq!(sealed_contribution_micro(i64::MAX, -1000), i64::MIN);
        assert_eq!(sealed_contribution_micro(i64::MAX, 1000), i64::MAX);
        assert_eq!(adjusted_score_micro(-5, &[(i64::MAX, -1000)]), i64::MIN);
    }
}
