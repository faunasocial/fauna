//! Integration test — `fauna.bridges.push.mailbox_state` push fires
//! when a state-mutating IMAP RPC commits, per I5 Phase F.1 (TODO §
//! Load-bearing claims, claim 2: event-translation table).
//!
//! Drives `WsState::subscribe` directly + invokes handlers through the
//! RPC router (same pattern as `tests/segments_changed_push.rs`). The
//! integration target is:
//!   - `subscribe_mailbox_state` registers a row in the in-memory
//!     `bridge_push_registry` keyed by `(served_user_actor_id,
//!     mailbox)` and returns a unique `subscription_id`;
//!   - APPEND on that `(actor, mailbox)` fires one
//!     `BridgeMailboxStatePush` with the expected UID + flags + modseq;
//!   - STORE +FLAGS fires a `Flags` event;
//!   - EXPUNGE fires per-UID `Expunge` events;
//!   - MOVE fires `Move` events to BOTH source and destination
//!     subscribers (claim 2: Move emits EXPUNGE for src AND
//!     EXISTS+FETCH for dst).
//!
//! Each test fully spawns an `AppState` (in-process), then exercises
//! the RPC router via `kind_meta(...).handler(state, mda_actor,
//! payload)` — the exact same dispatch path the WS request loop in
//! `routes::dispatch_request` takes.

mod common;
use common::{approve_bridge, sealed};

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_nest::bridge_imap_handlers::register_bridge_imap_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    AppendMessageRequest, BridgeMailboxStatePush, ExpungeRequest, MailboxStateEvent,
    MoveMessagesRequest, MoveSide, StoreFlagsOp, StoreFlagsRequest, SubscribeMailboxStateReply,
    SubscribeMailboxStateRequest,
};
use fauna_protocol::{Frame, PushEvent, decode_frame, encode_canonical};
use tokio::sync::mpsc;

/// Build an `AppState` plus a registered RPC router for the IMAP
/// handlers under test. Caller decides which bridge / actor roles to
/// approve.
fn build_state() -> Arc<AppState> {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
    let rpc_router = Arc::new({
        let mut b = RpcRouter::builder();
        register_bridge_imap_handlers(&mut b);
        b.build()
    });
    let mut state = AppState::for_test(db);
    state.rpc_router = rpc_router;
    Arc::new(state)
}

/// Invoke an RPC handler via the router; returns the encoded reply
/// bytes. Panics if the handler returns `Err` — tests assert success
/// paths and the inability to set up state-changing fixtures is
/// itself a failure.
async fn call_handler<T: serde::Serialize>(
    state: &Arc<AppState>,
    kind: &'static str,
    caller_actor: [u8; 32],
    req: &T,
) -> Bytes {
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    let payload = Bytes::from(encode_canonical(req).expect("encode req").to_vec());
    (meta.handler)(state.clone(), caller_actor, payload)
        .await
        .unwrap_or_else(|err| panic!("{kind} handler returned Err: {err:?}"))
}

/// Drain the WS-bound rx channel for exactly `expect` push events and
/// return the typed `BridgeMailboxStatePush` payloads. Times out
/// after 1 s; panics with diagnostic context on shortfall.
async fn drain_pushes(
    rx: &mut mpsc::Receiver<bytes::Bytes>,
    expect: usize,
) -> Vec<BridgeMailboxStatePush> {
    let mut out = Vec::with_capacity(expect);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while out.len() < expect {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let bytes = tokio::time::timeout(remaining, rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timeout waiting for push event {}/{}",
                    out.len() + 1,
                    expect,
                )
            })
            .expect("channel open");
        let frame = decode_frame(&bytes).expect("decode frame");
        let push = match frame {
            Frame::Push(p) => p,
            other => panic!("expected Push frame, got {other:?}"),
        };
        let event = PushEvent::from_push(&push.kind, push.payload);
        match event {
            PushEvent::BridgeMailboxState(p) => out.push(p),
            other => panic!(
                "expected BridgeMailboxState push, got {} (frame seq={})",
                other.kind(),
                push.seq,
            ),
        }
    }
    out
}

/// End-to-end: subscribe via `fauna.bridges.subscribe_mailbox_state`,
/// then drive `fauna.bridges.append` for the same `(actor, mailbox)`
/// pair, then assert the IDLE/NOTIFY push lands on the MDA's WS
/// channel with the expected `MailboxStateEvent::Append` payload.
#[tokio::test]
async fn append_emits_mailbox_state_push_to_subscriber() {
    let state = build_state();

    let mda = [0x11u8; 32];
    let served = [0x33u8; 32];
    approve_bridge(&state.db, &mda, BridgeRole::Mda, &[0x99u8; 32]).await;

    // Subscribe the MDA's WS channel BEFORE the subscribe RPC so the
    // returned subscription routes onto an rx the test can read from.
    let (_conn, mut rx) = state.ws.subscribe(mda);

    // Subscribe to (served, INBOX) — captures the subscription_id we
    // expect to see echoed back in the push payload.
    let sub_req = SubscribeMailboxStateRequest {
        actor_id: served.to_vec(),
        mailbox: "INBOX".into(),
    };
    let bytes = call_handler(
        &state,
        "fauna.bridges.subscribe_mailbox_state",
        mda,
        &sub_req,
    )
    .await;
    let sub_reply: SubscribeMailboxStateReply = fauna_cbor::decode_strict(&bytes).unwrap();
    let SubscribeMailboxStateReply::Subscribed { subscription_id } = sub_reply;
    assert!(subscription_id >= 1, "subscription_id must be allocated");

    // APPEND a message to (served, INBOX). The handler emits one push
    // matching the subscription.
    let body = sealed(b"From: a@x\r\nTo: b@y\r\nSubject: hi\r\n\r\nhello\r\n");
    let body_len = body.len() as u32;
    let append_req = AppendMessageRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: served.to_vec(),
        mailbox: "INBOX".into(),
        flags: vec!["\\Seen".into()],
        encrypted_body: body,
        encrypted_index_hint: sealed(b"hint"),
        timestamp: 1_715_000_000,
        ciphertext_size: body_len,
        sender_domain: "x.example".into(),
        ..Default::default()
    };
    let _ = call_handler(&state, "fauna.bridges.append", mda, &append_req).await;

    let pushes = drain_pushes(&mut rx, 1).await;
    assert_eq!(pushes.len(), 1);
    let p = &pushes[0];
    assert_eq!(p.subscription_id, subscription_id);
    assert_eq!(p.actor_id, served.to_vec());
    assert_eq!(p.mailbox, "INBOX");
    match &p.event {
        MailboxStateEvent::Append { uid, flags, modseq } => {
            assert_eq!(*uid, 1, "first APPEND yields UID 1");
            assert!(*modseq > 0, "modseq must be set");
            assert!(
                flags.iter().any(|f| f == "\\Seen"),
                "appended flags echo back: {flags:?}"
            );
        }
        other => panic!("expected Append, got {other:?}"),
    }
}

/// STORE +FLAGS emits one `Flags` push per UID modified.
#[tokio::test]
async fn store_flags_emits_flags_push() {
    let state = build_state();

    let mda = [0x12u8; 32];
    let served = [0x34u8; 32];
    approve_bridge(&state.db, &mda, BridgeRole::Mda, &[0x99u8; 32]).await;

    // Pre-seed one message via APPEND so STORE has a UID to target.
    let body = sealed(b"body");
    let body_len = body.len() as u32;
    let _ = call_handler(
        &state,
        "fauna.bridges.append",
        mda,
        &AppendMessageRequest {
            dedup_key: "env:v1:fixture".into(),
            envelope_key: "env:v1:fixture".into(),
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
            flags: vec![],
            encrypted_body: body,
            encrypted_index_hint: sealed(b"h"),
            timestamp: 1_715_000_001,
            ciphertext_size: body_len,
            sender_domain: "x.example".into(),
            ..Default::default()
        },
    )
    .await;

    // Subscribe AFTER APPEND so the APPEND push isn't in our rx queue
    // (we only care about the STORE push here).
    let (_conn, mut rx) = state.ws.subscribe(mda);
    let _ = call_handler(
        &state,
        "fauna.bridges.subscribe_mailbox_state",
        mda,
        &SubscribeMailboxStateRequest {
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
        },
    )
    .await;

    // STORE +FLAGS \Seen on UID 1.
    let _ = call_handler(
        &state,
        "fauna.bridges.store_flags",
        mda,
        &StoreFlagsRequest {
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![1],
            op: StoreFlagsOp::Add,
            flags: vec!["\\Seen".into()],
            unchanged_since: None,
        },
    )
    .await;

    let pushes = drain_pushes(&mut rx, 1).await;
    assert_eq!(pushes.len(), 1);
    match &pushes[0].event {
        MailboxStateEvent::Flags { uid, flags, .. } => {
            assert_eq!(*uid, 1);
            assert!(flags.iter().any(|f| f == "\\Seen"));
        }
        other => panic!("expected Flags, got {other:?}"),
    }
}

/// EXPUNGE emits one `Expunge` push per UID expunged.
#[tokio::test]
async fn expunge_emits_per_uid_expunge_push() {
    let state = build_state();

    let mda = [0x13u8; 32];
    let served = [0x35u8; 32];
    approve_bridge(&state.db, &mda, BridgeRole::Mda, &[0x99u8; 32]).await;

    // Pre-seed two messages via APPEND with \Deleted flag so EXPUNGE
    // has UIDs to target.
    let body1 = sealed(b"body-one");
    let body1_len = body1.len() as u32;
    let _ = call_handler(
        &state,
        "fauna.bridges.append",
        mda,
        &AppendMessageRequest {
            dedup_key: "env:v1:fixture".into(),
            envelope_key: "env:v1:fixture".into(),
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
            flags: vec!["\\Deleted".into()],
            encrypted_body: body1,
            encrypted_index_hint: sealed(b"h1"),
            timestamp: 1_715_000_010,
            ciphertext_size: body1_len,
            sender_domain: "x.example".into(),
            ..Default::default()
        },
    )
    .await;
    let body2 = sealed(b"body-two");
    let body2_len = body2.len() as u32;
    let _ = call_handler(
        &state,
        "fauna.bridges.append",
        mda,
        &AppendMessageRequest {
            dedup_key: "env:v1:fixture".into(),
            envelope_key: "env:v1:fixture".into(),
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
            flags: vec!["\\Deleted".into()],
            encrypted_body: body2,
            encrypted_index_hint: sealed(b"h2"),
            timestamp: 1_715_000_011,
            ciphertext_size: body2_len,
            sender_domain: "x.example".into(),
            ..Default::default()
        },
    )
    .await;

    let (_conn, mut rx) = state.ws.subscribe(mda);
    let _ = call_handler(
        &state,
        "fauna.bridges.subscribe_mailbox_state",
        mda,
        &SubscribeMailboxStateRequest {
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
        },
    )
    .await;

    // EXPUNGE UIDs 1 and 2.
    let _ = call_handler(
        &state,
        "fauna.bridges.expunge",
        mda,
        &ExpungeRequest {
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![1, 2],
        },
    )
    .await;

    // Two pushes, one per expunged UID.
    let pushes = drain_pushes(&mut rx, 2).await;
    let mut uids: Vec<u32> = pushes
        .iter()
        .map(|p| match &p.event {
            MailboxStateEvent::Expunge { uid, .. } => *uid,
            other => panic!("expected Expunge, got {other:?}"),
        })
        .collect();
    uids.sort();
    assert_eq!(uids, vec![1, 2]);
}

/// MOVE emits one `Move` push per UID pair to subscribers on BOTH the
/// source AND the destination mailbox.
#[tokio::test]
async fn move_emits_move_pushes_to_both_endpoints() {
    let state = build_state();

    let mda = [0x14u8; 32];
    let served = [0x36u8; 32];
    approve_bridge(&state.db, &mda, BridgeRole::Mda, &[0x99u8; 32]).await;

    // Pre-seed two messages in INBOX.
    let body = sealed(b"body");
    let body_len = body.len() as u32;
    let _ = call_handler(
        &state,
        "fauna.bridges.append",
        mda,
        &AppendMessageRequest {
            dedup_key: "env:v1:fixture".into(),
            envelope_key: "env:v1:fixture".into(),
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
            flags: vec![],
            encrypted_body: body,
            encrypted_index_hint: sealed(b"h"),
            timestamp: 1_715_000_100,
            ciphertext_size: body_len,
            sender_domain: "x.example".into(),
            ..Default::default()
        },
    )
    .await;

    let (_conn, mut rx) = state.ws.subscribe(mda);
    // Subscribe on INBOX (source) AND Archive (destination).
    let _ = call_handler(
        &state,
        "fauna.bridges.subscribe_mailbox_state",
        mda,
        &SubscribeMailboxStateRequest {
            actor_id: served.to_vec(),
            mailbox: "INBOX".into(),
        },
    )
    .await;
    let _ = call_handler(
        &state,
        "fauna.bridges.subscribe_mailbox_state",
        mda,
        &SubscribeMailboxStateRequest {
            actor_id: served.to_vec(),
            mailbox: "Archive".into(),
        },
    )
    .await;

    // MOVE UID 1 → Archive.
    let _ = call_handler(
        &state,
        "fauna.bridges.move",
        mda,
        &MoveMessagesRequest {
            actor_id: served.to_vec(),
            source_mailbox: "INBOX".into(),
            uids: vec![1],
            dest_mailbox: "Archive".into(),
        },
    )
    .await;

    // Two pushes: one to source subscriber, one to dest, each naming its
    // own side. src_uid and dst_uid are both 1 here — the collision that
    // makes the side unrecoverable from the UIDs, so it must ride the wire.
    let pushes = drain_pushes(&mut rx, 2).await;
    assert_eq!(pushes.len(), 2);
    let mut mailboxes: Vec<String> = pushes.iter().map(|p| p.mailbox.clone()).collect();
    mailboxes.sort();
    assert_eq!(mailboxes, vec!["Archive".to_string(), "INBOX".to_string()]);
    for p in &pushes {
        match &p.event {
            MailboxStateEvent::Move {
                src_uid,
                dst_uid,
                side,
                ..
            } => {
                assert_eq!(*src_uid, 1);
                assert_eq!(*dst_uid, 1, "fresh dest mailbox allocates UID 1");
                let want = if p.mailbox == "INBOX" {
                    MoveSide::Source
                } else {
                    MoveSide::Destination
                };
                assert_eq!(*side, want, "push on {} names the wrong side", p.mailbox);
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }
}
