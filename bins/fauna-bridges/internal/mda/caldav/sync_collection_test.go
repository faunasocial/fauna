package caldav

import (
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// syncCollectionFullSyncBody is the canonical RFC 6578 §3.2 request
// for a fresh sync (sync-token=0, sync-level=1). Modern MUAs send this
// on the first sync of any calendar.
const syncCollectionFullSyncBody = `<?xml version="1.0" encoding="utf-8"?>
<D:sync-collection xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:sync-token>0</D:sync-token>
  <D:sync-level>1</D:sync-level>
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
</D:sync-collection>`

// syncCollectionIncrementalBody resumes a sync from a prior token. The
// token shape is opaque to the MUA (decimal modseq on our wire).
func syncCollectionIncrementalBody(token string) string {
	return `<?xml version="1.0" encoding="utf-8"?>
<D:sync-collection xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:sync-token>` + token + `</D:sync-token>
  <D:sync-level>1</D:sync-level>
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
</D:sync-collection>`
}

// TestSyncCollectionFullSyncReturnsChangedAndToken pins the full-sync
// path: a sync-token of "0" → server returns every event as a change
// + the current new_sync_token suffix. The multistatus body MUST carry
// one <response> per event AND the <sync-token> element at the
// closing.
func TestSyncCollectionFullSyncReturnsChangedAndToken(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceChanged = []wsrpc.EventEntry{
		{
			UIDHash:       uidHash("sync-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-changed-1",
		},
	}
	caller.syncCalendarSinceNewToken = "100"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionFullSyncBody,
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
	// One sync_calendar_since RPC fired with sync_token="0".
	syncs := caller.callsOf(wsrpc.MethodSyncCalendarSince)
	if len(syncs) != 1 {
		t.Fatalf("sync_calendar_since fired %d times, want 1", len(syncs))
	}
	var sReq struct {
		ActorID    []byte `cbor:"actor_id"`
		CalendarID []byte `cbor:"calendar_id"`
		SyncToken  string `cbor:"sync_token"`
	}
	if err := cbor.Unmarshal(syncs[0].body, &sReq); err != nil {
		t.Fatalf("decode sync_calendar_since: %v", err)
	}
	if sReq.SyncToken != "0" {
		t.Errorf("sync_token = %q, want %q", sReq.SyncToken, "0")
	}
	// Multistatus carries the changed event's resource path.
	wantPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("sync-test-uid-1")) + ".ics"
	if !strings.Contains(string(body), wantPath) {
		t.Errorf("response missing changed-event href %q: %q", wantPath, body)
	}
	if !strings.Contains(string(body), "Report test event") {
		t.Errorf("response missing decrypted body: %q", body)
	}
	// New sync-token element at multistatus closing.
	if !strings.Contains(string(body), "<sync-token>100</sync-token>") &&
		!strings.Contains(string(body), "<D:sync-token>100</D:sync-token>") {
		t.Errorf("response missing new <sync-token>100</sync-token>: %q", body)
	}
}

// TestSyncCollectionSkipsRawEvent pins the sync-collection REPORT path's
// refusal of an unsealed changed event: every event rests sealed, so a raw
// body is skipped + logged, never served verbatim, while its sealed sibling
// HPKE-opens via the per-session opener.
func TestSyncCollectionSkipsRawEvent(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceChanged = []wsrpc.EventEntry{
		{
			UIDHash:       uidHash("sync-test-uid-raw"),
			EncryptedBody: []byte(testReportEventOutsideWindow), // RAW — never a resting shape
			ETag:          "etag-raw",
		},
		{
			UIDHash:       uidHash("sync-test-uid-sealed"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-sealed",
		},
	}
	caller.syncCalendarSinceNewToken = "100"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionFullSyncBody,
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
	if strings.Contains(string(body), "Outside window") {
		t.Errorf("raw changed event must be skipped, never served verbatim: %q", body)
	}
	if !strings.Contains(string(body), "Report test event") {
		t.Errorf("sealed changed event missing from the same one-path sync REPORT: %q", body)
	}
}

// TestSyncCollectionIncrementalSurfacesTombstones confirms the
// incremental sync path: changed events appear with 200 + body, and
// expunged events appear with 404 status per RFC 6578 §3.6.
func TestSyncCollectionIncrementalSurfacesTombstones(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceChanged = []wsrpc.EventEntry{
		{
			UIDHash:       uidHash("sync-test-uid-1"),
			EncryptedBody: fx.sealEvent(t, []byte(testReportEvent)),
			ETag:          "etag-changed",
		},
	}
	caller.syncCalendarSinceExpunged = []wsrpc.ExpungedEntry{
		{
			UIDHash: uidHash("sync-test-uid-deleted"),
			Modseq:  98,
		},
	}
	caller.syncCalendarSinceNewToken = "150"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionIncrementalBody("99"),
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
	// Changed event present.
	changedPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("sync-test-uid-1")) + ".ics"
	if !strings.Contains(string(body), changedPath) {
		t.Errorf("response missing changed-event href %q: %q", changedPath, body)
	}
	// Tombstone present with 404 status.
	expungedPath := "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/" +
		hex.EncodeToString(testCalendarID) + "/" +
		hex.EncodeToString(uidHash("sync-test-uid-deleted")) + ".ics"
	if !strings.Contains(string(body), expungedPath) {
		t.Errorf("response missing tombstone href %q: %q", expungedPath, body)
	}
	if !strings.Contains(string(body), "404") {
		t.Errorf("response missing tombstone 404 status: %q", body)
	}
	// sync-token rolled forward.
	if !strings.Contains(string(body), "150") {
		t.Errorf("response missing new sync-token 150: %q", body)
	}
	// Verify the sync RPC carried the incremental token.
	syncs := caller.callsOf(wsrpc.MethodSyncCalendarSince)
	if len(syncs) != 1 {
		t.Fatalf("sync_calendar_since fired %d times, want 1", len(syncs))
	}
	var sReq struct {
		SyncToken string `cbor:"sync_token"`
	}
	if err := cbor.Unmarshal(syncs[0].body, &sReq); err != nil {
		t.Fatalf("decode sync_calendar_since: %v", err)
	}
	if sReq.SyncToken != "99" {
		t.Errorf("sync_token = %q, want %q", sReq.SyncToken, "99")
	}
}

// TestSyncCollectionCalendarNotFound confirms a sync-collection
// against a missing calendar returns 404.
func TestSyncCollectionCalendarNotFound(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceOutcome = wsrpc.SyncCalendarSinceCalendarNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionFullSyncBody,
	)
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

// TestSyncCollectionStaleTokenReturns403 confirms that when the nest
// returns SyncCalendarSinceStale (the post-DR-restore "MUA ahead" case,
// spec § D6 (γ)), the REPORT handler emits HTTP 403 with the RFC 6578
// §3.8 DAV:valid-sync-token precondition-failure XML body so the MUA
// falls through to a full PROPFIND.
func TestSyncCollectionStaleTokenReturns403(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceOutcome = wsrpc.SyncCalendarSinceStale
	caller.syncCalendarSinceServerModseq = 42

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionIncrementalBody("99"),
	)
	req.Header.Set("User-Agent", "Apple Calendar/14.0")

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	// Handler must return 403.
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusForbidden, body)
	}
	// Body must carry the DAV:valid-sync-token error element per RFC 6578 §3.8.
	if !strings.Contains(string(body), "valid-sync-token") {
		t.Errorf("response body missing DAV:valid-sync-token element: %q", body)
	}
	// Verify the RPC was called with the forwarded User-Agent as mua_id.
	syncs := caller.callsOf(wsrpc.MethodSyncCalendarSince)
	if len(syncs) != 1 {
		t.Fatalf("sync_calendar_since fired %d times, want 1", len(syncs))
	}
	var sReq struct {
		MuaID *string `cbor:"mua_id"`
	}
	if err := cbor.Unmarshal(syncs[0].body, &sReq); err != nil {
		t.Fatalf("decode sync_calendar_since request: %v", err)
	}
	if sReq.MuaID == nil || *sReq.MuaID != "Apple Calendar/14.0" {
		t.Errorf("mua_id = %v, want %q", sReq.MuaID, "Apple Calendar/14.0")
	}
}

// TestSyncCollectionStaleOkTokenReturns403 confirms that when nest returns
// an Ok reply with stale=true (the supplied sync-token is valid but
// predates the tombstone-retention window, caldav-server.md § Stale
// sync-token handling), the REPORT handler emits the same RFC 6578 §3.8
// DAV:valid-sync-token 403 as the Stale outcome — even though Changed /
// Expunged would otherwise be present — so the MUA full-resyncs.
func TestSyncCollectionStaleOkTokenReturns403(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceOutcome = wsrpc.SyncCalendarSinceOk
	caller.syncCalendarSinceStale = true
	caller.syncCalendarSinceNewToken = "7"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionIncrementalBody("3"),
	)

	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusForbidden, body)
	}
	if !strings.Contains(string(body), "valid-sync-token") {
		t.Errorf("response body missing DAV:valid-sync-token element: %q", body)
	}
}

// TestSyncCollectionEmptyResultStillCarriesToken confirms a sync
// against an empty calendar returns 207 with an empty <response> list
// plus the current <sync-token>. MUAs use this to advance their
// stored token on otherwise-idle calendars.
func TestSyncCollectionEmptyResultStillCarriesToken(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncCalendarSinceChanged = nil
	caller.syncCalendarSinceExpunged = nil
	caller.syncCalendarSinceNewToken = "200"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t,
		calendarURL(baseURL, testCalendarID),
		syncCollectionIncrementalBody("200"),
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
	if !strings.Contains(string(body), "200") {
		t.Errorf("response missing sync-token 200: %q", body)
	}
}
