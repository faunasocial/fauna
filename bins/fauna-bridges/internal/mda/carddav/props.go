package carddav

// PROPPATCH dispatch lives here — the twin of caldav/props.go. emersion/go-webdav
// v0.7.0's carddav.Handler routes PROPPATCH through its internal Backend, which
// unconditionally 501s. The interceptor below sits in the middleware chain ahead
// of the inner handler so PROPPATCH never reaches emersion's 501 stub.
//
// Two paths land in this file (goal doc carddav-server.md § Write surface —
// "PROPPATCH on collection metadata" is part of the parity-or-better bar):
//
//  1. PROPPATCH on the address-book collection
//     (`/carddav/{user}/{ab_hex}/`): parse the propertyupdate body, mutate the
//     recognized DAV properties (`{DAV:}displayname`,
//     `{urn:ietf:params:xml:ns:carddav}addressbook-description`) on the decrypted
//     metadata struct, re-seal via SealCollectionMetadata, call
//     `wsrpc.ProvisionAddressbook(update_metadata=true)`. Map `Updated` → 207
//     multistatus with status 200 on each recognized prop; map `NotFound` → 404.
//     Unknown properties appear in the same 207 with status 403 +
//     `<error><cannot-modify-protected-property/></error>` per RFC 4918 §15.2;
//     the recognized changes still apply. An empty PROPPATCH (no set/remove) is
//     idempotent 207 with an empty propstat. Address books carry no color (the
//     CalDAV twin's third recognized prop), so displayname + description are the
//     full recognized set.
//
//  2. PROPPATCH on a card resource
//     (`/carddav/{user}/{ab_hex}/{uid_hash}.vcf`): rejected outright with 403
//     Forbidden + a top-level `<error><cannot-modify-protected-property/></error>`
//     body. Cards are atomic re-PUTs; per-card property mutation is rarely-used
//     and adds API surface for no product value (the CardDAV twin of CalDAV's
//     event-resource rejection).

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// createVisibilityRetrySchedule is the bounded back-off the collection PROPPATCH
// handler walks when the target address book is not yet visible in
// list_addressbooks. A client's "add an address book and name it" action fires
// MKCOL and the rename PROPPATCH back-to-back on separate HTTP connections;
// because every CardDAV request shares the one MDA→nest WS-RPC caller and the
// nest serializes on a single SQLite connection, the PROPPATCH's
// list_addressbooks can win the conn lock microseconds before the concurrent
// MKCOL insert commits → an empty list → a spurious 404 that makes the client
// roll the name back to its local placeholder. Re-reading after a short back-off
// lets the in-flight insert land. The common path — address book already present
// — never sleeps (the very first list_addressbooks finds it), so a standalone
// post-create rename pays nothing. A genuine PROPPATCH against a book the actor
// doesn't own walks the whole schedule then 404s, paying this bounded latency
// once. Overridden in tests via setCreateVisibilityRetry. Twin of the CalDAV
// schedule (caldav-server.md § Create-
// then-rename race).
var createVisibilityRetrySchedule = []time.Duration{
	25 * time.Millisecond,
	75 * time.Millisecond,
	150 * time.Millisecond,
	300 * time.Millisecond,
}

// XML element names the interceptor matches against. `xml.Name` zero-values both
// Space and Local; we always set both so namespace-prefixed elements (e.g.
// `<D:displayname>` with `xmlns:D="DAV:"`) match the canonical {namespace}local
// form.
var (
	propertyUpdateRoot = xml.Name{Space: "DAV:", Local: "propertyupdate"}
	propSetElem        = xml.Name{Space: "DAV:", Local: "set"}
	propRemoveElem     = xml.Name{Space: "DAV:", Local: "remove"}
	propElem           = xml.Name{Space: "DAV:", Local: "prop"}

	// Recognized property names. `displayname` lives in the DAV namespace;
	// `addressbook-description` in the CardDAV namespace. Address books have no
	// color analog (the CalDAV twin's third recognized prop).
	propDisplayname            = xml.Name{Space: "DAV:", Local: "displayname"}
	propAddressbookDescription = xml.Name{Space: "urn:ietf:params:xml:ns:carddav", Local: "addressbook-description"}
)

// newPropPatchInterceptor wraps `next` so that PROPPATCH requests are handled
// here instead of reaching emersion's 501 stub. Non-PROPPATCH requests pass
// through unchanged.
func newPropPatchInterceptor(next http.Handler, logger *slog.Logger) http.Handler {
	if next == nil {
		panic("carddav: newPropPatchInterceptor: next must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &propPatchInterceptor{next: next, logger: logger}
}

type propPatchInterceptor struct {
	next   http.Handler
	logger *slog.Logger
}

func (m *propPatchInterceptor) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != "PROPPATCH" {
		m.next.ServeHTTP(w, r)
		return
	}
	sess := davauth.SessionFromContext(r.Context())
	if sess == nil {
		// Auth middleware should have short-circuited before reaching here;
		// defensive 401 if a future routing change drops auth.
		w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, carddavRealm))
		http.Error(w, davauth.ErrAuthFailed.Error(), http.StatusUnauthorized)
		return
	}

	// Card-resource PROPPATCH is unconditionally rejected before any body
	// parsing. The `.vcf` suffix is the path-side signal (uid_hash filename
	// always carries it; the address-book collection path never does).
	if isCardResourcePath(sess, r.URL.Path) {
		writeForbiddenProtectedProperty(w)
		return
	}

	// Body is already w1-buffered + size-capped by w1Mitigations upstream.
	// ReadAll is safe.
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, fmt.Sprintf("carddav: read PROPPATCH body: %v", err), http.StatusBadRequest)
		return
	}

	abID, perr := parseAddressbookCollectionID(sess, r.URL.Path)
	if perr != nil {
		http.Error(w, perr.msg, perr.status)
		return
	}

	updates, parseErr := parsePropertyUpdate(body)
	if parseErr != nil {
		http.Error(w, fmt.Sprintf("carddav: parse propertyupdate: %v", parseErr), http.StatusBadRequest)
		return
	}

	m.handleCollectionPropPatch(w, r, sess, abID, updates)
}

// propUpdate is one parsed (Name, IsRemove[, Value]) entry from a `<D:set>` or
// `<D:remove>` block. `Value` is the inner character data for set entries;
// meaningless for removes.
type propUpdate struct {
	Name     xml.Name
	IsRemove bool
	Value    string
}

// parsePropertyUpdate walks a PROPPATCH body and returns the list of property
// mutations the request carries, in document order. An empty body or empty
// `<D:propertyupdate/>` returns an empty slice without error — an idempotent
// no-op PROPPATCH lands as 207 with no propstat entries.
func parsePropertyUpdate(body []byte) ([]propUpdate, error) {
	dec := xml.NewDecoder(bytes.NewReader(body))

	// First non-whitespace token must be the propertyupdate root.
	for {
		tok, err := dec.Token()
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil, errors.New("empty PROPPATCH body")
			}
			return nil, err
		}
		if start, ok := tok.(xml.StartElement); ok {
			if start.Name != propertyUpdateRoot {
				return nil, fmt.Errorf("unexpected root element %s; expected %s", xmlNameString(start.Name), xmlNameString(propertyUpdateRoot))
			}
			break
		}
	}

	var out []propUpdate
	// Walk children of <propertyupdate>: each is either <set> or <remove>.
	// Inside each, a single <prop> wraps the named props.
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
			isRemove := false
			switch t.Name {
			case propSetElem:
				isRemove = false
			case propRemoveElem:
				isRemove = true
			default:
				// Skip unknown children of propertyupdate (forward-compat).
				if err := dec.Skip(); err != nil {
					return nil, err
				}
				continue
			}
			block, err := parsePropBlock(dec, isRemove)
			if err != nil {
				return nil, err
			}
			out = append(out, block...)
		case xml.EndElement:
			if t.Name == propertyUpdateRoot {
				return out, nil
			}
		}
	}
}

// parsePropBlock parses the body of a <D:set> or <D:remove> element. Expects
// exactly one <D:prop> child (per RFC 4918 §14.18); anything else is silently
// ignored (forward-compat with extensions). Each child of <D:prop> becomes one
// propUpdate.
func parsePropBlock(dec *xml.Decoder, isRemove bool) ([]propUpdate, error) {
	var out []propUpdate
	for {
		tok, err := dec.Token()
		if err != nil {
			return nil, err
		}
		switch t := tok.(type) {
		case xml.StartElement:
			if t.Name != propElem {
				if err := dec.Skip(); err != nil {
					return nil, err
				}
				continue
			}
			// Walk each named property under <prop> until its end tag.
			done := false
			for !done {
				propTok, err := dec.Token()
				if err != nil {
					return nil, err
				}
				switch pt := propTok.(type) {
				case xml.StartElement:
					value, err := captureCharData(dec, pt.Name)
					if err != nil {
						return nil, err
					}
					out = append(out, propUpdate{
						Name:     pt.Name,
						IsRemove: isRemove,
						Value:    value,
					})
				case xml.EndElement:
					if pt.Name == propElem {
						done = true
					}
				}
			}
		case xml.EndElement:
			// End of <set> / <remove>.
			return out, nil
		}
	}
}

// captureCharData reads the character data inside a property element (the inner
// text) and consumes the matching end element. Nested child elements (rare in
// real PROPPATCH) are collected as serialized text for forward-compat; the
// recognized properties only carry character data.
func captureCharData(dec *xml.Decoder, end xml.Name) (string, error) {
	var sb strings.Builder
	for {
		tok, err := dec.Token()
		if err != nil {
			return "", err
		}
		switch t := tok.(type) {
		case xml.CharData:
			sb.Write(t)
		case xml.StartElement:
			// Skip nested elements but include their inner text.
			inner, err := captureCharData(dec, t.Name)
			if err != nil {
				return "", err
			}
			sb.WriteString(inner)
		case xml.EndElement:
			if t.Name == end {
				return sb.String(), nil
			}
		}
	}
}

// handleCollectionPropPatch routes a parsed PROPPATCH on an address-book
// collection: applies the recognized mutations to the decrypted metadata struct,
// re-seals, calls provision_addressbook(update_metadata=true) if any recognized
// prop actually changed, then emits a 207 multistatus body with per-prop status.
func (m *propPatchInterceptor) handleCollectionPropPatch(
	w http.ResponseWriter,
	r *http.Request,
	sess *davauth.Session,
	abID []byte,
	updates []propUpdate,
) {
	ctx := r.Context()

	// Partition updates into recognized + unknown. Recognized props are folded
	// into a single metadata mutation; unknowns are reported as 403 in the
	// multistatus.
	type recognized struct {
		Name  xml.Name
		Apply func(m *EncryptedCollectionMetadata)
	}
	var (
		recognizedProps []recognized
		unknownProps    []xml.Name
	)
	for i := range updates {
		u := updates[i]
		switch {
		case u.Name == propDisplayname:
			value := u.Value
			if u.IsRemove {
				value = ""
			}
			recognizedProps = append(recognizedProps, recognized{
				Name:  u.Name,
				Apply: func(m *EncryptedCollectionMetadata) { m.Displayname = value },
			})
		case u.Name == propAddressbookDescription:
			value := u.Value
			if u.IsRemove {
				value = ""
			}
			recognizedProps = append(recognizedProps, recognized{
				Name:  u.Name,
				Apply: func(m *EncryptedCollectionMetadata) { m.Description = value },
			})
		default:
			unknownProps = append(unknownProps, u.Name)
		}
	}

	// Honor recognized mutations against the current encrypted state. Empty
	// `recognized` is fine — the response is still 207 with just the unknown-prop
	// entries (or no propstat at all for the pure-empty case).
	if len(recognizedProps) > 0 {
		entry, found, err := m.findAddressBookWithCreateRaceRetry(ctx, sess, abID)
		if err != nil {
			http.Error(w, fmt.Sprintf("carddav: list_addressbooks: %v", err), http.StatusInternalServerError)
			return
		}
		if !found {
			m.logger.Warn("carddav: PROPPATCH target address book not found after create-visibility retries",
				"addressbook_id", hex.EncodeToString(abID))
			http.Error(w, fmt.Sprintf("carddav: address book %s not found", hex.EncodeToString(abID)), http.StatusNotFound)
			return
		}
		meta, err := UnsealCollectionMetadata(entry.EncryptedMetadata, sess.RecordOpener())
		if err != nil {
			http.Error(w, fmt.Sprintf("carddav: unseal metadata: %v", err), http.StatusInternalServerError)
			return
		}
		for i := range recognizedProps {
			recognizedProps[i].Apply(&meta)
		}
		sealed, err := SealCollectionMetadata(meta, sess.MLSPubkey(), sess.MlkemEk())
		if err != nil {
			http.Error(w, fmt.Sprintf("carddav: seal metadata: %v", err), http.StatusInternalServerError)
			return
		}
		outcome, err := wsrpc.ProvisionAddressbook(
			ctx, sess.Client(),
			sess.ActorID(), abID, sealed,
			true, // PROPPATCH overwrite path
		)
		if err != nil {
			http.Error(w, fmt.Sprintf("carddav: provision_addressbook: %v", err), http.StatusInternalServerError)
			return
		}
		switch outcome {
		case wsrpc.ProvisionAddressbookUpdated:
			// fall through to multistatus
		case wsrpc.ProvisionAddressbookNotFound:
			http.Error(w, fmt.Sprintf("carddav: address book %s not found", hex.EncodeToString(abID)), http.StatusNotFound)
			return
		default:
			http.Error(w, fmt.Sprintf("carddav: provision_addressbook: unexpected outcome %q", outcome), http.StatusInternalServerError)
			return
		}
	}

	href := userBasePath(sess) + hex.EncodeToString(abID) + "/"
	recognizedNames := make([]xml.Name, len(recognizedProps))
	for i := range recognizedProps {
		recognizedNames[i] = recognizedProps[i].Name
	}
	m.writeMultistatus(w, href, recognizedNames, unknownProps)
}

// findAddressBookWithCreateRaceRetry resolves the PROPPATCH target address book
// from list_addressbooks, re-reading under createVisibilityRetrySchedule when it
// isn't present yet so a rename that races its own MKCOL create doesn't spuriously
// 404. Returns (entry, true, nil) once the book appears, (zero, false, nil) when
// the whole schedule is exhausted without it (a genuine "not your book" 404), or
// (zero, false, err) on a transport failure / client disconnect. The first
// list_addressbooks happens with no delay, so the common case — book already
// provisioned — costs nothing. Twin of CalDAV's findCalendarWithCreateRaceRetry.
func (m *propPatchInterceptor) findAddressBookWithCreateRaceRetry(
	ctx context.Context,
	sess *davauth.Session,
	abID []byte,
) (wsrpc.AddressbookEntry, bool, error) {
	for attempt := 0; ; attempt++ {
		entries, err := wsrpc.ListAddressbooks(ctx, sess.Client(), sess.ActorID())
		if err != nil {
			return wsrpc.AddressbookEntry{}, false, err
		}
		for i := range entries {
			if bytes.Equal(entries[i].AddressbookID, abID) {
				if attempt > 0 {
					// A real create-then-rename race was caught and absorbed;
					// surface it so the fix's effect is visible in production
					// logs without a separate access log.
					m.logger.Info("carddav: PROPPATCH absorbed an address-book create race",
						"addressbook_id", hex.EncodeToString(abID), "retries", attempt)
				}
				return entries[i], true, nil
			}
		}
		if attempt >= len(createVisibilityRetrySchedule) {
			return wsrpc.AddressbookEntry{}, false, nil
		}
		select {
		case <-ctx.Done():
			return wsrpc.AddressbookEntry{}, false, ctx.Err()
		case <-time.After(createVisibilityRetrySchedule[attempt]):
		}
	}
}

// writeMultistatus emits the 207 PROPPATCH response per RFC 4918 §15.2. Two
// propstat groups maximum: one 200 OK for recognized props, one 403 with
// cannot-modify-protected-property for unknown props. The caller has already
// returned early on 404/500/error paths; this is the success-or-partial-success
// terminal.
func (m *propPatchInterceptor) writeMultistatus(
	w http.ResponseWriter,
	href string,
	recognized []xml.Name,
	unknown []xml.Name,
) {
	var sb strings.Builder
	sb.WriteString(`<?xml version="1.0" encoding="utf-8"?>` + "\n")
	sb.WriteString(`<D:multistatus xmlns:D="DAV:">` + "\n")
	sb.WriteString("  <D:response>\n")
	sb.WriteString("    <D:href>")
	_ = xml.EscapeText(&strBuilderWriter{&sb}, []byte(href))
	sb.WriteString("</D:href>\n")
	if len(recognized) > 0 {
		sb.WriteString("    <D:propstat>\n")
		sb.WriteString("      <D:prop>\n")
		for _, name := range recognized {
			writePropEmpty(&sb, name)
		}
		sb.WriteString("      </D:prop>\n")
		sb.WriteString("      <D:status>HTTP/1.1 200 OK</D:status>\n")
		sb.WriteString("    </D:propstat>\n")
	}
	if len(unknown) > 0 {
		sb.WriteString("    <D:propstat>\n")
		sb.WriteString("      <D:prop>\n")
		for _, name := range unknown {
			writePropEmpty(&sb, name)
		}
		sb.WriteString("      </D:prop>\n")
		sb.WriteString("      <D:status>HTTP/1.1 403 Forbidden</D:status>\n")
		sb.WriteString("      <D:error><D:cannot-modify-protected-property/></D:error>\n")
		sb.WriteString("    </D:propstat>\n")
	}
	sb.WriteString("  </D:response>\n")
	sb.WriteString("</D:multistatus>\n")

	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.WriteHeader(http.StatusMultiStatus)
	_, _ = io.WriteString(w, sb.String())
}

// writePropEmpty renders one self-closing property element with the caller's
// namespace. Uses the literal namespace URI in the `xmlns="..."` attribute so
// the consumer sees the same namespace the request used — the only safe
// round-trip across DAV: / CardDAV extensions without negotiating prefix aliases.
func writePropEmpty(sb *strings.Builder, name xml.Name) {
	sb.WriteString("        <")
	sb.WriteString(name.Local)
	if name.Space != "" {
		sb.WriteString(` xmlns="`)
		_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(name.Space))
		sb.WriteString(`"`)
	}
	sb.WriteString("/>\n")
}

// writeForbiddenProtectedProperty emits the top-level 403 body the card-resource
// PROPPATCH path returns. Single error element, no multistatus. Twin of CalDAV's
// event-resource rejection body.
func writeForbiddenProtectedProperty(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.WriteHeader(http.StatusForbidden)
	_, _ = io.WriteString(w,
		`<?xml version="1.0" encoding="utf-8"?>`+"\n"+
			`<D:error xmlns:D="DAV:"><D:cannot-modify-protected-property/></D:error>`+"\n",
	)
}

// isCardResourcePath returns true when the path is shaped like a card resource:
// `/carddav/{user}/{ab_hex}/{uid_hash}.vcf`. The .vcf suffix is the unambiguous
// signal; address-book collection paths never carry it. Twin of CalDAV's
// isEventResourcePath.
func isCardResourcePath(sess *davauth.Session, urlPath string) bool {
	base := userBasePath(sess)
	if !strings.HasPrefix(urlPath, base) {
		return false
	}
	rest := strings.TrimPrefix(urlPath, base)
	rest = strings.TrimSuffix(rest, "/")
	parts := strings.SplitN(rest, "/", 2)
	if len(parts) < 2 {
		return false
	}
	return strings.HasSuffix(parts[1], ".vcf")
}

// xmlNameString renders an xml.Name as `{namespace}local` for error messages.
// Mirrors Go's encoding/xml debug rendering.
func xmlNameString(n xml.Name) string {
	if n.Space == "" {
		return n.Local
	}
	return "{" + n.Space + "}" + n.Local
}
