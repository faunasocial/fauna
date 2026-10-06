//! One-shot regenerator for the four cross-language Push-frame fixtures
//! under `bins/fauna-bridges/internal/wsrpc/testdata/`:
//!
//!   push-mailbox-state-append.cbor
//!   push-mailbox-state-flags.cbor
//!   push-mailbox-state-expunge.cbor
//!   push-mailbox-state-move.cbor
//!
//! These pin the `fauna.bridges.push.mailbox_state` push wire — the *only*
//! push kind the Go mail-bridge decodes in production (every other push kind
//! is consumed solely by the shared Rust `PushEvent::from_push` decoder, which
//! the WASM/UniFFI receivers share, so there is no independent decoder to drift
//! against). The Go bridge runs its own hand-written dag-cbor decoder
//! (`internal/dagcbor`), so this fixture is what catches Rust→Go drift in the
//! `BridgeMailboxStatePush` / `MailboxStateEvent` wire shape: the existing Go
//! test (`client_test.go:TestPushHandler_BridgeMailboxState`) only round-trips
//! Go→Go and cannot see a Rust-side field rename or tag-style change.
//!
//! One fixture per `MailboxStateEvent` variant because the variants have
//! disjoint field sets — `Move` in particular carries `src_uid`/`dst_uid`/
//! `modseq_src`/`modseq_dst`/`side`, field names that appear in no other variant.
//!
//! The bytes are produced by the production Rust path (encode the typed
//! payload canonically, lift to a generic `Value`, wrap in `Frame::Push`,
//! `encode_frame`) so they are byte-identical to what `WsState::notify_push`
//! emits in nest (`bins/fauna-nest/src/ws.rs`).
//!
//! Run from the workspace root:
//!
//!     cargo run -p fauna-protocol --example regen_go_push_fixture
//!
//! then commit the updated fixtures.

use fauna_protocol::Value;
use fauna_protocol::bridge_routing::{
    BridgeMailboxStatePush, MailboxStateEvent, MoveSide, PUSH_KIND_BRIDGE_MAILBOX_STATE,
};
use fauna_protocol::envelope::{Frame, Push, encode_frame};
use fauna_protocol::{decode_strict, encode_canonical};
use std::path::PathBuf;

/// Common actor id across all four fixtures: a recognizable 32-byte pattern.
const ACTOR_ID: [u8; 32] = [0x11; 32];

/// Build a canonical Push frame for one `BridgeMailboxStatePush`, mirroring
/// `WsState::notify_push` exactly: typed payload → canonical bytes → generic
/// `Value` → `Frame::Push` → `encode_frame`. `seq` is fixed (the value is
/// per-connection in production but irrelevant to the wire-shape contract).
fn push_frame_bytes(body: &BridgeMailboxStatePush) -> Vec<u8> {
    let cbor = encode_canonical(body).expect("encode_canonical(BridgeMailboxStatePush)");
    let payload: Value = decode_strict(&cbor).expect("decode_strict to Value");
    let frame = Frame::Push(Push {
        ty: Push::TYPE,
        kind: PUSH_KIND_BRIDGE_MAILBOX_STATE.to_string(),
        payload,
        seq: 1,
    });
    encode_frame(&frame).expect("encode_frame").to_vec()
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let testdata = manifest.join("../../bins/fauna-bridges/internal/wsrpc/testdata");

    let fixtures = [
        (
            "push-mailbox-state-append.cbor",
            BridgeMailboxStatePush {
                subscription_id: 1,
                actor_id: ACTOR_ID.to_vec(),
                mailbox: "INBOX".to_string(),
                event: MailboxStateEvent::Append {
                    uid: 101,
                    flags: vec!["\\Seen".to_string(), "\\Recent".to_string()],
                    modseq: 5000,
                },
            },
        ),
        (
            "push-mailbox-state-flags.cbor",
            BridgeMailboxStatePush {
                subscription_id: 2,
                actor_id: ACTOR_ID.to_vec(),
                mailbox: "INBOX".to_string(),
                event: MailboxStateEvent::Flags {
                    uid: 102,
                    flags: vec!["\\Flagged".to_string()],
                    modseq: 5001,
                },
            },
        ),
        (
            "push-mailbox-state-expunge.cbor",
            BridgeMailboxStatePush {
                subscription_id: 3,
                actor_id: ACTOR_ID.to_vec(),
                mailbox: "INBOX".to_string(),
                event: MailboxStateEvent::Expunge {
                    uid: 103,
                    modseq: 5002,
                },
            },
        ),
        (
            "push-mailbox-state-move.cbor",
            BridgeMailboxStatePush {
                subscription_id: 4,
                actor_id: ACTOR_ID.to_vec(),
                mailbox: "Archive".to_string(),
                event: MailboxStateEvent::Move {
                    src_uid: 104,
                    dst_uid: 204,
                    modseq_src: 5003,
                    modseq_dst: 5004,
                    side: MoveSide::Destination,
                },
            },
        ),
    ];

    for (name, body) in &fixtures {
        let bytes = push_frame_bytes(body);
        let out = testdata.join(name);
        std::fs::write(&out, &bytes).expect("write fixture");
        println!("wrote {} ({} bytes)", out.display(), bytes.len());
    }
}
