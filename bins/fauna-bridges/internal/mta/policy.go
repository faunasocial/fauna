// Phase C.2 connection-time policy primitives.
//
// Five gates that fire on the inbound SMTP socket before any
// recipient-validate or message-parse work happens:
//
//  1. Per-peer-IP rate limit — token bucket over a configurable window.
//     Exhausting the bucket returns 421 4.7.0 at NewSession.
//  2. DNSBL lookup — query each configured RBL for the peer IP's
//     reversed-zone A record; Spamhaus-style 127.0.0.2-9 codes reject
//     (550 5.7.1), PBL codes (127.0.0.10/.11) score-only, resolver
//     errors fail open.
//  3. Greylist — first-seen tuples tempfail with 451 4.7.1. Enforced
//     **nest-side** (`fauna.bridges.check_greylist`); the state is not in
//     this process so it survives bridge restart (smtp-server.md §
//     Greylisting). The bridge calls it at RCPT TO (see server.go).
//  4. FCrDNS — PTR + forward-A round-trip; modes off/score_signal/
//     enforce drive accept-with-counter vs 550 5.7.25.
//  5. HELO identity — DNS check that the HELO domain A-resolves to a
//     set including the peer IP; production default rejects non-
//     loopback mismatches with 554 5.7.0.
//
// All five are tested with a fake clock (`fakeClock`) + fake DNS
// (`fakeResolver`) so the unit tests are hermetic. Production wiring
// uses `time.Now`-backed `realClock{}` and `net.DefaultResolver`.
//
// Legacy reference (retired; see git history): `bins/fauna-bridge-imap/internal/smtp/{dnsbl,
// fcrdns,policy,ratelimit}.go`. Shape lifted; env-var reads dropped
// (policy comes from nest via WS-RPC per the bridge-config invariant).
//
// Logging: package-level `slog.Default()` at Warn for fail-open paths
// so admin observability survives. The metrics surface lands in a
// follow-up that wires Prometheus counters next to the existing
// MetricInbound* in `internal/metrics`; Phase C.2 stays log-only.
package mta

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// ── Clock + DNS resolver interfaces ────────────────────────────────

// Clock is the time source used by the RateLimiter. Production callers pass
// `realClock{}`; tests pass `fakeClock`.
type Clock interface {
	Now() time.Time
}

// realClock is the production Clock — delegates to `time.Now`.
type realClock struct{}

func (realClock) Now() time.Time { return time.Now() }

// DNSResolver is the union of the lookups the connection-time + envelope-
// time gates need. The stdlib `*net.Resolver` satisfies this interface
// (each method has the matching signature). Tests inject `fakeResolver` /
// `erroringResolver`. LookupMX is consumed only by SenderDomainChecker.
type DNSResolver interface {
	LookupAddr(ctx context.Context, addr string) ([]string, error)
	LookupHost(ctx context.Context, host string) ([]string, error)
	LookupIPAddr(ctx context.Context, host string) ([]net.IPAddr, error)
	LookupMX(ctx context.Context, name string) ([]*net.MX, error)
}

// ── Per-peer-IP rate limit ─────────────────────────────────────────

// RateLimiter is a per-IP fixed-window connection counter. Up to
// `limit` Allow() calls per IP per `window` return true; the
// `(limit+1)`-th call returns false until the window rolls.
//
// Fixed-window matches the legacy `bins/fauna-bridge-imap/internal/smtp/
// ratelimit.go` shape (retired; see git
// history); sliding-window is unnecessary for this gate
// since the goal is "reject obvious botnets / connection floods", not
// strict QoS.
type RateLimiter struct {
	limit  uint32
	window time.Duration
	clock  Clock

	mu      sync.Mutex
	buckets map[string]*rateBucket
}

type rateBucket struct {
	windowAt time.Time
	count    uint32
}

// NewRateLimiter constructs a rate limiter with the given limit
// (allowed calls per window), window duration, and Clock.
//
// A limit of 0 DISABLES the gate (admit everything) — see Allow. That is the
// same sentinel its sibling per-IP knob uses (`internal/connlimit`'s
// PerIPLimiter, "a max of 0 means DISABLED"), so the two per-IP knobs sitting
// beside each other in the admin pane cannot mean opposite things.
func NewRateLimiter(limit uint32, window time.Duration, clk Clock) *RateLimiter {
	if clk == nil {
		clk = realClock{}
	}
	return &RateLimiter{
		limit:   limit,
		window:  window,
		clock:   clk,
		buckets: make(map[string]*rateBucket),
	}
}

// Allow returns true when the per-IP bucket has room for another
// connection, false when the cap is reached. Roll-over fires when the
// trailing-window age crosses `window` (fixed-window reset, not
// sliding).
//
// A limit of 0 means the gate is DISABLED and everything is admitted — the
// `0 = disabled` sentinel every protection knob in the mail-policy catalog
// uses (`mail-policy-config.md` § Inbound perimeter). Without this branch the
// `count >= limit` test below would trip on the FIRST connection from every IP
// (count starts at 0), and since server.go applies it at NewSession, one typed
// `0` in the admin pane would 421 every inbound sender before the banner — a
// total, silent inbound-mail outage. Ruled by sweep.
func (rl *RateLimiter) Allow(ip string) bool {
	rl.mu.Lock()
	defer rl.mu.Unlock()

	if rl.limit == 0 {
		return true // disabled
	}

	now := rl.clock.Now()
	b, ok := rl.buckets[ip]
	if !ok {
		b = &rateBucket{windowAt: now}
		rl.buckets[ip] = b
	}
	if now.Sub(b.windowAt) >= rl.window {
		b.windowAt = now
		b.count = 0
	}
	if b.count >= rl.limit {
		return false
	}
	b.count++
	return true
}

// ── DNSBL ──────────────────────────────────────────────────────────

// DNSBLAction tells the bridge what to do with a DNSBL hit.
type DNSBLAction int

const (
	// DNSBLActionReject — definite badness; reject the session at 550 5.7.1.
	DNSBLActionReject DNSBLAction = iota
	// DNSBLActionScore — soft signal; accept the session, surface the
	// reason to the scorer (Phase C.7).
	DNSBLActionScore
	// DNSBLActionIgnore — a resolver/error response, NOT a listing. Spamhaus
	// (RFC 5782 §2.3) returns the 127.255.255.0/24 range for query errors —
	// e.g. 127.255.255.254 "query refused: public/open resolver",
	// 127.255.255.252 anonymous-query, 127.255.255.255 rate-limited. These must
	// fail open (neither reject nor score); treating them as a hit rejects every
	// legitimate sender when the box resolves DNSBLs through a public resolver.
	DNSBLActionIgnore
)

// DNSBLPolicy maps a DNSBL's response codes to actions. Unknown codes
// fall back to DefaultAction.
type DNSBLPolicy struct {
	DefaultAction DNSBLAction
	ResponseCodes map[string]DNSBLAction
}

// LookupAction returns the action for `addr`. Non-IPv4 input falls back
// to DefaultAction (Spamhaus zones publish IPv4 codes; an IPv6 response
// is unexpected, safer to fall through).
func (p DNSBLPolicy) LookupAction(addr net.IP) DNSBLAction {
	if addr == nil || addr.To4() == nil {
		return p.DefaultAction
	}
	v4 := addr.To4()
	// Error sentinels (Spamhaus / RFC 5782 §2.3) live in 127.255.255.0/24 — they
	// signal a failed/refused query (e.g. .254 = "via public resolver"), NOT a
	// listing. Fail open here BEFORE the DefaultAction=Reject fall-through, or a
	// box querying through a public resolver rejects all legitimate inbound.
	if v4[0] == 127 && v4[1] == 255 && v4[2] == 255 {
		return DNSBLActionIgnore
	}
	if a, ok := p.ResponseCodes[v4.String()]; ok {
		return a
	}
	return p.DefaultAction
}

// ZenSpamhausPolicy pins the response-code routing for zen.spamhaus.org
// per https://www.spamhaus.org/zen/ and RFC 5782 §2.3.
//
//	127.0.0.2  SBL       — definite badness, reject
//	127.0.0.3  SBL CSS   — definite badness, reject
//	127.0.0.4  XBL CBL   — definite badness, reject
//	127.0.0.5  XBL NJABL — definite badness, reject (historical)
//	127.0.0.6  XBL       — definite badness, reject
//	127.0.0.7  XBL       — definite badness, reject
//	127.0.0.9  SBL DROP/EDROP — definite badness, reject
//	127.0.0.10 PBL ISP-managed — residential / dynamic IP, score-only
//	127.0.0.11 PBL non-ISP-managed — residential / dynamic IP, score-only
var ZenSpamhausPolicy = DNSBLPolicy{
	DefaultAction: DNSBLActionReject,
	ResponseCodes: map[string]DNSBLAction{
		"127.0.0.2":  DNSBLActionReject,
		"127.0.0.3":  DNSBLActionReject,
		"127.0.0.4":  DNSBLActionReject,
		"127.0.0.5":  DNSBLActionReject,
		"127.0.0.6":  DNSBLActionReject,
		"127.0.0.7":  DNSBLActionReject,
		"127.0.0.9":  DNSBLActionReject,
		"127.0.0.10": DNSBLActionScore,
		"127.0.0.11": DNSBLActionScore,
	},
}

// dnsblDefaultPolicies maps known DNSBL hostnames to their routing.
// Lists not in the registry fall back to dnsblUnknownPolicy.
var dnsblDefaultPolicies = map[string]DNSBLPolicy{
	"zen.spamhaus.org": ZenSpamhausPolicy,
}

// dnsblUnknownPolicy applies to any DNSBL hostname not in
// `dnsblDefaultPolicies`. Safe default: any non-zero A-record response
// → Reject. Whoever wants PBL-class soft routing on a new list needs
// a code change here.
var dnsblUnknownPolicy = DNSBLPolicy{DefaultAction: DNSBLActionReject}

// DNSBLResult is the structured outcome of a DNSBL.Check.
type DNSBLResult struct {
	Rejected     bool
	RejectReason string // "zen.spamhaus.org (127.0.0.2)"
	// (T3.1) Score-class hits are deliberately inert: rspamd is the sole
	// deployment-wide content scorer and its RBL module covers score-class
	// signals, so a score-class response's only effect here is NOT rejecting.
	// The reject-class verdict above stays a perimeter hard-gate.
}

// DNSBLChecker queries one or more RBLs for a peer IP and classifies
// the responses via each list's DNSBLPolicy.
type DNSBLChecker struct {
	Servers  []string
	Policies map[string]DNSBLPolicy
	Resolver DNSResolver
}

// NewDNSBLChecker constructs a checker with the registered policies
// (zen.spamhaus.org gets the curated routing; others fall back to
// reject-any-hit).
func NewDNSBLChecker(servers []string, resolver DNSResolver) *DNSBLChecker {
	policies := make(map[string]DNSBLPolicy, len(servers))
	for _, s := range servers {
		s = strings.TrimSuffix(s, ".")
		if p, ok := dnsblDefaultPolicies[s]; ok {
			policies[s] = p
		} else {
			policies[s] = dnsblUnknownPolicy
		}
	}
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	return &DNSBLChecker{
		Servers:  servers,
		Policies: policies,
		Resolver: resolver,
	}
}

// Check looks up `ip` in every configured RBL and applies the routing
// policy. Returns the structured outcome; resolver errors fail open
// (logged at Warn) and contribute neither a reject nor a score reason.
func (c *DNSBLChecker) Check(ip string) DNSBLResult {
	parsed := net.ParseIP(ip)
	if parsed == nil || parsed.To4() == nil {
		// IPv6 not in scope; legacy treated the same.
		return DNSBLResult{}
	}
	octets := parsed.To4()
	reversed := fmt.Sprintf("%d.%d.%d.%d", octets[3], octets[2], octets[1], octets[0])

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	var result DNSBLResult
	for _, server := range c.Servers {
		server = strings.TrimSuffix(server, ".")
		query := reversed + "." + server
		addrs, err := c.Resolver.LookupIPAddr(ctx, query)
		if err != nil {
			var dnsErr *net.DNSError
			if errors.As(err, &dnsErr) && dnsErr.IsNotFound {
				// Not listed — normal clean path.
				continue
			}
			slog.Warn("DNSBL lookup failed; failing open",
				"client_ip", ip, "server", server, "error", err.Error())
			continue
		}
		if len(addrs) == 0 {
			continue
		}
		c.classifyHits(server, addrs, &result)
		if result.Rejected {
			// Reject-class hit found — short-circuit the remaining
			// lists. Score reasons collected so far stay on the result.
			return result
		}
	}
	return result
}

// classifyHits applies `server`'s policy to each response IP and
// folds the verdicts into `out`. Sorted iteration keeps the output
// deterministic for tests.
func (c *DNSBLChecker) classifyHits(server string, addrs []net.IPAddr, out *DNSBLResult) {
	policy, ok := c.Policies[server]
	if !ok {
		policy = dnsblUnknownPolicy
	}
	sorted := make([]net.IPAddr, len(addrs))
	copy(sorted, addrs)
	sort.Slice(sorted, func(i, j int) bool {
		return sorted[i].IP.String() < sorted[j].IP.String()
	})
	for _, addr := range sorted {
		action := policy.LookupAction(addr.IP)
		switch action {
		case DNSBLActionReject:
			if !out.Rejected {
				out.Rejected = true
				out.RejectReason = fmt.Sprintf("%s (%s)", server, addr.IP.String())
			}
		case DNSBLActionScore:
			// Deliberately inert (T3.1) — see DNSBLResult: the score class
			// exists only so these responses do not reject.
		}
	}
}

// Greylisting moved **nest-side**: the
// `(sender_domain, recipient, subnet)` tuple-key + defer/pass decision now
// live in shared `fauna_mail::greylist`, persisted in nest's `greylist_tuples`
// and queried via `fauna.bridges.check_greylist` (smtp-server.md §
// Greylisting). The in-process map that lived here was wiped on every
// supervisor-restart; the nest-side version is uniform across restart.

// ── FCrDNS ─────────────────────────────────────────────────────────

// FCrDNSMode controls how the FCrDNS check affects the session.
type FCrDNSMode int

const (
	// FCrDNSModeOff disables the check entirely.
	FCrDNSModeOff FCrDNSMode = iota
	// FCrDNSModeScoreSignal runs the check and forwards the verdict to
	// the scorer; never rejects the session.
	FCrDNSModeScoreSignal
	// FCrDNSModeEnforce rejects sessions whose FCrDNS check fails (and
	// the policy carries reject_fcrdns_fail) with 550 5.7.25 at MAIL
	// FROM. Resolver errors always fail open.
	FCrDNSModeEnforce
)

// ParseFCrDNSMode parses the wire-shape string. Empty or unknown
// values default to ModeScoreSignal (the safe-by-default for an
// observability-first policy).
func ParseFCrDNSMode(s string) FCrDNSMode {
	switch wsrpc.FCrDNSModeWire(s) {
	case "":
		return FCrDNSModeScoreSignal
	case wsrpc.FCrDNSModeWireOff:
		return FCrDNSModeOff
	case wsrpc.FCrDNSModeWireScoreSignal:
		return FCrDNSModeScoreSignal
	case wsrpc.FCrDNSModeWireEnforce:
		return FCrDNSModeEnforce
	default:
		slog.Warn("unknown FCrDNS mode; defaulting to score_signal", "mode", s)
		return FCrDNSModeScoreSignal
	}
}

// FCrDNSResult is the structured outcome of FCrDNSChecker.Check.
type FCrDNSResult struct {
	Passed   bool   // forward A includes the peer IP
	PTRName  string // first PTR returned, trailing dot stripped
	Reason   string // human-readable failure description; empty on pass
	FailOpen bool   // resolver error — sessionn always accepted
}

// FCrDNSChecker performs forward-confirmed reverse DNS checks.
type FCrDNSChecker struct {
	Resolver DNSResolver
}

// NewFCrDNSChecker constructs the checker against the given resolver
// (or `net.DefaultResolver` when nil).
func NewFCrDNSChecker(resolver DNSResolver) *FCrDNSChecker {
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	return &FCrDNSChecker{Resolver: resolver}
}

// Check resolves the PTR for `ip`, then the forward A on the PTR name,
// and reports whether the forward set includes the peer IP. Resolver
// errors always fail open.
func (c *FCrDNSChecker) Check(ip string) FCrDNSResult {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	ptrs, err := c.Resolver.LookupAddr(ctx, ip)
	if err != nil {
		var dnsErr *net.DNSError
		if errors.As(err, &dnsErr) && dnsErr.IsNotFound {
			return FCrDNSResult{Passed: false, Reason: "no PTR record"}
		}
		slog.Warn("FCrDNS PTR lookup failed; failing open",
			"client_ip", ip, "error", err.Error())
		return FCrDNSResult{Passed: false, FailOpen: true, Reason: "resolver error: " + err.Error()}
	}
	if len(ptrs) == 0 {
		return FCrDNSResult{Passed: false, Reason: "no PTR record"}
	}
	ptrName := strings.TrimSuffix(ptrs[0], ".")

	ctx2, cancel2 := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel2()
	addrs, err := c.Resolver.LookupHost(ctx2, ptrName)
	if err != nil {
		var dnsErr *net.DNSError
		if errors.As(err, &dnsErr) && dnsErr.IsNotFound {
			return FCrDNSResult{Passed: false, PTRName: ptrName, Reason: "PTR forward record not found"}
		}
		slog.Warn("FCrDNS forward lookup failed; failing open",
			"client_ip", ip, "ptr_name", ptrName, "error", err.Error())
		return FCrDNSResult{Passed: false, FailOpen: true, Reason: "resolver error: " + err.Error()}
	}
	for _, addr := range addrs {
		if addr == ip {
			return FCrDNSResult{Passed: true, PTRName: ptrName}
		}
	}
	return FCrDNSResult{Passed: false, PTRName: ptrName, Reason: "forward set does not include peer IP"}
}

// ── HELO identity ──────────────────────────────────────────────────

// ValidateHELOIdentity returns (ok, failOpen, reason) for whether the
// HELO/EHLO domain DNS-resolves to a set including `clientIP`.
//
// Loopback peers (127.0.0.0/8, ::1) are exempt by default so local test
// harnesses keep working; production callers pass
// `skipLoopbackExemption=false`. **Porting hazard:** when
// `skipLoopbackExemption=true` *production* callers pass `false`
// (loopback-exemption ON); the fake-DNS e2e harness passes `true` to
// drive the check from 127.0.0.1. Drop the bool and every fake-DNS
// test would still pass while production silently bypasses non-
// loopback rejections.
//
// Bracket-form IP literals (`[1.2.3.4]`) are also exempt — the
// syntactic check in `ValidateHELO` (Phase C.2 keeps that legacy shape
// inline at Session.Mail) handles them, and a parallel DNS check on a
// literal would always tempfail without information value.
//
// Resolver errors fail open with `ok=true, failOpen=true` so a DNS
// outage doesn't reject otherwise-legitimate mail.
func ValidateHELOIdentity(helo, clientIP string, resolver DNSResolver, skipLoopbackExemption bool, timeout time.Duration) (ok bool, failOpen bool, reason string) {
	helo = strings.TrimSpace(helo)
	if strings.HasPrefix(helo, "[") && strings.HasSuffix(helo, "]") {
		return true, false, ""
	}
	if !skipLoopbackExemption && isLoopbackIP(clientIP) {
		return true, false, ""
	}
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	if timeout == 0 {
		timeout = 3 * time.Second
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	addrs, err := resolver.LookupHost(ctx, helo)
	if err != nil {
		var dnsErr *net.DNSError
		if errors.As(err, &dnsErr) && dnsErr.IsNotFound {
			return false, false, "HELO domain has no A/AAAA record"
		}
		slog.Warn("HELO identity lookup failed; failing open",
			"helo", helo, "client_ip", clientIP, "error", err.Error())
		return true, true, "resolver error: " + err.Error()
	}
	for _, a := range addrs {
		if a == clientIP {
			return true, false, ""
		}
	}
	return false, false, "HELO domain does not resolve to peer IP"
}

// isLoopbackIP returns true when `s` is in 127.0.0.0/8 or is ::1.
// Unparseable input returns false (we don't accidentally exempt
// sessions whose peer-IP capture failed).
func isLoopbackIP(s string) bool {
	if s == "" {
		return false
	}
	ip := net.ParseIP(s)
	return ip != nil && ip.IsLoopback()
}

// validateHELOSyntax is the cheap, no-DNS HELO/EHLO check that fires
// before ValidateHELOIdentity. Rules (lifted from legacy
// `bins/fauna-bridge-imap/internal/smtp/policy.go::ValidateHELO`, retired
// (see git history):
//
//   - empty / whitespace-only → reject
//   - any non-printable ASCII / control char → reject
//   - bare `localhost` / `localhost.localdomain` from non-loopback peer → reject
//   - bare hostname (no `.`) from non-loopback peer → reject
//   - IP-literal `[1.2.3.4]` whose address ≠ peer IP → reject
//
// Loopback peers (127.0.0.0/8, ::1) get a pass on the FQDN/localhost
// rules so local test harnesses keep working.
func validateHELOSyntax(helo, clientIP string) error {
	helo = strings.TrimSpace(helo)
	if helo == "" {
		return fmt.Errorf("empty hostname")
	}
	for _, r := range helo {
		if r < 0x20 || r > 0x7e {
			return fmt.Errorf("non-printable or non-ASCII byte")
		}
	}
	loopback := isLoopbackIP(clientIP)
	// IP literal: `[a.b.c.d]` or `[IPv6:::1]`. RFC 5321 §4.1.3.
	if strings.HasPrefix(helo, "[") && strings.HasSuffix(helo, "]") {
		inner := helo[1 : len(helo)-1]
		inner = strings.TrimPrefix(inner, "IPv6:")
		ip := net.ParseIP(inner)
		if ip == nil {
			return fmt.Errorf("malformed IP literal %q", helo)
		}
		peer := net.ParseIP(clientIP)
		if peer == nil || !ip.Equal(peer) {
			return fmt.Errorf("IP literal %s does not match peer %s", ip, clientIP)
		}
		return nil
	}
	lower := strings.ToLower(helo)
	if !loopback && (lower == "localhost" || lower == "localhost.localdomain") {
		return fmt.Errorf("%q from non-loopback peer", helo)
	}
	if !loopback && !strings.Contains(helo, ".") {
		return fmt.Errorf("bare hostname %q (FQDN required)", helo)
	}
	return nil
}

// ── Sender-domain MX/A ─────────────────────────────────────────────

// SenderDomainChecker resolves MX (then A/AAAA fallback) for the
// envelope sender's domain at MAIL FROM. Successful resolution
// authorizes the transaction to continue; an authoritative NXDOMAIN /
// NoData on every record type rejects (smtp-server.md § Sender-domain,
// 550 5.7.1). Lifted from the retired legacy
// `bins/fauna-bridge-imap/internal/smtp/policy.go::SenderDomainChecker`
// (retired; see git history); the env-var knob is
// dropped (this is an unconditional compile-time defense — the policy
// catalog has no sender-domain row).
//
// Fail-open posture: any resolver error (timeout, SERVFAIL, network)
// returns ok=true and increments smtp_inbound_sender_domain_fail_open_total
// — never reject otherwise-legitimate mail because of a wonky resolver.
// Mirrors the DNSBL / FCrDNS fail-open posture. The reason label is
// bounded by error class ("timeout" / "resolver_error", smtp-server.md
// § Metrics) — the detailed error rides the warn log, not the label, so
// label cardinality stays bounded.
type SenderDomainChecker struct {
	Resolver      DNSResolver
	CacheTTL      time.Duration
	LookupTimeout time.Duration

	mu    sync.RWMutex
	cache map[string]senderDomainEntry
}

type senderDomainEntry struct {
	ok      bool
	expires time.Time
}

// NewSenderDomainChecker returns a checker with sensible defaults:
// 5-min cache, 3-second lookup timeout. A nil resolver falls back to
// net.DefaultResolver.
func NewSenderDomainChecker(resolver DNSResolver) *SenderDomainChecker {
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	return &SenderDomainChecker{
		Resolver:      resolver,
		CacheTTL:      5 * time.Minute,
		LookupTimeout: 3 * time.Second,
		cache:         make(map[string]senderDomainEntry),
	}
}

// CheckSenderDomain returns ok=true (authorized to continue) when the
// domain has any MX, A, or AAAA record. Returns ok=false with a short
// reason when DNS authoritatively says the domain has no mail-receiving
// records. On resolver error it fails OPEN (ok=true) — incrementing the
// fail-open counter and warn-logging — so a transient DNS glitch never
// rejects legitimate mail.
func (c *SenderDomainChecker) CheckSenderDomain(domain string) (ok bool, reason string) {
	domain = strings.ToLower(strings.TrimSpace(domain))
	if domain == "" {
		return false, "empty domain"
	}
	c.mu.RLock()
	if entry, hit := c.cache[domain]; hit && time.Now().Before(entry.expires) {
		c.mu.RUnlock()
		if entry.ok {
			return true, ""
		}
		return false, "cached: no MX or A records"
	}
	c.mu.RUnlock()

	ok, reason, hadError := c.lookup(domain)
	// Don't cache fail-open results — the next lookup may succeed and we
	// don't want a transient resolver glitch to silently authorize this
	// domain for the next CacheTTL.
	if !hadError {
		c.mu.Lock()
		c.cache[domain] = senderDomainEntry{ok: ok, expires: time.Now().Add(c.CacheTTL)}
		c.mu.Unlock()
	}
	return ok, reason
}

func (c *SenderDomainChecker) lookup(domain string) (ok bool, reason string, hadError bool) {
	resolver := c.Resolver
	if resolver == nil {
		resolver = net.DefaultResolver
	}
	ctx, cancel := context.WithTimeout(context.Background(), c.LookupTimeout)
	defer cancel()

	mxs, mxErr := resolver.LookupMX(ctx, domain)
	if mxErr == nil && len(mxs) > 0 {
		return true, "", false
	}
	if mxErr != nil && !isDNSNotFound(mxErr) {
		c.failOpen(domain, "MX", mxErr)
		return true, "", true
	}
	// MX absent (or NXDOMAIN). Fall back to A/AAAA — RFC 5321 §5.1: the
	// implicit MX is the address record for the domain itself.
	addrs, aErr := resolver.LookupHost(ctx, domain)
	if aErr == nil && len(addrs) > 0 {
		return true, "", false
	}
	if aErr != nil && !isDNSNotFound(aErr) {
		c.failOpen(domain, "A", aErr)
		return true, "", true
	}
	return false, "no MX or A records", false
}

// failOpen warn-logs the detailed resolver error and increments the
// fail-open counter with a bounded `reason` label (error class, not the
// raw error string — keeps label cardinality bounded per smtp-server.md
// § Metrics).
func (c *SenderDomainChecker) failOpen(domain, stage string, err error) {
	slog.Warn("sender-domain lookup failed; failing open",
		"domain", domain, "stage", stage, "error", err.Error())
	metrics.SMTPSenderDomainFailOpen.WithLabelValues(failOpenReason(err)).Inc()
}

// failOpenReason maps a resolver error to a bounded metric label. A
// net.DNSError timeout reports "timeout"; everything else (SERVFAIL,
// network, refused) is "resolver_error". The label space is therefore
// fixed at two values — the detail lives in the warn log.
func failOpenReason(err error) string {
	var dnsErr *net.DNSError
	if errors.As(err, &dnsErr) && dnsErr.IsTimeout {
		return "timeout"
	}
	return "resolver_error"
}

func isDNSNotFound(err error) bool {
	var dnsErr *net.DNSError
	return errors.As(err, &dnsErr) && dnsErr.IsNotFound
}

// ── Policy bundle ──────────────────────────────────────────────────

// Policy bundles the five connection-time gates for the MTA inbound
// listener. Each field is what the per-session `inboundBackend` hooks
// consume on NewSession / Session.Mail / Session.Rcpt.
//
// Nil Policy means "no gates" — the backend behaves like the Phase
// C.1 skeleton. Tests construct a Policy directly with fake clocks
// and resolvers; production constructs it via NewPolicyFromSnapshot.
type Policy struct {
	RateLimiter          *RateLimiter
	DNSBL                *DNSBLChecker
	FCrDNS               *FCrDNSChecker
	FCrDNSMode           FCrDNSMode
	RejectFCrDNSFail     bool
	HELOIdentityRequired bool
	HELOResolver         DNSResolver
	HELOLookupTimeout    time.Duration
	// SenderDomain checks the envelope-sender domain has MX or A/AAAA
	// records at MAIL FROM (smtp-server.md § Sender-domain). Always-on
	// in production (no catalog knob); nil in unit tests that don't
	// exercise the gate.
	SenderDomain *SenderDomainChecker
	// SkipSenderDomainLoopback toggles off the loopback exemption on the
	// sender-domain check. Production keeps this `false` so loopback peers
	// (local relays / cron, mirroring postfix's permit_mynetworks ordering,
	// and the e2e harness on 127.0.0.1) bypass the MX/A lookup. Wire-level
	// tests that need to drive the rejection from a loopback peer set it
	// `true` (mirrors SkipHELOLoopback).
	SkipSenderDomainLoopback bool
	// SkipHELOLoopback toggles off the loopback exemption inside
	// ValidateHELOIdentity. Production keeps this `false` (loopback
	// exemption ON) so local test harnesses still work; the fake-DNS
	// e2e harness sets it to `true` to drive the check from 127.0.0.1.
	SkipHELOLoopback bool
}
