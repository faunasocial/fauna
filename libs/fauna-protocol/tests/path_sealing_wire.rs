//! The additive `path_sealed` / `path_hash` wire siblings of the sealed-names-&-paths
//! expand phase (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
//!
//! Two obligations, and they pull in opposite directions — which is why both are
//! pinned here rather than assumed:
//!
//! 1. **A sealed label survives the wire byte-for-byte.** It is an opaque AEAD
//!    envelope; a single flipped byte makes it un-openable, and the nest that
//!    relays it holds no key to notice. So every carrier round-trips exactly.
//! 2. **Absence is a live wire state, not an error**
//!    (`../architecture/version-compatibility.md` § 2, *Wire*: the additive
//!    `#[serde(default)]` discipline). A frame written *without* the siblings
//!    must still decode, with them `None` — the nest withholds both from a
//!    reader outside the set's label audience, and a keyless writer on a
//!    plaintext-resting plane records without them.
//!
//! These are codec-level tests: no nest, no driver — tier_1.

use std::collections::BTreeMap;

use fauna_protocol::filesync::{SnapshotDiffEntry, SnapshotFileEntry, SnapshotModifiedEntry};
use fauna_protocol::folders::SyncConflict;
use fauna_protocol::media::MediaItem;
use fauna_protocol::sync::{SyncChange, SyncChangeRecordRequest, SyncFile};
use fauna_protocol::{ByteBuf, Value, decode_strict as decode, encode_canonical};

/// Stand-in for a real `SealedLabel`'s canonical dag-cbor bytes. The wire treats
/// it as opaque, so arbitrary bytes are the honest fixture — including a 0x00
/// and a 0xFF, the two a sloppy string round-trip would mangle.
fn sealed() -> ByteBuf {
    ByteBuf::from(vec![0xa4, 0x00, 0x61, 0x76, 0xff, 0x01, 0x62, 0x63, 0x74])
}

fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    decode(&encode_canonical(v).expect("encode")).expect("decode")
}

/// Decode a hand-built map that omits the sealed siblings entirely — the
/// withheld projection a non-audience reader receives.
fn decode_without_sealed<T: serde::de::DeserializeOwned>(fields: &[(&str, Value)]) -> T {
    let map = Value::Map(
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
    );
    decode(&encode_canonical(&map).expect("encode"))
        .expect("a peer without the field still decodes")
}

#[test]
fn the_record_request_carries_a_seal_verbatim() {
    let req = SyncChangeRecordRequest {
        folder: "family-documents".into(),
        device_id: "aa".repeat(32),
        path: "2026/eviction_notice.pdf".into(),
        manifest_hash: Some("bb".repeat(32)),
        size_bytes: 42,
        change_type: "create".into(),
        path_sealed: Some(sealed()),
        ..Default::default()
    };
    assert_eq!(roundtrip(&req).path_sealed, Some(sealed()));
}

#[test]
fn every_read_surface_carries_a_seal_verbatim() {
    assert_eq!(
        roundtrip(&SyncChange {
            path_sealed: Some(sealed()),
            ..Default::default()
        })
        .path_sealed,
        Some(sealed())
    );

    assert_eq!(
        roundtrip(&SyncFile {
            path: "a.txt".into(),
            manifest_hash: "cc".repeat(32),
            size_bytes: 1,
            updated_at: 2,
            path_sealed: Some(sealed()),
            ..Default::default()
        })
        .path_sealed,
        Some(sealed())
    );

    // `..Default::default()` rather than an exhaustive literal: this struct
    // grows on the sealing axis slice by slice, and two branches growing it
    // independently then merge cleanly instead of colliding on the grown axis.
    let media = roundtrip(&MediaItem {
        folder: "photos".into(),
        path: "a.jpg".into(),
        size_bytes: 1,
        updated_at: 2,
        source_online: true,
        path_sealed: Some(sealed()),
        // The convergent salt the seal opens under — without it a scrubbed row
        // is unrenderable, so it rides the media plane too.
        path_hash: Some(ByteBuf::from(vec![9u8; 32])),
        ..Default::default()
    });
    assert_eq!(media.path_sealed, Some(sealed()));
    assert_eq!(media.path_hash, Some(ByteBuf::from(vec![9u8; 32])));

    let entry = SnapshotFileEntry {
        path: "a.txt".into(),
        manifest_hash: ByteBuf::from(vec![1, 2, 3]),
        size_bytes: 1,
        mtime: 2,
        mode: 0,
        file_type: "file".into(),
        symlink_target: None,
        path_hash: Some(ByteBuf::from(vec![9; 32])),
        path_sealed: Some(sealed()),
        extra: BTreeMap::new(),
    };
    let back = roundtrip(&entry);
    assert_eq!(back.path_sealed, Some(sealed()));
    assert_eq!(back.path_hash, Some(ByteBuf::from(vec![9; 32])));

    let diff = roundtrip(&SnapshotDiffEntry {
        path: "a.txt".into(),
        size_bytes: 1,
        path_hash: Some(ByteBuf::from(vec![9; 32])),
        path_sealed: Some(sealed()),
        extra: BTreeMap::new(),
    });
    assert_eq!(diff.path_sealed, Some(sealed()));

    let modified = roundtrip(&SnapshotModifiedEntry {
        path: "a.txt".into(),
        old_size: 1,
        new_size: 2,
        path_hash: Some(ByteBuf::from(vec![9; 32])),
        path_sealed: Some(sealed()),
        extra: BTreeMap::new(),
    });
    assert_eq!(modified.path_sealed, Some(sealed()));

    let conflict = roundtrip(&SyncConflict {
        path_sealed: Some(sealed()),
        details_sealed: Some(sealed()),
        path_hash: ByteBuf::from(vec![9; 32]),
        ..Default::default()
    });
    assert_eq!(conflict.path_sealed, Some(sealed()));
    assert_eq!(conflict.details_sealed, Some(sealed()));
}

/// The absent-key half of I2: a frame with no `path_sealed` key at all — the
/// public projection's withheld envelope — must decode, not error, and must
/// not fabricate a value.
#[test]
fn a_frame_without_the_sealed_key_still_decodes_with_no_seal() {
    let change: SyncChange = decode_without_sealed(&[
        ("seq", Value::Integer(7)),
        ("path_hash", Value::String("dd".repeat(32))),
        ("manifest_hash", Value::Null),
        ("size_bytes", Value::Integer(0)),
        ("change_type", Value::String("create".into())),
        ("created_at", Value::Integer(1)),
        ("path", Value::String("a.txt".into())),
        ("device_id", Value::Null),
    ]);
    assert_eq!(change.seq, 7);
    assert_eq!(change.path_sealed, None);

    let req: SyncChangeRecordRequest = decode_without_sealed(&[
        ("folder", Value::String("docs".into())),
        ("device_id", Value::String("aa".repeat(32))),
        ("path", Value::String("a.txt".into())),
        ("size_bytes", Value::Integer(0)),
        ("change_type", Value::String("create".into())),
    ]);
    assert_eq!(req.path, "a.txt");
    assert_eq!(req.path_sealed, None);

    let file: SyncFile = decode_without_sealed(&[
        ("path", Value::String("a.txt".into())),
        ("manifest_hash", Value::String("cc".repeat(32))),
        ("size_bytes", Value::Integer(1)),
        ("updated_at", Value::Integer(2)),
    ]);
    assert_eq!(file.path_sealed, None);
}

/// The forward half: a `None` seal must be **absent** from the encoding, not an
/// encoded nil. An old peer decoding a nil into a non-`Option` mirror field
/// would fault, and every one of these carriers is `skip_serializing_if`.
#[test]
fn an_absent_seal_is_omitted_from_the_wire_not_encoded_as_nil() {
    let bytes = encode_canonical(&SyncChange::default()).unwrap();
    let as_map: Value = decode(&bytes).unwrap();
    let Value::Map(m) = as_map else {
        panic!("a SyncChange encodes as a map")
    };
    assert!(
        !m.contains_key("path_sealed"),
        "a `None` seal must not put a key on the wire at all, got: {:?}",
        m.keys().collect::<Vec<_>>()
    );
}
