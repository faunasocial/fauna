//! Conformance + round-trip for `fauna.email.inbox.fetch` — the
//! **User-class, caller-scoped** inbound mail read feed (Slice 1;
//! tracked internally). This is the inbound twin of
//! `fauna.email.send`: a client reads mail delivered to its own
//! `user@domain` INBOX as opaque sealed `MailRecordEnvelope` bytes it
//! opens client-side.
//!
//! **tier_3** — exercises the real seal (`fauna_mls::wrapped_blob::
//! seal_to_recipient` to the MSEK-derived recipient pubkey), the real
//! **new-path** ingest (`fauna.bridges.ingest_inbound_mail`, BridgeMta,
//! which seals into the `__mail/<actor>` segment store AND places a row
//! in `bridge_imap_messages` INBOX), and the real `inbox.fetch` handler
//! reading both back through `query_bridge_imap_messages` +
//! `read_envelopes_bulk`. No stubs, no plaintext path. Only this depth
//! catches a seal/store/read seam break — the user directive
//! `green-test-or-it-doesnt-work`.
//!
//! New-vs-deprecated (TODO § DO NOT CONFUSE): this drives ONLY the Go
//! mail-bridge path — `ingest_inbound_mail` → segment store +
//! `bridge_imap_messages`. It does NOT touch `deliver_local`/`push_inbox`/
//! `email_aliases` (nor the Plan-5 `IndexRegistry::ingest_mail`, deleted
//! 2026-07-13 — unsealed nest-side ingest, `content-index.md` § Two search
//! backends today).
//!
//! Authority for the wire types: `libs/fauna-protocol/src/email.rs`.
//! Authority for the slice: tracked internally (§ Slice 1).

mod common;
use common::approve_bridge;
use common::dispatch;
use common::seal_and_ingest;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::wrapped_blob::{
    MailRecordEnvelope as BridgeMailEnvelope, derive_recipient_hpke_keypair, unseal_mail_record,
};
use fauna_nest::bridge_method_allowlist::{CallerClass, is_permitted};
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::email::{InboxFetchReply, InboxFetchRequest};
use fauna_protocol::{decode_strict as decode, encode_canonical};

/// Segment-record envelope (`fauna_mail::segments::MailRecordEnvelope`) —
/// the outer per-record blob `read_envelopes_bulk` returns. Its
/// `.encrypted_body` is the inner bridge seal (`BridgeMailEnvelope`). The
/// segment envelope and the bridge envelope are distinct same-named
/// types; the test imports the bridge one (above) under an alias and the
/// segment one only here, where it unwraps the fetch reply.
use fauna_mail::segments::MailRecordEnvelope as SegmentMailEnvelope;

/// Router with both handler sets registered: `ingest_inbound_mail`
/// (BridgeMta, the new-path write) lives in `bridge_routing_handlers`;
/// `inbox.fetch` (User, the new client read) lives in `email_handlers`.
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

async fn inbox_fetch(
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
        "fauna.email.inbox.fetch",
        fetch_payload(after_uid, limit),
    )
    .await
    .expect("inbox.fetch ok");
    decode(&reply_bytes).expect("decode inbox.fetch reply")
}

// ── (a) seal → ingest → fetch as owner → unseal == original ───────

#[tokio::test]
async fn inbox_fetch_returns_sealed_envelope_that_opens_to_the_sent_body() {
    let (router, state) = router_and_state().await;

    // Recipient is a User-class actor (any actor that is not an approved
    // bridge and not an admin is User — `caller_class_for_actor`).
    let recipient: [u8; 32] = [0x42; 32];
    let msek: [u8; 32] = [0x5e; 32];
    let (recipient_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&msek);
    common::seed_recipient_seal_key(&state.db, &recipient, &msek).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    let body: &[u8] = b"From: External Sender <sender@external.test>\r\n\
        To: alice@local.test\r\n\
        Subject: inbox.fetch round-trip\r\n\
        \r\n\
        Hello from the client mail-receive feed test.\r\n";
    // The `Date:` header the sender claims. Deliberately not the value the
    // feed reports below — see the `internal_date` assertion.
    let claimed_date = 1_715_000_000;
    let before_ingest = fauna_core::data::Timestamp::now_secs();
    let sealed_body = seal_and_ingest(
        &router,
        &state,
        mta_actor,
        recipient,
        &recipient_pubkey,
        body,
        claimed_date,
    )
    .await;
    let after_ingest = fauna_core::data::Timestamp::now_secs();

    // The client reads its own INBOX. No actor_id param — caller-scoped.
    let reply = inbox_fetch(&router, &state, recipient, 0, 0).await;
    assert_eq!(reply.messages.len(), 1, "one INBOX message");
    assert!(!reply.more, "single page");
    let msg = &reply.messages[0];
    assert!(msg.uid >= 1, "INBOX UID assigned");
    assert_eq!(msg.message_id.len(), 32, "32-byte message_id");
    // Epoch **seconds** (not millis), and the NEST's own receipt instant —
    // the floor's server-assigned `received_at`, never the sender's claimed
    // `Date:` header (`imap-server.md` § SEARCH → *INTERNALDATE is the nest's
    // own receipt time*). Asserted as the window this test bracketed the
    // ingest with, so it stays true under any machine load.
    assert!(
        (before_ingest..=after_ingest).contains(&msg.internal_date),
        "internal_date {} must be the nest's receipt instant in seconds, \
         inside [{before_ingest}, {after_ingest}] — the sender claimed \
         {claimed_date}",
        msg.internal_date
    );

    // `sealed_envelope` is exactly what `read_envelopes_bulk` returns —
    // the OUTER segment envelope. Its `.encrypted_body` is the inner
    // bridge seal the client opens. Unwrap one layer, then unseal.
    let segment_env =
        SegmentMailEnvelope::decode(&msg.sealed_envelope).expect("decode segment envelope");
    assert_eq!(
        segment_env.encrypted_body, sealed_body,
        "segment envelope wraps the verbatim bridge seal"
    );
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode bridge envelope");
    let opened =
        unseal_mail_record(&bridge_env, &recipient_secret).expect("recipient opens its own mail");
    assert_eq!(
        opened.as_slice(),
        body,
        "received mail decrypts byte-for-byte back to the sent body"
    );

    // Negative control — a non-recipient secret must NOT open it.
    let (wrong_secret, _) = derive_recipient_hpke_keypair(&[0xAB; 32]);
    assert!(
        unseal_mail_record(&bridge_env, &wrong_secret).is_err(),
        "a non-recipient secret must fail to open the sealed envelope"
    );
}

// ── (b) caller-scoping: a different actor sees an empty inbox ─────

#[tokio::test]
async fn inbox_fetch_is_caller_scoped_other_actor_sees_nothing() {
    let (router, state) = router_and_state().await;

    let alice: [u8; 32] = [0x42; 32];
    let (_alice_secret, alice_pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&state.db, &alice, &[0x5e; 32]).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    seal_and_ingest(
        &router,
        &state,
        mta_actor,
        alice,
        &alice_pubkey,
        b"To: alice@local.test\r\n\r\nfor alice only\r\n",
        1_715_000_100,
    )
    .await;

    // Bob is a different User-class actor — his INBOX is empty.
    let bob: [u8; 32] = [0x77; 32];
    let reply = inbox_fetch(&router, &state, bob, 0, 0).await;
    assert!(
        reply.messages.is_empty(),
        "a caller only ever reads its own mailbox"
    );
    assert!(!reply.more);

    // Alice still sees her own message.
    let alice_reply = inbox_fetch(&router, &state, alice, 0, 0).await;
    assert_eq!(alice_reply.messages.len(), 1, "alice reads her own inbox");
}

// ── (c) paging via after_uid cursor + more sentinel ──────────────

#[tokio::test]
async fn inbox_fetch_pages_via_after_uid_cursor() {
    let (router, state) = router_and_state().await;

    let recipient: [u8; 32] = [0x42; 32];
    let (_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&state.db, &recipient, &[0x5e; 32]).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    // Ingest 3 distinct messages (distinct timestamps → distinct
    // message_ids → distinct placement rows / UIDs).
    for i in 0..3u8 {
        let body = format!("To: alice@local.test\r\n\r\nmessage number {i}\r\n");
        seal_and_ingest(
            &router,
            &state,
            mta_actor,
            recipient,
            &recipient_pubkey,
            body.as_bytes(),
            1_715_000_200 + i as i64,
        )
        .await;
    }

    // First page: limit=2 → 2 messages + more=true.
    let page1 = inbox_fetch(&router, &state, recipient, 0, 2).await;
    assert_eq!(page1.messages.len(), 2, "first page has the limit");
    assert!(page1.more, "more pages remain");
    let uids1: Vec<u32> = page1.messages.iter().map(|m| m.uid).collect();
    assert!(uids1[0] < uids1[1], "UIDs ascending within a page");

    // Second page: after_uid = last UID of page 1 → the 3rd message,
    // more=false.
    let cursor = *uids1.last().unwrap();
    let page2 = inbox_fetch(&router, &state, recipient, cursor, 2).await;
    assert_eq!(page2.messages.len(), 1, "third message only");
    assert!(!page2.more, "no more pages");
    assert!(
        page2.messages[0].uid > cursor,
        "cursor advances past the page-1 tail"
    );
}

// ── (c.2) limit clamp: 0 → default, oversize → capped at 50 ──────

#[tokio::test]
async fn inbox_fetch_clamps_limit() {
    let (router, state) = router_and_state().await;

    let recipient: [u8; 32] = [0x42; 32];
    let (_secret, recipient_pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&state.db, &recipient, &[0x5e; 32]).await;

    let mta_actor: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta_actor, BridgeRole::Mta, &[0x91; 32]).await;

    // 3 messages, ask for limit=0 (→ default 50) and a huge limit (→
    // capped at 50): both return all 3 with more=false.
    for i in 0..3u8 {
        let body = format!("To: alice@local.test\r\n\r\nclamp {i}\r\n");
        seal_and_ingest(
            &router,
            &state,
            mta_actor,
            recipient,
            &recipient_pubkey,
            body.as_bytes(),
            1_715_000_300 + i as i64,
        )
        .await;
    }

    let zero = inbox_fetch(&router, &state, recipient, 0, 0).await;
    assert_eq!(zero.messages.len(), 3, "limit=0 → default page returns all");
    assert!(!zero.more);

    let huge = inbox_fetch(&router, &state, recipient, 0, 10_000).await;
    assert_eq!(
        huge.messages.len(),
        3,
        "oversize limit clamped, all returned"
    );
    assert!(!huge.more);
}

// ── (d) allowlist: User + Admin permitted, bridge classes denied ──

#[test]
fn inbox_fetch_user_and_admin_permitted_bridges_denied() {
    // User + Admin permitted (admin ⊇ user, and the read is caller-scoped, so
    // an Admin only ever reads its OWN inbox — harmless). Bridge classes denied
    // (they read mail via the BridgeMda `fauna.bridges.*` plane). Mirrors the
    // allowlist unit test `email_inbox_fetch_user_and_admin_permitted` and the
    // Sent sibling `sent_fetch_user_and_admin_permitted_bridges_denied`. (Was
    // a stale `others_denied` assertion that contradicted the allowlist.)
    assert!(is_permitted(CallerClass::User, "fauna.email.inbox.fetch"));
    assert!(is_permitted(CallerClass::Admin, "fauna.email.inbox.fetch"));
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(
            !is_permitted(class, "fauna.email.inbox.fetch"),
            "fauna.email.inbox.fetch should be denied for {class:?}"
        );
    }
}
