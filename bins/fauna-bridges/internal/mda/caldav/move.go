package caldav

import (
	"bytes"
	"context"
	"encoding/hex"
	"fmt"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// newMoveCopyInterceptor serves MOVE and COPY of an event resource between the
// AUTH'd actor's own calendars — caldav-server.md § Write surface (the MOVE /
// COPY rows) and § Atomicity rule. The DAV library's handler answers both with
// 501 for this backend, so they are served here, ahead of it, like MKCALENDAR.
//
// The event is opened in the user's session and re-sealed fresh for the
// destination calendar under the same uid_hash (never copied as identical
// ciphertext — see the note at the re-seal), and the Fauna-only sidecar
// travels with it unchanged. Order is the atomicity rule's:
// the destination write first, and for MOVE the source delete only after it
// succeeds, so a failed MOVE leaves the source exactly where it was. A source
// delete that fails after the destination landed answers 202 with the
// inconsistency described, and the calendar app's next sync sees both copies.
//
// Status mapping (RFC 4918 §9.8 / §9.9): a new destination → 201, an
// overwritten one → 204; `Overwrite: F` onto an existing event → 412; a
// destination outside the actor's home set → 403 (another user's calendars are
// never reachable); a destination calendar that does not exist → 409; the same
// calendar as the source → 403; a missing source → 404.
func newMoveCopyInterceptor(next http.Handler, logger *slog.Logger) http.Handler {
	if next == nil {
		panic("caldav: newMoveCopyInterceptor: next must not be nil")
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &moveCopyInterceptor{next: next, logger: logger}
}

type moveCopyInterceptor struct {
	next   http.Handler
	logger *slog.Logger
}

func (m *moveCopyInterceptor) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != "MOVE" && r.Method != "COPY" {
		m.next.ServeHTTP(w, r)
		return
	}
	sess := davauth.SessionFromContext(r.Context())
	if sess == nil {
		w.Header().Set("WWW-Authenticate", fmt.Sprintf(`Basic realm=%q`, caldavRealm))
		http.Error(w, davauth.ErrAuthFailed.Error(), http.StatusUnauthorized)
		return
	}
	// Explicit statuses throughout: the shared path parsers wrap their errors
	// in the DAV library's HTTPError, whose status is unreadable from here
	// (the sync-collection interceptor's `syncPathError` note).
	fail := func(err error) {
		http.Error(w, err.Error(), http.StatusInternalServerError)
	}
	base := userBasePath(sess)
	if !strings.HasPrefix(r.URL.Path, base) {
		http.Error(w, "caldav: the source is not one of your calendars", http.StatusForbidden)
		return
	}
	srcCal, uidHash, err := parseEventResourcePath(sess, r.URL.Path)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	dest, err := url.Parse(r.Header.Get("Destination"))
	if err != nil || dest.Path == "" {
		http.Error(w, "caldav: MOVE/COPY needs a Destination", http.StatusBadRequest)
		return
	}
	if !strings.HasPrefix(dest.Path, base) {
		http.Error(w, "caldav: the destination is not one of your calendars", http.StatusForbidden)
		return
	}
	destCal, err := parseCalendarID(sess, dest.Path)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	if bytes.Equal(destCal, srcCal) {
		http.Error(w, "caldav: source and destination are the same calendar", http.StatusForbidden)
		return
	}

	ctx := r.Context()
	source, srcExists, err := findEvent(ctx, sess, srcCal, uidHash)
	if err != nil {
		fail(err)
		return
	}
	if !srcExists || source == nil {
		http.Error(w, "caldav: no such event", http.StatusNotFound)
		return
	}
	existing, destExists, err := findEvent(ctx, sess, destCal, uidHash)
	if err != nil {
		fail(err)
		return
	}
	if !destExists {
		http.Error(w, "caldav: the destination calendar does not exist", http.StatusConflict)
		return
	}
	if existing != nil && strings.EqualFold(r.Header.Get("Overwrite"), "F") {
		http.Error(w, "caldav: the destination already holds this event", http.StatusPreconditionFailed)
		return
	}

	// Re-seal rather than copy the ciphertext: nest files a body as a content
	// record keyed by its hash, so a destination row holding the source's exact
	// bytes would share the source's record, and the MOVE's own source delete
	// (or, after a COPY, any later delete of either copy) would tombstone the
	// body out from under the other row. Opening needs the session's MLS
	// snapshot, the same precondition as reading the calendar at all.
	var opener mailfauna.RecordOpener
	if o := sess.RecordOpener(); o != nil {
		opener = o
	}
	plaintext, err := mailfauna.OpenStoredRecord(opener, source.EncryptedBody)
	if err != nil {
		http.Error(w, fmt.Sprintf("caldav: cannot open the event to move it: %v", err), http.StatusInternalServerError)
		return
	}
	body, hint, err := sealEventForSession(sess, plaintext)
	if err != nil {
		fail(err)
		return
	}
	put, err := wsrpc.PutEventCiphertextCarrying(
		ctx, sess.Client(),
		sess.ActorID(), destCal, uidHash,
		body, hint,
		time.Now().Unix(), uint32(len(body)),
		nil, source.EncryptedFaunaExt,
	)
	if err != nil {
		if dav.IsOverQuota(err) {
			// The destination has no room on the account: 507, and the
			// source stays where it was — its delete below never runs.
			dav.WriteQuotaNotExceeded(w)
			return
		}
		fail(fmt.Errorf("put_event_ciphertext: %w", err))
		return
	}
	switch put.Outcome {
	case wsrpc.PutEventCreated, wsrpc.PutEventUpdated:
	case wsrpc.PutEventCalendarNotFound:
		http.Error(w, "caldav: the destination calendar does not exist", http.StatusConflict)
		return
	default:
		fail(fmt.Errorf("put_event_ciphertext: unexpected outcome %q", put.Outcome))
		return
	}

	if r.Method == "MOVE" {
		del, err := wsrpc.DeleteEvent(ctx, sess.Client(), sess.ActorID(), srcCal, uidHash, nil)
		if err != nil || del.Outcome != wsrpc.DeleteEventDeleted {
			m.logger.Warn("caldav: MOVE copied the event but could not remove the source",
				"uid_hash", hex.EncodeToString(uidHash), "err", err)
			w.WriteHeader(http.StatusAccepted)
			fmt.Fprintf(w, "caldav: the event was copied to its destination but is still in the source calendar; the next sync shows both\n")
			return
		}
	}
	w.Header().Set("Location", userBasePath(sess)+hex.EncodeToString(destCal)+"/"+hex.EncodeToString(uidHash)+".ics")
	if existing != nil {
		w.WriteHeader(http.StatusNoContent)
		return
	}
	w.WriteHeader(http.StatusCreated)
}

// findEvent pages calendar `calID` for the stored entry with `uidHash`.
// Returns (entry or nil, whether the calendar exists, error).
func findEvent(
	ctx context.Context, sess *davauth.Session, calID, uidHash []byte,
) (*wsrpc.EventEntry, bool, error) {
	var cursor []byte
	for {
		res, err := wsrpc.QueryEvents(ctx, sess.Client(), sess.ActorID(), calID, nil, cursor, reportPageSize)
		if err != nil {
			return nil, false, fmt.Errorf("query_events: %w", err)
		}
		switch res.Outcome {
		case wsrpc.QueryEventsOk:
		case wsrpc.QueryEventsCalendarNotFound:
			return nil, false, nil
		default:
			return nil, false, fmt.Errorf("query_events: unexpected outcome %q", res.Outcome)
		}
		for i := range res.Events {
			if bytes.Equal(res.Events[i].UIDHash, uidHash) {
				return &res.Events[i], true, nil
			}
		}
		if !res.More || len(res.Events) == 0 {
			return nil, true, nil
		}
		cursor = res.Events[len(res.Events)-1].EventID
	}
}
