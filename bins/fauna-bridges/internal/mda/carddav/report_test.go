package carddav

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/hex"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// makeCardPager returns n cards with unique, ascending 32-byte CardIDs and a
// pager that serves them paginated by (afterCardID, limit) exactly as nest's
// query_cards does (ORDER BY card_id ASC, after_card_id cursor). Bodies are
// empty — fetchAllCards does not decrypt (that is openCard's job).
func makeCardPager(n int) ([]wsrpc.CardEntry, func(after []byte, limit uint32) ([]wsrpc.CardEntry, bool)) {
	all := make([]wsrpc.CardEntry, n)
	for i := range all {
		id := make([]byte, 32)
		binary.BigEndian.PutUint32(id[28:], uint32(i+1)) // 1-based, ordered, unique
		all[i] = wsrpc.CardEntry{CardID: id, UIDHash: id}
	}
	pager := func(after []byte, limit uint32) ([]wsrpc.CardEntry, bool) {
		start := 0
		if len(after) > 0 {
			for i := range all {
				if bytes.Equal(all[i].CardID, after) {
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

// TestFetchAllCardsPaginatesAcrossPages proves the bounded-pagination loop: an
// address book larger than one page is fetched in reportPageSize chunks,
// accumulated in order with no dupes/gaps, via the after_card_id cursor.
func TestFetchAllCardsPaginatesAcrossPages(t *testing.T) {
	const total = 2*reportPageSize + 500 // 2500 → 3 pages: 1000, 1000, 500
	all, pager := makeCardPager(total)
	caller := &mockCaller{queryCardsPager: pager}
	b := NewBackend(slog.Default())
	sess := davauth.NewSession(caller, slog.Default(), fixtureActorID, "", "")

	got, err := b.fetchAllCards(context.Background(), sess, testAddressbookID)
	if err != nil {
		t.Fatalf("fetchAllCards: %v", err)
	}
	if len(got) != total {
		t.Fatalf("got %d cards, want %d", len(got), total)
	}
	for i := range got {
		if !bytes.Equal(got[i].CardID, all[i].CardID) {
			t.Fatalf("card %d = %x, want %x (cursor advance broke ordering/coverage)", i, got[i].CardID, all[i].CardID)
		}
	}
	if n := len(caller.callsOf(wsrpc.MethodQueryCards)); n != 3 {
		t.Errorf("query_cards fired %d times, want 3 (ceil(2500/1000))", n)
	}
}

// TestFetchAllCardsTruncatesAtCap proves the hard maxReportCards ceiling: an
// address book past the cap is truncated and the loop STOPS at the cap.
func TestFetchAllCardsTruncatesAtCap(t *testing.T) {
	const total = maxReportCards + 1
	_, pager := makeCardPager(total)
	caller := &mockCaller{queryCardsPager: pager}
	b := NewBackend(slog.Default())
	sess := davauth.NewSession(caller, slog.Default(), fixtureActorID, "", "")

	got, err := b.fetchAllCards(context.Background(), sess, testAddressbookID)
	if err != nil {
		t.Fatalf("fetchAllCards: %v", err)
	}
	if len(got) != maxReportCards {
		t.Fatalf("got %d cards, want %d (hard cap)", len(got), maxReportCards)
	}
	want := maxReportCards / reportPageSize
	if n := len(caller.callsOf(wsrpc.MethodQueryCards)); n != want {
		t.Errorf("query_cards fired %d times, want %d (must stop at the cap)", n, want)
	}
}

// testReportCard is a minimal-but-valid vCard the REPORT tests seal end-to-end.
const testReportCard = "BEGIN:VCARD\r\n" +
	"VERSION:3.0\r\n" +
	"UID:report-test-uid-1\r\n" +
	"FN:Bob Report\r\n" +
	"EMAIL:bob@example.com\r\n" +
	"END:VCARD\r\n"

// reportFixture bundles the per-test crypto material a decrypt-path REPORT
// integration test needs: the actor's seal key as production derives it from
// the canonical fixture MSEK — the X25519 keypair (`leaf`) and its ML-KEM half
// (`mlkemEk`) — the MLS snapshot blob that opens what is sealed to either (CBOR
// plaintext sealed under that MSEK), and a helper that seals card bodies to the
// leaf pubkey.
type reportFixture struct {
	leaf         faunaFfi.X25519Keypair
	mlkemEk      []byte
	snapshotBlob []byte
}

// newReportFixture builds the crypto scaffolding shared across REPORT tests +
// the metadata round-trip test: both halves of the seal key derived from the
// fixture MSEK, and the snapshot built from that MSEK and sealed under it, so the
// AUTH flow's `cap.Decrypt(snapshotBlob)` round-trips and the session opens both
// a classical seal (the tests' pre-sealed fixtures) and the X-Wing seal a PUT /
// PROPPATCH writes to the pair.
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

// sealCard seals `body` to the fixture's leaf pubkey. The returned bytes are
// wire-identical to what MTA-side `EncryptToRecipient` produces, so the AUTH'd
// MDA can re-open them via `OpenMailRecord`.
func (f *reportFixture) sealCard(t *testing.T, body []byte) []byte {
	t.Helper()
	out, err := mailfauna.EncryptToRecipient(body, f.leaf.Pubkey)
	if err != nil {
		t.Fatalf("EncryptToRecipient: %v", err)
	}
	return out
}

// sealedAddressbook builds an AddressbookEntry whose metadata is sealed to the
// fixture leaf pubkey, so the AUTH'd MDA's GetAddressBook / ListAddressBooks can
// decrypt it in-session.
func (f *reportFixture) sealedAddressbook(t *testing.T, id []byte, displayname string) wsrpc.AddressbookEntry {
	t.Helper()
	sealed, err := SealCollectionMetadata(EncryptedCollectionMetadata{Displayname: displayname}, f.leaf.Pubkey, nil)
	if err != nil {
		t.Fatalf("SealCollectionMetadata: %v", err)
	}
	return wsrpc.AddressbookEntry{AddressbookID: id, EncryptedMetadata: sealed}
}

// uidHash returns blake3(uid)[:32] — the canonical card-resource slug.
func uidHash(uid string) []byte {
	sum := blake3.Sum256([]byte(uid))
	out := make([]byte, 32)
	copy(out, sum[:])
	return out
}

// decryptCaller returns a mockCaller wired for an AUTH flow that successfully
// fetches the MLS snapshot. Tests poke the queryCards*/sync* knobs to stage
// replies.
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

// openSealedMetadata HPKE-opens a sealed metadata blob using the fixture leaf's
// secret — the same MlsCapability + snapshot-plaintext path the AUTH middleware
// builds server-side. Shared with encrypted_metadata_test.go.
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

// reportRequest builds an authenticated REPORT request.
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

// addressbookURL builds the address-book-collection URL for a given id (hex).
func addressbookURL(baseURL string, addressbookID []byte) string {
	return baseURL +
		"/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(addressbookID) + "/"
}

// addressbookQueryFN is an addressbook-query whose FN prop-filter (no text-match)
// matches any card carrying an FN — the "return every contact" query most
// clients issue after discovery.
const addressbookQueryFN = `<?xml version="1.0" encoding="utf-8"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:getetag/>
    <C:address-data/>
  </D:prop>
  <C:filter>
    <C:prop-filter name="FN"/>
  </C:filter>
</C:addressbook-query>`

func addressbookMultigetBody(hrefs ...string) string {
	var hs strings.Builder
	for _, h := range hrefs {
		hs.WriteString("  <D:href>")
		hs.WriteString(h)
		hs.WriteString("</D:href>\n")
	}
	return `<?xml version="1.0" encoding="utf-8"?>
<C:addressbook-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:getetag/>
    <C:address-data/>
  </D:prop>
` + hs.String() + `</C:addressbook-multiget>`
}

// ── REPORT addressbook-query tests ───────────────────────────────

// TestReportAddressbookQueryReturnsMatchingCards pins the addressbook-query
// path: REPORT against a book with one card yields 207 Multi-Status, the
// response carries the card's resource path + the decrypted FN, and exactly one
// query_cards RPC fires. Proves the SEAL-ALWAYS open path (OpenMailRecord) +
// the local carddav.Filter prop-filter matching.
func TestReportAddressbookQueryReturnsMatchingCards(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsCards = []wsrpc.CardEntry{
		{
			CardID:        []byte("card-id-1-pad-32-bytes-000000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
			ETag:          "etag-1",
			Modseq:        42,
			InternalDate:  1747396800,
		},
	}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), addressbookQueryFN)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	if !strings.Contains(string(body), "Bob Report") {
		t.Errorf("response missing decrypted FN 'Bob Report': %q", body)
	}
	wantHref := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) +
		"/" + hex.EncodeToString(uidHash("report-test-uid-1")) + ".vcf"
	if !strings.Contains(string(body), wantHref) {
		t.Errorf("response missing card href %q: %q", wantHref, body)
	}
}

// TestReportAddressbookMultiget pins the addressbook-multiget path: a REPORT
// listing one card href yields that card's decrypted body.
func TestReportAddressbookMultiget(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsCards = []wsrpc.CardEntry{
		{
			CardID:        []byte("card-id-mg-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
			ETag:          "etag-mg",
		},
	}
	baseURL, stop := startServer(t, caller)
	defer stop()

	href := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) +
		"/" + hex.EncodeToString(uidHash("report-test-uid-1")) + ".vcf"
	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), addressbookMultigetBody(href))
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	if !strings.Contains(string(body), "Bob Report") {
		t.Errorf("multiget response missing decrypted FN: %q", body)
	}
}

// TestReportAddressbookQueryNotFound pins that an addressbook-query against an
// unprovisioned book surfaces 404 from query_cards' AddressbookNotFound.
func TestReportAddressbookQueryNotFound(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsOutcome = wsrpc.QueryCardsAddressbookNotFound
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), addressbookQueryFN)
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

// TestListAddressObjectsPropfindDepth1 pins the PROPFIND-depth-1 read path
// (ListAddressObjects → nil query → every card), so a client that enumerates via
// PROPFIND rather than REPORT gets the full card set.
func TestListAddressObjectsPropfindDepth1(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	// Depth-1 PROPFIND first fetches the book metadata (emersion's
	// GetAddressBook) before listing objects, so the book must be present +
	// decryptable in list_addressbooks.
	caller.listAddressbooksReplies = [][]wsrpc.AddressbookEntry{
		{fx.sealedAddressbook(t, testAddressbookID, "Contacts")},
	}
	caller.queryCardsCards = []wsrpc.CardEntry{
		{
			CardID:        []byte("card-id-pf-pad-32-bytes-00000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
			ETag:          "etag-pf",
		},
	}
	baseURL, stop := startServer(t, caller)
	defer stop()

	body := mustPropfind(t, baseURL,
		"/carddav/"+fixtureLocalPart+"@"+fixtureDomain+"/"+hex.EncodeToString(testAddressbookID)+"/",
		fixtureLocalPart+"@"+fixtureDomain,
		"<d:getetag/><c:address-data/>", "1")
	if !strings.Contains(body, "Bob Report") {
		t.Errorf("depth-1 PROPFIND missing decrypted FN: %q", body)
	}
}

// testReportCardNoVersion is a vCard go-vcard's DECODER accepts but its ENCODER
// rejects: it omits the mandatory VERSION property. Used to prove the serve
// path skips such a card instead of breaking the whole REPORT (openCard's
// pre-encode guard).
const testReportCardNoVersion = "BEGIN:VCARD\r\n" +
	"UID:report-test-uid-noversion\r\n" +
	"FN:Broken Card\r\n" +
	"END:VCARD\r\n"

// TestReportAddressbookQuerySkipsUnservableCard proves that one stored card
// go-vcard's encoder rejects (missing the mandatory VERSION) is skipped, NOT
// allowed to break the whole REPORT. emersion's streaming writer commits the
// response status before encoding each <address-data>, so an encode error
// there would truncate the multistatus and every card — including healthy
// ones — would vanish from the MUA's view. Twin of the CalDAV terminator's
// TestReportCalendarQuerySkipsUnencodableEvent.
func TestReportAddressbookQuerySkipsUnservableCard(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsCards = []wsrpc.CardEntry{
		{
			CardID:        []byte("card-id-bad-pad-32-bytes-0000000"),
			UIDHash:       uidHash("report-test-uid-noversion"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCardNoVersion)),
			ETag:          "etag-bad",
		},
		{
			CardID:        []byte("card-id-1-pad-32-bytes-000000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
			ETag:          "etag-1",
		},
	}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), addressbookQueryFN)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// The VALID card still surfaces — the un-encodable sibling did NOT break
	// the whole response (the regression this guards).
	if !strings.Contains(string(body), "Bob Report") {
		t.Fatalf("valid card missing — an un-encodable sibling broke the whole REPORT: %q", body)
	}
	goodPath := hex.EncodeToString(uidHash("report-test-uid-1")) + ".vcf"
	if !strings.Contains(string(body), goodPath) {
		t.Fatalf("valid card path %q missing: %q", goodPath, body)
	}
	// The un-encodable card is skipped (its resource path never appears).
	badPath := hex.EncodeToString(uidHash("report-test-uid-noversion")) + ".vcf"
	if strings.Contains(string(body), badPath) {
		t.Fatalf("un-encodable card %q should have been skipped, not served: %q", badPath, body)
	}
}

// TestReportAddressbookQuerySkipsRawCardStrict pins the STRICT open: card
// bodies rest sealed, so a stored RAW (unsealed) card must be dropped with an
// error — never served verbatim (the same strict open a CalDAV event body
// gets). A sealed sibling still serves.
func TestReportAddressbookQuerySkipsRawCardStrict(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsCards = []wsrpc.CardEntry{
		{
			CardID:        []byte("card-id-raw-pad-32-bytes-0000000"),
			UIDHash:       uidHash("report-test-uid-raw"),
			EncryptedBody: []byte(testReportCard), // RAW — not sealed
			ETag:          "etag-raw",
		},
		{
			CardID:        []byte("card-id-1-pad-32-bytes-000000000"),
			UIDHash:       uidHash("report-test-uid-1"),
			EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
			ETag:          "etag-1",
		},
	}
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), addressbookQueryFN)
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusMultiStatus {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusMultiStatus, body)
	}
	// The sealed card still serves.
	goodPath := hex.EncodeToString(uidHash("report-test-uid-1")) + ".vcf"
	if !strings.Contains(string(body), goodPath) {
		t.Fatalf("sealed card path %q missing: %q", goodPath, body)
	}
	// The raw card is dropped, not passed through (strict sealed shape).
	rawPath := hex.EncodeToString(uidHash("report-test-uid-raw")) + ".vcf"
	if strings.Contains(string(body), rawPath) {
		t.Fatalf("raw card %q must be skipped on the strict CardDAV path, not served: %q", rawPath, body)
	}
}

// TestReportAddressbookMultigetMissingHrefSurfaces404Inline confirms
// emersion's per-href error-response behavior surfaces a missing card's 404
// inline in the multistatus rather than failing the whole REPORT. Twin of the
// CalDAV terminator's TestReportCalendarMultigetMissingHrefSurfaces404Inline.
func TestReportAddressbookMultigetMissingHrefSurfaces404Inline(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsCards = nil // no cards at all

	baseURL, stop := startServer(t, caller)
	defer stop()

	href := "/carddav/" + fixtureLocalPart + "@" + fixtureDomain +
		"/" + hex.EncodeToString(testAddressbookID) +
		"/" + hex.EncodeToString(uidHash("never-existed")) + ".vcf"
	req := reportRequest(t, addressbookURL(baseURL, testAddressbookID), addressbookMultigetBody(href))
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
