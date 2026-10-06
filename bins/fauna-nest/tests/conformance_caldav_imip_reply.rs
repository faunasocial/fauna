//! End-to-end proof of the **client-driven inbound iMIP `REPLY` merge** path
//! (caldav-server.md § Scheduling & invitations → "The one operation with a
//! cost: applying a `REPLY` to the organizer's stored event" — v1: the server
//! delivers the `REPLY` encrypt-only into the organizer's mailbox, and the
//! organizer's Fauna app merges it and re-PUTs the event).
//!
//! The full receiving half, end-to-end across binaries (the symmetric twin of
//! the outbound `conformance_caldav_imip_send.rs`):
//!
//!   organizer alice provisions Personal + PUTs an event with bob `NEEDS-ACTION`
//!   → bob builds an iTIP `REPLY` (`build_event_imip`) and sends it over the real
//!   `fauna.email.send` handler → alice reads the sealed message from her own
//!   INBOX, unseals it, and `CalDavClient::apply_inbound_reply_from_mail` extracts
//!   the `text/calendar; method=REPLY` part, merges bob's `ACCEPTED` into her
//!   stored roster, and re-PUTs → a `query_events` of alice's calendar shows bob
//!   `ACCEPTED`.
//!
//! **tier_3** — the real CalDAV handlers (`register_bridge_caldav_handlers`), the
//! real send + `inbox.fetch` handlers, real `CacheDb`, real seal/unseal. No
//! stubs; the only stand-in is the in-process router dispatch for the WebSocket
//! (the same seam `conformance_caldav_client.rs` uses). This is the cross-binary
//! coverage the shared-Rust unit tests (`apply_reply_to_roster`,
//! `extract_text_calendar_part`) cannot reach: that an inbound REPLY which rode
//! the encrypted mail path actually updates the organizer's encrypted event.

mod common;
use common::dispatch;
use common::open_only_inbox_message;
use common::provision_recipient;

use std::sync::Arc;

use bytes::Bytes;

use fauna_client_caldav::{
    AttendeeInfo, CalDavClient, DavRecipientKeys, DecodedEventsPage, EventFields, ITipMethod,
    InboundReplyOutcome, RefusalReason, build_event_imip, parse_ical_attendees,
    personal_calendar_id, seal_event_body, uid_hash,
};
use fauna_nest::{
    bridge_caldav_handlers::register_bridge_caldav_handlers, db::CacheDb,
    email_handlers::register_email_handlers, routes::AppState, rpc_router::RpcRouter,
};
use fauna_protocol::bridge_routing::{
    ProvisionCalendarReply, ProvisionCalendarRequest, PutEventCiphertextReply, QueryEventsRequest,
};
use fauna_protocol::email::{SendEmailReply, SendEmailRequest};
use fauna_protocol::{RpcRequester, decode_strict as decode, encode_canonical};

use common::TEST_DOMAIN as DOMAIN;
/// Organizer alice — a plain User-class actor (not bridge, not admin), so the
/// CalDAV event RPCs admit her caller-scoped to her own actor (Step 3a).
const ALICE: [u8; 32] = [0xA1; 32];
/// Responder bob.
const BOB: [u8; 32] = [0x42; 32];
/// Alice's MSEK — seals her events AND derives the recipient keypair her inbox
/// is sealed to (so one secret unseals both her calendar bodies and her mail).
const ALICE_MSEK: [u8; 32] = [0x5a; 32];
const EVENT_UID: &str = "kickoff-reply-1@fauna.test";

/// The crate's `RpcRequester` over a direct router dispatch keyed on `actor`.
struct RouterRequester {
    router: RpcRouter,
    state: Arc<AppState>,
    actor: [u8; 32],
}

impl RpcRequester for RouterRequester {
    type Error = String;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        common::seed_dispatch_actor(&self.state.db, &self.actor).await;
        let bytes = Bytes::from(
            encode_canonical(&payload)
                .map_err(|e| e.to_string())?
                .to_vec(),
        );
        let meta = self
            .router
            .kind_meta(kind)
            .ok_or_else(|| format!("kind not registered: {kind}"))?;
        let reply = (meta.handler)(self.state.clone(), self.actor, bytes)
            .await
            .map_err(|e| format!("{e:?}"))?;
        decode(&reply).map_err(|e| e.to_string())
    }
}

/// A router carrying BOTH the CalDAV event handlers and the email send/inbox
/// handlers, plus the shared `AppState` (mail domain set).
async fn harness() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    db.add_mail_domain(DOMAIN, true, "testing", "self_signed", None, None)
        .await
        .unwrap();
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_caldav_handlers(&mut b);
    register_email_handlers(&mut b);
    (b.build(), state)
}

async fn send(
    router: &RpcRouter,
    state: &Arc<AppState>,
    sender: [u8; 32],
    recipients: Vec<String>,
    raw_rfc5322: Vec<u8>,
) -> SendEmailReply {
    let req = SendEmailRequest {
        extra: Default::default(),
        recipients,
        raw_rfc5322,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let reply = dispatch(router, state.clone(), sender, "fauna.email.send", payload)
        .await
        .expect("send ok");
    decode(&reply).expect("decode send reply")
}

fn kickoff_event() -> EventFields {
    EventFields {
        summary: "Project kickoff".into(),
        dtstart: "2026-07-01T15:00:00Z".into(),
        dtend: "2026-07-01T16:00:00Z".into(),
        uid: EVENT_UID.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn inbound_reply_merges_into_organizers_stored_event_end_to_end() {
    let (router, state) = harness().await;
    let alice = CalDavClient::new(RouterRequester {
        router: {
            // A second router instance for the crate's requester — same handler
            // set + shared `state`, so it reads/writes the same store.
            let mut b = RpcRouter::builder();
            register_bridge_caldav_handlers(&mut b);
            register_email_handlers(&mut b);
            b.build()
        },
        state: state.clone(),
        actor: ALICE,
    });

    // Alice can receive mail (her inbox is sealed to ALICE_MSEK's recipient key).
    let alice_secret = provision_recipient(&state, ALICE, "alice", ALICE_MSEK).await;
    // Bob is a handled in-domain user so his REPLY's From: bob@fauna.test passes
    // the send handler's sender-handle verification.
    state
        .db
        .create_user_with_handle(&BOB, "free", "bob", None)
        .await
        .expect("create bob with handle");

    // 1. Alice provisions her Personal calendar and PUTs an event inviting bob
    //    (NEEDS-ACTION), organizer = alice.
    let personal = personal_calendar_id();
    let metadata = seal_event_body(b"{\"name\":\"Personal\"}", &ALICE_MSEK).expect("seal metadata");
    let prov = alice
        .provision_calendar(ProvisionCalendarRequest {
            actor_id: ALICE.to_vec(),
            calendar_id: personal.to_vec(),
            encrypted_metadata: metadata,
            ..Default::default()
        })
        .await
        .expect("provision ok");
    assert_eq!(prov, ProvisionCalendarReply::Created);

    let event = kickoff_event();
    let invited = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let put = alice
        .seal_and_put_event(
            &ALICE,
            &personal,
            &uid_hash(EVENT_UID),
            &ALICE_MSEK,
            &event,
            &invited,
            "alice@fauna.test",
            None,
            1_700_000_000,
            None,
        )
        .await
        .expect("seal + put ok");
    assert!(matches!(put, PutEventCiphertextReply::Created { .. }));

    // 2. Bob builds an iTIP REPLY (ACCEPTED) and sends it over the real outbound
    //    mail path to alice (responder → organizer).
    let bob_accepts = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "ACCEPTED".into(),
        fauna_status: "going".into(),
    }];
    let reply = build_event_imip(
        ITipMethod::Reply,
        &event,
        &bob_accepts,
        "alice@fauna.test",
        "2026-06-04T12:30:00Z",
    )
    .expect("a reply targets the organizer");
    assert_eq!(reply.recipients, vec!["alice@fauna.test"]);

    let send_reply = send(
        &router,
        &state,
        BOB,
        reply.recipients.clone(),
        reply.raw_rfc5322.clone(),
    )
    .await;
    assert_eq!(send_reply.local_delivered, 1, "alice delivered locally");
    assert!(
        send_reply.remote_errors.is_empty(),
        "{:?}",
        send_reply.remote_errors
    );

    // 3. Alice reads the sealed REPLY from her INBOX and merges it.
    let raw = open_only_inbox_message(&router, &state, ALICE, &alice_secret).await;
    let outcome = alice
        .apply_inbound_reply_from_mail(&ALICE, &ALICE_MSEK, &raw, 1_700_000_100)
        .await
        .expect("apply inbound reply ok");
    match &outcome {
        InboundReplyOutcome::Applied {
            uid_hash: uh,
            attendees,
        } => {
            assert_eq!(uh, &uid_hash(EVENT_UID));
            let bob = attendees
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case("bob@fauna.test"))
                .expect("bob on merged roster");
            assert_eq!(bob.partstat, "ACCEPTED");
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    // 4. The merge persisted: a fresh query of alice's calendar shows bob ACCEPTED
    //    on the stored (re-sealed) event — the organizer's calendar reflects the
    //    response, exactly what an Apple Calendar reading the same store would.
    let page = alice
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: ALICE.to_vec(),
                calendar_id: personal.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&ALICE_MSEK),
        )
        .await
        .expect("query ok");
    let events = match page {
        DecodedEventsPage::Ok { events, .. } => events,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(
        events.len(),
        1,
        "still exactly the one event (updated in place)"
    );
    let roster = parse_ical_attendees(&events[0].ics);
    let bob = roster
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case("bob@fauna.test"))
        .expect("bob still on the stored roster");
    assert_eq!(
        bob.partstat, "ACCEPTED",
        "the inbound REPLY flipped bob's PARTSTAT on the organizer's stored event"
    );
}

#[tokio::test]
async fn ordinary_mail_is_not_treated_as_a_reply() {
    let (_router, state) = harness().await;
    let alice = CalDavClient::new(RouterRequester {
        router: {
            let mut b = RpcRouter::builder();
            register_bridge_caldav_handlers(&mut b);
            b.build()
        },
        state: state.clone(),
        actor: ALICE,
    });
    let plain = b"From: bob@fauna.test\r\n\
To: alice@fauna.test\r\n\
Subject: Lunch?\r\n\
Content-Type: text/plain; charset=UTF-8\r\n\
\r\n\
Want to grab lunch?\r\n";
    let outcome = alice
        .apply_inbound_reply_from_mail(&ALICE, &ALICE_MSEK, plain, 1_700_000_000)
        .await
        .expect("ok");
    assert_eq!(outcome, InboundReplyOutcome::NotCalendarReply);
}

// ── The mail rail's sender check — caldav-server.md § Who may mutate an
//    existing event over the inbound rail → *The mail rail*. ──

/// A third handled in-domain user, who is NOT on alice's roster.
const MALLORY: [u8; 32] = [0x77; 32];

/// Alice's calendar as a fresh `query_events` returns it: the one stored
/// event's `.ics`.
async fn alice_stored_ics(alice: &CalDavClient<RouterRequester>) -> String {
    let page = alice
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: ALICE.to_vec(),
                calendar_id: personal_calendar_id().to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&ALICE_MSEK),
        )
        .await
        .expect("query ok");
    let events = match page {
        DecodedEventsPage::Ok { events, .. } => events,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(events.len(), 1, "exactly the one stored event");
    events[0].ics.clone()
}

/// The event's `SEQUENCE:` line (or `None` when the writer omitted it).
fn sequence_line(ics: &str) -> Option<String> {
    ics.lines()
        .find(|l| l.starts_with("SEQUENCE:"))
        .map(str::to_string)
}

/// Alice provisions Personal and PUTs the kickoff inviting bob (`NEEDS-ACTION`);
/// mallory (a handled in-domain user) exists. Returns the harness, alice's
/// client, and alice's mail secret.
async fn alice_invites_bob_and_mallory_exists() -> (
    RpcRouter,
    Arc<AppState>,
    CalDavClient<RouterRequester>,
    common::RecipientKeys,
) {
    let (router, state) = harness().await;
    let alice = CalDavClient::new(RouterRequester {
        router: {
            let mut b = RpcRouter::builder();
            register_bridge_caldav_handlers(&mut b);
            register_email_handlers(&mut b);
            b.build()
        },
        state: state.clone(),
        actor: ALICE,
    });
    let alice_secret = provision_recipient(&state, ALICE, "alice", ALICE_MSEK).await;
    state
        .db
        .create_user_with_handle(&BOB, "free", "bob", None)
        .await
        .expect("create bob with handle");
    state
        .db
        .create_user_with_handle(&MALLORY, "free", "mallory", None)
        .await
        .expect("create mallory with handle");

    let personal = personal_calendar_id();
    let metadata = seal_event_body(b"{\"name\":\"Personal\"}", &ALICE_MSEK).expect("seal metadata");
    let prov = alice
        .provision_calendar(ProvisionCalendarRequest {
            actor_id: ALICE.to_vec(),
            calendar_id: personal.to_vec(),
            encrypted_metadata: metadata,
            ..Default::default()
        })
        .await
        .expect("provision ok");
    assert_eq!(prov, ProvisionCalendarReply::Created);
    let invited = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let put = alice
        .seal_and_put_event(
            &ALICE,
            &personal,
            &uid_hash(EVENT_UID),
            &ALICE_MSEK,
            &kickoff_event(),
            &invited,
            "alice@fauna.test",
            None,
            1_700_000_000,
            None,
        )
        .await
        .expect("seal + put ok");
    assert!(matches!(put, PutEventCiphertextReply::Created { .. }));
    (router, state, alice, alice_secret)
}

/// A `REPLY` whose `.ics` speaks for bob (`ACCEPTED`), but whose `From:` is
/// mallory's own address — the only `From:` the send handler's handle gate
/// lets mallory use. `build_event_imip` derives `From:` from the responding
/// attendee, so the one `From:` field is rewritten in place.
fn mallory_speaks_for_bob() -> Vec<u8> {
    let bob_accepts = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "ACCEPTED".into(),
        fauna_status: "going".into(),
    }];
    let reply = build_event_imip(
        ITipMethod::Reply,
        &kickoff_event(),
        &bob_accepts,
        "alice@fauna.test",
        "2026-06-04T12:30:00Z",
    )
    .expect("a reply targets the organizer");
    let text = String::from_utf8(reply.raw_rfc5322).expect("utf-8 imip");
    assert_eq!(
        text.matches("From: bob@fauna.test\r\n").count(),
        1,
        "the builder writes exactly one From: naming the attendee:\n{text}"
    );
    text.replacen(
        "From: bob@fauna.test\r\n",
        "From: mallory@fauna.test\r\n",
        1,
    )
    .into_bytes()
}

/// The refusal a mailed `REPLY` must produce, and the proof nothing was
/// written: bob still `NEEDS-ACTION`, the stored event byte-identical (so its
/// `SEQUENCE` is unchanged too).
async fn assert_refused_and_untouched(
    alice: &CalDavClient<RouterRequester>,
    raw: &[u8],
    ics_before: &str,
    expect_reason: RefusalReason,
    expect_sender: &str,
) {
    let outcome = alice
        .apply_inbound_reply_from_mail(&ALICE, &ALICE_MSEK, raw, 1_700_000_100)
        .await
        .expect("a refusal is an outcome, not an error");
    match &outcome {
        InboundReplyOutcome::Refused {
            uid_hash: uh,
            summary,
            reason,
        } => {
            assert_eq!(*reason, expect_reason, "refusal reason");
            assert_eq!(summary, "Project kickoff");
            assert_eq!(uh, &uid_hash(EVENT_UID));
        }
        other => panic!("expected Refused {{ {expect_reason:?} }}, got {other:?}"),
    }
    let row = outcome
        .refused_mail_change_record(raw, 1_700_000_100)
        .expect("a refusal yields a surfaced row");
    assert_eq!(
        row.sender_address, expect_sender,
        "the row names the door-authenticated sender (never the .ics's attendee)"
    );

    let ics_after = alice_stored_ics(alice).await;
    let bob = parse_ical_attendees(&ics_after)
        .into_iter()
        .find(|a| a.email.eq_ignore_ascii_case("bob@fauna.test"))
        .expect("bob still on the stored roster");
    assert_eq!(bob.partstat, "NEEDS-ACTION", "bob's answer was not forged");
    assert_eq!(
        sequence_line(&ics_after),
        sequence_line(ics_before),
        "SEQUENCE unchanged"
    );
    assert_eq!(ics_after, ics_before, "the stored event is untouched");
}

#[tokio::test]
async fn a_mailed_reply_from_someone_else_is_refused_and_leaves_the_organizers_event_untouched() {
    // (1) Mallory's REPLY speaking for bob, over the real send door. Door 1
    //     stamps mallory (her handle-gated From), so the copy names mallory
    //     while the .ics names bob → NotTheAttendee.
    {
        let (router, state, alice, alice_secret) = alice_invites_bob_and_mallory_exists().await;
        let before = alice_stored_ics(&alice).await;
        let sent = send(
            &router,
            &state,
            MALLORY,
            vec!["alice@fauna.test".into()],
            mallory_speaks_for_bob(),
        )
        .await;
        assert_eq!(sent.local_delivered, 1, "alice delivered locally");
        let raw = open_only_inbox_message(&router, &state, ALICE, &alice_secret).await;
        assert_eq!(
            fauna_mail::sender_auth::read_authenticated_sender_stamp(&raw).as_deref(),
            Some("mallory@fauna.test"),
            "door 1 stamped the sender it verified"
        );
        assert_refused_and_untouched(
            &alice,
            &raw,
            &before,
            RefusalReason::NotTheAttendee,
            "mallory@fauna.test",
        )
        .await;

        // (3) The same copy as alice would hold it had no door stamped it:
        //     no stamp is no answer, and no answer refuses.
        let unstamped = fauna_mail::received_header::strip_fauna_headers(&raw);
        assert_eq!(
            fauna_mail::sender_auth::read_authenticated_sender_stamp(&unstamped),
            None
        );
        assert_refused_and_untouched(
            &alice,
            &unstamped,
            &before,
            RefusalReason::SenderUnauthenticated,
            "",
        )
        .await;
    }

    // (2) The same REPLY carrying a forged `X-Fauna-Authenticated-Sender:
    //     bob@fauna.test` on top: the door strips it and writes its own.
    {
        let (router, state, alice, alice_secret) = alice_invites_bob_and_mallory_exists().await;
        let before = alice_stored_ics(&alice).await;
        let mut forged = b"X-Fauna-Authenticated-Sender: bob@fauna.test\r\n".to_vec();
        forged.extend_from_slice(&mallory_speaks_for_bob());
        let sent = send(
            &router,
            &state,
            MALLORY,
            vec!["alice@fauna.test".into()],
            forged,
        )
        .await;
        assert_eq!(sent.local_delivered, 1, "alice delivered locally");
        let raw = open_only_inbox_message(&router, &state, ALICE, &alice_secret).await;
        assert_eq!(
            fauna_mail::sender_auth::read_authenticated_sender_stamp(&raw).as_deref(),
            Some("mallory@fauna.test"),
            "the forged stamp never survives the door"
        );
        assert!(
            !String::from_utf8_lossy(&raw)
                .to_ascii_lowercase()
                .contains("x-fauna-authenticated-sender: bob@fauna.test"),
            "the forged stamp reached the sealed copy"
        );
        assert_refused_and_untouched(
            &alice,
            &raw,
            &before,
            RefusalReason::NotTheAttendee,
            "mallory@fauna.test",
        )
        .await;
    }
}
