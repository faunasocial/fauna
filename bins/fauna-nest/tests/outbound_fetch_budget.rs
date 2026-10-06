//! Integration test — the outbound-queue staged-envelope pair + reply byte
//! budget (smtp-server.md § Message size limits, the staged-envelope rule;
//! transport.md § Max frame corollary pointer).
//!
//! Two behaviours, both on `outbound_mail_queue`:
//!
//! 1. **Reply paging.** Un-budgeted, `fetch_outbound_due` summed every due row
//!    into one frame: two ~1.2 MiB inline rows assembled an over-frame reply the
//!    transport refuses, and since the rows stay due, every poll rebuilt the
//!    same reply — all outbound mail wedged permanently. The reply pages: close
//!    early on the budget; rows are independent (a due-set poll, not a cursor
//!    walk), so the remainder rides the next poll.
//! 2. **The staged-envelope pair (S9.3).** A body over the inline budget no
//!    longer wedges or permfails: the enqueue leg resolves a `staged_body`
//!    reference back to the plaintext, and `fetch_outbound_due` seals an
//!    over-frame row under a one-shot key, stages the ciphertext on the byte
//!    plane, and serves a reference the worker opens back byte-for-byte. So the
//!    interim inline-budget backstop is retired and a body up to the product
//!    ceiling delivers.
//!
//! Run with: cargo test -p fauna-nest --test outbound_fetch_budget

mod common;
use common::approve_bridge;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::backup::service::BackupService;
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::db::outbound::{InboundVerdictsSnapshot, NewOutbound};
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{
    EnqueueOutboundMailRequest, FetchOutboundDueReply, FetchOutboundDueRequest,
};
use fauna_protocol::{decode_strict as decode, encode_canonical};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    // A real byte plane so the staged-envelope legs can seal/stage/resolve.
    let dir = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, dir.path().to_path_buf(), None).unwrap(),
    );
    std::mem::forget(dir); // outlive the test; a leaked temp dir is fine here
    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    (b.build(), state)
}

fn empty_verdicts() -> InboundVerdictsSnapshot {
    InboundVerdictsSnapshot {
        spf: String::new(),
        dmarc: String::new(),
        dmarc_policy: String::new(),
    }
}

async fn enqueue_row(db: &CacheDb, msgid: &str, raw: &[u8]) -> i64 {
    let ids = db
        .enqueue_outbound(NewOutbound {
            original_msgid: msgid,
            original_sender: "alice@example.com",
            recipients: &["bob@external.test"],
            raw_message: raw,
            inbound_verdicts: empty_verdicts(),
            is_forwarded: false,
            forward_actor_id: None,
            forward_rule_id: None,
            forward_copy_mode: None,
            submit_actor_id: None,
        })
        .await
        .expect("enqueue");
    assert_eq!(ids.len(), 1);
    ids[0]
}

async fn fetch_due(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
) -> FetchOutboundDueReply {
    let payload = Bytes::from(
        encode_canonical(&FetchOutboundDueRequest {
            max: 10,
            lease_seconds: 60,
        })
        .expect("encode req")
        .to_vec(),
    );
    let meta = router
        .kind_meta("fauna.bridges.fetch_outbound_due")
        .expect("kind registered");
    let reply_bytes = (meta.handler)(state.clone(), caller, payload)
        .await
        .expect("fetch_outbound_due ok");
    decode(&reply_bytes).expect("decode reply")
}

/// The reply pages on the summed byte budget, not the row count: three
/// 900 KB rows fit `max = 10` but only two fit one frame — the first poll
/// carries exactly two, the remainder rides the next poll, and nothing is
/// skipped or lost.
#[tokio::test]
async fn fetch_outbound_due_pages_on_the_reply_byte_budget() {
    let (router, state) = router_and_state().await;
    let mta: [u8; 32] = [0x0A; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;

    let raw = vec![b'A'; 900_000];
    let id1 = enqueue_row(&state.db, "<m1@example>", &raw).await;
    let id2 = enqueue_row(&state.db, "<m2@example>", &raw).await;
    let id3 = enqueue_row(&state.db, "<m3@example>", &raw).await;

    let page1 = fetch_due(&router, &state, mta).await;
    assert_eq!(
        page1.units.len(),
        2,
        "two 900 KB rows fit the frame budget; the third must NOT be summed in \
         (an un-budgeted reply would carry all 3 and overrun the 2 MiB frame)"
    );
    assert_eq!(page1.units[0].id, id1);
    assert_eq!(page1.units[1].id, id2);

    // The page closed early — nothing was skipped: the remainder is served on
    // the next poll once the first page's rows are marked.
    state.db.mark_outbound_sent(id1).await.expect("mark sent");
    state.db.mark_outbound_sent(id2).await.expect("mark sent");
    let page2 = fetch_due(&router, &state, mta).await;
    assert_eq!(page2.units.len(), 1, "the third row rides the next poll");
    assert_eq!(page2.units[0].id, id3);
}

/// S9.3 downward leg: a row over the inline reply budget is sealed under a
/// one-shot key, staged on the byte plane, and served as a reference — it
/// delivers by staging rather than permfailing or wedging the poll, and the
/// reference resolves back byte-for-byte.
#[tokio::test]
async fn a_row_over_the_inline_budget_is_staged_and_delivered() {
    let (router, state) = router_and_state().await;
    let mta: [u8; 32] = [0x0B; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;

    let big = vec![b'Z'; 2_500_000]; // over the inline budget, under 50 MB
    let big_id = enqueue_row(&state.db, "<big@example>", &big).await;
    let small_id = enqueue_row(&state.db, "<small@example>", b"tiny body").await;

    let page = fetch_due(&router, &state, mta).await;

    // The over-frame row is delivered by reference, not dropped or permfailed.
    let big_unit = page
        .units
        .iter()
        .find(|u| u.id == big_id)
        .expect("the over-frame row must be delivered by reference");
    assert!(
        big_unit.raw_message.is_empty(),
        "a staged unit carries no inline body"
    );
    let sref = big_unit
        .staged_body
        .clone()
        .expect("the over-frame row must carry a staged reference");
    let recovered = fauna_nest::mail_body_plane::resolve_staged_body(&state, &sref, u64::MAX)
        .await
        .expect("the staged reference resolves");
    assert_eq!(recovered, big, "the staged body round-trips byte-for-byte");

    // The row behind it rides inline in the same page.
    let small_unit = page
        .units
        .iter()
        .find(|u| u.id == small_id)
        .expect("the small row must still be served this poll");
    assert!(small_unit.staged_body.is_none());
    assert_eq!(small_unit.raw_message, b"tiny body");

    // A staged row stays due until delivered — it is NOT terminally failed.
    let due = state
        .db
        .fetch_due_outbound(i64::MAX, 10)
        .await
        .expect("due query");
    assert!(
        due.iter().any(|r| r.id == big_id),
        "a staged row stays due until delivered, never permfailed"
    );
}

/// S9.3 upward leg: the interim inline-budget backstop is retired — a body over
/// the inline request budget enters the queue via a `staged_body` reference,
/// which nest resolves back to the plaintext and enqueues exactly as an inline
/// submission.
#[tokio::test]
async fn enqueue_over_the_inline_budget_via_staged_body_is_accepted() {
    let (router, state) = router_and_state().await;
    let mta: [u8; 32] = [0x0C; 32];
    approve_bridge(&state.db, &mta, BridgeRole::Mta, &[0x99u8; 32]).await;

    // Stand in for the bridge's stage step: seal + stage the plaintext on nest's
    // own blob store (the enqueue resolver reads from the same store).
    let plaintext = vec![b'Q'; 2_500_000]; // over the inline budget, under 50 MB
    let sref = fauna_nest::mail_body_plane::stage_staged_body(&state, &plaintext)
        .await
        .expect("stage the plaintext body");

    let req = EnqueueOutboundMailRequest {
        original_msgid: "big@example".into(),
        original_sender: "alice@example.com".into(),
        recipients: vec!["bob@external.test".into()],
        raw_message: vec![], // empty — the body rides the staged reference
        staged_body: Some(sref),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).expect("encode req").to_vec());
    let meta = router
        .kind_meta("fauna.bridges.enqueue_outbound_mail")
        .expect("kind registered");
    (meta.handler)(state.clone(), mta, payload)
        .await
        .expect("a staged over-inline-budget enqueue must be accepted");

    // Exactly one outbound row for the one external recipient, carrying the
    // resolved plaintext body verbatim.
    let due = state
        .db
        .fetch_due_outbound(i64::MAX, 10)
        .await
        .expect("due query");
    assert_eq!(due.len(), 1, "one row for the one external recipient");
    assert_eq!(
        due[0].raw_message, plaintext,
        "the resolved staged body is stored verbatim"
    );

    // Exactly-one-of: sending both an inline body and a staged reference is
    // rejected.
    let sref2 = fauna_nest::mail_body_plane::stage_staged_body(&state, b"x")
        .await
        .expect("stage");
    let both = EnqueueOutboundMailRequest {
        original_msgid: "both@example".into(),
        original_sender: "alice@example.com".into(),
        recipients: vec!["bob@external.test".into()],
        raw_message: b"inline".to_vec(),
        staged_body: Some(sref2),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&both).expect("encode").to_vec());
    let err = (meta.handler)(state.clone(), mta, payload)
        .await
        .expect_err("both inline body and staged_body must be rejected");
    assert!(
        format!("{err:?}").contains("must be empty when staged_body is set"),
        "got {err:?}"
    );
}
