package carddav

import (
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// deleteRequest builds an authenticated DELETE against a card resource. The
// filename MUST be the canonical uid_hash hex (DELETE addresses by slug).
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

// TestDeleteAddressObjectDeletes pins the DELETE path: a DELETE of an existing
// card resource lands on nest as `delete_card`, the Deleted outcome maps to 204.
func TestDeleteAddressObjectDeletes(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.deleteCardOutcome = wsrpc.DeleteCardDeleted
	caller.deleteCardID = []byte("card-id-del-pad-32-bytes-0000000")

	baseURL, stop := startServer(t, caller)
	defer stop()

	url := cardURL(baseURL, testAddressbookID, canonicalUIDHashHex("some-card-uid"))
	resp, err := httpsClient().Do(deleteRequest(t, url, ""))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNoContent)
	}
	if dels := caller.callsOf(wsrpc.MethodDeleteCard); len(dels) != 1 {
		t.Fatalf("delete_card fired %d times, want 1", len(dels))
	}
}

// TestDeleteAddressObjectNotFound pins that deleting a missing card yields 404.
func TestDeleteAddressObjectNotFound(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.deleteCardOutcome = wsrpc.DeleteCardNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	url := cardURL(baseURL, testAddressbookID, canonicalUIDHashHex("missing-uid"))
	resp, err := httpsClient().Do(deleteRequest(t, url, ""))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNotFound)
	}
}

// TestDeleteAddressObjectStaleIfMatch pins that a DELETE with a stale If-Match
// yields 412 with the current ETag surfaced. Also proves the If-Match ctx-stash
// middleware plumbs the header to delete_card (emersion's DeleteAddressObject
// signature carries no options).
func TestDeleteAddressObjectStaleIfMatch(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.deleteCardOutcome = wsrpc.DeleteCardPreconditionFailed
	caller.deleteCardCurrentETag = "server-etag-del-99"

	baseURL, stop := startServer(t, caller)
	defer stop()

	url := cardURL(baseURL, testAddressbookID, canonicalUIDHashHex("stale-del-uid"))
	resp, err := httpsClient().Do(deleteRequest(t, url, `"client-stale"`))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusPreconditionFailed {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusPreconditionFailed, body)
	}
	if !strings.Contains(string(body), "server-etag-del-99") {
		t.Errorf("response body missing current etag: %q", body)
	}
	// The If-Match value must have reached nest (not nil).
	dels := caller.callsOf(wsrpc.MethodDeleteCard)
	if len(dels) != 1 {
		t.Fatalf("delete_card fired %d times, want 1", len(dels))
	}
}

// TestDeleteAddressObjectMalformedPath pins that a DELETE whose filename lacks
// the .vcf suffix is rejected 400 before any RPC fires.
func TestDeleteAddressObjectMalformedPath(t *testing.T) {
	caller := putAuthedCaller(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	// No .vcf suffix — parseCardResourcePath rejects it.
	url := baseURL + "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) + "/not-a-vcf-file"
	resp, err := httpsClient().Do(deleteRequest(t, url, ""))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	if dels := caller.callsOf(wsrpc.MethodDeleteCard); len(dels) != 0 {
		t.Errorf("delete_card fired %d times on malformed path, want 0", len(dels))
	}
}

// TestDeleteAddressBookDeletes pins the address-book collection DELETE path: a
// DELETE of a collection URL lands on nest as `delete_addressbook`, and the
// Deleted outcome maps to 204 (the whole book + all its cards are cascade-
// deleted nest-side).
func TestDeleteAddressBookDeletes(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.deleteAddressbookOutcome = wsrpc.DeleteAddressbookDeleted
	caller.deleteAddressbookCardsDeleted = 3

	baseURL, stop := startServer(t, caller)
	defer stop()

	// Collection URL (depth-3, trailing slash) → resourceTypeAddressBook →
	// DeleteAddressBook.
	url := addressbookURL(baseURL, testAddressbookID)
	resp, err := httpsClient().Do(deleteRequest(t, url, ""))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNoContent)
	}
	if dels := caller.callsOf(wsrpc.MethodDeleteAddressbook); len(dels) != 1 {
		t.Fatalf("delete_addressbook fired %d times, want 1", len(dels))
	}
	// No card-level RPC fires — the cascade happens nest-side.
	if dels := caller.callsOf(wsrpc.MethodDeleteCard); len(dels) != 0 {
		t.Errorf("delete_card fired %d times on address-book DELETE, want 0", len(dels))
	}
}

// TestDeleteAddressBookNotFound pins that deleting a missing address book yields
// 404 (nest returns not_found; idempotent re-delete).
func TestDeleteAddressBookNotFound(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.deleteAddressbookOutcome = wsrpc.DeleteAddressbookNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	url := addressbookURL(baseURL, testAddressbookID)
	resp, err := httpsClient().Do(deleteRequest(t, url, ""))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNotFound)
	}
	if dels := caller.callsOf(wsrpc.MethodDeleteAddressbook); len(dels) != 1 {
		t.Fatalf("delete_addressbook fired %d times, want 1", len(dels))
	}
}
