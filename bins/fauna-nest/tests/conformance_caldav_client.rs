//! Cross-binary round-trip for the **events.md Decision-B** encrypted-calendar
//! path: the real shared `fauna-client-caldav` crate (`CalDavClient` + the
//! client-side seal/unseal + the native read path) drives the real nest
//! `fauna.bridges.*` CalDAV handlers against a real in-memory `CacheDb`. This is
//! the proof that the crate and nest agree on the encrypted `bridge_caldav_*`
//! store: a User-class Fauna app provisions its Personal calendar, seals +
//! PUTs an event, queries it back, and unseals the body to recover the *exact*
//! `.ics` the shared writer produced.
//!
//! ⚠ SCOPE: this proves the **Rust** round-trip (client seal → nest store →
//! client unseal) only. It does **not** exercise the Go MDA decrypting + serving
//! a *client-written* event to a real CalDAV MUA — the cross-language path
//! (Rust `fauna_client_caldav::seal_event_body` → Go `mailfauna.OpenMailRecord`
//! → emersion REPORT encode). That direction is covered by the tier_3
//! `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py` (real linux app
//! seal → Python `CalDAVClient` MUA against a local nest + MDA), added when it
//! caught GAP 2 — the writer omitted the RFC-5545-mandatory `DTSTAMP`, which
//! go-ical decodes but its *encoder* rejects on the MDA serve path, so a
//! client-written event was invisible to every MUA (FIXED 2026-06-05 in the
//! shared `fauna_core::ical` writer; `caldav-server.md` § Implementation status
//! today → GAP 2). This Rust test now also asserts the stamped DTSTAMP rides the
//! sealed body.
//!
//! The WS transport seam is replaced by a direct router dispatch (the same seam
//! `conformance_calendars.rs` uses): `RouterRequester` implements the crate's
//! `RpcRequester` by encoding the payload, invoking the registered handler with
//! a fixed connection actor, and decoding the reply. The connection actor is a
//! plain actor (no bridge enrollment, not admin) → `CallerClass::User`
//! (`caller_class_for_actor`), which Step 3a widened the event RPCs to admit
//! (caller-scoped to its own `actor_id`).
//!
//! Tier: tier_3 (real `fauna-nest` `AppState` + real `CacheDb` — no mocks; the
//! only stand-in is the in-process dispatch for the WebSocket).

mod common;

use std::sync::Arc;

use bytes::Bytes;

use fauna_client_caldav::{
    AttendeeInfo, CalDavClient, DavRecipientKeys, DecodedEventsPage, EventFields, FaunaEventExt,
    bridge_routing, epoch_secs_to_ical_utc, generate_ical, parse_ical_attendees,
    personal_calendar_id, project_attendee_rsvp, seal_event_body, unseal_event_body,
};
use fauna_nest::{
    bridge_caldav_handlers::register_bridge_caldav_handlers, db::CacheDb, routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{RpcRequester, decode_strict, encode_canonical};

use bridge_routing::{
    ProvisionCalendarReply, ProvisionCalendarRequest, PutEventCiphertextReply, QueryEventsReply,
    QueryEventsRequest,
};

/// A regular user actor — not a bridge service user, not admin — so
/// `caller_class_for_actor` resolves it to `CallerClass::User`.
const USER_ACTOR: [u8; 32] = [20u8; 32];
const MSEK: [u8; 32] = [7u8; 32];

/// The crate's `RpcRequester`, with the WebSocket transport replaced by a direct
/// dispatch into the registered handler keyed on `actor`. Encodes the request,
/// runs the handler, decodes the reply — the exact wire contract the live WS path
/// drives, minus the socket.
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
        decode_strict(&reply).map_err(|e| e.to_string())
    }
}

fn user_client() -> (Arc<AppState>, CalDavClient<RouterRequester>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_caldav_handlers(&mut b);
    let client = CalDavClient::new(RouterRequester {
        router: b.build(),
        state: state.clone(),
        actor: USER_ACTOR,
    });
    (state, client)
}

fn standup_event() -> EventFields {
    EventFields {
        summary: "Standup".into(),
        dtstart: "2026-06-02T09:00:00Z".into(),
        dtend: "2026-06-02T09:15:00Z".into(),
        uid: "evt-1@fauna.test".into(),
        ..Default::default()
    }
}

/// The full Decision-B write→read round-trip through the shared crate against
/// real nest handlers: provision Personal → `seal_and_put_event` → `query_events`
/// → unseal recovers the exact writer output → `query_events_decoded` parses it.
#[tokio::test]
async fn user_class_encrypted_calendar_roundtrip_via_crate() {
    let (_state, client) = user_client();
    let personal = personal_calendar_id();

    // 1. Provision the Personal calendar (sealed metadata; the handler only
    //    requires non-empty ciphertext).
    let metadata = seal_event_body(b"{\"name\":\"Personal\"}", &MSEK).expect("seal metadata");
    let prov = client
        .provision_calendar(ProvisionCalendarRequest {
            actor_id: USER_ACTOR.to_vec(),
            calendar_id: personal.to_vec(),
            encrypted_metadata: metadata,
            ..Default::default()
        })
        .await
        .expect("provision transport ok");
    assert_eq!(prov, ProvisionCalendarReply::Created);

    // 2. Build + seal + PUT an event in one round-trip (the headline write op),
    //    carrying a Fauna sidecar (the `interested` refinement) so the full
    //    encrypted two-payload round-trip is exercised end-to-end through nest.
    let event = standup_event();
    let uid_hash = [0xCCu8; 32]; // any 32-byte dedup key; the real UID stays sealed.
    let ext = FaunaEventExt {
        interested_attendees: vec!["alice@fauna.test".into()],
        ..Default::default()
    };
    let put = client
        .seal_and_put_event(
            &USER_ACTOR,
            &personal,
            &uid_hash,
            &MSEK,
            &event,
            &[],
            "alice@fauna.test",
            Some(&ext),
            1_700_000_000,
            None,
        )
        .await
        .expect("seal + put ok");
    assert!(
        matches!(put, PutEventCiphertextReply::Created { .. }),
        "first PUT must Create, got {put:?}"
    );

    // 3. Query it back; nest stores ciphertext only, so the body is sealed —
    //    unsealing with the same msek recovers the EXACT writer output.
    let reply = client
        .query_events(QueryEventsRequest {
            actor_id: USER_ACTOR.to_vec(),
            calendar_id: personal.to_vec(),
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        })
        .await
        .expect("query transport ok");
    let events = match reply {
        QueryEventsReply::Ok { events, .. } => events,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(events.len(), 1, "exactly the one PUT event");
    let stored = &events[0];
    assert_eq!(stored.uid_hash, uid_hash.to_vec());
    assert_ne!(
        stored.encrypted_body,
        generate_ical(&event, &[], "alice@fauna.test").into_bytes(),
        "nest stores ciphertext, never plaintext"
    );
    let recovered = unseal_event_body(&stored.encrypted_body, &MSEK).expect("unseal stored body");
    // seal_and_put_event stamps the RFC-5545-mandatory DTSTAMP from the write
    // timestamp (1_700_000_000) — without it go-ical's encoder rejects the
    // VEVENT on the MDA serve path (GAP 2), so the stored body carries it.
    let mut stamped = event.clone();
    stamped.dtstamp = epoch_secs_to_ical_utc(1_700_000_000);
    let expected_ics = generate_ical(&stamped, &[], "alice@fauna.test");
    assert!(
        expected_ics.contains("DTSTAMP:"),
        "the write path must emit DTSTAMP: {expected_ics}"
    );
    assert_eq!(
        String::from_utf8(recovered).unwrap(),
        expected_ics,
        "the round-tripped body is byte-identical to the shared writer output"
    );

    // 4. The native read path turns the same sealed reply into a typed event.
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: USER_ACTOR.to_vec(),
                calendar_id: personal.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&MSEK),
        )
        .await
        .expect("decoded read ok");
    match page {
        DecodedEventsPage::Ok { events, .. } => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].ics, expected_ics);
            let vevent = events[0]
                .document
                .components
                .iter()
                .find(|c| c.name == "VEVENT")
                .expect("a VEVENT");
            let summary = vevent
                .properties
                .iter()
                .find(|p| p.name == "SUMMARY")
                .expect("SUMMARY");
            assert_eq!(summary.value, "Standup");
            // The sidecar survived the full nest round-trip (stored opaquely in
            // the `encrypted_fauna_ext` column, never decrypted server-side) and
            // drives the asymmetric RSVP projection client-side.
            let decoded_ext = events[0].fauna_ext.as_ref().expect("sidecar round-tripped");
            assert_eq!(decoded_ext, &ext);
            assert_eq!(
                project_attendee_rsvp("TENTATIVE", "alice@fauna.test", Some(decoded_ext)),
                "interested"
            );
        }
        other => panic!("expected decoded Ok, got {other:?}"),
    }
}

/// The **add-attendee re-PUT** contract end-to-end (events.md § Scheduling &
/// invitations, the linux `attendee-invite-field` flow): an event created with
/// an empty roster is re-PUT with a `mailto:` attendee added (the shape
/// `caldav_backend::add_attendee` produces — a `NEEDS-ACTION` invitee + bumped
/// `SEQUENCE`); the nest store faithfully round-trips it so a subsequent query
/// recovers a VEVENT carrying that `ATTENDEE;mailto:` line. Path-agnostic (no UI)
/// — the linux e2e exercises the same flow through the detail panel.
#[tokio::test]
async fn add_attendee_re_put_carries_mailto_into_stored_roster() {
    let (_state, client) = user_client();
    let personal = personal_calendar_id();
    let uid_hash = [0xADu8; 32];

    let metadata = seal_event_body(b"{\"name\":\"Personal\"}", &MSEK).expect("seal metadata");
    client
        .provision_calendar(ProvisionCalendarRequest {
            actor_id: USER_ACTOR.to_vec(),
            calendar_id: personal.to_vec(),
            encrypted_metadata: metadata,
            ..Default::default()
        })
        .await
        .expect("provision ok");

    // 1. Create the event with an empty roster (a Fauna-authored event).
    let event = standup_event();
    let put = client
        .seal_and_put_event(
            &USER_ACTOR,
            &personal,
            &uid_hash,
            &MSEK,
            &event,
            &[],
            "alice@fauna.test",
            None,
            1_700_000_000,
            None,
        )
        .await
        .expect("create ok");
    assert!(matches!(put, PutEventCiphertextReply::Created { .. }));

    // 2. Re-PUT with a mailto: attendee added + SEQUENCE bumped — the exact shape
    //    `add_attendee` emits for "type guest@example.com → Invite".
    let mut updated = event.clone();
    updated.sequence = 1;
    let roster = vec![AttendeeInfo {
        name: String::new(),
        email: "guest@example.com".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let reput = client
        .seal_and_put_event(
            &USER_ACTOR,
            &personal,
            &uid_hash,
            &MSEK,
            &updated,
            &roster,
            "alice@fauna.test",
            None,
            1_700_000_100,
            None,
        )
        .await
        .expect("re-PUT ok");
    assert!(
        matches!(reput, PutEventCiphertextReply::Updated { .. }),
        "the same uid_hash must UPDATE the existing row, got {reput:?}"
    );

    // 3. Query it back: still exactly one row, now carrying the mailto: attendee.
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: USER_ACTOR.to_vec(),
                calendar_id: personal.to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&MSEK),
        )
        .await
        .expect("decoded read ok");
    let DecodedEventsPage::Ok { events, .. } = page else {
        panic!("expected decoded Ok");
    };
    assert_eq!(events.len(), 1, "the add is an UPDATE, not a second row");
    assert!(
        events[0].ics.contains("mailto:guest@example.com"),
        "the stored VEVENT carries the added attendee: {}",
        events[0].ics
    );
    let parsed = parse_ical_attendees(&events[0].ics);
    let guest = parsed
        .iter()
        .find(|a| a.email == "guest@example.com")
        .expect("guest on the round-tripped roster");
    assert_eq!(guest.partstat, "NEEDS-ACTION");
}

/// Reading a calendar that was never provisioned returns the `CalendarNotFound`
/// signal end-to-end (nest → crate), the state a fresh client sees before it
/// provisions Personal.
#[tokio::test]
async fn query_events_decoded_calendar_not_found_end_to_end() {
    let (_state, client) = user_client();
    let page = client
        .query_events_decoded(
            QueryEventsRequest {
                actor_id: USER_ACTOR.to_vec(),
                calendar_id: personal_calendar_id().to_vec(),
                since_modseq: None,
                after_event_id: None,
                limit: 0,
            },
            &DavRecipientKeys::derive(&MSEK),
        )
        .await
        .expect("transport ok");
    assert_eq!(page, DecodedEventsPage::CalendarNotFound);
}

/// A different actor cannot reach `USER_ACTOR`'s calendar: the caller-scope guard
/// (Step 3a) rejects any non-MDA caller whose `request.actor_id` is not its own
/// connection actor — the load-bearing invariant that makes the `BridgeMda | User`
/// allowlist safe for the direct client path.
#[tokio::test]
async fn other_actor_cannot_read_via_crate() {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    register_bridge_caldav_handlers(&mut b);
    // The connection is actor B; the request targets actor A's calendar.
    let client = CalDavClient::new(RouterRequester {
        router: b.build(),
        state,
        actor: [99u8; 32],
    });
    let err = client
        .query_events(QueryEventsRequest {
            actor_id: USER_ACTOR.to_vec(),
            calendar_id: personal_calendar_id().to_vec(),
            since_modseq: None,
            after_event_id: None,
            limit: 0,
        })
        .await
        .expect_err("cross-actor read must be denied");
    assert!(
        err.contains("permission_denied") || err.contains("another actor"),
        "expected a permission error, got: {err}"
    );
}
