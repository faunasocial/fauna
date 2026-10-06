//! End-to-end test of the label plane: a scoring position's verdict → stored
//! `content_labels` → feed filter. The classify step runs OUTSIDE the nest
//! process (perimeter / client / granted holder — content-scoring.md § The
//! placement matrix); these tests call the shared `classify_text` directly to
//! stand in for that position, then pin the nest-side consume path.

use fauna_nest::db::CacheDb;

#[tokio::test]
async fn heuristic_labels_stored_for_spammy_post() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id = [0xAAu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;
    db.insert_post_index_entry(&post_id, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();

    // Stand in for a scoring position (client / granted holder): run the
    // shared classifier there, store the resulting labels on the nest
    let text = "BUY NOW!!! CLICK HERE for FREE MONEY!!! Limited time!!! https://a.com https://b.com https://c.com https://d.com";
    let results = fauna_nest::text_heuristic::classify_text(text);
    assert!(!results.is_empty(), "heuristic should detect spam");

    let post_id_hex = hex::encode(post_id).to_lowercase();
    let heuristic_cid = fauna_nest::text_heuristic::classifier_id();
    for result in &results {
        db.upsert_content_label(
            "post",
            &post_id_hex,
            &result.category,
            result.confidence,
            1, // TextClassifier
            &heuristic_cid,
            1,
            0,
            None,
            None,
            now,
            &[0u8; 32],
            &[],
        )
        .await
        .unwrap();
    }

    // Verify labels are stored
    let labels = db.get_content_labels("post", &post_id_hex).await.unwrap();
    assert!(!labels.is_empty());
    assert!(labels.iter().any(|l| l.category == "spam"));
}

#[tokio::test]
async fn spam_labels_filter_from_feed() {
    use fauna_core::scoring::{FilterCombination, FilterRule};

    let db = CacheDb::open_in_memory().unwrap();

    let spam_post = [0xBBu8; 32];
    let clean_post = [0xCCu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    db.insert_post_index_entry(&spam_post, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&clean_post, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Classify and label the spam post
    let text = "BUY NOW!!! FREE MONEY!!! CLICK HERE!!! ACT NOW!!! https://a.com https://b.com https://c.com https://d.com";
    let results = fauna_nest::text_heuristic::classify_text(text);
    let spam_post_hex = hex::encode(spam_post).to_lowercase();
    let heuristic_cid = fauna_nest::text_heuristic::classifier_id();
    for result in &results {
        db.upsert_content_label(
            "post",
            &spam_post_hex,
            &result.category,
            result.confidence,
            1,
            &heuristic_cid,
            1,
            0,
            None,
            None,
            now,
            &[0u8; 32],
            &[],
        )
        .await
        .unwrap();
    }

    // Feed with LabelBelow spam 0.2 should exclude the spam post
    let feed = db
        .query_feed(
            &[FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 200,
            }],
            FilterCombination::All,
            &[],
            None,
            100,
        )
        .await
        .unwrap();

    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].post_id.as_slice(), &clean_post);
}

#[tokio::test]
async fn clean_text_produces_no_labels() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id = [0xDDu8; 32];
    let author = [1u8; 32];
    db.insert_post_index_entry(&post_id, &author, 1000000, false, false, "fauna", &[])
        .await
        .unwrap();

    // Clean text should produce no labels
    let text = "Just had a great day hiking in the mountains. The views were amazing!";
    let results = fauna_nest::text_heuristic::classify_text(text);
    assert!(results.is_empty());

    // Verify no labels stored
    let post_id_hex = hex::encode(post_id).to_lowercase();
    let labels = db.get_content_labels("post", &post_id_hex).await.unwrap();
    assert!(labels.is_empty());
}
