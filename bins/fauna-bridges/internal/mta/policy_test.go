// Phase C.2 connection-time policy tests. Driven by a fake clock and a
// fake DNS resolver so the suite is hermetic (no real time, no real
// DNS). The rate-limit + greylist clocks use Clock (interface-typed);
// the DNSBL + FCrDNS + HELO-identity DNS uses fakeDNSResolver.
package mta

import (
	"context"
	"errors"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

// ── fake clock ─────────────────────────────────────────────────────

// fakeClock is a hermetic test clock. Tests advance it explicitly with
// Advance; concurrent reads via Now() are safe through the mutex.
type fakeClock struct {
	mu  sync.Mutex
	now time.Time
}

func newFakeClock(t time.Time) *fakeClock { return &fakeClock{now: t} }

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) Advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

// ── per-IP rate limit ──────────────────────────────────────────────

func TestRateLimiterAllowsWithinLimit(t *testing.T) {
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	rl := NewRateLimiter(60, time.Minute, clk)
	for i := 0; i < 60; i++ {
		if !rl.Allow("203.0.113.5") {
			t.Fatalf("attempt %d unexpectedly rejected", i+1)
		}
	}
}

func TestRateLimiterRejectsBeyondLimit(t *testing.T) {
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	rl := NewRateLimiter(60, time.Minute, clk)
	for i := 0; i < 60; i++ {
		if !rl.Allow("203.0.113.5") {
			t.Fatalf("attempt %d unexpectedly rejected", i+1)
		}
	}
	if rl.Allow("203.0.113.5") {
		t.Fatal("61st attempt should be rejected by rate limit")
	}
}

func TestRateLimiterResetsAfterWindow(t *testing.T) {
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	rl := NewRateLimiter(60, time.Minute, clk)
	for i := 0; i < 60; i++ {
		_ = rl.Allow("203.0.113.5")
	}
	if rl.Allow("203.0.113.5") {
		t.Fatal("61st pre-advance attempt should be rejected")
	}
	// Advancing past the window must reset the bucket.
	clk.Advance(time.Minute)
	if !rl.Allow("203.0.113.5") {
		t.Fatal("post-window attempt should be accepted (bucket reset)")
	}
}

func TestRateLimiterIsolatesPerIP(t *testing.T) {
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	rl := NewRateLimiter(60, time.Minute, clk)
	for i := 0; i < 60; i++ {
		_ = rl.Allow("203.0.113.5")
	}
	// 203.0.113.5 is now exhausted; a different IP must still be
	// allowed because buckets are per-IP.
	if !rl.Allow("198.51.100.10") {
		t.Fatal("different IP should not be affected by another IP's bucket")
	}
	if rl.Allow("203.0.113.5") {
		t.Fatal("exhausted IP must remain rejected")
	}
}

// A limit of 0 DISABLES the connection-rate gate (admit everything) — it does
// not mean "admit nothing". Sweep (`value-formatting.md` § Mail-knob
// validation ledger): every unsigned mail-policy knob's `0` is either an
// admin-chosen "no allowance" or an off-switch for a protection, and
// `max_conn_per_min` is a protection threshold, so `0` disarms it. The sentinel
// matches its sibling per-IP knob `max_conn_per_ip`
// (`internal/connlimit/perip.go` — "A max of 0 means DISABLED (admit
// everything)"), which sits beside it in the same admin pane.
//
// Before this rule, `Allow` compared `count >= limit` with count starting at 0,
// so a 0 limit rejected the FIRST connection from every IP: `server.go`'s
// NewSession gate turned one typed `0` into a total, silent inbound-mail outage
// (421 before the banner, for every sender, forever).
func TestRateLimiterZeroLimitDisablesTheGate(t *testing.T) {
	clk := newFakeClock(time.Date(2026, 5, 14, 12, 0, 0, 0, time.UTC))
	rl := NewRateLimiter(0, time.Minute, clk)
	for i := 0; i < 1000; i++ {
		if !rl.Allow("203.0.113.5") {
			t.Fatalf("a 0 limit must admit every connection (disabled), rejected at attempt %d", i+1)
		}
	}
	if !rl.Allow("198.51.100.10") {
		t.Fatal("a 0 limit must admit every source IP")
	}
}

// ── DNSBL gate ─────────────────────────────────────────────────────

// fakeResolver implements the minimal LookupAddr / LookupHost / LookupIPAddr
// surface the policy primitives use. Maps are keyed by the lookup
// argument; absence returns an empty slice + nil (matching the "no
// records, no error" branch the production resolver takes when
// authoritative servers return NoData). NXDOMAIN is signalled by
// listing the key in nxAddr / nxHost / nxIP.
type fakeResolver struct {
	ptr    map[string][]string
	host   map[string][]string
	ipAddr map[string][]net.IP  // for DNSBL A-record lookups (reversed IP zone)
	mx     map[string][]*net.MX // for SenderDomainChecker MX lookups
	nxAddr map[string]bool
	nxHost map[string]bool
	nxIP   map[string]bool
	nxMX   map[string]bool
}

func newFakeResolver() *fakeResolver {
	return &fakeResolver{
		ptr:    map[string][]string{},
		host:   map[string][]string{},
		ipAddr: map[string][]net.IP{},
		mx:     map[string][]*net.MX{},
		nxAddr: map[string]bool{},
		nxHost: map[string]bool{},
		nxIP:   map[string]bool{},
		nxMX:   map[string]bool{},
	}
}

func (r *fakeResolver) LookupAddr(_ context.Context, ip string) ([]string, error) {
	if r.nxAddr[ip] {
		return nil, &net.DNSError{Err: "no such host", Name: ip, IsNotFound: true}
	}
	return r.ptr[ip], nil
}

func (r *fakeResolver) LookupHost(_ context.Context, host string) ([]string, error) {
	if r.nxHost[host] {
		return nil, &net.DNSError{Err: "no such host", Name: host, IsNotFound: true}
	}
	return r.host[host], nil
}

func (r *fakeResolver) LookupIPAddr(_ context.Context, host string) ([]net.IPAddr, error) {
	if r.nxIP[host] {
		return nil, &net.DNSError{Err: "no such host", Name: host, IsNotFound: true}
	}
	out := make([]net.IPAddr, 0, len(r.ipAddr[host]))
	for _, ip := range r.ipAddr[host] {
		out = append(out, net.IPAddr{IP: ip})
	}
	return out, nil
}

func (r *fakeResolver) LookupMX(_ context.Context, name string) ([]*net.MX, error) {
	if r.nxMX[name] {
		return nil, &net.DNSError{Err: "no such host", Name: name, IsNotFound: true}
	}
	return r.mx[name], nil
}

// erroringResolver returns a generic non-NotFound DNS error from every
// lookup. Used to exercise the fail-open paths in FCrDNS, HELO, and the
// sender-domain checker.
type erroringResolver struct{}

func (erroringResolver) LookupAddr(_ context.Context, _ string) ([]string, error) {
	return nil, errors.New("resolver: connection refused")
}
func (erroringResolver) LookupHost(_ context.Context, _ string) ([]string, error) {
	return nil, errors.New("resolver: connection refused")
}
func (erroringResolver) LookupIPAddr(_ context.Context, _ string) ([]net.IPAddr, error) {
	return nil, errors.New("resolver: connection refused")
}
func (erroringResolver) LookupMX(_ context.Context, _ string) ([]*net.MX, error) {
	return nil, errors.New("resolver: connection refused")
}

func TestDNSBLAllowsWhenUnlisted(t *testing.T) {
	res := newFakeResolver()
	res.nxIP["5.113.0.203.zen.spamhaus.org"] = true
	chk := NewDNSBLChecker([]string{"zen.spamhaus.org"}, res)
	result := chk.Check("203.0.113.5")
	if result.Rejected {
		t.Fatalf("unlisted IP must not be rejected: %+v", result)
	}
}

func TestDNSBLRejectsListed(t *testing.T) {
	res := newFakeResolver()
	res.ipAddr["5.113.0.203.zen.spamhaus.org"] = []net.IP{net.ParseIP("127.0.0.2")}
	chk := NewDNSBLChecker([]string{"zen.spamhaus.org"}, res)
	result := chk.Check("203.0.113.5")
	if !result.Rejected {
		t.Fatalf("Spamhaus SBL hit (127.0.0.2) must reject: %+v", result)
	}
}

func TestDNSBLErrorSentinelFailsOpen(t *testing.T) {
	// 127.255.255.254 is Spamhaus's "query refused: public/open resolver" error
	// sentinel (RFC 5782 §2.3 reserves 127.255.255.0/24 for errors) — NOT a
	// listing. It must fail open, else a box resolving DNSBLs through a public
	// resolver rejects every legitimate sender (the example.com inbound bounce).
	for _, code := range []string{"127.255.255.252", "127.255.255.254", "127.255.255.255"} {
		res := newFakeResolver()
		res.ipAddr["5.113.0.203.zen.spamhaus.org"] = []net.IP{net.ParseIP(code)}
		chk := NewDNSBLChecker([]string{"zen.spamhaus.org"}, res)
		if result := chk.Check("203.0.113.5"); result.Rejected {
			t.Fatalf("error sentinel %s must NOT reject (fail open): %+v", code, result)
		}
	}
}

func TestDNSBLPBLOnlyScores(t *testing.T) {
	res := newFakeResolver()
	res.ipAddr["10.113.0.203.zen.spamhaus.org"] = []net.IP{net.ParseIP("127.0.0.10")}
	chk := NewDNSBLChecker([]string{"zen.spamhaus.org"}, res)
	result := chk.Check("203.0.113.10")
	if result.Rejected {
		t.Fatal("PBL hit (127.0.0.10) must not reject; score signal only")
	}
	// (T3.1) A PBL (score-class) hit is deliberately inert beyond not
	// rejecting — rspamd's RBL module covers score-class signals; the
	// reject-class verdict stays the perimeter hard-gate (asserted by the
	// reject tests above).
}

func TestDNSBLFailOpenOnResolverError(t *testing.T) {
	chk := NewDNSBLChecker([]string{"zen.spamhaus.org"}, erroringResolver{})
	result := chk.Check("203.0.113.5")
	if result.Rejected {
		t.Fatal("resolver error must fail open (not reject)")
	}
}

// Greylist tests moved to the nest side with the state itself: the
// tuple-key + defer/pass logic is unit-tested
// in `fauna_mail::greylist` (Rust) and the `fauna.bridges.check_greylist`
// handler in `bridge_routing_handlers.rs`; the Go MTA's gate is covered by
// the wsrpc CheckGreylist test + the tier_3 e2e.

// ── FCrDNS ─────────────────────────────────────────────────────────

func TestFCrDNSPassesWhenForwardMatches(t *testing.T) {
	res := newFakeResolver()
	res.ptr["1.2.3.4"] = []string{"host.example."}
	res.host["host.example"] = []string{"1.2.3.4"}
	chk := NewFCrDNSChecker(res)
	r := chk.Check("1.2.3.4")
	if !r.Passed {
		t.Fatalf("forward A includes peer IP must pass: %+v", r)
	}
}

func TestFCrDNSFailsWhenForwardMismatches(t *testing.T) {
	res := newFakeResolver()
	res.ptr["1.2.3.4"] = []string{"host.example."}
	res.host["host.example"] = []string{"5.6.7.8"}
	chk := NewFCrDNSChecker(res)
	r := chk.Check("1.2.3.4")
	if r.Passed {
		t.Fatalf("forward A not including peer IP must fail: %+v", r)
	}
	if r.FailOpen {
		t.Fatal("definite mismatch must not be marked fail-open")
	}
}

func TestFCrDNSFailsOpenOnResolverError(t *testing.T) {
	chk := NewFCrDNSChecker(erroringResolver{})
	r := chk.Check("1.2.3.4")
	if !r.FailOpen {
		t.Fatalf("resolver error must fail open: %+v", r)
	}
}

func TestFCrDNSModeParseDefaultIsScoreSignal(t *testing.T) {
	// Empty string + unknown value → score_signal (the production default).
	for _, in := range []string{"", "what", "ENFORCE"} { // case-sensitive
		if got := ParseFCrDNSMode(in); got != FCrDNSModeScoreSignal {
			t.Errorf("ParseFCrDNSMode(%q) = %v, want score_signal default", in, got)
		}
	}
	if got := ParseFCrDNSMode("off"); got != FCrDNSModeOff {
		t.Errorf("ParseFCrDNSMode(off) = %v, want off", got)
	}
	if got := ParseFCrDNSMode("enforce"); got != FCrDNSModeEnforce {
		t.Errorf("ParseFCrDNSMode(enforce) = %v, want enforce", got)
	}
}

// ── HELO identity ──────────────────────────────────────────────────

func TestHELOIdentityPassesWhenForwardMatches(t *testing.T) {
	res := newFakeResolver()
	res.host["host.example"] = []string{"1.2.3.4"}
	ok, failOpen, _ := ValidateHELOIdentity("host.example", "1.2.3.4", res, false, time.Second)
	if !ok {
		t.Fatal("HELO domain whose A includes peer IP must pass")
	}
	if failOpen {
		t.Fatal("definite pass must not be marked fail-open")
	}
}

func TestHELOIdentityFailsWhenForwardMismatches(t *testing.T) {
	res := newFakeResolver()
	res.host["host.example"] = []string{"5.6.7.8"}
	// skipLoopbackExemption=true so the production-default check fires
	// even when the synthetic peer IP would be loopback in some runs.
	ok, failOpen, reason := ValidateHELOIdentity("host.example", "1.2.3.4", res, true, time.Second)
	if ok {
		t.Fatalf("HELO domain whose A excludes peer IP must fail: reason=%q", reason)
	}
	if failOpen {
		t.Fatal("definite mismatch must not be marked fail-open")
	}
}

func TestHELOIdentityNXDOMAINHardFails(t *testing.T) {
	res := newFakeResolver()
	res.nxHost["ghost.example"] = true
	ok, failOpen, _ := ValidateHELOIdentity("ghost.example", "1.2.3.4", res, true, time.Second)
	if ok {
		t.Fatal("NXDOMAIN on HELO domain must hard-fail (not fail-open)")
	}
	if failOpen {
		t.Fatal("NXDOMAIN must not be marked fail-open")
	}
}

func TestHELOIdentityFailsOpenOnResolverError(t *testing.T) {
	ok, failOpen, _ := ValidateHELOIdentity("host.example", "1.2.3.4", erroringResolver{}, true, time.Second)
	if !ok {
		t.Fatal("resolver error must fail open (ok=true)")
	}
	if !failOpen {
		t.Fatal("resolver error must set failOpen=true")
	}
}

// Production-default loopback exemption: a non-loopback HELO check
// would normally fail, but loopback peer is exempt unless
// skipLoopbackExemption=true.
func TestHELOIdentityLoopbackExempt(t *testing.T) {
	res := newFakeResolver()
	res.host["host.example"] = []string{"5.6.7.8"} // would fail at non-loopback
	ok, _, _ := ValidateHELOIdentity("host.example", "127.0.0.1", res, false, time.Second)
	if !ok {
		t.Fatal("loopback peer must be exempted from HELO identity check by default")
	}
}

// Regression guard for the porting hazard called out in the plan:
// `ValidateHELOIdentity` must NOT silently exempt non-loopback peer IPs
// when the production caller passes skipLoopbackExemption=false. The
// non-loopback peer path must hit the resolver and act on the verdict.
func TestHELOIdentityProductionRejectsNonLoopbackMismatch(t *testing.T) {
	res := newFakeResolver()
	res.host["host.example"] = []string{"5.6.7.8"}
	ok, _, _ := ValidateHELOIdentity("host.example", "1.2.3.4", res, false, time.Second)
	if ok {
		t.Fatal("production default (skipLoopbackExemption=false) must reject HELO whose A excludes non-loopback peer IP")
	}
}

// ── sender-domain MX/A ─────────────────────────────────────────────

// A domain with an MX record is authorized (the common case).
func TestSenderDomainAcceptsWithMX(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.mx["example.com"] = []*net.MX{{Host: "mail.example.com.", Pref: 10}}
	chk := NewSenderDomainChecker(res)
	if ok, reason := chk.CheckSenderDomain("example.com"); !ok {
		t.Fatalf("domain with MX must be authorized; got ok=false reason=%q", reason)
	}
}

// No MX (NXDOMAIN) but an A record present → authorized via the RFC 5321
// §5.1 implicit-MX fallback.
func TestSenderDomainAcceptsWithAOnly(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.nxMX["a-only.example"] = true
	res.host["a-only.example"] = []string{"203.0.113.9"}
	chk := NewSenderDomainChecker(res)
	if ok, reason := chk.CheckSenderDomain("a-only.example"); !ok {
		t.Fatalf("domain with A but no MX must be authorized (implicit MX); got ok=false reason=%q", reason)
	}
}

// Neither MX nor A/AAAA (authoritative NXDOMAIN on both) → reject.
func TestSenderDomainRejectsNoRecords(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.nxMX["nodns.example"] = true
	res.nxHost["nodns.example"] = true
	chk := NewSenderDomainChecker(res)
	ok, reason := chk.CheckSenderDomain("nodns.example")
	if ok {
		t.Fatal("domain with no MX and no A must be rejected (ok=false)")
	}
	if reason == "" {
		t.Error("reject must carry a non-empty reason")
	}
}

// A resolver error (non-NXDOMAIN) fails OPEN — never reject legitimate
// mail on a sick resolver — and increments the bounded fail-open counter.
func TestSenderDomainFailsOpenOnResolverError(t *testing.T) {
	// Not parallel: asserts a delta on the process-global counter.
	before := testutil.ToFloat64(metrics.SMTPSenderDomainFailOpen.WithLabelValues("resolver_error"))
	chk := NewSenderDomainChecker(erroringResolver{})
	ok, _ := chk.CheckSenderDomain("flaky.example")
	if !ok {
		t.Fatal("resolver error must fail open (ok=true), not reject")
	}
	after := testutil.ToFloat64(metrics.SMTPSenderDomainFailOpen.WithLabelValues("resolver_error"))
	if after-before != 1 {
		t.Errorf("fail-open counter delta = %v, want 1", after-before)
	}
}

// Fail-open results are NOT cached (a transient glitch must not authorize
// the domain for the cache TTL), but authoritative answers are.
func TestSenderDomainDoesNotCacheFailOpen(t *testing.T) {
	t.Parallel()
	res := newFakeResolver()
	res.nxMX["later.example"] = true
	res.nxHost["later.example"] = true
	chk := NewSenderDomainChecker(res)
	chk.CheckSenderDomain("later.example") // authoritative reject — cached
	chk.mu.RLock()
	_, cached := chk.cache["later.example"]
	chk.mu.RUnlock()
	if !cached {
		t.Error("authoritative reject should be cached")
	}

	errChk := NewSenderDomainChecker(erroringResolver{})
	errChk.CheckSenderDomain("flaky.example")
	errChk.mu.RLock()
	_, cachedErr := errChk.cache["flaky.example"]
	errChk.mu.RUnlock()
	if cachedErr {
		t.Error("fail-open result must not be cached")
	}
}
