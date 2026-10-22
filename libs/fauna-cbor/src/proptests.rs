//! Property tests over the strict decoder every pre-auth WS-RPC frame, federation
//! hello and signed envelope goes through. The residual bug class on a stranger's
//! bytes is a panic, a stack overflow on deep nesting, or an unbounded
//! allocation off a lying length prefix — never an out-of-bounds read, the decoder
//! being safe Rust. The properties: never panic, bounded work on hostile length
//! prefixes and nesting, and "accepted means canonical" (re-encoding what was
//! accepted reproduces the input byte for byte).

use crate::{Value, decode_strict, encode_canonical};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = decode_strict::<Value>(&bytes);
    }

    /// Accepted input is canonical: re-encoding the decoded value is the input.
    #[test]
    fn accepted_input_reencodes_identically(
        bytes in proptest::collection::vec(any::<u8>(), 0..256)
    ) {
        if let Ok(v) = decode_strict::<Value>(&bytes) {
            let again = encode_canonical(&v).expect("a decoded value re-encodes");
            prop_assert_eq!(again, bytes);
        }
    }

    /// Typed targets (a struct-shaped map, a byte string, a list) refuse a
    /// shape mismatch with an error rather than a panic.
    #[test]
    fn typed_targets_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = decode_strict::<std::collections::BTreeMap<String, u64>>(&bytes);
        let _ = decode_strict::<Vec<String>>(&bytes);
        let _ = decode_strict::<(u8, String, Vec<u8>)>(&bytes);
        let _ = decode_strict::<Option<u64>>(&bytes);
    }

    /// A length prefix claiming up to 2^64 items/bytes over a short body must
    /// be refused without reserving that much memory.
    #[test]
    fn lying_length_prefixes_are_refused_cheaply(
        major in 2u8..=5,
        len in any::<u64>(),
        tail in proptest::collection::vec(any::<u8>(), 0..32),
    ) {
        let mut buf = vec![(major << 5) | 27];
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&tail);
        if len > tail.len() as u64 {
            prop_assert!(decode_strict::<Value>(&buf).is_err());
        } else {
            let _ = decode_strict::<Value>(&buf);
        }
    }

    /// Nesting far past the validator's depth bound (256) is an error, not a
    /// stack overflow. Array-of-array and map-of-map shapes, arbitrary depth.
    #[test]
    fn deep_nesting_is_refused_not_overflowed(depth in 257usize..20_000, map in any::<bool>()) {
        let mut buf = Vec::with_capacity(depth * 2 + 1);
        for _ in 0..depth {
            if map {
                buf.extend_from_slice(&[0xa1, 0x00]); // {0: ...}
            } else {
                buf.push(0x81); // [ ...]
            }
        }
        buf.push(0x00);
        prop_assert!(decode_strict::<Value>(&buf).is_err());
    }

    /// Anything the canonical encoder writes decodes back to the same value.
    #[test]
    fn encoder_output_roundtrips(v in arb_value()) {
        let bytes = encode_canonical(&v).expect("encodes");
        let back: Value = decode_strict(&bytes).expect("canonical decodes");
        prop_assert_eq!(&back, &v);
        prop_assert_eq!(encode_canonical(&back).unwrap(), bytes);
    }
}

fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-1_000_000_000_000i64..1_000_000_000_000).prop_map(|i| Value::Integer(i128::from(i))),
        ".{0,16}".prop_map(Value::String),
        proptest::collection::vec(any::<u8>(), 0..16).prop_map(Value::Bytes),
    ];
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::List),
            proptest::collection::btree_map("[a-z]{0,6}", inner, 0..4).prop_map(Value::Map),
        ]
    })
}
