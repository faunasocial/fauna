//! End-to-end conv snapshot/restore round trip (Plan 8 T6).
//!
//! Sibling of `conv_segment_round_trip.rs`, exercising the conv
//! snapshot/restore surface added in Plan 8 (T4/T5) through the WS-RPC
//! router rather than calling the handlers directly. The full pipeline:
//!
//! `fauna.conversations.channel.send` ×N
//!   → `segments::conv::append` over the `__conv` `SegmentManager`
//!   → `segment_records` conv mirror + on-disk segment
//! `fauna.filesync.snapshot.create_message_kind {kind:"conv", actor_id:Some(channel)}`
//!   → `create_conv` finalizes the open segment, pins its `Manifest`
//! `fauna.conversations.channel.send` ×M (rotate into a NEW, unpinned segment)
//! `fauna.filesync.snapshot.restore_message_kind {snapshot_id, confirm_id}`
//!   → `restore_conv` DELETEs the channel's conv rows + rebuilds from the
//!     pinned segments' on-disk footers
//! `fauna.conversations.channel.fetch`
//!   → exactly the first N records survive; the post-snapshot M are gone.
//!
//! What this adds over the in-source `filesync_handlers` conv tests: the
//! conversations send/fetch handlers AND the filesync snapshot handlers
//! share ONE `RpcRouter` + `AppState`, so the kind-string dispatch table,
//! the `conv_segments` field being threaded into BOTH handler families,
//! and the request/reply DAG-CBOR encode/decode all participate — the
//! seam the per-handler unit tests deliberately don't cross.
//!
//! The snapshot's pin property is the load-bearing assertion: because
//! `create_conv` calls `finalize_open` itself, the N sends BEFORE the
//! snapshot land in a segment the snapshot pins, while the M sends AFTER
//! rotate into a fresh (unpinned) segment that restore drops.

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorId;
use fauna_nest::{
    conversations_handlers, db::CacheDb, filesync_handlers, routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::{
    conversations::{
        ChannelEnvelope, ChannelFetchReply, ChannelFetchRequest, ChannelSendReply,
        ChannelSendRequest,
    },
    decode_strict as decode, encode_canonical,
    filesync::{
        SnapshotCreateMessageKindReply, SnapshotCreateMessageKindRequest,
        SnapshotRestoreMessageKindReply, SnapshotRestoreMessageKindRequest,
    },
};

/// Inner bytes must clear the AEAD-shape floor the strict ingest verifier
/// applies to every channel envelope (>= 28 bytes = 12-byte nonce + 16-byte
/// tag, no plaintext-content magic prefix). Raw byte literals only ever
/// worked because `AppState::for_test` used to install the permissive
/// plaintext arm; it now installs the one and only `SealedStorage`, whose
/// `ingest_channel_envelope` always strict-decodes + AEAD-shape-checks.
/// `marker` keeps the byte-level distinguishability the raw literals gave
/// the pre/post-snapshot bodies.
fn app_env(marker: u8) -> Vec<u8> {
    ChannelEnvelope::Application(vec![marker; 32])
        .to_bytes()
        .unwrap()
}

/// `(tempdir, state)` with BOTH the conversations and filesync handlers
/// registered on one RPC router, and `conv_segments` rooted in a per-test
/// tempdir.
///
/// The `AppState::for_test` conv-segments dir is process-shared by PID
/// (memory: `segment-store-for-test-tempdir-shared`); a per-test tempdir
/// gives the snapshot a stable, isolated `__conv/<channel_hex>/` to pin
/// and restore-from. We hold the `TempDir` for the test's lifetime so the
/// on-disk segment files outlive the state (restore reads them back).
fn fixture_state() -> (tempfile::TempDir, Arc<AppState>) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        conversations_handlers::register_conversations_handlers(&mut b);
        filesync_handlers::register_filesync_handlers(&mut b);
        b.build()
    });
    let mut state = AppState {
        rpc_router,
        ..AppState::for_test(db)
    };
    state.conv_segments = Arc::new(fauna_segment_store::SegmentManager::new(
        tmp.path().to_path_buf(),
        "conv",
    ));
    (tmp, Arc::new(state))
}

/// Drive one RPC kind through the router and decode the reply.
async fn drive<Req, Reply>(state: &Arc<AppState>, actor: [u8; 32], kind: &str, req: &Req) -> Reply
where
    Req: serde::Serialize,
    Reply: serde::de::DeserializeOwned,
{
    common::seed_dispatch_actor(&state.db, &actor).await;
    let payload = Bytes::from(encode_canonical(req).expect("encode req").to_vec());
    let meta = state
        .rpc_router
        .kind_meta(kind)
        .unwrap_or_else(|| panic!("{kind} kind registered"));
    let reply_bytes = (meta.handler)(state.clone(), actor, payload)
        .await
        .unwrap_or_else(|e| panic!("{kind} handler ok: {e:?}"));
    decode(&reply_bytes).unwrap_or_else(|e| panic!("decode {kind} reply: {e:?}"))
}

#[tokio::test]
async fn conv_snapshot_restore_round_trip() {
    let (_tmp, state) = fixture_state();

    // `drive` seeds this actor's `users` row before every dispatch: the authority
    // gate (`bridge_method_allowlist::caller_class_for_actor`) resolves an actor
    // with no row to *no* class at all, so a bare unenrolled actor is refused
    // ahead of the handler body — it no longer falls through to
    // `CallerClass::User`. Once seeded it is a plain user, which the conversations
    // channel send/fetch kinds permit. The first `channel.send` then auto-registers
    // the sender into the channel (`register_actor_channel`), so by snapshot-create
    // time the caller is a channel member and passes the membership auth on the
    // filesync handlers.
    let actor: [u8; 32] = [0x4a; 32];
    // Distinct channel id (PID-shared for-test dir would otherwise collide).
    let channel_id: [u8; 32] = [0x4c; 32];
    let channel_id_hex = hex::encode(channel_id);

    // ── 1. Send N=2 messages (pre-snapshot) ──────────────────────
    let pre_bodies: [Vec<u8>; 2] = [app_env(0x01), app_env(0x02)];
    for (i, body) in pre_bodies.iter().enumerate() {
        let send: ChannelSendReply = drive(
            &state,
            actor,
            "fauna.conversations.channel.send",
            &ChannelSendRequest {
                channel_id: channel_id_hex.clone(),
                envelope: body.clone(),
                expect_no_commit_since: None,
                attachment_refs: Vec::new(),
                extra: std::collections::BTreeMap::new(),
            },
        )
        .await;
        assert_eq!(
            send.seq,
            (i as i64) + 1,
            "pre-snapshot send gets sequential seq"
        );
    }

    // ── 2. Create the conv snapshot (membership-checked) ─────────
    // `create_conv` finalizes the open segment, so the 2 pre-snapshot
    // records are pinned in a closed segment the snapshot's Manifest covers.
    let create: SnapshotCreateMessageKindReply = drive(
        &state,
        actor,
        "fauna.filesync.snapshot.create_message_kind",
        &SnapshotCreateMessageKindRequest {
            kind: "conv".into(),
            actor_id: Some(ActorId(channel_id)),
            extra: std::collections::BTreeMap::new(),
        },
    )
    .await;
    assert_eq!(create.kind, "conv");
    assert_eq!(create.actor_id.0, actor, "reply actor is the bearer");
    assert!(create.snapshot_id > 0, "a real snapshot row was created");
    assert_eq!(
        create.snapshot_ids.len(),
        1,
        "per-channel scope → exactly one snapshot id"
    );
    assert_eq!(create.snapshot_id, create.snapshot_ids[0]);
    let snapshot_id = create.snapshot_id;

    // ── 3. Send M=3 more messages (post-snapshot) ────────────────
    // These rotate into a NEW segment that the pinned manifest does NOT
    // include, so restore physically reverts them.
    let post_bodies: [Vec<u8>; 3] = [app_env(0x03), app_env(0x04), app_env(0x05)];
    for (i, body) in post_bodies.iter().enumerate() {
        let send: ChannelSendReply = drive(
            &state,
            actor,
            "fauna.conversations.channel.send",
            &ChannelSendRequest {
                channel_id: channel_id_hex.clone(),
                envelope: body.clone(),
                expect_no_commit_since: None,
                attachment_refs: Vec::new(),
                extra: std::collections::BTreeMap::new(),
            },
        )
        .await;
        assert_eq!(
            send.seq,
            (i as i64) + 3,
            "post-snapshot send continues the seq counter"
        );
    }

    // ── 4. Sanity: all 5 messages visible before restore ─────────
    let before_restore: ChannelFetchReply = drive(
        &state,
        actor,
        "fauna.conversations.channel.fetch",
        &ChannelFetchRequest {
            channel_id: channel_id_hex.clone(),
            after: 0,
            limit: 100,
            // Same-nest fetch from the local log (cross-nest relay added
            // `nest_url`; absent/None ⇒ same-nest).
            nest_url: None,
            extra: std::collections::BTreeMap::new(),
        },
    )
    .await;
    assert_eq!(
        before_restore.messages.len(),
        5,
        "all five records visible before restore (2 pinned + 3 post-snapshot)"
    );

    // ── 5. Restore the snapshot ──────────────────────────────────
    let restore: SnapshotRestoreMessageKindReply = drive(
        &state,
        actor,
        "fauna.filesync.snapshot.restore_message_kind",
        &SnapshotRestoreMessageKindRequest {
            snapshot_id,
            confirm_id: snapshot_id.to_string(),
            extra: std::collections::BTreeMap::new(),
        },
    )
    .await;
    assert_eq!(restore.kind, "conv");
    assert_eq!(restore.snapshot_id, snapshot_id);

    // ── 6. Fetch: exactly the 2 pinned messages survive ──────────
    let after_restore: ChannelFetchReply = drive(
        &state,
        actor,
        "fauna.conversations.channel.fetch",
        &ChannelFetchRequest {
            channel_id: channel_id_hex.clone(),
            after: 0,
            limit: 100,
            // Same-nest fetch from the local log (cross-nest relay added
            // `nest_url`; absent/None ⇒ same-nest).
            nest_url: None,
            extra: std::collections::BTreeMap::new(),
        },
    )
    .await;
    assert_eq!(
        after_restore.messages.len(),
        2,
        "restore reverts to the 2 pinned records; the 3 post-snapshot records are dropped"
    );
    assert_eq!(
        after_restore
            .messages
            .iter()
            .map(|m| m.seq)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "the surviving records are the first two by seq"
    );
    assert_eq!(
        after_restore.messages[0].envelope, pre_bodies[0],
        "seq 1 envelope is byte-identical to the first posted body"
    );
    assert_eq!(
        after_restore.messages[1].envelope, pre_bodies[1],
        "seq 2 envelope is byte-identical to the second posted body"
    );
}
