package caldav

import (
	"io"
	"log/slog"
	"net/http"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// schedulingCaller builds an AUTH-capable mockCaller for the scheduling
// advertisement tests — the same wrapped-blob fixture the discovery test
// uses, so a Basic-auth request resolves to a live Session.
func schedulingCaller(t *testing.T) *mockCaller {
	t.Helper()
	return &mockCaller{
		validateRecipientActor: fixtureActorID,
		wrappedBlob:            mustReadFixture(t, "wrapped_msek.bin"),
		mlsPubkey:              fixtureMLSPubkey,
		indexKey:               fixtureIndexKey,
	}
}

// startServerCardDAVEnabled is startServer with CardDAVEnabled: true — the
// both-protocols-on deployment where this CalDAV chain owns the mux catch-all
// `/` and so serves the UNIFIED root principal (both calendar-home-set and
// addressbook-home-set). Mirrors startServer / startServerPlaintext.
func startServerCardDAVEnabled(t *testing.T, caller wsrpc.Caller) (string, func()) {
	t.Helper()
	ln := newTLSListener(t, selfSignedCert(t))
	srv := NewServer(ServerConfig{
		Logger:         slog.Default(),
		MailEnabled:    true,
		CardDAVEnabled: true,
		ClassifyTransport: func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "email"}, nil
		},
	}, caller)
	go func() { _ = srv.Serve(ln) }()
	return "https://" + ln.Addr().String(), func() {
		_ = srv.Close()
		_ = ln.Close()
	}
}

// TestServerAdvertisesCalendarAutoScheduleInOptions pins the RFC 6638 §2
// requirement: the `DAV:` response header on an OPTIONS request advertises
// `calendar-auto-schedule`. Without this token a client that does its own
// client-side iMIP (e.g. Apple Calendar with a mail account) never defers to
// the server-side organizer fan-out gateway, and BOTH send the invite →
// duplicate invites (caldav-server.md § Server-side auto-schedule).
func TestServerAdvertisesCalendarAutoScheduleInOptions(t *testing.T) {
	url, stop := startServer(t, schedulingCaller(t))
	defer stop()
	user := fixtureLocalPart + "@" + fixtureDomain

	req, err := http.NewRequest(http.MethodOptions, url+"/caldav/"+user+"/", nil)
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.SetBasicAuth(user, string(fixturePlainPassword))
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)

	dav := strings.Join(resp.Header.Values("DAV"), ", ")
	if !strings.Contains(dav, "calendar-access") {
		t.Fatalf("DAV header lost calendar-access (emersion baseline regression): %q", dav)
	}
	if !strings.Contains(dav, "calendar-auto-schedule") {
		t.Fatalf("DAV header must advertise calendar-auto-schedule (RFC 6638 §2) so a client defers to server-side scheduling; got %q", dav)
	}
}

// TestServerPrincipalAdvertisesSchedulingProps pins the RFC 6638 §§2.1.1/2.2.1/
// 2.4.1 principal properties: a PROPFIND of the principal serves
// `calendar-user-address-set` (≥1 mailto:), `schedule-inbox-URL`, and
// `schedule-outbox-URL` — AND still serves `calendar-home-set` so the Gap-1c
// discovery walk keeps working when a real client bundles all of these into the
// one account-setup PROPFIND.
func TestServerPrincipalAdvertisesSchedulingProps(t *testing.T) {
	url, stop := startServer(t, schedulingCaller(t))
	defer stop()
	user := fixtureLocalPart + "@" + fixtureDomain

	// Discover the principal href (single-segment, per the Gap-1c fix) the
	// way a real client does, rather than hardcoding the path.
	rootBody := mustPropfind(t, url, "/", user, "<d:current-user-principal/>", "0")
	principal := hrefInProp(rootBody, "current-user-principal")
	if principal == "" {
		t.Fatalf("no current-user-principal href; body=%s", rootBody)
	}

	props := "<c:calendar-home-set/><c:calendar-user-address-set/>" +
		"<c:schedule-inbox-URL/><c:schedule-outbox-URL/>"
	body := mustPropfind(t, url, principal, user, props, "0")

	if home := hrefInProp(body, "calendar-home-set"); home == "" || !strings.Contains(home, "/caldav/") {
		t.Fatalf("principal must still serve calendar-home-set under /caldav/ (discovery contract) when bundled with scheduling props; got %q body=%s", home, body)
	}
	if addr := hrefInProp(body, "calendar-user-address-set"); !strings.Contains(addr, "mailto:"+user) {
		t.Fatalf("calendar-user-address-set must contain mailto:%s so the client matches the VEVENT ORGANIZER/ATTENDEE; got %q body=%s", user, addr, body)
	}
	if inbox := hrefInProp(body, "schedule-inbox-URL"); inbox == "" {
		t.Fatalf("principal must advertise schedule-inbox-URL; body=%s", body)
	}
	if outbox := hrefInProp(body, "schedule-outbox-URL"); outbox == "" {
		t.Fatalf("principal must advertise schedule-outbox-URL; body=%s", body)
	}
}

// TestServerScheduleInboxOutboxReportResourceType pins the RFC 6638 §2.1/§2.2
// MUST: the advertised schedule Inbox/Outbox are real collections that report
// the `DAV:collection` + `CALDAV:schedule-inbox`/`schedule-outbox` resourcetype
// on a PROPFIND. A half-advertised surface (token present, URLs 404) is the
// worst case — a client may suppress its own iMIP against a broken server. The
// collections are empty (no scheduling messages surfaced; reply-merge is
// client-side) and not browsable, but they MUST exist + be correctly typed.
func TestServerScheduleInboxOutboxReportResourceType(t *testing.T) {
	url, stop := startServer(t, schedulingCaller(t))
	defer stop()
	user := fixtureLocalPart + "@" + fixtureDomain

	rootBody := mustPropfind(t, url, "/", user, "<d:current-user-principal/>", "0")
	principal := hrefInProp(rootBody, "current-user-principal")
	princBody := mustPropfind(t, url, principal, user,
		"<c:schedule-inbox-URL/><c:schedule-outbox-URL/>", "0")
	inbox := hrefInProp(princBody, "schedule-inbox-URL")
	outbox := hrefInProp(princBody, "schedule-outbox-URL")
	if inbox == "" || outbox == "" {
		t.Fatalf("principal did not advertise inbox/outbox URLs; body=%s", princBody)
	}

	inBody := mustPropfind(t, url, inbox, user, "<d:resourcetype/>", "0")
	if !strings.Contains(inBody, "schedule-inbox") {
		t.Fatalf("inbox %q resourcetype must include CALDAV:schedule-inbox (RFC 6638 §2.2); body=%s", inbox, inBody)
	}
	outBody := mustPropfind(t, url, outbox, user, "<d:resourcetype/>", "0")
	if !strings.Contains(outBody, "schedule-outbox") {
		t.Fatalf("outbox %q resourcetype must include CALDAV:schedule-outbox (RFC 6638 §2.1); body=%s", outbox, outBody)
	}
}

// TestServerPrincipalAdvertisesAddressbookHomeSetWhenCardDAVEnabled pins the 2d
// unified-principal follow-up: on a both-protocols-on deployment (CardDAVEnabled
// = true) this CalDAV chain owns the mux catch-all `/`, so the single-segment
// principal `/{user}/` — the SAME path both backends return from
// CurrentUserPrincipal — must advertise BOTH `calendar-home-set` (→ /caldav/…)
// AND `addressbook-home-set` (→ /carddav/…). Without the addressbook arm a
// CardDAV client (Apple Contacts / DAVx5) that only knows the server host walks
// current-user-principal → addressbook-home-set and finds nothing (the CalDAV
// catch-all shadows the CardDAV chain's own principal), so host-only
// autodiscovery of the address book breaks. A client pointed straight at
// /carddav/{user}/ round-trips regardless — this is purely the autodiscovery
// completion (carddav-server design § 8).
func TestServerPrincipalAdvertisesAddressbookHomeSetWhenCardDAVEnabled(t *testing.T) {
	url, stop := startServerCardDAVEnabled(t, schedulingCaller(t))
	defer stop()
	user := fixtureLocalPart + "@" + fixtureDomain

	// Discover the principal href the way a real client does (single-segment,
	// per the Gap-1c fix) rather than hardcoding it.
	rootBody := mustPropfind(t, url, "/", user, "<d:current-user-principal/>", "0")
	principal := hrefInProp(rootBody, "current-user-principal")
	if principal == "" {
		t.Fatalf("no current-user-principal href; body=%s", rootBody)
	}

	// A CardDAV client bundles addressbook-home-set into the account-setup
	// PROPFIND; a well-behaved client that also does CalDAV bundles both.
	body := mustPropfind(t, url, principal, user,
		"<c:calendar-home-set/><card:addressbook-home-set/>", "0")

	if home := hrefInProp(body, "calendar-home-set"); home == "" || !strings.Contains(home, "/caldav/") {
		t.Fatalf("unified principal must STILL serve calendar-home-set under /caldav/ (no CalDAV regression); got %q body=%s", home, body)
	}
	abHome := hrefInProp(body, "addressbook-home-set")
	if abHome == "" || !strings.Contains(abHome, "/carddav/") {
		t.Fatalf("unified principal must serve addressbook-home-set under /carddav/ for host-only CardDAV autodiscovery; got %q body=%s", abHome, body)
	}
	if want := "/carddav/" + user + "/"; abHome != want {
		t.Fatalf("addressbook-home-set = %q, want the CardDAV home set %q (twin of CalDAV's /caldav/{user}/)", abHome, want)
	}
}

// TestServerPrincipalOmitsAddressbookHomeSetWhenCardDAVDisabled pins the
// converse: a CalDAV-only deployment (CardDAVEnabled = false, the default) must
// NOT advertise addressbook-home-set on the principal — there is no CardDAV
// surface mounted, so pointing a client at a /carddav/ home set would just 404.
// This keeps the CalDAV-only principal response unchanged (the byte-identical
// goal of the 2d shared-listener refactor) rather than dangling a dead prop.
func TestServerPrincipalOmitsAddressbookHomeSetWhenCardDAVDisabled(t *testing.T) {
	url, stop := startServer(t, schedulingCaller(t))
	defer stop()
	user := fixtureLocalPart + "@" + fixtureDomain

	rootBody := mustPropfind(t, url, "/", user, "<d:current-user-principal/>", "0")
	principal := hrefInProp(rootBody, "current-user-principal")
	if principal == "" {
		t.Fatalf("no current-user-principal href; body=%s", rootBody)
	}

	body := mustPropfind(t, url, principal, user,
		"<c:calendar-home-set/><card:addressbook-home-set/>", "0")

	// calendar-home-set is still served (sanity: the principal PROPFIND worked).
	if home := hrefInProp(body, "calendar-home-set"); home == "" || !strings.Contains(home, "/caldav/") {
		t.Fatalf("CalDAV-only principal must serve calendar-home-set; got %q body=%s", home, body)
	}
	// addressbook-home-set must NOT resolve to a /carddav/ href (it belongs in
	// the 404 propstat when CardDAV is off).
	if abHome := hrefInProp(body, "addressbook-home-set"); abHome != "" {
		t.Fatalf("CalDAV-only principal must NOT advertise addressbook-home-set (no CardDAV surface mounted); got href %q body=%s", abHome, body)
	}
}
