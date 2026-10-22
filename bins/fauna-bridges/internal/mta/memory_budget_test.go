// Tests for the port-25 memory budget (deployment-threat-model.md § Concern 1a,
// finding M3; smtp-server.md § Connection-time limits): the in-flight DATA byte
// budget, the unknown-recipient cap, and the sweeps of the two limiter maps
// keyed on what a stranger chooses.
package mta

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/smtp"
	"testing"
	"time"

	gosmtp "github.com/emersion/go-smtp"
)

// A DATA that does not fit the budget left by other sessions is deferred with
// the infra-class 451 4.3.1 — not tarpitted, nothing parsed — and leaves the
// budget exactly as it found it; one that fits is read and its charge returned
// when Data ends.
func TestInboundDataPastTheByteBudgetIsDeferred(t *testing.T) {
	t.Parallel()
	budget := newDataBudget(4 * dataReadChunk)
	// Another source's session holds three of the four chunks.
	other := budget.open("198.51.100.7")
	if err := other.grow(3 * dataReadChunk); err != nil {
		t.Fatalf("setup: reserve: %v", err)
	}
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "192.0.2.1",
		from:            "alice@example.org",
		maxMessageBytes: 1 << 20,
		dataBudget:      budget,
		sleep:           func(time.Duration) { t.Error("an over-budget DATA must not be tarpitted") },
	}
	err := s.Data(bytes.NewReader(bytes.Repeat([]byte("x"), 2*dataReadChunk)))
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 451 || smtpErr.EnhancedCode != (gosmtp.EnhancedCode{4, 3, 1}) {
		t.Errorf("over-budget reply: got %d %v, want 451 4.3.1", smtpErr.Code, smtpErr.EnhancedCode)
	}
	if s.errCount != 0 {
		t.Errorf("errCount = %d, want 0 (infra-class, not a policy rejection)", s.errCount)
	}
	if s.parsed != nil {
		t.Error("an over-budget DATA must not reach the parser")
	}
	if got := budget.inUse(); got != 3*dataReadChunk {
		t.Errorf("budget after a refused DATA: %d in use, want %d", got, 3*dataReadChunk)
	}

	// The other session finishes; a message now fits, and its charge is
	// returned once Data ends, whatever Data answered.
	other.close()
	s.sleep = func(time.Duration) {} // this one may meet a policy reject downstream
	_ = s.Data(bytes.NewReader([]byte("Subject: x\r\n\r\nhi\r\n")))
	if got := budget.inUse(); got != 0 {
		t.Errorf("budget after a DATA that fit: %d in use, want 0", got)
	}
}

// The charge follows the buffer's real growth, not just the bytes that
// arrived: what the read holds in memory is its capacity, so that is what the
// budget is charged: append's doubling once left real memory at up to twice
// the charge.
func TestReadDataWithinBudgetChargesTheBuffersRealGrowth(t *testing.T) {
	t.Parallel()
	budget := newDataBudget(8 * dataReadChunk)
	for _, size := range []int{1000, dataReadChunk + 1, 3*dataReadChunk + 17} {
		body := bytes.Repeat([]byte("y"), size)
		charge := budget.open("192.0.2.1")
		raw, err := readDataWithinBudget(oneByteReader{bytes.NewReader(body)}, 0, charge)
		if err != nil {
			t.Fatalf("read %d: %v", size, err)
		}
		if !bytes.Equal(raw, body) {
			t.Fatalf("read %d bytes, want %d", len(raw), size)
		}
		if charge.held != int64(cap(raw)) || budget.inUse() != int64(cap(raw)) {
			t.Errorf("size %d: charged %d, in use %d; want the buffer's capacity %d", size, charge.held, budget.inUse(), cap(raw))
		}
		if cap(raw) > size+size/2+dataReadChunk {
			t.Errorf("size %d: capacity %d overshoots the growth step", size, cap(raw))
		}
		charge.close()
		if got := budget.inUse(); got != 0 {
			t.Errorf("size %d: %d in use after close, want 0", size, got)
		}
	}
}

// One source fanning DATA over many connections holds at
// most its share of the budget, so another source's message is still
// admitted. A source's sole transaction may pass the share (one message at
// the size cap stays admissible), and having passed it keeps growing while
// the same source's other transactions are refused.
func TestDataBudgetSourceShareLeavesRoomForOtherSources(t *testing.T) {
	t.Parallel()
	const limit = 16 * dataReadChunk
	budget := newDataBudget(limit)
	share := budget.share
	if share != limit/dataSourceShareDivisor {
		t.Fatalf("share = %d, want limit/%d", share, dataSourceShareDivisor)
	}
	// One stranger opens six transactions and grows each as far as it can.
	attacker := dataShareKey("198.51.100.7")
	var charges []*dataCharge
	for i := 0; i < 6; i++ {
		charges = append(charges, budget.open(attacker))
	}
	for _, c := range charges {
		for c.grow(dataReadChunk) == nil {
		}
	}
	held := int64(0)
	for _, c := range charges {
		held += c.held
	}
	if held > share {
		t.Fatalf("one source holds %d across six transactions, want at most its share %d", held, share)
	}
	// Another source's message the size of the rest of the budget fits.
	victim := budget.open(dataShareKey("203.0.113.9"))
	if err := victim.grow(limit - share); err != nil {
		t.Errorf("another source's DATA was refused while one source held its share: %v", err)
	}
	victim.close()
	for _, c := range charges {
		c.close()
	}
	if got := budget.inUse(); got != 0 {
		t.Fatalf("in use after every close: %d, want 0", got)
	}

	// A source's sole transaction passes the share...
	big := budget.open(attacker)
	if err := big.grow(share + dataReadChunk); err != nil {
		t.Fatalf("a source's sole transaction was held to the share: %v", err)
	}
	// ...a second one from the same source then gets nothing...
	second := budget.open(attacker)
	if err := second.grow(dataReadChunk); !errors.Is(err, errDataSourceShareFull) {
		t.Errorf("second transaction past the share: got %v, want errDataSourceShareFull", err)
	}
	// ...while the one already past it may finish.
	if err := big.grow(dataReadChunk); err != nil {
		t.Errorf("the transaction already past the share was cut off: %v", err)
	}
	second.close()
	big.close()
	if got := budget.inUse(); got != 0 {
		t.Errorf("in use after every close: %d, want 0", got)
	}
}

// The share is keyed on the IPv4 address or the IPv6 /64 — one IPv6 host
// holds a /64, so per-address keying would give it 2^64 shares — and
// loopback (the in-container dials) is exempt, as for the per-IP caps.
func TestDataShareKey(t *testing.T) {
	t.Parallel()
	if a, b := dataShareKey("2001:db8:1:2::1"), dataShareKey("2001:db8:1:2:ffff::9"); a != b {
		t.Errorf("one /64 gave two keys: %q, %q", a, b)
	}
	if a, b := dataShareKey("2001:db8:1:2::1"), dataShareKey("2001:db8:1:3::1"); a == b {
		t.Errorf("two /64s shared the key %q", a)
	}
	if a, b := dataShareKey("192.0.2.1"), dataShareKey("192.0.2.2"); a == b {
		t.Errorf("two IPv4 addresses shared the key %q", a)
	}
	for _, lo := range []string{"127.0.0.1", "::1"} {
		if k := dataShareKey(lo); k != "" {
			t.Errorf("loopback %s keyed %q, want exempt", lo, k)
		}
	}
	if k := dataShareKey(""); k == "" {
		t.Error("an unparsed peer address was exempted")
	}
}

// A DATA trickled under the per-read timeout cannot hold its
// charge for ever — past the whole-DATA deadline (a grace, then a minimum
// sustained rate) it is dropped with 421 4.4.2, not tarpitted and nothing
// parsed, and its charge is returned.
func TestInboundTricklingDataPastTheDeadlineIsDropped(t *testing.T) {
	t.Parallel()
	clk := newFakeClock(time.Unix(1_700_000_000, 0))
	budget := newDataBudget(16 * dataReadChunk)
	budget.now = clk.Now
	s := &inboundSession{
		logger:          slog.Default(),
		clientIP:        "192.0.2.1",
		from:            "alice@example.org",
		maxMessageBytes: 1 << 20,
		dataBudget:      budget,
		sleep:           func(time.Duration) { t.Error("a dropped trickle must not be tarpitted") },
	}
	// A byte every 29 s: each read is inside the per-read timeout.
	trickle := &clockedReader{clk: clk, step: 29 * time.Second}
	err := s.Data(trickle)
	var smtpErr *gosmtp.SMTPError
	if !errors.As(err, &smtpErr) {
		t.Fatalf("expected *gosmtp.SMTPError, got %T: %v", err, err)
	}
	if smtpErr.Code != 421 || smtpErr.EnhancedCode != (gosmtp.EnhancedCode{4, 4, 2}) {
		t.Errorf("trickle reply: got %d %v, want 421 4.4.2", smtpErr.Code, smtpErr.EnhancedCode)
	}
	if trickle.reads*29 > int(dataDeadlineGrace/time.Second)+29*2 {
		t.Errorf("the trickle was read %d times (%d s), past the %v grace", trickle.reads, trickle.reads*29, dataDeadlineGrace)
	}
	if s.parsed != nil {
		t.Error("a dropped trickle must not reach the parser")
	}
	if got := budget.inUse(); got != 0 {
		t.Errorf("budget after a dropped trickle: %d in use, want 0", got)
	}

	// A sender at the minimum sustained rate is never cut off.
	steady := &clockedReader{clk: clk, step: time.Second, chunk: dataMinBytesPerSecond, total: 4 * dataMinBytesPerSecond * int(dataDeadlineGrace/time.Second)}
	roomy := newDataBudget(16 << 20)
	roomy.now = clk.Now
	charge := roomy.open("192.0.2.1")
	raw, err := readDataWithinBudget(steady, 0, charge)
	charge.close()
	if err != nil {
		t.Errorf("a sender at the minimum rate was cut off after %d bytes: %v", len(raw), err)
	}
}

// clockedReader yields chunk bytes (1 when zero) per Read, advancing the fake
// clock step first, until total bytes (unbounded when zero), then io.EOF.
type clockedReader struct {
	clk   *fakeClock
	step  time.Duration
	chunk int
	total int
	reads int
	sent  int
}

func (c *clockedReader) Read(p []byte) (int, error) {
	if c.total > 0 && c.sent >= c.total {
		return 0, io.EOF
	}
	c.clk.Advance(c.step)
	c.reads++
	n := c.chunk
	if n == 0 {
		n = 1
	}
	if n > len(p) {
		n = len(p)
	}
	if c.total > 0 && n > c.total-c.sent {
		n = c.total - c.sent
	}
	for i := range p[:n] {
		p[i] = 'z'
	}
	c.sent += n
	return n, nil
}

type oneByteReader struct{ r *bytes.Reader }

func (o oneByteReader) Read(p []byte) (int, error) {
	if len(p) > 1 {
		p = p[:1]
	}
	return o.r.Read(p)
}

// The production budget admits one message at the admin's maximum size cap.
func TestDataBudgetAdmitsOneMessageAtTheSizeCapMaximum(t *testing.T) {
	t.Parallel()
	const adminMaxMessageBytes = 250_000_000
	if maxInFlightDataBytes < adminMaxMessageBytes+dataReadChunk {
		t.Fatalf("maxInFlightDataBytes = %d cannot hold one %d-byte message", maxInFlightDataBytes, adminMaxMessageBytes)
	}
}

// A session that has the resolver refuse maxUnknownRcptsPerSession recipients
// is closed: the next RCPT is answered 421 without another resolver call, and
// an RSET buys no fresh round.
func TestInboundUnknownRecipientCapClosesTheSession(t *testing.T) {
	t.Parallel()
	mc := newMockCaller()
	for i := 0; i <= maxUnknownRcptsPerSession; i++ {
		mc.decisions[fmt.Sprintf("u%d@test.example", i)] = rcptDecision{resolved: false, reason: "unknown_local_part"}
	}
	addr, cancel := startBackendListener(t, &inboundBackend{
		logger: slog.Default(),
		caller: mc,
		cfg: staticMTAConfig(&mtaLiveConfig{
			localDomains: []string{"test.example"},
		}),
	})
	defer cancel()
	c, err := smtp.Dial(addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer func() { _ = c.Close() }()
	if err := c.Hello("loopback.example"); err != nil {
		t.Fatalf("HELO: %v", err)
	}
	if err := c.Mail("alice@example.org"); err != nil {
		t.Fatalf("MAIL: %v", err)
	}
	for i := 0; i < maxUnknownRcptsPerSession; i++ {
		if code := smtpCodeOf(c.Rcpt(fmt.Sprintf("u%d@test.example", i))); code != 550 {
			t.Fatalf("unknown RCPT %d: got %d, want 550", i, code)
		}
	}
	capped := fmt.Sprintf("u%d@test.example", maxUnknownRcptsPerSession)
	if code := smtpCodeOf(c.Rcpt(capped)); code != 421 {
		t.Fatalf("RCPT past the unknown-recipient cap: got %d, want 421", code)
	}
	mc.mu.Lock()
	got := mc.calls[capped]
	mc.mu.Unlock()
	if got != 0 {
		t.Errorf("the capped RCPT reached the resolver (%d calls, want 0)", got)
	}
	// RSET then a fresh MAIL: the session stays closed. The connection may be
	// gone already, which is closed as well.
	if err := c.Reset(); err == nil {
		if code := smtpCodeOf(c.Mail("alice@example.org")); code != 421 && code != 0 {
			t.Errorf("MAIL after the cap: got %d, want 421", code)
		}
	}
}

// Expired rate buckets are swept once the map passes the threshold, so a
// stream of distinct source IPs does not grow it for the process's life.
func TestRateLimiterSweepsExpiredBuckets(t *testing.T) {
	t.Parallel()
	clk := newFakeClock(time.Unix(1_700_000_000, 0))
	rl := NewRateLimiter(10, time.Minute, clk)
	for i := 0; i < limiterSweepThreshold; i++ {
		rl.Allow(fmt.Sprintf("198.51.%d.%d", i/256, i%256))
	}
	clk.Advance(2 * time.Minute)
	rl.Allow("203.0.113.1")
	rl.mu.Lock()
	n := len(rl.buckets)
	rl.mu.Unlock()
	if n != 1 {
		t.Errorf("buckets after the sweep: %d, want 1 (only the live one)", n)
	}
}

// The sender-domain cache — keyed on the MAIL FROM domain a stranger picks —
// drops expired answers past the threshold and never grows beyond it.
func TestSenderDomainCacheIsSweptAndBounded(t *testing.T) {
	t.Parallel()
	c := NewSenderDomainChecker(newFakeResolver())
	c.CacheTTL = time.Hour
	for i := 0; i < limiterSweepThreshold+100; i++ {
		c.CheckSenderDomain(fmt.Sprintf("d%d.example", i))
	}
	c.mu.RLock()
	n := len(c.cache)
	c.mu.RUnlock()
	if n > limiterSweepThreshold {
		t.Errorf("cache holds %d live entries, want at most %d", n, limiterSweepThreshold)
	}

	c.CacheTTL = time.Nanosecond
	c.mu.Lock()
	for k, e := range c.cache {
		e.expires = time.Now().Add(-time.Second)
		c.cache[k] = e
	}
	c.mu.Unlock()
	c.CheckSenderDomain("fresh.example")
	c.mu.RLock()
	n = len(c.cache)
	c.mu.RUnlock()
	if n != 1 {
		t.Errorf("cache after the sweep: %d entries, want 1", n)
	}
}
