//! **tier_3** end-to-end proof of the **mailbox-less WS-RPC sealed iMIP delivery**
//! path (`caldav-server.md` § Server-side auto-schedule → "mailbox-less Fauna user
//! (CalDAV enabled, email disabled) → WS-RPC sealed delivery"; TRACK B Half-1,
//! Slice 6a).
//!
//! A CalDAV organizer invites a Fauna attendee who has **no mailbox** (email
//! disabled on their deployment), so the iMIP cannot ride email. Instead it rides
//! the **MLS `Scheduling` welcome rail**: the organizer delivers a one-off
//! `WelcomeKind::Scheduling` group carrying the raw RFC 5322 iMIP as its first
//! application message; the attendee's receive loop drains that channel to the
//! calendar-apply path (never a chat thread) and merges it. The full round-trip:
//!
//!   alice (organizer) provisions Personal + PUTs an event inviting bob NEEDS-ACTION
//!   → alice `deliver_scheduling_imip(bob)` (MLS welcome + iMIP REQUEST app message)
//!   → bob `ingest_scheduling_welcome` + `poll_inbound_scheduling` drains the iMIP
//!     off the **real nest** channel log → `apply_inbound_scheduling_from_message`
//!     materializes the event in bob's calendar (he "sees the invite")
//!   → bob RSVPs going → `deliver_scheduling_imip(alice)` sends the REPLY back the
//!     same rail → alice drains it → the merge flips bob to ACCEPTED on alice's
//!     stored event (`query_events` confirms).
//!
//! What only this test catches over the Slice 1–5 shared-Rust unit tests (which
//! mock the nest): the iMIP actually crosses the **real** `fauna-nest`
//! conversations data plane — `welcome.deliver` (→ the recipient's inbox),
//! `channel.send` (→ the conv segment store), and `channel.fetch` (the drain's
//! pull) — sealed end-to-end, then lands on the real encrypted `bridge_caldav_*`
//! store. It drives the production seams unchanged: `FaunaMlsBackend` over
//! `NestConversationsRpc` over an authenticated `NestClient`, and `CalDavClient`
//! over that *same* `NestClient` (the linux/FFI call-site shape).
//!
//! ## Scope: SAME-NEST only (organizer and mailbox-less attendee on one nest)
//!
//! This proves the wired half. The **cross-nest** case (organizer on nest A,
//! mailbox-less attendee on a different, unpaired nest B) is NOT wired today and is
//! deliberately out of scope: the `Scheduling` *welcome* relays cross-nest, but the
//! iMIP *application message* is `channel.send`'d to the organizer's own nest with
//! no foreign-member fan-out, and the recipient's `poll_inbound_scheduling` fetches
//! from its **own** nest — so an unpaired attendee never receives it
//! (`direct-messages.md` § Cross-nest flow: "Unpaired nests — Actor B must check
//! Nest-1 directly; there is no automatic bridging"). That gap is a *federation*
//! concern (it equally affects cross-nest DM messages), tracked separately — see
//! `caldav-server.md` § Implementation status today (Slice 6b). A same-nest
//! CalDAV-only deployment (email off, multiple users) is the
//! real, fully-wired mailbox-less topology this proves.

mod common;
use common::connected_client;
use common::welcome_bytes_from_inbox;

use std::sync::Arc;

use fauna_client_caldav::{
    AttendeeInfo, CalDavClient, DavRecipientKeys, DecodedEventsPage, EventFields, ITipMethod,
    InboundOrigin, InboundReplyOutcome, InboundRequestOutcome, PrincipalResolver, RefusalReason,
    SchedulingApplyOutcome, SchedulingPrincipal, apply_rsvp, build_event_imip,
    epoch_secs_to_ical_utc, imip_reply_for_rsvp, imip_request_for_invite, parse_ical_attendees,
    personal_calendar_id, seal_event_body, uid_hash,
};
use fauna_client_conversations::NestConversationsRpc;
use fauna_conversations::backend::{ConversationsRpc, SchedulingOrigin, SchedulingSink};
use fauna_conversations::backends::fauna_mls::{
    FaunaMlsBackend, ingest_scheduling_welcome, poll_inbound_scheduling,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::bridge_routing::{
    ProvisionCalendarReply, ProvisionCalendarRequest, PutEventCiphertextReply, QueryEventsRequest,
};

const FAR_FUTURE: u64 = u64::MAX / 2;
/// Epoch seconds for DTSTAMP / CREATED on materialized events (test-fixed so the
/// run is deterministic — the crate never reads the wall clock here).
const TS: i64 = 1_700_000_000;
/// The plaintext iCalendar UID; its `uid_hash` is the dedup key that lets the
/// inbound REPLY merge find alice's stored event (and the REQUEST find/own bob's).
const EVENT_UID: &str = "kickoff-mailboxless-1@fauna.test";
/// Per-actor client-held master sealing keys (never sent to the nest; the
/// bridge_caldav store is opaque ciphertext, so any 32 bytes work).
const ALICE_MSEK: [u8; 32] = [0x5a; 32];
const BOB_MSEK: [u8; 32] = [0xb0; 32];

/// One nest serving everything the same-nest scheduling round-trip needs: the
/// auth-bootstrap kinds (the authenticated `NestClient` mints its bearer over
/// `fauna.auth.handshake`), discovery, the conversations data plane (keypackage /
/// welcome / channel send+fetch), and the encrypted CalDAV store handlers.
/// `require_registration` so a client must be a registered actor to auth; the
/// nest's own authority is its `handle_domain`. Returns `(http_base, state,
/// blob_dir)` — the blob-backed handlers rest in blob storage, so the directory
/// has to outlive the test that holds it.
async fn start_nest() -> (String, Arc<AppState>, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let blobs = tempfile::tempdir().unwrap();
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(
            db.clone(),
            None,
            false,
            blobs.path().to_path_buf(),
            None,
        )
        .unwrap(),
    );
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::conversations_handlers::register_conversations_handlers(&mut b);
            fauna_nest::bridge_caldav_handlers::register_bridge_caldav_handlers(&mut b);
            // The succeeded-organizer test: the ceremony's kinds, and the
            // account plane the production sink derives its keys from.
            fauna_nest::recovery_handlers::register_recovery_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        backup_service: Some(backup_svc),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state, blobs)
}

/// Register `actor` as a handled user with `count` published key packages drawn
/// from its own MLS engine (so a welcome built against one can be joined by the
/// same engine).
async fn register_addressable(
    state: &Arc<AppState>,
    actor: &[u8; 32],
    handle: &str,
    engine: &MlsEngine,
    count: usize,
) {
    state
        .db
        .create_user_with_handle(actor, "free", handle, None)
        .await
        .unwrap();
    // Everyone here is a stranger to everyone else, and these tests deliver on
    // the *client-driven* rail, where a `Scheduling` label buys no exemption
    // from the recipient's inbox mode (only the server-side gateway's rail is
    // exempt — `direct-messages.md` § Reach policy → § Scope). `open` is the
    // mode under which a stranger's invite is deliverable at all; what these
    // tests are about is what happens to it AFTER it is delivered.
    state.db.set_inbox_mode(actor, "open").await.unwrap();
    let pkgs = engine
        .generate_key_packages_bytes(count)
        .expect("key packages");
    for (i, pkg) in pkgs.iter().enumerate() {
        state
            .db
            .put_key_package(&format!("{handle}-kp-{i}"), actor, pkg, 0, FAR_FUTURE)
            .await
            .unwrap();
    }
}

/// Capture the raw iMIPs a scheduling drain hands out, so the test can assert the
/// bytes crossed the rail verbatim and then apply them to a `CalDavClient`. (The
/// production `NestSchedulingSink` applies inline; capturing keeps the two halves
/// — "the iMIP crossed" and "the calendar merged" — independently asserted.)
#[derive(Default)]
struct CapturingSink {
    imips: std::sync::Mutex<Vec<(Vec<u8>, SchedulingOrigin)>>,
}

impl CapturingSink {
    /// The one captured iMIP **and the origin the drain reported for it** — the
    /// nest-attested author is what the inbound-mutation rule rests on, so every
    /// test here reads it off the real nest rather than constructing it.
    fn take_one(&self) -> (Vec<u8>, InboundOrigin) {
        let mut g = self.imips.lock().unwrap();
        assert_eq!(g.len(), 1, "exactly one iMIP captured");
        let (imip, origin) = g.remove(0);
        (
            imip,
            InboundOrigin {
                author: origin.author,
                home_nest_url: origin.home_nest_url,
            },
        )
    }
}

#[async_trait::async_trait]
impl SchedulingSink for CapturingSink {
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        origin: SchedulingOrigin,
    ) -> Result<(), String> {
        self.imips.lock().unwrap().push((raw_rfc5322, origin));
        Ok(())
    }
}

/// CAL-ADDRESS → principal for the actors a test registered (all on the one
/// nest, so every home is the recipient's own — the empty URL). Stands in for
/// the anon `by_handle` discovery, which needs DNS the harness has none of.
struct Directory(Vec<(&'static str, ActorId)>);

impl PrincipalResolver for Directory {
    async fn resolve_principal(&self, caladdr: &str) -> Option<SchedulingPrincipal> {
        let addr = caladdr.trim().trim_start_matches("mailto:");
        self.0
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(addr))
            .map(|(_, id)| SchedulingPrincipal {
                actor_id: hex::encode(id.0),
                home_nest_url: String::new(),
            })
    }
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

fn query(actor: &[u8; 32]) -> QueryEventsRequest {
    QueryEventsRequest {
        actor_id: actor.to_vec(),
        calendar_id: personal_calendar_id().to_vec(),
        since_modseq: None,
        after_event_id: None,
        limit: 0,
    }
}

#[tokio::test]
async fn mailbox_less_attendee_receives_and_rsvps_over_the_scheduling_rail_same_nest() {
    let (base, state, _blobs) = start_nest().await;

    // ── Identities: one secret each backs the WS auth keypair AND the local
    //    MLS engine (`ActorKeypair` is not `Clone`, so reconstruct from secret). ──
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    let alice_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(alice_secret)).expect("engine"),
    );

    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));

    // Both addressable: a published key package each (one delivery per direction
    // consumes one — alice→bob REQUEST, bob→alice REPLY; publish a couple each).
    register_addressable(&state, &alice_id.0, "alice", &alice_engine, 2).await;
    register_addressable(&state, &bob_id.0, "bob", &bob_engine, 2).await;

    // Each actor's single authenticated `NestClient` drives BOTH its conversations
    // backend (the scheduling rail) and its CalDavClient (the calendar store) —
    // the production native-client shape.
    let alice_nest = connected_client(&base, ActorKeypair::from_secret(alice_secret)).await;
    let bob_nest = connected_client(&base, ActorKeypair::from_secret(bob_secret)).await;

    let alice_backend = FaunaMlsBackend::new(
        alice_engine.clone(),
        Arc::new(NestConversationsRpc::new(alice_nest.clone())) as Arc<dyn ConversationsRpc>,
        "alice@fauna.test",
        alice_id,
    );
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest.clone())) as Arc<dyn ConversationsRpc>,
        "bob@fauna.test",
        bob_id,
    );
    let alice_caldav = CalDavClient::new(alice_nest.clone());
    let bob_caldav = CalDavClient::new(bob_nest.clone());

    // ── 1. Organizer alice provisions Personal + PUTs the event inviting bob. ──
    let personal = personal_calendar_id();
    let metadata = seal_event_body(b"{\"name\":\"Personal\"}", &ALICE_MSEK).expect("seal metadata");
    let prov = alice_caldav
        .provision_calendar(ProvisionCalendarRequest {
            actor_id: alice_id.0.to_vec(),
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
    let put = alice_caldav
        .seal_and_put_event(
            &alice_id.0,
            &personal,
            &uid_hash(EVENT_UID),
            &ALICE_MSEK,
            &event,
            &invited,
            "alice@fauna.test",
            None,
            TS,
            None,
        )
        .await
        .expect("seal + put ok");
    assert!(matches!(put, PutEventCiphertextReply::Created { .. }));

    // ── 2. alice builds the iMIP REQUEST and delivers it over the scheduling rail
    //    (peer_domain = None = same nest). ──
    let request = imip_request_for_invite(&event, &invited, "alice@fauna.test", TS)
        .expect("a roster yields a REQUEST");
    assert_eq!(request.recipients, vec!["bob@fauna.test".to_string()]);
    alice_backend
        .deliver_scheduling_imip(bob_id, None, request.raw_rfc5322.clone())
        .await
        .expect("deliver REQUEST over the MLS scheduling rail");

    // ── 3. bob ingests the scheduling welcome and drains the iMIP off the REAL
    //    nest channel log (this is the data-plane proof the unit tests can't make). ──
    let bob_inbox = state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(bob_inbox.len(), 1, "exactly one scheduling welcome to bob");
    let welcome = welcome_bytes_from_inbox(&bob_inbox[0].1);
    let bob_channel = ingest_scheduling_welcome(&bob_backend, "", &welcome, "")
        .await
        .expect("bob joins the scheduling group");

    let bob_sink = CapturingSink::default();
    let mut after = 0i64;
    let drained = poll_inbound_scheduling(&bob_backend, &bob_sink, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the scheduling channel");
    assert_eq!(drained, 1, "exactly one iMIP drained off the rail");
    let (captured, origin) = bob_sink.take_one();
    assert_eq!(
        captured, request.raw_rfc5322,
        "the raw RFC 5322 iMIP crossed the sealed MLS rail verbatim"
    );
    assert_eq!(
        origin.author,
        Some(hex::encode(alice_id.0)),
        "the nest attests the record's author — the authenticated `channel.send` caller"
    );
    let directory = Directory(vec![
        ("alice@fauna.test", alice_id),
        ("bob@fauna.test", bob_id),
    ]);

    // ── 4. bob applies the REQUEST → the event materializes in his calendar
    //    (his lazy Personal calendar is auto-provisioned by the apply). ──
    let outcome = bob_caldav
        .apply_inbound_scheduling_from_message(
            &bob_id.0, &BOB_MSEK, &captured, TS, &origin, &directory,
        )
        .await
        .expect("apply inbound REQUEST ok");
    assert!(
        matches!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Created { .. })
        ),
        "the REQUEST materializes a new event, got {outcome:?}"
    );

    let page = bob_caldav
        .query_events_decoded(query(&bob_id.0), &DavRecipientKeys::derive(&BOB_MSEK))
        .await
        .expect("bob query ok");
    let bob_events = match page {
        DecodedEventsPage::Ok { events, .. } => events,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(bob_events.len(), 1, "bob's calendar shows the invite");
    assert!(
        bob_events[0].ics.contains("Project kickoff"),
        "the materialized event carries the organizer's summary"
    );

    // ── 5. bob RSVPs going and delivers the REPLY back over the same rail. ──
    let rw = apply_rsvp(
        &bob_events[0].ics,
        bob_events[0].fauna_ext.as_ref(),
        "bob@fauna.test",
        "going",
    )
    .expect("apply rsvp");
    let reply =
        imip_reply_for_rsvp(&rw, "bob@fauna.test", TS).expect("a reply targets the organizer");
    assert_eq!(reply.recipients, vec!["alice@fauna.test".to_string()]);
    bob_backend
        .deliver_scheduling_imip(alice_id, None, reply.raw_rfc5322.clone())
        .await
        .expect("deliver REPLY over the MLS scheduling rail");

    // ── 6. alice ingests + drains the REPLY off the rail. ──
    let alice_inbox = state.db.list_inbox_all(&alice_id.0).await.unwrap();
    assert_eq!(
        alice_inbox.len(),
        1,
        "exactly one scheduling welcome to alice (the reply)"
    );
    let alice_welcome = welcome_bytes_from_inbox(&alice_inbox[0].1);
    let alice_channel = ingest_scheduling_welcome(&alice_backend, "", &alice_welcome, "")
        .await
        .expect("alice joins the reply scheduling group");
    let alice_sink = CapturingSink::default();
    let mut after_reply = 0i64;
    let drained_reply = poll_inbound_scheduling(
        &alice_backend,
        &alice_sink,
        &alice_channel,
        &mut after_reply,
        0,
    )
    .await
    .expect("alice drains the reply channel");
    assert_eq!(drained_reply, 1, "exactly one REPLY drained off the rail");
    let (captured_reply, reply_origin) = alice_sink.take_one();
    assert_eq!(
        reply_origin.author,
        Some(hex::encode(bob_id.0)),
        "the REPLY's attested author is the replying attendee"
    );

    // ── 7. alice applies the REPLY → bob flips to ACCEPTED on her stored event. ──
    let merge = alice_caldav
        .apply_inbound_scheduling_from_message(
            &alice_id.0,
            &ALICE_MSEK,
            &captured_reply,
            TS + 100,
            &reply_origin,
            &directory,
        )
        .await
        .expect("apply inbound REPLY ok");
    match &merge {
        SchedulingApplyOutcome::Reply(InboundReplyOutcome::Applied {
            uid_hash: uh,
            attendees,
        }) => {
            assert_eq!(uh, &uid_hash(EVENT_UID));
            let bob = attendees
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case("bob@fauna.test"))
                .expect("bob on the merged roster");
            assert_eq!(bob.partstat, "ACCEPTED");
        }
        other => panic!("expected Reply(Applied), got {other:?}"),
    }

    // The merge persisted: a fresh query of alice's calendar shows bob ACCEPTED on
    // the stored (re-sealed) event — exactly what an Apple Calendar reading the
    // same store would render.
    let page = alice_caldav
        .query_events_decoded(query(&alice_id.0), &DavRecipientKeys::derive(&ALICE_MSEK))
        .await
        .expect("alice query ok");
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
        .expect("bob still on alice's stored roster");
    assert_eq!(
        bob.partstat, "ACCEPTED",
        "the inbound REPLY (over the WS-RPC scheduling rail) flipped bob's PARTSTAT \
         on the organizer's stored event"
    );
}

/// **Load-bearing assumption for the MDA server-side gateway (E2E-from-nest
/// design, 2026-06-15).** The MDA fanning a stock organizer's invite to a
/// mailbox-less Fauna attendee must seal the iMIP itself (so the nest sees only
/// ciphertext — the encryption invariant), but it **never holds the organizer's
/// Ed25519 secret** (CalDAV auth is password→capability), so it cannot build the
/// one-off MLS group as the organizer's real MLS identity. This proves the
/// resolution: a scheduling delivery whose MLS sender is an **ephemeral** engine
/// (a fresh throwaway keypair, NOT the organizer's) is still joined + drained +
/// applied by the recipient **verbatim** — the iMIP's `ORGANIZER` field is
/// authoritative, and the receive path ignores the MLS creator credential. The
/// app-level `sender` is stamped as the real organizer (the MDA's
/// `self_actor`), only the MLS signing identity is ephemeral.
///
/// caldav-server.md § Server-side auto-schedule — the MDA-gateway signing-
/// identity resolution. If this ever goes red, the whole E2E-from-nest gateway
/// rests on a false premise.
#[tokio::test]
async fn ephemeral_mls_sender_scheduling_delivery_is_received_and_applied() {
    let (base, state, _blobs) = start_nest().await;

    // Organizer alice: registered + connected only so her authenticated rpc can
    // AUTH the delivery (the caller-scoping to BridgeMda is a separate later
    // step). Her real MLS engine is deliberately NEVER constructed here — the
    // whole point is that the sender does not need it.
    let mut alice_secret = [0u8; 32];
    getrandom::fill(&mut alice_secret).unwrap();
    let alice_id = ActorKeypair::from_secret(alice_secret).actor_id();
    state
        .db
        .create_user_with_handle(&alice_id.0, "free", "alice", None)
        .await
        .unwrap();
    let alice_nest = connected_client(&base, ActorKeypair::from_secret(alice_secret)).await;

    // Mailbox-less recipient bob: a real Fauna user with a real engine + a
    // published key package (he must be addressable for the organizer to add him
    // to the one-off group).
    let mut bob_secret = [0u8; 32];
    getrandom::fill(&mut bob_secret).unwrap();
    let bob_id = ActorKeypair::from_secret(bob_secret).actor_id();
    let bob_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(bob_secret)).expect("engine"));
    register_addressable(&state, &bob_id.0, "bob", &bob_engine, 1).await;
    let bob_nest = connected_client(&base, ActorKeypair::from_secret(bob_secret)).await;
    let bob_backend = FaunaMlsBackend::new(
        bob_engine.clone(),
        Arc::new(NestConversationsRpc::new(bob_nest.clone())) as Arc<dyn ConversationsRpc>,
        "bob@fauna.test",
        bob_id,
    );
    let bob_caldav = CalDavClient::new(bob_nest.clone());

    // ── The ephemeral sender: a fresh throwaway MLS engine (the MDA's stance —
    //    no organizer secret), driving alice's authenticated transport, stamping
    //    alice as the app-level organizer. ──
    let mut ephemeral_secret = [0u8; 32];
    getrandom::fill(&mut ephemeral_secret).unwrap();
    let ephemeral_engine = Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::from_secret(ephemeral_secret)).expect("engine"),
    );
    assert_ne!(
        ActorKeypair::from_secret(ephemeral_secret).actor_id().0,
        alice_id.0,
        "the ephemeral sender identity is genuinely not the organizer's"
    );
    let mda_like_backend = FaunaMlsBackend::new(
        ephemeral_engine,
        Arc::new(NestConversationsRpc::new(alice_nest.clone())) as Arc<dyn ConversationsRpc>,
        "alice@fauna.test",
        alice_id, // app-level sender = the real organizer
    );

    // ── Deliver a REQUEST iMIP (ORGANIZER = alice) over the ephemeral rail. ──
    let event = kickoff_event();
    let invited = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let request = imip_request_for_invite(&event, &invited, "alice@fauna.test", TS)
        .expect("a roster yields a REQUEST");
    mda_like_backend
        .deliver_scheduling_imip(bob_id, None, request.raw_rfc5322.clone())
        .await
        .expect("deliver REQUEST over the ephemeral-sender scheduling rail");

    // ── bob joins + drains: the iMIP crosses verbatim despite the ephemeral
    //    creator credential. ──
    let bob_inbox = state.db.list_inbox_all(&bob_id.0).await.unwrap();
    assert_eq!(bob_inbox.len(), 1, "exactly one scheduling welcome to bob");
    let welcome = welcome_bytes_from_inbox(&bob_inbox[0].1);
    let bob_channel = ingest_scheduling_welcome(&bob_backend, "", &welcome, "")
        .await
        .expect("bob joins the ephemeral-sender scheduling group");
    let bob_sink = CapturingSink::default();
    let mut after = 0i64;
    let drained = poll_inbound_scheduling(&bob_backend, &bob_sink, &bob_channel, &mut after, 0)
        .await
        .expect("bob drains the scheduling channel");
    assert_eq!(drained, 1, "exactly one iMIP drained off the rail");
    let (captured, origin) = bob_sink.take_one();
    assert_eq!(
        captured, request.raw_rfc5322,
        "the iMIP crossed the ephemeral-sender sealed MLS rail verbatim"
    );
    // THE premise the inbound-mutation rule rests on for the gateway rail: the
    // MLS signer is a throwaway, yet the record's nest-attested author is the
    // organizer the nest authenticated the send for.
    assert_eq!(
        origin.author,
        Some(hex::encode(alice_id.0)),
        "an ephemeral-signed delivery is still attested to the organizer"
    );
    let directory = Directory(vec![
        ("alice@fauna.test", alice_id),
        ("bob@fauna.test", bob_id),
    ]);

    // ── bob applies it: the event materializes in his calendar (the gateway's
    //    user-visible effect — a mailbox-less attendee "sees the invite"). ──
    let outcome = bob_caldav
        .apply_inbound_scheduling_from_message(
            &bob_id.0,
            &BOB_MSEK,
            &request.raw_rfc5322,
            TS,
            &origin,
            &directory,
        )
        .await
        .expect("apply inbound REQUEST ok");
    assert!(
        matches!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Created { .. })
        ),
        "the ephemeral-delivered REQUEST materializes a new event, got {outcome:?}"
    );
    let page = bob_caldav
        .query_events_decoded(query(&bob_id.0), &DavRecipientKeys::derive(&BOB_MSEK))
        .await
        .expect("bob query ok");
    let bob_events = match page {
        DecodedEventsPage::Ok { events, .. } => events,
        other => panic!("expected Ok, got {other:?}"),
    };
    assert_eq!(bob_events.len(), 1, "bob's calendar shows the invite");
    assert!(
        bob_events[0].ics.contains("Project kickoff"),
        "the materialized event carries the organizer's summary"
    );
}

/// One actor's full seat: identity, addressable key packages, an authenticated
/// transport, the scheduling backend over it, and the calendar client.
struct Seat {
    id: ActorId,
    nest: Arc<fauna_client::NestClient>,
    backend: FaunaMlsBackend,
    caldav: CalDavClient<Arc<fauna_client::NestClient>>,
}

async fn seat(base: &str, state: &Arc<AppState>, handle: &'static str, packages: usize) -> Seat {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    seat_from_secret(base, state, handle, packages, secret, true).await
}

/// [`seat`] for a chosen identity. `register = false` seats an account the nest
/// already holds — a successor, whose `users` row and handle the succession
/// itself created — so nothing here pre-empts what the ceremony must do.
async fn seat_from_secret(
    base: &str,
    state: &Arc<AppState>,
    handle: &'static str,
    packages: usize,
    secret: [u8; 32],
    register: bool,
) -> Seat {
    let id = ActorKeypair::from_secret(secret).actor_id();
    let engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).expect("engine"));
    if register {
        register_addressable(state, &id.0, handle, &engine, packages).await;
    }
    let nest = connected_client(base, ActorKeypair::from_secret(secret)).await;
    let backend = FaunaMlsBackend::new(
        engine,
        Arc::new(NestConversationsRpc::new(nest.clone())) as Arc<dyn ConversationsRpc>,
        format!("{handle}@fauna.test"),
        id,
    );
    Seat {
        id,
        caldav: CalDavClient::new(Arc::clone(&nest)),
        nest,
        backend,
    }
}

/// Join + drain the scheduling welcome(s) `to` has not seen yet (`seen` holds
/// the inbox row ids already consumed) and return the one iMIP with the origin
/// the real nest reported for it.
async fn receive_one(
    state: &Arc<AppState>,
    to: &Seat,
    seen: &mut Vec<i64>,
) -> (Vec<u8>, InboundOrigin) {
    let sink = CapturingSink::default();
    receive_into(state, to, seen, &sink).await;
    sink.take_one()
}

/// [`receive_one`]'s join + drain, into a caller-chosen sink — the production
/// `NestSchedulingSink` included.
async fn receive_into(
    state: &Arc<AppState>,
    to: &Seat,
    seen: &mut Vec<i64>,
    sink: &dyn SchedulingSink,
) {
    let inbox = state.db.list_inbox_all(&to.id.0).await.unwrap();
    let fresh: Vec<_> = inbox.iter().filter(|row| !seen.contains(&row.0)).collect();
    assert_eq!(fresh.len(), 1, "exactly one new scheduling welcome");
    seen.push(fresh[0].0);
    let welcome = welcome_bytes_from_inbox(&fresh[0].1);
    let channel = ingest_scheduling_welcome(&to.backend, "", &welcome, "")
        .await
        .expect("join the scheduling group");
    let mut after = 0i64;
    let drained = poll_inbound_scheduling(&to.backend, sink, &channel, &mut after, 0)
        .await
        .expect("drain the scheduling channel");
    assert_eq!(drained, 1);
}

async fn events_of(seat: &Seat, msek: &[u8; 32]) -> usize {
    match seat
        .caldav
        .query_events_decoded(query(&seat.id.0), &DavRecipientKeys::derive(msek))
        .await
        .expect("query ok")
    {
        DecodedEventsPage::Ok { events, .. } => events.len(),
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// **The hole this file used to leave open** (caldav-server.md § Who may mutate
/// an existing event over the inbound rail). mallory is a co-attendee: she
/// holds the event's UID and the organizer's address, so her `CANCEL` and her
/// rewriting `REQUEST` are *byte-identical* to ones alice could send. They
/// travel the same stranger-reachable rail, are joined and drained like any
/// other — and the only thing that differs is what the nest attests about who
/// posted them. bob's event must survive both, and alice's own `CANCEL` must
/// still remove it (no false positive).
#[tokio::test]
async fn a_non_organizer_cannot_cancel_or_rewrite_the_victims_event() {
    let (base, state, _blobs) = start_nest().await;
    let alice = seat(&base, &state, "alice", 0).await;
    let bob = seat(&base, &state, "bob", 4).await;
    let mallory = seat(&base, &state, "mallory", 0).await;
    let directory = Directory(vec![
        ("alice@fauna.test", alice.id),
        ("bob@fauna.test", bob.id),
        ("mallory@fauna.test", mallory.id),
    ]);
    let mut seen = Vec::new();

    let event = kickoff_event();
    let roster = vec![
        AttendeeInfo {
            name: "Bob".into(),
            email: "bob@fauna.test".into(),
            partstat: "NEEDS-ACTION".into(),
            fauna_status: "invited".into(),
        },
        AttendeeInfo {
            name: "Mallory".into(),
            email: "mallory@fauna.test".into(),
            partstat: "NEEDS-ACTION".into(),
            fauna_status: "invited".into(),
        },
    ];
    let stamp = epoch_secs_to_ical_utc(TS);
    let imip = |method| {
        build_event_imip(method, &event, &roster, "alice@fauna.test", &stamp)
            .expect("a roster yields an iMIP")
            .raw_rfc5322
    };

    // alice invites; bob's client materializes the event and binds it to her.
    alice
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Request))
        .await
        .expect("alice delivers the REQUEST");
    let (request, origin) = receive_one(&state, &bob, &mut seen).await;
    let created = bob
        .caldav
        .apply_inbound_scheduling_from_message(
            &bob.id.0, &BOB_MSEK, &request, TS, &origin, &directory,
        )
        .await
        .expect("apply REQUEST");
    assert!(matches!(
        created,
        SchedulingApplyOutcome::Request(InboundRequestOutcome::Created { .. })
    ));
    assert_eq!(events_of(&bob, &BOB_MSEK).await, 1);

    // mallory replays alice's messages over the same rail.
    for method in [ITipMethod::Cancel, ITipMethod::Request] {
        mallory
            .backend
            .deliver_scheduling_imip(bob.id, None, imip(method))
            .await
            .expect("the rail is stranger-reachable: mallory's delivery is accepted");
        let (forged, origin) = receive_one(&state, &bob, &mut seen).await;
        assert_eq!(
            origin.author,
            Some(hex::encode(mallory.id.0)),
            "the nest attests mallory, whatever the .ics claims"
        );
        let outcome = bob
            .caldav
            .apply_inbound_scheduling_from_message(
                &bob.id.0,
                &BOB_MSEK,
                &forged,
                TS + 10,
                &origin,
                &directory,
            )
            .await
            .expect("a refusal is an outcome, not an error");
        assert!(
            matches!(
                outcome,
                SchedulingApplyOutcome::Request(InboundRequestOutcome::Refused {
                    reason: RefusalReason::NotTheOrganizer,
                    ..
                })
            ),
            "mallory's message is refused, got {outcome:?}"
        );
        assert_eq!(events_of(&bob, &BOB_MSEK).await, 1, "bob's event survives");
    }

    // alice's own CANCEL still works.
    alice
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("alice delivers the CANCEL");
    let (cancel, origin) = receive_one(&state, &bob, &mut seen).await;
    let outcome = bob
        .caldav
        .apply_inbound_scheduling_from_message(
            &bob.id.0,
            &BOB_MSEK,
            &cancel,
            TS + 20,
            &origin,
            &directory,
        )
        .await
        .expect("apply CANCEL");
    assert!(
        matches!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Cancelled { .. })
        ),
        "the organizer's own CANCEL is applied, got {outcome:?}"
    );
    assert_eq!(events_of(&bob, &BOB_MSEK).await, 0);
}

/// **A succeeded organizer can still cancel — end to end, through the SHIPPED
/// sink** (caldav-server.md § Who may mutate an existing event over the inbound
/// rail → *A succeeded organizer*). The library rule is pinned in
/// `fauna-client-caldav` against a scripted resolver; what that cannot show is
/// that anything in a shipped app ever *answers* the question. So bob's side
/// here is the production `NestSchedulingSink`, nothing injected: its resolver
/// must dial bob's own nest (the binding's blank home) anonymously, fetch
/// alice's succession statement, verify it against her registration chain, and
/// only then let the successor's `CANCEL` through. A stranger's byte-identical
/// `CANCEL` sent after the same succession must still leave the event alone —
/// the walk admits where the account ended up, not whoever asks. And one sent
/// BEFORE the succession must not pin its "never succeeded" answer for the
/// session: the successor's `CANCEL` is honoured
/// by the same sink afterwards.
#[tokio::test]
async fn a_succeeded_organizers_cancel_is_honoured_through_the_production_sink() {
    use fauna_client_conversations::NestSchedulingSink;

    let (base, state, _blobs) = start_nest().await;
    let alice_secret = [0xa1u8; 32];
    let alice = seat_from_secret(&base, &state, "alice", 0, alice_secret, true).await;
    // One key package per delivery bob receives: the REQUEST and four CANCELs.
    let bob = seat(&base, &state, "bob", 5).await;
    let mallory = seat(&base, &state, "mallory", 0).await;
    let directory = Directory(vec![
        ("alice@fauna.test", alice.id),
        ("bob@fauna.test", bob.id),
    ]);
    let mut seen = Vec::new();

    // The production sink derives its keys from bob's mail custody, so his
    // calendar's MSEK rests where an app would have put it.
    let bob_mail: Arc<dyn fauna_client_config::MailStore> = Arc::new(
        fauna_client_config::test_helpers::FakeMailStore::with(&fauna_core::data::MailConfig {
            msek: Some(BOB_MSEK.into()),
            ..Default::default()
        }),
    );
    // No account store here, so the refused-change inbox holds each notice
    // the sink records — the witness that a refusal is reported, not only
    // refused.
    let refused = Arc::new(fauna_conversations::refused_changes::RefusedChangeInbox::default());
    // The walk hands the held chain head from the session manager's
    // peer-anchor store; an empty store is bob's genuine first contact with
    // alice's chain (no store at all would answer "not asked").
    let manager = fauna_conversations::ConversationsManager::new();
    manager.register_peer_anchor_store(Some(Arc::new(
        fauna_conversations::backend::MemoryPeerAnchorStore::default(),
    )));
    let production = NestSchedulingSink::new(
        Arc::clone(&bob.nest),
        Arc::clone(&bob_mail),
        Arc::downgrade(&manager),
        Arc::clone(&refused),
    );

    let event = kickoff_event();
    let roster = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@fauna.test".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let stamp = epoch_secs_to_ical_utc(TS);
    let imip = |method| {
        build_event_imip(method, &event, &roster, "alice@fauna.test", &stamp)
            .expect("a roster yields an iMIP")
            .raw_rfc5322
    };

    // alice invites; bob's client materializes the event and binds it to her.
    // (Creation resolves an ADDRESS, which needs DNS the harness has none of —
    // so this one apply keeps the directory. Every mutation below is the
    // production sink's.)
    alice
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Request))
        .await
        .expect("alice delivers the REQUEST");
    let (request, origin) = receive_one(&state, &bob, &mut seen).await;
    bob.caldav
        .apply_inbound_scheduling_from_message(
            &bob.id.0, &BOB_MSEK, &request, TS, &origin, &directory,
        )
        .await
        .expect("apply REQUEST");
    assert_eq!(events_of(&bob, &BOB_MSEK).await, 1);

    // A stranger's CANCEL BEFORE any succession: refused on the bound nest's
    // definitive "never succeeded" — which the sink must not remember, or the
    // successor's CANCEL below would meet it for the rest of the session.
    mallory
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("the rail is stranger-reachable");
    receive_into(&state, &bob, &mut seen, &production).await;
    assert_eq!(
        events_of(&bob, &BOB_MSEK).await,
        1,
        "an un-succeeded organizer does not make mallory the organizer"
    );
    let [notice]: [_; 1] = refused
        .held()
        .open()
        .try_into()
        .expect("the refused CANCEL is recorded as one notice");
    assert_eq!(notice.method, "CANCEL");
    assert_eq!(notice.reason, "not_the_organizer");

    // alice registers a RecoveryKey, then succeeds her identity on her home
    // nest — the real ceremony kinds, over the anonymous door.
    let alice_seed = ed25519_dalek::SigningKey::from_bytes(&alice_secret);
    let recovery = fauna_core::recovery::RecoveryKey::from_bytes([0x22; 32]);
    common::register_recovery_key(
        &state.rpc_router,
        &state,
        &alice_seed,
        alice.id.0,
        &recovery,
        None,
        1,
    )
    .await;
    let successor_secret = [0xa2u8; 32];
    let successor_seed = ed25519_dalek::SigningKey::from_bytes(&successor_secret);
    common::submit_succession(
        &state.rpc_router,
        &state,
        common::succession_bytes(&recovery, alice.id.0, &successor_seed, None, 2),
    )
    .await
    .expect("the succession lands on alice's home nest");
    let successor = seat_from_secret(&base, &state, "alice", 0, successor_secret, false).await;
    assert_ne!(successor.id, alice.id);

    // A stranger's CANCEL, after the succession: still refused — asked afresh,
    // since the earlier "never succeeded" was not remembered. The sink
    // reports a refusal as a log line, so the calendar is the witness.
    mallory
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("the rail is stranger-reachable");
    receive_into(&state, &bob, &mut seen, &production).await;
    assert_eq!(
        events_of(&bob, &BOB_MSEK).await,
        1,
        "a succession somewhere does not make mallory the organizer"
    );

    // mallory again: the sink now answers from its session memo (one lookup
    // per bound identity per session) — and the remembered answer is still
    // where alice ended up, which is not mallory.
    mallory
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("the rail is stranger-reachable");
    receive_into(&state, &bob, &mut seen, &production).await;
    assert_eq!(
        events_of(&bob, &BOB_MSEK).await,
        1,
        "a remembered succession does not make mallory the organizer either"
    );

    // The successor's CANCEL: the nest attests an id the binding has never
    // seen, and only the verified walk — here the one mallory's post-succession
    // CANCEL paid for, remembered — can connect the two.
    successor
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("the successor delivers the CANCEL");
    receive_into(&state, &bob, &mut seen, &production).await;
    assert_eq!(
        events_of(&bob, &BOB_MSEK).await,
        0,
        "the succeeded organizer's CANCEL removes the event"
    );
}

/// The shipped anon dial speaks TLS; this harness's nest serves plain HTTP.
/// So the one thing this connector changes is the scheme — the discovery body
/// it drives is the production `AnonDiscovery`, against the real nest's
/// `fauna.actor.by_handle` and `fauna.nest.info` handlers.
struct PlainHttpConnector;

impl fauna_client_caldav::AnonConnector for PlainHttpConnector {
    type Client = fauna_anon_client::AnonymousNestClient;

    async fn connect(
        &self,
        base_url: &str,
    ) -> Result<Self::Client, fauna_anon_client::AnonClientError> {
        let url = base_url.replacen("https://", "http://", 1);
        fauna_anon_client::AnonymousNestClient::connect(&url).await
    }
}

/// **An unbound event, a client that reaches its nest by another name**
/// (caldav-server.md § Who may mutate an existing event over the inbound rail;
/// the shared `DiscoveryPrincipalResolver` every app now hands the apply). An
/// event with no binding is mutable only by whoever its stored `ORGANIZER`
/// resolves to *now*, so discovery must answer — and the recipient's own nest
/// must be recognised as such however this client spells its URL. bob reaches
/// his nest as `localhost` while alice's handle domain names it `127.0.0.1`:
/// resolving by URL spelling would leave alice's principal homed at a URL the
/// rail never reports for a same-nest channel, and her own `CANCEL` would be
/// refused. The nest's identity (`fauna.nest.info`) is what matches.
#[tokio::test]
async fn an_unbound_event_answers_to_its_organizer_through_discovery_whatever_url_reaches_the_nest()
{
    let (base, state, _blobs) = start_nest().await;
    let alice = seat(&base, &state, "alice", 0).await;
    let bob = seat(&base, &state, "bob", 4).await;
    let mallory = seat(&base, &state, "mallory", 0).await;
    let mut seen = Vec::new();

    // Handles live at the nest's own authority — `127.0.0.1:<port>`.
    let authority = base.trim_start_matches("http://").to_string();
    let organizer = format!("alice@{authority}");
    let discovery = fauna_client_caldav::DiscoveryPrincipalResolver {
        discovery: fauna_client_caldav::AnonDiscovery(PlainHttpConnector),
        // The LAN-style spelling: the same nest, not the same string.
        own_nest_url: base.replacen("127.0.0.1", "localhost", 1),
    };

    let event = kickoff_event();
    let roster = vec![AttendeeInfo {
        name: "Bob".into(),
        email: format!("bob@{authority}"),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];
    let stamp = epoch_secs_to_ical_utc(TS);
    let imip = |method| {
        build_event_imip(method, &event, &roster, &organizer, &stamp)
            .expect("a roster yields an iMIP")
            .raw_rfc5322
    };

    // The event lands UNBOUND: its record carries no attested author, as one
    // written before the nest stored authors does. Creation is open.
    alice
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Request))
        .await
        .expect("alice delivers the REQUEST");
    let (request, origin) = receive_one(&state, &bob, &mut seen).await;
    let created = bob
        .caldav
        .apply_inbound_scheduling_from_message(
            &bob.id.0,
            &BOB_MSEK,
            &request,
            TS,
            &InboundOrigin {
                author: None,
                ..origin
            },
            &discovery,
        )
        .await
        .expect("apply REQUEST");
    assert!(matches!(
        created,
        SchedulingApplyOutcome::Request(InboundRequestOutcome::Created { .. })
    ));

    // The discovery answer itself: alice, on bob's OWN nest.
    assert_eq!(
        discovery.resolve_principal(&organizer).await,
        Some(SchedulingPrincipal {
            actor_id: hex::encode(alice.id.0),
            home_nest_url: String::new(),
        }),
        "the nest that answered for alice's domain is bob's own nest — by identity"
    );

    // mallory's byte-identical CANCEL: alice's address resolves to alice, not her.
    mallory
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("the rail is stranger-reachable");
    let (forged, origin) = receive_one(&state, &bob, &mut seen).await;
    let outcome = bob
        .caldav
        .apply_inbound_scheduling_from_message(
            &bob.id.0,
            &BOB_MSEK,
            &forged,
            TS + 10,
            &origin,
            &discovery,
        )
        .await
        .expect("a refusal is an outcome, not an error");
    assert!(
        matches!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Refused {
                reason: RefusalReason::NotTheOrganizer,
                ..
            })
        ),
        "mallory's CANCEL is refused, got {outcome:?}"
    );
    assert_eq!(events_of(&bob, &BOB_MSEK).await, 1, "bob's event survives");

    // alice's own CANCEL: her address resolves to exactly her origin.
    alice
        .backend
        .deliver_scheduling_imip(bob.id, None, imip(ITipMethod::Cancel))
        .await
        .expect("alice delivers the CANCEL");
    let (cancel, origin) = receive_one(&state, &bob, &mut seen).await;
    let outcome = bob
        .caldav
        .apply_inbound_scheduling_from_message(
            &bob.id.0,
            &BOB_MSEK,
            &cancel,
            TS + 20,
            &origin,
            &discovery,
        )
        .await
        .expect("apply CANCEL");
    assert!(
        matches!(
            outcome,
            SchedulingApplyOutcome::Request(InboundRequestOutcome::Cancelled { .. })
        ),
        "the organizer's own CANCEL is applied on an unbound event, got {outcome:?}"
    );
    assert_eq!(events_of(&bob, &BOB_MSEK).await, 0);
}
