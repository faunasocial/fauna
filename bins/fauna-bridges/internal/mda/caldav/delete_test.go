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

// deleteRequest builds an authenticated DELETE request with an optional
// If-Match header.
func deleteRequest(t *testing.T, url, ifMatch string) *http.Request {
	t.Helper()
	req, err := http.NewRequest(http.MethodDelete, url, nil)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	if ifMatch != "" {
		req.Header.Set("If-Match", ifMatch)
	}
	return req
}

// deleteAuthedCaller mirrors putAuthedCaller — provisions the AUTH
// flow so the request reaches the backend.
func deleteAuthedCaller(t *testing.T) *mockCaller {
	t.Helper()
	blob := mustReadFixture(t, "wrapped_msek.bin")
	return &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            blob,
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
		mlsSnapshotBlob:        nil,
	}
}

// canonicalUIDHashHex is shared with put_test.go (same package).
//
// TestDeleteCalendarObjectDeleted pins the success path: DELETE
// against an existing event yields 204 No Content (emersion writes
// 204 itself when the backend returns nil error) and a single
// `delete_event` RPC fires.
func TestDeleteCalendarObjectDeleted(t *testing.T) {
	caller := deleteAuthedCaller(t)
	caller.deleteEventOutcome = wsrpc.DeleteEventDeleted
	caller.deleteEventID = []byte("event-id-0001-pad-32-bytes-00000")
	caller.deleteEventModseq = 42

	baseURL, stop := startServer(t, caller)
	defer stop()

	uidHashHex := canonicalUIDHashHex("delete-test-uid-001")
	req := deleteRequest(t, eventURL(baseURL, testCalendarID, uidHashHex), "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNoContent)
	}

	// Exactly one delete_event RPC fired against the right path.
	dels := caller.callsOf(wsrpc.MethodDeleteEvent)
	if len(dels) != 1 {
		t.Fatalf("delete_event fired %d times, want 1", len(dels))
	}
	// The actor + calendar + uid_hash must match the URL.
	var got struct {
		ActorID    []byte  `cbor:"actor_id"`
		CalendarID []byte  `cbor:"calendar_id"`
		UIDHash    []byte  `cbor:"uid_hash"`
		IfMatch    *string `cbor:"if_match"`
	}
	if err := cbor.Unmarshal(dels[0].body, &got); err != nil {
		t.Fatalf("decode delete_event request: %v", err)
	}
	if !equalBytes(got.ActorID, fixtureActorID) {
		t.Errorf("delete_event actor_id = %x, want %x", got.ActorID, fixtureActorID)
	}
	if !equalBytes(got.CalendarID, testCalendarID) {
		t.Errorf("delete_event calendar_id = %x, want %x", got.CalendarID, testCalendarID)
	}
	wantUIDHash, _ := hex.DecodeString(uidHashHex)
	if !equalBytes(got.UIDHash, wantUIDHash) {
		t.Errorf("delete_event uid_hash = %x, want %x", got.UIDHash, wantUIDHash)
	}
	if got.IfMatch != nil {
		t.Errorf("delete_event if_match = %v, want nil (unconditional)", got.IfMatch)
	}
}

// TestDeleteCalendarObjectStaleIfMatch confirms a DELETE with a stale
// If-Match yields 412 PreconditionFailed and the current ETag the
// row holds is surfaced in the response body.
func TestDeleteCalendarObjectStaleIfMatch(t *testing.T) {
	caller := deleteAuthedCaller(t)
	caller.deleteEventOutcome = wsrpc.DeleteEventPreconditionFailed
	caller.deleteEventCurrentETag = "server-side-etag-xyz"

	baseURL, stop := startServer(t, caller)
	defer stop()

	uidHashHex := canonicalUIDHashHex("delete-test-stale")
	req := deleteRequest(t,
		eventURL(baseURL, testCalendarID, uidHashHex),
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
	if !strings.Contains(string(body), "server-side-etag-xyz") {
		t.Errorf("response body missing current etag: %q", body)
	}

	// The DELETE RPC must carry the parsed If-Match value, not nil.
	dels := caller.callsOf(wsrpc.MethodDeleteEvent)
	if len(dels) != 1 {
		t.Fatalf("delete_event fired %d times, want 1", len(dels))
	}
	var got struct {
		IfMatch *string `cbor:"if_match"`
	}
	if err := cbor.Unmarshal(dels[0].body, &got); err != nil {
		t.Fatalf("decode delete_event request: %v", err)
	}
	if got.IfMatch == nil || *got.IfMatch != "client-stale-etag" {
		t.Errorf("delete_event if_match = %v, want &%q", got.IfMatch, "client-stale-etag")
	}
}

// TestDeleteCalendarObjectNotFound confirms DELETE against an event
// (or calendar) that does not exist yields 404. Per goal doc § Write
// surface row 102: NotFound collapses event-missing vs calendar-
// missing.
func TestDeleteCalendarObjectNotFound(t *testing.T) {
	caller := deleteAuthedCaller(t)
	caller.deleteEventOutcome = wsrpc.DeleteEventNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	uidHashHex := canonicalUIDHashHex("delete-test-missing")
	req := deleteRequest(t, eventURL(baseURL, testCalendarID, uidHashHex), "")
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

// TestDeleteCalendarObjectRejectsMalformedPath confirms a DELETE
// against a URL where the filename is not a valid 32-byte hex
// uid_hash yields 400 before reaching the nest RPC. Defense against
// MUAs that construct event paths arbitrarily.
func TestDeleteCalendarObjectRejectsMalformedPath(t *testing.T) {
	caller := deleteAuthedCaller(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := deleteRequest(t, eventURL(baseURL, testCalendarID, "not-a-hex-uid"), "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	if dels := caller.callsOf(wsrpc.MethodDeleteEvent); len(dels) != 0 {
		t.Errorf("delete_event fired %d times on malformed path, want 0", len(dels))
	}
}
