package caldav

import (
	"context"
	"encoding/hex"
	"fmt"
	"net/http"
	"strings"

	"github.com/emersion/go-webdav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// DeleteCalendarObject lands the CalDAV DELETE path per `caldav-server.md`
// § Write surface row 102.
//
// emersion/go-webdav v0.7.0's `caldav.Backend.DeleteCalendarObject`
// signature does not receive an options struct — `server.go:714` just
// dispatches `b.Backend.DeleteCalendarObject(r.Context(), r.URL.Path)`
// with no `If-Match` plumbed in. We preserve the conditional-DELETE
// semantics goal doc requires by reading the `If-Match` header in a
// ctx-stash middleware (`ifMatchMiddleware` below) and popping it
// here via `ifMatchFromContext`. The middleware runs unconditionally;
// PUT ignores it (PUT receives `If-Match` through `opts.IfMatch`).
//
// Path scheme is `/caldav/{user}/{cal}/{uid_hash_hex}.ics`; filename
// IS the uid_hash. Malformed paths surface as 400 before any RPC
// fires.
//
// Outcomes:
//   - Deleted → nil (emersion writes 204 No Content).
//   - NotFound → 404 (per goal doc Decision 7, collapses missing-event
//     and missing-calendar).
//   - PreconditionFailed → 412 with the current ETag in the body.
func (b *Backend) DeleteCalendarObject(ctx context.Context, urlPath string) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	calID, uidHash, err := parseEventResourcePath(sess, urlPath)
	if err != nil {
		return err
	}
	ifMatch := ifMatchFromContext(ctx)

	// Read-before-delete for CANCEL-on-removal: capture the event being deleted
	// BEFORE the tombstone, so an organizer DELETE can withdraw it from its
	// attendees (caldav-server.md § Server-side auto-schedule). Best-effort
	// (nil on a snapshot-less session); only the ORGANIZER's DELETE cancels,
	// gated inside maybeFanOutCancelOnDelete.
	priorICS := b.fetchPriorEventICS(ctx, sess, calID, uidHash)

	result, err := wsrpc.DeleteEvent(
		ctx, sess.Client(),
		sess.ActorID(), calID, uidHash, ifMatch,
	)
	if err != nil {
		return fmt.Errorf("delete_event: %w", err)
	}
	switch result.Outcome {
	case wsrpc.DeleteEventDeleted:
		b.maybeFanOutCancelOnDelete(ctx, sess, priorICS, uidHash)
		return nil
	case wsrpc.DeleteEventNotFound:
		return webdav.NewHTTPError(http.StatusNotFound,
			fmt.Errorf("event %s not found in calendar %s",
				hex.EncodeToString(uidHash), hex.EncodeToString(calID)))
	case wsrpc.DeleteEventPreconditionFailed:
		return webdav.NewHTTPError(http.StatusPreconditionFailed,
			fmt.Errorf("If-Match mismatch; current etag=%s", result.CurrentETag))
	default:
		return fmt.Errorf("delete_event: unexpected outcome %q", result.Outcome)
	}
}

// parseEventResourcePath extracts (calendar_id, uid_hash) from an
// event resource URL. Path scheme:
//
//	/caldav/{user@domain}/{calendar_id_hex}/{uid_hash_hex}.ics
//
// Reuses `parseCalendarID` for the calendar half + adds the filename
// parse. 400 on any malformed segment; 403 if the path strays outside
// the AUTH'd actor's home set (the calendar parser already enforces
// this).
func parseEventResourcePath(sess *davauth.Session, urlPath string) ([]byte, []byte, error) {
	calID, err := parseCalendarID(sess, urlPath)
	if err != nil {
		return nil, nil, err
	}
	calBase := userBasePath(sess) + hex.EncodeToString(calID) + "/"
	if !strings.HasPrefix(urlPath, calBase) {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("path %q missing event filename", urlPath))
	}
	filename := strings.TrimPrefix(urlPath, calBase)
	filename = strings.TrimSuffix(filename, "/")
	if filename == "" {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("path %q is the calendar collection, not an event resource", urlPath))
	}
	slug := strings.TrimSuffix(filename, ".ics")
	if slug == filename {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("event filename %q missing .ics suffix", filename))
	}
	uidHash, err := hex.DecodeString(slug)
	if err != nil {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("event filename slug %q not hex: %w", slug, err))
	}
	if len(uidHash) != 32 {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("event filename slug %q must decode to 32 bytes, got %d", slug, len(uidHash)))
	}
	return calID, uidHash, nil
}

// ── If-Match ctx-stash middleware ────────────────────────────────

// ifMatchCtxKey is the unexported context key the If-Match middleware
// uses to stash the header value for downstream handlers.
type ifMatchCtxKey struct{}

// ifMatchMiddleware reads `If-Match` from every incoming request and
// stashes the raw value in the request context. CalDAV DELETE needs
// the header but emersion's `caldav.Backend.DeleteCalendarObject`
// signature does not plumb it through, so we route it via context.
//
// Wildcard ("*") is treated identically to "no If-Match"; nest's
// `delete_event` has no must-exist gate today (Decision 7 collapses
// NotFound). Empty / missing header → nil pointer in context.
//
// The middleware is a no-op for non-DELETE methods (PUT receives
// If-Match through `opts.IfMatch`); leaving the value in context for
// other methods is harmless.
func ifMatchMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw := r.Header.Get("If-Match")
		var ifMatch *string
		if raw != "" && raw != "*" {
			// Strip the quoted form (`"value"`). RFC 7232 §3.1 says
			// ETags are always quoted on the wire; nest stores +
			// compares the bare value. If the header is malformed
			// (unbalanced quotes) we still forward the raw bytes —
			// the comparison will simply miss + surface as
			// PreconditionFailed, which is the correct semantic.
			trimmed := strings.TrimPrefix(strings.TrimSuffix(raw, `"`), `"`)
			ifMatch = &trimmed
		}
		ctx := context.WithValue(r.Context(), ifMatchCtxKey{}, ifMatch)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// ifMatchFromContext returns the parsed `If-Match` value the
// middleware stashed in `ctx`. Returns nil when no header was sent
// or the value was the wildcard "*".
func ifMatchFromContext(ctx context.Context) *string {
	v, _ := ctx.Value(ifMatchCtxKey{}).(*string)
	return v
}
