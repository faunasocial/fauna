package caldav

import (
	"bytes"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// mkcalendarRequest builds an authenticated MKCALENDAR against `url` carrying
// `body` as its RFC 4791 §5.3.1 request body (empty body allowed).
func mkcalendarRequest(t *testing.T, url, body string) *http.Request {
	t.Helper()
	req, err := http.NewRequest("MKCALENDAR", url, strings.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(fixtureLocalPart+"@"+fixtureDomain, string(fixturePlainPassword))
	if body != "" {
		req.Header.Set("Content-Type", "application/xml; charset=utf-8")
	}
	return req
}

// homeSetURL is the AUTH'd actor's calendar-home base; a MKCALENDAR target is
// this prefix + the client-chosen collection slug + "/".
func homeSetURL(baseURL string) string {
	return baseURL + "/caldav/" + fixtureLocalPart + "@" + fixtureDomain + "/"
}

// TestMkcalendarCreatesCalendarAtClientChosenURL pins the macOS Calendar.app
// "add calendar" flow: a MKCALENDAR at a client-chosen, NON-hex (UUID-style)
// collection URL provisions a calendar via provision_calendar(update_metadata=
// false) and returns 201 Created. The calendar_id nest receives is the
// deterministic blake3(segment)[:32] of the URL slug, and the sealed metadata
// carries the client's displayname + color. Regression for the "This is not a
// location that supports this request" 405 (emersion routes only MKCOL).
func TestMkcalendarCreatesCalendarAtClientChosenURL(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.provisionOutcome = wsrpc.ProvisionCalendarCreated

	baseURL, stop := startServer(t, caller)
	defer stop()

	// The opaque slug a stock client picks — not 64-hex, so it routes through
	// the blake3 branch of resolveCalendarSegment.
	const slug = "1A2B3C4D-5E6F-7081-9293-A4B5C6D7E8F9"
	url := homeSetURL(baseURL) + slug + "/"

	body := `<?xml version="1.0" encoding="utf-8"?>
<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:A="http://apple.com/ns/ical/">
  <D:set>
    <D:prop>
      <D:displayname>Vacances</D:displayname>
      <A:calendar-color>#FF2D55</A:calendar-color>
    </D:prop>
  </D:set>
</C:mkcalendar>`

	resp, err := httpsClient().Do(mkcalendarRequest(t, url, body))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusCreated, respBody)
	}

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
	if pReq.UpdateMetadata {
		t.Errorf("update_metadata = true, want false on MKCALENDAR insert path")
	}
	wantID, _ := resolveCalendarSegment(slug)
	if !bytes.Equal(pReq.CalendarID, wantID) {
		t.Errorf("calendar_id = %x, want blake3(slug) = %x", pReq.CalendarID, wantID)
	}

	// The encrypted_metadata nest received must unseal to the client's
	// displayname + color when opened with the fixture leaf — pins that the
	// initial properties round-tripped through the seal layer.
	openedBytes, err := openSealedMetadata(t, pReq.EncryptedMetadata, fx)
	if err != nil {
		t.Fatalf("re-open new-calendar metadata: %v", err)
	}
	var meta EncryptedCollectionMetadata
	if err := cbor.Unmarshal(openedBytes, &meta); err != nil {
		t.Fatalf("decode new-calendar metadata: %v", err)
	}
	if meta.Displayname != "Vacances" {
		t.Errorf("displayname = %q, want %q", meta.Displayname, "Vacances")
	}
	if meta.Color != "#FF2D55" {
		t.Errorf("color = %q, want %q", meta.Color, "#FF2D55")
	}
}

// TestMkcalendarBareBodyUsesDefaults pins that a property-less MKCALENDAR (a
// valid bare create per RFC 4791 §5.3.1) still lands a calendar with the
// default displayname + color — works-out-of-the-box.
func TestMkcalendarBareBodyUsesDefaults(t *testing.T) {
	fx := newReportFixture(t)
	caller := decryptCaller(t, fx)
	caller.provisionOutcome = wsrpc.ProvisionCalendarCreated

	baseURL, stop := startServer(t, caller)
	defer stop()

	url := homeSetURL(baseURL) + "bare-create-slug/"
	resp, err := httpsClient().Do(mkcalendarRequest(t, url, ""))
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d (body=%q)", resp.StatusCode, http.StatusCreated, respBody)
	}

	provs := caller.callsOf(wsrpc.MethodProvisionCalendar)
	if len(provs) != 1 {
		t.Fatalf("provision_calendar fired %d times, want 1", len(provs))
	}
	var pReq struct {
		EncryptedMetadata []byte `cbor:"encrypted_metadata"`
	}
	if err := cbor.Unmarshal(provs[0].body, &pReq); err != nil {
		t.Fatalf("decode provision_calendar body: %v", err)
	}
	openedBytes, err := openSealedMetadata(t, pReq.EncryptedMetadata, fx)
	if err != nil {
		t.Fatalf("re-open metadata: %v", err)
	}
	var meta EncryptedCollectionMetadata
	if err := cbor.Unmarshal(openedBytes, &meta); err != nil {
		t.Fatalf("decode metadata: %v", err)
	}
	if meta.Displayname != defaultDisplayname || meta.Color != defaultColor {
		t.Errorf("bare MKCALENDAR metadata = {%q,%q}, want defaults {%q,%q}",
			meta.Displayname, meta.Color, defaultDisplayname, defaultColor)
	}
}

// TestMkcalendarExistingReturns405 pins RFC 4791 §5.3.1 / RFC 4918 §9.3:
// MKCALENDAR on an already-provisioned collection → 405 Method Not Allowed,
// whatever metadata is stored. BOTH nest outcomes for an existing row map to
// 405: AlreadyExists (byte-identical) and Conflict (different bytes). The
// Conflict branch is in fact the ONLY one a real client reaches —
// SealCollectionMetadata HPKE-re-seals non-deterministically, so a re-create's
// ciphertext never byte-matches the stored blob and the nest always answers
// Conflict — which the in-process twin can't show (it mocks the outcome); the
// tier_3 round-trip is what surfaced the original 409-instead-of-405 violation.
func TestMkcalendarExistingReturns405(t *testing.T) {
	for _, outcome := range []wsrpc.ProvisionCalendarOutcome{
		wsrpc.ProvisionCalendarAlreadyExists,
		wsrpc.ProvisionCalendarConflict,
	} {
		t.Run(string(outcome), func(t *testing.T) {
			fx := newReportFixture(t)
			caller := decryptCaller(t, fx)
			caller.provisionOutcome = outcome

			baseURL, stop := startServer(t, caller)
			defer stop()

			url := homeSetURL(baseURL) + "already-there-slug/"
			resp, err := httpsClient().Do(mkcalendarRequest(t, url, ""))
			if err != nil {
				t.Fatalf("Do: %v", err)
			}
			defer resp.Body.Close()
			if resp.StatusCode != http.StatusMethodNotAllowed {
				t.Fatalf("status = %d, want %d (existing calendar, outcome=%v)", resp.StatusCode, http.StatusMethodNotAllowed, outcome)
			}
		})
	}
}
