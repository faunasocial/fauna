package caldav

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"sync/atomic"

	"github.com/emersion/go-ical"
	"github.com/emersion/go-webdav"
	"github.com/emersion/go-webdav/caldav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/undecryptable"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// maxResourceSizeBytes is the 16 MiB per-event-body cap surfaced as
// the `max-resource-size` DAV property and enforced on PUT
// (Phase E.3). Mirrors `caldav-server.md` § iCalendar parsing rules.
const maxResourceSizeBytes = 16 << 20

// defaultColor is the lazy-Personal calendar color per goal doc
// § Lazy "Personal" calendar.
const defaultColor = "#3273dc"

// defaultDisplayname is the lazy-Personal calendar name per goal doc.
const defaultDisplayname = "Personal"

// errAuthRequired is returned when a backend method runs without a
// Session in context. The auth middleware short-circuits 401 before
// reaching here, so this is defensive against future routing
// changes that might skip auth (e.g. a public `/.well-known/caldav`
// path mounted under the same handler).
var errAuthRequired = webdav.NewHTTPError(http.StatusUnauthorized, errors.New("caldav: authentication required"))

// personalCalendarID returns the canonical 32-byte calendar
// identifier for the lazy-created "Personal" calendar. Derivation:
// `blake3("personal")[:32]` per `caldav-server.md` § Lazy
// "Personal" calendar. Returns a fresh slice so callers cannot
// mutate the cached value.
func personalCalendarID() []byte {
	sum := blake3.Sum256([]byte("personal"))
	out := make([]byte, 32)
	copy(out, sum[:])
	return out
}

// Backend is the production CalDAV backend. One instance per MDA
// process; per-request state rides in the request context via
// davauth.SessionFromContext. The backend dispatches each call to the
// AUTH'd actor's WS-RPC caller, decrypting any nest-returned
// blobs in-session.
type Backend struct {
	logger *slog.Logger
	// localDomains is the box's hosted-domain set (lowercased), shared with the
	// Server and hot-swapped via Server.ApplyConfig. The auto-schedule classifier
	// Loads it per organizer PUT to decide which attendees are local-domain
	// candidates for the mailbox-less sealed rail. nil ⇒ no domain is local (the
	// pre-mailbox-less behavior: every attendee rides the email rail).
	localDomains *atomic.Pointer[[]string]
	// mailEnabled mirrors the deployment's `ConfigSnapshot.MailEnabled` toggle,
	// shared with the Server and hot-swapped via Server.ApplyConfig. The
	// auto-schedule classifier reads it to decide whether a local attendee's
	// canonical `<handle>@<domain>` alias means email-reachable: on an
	// email-ENABLED nest it does (→ email rail), but on an email-DISABLED nest
	// (a CalDAV-only deployment) every CalDAV-enabled actor still holds that
	// canonical alias *for AUTH* (`ensure_canonical_handle_alias` fires on the
	// recipient-pubkey provision regardless of email), yet no MTA/INBOX delivers
	// to it — so a local Fauna attendee must ride the sealed scheduling rail
	// instead (caldav-server.md § Server-side auto-schedule — email-disabled nest
	// → WS-RPC sealed delivery). nil ⇒ treated as email-enabled (the established
	// default, so a Backend built without this wired keeps today's behavior).
	mailEnabled *atomic.Bool
	// classifyTransport, when non-nil, replaces the production cross-nest discovery
	// (mailfauna.ClassifyAttendeeTransport) the off-box-domain branch of the
	// auto-schedule classifier calls. Injectable so the Go unit twin can exercise
	// that branch without the cgo anon-discovery network call (which needs a live
	// peer nest). nil ⇒ the production resolver (see classifyOffBox).
	classifyTransport func(addr string) (mailfauna.AttendeeTransport, error)
	// undecryptableWarn collapses the per-PROPFIND "undecryptable metadata"
	// WARN to once per calendar. Zero value ready for use.
	undecryptableWarn undecryptable.WarnDedup
}

// nestMailEnabled reports whether the deployment has email enabled, defaulting to
// true when unset so a Backend constructed without the flag wired (older callers,
// some unit tests) keeps the pre-email-disabled-nest classification behavior.
func (b *Backend) nestMailEnabled() bool {
	if b.mailEnabled == nil {
		return true
	}
	return b.mailEnabled.Load()
}

// NewBackend returns the production CalDAV backend. `localDomains` is the
// hot-swappable hosted-domain set the auto-schedule classifier reads; a nil
// pointer disables mailbox-less classification (every attendee → email rail).
func NewBackend(logger *slog.Logger, localDomains *atomic.Pointer[[]string]) *Backend {
	if logger == nil {
		logger = slog.Default()
	}
	return &Backend{logger: logger, localDomains: localDomains}
}

// normalizeDomains lowercases + trims each domain and drops empties, so the
// classifier's membership test is a case-insensitive exact match (mail domains
// are ASCII-case-insensitive). Returns a fresh slice (never shares cfg's).
func normalizeDomains(in []string) []string {
	out := make([]string, 0, len(in))
	for _, d := range in {
		if d = strings.ToLower(strings.TrimSpace(d)); d != "" {
			out = append(out, d)
		}
	}
	return out
}

// isLocalDomain reports whether `domain` is one of the box's hosted domains —
// the locality pre-filter for mailbox-less auto-schedule classification. Case-
// insensitive (the set is stored lowercased). False when the set is unset/empty,
// which keeps a deployment with no configured domains on the email-only path.
func (b *Backend) isLocalDomain(domain string) bool {
	if b.localDomains == nil {
		return false
	}
	set := b.localDomains.Load()
	if set == nil {
		return false
	}
	domain = strings.ToLower(strings.TrimSpace(domain))
	for _, d := range *set {
		if d == domain {
			return true
		}
	}
	return false
}

// session recovers the per-request session attached by the auth
// middleware. Returns an HTTPError(401) when missing.
func (b *Backend) session(ctx context.Context) (*davauth.Session, error) {
	sess := davauth.SessionFromContext(ctx)
	if sess == nil {
		return nil, errAuthRequired
	}
	return sess, nil
}

// userBasePath is the `/caldav/{local}@{domain}/` prefix the MDA
// uses for the AUTH'd actor's collection home set. The local +
// domain come from the Basic-Auth username; the AUTH'd actor_id is
// the only authority — the URL is informational.
func userBasePath(sess *davauth.Session) string {
	return "/caldav/" + sess.AuthedLocalPart() + "@" + sess.AuthedDomain() + "/"
}

// calendarPath returns `/caldav/{user}/{calendar_id_hex}/`.
func calendarPath(sess *davauth.Session, calendarID []byte) string {
	return userBasePath(sess) + hex.EncodeToString(calendarID) + "/"
}

// ── caldav.Backend interface ─────────────────────────────────────

// CurrentUserPrincipal returns the principal URL for the AUTH'd
// actor: a SINGLE-segment path `/{user@domain}/`. The path is what a
// MUA stores in its account config, the target of the
// `/.well-known/caldav` redirect, and the resource a real CalDAV
// client (macOS Calendar.app) PROPFINDs for `calendar-home-set`
// during RFC-6764/RFC-5397 discovery.
//
// ⚠ The single-segment shape is load-bearing, not cosmetic.
// emersion/go-webdav routes resources by path-segment DEPTH
// (`caldav.backend.resourceTypeAtPath`: depth-1 → user-principal,
// depth-2 → calendar-home-set, …). A 2-segment principal like
// `/principals/{u}@{d}/` lands at the same depth as the home set
// (`/caldav/{u}@{d}/`, see CalendarHomeSetPath), so emersion misroutes
// a PROPFIND of it into the calendar-home-set branch, whose
// `r.URL.Path == homeSetPath` guard then fails and it serves an EMPTY
// multistatus — the macOS Calendar "Connecting…" stall (the Gap-1c
// discovery bug). At a single segment emersion's own
// `propFindUserPrincipal` fires and serves `calendar-home-set`.
// The goal doc pins only the `/caldav/{user}/…` tree, not the
// principal path; see `caldav-server.md` § Authentication (discovery).
func (b *Backend) CurrentUserPrincipal(ctx context.Context) (string, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return "", err
	}
	// ONE source of truth for the principal path: the scheduling interceptor
	// (schedule.go) matches PROPFINDs against the same `principalPath(sess)`.
	return principalPath(sess), nil
}

// CalendarHomeSetPath returns `/caldav/{user@domain}/` — the
// collection root under which calendar collections live.
func (b *Backend) CalendarHomeSetPath(ctx context.Context) (string, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return "", err
	}
	return userBasePath(sess), nil
}

// ListCalendars fetches every provisioned calendar for the AUTH'd
// actor. On empty list (the new-user path) the backend
// auto-provisions the lazy "Personal" calendar per
// `caldav-server.md` § Lazy "Personal" calendar and re-fetches so
// the MUA's first PROPFIND lands on a working surface.
func (b *Backend) ListCalendars(ctx context.Context) ([]caldav.Calendar, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	entries, err := wsrpc.ListCalendars(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, fmt.Errorf("caldav: list_calendars: %w", err)
	}
	if len(entries) == 0 {
		entries, err = b.lazyProvisionPersonal(ctx, sess)
		if err != nil {
			return nil, err
		}
	}
	cals := make([]caldav.Calendar, 0, len(entries))
	for i := range entries {
		cal, err := b.entryToCalendar(sess, &entries[i])
		if err != nil {
			// A single decrypt failure shouldn't drop the entire
			// home set — log + skip. The user can re-PUT the
			// metadata via PROPPATCH (Phase E.4) to heal the row.
			//
			// WARN once per calendar, DEBUG on the repeats: this fires on
			// every PROPFIND for as long as the row stays unopenable, which
			// is indefinitely (see undecryptable.WarnDedup).
			idHex := hex.EncodeToString(entries[i].CalendarID)
			b.undecryptableWarn.Log(
				b.logger, idHex,
				"caldav: skipping calendar with undecryptable metadata",
				"calendar_id_hex", idHex,
				"err", err,
			)
			continue
		}
		cals = append(cals, cal)
	}
	return cals, nil
}

// lazyProvisionPersonal seals the default Personal-calendar
// metadata to the AUTH'd actor and provisions the row via
// `wsrpc.ProvisionCalendar`. Re-fetches `list_calendars` so the
// caller sees the freshly-created entry; this is the trade-off
// between "one extra RPC on the new-user path" and "synthesizing
// a CalendarEntry from the seal we just shipped" — the round-trip
// is the truthful source of `ctag`/`highestmodseq`/`created_at`.
func (b *Backend) lazyProvisionPersonal(ctx context.Context, sess *davauth.Session) ([]wsrpc.CalendarEntry, error) {
	metadata := EncryptedCollectionMetadata{
		Displayname: defaultDisplayname,
		Color:       defaultColor,
	}
	sealed, err := SealCollectionMetadata(metadata, sess.MLSPubkey(), sess.MlkemEk())
	if err != nil {
		return nil, fmt.Errorf("caldav: seal personal metadata: %w", err)
	}
	outcome, err := wsrpc.ProvisionCalendar(
		ctx, sess.Client(),
		sess.ActorID(), personalCalendarID(), sealed,
		false, // MKCOL/insert path; PROPPATCH (`update_metadata=true`) is handled by props.go
	)
	if err != nil {
		return nil, fmt.Errorf("caldav: provision personal calendar: %w", err)
	}
	switch outcome {
	case wsrpc.ProvisionCalendarCreated, wsrpc.ProvisionCalendarAlreadyExists:
		// expected outcomes — fall through and re-fetch.
	case wsrpc.ProvisionCalendarConflict:
		// Different metadata bytes already exist for the personal
		// calendar_id — a prior session sealed it with a different
		// pubkey or HPKE freshness produced a different ciphertext
		// on a re-derive. Either way the actor already has the row;
		// continue and re-fetch. The existing row wins.
		b.logger.Info("caldav: personal calendar provision returned conflict; treating as already-provisioned")
	default:
		return nil, fmt.Errorf("caldav: provision personal calendar: unexpected outcome %q", outcome)
	}
	entries, err := wsrpc.ListCalendars(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, fmt.Errorf("caldav: list_calendars after provision: %w", err)
	}
	return entries, nil
}

// entryToCalendar decrypts a single CalendarEntry's metadata and
// maps it into the emersion/go-webdav caldav.Calendar shape.
func (b *Backend) entryToCalendar(sess *davauth.Session, e *wsrpc.CalendarEntry) (caldav.Calendar, error) {
	meta, err := UnsealCollectionMetadata(e.EncryptedMetadata, sess.RecordOpener())
	if err != nil {
		return caldav.Calendar{}, err
	}
	return caldav.Calendar{
		Path:                  calendarPath(sess, e.CalendarID),
		Name:                  meta.Displayname,
		Description:           meta.Description,
		MaxResourceSize:       maxResourceSizeBytes,
		SupportedComponentSet: []string{ical.CompEvent, ical.CompToDo},
	}, nil
}

// GetCalendar returns the metadata for a single calendar by URL
// path. emersion/go-webdav uses this on PROPFIND of a calendar
// resource itself.
func (b *Backend) GetCalendar(ctx context.Context, urlPath string) (*caldav.Calendar, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	calID, err := parseCalendarID(sess, urlPath)
	if err != nil {
		return nil, err
	}
	entries, err := wsrpc.ListCalendars(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, fmt.Errorf("caldav: list_calendars: %w", err)
	}
	for i := range entries {
		if bytes.Equal(entries[i].CalendarID, calID) {
			cal, err := b.entryToCalendar(sess, &entries[i])
			if err != nil {
				return nil, err
			}
			return &cal, nil
		}
	}
	return nil, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("calendar %q not found", urlPath))
}

// CreateCalendar wires MKCOL on `/caldav/{user}/{calendar_id_hex}/`
// to `provision_calendar(update_metadata=false)`. Phase E.2 ships
// the MKCOL path because emersion/go-webdav routes Mkcol through
// the Backend; without it MKCOL would 500. Re-MKCOL on an existing
// id maps to `AlreadyExists` → RFC 4918 §9.3 405 Method Not Allowed.
func (b *Backend) CreateCalendar(ctx context.Context, cal *caldav.Calendar) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	calID, err := parseCalendarID(sess, cal.Path)
	if err != nil {
		return err
	}
	metadata := EncryptedCollectionMetadata{
		Displayname: cal.Name,
		Color:       defaultColor,
		Description: cal.Description,
	}
	sealed, err := SealCollectionMetadata(metadata, sess.MLSPubkey(), sess.MlkemEk())
	if err != nil {
		return fmt.Errorf("caldav: seal new calendar metadata: %w", err)
	}
	outcome, err := wsrpc.ProvisionCalendar(
		ctx, sess.Client(),
		sess.ActorID(), calID, sealed,
		false, // MKCOL/insert path; PROPPATCH (`update_metadata=true`) is handled by props.go
	)
	if err != nil {
		return fmt.Errorf("caldav: provision_calendar: %w", err)
	}
	switch outcome {
	case wsrpc.ProvisionCalendarCreated:
		return nil
	case wsrpc.ProvisionCalendarAlreadyExists, wsrpc.ProvisionCalendarConflict:
		// RFC 4918 §9.3 (MKCOL on a Request-URI where a collection already exists
		// → 405), uniform with the MKCALENDAR interceptor. The nest's AlreadyExists
		// vs Conflict split is unreachable from a real client (SealCollectionMetadata
		// HPKE-re-seals non-deterministically → a re-create's bytes never match →
		// always Conflict), and a create verb maps "already exists" to 405 whatever
		// the metadata; a metadata *edit* is PROPPATCH, not MKCOL.
		return webdav.NewHTTPError(http.StatusMethodNotAllowed, fmt.Errorf("calendar %s already exists", hex.EncodeToString(calID)))
	default:
		return fmt.Errorf("caldav: provision_calendar: unexpected outcome %q", outcome)
	}
}

// resolveCalendarSegment maps a CalDAV URL path segment to the canonical
// 32-byte calendar_id. A 64-hex segment decodes directly — that is the URL
// shape a Fauna app (and the lazy "Personal" calendar) uses, where the
// segment IS hex(calendar_id). Any other non-empty segment is the opaque
// collection name a stock CalDAV client (macOS Calendar.app) picks for a
// MKCALENDAR it issues; it maps deterministically to
// `calendar_id = blake3(segment)[:32]`, so the nest keeps fixed-width 32-byte
// ids while the client chooses its own URL. Deterministic ⇒ every later
// PROPFIND/PUT/REPORT/PROPPATCH/sync on the same URL resolves to the same id.
// Returns (id, true) for any non-empty segment; (nil, false) for an empty one.
func resolveCalendarSegment(segment string) ([]byte, bool) {
	if segment == "" {
		return nil, false
	}
	if len(segment) == 64 {
		if id, err := hex.DecodeString(segment); err == nil {
			return id, true
		}
	}
	sum := blake3.Sum256([]byte(segment))
	id := make([]byte, 32)
	copy(id, sum[:])
	return id, true
}

// parseCalendarID extracts the 32-byte calendar_id from a URL path.
// Accepts `/caldav/{user}/{segment}/` and the resource form
// `/caldav/{user}/{segment}/<event>.ics`, with or without a trailing slash;
// rejects anything outside the AUTH'd actor's home set (defense in depth — the
// auth middleware already scoped the actor). The `{segment}` → id mapping is
// `resolveCalendarSegment` (hex decode, else blake3).
func parseCalendarID(sess *davauth.Session, urlPath string) ([]byte, error) {
	base := userBasePath(sess)
	if !strings.HasPrefix(urlPath, base) {
		return nil, webdav.NewHTTPError(http.StatusForbidden, fmt.Errorf("path %q not under AUTH'd actor's home set", urlPath))
	}
	rest := strings.TrimPrefix(urlPath, base)
	rest = strings.TrimSuffix(rest, "/")
	// rest may be just the segment (collection itself) or
	// "<segment>/<event>.ics" (resource); split at the first '/'.
	parts := strings.SplitN(rest, "/", 2)
	id, ok := resolveCalendarSegment(parts[0])
	if !ok {
		return nil, webdav.NewHTTPError(http.StatusBadRequest, fmt.Errorf("path %q missing calendar id", urlPath))
	}
	return id, nil
}

// caldav.Backend method routing:
//
//   - GetCalendarObject / QueryCalendarObjects / ListCalendarObjects
//     live in report.go (E.3 Steps 5–6, REPORT calendar-query +
//     calendar-multiget + PROPFIND depth=1).
//   - PutCalendarObject lives in put.go (E.3 Step 3).
//   - DeleteCalendarObject lives in delete.go (E.3 Step 4).
//
// sync-collection REPORT is handled by a separate middleware
// (sync_collection.go) wrapping the inner caldav.Handler — emersion's
// reportReq parser does not recognize {DAV:}sync-collection, so the
// middleware peeks the body and routes it directly to nest's
// `sync_calendar_since` RPC.

// ─── compile-time interface satisfaction check ────────────────────

var _ caldav.Backend = (*Backend)(nil)
