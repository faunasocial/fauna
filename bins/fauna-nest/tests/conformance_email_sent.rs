//! Conformance + round-trip for `fauna.email.sent.fetch` — the
//! **Sent-mailbox sibling** of `fauna.email.inbox.fetch`. A message the
//! user sent from an external MUA (macOS Mail via SMTP submission) leaves
//! a server-side `Sent` copy sealed to the sender's own MSEK-derived
//! recipient key; this read feed lets the native app surface that
//! outbound copy in its unified conversations view.
//!
//! **tier_3** — same depth as the inbox twin: real seal
//! (`fauna_mls::wrapped_blob::seal_to_recipient` to the MSEK-derived
//! recipient pubkey), real new-path own-submission delivery
//! (`fauna.bridges.submit_inbound_mail`, BridgeMta — the door the nest files
//! into the `Sent` mailbox), and the real
//! `sent.fetch` handler reading back through `query_bridge_imap_messages`
//! + `read_envelopes_bulk`. No stubs, no plaintext path.
//!
//! The handler shares its body with `inbox.fetch` (the only differences
//! are the literal mailbox name `"Sent"` and the permission string);
//! `InboxFetchRequest`/`InboxFetchReply`/`InboxMessage` are reused as the
//! generic mail-page wire types serving both mailboxes.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/email.rs`.

mod common;
use common::approve_bridge;
use common::dispatch;
use common::seal_and_ingest_sent;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::wrapped_blob::{
    MailRecordEnvelope as BridgeMailEnvelope, derive_recipient_hpke_keypair, seal_to_recipient,
    unseal_mail_record,
};
use fauna_nest::bridge_method_allowlist::{CallerClass, is_permitted};
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{IngestInboundMailRequest, PublicMailMetadata};
use fauna_protocol::email::{InboxFetchReply, InboxFetchRequest};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// Outer segment-record envelope (`fauna_mail::segments::MailRecordEnvelope`)
/// — the per-record blob `read_envelopes_bulk` returns. Its `.encrypted_body`
/// is the inner bridge seal (`BridgeMailEnvelope`). Same-named distinct types;
/// the bridge one is imported under an alias above, the segment one only here.
use fauna_mail::segments::MailRecordEnvelope as SegmentMailEnvelope;

/// Router with both handler sets registered: `ingest_inbound_mail`
/// (BridgeMta, the new-path write) lives in `bridge_routing_handlers`;
/// `sent.fetch` (User, the read) lives in `email_handlers`.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    register_email_handlers(&mut b);
    (b.build(), state)
}

fn fetch_payload(after_uid: u32, limit: u32) -> Bytes {
    let req = InboxFetchRequest {
        extra: Default::default(),
        after_uid,
        limit,
    };
    Bytes::from(encode_canonical(&req).expect("encode fetch req").to_vec())
}

async fn sent_fetch(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    after_uid: u32,
    limit: u32,
) -> InboxFetchReply {
    let reply_bytes = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.sent.fetch",
        fetch_payload(after_uid, limit),
    )
    .await
    .expect("sent.fetch ok");
    decode(&reply_bytes).expect("decode sent.fetch reply")
}

// ── (a) seal → ingest into Sent → fetch as owner → unseal == original ──

#[tokio::test]
async fn sent_fetch_returns_sealed_envelope_that_opens_to_the_sent_body() {
    let (router, state) = router_and_state().await;

    // The actor is the sender reading its own Sent copy — sealed to its own
    // MSEK-derived recipient key, exactly like an inbound message.
    let sender: [u8; 32] = [0x42; 32];
    let msek: [u8; 32] = [0x5e; 32];
    let (sender_secret, sender_pubkey) = derive_recipient_hpke_keypair(&msek);
    common::seed_recipient_seal_key(&state.db, &sender, &msek).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    let body: &[u8] = b"From: alice@self.test\r\n\
        To: External Person <person@external.test>\r\n\
        Subject: sent.fetch round-trip\r\n\
        \r\n\
        A copy of what I sent from my external MUA.\r\n";
    // The `Date:` header on the submitted copy. Deliberately not the value
    // the feed reports below — see the `internal_date` assertion.
    let claimed_date = 1_715_100_000;
    let before_ingest = fauna_core::data::Timestamp::now_secs();
    let sealed_body = seal_and_ingest_sent(
        &router,
        &state,
        mta_actor,
        sender,
        &sender_pubkey,
        body,
        claimed_date,
    )
    .await;
    let after_ingest = fauna_core::data::Timestamp::now_secs();

    // The client reads its own Sent. No actor_id param — caller-scoped.
    let reply = sent_fetch(&router, &state, sender, 0, 0).await;
    assert_eq!(reply.messages.len(), 1, "one Sent message");
    assert!(!reply.more, "single page");
    let msg = &reply.messages[0];
    assert!(msg.uid >= 1, "Sent UID assigned");
    assert_eq!(msg.message_id.len(), 32, "32-byte message_id");
    // Epoch **seconds** (not millis), and the NEST's own instant — for a Sent
    // copy that is when the nest stored it, which is what INTERNALDATE means
    // on this path too (`imap-server.md` § SEARCH). Bracketed rather than
    // fixed, so it holds under any machine load.
    assert!(
        (before_ingest..=after_ingest).contains(&msg.internal_date),
        "internal_date {} must be the nest's store instant in seconds, inside \
         [{before_ingest}, {after_ingest}] — the copy claimed {claimed_date}",
        msg.internal_date
    );

    // `sealed_envelope` is the OUTER segment envelope; its `.encrypted_body`
    // is the inner bridge seal the client opens. Unwrap one layer, then unseal.
    let segment_env =
        SegmentMailEnvelope::decode(&msg.sealed_envelope).expect("decode segment envelope");
    assert_eq!(
        segment_env.encrypted_body, sealed_body,
        "segment envelope wraps the verbatim bridge seal"
    );
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode bridge envelope");
    let opened =
        unseal_mail_record(&bridge_env, &sender_secret).expect("sender opens its own Sent copy");
    assert_eq!(
        opened.as_slice(),
        body,
        "sent copy decrypts byte-for-byte back to the sent body"
    );
}

// ── (b) caller-scoping: a different actor sees an empty Sent mailbox ──

#[tokio::test]
async fn sent_fetch_is_caller_scoped_other_actor_sees_nothing() {
    let (router, state) = router_and_state().await;

    let alice: [u8; 32] = [0x42; 32];
    let (_alice_secret, alice_pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&state.db, &alice, &[0x5e; 32]).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    seal_and_ingest_sent(
        &router,
        &state,
        mta_actor,
        alice,
        &alice_pubkey,
        b"From: alice@self.test\r\n\r\nalice's sent copy\r\n",
        1_715_100_100,
    )
    .await;

    // Bob is a different User-class actor — his Sent mailbox is empty.
    let bob: [u8; 32] = [0x77; 32];
    let reply = sent_fetch(&router, &state, bob, 0, 0).await;
    assert!(
        reply.messages.is_empty(),
        "a caller only ever reads its own Sent mailbox"
    );
    assert!(!reply.more);

    // Alice still sees her own Sent message.
    let alice_reply = sent_fetch(&router, &state, alice, 0, 0).await;
    assert_eq!(alice_reply.messages.len(), 1, "alice reads her own Sent");
}

// ── (c) Sent and INBOX are distinct mailboxes for the same actor ──────

#[tokio::test]
async fn sent_fetch_does_not_return_inbox_messages() {
    let (router, state) = router_and_state().await;

    let actor: [u8; 32] = [0x42; 32];
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&state.db, &actor, &[0x5e; 32]).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    // One message into INBOX (default mailbox), one into Sent.
    let inbox_body = seal_to_recipient(b"To: me\r\n\r\ninbound\r\n", &pubkey)
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    let inbox_hint = seal_to_recipient(b"hint", &pubkey)
        .expect("seal")
        .to_canonical_bytes()
        .expect("encode");
    let inbox_req = IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: actor.to_vec(),
        encrypted_body: inbox_body.clone(),
        encrypted_index_hint: inbox_hint,
        // No mailbox override → default INBOX.
        public_metadata: PublicMailMetadata {
            timestamp: 1_715_100_200,
            ciphertext_size: inbox_body.len() as u32,
            sender_domain: "external.test".into(),
        },
        ..Default::default()
    };
    dispatch(
        &router,
        state.clone(),
        mta_actor,
        "fauna.bridges.ingest_inbound_mail",
        Bytes::from(encode_canonical(&inbox_req).expect("encode").to_vec()),
    )
    .await
    .expect("inbox ingest ok");

    seal_and_ingest_sent(
        &router,
        &state,
        mta_actor,
        actor,
        &pubkey,
        b"From: me\r\n\r\noutbound\r\n",
        1_715_100_300,
    )
    .await;

    // sent.fetch sees only the Sent message, not the INBOX one.
    let reply = sent_fetch(&router, &state, actor, 0, 0).await;
    assert_eq!(reply.messages.len(), 1, "Sent mailbox holds one message");
}

// ── (d) allowlist: User + Admin permitted, bridges denied ─────────────

#[test]
fn sent_fetch_user_and_admin_permitted_bridges_denied() {
    assert!(
        is_permitted(CallerClass::User, "fauna.email.sent.fetch"),
        "the sent feed is a User-class read"
    );
    assert!(
        is_permitted(CallerClass::Admin, "fauna.email.sent.fetch"),
        "the claimer-admin reads its own Sent on a single-user nest"
    );
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(
            !is_permitted(class, "fauna.email.sent.fetch"),
            "fauna.email.sent.fetch should be denied for {class:?}"
        );
    }
}
