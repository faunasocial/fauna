package authlock

import (
	"fmt"
	"sync"
	"testing"
	"time"
)

// fakeClock is a goroutine-safe deterministic clock for the lockout tests.
type fakeClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
}

func newFakeClock() *fakeClock { return &fakeClock{now: time.Unix(1_700_000_000, 0)} }

// --- fine (user, credential, IP) bucket: the original per-triple behavior ---

func TestAllowsUpToLimitThenLocks(t *testing.T) {
	t.Parallel()
	lockout := New(30, time.Minute, newFakeClock())
	const u, c, ip = "alice@example.com", "default", "10.0.0.1"
	for i := 0; i < 30; i++ {
		if lockout.IsLockedFor(u, c, ip) {
			t.Fatalf("IsLockedFor returned true on failure %d (limit not yet reached)", i+1)
		}
		lockout.RecordFailureFor(u, c, ip)
	}
	if !lockout.IsLockedFor(u, c, ip) {
		t.Fatal("IsLockedFor must return true on the 31st check after 30 failures")
	}
}

func TestPerKeyIsolation(t *testing.T) {
	t.Parallel()
	lockout := New(3, time.Minute, newFakeClock())
	for i := 0; i < 3; i++ {
		lockout.RecordFailureFor("alice@example.com", "default", "10.0.0.1")
	}
	if !lockout.IsLockedFor("alice@example.com", "default", "10.0.0.1") {
		t.Fatal("alice@10.0.0.1 should be locked after 3 failures")
	}
	// Different user, same IP: only 3 failures on the shared IP bucket (limit
	// 3*ipMultiplier), so bob is not locked by alice's fine-bucket failures.
	if lockout.IsLockedFor("bob@example.com", "default", "10.0.0.1") {
		t.Error("bob@10.0.0.1 must not be locked just because alice was")
	}
	// Same user+credential, different IP: only 3 failures on the shared mid
	// bucket (limit 3*midMultiplier), so the remote IP is not locked either.
	if lockout.IsLockedFor("alice@example.com", "default", "10.0.0.2") {
		t.Error("alice@10.0.0.2 must not be locked just because alice@10.0.0.1 was")
	}
}

func TestWindowRolloverClearsState(t *testing.T) {
	t.Parallel()
	clk := newFakeClock()
	lockout := New(3, time.Minute, clk)
	const u, c, ip = "alice@example.com", "default", "10.0.0.1"
	for i := 0; i < 3; i++ {
		lockout.RecordFailureFor(u, c, ip)
	}
	if !lockout.IsLockedFor(u, c, ip) {
		t.Fatal("must be locked after 3 failures")
	}
	clk.advance(time.Minute + time.Second)
	if lockout.IsLockedFor(u, c, ip) {
		t.Error("IsLockedFor must return false once the window expires")
	}
	lockout.RecordFailureFor(u, c, ip)
	if lockout.IsLockedFor(u, c, ip) {
		t.Error("first failure after rollover must not lock")
	}
}

func TestResetForClearsFineBucket(t *testing.T) {
	t.Parallel()
	lockout := New(3, time.Minute, newFakeClock())
	const u, c, ip = "alice@example.com", "default", "10.0.0.1"
	for i := 0; i < 2; i++ {
		lockout.RecordFailureFor(u, c, ip)
	}
	lockout.ResetFor(u, c, ip)
	if lockout.IsLockedFor(u, c, ip) {
		t.Fatal("ResetFor must clear the failure counter")
	}
	for i := 0; i < 3; i++ {
		lockout.RecordFailureFor(u, c, ip)
	}
	if !lockout.IsLockedFor(u, c, ip) {
		t.Fatal("ResetFor must NOT raise the limit — 3 fresh failures still lock")
	}
}

func TestZeroLimitIsDisabled(t *testing.T) {
	t.Parallel()
	lockout := New(0, time.Minute, newFakeClock())
	const u, c, ip = "alice@example.com", "default", "10.0.0.1"
	for i := 0; i < 100; i++ {
		lockout.RecordFailureFor(u, c, ip)
		if lockout.IsLockedFor(u, c, ip) {
			t.Fatalf("zero-limit lockout must never report locked (failure %d)", i+1)
		}
	}
}

func TestNilReceiverTolerant(t *testing.T) {
	t.Parallel()
	var lockout *Lockout
	if lockout.IsLockedFor("u", "c", "ip") {
		t.Error("nil receiver must report not-locked")
	}
	lockout.RecordFailureFor("u", "c", "ip") // must not panic
	lockout.ResetFor("u", "c", "ip")         // must not panic
}

// --- § F11 regression: coarse buckets that close the distributed/rotating
// brute-force evasion the per-triple fine bucket alone missed. ---

// TestMidBucketLocksAcrossIPs pins the botnet-one-credential-from-many-IPs half:
// one (user, credential) failing from many distinct IPs — each IP below the fine
// and IP ceilings — still trips the (user|credential) mid bucket at base*mid.
func TestMidBucketLocksAcrossIPs(t *testing.T) {
	t.Parallel()
	const base = 3
	lockout := New(base, time.Minute, newFakeClock())
	const u, c = "victim@example.com", "default"
	midLimit := base * midMultiplier // 9
	for i := uint32(0); i < midLimit; i++ {
		ip := fmt.Sprintf("203.0.113.%d", i) // a distinct IP each time
		if lockout.IsLockedFor(u, c, ip) {
			t.Fatalf("fine/ip buckets must not trip from one failure each (i=%d)", i)
		}
		lockout.RecordFailureFor(u, c, ip)
	}
	// A brand-new IP for the same credential is now refused via the mid bucket,
	// even though that IP has zero failures of its own.
	if !lockout.IsLockedFor(u, c, "198.51.100.7") {
		t.Fatal("mid (user|credential) bucket must lock after base*midMultiplier cross-IP failures")
	}
	// A different credential from the same flood of IPs is unaffected.
	if lockout.IsLockedFor("other@example.com", "default", "198.51.100.7") {
		t.Error("a different credential must not be locked by the victim's mid bucket")
	}
}

// TestIPBucketLocksAcrossUsernames pins the spray-many-usernames-from-one-IP
// half: many distinct usernames failing once each from one IP — each below the
// fine and mid ceilings — still trips the (source IP) coarse bucket at base*ip.
func TestIPBucketLocksAcrossUsernames(t *testing.T) {
	t.Parallel()
	const base = 3
	lockout := New(base, time.Minute, newFakeClock())
	const ip = "203.0.113.9"
	ipLimit := base * ipMultiplier // 30
	for i := uint32(0); i < ipLimit; i++ {
		u := fmt.Sprintf("user%d@example.com", i) // a distinct username each time
		if lockout.IsLockedFor(u, "default", ip) {
			t.Fatalf("fine/mid buckets must not trip from one failure each (i=%d)", i)
		}
		lockout.RecordFailureFor(u, "default", ip)
	}
	// A brand-new username from the same IP is now refused via the IP bucket.
	if !lockout.IsLockedFor("fresh@example.com", "default", ip) {
		t.Fatal("coarse (source IP) bucket must lock after base*ipMultiplier cross-username failures")
	}
	// The same usernames from a different IP are unaffected.
	if lockout.IsLockedFor("user0@example.com", "default", "198.51.100.8") {
		t.Error("a different IP must not be locked by the sprayed IP's bucket")
	}
}

// TestResetForDoesNotFlushIPBucket pins the shared-NAT defense: a successful
// auth from one user behind a shared IP must NOT clear the IP-wide spray counter
// for the other users behind that IP.
func TestResetForDoesNotFlushIPBucket(t *testing.T) {
	t.Parallel()
	const base = 3
	lockout := New(base, time.Minute, newFakeClock())
	const ip = "203.0.113.50"
	ipLimit := base * ipMultiplier // 30
	for i := uint32(0); i < ipLimit; i++ {
		lockout.RecordFailureFor(fmt.Sprintf("user%d@example.com", i), "default", ip)
	}
	if !lockout.IsLockedFor("anyone@example.com", "default", ip) {
		t.Fatal("IP bucket should be locked after a full spray")
	}
	// user0 succeeds — clears only its own fine + mid buckets.
	lockout.ResetFor("user0@example.com", "default", ip)
	if !lockout.IsLockedFor("user0@example.com", "default", ip) {
		t.Error("one success from a shared NAT must NOT flush the IP-wide spray counter")
	}
	if !lockout.IsLockedFor("victim@example.com", "default", ip) {
		t.Error("other users behind the shared IP stay locked after one peer's success")
	}
}

// TestExpiredBucketsAreSwept pins the § D8 fix: a flood of distinct keys (the
// unbounded-map vector) is reclaimed once their windows expire, instead of
// growing forever. RecordFailureFor creates up to three buckets per call, so the
// post-expiry sweep leaves only the freshly-recorded trigger's three buckets.
func TestExpiredBucketsAreSwept(t *testing.T) {
	t.Parallel()
	clk := newFakeClock()
	lockout := New(3, time.Minute, clk)

	// Fill well past the sweep threshold with distinct keys.
	for i := 0; i < sweepThreshold+10; i++ {
		lockout.RecordFailureFor(fmt.Sprintf("user%d@example.com", i), "default", fmt.Sprintf("10.0.0.%d", i%256))
	}
	lockout.mu.Lock()
	grew := len(lockout.buckets)
	lockout.mu.Unlock()
	if grew <= sweepThreshold {
		t.Fatalf("expected the map to grow past the sweep threshold, got %d", grew)
	}

	// Expire every bucket, then trigger one more failure (a fresh key) so
	// RecordFailureFor runs the opportunistic sweep.
	clk.advance(time.Minute + time.Second)
	lockout.RecordFailureFor("trigger@example.com", "default", "172.16.0.1")

	lockout.mu.Lock()
	after := len(lockout.buckets)
	lockout.mu.Unlock()
	// Only the trigger's fine + mid + ip buckets should remain.
	if after != 3 {
		t.Fatalf("expired buckets not swept: %d remain, want 3", after)
	}
}
