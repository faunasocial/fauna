package caldav

// MKCALENDAR dispatch lives here. emersion/go-webdav v0.7.0's caldav.Handler
// special-cases only REPORT (and the `/.well-known/caldav` redirect) and routes
// every other method to the internal WebDAV handler — which knows MKCOL (→
// Backend.CreateCalendar) but has NO case for the dedicated CalDAV MKCALENDAR
// verb (RFC 4791 §5.3.1), so MKCALENDAR falls through to 405 Method Not
// Allowed. macOS Calendar.app issues MKCALENDAR — not the extended-MKCOL form —
// when a user adds a calendar, so without this interceptor the create surfaces
// in Calendar.app as the alert "This is not a location that supports this
// request." The interceptor sits ahead of the inner handler in the middleware
// chain (after auth, so a Session is in context) and translates MKCALENDAR to
// the same `provision_calendar(update_metadata=false)` insert that MKCOL uses.
//
// The target collection URL is the client's choice (macOS picks an opaque
// slug). `resolveCalendarSegment` maps it deterministically to the 32-byte
// calendar_id (blake3 of a non-hex slug), so the nest keeps fixed-width ids
// while honoring the client's URL on every later request.

import (
	"bytes"
	"encoding/hex"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// mkcalendarRoot is the RFC 4791 §5.3.1 request-body root element a stock
// CalDAV client sends to create a calendar collection.
var mkcalendarRoot = xml.Name{Space: "urn:ietf:params:xml:ns:caldav", Local: "mkcalendar"}

// newMkcalendarInterceptor wraps `next` so MKCALENDAR requests are handled here
// instead of reaching emersion's 405. Non-MKCALENDAR requests pass through
// unchanged.
func newMkcalendarInterceptor(next http.Handler, logger *slog.Logger) http.Handler {
	if next == nil {
		panic("caldav: newMkcalendarInterceptor: next must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &mkcalendarInterceptor{next: next, logger: logger}
}

type mkcalendarInterceptor struct {
	next   http.Handler
	logger *slog.Logger
}

func (m *mkcalendarInterceptor) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != "MKCALENDAR" {
		m.next.ServeHTTP(w, r)
		return
	}
	sess := davauth.SessionFromContext(r.Context())
	if sess == nil {
		// Auth middleware should have short-circuited before here; defensive
		// 401 if a future routing change drops auth.
		w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, caldavRealm))
		http.Error(w, davauth.ErrAuthFailed.Error(), http.StatusUnauthorized)
		return
	}

	// MKCALENDAR targets a calendar collection under the AUTH'd actor's home
	// set (`/caldav/{user}/{segment}/`). An event-resource path (…/{file}.ics)
	// is never a valid MKCALENDAR target.
	if isEventResourcePath(sess, r.URL.Path) {
		http.Error(w, "caldav: MKCALENDAR target must be a calendar collection, not an event resource", http.StatusForbidden)
		return
	}
	calID, perr := parseCalendarCollectionID(sess, r.URL.Path)
	if perr != nil {
		http.Error(w, perr.msg, perr.status)
		return
	}

	// Body is w1-buffered + size-capped upstream; ReadAll is safe. The body is
	// OPTIONAL per RFC 4791 §5.3.1 — a bare MKCALENDAR with no initial
	// properties is valid and lands a calendar with the default metadata.
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: read MKCALENDAR body: %v", err), http.StatusBadRequest)
		return
	}
	props, perr2 := parseMkcalendarProps(body)
	if perr2 != nil {
		http.Error(w, fmt.Sprintf("caldav: parse MKCALENDAR body: %v", perr2), http.StatusBadRequest)
		return
	}

	// Default the metadata (works-out-of-the-box: a no-property MKCALENDAR
	// still renders a named, colored calendar), then fold in the client's
	// recognized initial properties. Unknown props are ignored (RFC 4791
	// §5.3.1 lets a server ignore properties it does not recognize on create).
	meta := EncryptedCollectionMetadata{
		Displayname: defaultDisplayname,
		Color:       defaultColor,
	}
	for i := range props {
		switch props[i].Name {
		case propDisplayname:
			meta.Displayname = props[i].Value
		case propCalendarColorApple, propCalendarColorIETF:
			meta.Color = props[i].Value
		case propCalendarDescription:
			meta.Description = props[i].Value
		}
	}

	sealed, err := SealCollectionMetadata(meta, sess.MLSPubkey(), sess.MlkemEk())
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: seal new calendar metadata: %v", err), http.StatusInternalServerError)
		return
	}
	outcome, err := wsrpc.ProvisionCalendar(
		r.Context(), sess.Client(),
		sess.ActorID(), calID, sealed,
		false, // insert path; PROPPATCH (update_metadata=true) handles later edits
	)
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: provision_calendar: %v", err), http.StatusInternalServerError)
		return
	}
	switch outcome {
	case wsrpc.ProvisionCalendarCreated:
		w.WriteHeader(http.StatusCreated)
	case wsrpc.ProvisionCalendarAlreadyExists, wsrpc.ProvisionCalendarConflict:
		// RFC 4791 §5.3.1 / RFC 4918 §9.3: MKCALENDAR on a Request-URI where a
		// collection already exists → 405 Method Not Allowed, regardless of the
		// stored metadata. The nest's AlreadyExists (byte-identical) vs Conflict
		// (different bytes) split is unreachable from a real client:
		// SealCollectionMetadata HPKE-seals with a fresh ephemeral key on every
		// call (EncryptToRecipient), so a re-MKCALENDAR's ciphertext never
		// byte-matches the stored blob and the nest always answers Conflict — yet
		// "a collection already exists here" is 405 either way (the metadata-bytes
		// distinction is a create-path non-signal; a metadata *edit* is PROPPATCH,
		// not MKCALENDAR). Returning 409 here violated §5.3.1 and was caught by the
		// tier_3 round-trip (the in-process twin mocks the outcome, so it couldn't).
		http.Error(w, fmt.Sprintf("caldav: calendar %s already exists", hex.EncodeToString(calID)), http.StatusMethodNotAllowed)
	default:
		http.Error(w, fmt.Sprintf("caldav: provision_calendar: unexpected outcome %q", outcome), http.StatusInternalServerError)
	}
}

// parseMkcalendarProps extracts the initial properties from an RFC 4791 §5.3.1
// MKCALENDAR body:
//
//	<C:mkcalendar><D:set><D:prop>…</D:prop></D:set></C:mkcalendar>
//
// An empty body (a valid bare MKCALENDAR) returns no props. Reuses the
// PROPPATCH `<D:set>` block parser (parsePropBlock), so the recognized property
// shapes (displayname / calendar-color / calendar-description) match the edit
// path exactly.
func parseMkcalendarProps(body []byte) ([]propUpdate, error) {
	if len(bytes.TrimSpace(body)) == 0 {
		return nil, nil
	}
	dec := xml.NewDecoder(bytes.NewReader(body))
	// First non-whitespace token must be the mkcalendar root.
	for {
		tok, err := dec.Token()
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil, nil
			}
			return nil, err
		}
		if start, ok := tok.(xml.StartElement); ok {
			if start.Name != mkcalendarRoot {
				return nil, fmt.Errorf("unexpected root element %s; expected %s", xmlNameString(start.Name), xmlNameString(mkcalendarRoot))
			}
			break
		}
	}
	var out []propUpdate
	for {
		tok, err := dec.Token()
		if err != nil {
			if errors.Is(err, io.EOF) {
				return out, nil
			}
			return nil, err
		}
		switch t := tok.(type) {
		case xml.StartElement:
			if t.Name == propSetElem {
				block, err := parsePropBlock(dec, false)
				if err != nil {
					return nil, err
				}
				out = append(out, block...)
			} else {
				// Skip unknown children of <mkcalendar> (e.g. a stray
				// <D:remove>, forward-compat).
				if err := dec.Skip(); err != nil {
					return nil, err
				}
			}
		case xml.EndElement:
			if t.Name == mkcalendarRoot {
				return out, nil
			}
		}
	}
}
