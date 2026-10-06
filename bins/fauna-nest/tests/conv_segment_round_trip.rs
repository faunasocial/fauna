//! End-to-end conv-segment-store round trip (Plan 7 T7).
//!
//! Sibling of `mail_segment_round_trip.rs` for the conversation kind.
//! Drives the full WS-RPC pipeline:
//! `fauna.conversations.channel.send` → `segments::conv::append` over the
//! `__conv` `SegmentManager` → on-disk segment + `segment_records` mirror →
//! `fauna.conversations.channel.fetch` → decoded `ChannelFetchEntry`.
//!
//! The conformance tests in `conformance_conversations_channel.rs` already
//! cover the send→fetch contract; what *this* test adds is assertions on the
//! SEGMENT-STORE INTERNALS the conformance tests deliberately don't touch:
//!
//! * The `segment_records` conv mirror row landed with the right
//!   `(seq, segment_id, record_id)` — proving the SQLite half of the
//!   `segments::conv::append` chain executed, not just the in-memory reply.
//! * The on-disk `__conv/<channel_hex>/seg-NNNNNNNN.dat` segment file exists
//!   at the path the mirror row points at — proving the `SegmentManager` half
//!   wrote the record to disk and that the two halves agree.
//!
//! The handler is driven *through* the `RpcRouter` (rather than calling
//! `segments::conv::append` directly) so the kind-string + dispatch table +
//! `AppState` plumbing (`conv_segments` being threaded into the handler)
//! participate in the test surface — mirroring `mail_segment_round_trip.rs`.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{conversations_handlers, db::CacheDb, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    conversations::{
        ChannelEnvelope, ChannelFetchReply, ChannelFetchRequest, ChannelSendReply,
        ChannelSendRequest,
    },
    decode_strict as decode, encode_canonical,
};

/// Build an `AppState` with the conversations handlers actually registered on
/// the RPC router (the bare `AppState::for_test` router is empty). `for_test`
/// installs `SealedStorage` — the one `Storage` impl
/// (`docs/goal/architecture/nest/storage-modes.md`) — which always runs the
/// strict AEAD-shape verifier (`ingest_channel_envelope`), plus a
/// process-unique `conv_segments` tempdir.
async fn fixture_state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        conversations_handlers::register_conversations_handlers(&mut b);
        b.build()
    });
    Arc::new(AppState {
        rpc_router,
        ..AppState::for_test(db)
    })
}

#[tokio::test]
async fn channel_send_to_fetch_round_trip() {
    let state = fixture_state().await;

    // The conversations handlers gate on caller class via `require_permission` →
    // `caller_class_for_actor`, which resolves an actor with **no `users` row** to
    // no class at all — the dispatch is refused before the handler body runs. A
    // bare unenrolled actor is therefore *not* a User-class caller, so seed the row
    // production would have created at registration. Once seeded the actor is a
    // plain user, and User is permitted for `fauna.conversations.channel.{send,fetch}`.
    let actor: [u8; 32] = [0x11; 32];
    common::seed_dispatch_actor(&state.db, &actor).await;

    // Process-unique channel_id: `for_test` shares one `fauna-test-conv-<pid>`
    // tempdir across instances, so a distinct channel avoids colliding on
    // `__conv/<channel_hex>/` with any leftover from a prior run.
    let channel_id: [u8; 32] = [0xc7; 32];
    let channel_id_hex = hex::encode(channel_id);
    // The strict `SealedStorage::ingest_channel_envelope` verifier requires the
    // wire body to dag-cbor-decode as a `ChannelEnvelope` whose inner bytes are
    // AEAD-shaped (length >= 28, no plaintext-content magic prefix) —
    // `sealed-conv-payload`-style raw text no longer passes. Build a real
    // `ChannelEnvelope::Application(..)` over AEAD-shaped filler bytes.
    let body: Vec<u8> = ChannelEnvelope::Application(vec![0x5eu8; 32])
        .to_bytes()
        .expect("encode ChannelEnvelope::Application");

    // 1. Drive `fauna.conversations.channel.send` through the RPC router —
    //    same path the WS dispatcher takes in production.
    let send_req = ChannelSendRequest {
        channel_id: channel_id_hex.clone(),
        envelope: body.clone(),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    let send_payload = Bytes::from(encode_canonical(&send_req).expect("encode send").to_vec());
    let send_meta = state
        .rpc_router
        .kind_meta("fauna.conversations.channel.send")
        .expect("channel.send kind registered");
    let send_reply_bytes = (send_meta.handler)(state.clone(), actor, send_payload)
        .await
        .expect("send handler ok");
    let send_reply: ChannelSendReply = decode(&send_reply_bytes).expect("decode send reply");
    assert_eq!(send_reply.seq, 1, "first message on the channel gets seq 1");

    // 2. `segment_records` conv mirror row — the SQLite half of
    //    `segments::conv::append` landed: one row, seq 1, in segment 1, with a
    //    32-byte record_id. (The conformance tests never read this table.)
    let mirror = state
        .db
        .segment_records_list_conv_after_seq(&channel_id, 0, 100)
        .await
        .expect("segment_records_list_conv_after_seq");
    assert_eq!(
        mirror.len(),
        1,
        "exactly one conv mirror row for the channel"
    );
    let (mirror_seq, mirror_seg_id, mirror_record_id, mirror_legal_ref) = &mirror[0];
    assert_eq!(*mirror_seq, 1, "mirror row carries the assigned seq");
    assert_eq!(
        *mirror_legal_ref, None,
        "a live conv record has no takedown ref"
    );
    assert_ne!(
        mirror_record_id.digest(),
        [0u8; 32],
        "record_id is a real (non-zero) 32-byte digest"
    );

    // 3. On-disk segment file — the `SegmentManager` half wrote the record to
    //    disk, and the file lives at the path the mirror row points at. Flush
    //    the open segment first (production reads do this lazily; here we
    //    assert the on-disk artifact directly, before the fetch).
    state
        .conv_segments
        .finalize_open(&channel_id)
        .await
        .expect("finalize open conv segment");
    let seg_path = state
        .conv_segments
        .segment_file_path(&channel_id, *mirror_seg_id);
    assert!(
        seg_path.exists(),
        "on-disk conv segment file must exist at the mirror-pointed path: {}",
        seg_path.display(),
    );
    // Robustness: the channel's scope dir is non-empty regardless of exact
    // seg id arithmetic (`__conv/<channel_hex>/`).
    let scope_dir = state
        .conv_segments
        .data_dir()
        .join("__conv")
        .join(&channel_id_hex);
    let mut entries = std::fs::read_dir(&scope_dir)
        .unwrap_or_else(|e| panic!("read scope dir {}: {e}", scope_dir.display()));
    assert!(
        entries.next().is_some(),
        "conv scope dir {} must be non-empty",
        scope_dir.display(),
    );

    // 4. Drive `fauna.conversations.channel.fetch` — the read path opens the
    //    segment file, streams the framed record, and yields bytes that decode
    //    back to the exact posted envelope (the load-bearing guarantee).
    let fetch_req = ChannelFetchRequest {
        channel_id: channel_id_hex.clone(),
        after: 0,
        limit: 100,
        nest_url: None,
        extra: std::collections::BTreeMap::new(),
    };
    let fetch_payload = Bytes::from(encode_canonical(&fetch_req).expect("encode fetch").to_vec());
    let fetch_meta = state
        .rpc_router
        .kind_meta("fauna.conversations.channel.fetch")
        .expect("channel.fetch kind registered");
    let fetch_reply_bytes = (fetch_meta.handler)(state.clone(), actor, fetch_payload)
        .await
        .expect("fetch handler ok");
    let fetch_reply: ChannelFetchReply = decode(&fetch_reply_bytes).expect("decode fetch reply");
    assert_eq!(fetch_reply.messages.len(), 1, "one message fetched back");
    assert_eq!(fetch_reply.messages[0].seq, 1, "fetched seq matches");
    assert_eq!(
        fetch_reply.messages[0].envelope, body,
        "fetched envelope is byte-identical to the posted body",
    );
}

/// The conversation kind's blob-reachability floor, driven through the RPC
/// router exactly as a client's `channel.send` arrives: the plaintext
/// `attachment_refs` a sender lists beside the sealed envelope land in
/// `conv_attachment_refs` under the record's assigned seq (the same
/// transaction as its mirror row), a send without the field records nothing,
/// and a malformed entry is refused as `invalid_params` rather than silently
/// dropped (`docs/goal/ui/conversations.md` § Encryption at rest →
/// *Attachment reachability*; `encryption-at-rest.md` § Per-content-kind
/// conformance → Conversation messages row).
#[tokio::test]
async fn channel_send_records_attachment_refs_beside_the_mirror_row() {
    let state = fixture_state().await;
    let actor: [u8; 32] = [0x12; 32];
    common::seed_dispatch_actor(&state.db, &actor).await;
    let channel_id: [u8; 32] = [0xc8; 32];
    let channel_id_hex = hex::encode(channel_id);
    let send_meta = state
        .rpc_router
        .kind_meta("fauna.conversations.channel.send")
        .expect("channel.send kind registered");

    let photo = [0xa1u8; 32];
    let thumb = [0xb2u8; 32];
    let with_refs = ChannelSendRequest {
        channel_id: channel_id_hex.clone(),
        envelope: ChannelEnvelope::Application(vec![0x5eu8; 32])
            .to_bytes()
            .expect("encode"),
        expect_no_commit_since: None,
        attachment_refs: vec![hex::encode(photo), hex::encode(thumb)],
        extra: std::collections::BTreeMap::new(),
    };
    let payload = Bytes::from(encode_canonical(&with_refs).expect("encode").to_vec());
    let reply: ChannelSendReply = decode(
        &(send_meta.handler)(state.clone(), actor, payload)
            .await
            .expect("send with refs ok"),
    )
    .expect("decode reply");
    assert_eq!(reply.seq, 1);
    assert_eq!(
        state
            .db
            .conv_attachment_refs(&channel_id, 1)
            .await
            .expect("refs"),
        vec![photo, thumb],
        "both refs recorded under the record's seq"
    );

    // An attachment-less send records nothing.
    let plain = ChannelSendRequest {
        channel_id: channel_id_hex.clone(),
        envelope: ChannelEnvelope::Application(vec![0x5fu8; 32])
            .to_bytes()
            .expect("encode"),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    let payload = Bytes::from(encode_canonical(&plain).expect("encode").to_vec());
    let reply: ChannelSendReply = decode(
        &(send_meta.handler)(state.clone(), actor, payload)
            .await
            .expect("plain send ok"),
    )
    .expect("decode reply");
    assert_eq!(reply.seq, 2);
    assert!(
        state
            .db
            .conv_attachment_refs(&channel_id, 2)
            .await
            .expect("refs")
            .is_empty()
    );

    // A ref the nest cannot key by is refused, never dropped: dropping it
    // would leave that attachment unpinned while the client believes it named it.
    let malformed = ChannelSendRequest {
        channel_id: channel_id_hex.clone(),
        envelope: ChannelEnvelope::Application(vec![0x60u8; 32])
            .to_bytes()
            .expect("encode"),
        expect_no_commit_since: None,
        attachment_refs: vec!["not-a-hash".into()],
        extra: std::collections::BTreeMap::new(),
    };
    let payload = Bytes::from(encode_canonical(&malformed).expect("encode").to_vec());
    let err = (send_meta.handler)(state.clone(), actor, payload)
        .await
        .expect_err("a malformed attachment ref is refused");
    assert_eq!(
        err.code, "fauna.conversations.invalid_params",
        "refused as an input error, got {err:?}"
    );
    // …and consumed no seq: the next good send is seq 3, not 4.
    let after = ChannelSendRequest {
        channel_id: channel_id_hex,
        envelope: ChannelEnvelope::Application(vec![0x61u8; 32])
            .to_bytes()
            .expect("encode"),
        expect_no_commit_since: None,
        attachment_refs: Vec::new(),
        extra: std::collections::BTreeMap::new(),
    };
    let payload = Bytes::from(encode_canonical(&after).expect("encode").to_vec());
    let reply: ChannelSendReply = decode(
        &(send_meta.handler)(state.clone(), actor, payload)
            .await
            .expect("send ok"),
    )
    .expect("decode reply");
    assert_eq!(reply.seq, 3, "a refused send consumed no seq");
}
