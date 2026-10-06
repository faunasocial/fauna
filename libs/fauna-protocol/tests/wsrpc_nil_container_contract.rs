//! The Rust strict-decode half of the WS-RPC nil-container wire contract.
//!
//! This codifies *why* the Go mail-bridge must encode a nil slice as the
//! canonical empty list `[]` (0x80) and never `null` (0xf6): the nest's
//! `decode_strict` accepts `[]` for a non-`Option` `Vec<T>` field but
//! REJECTS `null`. A nil Go slice that marshals to `null` is exactly the
//! `UID SEARCH ALL` → `ok=false` bug (point-fixed in
//! `wsrpc.SearchMessages.terms`), generalized.
//!
//! The Go-side coverage of the encode direction lives in
//! `bins/fauna-bridges/internal/wsrpc/wsrpc_conformance_test.go`
//! (`TestWsrpcNonOptionalContainersEncodeEmptyNotNull`). Together the two
//! sides pin the contract: Go must emit `[]`, Rust requires `[]`. The wire
//! invariant is documented in
//! `docs/goal/architecture/serialization.md` § WS-RPC nil-container & float
//! invariants.

use std::collections::BTreeMap;

use fauna_protocol::bridge_routing::{SearchMessagesReply, SearchMessagesRequest};
use fauna_protocol::{Value, decode_strict, encode_canonical};

/// The canonical empty-list byte (CBOR major type 4, length 0) and the null
/// byte (major type 7, simple value 22). These are the two wire shapes the
/// contract distinguishes.
const EMPTY_LIST: u8 = 0x80;
const NULL: u8 = 0xf6;

/// Build the wire bytes for a `SearchMessagesRequest` whose `terms` field is
/// set to an arbitrary `Value` — used to inject `[]` vs `null` into the
/// `Vec<SearchTerm>` position and observe how `decode_strict` reacts.
fn request_bytes_with_terms(terms: Value) -> Vec<u8> {
    let map = Value::Map(BTreeMap::from([
        ("actor_id".to_string(), Value::Bytes(vec![0x11; 32])),
        ("mailbox".to_string(), Value::String("INBOX".to_string())),
        ("terms".to_string(), terms),
    ]));
    encode_canonical(&map)
        .expect("encode canonical request map")
        .to_vec()
}

#[test]
fn empty_vec_encodes_as_empty_list_not_null() {
    // The bedrock fact the whole contract rests on: in this codec an empty
    // Vec is `[]` (0x80) and `Option::None` / a null is `null` (0xf6). They
    // are distinct wire bytes and a strict decoder treats them differently.
    let empty = encode_canonical(&Vec::<u32>::new()).unwrap();
    assert_eq!(&empty[..], &[EMPTY_LIST], "empty Vec must encode as 0x80");

    let null = encode_canonical(&Value::Null).unwrap();
    assert_eq!(&null[..], &[NULL], "a null must encode as 0xf6");
}

#[test]
fn strict_decode_accepts_empty_list_for_non_option_vec() {
    // `terms: []` — the shape the Go side must produce for `UID SEARCH ALL`.
    let bytes = request_bytes_with_terms(Value::List(vec![]));
    let req: SearchMessagesRequest =
        decode_strict(&bytes).expect("decode_strict must ACCEPT an empty `terms` list");
    assert!(
        req.terms.is_empty(),
        "empty `terms` list decodes to an empty Vec"
    );
    assert_eq!(req.mailbox, "INBOX");
}

#[test]
fn strict_decode_rejects_null_for_non_option_vec() {
    // `terms: null` — the shape a nil Go slice produces under the default
    // fxamacker encMode. This is the bug: a non-`Option` `Vec<SearchTerm>`
    // field cannot decode `null`. `null` IS canonical CBOR, so this fails at
    // the serde deserialization layer (expected a sequence, found null),
    // *not* at the canonical-form validator — i.e. it is a genuine wire-shape
    // rejection, exactly what the MUA saw as `ok=false`.
    let bytes = request_bytes_with_terms(Value::Null);
    let res: Result<SearchMessagesRequest, _> = decode_strict(&bytes);
    assert!(
        res.is_err(),
        "decode_strict MUST REJECT `null` in a non-Option Vec position; \
         got Ok({:?}) — the nil→null wire bug would be undetected",
        res.ok()
    );
}

#[test]
fn populated_and_empty_replies_round_trip() {
    // Populated and empty `Vec<u32>` both round-trip through canonical
    // encode → strict decode unchanged (no enum construction needed — uids is
    // a plain Vec<u32>).
    for uids in [vec![1u32, 2, 9], vec![]] {
        let reply = SearchMessagesReply { uids: uids.clone() };
        let bytes = encode_canonical(&reply).unwrap();
        let back: SearchMessagesReply =
            decode_strict(&bytes).expect("reply round-trips through strict decode");
        assert_eq!(back.uids, uids);
    }
}
