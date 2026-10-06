package caldav

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// Test iCalendar bodies. Kept minimal — emersion/go-ical accepts these
// at the decoder layer; backend validation is what we exercise.
const (
	testPutValidEvent = "BEGIN:VCALENDAR\r\n" +
		"VERSION:2.0\r\n" +
		"PRODID:-//fauna//test//EN\r\n" +
		"BEGIN:VEVENT\r\n" +
		"UID:put-test-uid-001\r\n" +
		"DTSTAMP:20260516T120000Z\r\n" +
		"DTSTART:20260601T100000Z\r\n" +
		"DTEND:20260601T110000Z\r\n" +
		"SUMMARY:Phase E.3 PUT smoke\r\n" +
		"END:VEVENT\r\n" +
		"END:VCALENDAR\r\n"

	testPutEventNoUID = "BEGIN:VCALENDAR\r\n" +
		"VERSION:2.0\r\n" +
		"PRODID:-//fauna//test//EN\r\n" +
		"BEGIN:VEVENT\r\n" +
		"DTSTAMP:20260516T120000Z\r\n" +
		"DTSTART:20260601T100000Z\r\n" +
		"SUMMARY:Missing UID\r\n" +
		"END:VEVENT\r\n" +
		"END:VCALENDAR\r\n"

	testPutEventNoDTSTART = "BEGIN:VCALENDAR\r\n" +
		"VERSION:2.0\r\n" +
		"PRODID:-//fauna//test//EN\r\n" +
		"BEGIN:VEVENT\r\n" +
		"UID:put-test-no-dtstart\r\n" +
		"DTSTAMP:20260516T120000Z\r\n" +
		"SUMMARY:Missing DTSTART\r\n" +
		"END:VEVENT\r\n" +
		"END:VCALENDAR\r\n"
)

// putRequest builds an authenticated PUT request to the given event
// resource URL with `testPutValidEvent`-style body and optional
// If-Match header. Tests with non-default bodies pass `body` directly.
func putRequest(t *testing.T, url, body, ifMatch string) *http.Request {
	t.Helper()
	req, err := http.NewRequest(http.MethodPut, url, bytes.NewReader([]byte(body)))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	req.Header.Set("Content-Type", "text/calendar; charset=utf-8")
	if ifMatch != "" {
		req.Header.Set("If-Match", ifMatch)
	}
	return req
}

// testCalendarID is a deterministic non-zero 32-byte calendar id used
// in PUT/DELETE tests so request paths and assertions line up
// regardless of mock state.
var testCalendarID = func() []byte {
	sum := sha256.Sum256([]byte("put-test-calendar"))
	out := make([]byte, 32)
	copy(out, sum[:])
	return out
}()

// putAuthedCaller returns a mockCaller pre-wired with the AUTH flow's
// required state so a PUT request reaches the backend.
func putAuthedCaller(t *testing.T) *mockCaller {
	t.Helper()
	blob := mustReadFixture(t, "wrapped_msek.bin")
	return &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		// PUT does not unseal; snapshot can stay nil — AUTH succeeds
		// per TestAuthSucceedsWithoutMLSSnapshotProvisioned.
		mlsSnapshotBlob: nil,
	}
}

// eventURL builds the request URL for a PUT/DELETE against an event
// resource. The filename portion is arbitrary on PUT (the MDA
// recomputes from the parsed UID) but must be the canonical
// uid_hash hex on DELETE.
func eventURL(baseURL string, calendarID []byte, filename string) string {
	return baseURL +
		"/caldav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(calendarID) +
		"/" + filename + ".ics"
}

// canonicalUIDHashHex returns the hex form of blake3(uid)[:32] — the
// canonical filename slug the MDA writes into the Location header on
// PUT and uses as the dedup key on nest.
func canonicalUIDHashHex(uid string) string {
	sum := blake3.Sum256([]byte(uid))
	return hex.EncodeToString(sum[:])
}

// TestPutCalendarObjectCreates pins the create path: a fresh PUT with
// a valid VEVENT body lands on nest as `put_event_ciphertext`, the
// reply's `Created` outcome maps to 201, the ETag header carries the
// reply's etag, and the Location header carries the canonical event
// resource path (i.e. the filename is the blake3(UID)[:32] hex, not
// whatever filename the client used in the request URL).
func TestPutCalendarObjectCreates(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putEventOutcome = wsrpc.PutEventCreated
	// Nest returns bare etag values; emersion's wire writer wraps them
	// in DQUOTEs via internal.ETag.String() (RFC 7232 ETag form).
	caller.putEventETag = "etag-for-created"
	caller.putEventID = []byte("event-id-0001-pad-32-bytes-00000")

	baseURL, stop := startServer(t, caller)
	defer stop()

	// Request URL uses an arbitrary filename; the backend MUST rewrite
	// it to the canonical uid_hash hex in the Location header.
	req := putRequest(t, eventURL(baseURL, testCalendarID, "client-chose-this"), testPutValidEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}
	if got := resp.Header.Get("ETag"); got != `"etag-for-created"` {
		t.Errorf("ETag header = %q, want %q", got, `"etag-for-created"`)
	}
	wantLocation := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testCalendarID) +
		"/" + canonicalUIDHashHex("put-test-uid-001") + ".ics"
	if got := resp.Header.Get("Location"); got != wantLocation {
		t.Errorf("Location header = %q, want %q", got, wantLocation)
	}

	// Exactly one put_event_ciphertext call fired.
	puts := caller.callsOf(wsrpc.MethodPutEventCiphertext)
	if len(puts) != 1 {
		t.Fatalf("put_event_ciphertext fired %d times, want 1", len(puts))
	}
}

// TestPutCalendarObjectPlaintextModeSealsToo pins Phase-3 D1 (design
// `2026-07-07-phase-3-sealed-both-modes-design.md`): a committed
// plaintext-mode deployment seals the CalDAV PUT body at ingest exactly like
// an encrypted-mode one — the design-(b) no-seal branch is deleted; the
// put_event_ciphertext request carries a sealed MailRecordEnvelope with no
// readable event text.
func TestPutCalendarObjectPlaintextModeSealsToo(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putEventOutcome = wsrpc.PutEventCreated
	caller.putEventETag = "etag-plaintext"
	caller.putEventID = []byte("event-id-plain-pad-32-bytes-0000")

	baseURL, stop := startServerPlaintext(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "plaintext-ship"), testPutValidEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	puts := caller.callsOf(wsrpc.MethodPutEventCiphertext)
	if len(puts) != 1 {
		t.Fatalf("put_event_ciphertext fired %d times, want 1", len(puts))
	}
	// Phase-3 D1: the event body ships SEALED — a valid MailRecordEnvelope
	// with no plaintext SUMMARY visible, exactly the encrypted-mode shape.
	// (recordedCall.body is the whole CBOR request; decode out the field.)
	var putReq struct {
		EncryptedBody      []byte `cbor:"encrypted_body"`
		EncryptedIndexHint []byte `cbor:"encrypted_index_hint"`
	}
	if err := cbor.Unmarshal(puts[0].body, &putReq); err != nil {
		t.Fatalf("decode put_event_ciphertext request: %v", err)
	}
	if !mailfauna.IsSealedMailRecord(putReq.EncryptedBody) {
		t.Error("plaintext mode: encrypted_body must be a sealed MailRecordEnvelope (Phase-3 D1)")
	}
	if !mailfauna.IsSealedMailRecord(putReq.EncryptedIndexHint) {
		t.Error("plaintext mode: encrypted_index_hint must be sealed too (Phase-3 D1)")
	}
	if bytes.Contains(puts[0].body, []byte("Phase E.3 PUT smoke")) {
		t.Error("plaintext mode: the request must NOT contain the literal iCalendar SUMMARY")
	}
}

// TestPutCalendarObjectRejectsMissingUID confirms a VEVENT without a
// UID property fails with 400. The body either fails emersion's
// parser or fails our `valid-calendar-object-resource` check; both
// surface as 400.
func TestPutCalendarObjectRejectsMissingUID(t *testing.T) {
	caller := putAuthedCaller(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "no-uid"), testPutEventNoUID, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	// No nest RPC fires when the body is rejected client-side.
	if puts := caller.callsOf(wsrpc.MethodPutEventCiphertext); len(puts) != 0 {
		t.Errorf("put_event_ciphertext fired %d times on bad body, want 0", len(puts))
	}
}

// TestPutOverQuotaIs507 pins caldav-server.md § QUOTA → § Enforcement points:
// a PUT the nest refuses with `fauna.bridges.over_quota` answers `507
// Insufficient Storage` carrying the RFC 4918 §15 `DAV:quota-not-exceeded`
// precondition body, never a 500.
func TestPutOverQuotaIs507(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putEventErrCode = wsrpc.CodeOverQuota
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "over-quota"), testPutValidEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusInsufficientStorage {
		t.Fatalf("status = %d (%s), want 507", resp.StatusCode, body)
	}
	if !strings.Contains(string(body), "<D:quota-not-exceeded/>") {
		t.Errorf("507 body = %q, want the DAV:quota-not-exceeded precondition", body)
	}
}

// TestPutCalendarObjectRejectsMissingDTSTART confirms a VEVENT without
// a DTSTART property fails with 400.
func TestPutCalendarObjectRejectsMissingDTSTART(t *testing.T) {
	caller := putAuthedCaller(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "no-dtstart"), testPutEventNoDTSTART, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	if puts := caller.callsOf(wsrpc.MethodPutEventCiphertext); len(puts) != 0 {
		t.Errorf("put_event_ciphertext fired %d times on bad body, want 0", len(puts))
	}
}

// TestPutCalendarObjectStaleIfMatch confirms a PUT with an If-Match
// the row no longer matches yields 412 PreconditionFailed; the
// CurrentETag value carried in the reply is surfaced (here we just
// assert it appears in the response body text — exact wire shape
// per goal doc § Write surface is "the current ETag").
func TestPutCalendarObjectStaleIfMatch(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putEventOutcome = wsrpc.PutEventPreconditionFailed
	caller.putEventCurrentETag = "server-side-etag-123"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t,
		eventURL(baseURL, testCalendarID, "stale"),
		testPutValidEvent,
		`"client-stale-etag"`)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusPreconditionFailed {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusPreconditionFailed, body)
	}
	if !strings.Contains(string(body), "server-side-etag-123") {
		t.Errorf("response body missing current etag: %q", body)
	}
}

// TestPutCalendarObjectCalendarNotFound confirms a PUT against a
// calendar the AUTH'd actor has not provisioned yields 404.
func TestPutCalendarObjectCalendarNotFound(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putEventOutcome = wsrpc.PutEventCalendarNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t,
		eventURL(baseURL, testCalendarID, "missing-cal"),
		testPutValidEvent,
		"")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNotFound)
	}
}
