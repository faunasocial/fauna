//! End-to-end proof of the **client-driven iMIP dispatch** path
//! (caldav-server.md § Scheduling & invitations → Server-side auto-schedule,
//! "organizer fan-out → iMIP email via the bridge MTA"): an organizer builds
//! a scheduling `REQUEST` with the shared `fauna_client_caldav::build_event_imip`
//! and submits it over the real `fauna.email.send` handler; an in-domain
//! attendee then reads the message, sealed, from their own INBOX and it opens
//! back to the exact iMIP body — METHOD + VEVENT intact.
//!
//! **tier_3** — the real send handler (partition + `seal_to_recipient` to the
//! recipient's MSEK-derived pubkey + the sealed-ingest persist core), the real
//! `inbox.fetch` read-back, and client-side unseal. No stubs. This is the
//! cross-binary seam the shared-Rust unit tests (`build_event_imip`,
//! `generate_itip`) cannot reach: that the bytes the scheduling crate emits
//! survive the outbound mail path to a recipient's encrypted mailbox.
//!
//! Mirrors the in-domain harness in `conformance_email_send_in_domain.rs`.

mod common;
use common::dispatch;
use common::open_only_inbox_message;
use common::provision_recipient;

use std::sync::Arc;

use bytes::Bytes;

use fauna_client_caldav::{AttendeeInfo, EventFields, ITipMethod, build_event_imip};
use fauna_nest::db::CacheDb;
use fauna_nest::email_handlers::register_email_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::email::{SendEmailReply, SendEmailRequest};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};

use common::TEST_DOMAIN as DOMAIN;

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

// ── organizer fan-out: build_event_imip(REQUEST) → fauna.email.send → INBOX ──

#[tokio::test]
async fn imip_request_reaches_in_domain_attendee_inbox_intact() {
    let (router, state) = router_and_state().await;

    // Attendee bob (in-domain, mail-enabled): provision his sealing key.
    let bob: [u8; 32] = [0x42; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x5e; 32]).await;

    // Organizer alice: a User-class actor with handle "alice" so the
    // From: alice@fauna.test sender-handle verification passes.
    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");

    // Alice builds the scheduling REQUEST for an event with bob on the roster.
    let event = EventFields {
        summary: "Project kickoff".into(),
        dtstart: "2026-07-01T15:00:00Z".into(),
        dtend: "2026-07-01T16:00:00Z".into(),
        uid: "kickoff-imip-1@fauna.test".into(),
        ..Default::default()
    };
    let roster = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let message = build_event_imip(
        ITipMethod::Request,
        &event,
        &roster,
        "alice@fauna.test",
        "2026-06-04T12:00:00Z",
    )
    .expect("a roster yields an iMIP message");
    assert_eq!(message.recipients, vec!["bob@fauna.test"]);

    // Submit it over the real outbound mail path.
    let reply = send(
        &router,
        &state,
        alice,
        message.recipients.clone(),
        message.raw_rfc5322.clone(),
    )
    .await
    .expect("iMIP send ok");
    assert_eq!(reply.local_delivered, 1, "bob delivered locally");
    assert_eq!(reply.remote_queued, 0);
    assert!(reply.remote_errors.is_empty(), "{:?}", reply.remote_errors);

    // Bob reads it from his sealed INBOX; it opens to the EXACT iMIP message.
    let opened = open_only_inbox_message(&router, &state, bob, &bob_secret).await;
    common::assert_delivered_intact(
        opened.as_slice(),
        message.raw_rfc5322.as_slice(),
        "the iMIP REQUEST decrypts byte-for-byte",
    );

    // The scheduling semantics survived the round-trip end-to-end.
    let text = String::from_utf8(opened).expect("utf-8 message");
    assert!(text.contains("Content-Type: text/calendar; charset=UTF-8; method=REQUEST"));
    assert!(text.contains("METHOD:REQUEST"));
    assert!(text.contains("UID:kickoff-imip-1@fauna.test"));
    assert!(text.contains("ATTENDEE"));
    assert!(text.contains("mailto:bob@fauna.test"));
}

// ── cancellation: build_event_imip(CANCEL) → the same path, METHOD:CANCEL ──

#[tokio::test]
async fn imip_cancel_reaches_attendee_inbox_with_cancel_method() {
    let (router, state) = router_and_state().await;

    let bob: [u8; 32] = [0x42; 32];
    let bob_secret = provision_recipient(&state, bob, "bob", [0x5e; 32]).await;

    let alice: [u8; 32] = [0xA1; 32];
    state
        .db
        .create_user_with_handle(&alice, "free", "alice", None)
        .await
        .expect("create alice with handle");

    let event = EventFields {
        summary: "Project kickoff".into(),
        dtstart: "2026-07-01T15:00:00Z".into(),
        uid: "kickoff-imip-1@fauna.test".into(),
        status: "cancelled".into(),
        sequence: 1,
        ..Default::default()
    };
    let roster = vec![AttendeeInfo {
        email: "bob@fauna.test".into(),
        partstat: "ACCEPTED".into(),
        ..Default::default()
    }];
    let message = build_event_imip(
        ITipMethod::Cancel,
        &event,
        &roster,
        "alice@fauna.test",
        "2026-06-04T13:00:00Z",
    )
    .expect("cancel message");

    let reply = send(
        &router,
        &state,
        alice,
        message.recipients.clone(),
        message.raw_rfc5322.clone(),
    )
    .await
    .expect("iMIP cancel send ok");
    assert_eq!(reply.local_delivered, 1);

    let opened = open_only_inbox_message(&router, &state, bob, &bob_secret).await;
    let text = String::from_utf8(opened).expect("utf-8 message");
    assert!(text.contains("method=CANCEL"));
    assert!(text.contains("METHOD:CANCEL"));
    assert!(text.contains("STATUS:CANCELLED"));
}
