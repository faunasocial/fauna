package carddav

import (
	"bytes"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// propPatchRequest builds an authenticated PROPPATCH against `url` carrying
// `body` as its propertyupdate XML. Twin of the CalDAV props_test helper.
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

// propPatchFixture seals the existing-collection metadata to the fixture leaf so
// the PROPPATCH unseal/mutate/re-seal round-trip works end-to-end. Returns a
// configured mockCaller with one address book present and the
// ProvisionAddressbook outcome staged to Updated. Twin of CalDAV's
// propPatchFixture (address books carry no color, so only displayname +
// description).
func propPatchFixture(t *testing.T, displayname, description string) (*mockCaller, reportFixture) {
	t.Helper()
	fx := newReportFixture(t)
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{
		Displayname: displayname,
		Description: description,
	}, fx.leaf.Pubkey, nil) // nil ek = classical seal
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	caller := decryptCaller(t, fx)
	caller.listAddressbooksReplies = [][]wsrpc.AddressbookEntry{
		{
			{
				AddressbookID:     testAddressbookID,
				EncryptedMetadata: sealed,
			},
		},
	}
	caller.provisionOutcome = wsrpc.ProvisionAddressbookUpdated
	return caller, fx
}

// setCreateVisibilityRetry swaps the package-level create-visibility retry
// schedule for the duration of one test (restored via t.Cleanup): a
// millisecond back-off for the race regression, nil for the genuine-404 path so
// it 404s instantly. The CardDAV package tests run sequentially (no
// t.Parallel), and the swap happens before startServer + is restored after the
// server drains, so the read in the request goroutine never races the write.
func setCreateVisibilityRetry(t *testing.T, sched []time.Duration) {
	t.Helper()
	prev := createVisibilityRetrySchedule
	createVisibilityRetrySchedule = sched
	t.Cleanup(func() { createVisibilityRetrySchedule = prev })
}

// TestPropPatchUpdatesDisplayname pins the happy-path collection PROPPATCH: a
// `<D:set>` on `displayname` round-trips through unseal/mutate/re-seal/
// provision_addressbook(update_metadata=true) and surfaces as 207 with a 200 OK
// propstat on the named prop. The mutated metadata reaches nest carrying the new
// name, and the unrelated Description survives.
func TestPropPatchUpdatesDisplayname(t *testing.T) {
	caller, fx := propPatchFixture(t, "Contacts", "my people")

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

	req := propPatchRequest(t, addressbookURL(baseURL, testAddressbookID), body)
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

	// Verify provision_addressbook fired exactly once with update_metadata=true.
	provs := caller.callsOf(wsrpc.MethodProvisionAddressbook)
	if len(provs) != 1 {
		t.Fatalf("provision_addressbook fired %d times, want 1", len(provs))
	}
	var pReq struct {
		ActorID           []byte `cbor:"actor_id"`
		AddressbookID     []byte `cbor:"addressbook_id"`
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_addressbook body: %v", err)
	}
	if !pReq.UpdateMetadata {
		t.Errorf("update_metadata = false, want true on PROPPATCH path")
	}
	if !bytes.Equal(pReq.AddressbookID, testAddressbookID) {
		t.Errorf("addressbook_id = %x, want %x", pReq.AddressbookID, testAddressbookID)
	}

	// The encrypted_metadata nest received must unseal to the new displayname
	// when opened with the fixture leaf — pins that the mutation actually
	// round-tripped through the seal layer, and that the unrelated Description
	// field was preserved.
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
	if newMeta.Description != "my people" {
		t.Errorf("unrelated Description was modified: got %q, want %q", newMeta.Description, "my people")
	}
}

// TestPropPatchUpdatesDescription pins the second recognized property — the
// CardDAV `addressbook-description` — round-trips through the same seal layer,
// leaving Displayname untouched. Address books have no color (the CalDAV third
// recognized prop), so description is CardDAV's parity coverage for a non-
// displayname collection prop.
func TestPropPatchUpdatesDescription(t *testing.T) {
	caller, fx := propPatchFixture(t, "Contacts", "old blurb")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:set>
    <D:prop>
      <C:addressbook-description>friends and family</C:addressbook-description>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, addressbookURL(baseURL, testAddressbookID), body)
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
	if !strings.Contains(string(respBody), "addressbook-description") {
		t.Errorf("response missing addressbook-description element: %q", respBody)
	}

	provs := caller.callsOf(wsrpc.MethodProvisionAddressbook)
	if len(provs) != 1 {
		t.Fatalf("provision_addressbook fired %d times, want 1", len(provs))
	}
	var pReq struct {
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_addressbook body: %v", err)
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
	if newMeta.Description != "friends and family" {
		t.Errorf("mutated description = %q, want %q", newMeta.Description, "friends and family")
	}
	if newMeta.Displayname != "Contacts" {
		t.Errorf("unrelated Displayname was modified: got %q, want %q", newMeta.Displayname, "Contacts")
	}
}

// TestPropPatchUnknownPropertyReturns403InMultistatus pins the partial-success
// path: a known prop in the same request lands 200 OK; the unknown prop lands
// 403 + cannot-modify-protected-property, in the same multistatus body. The
// recognized mutation still applies on the wire.
func TestPropPatchUnknownPropertyReturns403InMultistatus(t *testing.T) {
	caller, _ := propPatchFixture(t, "Contacts", "")

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

	req := propPatchRequest(t, addressbookURL(baseURL, testAddressbookID), body)
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
	if !strings.Contains(string(respBody), "HTTP/1.1 403 Forbidden") {
		t.Errorf("response missing 403 Forbidden propstat: %q", respBody)
	}
	if !strings.Contains(string(respBody), "cannot-modify-protected-property") {
		t.Errorf("response missing cannot-modify-protected-property error: %q", respBody)
	}
	if !strings.Contains(string(respBody), "custom-thing") {
		t.Errorf("response missing unknown prop element: %q", respBody)
	}
	// Recognized mutation still applied — nest got one provision_addressbook.
	provs := caller.callsOf(wsrpc.MethodProvisionAddressbook)
	if len(provs) != 1 {
		t.Fatalf("provision_addressbook fired %d times, want 1", len(provs))
	}
}

// TestPropPatchRejectsCardResource pins that PROPPATCH on a card resource
// (`/carddav/{user}/{ab}/{uid_hash}.vcf`) is rejected outright with 403
// Forbidden + cannot-modify-protected-property, no XML parsing, no nest RPC.
// Cards are atomic re-PUTs; per-card property mutation adds API surface for no
// product value — the CardDAV twin of CalDAV's event-resource rejection.
func TestPropPatchRejectsCardResource(t *testing.T) {
	caller, _ := propPatchFixture(t, "Contacts", "")

	baseURL, stop := startServer(t, caller)
	defer stop()

	cardResourceURL := cardURL(baseURL, testAddressbookID, canonicalUIDHashHex("any-card-uid"))

	// Body shape is irrelevant — the path-level rejection fires before parsing.
	// Send a syntactically-valid one so the test isolates the rejection axis.
	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop>
      <D:displayname>Should not happen</D:displayname>
    </D:prop>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, cardResourceURL, body)
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
	// No nest RPCs fired (no provision_addressbook, no list_addressbooks on this
	// PROPPATCH path).
	if got := caller.callsOf(wsrpc.MethodProvisionAddressbook); len(got) != 0 {
		t.Errorf("provision_addressbook fired %d times, want 0 (card-path PROPPATCH must short-circuit)", len(got))
	}
}

// TestPropPatchEmptySetIsIdempotent pins that a PROPPATCH carrying no recognized
// or unknown mutations (empty `<D:set><D:prop/></D:set>`) emits 207 without
// calling nest. MUAs sometimes batch zero-change PROPPATCH alongside other DAV
// operations; we accept it as a no-op.
func TestPropPatchEmptySetIsIdempotent(t *testing.T) {
	caller, _ := propPatchFixture(t, "Contacts", "")

	baseURL, stop := startServer(t, caller)
	defer stop()

	body := `<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop/>
  </D:set>
</D:propertyupdate>`

	req := propPatchRequest(t, addressbookURL(baseURL, testAddressbookID), body)
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
	if got := caller.callsOf(wsrpc.MethodProvisionAddressbook); len(got) != 0 {
		t.Errorf("provision_addressbook fired %d times, want 0 (empty PROPPATCH must short-circuit)", len(got))
	}
	if got := caller.callsOf(wsrpc.MethodListAddressbooks); len(got) != 0 {
		t.Errorf("list_addressbooks fired %d times, want 0 (no recognized prop → skip fetch)", len(got))
	}
}

// TestPropPatchAddressbookNotFoundReturns404 pins the 404 path: PROPPATCH
// against an address book the actor doesn't own (ListAddressbooks doesn't return
// it) surfaces as 404 before any provision_addressbook call.
func TestPropPatchAddressbookNotFoundReturns404(t *testing.T) {
	// An address book the actor genuinely doesn't own is a real 404, not a
	// create race: disable the create-visibility retry so the 404 is immediate
	// and list_addressbooks fires exactly once.
	setCreateVisibilityRetry(t, nil)
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	// list_addressbooks returns an empty list — no address book found.
	caller.listAddressbooksReplies = [][]wsrpc.AddressbookEntry{
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

	req := propPatchRequest(t, addressbookURL(baseURL, testAddressbookID), body)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusNotFound)
	}
	// Confirm no provision_addressbook fired — we 404'd before mutating.
	if got := caller.callsOf(wsrpc.MethodProvisionAddressbook); len(got) != 0 {
		t.Errorf("provision_addressbook fired %d times, want 0 (404 path must short-circuit)", len(got))
	}
	// With retries disabled, the empty read 404s on the first list_addressbooks
	// — no spurious re-reads on a genuine miss.
	if got := caller.callsOf(wsrpc.MethodListAddressbooks); len(got) != 1 {
		t.Errorf("list_addressbooks fired %d times, want 1 (nil retry schedule → no re-read)", len(got))
	}
}

// TestPropPatchAbsorbsCreateThenRenameRace pins the CardDAV twin of the CalDAV
// create-then-rename race (2026-06-08): a client that fires
// MKCOL (create address book) and the rename PROPPATCH back-to-back on separate
// connections. Because every CardDAV request shares the one MDA→nest caller and
// the nest serializes on a single SQLite connection, the PROPPATCH's
// list_addressbooks can read before the concurrent MKCOL insert is visible → an
// empty list → a spurious 404. The handler re-reads list_addressbooks under a
// bounded back-off so the in-flight insert lands.
//
// Deterministic reproduction via the reply queue: list_addressbooks returns
// EMPTY on the first call (insert not yet visible) and the provisioned book on
// the second (insert committed). RED before the retry — the single
// list_addressbooks → 404. GREEN after — the retry's second read finds it → 207
// with the rename applied.
func TestPropPatchAbsorbsCreateThenRenameRace(t *testing.T) {
	setCreateVisibilityRetry(t, []time.Duration{time.Millisecond})
	caller, fx := propPatchFixture(t, "Untitled", "")
	// Model the race: the PROPPATCH's first list_addressbooks wins the conn lock
	// before the MKCOL insert commits → empty; the retry's read sees it.
	caller.listAddressbooksReplies = append(
		[][]wsrpc.AddressbookEntry{{}}, caller.listAddressbooksReplies...,
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

	req := propPatchRequest(t, addressbookURL(baseURL, testAddressbookID), body)
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
	// The retry re-read list_addressbooks exactly once (empty → populated).
	if got := caller.callsOf(wsrpc.MethodListAddressbooks); len(got) != 2 {
		t.Fatalf("list_addressbooks fired %d times, want 2 (one racing empty read + one retry)", len(got))
	}
	// The rename actually applied: one provision_addressbook(update_metadata=true)
	// carrying the new name, which must unseal back to it under the fixture leaf.
	provs := caller.callsOf(wsrpc.MethodProvisionAddressbook)
	if len(provs) != 1 {
		t.Fatalf("provision_addressbook fired %d times, want 1", len(provs))
	}
	var pReq struct {
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
		UpdateMetadata    bool   `cbor:"update_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_addressbook body: %v", err)
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
