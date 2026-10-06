//! The debug encoder guard: a plain-derive `[u8; N]` anywhere in a value is
//! refused by `encode_canonical`, naming its field path, and the attributed
//! spellings — `#[serde(with = "serde_bytes")]` on a bare or `Option<>`
//! `[u8; N]` — ride as byte strings (`0x58 0x20` for 32 bytes) and round-trip
//! through the strict decoder. Owner: `docs/goal/architecture/serialization.md`
//! § Canonical IPLD dag-cbor, "Fixed-size byte arrays".

use fauna_cbor::{EncodeError, decode_strict, encode_canonical};
use serde::{Deserialize, Serialize};

#[cfg(debug_assertions)]
fn refusal<T: Serialize>(v: &T) -> String {
    match encode_canonical(v) {
        Err(EncodeError::SchemaInvalid(msg)) => msg,
        Ok(bytes) => panic!("guard let a plain-derive array through: {bytes:02x?}"),
    }
}

#[cfg(debug_assertions)]
#[test]
fn a_bare_byte_array_field_is_refused_by_path() {
    #[derive(Serialize)]
    struct Inner {
        writer: [u8; 32],
    }
    #[derive(Serialize)]
    struct Outer {
        n: u32,
        stamps: Vec<Inner>,
    }
    let msg = refusal(&Outer {
        n: 1,
        stamps: vec![Inner { writer: [7; 32] }],
    });
    assert!(msg.contains("`stamps.[0].writer`"), "{msg}");
    assert!(msg.contains("Fixed-size byte arrays"), "{msg}");
}

#[cfg(debug_assertions)]
#[test]
fn the_guard_sees_through_options_vecs_maps_newtypes_and_variants() {
    #[derive(Serialize)]
    struct Id([u8; 16]);
    #[derive(Serialize)]
    enum Ref {
        Nest { actor: Id },
    }
    #[derive(Serialize)]
    struct Holder {
        maybe: Option<[u8; 8]>,
        list: Vec<[u8; 4]>,
        by_name: std::collections::BTreeMap<String, [u8; 2]>,
        r: Ref,
    }
    let base = || Holder {
        maybe: None,
        list: vec![],
        by_name: Default::default(),
        r: Ref::Nest { actor: Id([0; 16]) },
    };
    assert!(refusal(&base()).contains("`r.Nest.actor`"));
    let mut h = base();
    h.maybe = Some([1; 8]);
    assert!(refusal(&h).contains("`maybe`"));
    let mut h = base();
    h.list = vec![[1; 4]];
    assert!(refusal(&h).contains("`list.[0]`"));
    let mut h = base();
    h.by_name.insert("k".into(), [1; 2]);
    assert!(refusal(&h).contains("`by_name.k`"));
}

#[test]
fn attributed_arrays_are_byte_strings_and_round_trip() {
    // Settles the mechanism question: the pinned `serde_bytes` accepts a bare
    // `[u8; N]` and an `Option<[u8; N]>` through `with = "serde_bytes"`.
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Probe {
        #[serde(with = "serde_bytes")]
        id: [u8; 32],
        #[serde(with = "serde_bytes")]
        maybe: Option<[u8; 32]>,
        #[serde(with = "serde_bytes")]
        absent: Option<[u8; 16]>,
    }
    let p = Probe {
        id: [0x11; 32],
        maybe: Some([0x22; 32]),
        absent: None,
    };
    let bytes = encode_canonical(&p).expect("attributed arrays pass the guard");
    let mut id = vec![0x58, 0x20];
    id.extend([0x11; 32]);
    assert!(bytes.windows(34).any(|w| w == id.as_slice()));
    assert!(!bytes.windows(2).any(|w| w == [0x98, 0x20]));
    let back: Probe = decode_strict(&bytes).unwrap();
    assert_eq!(back, p);
}

#[test]
fn attributed_arrays_refuse_a_wrong_length_byte_string() {
    #[derive(Serialize)]
    struct Short {
        #[serde(with = "serde_bytes")]
        id: [u8; 31],
    }
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct Want {
        #[serde(with = "serde_bytes")]
        id: [u8; 32],
    }
    let bytes = encode_canonical(&Short { id: [1; 31] }).unwrap();
    assert!(decode_strict::<Want>(&bytes).is_err());
}

#[test]
fn attributed_byte_vectors_tuple_structs_and_empty_shapes_are_not_findings() {
    #[derive(Serialize)]
    struct Rgb(u8, u8, u8);
    #[derive(Serialize)]
    struct Fine {
        #[serde(with = "serde_bytes")]
        blob: Vec<u8>,
        #[serde(with = "serde_bytes")]
        maybe: Option<Vec<u8>>,
        colour: Rgb,
        empty: [u8; 0],
        none_yet: Vec<u8>,
    }
    encode_canonical(&Fine {
        blob: vec![1, 2],
        maybe: Some(vec![3]),
        colour: Rgb(1, 2, 3),
        empty: [],
        none_yet: vec![],
    })
    .expect("an attributed or empty byte field says nothing");
}

#[cfg(debug_assertions)]
#[test]
fn a_plain_byte_vector_is_refused_by_path_naming_the_variable_length_rule() {
    #[derive(Serialize)]
    struct Envelope {
        n: u32,
        sealed: Vec<u8>,
    }
    let msg = refusal(&Envelope {
        n: 1,
        sealed: vec![0xAA; 40],
    });
    assert!(msg.contains("plain-derive `Vec<u8>` at `sealed`"), "{msg}");
    assert!(msg.contains("Variable-length byte fields"), "{msg}");

    // One byte is enough: the guard reads the element type, not the length.
    assert!(
        refusal(&Envelope {
            n: 1,
            sealed: vec![7]
        })
        .contains("`sealed`")
    );
}

#[cfg(debug_assertions)]
#[test]
fn the_guard_sees_plain_byte_vectors_inside_options_lists_and_variants() {
    #[derive(Serialize)]
    enum Body {
        Sealed(Vec<u8>),
        Wraps { wraps: Vec<Vec<u8>> },
    }
    #[derive(Serialize)]
    struct Holder {
        maybe: Option<Vec<u8>>,
        body: Option<Body>,
    }
    let msg = refusal(&Holder {
        maybe: Some(vec![1, 2]),
        body: None,
    });
    assert!(msg.contains("`maybe`"), "{msg}");
    let msg = refusal(&Holder {
        maybe: None,
        body: Some(Body::Sealed(vec![1])),
    });
    assert!(msg.contains("`body.Sealed`"), "{msg}");
    let msg = refusal(&Holder {
        maybe: None,
        body: Some(Body::Wraps {
            wraps: vec![vec![], vec![9]],
        }),
    });
    assert!(msg.contains("`body.Wraps.wraps.[1]`"), "{msg}");
}

#[test]
fn attributed_byte_vectors_are_byte_strings_and_round_trip() {
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Probe {
        #[serde(with = "serde_bytes")]
        sealed: Vec<u8>,
        #[serde(with = "serde_bytes")]
        maybe: Option<Vec<u8>>,
    }
    let p = Probe {
        sealed: vec![0xFF; 40],
        maybe: Some(vec![0x30; 3]),
    };
    let bytes = encode_canonical(&p).expect("attributed vectors pass the guard");
    let mut sealed = vec![0x58, 40];
    sealed.extend([0xFF; 40]);
    assert!(bytes.windows(42).any(|w| w == sealed.as_slice()));
    assert!(!bytes.windows(2).any(|w| w == [0x98, 40]));
    let back: Probe = decode_strict(&bytes).unwrap();
    assert_eq!(back, p);
}
