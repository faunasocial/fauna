//! Conformance for the app-facing mail read-state surface
//! (`docs/goal/behavior/mail-app-surface.md` § Read state):
//!
//! - `fauna.email.inbox.mark_seen` adds `\Seen` to the caller's own `INBOX`
//!   UIDs and does nothing else — no other flag, no removal, no other mailbox,
//!   no other actor — through the same flag-write internals as
//!   `fauna.bridges.store_flags(op = add)`: the row's modseq and the mailbox's
//!   bump, the `StoreFlags` placement record is journaled.
//! - `fauna.email.inbox.flag_changes` returns exactly the `INBOX` rows past its
//!   `(since_modseq, after_uid)` cursor, each with its whole flag set, and
//!   pages inside one flag write without losing or repeating a row.
//! - `fauna.email.inbox.fetch` carries the mailbox `highest_modseq` baseline.
//! - `fauna.mail.flags_changed` wakes the actor's own sessions on an `INBOX`
//!   flag write, whether a mail client made it (the MDA's `store_flags`) or
//!   another Fauna device did (`mark_seen`) — and not on a no-op.
//!
//! **tier_3** — real seal + real new-path ingest into the `__mail/<actor>`
//! segment store and `bridge_imap_messages`, the real handlers, the real WS
//! push fan-out. Pushes are read with `try_recv` after the handler returned:
//! `notify_push` enqueues synchronously, so no wall-clock wait is involved.

mod common;
use common::{approve_bridge, dispatch, seal_and_ingest};

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::wrapped_blob::derive_recipient_hpke_keypair;
use fauna_nest::bridge_imap_handlers::register_bridge_imap_handlers;
use fauna_nest::bridge_method_allowlist::{CallerClass, is_permitted};
use fauna_nest::bridge_routing_handlers::register_bridge_routing_handlers;
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::bridge_routing::{StoreFlagsOp, StoreFlagsRequest};
use fauna_protocol::email::{
    ApplySpamDispositionRequest, FlagChangesReply, FlagChangesRequest, InboxFetchReply,
    InboxFetchRequest, MarkSeenReply, MarkSeenRequest,
};
use fauna_protocol::{
    Frame, PushEvent, RpcError, decode_frame, decode_strict as decode, encode_canonical,
};
use tokio::sync::mpsc;

const SEEN: &str = "\\Seen";

const ALICE: [u8; 32] = [0x42; 32];
const BOB: [u8; 32] = [0x77; 32];
const MTA: [u8; 32] = [0x11; 32];
const MDA: [u8; 32] = [0x12; 32];

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_routing_handlers(&mut b);
    register_bridge_imap_handlers(&mut b);
    register_email_handlers(&mut b);
    (b.build(), state)
}

fn payload<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).expect("encode req").to_vec())
}

/// Alice with `n` INBOX messages, delivered through the real MTA ingest.
/// Returns her INBOX UIDs in order.
async fn alice_with_inbox(router: &RpcRouter, state: &Arc<AppState>, n: u8) -> Vec<u32> {
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5e; 32]);
    common::seed_recipient_seal_key(&state.db, &ALICE, &[0x5e; 32]).await;
    approve_bridge(&state.db, &MTA, BridgeRole::Mta, &[0x91; 32]).await;
    for i in 0..n {
        let body = format!("To: alice@local.test\r\n\r\nmessage {i}\r\n");
        seal_and_ingest(
            router,
            state,
            MTA,
            ALICE,
            &pubkey,
            body.as_bytes(),
            1_715_000_000 + i as i64,
        )
        .await;
    }
    let uids: Vec<u32> = inbox_fetch(router, state, ALICE)
        .await
        .messages
        .iter()
        .map(|m| m.uid)
        .collect();
    assert_eq!(uids.len(), n as usize, "fixture: {n} INBOX messages");
    uids
}

async fn inbox_fetch(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
) -> InboxFetchReply {
    let bytes = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.inbox.fetch",
        payload(&InboxFetchRequest::default()),
    )
    .await
    .expect("inbox.fetch ok");
    decode(&bytes).expect("decode inbox.fetch reply")
}

async fn mark_seen(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    uids: Vec<u32>,
) -> Result<MarkSeenReply, RpcError> {
    let bytes = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.inbox.mark_seen",
        payload(&MarkSeenRequest {
            uids,
            extra: Default::default(),
        }),
    )
    .await?;
    Ok(decode(&bytes).expect("decode mark_seen reply"))
}

async fn flag_changes(
    router: &RpcRouter,
    state: &Arc<AppState>,
    caller: [u8; 32],
    since_modseq: u64,
    after_uid: u32,
    limit: u32,
) -> FlagChangesReply {
    let bytes = dispatch(
        router,
        state.clone(),
        caller,
        "fauna.email.inbox.flag_changes",
        payload(&FlagChangesRequest {
            since_modseq,
            limit,
            after_uid,
            extra: Default::default(),
        }),
    )
    .await
    .expect("flag_changes ok");
    decode(&bytes).expect("decode flag_changes reply")
}

/// `(flags, modseq)` of `actor`'s row at `(mailbox, uid)`, straight off the
/// placement table.
async fn row(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    mailbox: &str,
    uid: u32,
) -> (BTreeSet<String>, i64) {
    let rows = state
        .db
        .query_bridge_imap_messages(actor, mailbox, None, None, Some(&[uid]), None)
        .await
        .expect("query messages");
    let r = rows
        .into_iter()
        .find(|r| r.uid == uid)
        .unwrap_or_else(|| panic!("uid {uid} not present in {mailbox}"));
    (
        r.flags.split_whitespace().map(String::from).collect(),
        r.modseq,
    )
}

/// Every `fauna.mail.flags_changed` push already queued on `rx`, other kinds
/// skipped (ingest co-emits `fauna.mail.received` / `fauna.segments.changed`).
fn drain_flags_changed(rx: &mut mpsc::Receiver<Bytes>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(bytes) = rx.try_recv() {
        let Frame::Push(push) = decode_frame(&bytes).expect("decode frame") else {
            panic!("expected a Push frame");
        };
        if let PushEvent::MailFlagsChanged(p) = PushEvent::from_push(&push.kind, push.payload) {
            out.push(p.actor_id);
        }
    }
    out
}

// ── mark_seen ──────────────────────────────────────────────────────

#[tokio::test]
async fn mark_seen_adds_seen_and_nothing_else_bumps_modseq_and_journals() {
    let (router, state) = router_and_state().await;
    let uids = alice_with_inbox(&router, &state, 2).await;
    let (target, other) = (uids[0], uids[1]);
    let baseline = inbox_fetch(&router, &state, ALICE).await.highest_modseq;
    assert!(
        baseline > 0,
        "inbox.fetch carries the mailbox HIGHESTMODSEQ"
    );
    let (before, before_modseq) = row(&state, &ALICE, "INBOX", target).await;
    let (other_before, other_modseq) = row(&state, &ALICE, "INBOX", other).await;
    assert!(!before.contains(SEEN), "fixture: arrives unread");

    // An unknown UID rides along and is skipped without error.
    let reply = mark_seen(&router, &state, ALICE, vec![target, 9_999])
        .await
        .expect("mark_seen ok");
    assert_eq!(
        reply.updated, 1,
        "exactly the one real, unread row gained \\Seen"
    );

    let (after, after_modseq) = row(&state, &ALICE, "INBOX", target).await;
    let mut expected = before.clone();
    expected.insert(SEEN.to_string());
    assert_eq!(after, expected, "\\Seen added, no other flag touched");
    assert!(after_modseq > before_modseq, "the row's modseq bumped");
    assert_eq!(
        row(&state, &ALICE, "INBOX", other).await,
        (other_before, other_modseq),
        "an unnamed row is untouched"
    );
    assert!(
        inbox_fetch(&router, &state, ALICE).await.highest_modseq >= after_modseq as u64,
        "the mailbox HIGHESTMODSEQ advanced with the row"
    );

    // The placement journal (the authoritative rebuild source) carries it.
    let manifest = state
        .mail_placement
        .current_manifest(&ALICE)
        .await
        .expect("placement manifest");
    let placed = manifest
        .placements
        .iter()
        .find(|p| p.mailbox == "INBOX" && p.uid == target)
        .expect("journaled placement for the marked row");
    assert!(
        placed.flags.iter().any(|f| f == SEEN),
        "the StoreFlags record was journaled: {:?}",
        placed.flags
    );
}

#[tokio::test]
async fn mark_seen_repeat_is_a_no_op() {
    let (router, state) = router_and_state().await;
    let uid = alice_with_inbox(&router, &state, 1).await[0];
    assert_eq!(
        mark_seen(&router, &state, ALICE, vec![uid])
            .await
            .unwrap()
            .updated,
        1
    );
    let settled = row(&state, &ALICE, "INBOX", uid).await;
    let hms = inbox_fetch(&router, &state, ALICE).await.highest_modseq;

    let again = mark_seen(&router, &state, ALICE, vec![uid])
        .await
        .expect("replay ok");
    assert_eq!(again.updated, 0, "an already-seen row is not counted");
    assert_eq!(row(&state, &ALICE, "INBOX", uid).await, settled, "no write");
    assert_eq!(
        inbox_fetch(&router, &state, ALICE).await.highest_modseq,
        hms,
        "a no-op bumps no modseq"
    );
}

#[tokio::test]
async fn mark_seen_never_removes_a_flag() {
    let (router, state) = router_and_state().await;
    let uid = alice_with_inbox(&router, &state, 1).await[0];
    // A mail client flagged it; marking it read must keep that flag.
    approve_bridge(&state.db, &MDA, BridgeRole::Mda, &[0x92; 32]).await;
    dispatch(
        &router,
        state.clone(),
        MDA,
        "fauna.bridges.store_flags",
        payload(&StoreFlagsRequest {
            actor_id: ALICE.to_vec(),
            mailbox: "INBOX".into(),
            uids: vec![uid],
            op: StoreFlagsOp::Add,
            flags: vec!["\\Flagged".into()],
            unchanged_since: None,
        }),
    )
    .await
    .expect("MDA store_flags ok");

    mark_seen(&router, &state, ALICE, vec![uid])
        .await
        .expect("mark_seen ok");
    let (flags, _) = row(&state, &ALICE, "INBOX", uid).await;
    assert!(flags.contains("\\Flagged"), "no removal: {flags:?}");
    assert!(flags.contains(SEEN), "{flags:?}");
}

#[tokio::test]
async fn mark_seen_touches_no_other_mailbox() {
    let (router, state) = router_and_state().await;
    let uid = alice_with_inbox(&router, &state, 1).await[0];
    // File it to Junk through the app's own spam door; INBOX is now empty.
    dispatch(
        &router,
        state.clone(),
        ALICE,
        "fauna.email.apply_spam_disposition",
        payload(&ApplySpamDispositionRequest {
            scored_uids: vec![uid],
            junk_uids: vec![uid],
            extra: Default::default(),
        }),
    )
    .await
    .expect("disposition ok");
    let junk = state
        .db
        .query_bridge_imap_messages(&ALICE, "Junk", None, None, None, None)
        .await
        .expect("query junk");
    let junk_uid = junk[0].uid;

    let reply = mark_seen(&router, &state, ALICE, vec![junk_uid])
        .await
        .expect("ok");
    assert_eq!(reply.updated, 0, "a Junk UID is not an INBOX UID");
    let (flags, _) = row(&state, &ALICE, "Junk", junk_uid).await;
    assert!(
        !flags.contains(SEEN),
        "the Junk row stays unread: {flags:?}"
    );
}

#[tokio::test]
async fn mark_seen_is_caller_scoped() {
    let (router, state) = router_and_state().await;
    let uid = alice_with_inbox(&router, &state, 1).await[0];

    let reply = mark_seen(&router, &state, BOB, vec![uid])
        .await
        .expect("ok (no-op)");
    assert_eq!(reply.updated, 0, "bob has no INBOX row at alice's UID");
    let (flags, _) = row(&state, &ALICE, "INBOX", uid).await;
    assert!(!flags.contains(SEEN), "alice's row untouched by bob's call");
}

// ── flag_changes ───────────────────────────────────────────────────

#[tokio::test]
async fn flag_changes_returns_exactly_the_rows_past_the_cursor_with_whole_flag_sets() {
    let (router, state) = router_and_state().await;
    let uids = alice_with_inbox(&router, &state, 2).await;
    let baseline = inbox_fetch(&router, &state, ALICE).await.highest_modseq;

    // Nothing changed since the baseline.
    let quiet = flag_changes(&router, &state, ALICE, baseline, 0, 0).await;
    assert!(quiet.changes.is_empty(), "{:?}", quiet.changes);
    assert!(!quiet.more);
    assert_eq!(quiet.highest_modseq, baseline);

    mark_seen(&router, &state, ALICE, vec![uids[1]])
        .await
        .unwrap();
    let delta = flag_changes(&router, &state, ALICE, baseline, 0, 0).await;
    assert_eq!(
        delta.changes.len(),
        1,
        "only the marked row: {:?}",
        delta.changes
    );
    let change = &delta.changes[0];
    assert_eq!(change.uid, uids[1]);
    let (flags, modseq) = row(&state, &ALICE, "INBOX", uids[1]).await;
    assert_eq!(
        change.flags.iter().cloned().collect::<BTreeSet<_>>(),
        flags,
        "the row's whole current flag set, not an edit"
    );
    assert_eq!(change.modseq, modseq as u64);
    assert!(!delta.more);
    assert_eq!(delta.highest_modseq, modseq as u64);

    // Advancing the cursor to the reply's highest_modseq drains it.
    let drained = flag_changes(&router, &state, ALICE, delta.highest_modseq, 0, 0).await;
    assert!(drained.changes.is_empty());
}

#[tokio::test]
async fn flag_changes_pages_inside_one_write_without_loss_or_repeat() {
    let (router, state) = router_and_state().await;
    let uids = alice_with_inbox(&router, &state, 3).await;
    let baseline = inbox_fetch(&router, &state, ALICE).await.highest_modseq;
    // One write → one shared modseq across all three rows.
    assert_eq!(
        mark_seen(&router, &state, ALICE, uids.clone())
            .await
            .unwrap()
            .updated,
        3
    );

    let first = flag_changes(&router, &state, ALICE, baseline, 0, 2).await;
    assert_eq!(first.changes.len(), 2);
    assert!(first.more, "a row remains past the page");
    let last = first.changes.last().unwrap();
    let second = flag_changes(&router, &state, ALICE, last.modseq, last.uid, 2).await;
    assert!(!second.more);

    let seen: Vec<u32> = first
        .changes
        .iter()
        .chain(second.changes.iter())
        .map(|c| c.uid)
        .collect();
    assert_eq!(seen, uids, "every row exactly once, in (modseq, uid) order");
}

#[tokio::test]
async fn flag_changes_is_caller_scoped() {
    let (router, state) = router_and_state().await;
    let uids = alice_with_inbox(&router, &state, 1).await;
    mark_seen(&router, &state, ALICE, uids).await.unwrap();
    let bobs = flag_changes(&router, &state, BOB, 0, 0, 0).await;
    assert!(bobs.changes.is_empty(), "bob sees none of alice's rows");
}

// ── fauna.mail.flags_changed ───────────────────────────────────────

#[tokio::test]
async fn flags_changed_wakes_the_actor_on_an_app_write_and_not_on_a_no_op() {
    let (router, state) = router_and_state().await;
    let uid = alice_with_inbox(&router, &state, 1).await[0];
    let (_conn, mut rx) = state.ws.subscribe(ALICE);
    let (_bob_conn, mut bob_rx) = state.ws.subscribe(BOB);

    mark_seen(&router, &state, ALICE, vec![uid]).await.unwrap();
    assert_eq!(drain_flags_changed(&mut rx), vec![hex::encode(ALICE)]);
    assert!(
        drain_flags_changed(&mut bob_rx).is_empty(),
        "own sessions only"
    );

    mark_seen(&router, &state, ALICE, vec![uid]).await.unwrap();
    assert!(
        drain_flags_changed(&mut rx).is_empty(),
        "a no-op wakes nobody"
    );
}

#[tokio::test]
async fn flags_changed_wakes_the_actor_on_a_mail_client_write_to_inbox_only() {
    let (router, state) = router_and_state().await;
    let uid = alice_with_inbox(&router, &state, 1).await[0];
    approve_bridge(&state.db, &MDA, BridgeRole::Mda, &[0x92; 32]).await;
    let (_conn, mut rx) = state.ws.subscribe(ALICE);

    let store = |mailbox: &str, uid: u32| StoreFlagsRequest {
        actor_id: ALICE.to_vec(),
        mailbox: mailbox.into(),
        uids: vec![uid],
        op: StoreFlagsOp::Add,
        flags: vec![SEEN.into()],
        unchanged_since: None,
    };
    dispatch(
        &router,
        state.clone(),
        MDA,
        "fauna.bridges.store_flags",
        payload(&store("INBOX", uid)),
    )
    .await
    .expect("MDA store_flags ok");
    assert_eq!(drain_flags_changed(&mut rx), vec![hex::encode(ALICE)]);

    // The app half syncs INBOX only; a flag write elsewhere is not its wake.
    dispatch(
        &router,
        state.clone(),
        ALICE,
        "fauna.email.apply_spam_disposition",
        payload(&ApplySpamDispositionRequest {
            scored_uids: vec![uid],
            junk_uids: vec![uid],
            extra: Default::default(),
        }),
    )
    .await
    .expect("disposition ok");
    let junk_uid = state
        .db
        .query_bridge_imap_messages(&ALICE, "Junk", None, None, None, None)
        .await
        .expect("query junk")[0]
        .uid;
    let _ = drain_flags_changed(&mut rx);
    let mut flag = store("Junk", junk_uid);
    flag.flags = vec!["\\Flagged".into()];
    dispatch(
        &router,
        state.clone(),
        MDA,
        "fauna.bridges.store_flags",
        payload(&flag),
    )
    .await
    .expect("MDA store_flags on Junk ok");
    assert!(
        drain_flags_changed(&mut rx).is_empty(),
        "a Junk write is not an INBOX change"
    );
}

// ── allowlist ──────────────────────────────────────────────────────

#[test]
fn read_state_kinds_are_user_and_admin_only() {
    for kind in [
        "fauna.email.inbox.mark_seen",
        "fauna.email.inbox.flag_changes",
    ] {
        assert!(is_permitted(CallerClass::User, kind), "{kind}");
        assert!(is_permitted(CallerClass::Admin, kind), "{kind}");
        for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
            assert!(
                !is_permitted(class, kind),
                "{kind} must be denied for {class:?}"
            );
        }
    }
}
