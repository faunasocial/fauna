//! Integration tests for MinReplies and MinReposts feed filter rules.

use fauna_core::scoring::{FilterCombination, FilterRule};
use fauna_nest::db::CacheDb;

#[tokio::test]
async fn min_replies_filter() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [1u8; 32];

    // Create two posts via index entries (creates content + content_meta rows)
    let p1 = [0xA1u8; 32];
    let p2 = [0xA2u8; 32];
    db.insert_post_index_entry(&p1, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p2, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give post1 three replies
    db.increment_engagement_count(&p1, "reply").await.unwrap();
    db.increment_engagement_count(&p1, "reply").await.unwrap();
    db.increment_engagement_count(&p1, "reply").await.unwrap();

    // Give post2 one reply (below threshold)
    db.increment_engagement_count(&p2, "reply").await.unwrap();

    // Query with MinReplies { count: 2 }
    let results = db
        .query_feed(
            &[FilterRule::MinReplies { count: 2 }],
            FilterCombination::All,
            &[],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &p1);
}

#[tokio::test]
async fn min_reposts_filter() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [1u8; 32];

    let p1 = [0xB1u8; 32];
    let p2 = [0xB2u8; 32];
    db.insert_post_index_entry(&p1, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p2, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give post1 five reposts
    for _ in 0..5 {
        db.increment_engagement_count(&p1, "repost").await.unwrap();
    }

    // Give post2 one repost (below threshold)
    db.increment_engagement_count(&p2, "repost").await.unwrap();

    // Query with MinReposts { count: 3 }
    let results = db
        .query_feed(
            &[FilterRule::MinReposts { count: 3 }],
            FilterCombination::All,
            &[],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &p1);
}

#[tokio::test]
async fn min_replies_filter_for_authors() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [1u8; 32];

    let p1 = [0xC1u8; 32];
    let p2 = [0xC2u8; 32];
    db.insert_post_index_entry(&p1, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p2, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give post1 three replies
    db.increment_engagement_count(&p1, "reply").await.unwrap();
    db.increment_engagement_count(&p1, "reply").await.unwrap();
    db.increment_engagement_count(&p1, "reply").await.unwrap();

    // Query scoped to author with MinReplies { count: 2 }
    let results = db
        .query_feed_for_authors(
            &[FilterRule::MinReplies { count: 2 }],
            FilterCombination::All,
            &[],
            &[author.to_vec()],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &p1);
}

#[tokio::test]
async fn min_reposts_filter_for_authors() {
    let db = CacheDb::open_in_memory().unwrap();
    let author = [1u8; 32];

    let p1 = [0xD1u8; 32];
    let p2 = [0xD2u8; 32];
    db.insert_post_index_entry(&p1, &author, 1_000_000, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&p2, &author, 2_000_000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Give post1 four reposts
    for _ in 0..4 {
        db.increment_engagement_count(&p1, "repost").await.unwrap();
    }

    // Query scoped to author with MinReposts { count: 3 }
    let results = db
        .query_feed_for_authors(
            &[FilterRule::MinReposts { count: 3 }],
            FilterCombination::All,
            &[],
            &[author.to_vec()],
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &p1);
}
