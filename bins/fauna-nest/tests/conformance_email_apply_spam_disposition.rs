//! Conformance for `fauna.email.apply_spam_disposition` — the **User-class,
//! caller-scoped** on-device spam-scorer outcome: watermark the scored INBOX
//! UIDs with the internal `$FaunaSpamScored` keyword, then move the spam
//! subset INBOX→Junk. The `User` twin of the MDA's `scoreSelectedInbox`
//! StoreFlags+Move pair (`bins/fauna-bridges/internal/mda/imap/
//! spam_score.go`), over the identical `db.apply_store_flags` / `db.apply_move`
//! machinery the BridgeMda `store_flags` / `move` handlers use.
//!
//! **tier_3** — real seal + real new-path ingest (`fauna.bridges.
//! ingest_inbound_mail`, BridgeMta) into the `__mail/<actor>` segment store +
//! `bridge_imap_messages` INBOX, then the real handler mutating both the flags
//! column (watermark) and the placement (INBOX→Junk move). No stubs.
//!
//! Authority: `docs/goal/behavior/mail-spam.md` § Wire shapes + § Re-file
//! timing ("The Fauna-app score-at-ingest flow") (tracked internally,
//! Part 2 step 2).

mod common;
use common::approve_bridge;
use common::dispatch;
use common::seal_and_ingest;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::wrapped_blob::derive_recipient_hpke_keypair;
use fauna_nest::bridge_method_allowlist::{CallerClass, is_permitted};
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::email::{
    ApplySpamDispositionReply, ApplySpamDispositionRequest, InboxFetchReply, InboxFetchRequest,
    SPAM_SCORED_KEYWORD,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    register_email_handlers(&mut b);
    (b.build(), state)
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
    let payload = Bytes::from(encode_canonical(&req).expect("encode fetch req").to_vec());
    let reply_bytes = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.inbox.fetch",
        payload,
    )
    .await
    .expect("inbox.fetch ok");
    decode(&reply_bytes).expect("decode inbox.fetch reply")
}

async fn apply_disposition(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    scored_uids: Vec<u32>,
    junk_uids: Vec<u32>,
) -> Result<ApplySpamDispositionReply, RpcError> {
    let req = ApplySpamDispositionRequest {
        scored_uids,
        junk_uids,
        extra: Default::default(),
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let reply_bytes = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.apply_spam_disposition",
        payload,
    )
    .await?;
    Ok(decode(&reply_bytes).expect("decode reply"))
}

/// Provision `actor`'s MLS pubkey + return the recipient pubkey for sealing.
async fn provision_actor(state: &Arc<AppState>, actor: [u8; 32], msek: [u8; 32]) -> [u8; 32] {
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&msek);
    common::seed_recipient_seal_key(&state.db, &actor, &msek).await;
    pubkey
}

/// The flag set on `actor`'s message in `mailbox` at `uid` (or panics if
/// absent). Reads the raw `bridge_imap_messages.flags` column via the DB.
async fn flags_of(state: &Arc<AppState>, actor: &[u8; 32], mailbox: &str, uid: u32) -> Vec<String> {
    let rows = state
        .db
        .query_bridge_imap_messages(actor, mailbox, None, None, Some(&[uid]), None)
        .await
        .expect("query messages");
    let row = rows
        .into_iter()
        .find(|r| r.uid == uid)
        .unwrap_or_else(|| panic!("uid {uid} not present in {mailbox}"));
    row.flags.split_whitespace().map(String::from).collect()
}

// ── (a) spam → watermark + move to Junk; ham → watermark only ─────

#[tokio::test]
async fn spam_moves_to_junk_watermarked_ham_stays_watermarked() {
    let (router, state) = router_and_state().await;
    let alice: [u8; 32] = [0x42; 32];
    let alice_pubkey = provision_actor(&state, alice, [0x5e; 32]).await;
    let mta: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x91; 32]).await;

    // Two INBOX messages.
    for i in 0..2u8 {
        let body = format!("To: alice@local.test\r\n\r\nmessage {i}\r\n");
        seal_and_ingest(
            &router,
            &state,
            mta,
            alice,
            &alice_pubkey,
            body.as_bytes(),
            1_715_000_000 + i as i64,
        )
        .await;
    }
    let before = inbox_fetch(&router, &state, alice).await;
    assert_eq!(before.messages.len(), 2, "two INBOX messages");
    let uids: Vec<u32> = before.messages.iter().map(|m| m.uid).collect();
    let (spam_uid, ham_uid) = (uids[0], uids[1]);
    // Neither is watermarked yet.
    assert!(
        before
            .messages
            .iter()
            .all(|m| !m.flags.iter().any(|f| f == SPAM_SCORED_KEYWORD))
    );

    // Score both; classify the first as spam.
    let reply = apply_disposition(
        &router,
        &state,
        alice,
        vec![spam_uid, ham_uid],
        vec![spam_uid],
    )
    .await
    .expect("apply ok");
    assert_eq!(reply.watermarked, 2, "both scored UIDs watermarked");
    assert_eq!(reply.moved_to_junk, 1, "the spam UID moved to Junk");

    // INBOX now holds only the ham message, and it carries the watermark.
    let after = inbox_fetch(&router, &state, alice).await;
    assert_eq!(after.messages.len(), 1, "spam left INBOX");
    let ham = &after.messages[0];
    assert_eq!(ham.uid, ham_uid);
    assert!(
        ham.flags.iter().any(|f| f == SPAM_SCORED_KEYWORD),
        "ham stays in INBOX but is watermarked so it is not re-scored: {:?}",
        ham.flags
    );

    // The spam message is in Junk AND carries the watermark (stamped before
    // the move, so it carries on COPY/MOVE — a later 'not spam' move-back is
    // not re-Junked). Its Junk UID differs from its old INBOX UID.
    let junk = state
        .db
        .query_bridge_imap_messages(&alice, "Junk", None, None, None, None)
        .await
        .expect("query junk");
    assert_eq!(junk.len(), 1, "one message in Junk");
    assert!(
        junk[0]
            .flags
            .split_whitespace()
            .any(|f| f == SPAM_SCORED_KEYWORD),
        "moved spam carries the watermark: {:?}",
        junk[0].flags
    );
}

// ── (b) subset guard: a junk UID not in scored_uids is malformed ──

#[tokio::test]
async fn junk_uid_outside_scored_uids_is_malformed() {
    let (router, state) = router_and_state().await;
    let alice: [u8; 32] = [0x42; 32];
    let alice_pubkey = provision_actor(&state, alice, [0x5e; 32]).await;
    let mta: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x91; 32]).await;
    seal_and_ingest(
        &router,
        &state,
        mta,
        alice,
        &alice_pubkey,
        b"To: alice@local.test\r\n\r\nm\r\n",
        1_715_000_000,
    )
    .await;
    let uid = inbox_fetch(&router, &state, alice).await.messages[0].uid;

    // junk_uid 9999 was never scored → malformed, and nothing is mutated.
    let err = apply_disposition(&router, &state, alice, vec![uid], vec![9999])
        .await
        .expect_err("stray junk uid rejected");
    assert_eq!(err.code, "fauna.protocol.malformed");
    // The real message is untouched (still in INBOX, unwatermarked).
    let flags = flags_of(&state, &alice, "INBOX", uid).await;
    assert!(
        !flags.iter().any(|f| f == SPAM_SCORED_KEYWORD),
        "a rejected request mutates nothing"
    );
}

// ── (c) caller-scoped: another actor's UIDs are a no-op ───────────

#[tokio::test]
async fn disposition_is_caller_scoped() {
    let (router, state) = router_and_state().await;
    let alice: [u8; 32] = [0x42; 32];
    let alice_pubkey = provision_actor(&state, alice, [0x5e; 32]).await;
    let mta: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x91; 32]).await;
    seal_and_ingest(
        &router,
        &state,
        mta,
        alice,
        &alice_pubkey,
        b"To: alice@local.test\r\n\r\nm\r\n",
        1_715_000_000,
    )
    .await;
    let alice_uid = inbox_fetch(&router, &state, alice).await.messages[0].uid;

    // Bob (a different User actor) tries to act on alice's UID. The handler is
    // caller-scoped — it operates only on BOB's own INBOX, where that UID does
    // not exist → a clean no-op (missing UIDs are silently skipped), and
    // alice's mailbox is untouched.
    let bob: [u8; 32] = [0x77; 32];
    let reply = apply_disposition(&router, &state, bob, vec![alice_uid], vec![alice_uid])
        .await
        .expect("apply ok (no-op)");
    assert_eq!(reply.moved_to_junk, 0, "no cross-actor move");

    let alice_after = inbox_fetch(&router, &state, alice).await;
    assert_eq!(alice_after.messages.len(), 1, "alice's INBOX untouched");
    assert!(
        !alice_after.messages[0]
            .flags
            .iter()
            .any(|f| f == SPAM_SCORED_KEYWORD),
        "alice's message was not watermarked by bob's call"
    );
}

// ── (d) idempotent on replay: re-applying finds the UID already moved ─

#[tokio::test]
async fn replay_is_idempotent() {
    let (router, state) = router_and_state().await;
    let alice: [u8; 32] = [0x42; 32];
    let alice_pubkey = provision_actor(&state, alice, [0x5e; 32]).await;
    let mta: [u8; 32] = [0x11; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x91; 32]).await;
    seal_and_ingest(
        &router,
        &state,
        mta,
        alice,
        &alice_pubkey,
        b"To: alice@local.test\r\n\r\nm\r\n",
        1_715_000_000,
    )
    .await;
    let uid = inbox_fetch(&router, &state, alice).await.messages[0].uid;

    let first = apply_disposition(&router, &state, alice, vec![uid], vec![uid])
        .await
        .expect("first apply");
    assert_eq!(first.moved_to_junk, 1);

    // Replay with the same UID: it is already gone from INBOX, so the move is
    // a no-op — no double-move, no error.
    let second = apply_disposition(&router, &state, alice, vec![uid], vec![uid])
        .await
        .expect("replay apply");
    assert_eq!(second.moved_to_junk, 0, "no double-move on replay");
}

// ── (e) allowlist: User + Admin permitted, bridge classes denied ──

#[test]
fn apply_spam_disposition_user_and_admin_permitted_bridges_denied() {
    assert!(is_permitted(
        CallerClass::User,
        "fauna.email.apply_spam_disposition"
    ));
    assert!(is_permitted(
        CallerClass::Admin,
        "fauna.email.apply_spam_disposition"
    ));
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(
            !is_permitted(class, "fauna.email.apply_spam_disposition"),
            "apply_spam_disposition should be denied for {class:?}"
        );
    }
}
