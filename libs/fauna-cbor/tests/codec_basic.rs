use fauna_cbor::{Cid, decode_strict, encode_canonical};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Sample {
    a: u32,
    b: String,
    #[serde(with = "serde_bytes")]
    c: Vec<u8>,
}

#[test]
fn roundtrip_simple_struct() {
    let v = Sample {
        a: 42,
        b: "hello".to_string(),
        c: vec![1, 2, 3],
    };
    let bytes = encode_canonical(&v).expect("encode");
    let back: Sample = decode_strict(&bytes).expect("decode");
    assert_eq!(v, back);
}

#[test]
fn cid_of_encoded_bytes_is_deterministic() {
    let v = Sample {
        a: 1,
        b: "x".to_string(),
        c: vec![],
    };
    let b1 = encode_canonical(&v).expect("encode 1");
    let b2 = encode_canonical(&v).expect("encode 2");
    assert_eq!(b1, b2);
    let c1 = Cid::of_dag_cbor(&b1);
    let c2 = Cid::of_dag_cbor(&b2);
    assert_eq!(c1, c2);
}

#[test]
fn decode_rejects_truncated_input() {
    let v = Sample {
        a: 1,
        b: "x".to_string(),
        c: vec![1, 2, 3],
    };
    let bytes = encode_canonical(&v).expect("encode");
    let truncated = &bytes[..bytes.len() - 2];
    let result: Result<Sample, _> = decode_strict(truncated);
    assert!(result.is_err());
}

#[test]
fn decode_rejects_empty_input() {
    let result: Result<Sample, _> = decode_strict(&[]);
    assert!(result.is_err());
}
