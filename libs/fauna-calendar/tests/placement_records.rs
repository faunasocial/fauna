//! Round-trip tests for CalPlacementRecord + CalPlacementManifest.

#![cfg(feature = "nest-segments")]

use fauna_calendar::segments::placement::{
    CAL_PLACEMENT_FORMAT_VERSION, CalPlacementManifest, CalPlacementRecord, CalendarState,
    EventPlacement, EventTombstoneRef,
};
use fauna_segment_store::{KindManifest, VersionedManifest};

#[test]
fn provision_calendar_roundtrip() {
    let record = CalPlacementRecord::ProvisionCalendar {
        calendar_id: [0x42; 32],
        encrypted_metadata: vec![1, 2, 3, 4, 5],
    };
    let bytes = record.encode().expect("encode");
    let decoded = CalPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn put_event_roundtrip() {
    let record = CalPlacementRecord::PutEvent {
        calendar_id: [0x42; 32],
        uid_hash: [0xab; 32],
        etag: "abc123".to_string(),
        modseq: 1_000,
        ciphertext_size: 1_024,
        event_id: [0x77; 32],
        encrypted_fauna_ext: Some(vec![9, 9, 9]),
    };
    let bytes = record.encode().expect("encode");
    let decoded = CalPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn delete_event_roundtrip() {
    let record = CalPlacementRecord::DeleteEvent {
        calendar_id: [0x42; 32],
        uid_hash: [0xab; 32],
        modseq: 1_001,
        event_id: [0x77; 32],
        deleted_at: 1_752_000_000,
    };
    let bytes = record.encode().expect("encode");
    let decoded = CalPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn manifest_roundtrip() {
    let manifest = CalPlacementManifest {
        format_version: CAL_PLACEMENT_FORMAT_VERSION,
        kind_manifest: KindManifest::empty(),
        calendars: vec![CalendarState {
            calendar_id: [0x42; 32],
            encrypted_metadata: vec![1, 2, 3, 4, 5],
            highestmodseq: 1_005,
        }],
        events: vec![EventPlacement {
            calendar_id: [0x42; 32],
            uid_hash: [0xab; 32],
            etag: "abc123".to_string(),
            modseq: 1_000,
            ciphertext_size: 1_024,
            event_id: [0x77; 32],
            encrypted_fauna_ext: None,
        }],
        tombstones: vec![EventTombstoneRef {
            calendar_id: [0x42; 32],
            uid_hash: [0xcd; 32],
            modseq: 999,
            event_id: [0x78; 32],
            deleted_at: 1_752_000_000,
        }],
    };
    let bytes = manifest.encode().expect("encode");
    let decoded = CalPlacementManifest::decode(&bytes).expect("decode");
    assert_eq!(manifest, decoded);
}
