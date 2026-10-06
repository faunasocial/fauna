//! Pin Rust types against the canonical test vectors.
//!
//! For each vector: decode bytes → matching Rust type → re-encode →
//! assert byte-for-byte equality. This catches: (a) Rust type drift from
//! CDDL, (b) non-canonical encoder behavior.

use std::fs;

use fauna_protocol::codec::{decode_strict as decode, encode_canonical};
use fauna_protocol::envelope::{Frame, decode_frame, encode_frame};
use fauna_protocol::protocol_kinds::{EchoReply, EchoRequest};
use fauna_protocol::push_events::{KnockPayload, ResyncRequiredPayload};

const VECTORS: &str = "schemas/test_vectors";

fn read_vector(name: &str) -> Vec<u8> {
    fs::read(format!("{VECTORS}/{name}")).unwrap_or_else(|e| {
        panic!("read vector {name}: {e}");
    })
}

#[test]
fn envelope_request_pins() {
    let bytes = read_vector("envelope_request.bin");
    let frame = decode_frame(&bytes).unwrap();
    match &frame {
        Frame::Request(req) => {
            assert_eq!(req.correlation_id, 42);
            assert_eq!(req.kind, "fauna.protocol.echo");
        }
        _ => panic!("expected Request"),
    }
    let reencoded = encode_frame(&frame).unwrap();
    assert_eq!(&reencoded[..], &bytes[..], "non-canonical re-encoding");
}

#[test]
fn envelope_reply_ok_pins() {
    let bytes = read_vector("envelope_reply_ok.bin");
    let frame = decode_frame(&bytes).unwrap();
    match &frame {
        Frame::Reply(r) => assert!(r.ok),
        _ => panic!("expected Reply"),
    }
    let reencoded = encode_frame(&frame).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn envelope_reply_err_pins() {
    let bytes = read_vector("envelope_reply_err.bin");
    let frame = decode_frame(&bytes).unwrap();
    match &frame {
        Frame::Reply(r) => assert!(!r.ok),
        _ => panic!("expected Reply"),
    }
    let reencoded = encode_frame(&frame).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn envelope_push_pins() {
    let bytes = read_vector("envelope_push.bin");
    let frame = decode_frame(&bytes).unwrap();
    match &frame {
        Frame::Push(p) => assert_eq!(p.kind, "fauna.knock"),
        _ => panic!("expected Push"),
    }
    let reencoded = encode_frame(&frame).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn envelope_cancel_pins() {
    let bytes = read_vector("envelope_cancel.bin");
    let frame = decode_frame(&bytes).unwrap();
    match &frame {
        Frame::Cancel(c) => assert_eq!(c.correlation_id, 42),
        _ => panic!("expected Cancel"),
    }
    let reencoded = encode_frame(&frame).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn push_knock_pins() {
    let bytes = read_vector("push_knock.bin");
    let p: KnockPayload = decode(&bytes).unwrap();
    assert_eq!(p.sender_id, "abcd1234");
    let reencoded = encode_canonical(&p).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn push_resync_required_pins() {
    let bytes = read_vector("push_resync_required.bin");
    let p: ResyncRequiredPayload = decode(&bytes).unwrap();
    assert_eq!(p.dropped_count, 7);
    let reencoded = encode_canonical(&p).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn echo_request_pins() {
    let bytes = read_vector("echo_request.bin");
    let r: EchoRequest = decode(&bytes).unwrap();
    assert_eq!(r.data, vec![1, 2, 3]);
    let reencoded = encode_canonical(&r).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}

#[test]
fn echo_reply_pins() {
    let bytes = read_vector("echo_reply.bin");
    let r: EchoReply = decode(&bytes).unwrap();
    assert_eq!(r.data, vec![1, 2, 3]);
    let reencoded = encode_canonical(&r).unwrap();
    assert_eq!(&reencoded[..], &bytes[..]);
}
