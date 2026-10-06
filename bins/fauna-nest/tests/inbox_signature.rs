//! Tests: the nest rejects inbox payloads with invalid signatures.
//!
//! Migrated 2026-07-09 from the deleted HTTP twin `POST /api/v1/inbox/{actor}`
//! (WS-RPC-everywhere rip) onto the surviving carrier, `fauna.inbox.send`'s
//! local-delivery branch — the four legacy tests had been silently POSTing
//! into the SPA fallback (200) and failing. Same four properties: a valid
//! signed `(ContactRequest, Post)` tuple delivers; garbage bytes, a tampered
//! signature, and a forged sender (CR.sender ≠ post.author) are rejected by
//! `verify_inbox_payload` before any routing.

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::data::{ContactRequest, Post, PostBody, StructuredField, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, canonical_encode, compute_post_id, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::inbox_handlers::register_inbox_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::inbox::{InboxSendReply, InboxSendRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical as enc};

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    register_inbox_handlers(&mut b);
    b.build()
}

/// State + a registered sender (the send handler's caller-class gate needs a
/// real user) + a registered recipient with an `open` inbox.
async fn setup() -> (Arc<AppState>, RpcRouter, ActorKeypair, ActorKeypair) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db.clone()));
    let sender = ActorKeypair::generate();
    let recipient = ActorKeypair::generate();
    db.create_user_with_handle(&sender.actor_id().0, "free", "sender", None)
        .await
        .unwrap();
    db.create_user_with_handle(&recipient.actor_id().0, "free", "recipient", None)
        .await
        .unwrap();
    db.set_inbox_mode(&recipient.actor_id().0, "open")
        .await
        .unwrap();
    (state, router(), sender, recipient)
}

async fn send(
    state: Arc<AppState>,
    router: &RpcRouter,
    caller: [u8; 32],
    recipient: &[u8; 32],
    payload_bytes: Vec<u8>,
) -> Result<Bytes, RpcError> {
    let req = InboxSendRequest {
        recipient_actor_id: hex::encode(recipient),
        recipient_nest_url: None,
        payload_bytes,
        extra: Default::default(),
    };
    let meta = router
        .kind_meta("fauna.inbox.send")
        .expect("kind registered");
    (meta.handler)(state, caller, Bytes::from(enc(&req).unwrap().to_vec())).await
}

/// Build a valid signed email payload (ContactRequest + Post).
fn build_valid_payload(sender: &ActorKeypair, recipient: &ActorKeypair) -> Vec<u8> {
    let author = sender.actor_id();
    let to = recipient.actor_id();

    let fields = vec![
        StructuredField {
            key: "subject".into(),
            value: "Test".into(),
        },
        StructuredField {
            key: "to".into(),
            value: hex::encode(to.0),
        },
    ];

    let post = Post {
        author,
        created_at: Timestamp::now(),
        body: PostBody::Structured {
            schema: "email/v1".into(),
            fields,
            content: Some("Hello".into()),
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
        summary: "Test".into(),
        created_at: Timestamp::now(),
    };
    let (cr_bytes, cr_env) = sign_envelope(sender, &cr).unwrap();
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);

    canonical_encode(&(&cr_wire, &post_wire)).unwrap()
}

#[tokio::test]
async fn valid_signed_payload_accepted() {
    let (state, r, sender, recipient) = setup().await;
    let payload = build_valid_payload(&sender, &recipient);
    let out = send(
        state,
        &r,
        sender.actor_id().0,
        &recipient.actor_id().0,
        payload,
    )
    .await
    .expect("valid signed payload should be accepted");
    let reply: InboxSendReply = decode(&out).unwrap();
    assert!(reply.inbox_id.is_some(), "open inbox → delivered");
}

#[tokio::test]
async fn garbage_bytes_rejected() {
    let (state, r, sender, recipient) = setup().await;
    let err = send(
        state,
        &r,
        sender.actor_id().0,
        &recipient.actor_id().0,
        b"this is not a valid payload".to_vec(),
    )
    .await
    .expect_err("garbage bytes should be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn tampered_post_signature_rejected() {
    let (state, r, sender, recipient) = setup().await;
    let mut payload = build_valid_payload(&sender, &recipient);
    // Tamper with the last byte (inside the post body/signature area).
    if let Some(b) = payload.last_mut() {
        *b ^= 0xff;
    }
    let err = send(
        state,
        &r,
        sender.actor_id().0,
        &recipient.actor_id().0,
        payload,
    )
    .await
    .expect_err("tampered payload should be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

#[tokio::test]
async fn forged_sender_rejected() {
    // The CR claims the impersonator as sender while the post is signed by
    // the real sender. The impersonator is the authed caller (so the send
    // handler's sender-binding passes — the caller IS cr.sender); the
    // cross-check in `verify_inbox_payload` (CR.sender must equal
    // post.author) rejects the forgery.
    let (state, r, _sender, recipient) = setup().await;
    let real_sender = ActorKeypair::generate();
    let impersonator = ActorKeypair::generate();
    let db = state.db.clone();
    db.create_user_with_handle(&impersonator.actor_id().0, "free", "impersonator", None)
        .await
        .unwrap();

    let author = real_sender.actor_id();
    let post = Post {
        author,
        created_at: Timestamp::now(),
        body: PostBody::Structured {
            schema: "email/v1".into(),
            fields: vec![StructuredField {
                key: "to".into(),
                value: hex::encode(recipient.actor_id().0),
            }],
            content: Some("Forged message".into()),
            facets: vec![],
            items: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let (post_bytes, post_env) = sign_envelope(&real_sender, &post).unwrap();
    let post_wire = EmbedAsBytes::from_signed(post_bytes, post_env);
    let post_id = compute_post_id(&post).unwrap();

    let cr = ContactRequest {
        sender: impersonator.actor_id(),
        post_id,
        sender_node: b"http://localhost:3000".to_vec(),
        summary: "Forged".into(),
        created_at: Timestamp::now(),
    };
    let (cr_bytes, cr_env) = sign_envelope(&impersonator, &cr).unwrap();
    let cr_wire = EmbedAsBytes::from_signed(cr_bytes, cr_env);
    let payload = canonical_encode(&(&cr_wire, &post_wire)).unwrap();

    let err = send(
        state,
        &r,
        impersonator.actor_id().0,
        &recipient.actor_id().0,
        payload,
    )
    .await
    .expect_err("forged sender (CR.sender != post.author) should be rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
}
