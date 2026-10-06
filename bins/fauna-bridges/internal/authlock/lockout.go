// Package authlock is the shared AUTH-failure lockout used by every
// authenticated mail surface: SMTP submission (MTA), IMAP, and CalDAV (MDA). It
// was lifted out of `internal/mta` so the MDA's IMAP/CalDAV auth paths get the
// same online-guessing brake the submission path already had (2026-06-01
// security review § M1) without the MDA importing the whole MTA package.
//
// Shape: fixed-window failure counters, mutex-guarded map, Clock-driven for
// tests. The auth workhorse queries IsLockedFor before any nest call / KDF, so
// a failed attempt past a ceiling is refused *before* the expensive Argon2id
// AEAD-unwrap — denying the attacker both an AEAD-timing oracle and the
// CPU/memory cost of the KDF.
//
// Three counters per attempt (2026-06-23 firewall-exposure review § F11 —
// distributed/rotating brute-force evasion):
//
//   - **fine** `(principal, credential_id, source_ip)` @ the admin base
//     limit — the original per-triple backstop.
//   - **mid** `(principal, credential_id)` @ base × midMultiplier —
//     aggregates one credential's failures across ALL source IPs, so a botnet
//     guessing one victim's credential from many IPs trips a single bucket
//     (which the per-triple fine bucket never would).
//
// `principal` is the **canonical** `base@domain` identity the caller derives via
// `auth.PrincipalKey` (NOT the raw presented username), so every presented form
// that resolves to one credential — `alice`, `alice+`, `alice+default`,
// `alice@<primary>` — shares a single bucket instead of each opening a fresh one
// (2026-06-24 email-component-compromise review § B7). `credential_id` stays a
// separate key dimension, so distinct credentials keep distinct allowances.
//   - **coarse** `(source_ip)` @ base × ipMultiplier — aggregates all usernames
//     from one IP, so a password-spray of many usernames from one IP trips a
//     single bucket.
//
// IsLockedFor locks if ANY of the three has reached its ceiling; RecordFailureFor
// increments all three; ResetFor (on success) clears the fine + mid buckets but
// deliberately NOT the coarse IP bucket (see ResetFor).
//
// Catalog row: `docs/goal/behavior/mail-policy-config.md` § Submission policy —
// `mail.auth.max_auth_failures_per_minute`, default 30 (the base). Wire mirror:
// `AuthPolicy.max_auth_failures_per_minute`. `0` disables the whole gate. The
// coarse ceilings derive from the base via the hard-coded multipliers below —
// not separate admin knobs (a value no human
// chooses is a constant, never a config surface).
package authlock

import (
	"sync"
	"time"
)

// Clock is the minimal time source the lockout needs; production passes a
// realClock, tests pass a deterministic fake. Any type with `Now() time.Time`
// satisfies it (so the MTA's own `Clock` works structurally).
type Clock interface {
	Now() time.Time
}

type realClock struct{}

func (realClock) Now() time.Time { return time.Now() }

// sweepThreshold is the bucket count past which RecordFailureFor opportunistically
// reclaims expired buckets. Bounds steady-state memory without a background
// goroutine: a flood of distinct keys — the unbounded-map vector the security
// review flagged (§ D8) — grows the map only until the next RecordFailureFor past
// the threshold, which drops every bucket whose window has elapsed. (Within-window
// growth is separately bounded upstream by the per-source rate limits and
// connection caps.) Sweeping only *expired* buckets never evicts an active
// lockout, so an attacker can't flush a live lock by flooding fresh keys.
const sweepThreshold = 4096

// Coarse-bucket ceiling multipliers (× the admin base limit) — defense-in-depth
// against a brute force that rotates whatever discriminator the fine bucket keys
// on (§ F11). They are constants, never config: no human chooses them, so
// they are hard-coded, not a knob.
//
//   - midMultiplier: the (username|credential) bucket aggregates one credential
//     across all source IPs. A legitimate credential rarely fails even across a
//     handful of devices, so 3× the base is generous headroom while still
//     tripping a many-IP botnet long before it can exhaust a high-entropy token.
//   - ipMultiplier: the (source IP) bucket aggregates all usernames from one IP.
//     Kept generous (10×) so shared NAT / CGNAT — many legitimate users behind a
//     single address — does not lock out real users on incidental failures, while
//     a username spray of thousands still trips it; the 1-minute window
//     self-heals any transient false positive.
const (
	midMultiplier uint32 = 3
	ipMultiplier  uint32 = 10
)

// Lockout holds the three fixed-window AUTH-failure counters in one map,
// distinguished by a per-class key prefix. `base == 0` is the disabled sentinel.
type Lockout struct {
	base   uint32
	window time.Duration
	clock  Clock

	mu      sync.Mutex
	buckets map[string]*bucket
}

type bucket struct {
	windowAt time.Time
	failures uint32
}

// New constructs a lockout with the given per-window base failure limit, window
// duration, and Clock. `limit == 0` returns a sentinel that IsLockedFor
// short-circuits to false and RecordFailureFor no-ops (the "disabled" catalog
// default / test opt-out). A nil clock defaults to the real clock.
func New(limit uint32, window time.Duration, clk Clock) *Lockout {
	if clk == nil {
		clk = realClock{}
	}
	return &Lockout{
		base:    limit,
		window:  window,
		clock:   clk,
		buckets: make(map[string]*bucket),
	}
}

// Per-class key builders. The class prefix (one byte + NUL) keeps the three
// namespaces disjoint in the single map; the `|` joiners can't legitimately
// appear in their components (the canonical `base@domain` principal, a
// bridge-issued credential_id, a stringified net.IP).
func fineKey(principal, credentialID, sourceIP string) string {
	return "f\x00" + principal + "|" + credentialID + "|" + sourceIP
}

func midKey(principal, credentialID string) string {
	return "m\x00" + principal + "|" + credentialID
}

func ipKey(sourceIP string) string {
	return "i\x00" + sourceIP
}

// satMul multiplies, saturating at uint32 max instead of wrapping — so a
// pathologically large admin base can never wrap a coarse ceiling down to a
// tiny (over-aggressive) value.
func satMul(a, b uint32) uint32 {
	if a == 0 || b == 0 {
		return 0
	}
	if a > ^uint32(0)/b {
		return ^uint32(0)
	}
	return a * b
}

// IsLockedFor reports whether this AUTH attempt has reached the failure ceiling
// of ANY of its three buckets within the current window. A nil receiver or
// zero-limit lockout is never locked.
func (l *Lockout) IsLockedFor(principal, credentialID, sourceIP string) bool {
	if l == nil || l.base == 0 {
		return false
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	now := l.clock.Now()
	return l.lockedLocked(fineKey(principal, credentialID, sourceIP), l.base, now) ||
		l.lockedLocked(midKey(principal, credentialID), satMul(l.base, midMultiplier), now) ||
		l.lockedLocked(ipKey(sourceIP), satMul(l.base, ipMultiplier), now)
}

// RecordFailureFor increments all three of this attempt's counters, rolling each
// window over if its previous failure was past `window` ago. A nil receiver or
// zero-limit lockout is a no-op. Opportunistically reclaims expired buckets once
// the map grows past sweepThreshold (§ D8).
func (l *Lockout) RecordFailureFor(principal, credentialID, sourceIP string) {
	if l == nil || l.base == 0 {
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	now := l.clock.Now()
	l.recordLocked(fineKey(principal, credentialID, sourceIP), now)
	l.recordLocked(midKey(principal, credentialID), now)
	l.recordLocked(ipKey(sourceIP), now)
}

// ResetFor clears this attempt's fine (user, credential, IP) and mid
// (user, credential) buckets — fired on every successful AUTH so a legitimate
// user isn't locked out by earlier typos, and so a credential owner who proves
// liveness clears the cross-IP failure noise for their own credential.
//
// It deliberately does NOT clear the coarse (source IP) bucket: otherwise one
// valid login from a shared NAT / CGNAT address would flush the password-spray
// counter for every other user behind that IP, defeating § F11's spray defense.
// The IP bucket self-heals on its 1-minute window instead. Nil/zero-limit is a
// no-op.
func (l *Lockout) ResetFor(principal, credentialID, sourceIP string) {
	if l == nil || l.base == 0 {
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	delete(l.buckets, fineKey(principal, credentialID, sourceIP))
	delete(l.buckets, midKey(principal, credentialID))
}

// lockedLocked reports whether `key` has reached `limit` within the current
// window. Caller holds l.mu. A read on an expired bucket stays consistently
// false (RecordFailureFor rolls the window over on the next failure).
func (l *Lockout) lockedLocked(key string, limit uint32, now time.Time) bool {
	b, ok := l.buckets[key]
	if !ok {
		return false
	}
	if now.Sub(b.windowAt) >= l.window {
		return false
	}
	return b.failures >= limit
}

// recordLocked increments `key`'s counter, rolling the window over if the
// previous failure was past `window` ago, creating the bucket on first use (and
// sweeping expired buckets once the map is past the threshold). Caller holds l.mu.
func (l *Lockout) recordLocked(key string, now time.Time) {
	b, ok := l.buckets[key]
	if !ok {
		if len(l.buckets) >= sweepThreshold {
			l.sweepExpiredLocked(now)
		}
		b = &bucket{windowAt: now}
		l.buckets[key] = b
	}
	if now.Sub(b.windowAt) >= l.window {
		b.windowAt = now
		b.failures = 0
	}
	b.failures++
}

// sweepExpiredLocked drops every bucket whose window has elapsed. Caller holds
// l.mu. Only expired buckets go, so a live lockout is never reclaimed.
func (l *Lockout) sweepExpiredLocked(now time.Time) {
	for k, b := range l.buckets {
		if now.Sub(b.windowAt) >= l.window {
			delete(l.buckets, k)
		}
	}
}
