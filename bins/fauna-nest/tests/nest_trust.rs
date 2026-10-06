//! Integration tests for nest engagement stats, trust scoring, and anomaly detection.

use fauna_core::engagement::AnomalyFlag;

#[tokio::test]
async fn track_nest_engagement_stats() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();
    let nest_id = [0x01u8; 32];

    // Record 4 events: 2 like, 1 view_complete, 1 repost
    db.record_nest_engagement(&nest_id, "like", 1000)
        .await
        .unwrap();
    db.record_nest_engagement(&nest_id, "like", 2000)
        .await
        .unwrap();
    db.record_nest_engagement(&nest_id, "view_complete", 3000)
        .await
        .unwrap();
    db.record_nest_engagement(&nest_id, "repost", 4000)
        .await
        .unwrap();

    let stats = db.get_nest_engagement_stats(&nest_id).await.unwrap();
    assert_eq!(stats.total_events, 4);
    assert_eq!(stats.distinct_event_types, 3);
}

#[tokio::test]
async fn nest_trust_score_defaults_to_one() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();
    let unknown_nest = [0xFFu8; 32];

    let trust = db.get_nest_trust(&unknown_nest).await.unwrap();
    assert!((trust - 1.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn set_and_get_nest_trust() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();
    let nest_id = [0x02u8; 32];

    db.set_nest_trust(&nest_id, 0.5, "test demotion")
        .await
        .unwrap();
    let trust = db.get_nest_trust(&nest_id).await.unwrap();
    assert!((trust - 0.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn detect_volume_anomaly() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();

    // High-volume nest: 1000 events of the same type
    let high_nest = [0x10u8; 32];
    for i in 0..1000 {
        db.record_nest_engagement(&high_nest, "like", i)
            .await
            .unwrap();
    }

    let flags = db
        .detect_engagement_anomalies(&high_nest, 100)
        .await
        .unwrap();
    assert!(flags.contains(&AnomalyFlag::VolumeAnomaly));
    // Also uniform timing since only 1 event type with >10 events
    assert!(flags.contains(&AnomalyFlag::UniformTiming));

    // Low-volume nest: 10 events
    let low_nest = [0x11u8; 32];
    for i in 0..10 {
        db.record_nest_engagement(&low_nest, "like", i)
            .await
            .unwrap();
    }

    let flags = db
        .detect_engagement_anomalies(&low_nest, 100)
        .await
        .unwrap();
    assert!(
        flags.is_empty(),
        "10 events should not flag anything with threshold=100"
    );
}
