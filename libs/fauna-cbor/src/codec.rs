use core::convert::Infallible;
use serde::{Serialize, de::DeserializeOwned};

use crate::cid::Cid;
use crate::error::{DecodeError, EncodeError};

/// Encode `v` as canonical IPLD-dag-cbor. Map keys are sorted length-first then bytewise;
/// integers are shortest-form; no floats/NaN/Inf; no indefinite-length; no tags except 42.
///
/// Enforced by `serde_ipld_dagcbor` — the rust-ipld project's IPLD-conformant encoder.
///
/// Debug builds first run the byte-field guard: a plain-derive `[u8; N]` or
/// `Vec<u8>` anywhere in `v` (it would land as an array of integers instead
/// of the byte string every raw-byte field rides as) is refused, naming its
/// field path — `docs/goal/architecture/serialization.md` § Canonical IPLD
/// dag-cbor, "Fixed-size byte arrays" and "Variable-length byte fields".
/// Release builds skip the extra walk.
pub fn encode_canonical<T: Serialize>(v: &T) -> Result<Vec<u8>, EncodeError> {
    #[cfg(debug_assertions)]
    if let Some((path, shape)) = crate::byte_array_guard::find_plain_byte_field(v) {
        use crate::byte_array_guard::ByteShape;
        return Err(EncodeError::SchemaInvalid(match shape {
            ByteShape::FixedArray => format!(
                "plain-derive `[u8; N]` at `{path}` would encode as an array of integers; \
                 a fixed-width byte field is a byte string (`#[serde(with = \"serde_bytes\")]` \
                 or `fauna_core::byte_array`): docs/goal/architecture/serialization.md \
                 § Canonical IPLD dag-cbor, \"Fixed-size byte arrays\""
            ),
            ByteShape::Vec => format!(
                "plain-derive `Vec<u8>` at `{path}` would encode as an array of integers; \
                 a variable-length byte field is a byte string (`#[serde(with = \
                 \"serde_bytes\")]` or `ByteBuf`): docs/goal/architecture/serialization.md \
                 § Canonical IPLD dag-cbor, \"Variable-length byte fields\""
            ),
        }));
    }
    serde_ipld_dagcbor::to_vec(v).map_err(|e| EncodeError::SchemaInvalid(e.to_string()))
}

/// Encode `v` canonically and derive its filing CID from the exact bytes
/// returned in one step — `Cid::of_dag_cbor(bytes)` over what
/// [`encode_canonical`] produced, so a caller can never mint an identity for
/// any bytes but the ones actually stored. This is the record-identity rule
/// every segment-store kind's envelope relies on
/// (`docs/goal/architecture/message-segment-store.md` § Record identity per
/// kind: "identity IS the content hash of the stored bytes, for every
/// kind") — the single mint for a plain `Serialize` envelope; a kind whose
/// wire shape is version-negotiated (encoded some way other than bare
/// [`encode_canonical`]) derives the pair by hand from its own encoder
/// instead (e.g. `fauna_mail::segments::ops::encode_record`).
pub fn encode_with_cid<T: Serialize>(v: &T) -> Result<(Cid, Vec<u8>), EncodeError> {
    let bytes = encode_canonical(v)?;
    let cid = Cid::of_dag_cbor(&bytes);
    Ok((cid, bytes))
}

/// Strict decode: rejects non-canonical input, returns typed schema mismatch on shape errors.
pub fn decode_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    crate::canonical::validate_canonical(bytes)?;
    serde_ipld_dagcbor::from_slice(bytes).map_err(map_decode_error)
}

/// Relaxed decode: tolerates non-canonical input. Use only for debug/inspection tools;
/// FORBIDDEN in the security path (CID verification, signature verification).
#[doc(hidden)]
pub fn decode_relaxed<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    // serde_ipld_dagcbor doesn't expose a relaxed mode; relaxed-decode users should
    // fall through to a permissive cbor decoder. For now this is an alias for strict.
    decode_strict(bytes)
}

/// Map `serde_ipld_dagcbor::DecodeError<Infallible>` (returned by `from_slice`) to our
/// typed `DecodeError`.
///
/// Mapping rules (contract — implementation detail is the match arms):
/// - canonical violations → `NotCanonical`
/// - type/schema mismatch → `SchemaMismatch`
/// - everything else      → `NotValidCbor`
fn map_decode_error(e: serde_ipld_dagcbor::DecodeError<Infallible>) -> DecodeError {
    use serde_ipld_dagcbor::DecodeError as E;
    match e {
        // Canonical-form violations.
        E::IndefiniteSize => DecodeError::NotCanonical {
            reason: "indefinite-size item".to_string(),
        },
        // Type / schema mismatches.
        E::Mismatch { expect_major, byte } => DecodeError::SchemaMismatch(format!(
            "major type mismatch: expected {expect_major:#04x} ({}), got {byte:#04x} ({}){}",
            major_type_name(expect_major),
            major_type_name(byte >> 5),
            array_vs_byte_string_hint(expect_major, byte >> 5),
        )),
        E::TypeMismatch { name, byte } => DecodeError::SchemaMismatch(format!(
            "type mismatch: expected {name}, got byte {byte:#04x}"
        )),
        E::Overflow { name } => {
            DecodeError::SchemaMismatch(format!("integer overflow for type {name}"))
        }
        E::CastOverflow(e) => DecodeError::SchemaMismatch(format!("cast overflow: {e}")),
        E::Msg(msg) => {
            // serde custom errors — typically "missing field", "invalid type", etc.
            if msg.contains("missing field")
                || msg.contains("invalid type")
                || msg.contains("unknown field")
                || msg.contains("duplicate field")
            {
                DecodeError::SchemaMismatch(msg)
            } else {
                DecodeError::NotValidCbor
            }
        }
        // Everything else is malformed/truncated CBOR.
        E::Eof
        | E::RequireBorrowed { .. }
        | E::RequireLength { .. }
        | E::InvalidUtf8(_)
        | E::Unsupported { .. }
        | E::DepthLimit
        | E::TrailingData
        | E::Read(_) => DecodeError::NotValidCbor,
    }
}

/// Human name of a CBOR major type (RFC 8949 § 3.1) for the schema-mismatch
/// text a refused caller reads back.
fn major_type_name(major: u8) -> &'static str {
    match major {
        0 => "unsigned integer",
        1 => "negative integer",
        2 => "byte string",
        3 => "text string",
        4 => "array",
        5 => "map",
        6 => "tag",
        _ => "simple value or float",
    }
}

/// The one mismatch pair with a known, documented cause: every raw-byte
/// field rides as a CBOR byte string, so an array of integers where a byte
/// string is expected is a bare `[u8; N]` or `Vec<u8>` with a plain serde
/// derive — on the sender's side — that reached the wire unattributed. The
/// bare major-type text named neither the rule nor its owner, which once
/// cost a whole e2e run to diagnose; the hint points at the owner.
fn array_vs_byte_string_hint(expect_major: u8, got_major: u8) -> &'static str {
    match (expect_major, got_major) {
        (2, 4) => {
            " — every raw-byte field, fixed-width ids included, rides as a byte string; an \
             array of integers here is a bare `[u8; N]` or `Vec<u8>` with a plain derive that \
             reached the wire — attribute it: docs/goal/architecture/serialization.md \
             § Canonical IPLD dag-cbor, \"Fixed-size byte arrays\""
        }
        _ => "",
    }
}

#[cfg(test)]
mod value_tests {
    //! Round-trips for the generic dag-cbor node `crate::Value` (= `ipld_core::Ipld`)
    //! through the canonical codec — the type that carries kind-agnostic payloads
    //! (the WS-RPC payload seam, forward-compat `extra` maps, `Unknown`, error details)
    //! after the Layer-4 ciborium→fauna_cbor flip.
    use crate::{Value, decode_strict, encode_canonical};
    use std::collections::BTreeMap;

    #[test]
    fn value_round_trips_scalars_and_containers() {
        let v = Value::Map(BTreeMap::from([
            ("s".to_string(), Value::String("hello".into())),
            ("n".to_string(), Value::Integer(42)),
            ("b".to_string(), Value::Bool(true)),
            ("nul".to_string(), Value::Null),
            ("bytes".to_string(), Value::Bytes(vec![1, 2, 3])),
            (
                "list".to_string(),
                Value::List(vec![Value::Integer(1), Value::Integer(2)]),
            ),
        ]));
        let bytes = encode_canonical(&v).unwrap();
        let decoded: Value = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, v);
    }

    #[test]
    fn serde_flatten_btreemap_round_trips_canonically() {
        // `#[serde(flatten)] BTreeMap<String, Value>` must round-trip through
        // `encode_canonical` AND pass strict `decode_strict` — which validates
        // length-first/bytewise key order. Interleaving named fields with the
        // flattened map keys is the stressor (the encoder must sort the merged
        // key set). This capability is what lets at-rest types with a
        // forward-compat `extra` map (e.g. `fauna_protocol::SegmentRef`) use
        // canonical dag-cbor instead of ciborium.
        use serde::{Deserialize, Serialize};
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Outer {
            id: u32,
            is_open: bool,
            #[serde(flatten)]
            extra: BTreeMap<String, Value>,
        }
        let mut extra = BTreeMap::new();
        extra.insert("z_custom".to_string(), Value::Integer(7));
        extra.insert("a_custom".to_string(), Value::String("x".into()));
        let v = Outer {
            id: 1,
            is_open: true,
            extra,
        };
        let bytes = encode_canonical(&v).expect("encode with serde(flatten)");
        let back: Outer = decode_strict(&bytes).expect("strict-decode with serde(flatten)");
        assert_eq!(back, v);
    }

    #[test]
    fn value_round_trips_cid_link_tag42() {
        // A CID link must survive as `Value::Link` (dag-cbor tag 42), proving the
        // generic node interoperates with the security-path CID vocabulary —
        // `ciborium::value::Value` modelled tag 42 only as an opaque tag, so this
        // is a capability the new node type gains.
        use ::multihash_codetable::{Code, MultihashDigest};
        let mh = Code::Blake3_256.digest(b"layer-4 cid link");
        let cid = ::cid::Cid::new_v1(0x71, mh);
        let v = Value::Link(cid);
        let bytes = encode_canonical(&v).unwrap();
        let decoded: Value = decode_strict(&bytes).unwrap();
        assert_eq!(
            decoded, v,
            "tag-42 CID link must survive the canonical round-trip"
        );
    }
}
