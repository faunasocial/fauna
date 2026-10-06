//! Integration test for MailPlacementSegmentManager round-trip.

use std::sync::Arc;

use fauna_mail::segments::placement::MailPlacementRecord;
use fauna_nest::segments::mail_placement::MailPlacementSegmentManager;
use tempfile::TempDir;

#[tokio::test]
async fn append_and_read_back_records() {
    let tmp = TempDir::new().expect("tmp");
    let manager = Arc::new(MailPlacementSegmentManager::new(tmp.path().to_path_buf()));

    let actor = [0x42u8; 32];
    let record = MailPlacementRecord::Append {
        mailbox: "INBOX".to_string(),
        uid: 42,
        modseq: 1_000,
        flags: vec!["\\Seen".to_string()],
        content_record_id: vec![1, 2, 3, 4],
        internal_date: 1_715_000_000,
    };

    manager.append_event(&actor, &record).await.expect("append");

    let manifest = manager
        .current_manifest(&actor)
        .await
        .expect("current_manifest");
    assert_eq!(manifest.placements.len(), 1);
    assert_eq!(manifest.placements[0].mailbox, "INBOX");
    assert_eq!(manifest.placements[0].uid, 42);
}
