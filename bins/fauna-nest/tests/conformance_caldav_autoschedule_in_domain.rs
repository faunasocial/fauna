//! End-to-end conformance for the **in-domain local-delivery short-circuit**
//! in `fauna.bridges.enqueue_outbound_mail` (smtp-server.md § Outbound delivery
//! — local short-circuit).
//!
//! The MDA `calendar-auto-schedule` gateway fans an iMIP out by handing ALL
//! email-reachable attendees to `enqueue_outbound_mail` — including attendees on
//! the deployment's OWN mail domain (the canonical case: an organizer invites
//! its own sub-address so the live auto-schedule proof can observe the iMIP).
//! Without the short-circuit those in-domain recipients are MX-relayed back to
//! the box and, on a containerized deploy, 554-rejected at the inbound
//! HELO-identity gate (the in-domain self-loop bounce). With it, an in-domain
//! recipient that resolves to a local mailbox is sealed + delivered locally,
//! never enqueued for relay.
//!
//! Exercises the real superset resolver (`resolve_recipient_inner` —
//! sub-addressing-aware), the real seal (`fauna_mls::wrapped_blob::
//! seal_to_recipient`), the shared sealed-ingest core
//! (`seal_and_ingest_to_local_actor` → `__mail/<actor>` segment store +
//! `bridge_imap_messages` INBOX), and the real `fauna.email.inbox.fetch`
//! read-back + client-side unseal. No stubs, no plaintext path.
//!
//! Authority: `smtp-server.md` § Outbound delivery (local short-circuit) +
//! § Inbound client receive; `caldav-server.md` § Server-side auto-schedule
//! (the in-domain attendee case).

mod common;
use common::dispatch;
use common::open_only_inbox_message;
use common::provision_recipient;

use std::sync::Arc;

use bytes::Bytes;
use fauna_nest::{
    bridge_routing_handlers, db::CacheDb, email_handlers::register_email_handlers,
    routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::bridge_routing::{EnqueueOutboundMailReply, EnqueueOutboundMailRequest};
use fauna_protocol::{decode_strict as decode, encode_canonical};

use common::TEST_DOMAIN as DOMAIN;

/// Router with both the bridge-routing handlers (`enqueue_outbound_mail`) and
/// the email handlers (`inbox.fetch`), plus the deployment domain set so the
/// enqueue handler partitions same-domain recipients into local delivery.
async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    db.add_mail_domain(DOMAIN, true, "testing", "self_signed", None, None)
        .await
        .unwrap();
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    bridge_routing_handlers::register_bridge_routing_handlers(&mut b);
    register_email_handlers(&mut b);
    (b.build(), state)
}

/// Drive the MDA gateway's enqueue (caller-scoped to `organizer`).
async fn enqueue_as_mda(
    router: &RpcRouter,
    state: &Arc<AppState>,
    mda: [u8; 32],
    organizer: [u8; 32],
    original_sender: &str,
    recipients: Vec<String>,
    raw: Vec<u8>,
) -> EnqueueOutboundMailReply {
    let req = EnqueueOutboundMailRequest {
        original_msgid: "evt-001@fauna.test".into(),
        original_sender: original_sender.into(),
        recipients,
        raw_message: raw,
        on_behalf_of_actor: Some(organizer.to_vec()),
        ..Default::default()
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(
        router,
        state.clone(),
        mda,
        "fauna.bridges.enqueue_outbound_mail",
        payload,
    )
    .await
    .expect("enqueue_outbound_mail ok");
    decode(&reply).expect("decode enqueue reply")
}

fn imip_request(from: &str, to: &str, summary: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: Invitation: {summary}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/calendar; method=REQUEST; charset=UTF-8\r\n\r\n\
         BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
         UID:evt-001@fauna.test\r\nSUMMARY:{summary}\r\nORGANIZER:mailto:{from}\r\n\
         ATTENDEE:mailto:{to}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    )
    .into_bytes()
}

// ── (a) in-domain attendee → local INBOX; external attendee → relay queue ──

#[tokio::test]
async fn mda_enqueue_in_domain_attendee_delivered_locally_external_relayed() {
    let (router, state) = router_and_state().await;
    let mda: [u8; 32] = [9u8; 32];
    common::approve_mda(&state, mda).await;

    // The organizer the MDA is fanning out for — resolves (exact alias) to
    // satisfy the MDA caller-scope.
    let organizer: [u8; 32] = [0x0a; 32];
    state
        .db
        .put_exact_alias(DOMAIN, "organizer", "exact", &organizer)
        .await
        .unwrap();

    // In-domain attendee: a real local mailbox with a sealing pubkey.
    let attendee: [u8; 32] = [0x42; 32];
    let attendee_secret = provision_recipient(&state, attendee, "attendee", [0x5e; 32]).await;

    let raw = imip_request(
        "organizer@fauna.test",
        "attendee@fauna.test",
        "Sprint Planning",
    );
    let reply = enqueue_as_mda(
        &router,
        &state,
        mda,
        organizer,
        "organizer@fauna.test",
        vec!["attendee@fauna.test".into(), "guest@external.test".into()],
        raw.clone(),
    )
    .await;

    // Only the external attendee becomes a relay row; the in-domain attendee is
    // delivered locally (no self-loop).
    let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
    let relayed: Vec<String> = rows.iter().map(|r| r.recipient.clone()).collect();
    assert_eq!(
        relayed,
        vec!["guest@external.test".to_string()],
        "only the external attendee is MX-relayed"
    );
    assert_eq!(reply.ids.len(), 1, "reply ids cover only the relayed row");

    // The in-domain attendee genuinely received the iMIP, sealed + readable.
    let opened = open_only_inbox_message(&router, &state, attendee, &attendee_secret).await;
    common::assert_delivered_intact(
        opened.as_slice(),
        raw.as_slice(),
        "the in-domain attendee's INBOX opens to the exact iMIP REQUEST",
    );
}

// ── (b) the live self-loop shape: organizer invites its OWN sub-address ─────

#[tokio::test]
async fn mda_enqueue_organizer_subaddress_self_loop_delivered_locally() {
    let (router, state) = router_and_state().await;
    let mda: [u8; 32] = [9u8; 32];
    common::approve_mda(&state, mda).await;

    // The organizer is also the observer: it provisions a pubkey + alias, and
    // sub-addressing routes its own `organizer+autosched-<nonce>` back to it.
    let organizer: [u8; 32] = [0x0a; 32];
    let organizer_secret = provision_recipient(&state, organizer, "organizer", [0x7a; 32]).await;

    let nonce = "autosched-deadbeef";
    let to = format!("organizer+{nonce}@fauna.test");
    let raw = imip_request("organizer@fauna.test", &to, "Self-loop Proof");
    enqueue_as_mda(
        &router,
        &state,
        mda,
        organizer,
        "organizer@fauna.test",
        vec![to.clone()],
        raw.clone(),
    )
    .await;

    // No relay row: the sub-address self-loop is delivered locally, never
    // MX-relayed (the bounce the live proof hit).
    let rows = state.db.fetch_all_outbound_for_test().await.unwrap();
    assert!(
        rows.is_empty(),
        "the organizer sub-address must be delivered locally, not relayed: {:?}",
        rows.iter().map(|r| r.recipient.clone()).collect::<Vec<_>>()
    );

    // The organizer observes the iMIP in its own INBOX (how the live
    // `test_caldav_autoschedule_live_nest.py` self-loop observes the REQUEST).
    let opened = open_only_inbox_message(&router, &state, organizer, &organizer_secret).await;
    common::assert_delivered_intact(
        opened.as_slice(),
        raw.as_slice(),
        "the organizer's INBOX opens to the exact self-loop iMIP REQUEST",
    );
}
