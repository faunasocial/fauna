package dav

import (
	"log/slog"
	"sync"
)

// maxTrackedUndecryptableCollections bounds [UndecryptableWarnDedup]'s memory.
// A box's real collection count is small (a handful per actor), but the key
// space is client-chosen — a stock MUA that keeps re-issuing MKCALENDAR with
// fresh slugs mints a new collection id each time — so the map is capped
// rather than trusted to stay small. Past the cap every collection logs at
// WARN, i.e. the pre-dedup behaviour: degrading to "noisy" is correct, and
// degrading to "silent" would not be.
const maxTrackedUndecryptableCollections = 1024

// UndecryptableWarnDedup collapses the repeated "skipping <collection> with
// undecryptable metadata" WARN to **once per collection id**, then DEBUG.
//
// The read path answers an unopenable collection by logging and dropping it
// from the PROPFIND home set, so the WARN repeats on *every* PROPFIND — one
// per undecryptable collection per poll. A MUA polling a handful of orphaned
// collections sustains tens of WARNs a minute indefinitely (~1,180 in 29
// minutes was observed on the dogfood box), which buries any real signal in
// the log. The condition is per-collection state, not per-request state, so
// the first observation carries the alert and the repeats carry nothing new.
//
// Deduping the LOG never dedups the HARM: the collection is still dropped from
// the home set on every request, and the fix for that is upstream — the
// paired ek/dk publication in `post-quantum.md` § Post-quantum key publication
// and derivation, which stops a collection from becoming unopenable at all.
//
// The zero value is ready to use and safe for concurrent use.
type UndecryptableWarnDedup struct {
	seen  sync.Map // collection id (hex) → struct{}
	count int64
	mu    sync.Mutex
}

// Log emits `msg` with the caller's attrs at WARN the first time `collectionID`
// is reported and at DEBUG every time after. `collectionID` is the hex id the
// caller would have logged anyway, so callers key on exactly what they print.
func (d *UndecryptableWarnDedup) Log(logger *slog.Logger, collectionID, msg string, attrs ...any) {
	if logger == nil {
		return
	}
	if d.firstSighting(collectionID) {
		logger.Warn(msg, attrs...)
		return
	}
	logger.Debug(msg, attrs...)
}

// firstSighting reports whether this is the first time `collectionID` has been
// seen, recording it when there is room left under the cap. Past the cap it
// reports true for every unrecorded id — see the cap's rationale.
func (d *UndecryptableWarnDedup) firstSighting(collectionID string) bool {
	if _, ok := d.seen.Load(collectionID); ok {
		return false
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	// Re-check under the lock: a racing caller may have recorded it between
	// the Load above and the lock, and both would otherwise claim the first
	// sighting and both log at WARN.
	if _, ok := d.seen.Load(collectionID); ok {
		return false
	}
	if d.count >= maxTrackedUndecryptableCollections {
		return true
	}
	d.seen.Store(collectionID, struct{}{})
	d.count++
	return true
}
