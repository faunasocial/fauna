package xrpc

import (
	"sync"
	"time"
)

// IPLimiter holds fixed-window per-(IP, endpoint-class) counters — the
// bridge-side layer of the two-layer rate-limit design (§ Wire & process
// topology; the nest-side layer is the per-caller windows on the PDS-facing
// kinds). Windows and limits are HARD-CODED constants per the product
// invariant (no human chooses them); F1/F2 tune before ship.
//
// Starting constants (ecosystem-aligned, atproto-pds-full.md):
//
//	ClassAuth       10 / 5 min  — createSession, refreshSession, deleteSession
//	ClassPublicRead 60 / 60 s   — the anonymous-surface default
//	ClassAuthed     120 / 60 s  — authenticated reads/proxying headroom
//	ClassWrite      30 / 60 s   — repo writes; each costs a nest round-trip
//	                              plus a signed funnel commit, and 30/min is
//	                              already far above any human posting rate.
//	                              The per-ACCOUNT bound is the second layer,
//	                              nest-side on ingest_external_write, since
//	                              only the nest knows the authenticated actor
//	                              behind a shared egress IP.
//	ClassBlob       20 / 60 s   — blob uploads. Tighter than ClassWrite in
//	                              *count* because each call may carry up to the
//	                              per-blob ceiling, so the bytes-per-minute a
//	                              single IP can push is what this bucket bounds,
//	                              not the call count. 20/min still clears the
//	                              four-image maximum of an app.bsky.feed.post
//	                              several times over.
type IPLimiter struct {
	clock func() time.Time

	mu      sync.Mutex
	buckets map[ipClassKey]*window
}

type ipClassKey struct {
	ip    string
	class EndpointClass
}

type window struct {
	startedAt time.Time
	count     uint32
}

type classPolicy struct {
	limit  uint32
	window time.Duration
}

func policyFor(class EndpointClass) classPolicy {
	switch class {
	case ClassAuth:
		return classPolicy{limit: 10, window: 5 * time.Minute}
	case ClassPublicRead:
		return classPolicy{limit: 60, window: time.Minute}
	case ClassWrite:
		return classPolicy{limit: 30, window: time.Minute}
	case ClassBlob:
		return classPolicy{limit: 20, window: time.Minute}
	default:
		return classPolicy{limit: 120, window: time.Minute}
	}
}

// sweepThreshold bounds steady-state memory the same way authlock does:
// past this many buckets, an Allow call reclaims every expired window
// opportunistically — no background goroutine, and an active window can
// never be flushed by flooding fresh keys.
const sweepThreshold = 8192

// NewIPLimiter builds a limiter. A nil clock uses the real clock.
func NewIPLimiter(clock func() time.Time) *IPLimiter {
	if clock == nil {
		clock = time.Now
	}
	return &IPLimiter{clock: clock, buckets: make(map[ipClassKey]*window)}
}

// Allow reports whether one more request from ip in class fits the window,
// counting it if so.
func (l *IPLimiter) Allow(ip string, class EndpointClass) bool {
	pol := policyFor(class)
	now := l.clock()
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.buckets) > sweepThreshold {
		for k, w := range l.buckets {
			if now.Sub(w.startedAt) >= policyFor(k.class).window {
				delete(l.buckets, k)
			}
		}
	}
	key := ipClassKey{ip: ip, class: class}
	w := l.buckets[key]
	if w == nil || now.Sub(w.startedAt) >= pol.window {
		l.buckets[key] = &window{startedAt: now, count: 1}
		return true
	}
	if w.count >= pol.limit {
		return false
	}
	w.count++
	return true
}
