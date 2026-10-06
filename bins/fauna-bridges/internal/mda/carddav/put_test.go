package carddav

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// Test vCard bodies. Minimal but RFC-6350-valid where required; the missing-*
// variants drop exactly one required property to exercise the 400 path.
const (
	testPutValidCard = "BEGIN:VCARD\r\n" +
		"VERSION:3.0\r\n" +
		"UID:put-test-uid-001\r\n" +
		"FN:Alice Example\r\n" +
		"EMAIL:alice@example.com\r\n" +
		"TEL:+1-555-0100\r\n" +
		"END:VCARD\r\n"

	testPutCardNoUID = "BEGIN:VCARD\r\n" +
		"VERSION:3.0\r\n" +
		"FN:No UID\r\n" +
		"END:VCARD\r\n"

	testPutCardNoFN = "BEGIN:VCARD\r\n" +
		"VERSION:3.0\r\n" +
		"UID:put-test-no-fn\r\n" +
		"END:VCARD\r\n"

	testPutCardNoVersion = "BEGIN:VCARD\r\n" +
		"UID:put-test-no-version\r\n" +
		"FN:No Version\r\n" +
		"END:VCARD\r\n"
)

// putRequest builds an authenticated PUT request to the given card resource URL.
func putRequest(t *testing.T, url, body, ifMatch string) *http.Request {
	t.Helper()
	req, err := http.NewRequest(http.MethodPut, url, bytes.NewReader([]byte(body)))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	req.Header.Set("Content-Type", "text/vcard; charset=utf-8")
	if ifMatch != "" {
		req.Header.Set("If-Match", ifMatch)
	}
	return req
}

// testAddressbookID is a deterministic non-zero 32-byte addressbook id used in
// PUT/DELETE tests so request paths and assertions line up regardless of mock
// state.
var testAddressbookID = func() []byte {
	sum := sha256.Sum256([]byte("put-test-addressbook"))
	out := make([]byte, 32)
	copy(out, sum[:])
	return out
}()

// putAuthedCaller returns a mockCaller pre-wired with the AUTH flow's required
// state so a PUT request reaches the backend. PUT seals (needs mlsPubkey) but
// does not unseal, so the snapshot can stay nil.
func putAuthedCaller(t *testing.T) *mockCaller {
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

// cardURL builds the request URL for a PUT/DELETE against a card resource. The
// filename portion is arbitrary on PUT (the MDA recomputes from the parsed UID)
// but must be the canonical uid_hash hex on DELETE.
func cardURL(baseURL string, addressbookID []byte, filename string) string {
	return baseURL +
		"/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(addressbookID) +
		"/" + filename + ".vcf"
}

// canonicalUIDHashHex returns hex(blake3(uid)[:32]) — the canonical filename
// slug the MDA writes into the Location header on PUT and uses as the dedup key
// on nest.
func canonicalUIDHashHex(uid string) string {
	sum := blake3.Sum256([]byte(uid))
	return hex.EncodeToString(sum[:])
}

// TestPutAddressObjectCreates pins the create path: a fresh PUT with a valid
// vCard body lands on nest as `put_card_ciphertext`, the reply's Created outcome
// maps to 201, the ETag header carries the reply's etag, and the Location header
// carries the canonical card resource path (the filename is blake3(UID)[:32]
// hex, not the client's chosen filename). card_id/etag come FROM the reply.
func TestPutAddressObjectCreates(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putCardOutcome = wsrpc.PutCardCreated
	caller.putCardETag = "etag-for-created"
	caller.putCardID = []byte("card-id-0001-pad-32-bytes-000000")

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, cardURL(baseURL, testAddressbookID, "client-chose-this"), testPutValidCard, "")
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
	wantLocation := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) +
		"/" + canonicalUIDHashHex("put-test-uid-001") + ".vcf"
	if got := resp.Header.Get("Location"); got != wantLocation {
		t.Errorf("Location header = %q, want %q", got, wantLocation)
	}
	if puts := caller.callsOf(wsrpc.MethodPutCardCiphertext); len(puts) != 1 {
		t.Fatalf("put_card_ciphertext fired %d times, want 1", len(puts))
	}
}

// TestPutAddressObjectSealsBody pins SEAL-ALWAYS: the put_card_ciphertext
// request body must NOT contain the readable vCard FN — the encrypted_body field
// is HPKE ciphertext in BOTH storage modes (contacts have no plaintext-at-rest
// carve-out). This is the inverse of the CalDAV terminator's plaintext-mode
// test; there is no CardDAV plaintext mode to test.
func TestPutAddressObjectSealsBody(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putCardOutcome = wsrpc.PutCardCreated
	caller.putCardETag = "etag-seal"
	caller.putCardID = []byte("card-id-seal-pad-32-bytes-000000")

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, cardURL(baseURL, testAddressbookID, "seal-check"), testPutValidCard, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	puts := caller.callsOf(wsrpc.MethodPutCardCiphertext)
	if len(puts) != 1 {
		t.Fatalf("put_card_ciphertext fired %d times, want 1", len(puts))
	}
	if bytes.Contains(puts[0].body, []byte("Alice Example")) {
		t.Error("SEAL-ALWAYS violated: put_card_ciphertext body contains the readable vCard FN (must be ciphertext)")
	}
}

// TestPutAddressObjectRejectsMissingUID confirms a vCard without a UID fails
// with 400 and fires no nest RPC.
func TestPutAddressObjectRejectsMissingUID(t *testing.T) {
	assertPutRejected(t, testPutCardNoUID, "no-uid")
}

// TestPutAddressObjectRejectsMissingFN confirms a vCard without an FN fails with
// 400 and fires no nest RPC.
func TestPutAddressObjectRejectsMissingFN(t *testing.T) {
	assertPutRejected(t, testPutCardNoFN, "no-fn")
}

// TestPutAddressObjectRejectsMissingVERSION confirms a vCard without a VERSION
// fails with 400 (the encoder rejects it) and fires no nest RPC.
func TestPutAddressObjectRejectsMissingVERSION(t *testing.T) {
	assertPutRejected(t, testPutCardNoVersion, "no-version")
}

func assertPutRejected(t *testing.T, body, filename string) {
	t.Helper()
	caller := putAuthedCaller(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, cardURL(baseURL, testAddressbookID, filename), body, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	if puts := caller.callsOf(wsrpc.MethodPutCardCiphertext); len(puts) != 0 {
		t.Errorf("put_card_ciphertext fired %d times on bad body, want 0", len(puts))
	}
}

// TestPutAddressObjectStaleIfMatch confirms a PUT with a stale If-Match yields
// 412 PreconditionFailed with the current ETag surfaced.
func TestPutAddressObjectStaleIfMatch(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putCardOutcome = wsrpc.PutCardPreconditionFailed
	caller.putCardCurrentETag = "server-side-etag-123"

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, cardURL(baseURL, testAddressbookID, "stale"), testPutValidCard, `"client-stale-etag"`)
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

// TestPutAddressObjectAddressbookNotFound confirms a PUT against an unprovisioned
// address book yields 404.
func TestPutAddressObjectAddressbookNotFound(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putCardOutcome = wsrpc.PutCardAddressbookNotFound

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, cardURL(baseURL, testAddressbookID, "missing-book"), testPutValidCard, "")
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

// TestPutAddressObjectOverQuotaIs507: the card twin of the CalDAV PUT's
// over-quota answer (carddav-server.md delegates card quota to
// caldav-server.md § QUOTA → § Enforcement points) — `507` with the RFC 4918
// §15 DAV:quota-not-exceeded precondition body.
func TestPutAddressObjectOverQuotaIs507(t *testing.T) {
	caller := putAuthedCaller(t)
	caller.putCardErrCode = wsrpc.CodeOverQuota

	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, cardURL(baseURL, testAddressbookID, "over-quota"), testPutValidCard, "")
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
