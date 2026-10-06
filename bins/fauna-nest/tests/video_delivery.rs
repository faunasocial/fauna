//! Integration tests for delivery receipt storage.

#[tokio::test]
async fn record_and_list_delivery_receipts() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();

    let content_hash = vec![0xAA; 32];
    let server_nest = [1u8; 32];
    let requesting_nest = [2u8; 32];
    let bytes_served: u64 = 5_000_000;

    db.record_peer_delivery_receipt(&content_hash, &server_nest, &requesting_nest, bytes_served)
        .await
        .unwrap();

    let receipts = db
        .list_delivery_receipts_for_content(&content_hash)
        .await
        .unwrap();

    assert_eq!(receipts.len(), 1);
    let r = &receipts[0];
    assert_eq!(r.content_hash, content_hash);
    assert_eq!(r.server_nest, server_nest);
    assert_eq!(r.requesting_nest, requesting_nest);
    assert_eq!(r.bytes_served, bytes_served);
    assert!(r.created_at > 0);
}

#[tokio::test]
async fn multiple_receipts_for_same_content() {
    let db = fauna_nest::db::CacheDb::open_in_memory().unwrap();

    let content_hash = vec![0xBB; 32];
    let server_nest = [3u8; 32];
    let nest_a = [4u8; 32];
    let nest_b = [5u8; 32];

    db.record_peer_delivery_receipt(&content_hash, &server_nest, &nest_a, 1_000_000)
        .await
        .unwrap();
    db.record_peer_delivery_receipt(&content_hash, &server_nest, &nest_b, 2_000_000)
        .await
        .unwrap();

    let receipts = db
        .list_delivery_receipts_for_content(&content_hash)
        .await
        .unwrap();

    assert_eq!(receipts.len(), 2);
    // Most recent first (DESC order)
    assert_eq!(receipts[0].requesting_nest, nest_b);
    assert_eq!(receipts[1].requesting_nest, nest_a);
}
