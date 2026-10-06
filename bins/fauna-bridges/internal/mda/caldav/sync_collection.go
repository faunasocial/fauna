package caldav

// REPORT sync-collection (RFC 6578) is intercepted before emersion's
// caldav.Handler sees it: emersion v0.7.0's reportReq only recognizes
// {urn:ietf:params:xml:ns:caldav}calendar-query and calendar-multiget
// (caldav/elements.go:233), and 400s on any other REPORT root. The
// interceptor below peeks the REPORT body, routes {DAV:}sync-collection
// to nest's `sync_calendar_since` RPC, and passes everything else
// through to the inner handler unchanged.
//
// Stale-token handling (goal doc § Stale sync-token handling) landed in
// T3.5: nest's sync_calendar_since_handler now runs a tombstone-retention
// check and returns a distinct `SyncCalendarSinceReply::Ok { stale: true }`
// signal when the supplied token predates the retention window — separate
// from the steady-state "no changes" reply (which has stale=false) and
// from the `Stale` outcome (MUA-ahead / restore). On either signal
// (`Ok{stale:true}` or the `Stale` outcome) the interceptor emits the RFC
// 6578 §3.8 `DAV:valid-sync-token` precondition failure (writeValidSyncToken)
// so the MUA falls through to a full PROPFIND + per-event GET.

import (
	"bytes"
	"encoding/hex"
	"encoding/xml"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// syncCollectionRoot is the XML element name (namespace + local) we
// match against incoming REPORT bodies to decide whether to intercept.
var syncCollectionRoot = xml.Name{Space: "DAV:", Local: "sync-collection"}

// newSyncCollectionInterceptor wraps `next` so that REPORT requests
// whose body is a `{DAV:}sync-collection` element are handled here
// instead of delegated. The middleware peeks the buffered body
// (w1Mitigations already drained + restored it), so the body is
// available without consuming the downstream reader. Non-REPORT
// requests + REPORT requests with other root elements (calendar-query,
// calendar-multiget) pass through unchanged.
func newSyncCollectionInterceptor(next http.Handler, logger *slog.Logger) http.Handler {
	if next == nil {
		panic("caldav: newSyncCollectionInterceptor: next must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &syncCollectionInterceptor{next: next, logger: logger}
}

type syncCollectionInterceptor struct {
	next   http.Handler
	logger *slog.Logger
}

func (m *syncCollectionInterceptor) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != "REPORT" {
		m.next.ServeHTTP(w, r)
		return
	}
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: read REPORT body: %v", err), http.StatusBadRequest)
		return
	}
	// Restore the body for downstream handlers regardless of which
	// branch we take.
	r.Body = io.NopCloser(bytes.NewReader(body))
	r.ContentLength = int64(len(body))

	if !isSyncCollection(body) {
		m.next.ServeHTTP(w, r)
		return
	}
	m.handleSyncCollection(w, r, body)
}

// isSyncCollection scans XML tokens until the first StartElement and
// returns true if it matches {DAV:}sync-collection. Empty / malformed
// bodies return false — the downstream handler will produce a 400.
func isSyncCollection(body []byte) bool {
	dec := xml.NewDecoder(bytes.NewReader(body))
	for {
		tok, err := dec.Token()
		if err != nil {
			return false
		}
		if start, ok := tok.(xml.StartElement); ok {
			return start.Name == syncCollectionRoot
		}
	}
}

// syncCollectionReq is the parsed RFC 6578 §3.2 request envelope. The
// Prop element is ignored — we always emit getetag + calendar-data
// (the only properties RFC 6578 + RFC 4791 specify for a
// sync-collection response on a calendar collection).
type syncCollectionReq struct {
	XMLName   xml.Name `xml:"DAV: sync-collection"`
	SyncToken string   `xml:"DAV: sync-token"`
	// SyncLevel is informational. RFC 6578 §6.3 declares "1" or
	// "infinity"; for a calendar collection (a flat collection of
	// resources) both behave identically, so we accept any value
	// without further validation.
	SyncLevel string `xml:"DAV: sync-level"`
}

func (m *syncCollectionInterceptor) handleSyncCollection(w http.ResponseWriter, r *http.Request, body []byte) {
	sess := davauth.SessionFromContext(r.Context())
	if sess == nil {
		// Auth middleware should have short-circuited before reaching
		// here; defensive 401 if a future routing change drops auth.
		w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, caldavRealm))
		http.Error(w, davauth.ErrAuthFailed.Error(), http.StatusUnauthorized)
		return
	}
	calID, perr := parseCalendarIDForSync(sess, r.URL.Path)
	if perr != nil {
		http.Error(w, perr.msg, perr.status)
		return
	}
	var req syncCollectionReq
	if err := xml.Unmarshal(body, &req); err != nil {
		http.Error(w, fmt.Sprintf("caldav: parse sync-collection: %v", err), http.StatusBadRequest)
		return
	}
	syncToken := strings.TrimSpace(req.SyncToken)
	if syncToken == "" {
		syncToken = "0"
	}
	muaID := r.Header.Get("User-Agent")

	ctx := r.Context()
	res, err := wsrpc.SyncCalendarSince(
		ctx, sess.Client(),
		sess.ActorID(), calID,
		syncToken, 0,
		muaID,
	)
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: sync_calendar_since: %v", err), http.StatusInternalServerError)
		return
	}
	switch res.Outcome {
	case wsrpc.SyncCalendarSinceOk:
		if res.Stale {
			// RFC 6578 §3.8: the supplied sync-token is valid but predates
			// the tombstone-retention window, so nest cannot honestly
			// enumerate the deletions since then (caldav-server.md § Stale
			// sync-token handling). Same client-facing remedy as the
			// MUA-ahead Stale outcome — DAV:valid-sync-token → full PROPFIND.
			writeValidSyncToken(w)
			return
		}
		// continue below.
	case wsrpc.SyncCalendarSinceCalendarNotFound:
		http.Error(w, "calendar not found", http.StatusNotFound)
		return
	case wsrpc.SyncCalendarSinceStale:
		// RFC 6578 §3.8: the server's sync-token is behind what the client
		// holds (post-DR-restore "MUA ahead" case). Return the
		// DAV:valid-sync-token precondition failure so the MUA falls through
		// to a full PROPFIND.
		writeValidSyncToken(w)
		return
	default:
		http.Error(w, fmt.Sprintf("caldav: sync_calendar_since: unexpected outcome %q", res.Outcome), http.StatusInternalServerError)
		return
	}

	// ONE serve path (sealed-both-modes design D1) via OpenStoredRecord. The
	// per-session opener is nil when no MLS snapshot is on file; a record
	// then errors at open time (logged + skipped below), as does an unsealed
	// body.
	var opener mailfauna.RecordOpener
	if o := sess.RecordOpener(); o != nil {
		opener = o
	}

	var sb strings.Builder
	sb.WriteString(`<?xml version="1.0" encoding="utf-8"?>` + "\n")
	sb.WriteString(`<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">` + "\n")

	for i := range res.Changed {
		entry := &res.Changed[i]
		href := userBasePath(sess) +
			hex.EncodeToString(calID) + "/" +
			hex.EncodeToString(entry.UIDHash) + ".ics"
		plaintext, decErr := mailfauna.OpenStoredRecord(opener, entry.EncryptedBody)
		if decErr != nil {
			m.logger.Warn(
				"caldav: sync-collection: skipping undecryptable changed event",
				"calendar_id_hex", hex.EncodeToString(calID),
				"uid_hash_hex", hex.EncodeToString(entry.UIDHash),
				"err", decErr,
			)
			continue
		}
		writeChangedResponse(&sb, href, entry.ETag, plaintext)
	}

	for i := range res.Expunged {
		entry := &res.Expunged[i]
		href := userBasePath(sess) +
			hex.EncodeToString(calID) + "/" +
			hex.EncodeToString(entry.UIDHash) + ".ics"
		writeExpungedResponse(&sb, href)
	}

	sb.WriteString("  <D:sync-token>")
	_ = xml.EscapeText(&strBuilderWriter{&sb}, []byte(res.NewSyncToken))
	sb.WriteString("</D:sync-token>\n")
	sb.WriteString("</D:multistatus>\n")

	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.WriteHeader(http.StatusMultiStatus)
	_, _ = io.WriteString(w, sb.String())
}

// writeChangedResponse emits one <D:response> for a changed event,
// carrying the wrapped ETag and the decrypted calendar-data inline.
// XML-escaping covers anything the iCalendar body or href happens to
// contain — DTSTART literals like 'T'/'Z' are XML-safe, but a SUMMARY
// containing '<' or '&' would corrupt the envelope without escaping.
func writeChangedResponse(sb *strings.Builder, href, etag string, plaintext []byte) {
	sb.WriteString("  <D:response>\n")
	sb.WriteString("    <D:href>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(href))
	sb.WriteString("</D:href>\n")
	sb.WriteString("    <D:propstat>\n")
	sb.WriteString("      <D:prop>\n")
	sb.WriteString("        <D:getetag>")
	// RFC 7232 §3.1 ETag form is DQUOTE-wrapped; we wrap here so the
	// MUA sees the same format as PROPFIND / calendar-query (emersion's
	// internal.ETag.String() does the same wrap for those paths).
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(`"`+etag+`"`))
	sb.WriteString("</D:getetag>\n")
	sb.WriteString("        <C:calendar-data>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, plaintext)
	sb.WriteString("</C:calendar-data>\n")
	sb.WriteString("      </D:prop>\n")
	sb.WriteString("      <D:status>HTTP/1.1 200 OK</D:status>\n")
	sb.WriteString("    </D:propstat>\n")
	sb.WriteString("  </D:response>\n")
}

// writeExpungedResponse emits one <D:response> for a tombstone. Per
// RFC 6578 §3.6 the status alone is sufficient — no propstat needed.
func writeExpungedResponse(sb *strings.Builder, href string) {
	sb.WriteString("  <D:response>\n")
	sb.WriteString("    <D:href>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(href))
	sb.WriteString("</D:href>\n")
	sb.WriteString("    <D:status>HTTP/1.1 404 Not Found</D:status>\n")
	sb.WriteString("  </D:response>\n")
}

// strBuilderWriter adapts strings.Builder to io.Writer for the XML
// escape helpers (xml.EscapeText needs an io.Writer).
type strBuilderWriter struct {
	sb *strings.Builder
}

func (w *strBuilderWriter) Write(p []byte) (int, error) {
	return w.sb.Write(p)
}

// syncPathError carries an HTTP status + message for path-parse
// failures inside the sync-collection interceptor. The standard
// path parser (parseCalendarID) wraps errors with emersion's
// internal HTTPError type which we cannot read .Code from outside
// the internal package — so the sync-collection path uses this
// local parser instead, preserving explicit status codes.
type syncPathError struct {
	status int
	msg    string
}

// parseCalendarIDForSync extracts the 32-byte calendar_id from a
// sync-collection request URL with explicit status-code semantics.
// Path scheme: `/caldav/{user@domain}/{segment}/`. Surfaces 403 for paths
// outside the AUTH'd actor's home set, 400 for a missing segment. The
// `{segment}` → id mapping is `resolveCalendarSegment` (hex decode, else
// blake3 of a client-chosen slug).
func parseCalendarIDForSync(sess *davauth.Session, urlPath string) ([]byte, *syncPathError) {
	base := userBasePath(sess)
	if !strings.HasPrefix(urlPath, base) {
		return nil, &syncPathError{
			status: http.StatusForbidden,
			msg:    fmt.Sprintf("path %q not under AUTH'd actor's home set", urlPath),
		}
	}
	rest := strings.TrimPrefix(urlPath, base)
	rest = strings.TrimSuffix(rest, "/")
	parts := strings.SplitN(rest, "/", 2)
	id, ok := resolveCalendarSegment(parts[0])
	if !ok {
		return nil, &syncPathError{
			status: http.StatusBadRequest,
			msg:    fmt.Sprintf("path %q missing calendar id", urlPath),
		}
	}
	return id, nil
}

// writeValidSyncToken emits the RFC 6578 §3.8 DAV:valid-sync-token
// precondition-failure response (HTTP 403), telling the MUA its sync-token
// is no longer usable so it must fall through to a full PROPFIND +
// per-event GET. Shared by the two nest signals that mean "this token can
// no longer be honored": the SyncCalendarSinceStale outcome (MUA ahead of
// the server, post-restore) and an Ok reply with Stale set (token behind
// the tombstone-retention window).
func writeValidSyncToken(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/xml; charset=utf-8")
	w.WriteHeader(http.StatusForbidden)
	_, _ = fmt.Fprint(w,
		`<?xml version="1.0" encoding="utf-8"?>`+"\n"+
			`<D:error xmlns:D="DAV:">`+"\n"+
			`  <D:valid-sync-token/>`+"\n"+
			`</D:error>`+"\n",
	)
}
