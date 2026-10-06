//! Cross-surface contract: the canonical iCalendar **writer**
//! (`fauna_core::ical::generate_ical`, WASM-safe) must emit bytes the MDA's
//! crate-backed **parser** (`fauna_mail::icalendar::parse_icalendar`) accepts
//! and reads faithfully.
//!
//! Why this is load-bearing: the CalDAV flow Decision B builds is
//! Fauna-app → `generate_ical` (fauna-core) → seal → nest store → MDA
//! serves verbatim → on a `time-range` REPORT the MDA re-parses via
//! `parse_icalendar` (fauna-mail) + `expand_recurrence`. The two iCalendar
//! surfaces are independent implementations (fauna-core hand-rolls RFC 5545 to
//! stay WASM-safe; fauna-mail wraps the `icalendar` crate for the MDA), and
//! were never cross-validated. If the writer emits anything the parser
//! rejects or misreads, the whole chain breaks at the MDA — so this test pins
//! the contract every later Decision B step sits on.
//!
//! See `docs/goal/behavior/caldav-server.md` § Where logic lives /
//! § iCalendar parsing rules and `docs/goal/ui/events.md` § Where logic lives.

use fauna_core::ical::{AttendeeInfo, EventFields, ITipMethod, generate_ical, generate_itip};
use fauna_mail::icalendar::{ICalComponent, ICalDocument, expand_recurrence, parse_icalendar};

/// Fetch the first VEVENT component from a parsed VCALENDAR tree.
fn first_vevent(ics: &str) -> ICalComponent {
    let doc = parse_icalendar(ics.as_bytes())
        .expect("the MDA parser must accept the canonical writer's output");
    doc.components
        .into_iter()
        .find(|c| c.name.eq_ignore_ascii_case("VEVENT"))
        .expect("writer output must contain a VEVENT the MDA parser recognises")
}

/// Parse a full VCALENDAR (keeps the top-level VCALENDAR properties, e.g.
/// `METHOD`, which `first_vevent` discards).
fn parse_doc(ics: &str) -> ICalDocument {
    parse_icalendar(ics.as_bytes())
        .expect("the MDA parser must accept the canonical writer's output")
}

fn prop<'a>(comp: &'a ICalComponent, name: &str) -> Option<&'a str> {
    comp.properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(|p| p.value.as_str())
}

#[test]
fn writer_output_parses_through_mda_parser() {
    let event = EventFields {
        summary: "Team Meeting".into(),
        dtstart: "2026-04-01T10:00:00Z".into(),
        dtend: "2026-04-01T11:00:00Z".into(),
        location: "Room 42".into(),
        uid: "contract-1@fauna".into(),
        description: "Weekly sync".into(),
        ..Default::default()
    };

    let ics = generate_ical(&event, &[], "alice@example.com");
    let vevent = first_vevent(&ics);

    assert_eq!(prop(&vevent, "UID"), Some("contract-1@fauna"));
    assert_eq!(prop(&vevent, "SUMMARY"), Some("Team Meeting"));
    assert_eq!(prop(&vevent, "DTSTART"), Some("20260401T100000Z"));
    assert_eq!(prop(&vevent, "DTEND"), Some("20260401T110000Z"));
    assert_eq!(prop(&vevent, "LOCATION"), Some("Room 42"));
    assert_eq!(prop(&vevent, "DESCRIPTION"), Some("Weekly sync"));
    assert_eq!(prop(&vevent, "ORGANIZER"), Some("mailto:alice@example.com"));
}

#[test]
fn writer_attendee_roster_parses_through_mda_parser() {
    // ORGANIZER = author; ATTENDEE;PARTSTAT = roster — both must survive into
    // the canonical VEVENT the MDA serves (caldav-server.md § Scheduling).
    let event = EventFields {
        summary: "Planning".into(),
        dtstart: "2026-04-01T18:00:00Z".into(),
        uid: "roster-1@fauna".into(),
        ..Default::default()
    };
    let attendees = vec![
        AttendeeInfo {
            name: "Bob".into(),
            email: "bob@example.com".into(),
            partstat: "ACCEPTED".into(),
            fauna_status: "going".into(),
        },
        AttendeeInfo {
            name: "Carol".into(),
            email: "carol@example.com".into(),
            partstat: "TENTATIVE".into(),
            fauna_status: "interested".into(),
        },
    ];

    let ics = generate_ical(&event, &attendees, "alice@example.com");
    let vevent = first_vevent(&ics);

    let attendees_parsed: Vec<_> = vevent
        .properties
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("ATTENDEE"))
        .collect();
    assert_eq!(attendees_parsed.len(), 2, "both attendees survive");

    // PARTSTAT rides as a parameter on the ATTENDEE property.
    let partstats: Vec<String> = attendees_parsed
        .iter()
        .map(|a| {
            a.parameters
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case("PARTSTAT"))
                .map(|p| p.value.to_ascii_uppercase())
                .unwrap_or_default()
        })
        .collect();
    assert!(partstats.contains(&"ACCEPTED".to_string()));
    assert!(
        partstats.contains(&"TENTATIVE".to_string()),
        "interested projects to PARTSTAT=TENTATIVE on the wire"
    );
}

#[test]
fn writer_rrule_event_expands_through_mda() {
    // The MDA's time-range REPORT filter runs `expand_recurrence` over the
    // writer's RRULE output. A daily event over a 7-day window → 7 instances.
    let event = EventFields {
        summary: "Daily Standup".into(),
        dtstart: "2026-06-01T10:00:00Z".into(),
        dtend: "2026-06-01T10:15:00Z".into(),
        uid: "rrule-contract@fauna".into(),
        rrule: "FREQ=DAILY".into(),
        ..Default::default()
    };

    let ics = generate_ical(&event, &[], "");
    let vevent = first_vevent(&ics);
    assert_eq!(prop(&vevent, "RRULE"), Some("FREQ=DAILY"));

    // [2026-06-01T00:00Z, 2026-06-08T00:00Z) epoch seconds.
    let window_start = 1_780_272_000; // 2026-06-01T00:00:00Z
    let window_end = window_start + 7 * 86_400;
    let occ = expand_recurrence(&vevent, window_start, window_end)
        .expect("the writer's RRULE must expand through the MDA path");
    assert_eq!(
        occ.len(),
        7,
        "seven daily occurrences in a seven-day window"
    );
    // 15-minute duration preserved across expansion.
    for o in &occ {
        assert_eq!(o.dtend - o.dtstart, 900);
    }
}

#[test]
fn writer_special_chars_survive_mda_parser() {
    // RFC 5545 TEXT escaping (`\ ; , \n`) emitted by the writer must be
    // accepted by the crate-backed parser without choking.
    let event = EventFields {
        summary: "Sync; with, special\\chars".into(),
        dtstart: "2026-04-01T10:00:00Z".into(),
        uid: "special-contract@fauna".into(),
        description: "Line one\nLine two".into(),
        ..Default::default()
    };

    let ics = generate_ical(&event, &[], "");
    // Must parse without error; the value stays escaped at the low-level tree
    // (the parser preserves raw property values — unescaping is the high-level
    // surface's job), so we assert structural acceptance + UID fidelity.
    let vevent = first_vevent(&ics);
    assert_eq!(prop(&vevent, "UID"), Some("special-contract@fauna"));
    assert!(
        prop(&vevent, "SUMMARY").is_some(),
        "escaped SUMMARY survives parsing"
    );
}

#[test]
fn itip_request_message_parses_through_mda_parser() {
    // The iMIP REQUEST the client dispatches (and the MDA gateway will later
    // build) must parse through the MDA parser: METHOD:REQUEST at the
    // VCALENDAR level, DTSTAMP + the full roster inside the VEVENT
    // (caldav-server.md § Scheduling & invitations / § Where logic lives).
    let event = EventFields {
        summary: "Project kickoff".into(),
        dtstart: "2026-07-01T15:00:00Z".into(),
        dtend: "2026-07-01T16:00:00Z".into(),
        uid: "itip-req@fauna".into(),
        ..Default::default()
    };
    let attendees = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@example.com".into(),
        partstat: "NEEDS-ACTION".into(),
        fauna_status: "invited".into(),
    }];

    let ics = generate_itip(
        ITipMethod::Request,
        &event,
        &attendees,
        "alice@example.com",
        "2026-06-04T12:00:00Z",
    );

    // METHOD is a top-level VCALENDAR property.
    let doc = parse_doc(&ics);
    let method = doc
        .properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("METHOD"))
        .map(|p| p.value.to_ascii_uppercase());
    assert_eq!(method.as_deref(), Some("REQUEST"));

    // DTSTAMP + UID + ORGANIZER survive in the VEVENT the recipient parses.
    let vevent = first_vevent(&ics);
    assert_eq!(prop(&vevent, "UID"), Some("itip-req@fauna"));
    assert_eq!(prop(&vevent, "DTSTAMP"), Some("20260604T120000Z"));
    assert_eq!(prop(&vevent, "ORGANIZER"), Some("mailto:alice@example.com"));
    assert_eq!(
        vevent
            .properties
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case("ATTENDEE"))
            .count(),
        1,
        "the roster rides in the REQUEST"
    );
}

#[test]
fn itip_reply_message_parses_through_mda_parser() {
    // The attendee's REPLY must parse: METHOD:REPLY + the single responding
    // ATTENDEE with their PARTSTAT + REQUEST-STATUS.
    let event = EventFields {
        uid: "itip-req@fauna".into(),
        dtstart: "2026-07-01T15:00:00Z".into(),
        ..Default::default()
    };
    let responder = vec![AttendeeInfo {
        name: "Bob".into(),
        email: "bob@example.com".into(),
        partstat: "ACCEPTED".into(),
        fauna_status: "going".into(),
    }];

    let ics = generate_itip(
        ITipMethod::Reply,
        &event,
        &responder,
        "alice@example.com",
        "2026-06-04T12:30:00Z",
    );

    let doc = parse_doc(&ics);
    let method = doc
        .properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("METHOD"))
        .map(|p| p.value.to_ascii_uppercase());
    assert_eq!(method.as_deref(), Some("REPLY"));

    let vevent = first_vevent(&ics);
    assert_eq!(prop(&vevent, "REQUEST-STATUS"), Some("2.0;Success"));
    let attendees: Vec<_> = vevent
        .properties
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case("ATTENDEE"))
        .collect();
    assert_eq!(attendees.len(), 1, "a REPLY carries only the responder");
    assert_eq!(attendees[0].value, "mailto:bob@example.com");
}
