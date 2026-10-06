//! Integration tests for composed feed ordering (frame § Composition,
//! `content-moderation-and-ranking.md`): `query_feed_scored` with a
//! non-empty composition orders by the weighted sum over `content_scores`
//! bus factors, `final = Σ (weight_permille · factor_value) / 1000`, with
//! the nest-computed cumulative-engagement scalar (`content_meta.score`, an
//! f64 in `[0,1)` the feed multiplies by the caller weight) as one factor
//! among many (`factor::ENGAGEMENT`). The scalar is materialized by the
//! engagement counters (`increment_engagement_count`), so these tests drive
//! the counts directly — no separate recompute step; it is decay-free (the
//! decayed sibling is the `trending` factor).
//!
//! Bus rows are seeded the production way (`insert_content_scores`) with
//! `content_kind='post'` and `actor_id = NULL` — public posts have no
//! at-rest owner scope, and the composed read JOINs by `content_id` alone
//! (the labeler-registry List materialization contract).
//!
//! Tier: tier_3 (real `CacheDb`, no mocks).

use fauna_core::scoring::{
    CompositionEntry, FilterCombination, ScoreEntry, TIER_COMMUNITY, factor,
};
use fauna_nest::db::{CacheDb, ScoreCursor};

const LABELER: &str = "labeler:aabb";

fn entry(factor: &str, weight_permille: i64) -> CompositionEntry {
    CompositionEntry {
        factor: factor.to_string(),
        weight_permille,
    }
}

/// Seed one post + one bus row carrying `score` for [`LABELER`].
async fn seed_post_with_factor(db: &CacheDb, post_id: [u8; 32], created_at: i64, score: i64) {
    let author = [7u8; 32];
    db.seed_scored_post_for_test(&post_id, &author, created_at, LABELER, score)
        .await
        .unwrap();
}

#[tokio::test]
async fn composed_feed_orders_by_weighted_bus_factor() {
    let db = CacheDb::open_in_memory().unwrap();
    // B is newer and would win chronologically; A carries the higher factor.
    let pa = [0xA1u8; 32];
    let pb = [0xB1u8; 32];
    seed_post_with_factor(&db, pa, 1_000_000, 900).await;
    seed_post_with_factor(&db, pb, 2_000_000, 100).await;

    // Positive weight: the "cats" feed — high catness first.
    let promote = [entry(LABELER, 1000)];
    let rows = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &promote)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].post_id.as_slice(), &pa, "promoted: A (900) first");
    assert_eq!(rows[1].post_id.as_slice(), &pb);
    // The composed key rides the existing score field: w·f/1000.
    assert_eq!(rows[0].score, Some(900.0));
    assert_eq!(rows[1].score, Some(100.0));

    // Negative weight: the same factor as a penalty — order flips.
    let penalize = [entry(LABELER, -1000)];
    let rows = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &penalize)
        .await
        .unwrap();
    assert_eq!(rows[0].post_id.as_slice(), &pb, "penalized: B (−100) first");
    assert_eq!(rows[1].post_id.as_slice(), &pa);
}

#[tokio::test]
async fn composed_strong_negative_factor_sinks_item_below_engagement() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [8u8; 32];

    // A: engaged post, no factor row. B: engaged post carrying a full-score
    // muted-keyword-style penalty factor.
    let pa = [0xA2u8; 32];
    let pb = [0xB2u8; 32];
    for (pid, created_at) in [(pa, 1_000_000), (pb, 2_000_000)] {
        db.insert_post_index_entry(&pid, &author, created_at, false, false, "fauna", &[])
            .await
            .unwrap();
        for _ in 0..10 {
            db.increment_engagement_count(&pid, "like").await.unwrap();
        }
    }
    db.insert_content_scores(
        &pb,
        "post",
        None,
        2_000_000,
        &[ScoreEntry {
            factor: LABELER.to_string(),
            score: 1000,
            tier: TIER_COMMUNITY,
            scorer_version: 1,
        }],
    )
    .await
    .unwrap();

    // engagement promoted at 1.0×, the penalty factor at −1000‰ (the frame's
    // filter verb): B's −1000 contribution swamps any engagement value and
    // sinks it — filtering falls out of ordering, no separate filter path.
    let composition = [entry(factor::ENGAGEMENT, 1000), entry(LABELER, -1000)];
    let rows = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &composition)
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        2,
        "soft exclusion: the item is sunk, not dropped"
    );
    assert_eq!(rows[0].post_id.as_slice(), &pa);
    assert_eq!(rows[1].post_id.as_slice(), &pb, "penalized post sinks last");
    assert!(
        rows[1].score.unwrap() < 0.0,
        "the −1000 contribution dominates: {:?}",
        rows[1].score
    );
}

#[tokio::test]
async fn composed_engagement_factor_matches_legacy_order_at_unit_weight() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [9u8; 32];

    let posts = [[0xC1u8; 32], [0xC2u8; 32], [0xC3u8; 32]];
    for (i, pid) in posts.iter().enumerate() {
        db.insert_post_index_entry(
            pid,
            &author,
            1_000_000 + i as i64,
            false,
            false,
            "fauna",
            &[],
        )
        .await
        .unwrap();
        for _ in 0..(posts.len() - i) * 5 {
            db.increment_engagement_count(pid, "like").await.unwrap();
        }
    }

    let legacy = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &[])
        .await
        .unwrap();
    let composed = db
        .query_feed_scored(
            &[],
            FilterCombination::All,
            &[],
            None,
            10,
            &[entry(factor::ENGAGEMENT, 1000)],
        )
        .await
        .unwrap();
    assert_eq!(
        legacy.iter().map(|p| p.post_id.clone()).collect::<Vec<_>>(),
        composed
            .iter()
            .map(|p| p.post_id.clone())
            .collect::<Vec<_>>(),
        "engagement at weight 1000 reproduces the legacy single-score order"
    );
    // The composed key expresses engagement per-mille (score × 1000).
    let legacy_score = legacy[0].score.unwrap();
    let composed_score = composed[0].score.unwrap();
    assert!(
        (composed_score - legacy_score * 1000.0).abs() < 1e-6,
        "composed engagement = legacy × 1000: {legacy_score} vs {composed_score}"
    );
}

/// The nest-side/client-side seam (frame § Composition — where it runs):
/// a factor the nest has no bus rows for (a sealed tier-1 factor like the
/// muted-keyword list, whose data the nest cannot read) contributes exactly
/// zero nest-side — the nest composes only bus rows it holds, never errors,
/// never tries to read sealed factor data. The sealed contribution is
/// composed client-side post-decrypt (the personalization /
/// local-flag moderation tracks).
#[tokio::test]
async fn composed_sealed_factor_without_bus_rows_contributes_zero() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [10u8; 32];

    let pa = [0xD1u8; 32];
    let pb = [0xD2u8; 32];
    for (pid, likes) in [(pa, 10), (pb, 2)] {
        db.insert_post_index_entry(&pid, &author, 1_000_000, false, false, "fauna", &[])
            .await
            .unwrap();
        for _ in 0..likes {
            db.increment_engagement_count(&pid, "like").await.unwrap();
        }
    }

    let with_sealed = [
        entry(factor::ENGAGEMENT, 1000),
        // A sealed tier-1 factor: no content_scores rows exist nest-side.
        entry("keyword-mute", -1_000_000),
    ];
    let without = [entry(factor::ENGAGEMENT, 1000)];
    let a = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &with_sealed)
        .await
        .unwrap();
    let b = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &without)
        .await
        .unwrap();
    assert_eq!(
        a.iter()
            .map(|p| (p.post_id.clone(), p.score))
            .collect::<Vec<_>>(),
        b.iter()
            .map(|p| (p.post_id.clone(), p.score))
            .collect::<Vec<_>>(),
        "a factor with no nest-readable rows is a zero term nest-side"
    );
}

#[tokio::test]
async fn composed_cursor_paginates_on_the_composed_key() {
    let db = CacheDb::open_in_memory().unwrap();
    let p1 = [0xE1u8; 32];
    let p2 = [0xE2u8; 32];
    let p3 = [0xE3u8; 32];
    seed_post_with_factor(&db, p1, 1_000_000, 900).await;
    seed_post_with_factor(&db, p2, 2_000_000, 500).await;
    seed_post_with_factor(&db, p3, 3_000_000, 100).await;

    let composition = [entry(LABELER, 1000)];
    let page1 = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 2, &composition)
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].post_id.as_slice(), &p1);
    assert_eq!(page1[1].post_id.as_slice(), &p2);

    let page2 = db
        .query_feed_scored(
            &[],
            FilterCombination::All,
            &[],
            cursor_of(&page1),
            2,
            &composition,
        )
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].post_id.as_slice(), &p3);
}

/// The keyset cursor of a page's last row — what `query_feed_core` hands the
/// client as `(score_cursor, score_cursor_created_at)`. The tiebreak component
/// is what makes the cursor agree with the compound `ORDER BY key DESC,
/// created_at DESC`.
fn cursor_of(page: &[fauna_nest::db::FeedPostRow]) -> Option<ScoreCursor> {
    let last = page.last()?;
    Some(ScoreCursor {
        key: last.score?,
        created_at: last.created_at,
    })
}

/// **Ties must not be skipped.** The sort is `ORDER BY key DESC, created_at
/// DESC`, so a score-only `key < cursor` predicate silently drops every post
/// that *shares* the boundary page's key. Tied keys are ordinary (engagement
/// scores collide constantly), so this is a plain post-loss bug, not an edge
/// case: here p2 and p3 tie at 500 and p3 is the first casualty.
#[tokio::test]
async fn composed_cursor_does_not_drop_posts_sharing_the_boundary_key() {
    let db = CacheDb::open_in_memory().unwrap();
    let p1 = [0xF1u8; 32];
    let p2 = [0xF2u8; 32];
    let p3 = [0xF3u8; 32];
    // p2 and p3 tie on the key; the compound sort breaks the tie by recency,
    // so page 1 = [p1, p2] and page 2 must still yield p3.
    seed_post_with_factor(&db, p1, 1_000_000, 900).await;
    seed_post_with_factor(&db, p2, 3_000_000, 500).await;
    seed_post_with_factor(&db, p3, 2_000_000, 500).await;

    let composition = [entry(LABELER, 1000)];
    let page1 = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 2, &composition)
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(
        page1[1].post_id.as_slice(),
        &p2,
        "p2 wins the tie on recency"
    );

    let page2 = db
        .query_feed_scored(
            &[],
            FilterCombination::All,
            &[],
            cursor_of(&page1),
            2,
            &composition,
        )
        .await
        .unwrap();
    assert_eq!(
        page2.iter().map(|p| p.post_id.clone()).collect::<Vec<_>>(),
        vec![p3.to_vec()],
        "a post tied with the boundary post's key must survive pagination",
    );
}

/// **The canonical trained-topic feed must paginate at all.** A feed whose
/// composition holds only *sealed* tier-1 factors (a `topic:*` model — the
/// flagship personalization case, `topic-factors.md` § Scoring) has no
/// nest-readable bus rows, so the zero-term seam makes **every** nest-side key
/// exactly 0. A score-only `key < 0` cursor then matches nothing and the feed
/// dead-ends after page 1 — the client sees `has_more == false` and the user's
/// trained feed is silently truncated to one page. The compound cursor
/// degenerates to chronological keyset pagination, which is exactly right: with
/// a flat key, `created_at DESC` *is* the order.
#[tokio::test]
async fn a_sealed_only_composition_paginates_to_completion() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [11u8; 32];
    let ids: Vec<[u8; 32]> = (0u8..5).map(|i| [0xA0 + i; 32]).collect();
    for (i, pid) in ids.iter().enumerate() {
        // No `content_scores` row at all: the nest cannot read a sealed factor.
        db.insert_post_index_entry(
            pid,
            &author,
            1_000_000 * (i as i64 + 1),
            false,
            false,
            "fauna",
            &[],
        )
        .await
        .unwrap();
    }

    // The user's feed is dominated by their sealed trained topic.
    let composition = [entry("topic:aabbccddeeff00112233445566778899", 3000)];

    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut cursor: Option<ScoreCursor> = None;
    for _ in 0..5 {
        let page = db
            .query_feed_scored(&[], FilterCombination::All, &[], cursor, 2, &composition)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        seen.extend(page.iter().map(|p| p.post_id.clone()));
        cursor = cursor_of(&page);
    }

    assert_eq!(
        seen.len(),
        5,
        "every post must be reachable through a sealed-only composition's pages (got {seen:?})",
    );
    // Flat key ⇒ the `created_at DESC` tiebreak is the whole order: newest first.
    let expected: Vec<Vec<u8>> = ids.iter().rev().map(|p| p.to_vec()).collect();
    assert_eq!(seen, expected, "a flat key orders by recency");
}
