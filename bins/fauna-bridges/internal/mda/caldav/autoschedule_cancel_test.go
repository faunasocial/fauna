package caldav

import (
	"bytes"
	"io"
	"net/http"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// Event bodies for the CANCEL-on-removal (prior-roster diff) gateway. alice
// (the AUTH'd fixture user) organizes; bob/carol are external attendees. Each
// prior/new pair shares a UID so the new PUT's blake3(UID) slug addresses the
// same stored row the read-before-write fetches.

// Prior: alice organizes bob + carol. New: carol removed (bob remains).
const priorTwoAttendeeEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-removal\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:Design review\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:carol@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

const newOneAttendeeEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-removal\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:Design review\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

// Prior: alice organizes the sole attendee bob (a 1-on-1). New: bob removed
// (no email-reachable attendee left) — the common 1-on-1 "uninvite" case that
// must still CANCEL even though there is no REQUEST.
const priorOneAttendeeEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-removelast\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:1:1\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

const newNoAttendeeEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-removelast\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:1:1\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

// Prior: alice organizes bob. New: carol added (bob remains) — an add must not
// CANCEL anyone.
const priorAddBaseEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-add\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:Growing meeting\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

const newTwoAttendeeEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-add\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:Growing meeting\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:carol@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

// Deleted by the organizer: every email-reachable attendee is cancelled.
const deleteTwoAttendeeEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-del\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:Cancelled meeting\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:carol@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

// Deleted by an attendee (alice) of an event carol organizes — alice removing
// her own copy must NOT cancel the meeting for everyone.
const deleteForeignOrganizerEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\nPRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-delforeign\r\n" +
	"DTSTAMP:20260516T120000Z\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T110000Z\r\n" +
	"SUMMARY:Someone else's meeting\r\n" +
	"ORGANIZER:mailto:carol@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:carol@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"END:VEVENT\r\nEND:VCALENDAR\r\n"

// autoSchedDecryptCaller wires the full MLS decrypt path (so the read-before-
// write can unseal the prior event via OpenMailRecord) plus the PUT outcome
// knobs the scheduling gateway needs.
func autoSchedDecryptCaller(t *testing.T, fx reportFixture) *mockCaller {
	t.Helper()
	caller := decryptCaller(t, fx)
	caller.putEventOutcome = wsrpc.PutEventUpdated
	caller.putEventETag = "etag-autosched-cancel"
	caller.putEventID = []byte("event-id-autosched-pad-32-byte00")
	return caller
}

// decodeEnqueues decodes every recorded enqueue_outbound_mail call body.
func decodeEnqueues(t *testing.T, caller *mockCaller) []enqueueOutboundDecoded {
	t.Helper()
	var out []enqueueOutboundDecoded
	for _, c := range caller.callsOf(wsrpc.MethodEnqueueOutboundMail) {
		var d enqueueOutboundDecoded
		if err := cbor.Unmarshal(c.body, &d); err != nil {
			t.Fatalf("decode enqueue body: %v", err)
		}
		out = append(out, d)
	}
	return out
}

// imipMethodOf classifies a raw iMIP message by its METHOD property.
func imipMethodOf(raw []byte) string {
	switch {
	case bytes.Contains(raw, []byte("METHOD:REQUEST")):
		return "REQUEST"
	case bytes.Contains(raw, []byte("METHOD:CANCEL")):
		return "CANCEL"
	default:
		return "?"
	}
}

// byMethod returns the (single expected) enqueue carrying the given iMIP method.
func byMethod(t *testing.T, enq []enqueueOutboundDecoded, method string) *enqueueOutboundDecoded {
	t.Helper()
	var hit *enqueueOutboundDecoded
	for i := range enq {
		if imipMethodOf(enq[i].RawMessage) == method {
			if hit != nil {
				t.Fatalf("more than one %s enqueue", method)
			}
			hit = &enq[i]
		}
	}
	return hit
}

func assertOrganizerScoped(t *testing.T, e *enqueueOutboundDecoded) {
	t.Helper()
	if e.OriginalSender != "alice@example.com" {
		t.Errorf("original_sender = %q, want alice@example.com", e.OriginalSender)
	}
	if e.OnBehalfOfActor == nil || !bytes.Equal(*e.OnBehalfOfActor, fixtureActorID) {
		t.Errorf("on_behalf_of_actor = %x, want %x (the AUTH'd organizer)", e.OnBehalfOfActor, fixtureActorID)
	}
}

// TestPutOrganizerRemovesAttendeeFansOutCancel pins the roster-diff: an
// organizer PUT that drops carol (bob remains) fires a REQUEST to the remaining
// roster (bob) AND a CANCEL to the removed attendee only (carol), both scoped
// to the organizer (caldav-server.md § Server-side auto-schedule).
func TestPutOrganizerRemovesAttendeeFansOutCancel(t *testing.T) {
	fx := newReportFixture(t)
	caller := autoSchedDecryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{{
		UIDHash:       uidHash("autosched-removal"),
		EncryptedBody: fx.sealEvent(t, []byte(priorTwoAttendeeEvent)),
		ETag:          "prior-etag",
	}}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "removal"), newOneAttendeeEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	enq := decodeEnqueues(t, caller)
	if len(enq) != 2 {
		t.Fatalf("enqueues = %d, want 2 (REQUEST + CANCEL)", len(enq))
	}
	request := byMethod(t, enq, "REQUEST")
	cancel := byMethod(t, enq, "CANCEL")
	if request == nil || cancel == nil {
		t.Fatalf("missing REQUEST(%v) or CANCEL(%v)", request, cancel)
	}
	if len(request.Recipients) != 1 || request.Recipients[0] != "bob@example.com" {
		t.Errorf("REQUEST recipients = %v, want [bob@example.com]", request.Recipients)
	}
	if len(cancel.Recipients) != 1 || cancel.Recipients[0] != "carol@example.com" {
		t.Errorf("CANCEL recipients = %v, want [carol@example.com] (removed only)", cancel.Recipients)
	}
	assertOrganizerScoped(t, request)
	assertOrganizerScoped(t, cancel)
}

// TestPutOrganizerRemovesLastAttendeeFansOutCancel covers the 1-on-1 uninvite:
// the new body has no email-reachable attendee (so no REQUEST), but the removed
// sole attendee must still get a CANCEL.
func TestPutOrganizerRemovesLastAttendeeFansOutCancel(t *testing.T) {
	fx := newReportFixture(t)
	caller := autoSchedDecryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{{
		UIDHash:       uidHash("autosched-removelast"),
		EncryptedBody: fx.sealEvent(t, []byte(priorOneAttendeeEvent)),
		ETag:          "prior-etag",
	}}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "removelast"), newNoAttendeeEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	enq := decodeEnqueues(t, caller)
	if len(enq) != 1 {
		t.Fatalf("enqueues = %d, want 1 (CANCEL only)", len(enq))
	}
	if imipMethodOf(enq[0].RawMessage) != "CANCEL" {
		t.Fatalf("enqueue method = %q, want CANCEL", imipMethodOf(enq[0].RawMessage))
	}
	if len(enq[0].Recipients) != 1 || enq[0].Recipients[0] != "bob@example.com" {
		t.Errorf("CANCEL recipients = %v, want [bob@example.com]", enq[0].Recipients)
	}
	assertOrganizerScoped(t, &enq[0])
}

// TestPutOrganizerAddsAttendeeNoCancel confirms adding an attendee fires only a
// REQUEST (to the full new roster) and no spurious CANCEL.
func TestPutOrganizerAddsAttendeeNoCancel(t *testing.T) {
	fx := newReportFixture(t)
	caller := autoSchedDecryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{{
		UIDHash:       uidHash("autosched-add"),
		EncryptedBody: fx.sealEvent(t, []byte(priorAddBaseEvent)),
		ETag:          "prior-etag",
	}}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "add"), newTwoAttendeeEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	enq := decodeEnqueues(t, caller)
	if len(enq) != 1 {
		t.Fatalf("enqueues = %d, want 1 (REQUEST only, no CANCEL)", len(enq))
	}
	if imipMethodOf(enq[0].RawMessage) != "REQUEST" {
		t.Fatalf("enqueue method = %q, want REQUEST", imipMethodOf(enq[0].RawMessage))
	}
	if len(enq[0].Recipients) != 2 ||
		enq[0].Recipients[0] != "bob@example.com" || enq[0].Recipients[1] != "carol@example.com" {
		t.Errorf("REQUEST recipients = %v, want [bob@example.com carol@example.com]", enq[0].Recipients)
	}
}

// TestDeleteOrganizerEventFansOutCancel: an organizer DELETE withdraws the
// event from ALL its email-reachable attendees.
func TestDeleteOrganizerEventFansOutCancel(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.deleteEventOutcome = wsrpc.DeleteEventDeleted
	caller.queryEventsEvents = []wsrpc.EventEntry{{
		UIDHash:       uidHash("autosched-del"),
		EncryptedBody: fx.sealEvent(t, []byte(deleteTwoAttendeeEvent)),
		ETag:          "prior-etag",
	}}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := deleteRequest(t, eventURL(baseURL, testCalendarID, canonicalUIDHashHex("autosched-del")), "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNoContent)
	}
	if n := len(caller.callsOf(wsrpc.MethodDeleteEvent)); n != 1 {
		t.Fatalf("delete_event fired %d times, want 1", n)
	}

	enq := decodeEnqueues(t, caller)
	if len(enq) != 1 {
		t.Fatalf("enqueues = %d, want 1 (CANCEL to all)", len(enq))
	}
	if imipMethodOf(enq[0].RawMessage) != "CANCEL" {
		t.Fatalf("enqueue method = %q, want CANCEL", imipMethodOf(enq[0].RawMessage))
	}
	if len(enq[0].Recipients) != 2 ||
		enq[0].Recipients[0] != "bob@example.com" || enq[0].Recipients[1] != "carol@example.com" {
		t.Errorf("CANCEL recipients = %v, want [bob@example.com carol@example.com]", enq[0].Recipients)
	}
	assertOrganizerScoped(t, &enq[0])
}

// TestDeleteNonOrganizerEventNoCancel: alice deleting her copy of an event
// carol organizes must NOT fan out a CANCEL (only the organizer cancels).
func TestDeleteNonOrganizerEventNoCancel(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.deleteEventOutcome = wsrpc.DeleteEventDeleted
	caller.queryEventsEvents = []wsrpc.EventEntry{{
		UIDHash:       uidHash("autosched-delforeign"),
		EncryptedBody: fx.sealEvent(t, []byte(deleteForeignOrganizerEvent)),
		ETag:          "prior-etag",
	}}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := deleteRequest(t, eventURL(baseURL, testCalendarID, canonicalUIDHashHex("autosched-delforeign")), "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNoContent)
	}
	if n := len(caller.callsOf(wsrpc.MethodEnqueueOutboundMail)); n != 0 {
		t.Errorf("enqueue_outbound_mail fired %d times for a non-organizer DELETE, want 0", n)
	}
}
