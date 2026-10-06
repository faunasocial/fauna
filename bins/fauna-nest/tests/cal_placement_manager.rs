//! Integration test for CalPlacementSegmentManager round-trip.

use std::sync::Arc;

use fauna_calendar::segments::placement::CalPlacementRecord;
use fauna_nest::segments::cal_placement::CalPlacementSegmentManager;
use tempfile::TempDir;

#[tokio::test]
async fn append_and_read_back_records() {
    let tmp = TempDir::new().expect("tmp");
    let manager = Arc::new(CalPlacementSegmentManager::new(tmp.path().to_path_buf()));

    let actor = [0x42u8; 32];
    let calendar = [0x55u8; 32];
    let uid_hash = [0x66u8; 32];
    let record = CalPlacementRecord::PutEvent {
        calendar_id: calendar,
        uid_hash,
        etag: "etag-abc".to_string(),
        modseq: 1_000,
        ciphertext_size: 256,
        event_id: [0x77u8; 32],
        encrypted_fauna_ext: None,
    };

    manager.append_event(&actor, &record).await.expect("append");

    let manifest = manager
        .current_manifest(&actor)
        .await
        .expect("current_manifest");
    assert_eq!(manifest.events.len(), 1);
    assert_eq!(manifest.events[0].calendar_id, calendar);
    assert_eq!(manifest.events[0].uid_hash, uid_hash);
    assert_eq!(manifest.events[0].etag, "etag-abc");
    assert_eq!(manifest.events[0].modseq, 1_000);
    assert_eq!(manifest.events[0].ciphertext_size, 256);
}
