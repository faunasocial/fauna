//! Integration tests verifying that DB methods support the DM behavior-tracking use case.

use fauna_nest::db::CacheDb;

#[tokio::test]
async fn dm_delivery_records_sender_behavior() {
    let db = CacheDb::open_in_memory().unwrap();
    let sender = [1u8; 32];
    let now = fauna_core::data::Timestamp::now().0 as i64;

    db.record_sender_event(&sender, "dm_sent", Some(&[2u8; 32]), now)
        .await
        .unwrap();
    db.record_sender_event(&sender, "dm_sent", Some(&[3u8; 32]), now)
        .await
        .unwrap();

    let profile = db
        .get_behavioral_profile(&sender, &[99u8; 32], now)
        .await
        .unwrap();
    assert_eq!(profile.unique_dm_recipients_1h, 2);
}

// `channel_creation_records_behavior` was deleted with the `channel_created`
// event type and the `channel_create_rate_1h` profile field. It was the only writer that event ever had, and the field it proved
// was never read by the scorer — so the test asserted a closed loop between two
// dead ends while reading as coverage of a live signal. If channel-creation
// rate is ever wanted as a real input, it needs a production writer and a
// scoring rule landed together; see `db/sender_behavior.rs`'s module doc.
