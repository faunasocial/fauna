package carddav

import (
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// syncCollectionBody builds a RFC 6578 {DAV:}sync-collection REPORT body with
// the given sync-token (empty → an empty <D:sync-token/> element).
func syncCollectionBody(token string) string {
	tokenEl := "<D:sync-token/>"
	if token != "" {
		tokenEl = "<D:sync-token>" + token + "</D:sync-token>"
	}
	return `<?xml version="1.0" encoding="utf-8"?>
<D:sync-collection xmlns:D="DAV:">
  ` + tokenEl + `
  <D:sync-level>1</D:sync-level>
  <D:prop><D:getetag/><C:address-data xmlns:C="urn:ietf:params:xml:ns:carddav"/></D:prop>
</D:sync-collection>`
}

// TestSyncCollectionReturnsChangesAndTombstones pins the sync-collection
// interceptor's happy path: a changed card surfaces with its decrypted
// address-data, an expunged card surfaces as a 404 tombstone, and the new
// sync-token is echoed. Also proves the interceptor peels the REPORT off before
// emersion (which would 400 on the unknown {DAV:}sync-collection root).
func TestSyncCollectionReturnsChangesAndTombstones(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncOutcome = wsrpc.SyncAddressbookSinceOk
	caller.syncChanged = []wsrpc.CardEntry{
		{
			CardID:        []byte("card-id-sc-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
			ETag:          "etag-sync-1",
		},
	}
	caller.syncExpunged = []wsrpc.ExpungedCardEntry{
		{CardID: []byte("card-id-gone-pad-32-bytes-000000"), UIDHash: uidHash("gone-uid"), Modseq: 7},
	}
	caller.syncNewToken = "99"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), syncCollectionBody("5"))
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	s := string(body)
	if !strings.Contains(s, "Bob Report") {
		t.Errorf("sync response missing decrypted changed-card FN: %q", s)
	}
	changedHref := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) + "/" + hex.EncodeToString(uidHash("report-test-uid-1")) + ".vcf"
	if !strings.Contains(s, changedHref) {
		t.Errorf("sync response missing changed href %q: %q", changedHref, s)
	}
	goneHref := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) + "/" + hex.EncodeToString(uidHash("gone-uid")) + ".vcf"
	if !strings.Contains(s, goneHref) || !strings.Contains(s, "404 Not Found") {
		t.Errorf("sync response missing expunged-tombstone 404 for %q: %q", goneHref, s)
	}
	if !strings.Contains(s, "<D:sync-token>99</D:sync-token>") {
		t.Errorf("sync response missing new sync-token 99: %q", s)
	}
}

// TestSyncCollectionStaleOutcome pins the MUA-ahead Stale outcome → RFC 6578
// §3.8 DAV:valid-sync-token (HTTP 403) so the client falls through to a full
// PROPFIND.
func TestSyncCollectionStaleOutcome(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncOutcome = wsrpc.SyncAddressbookSinceStale
	caller.syncServerModseq = 3

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), syncCollectionBody("500"))
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
		t.Errorf("stale response missing DAV:valid-sync-token: %q", body)
	}
}

// TestSyncCollectionOkStaleFlag pins the token-behind-retention-window signal
// (Ok reply with stale=true) → the same DAV:valid-sync-token 403.
func TestSyncCollectionOkStaleFlag(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncOutcome = wsrpc.SyncAddressbookSinceOk
	caller.syncStale = true

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), syncCollectionBody("1"))
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
		t.Errorf("stale-flag response missing DAV:valid-sync-token: %q", body)
	}
}

// TestSyncCollectionAddressbookNotFound pins that a sync against an
// unprovisioned book surfaces 404.
func TestSyncCollectionAddressbookNotFound(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncOutcome = wsrpc.SyncAddressbookSinceAddressbookNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), syncCollectionBody("1"))
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

// TestSyncCollectionEmptyTokenNormalizedToZero pins that an empty
// <D:sync-token/> (the initial full-sync request) reaches nest as sync_token
// "0".
func TestSyncCollectionEmptyTokenNormalizedToZero(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.syncOutcome = wsrpc.SyncAddressbookSinceOk
	caller.syncNewToken = "1"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), syncCollectionBody(""))
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusMultiStatus)
	}

	syncs := caller.callsOf(wsrpc.MethodSyncAddressbookSince)
	if len(syncs) != 1 {
		t.Fatalf("sync_addressbook_since fired %d times, want 1", len(syncs))
	}
	var decoded struct {
		SyncToken string `cbor:"sync_token"`
	}
	if err := cbor.Unmarshal(syncs[0].body, &decoded); err != nil {
		t.Fatalf("decode sync_addressbook_since body: %v", err)
	}
	if decoded.SyncToken != "0" {
		t.Errorf("empty sync-token normalized to %q, want \"0\"", decoded.SyncToken)
	}
}
