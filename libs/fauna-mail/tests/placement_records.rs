//! Round-trip tests for MailPlacementRecord + MailPlacementManifest.

#![cfg(feature = "nest-segments")]

use fauna_mail::segments::placement::{
    MAIL_PLACEMENT_FORMAT_VERSION, MailPlacementManifest, MailPlacementRecord, MailboxState,
    RecordPlacement, TombstoneRef,
};
use fauna_segment_store::{KindManifest, VersionedManifest};

#[test]
fn append_record_roundtrip() {
    let record = MailPlacementRecord::Append {
        mailbox: "INBOX".to_string(),
        uid: 42,
        modseq: 1_000,
        flags: vec!["\\Seen".to_string(), "\\Answered".to_string()],
        content_record_id: vec![1, 2, 3, 4],
        internal_date: 1_715_000_000,
    };
    let bytes = record.encode().expect("encode");
    let decoded = MailPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn store_flags_roundtrip() {
    let record = MailPlacementRecord::StoreFlags {
        mailbox: "INBOX".to_string(),
        uid_set: vec![42, 43, 44],
        modseq: 1_001,
        before_flags: vec!["\\Unseen".to_string()],
        after_flags: vec!["\\Seen".to_string()],
    };
    let bytes = record.encode().expect("encode");
    let decoded = MailPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn move_roundtrip() {
    let record = MailPlacementRecord::Move {
        src_mailbox: "INBOX".to_string(),
        src_uid_set: vec![42],
        dst_mailbox: "Archive".to_string(),
        dst_uid_set: vec![17],
        modseq_src: 1_002,
        modseq_dst: 1_003,
        deleted_at: 1_752_000_000,
    };
    let bytes = record.encode().expect("encode");
    let decoded = MailPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn expunge_roundtrip() {
    let record = MailPlacementRecord::Expunge {
        mailbox: "Trash".to_string(),
        uid_set: vec![100, 101, 102],
        modseq: 2_000,
        deleted_at: 1_752_000_000,
    };
    let bytes = record.encode().expect("encode");
    let decoded = MailPlacementRecord::decode(&bytes).expect("decode");
    assert_eq!(record, decoded);
}

#[test]
fn manifest_roundtrip() {
    let manifest = MailPlacementManifest {
        format_version: MAIL_PLACEMENT_FORMAT_VERSION,
        kind_manifest: KindManifest::empty(),
        mailboxes: vec![MailboxState {
            name: "INBOX".to_string(),
            uid_validity: 0x4242_0001,
            uid_next: 43,
            highestmodseq: 1_005,
            attrs: vec!["\\Inbox".to_string()],
        }],
        placements: vec![RecordPlacement {
            mailbox: "INBOX".to_string(),
            uid: 42,
            modseq: 1_000,
            flags: vec!["\\Seen".to_string()],
            content_record_id: vec![1, 2, 3, 4],
            internal_date: 1_715_000_000,
        }],
        tombstones: vec![TombstoneRef {
            mailbox: "INBOX".to_string(),
            uid: 5,
            modseq: 999,
            deleted_at: 1_752_000_000,
        }],
        subscriptions: vec!["INBOX".to_string(), "Archive".to_string()],
    };
    let bytes = manifest.encode().expect("encode");
    let decoded = MailPlacementManifest::decode(&bytes).expect("decode");
    assert_eq!(manifest, decoded);
}
