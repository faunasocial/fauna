//! Integration tests for cache-on-fetch delivery source tracking.

#[tokio::test]
async fn delivery_sources_tracked_for_cache_fetch() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();
    let hash = [0xAA; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;

    // Record that a remote nest served this content
    db.record_delivery_receipt(&hash, "https://source-nest.example", 1024, now)
        .await
        .unwrap();

    // Should be able to look up sources
    let sources = db.get_delivery_sources(&hash).await.unwrap();
    assert!(!sources.is_empty());
    assert_eq!(sources[0].nest_url, "https://source-nest.example");
}
