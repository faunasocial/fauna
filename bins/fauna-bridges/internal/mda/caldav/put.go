package caldav

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/emersion/go-ical"
	"github.com/emersion/go-webdav"
	"github.com/emersion/go-webdav/caldav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// PutCalendarObject lands the CalDAV PUT path per `caldav-server.md`
// § Write surface row 101. Flow:
//
//  1. Re-encode the parsed *ical.Calendar via emersion's encoder so
//     PUT and the eventual REPORT decrypt agree on byte-stable bodies.
//  2. mailfauna.ParseICalendar → enforce UID/DTSTAMP/DTSTART per
//     § iCalendar parsing rules; missing any → 400.
//  3. uid_hash = blake3(UID)[:32] becomes the canonical filename slug;
//     the client's chosen filename is ignored — we rewrite Location.
//  4. Tokenize the re-encoded text into a CanonicalTokenSet (encrypted-
//     mode SEARCH-axis index hint).
//  5. HPKE-seal raw body to actor's MLS pubkey; HPKE-seal hint bytes to
//     actor's index pubkey (falling back to MLS pubkey when the actor
//     has no index key yet, mirroring imap/append.go's Phase-E gap
//     fallback). Wire bytes never see plaintext.
//  6. wsrpc.PutEventCiphertext → outcome dispatch:
//     - Created/Updated → 201 + ETag + Location (emersion hard-codes
//     201; v1 accepts the wart per goal doc row 101 commentary).
//     - PreconditionFailed → 412 with current ETag in body.
//     - CalendarNotFound → 404.
//
// emersion's `caldav.Handler.Put` (server.go:668-712) reads `opts.IfMatch`
// from the `If-Match` request header before dispatching here; we don't
// touch the header directly.
func (b *Backend) PutCalendarObject(
	ctx context.Context,
	urlPath string,
	c *ical.Calendar,
	opts *caldav.PutCalendarObjectOptions,
) (*caldav.CalendarObject, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	calID, err := parseCalendarID(sess, urlPath)
	if err != nil {
		return nil, err
	}

	rawBytes, err := encodeICalendar(c)
	if err != nil {
		return nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("re-encode iCalendar body: %w", err))
	}
	if len(rawBytes) > maxResourceSizeBytes {
		return nil, webdav.NewHTTPError(http.StatusRequestEntityTooLarge,
			fmt.Errorf("iCalendar body %d bytes exceeds %d cap", len(rawBytes), maxResourceSizeBytes))
	}

	doc, err := mailfauna.ParseICalendar(rawBytes)
	if err != nil {
		return nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("parse iCalendar body: %w", err))
	}
	uid, parseErr := extractRequiredEventFields(doc)
	if parseErr != nil {
		return nil, parseErr
	}

	uidHashArr := blake3.Sum256([]byte(uid))
	uidHash := make([]byte, 32)
	copy(uidHash, uidHashArr[:])

	encryptedBody, encryptedHint, err := sealEventForSession(sess, rawBytes)
	if err != nil {
		return nil, err
	}

	ifMatch, err := resolveIfMatch(opts)
	if err != nil {
		return nil, err
	}
	if opts != nil && opts.IfNoneMatch.IsSet() {
		// `If-None-Match` (RFC 7232 §3.2): `*` refuses a PUT onto an event that
		// already exists — a calendar app's create-only write — and an ETag
		// refuses one whose current version carries it. put_event_ciphertext
		// carries If-Match only, so this reads the calendar first (the CardDAV
		// PUT's twin; same read-then-write window).
		if existing, _, qerr := findEvent(ctx, sess, calID, uidHash); qerr == nil && existing != nil {
			if opts.IfNoneMatch.IsWildcard() {
				return nil, webdav.NewHTTPError(http.StatusPreconditionFailed,
					errors.New("If-None-Match: * — the event already exists"))
			}
			if etag, eerr := opts.IfNoneMatch.ETag(); eerr == nil && etag == existing.ETag {
				return nil, webdav.NewHTTPError(http.StatusPreconditionFailed,
					fmt.Errorf("If-None-Match: the event is still at etag %s", etag))
			}
		}
	}

	// Read-before-write for CANCEL-on-removal: capture the PRIOR stored roster
	// BEFORE the PUT overwrites it, so a removed attendee can be sent an iMIP
	// CANCEL (caldav-server.md § Server-side auto-schedule). Gated on the new
	// body's ORGANIZER, so the common non-organizer/personal PUT pays nothing;
	// best-effort (nil on a create or a snapshot-less session).
	priorICS := b.priorRosterForAutoSchedule(ctx, sess, calID, rawBytes, uidHash)

	timestamp := time.Now().Unix()
	result, err := wsrpc.PutEventCiphertext(
		ctx, sess.Client(),
		sess.ActorID(), calID, uidHash,
		encryptedBody, encryptedHint,
		timestamp, uint32(len(encryptedBody)),
		ifMatch,
	)
	if err != nil {
		if dav.IsOverQuota(err) {
			// No room on the account: 507, answered with the
			// DAV:quota-not-exceeded body by dav.QuotaBody.
			return nil, webdav.NewHTTPError(http.StatusInsufficientStorage, err)
		}
		return nil, fmt.Errorf("put_event_ciphertext: %w", err)
	}

	canonicalPath := userBasePath(sess) +
		hex.EncodeToString(calID) + "/" +
		hex.EncodeToString(uidHash) + ".ics"

	switch result.Outcome {
	case wsrpc.PutEventCreated, wsrpc.PutEventUpdated:
		// Server-side `calendar-auto-schedule` organizer fan-out: if the AUTH'd
		// actor is this event's ORGANIZER, fan an iMIP REQUEST out to its
		// email-reachable attendees, and an iMIP CANCEL to any attendee dropped
		// from the prior roster (caldav-server.md § Server-side auto-schedule).
		// Best-effort — never fails the committed PUT.
		b.maybeFanOutAutoSchedule(ctx, sess, rawBytes, priorICS, uidHash)
		return &caldav.CalendarObject{
			Path:          canonicalPath,
			ModTime:       time.Unix(timestamp, 0),
			ContentLength: int64(len(rawBytes)),
			ETag:          result.ETag,
			Data:          c,
		}, nil
	case wsrpc.PutEventPreconditionFailed:
		return nil, webdav.NewHTTPError(http.StatusPreconditionFailed,
			fmt.Errorf("If-Match mismatch; current etag=%s", result.CurrentETag))
	case wsrpc.PutEventCalendarNotFound:
		return nil, webdav.NewHTTPError(http.StatusNotFound,
			fmt.Errorf("calendar %s not found", hex.EncodeToString(calID)))
	default:
		return nil, fmt.Errorf("put_event_ciphertext: unexpected outcome %q", result.Outcome)
	}
}

// sealEventForSession seals a stored event body and its search-index hint to
// the AUTH'd actor — the write half every event the MDA stores goes through (a
// PUT, and the re-seal a MOVE/COPY performs). Encrypt-only.
//
// Phase-3 D1 (`2026-07-07-phase-3-sealed-both-modes-design.md`): sealed at
// rest in BOTH storage modes — one at-rest byte shape. The body seals X-Wing
// to the actor's two key halves (leg D2a), and so does the index hint while
// its key is the MLS-pubkey fallback. The
// index hint falls back to the MLS pubkey when the actor has no index key
// (the Phase-E gap), mirroring imap/append.go so REPORT opens both shapes.
//
// Every call yields FRESH ciphertext (HPKE encapsulation is randomized), and
// that is load-bearing: nest files a body as a content record keyed by its
// hash, so two rows holding byte-identical ciphertext would share one record,
// and deleting either row would tombstone the other's body.
func sealEventForSession(sess *davauth.Session, raw []byte) (body, hint []byte, err error) {
	mlsPubkey := sess.MLSPubkey()
	if mlsPubkey == nil {
		return nil, nil, webdav.NewHTTPError(http.StatusInternalServerError,
			errors.New("session missing MLS pubkey"))
	}
	indexKey := sess.IndexKey()
	if indexKey == nil {
		indexKey = mlsPubkey
	}
	body, err = mailfauna.EncryptToRecipientHybrid(raw, mlsPubkey, sess.MlkemEk())
	if err != nil {
		return nil, nil, fmt.Errorf("seal event body: %w", err)
	}
	tokens := mailfauna.Tokenize(string(raw))
	hint, err = mailfauna.EncryptToRecipientHybrid(
		tokens.CanonicalBytes, indexKey, mailfauna.IndexHintMlkemEk(indexKey, mlsPubkey, sess.MlkemEk()))
	if err != nil {
		return nil, nil, fmt.Errorf("seal event index hint: %w", err)
	}
	return body, hint, nil
}

// encodeICalendar re-encodes a parsed *ical.Calendar back to wire
// bytes. emersion's encoder/decoder pair round-trips byte-stably for
// the subset CalDAV MUAs produce; PUT and the eventual REPORT decrypt
// share the same encoder so the recipient sees byte-identical bodies.
func encodeICalendar(c *ical.Calendar) ([]byte, error) {
	var buf bytes.Buffer
	if err := ical.NewEncoder(&buf).Encode(c); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// extractRequiredEventFields walks `doc`'s top-level components for
// the first VEVENT or VTODO, then verifies UID / DTSTAMP / DTSTART
// per `caldav-server.md` § iCalendar parsing rules. Returns the UID
// string on success; returns a 400 HTTPError otherwise.
//
// VJOURNAL is explicitly rejected (out of scope per goal doc); any
// other top-level component leads to "no VEVENT or VTODO present" and
// 400.
func extractRequiredEventFields(doc mailfauna.ICalDocument) (string, error) {
	for i := range doc.Components {
		comp := &doc.Components[i]
		switch {
		case strings.EqualFold(comp.Name, ical.CompEvent),
			strings.EqualFold(comp.Name, ical.CompToDo):
			return validateEventComponent(comp)
		case strings.EqualFold(comp.Name, ical.CompJournal):
			return "", webdav.NewHTTPError(http.StatusBadRequest,
				errors.New("VJOURNAL is out of scope; PUT only accepts VEVENT or VTODO"))
		}
	}
	return "", webdav.NewHTTPError(http.StatusBadRequest,
		errors.New("iCalendar body has no VEVENT or VTODO component"))
}

// validateEventComponent enforces UID / DTSTAMP / DTSTART presence on
// the given VEVENT-or-VTODO component. Returns the UID string.
func validateEventComponent(comp *mailfauna.ICalComponent) (string, error) {
	uid := findProperty(comp, "UID")
	dtstamp := findProperty(comp, "DTSTAMP")
	dtstart := findProperty(comp, "DTSTART")
	var missing []string
	if uid == "" {
		missing = append(missing, "UID")
	}
	if dtstamp == "" {
		missing = append(missing, "DTSTAMP")
	}
	if dtstart == "" {
		missing = append(missing, "DTSTART")
	}
	if len(missing) > 0 {
		return "", webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("iCalendar %s missing required properties: %s",
				comp.Name, strings.Join(missing, ", ")))
	}
	return uid, nil
}

// findProperty does a case-insensitive lookup of `name` in `comp`'s
// properties. iCalendar property names are case-insensitive per
// RFC 5545; the Rust parser preserves whatever casing the wire body
// supplied, so we match insensitively here.
func findProperty(comp *mailfauna.ICalComponent, name string) string {
	for _, p := range comp.Properties {
		if strings.EqualFold(p.Name, name) {
			return p.Value
		}
	}
	return ""
}

// resolveIfMatch reduces a `caldav.PutCalendarObjectOptions` to the
// `*string` shape `wsrpc.PutEventCiphertext` expects.
//
//   - opts == nil or IfMatch unset → nil (unconditional PUT).
//   - IfMatch "*" → nil (wildcard match-any; nest's `put_event_ciphertext`
//     has no "must-exist" gate today, so we accept the wart and treat
//     it as unconditional. A future tightening could send `&"*"` if
//     nest learns it.)
//   - IfMatch quoted ETag → unwrap via emersion's `.ETag()`, pass &etag.
//   - Malformed → 400 BadRequest.
func resolveIfMatch(opts *caldav.PutCalendarObjectOptions) (*string, error) {
	if opts == nil || !opts.IfMatch.IsSet() {
		return nil, nil
	}
	if opts.IfMatch.IsWildcard() {
		return nil, nil
	}
	etag, err := opts.IfMatch.ETag()
	if err != nil {
		return nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("malformed If-Match header: %w", err))
	}
	return &etag, nil
}
