//! Integration tests for sender_behavior table and behavioral profile DB methods.

use fauna_nest::db::CacheDb;

#[tokio::test]
async fn record_dm_sent_and_query_profile() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let recipient_a = [2u8; 32];
    let recipient_b = [3u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;

    db.record_sender_event(&sender, "dm_sent", Some(&recipient_a), now)
        .await
        .unwrap();
    db.record_sender_event(&sender, "dm_sent", Some(&recipient_b), now)
        .await
        .unwrap();

    let profile = db
        .get_behavioral_profile(&sender, &[4u8; 32], now)
        .await
        .unwrap();
    assert_eq!(profile.unique_dm_recipients_1h, 2);
    assert_eq!(profile.unique_dm_recipients_24h, 2);
}

#[tokio::test]
async fn behavioral_anomaly_score_from_db() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;

    for i in 0..25u8 {
        let mut recipient = [0u8; 32];
        recipient[0] = i;
        db.record_sender_event(&sender, "dm_sent", Some(&recipient), now)
            .await
            .unwrap();
    }

    let profile = db
        .get_behavioral_profile(&sender, &[99u8; 32], now)
        .await
        .unwrap();
    assert_eq!(profile.unique_dm_recipients_1h, 25);

    let score = fauna_core::behavioral::compute_behavioral_anomaly(&profile);
    assert!(
        score >= 0.3,
        "high fanout should produce score >= 0.3, got {score}"
    );
}
