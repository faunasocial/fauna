package carddav

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

// DeleteAddressObject lands the CardDAV DELETE path (RFC 6352 §6.3.2).
//
// emersion/go-webdav v0.7.0's `carddav.Backend.DeleteAddressObject` signature
// does not receive an options struct, so we preserve conditional-DELETE
// semantics by reading `If-Match` in a ctx-stash middleware (ifMatchMiddleware)
// and popping it here via ifMatchFromContext. Twin of CalDAV's
// DeleteCalendarObject.
//
// Path scheme is `/carddav/{user}/{ab}/{uid_hash_hex}.vcf`; filename IS the
// uid_hash. Malformed paths surface as 400 before any RPC fires.
//
// Outcomes:
//   - Deleted → nil (emersion writes 204 No Content).
//   - NotFound → 404 (nest collapses missing-card and missing-book).
//   - PreconditionFailed → 412 with the current ETag in the body.
func (b *Backend) DeleteAddressObject(ctx context.Context, urlPath string) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	abID, uidHash, err := parseCardResourcePath(sess, urlPath)
	if err != nil {
		return err
	}
	ifMatch := ifMatchFromContext(ctx)

	result, err := wsrpc.DeleteCard(
		ctx, sess.Client(),
		sess.ActorID(), abID, uidHash, ifMatch,
	)
	if err != nil {
		return fmt.Errorf("delete_card: %w", err)
	}
	switch result.Outcome {
	case wsrpc.DeleteCardDeleted:
		return nil
	case wsrpc.DeleteCardNotFound:
		return webdav.NewHTTPError(http.StatusNotFound,
			fmt.Errorf("card %s not found in address book %s",
				hex.EncodeToString(uidHash), hex.EncodeToString(abID)))
	case wsrpc.DeleteCardPreconditionFailed:
		return webdav.NewHTTPError(http.StatusPreconditionFailed,
			fmt.Errorf("If-Match mismatch; current etag=%s", result.CurrentETag))
	default:
		return fmt.Errorf("delete_card: unexpected outcome %q", result.Outcome)
	}
}

// parseCardResourcePath extracts (addressbook_id, uid_hash) from a card
// resource URL. Path scheme:
//
//	/carddav/{user@domain}/{addressbook_id_hex}/{uid_hash_hex}.vcf
//
// Reuses parseAddressbookID for the book half + adds the filename parse. 400 on
// any malformed segment; 403 if the path strays outside the AUTH'd actor's home
// set (the address-book parser already enforces this). Twin of CalDAV's
// parseEventResourcePath.
func parseCardResourcePath(sess *davauth.Session, urlPath string) ([]byte, []byte, error) {
	abID, err := parseAddressbookID(sess, urlPath)
	if err != nil {
		return nil, nil, err
	}
	abBase := userBasePath(sess) + hex.EncodeToString(abID) + "/"
	if !strings.HasPrefix(urlPath, abBase) {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("path %q missing card filename", urlPath))
	}
	filename := strings.TrimPrefix(urlPath, abBase)
	filename = strings.TrimSuffix(filename, "/")
	if filename == "" {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("path %q is the address-book collection, not a card resource", urlPath))
	}
	slug := strings.TrimSuffix(filename, ".vcf")
	if slug == filename {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("card filename %q missing .vcf suffix", filename))
	}
	uidHash, err := hex.DecodeString(slug)
	if err != nil {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("card filename slug %q not hex: %w", slug, err))
	}
	if len(uidHash) != 32 {
		return nil, nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("card filename slug %q must decode to 32 bytes, got %d", slug, len(uidHash)))
	}
	return abID, uidHash, nil
}

// ── If-Match ctx-stash middleware ────────────────────────────────

// ifMatchCtxKey is the unexported context key the If-Match middleware uses to
// stash the header value for downstream handlers.
type ifMatchCtxKey struct{}

// ifMatchMiddleware reads `If-Match` from every incoming request and stashes
// the raw value in the request context. CardDAV DELETE needs the header but
// emersion's `carddav.Backend.DeleteAddressObject` signature does not plumb it
// through, so we route it via context. Wildcard ("*") is treated identically to
// "no If-Match"; empty/missing header → nil pointer in context. No-op for
// non-DELETE methods (PUT receives If-Match through opts.IfMatch). Twin of
// CalDAV's ifMatchMiddleware.
func ifMatchMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw := r.Header.Get("If-Match")
		var ifMatch *string
		if raw != "" && raw != "*" {
			// Strip the quoted form (`"value"`). RFC 7232 §3.1 says ETags are
			// always quoted on the wire; nest stores + compares the bare value.
			trimmed := strings.TrimPrefix(strings.TrimSuffix(raw, `"`), `"`)
			ifMatch = &trimmed
		}
		ctx := context.WithValue(r.Context(), ifMatchCtxKey{}, ifMatch)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// ifMatchFromContext returns the parsed `If-Match` value the middleware stashed
// in `ctx`. Returns nil when no header was sent or the value was the wildcard
// "*".
func ifMatchFromContext(ctx context.Context) *string {
	v, _ := ctx.Value(ifMatchCtxKey{}).(*string)
	return v
}
