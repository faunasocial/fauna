package carddav

// REPORT sync-collection (RFC 6578) is intercepted before emersion's
// carddav.Handler sees it: emersion v0.7.0's reportReq only recognizes
// {urn:ietf:params:xml:ns:carddav}addressbook-query and addressbook-multiget
// (carddav/elements.go reportReq.UnmarshalXML), and errors on any other REPORT
// root. The interceptor below peeks the REPORT body, routes
// {DAV:}sync-collection to nest's `sync_addressbook_since` RPC, and passes
// everything else through to the inner handler unchanged. Twin of the CalDAV
// terminator's sync_collection.go (minus the plaintext-storage arm — CardDAV is
// seal-always).
//
// Stale-token handling: nest returns a distinct
// `SyncAddressbookSinceReply::Ok { stale: true }` when the supplied token
// predates the tombstone-retention window — separate from the steady-state "no
// changes" reply (stale=false) and from the `Stale` outcome (MUA-ahead /
// restore). On either signal (`Ok{stale:true}` or the `Stale` outcome) the
// interceptor emits the RFC 6578 §3.8 `DAV:valid-sync-token` precondition
// failure so the MUA falls through to a full PROPFIND + per-card GET.

import (
	"bytes"
	"encoding/hex"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// nsCardDAV is the CardDAV XML namespace (RFC 6352); the <address-data> element
// in a sync-collection response lives in it.
const nsCardDAV = "urn:ietf:params:xml:ns:carddav"

// syncCollectionRoot is the XML element name (namespace + local) we match
// against incoming REPORT bodies to decide whether to intercept.
var syncCollectionRoot = xml.Name{Space: "DAV:", Local: "sync-collection"}

// newSyncCollectionInterceptor wraps `next` so that REPORT requests whose body
// is a `{DAV:}sync-collection` element are handled here instead of delegated.
// The middleware peeks the buffered body (w1Mitigations already drained +
// restored it). Non-REPORT requests + REPORT requests with other root elements
// (addressbook-query, addressbook-multiget) pass through unchanged.
func newSyncCollectionInterceptor(next http.Handler, logger *slog.Logger) http.Handler {
	if next == nil {
		panic("carddav: newSyncCollectionInterceptor: next must not be nil")
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
		http.Error(w, fmt.Sprintf("carddav: read REPORT body: %v", err), http.StatusBadRequest)
		return
	}
	// Restore the body for downstream handlers regardless of which branch we
	// take.
	r.Body = io.NopCloser(bytes.NewReader(body))
	r.ContentLength = int64(len(body))

	if !isSyncCollection(body) {
		m.next.ServeHTTP(w, r)
		return
	}
	m.handleSyncCollection(w, r, body)
}

// isSyncCollection scans XML tokens until the first StartElement and returns
// true if it matches {DAV:}sync-collection. Empty / malformed bodies return
// false — the downstream handler will produce an error.
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

// syncCollectionReq is the parsed RFC 6578 §3.2 request envelope. The Prop
// element is ignored — we always emit getetag + address-data.
type syncCollectionReq struct {
	XMLName   xml.Name `xml:"DAV: sync-collection"`
	SyncToken string   `xml:"DAV: sync-token"`
	// SyncLevel is informational. RFC 6578 §6.3 declares "1" or "infinity";
	// for an address-book collection (a flat collection of resources) both
	// behave identically, so we accept any value without further validation.
	SyncLevel string `xml:"DAV: sync-level"`
}

func (m *syncCollectionInterceptor) handleSyncCollection(w http.ResponseWriter, r *http.Request, body []byte) {
	sess := davauth.SessionFromContext(r.Context())
	if sess == nil {
		// Auth middleware should have short-circuited before reaching here;
		// defensive 401 if a future routing change drops auth.
		w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, carddavRealm))
		http.Error(w, davauth.ErrAuthFailed.Error(), http.StatusUnauthorized)
		return
	}
	abID, perr := parseAddressbookCollectionID(sess, r.URL.Path)
	if perr != nil {
		http.Error(w, perr.msg, perr.status)
		return
	}
	var req syncCollectionReq
	if err := xml.Unmarshal(body, &req); err != nil {
		http.Error(w, fmt.Sprintf("carddav: parse sync-collection: %v", err), http.StatusBadRequest)
		return
	}
	syncToken := strings.TrimSpace(req.SyncToken)
	if syncToken == "" {
		syncToken = "0"
	}
	muaID := r.Header.Get("User-Agent")

	ctx := r.Context()
	res, err := wsrpc.SyncAddressbookSince(
		ctx, sess.Client(),
		sess.ActorID(), abID,
		syncToken, 0,
		muaID,
	)
	if err != nil {
		http.Error(w, fmt.Sprintf("carddav: sync_addressbook_since: %v", err), http.StatusInternalServerError)
		return
	}
	switch res.Outcome {
	case wsrpc.SyncAddressbookSinceOk:
		if res.Stale {
			// RFC 6578 §3.8: the supplied sync-token is valid but predates the
			// tombstone-retention window, so nest cannot honestly enumerate the
			// deletions since then. Same client-facing remedy as the MUA-ahead
			// Stale outcome — DAV:valid-sync-token → full PROPFIND.
			writeValidSyncToken(w)
			return
		}
		// continue below.
	case wsrpc.SyncAddressbookSinceAddressbookNotFound:
		http.Error(w, "address book not found", http.StatusNotFound)
		return
	case wsrpc.SyncAddressbookSinceStale:
		// RFC 6578 §3.8: the server's sync-token is behind what the client holds
		// (post-DR-restore "MUA ahead" case). Return the DAV:valid-sync-token
		// precondition failure so the MUA falls through to a full PROPFIND.
		writeValidSyncToken(w)
		return
	default:
		http.Error(w, fmt.Sprintf("carddav: sync_addressbook_since: unexpected outcome %q", res.Outcome), http.StatusInternalServerError)
		return
	}

	// Per-session F2 opener; openMailRecordForSync stays STRICT (card
	// bodies are sealed in BOTH storage modes).
	opener := sess.RecordOpener()

	var sb strings.Builder
	sb.WriteString(`<?xml version="1.0" encoding="utf-8"?>` + "\n")
	sb.WriteString(`<D:multistatus xmlns:D="DAV:" xmlns:C="` + nsCardDAV + `">` + "\n")

	for i := range res.Changed {
		entry := &res.Changed[i]
		href := userBasePath(sess) +
			hex.EncodeToString(abID) + "/" +
			hex.EncodeToString(entry.UIDHash) + ".vcf"
		plaintext, decErr := openMailRecordForSync(opener, entry)
		if decErr != nil {
			m.logger.Warn(
				"carddav: sync-collection: skipping undecryptable changed card",
				"addressbook_id_hex", hex.EncodeToString(abID),
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
			hex.EncodeToString(abID) + "/" +
			hex.EncodeToString(entry.UIDHash) + ".vcf"
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

// openMailRecordForSync is the same primitive openCard uses — extracted so the
// sync REPORT path routes through one place. SEAL-ALWAYS + STRICT: card bodies
// rest sealed, so opens go through the per-session F2 opener directly — a
// shape-corrupt blob must error, never pass through. A nil opener (no MLS snapshot on file) surfaces as a
// per-card error so the caller can log + skip.
func openMailRecordForSync(opener *mailfauna.MailRecordOpener, entry *wsrpc.CardEntry) ([]byte, error) {
	if opener == nil {
		return nil, errors.New("session missing MLS snapshot")
	}
	return opener.Open(entry.EncryptedBody)
}

// writeChangedResponse emits one <D:response> for a changed card, carrying the
// wrapped ETag and the decrypted address-data inline. XML-escaping covers
// anything the vCard body or href happens to contain.
func writeChangedResponse(sb *strings.Builder, href, etag string, plaintext []byte) {
	sb.WriteString("  <D:response>\n")
	sb.WriteString("    <D:href>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(href))
	sb.WriteString("</D:href>\n")
	sb.WriteString("    <D:propstat>\n")
	sb.WriteString("      <D:prop>\n")
	sb.WriteString("        <D:getetag>")
	// RFC 7232 §3.1 ETag form is DQUOTE-wrapped; we wrap here so the MUA sees
	// the same format as PROPFIND / addressbook-query.
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(`"`+etag+`"`))
	sb.WriteString("</D:getetag>\n")
	sb.WriteString("        <C:address-data>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, plaintext)
	sb.WriteString("</C:address-data>\n")
	sb.WriteString("      </D:prop>\n")
	sb.WriteString("      <D:status>HTTP/1.1 200 OK</D:status>\n")
	sb.WriteString("    </D:propstat>\n")
	sb.WriteString("  </D:response>\n")
}

// writeExpungedResponse emits one <D:response> for a tombstone. Per RFC 6578
// §3.6 the status alone is sufficient — no propstat needed.
func writeExpungedResponse(sb *strings.Builder, href string) {
	sb.WriteString("  <D:response>\n")
	sb.WriteString("    <D:href>")
	_ = xml.EscapeText(&strBuilderWriter{sb}, []byte(href))
	sb.WriteString("</D:href>\n")
	sb.WriteString("    <D:status>HTTP/1.1 404 Not Found</D:status>\n")
	sb.WriteString("  </D:response>\n")
}

// strBuilderWriter adapts strings.Builder to io.Writer for the XML escape
// helpers (xml.EscapeText needs an io.Writer).
type strBuilderWriter struct {
	sb *strings.Builder
}

func (w *strBuilderWriter) Write(p []byte) (int, error) {
	return w.sb.Write(p)
}

// syncPathError carries an HTTP status + message for path-parse failures inside
// the sync-collection interceptor (parseAddressbookID wraps errors with
// emersion's internal HTTPError type whose .Code is unreadable outside that
// package — so the sync path uses this local parser, preserving explicit status
// codes).
type syncPathError struct {
	status int
	msg    string
}

// parseAddressbookCollectionID extracts the 32-byte addressbook_id from a
// collection request URL with explicit status-code semantics. Path scheme:
// `/carddav/{user@domain}/{segment}/`. 403 for paths outside the AUTH'd actor's
// home set, 400 for a missing segment. Shared by the sync-collection interceptor
// and the PROPPATCH interceptor (props.go). CardDAV keeps a single collection
// parser here where CalDAV duplicates parseCalendarIDForSync + the props-local
// parseCalendarCollectionID.
func parseAddressbookCollectionID(sess *davauth.Session, urlPath string) ([]byte, *syncPathError) {
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
	id, ok := resolveAddressbookSegment(parts[0])
	if !ok {
		return nil, &syncPathError{
			status: http.StatusBadRequest,
			msg:    fmt.Sprintf("path %q missing address-book id", urlPath),
		}
	}
	return id, nil
}

// writeValidSyncToken emits the RFC 6578 §3.8 DAV:valid-sync-token
// precondition-failure response (HTTP 403), telling the MUA its sync-token is
// no longer usable so it must fall through to a full PROPFIND + per-card GET.
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
