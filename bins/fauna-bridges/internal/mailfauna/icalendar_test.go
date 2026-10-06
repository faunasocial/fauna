package mailfauna

import (
	"strings"
	"testing"
	"time"
)

const oneVEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:go-smoke-uid\r\n" +
	"DTSTAMP:20260515T120000Z\r\n" +
	"DTSTART:20260601T100000Z\r\n" +
	"DTEND:20260601T110000Z\r\n" +
	"SUMMARY:Hello\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

const dailyRRule = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:go-daily-rrule\r\n" +
	"DTSTAMP:20260515T120000Z\r\n" +
	"DTSTART:20260601T100000Z\r\n" +
	"DTEND:20260601T110000Z\r\n" +
	"RRULE:FREQ=DAILY;COUNT=5\r\n" +
	"SUMMARY:Daily5\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

// Each property name lookup is case-insensitive in the parser, but
// finding by exact-case is fine for the test fixtures we control.
func findProperty(c ICalComponent, name string) (string, bool) {
	for _, p := range c.Properties {
		if p.Name == name {
			return p.Value, true
		}
	}
	return "", false
}

func TestParseICalendar_RoundTripsUID(t *testing.T) {
	doc, err := ParseICalendar([]byte(oneVEvent))
	if err != nil {
		t.Fatalf("ParseICalendar: %v", err)
	}
	if len(doc.Components) != 1 {
		t.Fatalf("expected 1 VEVENT, got %d", len(doc.Components))
	}
	v := doc.Components[0]
	if got, _ := findProperty(v, "UID"); got != "go-smoke-uid" {
		t.Errorf("UID = %q, want go-smoke-uid", got)
	}
	if got, _ := findProperty(v, "DTSTART"); got != "20260601T100000Z" {
		t.Errorf("DTSTART = %q, want 20260601T100000Z", got)
	}
	if got, _ := findProperty(v, "SUMMARY"); got != "Hello" {
		t.Errorf("SUMMARY = %q, want Hello", got)
	}
}

func TestParseICalendar_RejectsNonUTF8(t *testing.T) {
	bad := []byte{0xff, 0xfe, 'X'}
	if _, err := ParseICalendar(bad); err == nil {
		t.Fatal("expected error on non-UTF-8 body")
	}
}

func TestExpandRecurrence_BareVEvent(t *testing.T) {
	doc, err := ParseICalendar([]byte(oneVEvent))
	if err != nil {
		t.Fatalf("ParseICalendar: %v", err)
	}
	start := time.Date(2026, 6, 1, 0, 0, 0, 0, time.UTC).Unix()
	end := time.Date(2026, 6, 2, 0, 0, 0, 0, time.UTC).Unix()
	occ, err := ExpandRecurrence(doc.Components[0], start, end)
	if err != nil {
		t.Fatalf("ExpandRecurrence: %v", err)
	}
	if len(occ) != 1 {
		t.Fatalf("expected 1 occurrence, got %d", len(occ))
	}
	wantStart := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC).Unix()
	wantEnd := time.Date(2026, 6, 1, 11, 0, 0, 0, time.UTC).Unix()
	if occ[0].Dtstart != wantStart {
		t.Errorf("Dtstart = %d, want %d", occ[0].Dtstart, wantStart)
	}
	if occ[0].Dtend != wantEnd {
		t.Errorf("Dtend = %d, want %d", occ[0].Dtend, wantEnd)
	}
}

func TestExpandRecurrence_DailyWithCount(t *testing.T) {
	doc, err := ParseICalendar([]byte(dailyRRule))
	if err != nil {
		t.Fatalf("ParseICalendar: %v", err)
	}
	start := time.Date(2026, 6, 1, 0, 0, 0, 0, time.UTC).Unix()
	end := time.Date(2026, 6, 30, 0, 0, 0, 0, time.UTC).Unix()
	occ, err := ExpandRecurrence(doc.Components[0], start, end)
	if err != nil {
		t.Fatalf("ExpandRecurrence: %v", err)
	}
	if len(occ) != 5 {
		t.Fatalf("expected COUNT=5 occurrences, got %d", len(occ))
	}
	for i, o := range occ {
		want := time.Date(2026, 6, 1+i, 10, 0, 0, 0, time.UTC).Unix()
		if o.Dtstart != want {
			t.Errorf("occ[%d].Dtstart = %d, want %d", i, o.Dtstart, want)
		}
	}
}

func TestExpandRecurrence_OutsideWindow(t *testing.T) {
	doc, err := ParseICalendar([]byte(oneVEvent))
	if err != nil {
		t.Fatalf("ParseICalendar: %v", err)
	}
	start := time.Date(2027, 1, 1, 0, 0, 0, 0, time.UTC).Unix()
	end := time.Date(2027, 2, 1, 0, 0, 0, 0, time.UTC).Unix()
	occ, err := ExpandRecurrence(doc.Components[0], start, end)
	if err != nil {
		t.Fatalf("ExpandRecurrence: %v", err)
	}
	if len(occ) != 0 {
		t.Errorf("expected 0 occurrences outside window, got %d", len(occ))
	}
}

// TestGenerateICal_RoundTripsThroughParser proves the writer half of the
// iCalendar FFI: the Go MDA's auto-schedule gateway builds a VEVENT via the
// shared-Rust writer (fauna_core::ical::generate_ical, reached as
// faunaMail.GenerateIcal), and the output re-parses through ParseICalendar —
// the same write→read contract the Rust contract test pins, but across the FFI.
func TestGenerateICal_RoundTripsThroughParser(t *testing.T) {
	ev := WriterEventFields{
		Summary: "Go writer smoke",
		Dtstart: "2026-06-10T15:00:00Z",
		Dtend:   "2026-06-10T16:00:00Z",
		Uid:     "go-gen-uid",
	}
	att := []WriterAttendeeInfo{{
		Name:        "Alice",
		Email:       "alice@example.com",
		Partstat:    "ACCEPTED",
		FaunaStatus: "going",
	}}

	ics := GenerateICal(ev, att, "organizer@example.com")

	for _, want := range []string{
		"BEGIN:VCALENDAR", "BEGIN:VEVENT", "SUMMARY:Go writer smoke",
		"UID:go-gen-uid", "ATTENDEE",
	} {
		if !strings.Contains(ics, want) {
			t.Errorf("generated ICS missing %q:\n%s", want, ics)
		}
	}

	doc, err := ParseICalendar([]byte(ics))
	if err != nil {
		t.Fatalf("writer output failed to re-parse: %v", err)
	}
	if len(doc.Components) == 0 {
		t.Fatal("no VEVENT in generated ICS")
	}
	if got, _ := findProperty(doc.Components[0], "UID"); got != "go-gen-uid" {
		t.Errorf("round-tripped UID = %q, want go-gen-uid", got)
	}
}

// TestGenerateItip_RoundTripsThroughParser proves the iTIP half of the FFI: the
// MDA's auto-schedule gateway builds a METHOD-tagged VCALENDAR via the
// shared-Rust writer (fauna_core::ical::generate_itip, reached as
// faunaMail.GenerateItip) and the output re-parses through ParseICalendar. The
// REQUEST carries METHOD + DTSTAMP (RFC 5546/5545 mandate both on an iTIP
// VEVENT) — what a stock Apple/Outlook organizer needs to schedule.
func TestGenerateItip_RoundTripsThroughParser(t *testing.T) {
	ev := WriterEventFields{
		Summary:  "Go iTIP smoke",
		Dtstart:  "2026-06-12T15:00:00Z",
		Dtend:    "2026-06-12T16:00:00Z",
		Uid:      "go-itip-uid",
		Sequence: 1,
	}
	att := []WriterAttendeeInfo{{
		Name:     "Bob",
		Email:    "bob@example.com",
		Partstat: "NEEDS-ACTION",
	}}

	ics := GenerateItip(ITipRequest, ev, att, "organizer@example.com", "2026-06-04T12:00:00Z")

	for _, want := range []string{
		"BEGIN:VCALENDAR", "METHOD:REQUEST", "BEGIN:VEVENT", "DTSTAMP:",
		"SUMMARY:Go iTIP smoke", "UID:go-itip-uid", "ATTENDEE",
	} {
		if !strings.Contains(ics, want) {
			t.Errorf("generated iTIP missing %q:\n%s", want, ics)
		}
	}

	doc, err := ParseICalendar([]byte(ics))
	if err != nil {
		t.Fatalf("iTIP output failed to re-parse: %v", err)
	}
	if len(doc.Components) == 0 {
		t.Fatal("no VEVENT in generated iTIP")
	}
	if got, _ := findProperty(doc.Components[0], "UID"); got != "go-itip-uid" {
		t.Errorf("round-tripped UID = %q, want go-itip-uid", got)
	}
}
