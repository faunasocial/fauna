// Package undecryptable holds the MDA's shared handling of an object its
// session keys cannot open — today the once-per-object WARN dedup the DAV
// collection read path and the IMAP FETCH record read path both log through,
// so the two cannot drift.
package undecryptable

import (
	"log/slog"
	"sync"
)

// maxTrackedIDs bounds [WarnDedup]'s memory. A box's real count of unopenable
// objects is small (a handful of orphaned collections per actor; a rare
// malformed or foreign-recipient mail record), but part of the key space is
// client-chosen — a stock MUA that keeps re-issuing MKCALENDAR with fresh slugs
// mints a new collection id each time — so the map is capped rather than
// trusted to stay small. Past the cap every id logs at WARN, i.e. the pre-dedup
// behaviour: degrading to "noisy" is correct, and degrading to "silent" would
// not be.
const maxTrackedIDs = 1024

// WarnDedup collapses a repeated "<object> could not be opened" WARN to **once
// per object id**, then DEBUG.
//
// Both read paths that use it answer an unopenable object on *every* request,
// so the WARN would otherwise repeat indefinitely: the DAV path drops an
// undecryptable collection from the PROPFIND home set on every poll (a MUA
// polling a handful of orphaned collections sustained ~1,180 WARNs in 29
// minutes on the dogfood box), and IMAP FETCH serves an unopenable mail record
// as a degraded placeholder on every FETCH that spans it (`imap-server.md`
// § Body-section FETCH → *Unopenable records*). The condition is per-object
// state, not per-request state, so the first observation carries the alert and
// the repeats carry nothing new.
//
// Deduping the LOG never dedups the HARM: the object is still dropped or
// degraded on every request. The upstream fix for the DAV case is the paired
// ek/dk publication in `post-quantum.md` § Post-quantum key publication and
// derivation, which stops a collection from becoming unopenable at all.
//
// The zero value is ready to use and safe for concurrent use.
type WarnDedup struct {
	seen  sync.Map // object id → struct{}
	count int64
	mu    sync.Mutex
}

// Log emits `msg` with the caller's attrs at WARN the first time `id` is
// reported and at DEBUG every time after. `id` is the id the caller would have
// logged anyway (a collection's hex id, a mail record's message id), so
// callers key on exactly what they print.
func (d *WarnDedup) Log(logger *slog.Logger, id, msg string, attrs ...any) {
	if logger == nil {
		return
	}
	if d.firstSighting(id) {
		logger.Warn(msg, attrs...)
		return
	}
	logger.Debug(msg, attrs...)
}

// firstSighting reports whether this is the first time `id` has been seen,
// recording it when there is room left under the cap. Past the cap it reports
// true for every unrecorded id — see the cap's rationale.
func (d *WarnDedup) firstSighting(id string) bool {
	if _, ok := d.seen.Load(id); ok {
		return false
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	// Re-check under the lock: a racing caller may have recorded it between
	// the Load above and the lock, and both would otherwise claim the first
	// sighting and both log at WARN.
	if _, ok := d.seen.Load(id); ok {
		return false
	}
	if d.count >= maxTrackedIDs {
		return true
	}
	d.seen.Store(id, struct{}{})
	d.count++
	return true
}
