//! The two wire shapes of a 32-byte actor id, pinned — and the refusal a
//! caller gets for the retired integer-array spelling.
//!
//! Every raw-byte field is a CBOR byte string (major 2): a `ByteBuf` field,
//! an attributed `[u8; 32]`, and a byte-string newtype (`ActorId`) alike. The
//! one other spelling is the `ActorIdHex` text form, a text string of 64 hex
//! chars (major 3) on JSON-bound push payloads. The array of 32 integers a
//! plain-derive `[u8; 32]` would emit is not a shape but a defect: refused at
//! strict decode wherever an id is expected, and refused at encode by the
//! debug guard in `fauna_cbor::encode_canonical`. Decode-widening stays
//! rejected because one value would then have two CIDs. The decision and the
//! rule are owned by `docs/goal/architecture/serialization.md` § Canonical
//! IPLD dag-cbor, "Fixed-size byte arrays"; this file is its Rust witness,
//! the sibling of `wsrpc_nil_container_contract.rs` for the nil-container
//! trap.

use std::collections::BTreeMap;

use fauna_cbor::DecodeError;
use fauna_core::identity::ActorId;
use fauna_protocol::admin::AdminFolderCreateRequest;
use fauna_protocol::filesync::SnapshotCreateMessageKindRequest;
use fauna_protocol::push_events::KnockPayload;
use fauna_protocol::subscriptions::OffersListRequest;
use fauna_protocol::{ByteBuf, Value, decode_strict, encode_canonical};

/// One actor id, every byte `0x11`.
const RAW: [u8; 32] = [0x11; 32];

/// CBOR initial bytes: byte string of length 32, array of length 32, text
/// string of length 64.
const BYTE_STRING_32: [u8; 2] = [0x58, 0x20];
const ARRAY_32: [u8; 2] = [0x98, 0x20];
const TEXT_64: [u8; 2] = [0x78, 0x40];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn byte_string_id() -> Vec<u8> {
    let mut v = BYTE_STRING_32.to_vec();
    v.extend_from_slice(&RAW);
    v
}

fn map(entries: Vec<(&str, Value)>) -> Vec<u8> {
    let m = Value::Map(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>(),
    );
    encode_canonical(&m).expect("encode canonical map").to_vec()
}

fn array_spelling() -> Value {
    Value::List(RAW.iter().map(|b| Value::Integer(i128::from(*b))).collect())
}

fn schema_mismatch<T: serde::de::DeserializeOwned + std::fmt::Debug>(bytes: &[u8]) -> String {
    match decode_strict::<T>(bytes) {
        Err(DecodeError::SchemaMismatch(msg)) => msg,
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

// ── the two shapes, as the types emit them ────────────────────────────────

#[test]
fn actor_id_rides_as_a_32_byte_string() {
    let bytes = encode_canonical(&ActorId(RAW)).unwrap();
    assert_eq!(bytes, byte_string_id());
    assert!(!contains(&bytes, &ARRAY_32));
}

#[test]
fn an_actor_id_field_and_a_byte_buf_field_ride_identically() {
    let req = OffersListRequest {
        author_id: ActorId(RAW),
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).unwrap();
    assert!(contains(&bytes, &byte_string_id()), "{bytes:02x?}");
    assert!(!contains(&bytes, &ARRAY_32), "{bytes:02x?}");

    let req = AdminFolderCreateRequest {
        name: "folder".into(),
        actor_id: ByteBuf::from(RAW.to_vec()),
        ..Default::default()
    };
    let bytes = encode_canonical(&req).unwrap();
    assert!(contains(&bytes, &byte_string_id()), "{bytes:02x?}");
    assert!(!contains(&bytes, &ARRAY_32), "{bytes:02x?}");
}

#[test]
fn actor_id_hex_rides_as_a_text_string() {
    let payload = KnockPayload {
        sender_id: hex::encode(RAW),
        summary: "hi".into(),
        ..Default::default()
    };
    let bytes = encode_canonical(&payload).unwrap();
    let mut expect = TEXT_64.to_vec();
    expect.extend_from_slice(hex::encode(RAW).as_bytes());
    assert!(contains(&bytes, &expect), "{bytes:02x?}");
    assert!(!contains(&bytes, &ARRAY_32), "{bytes:02x?}");
    assert!(!contains(&bytes, &BYTE_STRING_32), "{bytes:02x?}");
}

// ── an ActorId-typed field decodes the byte string and refuses the rest ───

#[test]
fn actor_id_field_decodes_the_byte_string_spelling() {
    let bytes = map(vec![
        ("kind", Value::String("mail".into())),
        ("actor_id", Value::Bytes(RAW.to_vec())),
    ]);
    let req: SnapshotCreateMessageKindRequest = decode_strict(&bytes).unwrap();
    assert_eq!(req.actor_id, Some(ActorId(RAW)));

    let bytes = map(vec![("author_id", Value::Bytes(RAW.to_vec()))]);
    let req: OffersListRequest = decode_strict(&bytes).unwrap();
    assert_eq!(req.author_id, ActorId(RAW));
}

#[test]
fn actor_id_field_refuses_the_array_spelling_naming_the_rule() {
    // The retired plain-derive spelling, sent to an `ActorId` field.
    let bytes = map(vec![
        ("kind", Value::String("mail".into())),
        ("actor_id", array_spelling()),
    ]);
    let msg = schema_mismatch::<SnapshotCreateMessageKindRequest>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x98 (array)"),
        "{msg}"
    );
    assert!(msg.contains("Fixed-size byte arrays"), "{msg}");

    let bytes = map(vec![("author_id", array_spelling())]);
    let msg = schema_mismatch::<OffersListRequest>(&bytes);
    assert!(msg.contains("got 0x98 (array)"), "{msg}");
}

#[test]
fn actor_id_field_refuses_the_hex_text_spelling() {
    let bytes = map(vec![("author_id", Value::String(hex::encode(RAW)))]);
    let msg = schema_mismatch::<OffersListRequest>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x78 (text string)"),
        "{msg}"
    );
}

#[test]
fn actor_id_field_refuses_a_wrong_width_byte_string() {
    let bytes = map(vec![("author_id", Value::Bytes(RAW[..31].to_vec()))]);
    assert!(decode_strict::<OffersListRequest>(&bytes).is_err());
}

// ── and a ByteBuf field refuses the array spelling the same way ───────────

#[test]
fn byte_buf_field_refuses_the_array_spelling_naming_the_rule() {
    let bytes = map(vec![
        ("name", Value::String("folder".into())),
        ("actor_id", array_spelling()),
    ]);
    let msg = schema_mismatch::<AdminFolderCreateRequest>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x98 (array)"),
        "{msg}"
    );
    assert!(msg.contains("Fixed-size byte arrays"), "{msg}");
}

// ── a variable-length byte field: the same one shape ──────────────────────
//
// `serialization.md` § Canonical IPLD dag-cbor, "Variable-length byte
// fields": a `Vec<u8>` is a byte string too. The key blob's sealed
// `encrypted_key` (inside a signed payload) stands for every one of them.

use fauna_core::subscription::types::{KemSuiteId, KeyBlobEntry};

/// A 92-byte sealed key (ephemeral key + nonce + 32-byte ciphertext + tag),
/// every byte `0xEE` — a value whose array spelling would cost two bytes each.
const SEALED: [u8; 92] = [0xEE; 92];

fn key_blob_entry() -> KeyBlobEntry {
    KeyBlobEntry {
        subscriber: ActorId(RAW),
        encrypted_key: SEALED.to_vec(),
        suite: KemSuiteId::Classical,
    }
}

#[test]
fn a_sealed_byte_vector_rides_as_one_byte_string() {
    let bytes = encode_canonical(&key_blob_entry()).unwrap();
    let mut want = vec![0x58, 92];
    want.extend_from_slice(&SEALED);
    assert!(contains(&bytes, &want), "{bytes:02x?}");
    assert!(!contains(&bytes, &[0x98, 92]), "{bytes:02x?}");
}

#[test]
fn a_byte_vector_field_refuses_the_array_spelling_naming_the_rule() {
    let bytes = map(vec![
        ("subscriber", Value::Bytes(RAW.to_vec())),
        (
            "encrypted_key",
            Value::List(
                SEALED
                    .iter()
                    .map(|b| Value::Integer(i128::from(*b)))
                    .collect(),
            ),
        ),
    ]);
    let msg = schema_mismatch::<KeyBlobEntry>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x98 (array)"),
        "{msg}"
    );
    assert!(msg.contains("`Vec<u8>`"), "{msg}");

    let bytes = map(vec![
        ("subscriber", Value::Bytes(RAW.to_vec())),
        ("encrypted_key", Value::Bytes(SEALED.to_vec())),
    ]);
    let entry: KeyBlobEntry = decode_strict(&bytes).unwrap();
    assert_eq!(entry.encrypted_key, SEALED.to_vec());
}

#[cfg(debug_assertions)]
#[test]
fn the_encoder_guard_refuses_a_plain_byte_vector_on_a_wire_type() {
    // The regression this pins: a new plain-derive `Vec<u8>` on a wire type
    // fails every test that encodes it, by name.
    #[derive(serde::Serialize)]
    struct NewWireType {
        sealed: Vec<u8>,
    }
    let err = encode_canonical(&NewWireType {
        sealed: SEALED.to_vec(),
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("plain-derive `Vec<u8>` at `sealed`"), "{err}");
    assert!(err.contains("Variable-length byte fields"), "{err}");
}
