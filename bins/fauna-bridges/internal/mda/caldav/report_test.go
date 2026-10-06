package caldav

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
	"lukechampine.com/blake3"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// makeEventPager returns n events with unique, ascending 32-byte EventIDs and a
// pager that serves them paginated by (afterEventID, limit) exactly as nest's
// query_caldav_events does (ORDER BY event_id ASC, after_event_id cursor). The
// bodies are empty — fetchAllEvents does not decrypt (that is openEvent's job),
// so the pagination tests need no sealing.
func makeEventPager(n int) ([]wsrpc.EventEntry, func(after []byte, limit uint32) ([]wsrpc.EventEntry, bool)) {
	all := make([]wsrpc.EventEntry, n)
	for i := range all {
		id := make([]byte, 32)
		binary.BigEndian.PutUint32(id[28:], uint32(i+1)) // 1-based, ordered, unique
		all[i] = wsrpc.EventEntry{EventID: id, UIDHash: id}
	}
	pager := func(after []byte, limit uint32) ([]wsrpc.EventEntry, bool) {
		start := 0
		if len(after) > 0 {
			for i := range all {
				if bytes.Equal(all[i].EventID, after) {
					start = i + 1
					break
				}
			}
		}
		end := len(all)
		if limit > 0 && start+int(limit) < end {
			end = start + int(limit)
		}
		return all[start:end], end < len(all)
	}
	return all, pager
}

// TestFetchAllEventsPaginatesAcrossPages proves the § B6 bounded-pagination
// loop: a calendar larger than one page is fetched in reportPageSize chunks
// (each well under the bridge's 16 MiB WS read cap), accumulated in order with
// no dupes/gaps, via the after_event_id cursor.
func TestFetchAllEventsPaginatesAcrossPages(t *testing.T) {
	const total = 2*reportPageSize + 500 // 2500 → 3 pages: 1000, 1000, 500
	all, pager := makeEventPager(total)
	caller := &mockCaller{queryEventsPager: pager}
	b := NewBackend(slog.Default(), nil)
	sess := davauth.NewSession(caller, slog.Default(), fixtureActorID, "", "")

	got, err := b.fetchAllEvents(context.Background(), sess, testCalendarID)
	if err != nil {
		t.Fatalf("fetchAllEvents: %v", err)
	}
	if len(got) != total {
		t.Fatalf("got %d events, want %d (whole calendar, under the cap)", len(got), total)
	}
	for i := range got {
		if !bytes.Equal(got[i].EventID, all[i].EventID) {
			t.Fatalf("event %d = %x, want %x (cursor advance broke ordering/coverage)", i, got[i].EventID, all[i].EventID)
		}
	}
	if n := len(caller.callsOf(wsrpc.MethodQueryEvents)); n != 3 {
		t.Errorf("query_events fired %d times, want 3 (ceil(2500/1000))", n)
	}
}

// TestFetchAllEventsTruncatesAtCap proves the hard maxReportEvents ceiling: a
// calendar past the documented ~10k envelope is truncated (not unbounded), and
// the loop STOPS at the cap rather than continuing to fetch every remaining page.
func TestFetchAllEventsTruncatesAtCap(t *testing.T) {
	const total = maxReportEvents + 1 // one event past the cap
	_, pager := makeEventPager(total)
	caller := &mockCaller{queryEventsPager: pager}
	b := NewBackend(slog.Default(), nil)
	sess := davauth.NewSession(caller, slog.Default(), fixtureActorID, "", "")

	got, err := b.fetchAllEvents(context.Background(), sess, testCalendarID)
	if err != nil {
		t.Fatalf("fetchAllEvents: %v", err)
	}
	if len(got) != maxReportEvents {
		t.Fatalf("got %d events, want %d (hard cap)", len(got), maxReportEvents)
	}
	want := maxReportEvents / reportPageSize
	if n := len(caller.callsOf(wsrpc.MethodQueryEvents)); n != want {
		t.Errorf("query_events fired %d times, want %d (must stop at the cap, not drain every page)", n, want)
	}
}

// testReportEvent is a minimal-but-valid VCALENDAR / VEVENT body the
// REPORT tests seal end-to-end. DTSTART falls inside a fixed May 2026
// window so the time-range tests can assert inclusion/exclusion against
// a deterministic boundary.
const testReportEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//fauna//report-test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:report-test-uid-1\r\n" +
	"DTSTAMP:20260516T120000Z\r\n" +
	"DTSTART:20260520T100000Z\r\n" +
	"DTEND:20260520T110000Z\r\n" +
	"SUMMARY:Report test event\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

// testReportEventOutsideWindow has a DTSTART well outside the May 2026
// boundary used by the time-range tests.
const testReportEventOutsideWindow = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//fauna//report-test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:report-test-uid-out\r\n" +
	"DTSTAMP:20260116T120000Z\r\n" +
	"DTSTART:20260120T100000Z\r\n" +
	"DTEND:20260120T110000Z\r\n" +
	"SUMMARY:Outside window\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

// testReportEventNoDtstamp is a VEVENT that go-ical's DECODER accepts but its
// ENCODER rejects: it omits the RFC-5545-mandatory DTSTAMP (the GAP-2 failure
// mode — a client write that didn't stamp it). Used to prove the serve path
// skips such an event instead of breaking the whole REPORT.
const testReportEventNoDtstamp = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//fauna//report-test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:report-test-uid-nodtstamp\r\n" +
	"DTSTART:20260520T100000Z\r\n" +
	"DTEND:20260520T110000Z\r\n" +
	"SUMMARY:No DTSTAMP event\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

// reportFixture bundles the per-test crypto material a decrypt-path
// REPORT integration test needs: the actor's seal key as production
// derives it from the canonical fixture MSEK — the X25519 keypair
// (`leaf`) and its ML-KEM half (`mlkemEk`) — the MLS snapshot blob that
// opens what is sealed to either (CBOR plaintext sealed under that
// MSEK), and a helper that seals event bodies to the leaf pubkey.
type reportFixture struct {
	leaf         faunaFfi.X25519Keypair
	mlkemEk      []byte
	snapshotBlob []byte
}

// newReportFixture builds the crypto scaffolding shared across REPORT
// tests: both halves of the seal key derived from the fixture MSEK, and
// the snapshot built from that MSEK and sealed under it, so the AUTH
// flow's `cap.Decrypt(snapshotBlob)` round-trips and the session opens
// both a classical seal (the tests' pre-sealed fixtures) and the X-Wing
// seal a PUT / MKCALENDAR / PROPPATCH writes to the pair.
func newReportFixture(t *testing.T) reportFixture {
	t.Helper()
	leaf, err := faunaFfi.DeriveRecipientHpkeKeypair(fixtureMSEK)
	if err != nil {
		t.Fatalf("DeriveRecipientHpkeKeypair: %v", err)
	}
	material, err := faunaFfi.DeriveRecipientMailXwingMaterial(fixtureMSEK)
	if err != nil {
		t.Fatalf("DeriveRecipientMailXwingMaterial: %v", err)
	}
	snapshotPlaintext, err := faunaFfi.EncodeMlsSnapshotPlaintextFromMseks([][]byte{fixtureMSEK})
	if err != nil {
		t.Fatalf("EncodeMlsSnapshotPlaintextFromMseks: %v", err)
	}
	snapshotBlob, err := faunaFfi.SealMlsSnapshotBlob(
		snapshotPlaintext, fixtureActorID, fixtureMSEK,
	)
	if err != nil {
		t.Fatalf("SealMlsSnapshotBlob: %v", err)
	}
	return reportFixture{leaf: leaf, mlkemEk: material.MlkemEk, snapshotBlob: snapshotBlob}
}

// sealEvent seals `body` to the fixture's leaf pubkey. The returned
// bytes are wire-identical to what MTA-side `EncryptToRecipient`
// produces, so the AUTH'd MDA can re-open them via `OpenMailRecord`.
func (f *reportFixture) sealEvent(t *testing.T, body []byte) []byte {
	t.Helper()
	out, err := mailfauna.EncryptToRecipient(body, f.leaf.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}
	return out
}

// uidHash returns blake3(uid)[:32] — the canonical event-resource slug
// the REPORT path uses to address events.
func uidHash(uid string) []byte {
	sum := blake3.Sum256([]byte(uid))
	out := make([]byte, 32)
	copy(out, sum[:])
	return out
}

// decryptCaller returns a mockCaller wired for an AUTH flow that
// successfully fetches the MLS snapshot. Tests poke the
// queryEvents*/syncCalendarSince* knobs to stage replies.
func decryptCaller(t *testing.T, f reportFixture) *mockCaller {
	t.Helper()
	blob := mustReadFixture(t, "wrapped_msek.bin")
	return &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              f.leaf.Pubkey,
		mlkemEk:                f.mlkemEk,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        f.snapshotBlob,
	}
}

// reportRequest builds an authenticated REPORT request against the
// given URL with the supplied XML body.
func reportRequest(t *testing.T, url, body string) *http.Request {
	t.Helper()
	req, err := http.NewRequest("REPORT", url, strings.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	req.Header.Set("Content-Type", "application/xml; charset=utf-8")
	req.Header.Set("Depth", "1")
	return req
}

// calendarURL builds the calendar-collection URL for a given calendar
// id (hex). Useful for REPORT calendar-query / sync-collection request
// targets; multiget href payloads embed event-resource paths instead.
func calendarURL(baseURL string, calendarID []byte) string {
	return baseURL +
		"/caldav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(calendarID) + "/"
}

const calendarQueryBody = `<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT"/>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>`

// calendarQueryWithRangeBody wraps the time-range filter (start/end in
// iCalendar UTC form) per RFC 4791 §9.9.
func calendarQueryWithRangeBody(start, end string) string {
	return fmt.Sprintf(`<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="%s" end="%s"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>`, start, end)
}

func calendarMultigetBody(hrefs ...string) string {
	var hs strings.Builder
	for _, h := range hrefs {
		hs.WriteString("  <D:href>")
		hs.WriteString(h)
		hs.WriteString("</D:href>\n")
	}
	return `<?xml version="1.0" encoding="utf-8"?>
<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
` + hs.String() + `</C:calendar-multiget>`
}

// ── REPORT calendar-query tests ──────────────────────────────────

// TestReportCalendarQueryReturnsAllEvents pins the no-filter
// calendar-query path: REPORT against a calendar with one event yields
// 207 Multi-Status, the response body carries the event's resource
// path + the decrypted iCalendar SUMMARY string, and exactly one
// query_events RPC fires.
func TestReportCalendarQueryReturnsAllEvents(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			EventID:       []byte("event-id-1-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-1",
			Modseq:        42,
			InternalDate:  1747396800, // 2025-05-16T12:00:00Z
		},
	}
	caller.queryEventsHighestModseq = 42

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, calendarURL(baseURL, testCalendarID), calendarQueryBody)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// Event resource path (canonical uid_hash filename) appears in
	// multistatus.
	wantPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("report-test-uid-1")) + ".ics"
	if !strings.Contains(string(body), wantPath) {
		t.Fatalf("response body missing path %q: %q", wantPath, body)
	}
	// Decrypted body's SUMMARY landed in the multistatus payload via
	// the inlined <calendar-data> element. emersion's encoder may
	// reformat lines, but the SUMMARY value is preserved.
	if !strings.Contains(string(body), "Report test event") {
		t.Fatalf("response body missing decrypted SUMMARY: %q", body)
	}
	// One query_events RPC.
	queries := caller.callsOf(wsrpc.MethodQueryEvents)
	if len(queries) != 1 {
		t.Fatalf("query_events fired %d times, want 1", len(queries))
	}
	// Verify the RPC payload targets the right actor + calendar.
	var qReq struct {
		ActorID    []byte `cbor:"actor_id"`
		CalendarID []byte `cbor:"calendar_id"`
	}
	if err := cbor.Unmarshal(queries[0].body, &qReq); err != nil {
		t.Fatalf("decode query_events: %v", err)
	}
	if !bytes.Equal(qReq.ActorID, fixtureActorID) {
		t.Errorf("query_events actor_id = %x, want %x", qReq.ActorID, fixtureActorID)
	}
	if !bytes.Equal(qReq.CalendarID, testCalendarID) {
		t.Errorf("query_events calendar_id = %x, want %x", qReq.CalendarID, testCalendarID)
	}
}

// TestReportCalendarQuerySkipsUnencodableEvent proves that one stored event
// go-ical's encoder rejects (a VEVENT missing the mandatory DTSTAMP — the GAP-2
// failure mode) is skipped, NOT allowed to break the whole REPORT. Before the
// openEvent pre-encode guard, emersion's streaming writer hit the encode error
// mid-multistatus ("superfluous WriteHeader"), truncating the response so EVERY
// event — including healthy ones — vanished from the MUA's view.
func TestReportCalendarQuerySkipsUnencodableEvent(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			EventID:       []byte("event-id-bad-pad-32-bytes-000000"),
			UIDHash:       uidHash("report-test-uid-nodtstamp"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEventNoDtstamp)),
			ETag:          "etag-bad",
			Modseq:        41,
			InternalDate:  1747396800,
		},
		{
			EventID:       []byte("event-id-1-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-1",
			Modseq:        42,
			InternalDate:  1747396800,
		},
	}
	caller.queryEventsHighestModseq = 42

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, calendarURL(baseURL, testCalendarID), calendarQueryBody)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// The VALID event still surfaces — the un-encodable sibling did NOT break
	// the whole response (the regression this guards).
	if !strings.Contains(string(body), "Report test event") {
		t.Fatalf("valid event missing — an un-encodable sibling broke the whole REPORT: %q", body)
	}
	goodPath := hex.EncodeToString(uidHash("report-test-uid-1")) + ".ics"
	if !strings.Contains(string(body), goodPath) {
		t.Fatalf("valid event path %q missing: %q", goodPath, body)
	}
	// The un-encodable event is skipped (its resource path never appears).
	badPath := hex.EncodeToString(uidHash("report-test-uid-nodtstamp")) + ".ics"
	if strings.Contains(string(body), badPath) {
		t.Fatalf("un-encodable event %q should have been skipped, not served: %q", badPath, body)
	}
}

// TestReportCalendarQuerySkipsRawEvent pins the one serve path's refusal of
// an unsealed event: every event rests sealed, so a raw body is corruption —
// it fails its open and is skipped + logged, never served verbatim. The
// sealed sibling still HPKE-opens via the per-session opener.
func TestReportCalendarQuerySkipsRawEvent(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			EventID:       []byte("event-id-raw-pad-32-bytes-000000"),
			UIDHash:       uidHash("report-test-uid-out"),
			EncryptedBody: []byte(testReportEventOutsideWindow), // RAW — never a resting shape
			ETag:          "etag-raw",
		},
		{
			EventID:       []byte("event-id-1-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-sealed",
		},
	}

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, calendarURL(baseURL, testCalendarID), calendarQueryBody)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// The raw event is skipped: neither its path nor its plaintext appears.
	rawPath := hex.EncodeToString(uidHash("report-test-uid-out")) + ".ics"
	if strings.Contains(string(body), rawPath) || strings.Contains(string(body), "Outside window") {
		t.Fatalf("raw event must be skipped, never served verbatim: %q", body)
	}
	// The sealed sibling opened via the per-session opener on the SAME path.
	if !strings.Contains(string(body), "Report test event") {
		t.Fatalf("sealed event missing from the same one-path REPORT: %q", body)
	}
}

// TestReportCalendarQuerySkipsWrongKeySealedEvent pins the AEAD-closed half of
// the strict open: a record that IS shape-valid sealed but was sealed to a
// DIFFERENT recipient key fails its open and is skipped + logged — never
// served as garbage and never passed through verbatim (an open failure is an
// error, not a fall-through). The healthy sibling still serves.
func TestReportCalendarQuerySkipsWrongKeySealedEvent(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	// Seal to a fresh keypair the session's snapshot does NOT carry.
	stranger := faunaFfi.GenerateX25519Keypair()
	wrongKeySealed, err := mailfauna.EncryptToRecipient([]byte(testReportEventOutsideWindow), stranger.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			EventID:       []byte("event-id-bad-pad-32-bytes-000000"),
			UIDHash:       uidHash("report-test-uid-out"),
			EncryptedBody: wrongKeySealed,
			ETag:          "etag-wrong-key",
		},
		{
			EventID:       []byte("event-id-1-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-good",
		},
	}

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, calendarURL(baseURL, testCalendarID), calendarQueryBody)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// The healthy event still serves.
	if !strings.Contains(string(body), "Report test event") {
		t.Fatalf("healthy event missing — a wrong-key sibling broke the whole REPORT: %q", body)
	}
	// The wrong-key event is skipped: neither its path nor its plaintext appears.
	badPath := hex.EncodeToString(uidHash("report-test-uid-out")) + ".ics"
	if strings.Contains(string(body), badPath) {
		t.Fatalf("wrong-key sealed event %q must be skipped, not served: %q", badPath, body)
	}
	if strings.Contains(string(body), "Outside window") {
		t.Fatalf("wrong-key sealed event's plaintext leaked into the response: %q", body)
	}
}

// TestReportCalendarQueryFiltersTimeRange confirms events whose
// DTSTART falls outside the requested <time-range> are dropped from
// the multistatus response. One event inside-window + one outside →
// only the inside-window event surfaces.
func TestReportCalendarQueryFiltersTimeRange(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-inside",
		},
		{
			UIDHash:       uidHash("report-test-uid-out"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEventOutsideWindow)),
			ETag:          "etag-outside",
		},
	}

	baseURL, stop := startServer(t, caller)
	defer stop()

	// Window covers May 2026 only.
	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		calendarQueryWithRangeBody("20260501T000000Z", "20260601T000000Z"),
	)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusMultiStatus)
	}
	wantPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("report-test-uid-1")) + ".ics"
	if !strings.Contains(string(body), wantPath) {
		t.Errorf("inside-window event missing from response: %q", body)
	}
	dropPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("report-test-uid-out")) + ".ics"
	if strings.Contains(string(body), dropPath) {
		t.Errorf("outside-window event leaked into response: %q", body)
	}
}

// TestReportCalendarQueryCalendarNotFound confirms a REPORT against a
// calendar the AUTH'd actor has not provisioned yields 404.
func TestReportCalendarQueryCalendarNotFound(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsOutcome = wsrpc.QueryEventsCalendarNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, calendarURL(baseURL, testCalendarID), calendarQueryBody)
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

// TestReportCalendarMultigetReturnsRequestedEvent confirms a multiget
// with one href returns 207 with the requested event's decrypted body
// and ETag.
func TestReportCalendarMultigetReturnsRequestedEvent(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-multiget",
		},
	}

	baseURL, stop := startServer(t, caller)
	defer stop()

	href := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("report-test-uid-1")) + ".ics"

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		calendarMultigetBody(href),
	)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	if !strings.Contains(string(body), href) {
		t.Errorf("response missing href %q: %q", href, body)
	}
	// emersion wraps the bare etag in RFC 7232 DQUOTEs on the wire;
	// XML-encoding renders them as `&#34;` entities. Search for the
	// bare value — both XML-encoded and non-encoded forms include it.
	if !strings.Contains(string(body), "etag-multiget") {
		t.Errorf("response missing etag in body: %q", body)
	}
	if !strings.Contains(string(body), "Report test event") {
		t.Errorf("response missing decrypted SUMMARY: %q", body)
	}
}

// TestReportCalendarMultigetMissingHrefSurfaces404Inline confirms
// emersion's per-href error-response behavior surfaces a missing
// event's 404 inline in the multistatus rather than failing the whole
// REPORT.
func TestReportCalendarMultigetMissingHrefSurfaces404Inline(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryEventsEvents = nil // no events at all

	baseURL, stop := startServer(t, caller)
	defer stop()

	href := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("never-existed")) + ".ics"

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		calendarMultigetBody(href),
	)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// emersion's NewErrorResponse path emits an embedded 404 status.
	if !strings.Contains(string(body), "404") {
		t.Errorf("response missing 404 for missing href: %q", body)
	}
}

// TestListCalendarObjectsPropfindDepth1 pins the PROPFIND-depth-1 read path
// (ListCalendarObjects → nil query → every event), so a client that enumerates
// via PROPFIND rather than REPORT gets the full event set. Twin of the CardDAV
// terminator's TestListAddressObjectsPropfindDepth1.
func TestListCalendarObjectsPropfindDepth1(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	// Depth-1 PROPFIND first fetches the calendar metadata (emersion's
	// GetCalendar) before listing objects, so the calendar must be present +
	// decryptable in list_calendars.
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: "Personal",
		Color:       "#3273dc",
	}, fx.leaf.Pubkey, nil)
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	caller.listCalendarsReplies = [][]wsrpc.CalendarEntry{
		{
			{
				CalendarID:        testCalendarID,
				EncryptedMetadata: sealed,
				HighestModseq:     42,
			},
		},
	}
	caller.queryEventsEvents = []wsrpc.EventEntry{
		{
			EventID:       []byte("event-id-pf-pad-32-bytes-0000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-pf",
			Modseq:        42,
			InternalDate:  1747396800,
		},
	}
	caller.queryEventsHighestModseq = 42
	baseURL, stop := startServer(t, caller)
	defer stop()

	body := mustPropfind(t, baseURL,
		"/caldav/"+fixtureLocalPart+"@"+fixtureDomain+"/"+hex.EncodeToString(testCalendarID)+"/",
		fixtureLocalPart+"@"+fixtureDomain,
		"<d:getetag/><c:calendar-data/>", "1")
	if !strings.Contains(body, "Report test event") {
		t.Errorf("depth-1 PROPFIND missing decrypted SUMMARY: %q", body)
	}
}
