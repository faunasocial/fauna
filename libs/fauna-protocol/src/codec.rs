//! Canonical DAG-CBOR encode/decode helpers.
//!
//! Thin wrappers over `fauna_cbor` (the canonical IPLD-dag-cbor codec — see
//! `docs/goal/architecture/serialization.md`). `encode_canonical` produces
//! canonical dag-cbor bytes; `decode_strict` rejects any non-canonical input
//! before deserializing. The generic CBOR node type for kind-agnostic
//! positions is `fauna_cbor::Value` (re-exported as `fauna_protocol::Value`).
//!
//! The former permissive, non-canonical-tolerant `decode` helper and the
//! foreign generic-node re-export were removed in the CBOR-DAG-everywhere
//! Layer-4 flip; nothing in the protocol path accepts non-canonical CBOR
//! any more.

use bytes::Bytes;

/// Encode a `serde::Serialize` value to canonical DAG-CBOR bytes via fauna-cbor.
///
/// Uses `serde_ipld_dagcbor` under the hood: shortest-form integers, map keys
/// sorted by CBOR encoding length then bytewise, no floats/NaN/Inf, no
/// indefinite-length, no tags except 42. Returns `Bytes` so callers can hand
/// the result straight to the transport sink without an extra copy.
pub fn encode_canonical<T: serde::Serialize>(value: &T) -> Result<Bytes, fauna_cbor::EncodeError> {
    fauna_cbor::encode_canonical(value).map(Bytes::from)
}

/// Strict decode via fauna-cbor: enforces every canonical-form axis
/// (shortest-form integers, length-first bytewise map ordering, no duplicate
/// map keys, no floats, tag 42 only, no indefinite-length, no reserved
/// additional-info) via fauna-cbor's pre-parse validator before the
/// underlying `serde_ipld_dagcbor` deserializer runs.
pub fn decode_strict<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, fauna_cbor::DecodeError> {
    fauna_cbor::decode_strict(bytes)
}

/// Round-trip + canonical-re-encode assertion shared by every protocol
/// type's own test module: canonical-encode, strict-decode, confirm the
/// decoded value equals the original, then confirm re-encoding it produces
/// byte-identical bytes (canonical form is unique).
#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) fn assert_round_trips<T>(value: &T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes1 = super::encode_canonical(value).unwrap();
        let decoded: T = super::decode_strict(&bytes1).unwrap();
        assert_eq!(value, &decoded);
        let bytes2 = super::encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2, "canonical re-encode is byte-stable");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_encodes_u64_shortest_form() {
        // 23 fits in one byte (0..=23 is the small-int range)
        let bytes = encode_canonical(&23u64).unwrap();
        assert_eq!(&bytes[..], &[0x17]);

        // 24 starts the one-extra-byte range
        let bytes = encode_canonical(&24u64).unwrap();
        assert_eq!(&bytes[..], &[0x18, 0x18]);

        // 256 is two bytes
        let bytes = encode_canonical(&256u64).unwrap();
        assert_eq!(&bytes[..], &[0x19, 0x01, 0x00]);
    }

    #[test]
    fn round_trip_u64() {
        let bytes = encode_canonical(&42u64).unwrap();
        let n: u64 = decode_strict(&bytes).unwrap();
        assert_eq!(n, 42);
    }

    #[test]
    fn decode_strict_rejects_non_canonical_int() {
        // 0x18 0x05 is "uint-with-1-byte-extension(5)" — non-canonical
        // because 5 fits in the small-int range and should be 0x05.
        let bytes = [0x18, 0x05];
        let result = decode_strict::<u64>(&bytes);
        assert!(
            matches!(result, Err(fauna_cbor::DecodeError::NotCanonical { .. })),
            "expected NotCanonical, got {:?}",
            result
        );
    }

    #[test]
    fn value_round_trips_through_codec_wrappers() {
        // The generic dag-cbor node `crate::Value` (= fauna_cbor::Value =
        // ipld_core::Ipld) carries kind-agnostic payloads (the WS-RPC payload
        // seam, forward-compat `extra` maps, `Unknown`, error details). Pin
        // that it round-trips through these `Bytes`-returning wrappers.
        use crate::Value;
        use std::collections::BTreeMap;
        let v = Value::Map(BTreeMap::from([
            ("s".to_string(), Value::String("hello".into())),
            ("n".to_string(), Value::Integer(42)),
            (
                "list".to_string(),
                Value::List(vec![Value::Integer(1), Value::Integer(2)]),
            ),
        ]));
        let bytes = encode_canonical(&v).unwrap();
        let decoded: Value = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, v);
    }
}
