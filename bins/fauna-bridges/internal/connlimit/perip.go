package connlimit

import (
	"net"
	"sync"
	"sync/atomic"
)

// DefaultMaxConnsPerIP mirrors the Rust
// fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP and AuthPolicy::default().
// max_conn_per_ip — the per-source concurrent-connection ceiling the
// authenticated mail listeners apply when the admin hasn't tuned it.
const DefaultMaxConnsPerIP = 256

// PerIPLimiter tracks live connection counts per source IP and enforces a
// per-IP concurrent-connection ceiling, shared across every listener it wraps
// in one bridge process (so a source's total simultaneous authenticated
// connections — submission + IMAP + CalDAV for the MDA process, 465 + 587 for
// the MTA process — are counted together, matching the catalog default's
// "several devices × persistent IMAP IDLE" rationale).
//
// It is the Go analogue of the Rust fauna_conn_limit::PerIpConnLimit (priority
// #2 — one shape, not a copy per binary): the nest TLS loop + SNI router use
// the Rust crate; the mail bridge's authenticated listeners use this. Two
// differences, both driven by the wire contract (AuthPolicy.max_conn_per_ip):
//
//   - The max is an atomic so a fauna.bridges.config_changed push can
//     hot-reload it (SetMax) at the next accept without a restart.
//   - A max of 0 means DISABLED (admit everything) — the catalog row's
//     "0 = disabled" sentinel — whereas the Rust crate's env-driven cap of 0
//     would reject everything.
//
// A loopback source is counted against LoopbackMaxConns instead of the admin's
// cap — bounded, not exempt (the in-container router/bridge dial loopback, so
// the admin's abuse cap must never starve them; but a leaking co-resident
// process must not be unbounded either — the 2026-08-22 nest incident, where
// an exempt loopback peer held 16 k accepted sockets and exhausted the box's
// network state). The loopback ceiling applies even when the admin's cap is
// disabled (max == 0): it is a safety bound, not the abuse knob. The count map
// is GC'd as connections close (an entry is removed when its count returns to
// zero) so rotating source IPs can't grow it unbounded — the same leak class
// the nest governor sweeper closed for the rate limiter (review § D11).
type PerIPLimiter struct {
	max         atomic.Uint32 // 0 = disabled (admit all non-loopback)
	loopbackMax int           // LoopbackMaxConns in production; a test seam only
	mu          sync.Mutex
	counts      map[string]int // keyed by IP string; entries GC'd when they hit zero
}

// LoopbackMaxConns is the ceiling a loopback source is counted against instead
// of the admin's per-IP cap — the Go mirror of the Rust
// fauna_conn_limit::LOOPBACK_MAX_CONNS (one shape, priority #2), and like it a
// hard-coded safety bound rather than a knob: the loopback peers are the
// deployment artifact's own co-resident processes, whose legitimate connection
// count is a property of the artifact, not a preference. 1024 is two orders of
// magnitude above any legitimate co-resident fleet and a quarter of the 4096
// global per-listener cap, so a leaking local process can never take more than
// that fraction of the pool from real clients.
const LoopbackMaxConns = 1024

// NewPerIPLimiter builds a limiter with the given initial ceiling (0 = disabled)
// and the production loopback ceiling.
func NewPerIPLimiter(max uint32) *PerIPLimiter {
	l := &PerIPLimiter{counts: make(map[string]int), loopbackMax: LoopbackMaxConns}
	l.max.Store(max)
	return l
}

// SetMax hot-swaps the per-IP ceiling (config_changed re-fetch). 0 disables the
// gate (admit all). Already-open connections keep their counted slots; the new
// ceiling applies to the next accept.
func (l *PerIPLimiter) SetMax(max uint32) { l.max.Store(max) }

// tryAcquire admits one connection from ip, returning a release func (call once,
// on Close) and true, or (nil, false) if ip already holds its ceiling — the
// admin's cap for an external source, LoopbackMaxConns for a loopback one. A
// nil IP, or a disabled (max == 0) limiter for a non-loopback source, always
// admits with a no-op release (fail-open: a malformed source never wedges the
// listener). Loopback is counted regardless of max — the safety bound is not
// the abuse knob.
func (l *PerIPLimiter) tryAcquire(ip net.IP) (func(), bool) {
	if ip == nil {
		return noopRelease, true
	}
	key := ip.String()
	l.mu.Lock()
	defer l.mu.Unlock()
	ceiling := l.ceilingForLocked(ip)
	if ceiling == 0 {
		return noopRelease, true // disabled (non-loopback only)
	}
	if l.counts[key] >= ceiling {
		return nil, false
	}
	l.counts[key]++
	return func() { l.release(key) }, true
}

// CeilingFor reports the ceiling ip is counted against: the admin's live cap
// (0 = disabled) for an external source, LoopbackMaxConns for a loopback one.
// A shed log line should carry this, not the admin's cap, so a loopback shed
// is never misreported as the admin's cap biting.
func (l *PerIPLimiter) CeilingFor(ip net.IP) int {
	return l.ceilingForLocked(ip) // reads only the atomic + an immutable field
}

func (l *PerIPLimiter) ceilingForLocked(ip net.IP) int {
	if ip != nil && ip.IsLoopback() {
		return l.loopbackMax
	}
	return int(l.max.Load())
}

func (l *PerIPLimiter) release(key string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if n := l.counts[key]; n > 1 {
		l.counts[key] = n - 1
	} else {
		delete(l.counts, key) // GC: no unbounded growth from rotating IPs
	}
}

func noopRelease() {}

// trackedIPs reports the count-map size (test/diagnostic).
func (l *PerIPLimiter) trackedIPs() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return len(l.counts)
}

// PerIPListener wraps a net.Listener so each accepted connection is admitted
// against a (process-shared) PerIPLimiter keyed on the connection's source IP.
// Over-cap connections are SHED — closed immediately, the accept loop continues
// to the next connection — rather than blocked (unlike the global Listener,
// whose Accept back-pressures): a per-IP abuser must not stall service for
// every other source. onShed fires once per shed connection (nil-safe, for the
// per_ip_shed metric result).
//
// The wrapper overrides only Accept; Close, Addr, and the rest are promoted
// from the embedded net.Listener (so a graceful-drain path closing the wrapped
// listener closes the inner one unchanged).
//
// ⚠ The source IP is read via conn.RemoteAddr(). For the directly-published
// listeners (465/587/993/143) that is the raw TCP peer (immediate, no I/O). For
// the CalDAV-443 listener fronted by the SNI router, the wrapped inner listener
// is an internal/proxyproto.Listener, so RemoteAddr() resolves the
// PROXY-v2-conveyed real client IP — which peels the header at accept time. The
// trusted in-container router writes the header immediately on dial, so this
// does not stall the accept loop in practice (and proxyproto bounds a slow
// header with its own 10s read deadline). Wrap this BELOW the global connlimit
// cap and ABOVE proxyproto, mirroring the nest serve_tls order (global
// semaphore first, then per-IP, then proxy-header resolve — lib.rs serve_tls).
type PerIPListener struct {
	net.Listener
	limiter *PerIPLimiter
	onShed  func()
}

// NewPerIPListener wraps inner so accepted connections are admitted against the
// shared limiter. onShed (nil-safe) fires once per shed connection.
func NewPerIPListener(inner net.Listener, limiter *PerIPLimiter, onShed func()) *PerIPListener {
	return &PerIPListener{Listener: inner, limiter: limiter, onShed: onShed}
}

// Accept returns the next connection admitted by the per-IP limiter, shedding
// (closing + onShed) any whose source IP is already at the cap and accepting
// the next, so a per-IP flood never blocks the accept loop.
func (l *PerIPListener) Accept() (net.Conn, error) {
	for {
		c, err := l.Listener.Accept()
		if err != nil {
			return nil, err
		}
		if release, ok := l.limiter.tryAcquire(ipOf(c.RemoteAddr())); ok {
			return &peripConn{Conn: c, release: release}, nil
		}
		_ = c.Close()
		if l.onShed != nil {
			l.onShed()
		}
		// shed: loop to accept the next connection
	}
}

// ipOf extracts the net.IP from a RemoteAddr, or nil if it can't be parsed
// (treated as exempt — fail-open, like loopback).
func ipOf(addr net.Addr) net.IP {
	switch a := addr.(type) {
	case *net.TCPAddr:
		return a.IP
	case nil:
		return nil
	default:
		host, _, err := net.SplitHostPort(addr.String())
		if err != nil {
			return nil
		}
		return net.ParseIP(host)
	}
}

// peripConn releases its per-IP slot exactly once, on the first Close.
type peripConn struct {
	net.Conn
	once    sync.Once
	release func()
}

func (c *peripConn) Close() error {
	err := c.Conn.Close()
	c.once.Do(c.release)
	return err
}
