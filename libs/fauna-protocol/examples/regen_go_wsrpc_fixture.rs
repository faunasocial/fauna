//! One-shot regenerator for
//! `bins/fauna-bridges/internal/wsrpc/testdata/request-validate-recipient-frame.cbor`.
//!
//! This is the FRAMED fixture (a full WS-RPC Request envelope), distinct from
//! the bare request-body fixtures `request-<kebab>.cbor` produced by the Go
//! generator (`internal/wsrpc/wsrpc_request_fixtures_gen_test.go`) and consumed
//! by `wsrpc_request_cross_language.rs`. The `-frame` suffix keeps the
//! `request-<kebab>.cbor` namespace uniformly = bare body, symmetric with
//! `reply-<kebab>.cbor`.
//!
//! This file is committed back into the Go bridge tree. The Rust generator
//! lives here (rather than in the Go test) because the fixture must be
//! produced by the Rust canonical encoder so that the bytes the Go side
//! decodes are byte-identical to what production Rust would emit.
//!
//! Run from the workspace root:
//!
//!     cargo run -p fauna-protocol --example regen_go_wsrpc_fixture

use fauna_protocol::Value;
use fauna_protocol::envelope::{Frame, Request, encode_frame};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() {
    let payload = Value::Map(BTreeMap::from([
        ("local_part".to_string(), Value::String("alice".into())),
        ("domain".to_string(), Value::String("example.com".into())),
    ]));
    let req = Request {
        ty: Request::TYPE,
        correlation_id: 42,
        kind: "fauna.bridges.validate_recipient".into(),
        idempotency_key: [0u8; 16],
        payload,
        replay_forbidden: None,
        deadline_ms: None,
    };
    let bytes = encode_frame(&Frame::Request(req)).unwrap();

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = manifest.join(
        "../../bins/fauna-bridges/internal/wsrpc/testdata/request-validate-recipient-frame.cbor",
    );
    std::fs::write(&out, bytes.as_ref()).expect("write fixture");
    println!("wrote {} ({} bytes)", out.display(), bytes.len());
}
