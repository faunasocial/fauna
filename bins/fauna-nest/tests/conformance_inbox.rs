//! Integration round-trip for the **fauna-native inbox** delivery-queue
//! kinds — `fauna.inbox.fetch` + `fauna.inbox.ack`. The WS-RPC successor
//! to the HTTP `GET /api/v1/inbox/{actor_id}` drain.
//!
//! These tests pin the load-bearing behaviour the migration adds over the
//! HTTP twin: **fetch is a pure peek** (it does NOT mark items delivered),
//! and an explicit **ack** is what consumes them. That split fixes the
//! latent data-loss bug in `routes::get_inbox`, which marked items
//! delivered the instant they were read — dropping them if the client
//! crashed before applying. They also pin caller-scoping (a caller drains
//! only its own queue, and can't ack another actor's items), the
//! limit/`more` paging, and the replay/allowlist metadata.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/inbox.rs`.
//! Slice: tracked internally.
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real in-memory `CacheDb` —
//! no mocks). Matches `conformance_contacts.rs`.

mod common;
use common::dispatch;
use common::encode;

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    db::CacheDb,
    inbox_handlers,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    RpcError, decode_strict as decode,
    inbox::{
        InboxAckReply, InboxAckRequest, InboxFetchReply, InboxFetchRequest, InboxSendReply,
        InboxSendRequest,
    },
};

async fn router_with_db() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    inbox_handlers::register_inbox_handlers(&mut b);
    (b.build(), state)
}

async fn fetch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    limit: u32,
) -> InboxFetchReply {
    fetch_after(router, state, actor, limit, None).await
}

async fn fetch_after(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    limit: u32,
    after_id: Option<i64>,
) -> InboxFetchReply {
    decode(
        &dispatch(
            router,
            state,
            actor,
            "fauna.inbox.fetch",
            encode(&InboxFetchRequest {
                extra: Default::default(),
                limit,
                after_id,
            }),
        )
        .await
        .expect("fetch ok"),
    )
    .unwrap()
}

async fn ack(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    ids: Vec<i64>,
) -> InboxAckReply {
    decode(
        &dispatch(
            router,
            state,
            actor,
            "fauna.inbox.ack",
            encode(&InboxAckRequest {
                extra: Default::default(),
                ids,
            }),
        )
        .await
        .expect("ack ok"),
    )
    .unwrap()
}

/// Build a valid signed inbox payload — the canonical `(EmbedAsBytes-cr,
/// EmbedAsBytes-post)` tuple the inbox handler decodes. Mirror of
/// `conformance_federation_channel::build_inbox_payload` (integration-test
/// binaries are isolated crates and can't share it directly).
///
/// `schema` is chosen by the *sender*, who signs it over their own post. It has
/// no bearing on routing — see `group_v1_relabel_does_not_bypass_contacts_only`.
fn build_signed_payload(sender: &ActorKeypair, recipient: &ActorId, schema: &str) -> Vec<u8> {
    use fauna_core::data::{ContactRequest, Post, PostBody, StructuredField, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, compute_post_id, sign_envelope};

    let author = sender.actor_id();
    let post = Post {
        author,
        created_at: Timestamp::now(),
        body: PostBody::Structured {
            schema: schema.into(),
            fields: vec![StructuredField {
                key: "to".into(),
                value: hex::encode(recipient.0),
            }],
            content: Some("hello".into()),
            facets: vec![],
            items: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (post_bytes, post_env) = sign_envelope(sender, &post).unwrap();
    let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);
    let post_id = compute_post_id(&post).unwrap();

    let cr = ContactRequest {
        sender: author,
        post_id,
        sender_node: b"http://localhost:3000".to_vec(),
        summary: "hi".into(),
        created_at: Timestamp::now(),
    };
    let (cr_bytes, cr_env) = sign_envelope(sender, &cr).unwrap();
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);

    canonical_encode(&(&cr_wire, &post_wire)).unwrap()
}

/// Dispatch `fauna.inbox.send` as `caller`, returning the raw handler result so
/// tests can assert on either the reply or the error.
async fn send(
    router: &RpcRouter,
    state: Arc<AppState>,
    caller: [u8; 32],
    req: &InboxSendRequest,
) -> Result<Bytes, RpcError> {
    dispatch(router, state, caller, "fauna.inbox.send", encode(req)).await
}

// ── fauna.inbox.fetch — peek of undelivered items ──────────────────

#[tokio::test]
async fn fetch_returns_undelivered_items_oldest_first() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    state.db.push_inbox(&actor, b"first", None).await.unwrap();
    state.db.push_inbox(&actor, b"second", None).await.unwrap();

    let reply = fetch(&router, state.clone(), actor, 0).await;
    assert_eq!(reply.items.len(), 2);
    assert!(!reply.more);
    assert_eq!(reply.items[0].payload, b"first");
    assert_eq!(reply.items[1].payload, b"second");
    // Link-id order is ascending (oldest first).
    assert!(reply.items[0].id < reply.items[1].id);
}

#[tokio::test]
async fn fetch_does_not_mark_delivered_regression() {
    // The HTTP twin (`get_inbox`) marked items delivered on read — a
    // re-fetch returned nothing and a crashed client lost them. The kind
    // must leave them undelivered until an explicit ack.
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    state.db.push_inbox(&actor, b"durable", None).await.unwrap();

    let first = fetch(&router, state.clone(), actor, 0).await;
    assert_eq!(first.items.len(), 1);
    // Same item is still undelivered on a second fetch (no consume on read).
    let second = fetch(&router, state.clone(), actor, 0).await;
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].id, first.items[0].id);
    assert_eq!(state.db.poll_inbox(&actor).await.unwrap().len(), 1);
}

#[tokio::test]
async fn fetch_scoped_to_connection_actor() {
    let (router, state) = router_with_db().await;
    let actor_a = [11u8; 32];
    let actor_b = [99u8; 32];
    state.db.push_inbox(&actor_a, b"for a", None).await.unwrap();
    state.db.push_inbox(&actor_b, b"for b", None).await.unwrap();

    let reply = fetch(&router, state, actor_a, 0).await;
    assert_eq!(reply.items.len(), 1);
    assert_eq!(reply.items[0].payload, b"for a");
}

#[tokio::test]
async fn fetch_limit_caps_page_and_sets_more() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    for n in 0..3u8 {
        state.db.push_inbox(&actor, &[n], None).await.unwrap();
    }
    let page = fetch(&router, state.clone(), actor, 2).await;
    assert_eq!(page.items.len(), 2);
    assert!(page.more, "more remains past the 2-item page");
}

#[tokio::test]
async fn fetch_after_skips_past_the_cursor_without_delivering_it() {
    // The skip cursor: a client that cannot apply the head (a kind with no
    // surface yet, or an `InboxKind::Unknown` from a newer nest) steps past it
    // so it stops shadowing the tail. Skipping must NOT deliver — the stepped
    // -over rows stay undelivered and a cursor-less fetch still returns them.
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    for n in 0..4u8 {
        state.db.push_inbox(&actor, &[n], None).await.unwrap();
    }
    let all = fetch(&router, state.clone(), actor, 0).await;
    assert_eq!(all.items.len(), 4);
    let second_id = all.items[1].id;

    let tail = fetch_after(&router, state.clone(), actor, 0, Some(second_id)).await;
    assert_eq!(
        tail.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![all.items[2].id, all.items[3].id],
        "only rows past the cursor"
    );

    // Nothing was consumed by being skipped over.
    let again = fetch(&router, state.clone(), actor, 0).await;
    assert_eq!(again.items.len(), 4, "skipping is not delivery");
    assert_eq!(state.db.poll_inbox(&actor).await.unwrap().len(), 4);
}

#[tokio::test]
async fn fetch_after_reaches_a_row_behind_a_full_page_of_unappliable_heads() {
    // The end-to-end shape of the defect: >`limit` un-ackable rows at the head
    // (every new-IP sign-in mints a `SecurityNotice` no app renders) made the
    // durable missed-push backstop unreachable. With the cursor, a client
    // walking page by page still reaches the row behind them.
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    for n in 0..5u8 {
        state.db.push_inbox(&actor, &[n], None).await.unwrap();
    }
    state.db.push_inbox(&actor, b"welcome", None).await.unwrap();

    // Page size 5: the first page is entirely un-appliable heads.
    let head = fetch(&router, state.clone(), actor, 5).await;
    assert_eq!(head.items.len(), 5);
    assert!(head.more, "the target row remains past this page");

    let cursor = head.items.last().unwrap().id;
    let tail = fetch_after(&router, state.clone(), actor, 5, Some(cursor)).await;
    assert_eq!(tail.items.len(), 1);
    assert_eq!(tail.items[0].payload, b"welcome");
    assert!(!tail.more);
}

// ── fauna.inbox.ack — explicit consume ─────────────────────────────

#[tokio::test]
async fn ack_marks_items_delivered_and_drains_them() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    state.db.push_inbox(&actor, b"a", None).await.unwrap();
    state.db.push_inbox(&actor, b"b", None).await.unwrap();

    let reply = fetch(&router, state.clone(), actor, 0).await;
    let ids: Vec<i64> = reply.items.iter().map(|i| i.id).collect();
    let ack_reply = ack(&router, state.clone(), actor, ids).await;
    assert_eq!(ack_reply.acked, 2);

    // Drained: re-fetch is empty, and poll_inbox (undelivered) is empty.
    let after = fetch(&router, state.clone(), actor, 0).await;
    assert!(after.items.is_empty());
    assert!(state.db.poll_inbox(&actor).await.unwrap().is_empty());
}

#[tokio::test]
async fn ack_is_idempotent_on_replay() {
    let (router, state) = router_with_db().await;
    let actor = [11u8; 32];
    let id = state.db.push_inbox(&actor, b"a", None).await.unwrap();

    assert_eq!(ack(&router, state.clone(), actor, vec![id]).await.acked, 1);
    // Replaying the same ack flips nothing more (already delivered).
    assert_eq!(ack(&router, state.clone(), actor, vec![id]).await.acked, 0);
}

#[tokio::test]
async fn ack_cannot_consume_another_actors_items() {
    let (router, state) = router_with_db().await;
    let actor_a = [11u8; 32];
    let actor_b = [99u8; 32];
    let b_id = state.db.push_inbox(&actor_b, b"for b", None).await.unwrap();

    // A acks B's link id — must be a no-op (caller-scoped UPDATE).
    let reply = ack(&router, state.clone(), actor_a, vec![b_id]).await;
    assert_eq!(reply.acked, 0);
    // B's item is still undelivered.
    assert_eq!(state.db.poll_inbox(&actor_b).await.unwrap().len(), 1);
}

// ── fauna.inbox.send — client→home-nest bearer leg (same-nest + binding) ──

#[tokio::test]
async fn send_same_nest_delivers_locally() {
    // `recipient_nest_url = None` ⇒ the recipient is on the caller's home nest;
    // the home nest delivers locally (no federation leg). The recipient's inbox
    // is `open`, so a stranger's post lands as a delivered row.
    let (router, state) = router_with_db().await;
    let sender = ActorKeypair::from_secret([0x11u8; 32]);
    let caller = sender.actor_id().0;
    let recipient = ActorId([0x77u8; 32]);
    // A recipient must hold an account here: existence is judged at the
    // (terminal) recipient id before routing, so a never-registered id is
    // refused rather than knocked on.
    state
        .db
        .create_user_with_handle(&recipient.0, "free", "recipient", None)
        .await
        .unwrap();
    state.db.set_inbox_mode(&recipient.0, "open").await.unwrap();
    let payload = build_signed_payload(&sender, &recipient, "note/v1");

    let req = InboxSendRequest {
        extra: Default::default(),
        recipient_actor_id: hex::encode(recipient.0),
        recipient_nest_url: None,
        payload_bytes: payload,
    };
    let reply: InboxSendReply = decode(
        &send(&router, state.clone(), caller, &req)
            .await
            .expect("send ok"),
    )
    .unwrap();
    assert!(
        reply.inbox_id.is_some(),
        "an open-mode recipient is delivered locally → an inbox row id"
    );

    // The row landed in the recipient's local inbox on this nest.
    let inbox = state.db.list_inbox_all(&recipient.0).await.unwrap();
    assert_eq!(inbox.len(), 1, "exactly one payload delivered locally");
}

/// The **adult** half of the `group/v1` fix.
///
/// `deliver_inbox_payload_core` used to return early for any post whose
/// sender-chosen `schema` was `"group/v1"`, ahead of the `InboxMode` routing. So
/// relabelling a post defeated `contacts_only` and `allow_knock` for *every*
/// recipient on the nest, not just supervised ones. Fixing this only for wards
/// would have left the same hole open for everyone else, which is why the
/// exemption was removed outright rather than narrowed.
#[tokio::test]
async fn group_v1_relabel_does_not_bypass_contacts_only() {
    let (router, state) = router_with_db().await;
    let sender = ActorKeypair::from_secret([0x11u8; 32]);
    let caller = sender.actor_id().0;
    let recipient = ActorId([0x77u8; 32]);
    state
        .db
        .create_user_with_handle(&recipient.0, "free", "recipient", None)
        .await
        .unwrap();
    state
        .db
        .set_inbox_mode(&recipient.0, "contacts_only")
        .await
        .unwrap();

    let req = InboxSendRequest {
        extra: Default::default(),
        recipient_actor_id: hex::encode(recipient.0),
        recipient_nest_url: None,
        payload_bytes: build_signed_payload(&sender, &recipient, "group/v1"),
    };
    let err = send(&router, state.clone(), caller, &req)
        .await
        .expect_err("a self-declared schema must not defeat contacts_only");
    assert_eq!(err.code, "fauna.inbox.forbidden");
    assert!(
        state
            .db
            .list_inbox_all(&recipient.0)
            .await
            .unwrap()
            .is_empty(),
        "nothing reached a contacts_only stranger's inbox"
    );
}

#[tokio::test]
async fn send_rejects_when_caller_is_not_sender() {
    // Sender-binding: the authed leg enforces `cr.sender == caller`. A different
    // authed actor relaying someone else's signed tuple is rejected — the
    // security property the unauthenticated HTTP twin could not enforce. The
    // signed payload itself is valid (sender signs it); only the *caller* differs.
    let (router, state) = router_with_db().await;
    let sender = ActorKeypair::from_secret([0x11u8; 32]);
    let imposter = [0x99u8; 32];
    assert_ne!(sender.actor_id().0, imposter);
    let recipient = ActorId([0x77u8; 32]);
    let req = InboxSendRequest {
        extra: Default::default(),
        recipient_actor_id: hex::encode(recipient.0),
        recipient_nest_url: None,
        payload_bytes: build_signed_payload(&sender, &recipient, "group/v1"),
    };

    let err = send(&router, state.clone(), imposter, &req)
        .await
        .expect_err("a non-sender caller must be rejected");
    assert_eq!(err.code, "fauna.inbox.permission_denied");
    // Nothing was delivered.
    assert!(
        state
            .db
            .list_inbox_all(&recipient.0)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn send_rejects_malformed_payload() {
    let (router, state) = router_with_db().await;
    let req = InboxSendRequest {
        extra: Default::default(),
        recipient_actor_id: hex::encode([0x77u8; 32]),
        recipient_nest_url: None,
        payload_bytes: vec![0xde, 0xad], // not a valid (cr, post) tuple
    };
    let err = send(&router, state, [0x11u8; 32], &req)
        .await
        .expect_err("a malformed payload must be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── replay metadata + allowlist ────────────────────────────────────

const INBOX_KINDS: [&str; 2] = ["fauna.inbox.fetch", "fauna.inbox.ack"];

#[tokio::test]
async fn replay_metadata_matches_spec() {
    let (router, _state) = router_with_db().await;
    for kind in INBOX_KINDS {
        let meta = router.kind_meta(kind).expect("kind registered");
        assert!(!meta.forbid_replay, "{kind} is replay-safe (idempotent)");
        assert_eq!(meta.default_deadline, std::time::Duration::from_secs(5));
    }
    // `send` is replay-FORBIDDEN at the longer 30 s federation-dial deadline:
    // a replayed send re-runs `check_submission_quota` (a consuming debit) and
    // can double-deliver a knock (the forbid_replay audit; the
    // handler's own module doc states it). This test lagged that flip.
    let send = router
        .kind_meta("fauna.inbox.send")
        .expect("send registered");
    assert!(
        send.forbid_replay,
        "send is not idempotent — replay-forbidden"
    );
    assert_eq!(send.default_deadline, std::time::Duration::from_secs(30));
}

#[tokio::test]
async fn inbox_kinds_user_and_admin_only_at_allowlist_layer() {
    // `send` shares the fetch/ack gate: User (Admin inherits), bridges denied.
    for kind in ["fauna.inbox.fetch", "fauna.inbox.ack", "fauna.inbox.send"] {
        assert!(is_permitted(CallerClass::User, kind), "{kind} for User");
        assert!(is_permitted(CallerClass::Admin, kind), "{kind} for Admin");
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(!is_permitted(class, kind), "{kind} denied for {class:?}");
        }
    }
}
