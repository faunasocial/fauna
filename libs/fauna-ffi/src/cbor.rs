//! UniFFI mirror of `fauna_protocol::Value` (= `ipld_core::Ipld`).
//!
//! `Value` is a foreign type (re-exported from `fauna_cbor` / `ipld_core`), so
//! it can't carry a `#[derive(uniffi::Enum)]`. [`FfiCborValue`] is the
//! FFI-visible representation: a recursive enum the Swift / Kotlin / C# side
//! builds to compose the dynamic `params` (`fauna.bridges.link`), `settings`
//! (`fauna.bridges.set_settings`) and follow-`extra` blobs, and reads back
//! the per-bridge setting values in `BridgeStatus`.
//!
//! This is what keeps CBOR encoding in Rust — clients never need a CBOR
//! codec of their own; they build a typed tree and the wrapper crates
//! (`fauna-client-bridges`, `fauna-client-email`) encode it canonically on
//! the wire.
//!
//! **Map keys are text-only.** dag-cbor (and thus `Value::Map`) only has
//! string keys, so `Map` carries `Vec<FfiCborEntry>` with a `String` key.
//! The variant names (`Text` / `Array` / `Map`) are the client-facing UniFFI
//! contract and intentionally keep the ciborium-era spelling even though the
//! underlying node now uses `String` / `List` / `Map` — renaming them would
//! break the Swift / Kotlin / C# / TS bindings.

use fauna_client::Value;

use crate::FfiError;

/// FFI-visible mirror of a CBOR value. See module docs for the text-key
/// constraint on `Map`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FfiCborValue {
    Null,
    Bool {
        v: bool,
    },
    /// Signed integer. CBOR integers are 128-bit on the wire; values
    /// outside `i64` range error on the inbound (server→client) path.
    Integer {
        v: i64,
    },
    Float {
        v: f64,
    },
    Text {
        v: String,
    },
    Bytes {
        v: Vec<u8>,
    },
    Array {
        items: Vec<FfiCborValue>,
    },
    Map {
        entries: Vec<FfiCborEntry>,
    },
}

/// One text-keyed entry in an [`FfiCborValue::Map`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiCborEntry {
    pub key: String,
    pub value: FfiCborValue,
}

// ── Outbound: FfiCborValue → Value (infallible) ────────────────────────
//
// Used when the client composes link params / settings / follow-extra.

impl From<FfiCborValue> for Value {
    fn from(v: FfiCborValue) -> Self {
        match v {
            FfiCborValue::Null => Value::Null,
            FfiCborValue::Bool { v } => Value::Bool(v),
            FfiCborValue::Integer { v } => Value::Integer(i128::from(v)),
            FfiCborValue::Float { v } => Value::Float(v),
            FfiCborValue::Text { v } => Value::String(v),
            FfiCborValue::Bytes { v } => Value::Bytes(v),
            FfiCborValue::Array { items } => {
                Value::List(items.into_iter().map(Value::from).collect())
            }
            FfiCborValue::Map { entries } => Value::Map(
                entries
                    .into_iter()
                    .map(|e| (e.key, Value::from(e.value)))
                    .collect(),
            ),
        }
    }
}

// ── Inbound: Value → FfiCborValue (fallible) ───────────────────────────
//
// Used when reading back per-bridge setting values / follow-extra. Errors
// on integers outside `i64` and on CID links (`Value::Link`) — neither is
// emitted by any `BridgeProvider`. Map keys are already `String`, so the
// inbound path no longer needs a non-text-key check.

impl TryFrom<Value> for FfiCborValue {
    type Error = FfiError;

    fn try_from(v: Value) -> Result<Self, FfiError> {
        Ok(match v {
            Value::Null => FfiCborValue::Null,
            Value::Bool(b) => FfiCborValue::Bool { v: b },
            Value::Integer(n) => {
                let v = i64::try_from(n).map_err(|_| FfiError::General {
                    msg: format!("CBOR integer {n} out of i64 range for FFI"),
                })?;
                FfiCborValue::Integer { v }
            }
            Value::Float(f) => FfiCborValue::Float { v: f },
            Value::String(s) => FfiCborValue::Text { v: s },
            Value::Bytes(b) => FfiCborValue::Bytes { v: b },
            Value::List(items) => FfiCborValue::Array {
                items: items
                    .into_iter()
                    .map(FfiCborValue::try_from)
                    .collect::<Result<_, _>>()?,
            },
            Value::Map(map) => {
                let mut entries = Vec::with_capacity(map.len());
                for (key, val) in map {
                    entries.push(FfiCborEntry {
                        key,
                        value: FfiCborValue::try_from(val)?,
                    });
                }
                FfiCborValue::Map { entries }
            }
            Value::Link(_) => {
                return Err(FfiError::General {
                    msg: "CID link not representable over FFI".to_string(),
                });
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entries are listed in BTreeMap key-sorted order so the round-trip
    /// (which canonicalizes map-key order through `Value::Map`'s `BTreeMap`)
    /// is byte-for-byte comparable: absent < handle < nested < ratio <
    /// smtp_port < tags < use_tls.
    fn sample_tree() -> FfiCborValue {
        FfiCborValue::Map {
            entries: vec![
                FfiCborEntry {
                    key: "absent".into(),
                    value: FfiCborValue::Null,
                },
                FfiCborEntry {
                    key: "handle".into(),
                    value: FfiCborValue::Text {
                        v: "alice.bsky.social".into(),
                    },
                },
                FfiCborEntry {
                    key: "nested".into(),
                    value: FfiCborValue::Map {
                        entries: vec![FfiCborEntry {
                            key: "k".into(),
                            value: FfiCborValue::Bytes { v: vec![1, 2, 3] },
                        }],
                    },
                },
                FfiCborEntry {
                    key: "ratio".into(),
                    value: FfiCborValue::Float { v: 1.5 },
                },
                FfiCborEntry {
                    key: "smtp_port".into(),
                    value: FfiCborValue::Integer { v: 587 },
                },
                FfiCborEntry {
                    key: "tags".into(),
                    value: FfiCborValue::Array {
                        items: vec![
                            FfiCborValue::Text { v: "a".into() },
                            FfiCborValue::Text { v: "b".into() },
                        ],
                    },
                },
                FfiCborEntry {
                    key: "use_tls".into(),
                    value: FfiCborValue::Bool { v: true },
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_cbor_value() {
        let ffi = sample_tree();
        let cbor: Value = ffi.clone().into();
        let back = FfiCborValue::try_from(cbor).unwrap();
        assert_eq!(ffi, back);
    }

    #[test]
    fn scalars_round_trip() {
        for v in [
            FfiCborValue::Null,
            FfiCborValue::Bool { v: false },
            FfiCborValue::Integer { v: -42 },
            FfiCborValue::Text { v: "x".into() },
            FfiCborValue::Bytes {
                v: vec![0xff, 0x00],
            },
        ] {
            let cbor: Value = v.clone().into();
            assert_eq!(FfiCborValue::try_from(cbor).unwrap(), v);
        }
    }

    #[test]
    fn integer_out_of_i64_range_errors() {
        let cbor = Value::Integer(i128::from(u64::MAX));
        assert!(FfiCborValue::try_from(cbor).is_err());
    }
}
