//! Byte-string serde for the raw-byte list forms `serde_bytes` refuses.
//!
//! Every serialized raw-byte field is a CBOR byte string
//! (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
//! "Fixed-size byte arrays" and "Variable-length byte fields"). A bare
//! `[u8; N]`, a `Vec<u8>` or an `Option<>` of either takes
//! `#[serde(with = "serde_bytes")]`; a list of fixed-width ids takes
//! `#[serde(with = "fauna_core::byte_array::vec")]` and a list of
//! variable-length byte strings
//! `#[serde(with = "fauna_core::byte_array::vec_of_bufs")]` from this module,
//! which the pinned `serde_bytes` has no impl for. Decoding of the
//! fixed-width list is strict on length: a byte string of any other width is
//! refused, never padded or truncated.

/// `Vec<[u8; N]>` as a list of byte strings.
pub mod vec {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize each element as a byte string.
    #[allow(clippy::ptr_arg)] // `with =` hands the field's own type.
    pub fn serialize<S: Serializer, const N: usize>(
        v: &Vec<[u8; N]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(v.iter().map(|a| serde_bytes::Bytes::new(a)))
    }

    /// Deserialize a list of exactly-`N`-byte byte strings.
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        deserializer: D,
    ) -> Result<Vec<[u8; N]>, D::Error> {
        let v = Vec::<serde_bytes::ByteArray<N>>::deserialize(deserializer)?;
        Ok(v.into_iter()
            .map(serde_bytes::ByteArray::into_array)
            .collect())
    }
}

/// `Vec<Vec<u8>>` as a list of byte strings.
pub mod vec_of_bufs {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize each element as a byte string.
    #[allow(clippy::ptr_arg)] // `with =` hands the field's own type.
    pub fn serialize<S: Serializer>(v: &Vec<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(v.iter().map(|b| serde_bytes::Bytes::new(b)))
    }

    /// Deserialize a list of byte strings.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Vec<u8>>, D::Error> {
        let v = Vec::<serde_bytes::ByteBuf>::deserialize(deserializer)?;
        Ok(v.into_iter().map(serde_bytes::ByteBuf::into_vec).collect())
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Bufs {
        #[serde(with = "super::vec_of_bufs")]
        wraps: Vec<Vec<u8>>,
    }

    #[test]
    fn a_list_of_byte_vectors_rides_as_byte_strings_and_round_trips() {
        let s = Bufs {
            wraps: vec![vec![0xAB; 40], vec![], vec![1]],
        };
        let bytes = fauna_cbor::encode_canonical(&s).unwrap();
        let mut want = vec![0x83, 0x58, 40];
        want.extend([0xAB; 40]);
        want.extend([0x40, 0x41, 1]);
        assert!(bytes.ends_with(&want), "{bytes:02x?}");
        let back: Bufs = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_list_of_integer_arrays_is_refused() {
        let v = fauna_cbor::Value::Map(
            [(
                "wraps".to_string(),
                fauna_cbor::Value::List(vec![fauna_cbor::Value::List(vec![
                    fauna_cbor::Value::Integer(1),
                ])]),
            )]
            .into(),
        );
        let bytes = fauna_cbor::encode_canonical(&v).unwrap();
        assert!(fauna_cbor::decode_strict::<Bufs>(&bytes).is_err());
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Shapes {
        #[serde(with = "serde_bytes")]
        bare: [u8; 32],
        #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
        maybe: Option<[u8; 16]>,
        #[serde(with = "super::vec")]
        list: Vec<[u8; 32]>,
    }

    fn byte_string_32(fill: u8) -> Vec<u8> {
        let mut v = vec![0x58, 0x20];
        v.extend([fill; 32]);
        v
    }

    #[test]
    fn the_three_shapes_ride_as_byte_strings_and_round_trip() {
        let s = Shapes {
            bare: [0x11; 32],
            maybe: Some([0x22; 16]),
            list: vec![[0x33; 32], [0x44; 32]],
        };
        let bytes = fauna_cbor::encode_canonical(&s).unwrap();
        for fill in [0x11, 0x33, 0x44] {
            let want = byte_string_32(fill);
            assert!(bytes.windows(34).any(|w| w == want.as_slice()), "{fill:#x}");
        }
        let mut sixteen = vec![0x50];
        sixteen.extend([0x22; 16]);
        assert!(bytes.windows(17).any(|w| w == sixteen.as_slice()));
        // `list` is an array of two byte strings, never an integer array.
        let mut list = vec![0x82];
        list.extend(byte_string_32(0x33));
        assert!(bytes.windows(35).any(|w| w == list.as_slice()));
        assert!(!bytes.windows(2).any(|w| w == [0x98, 0x20]));
        let back: Shapes = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn absent_option_and_empty_list_round_trip() {
        let s = Shapes {
            bare: [0; 32],
            maybe: None,
            list: vec![],
        };
        let bytes = fauna_cbor::encode_canonical(&s).unwrap();
        let back: Shapes = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_wrong_width_list_element_is_refused() {
        #[derive(Serialize)]
        struct Wide {
            #[serde(with = "serde_bytes")]
            bare: [u8; 32],
            list: Vec<serde_bytes::ByteBuf>,
        }
        let bytes = fauna_cbor::encode_canonical(&Wide {
            bare: [0; 32],
            list: vec![serde_bytes::ByteBuf::from(vec![1; 31])],
        })
        .unwrap();
        assert!(fauna_cbor::decode_strict::<Shapes>(&bytes).is_err());
    }

    #[test]
    fn the_integer_array_spelling_is_refused() {
        // The retired plain-derive shape: `list` as a list of integer arrays.
        let arr = fauna_cbor::Value::List(vec![fauna_cbor::Value::Integer(1); 32]);
        let v = fauna_cbor::Value::Map(
            [
                ("bare".to_string(), fauna_cbor::Value::Bytes(vec![0; 32])),
                ("list".to_string(), fauna_cbor::Value::List(vec![arr])),
            ]
            .into(),
        );
        let bytes = fauna_cbor::encode_canonical(&v).unwrap();
        assert!(fauna_cbor::decode_strict::<Shapes>(&bytes).is_err());
    }
}
