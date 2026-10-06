//! **tier_3** — the guardian mail gate (`docs/goal/behavior/family-safety.md`
//! § The mail gate), slice 3b (tracked internally).
//!
//! `unknown_sender_mail` is the one reach knob whose subject is an **email
//! address**, not an actor, which forces two enforcement points:
//!
//! - **`reject`** — refused per-recipient. Externally at the SMTP `RCPT TO`
//!   stage (the `fauna.bridges.resolve_recipient` resolver, pinned by the
//!   in-crate tests in `bridge_routing_handlers`); in-domain by refusing the
//!   *sender* of `fauna.email.send` with a typed error, since no SMTP stage
//!   exists on that path.
//! - **`hold`** — a *placement* decision at the shared sealed-ingest core, into
//!   the ward's held mailbox. Recomputed nest-side from stored policy, never
//!   from a placement flag the bridge carries (the MTA bridge itself is inside
//!   the mail TCB — § The mail gate states the honest boundary).
//!
//! Everything below drives the **real** stack: the real `account_aliases`
//! resolver, the real seal to the recipient's MSEK-derived pubkey, the real
//! sealed-ingest core (`persist_decoded_inbound_mail`), the real IMAP placement
//! rows, and the real `fauna.family.approvals.{list,decide}` / `graduate`
//! handlers. No stubs — only this depth catches the placement/release seam.
//!
//! The one thing not driven here is `MailIngress::System` (bounces / NDRs /
//! security notices are never gated). Its decision lives in the shared pure
//! primitive, unit-tested in `fauna_core::data` (`system_generated_mail_is_
//! never_gated`), and its call sites pass the discriminator explicitly.

mod common;
use common::{encode, sealed};

use std::sync::Arc;

use bytes::Bytes;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_imap::{GUARDIAN_HELD_MAILBOX, StoreFlagsDbOp};
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{bridge_imap_handlers, bridge_routing_handlers, email_handlers, family_handlers};
use fauna_protocol::bridge_routing::{
    CreateMailboxReply, CreateMailboxRequest, DeleteMailboxReply, DeleteMailboxRequest,
    ExpungeReply, ExpungeRequest, IngestInboundMailRequest, ListMessagesReply, ListMessagesRequest,
    MoveMessagesRequest, PublicMailMetadata, RenameMailboxReply, RenameMailboxRequest,
    ResolveRecipientReply, ResolveRecipientRequest,
};
use fauna_protocol::email::{InboxFetchReply, InboxFetchRequest, SendEmailRequest};
use fauna_protocol::family::{
    FamilyApprovalDecideRequest, FamilyApprovalsListReply, FamilyApprovalsListRequest,
    FamilyGraduateRequest, FamilyPolicyUpdateRequest, ReachPolicy,
};
use fauna_protocol::{ByteBuf, RpcError, decode_strict as decode};

const DOMAIN: &str = "fauna.test";

fn router() -> RpcRouter {
    let mut b = RpcRouter::builder();
    email_handlers::register_email_handlers(&mut b);
    family_handlers::register_family_handlers(&mut b);
    bridge_imap_handlers::register_bridge_imap_handlers(&mut b);
    bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
    b.build()
}

async fn state_with_mail() -> (Arc<CacheDb>, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    db.add_mail_domain(DOMAIN, true, "testing", "self_signed", None, None)
        .await
        .unwrap();
    let state = Arc::new(AppState::for_test(db.clone()));
    (db, state)
}

async fn dispatch(
    router: &RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    payload: Bytes,
) -> Result<Bytes, RpcError> {
    let meta = router.kind_meta(kind).expect("kind registered");
    (meta.handler)(state, actor, payload).await
}

/// A full account able to both send (`From: <handle>@DOMAIN` matches its
/// handle) and receive (exact alias + MSEK-derived recipient pubkey).
async fn mail_user(state: &Arc<AppState>, handle: &str, guardian: Option<[u8; 32]>) -> [u8; 32] {
    let id = ActorKeypair::generate().actor_id().0;
    state
        .db
        .create_user_with_handle(&id, "personal", handle, guardian.as_ref().map(|g| &g[..]))
        .await
        .expect("create user");
    // Deterministic MSEK per actor — the seal target; the test never opens the
    // body, only asserts placement.
    let msek = id;
    common::seed_recipient_seal_key(&state.db, &id, &msek).await;
    state
        .db
        .put_exact_alias(DOMAIN, handle, "exact", &id)
        .await
        .unwrap();
    id
}

fn rfc5322(from: &str, to: &str, subject: &str) -> Vec<u8> {
    format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n\r\nhello\r\n").into_bytes()
}

/// A deterministic Message-ID that *verifies* as Fauna-minted — the
/// self-describing mint both stamp paths share
/// (`fauna_conversations::rfc5322::new_message_id`; the Go submission
/// server's stamp over the same `fauna_mail::msgid`). Only verifying ids
/// seed the ward's sent-Message-ID set (§ The mail gate — a third-party
/// MUA's id is never seeded: its entropy is unknowable, and shape alone is
/// not provenance).
fn minted_msgid(seed: u8) -> String {
    let local = fauna_mail::msgid::mint_local(&[seed; fauna_mail::msgid::MSGID_RANDOM_LEN]);
    format!("<{local}@{DOMAIN}>")
}

/// The same, carrying an explicit `Message-ID:` — what a real client always
/// stamps, and what the ward's sent-Message-ID set is seeded from.
fn rfc5322_with_msgid(from: &str, to: &str, subject: &str, msgid: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: {msgid}\r\n\r\nhello\r\n"
    )
    .into_bytes()
}

/// Send one in-domain message as `sender` to `recipients`.
async fn send_mail(
    r: &RpcRouter,
    st: &Arc<AppState>,
    sender: [u8; 32],
    sender_handle: &str,
    recipients: &[&str],
    subject: &str,
) -> Result<Bytes, RpcError> {
    let from = format!("{sender_handle}@{DOMAIN}");
    let to = recipients.join(", ");
    dispatch(
        r,
        st.clone(),
        sender,
        "fauna.email.send",
        encode(&SendEmailRequest {
            recipients: recipients.iter().map(|s| s.to_string()).collect(),
            raw_rfc5322: rfc5322(&from, &to, subject),
            extra: Default::default(),
        }),
    )
    .await
}

/// Send one message carrying `msgid` — the outbound path the ward's
/// sent-Message-ID seed rides (`fauna.email.send`, path A of two).
async fn send_mail_with_msgid(
    r: &RpcRouter,
    st: &Arc<AppState>,
    sender: [u8; 32],
    sender_handle: &str,
    recipients: &[&str],
    msgid: &str,
) -> Result<Bytes, RpcError> {
    let from = format!("{sender_handle}@{DOMAIN}");
    let to = recipients.join(", ");
    dispatch(
        r,
        st.clone(),
        sender,
        "fauna.email.send",
        encode(&SendEmailRequest {
            recipients: recipients.iter().map(|s| s.to_string()).collect(),
            raw_rfc5322: rfc5322_with_msgid(&from, &to, "hi", msgid),
            extra: Default::default(),
        }),
    )
    .await
}

async fn set_policy(
    r: &RpcRouter,
    st: &Arc<AppState>,
    guardian: [u8; 32],
    ward: [u8; 32],
    unknown_sender_mail: &str,
) {
    dispatch(
        r,
        st.clone(),
        guardian,
        "fauna.family.policy.update",
        encode(&FamilyPolicyUpdateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            policy: ReachPolicy {
                unknown_sender_mail: unknown_sender_mail.into(),
                ..Default::default()
            },
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian sets policy");
}

async fn inbox_count(r: &RpcRouter, st: &Arc<AppState>, actor: [u8; 32]) -> usize {
    let reply: InboxFetchReply = decode(
        &dispatch(
            r,
            st.clone(),
            actor,
            "fauna.email.inbox.fetch",
            encode(&InboxFetchRequest::default()),
        )
        .await
        .expect("inbox fetch"),
    )
    .unwrap();
    reply.messages.len()
}

/// The one held message's id, or panic. Envelope sidecar only — never content.
async fn only_hold(db: &CacheDb, ward: &[u8; 32]) -> (Vec<u8>, String) {
    let holds = db.list_mail_holds(&ward[..]).await.unwrap();
    assert_eq!(holds.len(), 1, "exactly one hold expected, got {holds:?}");
    (holds[0].message_id.clone(), holds[0].sender_address.clone())
}

async fn is_placed(db: &CacheDb, actor: &[u8; 32], mailbox: &str, message_id: &[u8]) -> bool {
    let mid: [u8; 32] = message_id.try_into().expect("32-byte message id");
    db.find_message_uid(actor, mailbox, &mid)
        .await
        .unwrap()
        .is_some()
}

// ── `allow` (the default) ──────────────────────────────────────────────

#[tokio::test]
async fn a_default_policy_delivers_cold_mail_straight_to_inbox() {
    let (_db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;

    // The link exists, the policy is untouched → unsupervised-equivalent.
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "hi")
        .await
        .expect("cold mail delivered under the default policy");

    assert_eq!(inbox_count(&r, &st, ward).await, 1);
    assert!(_db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
}

// ── `hold` — placement, the queue, approve, deny ───────────────────────

#[tokio::test]
async fn cold_mail_to_a_hold_ward_lands_in_the_held_mailbox_never_inbox() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;

    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "hi")
        .await
        .expect("a hold is not a refusal — the message is accepted");

    // Accepted, sealed, and *placed* — in the held mailbox, not INBOX, and
    // never `Junk` (the ward must tell "guardian reviewing" from "spam").
    assert_eq!(inbox_count(&r, &st, ward).await, 0, "not in INBOX");
    let (message_id, sender) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert!(!is_placed(&db, &ward, "Junk", &message_id).await);
    assert_eq!(sender, "stranger@fauna.test", "normalized envelope sender");
}

#[tokio::test]
async fn the_guardian_queue_shows_envelope_metadata_only() {
    let (_db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(
        &r,
        &st,
        stranger,
        "stranger",
        &["kid@fauna.test"],
        "a secret subject",
    )
    .await
    .unwrap();

    let list: FamilyApprovalsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest::default()),
        )
        .await
        .expect("guardian reads the queue"),
    )
    .unwrap();

    assert_eq!(list.approvals.len(), 1);
    let e = &list.approvals[0];
    assert_eq!(e.kind, "mail_hold");
    assert_eq!(e.peer_address, "stranger@fauna.test");
    assert!(e.peer_actor_id.is_empty(), "a mail sender has no actor");
    assert!(!e.message_id.is_empty(), "decide names the message");
    // A subject line is content. The body is sealed to the ward; the nest could
    // not read it even if the design allowed.
    assert_eq!(e.summary, "", "never subject-derived text");
    assert!(!String::from_utf8_lossy(&e.message_id).contains("secret"));
}

#[tokio::test]
async fn guardian_approve_releases_to_inbox_allowlists_the_sender_and_lets_the_next_one_through() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "one")
        .await
        .unwrap();
    let (message_id, _) = only_hold(&db, &ward).await;

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(message_id.clone()),
            approve: true,
            ..Default::default()
        }),
    )
    .await
    .expect("guardian approves");

    // Released: a real IMAP move, so it leaves the held mailbox for INBOX.
    assert!(is_placed(&db, &ward, "INBOX", &message_id).await);
    assert!(!is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert_eq!(inbox_count(&r, &st, ward).await, 1);
    // Sidecar retired; sender now known.
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
    assert!(
        db.is_known_mail_sender(&ward[..], "stranger@fauna.test")
            .await
            .unwrap()
    );

    // ...and the sender's NEXT message is no longer held.
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "two")
        .await
        .unwrap();
    assert_eq!(inbox_count(&r, &st, ward).await, 2);
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
}

#[tokio::test]
async fn guardian_deny_discards_the_message_and_does_not_allowlist() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "one")
        .await
        .unwrap();
    let (message_id, _) = only_hold(&db, &ward).await;

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(message_id.clone()),
            approve: false,
            ..Default::default()
        }),
    )
    .await
    .expect("guardian denies");

    assert!(!is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert!(!is_placed(&db, &ward, "INBOX", &message_id).await);
    assert_eq!(inbox_count(&r, &st, ward).await, 0);
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
    assert!(
        !db.is_known_mail_sender(&ward[..], "stranger@fauna.test")
            .await
            .unwrap(),
        "deny must never allowlist the sender"
    );
}

#[tokio::test]
async fn deciding_one_held_message_never_sweeps_the_senders_other_held_messages() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "one")
        .await
        .unwrap();
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "two")
        .await
        .unwrap();
    let holds = db.list_mail_holds(&ward[..]).await.unwrap();
    assert_eq!(holds.len(), 2, "two distinct messages held");

    // Approve exactly one. The queue is keyed on message id, never on the
    // address, so the sibling stays held even though its sender is now known.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(holds[0].message_id.clone()),
            approve: true,
            ..Default::default()
        }),
    )
    .await
    .unwrap();

    let left = db.list_mail_holds(&ward[..]).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].message_id, holds[1].message_id);
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &holds[1].message_id).await);
}

#[tokio::test]
async fn a_guardian_cannot_decide_a_hold_that_is_not_their_wards() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian_a = mail_user(&st, "parenta", None).await;
    let guardian_b = mail_user(&st, "parentb", None).await;
    let ward_a = mail_user(&st, "kida", Some(guardian_a)).await;
    let ward_b = mail_user(&st, "kidb", Some(guardian_b)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian_a, ward_a, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kida@fauna.test"], "hi")
        .await
        .unwrap();
    let (message_id, _) = only_hold(&db, &ward_a).await;

    // B guards a different ward, so naming A's message id resolves to nothing.
    let err = dispatch(
        &r,
        st.clone(),
        guardian_b,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward_b.to_vec()),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(message_id.clone()),
            approve: true,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a hold is ward-scoped");
    assert_eq!(err.code, "fauna.family.not_found");
    assert!(is_placed(&db, &ward_a, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

// ── the outbound auto-seed: replies to the child's own mail always flow ─

#[tokio::test]
async fn a_reply_to_mail_the_ward_sent_reaches_inbox_not_the_held_mailbox() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let pal = mail_user(&st, "pal", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;

    // The ward mails a stranger first — `fauna.email.send` auto-seeds the
    // ward's known-sender set with every recipient, before the in-domain /
    // remote partition.
    send_mail(&r, &st, ward, "kid", &["pal@fauna.test"], "hello pal")
        .await
        .expect("ward may always send");
    assert!(
        db.is_known_mail_sender(&ward[..], "pal@fauna.test")
            .await
            .unwrap(),
        "outbound seeds the allowlist"
    );

    // ...so the reply is not cold mail. Straight to INBOX, nothing held.
    send_mail(&r, &st, pal, "pal", &["kid@fauna.test"], "Re: hello pal")
        .await
        .expect("reply delivered");

    assert_eq!(inbox_count(&r, &st, ward).await, 1, "reply reaches INBOX");
    assert!(
        db.list_mail_holds(&ward[..]).await.unwrap().is_empty(),
        "a reply to the child's own mail is never held"
    );
}

// ── `reject` — the in-domain twin refuses the SENDER ───────────────────

#[tokio::test]
async fn in_domain_reject_refuses_the_sender_and_delivers_to_no_one() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let adult = mail_user(&st, "adult", None).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "reject").await;

    // A Fauna user mailing both the ward and an adult on the same nest. There
    // is no SMTP stage to refuse per-recipient, so the *sender* is refused —
    // and refused BEFORE anyone is delivered to, since a silent partial
    // delivery would leave the sender believing the ward received it.
    let err = send_mail(
        &r,
        &st,
        stranger,
        "stranger",
        &["kid@fauna.test", "adult@fauna.test"],
        "hi",
    )
    .await
    .expect_err("in-domain twin refuses the sender");
    assert_eq!(err.code, "fauna.email.guardian_approval_required");

    assert_eq!(inbox_count(&r, &st, ward).await, 0);
    assert_eq!(
        inbox_count(&r, &st, adult).await,
        0,
        "no partial delivery — the whole send is refused, naming the recipient"
    );
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
}

#[tokio::test]
async fn reject_never_touches_a_known_sender_or_an_unsupervised_recipient() {
    let (_db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let adult = mail_user(&st, "adult", None).await;
    let pal = mail_user(&st, "pal", None).await;
    set_policy(&r, &st, guardian, ward, "reject").await;

    // The ward mails pal → pal becomes known → pal's reply is accepted.
    send_mail(&r, &st, ward, "kid", &["pal@fauna.test"], "hi")
        .await
        .unwrap();
    send_mail(&r, &st, pal, "pal", &["kid@fauna.test"], "Re: hi")
        .await
        .expect("a known sender is never rejected");
    assert_eq!(inbox_count(&r, &st, ward).await, 1);

    // And an unsupervised recipient is untouched by any ward's policy.
    send_mail(&r, &st, pal, "pal", &["adult@fauna.test"], "hi")
        .await
        .expect("unsupervised recipient");
    assert_eq!(inbox_count(&r, &st, adult).await, 1);
}

// ── graduation releases, never drops ───────────────────────────────────

#[tokio::test]
async fn graduation_releases_held_messages_to_inbox_before_dropping_the_link() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "one")
        .await
        .unwrap();
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "two")
        .await
        .unwrap();
    let held: Vec<Vec<u8>> = db
        .list_mail_holds(&ward[..])
        .await
        .unwrap()
        .into_iter()
        .map(|h| h.message_id)
        .collect();
    assert_eq!(held.len(), 2);

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.graduate",
        encode(&FamilyGraduateRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            extra: Default::default(),
        }),
    )
    .await
    .expect("guardian graduates the ward");

    // "Graduation releases, never drops" — no-user-data-loss is iron-clad.
    for message_id in &held {
        assert!(
            is_placed(&db, &ward, "INBOX", message_id).await,
            "every held message reaches the now-full account's INBOX"
        );
        assert!(!is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, message_id).await);
    }
    assert_eq!(inbox_count(&r, &st, ward).await, 2);
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
    assert!(db.get_guardian_of(&ward[..]).await.unwrap().is_none());
    assert!(db.get_guardian_policy(&ward[..]).await.unwrap().is_none());
}

#[tokio::test]
async fn holds_drain_even_after_the_guardian_relaxes_the_knob() {
    // The knob governs whether NEW mail is held, never whether already-held
    // mail can be released. Flipping back to `allow` with messages still held
    // must not strand them in a mailbox no queue entry can reach.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "one")
        .await
        .unwrap();
    let (message_id, _) = only_hold(&db, &ward).await;

    set_policy(&r, &st, guardian, ward, "allow").await;

    let list: FamilyApprovalsListReply = decode(
        &dispatch(
            &r,
            st.clone(),
            guardian,
            "fauna.family.approvals.list",
            encode(&FamilyApprovalsListRequest::default()),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(list.approvals.len(), 1, "the hold still surfaces");

    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(message_id.clone()),
            approve: true,
            ..Default::default()
        }),
    )
    .await
    .expect("still releasable");
    assert!(is_placed(&db, &ward, "INBOX", &message_id).await);
}

// ── the held mailbox is IMAP-hardened ──────────────────────────────────
//
// A ward's own MUA reaches the held mailbox over the same `fauna.bridges.*`
// RPC surface the MDA translates IMAP into. While a hold is live, relocation
// and destruction are refused; *reading* stays open (the transparency
// contract — the ward always sees that mail is being held). The guardian's
// approve/deny and graduation run server-side in `family_handlers` (direct
// `CacheDb` calls), so the handler-layer gate never touches them — proven by
// the approve/deny/graduation tests above running against the same router.

/// Enroll + approve an MDA bridge service user so `fauna.bridges.*`
/// dispatch passes `require_class`.
async fn mda(db: &CacheDb) -> [u8; 32] {
    let pk = [0xB7u8; 32];
    db.create_pending_bridge_service_user(&pk, BridgeRole::Mda, "test-bridge")
        .await
        .expect("create pending bridge");
    db.upsert_bridge_x25519(&pk, &[0x99u8; 32])
        .await
        .expect("upsert x25519");
    db.approve_bridge_service_user(&pk, None)
        .await
        .expect("approve bridge");
    pk
}

/// The held message's UID in the ward's held mailbox.
async fn held_uid(db: &CacheDb, ward: &[u8; 32], message_id: &[u8]) -> u32 {
    let mid: [u8; 32] = message_id.try_into().expect("32-byte message id");
    db.find_message_uid(ward, GUARDIAN_HELD_MAILBOX, &mid)
        .await
        .unwrap()
        .expect("held message placed")
}

/// One guardian, one ward under `hold`, one cold message held. Returns
/// `(db, state, guardian, ward, message_id, uid, mda_actor)`.
#[allow(clippy::type_complexity)]
async fn held_fixture() -> (
    Arc<CacheDb>,
    Arc<AppState>,
    RpcRouter,
    [u8; 32],
    [u8; 32],
    Vec<u8>,
    u32,
    [u8; 32],
) {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let stranger = mail_user(&st, "stranger", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    send_mail(&r, &st, stranger, "stranger", &["kid@fauna.test"], "held")
        .await
        .unwrap();
    let (message_id, _) = only_hold(&db, &ward).await;
    let uid = held_uid(&db, &ward, &message_id).await;
    let bridge = mda(&db).await;
    (db, st, r, guardian, ward, message_id, uid, bridge)
}

#[tokio::test]
async fn a_wards_mua_cannot_move_a_held_message_out_of_guardian_review() {
    let (db, st, r, _guardian, ward, message_id, uid, bridge) = held_fixture().await;

    let err = dispatch(
        &r,
        st.clone(),
        bridge,
        "fauna.bridges.move",
        encode(&MoveMessagesRequest {
            actor_id: ward.to_vec(),
            source_mailbox: GUARDIAN_HELD_MAILBOX.into(),
            uids: vec![uid],
            dest_mailbox: "INBOX".into(),
        }),
    )
    .await
    .expect_err("MOVE out of the held mailbox is refused while the hold is live");
    assert_eq!(err.code, "fauna.bridges.held_for_review");

    // Nothing relocated, sidecar intact, the guardian's queue still lists it.
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert!(!is_placed(&db, &ward, "INBOX", &message_id).await);
    assert_eq!(db.list_mail_holds(&ward[..]).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_wards_mua_cannot_expunge_a_held_message() {
    let (db, st, r, _guardian, ward, message_id, uid, bridge) = held_fixture().await;

    // The MUA STOREs \Deleted first (a bare expunge only removes flagged
    // rows) — the flag itself is harmless and allowed.
    db.apply_store_flags(
        &ward,
        GUARDIAN_HELD_MAILBOX,
        &[uid],
        StoreFlagsDbOp::Add,
        &["\\Deleted".to_string()],
        None,
    )
    .await
    .unwrap();

    // UID EXPUNGE naming the held UID: skipped, not an error (CLOSE and
    // routine expunges must keep working for the ward's own mail).
    let reply: ExpungeReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.expunge",
            encode(&ExpungeRequest {
                actor_id: ward.to_vec(),
                mailbox: GUARDIAN_HELD_MAILBOX.into(),
                uids: vec![uid],
            }),
        )
        .await
        .expect("expunge itself succeeds"),
    )
    .unwrap();
    assert!(reply.expunged_uids.is_empty(), "the held UID is skipped");

    // Plain EXPUNGE (all \Deleted rows): same.
    let reply: ExpungeReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.expunge",
            encode(&ExpungeRequest {
                actor_id: ward.to_vec(),
                mailbox: GUARDIAN_HELD_MAILBOX.into(),
                uids: vec![],
            }),
        )
        .await
        .expect("plain expunge succeeds"),
    )
    .unwrap();
    assert!(reply.expunged_uids.is_empty(), "the held UID is skipped");

    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert_eq!(db.list_mail_holds(&ward[..]).await.unwrap().len(), 1);
}

#[tokio::test]
async fn guardian_review_is_a_protected_mailbox_name() {
    let (_db, st, r, _guardian, ward, _mid, _uid, bridge) = held_fixture().await;

    let del: DeleteMailboxReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.delete_mailbox",
            encode(&DeleteMailboxRequest {
                actor_id: ward.to_vec(),
                name: GUARDIAN_HELD_MAILBOX.into(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(del, DeleteMailboxReply::Reserved);

    let ren: RenameMailboxReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.rename_mailbox",
            encode(&RenameMailboxRequest {
                actor_id: ward.to_vec(),
                old_name: GUARDIAN_HELD_MAILBOX.into(),
                new_name: "Elsewhere".into(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ren, RenameMailboxReply::ReservedSource);

    let ren: RenameMailboxReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.rename_mailbox",
            encode(&RenameMailboxRequest {
                actor_id: ward.to_vec(),
                old_name: "INBOX".into(),
                new_name: GUARDIAN_HELD_MAILBOX.into(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(ren, RenameMailboxReply::TargetReserved);

    let cre: CreateMailboxReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.create_mailbox",
            encode(&CreateMailboxRequest {
                actor_id: ward.to_vec(),
                name: GUARDIAN_HELD_MAILBOX.into(),
            }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(cre, CreateMailboxReply::Reserved);
}

#[tokio::test]
async fn the_ward_can_still_read_the_held_message() {
    // The transparency contract (§ The trust shape invariant 4): a hold is
    // never silent — the ward sees the held message in their own mailbox.
    let (_db, st, r, _guardian, ward, _mid, uid, bridge) = held_fixture().await;

    let reply: ListMessagesReply = decode(
        &dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.list_messages",
            encode(&ListMessagesRequest {
                actor_id: ward.to_vec(),
                mailbox: GUARDIAN_HELD_MAILBOX.into(),
                since_modseq: None,
                limit: 0,
                after_uid: None,
            }),
        )
        .await
        .expect("reading the held mailbox stays open"),
    )
    .unwrap();
    assert_eq!(reply.messages.len(), 1);
    assert_eq!(reply.messages[0].uid, uid);
}

#[tokio::test]
async fn the_gate_keys_on_the_live_hold_not_the_mailbox() {
    // A non-held message the ward parked in the held mailbox moves back out
    // freely, and once the guardian denies the hold the mailbox is unlocked —
    // the gate is the sidecar row, not the mailbox name.
    let (db, st, r, guardian, ward, message_id, _uid, bridge) = held_fixture().await;

    // Seed a second, NON-held message into the ward's INBOX (allow-listed
    // sender: the ward's own guardian writes from a known address only after
    // approval — simplest is a stranger under a relaxed knob).
    set_policy(&r, &st, guardian, ward, "allow").await;
    let stranger2 = mail_user(&st, "passerby", None).await;
    send_mail(&r, &st, stranger2, "passerby", &["kid@fauna.test"], "own")
        .await
        .unwrap();
    let inbox_uid = {
        let reply: ListMessagesReply = decode(
            &dispatch(
                &r,
                st.clone(),
                bridge,
                "fauna.bridges.list_messages",
                encode(&ListMessagesRequest {
                    actor_id: ward.to_vec(),
                    mailbox: "INBOX".into(),
                    since_modseq: None,
                    limit: 0,
                    after_uid: None,
                }),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reply.messages.len(), 1);
        reply.messages[0].uid
    };

    // Ward parks their own message in the held mailbox (harmless), then
    // moves it back out — no hold row, no refusal.
    for (src, dst, uid) in [
        ("INBOX", GUARDIAN_HELD_MAILBOX, inbox_uid),
        (GUARDIAN_HELD_MAILBOX, "INBOX", 0u32),
    ] {
        let uids = if uid == 0 {
            // Second leg: re-resolve the UID allocated by the first move.
            let reply: ListMessagesReply = decode(
                &dispatch(
                    &r,
                    st.clone(),
                    bridge,
                    "fauna.bridges.list_messages",
                    encode(&ListMessagesRequest {
                        actor_id: ward.to_vec(),
                        mailbox: GUARDIAN_HELD_MAILBOX.into(),
                        since_modseq: None,
                        limit: 0,
                        after_uid: None,
                    }),
                )
                .await
                .unwrap(),
            )
            .unwrap();
            let held = held_uid(&db, &ward, &message_id).await;
            let moved: Vec<u32> = reply
                .messages
                .iter()
                .map(|m| m.uid)
                .filter(|u| *u != held)
                .collect();
            assert_eq!(moved.len(), 1, "the parked message is present");
            moved
        } else {
            vec![uid]
        };
        dispatch(
            &r,
            st.clone(),
            bridge,
            "fauna.bridges.move",
            encode(&MoveMessagesRequest {
                actor_id: ward.to_vec(),
                source_mailbox: src.into(),
                uids,
                dest_mailbox: dst.into(),
            }),
        )
        .await
        .expect("a non-held message moves freely in either direction");
    }

    // Guardian denies the real hold → sidecar gone, message discarded — and
    // the ward's expunge of the now-hold-free mailbox proceeds.
    dispatch(
        &r,
        st.clone(),
        guardian,
        "fauna.family.approvals.decide",
        encode(&FamilyApprovalDecideRequest {
            supervised_actor_id: ByteBuf::from(ward.to_vec()),
            kind: "mail_hold".into(),
            message_id: ByteBuf::from(message_id.clone()),
            approve: false,
            ..Default::default()
        }),
    )
    .await
    .expect("the gate never blocks the guardian's own deny");
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
    assert!(!is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

// ── the null reverse-path (`MAIL FROM:<>`) is gated ────────────────────
//
// An external sender can always claim the SMTP null reverse-path. The gate
// fails open on an empty envelope sender ONLY for a report that names the
// **original Message-ID of a message the ward actually sent** — the MTA
// extracts it from the report's `message/rfc822` / `text/rfc822-headers` part,
// and the nest matches it against the ward's sent-Message-ID set. Everything
// else with an empty sender is held — including under `reject`, which cannot
// fire for `<>` (a per-recipient 550 at RCPT is impossible before DATA reveals
// DSN-ness, and a refusal after DATA could never be bounced to a null path).
//
// The **address** the report claims to bounce is NOT the authorizing fact
// and never was sufficient (the wire no longer carries it — the
// `dsn_recipient` hint left with the compat-remnant sweep): anyone may
// claim to bounce mail addressed to an address the ward has mailed, and the first address a
// supervised child mails is very often their own guardian's public one. That
// was the gap; its regression pin is
// `a_report_naming_an_allowlisted_address_without_a_sent_msgid_is_held`.

/// Enroll + approve an MTA bridge service user (the ingest/resolve kinds
/// require the `mta` role, not `mda`).
async fn mta(db: &CacheDb) -> [u8; 32] {
    let pk = [0xA1u8; 32];
    db.create_pending_bridge_service_user(&pk, BridgeRole::Mta, "test-mta")
        .await
        .expect("create pending mta");
    db.upsert_bridge_x25519(&pk, &[0x98u8; 32])
        .await
        .expect("upsert x25519");
    db.approve_bridge_service_user(&pk, None)
        .await
        .expect("approve mta");
    pk
}

/// Drive `fauna.bridges.ingest_inbound_mail` the way the Go MTA frames it:
/// external inbound, envelope sender `sender_address` (empty = the null
/// reverse-path), plus the facts the MTA extracts from a delivery-status
/// report — the original Message-ID it reports on
/// (`dsn_original_msgid`, the authorizing correlation), and the report's own
/// address-header set (`dsn_report_addresses` — what a one-click reply can be
/// addressed to; a genuine DSN carries `mailer-daemon@…`). `tag` varies the
/// sealed body so message ids never collide across calls.
#[allow(clippy::too_many_arguments)]
async fn ingest_external(
    r: &RpcRouter,
    st: &Arc<AppState>,
    mta_actor: [u8; 32],
    recipient: [u8; 32],
    sender_address: &str,
    dsn_original_msgid: Option<&str>,
    dsn_report_addresses: &[&str],
    tag: &str,
) -> Result<Bytes, RpcError> {
    let body = sealed(format!("sealed-body-{tag}").as_bytes());
    let req = IngestInboundMailRequest {
        dedup_key: "env:v1:fixture".into(),
        envelope_key: "env:v1:fixture".into(),
        actor_id: recipient.to_vec(),
        encrypted_body: body.clone(),
        encrypted_index_hint: sealed(format!("sealed-hint-{tag}").as_bytes()),
        // For a null-path message the MTA fills `sender_domain` from the
        // header `From:` (attacker-chosen); the envelope sender stays empty.
        public_metadata: PublicMailMetadata {
            timestamp: 1_715_000_000,
            ciphertext_size: body.len() as u32,
            sender_domain: "remote.test".into(),
        },
        sender_address: sender_address.to_string(),
        dsn_original_msgid: dsn_original_msgid.map(str::to_string),
        dsn_report_addresses: dsn_report_addresses.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    dispatch(
        r,
        st.clone(),
        mta_actor,
        "fauna.bridges.ingest_inbound_mail",
        encode(&req),
    )
    .await
}

#[tokio::test]
async fn a_null_sender_message_is_held_not_delivered_under_hold() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    ingest_external(&r, &st, mta_actor, ward, "", None, &[], "forged")
        .await
        .expect("a hold is not a refusal — the message is accepted");

    assert_eq!(inbox_count(&r, &st, ward).await, 0, "never the INBOX");
    let (message_id, sender) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert_eq!(sender, "", "the sidecar records the null path truthfully");
}

#[tokio::test]
async fn a_null_sender_message_is_held_not_bounced_under_reject() {
    // `reject` cannot fire for `<>`: RCPT precedes DATA so DSN-ness is
    // undecidable there, and a post-DATA refusal could never be bounced to a
    // null path. Hold is the strictest verdict that loses no mail.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "reject").await;
    let mta_actor = mta(&db).await;

    ingest_external(&r, &st, mta_actor, ward, "", None, &[], "forged-r")
        .await
        .expect("held, not an ingest error — the DATA reply covers everyone");

    assert_eq!(inbox_count(&r, &st, ward).await, 0);
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

#[tokio::test]
async fn a_dsn_reporting_a_message_the_ward_actually_sent_reaches_the_inbox() {
    // The ward mailed x@remote.test with a client-minted Message-ID (the
    // outbound seed records it); the remote MTA's bounce reports on exactly
    // that Message-ID, so it is a genuine bounce of the ward's own mail and
    // flows to INBOX even under `reject`.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "reject").await;
    let mta_actor = mta(&db).await;

    let msgid = minted_msgid(0x51);
    send_mail_with_msgid(&r, &st, ward, "kid", &["x@remote.test"], &msgid)
        .await
        .expect("the ward mails a remote correspondent");

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&msgid),
        &["mailer-daemon@remote.test"],
        "dsn",
    )
    .await
    .expect("a genuine bounce of the ward's own mail is never gated");

    assert_eq!(inbox_count(&r, &st, ward).await, 1, "delivered");
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_report_naming_an_allowlisted_address_without_a_sent_msgid_is_held() {
    //
    //
    // The forgery the address-only correlation could not see. The attacker owns
    // evil.com outright — SPF/DKIM/DMARC all *pass*, nothing is spoofed — and
    // sends a genuine, well-formed `multipart/report` whose human-readable part
    // is arbitrary attacker content and whose delivery-status part names an
    // address the ward has mailed (the ward's own guardian, their school: often
    // public, always guessable). Under the old rule that report authenticated
    // itself by naming a *known address* and landed in the ward's INBOX with the
    // guardian's queue none the wiser.
    //
    // It now needs the original Message-ID of a message the ward actually sent,
    // which the attacker cannot guess. HELD.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    // The ward really has mailed this address — so it really is allowlisted.
    send_mail_with_msgid(
        &r,
        &st,
        ward,
        "kid",
        &["x@remote.test"],
        &minted_msgid(0x52),
    )
    .await
    .expect("the ward mails a remote correspondent");
    assert!(
        db.is_known_mail_sender(&ward[..], "x@remote.test")
            .await
            .unwrap(),
        "precondition: the outbound auto-seed allowlisted the address"
    );

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        // The report names a Message-ID the ward never sent. The attacker
        // knows the address; they cannot know the id.
        Some("<attacker-invented@evil.com>"),
        &["attacker@evil.com"],
        "dsn-forged-correlated",
    )
    .await
    .expect("accepted, held");

    assert_eq!(
        inbox_count(&r, &st, ward).await,
        0,
        "a forged report must never reach the ward's INBOX"
    );
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(
        is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await,
        "the guardian's queue must see the contact"
    );
}

#[tokio::test]
async fn a_report_from_a_bridge_that_carries_no_msgid_is_held() {
    // Fail-closed against a report carrying no msgid (an unparseable or
    // costume report, whose structure the extractor could not parse): no correlation, no delivery.
    // Mail is held, never lost — the guardian releases it.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    send_mail_with_msgid(
        &r,
        &st,
        ward,
        "kid",
        &["x@remote.test"],
        &minted_msgid(0x53),
    )
    .await
    .expect("the ward mails a remote correspondent");

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        None, // a stale bridge omits the field entirely
        &[],
        "dsn-stale-bridge",
    )
    .await
    .expect("accepted, held");

    assert_eq!(inbox_count(&r, &st, ward).await, 0);
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

#[tokio::test]
async fn the_sent_msgid_correlation_is_per_ward() {
    // A Message-ID *another* account sent proves nothing about this ward. Two
    // supervised siblings, one guardian: a report on the sibling's Message-ID
    // must not deliver to this ward.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    let sibling = mail_user(&st, "sib", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    let sibs_msgid = minted_msgid(0x54);
    send_mail_with_msgid(&r, &st, sibling, "sib", &["x@remote.test"], &sibs_msgid)
        .await
        .expect("the sibling mails a remote correspondent");

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&sibs_msgid),
        &["mailer-daemon@remote.test"],
        "dsn-other-ward",
    )
    .await
    .expect("accepted, held");

    assert_eq!(inbox_count(&r, &st, ward).await, 0);
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

#[tokio::test]
async fn rcpt_accepts_the_null_sender_for_a_reject_ward() {
    // The per-recipient 550 must NOT fire for `<>` at RCPT — a remote DSN for
    // the ward's own mail would die there. The gate defers to ingest.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "reject").await;
    let mta_actor = mta(&db).await;

    let reply: ResolveRecipientReply = decode(
        &dispatch(
            &r,
            st.clone(),
            mta_actor,
            "fauna.bridges.resolve_recipient",
            encode(&ResolveRecipientRequest {
                local_part: "kid".into(),
                domain: DOMAIN.into(),
                sender_domain: String::new(),
                sender_address: String::new(),
            }),
        )
        .await
        .expect("resolve ok"),
    )
    .unwrap();
    assert!(
        matches!(reply, ResolveRecipientReply::Resolved { .. }),
        "RCPT must accept the null reverse-path (verdict deferred to ingest), got {reply:?}"
    );
}

#[tokio::test]
async fn a_null_sender_message_to_an_unsupervised_account_is_untouched() {
    let (db, st) = state_with_mail().await;
    let r = router();
    let adult = mail_user(&st, "grownup", None).await;
    let mta_actor = mta(&db).await;

    ingest_external(&r, &st, mta_actor, adult, "", None, &[], "adult")
        .await
        .expect("no policy, no gate");

    assert_eq!(inbox_count(&r, &st, adult).await, 1);
    assert!(db.list_mail_holds(&adult[..]).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_leaked_msgid_is_a_bounded_budget_not_an_open_channel() {
    //
    //
    // A Message-ID leaks beyond the recipient set: RFC 5322 § 3.6.4 threading
    // propagates it in `In-Reply-To:`/`References:` to every later participant
    // of the thread — a reply-all added CC, a forward, a quoted message — who
    // were never mailed by the ward and are exactly the population the gate
    // exists to exclude. The correlation therefore cannot be durable: each
    // sent message carries a small delivery budget (remote recipients + 2 —
    // room for every real per-recipient bounce plus a delayed/failed pair),
    // consumed per correlated delivery. One leaked id is then at most a
    // handful of deliveries, never an unlimited arbitrary-content channel.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    let msgid = minted_msgid(0xB1);
    send_mail_with_msgid(&r, &st, ward, "kid", &["x@remote.test"], &msgid)
        .await
        .expect("the ward mails one remote correspondent");

    // One remote recipient → budget 3. Every correlated report inside it
    // delivers (a delay-DSN and a failure-DSN for the same send are both
    // genuine)…
    for i in 0..3u8 {
        ingest_external(
            &r,
            &st,
            mta_actor,
            ward,
            "",
            Some(&msgid),
            &["mailer-daemon@remote.test"],
            &format!("dsn-budget-{i}"),
        )
        .await
        .expect("within budget: delivered");
    }
    assert_eq!(inbox_count(&r, &st, ward).await, 3, "budget spends open");
    assert!(db.list_mail_holds(&ward[..]).await.unwrap().is_empty());

    // …and the one past it is HELD: the id has done all the bouncing a real
    // send could need, so a further naming is a replay, not a bounce.
    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&msgid),
        &["mailer-daemon@remote.test"],
        "dsn-budget-exhausted",
    )
    .await
    .expect("accepted, held");
    assert_eq!(
        inbox_count(&r, &st, ward).await,
        3,
        "an exhausted correlation must stop delivering"
    );
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(
        is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await,
        "the guardian's queue must see the replay"
    );
}

#[tokio::test]
async fn a_third_party_mua_message_id_never_seeds_the_correlation() {
    // ── PIN: seed only ids a Fauna path minted ──
    //
    // A ward on a third-party MUA gets that MUA's Message-ID entropy, which
    // may be guessable — and "is this id weak?" is not decidable, while "did
    // a Fauna path mint it?" is (both mint paths stamp `<32-lowercase-hex@
    // domain>`). An id outside that shape is never seeded, so a report naming
    // it is HELD and the guardian releases it: such a ward pays a few held
    // bounces, which is exactly the trade the gate exists to make.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    // A perfectly real send — but the id is the MUA's own, not Fauna-minted.
    send_mail_with_msgid(
        &r,
        &st,
        ward,
        "kid",
        &["x@remote.test"],
        "<1699999999.12345@wards-laptop>",
    )
    .await
    .expect("the ward's MUA mails a remote correspondent");

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some("<1699999999.12345@wards-laptop>"),
        &["mailer-daemon@remote.test"],
        "dsn-mua",
    )
    .await
    .expect("accepted, held");

    assert_eq!(
        inbox_count(&r, &st, ward).await,
        0,
        "a non-Fauna-minted id must never correlate"
    );
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

#[tokio::test]
async fn an_in_domain_only_message_seeds_no_correlation() {
    // ── PIN: seed only messages that can actually be bounced ──
    //
    // An in-domain-only message never enters the outbound queue, so no remote
    // MTA can ever legitimately bounce it — its Message-ID would be a
    // correlatable token with zero legitimate use (and in-domain ids are the
    // ones most likely to leak into a shared household thread). A report
    // naming one is HELD.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    // The in-domain recipient must exist for delivery to resolve.
    mail_user(&st, "grownup", None).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    let msgid = minted_msgid(0xC2);
    send_mail_with_msgid(
        &r,
        &st,
        ward,
        "kid",
        &[&format!("grownup@{DOMAIN}")],
        &msgid,
    )
    .await
    .expect("the ward mails an in-domain adult");

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&msgid),
        &["mailer-daemon@remote.test"],
        "dsn-indomain",
    )
    .await
    .expect("accepted, held");

    assert_eq!(
        inbox_count(&r, &st, ward).await,
        0,
        "an unbounceable message's id must never correlate"
    );
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
}

#[tokio::test]
async fn a_reply_to_a_correlated_report_never_allowlists_its_author() {
    //
    //
    // Carol — a later thread participant, never mailed by the ward — learned
    // the ward's sent Message-ID from `References:`. She spends one budget
    // unit to land a well-formed report, whose From:/Cc: she chose, in the
    // ward's INBOX. The outbound auto-seed's premise ("the ward chose to mail
    // them, so replies flow") is false for a reply to that report: *Carol*
    // chose the addresses. So the reply still sends, but it must not
    // bootstrap the allowlist — neither for the report's From: nor for an
    // accomplice she parked in Cc: (what a one-click reply-all addresses).
    // Nothing legitimate is turned away: nobody replies to a genuine
    // MAILER-DAEMON bounce.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    let msgid = minted_msgid(0x61);
    send_mail_with_msgid(&r, &st, ward, "kid", &["x@remote.test"], &msgid)
        .await
        .expect("the ward mails a remote correspondent");

    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&msgid),
        // The report's own address headers — From: Carol, Cc: her accomplice.
        &["carol@evil.test", "accomplice@evil.test"],
        "dsn-carol",
    )
    .await
    .expect("a correlated report is delivered (one budget unit)");
    assert_eq!(inbox_count(&r, &st, ward).await, 1, "it reached the INBOX");

    // The ward hits reply-all.
    send_mail(
        &r,
        &st,
        ward,
        "kid",
        &["carol@evil.test", "accomplice@evil.test"],
        "re: your report",
    )
    .await
    .expect("the reply itself still sends — only the seed is declined");

    for addr in ["carol@evil.test", "accomplice@evil.test"] {
        assert!(
            !db.is_known_mail_sender(&ward[..], addr).await.unwrap(),
            "a reply to a correlation-delivered report must not auto-seed \
             {addr} — one delivery must never buy permanent access"
        );
    }

    // Carol's next message — now a named sender — is held, not delivered.
    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "carol@evil.test",
        None,
        &[],
        "carol-cold",
    )
    .await
    .expect("accepted, held");
    assert_eq!(
        inbox_count(&r, &st, ward).await,
        1,
        "the correlated report bought exactly one delivery, not a channel"
    );
    let (message_id, sender) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);
    assert_eq!(
        sender, "carol@evil.test",
        "and the guardian sees the contact"
    );
}

#[tokio::test]
async fn a_correlated_report_with_no_extractable_addresses_is_held() {
    // ── PIN: the escalation break fails CLOSED, and costs no budget ──
    //
    // A genuine DSN always carries `From: MAILER-DAEMON@…`, so a null-path
    // report whose address headers extract to nothing is either an unparseable
    // report or a costume built to dodge the reply-seed suppression. Either
    // way it is HELD — the same precedent as a missing `dsn_original_msgid`
    // — and the probe never runs, so the id's budget survives for the real
    // bounces that may still arrive.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    let msgid = minted_msgid(0x62);
    send_mail_with_msgid(&r, &st, ward, "kid", &["x@remote.test"], &msgid)
        .await
        .expect("the ward mails one remote correspondent");

    // Budget is 1 remote + 2 = 3. Three address-less correlated reports are
    // all held…
    for i in 0..3u8 {
        ingest_external(
            &r,
            &st,
            mta_actor,
            ward,
            "",
            Some(&msgid),
            &[],
            &format!("dsn-no-addrs-{i}"),
        )
        .await
        .expect("accepted, held");
    }
    assert_eq!(
        inbox_count(&r, &st, ward).await,
        0,
        "no extractable addresses → held, never delivered"
    );

    // …and they burned nothing: the full budget still delivers three genuine
    // bounces afterwards.
    for i in 0..3u8 {
        ingest_external(
            &r,
            &st,
            mta_actor,
            ward,
            "",
            Some(&msgid),
            &["mailer-daemon@remote.test"],
            &format!("dsn-genuine-{i}"),
        )
        .await
        .expect("within budget: delivered");
    }
    assert_eq!(
        inbox_count(&r, &st, ward).await,
        3,
        "the held address-less reports must not have spent the budget"
    );
}

#[tokio::test]
async fn a_correlated_report_with_an_implausible_address_list_is_held() {
    // ── PIN: the address-set cap fails CLOSED ──
    //
    // The recorded set is what the reply-seed suppression matches against, so
    // an attacker stuffing hundreds of Cc: addresses must not bloat it — and a
    // genuine DSN never carries more than a couple of addresses. Over the cap
    // → held, budget unburnt.
    let (db, st) = state_with_mail().await;
    let r = router();
    let guardian = mail_user(&st, "parent", None).await;
    let ward = mail_user(&st, "kid", Some(guardian)).await;
    set_policy(&r, &st, guardian, ward, "hold").await;
    let mta_actor = mta(&db).await;

    let msgid = minted_msgid(0x63);
    send_mail_with_msgid(&r, &st, ward, "kid", &["x@remote.test"], &msgid)
        .await
        .expect("the ward mails one remote correspondent");

    let stuffed: Vec<String> = (0..17).map(|i| format!("cc{i}@evil.test")).collect();
    let stuffed_refs: Vec<&str> = stuffed.iter().map(String::as_str).collect();
    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&msgid),
        &stuffed_refs,
        "dsn-stuffed",
    )
    .await
    .expect("accepted, held");

    assert_eq!(inbox_count(&r, &st, ward).await, 0, "over the cap → held");
    let (message_id, _) = only_hold(&db, &ward).await;
    assert!(is_placed(&db, &ward, GUARDIAN_HELD_MAILBOX, &message_id).await);

    // The cap is a pre-check: the budget must be intact for a genuine bounce.
    ingest_external(
        &r,
        &st,
        mta_actor,
        ward,
        "",
        Some(&msgid),
        &["mailer-daemon@remote.test"],
        "dsn-after-stuffed",
    )
    .await
    .expect("within budget: delivered");
    assert_eq!(inbox_count(&r, &st, ward).await, 1);
}
