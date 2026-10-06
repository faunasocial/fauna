//! A schema-mismatch refusal names both CBOR shapes, and for the one pair
//! with a documented cause — an array of integers where a byte string is
//! expected, i.e. a bare `[u8; N]` with a plain derive that reached the wire
//! — points at the owning rule (`docs/goal/architecture/serialization.md`
//! § Canonical IPLD dag-cbor, "Fixed-size byte arrays"). The bare
//! `expected 0x02, got 0x84` text this replaces named neither shape nor rule
//! and cost a whole e2e run to diagnose once. Every fixed-width byte field is
//! a byte string, so the hint is one-directional.

use fauna_cbor::{DecodeError, Value, decode_strict, encode_canonical};
use serde::Deserialize;
use serde_bytes::ByteBuf;

const RULE: &str = "Fixed-size byte arrays";

fn schema_mismatch<T: serde::de::DeserializeOwned + std::fmt::Debug>(bytes: &[u8]) -> String {
    match decode_strict::<T>(bytes) {
        Err(DecodeError::SchemaMismatch(msg)) => msg,
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
#[allow(dead_code)]
struct FixedId(#[serde(with = "serde_bytes")] [u8; 4]);

#[test]
fn array_for_a_byte_buf_field_names_both_shapes_and_the_rule() {
    let bytes = encode_canonical(&Value::List(vec![Value::Integer(0x11); 4])).unwrap();
    let msg = schema_mismatch::<ByteBuf>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x84 (array)"),
        "{msg}"
    );
    assert!(
        msg.contains("bare `[u8; N]` or `Vec<u8>` with a plain derive"),
        "{msg}"
    );
    assert!(msg.contains(RULE), "{msg}");
}

#[test]
fn array_for_a_fixed_width_id_names_both_shapes_and_the_rule() {
    let bytes = encode_canonical(&Value::List(vec![Value::Integer(0x11); 4])).unwrap();
    let msg = schema_mismatch::<FixedId>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x84 (array)"),
        "{msg}"
    );
    assert!(msg.contains(RULE), "{msg}");
}

#[test]
fn a_byte_string_where_an_integer_list_is_expected_carries_no_hint() {
    // The retired reverse arm: a byte string where a genuine list of
    // integers is expected is an ordinary mismatch, not the documented pair.
    let bytes = encode_canonical(&Value::Bytes(vec![0x11; 4])).unwrap();
    let msg = schema_mismatch::<Vec<u32>>(&bytes);
    assert!(
        msg.contains("expected 0x04 (array), got 0x44 (byte string)"),
        "{msg}"
    );
    assert!(!msg.contains(RULE), "{msg}");
}

#[test]
fn an_unrelated_mismatch_names_the_shapes_but_carries_no_hint() {
    // A text string where a byte string is expected: not the documented pair.
    let bytes = encode_canonical(&Value::String("0011".into())).unwrap();
    let msg = schema_mismatch::<FixedId>(&bytes);
    assert!(
        msg.contains("expected 0x02 (byte string), got 0x64 (text string)"),
        "{msg}"
    );
    assert!(!msg.contains(RULE), "{msg}");
}
