//! Property tests over `decode_frame`, the first parser every WS-RPC binary frame
//! meets before identity or allowlist checks. Bytes are canonical DAG-CBOR with
//! integer map keys; the residual bug class on stranger input is a panic or an
//! allocation off a lying length. The transport caps a frame at 2 MiB and the
//! validator caps nesting at 256, so the generators stay inside those bounds
//! and the properties are: never panic, a valid frame round-trips, and a
//! corrupted valid frame is an error or a frame, never a crash.

use crate::{Cancel, Frame, Push, Reply, Request, Value, decode_frame, encode_frame};
use proptest::prelude::*;

fn arb_payload() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        proptest::collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
        "[ -~]{0,24}".prop_map(Value::String),
    ]
}

fn arb_frame() -> impl Strategy<Value = Frame> {
    prop_oneof![
        (
            any::<u64>(),
            "[a-z.]{1,40}",
            any::<[u8; 16]>(),
            arb_payload()
        )
            .prop_map(
                |(correlation_id, kind, idempotency_key, payload)| Frame::Request(Request {
                    ty: Request::TYPE,
                    correlation_id,
                    kind,
                    idempotency_key,
                    payload,
                    replay_forbidden: None,
                    deadline_ms: None,
                })
            ),
        (any::<u64>(), arb_payload(), any::<bool>()).prop_map(|(correlation_id, payload, ok)| {
            Frame::Reply(Reply {
                ty: Reply::TYPE,
                correlation_id,
                payload,
                ok,
            })
        }),
        ("[a-z.]{1,40}", arb_payload(), any::<u64>()).prop_map(|(kind, payload, seq)| {
            Frame::Push(Push {
                ty: Push::TYPE,
                kind,
                payload,
                seq,
            })
        }),
        any::<u64>().prop_map(|correlation_id| Frame::Cancel(Cancel {
            ty: Cancel::TYPE,
            correlation_id,
        })),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = decode_frame(&bytes);
    }

    /// A frame the encoder writes decodes, and re-encodes byte-identically.
    #[test]
    fn encoded_frames_roundtrip(frame in arb_frame()) {
        let bytes = encode_frame(&frame).expect("encodes");
        let back = decode_frame(&bytes).expect("a canonical frame decodes");
        prop_assert_eq!(encode_frame(&back).expect("re-encodes"), bytes);
    }

    /// Single-byte corruption, truncation and trailing garbage on a valid frame
    /// are refused or accepted as some frame — never a panic.
    #[test]
    fn corrupted_frames_never_panic(
        frame in arb_frame(),
        idx in any::<prop::sample::Index>(),
        byte in any::<u8>(),
        cut in any::<prop::sample::Index>(),
        tail in proptest::collection::vec(any::<u8>(), 0..8),
    ) {
        let bytes = encode_frame(&frame).expect("encodes").to_vec();
        let mut flipped = bytes.clone();
        let i = idx.index(flipped.len());
        flipped[i] = byte;
        let _ = decode_frame(&flipped);
        let _ = decode_frame(&bytes[..cut.index(bytes.len() + 1)]);
        let mut extended = bytes;
        extended.extend_from_slice(&tail);
        let _ = decode_frame(&extended);
    }

    /// A frame-shaped map whose type discriminant is any integer: the unknown
    /// ones are `UnknownType`, never a panic.
    #[test]
    fn arbitrary_discriminants_never_panic(ty in any::<u64>(), rest in proptest::collection::vec(any::<u8>(), 0..16)) {
        let mut buf = vec![0xa1, 0x00, 0x1b];
        buf.extend_from_slice(&ty.to_be_bytes());
        buf.extend_from_slice(&rest);
        let _ = decode_frame(&buf);
    }

    /// Depth far past the validator bound inside a frame's payload is an error.
    #[test]
    fn deeply_nested_payload_is_refused(depth in 257usize..5_000) {
        let mut buf = vec![0xa2, 0x00, 0x03, 0x01]; // {0: 3, 1: <nested>}
        buf.extend(std::iter::repeat_n(0x81, depth));
        buf.push(0x00);
        prop_assert!(decode_frame(&buf).is_err());
    }
}
