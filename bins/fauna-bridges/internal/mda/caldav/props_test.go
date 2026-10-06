package caldav

import (
	"bytes"
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// propPatchRequest builds an authenticated PROPPATCH against `url`
// carrying `body` as its propertyupdate XML.
func propPatchRequest(t *testing.T, url, body string) *http.Request {
	t.Helper()
	req, err := http.NewRequest("PROPPATCH", url, strings.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	req.Header.Set("Content-Type", "application/xml; charset=utf-8")
	return req
}

// propPatchFixture seals the existing-collection metadata to the
// fixture leaf so the PROPPATCH unseal/mutate/re-seal round-trip
// works end-to-end. Returns a configured mockCaller with one calendar
// entry present and the ProvisionCalendar outcome staged.
func propPatchFixture(t *testing.T, displayname, color, description string) (*mockCaller, reportFixture) {
	t.Helper()
	fx := newReportFixture(t)
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: displayname,
		Color:       color,
		Description: description,
	}, fx.leaf.Pubkey, nil) // nil ek = classical seal
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	caller := decryptCaller(t, fx)
	caller.listCalendarsReplies = [][]wsrpc.CalendarEntry{
		{
			{
				CalendarID:        testCalendarID,
				EncryptedMetadata: sealed,
				HighestModseq:     7,
				EventCount:        0,
				CreatedAt:         time.Now().Unix(),
			},
		},
	}
	caller.provisionOutcome = wsrpc.ProvisionCalendarUpdated
	return caller, fx
}

// setCreateVisibilityRetry swaps the package-level create-visibility retry
// schedule for the duration of one test (restored via t.Cleanup): a
// millisecond back-off for the race regression, nil for the genuine-404 path
// so it 404s instantly. The CalDAV package tests run sequentially (no
// t.Parallel), and the swap happens before startServer + is restored after the
// server drains, so the read in the request goroutine never races the write.
func setCreateVisibilityRetry(t *testing.T, sched []time.Duration) {
	t.Helper()
	prev := createVisibilityRetrySchedule
	createVisibilityRetrySchedule = sched
	t.Cleanup(func() { createVisibilityRetrySchedule = prev })
}

// TestPropPatchUpdatesDisplayname pins the happy-path collection
// PROPPATCH: a `<D:set>` on `displayname` round-trips through unseal/
// mutate/re-seal/provision_calendar(update_metadata=true) and surfaces
// as 207 with a 200 OK propstat on the named prop. The mutated metadata
// reaches nest carrying the new name.
func TestPropPatchUpdatesDisplayname(t *testing.T) {
	caller, fx := propPatchFixture(t, "Work", "#3273dc", "")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop>
      <D:displayname>Renamed</D:displayname>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	if !strings.Contains(string(respBody), "HTTP/1.1 200 OK") {
		t.Errorf("response missing 200 OK propstat: %q", respBody)
	}
	if !strings.Contains(string(respBody), "displayname") {
		t.Errorf("response missing displayname element: %q", respBody)
	}

	// Verify provision_calendar fired exactly once with update_metadata=true.
	provs := caller.callsOf(wsrpc.MethodProvisionCalendar)
	if len(provs) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(provs))
	}
	var pReq struct {
		ActorID           []byte `cbor:"actor_id"`
		CalendarID        []byte `cbor:"calendar_id"`
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_calendar body: %v", err)
	}
	if !pReq.UpdateMetadata {
		t.Errorf("update_metadata = false, want true on PROPPATCH path")
	}
	if !bytes.Equal(pReq.CalendarID, testCalendarID) {
		t.Errorf("calendar_id = %x, want %x", pReq.CalendarID, testCalendarID)
	}

	// The encrypted_metadata nest received must unseal to the new
	// displayname when opened with the fixture leaf — pins that the
	// mutation actually round-tripped through the seal layer.
	openedBytes, err := openSealedMetadata(t, pReq.EncryptedMetadata, fx)
	if err != nil {
		t.Fatalf("re-open mutated metadata: %v", err)
	}
	var newMeta EncryptedCollectionMetadata
	if err := cbor.Unmarshal(openedBytes, &newMeta); err != nil {
		t.Fatalf("decode mutated metadata: %v", err)
	}
	if newMeta.Displayname != "Renamed" {
		t.Errorf("mutated displayname = %q, want %q", newMeta.Displayname, "Renamed")
	}
	if newMeta.Color != "#3273dc" {
		t.Errorf("unrelated Color was modified: got %q, want %q", newMeta.Color, "#3273dc")
	}
}

// TestPropPatchUpdatesDescription pins the second recognized property — the
// CalDAV `calendar-description` — round-trips through the same seal layer,
// leaving Displayname + Color untouched. Twin of the CardDAV terminator's
// TestPropPatchUpdatesDescription.
func TestPropPatchUpdatesDescription(t *testing.T) {
	caller, fx := propPatchFixture(t, "Work", "#3273dc", "old blurb")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:set>
    <D:prop>
      <C:calendar-description>team calendar</C:calendar-description>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	if !strings.Contains(string(respBody), "HTTP/1.1 200 OK") {
		t.Errorf("response missing 200 OK propstat: %q", respBody)
	}
	if !strings.Contains(string(respBody), "calendar-description") {
		t.Errorf("response missing calendar-description element: %q", respBody)
	}

	provs := caller.callsOf(wsrpc.MethodProvisionCalendar)
	if len(provs) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(provs))
	}
	var pReq struct {
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_calendar body: %v", err)
	}
	if !pReq.UpdateMetadata {
		t.Errorf("update_metadata = false, want true on PROPPATCH path")
	}
	openedBytes, err := openSealedMetadata(t, pReq.EncryptedMetadata, fx)
	if err != nil {
		t.Fatalf("re-open mutated metadata: %v", err)
	}
	var newMeta EncryptedCollectionMetadata
	if err := cbor.Unmarshal(openedBytes, &newMeta); err != nil {
		t.Fatalf("decode mutated metadata: %v", err)
	}
	if newMeta.Description != "team calendar" {
		t.Errorf("mutated description = %q, want %q", newMeta.Description, "team calendar")
	}
	if newMeta.Displayname != "Work" {
		t.Errorf("unrelated Displayname was modified: got %q, want %q", newMeta.Displayname, "Work")
	}
	if newMeta.Color != "#3273dc" {
		t.Errorf("unrelated Color was modified: got %q, want %q", newMeta.Color, "#3273dc")
	}
}

// TestPropPatchUpdatesColor pins the third recognized property — the Apple-
// namespace `calendar-color` (the form real MUAs like Apple Calendar send) —
// round-trips through the same seal layer, leaving Displayname + Description
// untouched. CalDAV-only: address books have no color prop, so this test has
// deliberately no CardDAV twin.
func TestPropPatchUpdatesColor(t *testing.T) {
	caller, fx := propPatchFixture(t, "Work", "#3273dc", "notes")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:A="http://apple.com/ns/ical/">
  <D:set>
    <D:prop>
      <A:calendar-color>#ff5733</A:calendar-color>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	if !strings.Contains(string(respBody), "HTTP/1.1 200 OK") {
		t.Errorf("response missing 200 OK propstat: %q", respBody)
	}
	if !strings.Contains(string(respBody), "calendar-color") {
		t.Errorf("response missing calendar-color element: %q", respBody)
	}

	provs := caller.callsOf(wsrpc.MethodProvisionCalendar)
	if len(provs) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(provs))
	}
	var pReq struct {
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_calendar body: %v", err)
	}
	if !pReq.UpdateMetadata {
		t.Errorf("update_metadata = false, want true on PROPPATCH path")
	}
	openedBytes, err := openSealedMetadata(t, pReq.EncryptedMetadata, fx)
	if err != nil {
		t.Fatalf("re-open mutated metadata: %v", err)
	}
	var newMeta EncryptedCollectionMetadata
	if err := cbor.Unmarshal(openedBytes, &newMeta); err != nil {
		t.Fatalf("decode mutated metadata: %v", err)
	}
	if newMeta.Color != "#ff5733" {
		t.Errorf("mutated color = %q, want %q", newMeta.Color, "#ff5733")
	}
	if newMeta.Displayname != "Work" {
		t.Errorf("unrelated Displayname was modified: got %q, want %q", newMeta.Displayname, "Work")
	}
	if newMeta.Description != "notes" {
		t.Errorf("unrelated Description was modified: got %q, want %q", newMeta.Description, "notes")
	}
}

// TestPropPatchUnknownPropertyReturns403InMultistatus pins the
// partial-success path per goal doc § Write surface row 104: a known
// prop in the same request lands 200 OK; the unknown prop lands 403 +
// cannot-modify-protected-property, in the same multistatus body. The
// recognized mutation still applies on the wire.
func TestPropPatchUnknownPropertyReturns403InMultistatus(t *testing.T) {
	caller, _ := propPatchFixture(t, "Work", "#3273dc", "")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:X="http://example.com/custom-ns">
  <D:set>
    <D:prop>
      <D:displayname>Mixed</D:displayname>
      <X:custom-thing>nope</X:custom-thing>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	// Both propstat groups present.
	if !strings.Contains(string(respBody), "HTTP/1.1 200 OK") {
		t.Errorf("response missing 200 OK propstat: %q", respBody)
	}
	if !strings.Contains(string(respBody), "HTTP/1.1 403 Forbidden") {
		t.Errorf("response missing 403 Forbidden propstat: %q", respBody)
	}
	if !strings.Contains(string(respBody), "cannot-modify-protected-property") {
		t.Errorf("response missing cannot-modify-protected-property error: %q", respBody)
	}
	if !strings.Contains(string(respBody), "custom-thing") {
		t.Errorf("response missing unknown prop element: %q", respBody)
	}
	// Recognized mutation still applied — nest got one provision_calendar.
	provs := caller.callsOf(wsrpc.MethodProvisionCalendar)
	if len(provs) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(provs))
	}
}

// TestPropPatchRejectsEventResource pins goal doc § Write surface row
// 105: PROPPATCH on `/caldav/{user}/{cal}/{file}.ics` is rejected
// outright with 403 Forbidden + cannot-modify-protected-property, no
// XML parsing, no nest RPC.
func TestPropPatchRejectsEventResource(t *testing.T) {
	caller, _ := propPatchFixture(t, "Work", "#3273dc", "")

	baseURL, stop := startServer(t, caller)
	defer stop()

	eventURL := baseURL +
		"/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("any-event-uid")) + ".ics"

	// Body shape is irrelevant — the path-level rejection fires before
	// parsing. Send a syntactically-valid one so the test isolates the
	// rejection axis.
	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop>
      <D:displayname>Should not happen</D:displayname>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, eventURL, body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusForbidden, respBody)
	}
	if !strings.Contains(string(respBody), "cannot-modify-protected-property") {
		t.Errorf("response missing cannot-modify-protected-property error: %q", respBody)
	}
	// No nest RPCs fired (no provision_calendar, no list_calendars on
	// this PROPPATCH path).
	if got := caller.callsOf(wsrpc.MethodProvisionCalendar); len(got) != 0 {
		t.Errorf("provision_calendar fired %d times, want 0 (event-path PROPPATCH must short-circuit)", len(got))
	}
}

// TestPropPatchEmptySetIsIdempotent pins that a PROPPATCH carrying no
// recognized or unknown mutations (empty `<D:set><D:prop/></D:set>`)
// emits 207 without calling nest. MUAs sometimes batch zero-change
// PROPPATCH alongside other DAV operations; we accept it as a no-op.
func TestPropPatchEmptySetIsIdempotent(t *testing.T) {
	caller, _ := propPatchFixture(t, "Work", "#3273dc", "")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop/>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	// No nest RPC fired on the no-op path.
	if got := caller.callsOf(wsrpc.MethodProvisionCalendar); len(got) != 0 {
		t.Errorf("provision_calendar fired %d times, want 0 (empty PROPPATCH must short-circuit)", len(got))
	}
	if got := caller.callsOf(wsrpc.MethodListCalendars); len(got) != 0 {
		t.Errorf("list_calendars fired %d times, want 0 (no recognized prop → skip fetch)", len(got))
	}
}

// TestPropPatchCalendarNotFoundReturns404 pins the 404 path: PROPPATCH
// against a calendar the actor doesn't own (ListCalendars doesn't
// return it) surfaces as 404 before any provision_calendar call.
func TestPropPatchCalendarNotFoundReturns404(t *testing.T) {
	// A calendar the actor genuinely doesn't own is a real 404, not a create
	// race: disable the create-visibility retry so the 404 is immediate and
	// list_calendars fires exactly once (the retried-then-resolved race is
	// pinned separately by TestPropPatchAbsorbsCreateThenRenameRace).
	setCreateVisibilityRetry(t, nil)
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	// list_calendars returns an empty list — no calendar found.
	caller.listCalendarsReplies = [][]wsrpc.CalendarEntry{
		{},
	}

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop>
      <D:displayname>Renamed</D:displayname>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNotFound)
	}
	// Confirm no provision_calendar fired — we 404'd before mutating.
	if got := caller.callsOf(wsrpc.MethodProvisionCalendar); len(got) != 0 {
		t.Errorf("provision_calendar fired %d times, want 0 (404 path must short-circuit)", len(got))
	}
	// With retries disabled, the empty read 404s on the first list_calendars —
	// no spurious re-reads on a genuine miss.
	if got := caller.callsOf(wsrpc.MethodListCalendars); len(got) != 1 {
		t.Errorf("list_calendars fired %d times, want 1 (nil retry schedule → no re-read)", len(got))
	}
}

// TestPropPatchAbsorbsCreateThenRenameRace pins the bug
// (2026-06-08): macOS Calendar.app's "add a calendar and rename it before
// tabbing away" fires MKCALENDAR and the rename PROPPATCH back-to-back on
// separate connections. Because every CalDAV request shares the one MDA→nest
// caller and the nest serializes on a single SQLite connection, the PROPPATCH's
// list_calendars can read before the concurrent MKCALENDAR insert is visible →
// an empty list → a spurious 404 that makes Calendar.app roll the name back to
// its local "Untitled" placeholder. The handler now re-reads list_calendars
// under a bounded back-off so the in-flight insert lands.
//
// Deterministic reproduction of the race via the reply queue: list_calendars
// returns EMPTY on the first call (insert not yet visible) and the provisioned
// calendar on the second (insert committed). RED before the retry — the single
// list_calendars → 404. GREEN after — the retry's second read finds it → 207
// with the rename applied. (The post-create rename "sticks" path is the
// no-retry case every other PROPPATCH test already covers.)
func TestPropPatchAbsorbsCreateThenRenameRace(t *testing.T) {
	setCreateVisibilityRetry(t, []time.Duration{time.Millisecond})
	caller, fx := propPatchFixture(t, "Untitled", "#3273dc", "")
	// Model the race: the PROPPATCH's first list_calendars wins the conn lock
	// before the MKCALENDAR insert commits → empty; the retry's read sees it.
	caller.listCalendarsReplies = append(
		[][]wsrpc.CalendarEntry{{}}, caller.listCalendarsReplies...,
	)

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop>
      <D:displayname>Renamed</D:displayname>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, calendarURL(baseURL, testCalendarID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d — the rename must survive a create race, not 404 (body=%q)",
			resp.StatusCode, http.StatusMultiStatus, respBody)
	}
	if !strings.Contains(string(respBody), "HTTP/1.1 200 OK") {
		t.Errorf("response missing 200 OK propstat: %q", respBody)
	}
	// The retry re-read list_calendars exactly once (empty → populated).
	if got := caller.callsOf(wsrpc.MethodListCalendars); len(got) != 2 {
		t.Fatalf("list_calendars fired %d times, want 2 (one racing empty read + one retry)", len(got))
	}
	// The rename actually applied: one provision_calendar(update_metadata=true)
	// carrying the new name, which must unseal back to it under the fixture leaf.
	provs := caller.callsOf(wsrpc.MethodProvisionCalendar)
	if len(provs) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(provs))
	}
	var pReq struct {
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_calendar body: %v", err)
	}
	if !pReq.UpdateMetadata {
		t.Errorf("update_metadata = false, want true on the rename path")
	}
	openedBytes, err := openSealedMetadata(t, pReq.EncryptedMetadata, fx)
	if err != nil {
		t.Fatalf("re-open mutated metadata: %v", err)
	}
	var newMeta EncryptedCollectionMetadata
	if err := cbor.Unmarshal(openedBytes, &newMeta); err != nil {
		t.Fatalf("decode mutated metadata: %v", err)
	}
	if newMeta.Displayname != "Renamed" {
		t.Errorf("create-race rename lost: displayname = %q, want %q", newMeta.Displayname, "Renamed")
	}
}

// openSealedMetadata is the test-side counterpart to
// SealCollectionMetadata: HPKE-opens a sealed metadata blob using the
// fixture leaf's secret. Mirrors the same MlsCapability + snapshot-
// plaintext path the AUTH middleware builds server-side so the test
// can verify the mutated plaintext nest receives actually carries the
// new field values.
func openSealedMetadata(t *testing.T, sealed []byte, fx reportFixture) ([]byte, error) {
	t.Helper()
	blob := mustReadFixture(t, "wrapped_msek.bin")
	cap, err := mailfauna.UnwrapMLSBlob(
		blob, fixturePlainPassword,
		fixtureActorID, fixtureCredentialID, mailfauna.KdfKindArgon2id,
	)
	if err != nil {
		return nil, err
	}
	defer cap.Zeroize()
	snapshotPlaintext, err := cap.Decrypt(fx.snapshotBlob)
	if err != nil {
		return nil, err
	}
	opener, err := mailfauna.NewMailRecordOpener(snapshotPlaintext)
	if err != nil {
		return nil, err
	}
	defer opener.Zeroize()
	return opener.Open(sealed)
}
