//! Round-trip for the **in-domain** branch of `fauna.email.send` —
//! Fauna→Fauna local delivery routed through the **sealed** new-path
//! store (Track A), so a first-party client's
//! in-domain mail is visible to `fauna.email.inbox.fetch`/IMAP exactly
//! like external inbound.
//!
//! **tier_3** — exercises the real resolver (`account_aliases` via
//! `lookup_exact_alias`), the real seal (`fauna_mls::wrapped_blob::
//! seal_to_recipient` to the recipient's MSEK-derived pubkey), the real
//! sealed-ingest persist core (`insert_inbound_mail` → `__mail/<actor>`
//! segment store + `place_inbound_mail` → `bridge_imap_messages` INBOX),
//! and the real `inbox.fetch` read-back + client-side unseal. No stubs,
//! no plaintext path, no `deliver_local`/`email_aliases`. Only this depth
//! catches the seal/store/read seam (`green-test-or-it-doesnt-work`).
//!
//! Pre-Track-A this file is RED: the in-domain branch delivers via the
//! deprecated `deliver_local` → legacy `inbox` rows (not the sealed
//! `__mail` store), so the recipient's `inbox.fetch` returns empty.
//!
//! Authority: `smtp-server.md` § Recipient handling on submission (the
//! sealed local-delivery mechanism) + § Inbound client receive;
//! `mail-aliases.md` § Implementation status (`lookup_exact_alias`).

mod common;
use common::dispatch;
use common::provision_recipient;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::wrapped_blob::{FAUNA_KEM_XWING, MailRecordEnvelope as BridgeMailEnvelope};
use fauna_nest::db::CacheDb;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::email::{InboxFetchReply, InboxFetchRequest, SendEmailReply, SendEmailRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

/// The OUTER segment-record envelope (`fauna_mail::segments::
/// MailRecordEnvelope`) `read_envelopes_bulk`/`inbox.fetch` ship; its
/// `.encrypted_body` is the inner bridge seal (`BridgeMailEnvelope`). Two
/// distinct same-named types — see `conformance_email_inbox.rs:49-55`.
use fauna_mail::segments::MailRecordEnvelope as SegmentMailEnvelope;

use common::TEST_DOMAIN as DOMAIN;

/// Router with `email_handlers` (both `fauna.email.send` and
/// `fauna.email.inbox.fetch`) + a deployment domain set, so the send
/// handler partitions same-domain recipients into local delivery.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    db.add_mail_domain(DOMAIN, true, "testing", "self_signed", None, None)
        .await
        .unwrap();
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_email_handlers(&mut b);
    (b.build(), state)
}

async fn send(
    router: &RpcRouter,
    state: &Arc<AppState>,
    sender: [u8; 32],
    recipients: Vec<String>,
    raw_rfc5322: Vec<u8>,
) -> Result<SendEmailReply, RpcError> {
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients,
        raw_rfc5322,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(router, state.clone(), sender, "fauna.email.send", payload).await?;
    Ok(decode(&reply).expect("decode send reply"))
}

async fn inbox_fetch(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
) -> InboxFetchReply {
    let req = InboxFetchRequest {
        extra: Default::default(),
        after_uid: 0,
        limit: 0,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.inbox.fetch",
        payload,
    )
    .await
    .expect("inbox.fetch ok");
    decode(&reply).expect("decode inbox.fetch reply")
}

/// Read `caller`'s own `Sent` mailbox over `fauna.email.sent.fetch` — the Sent
/// sibling of `inbox.fetch`, same wire types, caller-scoped.
async fn sent_fetch(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
) -> InboxFetchReply {
    let req = InboxFetchRequest {
        extra: Default::default(),
        after_uid: 0,
        limit: 0,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.sent.fetch",
        payload,
    )
    .await
    .expect("sent.fetch ok");
    decode(&reply).expect("decode sent.fetch reply")
}

fn message(from: &str, to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\r\n{body}",
    )
    .into_bytes()
}

/// Strip the nest's own delivery stamps off a RECIPIENT copy.
///
/// The delivery-time spam-threshold fold (2026-08-18) prepends
/// `X-Fauna-Spam-Threshold: <n>` — and the address-chain fold its sibling
/// `X-Fauna-Address-*` headers — to each recipient's copy at ingest
/// (`bridge_routing_handlers.rs` § the resolved-recipient stamp block). So a
/// recipient's opened bytes are the sender's bytes **plus** those stamp lines,
/// and the round-trip assertions here are about the part the sender actually
/// wrote. The sender's own `Sent` copy is not stamped, so it is compared raw.
///
/// Deliberately narrow: only `X-Fauna-`-prefixed leading lines are dropped, so
/// a regression that mangled any header the sender wrote still reds.
fn strip_delivery_stamps(opened: &[u8]) -> &[u8] {
    let mut rest = opened;
    while rest.starts_with(b"X-Fauna-") {
        match rest.windows(2).position(|w| w == b"\r\n") {
            Some(i) => rest = &rest[i + 2..],
            None => break,
        }
    }
    rest
}

// ── (a) alice → bob, in-domain → bob's sealed INBOX + alice's sealed Sent ──

#[tokio::test]
async fn in_domain_send_lands_in_recipient_sealed_inbox() {
    let (router, state) = router_and_state().await;

    let bob: [u8; 32] = [0x42; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x5e; 32]).await;

    // Sender alice: a User-class actor with handle "alice" so the
    // from-handle verification (From: alice@fauna.test) passes — AND with her
    // OWN MSEK-derived recipient key on file, so nest can seal her durable Sent
    // copy to her (the sender reads her own Sent mailbox with the same key she
    // uses for INBOX).
    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");
    let alice_secret = common::RecipientKeys::derive(&[0xA7; 32]);
    common::seed_recipient_seal_key(&state.db, &alice, &[0xA7; 32]).await;

    let raw = message(
        "alice@fauna.test",
        "bob@fauna.test",
        "in-domain round-trip",
        "Hello bob, this is in-domain mail.\r\n",
    );
    let reply = send(
        &router,
        &state,
        alice,
        vec!["bob@fauna.test".into()],
        raw.clone(),
    )
    .await
    .expect("in-domain send ok");
    assert_eq!(reply.local_delivered, 1, "one local recipient delivered");
    assert_eq!(reply.remote_queued, 0, "nothing remote");
    assert!(
        reply.remote_errors.is_empty(),
        "no errors: {:?}",
        reply.remote_errors
    );

    // Bob reads his own INBOX over the client mail-receive feed and the
    // sealed envelope opens to exactly what alice sent.
    let bob_inbox = inbox_fetch(&router, &state, bob).await;
    assert_eq!(bob_inbox.messages.len(), 1, "bob received one message");
    let segment_env = SegmentMailEnvelope::decode(&bob_inbox.messages[0].sealed_envelope)
        .expect("decode outer segment envelope");
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode inner bridge envelope");
    let opened = bob_secret
        .open(&bridge_env)
        .expect("bob opens his own mail");
    assert_eq!(
        strip_delivery_stamps(&opened),
        raw.as_slice(),
        "in-domain mail decrypts byte-for-byte back to the sent message"
    );
    // The stamp `strip_delivery_stamps` removes must actually be there — else
    // the strip is silently a no-op and this file would stay green through a
    // regression that dropped the delivery-time fold altogether.
    assert!(
        fauna_mail::aliases::read_spam_threshold_stamp(&opened).is_some(),
        "the recipient copy carries the delivery-time spam-threshold stamp"
    );

    // Alice is the sender — her own INBOX stays empty (you never receive your
    // own outbound into INBOX); the durable Sent copy lands in her `Sent`
    // mailbox, asserted next.
    let alice_inbox = inbox_fetch(&router, &state, alice).await;
    assert!(
        alice_inbox.messages.is_empty(),
        "the sender does not receive her own outbound in-domain message in INBOX"
    );

    // `fauna.email.send` writes a durable server-side Sent copy sealed to ALICE's
    // OWN MSEK-derived key into her `Sent` mailbox, so mail composed in a Fauna
    // app survives a restart and reloads via `fauna.email.sent.fetch` — a fresh
    // fetch from uid 0 IS exactly what a restarted client does (its conversation
    // store is in-memory). It opens byte-for-byte with alice's own recipient
    // secret, the same key she uses for INBOX (smtp-server.md § Inbound client
    // receive).
    let alice_sent = sent_fetch(&router, &state, alice).await;
    assert_eq!(
        alice_sent.messages.len(),
        1,
        "alice's send leaves exactly one durable Sent copy"
    );
    let sent_seg = SegmentMailEnvelope::decode(&alice_sent.messages[0].sealed_envelope)
        .expect("decode outer segment envelope of the Sent copy");
    let sent_bridge = BridgeMailEnvelope::from_canonical_bytes(&sent_seg.encrypted_body)
        .expect("decode inner bridge envelope of the Sent copy");
    let sent_opened = alice_secret
        .open(&sent_bridge)
        .expect("alice opens her own Sent copy");
    assert_eq!(
        sent_opened.as_slice(),
        raw.as_slice(),
        "the Sent copy decrypts byte-for-byte to the message alice sent"
    );
}

// ── (a1) sealed at rest, including the index hint ──

/// The storage-mode axis is retired (`docs/goal/architecture/nest/storage-modes.md`):
/// there is no "plaintext-mode nest" configuration left to distinguish from
/// `in_domain_send_lands_in_recipient_sealed_inbox` above — every nest seals
/// in-domain mail at ingest, one byte shape, unconditionally (the nest core
/// The `fauna.email.send` door of `smtp-server.md` § Architectural rules →
/// *The `X-Fauna-*` namespace*: a stamp the CLIENT wrote never reaches the
/// recipient's sealed copy. Bob's copy carries the genuine delivery-time
/// threshold stamp (folded nest-side, never `0` here) and nothing of the forged
/// pair, while the forward-loop trace and a substring look-alike survive;
/// beneath the genuine stamps the copy is byte-for-byte the stripped message.
#[tokio::test]
async fn a_client_supplied_fauna_stamp_never_reaches_the_recipient_copy() {
    let (router, state) = router_and_state().await;

    let bob: [u8; 32] = [0x43; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x5f; 32]).await;
    let alice: [u8; 32] = [0xA2; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");
    common::seed_recipient_seal_key(&state.db, &alice, &[0xA8; 32]).await;

    let mut raw = b"X-Fauna-Spam-Threshold: 0\r\n\
x-fauna-address-suffix: forged\r\n\
\tcontinued\r\n\
X-Fauna-Forwarded-By: actor=peer; t=1; rule=forward-all\r\n\
X-Not-Fauna: keepme\r\n"
        .to_vec();
    raw.extend_from_slice(&message(
        "alice@fauna.test",
        "bob@fauna.test",
        "reserved stamps",
        "Hello bob, trust nothing above the fold.\r\n",
    ));
    let reply = send(
        &router,
        &state,
        alice,
        vec!["bob@fauna.test".into()],
        raw.clone(),
    )
    .await
    .expect("in-domain send ok");
    assert_eq!(reply.local_delivered, 1, "one local recipient delivered");

    let bob_inbox = inbox_fetch(&router, &state, bob).await;
    assert_eq!(bob_inbox.messages.len(), 1, "bob received one message");
    let segment_env = SegmentMailEnvelope::decode(&bob_inbox.messages[0].sealed_envelope)
        .expect("decode outer segment envelope");
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode inner bridge envelope");
    let opened = bob_secret
        .open(&bridge_env)
        .expect("bob opens his own mail");

    let stamped = fauna_mail::aliases::read_spam_threshold_stamp(&opened);
    assert!(
        stamped.is_some() && stamped != Some(0),
        "the recipient copy carries the GENUINE delivery-time threshold, not the forged 0: {stamped:?}"
    );
    let text = String::from_utf8_lossy(&opened);
    assert!(
        !text.contains("forged") && !text.contains("continued"),
        "a client-supplied X-Fauna-* stamp reached the sealed copy:\n{text}"
    );
    assert!(
        text.contains("X-Fauna-Forwarded-By: actor=peer") && text.contains("X-Not-Fauna: keepme"),
        "over-stripped: the forward-loop trace or the look-alike is gone:\n{text}"
    );
    // Beneath the genuine stamps the copy is the stripped message, byte for
    // byte. `strip_delivery_stamps` skips every leading `X-Fauna-` line — the
    // kept forward-loop trace included, which is why both sides go through it;
    // the trace's survival is the `contains` assertion above.
    let expected = fauna_mail::received_header::strip_fauna_headers(&raw);
    assert_eq!(
        strip_delivery_stamps(&opened),
        strip_delivery_stamps(&expected),
        "beneath the genuine stamps the copy is the stripped message, byte for byte"
    );
}

/// never holds a content key). What this test still pins, distinctly from the
/// base case above: the sealed shape is checked explicitly
/// (`is_sealed_mail_record`), and the encrypted **index hint** seals too —
/// never raw token bytes at rest.
#[tokio::test]
async fn in_domain_send_seals_at_rest_including_index_hint() {
    let (router, state) = router_and_state().await;

    let bob: [u8; 32] = [0x42; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x5e; 32]).await;
    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");
    common::seed_recipient_seal_key(&state.db, &alice, &[0xA7; 32]).await;

    let raw = message(
        "alice@fauna.test",
        "bob@fauna.test",
        "sealed at rest",
        "Sealed at rest, unconditionally.\r\n",
    );
    send(
        &router,
        &state,
        alice,
        vec!["bob@fauna.test".into()],
        raw.clone(),
    )
    .await
    .expect("in-domain send ok");

    let bob_inbox = inbox_fetch(&router, &state, bob).await;
    assert_eq!(bob_inbox.messages.len(), 1, "bob received one message");
    let segment_env = SegmentMailEnvelope::decode(&bob_inbox.messages[0].sealed_envelope)
        .expect("decode outer segment envelope");
    assert!(
        fauna_mls::wrapped_blob::is_sealed_mail_record(&segment_env.encrypted_body),
        "the nest must store the record SEALED at rest, got raw bytes"
    );
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode inner bridge envelope");
    let opened = bob_secret.open(&bridge_env).expect("bob opens his mail");
    assert_eq!(strip_delivery_stamps(&opened), raw.as_slice());
    // The index hint seals too — never raw token bytes at rest.
    assert!(
        segment_env.encrypted_index_hint.is_empty()
            || fauna_mls::wrapped_blob::is_sealed_mail_record(&segment_env.encrypted_index_hint),
        "the index hint must be sealed (or absent), got raw token bytes"
    );
}

// ── (a2) post-quantum: the hybrid round-trip (S3f acceptance gate) ──

/// **S3f acceptance gate — hybrid → hybrid.** A recipient's seal key always
/// carries its ML-KEM ek, so the in-domain body seals with the X-Wing hybrid
/// suite (`enc` is the 1120-byte ciphertext) and decrypts byte-for-byte via the
/// hybrid opener. No capability token gates the suite (2026-09-24 ruling).
#[tokio::test]
async fn in_domain_send_seals_hybrid() {
    let (router, state) = router_and_state().await;

    let bob: [u8; 32] = [0x42; 32];
    let bob_keys = provision_recipient(&state, bob, "bob", [0x5e; 32]).await;

    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");
    common::seed_recipient_seal_key(&state.db, &alice, &[0xA7; 32]).await;

    let raw = message(
        "alice@fauna.test",
        "bob@fauna.test",
        "post-quantum round-trip",
        "Hello bob, this is hybrid-sealed mail.\r\n",
    );
    let reply = send(
        &router,
        &state,
        alice,
        vec!["bob@fauna.test".into()],
        raw.clone(),
    )
    .await
    .expect("in-domain hybrid send ok");
    assert_eq!(reply.local_delivered, 1, "one local recipient delivered");

    let bob_inbox = inbox_fetch(&router, &state, bob).await;
    assert_eq!(bob_inbox.messages.len(), 1, "bob received one message");
    let segment_env = SegmentMailEnvelope::decode(&bob_inbox.messages[0].sealed_envelope)
        .expect("decode outer segment envelope");
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode inner bridge envelope");
    // The body is sealed with the X-Wing hybrid suite (kem 0xFC00, enc 1120 B).
    assert_eq!(
        bridge_env.hpke.kem_suite.kem, FAUNA_KEM_XWING,
        "in-domain body seals X-Wing: a recipient's seal key always carries its ek"
    );
    assert_eq!(
        bridge_env.hpke.enc.len(),
        1120,
        "X-Wing enc is the 1120-byte ciphertext"
    );
    // …and it opens byte-for-byte via the hybrid opener (the two MSEK-derived halves).
    let opened = bob_keys
        .open(&bridge_env)
        .expect("bob opens his own hybrid mail");
    assert_eq!(
        strip_delivery_stamps(&opened),
        raw.as_slice(),
        "hybrid in-domain mail decrypts byte-for-byte back to the sent message"
    );

    // PQ-6: the companion index hint is ALSO sealed X-Wing (mirrors the body) so
    // the at-rest hint no longer leaks the plaintext body word-set under HNDL.
    // The hint travels in the same outer segment envelope as the body
    // (`encrypted_index_hint`), sealed as its own inner bridge `MailRecordEnvelope`.
    let hint_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_index_hint)
        .expect("decode inner bridge envelope of the index hint");
    assert_eq!(
        hint_env.hpke.kem_suite.kem, FAUNA_KEM_XWING,
        "PQ-6: in-domain index hint seals X-Wing, mirroring the body"
    );
    assert_eq!(
        hint_env.hpke.enc.len(),
        1120,
        "X-Wing hint enc is the 1120-byte ciphertext"
    );
    // …and the hint opens via the same hybrid opener to the tokenized subject+body
    // word-set (the read path uses the suite-dispatching opener unchanged).
    let opened_hint = bob_keys
        .open(&hint_env)
        .expect("bob opens his hybrid-sealed index hint");
    assert!(
        String::from_utf8_lossy(&opened_hint).contains("hybrid"),
        "the decrypted hint holds the tokenized subject+body word-set"
    );
}

// ── (b) local recipient without a provisioned key → local error ───────

#[tokio::test]
async fn in_domain_send_to_keyless_recipient_reports_local_error() {
    let (router, state) = router_and_state().await;

    // Carol has an exact alias but never provisioned an MLS pubkey, so
    // nest cannot seal to her — a per-recipient local error, not a
    // counted delivery (and never a silent drop).
    let carol: [u8; 32] = [0xC0; 32];
    state
        .db
        .put_exact_alias(DOMAIN, "carol", "exact", &carol)
        .await
        .expect("seed carol alias");

    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");

    let raw = message("alice@fauna.test", "carol@fauna.test", "no key", "hi\r\n");
    let reply = send(&router, &state, alice, vec!["carol@fauna.test".into()], raw)
        .await
        .expect("send returns ok with a per-recipient error, not a hard failure");
    assert_eq!(reply.local_delivered, 0, "keyless recipient not delivered");
    assert!(
        reply.remote_errors.iter().any(|e| e.starts_with("local:")),
        "a local-delivery error is surfaced: {:?}",
        reply.remote_errors
    );
}

// ── (c) mixed local + remote: both paths fire ────────────────────────

#[tokio::test]
async fn in_domain_and_remote_recipients_both_delivered() {
    let (router, state) = router_and_state().await;

    let bob: [u8; 32] = [0x42; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x5e; 32]).await;

    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");

    let raw = message(
        "alice@fauna.test",
        "bob@fauna.test, dave@external.test",
        "mixed",
        "to a local and a remote recipient\r\n",
    );
    let reply = send(
        &router,
        &state,
        alice,
        vec!["bob@fauna.test".into(), "dave@external.test".into()],
        raw.clone(),
    )
    .await
    .expect("mixed send ok");
    assert_eq!(reply.local_delivered, 1, "bob delivered locally");
    assert_eq!(reply.remote_queued, 1, "dave queued for outbound");

    // The local half is genuinely sealed + readable.
    let bob_inbox = inbox_fetch(&router, &state, bob).await;
    assert_eq!(bob_inbox.messages.len(), 1, "bob received the local copy");
    let segment_env = SegmentMailEnvelope::decode(&bob_inbox.messages[0].sealed_envelope).unwrap();
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body).unwrap();
    let opened = bob_secret.open(&bridge_env).expect("bob opens his copy");
    assert_eq!(strip_delivery_stamps(&opened), raw.as_slice());
}

// ── (d) the From-handle gate (mail-app-surface.md § First-party client send) ────────

/// A registered actor with NO handle cannot send from the deployment's own
/// domain — it owns no address there, so there is no From it may claim.
///
/// The shape under test is the reason this needed its own pin: a handle-less
/// registered actor stores the **empty string**, not NULL, so
/// `db.get_handle` answers `Some("")` and the gate's `Ok(None)` arm never fires
/// for anyone who can actually reach this handler. Until the empty case was
/// folded in, such a sender was refused with the nonsense
/// "from address does not match your handle ()".
#[tokio::test]
async fn a_handleless_sender_is_refused_with_the_no_handle_reason() {
    let (router, state) = router_and_state().await;

    // `create_user` (not `create_user_with_handle`) — the admin-direct-admit
    // shape, which admits without a handle.
    let mallory: [u8; 32] = [0xB1; 32];
    state
        .db
        .create_user(&mallory, "free", "mallory")
        .await
        .expect("admit a handle-less actor");

    let err = send(
        &router,
        &state,
        mallory,
        vec!["bob@external.test".into()],
        message(
            "mallory@fauna.test",
            "bob@external.test",
            "nope",
            "body\r\n",
        ),
    )
    .await
    .expect_err("a handle-less sender must be refused");

    assert_eq!(err.code, "fauna.email.permission_denied");
    let detail = format!("{:?}", err.details);
    assert!(
        detail.contains("must set a handle"),
        "the refusal must name the real reason (no handle), not an empty-parens \
         handle mismatch; got {detail}"
    );
}

/// The same actor sending from an OFF-domain From is untouched: the deployment
/// is not authoritative for that domain, so the gate does not apply.
#[tokio::test]
async fn a_handleless_sender_is_not_gated_on_an_off_domain_from() {
    let (router, state) = router_and_state().await;
    let mallory: [u8; 32] = [0xB2; 32];
    state
        .db
        .create_user(&mallory, "free", "mallory")
        .await
        .expect("admit a handle-less actor");

    send(
        &router,
        &state,
        mallory,
        vec!["bob@external.test".into()],
        message(
            "mallory@elsewhere.test",
            "bob@external.test",
            "fine",
            "body\r\n",
        ),
    )
    .await
    .expect("an off-domain From bypasses the handle gate");
}

// ── (e) the gate's DOMAIN SET — every active local domain, not just the primary ──

/// A SECOND active local domain is one the deployment claims just as much as
/// the primary — `mail-multidomain.md` § *The model: one handle, addressable on
/// every active domain* makes `bob@<any active domain>` bob's own address, and
/// `mail-app-surface.md` § *Sender-handle verification* scopes the gate to "the
/// `From:` domain is **one the deployment claims**", not to the primary alone.
///
/// So mallory, whose handle is `mallory`, may not send as `bob@second.test`.
/// Before the fix the gate compared only against the primary, so every
/// non-primary active domain was a free impersonation surface — and because the
/// chosen `From:` becomes the outbound envelope sender (`original_sender`) and
/// DKIM signs on the `From:` domain, the forgery went out DMARC-aligned.
#[tokio::test]
async fn a_secondary_active_domain_is_gated_like_the_primary() {
    let (router, state) = router_and_state().await;
    state
        .db
        .add_mail_domain("second.test", false, "testing", "self_signed", None, None)
        .await
        .expect("add a second active local domain");

    let mallory: [u8; 32] = [0xB3; 32];
    state
        .db
        .create_user_with_handle(&mallory, "free", "mallory", None)
        .await
        .expect("admit mallory with her own handle");

    let err = send(
        &router,
        &state,
        mallory,
        vec!["victim@external.test".into()],
        message(
            "bob@second.test",
            "victim@external.test",
            "not from bob",
            "body\r\n",
        ),
    )
    .await
    .expect_err("sending as another handle on a secondary active domain must be refused");

    assert_eq!(err.code, "fauna.email.permission_denied");
    let detail = format!("{:?}", err.details);
    assert!(
        detail.contains("does not match your handle"),
        "the refusal must be the handle-mismatch arm; got {detail}"
    );
}

/// The same actor sending as HER OWN local part on the secondary domain is
/// allowed — the gate is a handle match across the whole claimed set, not a
/// primary-only allowlist that would lock users out of their other addresses
/// (`mail-multidomain.md` § *Cross-domain submission policy*: "a user with
/// `bob@domain1` AND `bob@domain2` claimed may submit with either").
#[tokio::test]
async fn a_secondary_active_domain_still_admits_your_own_handle() {
    let (router, state) = router_and_state().await;
    state
        .db
        .add_mail_domain("second.test", false, "testing", "self_signed", None, None)
        .await
        .expect("add a second active local domain");

    let mallory: [u8; 32] = [0xB4; 32];
    state
        .db
        .create_user_with_handle(&mallory, "free", "mallory", None)
        .await
        .expect("admit mallory with her own handle");

    send(
        &router,
        &state,
        mallory,
        vec!["victim@external.test".into()],
        message(
            "mallory@second.test",
            "victim@external.test",
            "her own address",
            "body\r\n",
        ),
    )
    .await
    .expect("her own handle on a secondary active domain is hers to send from");
}

/// The owned-alias arm of the one sender-ownership rule (`mail-multidomain.md`
/// § From: header ownership; this door's statement: `mail-app-surface.md`
/// § Sender-handle verification): a `From:` whose local part is not the
/// handle but an exact alias the resolver attributes to the actor is theirs
/// to send from — the MUA-facing submission door accepts it as `MAIL FROM`
/// and `From:` alike (`TestSubmissionDataAcceptsFromHeaderNamingAnOwnedAlias`),
/// and an app must not be held to less.
#[tokio::test]
async fn an_owned_alias_is_admitted_as_the_from_address() {
    let (router, state) = router_and_state().await;
    let mallory: [u8; 32] = [0xB6; 32];
    state
        .db
        .create_user_with_handle(&mallory, "free", "mallory", None)
        .await
        .expect("admit mallory with her own handle");
    state
        .db
        .put_exact_alias(DOMAIN, "sales", "exact", &mallory)
        .await
        .expect("seed mallory's exact alias");

    let reply = send(
        &router,
        &state,
        mallory,
        vec!["victim@external.test".into()],
        message(
            &format!("\"Sales\" <sales@{DOMAIN}>"),
            "victim@external.test",
            "from her alias",
            "body\r\n",
        ),
    )
    .await
    .expect("an alias the resolver attributes to the sender is hers to send from");
    assert_eq!(
        reply.remote_queued, 1,
        "accepted and queued for the remote recipient"
    );
}

/// …and an alias the resolver attributes to ANOTHER actor is exactly the
/// impersonation the rule exists to stop: refused on the mismatch arm, which
/// now names both things the address could have been (the handle, an owned
/// alias). The submission door's twin is
/// `TestSubmissionDataRefusesFromHeaderOwnedByAnotherActor`.
#[tokio::test]
async fn another_actors_alias_is_refused_as_the_from_address() {
    let (router, state) = router_and_state().await;
    let mallory: [u8; 32] = [0xB7; 32];
    state
        .db
        .create_user_with_handle(&mallory, "free", "mallory", None)
        .await
        .expect("admit mallory with her own handle");
    let ceo: [u8; 32] = [0xC0; 32];
    state
        .db
        .create_user_with_handle(&ceo, "free", "ceo", None)
        .await
        .expect("admit the ceo");
    state
        .db
        .put_exact_alias(DOMAIN, "boss", "exact", &ceo)
        .await
        .expect("seed the ceo's exact alias");

    let err = send(
        &router,
        &state,
        mallory,
        vec!["victim@external.test".into()],
        message(
            &format!("boss@{DOMAIN}"),
            "victim@external.test",
            "not from the boss",
            "body\r\n",
        ),
    )
    .await
    .expect_err("sending as another actor's alias must be refused");
    assert_eq!(err.code, "fauna.email.permission_denied");
    let detail = format!("{:?}", err.details);
    assert!(
        detail.contains("does not match your handle (mallory) or an alias you own"),
        "the refusal names both things the address could have been; got {detail}"
    );
}

/// A `From:` field naming other than exactly one mailbox is refused before
/// any gate, seal or enqueue (`mail-multidomain.md` § From: header ownership
/// → *Exactly one mailbox*; the submission door's twin is
/// `TestSubmissionDataRefusesFromFieldWithOtherThanOneMailbox`): a domain-less
/// token that `mail-parser` cannot read as an addr-spec leaves nothing to own
/// and nothing for DKIM to key on, and two mailboxes would sign under the
/// first's domain with a foreign one riding along. Until 2026-09-27 the
/// domain-less shape fell through to the off-domain path on an
/// `unknown@unknown` sentinel; the sentinel is gone — the gate reads the one
/// mailbox the shared-Rust `from_mailboxes` admits, so no `@`-less string can
/// reach it.
#[tokio::test]
async fn a_from_field_naming_other_than_one_mailbox_is_refused() {
    let (router, state) = router_and_state().await;
    let mallory: [u8; 32] = [0xB5; 32];
    state
        .db
        .create_user_with_handle(&mallory, "free", "mallory", None)
        .await
        .expect("admit mallory with her own handle");

    let two_in_a_list = format!("mallory@{DOMAIN}, ceo@bank.example");
    let two_in_a_group = format!("Team: mallory@{DOMAIN}, bob@{DOMAIN};");
    for (from, why) in [
        ("no-at-sign-here", "a domain-less token names no mailbox"),
        ("\"Just A Name\"", "a display name alone names no mailbox"),
        (two_in_a_list.as_str(), "a list of two mailboxes"),
        (two_in_a_group.as_str(), "a group of two mailboxes"),
    ] {
        let err = send(
            &router,
            &state,
            mallory,
            vec!["victim@external.test".into()],
            message(from, "victim@external.test", "malformed", "body\r\n"),
        )
        .await
        .expect_err(&format!("{why}: From: {from:?} must be refused"));
        assert_eq!(
            err.code, "fauna.email.invalid_params",
            "{why}: a request-validation refusal, there is no identity to permit or deny; got {err:?}"
        );
        let detail = format!("{:?}", err.details);
        assert!(
            detail.contains("exactly one mailbox"),
            "{why}: the refusal names the rule; got {detail}"
        );
    }
}

/// The app door's side of the From-field rule (`smtp-server.md` § Architectural
/// rules): the handle gate reads the message's From through `mail-parser`,
/// which takes the LAST From field, and the relayed message is signed for it —
/// while a receiver's DMARC may align against the FIRST. So a message carrying
/// other than exactly one From field is refused before any gate, seal or
/// enqueue, whichever order its fields come in.
#[tokio::test]
async fn a_message_with_other_than_one_from_field_is_refused() {
    let (router, state) = router_and_state().await;
    let alice: [u8; 32] = [0xB6; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("admit alice with her own handle");

    let own = format!("alice@{DOMAIN}");
    let two_froms = |first: &str, last: &str| {
        format!(
            "From: {first}\r\nFrom: {last}\r\nTo: bob@external.test\r\nSubject: two\r\n\r\nbody"
        )
        .into_bytes()
    };
    for (label, raw) in [
        ("victim first", two_froms("ceo@bank.test", &own)),
        ("own first", two_froms(&own, "ceo@bank.test")),
        (
            "no From field",
            b"To: bob@external.test\r\nSubject: none\r\n\r\nbody".to_vec(),
        ),
    ] {
        let err = send(
            &router,
            &state,
            alice,
            vec!["bob@external.test".into()],
            raw,
        )
        .await
        .expect_err(label);
        assert_eq!(err.code, "fauna.email.invalid_params", "{label}");
        let detail = format!("{:?}", err.details);
        assert!(detail.contains("exactly one From"), "{label}: {detail}");
    }

    // The control: the same sender with one From field still sends.
    let reply = send(
        &router,
        &state,
        alice,
        vec!["bob@external.test".into()],
        message(&own, "bob@external.test", "one", "body\r\n"),
    )
    .await
    .expect("one From field sends");
    assert_eq!(reply.remote_queued, 1);
}

// ── (f) the authenticated-sender stamp (smtp-server.md § Architectural rules →
//        *The `X-Fauna-*` namespace*, "The authenticated-sender stamp") ──

/// Send `raw` from `sender` (handle `handle`, own recipient key on file) to a
/// freshly provisioned in-domain bob, and return bob's opened copy.
async fn send_to_bob_and_open(sender_seed: u8, handle: &str, raw: Vec<u8>) -> Vec<u8> {
    let (router, state) = router_and_state().await;
    let bob: [u8; 32] = [0x44; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x60; 32]).await;
    let sender: [u8; 32] = [sender_seed; 32];
    state
        .db
        .create_user_with_handle(&sender, "free", handle, None)
        .await
        .expect("create sender with handle");
    common::seed_recipient_seal_key(&state.db, &sender, &[sender_seed ^ 0x0F; 32]).await;

    let reply = send(&router, &state, sender, vec!["bob@fauna.test".into()], raw)
        .await
        .expect("in-domain send ok");
    assert_eq!(reply.local_delivered, 1, "one local recipient delivered");

    let bob_inbox = inbox_fetch(&router, &state, bob).await;
    assert_eq!(bob_inbox.messages.len(), 1, "bob received one message");
    let segment_env = SegmentMailEnvelope::decode(&bob_inbox.messages[0].sealed_envelope)
        .expect("decode outer segment envelope");
    let bridge_env = BridgeMailEnvelope::from_canonical_bytes(&segment_env.encrypted_body)
        .expect("decode inner bridge envelope");
    bob_secret
        .open(&bridge_env)
        .expect("bob opens his own mail")
}

/// The handle gate fired and passed, so the door vouches for the `From:` it
/// verified: bob's sealed copy names alice, lower-cased.
#[tokio::test]
async fn a_handle_gated_in_domain_from_is_stamped_as_the_authenticated_sender() {
    let opened = send_to_bob_and_open(
        0xC1,
        "alice",
        message(
            "Alice <Alice@Fauna.Test>",
            "bob@fauna.test",
            "stamped",
            "body\r\n",
        ),
    )
    .await;
    assert_eq!(
        fauna_mail::sender_auth::read_authenticated_sender_stamp(&opened).as_deref(),
        Some("alice@fauna.test"),
        "the recipient copy carries the gate-verified From as the authenticated sender:\n{}",
        String::from_utf8_lossy(&opened)
    );
}

/// An off-domain `From:` bypasses the handle gate — the door verified nothing,
/// so the copy carries no stamp ("absent means unauthenticated").
#[tokio::test]
async fn an_off_domain_from_is_not_stamped() {
    let opened = send_to_bob_and_open(
        0xC2,
        "alice",
        message(
            "alice@elsewhere.test",
            "bob@fauna.test",
            "unstamped",
            "body\r\n",
        ),
    )
    .await;
    assert_eq!(
        fauna_mail::sender_auth::read_authenticated_sender_stamp(&opened),
        None,
        "an off-domain From must leave the copy unstamped:\n{}",
        String::from_utf8_lossy(&opened)
    );
    assert!(
        !String::from_utf8_lossy(&opened)
            .to_ascii_lowercase()
            .contains("x-fauna-authenticated-sender"),
        "no authenticated-sender line at all on the off-domain bypass"
    );
}

/// A client-supplied `X-Fauna-Authenticated-Sender:` naming someone else is
/// stripped at the door: the in-domain copy carries only the genuine stamp
/// (the sender's own verified address), and the off-domain copy carries none.
#[tokio::test]
async fn a_client_supplied_authenticated_sender_stamp_is_stripped() {
    let forged = |from: &str| {
        let mut raw = b"X-Fauna-Authenticated-Sender: carol@fauna.test\r\n".to_vec();
        raw.extend_from_slice(&message(from, "bob@fauna.test", "forged", "body\r\n"));
        raw
    };

    let opened = send_to_bob_and_open(0xC3, "mallory", forged("mallory@fauna.test")).await;
    let text = String::from_utf8_lossy(&opened).to_string();
    assert_eq!(
        fauna_mail::sender_auth::read_authenticated_sender_stamp(&opened).as_deref(),
        Some("mallory@fauna.test"),
        "the genuine stamp names the verified sender, not the forged one:\n{text}"
    );
    assert!(
        !text.contains("carol@fauna.test"),
        "the forged stamp reached the sealed copy:\n{text}"
    );

    let opened = send_to_bob_and_open(0xC4, "mallory", forged("mallory@elsewhere.test")).await;
    let text = String::from_utf8_lossy(&opened).to_string();
    assert_eq!(
        fauna_mail::sender_auth::read_authenticated_sender_stamp(&opened),
        None,
        "off-domain: the forged stamp is stripped and none is written:\n{text}"
    );
    assert!(
        !text.contains("carol@fauna.test"),
        "the forged stamp reached the sealed copy:\n{text}"
    );
}
