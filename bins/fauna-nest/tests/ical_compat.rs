//! Compatibility tests for iCalendar parsing across major calendar systems.
//!
//! Each fixture contains realistic `.ics` content matching the quirks and
//! properties of a specific calendar vendor. Tests verify parsing + key-field
//! extraction against the shared `fauna_core::ical::parse_ical_multi` parser.
//! (The legacy plaintext `db::calendar`-store import/export round-trip tests
//! were removed with the § 4c legacy-calendar retirement; the parser/writer are
//! now exercised end-to-end by the encrypted-path `conformance_caldav_*` tests
//! and the `fauna-client-caldav` crate's `generate_ical`/`parse` round-trips.)

use fauna_core::ical::{EventFields, parse_ical_multi};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const GOOGLE_ICS: &str = "\
BEGIN:VCALENDAR\r\n\
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r\n\
VERSION:2.0\r\n\
CALSCALE:GREGORIAN\r\n\
METHOD:PUBLISH\r\n\
X-WR-CALNAME:Work\r\n\
X-WR-TIMEZONE:America/New_York\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:America/New_York\r\n\
BEGIN:DAYLIGHT\r\n\
TZOFFSETFROM:-0500\r\n\
TZOFFSETTO:-0400\r\n\
TZNAME:EDT\r\n\
DTSTART:19700308T020000\r\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\n\
END:DAYLIGHT\r\n\
BEGIN:STANDARD\r\n\
TZOFFSETFROM:-0400\r\n\
TZOFFSETTO:-0500\r\n\
TZNAME:EST\r\n\
DTSTART:19701101T020000\r\n\
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\n\
END:STANDARD\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=America/New_York:20260315T090000\r\n\
DTEND;TZID=America/New_York:20260315T100000\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r\n\
EXDATE;TZID=America/New_York:20260320T090000\r\n\
DTSTAMP:20260310T120000Z\r\n\
UID:google-standup-001@google.com\r\n\
SUMMARY:Daily Standup\r\n\
DESCRIPTION:Quick sync with the team\r\n\
LOCATION:Conference Room B\r\n\
STATUS:CONFIRMED\r\n\
CATEGORIES:Work,Meetings\r\n\
ATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n\
ATTENDEE;CN=Bob;PARTSTAT=TENTATIVE:mailto:bob@example.com\r\n\
SEQUENCE:2\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;VALUE=DATE:20260401\r\n\
DTEND;VALUE=DATE:20260402\r\n\
DTSTAMP:20260310T120000Z\r\n\
UID:google-allday-002@google.com\r\n\
SUMMARY:Company Holiday\r\n\
DESCRIPTION:Office closed\r\n\
STATUS:CONFIRMED\r\n\
CATEGORIES:Holiday\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=America/New_York:20260320T140000\r\n\
DTEND;TZID=America/New_York:20260320T160000\r\n\
DTSTAMP:20260310T120000Z\r\n\
UID:google-review-003@google.com\r\n\
SUMMARY:Q1 Review\r\n\
DESCRIPTION:Quarterly business review meeting\r\n\
LOCATION:Main Hall\r\n\
STATUS:CONFIRMED\r\n\
SEQUENCE:1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=America/New_York:20260325T120000\r\n\
DTEND;TZID=America/New_York:20260325T130000\r\n\
DTSTAMP:20260310T120000Z\r\n\
UID:google-lunch-004@google.com\r\n\
SUMMARY:Team Lunch\r\n\
LOCATION:Downtown Cafe\r\n\
CATEGORIES:Social\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=America/New_York:20260330T100000\r\n\
DTEND;TZID=America/New_York:20260330T103000\r\n\
DTSTAMP:20260310T120000Z\r\n\
UID:google-1on1-005@google.com\r\n\
SUMMARY:1:1 with Manager\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO\r\n\
DESCRIPTION:Weekly check-in\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

const APPLE_ICS: &str = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//Apple Inc.//macOS 14.3//EN\r\n\
CALSCALE:GREGORIAN\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260318T183000\r\n\
DTEND:20260318T200000\r\n\
DTSTAMP:20260310T080000Z\r\n\
UID:apple-dinner-001@icloud.com\r\n\
SUMMARY:Dinner with Sarah\r\n\
LOCATION:Chez Marie\r\n\
DESCRIPTION:Reservation under my name\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
DESCRIPTION:Reminder\r\n\
TRIGGER:-PT1H\r\n\
END:VALARM\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
DESCRIPTION:Second Reminder\r\n\
TRIGGER:-PT15M\r\n\
END:VALARM\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;VALUE=DATE:20260322\r\n\
DTEND;VALUE=DATE:20260323\r\n\
DTSTAMP:20260310T080000Z\r\n\
UID:apple-birthday-002@icloud.com\r\n\
SUMMARY:Mom's Birthday\r\n\
DESCRIPTION:Don't forget the flowers!\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
DESCRIPTION:Birthday reminder\r\n\
TRIGGER:-P1D\r\n\
END:VALARM\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260325T070000\r\n\
DTEND:20260325T080000\r\n\
DTSTAMP:20260310T080000Z\r\n\
UID:apple-gym-003@icloud.com\r\n\
SUMMARY:Gym Session\r\n\
LOCATION:FitLife Downtown\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
DESCRIPTION:Time to work out\r\n\
TRIGGER:-PT30M\r\n\
END:VALARM\r\n\
SEQUENCE:1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260328T140000\r\n\
DTEND:20260328T150000\r\n\
DTSTAMP:20260310T080000Z\r\n\
UID:apple-dentist-004@icloud.com\r\n\
SUMMARY:Dentist Appointment\r\n\
ATTENDEE;CN=\"Dr. O'Brien\":mailto:dentist@clinic.example.com\r\n\
LOCATION:Smile Dental Clinic\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

const OUTLOOK_ICS: &str = "\
BEGIN:VCALENDAR\r\n\
METHOD:PUBLISH\r\n\
PRODID:-//Microsoft Corporation//Outlook 16.0//EN\r\n\
VERSION:2.0\r\n\
X-WR-CALNAME:Calendar\r\n\
BEGIN:VTIMEZONE\r\n\
TZID:Eastern Standard Time\r\n\
BEGIN:STANDARD\r\n\
DTSTART:16011104T020000\r\n\
RRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\r\n\
TZOFFSETFROM:-0400\r\n\
TZOFFSETTO:-0500\r\n\
END:STANDARD\r\n\
BEGIN:DAYLIGHT\r\n\
DTSTART:16010311T020000\r\n\
RRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\r\n\
TZOFFSETFROM:-0500\r\n\
TZOFFSETTO:-0400\r\n\
END:DAYLIGHT\r\n\
END:VTIMEZONE\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=\"Eastern Standard Time\":20260316T090000\r\n\
DTEND;TZID=\"Eastern Standard Time\":20260316T093000\r\n\
DTSTAMP:20260310T140000Z\r\n\
UID:outlook-sync-001@outlook.com\r\n\
SUMMARY:Sprint Planning\r\n\
DESCRIPTION:Two-week sprint planning session\r\n\
LOCATION:Teams Meeting\r\n\
STATUS:CONFIRMED\r\n\
X-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\n\
X-MICROSOFT-CDO-IMPORTANCE:1\r\n\
SEQUENCE:3\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;VALUE=DATE:20260401\r\n\
DTEND;VALUE=DATE:20260402\r\n\
DTSTAMP:20260310T140000Z\r\n\
UID:outlook-pto-002@outlook.com\r\n\
SUMMARY:PTO Day\r\n\
X-MICROSOFT-CDO-BUSYSTATUS:OOF\r\n\
X-MICROSOFT-CDO-ALLDAYEVENT:TRUE\r\n\
STATUS:CONFIRMED\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=\"Eastern Standard Time\":20260318T150000\r\n\
DTEND;TZID=\"Eastern Standard Time\":20260318T160000\r\n\
DTSTAMP:20260310T140000Z\r\n\
UID:outlook-oneonone-003@outlook.com\r\n\
SUMMARY:1:1 with Director\r\n\
DESCRIPTION:Monthly sync\r\n\
X-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\n\
SEQUENCE:1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART;TZID=\"Eastern Standard Time\":20260320T110000\r\n\
DTEND;TZID=\"Eastern Standard Time\":20260320T113000\r\n\
DTSTAMP:20260310T140000Z\r\n\
UID:outlook-demo-004@outlook.com\r\n\
SUMMARY:Product Demo\r\n\
DESCRIPTION:Showcase new features to stakeholders\r\n\
LOCATION:Building 5 Auditorium\r\n\
X-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\n\
X-MICROSOFT-CDO-IMPORTANCE:2\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

const YAHOO_ICS: &str = "\
BEGIN:VCALENDAR\r\n\
PRODID:-//Yahoo Inc//Yahoo Calendar//EN\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260319T100000\r\n\
DTEND:20260319T110000\r\n\
DTSTAMP:20260310T090000Z\r\n\
UID:yahoo-call-001@yahoo.com\r\n\
SUMMARY:Client Call\r\n\
DESCRIPTION:Discuss project timeline\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260321T140000\r\n\
DTEND:20260321T150000\r\n\
DTSTAMP:20260310T090000Z\r\n\
UID:yahoo-gym-002@yahoo.com\r\n\
SUMMARY:Yoga Class\r\n\
RRULE:FREQ=WEEKLY;BYDAY=SA\r\n\
LOCATION:Community Center\r\n\
SEQUENCE:1\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260323T180000\r\n\
DTEND:20260323T200000\r\n\
DTSTAMP:20260310T090000Z\r\n\
UID:yahoo-dinner-003@yahoo.com\r\n\
SUMMARY:Family Dinner\r\n\
LOCATION:Home\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

const SAMSUNG_ICS: &str = "\
BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
PRODID:-//Samsung Electronics//Samsung Calendar 12.4//EN\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260317T080000\r\n\
DTEND:20260317T083000\r\n\
DTSTAMP:20260310T060000Z\r\n\
UID:samsung-commute-001@samsung.com\r\n\
SUMMARY:Morning Commute Reminder\r\n\
DESCRIPTION:Leave by 8am\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260319T190000\r\n\
DTEND:20260319T210000\r\n\
DTSTAMP:20260310T060000Z\r\n\
UID:samsung-movie-002@samsung.com\r\n\
SUMMARY:Movie Night\r\n\
RRULE:FREQ=WEEKLY;BYDAY=TH\r\n\
DESCRIPTION:Pick a film from the list\r\n\
SEQUENCE:0\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
DTSTART:20260322T100000\r\n\
DTEND:20260322T110000\r\n\
DTSTAMP:20260310T060000Z\r\n\
UID:samsung-meeting-003@samsung.com\r\n\
SUMMARY:Team Sync\r\n\
ATTENDEE:mailto:colleague@example.com\r\n\
SEQUENCE:1\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

// ---------------------------------------------------------------------------
// Parse tests
// ---------------------------------------------------------------------------

#[test]
fn parse_google_fixture() {
    let results = parse_ical_multi(GOOGLE_ICS);
    assert_eq!(results.len(), 5, "Google fixture should have 5 events");

    let events: Vec<EventFields> = results.into_iter().map(|r| r.unwrap()).collect();

    // Event 1: recurring standup with TZID, RRULE, EXDATE, ATTENDEE, CATEGORIES
    assert_eq!(events[0].uid, "google-standup-001@google.com");
    assert_eq!(events[0].summary, "Daily Standup");
    assert!(!events[0].dtstart.is_empty());
    assert!(!events[0].rrule.is_empty(), "should have RRULE");
    assert!(!events[0].exdates.is_empty(), "should have EXDATE");
    assert!(!events[0].categories.is_empty(), "should have CATEGORIES");
    assert!(events[0].status.eq_ignore_ascii_case("CONFIRMED"));
    assert_eq!(events[0].location, "Conference Room B");
    assert_eq!(events[0].sequence, 2);

    // Event 2: all-day event (VALUE=DATE)
    assert_eq!(events[1].uid, "google-allday-002@google.com");
    assert_eq!(events[1].summary, "Company Holiday");
    assert!(events[1].is_all_day, "should be flagged as all-day");

    // Event 3
    assert_eq!(events[2].uid, "google-review-003@google.com");
    assert_eq!(events[2].summary, "Q1 Review");

    // Event 4
    assert_eq!(events[3].uid, "google-lunch-004@google.com");
    assert_eq!(events[3].summary, "Team Lunch");

    // Event 5
    assert_eq!(events[4].uid, "google-1on1-005@google.com");
    assert_eq!(events[4].summary, "1:1 with Manager");
    assert!(!events[4].rrule.is_empty());
}

#[test]
fn parse_apple_fixture() {
    let results = parse_ical_multi(APPLE_ICS);
    assert_eq!(results.len(), 4, "Apple fixture should have 4 events");

    let events: Vec<EventFields> = results.into_iter().map(|r| r.unwrap()).collect();

    // Event 1: floating time, multiple VALARMs
    assert_eq!(events[0].uid, "apple-dinner-001@icloud.com");
    assert_eq!(events[0].summary, "Dinner with Sarah");
    assert_eq!(events[0].location, "Chez Marie");
    assert!(!events[0].alarm.is_empty(), "should have VALARM");

    // Event 2: all-day with alarm
    assert_eq!(events[1].uid, "apple-birthday-002@icloud.com");
    assert_eq!(events[1].summary, "Mom's Birthday");
    assert!(events[1].is_all_day);

    // Event 3: floating time
    assert_eq!(events[2].uid, "apple-gym-003@icloud.com");
    assert_eq!(events[2].summary, "Gym Session");

    // Event 4: escaped CN in ATTENDEE
    assert_eq!(events[3].uid, "apple-dentist-004@icloud.com");
    assert_eq!(events[3].summary, "Dentist Appointment");
}

#[test]
fn parse_outlook_fixture() {
    let results = parse_ical_multi(OUTLOOK_ICS);
    assert_eq!(results.len(), 4, "Outlook fixture should have 4 events");

    let events: Vec<EventFields> = results.into_iter().map(|r| r.unwrap()).collect();

    // Event 1: Windows timezone, X-MICROSOFT props
    assert_eq!(events[0].uid, "outlook-sync-001@outlook.com");
    assert_eq!(events[0].summary, "Sprint Planning");
    assert!(!events[0].dtstart.is_empty());
    assert!(events[0].status.eq_ignore_ascii_case("CONFIRMED"));
    assert_eq!(events[0].sequence, 3);

    // Event 2: all-day PTO
    assert_eq!(events[1].uid, "outlook-pto-002@outlook.com");
    assert_eq!(events[1].summary, "PTO Day");
    assert!(events[1].is_all_day);

    // Event 3
    assert_eq!(events[2].uid, "outlook-oneonone-003@outlook.com");
    assert_eq!(events[2].summary, "1:1 with Director");

    // Event 4
    assert_eq!(events[3].uid, "outlook-demo-004@outlook.com");
    assert_eq!(events[3].summary, "Product Demo");
    assert_eq!(events[3].location, "Building 5 Auditorium");
}

#[test]
fn parse_yahoo_fixture() {
    let results = parse_ical_multi(YAHOO_ICS);
    assert_eq!(results.len(), 3, "Yahoo fixture should have 3 events");

    let events: Vec<EventFields> = results.into_iter().map(|r| r.unwrap()).collect();

    // Event 1: floating time, minimal props
    assert_eq!(events[0].uid, "yahoo-call-001@yahoo.com");
    assert_eq!(events[0].summary, "Client Call");

    // Event 2: recurring
    assert_eq!(events[1].uid, "yahoo-gym-002@yahoo.com");
    assert_eq!(events[1].summary, "Yoga Class");
    assert!(!events[1].rrule.is_empty(), "should have RRULE");
    assert_eq!(events[1].location, "Community Center");

    // Event 3
    assert_eq!(events[2].uid, "yahoo-dinner-003@yahoo.com");
    assert_eq!(events[2].summary, "Family Dinner");
}

#[test]
fn parse_samsung_fixture() {
    let results = parse_ical_multi(SAMSUNG_ICS);
    assert_eq!(results.len(), 3, "Samsung fixture should have 3 events");

    let events: Vec<EventFields> = results.into_iter().map(|r| r.unwrap()).collect();

    // Event 1: simple floating time
    assert_eq!(events[0].uid, "samsung-commute-001@samsung.com");
    assert_eq!(events[0].summary, "Morning Commute Reminder");

    // Event 2: simple recurring
    assert_eq!(events[1].uid, "samsung-movie-002@samsung.com");
    assert_eq!(events[1].summary, "Movie Night");
    assert!(!events[1].rrule.is_empty());

    // Event 3: minimal ATTENDEE
    assert_eq!(events[2].uid, "samsung-meeting-003@samsung.com");
    assert_eq!(events[2].summary, "Team Sync");
}
