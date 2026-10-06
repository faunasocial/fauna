//! Integration test for HasLabel convenience filter rule.

use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_nest::db::CacheDb;

#[tokio::test]
async fn has_label_filter_matches_labeled_posts() {
    let db = CacheDb::open_in_memory().unwrap();

    let author = [1u8; 32];
    let now = 1_000_000i64;

    // Post 1: will be labeled "animal/cat" at 0.95 confidence
    let post1 = [0xAAu8; 32];
    db.insert_post_index_entry(&post1, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();

    // Post 2: NOT labeled
    let post2 = [0xBBu8; 32];
    db.insert_post_index_entry(&post2, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Label only post1
    let p1_hex = hex::encode(post1).to_lowercase();
    db.upsert_content_label(
        "post",
        &p1_hex,
        "animal/cat",
        0.95,
        1,
        &[2u8; 32],
        1,
        0,
        None,
        None,
        now,
        &[3u8; 32],
        &[0u8; 64],
    )
    .await
    .unwrap();

    // Query with HasLabel { category: "animal/cat" } — default threshold 0.5
    let results = db
        .query_feed(
            &[FilterRule::HasLabel {
                category: "animal/cat".to_string(),
            }],
            FilterCombination::All,
            &[],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &post1);
}

#[tokio::test]
async fn has_label_filter_ignores_low_confidence() {
    let db = CacheDb::open_in_memory().unwrap();

    let author = [1u8; 32];
    let now = 1_000_000i64;

    // Post with a low-confidence label (below 0.5 threshold)
    let post = [0xCCu8; 32];
    db.insert_post_index_entry(&post, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();

    let p_hex = hex::encode(post).to_lowercase();
    db.upsert_content_label(
        "post",
        &p_hex,
        "animal/cat",
        0.3,
        1,
        &[2u8; 32],
        1,
        0,
        None,
        None,
        now,
        &[3u8; 32],
        &[0u8; 64],
    )
    .await
    .unwrap();

    // HasLabel defaults to min_confidence 0.5, so 0.3 should NOT match
    let results = db
        .query_feed(
            &[FilterRule::HasLabel {
                category: "animal/cat".to_string(),
            }],
            FilterCombination::All,
            &[],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 0);
}

#[tokio::test]
async fn has_label_filter_works_for_authors_query() {
    let db = CacheDb::open_in_memory().unwrap();

    let author = [1u8; 32];
    let now = 1_000_000i64;

    let post1 = [0xAAu8; 32];
    let post2 = [0xBBu8; 32];
    db.insert_post_index_entry(&post1, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post2, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Label only post1
    let p1_hex = hex::encode(post1).to_lowercase();
    db.upsert_content_label(
        "post",
        &p1_hex,
        "humor/cute",
        0.8,
        1,
        &[2u8; 32],
        1,
        0,
        None,
        None,
        now,
        &[3u8; 32],
        &[0u8; 64],
    )
    .await
    .unwrap();

    // Query scoped to this author with HasLabel
    let results = db
        .query_feed_for_authors(
            &[FilterRule::HasLabel {
                category: "humor/cute".to_string(),
            }],
            FilterCombination::All,
            &[],
            &[author.to_vec()],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &post1);
}
