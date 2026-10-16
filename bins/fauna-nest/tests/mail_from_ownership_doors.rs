//! Integration test — the nest holds the `From:` header to its submitter at the
//! two remaining doors that put a user's bytes on the queue it signs
//! (`mail-multidomain.md` § From: header ownership → *Per door, nest-side*):
//! the MTA's vacation auto-reply and the list owner's fan-out. The enqueue
//! door is `mail_from_ownership_enqueue.rs`'s; the forward door is
//! `mail_dkim_oracle.rs` (a forward is never signed).
//!
//! Driven through the production kinds:
//!
//! `fauna.bridges.send_auto_reply` (as an approved MTA, the submitter being the
//! recipient the reply is sent for). The MTA names the reply — the recipient,
//! the delivered-to address, the triggering sender and `Message-ID`, the matched
//! rule — and the nest composes it from the recipient's stored rule:
//! 1. a reply whose From names another local user → refused with the typed
//!    code, nothing spooled, and the recipient's rate-limit slot for that
//!    sender is NOT spent (the next legitimate reply still leaves);
//! 2. a reply from the recipient's handle → spooled;
//! 3. a reply from an alias the recipient owns → spooled;
//! 4. the spooled reply is the stored rule's: its Subject and body, `From:` the
//!    delivered-to address, `To:` the triggering sender, `In-Reply-To` the
//!    triggering `Message-ID`, `Auto-Submitted: auto-replied`, a nest-minted
//!    `Message-ID`;
//! 5. a rule id that is another user's AutoReply, the recipient's own
//!    non-AutoReply rule, or no rule at all → refused as malformed, nothing
//!    spooled and no slot spent.
//!
//! `fauna.bridges.send_list_message` (as the list owner):
//! 4. a From naming another local user's handle → refused with the typed
//!    code, nothing spooled, no list quota reserved;
//! 5. a From naming ANOTHER list's posting address, even the owner's own
//!    other list → refused;
//! 6. a From naming the owner's handle → fans out;
//! 7. a From naming THIS list's posting address (with a display name) → fans
//!    out: the list's address is the one address the resolver attributes to
//!    nobody that its owner may author as;
//! 8. a From on a domain the deployment does not sign for → fans out (out of
//!    the rule's scope; the unit leaves unsigned).
//!
//! Run with: cargo test -p fauna-nest --test mail_from_ownership_doors

mod common;
use common::{approve_bridge, dispatch, register_user};

use std::sync::Arc;

use bytes::Bytes;
use serde_bytes::ByteBuf;

use fauna_nest::bridge_list_handlers::register_bridge_list_handlers;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    SENDER_NOT_OWNED_CODE, SendAutoReplyReply, SendAutoReplyRequest, SendListMessageReply,
    SendListMessageRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

const DOMAIN: &str = "example.test";
const MTA: [u8; 32] = [0x0A; 32];
const ALICE: [u8; 32] = [0x11; 32];
const BOB: [u8; 32] = [0x22; 32];

async fn nest() -> (RpcRouter, Arc<AppState>) {
    let deployment_seed = [0x5Eu8; 32];
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let public = ed25519_dalek::SigningKey::from_bytes(&deployment_seed)
        .verifying_key()
        .to_bytes();
    db.set_nest_keypair(&deployment_seed, &public)
        .await
        .expect("seat the deployment seed");
    db.add_mail_domain(DOMAIN, true, "testing", "none", None, None)
        .await
        .expect("add the mail domain");
    let state = Arc::new(AppState::for_test(db));
    approve_bridge(&state.db, &MTA, BridgeRole::Mta, &[0x99u8; 32]).await;
    register_user(&state, ALICE, "alice").await;
    register_user(&state, BOB, "bob").await;
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    register_bridge_list_handlers(&mut b);
    (b.build(), state)
}

fn message(from: &str, to: &str, tag: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {tag}\r\n\
         Date: Mon, 1 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{tag}@{DOMAIN}>\r\n\r\n\
         body of {tag}\r\n"
    )
    .into_bytes()
}

async fn spooled(state: &Arc<AppState>) -> Vec<Vec<u8>> {
    state
        .db
        .fetch_due_outbound(i64::MAX, 100)
        .await
        .expect("due query")
        .into_iter()
        .map(|r| r.raw_message)
        .collect()
}

// ── the auto-reply door ──────────────────────────────────────────────────────

const AWAY_SUBJECT: &str = "Out of office";
const AWAY_BODY: &str = "I am away until Monday.";

/// Store a filter rule for `owner` whose action is `action` (the on-disk
/// string form), returning its id.
async fn store_rule(state: &Arc<AppState>, owner: [u8; 32], action: &str) -> i64 {
    let no_conditions: Vec<fauna_protocol::email::EmailFilterRule> = Vec::new();
    let rules = encode_canonical(&no_conditions).expect("encode rules");
    state
        .db
        .create_email_filter(&owner, "rule", &rules, "all", action, 0, true, false)
        .await
        .expect("store the rule")
}

/// Store `owner`'s vacation rule, returning its id.
async fn vacation_rule(state: &Arc<AppState>, owner: [u8; 32]) -> i64 {
    store_rule(
        state,
        owner,
        &format!("autoreply:168:{AWAY_SUBJECT}\n{AWAY_BODY}"),
    )
    .await
}

/// The MTA asks the nest to send `recipient`'s vacation reply under `rule_id`,
/// from `delivered_to`, back to the external `sender`.
async fn auto_reply(
    router: &RpcRouter,
    state: &Arc<AppState>,
    recipient: [u8; 32],
    rule_id: i64,
    delivered_to: &str,
    sender: &str,
    tag: &str,
) -> Result<SendAutoReplyReply, RpcError> {
    let req = SendAutoReplyRequest {
        recipient_actor_id: recipient.to_vec(),
        envelope_from: sender.into(),
        from_address: delivered_to.into(),
        in_reply_to: format!("<{tag}@external.test>"),
        rule_id,
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode").to_vec());
    let meta = router
        .kind_meta("fauna.bridges.send_auto_reply")
        .expect("kind registered");
    let reply = (meta.handler)(state.clone(), MTA, payload).await?;
    Ok(decode(&reply).expect("decode reply"))
}

/// The spooled replies, as text.
async fn spooled_text(state: &Arc<AppState>) -> Vec<String> {
    spooled(state)
        .await
        .into_iter()
        .map(|raw| String::from_utf8(raw).expect("utf-8 reply"))
        .collect()
}

/// The value of header `name` in `msg`, if present.
fn header<'a>(msg: &'a str, name: &str) -> Option<&'a str> {
    let head = msg.split("\r\n\r\n").next().unwrap_or_default();
    head.split("\r\n").find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

#[tokio::test]
async fn an_auto_reply_naming_another_local_user_is_refused_and_spends_no_slot() {
    let (router, state) = nest().await;
    let rule = vacation_rule(&state, ALICE).await;
    let sender = "carol@external.test";
    let err = auto_reply(
        &router,
        &state,
        ALICE,
        rule,
        "Bob <bob@example.test>",
        sender,
        "forged-reply",
    )
    .await
    .expect_err("a reply sent for alice may not be from bob");
    assert_eq!(err.code, SENDER_NOT_OWNED_CODE);
    assert!(
        spooled(&state).await.is_empty(),
        "a refused reply never spools"
    );

    // The refusal came before the rate-limit claim: alice's genuine reply to
    // the same sender still leaves.
    let reply = auto_reply(
        &router,
        &state,
        ALICE,
        rule,
        "alice@example.test",
        sender,
        "genuine-reply",
    )
    .await
    .expect("alice's own reply is accepted");
    assert!(reply.sent, "the refused forgery spent no rate-limit slot");
    let out = spooled_text(&state).await;
    assert_eq!(out.len(), 1);
    assert_eq!(header(&out[0], "From"), Some("alice@example.test"));
}

#[tokio::test]
async fn an_auto_reply_from_the_recipients_handle_or_alias_spools() {
    let (router, state) = nest().await;
    let rule = vacation_rule(&state, ALICE).await;
    state
        .db
        .put_exact_alias(DOMAIN, "alice-away", "exact", &ALICE)
        .await
        .expect("alice's alias");

    let reply = auto_reply(
        &router,
        &state,
        ALICE,
        rule,
        "ALICE@Example.Test",
        "d1@external.test",
        "handle",
    )
    .await
    .expect("the handle, case-insensitively");
    assert!(reply.sent);

    let reply = auto_reply(
        &router,
        &state,
        ALICE,
        rule,
        "alice-away@example.test",
        "d2@external.test",
        "alias",
    )
    .await
    .expect("an alias alice owns");
    assert!(reply.sent);

    let froms: Vec<String> = spooled_text(&state)
        .await
        .iter()
        .map(|m| header(m, "From").unwrap_or_default().to_owned())
        .collect();
    assert_eq!(froms, ["ALICE@Example.Test", "alice-away@example.test"]);
}

#[tokio::test]
async fn the_spooled_auto_reply_is_composed_by_the_nest_from_the_stored_rule() {
    let (router, state) = nest().await;
    let rule = vacation_rule(&state, ALICE).await;
    let reply = auto_reply(
        &router,
        &state,
        ALICE,
        rule,
        "alice@example.test",
        "carol@external.test",
        "trigger",
    )
    .await
    .expect("alice's vacation reply");
    assert!(reply.sent);

    let out = spooled_text(&state).await;
    assert_eq!(out.len(), 1);
    let msg = &out[0];
    assert_eq!(header(msg, "From"), Some("alice@example.test"));
    assert_eq!(header(msg, "To"), Some("carol@external.test"));
    assert_eq!(header(msg, "Subject"), Some(AWAY_SUBJECT));
    assert_eq!(header(msg, "In-Reply-To"), Some("<trigger@external.test>"));
    assert_eq!(header(msg, "Auto-Submitted"), Some("auto-replied"));
    let msgid = header(msg, "Message-ID").expect("a nest-minted Message-ID");
    assert!(
        msgid.starts_with('<') && msgid.ends_with("@example.test>"),
        "minted under the reply's own domain: {msgid}"
    );
    assert!(header(msg, "Date").is_some());
    let body = msg.split_once("\r\n\r\n").expect("a body").1;
    assert_eq!(body, format!("{AWAY_BODY}\r\n"));
}

#[tokio::test]
async fn a_rule_id_that_is_not_the_recipients_auto_reply_is_refused() {
    let (router, state) = nest().await;
    let bobs_vacation = vacation_rule(&state, BOB).await;
    let alices_discard = store_rule(&state, ALICE, "discard").await;
    let alices_vacation = vacation_rule(&state, ALICE).await;
    let sender = "carol@external.test";

    for (rule, why) in [
        (bobs_vacation, "another user's AutoReply"),
        (
            alices_discard,
            "the recipient's own rule that is not an AutoReply",
        ),
        (alices_vacation + 1000, "no rule at all"),
    ] {
        let err = auto_reply(
            &router,
            &state,
            ALICE,
            rule,
            "alice@example.test",
            sender,
            "t",
        )
        .await
        .expect_err(why);
        assert_eq!(err.code, "fauna.protocol.malformed", "{why}");
    }
    assert!(spooled(&state).await.is_empty(), "nothing spools");

    // No refusal spent alice's slot for this sender.
    let reply = auto_reply(
        &router,
        &state,
        ALICE,
        alices_vacation,
        "alice@example.test",
        sender,
        "t",
    )
    .await
    .expect("alice's own vacation rule");
    assert!(reply.sent);
}

// ── the list door ────────────────────────────────────────────────────────────

async fn send_list(
    router: &RpcRouter,
    state: &Arc<AppState>,
    owner: [u8; 32],
    list_id: [u8; 16],
    raw: Vec<u8>,
) -> Result<SendListMessageReply, RpcError> {
    let req = SendListMessageRequest {
        list_id: ByteBuf::from(list_id.to_vec()),
        message: ByteBuf::from(raw),
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode").to_vec());
    let reply = dispatch(
        router,
        state.clone(),
        owner,
        "fauna.bridges.send_list_message",
        payload,
    )
    .await?;
    Ok(decode(&reply).expect("decode reply"))
}

/// Alice's list `news@example.test` with one external member.
async fn alices_list(state: &Arc<AppState>, pattern: &str) -> [u8; 16] {
    let (list_id, _alias_id) = state
        .db
        .create_list(&ALICE, DOMAIN, pattern, None, None, None, None, None)
        .await
        .expect("create alice's list");
    state
        .db
        .add_member(&list_id, "member@external.test", &format!("tok-{pattern}"))
        .await
        .expect("one external member");
    list_id
}

async fn account_quota_ceiling(state: &Arc<AppState>) -> u64 {
    state
        .db
        .get_mass_mailing_policy()
        .await
        .expect("read the mass-mailing policy")
        .effective()
        .list_recipients_per_account_per_day_ceiling as u64
}

#[tokio::test]
async fn a_list_send_naming_another_local_user_is_refused_before_any_cap() {
    let (router, state) = nest().await;
    let news = alices_list(&state, "news").await;
    let ceiling = account_quota_ceiling(&state).await;

    let err = send_list(
        &router,
        &state,
        ALICE,
        news,
        message("Bob <bob@example.test>", "news@example.test", "forged"),
    )
    .await
    .expect_err("alice's list may not carry bob as its author");
    assert_eq!(err.code, SENDER_NOT_OWNED_CODE);
    assert!(spooled(&state).await.is_empty(), "nothing fans out");

    // No quota was reserved by the refusal: the genuine send still has the
    // whole day's allowance ahead of it.
    let reply = send_list(
        &router,
        &state,
        ALICE,
        news,
        message("alice@example.test", "news@example.test", "own"),
    )
    .await
    .expect("the owner's handle fans out");
    assert_eq!(reply.queued_count, 1);
    assert_eq!(reply.estimated_quota_remaining, ceiling - 1);
}

#[tokio::test]
async fn a_list_send_naming_another_lists_address_is_refused_even_the_owners_own() {
    let (router, state) = nest().await;
    let news = alices_list(&state, "news").await;
    let _digest = alices_list(&state, "digest").await;

    let err = send_list(
        &router,
        &state,
        ALICE,
        news,
        message("digest@example.test", "news@example.test", "crossed"),
    )
    .await
    .expect_err("a send to news may not be from digest");
    assert_eq!(err.code, SENDER_NOT_OWNED_CODE);
    assert!(spooled(&state).await.is_empty());
}

#[tokio::test]
async fn a_list_send_from_the_lists_own_address_or_a_foreign_domain_fans_out() {
    let (router, state) = nest().await;
    let news = alices_list(&state, "news").await;

    let reply = send_list(
        &router,
        &state,
        ALICE,
        news,
        message(
            "Alice's Weekly <News@Example.Test>",
            "news@example.test",
            "list-addr",
        ),
    )
    .await
    .expect("this list's own posting address, case-insensitively");
    assert_eq!(reply.queued_count, 1);

    let reply = send_list(
        &router,
        &state,
        ALICE,
        news,
        message("alice@elsewhere.test", "news@example.test", "foreign"),
    )
    .await
    .expect("a From the deployment does not sign for is outside the rule");
    assert_eq!(reply.queued_count, 1);

    assert_eq!(spooled(&state).await.len(), 2);
}
