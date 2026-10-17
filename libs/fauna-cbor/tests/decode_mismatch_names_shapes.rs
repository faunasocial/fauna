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
        msg.starts_with("major type mismatch: "),
        "a root-level mismatch has no field path: {msg}"
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

// ── the refusal leads with the field path ─────────────────────────────────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Scope {
    #[serde(with = "serde_bytes")]
    actor_id: [u8; 4],
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Request {
    blob: ByteBuf,
    scope: Scope,
    scopes: Vec<Scope>,
}

fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

fn good_scope() -> Value {
    map(vec![("actor_id", Value::Bytes(vec![1, 2, 3, 4]))])
}

fn array_scope() -> Value {
    map(vec![("actor_id", Value::List(vec![Value::Integer(1); 4]))])
}

fn request(blob: Value, scope: Value, scopes: Vec<Value>) -> Vec<u8> {
    encode_canonical(&map(vec![
        ("blob", blob),
        ("scope", scope),
        ("scopes", Value::List(scopes)),
    ]))
    .unwrap()
}

#[test]
fn the_refusal_leads_with_the_field_beside_a_byte_buf() {
    let bytes = request(
        Value::List(vec![Value::Integer(7); 3]),
        good_scope(),
        vec![],
    );
    let msg = schema_mismatch::<Request>(&bytes);
    assert!(
        msg.starts_with("blob: major type mismatch: expected 0x02 (byte string), got 0x83 (array)"),
        "{msg}"
    );
    assert!(msg.contains(RULE), "{msg}");
}

#[test]
fn a_nested_refusal_names_every_segment() {
    let bytes = request(Value::Bytes(vec![7; 3]), array_scope(), vec![]);
    let msg = schema_mismatch::<Request>(&bytes);
    assert!(
        msg.starts_with(
            "scope.actor_id: major type mismatch: expected 0x02 (byte string), got 0x84 (array)"
        ),
        "{msg}"
    );
}

#[test]
fn a_refusal_inside_a_list_names_the_index() {
    let bytes = request(
        Value::Bytes(vec![7; 3]),
        good_scope(),
        vec![good_scope(), array_scope()],
    );
    let msg = schema_mismatch::<Request>(&bytes);
    assert!(msg.starts_with("scopes[1].actor_id: "), "{msg}");
}

#[test]
fn a_missing_field_names_the_struct_it_is_missing_from() {
    let bytes = request(Value::Bytes(vec![7; 3]), map(vec![]), vec![]);
    let msg = schema_mismatch::<Request>(&bytes);
    assert!(msg.starts_with("scope: "), "{msg}");
    assert!(msg.contains("missing field"), "{msg}");
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
enum Choice {
    Unit,
    Wrapped(Scope),
    Fields { scope: Scope },
}

#[test]
fn an_enum_variant_is_a_path_segment() {
    let wrapped = encode_canonical(&map(vec![("Wrapped", array_scope())])).unwrap();
    let msg = schema_mismatch::<Choice>(&wrapped);
    assert!(msg.starts_with("Wrapped.actor_id: "), "{msg}");

    let fields =
        encode_canonical(&map(vec![("Fields", map(vec![("scope", array_scope())]))])).unwrap();
    let msg = schema_mismatch::<Choice>(&fields);
    assert!(msg.starts_with("Fields.scope.actor_id: "), "{msg}");

    let unit = encode_canonical(&Value::String("Unit".into())).unwrap();
    assert!(matches!(decode_strict::<Choice>(&unit), Ok(Choice::Unit)));
}

#[test]
fn a_clean_decode_is_untouched_by_the_tracker() {
    let bytes = request(Value::Bytes(vec![7; 3]), good_scope(), vec![good_scope()]);
    let req: Request = decode_strict(&bytes).unwrap();
    assert_eq!(req.scopes.len(), 1);
    assert_eq!(req.scope.actor_id, [1, 2, 3, 4]);
}
