package mailfauna

import (
	"errors"
	"net"
	"net/netip"
	"sync"
	"sync/atomic"
	"time"
)

// Process-wide in-flight cap on credential Argon2id (network-exposure.md § Rulings
// F3 and deployment-threat-model.md § Concern 1a, finding M5).
//
// Every pre-auth password check — submission, IMAP, CalDAV, CardDAV, WebDAV and
// the dummy — runs a 64 MiB Argon2id. Each listener's lockout admits hundreds of
// failures per minute per source, and DAV re-derives on every request, so without
// a shared cap a few sources hold gigabytes of KDF memory and every core. The
// gate lives here, around the only two FFI calls that run the Argon2id arm, so
// no call site can forget it. HKDF (OAUTHBEARER) is microsecond-cheap and stays
// unthrottled. Hard-coded constants: no human ever chooses these.
//
// The gate is fair across sources: a freed slot goes to the queued
// source holding the fewest slots, so one source (an IPv4 address, or an IPv6
// /64 however many addresses it rotates through) cannot hold every slot while
// another source waits. A lone source still uses every slot.
const (
	// kdfMaxInFlight is the number of Argon2id runs allowed at once, bounding
	// pre-auth KDF memory to kdfMaxInFlight × 64 MiB.
	kdfMaxInFlight = 4
	// kdfMaxWait is how long an attempt queues for a slot before it fails with
	// ErrKDFBusy, which every caller answers as a TEMPORARY failure.
	kdfMaxWait = 2 * time.Second
)

// ErrKDFBusy means no KDF slot freed within the wait bound. It says nothing
// about the credential: callers answer a temporary failure (SMTP 454, IMAP
// [UNAVAILABLE], HTTP 503) and record neither a lockout failure nor a "fail"
// audit row — a flood must never book an honest user's correct password as a
// wrong one.
var ErrKDFBusy = errors.New("credential KDF busy")

// kdfWaiter is one queued attempt; granted is set under the gate's mutex.
type kdfWaiter struct {
	ready   chan struct{}
	granted bool
}

// fairGate is the slot scheduler. inFlight counts held slots per source key;
// queues holds each source's waiters in arrival order; order lists the sources
// that have waiters, the most recently served last.
type fairGate struct {
	mu       sync.Mutex
	free     int
	inFlight map[string]int
	queues   map[string][]*kdfWaiter
	order    []string
}

var (
	kdfGate = &fairGate{
		free:     kdfMaxInFlight,
		inFlight: map[string]int{},
		queues:   map[string][]*kdfWaiter{},
	}
	// kdfWait is a var only so tests can shorten it.
	kdfWait = kdfMaxWait

	// Test hooks: Argon2id runs admitted, current and peak in flight.
	kdfRuns     atomic.Uint64
	kdfInFlight atomic.Int64
	kdfPeak     atomic.Int64
)

// KDFRunCount reports how many Argon2id runs the gate has admitted since process
// start. A test hook: it lets a bridge test assert that a path ran (or skipped)
// the KDF without a flaky wall-clock tolerance.
func KDFRunCount() uint64 { return kdfRuns.Load() }

// SaturateKDFGateForTest holds every slot under a source no caller uses and sets
// the wait bound to wait, so the next Argon2id attempt fails with ErrKDFBusy
// after wait. The returned func releases the slots and restores the bound. A test
// hook for the listener packages' busy-gate tests; never called in production.
func SaturateKDFGateForTest(wait time.Duration) (restore func()) {
	old := kdfWait
	kdfWait = 5 * time.Second
	var held []func()
	for i := 0; i < kdfMaxInFlight; i++ {
		rel, err := acquireKDF(KdfKindArgon2id, "kdf-gate-test-saturator")
		if err != nil {
			panic("SaturateKDFGateForTest: " + err.Error())
		}
		held = append(held, rel)
	}
	kdfWait = wait
	return func() {
		for _, rel := range held {
			rel()
		}
		kdfWait = old
	}
}

// kdfSourceKey maps an attempt's source address to its fairness key: an IPv4
// address as itself, an IPv6 address as its /64 (one host or one rotating
// subnet is one source). A host:port form keys on its host; anything that does
// not parse keys on its raw text.
func kdfSourceKey(source string) string {
	addr, err := netip.ParseAddr(source)
	if err != nil {
		host, _, splitErr := net.SplitHostPort(source)
		if splitErr != nil {
			return source
		}
		if addr, err = netip.ParseAddr(host); err != nil {
			return source
		}
	}
	addr = addr.Unmap()
	if addr.Is4() {
		return addr.String()
	}
	prefix, _ := addr.WithZone("").Prefix(64)
	return prefix.String()
}

// acquireKDF takes a slot for an Argon2id run from source (the attempt's client
// address); the returned func releases it. Non-Argon2id kinds pass straight
// through.
func acquireKDF(kind KdfKind, source string) (release func(), err error) {
	if kind != KdfKindArgon2id {
		return func() {}, nil
	}
	key := kdfSourceKey(source)
	g := kdfGate

	g.mu.Lock()
	if g.free > 0 {
		// Dispatch is eager, so a free slot means nobody is queued.
		g.grantLocked(key)
		g.mu.Unlock()
		return admitted(key), nil
	}
	w := &kdfWaiter{ready: make(chan struct{})}
	if len(g.queues[key]) == 0 {
		g.order = append(g.order, key)
	}
	g.queues[key] = append(g.queues[key], w)
	g.mu.Unlock()

	timer := time.NewTimer(kdfWait)
	defer timer.Stop()
	select {
	case <-w.ready:
		return admitted(key), nil
	case <-timer.C:
	}
	g.mu.Lock()
	defer g.mu.Unlock()
	if w.granted {
		// Granted in the race with the timer: the slot is ours.
		return admitted(key), nil
	}
	g.dequeueLocked(key, w)
	return nil, ErrKDFBusy
}

// admitted counts an admitted run and returns its release.
func admitted(key string) func() {
	kdfRuns.Add(1)
	n := kdfInFlight.Add(1)
	for {
		p := kdfPeak.Load()
		if n <= p || kdfPeak.CompareAndSwap(p, n) {
			break
		}
	}
	var once sync.Once
	return func() {
		once.Do(func() {
			kdfInFlight.Add(-1)
			kdfGate.release(key)
		})
	}
}

func (g *fairGate) grantLocked(key string) {
	g.free--
	g.inFlight[key]++
}

func (g *fairGate) release(key string) {
	g.mu.Lock()
	defer g.mu.Unlock()
	g.free++
	if g.inFlight[key]--; g.inFlight[key] <= 0 {
		delete(g.inFlight, key)
	}
	for g.free > 0 && len(g.order) > 0 {
		// The queued source holding the fewest slots; ties go to the one served
		// longest ago (earliest in order).
		best := 0
		for i, k := range g.order[1:] {
			if g.inFlight[k] < g.inFlight[g.order[best]] {
				best = i + 1
			}
		}
		k := g.order[best]
		w := g.queues[k][0]
		g.queues[k] = g.queues[k][1:]
		g.order = append(g.order[:best], g.order[best+1:]...)
		if len(g.queues[k]) == 0 {
			delete(g.queues, k)
		} else {
			g.order = append(g.order, k)
		}
		g.grantLocked(k)
		w.granted = true
		close(w.ready)
	}
}

// dequeueLocked drops a timed-out waiter from its source's queue.
func (g *fairGate) dequeueLocked(key string, w *kdfWaiter) {
	q := g.queues[key]
	for i, x := range q {
		if x == w {
			q = append(q[:i], q[i+1:]...)
			break
		}
	}
	if len(q) > 0 {
		g.queues[key] = q
		return
	}
	delete(g.queues, key)
	for i, k := range g.order {
		if k == key {
			g.order = append(g.order[:i], g.order[i+1:]...)
			break
		}
	}
}
