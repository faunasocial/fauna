//! Integration tests for content labels and label-based feed filtering.

use fauna_nest::db::CacheDb;

#[tokio::test]
async fn upsert_and_get_labels() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id_hex = "aa".repeat(32);
    let scanner_id = [1u8; 32];
    let classifier_id = [2u8; 32];

    // Insert a spam label
    db.upsert_content_label(
        "post",
        &post_id_hex,
        "spam",
        0.85,
        1, // TextClassifier
        &classifier_id,
        1,
        0,
        None,
        None,
        1000000,
        &scanner_id,
        &[0u8; 64],
    )
    .await
    .unwrap();

    // Retrieve it
    let labels = db.get_content_labels("post", &post_id_hex).await.unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].category, "spam");
    assert!((labels[0].confidence - 0.85).abs() < f64::EPSILON);
}

#[tokio::test]
async fn upsert_updates_on_conflict() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id_hex = "bb".repeat(32);
    let scanner_id = [1u8; 32];
    let classifier_id = [2u8; 32];

    // Insert with confidence 0.5
    db.upsert_content_label(
        "post",
        &post_id_hex,
        "spam",
        0.5,
        1,
        &classifier_id,
        1,
        0,
        None,
        None,
        1000000,
        &scanner_id,
        &[0u8; 64],
    )
    .await
    .unwrap();

    // Upsert with higher confidence
    db.upsert_content_label(
        "post",
        &post_id_hex,
        "spam",
        0.95,
        1,
        &classifier_id,
        1,
        0,
        None,
        None,
        2000000,
        &scanner_id,
        &[0u8; 64],
    )
    .await
    .unwrap();

    let labels = db.get_content_labels("post", &post_id_hex).await.unwrap();
    assert_eq!(labels.len(), 1);
    assert!((labels[0].confidence - 0.95).abs() < f64::EPSILON);
}

#[tokio::test]
async fn has_label_above_threshold() {
    let db = CacheDb::open_in_memory().unwrap();

    let post_id_hex = "cc".repeat(32);
    let scanner_id = [1u8; 32];
    let classifier_id = [2u8; 32];

    db.upsert_content_label(
        "post",
        &post_id_hex,
        "spam",
        0.7,
        1,
        &classifier_id,
        1,
        0,
        None,
        None,
        1000000,
        &scanner_id,
        &[0u8; 64],
    )
    .await
    .unwrap();

    assert!(
        db.has_label_above("post", &post_id_hex, "spam", 0.5)
            .await
            .unwrap()
    );
    assert!(
        db.has_label_above("post", &post_id_hex, "spam", 0.7)
            .await
            .unwrap()
    );
    assert!(
        !db.has_label_above("post", &post_id_hex, "spam", 0.8)
            .await
            .unwrap()
    );
    assert!(
        !db.has_label_above("post", &post_id_hex, "nsfw", 0.1)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn label_below_filter_excludes_labeled_posts() {
    use fauna_core::scoring::{FilterCombination, FilterRule};

    let db = CacheDb::open_in_memory().unwrap();

    let post_a = [0xAAu8; 32];
    let post_b = [0xBBu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    // Create two posts in post_index
    db.insert_post_index_entry(&post_a, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post_b, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Label post_a as spam (0.8 confidence)
    let post_a_hex = hex::encode(post_a).to_lowercase();
    db.upsert_content_label(
        "post",
        &post_a_hex,
        "spam",
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

    // Query with LabelBelow { spam, 0.5 } — should exclude post_a
    let results = db
        .query_feed(
            &[FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 500,
            }],
            FilterCombination::All,
            &[],
            None,
            100,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &post_b);
}

#[tokio::test]
async fn label_above_filter_includes_only_labeled_posts() {
    use fauna_core::scoring::{FilterCombination, FilterRule};

    let db = CacheDb::open_in_memory().unwrap();

    let post_a = [0xAAu8; 32];
    let post_b = [0xBBu8; 32];
    let author = [1u8; 32];
    let now = 1000000i64;

    db.insert_post_index_entry(&post_a, &author, now, false, false, "fauna", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&post_b, &author, now + 1, false, false, "fauna", &[])
        .await
        .unwrap();

    // Label post_a as spam
    let post_a_hex = hex::encode(post_a).to_lowercase();
    db.upsert_content_label(
        "post",
        &post_a_hex,
        "spam",
        0.9,
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

    // Query with LabelAbove { spam, 0.5 } — should include ONLY post_a
    let results = db
        .query_feed(
            &[FilterRule::LabelAbove {
                category: "spam".into(),
                min_confidence_permille: 500,
            }],
            FilterCombination::All,
            &[],
            None,
            100,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].post_id.as_slice(), &post_a);
}

#[tokio::test]
async fn email_auth_labels_filter_inbox_feed() {
    use fauna_core::scoring::{FilterCombination, FilterRule};

    let db = CacheDb::open_in_memory().unwrap();

    let author = [1u8; 32];
    let now = 1000000i64;

    // Create 3 posts representing emails (stored as posts with structured body)
    let email_a = [0x01u8; 32]; // will be labeled spam
    let email_b = [0x02u8; 32]; // will be labeled auth-fail
    let email_c = [0x03u8; 32]; // clean

    db.insert_post_index_entry(&email_a, &author, now, false, false, "smtp", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&email_b, &author, now + 1, false, false, "smtp", &[])
        .await
        .unwrap();
    db.insert_post_index_entry(&email_c, &author, now + 2, false, false, "smtp", &[])
        .await
        .unwrap();

    // Label email_a as spam
    db.upsert_content_label(
        "post",
        &hex::encode(email_a).to_lowercase(),
        "spam",
        0.9,
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

    // Label email_b as auth-fail
    db.upsert_content_label(
        "post",
        &hex::encode(email_b).to_lowercase(),
        "auth-fail",
        1.0,
        4,
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

    // Inbox view: exclude spam > 0.5
    let inbox = db
        .query_feed(
            &[FilterRule::LabelBelow {
                category: "spam".into(),
                max_confidence_permille: 500,
            }],
            FilterCombination::All,
            &[],
            None,
            100,
        )
        .await
        .unwrap();
    // Should have email_b and email_c (email_a is spam)
    assert_eq!(inbox.len(), 2);

    // Junk view: only spam > 0.5
    let junk = db
        .query_feed(
            &[FilterRule::LabelAbove {
                category: "spam".into(),
                min_confidence_permille: 500,
            }],
            FilterCombination::All,
            &[],
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(junk.len(), 1);
    assert_eq!(junk[0].post_id.as_slice(), &email_a);

    // Combined filter: exclude spam AND auth-fail
    let strict_inbox = db
        .query_feed(
            &[
                FilterRule::LabelBelow {
                    category: "spam".into(),
                    max_confidence_permille: 500,
                },
                FilterRule::LabelBelow {
                    category: "auth-fail".into(),
                    max_confidence_permille: 500,
                },
            ],
            FilterCombination::All,
            &[],
            None,
            100,
        )
        .await
        .unwrap();
    // Only email_c passes both filters
    assert_eq!(strict_inbox.len(), 1);
    assert_eq!(strict_inbox[0].post_id.as_slice(), &email_c);
}
