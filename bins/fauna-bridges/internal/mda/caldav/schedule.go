package caldav

// RFC 6638 server-side auto-schedule ADVERTISEMENT lives here. The MDA already
// does the organizer fan-out (autoschedule.go); this file makes a stock client
// (e.g. macOS Calendar.app with a mail account) RECOGNIZE that and stand down
// from its own client-side iMIP — without the advertisement the client also
// sends the invite and the attendee gets DUPLICATE invites (caldav-server.md
// § Server-side auto-schedule).
//
// emersion/go-webdav v0.7.0 hardcodes both the OPTIONS DAV-header caps
// (`caldav/server.go:318` → only `calendar-access`) and the principal
// PROPFIND prop set (`propFindUserPrincipal` → only current-user-principal,
// calendar-home-set, resourcetype); neither is reachable through the Backend
// interface. So, mirroring the PROPPATCH (props.go) and sync-collection
// (sync_collection.go) interceptors, this middleware:
//
//  1. OPTIONS — wraps the ResponseWriter to APPEND `calendar-auto-schedule`
//     to emersion's `1, 3, calendar-access` DAV header (RFC 6638 §2: advertise
//     on every scheduling-capable resource).
//  2. PROPFIND of the principal — serves the RFC 6638 §§2.1.1/2.2.1/2.4.1
//     scheduling properties (calendar-user-address-set, schedule-inbox-URL,
//     schedule-outbox-URL) alongside the discovery props emersion already
//     served (current-user-principal, calendar-home-set, resourcetype), so the
//     Gap-1c discovery walk keeps working when a real client bundles them.
//  3. PROPFIND of the schedule Inbox/Outbox — RFC 6638 §2.1/§2.2 MUST: each is
//     a real collection reporting `DAV:collection` + `CALDAV:schedule-inbox` /
//     `schedule-outbox` in its resourcetype. They are EMPTY and not browsable
//     (incoming invites are sealed-to-recipient onto the calendar directly and
//     replies merge client-side — caldav-server.md § Server-side auto-schedule
//     + § Out of scope), but they MUST exist + be correctly typed: a
//     half-advertised surface (token present, URLs 404) is the worst case, since
//     a client may suppress its own iMIP against a broken server.

import (
	"bytes"
	"encoding/xml"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
)

// capCalendarAutoSchedule is the RFC 6638 §2 DAV-header capability token.
const capCalendarAutoSchedule = "calendar-auto-schedule"

// nsCalDAV is the CalDAV XML namespace (RFC 4791); the scheduling properties
// and the schedule-inbox/schedule-outbox resourcetype elements live in it.
const nsCalDAV = "urn:ietf:params:xml:ns:caldav"

// nsCardDAV is the CardDAV XML namespace (RFC 6352). The unified root principal
// advertises `addressbook-home-set` in it when the deployment also serves
// CardDAV — see principalServedProps + newSchedulingInterceptor.
const nsCardDAV = "urn:ietf:params:xml:ns:carddav"

// principalPath returns the AUTH'd actor's principal URL — the single-segment
// `/{local}@{domain}/` path (the Gap-1c shape, see backend.go
// CurrentUserPrincipal which delegates here so there is ONE source of truth).
func principalPath(sess *davauth.Session) string {
	return "/" + sess.AuthedLocalPart() + "@" + sess.AuthedDomain() + "/"
}

// scheduleInboxPath / scheduleOutboxPath are the RFC 6638 §2.1/§2.2 collections,
// served as siblings of the calendars under the home set.
func scheduleInboxPath(sess *davauth.Session) string  { return userBasePath(sess) + "inbox/" }
func scheduleOutboxPath(sess *davauth.Session) string { return userBasePath(sess) + "outbox/" }

// calendarUserAddress is the AUTH'd actor's primary calendar user address — the
// `mailto:` the client matches against the VEVENT ORGANIZER/ATTENDEE to decide
// "this is me, the organizer" (RFC 6638 §3.1). In production a Fauna handle
// domain == the mail domain, so this is both the iCalendar CAL-ADDRESS and a
// deliverable email address (caldav-server.md § Scheduling & invitations).
func calendarUserAddress(sess *davauth.Session) string {
	return "mailto:" + sess.AuthedLocalPart() + "@" + sess.AuthedDomain()
}

// carddavHomeSetPath returns the AUTH'd actor's CardDAV address-book home set,
// `/carddav/{local}@{domain}/`. It MIRRORS the CardDAV terminator's
// carddav.userBasePath: caldav importing carddav would be a layering inversion,
// and the mux in mda.go likewise hardcodes the `/carddav/` mount prefix. The two
// are kept in lock-step by that shared mux + emersion's depth-routing (a
// divergence would point the client at the wrong tree and 404); if
// carddav.userBasePath ever moves, this mirror moves with it.
func carddavHomeSetPath(sess *davauth.Session) string {
	return "/carddav/" + sess.AuthedLocalPart() + "@" + sess.AuthedDomain() + "/"
}

// newSchedulingInterceptor wraps `next` (emersion's caldav.Handler) with the
// RFC 6638 advertisement surface. Non-scheduling requests pass through
// unchanged.
//
// `carddavEnabled` makes the shared root principal a UNIFIED principal: when
// the deployment also serves CardDAV, this CalDAV chain owns the mux catch-all
// `/` (davMounts in mda.go), so the single-segment principal `/{user}/` — which
// both protocols address (backend.CurrentUserPrincipal is identical on each) —
// lands here even for a CardDAV client. Without the flag the principal would
// advertise only `calendar-home-set`, and a CardDAV client that only knows the
// server host could not discover its `addressbook-home-set` (the 2d
// unified-principal follow-up). See principalServedProps.
func newSchedulingInterceptor(next http.Handler, logger *slog.Logger, carddavEnabled bool) http.Handler {
	if next == nil {
		panic("caldav: newSchedulingInterceptor: next must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &schedulingInterceptor{next: next, logger: logger, carddavEnabled: carddavEnabled}
}

type schedulingInterceptor struct {
	next   http.Handler
	logger *slog.Logger
	// carddavEnabled advertises `addressbook-home-set` on the principal (the
	// unified-principal path — see newSchedulingInterceptor). Set from
	// ServerConfig.CardDAVEnabled, which mda.go seeds from the live carddav_enabled
	// gate; a runtime toggle triggers a full 443-rebind (reconstructing this
	// interceptor), so a construction-time bool is always fresh — no atomic needed.
	carddavEnabled bool
}

func (m *schedulingInterceptor) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	// OPTIONS: append the auto-schedule token to emersion's DAV header for
	// every resource (RFC 6638 §2). The wrapper is a no-op when emersion sets
	// no DAV header (e.g. an error response), so it never advertises on a 4xx.
	if r.Method == http.MethodOptions {
		m.next.ServeHTTP(&davAutoScheduleWriter{ResponseWriter: w}, r)
		return
	}
	if r.Method != "PROPFIND" {
		m.next.ServeHTTP(w, r)
		return
	}
	sess := davauth.SessionFromContext(r.Context())
	if sess == nil {
		// Auth middleware short-circuits 401 before here; defensive passthrough.
		m.next.ServeHTTP(w, r)
		return
	}

	path := strings.TrimSuffix(r.URL.Path, "/")
	switch path {
	case strings.TrimSuffix(principalPath(sess), "/"):
		m.handlePrincipalPropFind(w, r, sess)
	case strings.TrimSuffix(scheduleInboxPath(sess), "/"):
		m.handleScheduleCollectionPropFind(w, r, sess, scheduleInboxPath(sess), "schedule-inbox", "Schedule Inbox")
	case strings.TrimSuffix(scheduleOutboxPath(sess), "/"):
		m.handleScheduleCollectionPropFind(w, r, sess, scheduleOutboxPath(sess), "schedule-outbox", "Schedule Outbox")
	default:
		m.next.ServeHTTP(w, r)
	}
}

// davAutoScheduleWriter appends `calendar-auto-schedule` to the DAV response
// header on the first WriteHeader. emersion sets one DAV header value via
// `Header().Add("DAV", "1, 3, calendar-access")` (internal/server.go
// handleOptions), so we read it, append our token, and re-Set.
type davAutoScheduleWriter struct {
	http.ResponseWriter
	done bool
}

func (w *davAutoScheduleWriter) WriteHeader(code int) {
	if !w.done {
		w.done = true
		if vals := w.Header().Values("DAV"); len(vals) > 0 {
			joined := strings.Join(vals, ", ")
			if !strings.Contains(joined, capCalendarAutoSchedule) {
				w.Header().Set("DAV", joined+", "+capCalendarAutoSchedule)
			}
		}
	}
	w.ResponseWriter.WriteHeader(code)
}

func (w *davAutoScheduleWriter) Write(b []byte) (int, error) {
	if !w.done {
		w.WriteHeader(http.StatusOK)
	}
	return w.ResponseWriter.Write(b)
}

// handlePrincipalPropFind serves the principal PROPFIND. An explicit `<D:prop>`
// request is answered here (discovery + scheduling props); an allprop/propname
// request is delegated to emersion (Apple uses explicit prop lists for
// scheduling discovery, so preserving emersion's allprop set is the safe
// default).
func (m *schedulingInterceptor) handlePrincipalPropFind(w http.ResponseWriter, r *http.Request, sess *davauth.Session) {
	body, err := io.ReadAll(r.Body) // w1Mitigations already buffered + restored it.
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: read PROPFIND body: %v", err), http.StatusBadRequest)
		return
	}
	names, explicit := parsePropFindNames(body)
	if !explicit {
		r.Body = io.NopCloser(bytes.NewReader(body))
		m.next.ServeHTTP(w, r)
		return
	}
	m.writePropFindMultistatus(w, principalPath(sess), names, principalServedProps(sess, m.carddavEnabled))
}

// handleScheduleCollectionPropFind serves a minimal, empty schedule Inbox or
// Outbox collection. emersion would route these depth-3 paths to GetCalendar
// (→ 400, "inbox"/"outbox" is not a 32-byte hex calendar id), so they MUST be
// intercepted here. An allprop request gets every served prop.
func (m *schedulingInterceptor) handleScheduleCollectionPropFind(w http.ResponseWriter, r *http.Request, sess *davauth.Session, href, kind, displayname string) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: read PROPFIND body: %v", err), http.StatusBadRequest)
		return
	}
	served := scheduleCollectionServedProps(sess, kind, displayname)
	names, explicit := parsePropFindNames(body)
	if !explicit {
		names = names[:0]
		for n := range served {
			names = append(names, n)
		}
	}
	m.writePropFindMultistatus(w, href, names, served)
}

// principalServedProps maps each PROPFIND property this interceptor serves on
// the principal to a renderer. Requested-but-absent props fall into the 404
// propstat (standard PROPFIND semantics).
func principalServedProps(sess *davauth.Session, carddavEnabled bool) map[xml.Name]func(*strings.Builder) {
	principal := principalPath(sess)
	user := sess.AuthedLocalPart() + "@" + sess.AuthedDomain()
	props := map[xml.Name]func(*strings.Builder){
		{Space: "DAV:", Local: "current-user-principal"}: func(sb *strings.Builder) {
			writePropHref(sb, "current-user-principal", "DAV:", principal)
		},
		{Space: "DAV:", Local: "principal-URL"}: func(sb *strings.Builder) {
			writePropHref(sb, "principal-URL", "DAV:", principal)
		},
		{Space: nsCalDAV, Local: "calendar-home-set"}: func(sb *strings.Builder) {
			writePropHref(sb, "calendar-home-set", nsCalDAV, userBasePath(sess))
		},
		{Space: nsCalDAV, Local: "schedule-inbox-URL"}: func(sb *strings.Builder) {
			writePropHref(sb, "schedule-inbox-URL", nsCalDAV, scheduleInboxPath(sess))
		},
		{Space: nsCalDAV, Local: "schedule-outbox-URL"}: func(sb *strings.Builder) {
			writePropHref(sb, "schedule-outbox-URL", nsCalDAV, scheduleOutboxPath(sess))
		},
		{Space: nsCalDAV, Local: "calendar-user-address-set"}: func(sb *strings.Builder) {
			// mailto first (the entry that matches a stock ORGANIZER/ATTENDEE),
			// then the principal href (RFC 6638 §2.4.1 permits the principal URI
			// as an additional address).
			writePropHrefs(sb, "calendar-user-address-set", nsCalDAV, []string{calendarUserAddress(sess), principal})
		},
		{Space: "DAV:", Local: "resourcetype"}: func(sb *strings.Builder) {
			sb.WriteString("        <D:resourcetype><D:collection/><D:principal/></D:resourcetype>\n")
		},
		{Space: "DAV:", Local: "displayname"}: func(sb *strings.Builder) {
			writePropText(sb, "displayname", "DAV:", user)
		},
	}
	if carddavEnabled {
		// UNIFIED PRINCIPAL: on a both-protocols-on deployment this CalDAV chain
		// owns the mux catch-all `/`, so a CardDAV client's principal PROPFIND of
		// the shared single-segment `/{user}/` lands here. Advertise
		// addressbook-home-set so the current-user-principal → addressbook-home-set
		// host-only autodiscovery walk resolves (newSchedulingInterceptor). A
		// contacts-only deployment mounts CardDAV at `/` and needs no arm —
		// emersion's own carddav principal serves addressbook-home-set directly.
		props[xml.Name{Space: nsCardDAV, Local: "addressbook-home-set"}] = func(sb *strings.Builder) {
			writePropHref(sb, "addressbook-home-set", nsCardDAV, carddavHomeSetPath(sess))
		}
	}
	return props
}

// scheduleCollectionServedProps maps the props served on a schedule Inbox /
// Outbox collection. `kind` is "schedule-inbox" or "schedule-outbox".
func scheduleCollectionServedProps(sess *davauth.Session, kind, displayname string) map[xml.Name]func(*strings.Builder) {
	principal := principalPath(sess)
	return map[xml.Name]func(*strings.Builder){
		{Space: "DAV:", Local: "resourcetype"}: func(sb *strings.Builder) {
			sb.WriteString("        <D:resourcetype><D:collection/><C:" + kind + "/></D:resourcetype>\n")
		},
		{Space: "DAV:", Local: "displayname"}: func(sb *strings.Builder) {
			writePropText(sb, "displayname", "DAV:", displayname)
		},
		{Space: "DAV:", Local: "current-user-principal"}: func(sb *strings.Builder) {
			writePropHref(sb, "current-user-principal", "DAV:", principal)
		},
		{Space: "DAV:", Local: "owner"}: func(sb *strings.Builder) {
			writePropHref(sb, "owner", "DAV:", principal)
		},
	}
}

// writePropFindMultistatus emits a single-response 207 multistatus, partitioning
// the requested props into a 200 propstat (served) and a 404 propstat (absent),
// per RFC 4918 §9.1. Reuses props.go's writePropEmpty + strBuilderWriter.
func (m *schedulingInterceptor) writePropFindMultistatus(
	w http.ResponseWriter,
	href string,
	requested []xml.Name,
	served map[xml.Name]func(*strings.Builder),
) {
	var found, missing []xml.Name
	for _, n := range requested {
		if _, ok := served[n]; ok {
			found = append(found, n)
		} else {
			missing = append(missing, n)
		}
	}

	var sb strings.Builder
	sb.WriteString(`<?xml version="1.0" encoding="utf-8"?>` + "\n")
	// Declare xmlns:A (CardDAV) ONLY when a served prop actually uses it (the
	// unified principal's addressbook-home-set). A CalDAV-only response then stays
	// byte-identical to before this arm (the 2d byte-identical goal); a requested-
	// but-unserved carddav prop renders in the 404 propstat with its own default
	// xmlns and needs no root declaration.
	nsDecls := `xmlns:D="DAV:" xmlns:C="` + nsCalDAV + `"`
	for _, n := range found {
		if n.Space == nsCardDAV {
			nsDecls += ` xmlns:A="` + nsCardDAV + `"`
			break
		}
	}
	sb.WriteString(`<D:multistatus ` + nsDecls + `>` + "\n")
	sb.WriteString("  <D:response>\n")
	sb.WriteString("    <D:href>")
	_ = xml.EscapeText(&strBuilderWriter{&sb}, []byte(href))
	sb.WriteString("</D:href>\n")
	if len(found) > 0 {
		sb.WriteString("    <D:propstat>\n      <D:prop>\n")
		for _, n := range found {
			served[n](&sb)
		}
		sb.WriteString("      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n    </D:propstat>\n")
	}
	if len(missing) > 0 {
		sb.WriteString("    <D:propstat>\n      <D:prop>\n")
		for _, n := range missing {
			writePropEmpty(&sb, n)
		}
		sb.WriteString("      </D:prop>\n      <D:status>HTTP/1.1 404 Not Found</D:status>\n    </D:propstat>\n")
	}
	sb.WriteString("  </D:response>\n</D:multistatus>\n")

	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.WriteHeader(http.StatusMultiStatus)
	_, _ = io.WriteString(w, sb.String())
}

// parsePropFindNames walks a PROPFIND body and returns the requested property
// names. `explicit` is false for an allprop/propname/empty body (no `<D:prop>`),
// signalling the caller to delegate to emersion where applicable.
func parsePropFindNames(body []byte) (names []xml.Name, explicit bool) {
	propElemName := xml.Name{Space: "DAV:", Local: "prop"}
	dec := xml.NewDecoder(bytes.NewReader(body))
	inProp := false
	for {
		tok, err := dec.Token()
		if err != nil {
			break
		}
		switch t := tok.(type) {
		case xml.StartElement:
			if t.Name == propElemName {
				inProp = true
				explicit = true
				continue
			}
			if inProp {
				names = append(names, t.Name)
				_ = dec.Skip() // consume any children + the end tag
			}
		case xml.EndElement:
			if t.Name == propElemName {
				inProp = false
			}
		}
	}
	return names, explicit
}

// nsPrefix maps a namespace URI to the prefix declared on the multistatus root
// (D for DAV:, C for CalDAV, A for CardDAV — the last only on the unified
// principal when CardDAV is enabled). Every served prop lives in one of these.
func nsPrefix(ns string) string {
	switch ns {
	case nsCalDAV:
		return "C"
	case nsCardDAV:
		return "A"
	default:
		return "D"
	}
}

// writePropHref emits `<P:local><D:href>path</D:href></P:local>`.
func writePropHref(sb *strings.Builder, local, ns, path string) {
	p := nsPrefix(ns)
	sb.WriteString("        <" + p + ":" + local + "><D:href>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(path))
	sb.WriteString("</D:href></" + p + ":" + local + ">\n")
}

// writePropHrefs emits a prop containing zero or more `<D:href>` children.
func writePropHrefs(sb *strings.Builder, local, ns string, paths []string) {
	p := nsPrefix(ns)
	sb.WriteString("        <" + p + ":" + local + ">")
	for _, path := range paths {
		sb.WriteString("<D:href>")
		_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(path))
		sb.WriteString("</D:href>")
	}
	sb.WriteString("</" + p + ":" + local + ">\n")
}

// writePropText emits `<P:local>text</P:local>` with escaped char data.
func writePropText(sb *strings.Builder, local, ns, text string) {
	p := nsPrefix(ns)
	sb.WriteString("        <" + p + ":" + local + ">")
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(text))
	sb.WriteString("</" + p + ":" + local + ">\n")
}
