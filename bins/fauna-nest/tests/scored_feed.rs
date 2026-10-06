//! Integration tests for score-based feed ordering (query_feed_scored).
//!
//! The `engagement` factor scalar (`content_meta.score`) is materialized by the
//! counter choke point (`increment_engagement_count`), so these tests drive the
//! counts and read the resulting score directly — no separate recompute step.
//! It is decay-free (all-time accumulated engagement; the decayed sibling is the
//! `trending` factor), so ordering here is by cumulative engagement, not age.

use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_nest::db::{CacheDb, ScoreCursor};

#[tokio::test]
async fn scored_feed_orders_by_score() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [1u8; 32];

    // Post A: older, will get high engagement → high score
    let pa = [0xA0u8; 32];
    // Post B: newer, no engagement → low score
    let pb = [0xB0u8; 32];

    db.insert_post_index_entry(&pa, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&pb, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give post A lots of engagement (the counter materializes the score).
    for _ in 0..10 {
        db.increment_engagement_count(&pa, "like").await.unwrap();
    }
    for _ in 0..5 {
        db.increment_engagement_count(&pa, "reply").await.unwrap();
    }
    for _ in 0..3 {
        db.increment_engagement_count(&pa, "repost").await.unwrap();
    }

    // Verify scores diverged
    let score_a = db.get_content_score(&pa).await.unwrap();
    let score_b = db.get_content_score(&pb).await.unwrap();
    assert!(
        score_a > score_b,
        "post A (engaged) should have higher score than B, got A={score_a} B={score_b}"
    );

    // Chronological query: B first (newer)
    let chrono = db
        .query_feed(&[], FilterCombination::All, &[], None, 10)
        .await
        .unwrap();
    assert_eq!(chrono.len(), 2);
    assert_eq!(
        chrono[0].post_id.as_slice(),
        &pb,
        "chrono: B should come first"
    );
    assert_eq!(
        chrono[1].post_id.as_slice(),
        &pa,
        "chrono: A should come second"
    );

    // Scored query: A first (higher score)
    let scored = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &[])
        .await
        .unwrap();
    assert_eq!(scored.len(), 2);
    assert_eq!(
        scored[0].post_id.as_slice(),
        &pa,
        "scored: A should come first"
    );
    assert_eq!(
        scored[1].post_id.as_slice(),
        &pb,
        "scored: B should come second"
    );

    // Verify score is populated in results
    assert!(scored[0].score.is_some());
    assert!(scored[1].score.is_some());
    assert!(scored[0].score.unwrap() > scored[1].score.unwrap());
}

#[tokio::test]
async fn scored_feed_cursor_pagination() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [2u8; 32];

    // Create 3 posts with different engagement levels
    let p1 = [0xC1u8; 32]; // highest score
    let p2 = [0xC2u8; 32]; // medium score
    let p3 = [0xC3u8; 32]; // lowest score (no engagement)

    db.insert_post_index_entry(&p1, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p2, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p3, &author, 3_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // p1: lots of engagement
    for _ in 0..20 {
        db.increment_engagement_count(&p1, "like").await.unwrap();
    }
    // p2: some engagement
    for _ in 0..5 {
        db.increment_engagement_count(&p2, "like").await.unwrap();
    }
    // p3: no engagement → score stays at the ingestion default (0)

    // First page: limit 2
    let page1 = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 2, &[])
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].post_id.as_slice(), &p1);
    assert_eq!(page1[1].post_id.as_slice(), &p2);

    // Use the last row's keyset cursor (key + its created_at tiebreak) for the
    // second page.
    let page2 = db
        .query_feed_scored(
            &[],
            FilterCombination::All,
            &[],
            Some(ScoreCursor {
                key: page1[1].score.unwrap(),
                created_at: page1[1].created_at,
            }),
            2,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].post_id.as_slice(), &p3);
}

#[tokio::test]
async fn scored_feed_excludes_quarantined() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [3u8; 32];

    let p1 = [0xD1u8; 32];
    let p2 = [0xD2u8; 32];

    db.insert_post_index_entry(&p1, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p2, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give both posts engagement
    for _ in 0..5 {
        db.increment_engagement_count(&p1, "like").await.unwrap();
        db.increment_engagement_count(&p2, "like").await.unwrap();
    }

    // Quarantine p1
    db.set_post_quarantined(&p1, true).await.unwrap();

    let results = db
        .query_feed_scored(&[], FilterCombination::All, &[], None, 10, &[])
        .await
        .unwrap();
    assert_eq!(results.len(), 1, "quarantined post should be excluded");
    assert_eq!(results[0].post_id.as_slice(), &p2);
}

#[tokio::test]
async fn scored_feed_with_filter_rules() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [4u8; 32];

    // Post with media
    let p_media = [0xE1u8; 32];
    // Post without media
    let p_no_media = [0xE2u8; 32];

    db.insert_post_index_entry(&p_media, &author, 1_000_000, true, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p_no_media, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give both engagement
    for _ in 0..5 {
        db.increment_engagement_count(&p_media, "like")
            .await
            .unwrap();
        db.increment_engagement_count(&p_no_media, "like")
            .await
            .unwrap();
    }

    // Filter: has_media = true, ordered by score
    let results = db
        .query_feed_scored(
            &[FilterRule::HasMedia { required: true }],
            FilterCombination::All,
            &[],
            None,
            10,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &p_media);
}
