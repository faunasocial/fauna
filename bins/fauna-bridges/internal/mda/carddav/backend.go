package carddav

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"

	"github.com/emersion/go-webdav"
	"github.com/emersion/go-webdav/carddav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// maxResourceSizeBytes is the 16 MiB per-card-body cap surfaced as the
// `max-resource-size` DAV property and enforced on PUT. Mirrors the CalDAV
// terminator's cap.
const maxResourceSizeBytes = 16 << 20

// defaultDisplayname is the lazy-Contacts address-book name (twin of CalDAV's
// lazy "Personal" calendar).
const defaultDisplayname = "Contacts"

// errAuthRequired is returned when a backend method runs without a Session in
// context. The auth middleware short-circuits 401 before reaching here, so this
// is defensive against future routing changes that might skip auth.
var errAuthRequired = webdav.NewHTTPError(http.StatusUnauthorized, errors.New("carddav: authentication required"))

// contactsAddressbookID returns the canonical 32-byte address-book identifier
// for the lazy-created "Contacts" book. Derivation: `blake3("contacts")[:32]`
// (twin of CalDAV's `personalCalendarID`). Returns a fresh slice so callers
// cannot mutate the cached value.
func contactsAddressbookID() []byte {
	sum := blake3.Sum256([]byte("contacts"))
	out := make([]byte, 32)
	copy(out, sum[:])
	return out
}

// Backend is the production CardDAV backend. One instance per MDA process;
// per-request state rides in the request context via
// davauth.SessionFromContext. The backend dispatches each call to the AUTH'd
// actor's WS-RPC caller, decrypting any nest-returned blobs in-session.
//
// SEAL-ALWAYS: unlike the CalDAV Backend there is no plaintextStorage /
// nestPlaintext branch — every write seals, every read opens, in both modes.
type Backend struct {
	logger *slog.Logger
	// undecryptableWarn collapses the per-PROPFIND "undecryptable metadata"
	// WARN to once per address book. Zero value ready; see the CalDAV twin.
	undecryptableWarn dav.UndecryptableWarnDedup
}

// NewBackend returns the production CardDAV backend. Passing a nil logger falls
// back to slog.Default.
func NewBackend(logger *slog.Logger) *Backend {
	if logger == nil {
		logger = slog.Default()
	}
	return &Backend{logger: logger}
}

// session recovers the per-request session attached by the auth middleware.
// Returns an HTTPError(401) when missing.
func (b *Backend) session(ctx context.Context) (*davauth.Session, error) {
	sess := davauth.SessionFromContext(ctx)
	if sess == nil {
		return nil, errAuthRequired
	}
	return sess, nil
}

// userBasePath is the `/carddav/{local}@{domain}/` prefix the MDA uses for the
// AUTH'd actor's address-book home set. The local + domain come from the
// Basic-Auth username; the AUTH'd actor_id is the only authority — the URL is
// informational. Twin of CalDAV's userBasePath.
func userBasePath(sess *davauth.Session) string {
	return "/carddav/" + sess.AuthedLocalPart() + "@" + sess.AuthedDomain() + "/"
}

// addressbookPath returns `/carddav/{user}/{addressbook_id_hex}/`.
func addressbookPath(sess *davauth.Session, addressbookID []byte) string {
	return userBasePath(sess) + hex.EncodeToString(addressbookID) + "/"
}

// principalPath returns the AUTH'd actor's principal URL — the single-segment
// `/{local}@{domain}/` path. Relocated from the CalDAV terminator's SKIPPED
// schedule.go so the CardDAV backend keeps ONE source of truth for the
// principal shape (CurrentUserPrincipal delegates here).
//
// ⚠ The single-segment shape is load-bearing, not cosmetic. emersion/go-webdav
// routes resources by path-segment DEPTH (carddav.backend.resourceTypeAtPath:
// depth-1 → user-principal, depth-2 → addressbook-home-set, …). A 2-segment
// principal like `/principals/{u}@{d}/` lands at the same depth as the home set
// (`/carddav/{u}@{d}/`, see AddressBookHomeSetPath), so emersion misroutes a
// PROPFIND of it into the home-set branch, whose `r.URL.Path == homeSetPath`
// guard then fails and it serves an EMPTY multistatus (the macOS-Contacts
// discovery stall the CalDAV Gap-1c fix addressed). At a single segment
// emersion's own propFindUserPrincipal fires and serves addressbook-home-set.
func principalPath(sess *davauth.Session) string {
	return "/" + sess.AuthedLocalPart() + "@" + sess.AuthedDomain() + "/"
}

// ── carddav.Backend interface ────────────────────────────────────

// CurrentUserPrincipal returns the principal URL for the AUTH'd actor: a
// SINGLE-segment path `/{user@domain}/`. The path is what a MUA stores in its
// account config, the target of the `/.well-known/carddav` redirect, and the
// resource a real CardDAV client PROPFINDs for `addressbook-home-set` during
// RFC-6764 discovery.
func (b *Backend) CurrentUserPrincipal(ctx context.Context) (string, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return "", err
	}
	return principalPath(sess), nil
}

// AddressBookHomeSetPath returns `/carddav/{user@domain}/` — the collection
// root under which address-book collections live. Twin of CalDAV's
// CalendarHomeSetPath.
func (b *Backend) AddressBookHomeSetPath(ctx context.Context) (string, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return "", err
	}
	return userBasePath(sess), nil
}

// ListAddressBooks fetches every provisioned address book for the AUTH'd actor.
// On empty list (the new-user path) the backend auto-provisions the lazy
// "Contacts" book and re-fetches so the MUA's first PROPFIND lands on a working
// surface. Twin of CalDAV's ListCalendars + lazy-Personal.
func (b *Backend) ListAddressBooks(ctx context.Context) ([]carddav.AddressBook, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	entries, err := wsrpc.ListAddressbooks(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, fmt.Errorf("carddav: list_addressbooks: %w", err)
	}
	if len(entries) == 0 {
		entries, err = b.lazyProvisionContacts(ctx, sess)
		if err != nil {
			return nil, err
		}
	}
	books := make([]carddav.AddressBook, 0, len(entries))
	for i := range entries {
		ab, err := b.entryToAddressBook(sess, &entries[i])
		if err != nil {
			// A single decrypt failure shouldn't drop the entire home set —
			// log + skip. The user can re-seal the metadata to heal the row.
			//
			// WARN once per address book, DEBUG on the repeats — the CalDAV
			// twin's dedup, shared so the two cannot drift.
			idHex := hex.EncodeToString(entries[i].AddressbookID)
			b.undecryptableWarn.Log(
				b.logger, idHex,
				"carddav: skipping address book with undecryptable metadata",
				"addressbook_id_hex", idHex,
				"err", err,
			)
			continue
		}
		books = append(books, ab)
	}
	return books, nil
}

// lazyProvisionContacts seals the default Contacts-book metadata to the AUTH'd
// actor and provisions the row via `wsrpc.ProvisionAddressbook`. Re-fetches
// `list_addressbooks` so the caller sees the freshly-created entry; this is the
// trade-off between "one extra RPC on the new-user path" and "synthesizing an
// AddressbookEntry from the seal we just shipped" — the round-trip is the
// truthful source of `ctag`/`highestmodseq`/`created_at`. Twin of CalDAV's
// lazyProvisionPersonal.
func (b *Backend) lazyProvisionContacts(ctx context.Context, sess *davauth.Session) ([]wsrpc.AddressbookEntry, error) {
	metadata := EncryptedCollectionMetadata{
		Displayname: defaultDisplayname,
	}
	sealed, err := SealCollectionMetadata(metadata, sess.MLSPubkey(), sess.MlkemEk())
	if err != nil {
		return nil, fmt.Errorf("carddav: seal contacts metadata: %w", err)
	}
	outcome, err := wsrpc.ProvisionAddressbook(
		ctx, sess.Client(),
		sess.ActorID(), contactsAddressbookID(), sealed,
		false, // MKCOL/insert path; a metadata rename is update_metadata=true, handled by props.go.
	)
	if err != nil {
		return nil, fmt.Errorf("carddav: provision contacts address book: %w", err)
	}
	switch outcome {
	case wsrpc.ProvisionAddressbookCreated, wsrpc.ProvisionAddressbookAlreadyExists:
		// expected outcomes — fall through and re-fetch.
	case wsrpc.ProvisionAddressbookConflict:
		// Different metadata bytes already exist for the contacts addressbook_id
		// — a prior session sealed it with a different pubkey or HPKE freshness
		// produced different ciphertext on a re-derive. Either way the actor
		// already has the row; continue and re-fetch. The existing row wins.
		b.logger.Info("carddav: contacts address book provision returned conflict; treating as already-provisioned")
	default:
		return nil, fmt.Errorf("carddav: provision contacts address book: unexpected outcome %q", outcome)
	}
	entries, err := wsrpc.ListAddressbooks(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, fmt.Errorf("carddav: list_addressbooks after provision: %w", err)
	}
	return entries, nil
}

// entryToAddressBook decrypts a single AddressbookEntry's metadata and maps it
// into the emersion/go-webdav carddav.AddressBook shape.
func (b *Backend) entryToAddressBook(sess *davauth.Session, e *wsrpc.AddressbookEntry) (carddav.AddressBook, error) {
	meta, err := UnsealCollectionMetadata(e.EncryptedMetadata, sess.RecordOpener())
	if err != nil {
		return carddav.AddressBook{}, err
	}
	return carddav.AddressBook{
		Path:            addressbookPath(sess, e.AddressbookID),
		Name:            meta.Displayname,
		Description:     meta.Description,
		MaxResourceSize: maxResourceSizeBytes,
	}, nil
}

// GetAddressBook returns the metadata for a single address book by URL path.
// emersion/go-webdav uses this on PROPFIND of an address-book resource itself.
func (b *Backend) GetAddressBook(ctx context.Context, urlPath string) (*carddav.AddressBook, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	abID, err := parseAddressbookID(sess, urlPath)
	if err != nil {
		return nil, err
	}
	entries, err := wsrpc.ListAddressbooks(ctx, sess.Client(), sess.ActorID())
	if err != nil {
		return nil, fmt.Errorf("carddav: list_addressbooks: %w", err)
	}
	for i := range entries {
		if bytes.Equal(entries[i].AddressbookID, abID) {
			ab, err := b.entryToAddressBook(sess, &entries[i])
			if err != nil {
				return nil, err
			}
			return &ab, nil
		}
	}
	return nil, webdav.NewHTTPError(http.StatusNotFound, fmt.Errorf("address book %q not found", urlPath))
}

// CreateAddressBook wires extended-MKCOL on
// `/carddav/{user}/{addressbook_id_hex}/` to
// `provision_addressbook(update_metadata=false)`. emersion/go-webdav's carddav
// Mkcol routes extended-MKCOL through the Backend (there is no separate
// MKADDRESSBOOK verb — the CalDAV MKCALENDAR interceptor has no CardDAV twin).
// Re-MKCOL on an existing id maps to AlreadyExists/Conflict → RFC 4918 §9.3 405
// Method Not Allowed.
func (b *Backend) CreateAddressBook(ctx context.Context, ab *carddav.AddressBook) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	abID, err := parseAddressbookID(sess, ab.Path)
	if err != nil {
		return err
	}
	metadata := EncryptedCollectionMetadata{
		Displayname: ab.Name,
		Description: ab.Description,
	}
	sealed, err := SealCollectionMetadata(metadata, sess.MLSPubkey(), sess.MlkemEk())
	if err != nil {
		return fmt.Errorf("carddav: seal new address-book metadata: %w", err)
	}
	outcome, err := wsrpc.ProvisionAddressbook(
		ctx, sess.Client(),
		sess.ActorID(), abID, sealed,
		false, // MKCOL/insert path; a metadata rename is update_metadata=true (deferred).
	)
	if err != nil {
		return fmt.Errorf("carddav: provision_addressbook: %w", err)
	}
	switch outcome {
	case wsrpc.ProvisionAddressbookCreated:
		return nil
	case wsrpc.ProvisionAddressbookAlreadyExists, wsrpc.ProvisionAddressbookConflict:
		// RFC 4918 §9.3 (MKCOL on a Request-URI where a collection already
		// exists → 405). The nest's AlreadyExists vs Conflict split is
		// unreachable from a real client (SealCollectionMetadata HPKE-re-seals
		// non-deterministically → a re-create's bytes never match → always
		// Conflict); a create verb maps "already exists" to 405 whatever the
		// metadata. A metadata *edit* is PROPPATCH, not MKCOL.
		return webdav.NewHTTPError(http.StatusMethodNotAllowed, fmt.Errorf("address book %s already exists", hex.EncodeToString(abID)))
	default:
		return fmt.Errorf("carddav: provision_addressbook: unexpected outcome %q", outcome)
	}
}

// DeleteAddressBook handles DELETE of an address-book collection (RFC 6352): the
// whole book and every card it holds are cascade-deleted nest-side. A genuinely
// user-initiated destructive action — the *No user-data loss* invariant permits
// explicit user deletes; the nest store performs the cascade atomically under a
// single lock so a crash leaves the whole book or nothing (carddav-server.md
// § Address-book collection model). Twin-less: the CalDAV backend has no
// DeleteCalendar method at all (emersion's caldav.Backend omits it).
//
// Outcomes:
//   - Deleted → nil (emersion writes 204 No Content).
//   - NotFound → 404 (idempotent re-delete; nest returns not_found for an
//     absent book).
func (b *Backend) DeleteAddressBook(ctx context.Context, urlPath string) error {
	sess, err := b.session(ctx)
	if err != nil {
		return err
	}
	abID, err := parseAddressbookID(sess, urlPath)
	if err != nil {
		return err
	}
	result, err := wsrpc.DeleteAddressbook(ctx, sess.Client(), sess.ActorID(), abID)
	if err != nil {
		return fmt.Errorf("delete_addressbook: %w", err)
	}
	switch result.Outcome {
	case wsrpc.DeleteAddressbookDeleted:
		return nil
	case wsrpc.DeleteAddressbookNotFound:
		return webdav.NewHTTPError(http.StatusNotFound,
			fmt.Errorf("address book %s not found", hex.EncodeToString(abID)))
	default:
		return fmt.Errorf("delete_addressbook: unexpected outcome %q", result.Outcome)
	}
}

// resolveAddressbookSegment maps a CardDAV URL path segment to the canonical
// 32-byte addressbook_id. A 64-hex segment decodes directly — that is the URL
// shape a Fauna app (and the lazy "Contacts" book) uses, where the segment
// IS hex(addressbook_id). Any other non-empty segment is the opaque collection
// name a stock CardDAV client picks for a MKCOL it issues; it maps
// deterministically to `addressbook_id = blake3(segment)[:32]`, so the nest
// keeps fixed-width 32-byte ids while the client chooses its own URL.
// Deterministic ⇒ every later PROPFIND/PUT/REPORT/sync on the same URL resolves
// to the same id. Returns (id, true) for any non-empty segment; (nil, false)
// for an empty one. Twin of CalDAV's resolveCalendarSegment.
func resolveAddressbookSegment(segment string) ([]byte, bool) {
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

// parseAddressbookID extracts the 32-byte addressbook_id from a URL path.
// Accepts `/carddav/{user}/{segment}/` and the resource form
// `/carddav/{user}/{segment}/<card>.vcf`, with or without a trailing slash;
// rejects anything outside the AUTH'd actor's home set (defense in depth — the
// auth middleware already scoped the actor). Twin of CalDAV's parseCalendarID.
func parseAddressbookID(sess *davauth.Session, urlPath string) ([]byte, error) {
	base := userBasePath(sess)
	if !strings.HasPrefix(urlPath, base) {
		return nil, webdav.NewHTTPError(http.StatusForbidden, fmt.Errorf("path %q not under AUTH'd actor's home set", urlPath))
	}
	rest := strings.TrimPrefix(urlPath, base)
	rest = strings.TrimSuffix(rest, "/")
	// rest may be just the segment (collection itself) or
	// "<segment>/<card>.vcf" (resource); split at the first '/'.
	parts := strings.SplitN(rest, "/", 2)
	id, ok := resolveAddressbookSegment(parts[0])
	if !ok {
		return nil, webdav.NewHTTPError(http.StatusBadRequest, fmt.Errorf("path %q missing address-book id", urlPath))
	}
	return id, nil
}

// carddav.Backend method routing:
//
//   - GetAddressObject / QueryAddressObjects / ListAddressObjects live in
//     report.go (REPORT addressbook-query + addressbook-multiget + PROPFIND
//     depth=1).
//   - PutAddressObject lives in put.go.
//   - DeleteAddressObject lives in delete.go.
//
// sync-collection REPORT is handled by a separate middleware
// (sync_collection.go) wrapping the inner carddav.Handler — emersion's reportReq
// parser does not recognize {DAV:}sync-collection, so the middleware peeks the
// body and routes it directly to nest's `sync_addressbook_since` RPC.

// ─── compile-time interface satisfaction check ────────────────────

var _ carddav.Backend = (*Backend)(nil)
