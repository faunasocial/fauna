package carddav

import (
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestPutIfNoneMatchStarRefusesAnExistingCard pins carddav-server.md §
// Address-book collection model: conditional card PUTs honour
// `If-None-Match` — `*` onto a card that already exists is refused 412 and
// nothing is written; onto a new card it creates.
func TestPutIfNoneMatchStarRefusesAnExistingCard(t *testing.T) {
	for _, tc := range []struct {
		name     string
		existing []wsrpc.CardEntry
		want     int
		puts     int
	}{
		{"existing card", []wsrpc.CardEntry{{UIDHash: uidHash("put-test-uid-001"), ETag: "e1"}}, http.StatusPreconditionFailed, 0},
		{"new card", nil, http.StatusCreated, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			caller := putAuthedCaller(t)
			caller.putCardOutcome = wsrpc.PutCardCreated
			caller.putCardETag = "etag-new"
			caller.queryCardsCards = tc.existing
			baseURL, stop := startServer(t, caller)
			defer stop()

			req := putRequest(t, cardURL(baseURL, testAddressbookID, "any"), testPutValidCard, "")
			req.Header.Set("If-None-Match", "*")
			resp, err := httpsClient().Do(req)
			if err != nil {
				t.Fatalf("Do: %v", err)
			}
			io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			if resp.StatusCode != tc.want {
				t.Fatalf("status = %d, want %d", resp.StatusCode, tc.want)
			}
			if n := len(caller.callsOf(wsrpc.MethodPutCardCiphertext)); n != tc.puts {
				t.Errorf("put_card_ciphertext fired %d times, want %d", n, tc.puts)
			}
		})
	}
}

// TestAddressbookQueryTextMatchFollowsTheCollation pins the addressbook-query
// matcher (query_filter.go) to RFC 6352 §10.5: the default collation
// (`i;unicode-casemap`) is case-insensitive, `i;octet` is exact, match-type
// and negate-condition apply, and `allof` needs every prop-filter.
func TestAddressbookQueryTextMatchFollowsTheCollation(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.queryCardsCards = []wsrpc.CardEntry{{
		CardID:        []byte("card-id-1-pad-32-bytes-000000000"),
		UIDHash:       uidHash("report-test-uid-1"),
		EncryptedBody: fx.sealCard(t, []byte(testReportCard)),
		ETag:          "etag-1",
		Modseq:        42,
		InternalDate:  1747396800,
	}}
	baseURL, stop := startServer(t, caller)
	defer stop()

	tm := func(attrs, text string) string {
		return `<C:prop-filter name="FN"><C:text-match ` + attrs + `>` + text + `</C:text-match></C:prop-filter>`
	}
	for _, tc := range []struct {
		name   string
		filter string
		found  bool
	}{
		{"default collation ignores case", tm(``, "report"), true},
		{"unicode-casemap ignores case", tm(`collation="i;unicode-casemap" match-type="contains"`, "BOB"), true},
		{"octet is exact", tm(`collation="i;octet"`, "report"), false},
		{"starts-with", tm(`match-type="starts-with"`, "bob"), true},
		{"equals needs the whole value", tm(`match-type="equals"`, "bob"), false},
		{"negate-condition", tm(`negate-condition="yes"`, "report"), false},
		{"no match", tm(``, "turing"), false},
		{"anyof", tm(``, "turing") + tm(``, "bob"), true},
		{"allof", `<C:prop-filter name="FN" test="allof"><C:text-match>bob</C:text-match><C:text-match>turing</C:text-match></C:prop-filter>`, false},
		{"is-not-defined", `<C:prop-filter name="NICKNAME"><C:is-not-defined/></C:prop-filter>`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			body := `<?xml version="1.0" encoding="utf-8"?><C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav"><D:prop><D:getetag/><C:address-data/></D:prop><C:filter>` + tc.filter + `</C:filter></C:addressbook-query>`
			resp, err := httpsClient().Do(reportRequest(t, addressbookURL(baseURL, testAddressbookID), body))
			if err != nil {
				t.Fatal(err)
			}
			b, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			if resp.StatusCode != http.StatusMultiStatus {
				t.Fatalf("status = %d (%s)", resp.StatusCode, b)
			}
			if got := strings.Contains(string(b), "Bob Report"); got != tc.found {
				t.Errorf("found = %v, want %v:\n%s", got, tc.found, b)
			}
		})
	}
}
