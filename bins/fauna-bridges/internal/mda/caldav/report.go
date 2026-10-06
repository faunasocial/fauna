package caldav

import (
	"bytes"
	"context"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/emersion/go-ical"
	"github.com/emersion/go-webdav"
	"github.com/emersion/go-webdav/caldav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const (
	// reportPageSize bounds each query_events reply so a large calendar is
	// fetched in chunks that stay well under the bridge's 16 MiB WS read limit
	// (wsrpc.Client SetReadLimit). A single unbounded reply (the former
	// `limit=0`) could otherwise exceed that frame cap and fail the whole REPORT.
	reportPageSize = 1000
	// maxReportEvents caps the total events one REPORT / PROPFIND / multiget
	// unseals + parses, matching caldav-server.md § Read surface's documented
	// "~10k events per calendar" envelope (beyond which the § Future tightenings
	// "encrypted index on DTSTART buckets" pass is owed). Past this the result is
	// truncated + logged rather than letting an authenticated client force
	// unbounded unseal/parse work in one request (§ B6, 2026-06-24
	// email-component-compromise review). MUST be a multiple of reportPageSize.
	maxReportEvents = 10000
)

// QueryCalendarObjects implements emersion's caldav.Backend interface
// for REPORT calendar-query (RFC 4791 §7.8). Per `caldav-server.md`
// § Read surface line 91: time-range filtering is MDA-local — nest
// stores opaque sealed bodies, so we fetch every event in the calendar,
// HPKE-open in-session, parse via the shared-Rust iCalendar surface,
// and apply ExpandRecurrence against the requested time-range window.
//
// Flow:
//
//  1. fetchAllEvents — bounded pagination over wsrpc.QueryEvents
//     (reportPageSize chunks, capped at maxReportEvents; the goal doc
//     § Read surface notes the ~10k-event envelope past which the result
//     truncates pending the encrypted-DTSTART-index tightening).
//  2. Per EventEntry: OpenStoredRecord (every event rests sealed and opens
//     via the session opener; an unsealed body is refused) →
//     ParseICalendar → if a time-range filter is present,
//     ExpandRecurrence → drop on empty.
//  3. ical.NewDecoder rebuilds the *ical.Calendar emersion's response
//     writer encodes back into the <calendar-data> element.
//
// Errors:
//
//   - CalendarNotFound → 404.
//   - Per-event decrypt/parse failures are logged + the event is
//     skipped (mirrors ListCalendars' "drop one calendar, return the
//     rest" pattern). PROPFIND/REPORT remain useful when only a subset
//     of events is broken.
func (b *Backend) QueryCalendarObjects(
	ctx context.Context,
	urlPath string,
	query *caldav.CalendarQuery,
) ([]caldav.CalendarObject, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	calID, err := parseCalendarID(sess, urlPath)
	if err != nil {
		return nil, err
	}

	events, err := b.fetchAllEvents(ctx, sess, calID)
	if err != nil {
		return nil, err
	}

	windowStart, windowEnd, hasRange := extractTimeRange(query)

	out := make([]caldav.CalendarObject, 0, len(events))
	for i := range events {
		obj, ok, err := b.openEvent(sess, calID, &events[i], hasRange, windowStart, windowEnd)
		if err != nil {
			// `err` may be a decrypt failure OR an iCalendar re-encode failure
			// (a malformed stored body go-ical's encoder rejects, e.g. a missing
			// DTSTAMP) — either way one event is dropped, the rest returned. The
			// `err` field carries the specific cause.
			b.logger.Warn(
				"caldav: skipping unservable event",
				"calendar_id_hex", hex.EncodeToString(calID),
				"uid_hash_hex", hex.EncodeToString(events[i].UIDHash),
				"err", err,
			)
			continue
		}
		if !ok {
			continue
		}
		out = append(out, obj)
	}
	return out, nil
}

// GetCalendarObject implements emersion's caldav.Backend interface
// for both REPORT calendar-multiget (per-href fetch) AND PROPFIND on
// a single event resource. Path scheme:
//
//	/caldav/{user@domain}/{calendar_id_hex}/{uid_hash_hex}.ics
//
// nest has no by-uid_hash query RPC today; we full-fetch the calendar
// and pick the matching row. The goal doc § Read surface tolerates
// this up to ~10k events per calendar (notes a future encrypted-index
// pass for larger calendars).
//
// Errors:
//
//   - Calendar missing → 404 (CalendarNotFound from query_events).
//   - Event missing in the calendar → 404 (Decision 7 collapse: same
//     status as a deleted event).
func (b *Backend) GetCalendarObject(
	ctx context.Context,
	urlPath string,
	req *caldav.CalendarCompRequest,
) (*caldav.CalendarObject, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	calID, uidHash, err := parseEventResourcePath(sess, urlPath)
	if err != nil {
		return nil, err
	}

	events, err := b.fetchAllEvents(ctx, sess, calID)
	if err != nil {
		return nil, err
	}

	for i := range events {
		if !bytes.Equal(events[i].UIDHash, uidHash) {
			continue
		}
		obj, _, err := b.openEvent(sess, calID, &events[i], false, 0, 0)
		if err != nil {
			return nil, fmt.Errorf("open event %s: %w",
				hex.EncodeToString(uidHash), err)
		}
		return &obj, nil
	}
	return nil, webdav.NewHTTPError(http.StatusNotFound,
		fmt.Errorf("event %s not found in calendar %s",
			hex.EncodeToString(uidHash), hex.EncodeToString(calID)))
}

// ListCalendarObjects implements emersion's caldav.Backend interface
// for PROPFIND with Depth: 1 on a calendar collection. Delegates to
// QueryCalendarObjects with an empty filter so PROPFIND surfaces the
// same data the REPORT path returns — modern MUAs typically use REPORT
// but a depth-1 PROPFIND must produce the same result set per RFC 4918.
func (b *Backend) ListCalendarObjects(
	ctx context.Context,
	urlPath string,
	req *caldav.CalendarCompRequest,
) ([]caldav.CalendarObject, error) {
	return b.QueryCalendarObjects(ctx, urlPath, &caldav.CalendarQuery{})
}

// fetchAllEvents pulls every event in the calendar (sinceModseq=nil) via
// bounded pagination: it loops wsrpc.QueryEvents in reportPageSize chunks —
// resuming on the prior page's last EventID (nest orders by event_id ASC) —
// until the calendar is exhausted OR the maxReportEvents hard cap is reached.
//
// The former single `limit=0` fetch returned the whole calendar in one reply,
// which (a) could exceed the bridge's 16 MiB WS read cap on a large calendar and
// fail the REPORT outright, and (b) let an authenticated client force unbounded
// unseal+parse work in one request (§ B6). Paging fixes (a); the cap bounds (b),
// truncating + logging past the documented ~10k envelope (caldav-server.md
// § Read surface / § Future tightenings). Maps CalendarNotFound to a 404
// HTTPError so callers can return it verbatim.
func (b *Backend) fetchAllEvents(
	ctx context.Context, sess *davauth.Session, calID []byte,
) ([]wsrpc.EventEntry, error) {
	var (
		acc    []wsrpc.EventEntry
		cursor []byte // afterEventID; nil on the first page
	)
	for {
		res, err := wsrpc.QueryEvents(
			ctx, sess.Client(),
			sess.ActorID(), calID,
			nil, cursor, reportPageSize,
		)
		if err != nil {
			return nil, fmt.Errorf("query_events: %w", err)
		}
		switch res.Outcome {
		case wsrpc.QueryEventsOk:
			// fall through
		case wsrpc.QueryEventsCalendarNotFound:
			return nil, webdav.NewHTTPError(http.StatusNotFound,
				fmt.Errorf("calendar %s not found", hex.EncodeToString(calID)))
		default:
			return nil, fmt.Errorf("query_events: unexpected outcome %q", res.Outcome)
		}
		acc = append(acc, res.Events...)
		// Stop on the last page, an empty page (defensive against a
		// non-advancing cursor), or the hard cap.
		if !res.More || len(res.Events) == 0 {
			break
		}
		if len(acc) >= maxReportEvents {
			if len(acc) > maxReportEvents {
				acc = acc[:maxReportEvents]
			}
			b.logger.Warn(
				"caldav: calendar exceeds the REPORT event cap; truncating",
				"calendar_id_hex", hex.EncodeToString(calID),
				"returned", len(acc),
				"cap", maxReportEvents,
			)
			break
		}
		cursor = res.Events[len(res.Events)-1].EventID
	}
	return acc, nil
}

// openEvent unseals a single EventEntry's encrypted body and, when a
// time-range filter is in effect, drops the event if no recurrence
// occurrence falls inside the window. Returns the CalendarObject ready
// for emersion's response writer; the second return value is `false`
// when the event was excluded by the filter (not an error). The third
// return value is a per-event decrypt/parse failure; the caller logs +
// skips.
func (b *Backend) openEvent(
	sess *davauth.Session,
	calID []byte,
	entry *wsrpc.EventEntry,
	hasRange bool,
	windowStart, windowEnd int64,
) (caldav.CalendarObject, bool, error) {
	// ONE serve path (sealed-both-modes design D1): every event rests sealed
	// and HPKE-opens via the per-session opener (nil when no MLS snapshot is
	// on file — the record then errors at open time). An unsealed body, or a
	// sealed one that fails AEAD-open, errors, so the caller logs + skips it
	// (never silently serves corruption).
	var opener mailfauna.RecordOpener
	if o := sess.RecordOpener(); o != nil {
		opener = o
	}
	plaintext, err := mailfauna.OpenStoredRecord(opener, entry.EncryptedBody)
	if err != nil {
		return caldav.CalendarObject{}, false,
			fmt.Errorf("open stored record: %w", err)
	}

	if hasRange {
		doc, parseErr := mailfauna.ParseICalendar(plaintext)
		if parseErr != nil {
			return caldav.CalendarObject{}, false,
				fmt.Errorf("parse iCalendar: %w", parseErr)
		}
		if !anyComponentInWindow(doc, windowStart, windowEnd) {
			return caldav.CalendarObject{}, false, nil
		}
	}

	cal, err := ical.NewDecoder(bytes.NewReader(plaintext)).Decode()
	if err != nil {
		return caldav.CalendarObject{}, false,
			fmt.Errorf("decode iCalendar: %w", err)
	}

	// Pre-validate that `cal` RE-ENCODES before handing it to emersion's
	// streaming multistatus writer. go-ical's encoder is stricter than its
	// decoder — RFC 5545 requires exactly one DTSTAMP + UID per VEVENT — and
	// emersion has already committed the response status by the time it encodes
	// each <calendar-data>, so an encode error THERE breaks the ENTIRE
	// REPORT/GET response ("superfluous WriteHeader") and hides every OTHER
	// event in the calendar, with no per-event diagnostic. Catch it here so a
	// single malformed stored event is skipped + logged like an undecryptable
	// one (the "drop one event, return the rest" invariant — same as the decrypt
	// failure above). This was the GAP-2 failure mode: a client write that
	// omitted DTSTAMP (now fixed in the shared `fauna_core::ical` writer) made
	// the encoder reject the body mid-stream. caldav-server.md § Read surface.
	if encErr := ical.NewEncoder(io.Discard).Encode(cal); encErr != nil {
		return caldav.CalendarObject{}, false,
			fmt.Errorf("re-encode iCalendar: %w", encErr)
	}

	obj := caldav.CalendarObject{
		Path: userBasePath(sess) +
			hex.EncodeToString(calID) + "/" +
			hex.EncodeToString(entry.UIDHash) + ".ics",
		ContentLength: int64(len(plaintext)),
		ETag:          entry.ETag,
		Data:          cal,
	}
	if entry.InternalDate > 0 {
		obj.ModTime = time.Unix(entry.InternalDate, 0).UTC()
	}
	return obj, true, nil
}

// anyComponentInWindow returns true when any top-level VEVENT or
// VTODO in `doc` has a recurrence occurrence inside
// `[windowStart, windowEnd)`. Non-recurring events fall back to
// ExpandRecurrence's single-base-occurrence behavior (per the
// `mailfauna.ExpandRecurrence` doc).
//
// A component that fails to expand (malformed RRULE, missing DTSTART)
// is treated as "no overlap" rather than aborting the whole REPORT —
// the goal doc's permissive carve-out for partial decrypt failures
// extends to parse failures inside an otherwise-decryptable body.
func anyComponentInWindow(doc mailfauna.ICalDocument, windowStart, windowEnd int64) bool {
	for i := range doc.Components {
		comp := &doc.Components[i]
		if !isEventOrTodo(comp.Name) {
			continue
		}
		occs, err := mailfauna.ExpandRecurrence(*comp, windowStart, windowEnd)
		if err != nil {
			continue
		}
		if len(occs) > 0 {
			return true
		}
	}
	return false
}

// isEventOrTodo case-insensitively matches VEVENT and VTODO. Mirrors
// the put.go validation rule — VJOURNAL is rejected on PUT, so a
// VJOURNAL ever appearing in a returned body would be unexpected, but
// we filter it out here too as defense in depth.
func isEventOrTodo(name string) bool {
	return strings.EqualFold(name, ical.CompEvent) ||
		strings.EqualFold(name, ical.CompToDo)
}

// extractTimeRange walks a CalendarQuery's CompFilter tree for the
// first <time-range> element. Per RFC 4791 §9.7 the time-range can
// appear on either the VCALENDAR-level or VEVENT-level filter; we
// return the deepest non-zero range we find.
//
// `hasRange` is false when no time-range element appears anywhere in
// the filter tree — the caller skips ExpandRecurrence entirely (a
// no-filter REPORT must return every event regardless of DTSTART).
func extractTimeRange(query *caldav.CalendarQuery) (start, end int64, hasRange bool) {
	if query == nil {
		return 0, 0, false
	}
	return walkFilterForRange(&query.CompFilter)
}

// walkFilterForRange descends the CompFilter tree and returns the
// first non-zero time-range it finds (depth-first). When only one of
// Start/End is set, the unset endpoint is clamped to min/max int64 so
// ExpandRecurrence's window covers everything on that side.
func walkFilterForRange(f *caldav.CompFilter) (int64, int64, bool) {
	if !f.Start.IsZero() || !f.End.IsZero() {
		var start, end int64
		if f.Start.IsZero() {
			start = -1 << 62 // far past
		} else {
			start = f.Start.Unix()
		}
		if f.End.IsZero() {
			end = 1<<62 - 1 // far future
		} else {
			end = f.End.Unix()
		}
		return start, end, true
	}
	for i := range f.Comps {
		if s, e, ok := walkFilterForRange(&f.Comps[i]); ok {
			return s, e, true
		}
	}
	return 0, 0, false
}
