//! The payload of a carrying unknown arm — a value of an enum this build does
//! not name, kept whole so a reader that writes the record back out re-emits
//! exactly what it read (`docs/goal/architecture/transport.md` § Schema and
//! forward-compat discipline → *Rule 3 in full*: an enum with data variants is
//! open and carrying wherever a reader can write the value back out).
//!
//! The arm is spelled as the enum's last variant,
//! `#[serde(untagged)] Unknown(CarriedValue)`: serde tries every named variant
//! first, and a value none of them accepts lands here as the undecoded
//! dag-cbor value. Re-encoding it through the canonical encoder gives back the
//! newer writer's canonical bytes, so a signed or sealed record holding one
//! survives an older reader's read-modify-write byte-identically.
//!
//! An all-unit enum carries its unknown arm as `Other(String)` instead — the
//! bare string is the whole value, so no `CarriedValue` is needed there.

/// A value of an enum variant this build does not name, held undecoded. It is
/// defined in the codec crate so a lean crate with no `fauna-core` dependency
/// (`fauna-ipc`, loaded into Explorer by the Windows shell extension) spells
/// its carrying arms with the same type.
pub use fauna_cbor::CarriedValue;

/// The carrying arm of an enum that **also crosses FFI**, where a
/// `fauna_cbor::Value` cannot: the undecoded value held as its canonical
/// dag-cbor bytes (`transport.md` § Schema and forward-compat discipline →
/// *Rule 3 in full*: "or as its canonical bytes where the enum also crosses
/// FFI"). Spelled as a named-field variant, which every app binding renders
/// the same way:
///
/// ```ignore
/// #[serde(untagged, with = "fauna_core::carried::canonical_bytes")]
/// Unknown { canonical: Vec<u8> },
/// ```
///
/// Decoding takes whatever value no named variant accepted and keeps its
/// canonical encoding; encoding writes that value back, so a record holding
/// one re-encodes byte-identically (canonical in, canonical out — rule 5).
pub mod canonical_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// Write the carried value back out as the value it was, not as bytes.
    // The `&Vec<u8>` is serde's variant-`with` calling convention: the
    // variant's field, by reference.
    #[allow(clippy::ptr_arg)]
    pub fn serialize<S: Serializer>(canonical: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        let value: fauna_cbor::Value =
            fauna_cbor::decode_strict(canonical).map_err(serde::ser::Error::custom)?;
        value.serialize(s)
    }

    /// Read any value and keep its canonical encoding.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let value = fauna_cbor::Value::deserialize(d)?;
        fauna_cbor::encode_canonical(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::{canonical_decode, canonical_encode};
    use serde::{Deserialize, Serialize};

    /// The serde mechanism every carrying arm leans on: a newer writer's
    /// variant — with byte strings, a nested map, a negative integer and a
    /// link inside — decodes into the untagged arm and re-encodes to the same
    /// bytes, while every named variant still decodes as itself.
    #[test]
    fn an_unknown_data_variant_round_trips_byte_identically() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        enum Newer {
            Known {
                n: u32,
            },
            Added {
                #[serde(with = "serde_bytes")]
                raw: Vec<u8>,
                nested: std::collections::BTreeMap<String, i64>,
                link: fauna_cbor::Cid,
                tuple: (String, bool),
            },
            AddedUnit,
        }
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        enum Older {
            Known {
                n: u32,
            },
            #[serde(untagged)]
            Unknown(CarriedValue),
        }

        let cid = fauna_cbor::Cid::of_raw(b"linked");
        for newer in [
            Newer::Added {
                raw: vec![0, 1, 2, 255],
                nested: [("b".into(), -7), ("aa".into(), 3)].into(),
                link: cid,
                tuple: ("x".into(), true),
            },
            Newer::AddedUnit,
        ] {
            let bytes = canonical_encode(&newer).unwrap();
            let older: Older = canonical_decode(&bytes).unwrap();
            assert!(matches!(older, Older::Unknown(_)), "{older:?}");
            assert_eq!(canonical_encode(&older).unwrap(), bytes);
        }

        let bytes = canonical_encode(&Newer::Known { n: 9 }).unwrap();
        assert_eq!(
            canonical_decode::<Older>(&bytes).unwrap(),
            Older::Known { n: 9 }
        );
    }
}
