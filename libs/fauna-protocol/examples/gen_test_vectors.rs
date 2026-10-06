//! Generates conformance test vectors. Run once; check the .bin files in.
//! Re-run only when the canonical-form bytes for a vector change (which
//! they should NOT — that would be a forward-compat violation).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use fauna_protocol::Value;
use fauna_protocol::codec::encode_canonical;
use fauna_protocol::envelope::{Cancel, Frame, Push, Reply, Request, encode_frame};
use fauna_protocol::protocol_kinds::{EchoReply, EchoRequest};
use fauna_protocol::push_events::{KnockPayload, ResyncRequiredPayload};

fn write(out: &Path, name: &str, bytes: &[u8]) {
    let path = out.join(name);
    fs::write(&path, bytes).expect("write vector");
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}

fn main() {
    let out = Path::new("libs/fauna-protocol/schemas/test_vectors");
    fs::create_dir_all(out).expect("mkdir");

    // Envelope vectors.
    write(
        out,
        "envelope_request.bin",
        &encode_frame(&Frame::Request(Request {
            ty: Request::TYPE,
            correlation_id: 42,
            kind: "fauna.protocol.echo".into(),
            idempotency_key: [0u8; 16],
            payload: Value::Map(Default::default()),
            replay_forbidden: None,
            deadline_ms: None,
        }))
        .unwrap(),
    );

    write(
        out,
        "envelope_reply_ok.bin",
        &encode_frame(&Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id: 42,
            payload: Value::Null,
            ok: true,
        }))
        .unwrap(),
    );

    write(
        out,
        "envelope_reply_err.bin",
        &encode_frame(&Frame::Reply(Reply {
            ty: Reply::TYPE,
            correlation_id: 42,
            payload: Value::String("encoded_error".into()),
            ok: false,
        }))
        .unwrap(),
    );

    write(
        out,
        "envelope_push.bin",
        &encode_frame(&Frame::Push(Push {
            ty: Push::TYPE,
            kind: "fauna.knock".into(),
            payload: Value::Map(Default::default()),
            seq: 1,
        }))
        .unwrap(),
    );

    write(
        out,
        "envelope_cancel.bin",
        &encode_frame(&Frame::Cancel(Cancel {
            ty: Cancel::TYPE,
            correlation_id: 42,
        }))
        .unwrap(),
    );

    // Push payload vectors.
    write(
        out,
        "push_knock.bin",
        &encode_canonical(&KnockPayload {
            sender_id: "abcd1234".into(),
            summary: "wants to connect".into(),
            ..Default::default()
        })
        .unwrap(),
    );

    write(
        out,
        "push_resync_required.bin",
        &encode_canonical(&ResyncRequiredPayload {
            dropped_count: 7,
            extra: BTreeMap::new(),
        })
        .unwrap(),
    );

    // Protocol kind vectors.
    write(
        out,
        "echo_request.bin",
        &encode_canonical(&EchoRequest {
            data: vec![1, 2, 3],
            extra: BTreeMap::new(),
        })
        .unwrap(),
    );

    write(
        out,
        "echo_reply.bin",
        &encode_canonical(&EchoReply {
            data: vec![1, 2, 3],
            extra: BTreeMap::new(),
        })
        .unwrap(),
    );

    println!("done.");
}
