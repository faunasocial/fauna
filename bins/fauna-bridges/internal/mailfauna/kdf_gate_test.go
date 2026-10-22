package mailfauna

import (
	"errors"
	"sync"
	"testing"
	"time"
)

// M5 (deployment-threat-model.md § Concern 1a): N parallel wrong-password
// Argon2id runs hold at most kdfMaxInFlight slots at once. Real Argon2id, so the
// peak is the actual concurrency the FFI saw. RED without the gate (peak = N).
func TestKDFGateCapsInFlightArgon2id(t *testing.T) {
	PrewarmDummyKDF()
	kdfPeak.Store(0)
	before := KDFRunCount()

	const n = 4 * kdfMaxInFlight
	var wg sync.WaitGroup
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if err := DummyCredentialKDF([]byte("wrong"), KdfKindArgon2id, "198.51.100.1"); err != nil {
				t.Errorf("dummy KDF inside the wait bound: %v", err)
			}
		}()
	}
	wg.Wait()

	if got := KDFRunCount() - before; got != n {
		t.Fatalf("ran %d Argon2id, want %d (none may be dropped inside the wait bound)", got, n)
	}
	if peak := kdfPeak.Load(); peak > kdfMaxInFlight {
		t.Fatalf("peak in-flight Argon2id = %d, want <= %d", peak, kdfMaxInFlight)
	}
}

// A request that cannot get a slot within the wait bound fails with ErrKDFBusy
// rather than queueing forever.
func TestKDFGateTimesOutWhenSaturated(t *testing.T) {
	restore := SaturateKDFGateForTest(20 * time.Millisecond)
	defer restore()

	if _, err := acquireKDF(KdfKindArgon2id, "198.51.100.2"); !errors.Is(err, ErrKDFBusy) {
		t.Fatalf("saturated acquire: got %v, want ErrKDFBusy", err)
	}
	// HKDF is never throttled.
	rel, err := acquireKDF(KdfKindHkdf, "198.51.100.2")
	if err != nil {
		t.Fatalf("HKDF must bypass the gate: %v", err)
	}
	rel()
	// A timed-out waiter leaves nothing behind: the queue is empty again.
	kdfGate.mu.Lock()
	queued := len(kdfGate.order)
	kdfGate.mu.Unlock()
	if queued != 0 {
		t.Fatalf("timed-out waiter left %d sources queued", queued)
	}
}

// One source holding every slot with a deep queue of its own cannot
// starve another source. The newcomer is granted the very next freed slot,
// ahead of the flooding source's earlier waiters. RED under a plain FIFO /
// first-come semaphore (the newcomer waits behind the whole flood).
func TestKDFGateIsFairAcrossSources(t *testing.T) {
	old := kdfWait
	kdfWait = 5 * time.Second
	defer func() { kdfWait = old }()

	const flood = "2001:db8:1:2::1"
	var held []func()
	for i := 0; i < kdfMaxInFlight; i++ {
		rel, err := acquireKDF(KdfKindArgon2id, flood)
		if err != nil {
			t.Fatalf("slot %d: %v", i, err)
		}
		held = append(held, rel)
	}

	type grant struct {
		who string
		rel func()
	}
	grants := make(chan grant, 16)
	waitFor := func(who, source string) {
		rel, err := acquireKDF(KdfKindArgon2id, source)
		if err != nil {
			t.Errorf("%s: %v", who, err)
			return
		}
		grants <- grant{who, rel}
	}
	// The flood queues several more attempts — from rotating addresses inside
	// its own /64, which the gate must count as one source — before the honest
	// source arrives.
	for i := 0; i < 3; i++ {
		go waitFor("flood", "2001:db8:1:2::"+string(rune('a'+i)))
		waitQueued(t, 1+i)
	}
	go waitFor("honest", "203.0.113.9")
	waitQueued(t, 4)

	held[0]()
	g := <-grants
	if g.who != "honest" {
		t.Fatalf("first freed slot went to %s; the newcomer source must be served before the flood's backlog", g.who)
	}
	g.rel()
	for _, r := range held[1:] {
		r()
	}
	for i := 0; i < 3; i++ {
		(<-grants).rel()
	}
}

// waitQueued blocks until n attempts are queued at the gate.
func waitQueued(t *testing.T, n int) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		kdfGate.mu.Lock()
		got := 0
		for _, q := range kdfGate.queues {
			got += len(q)
		}
		kdfGate.mu.Unlock()
		if got == n {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("timed out waiting for %d queued attempts", n)
}

// The fairness key: an IPv6 /64 is one source, an IPv4 address is one source,
// a host:port form keys on its host.
func TestKDFSourceKey(t *testing.T) {
	cases := map[string]string{
		"2001:db8:1:2::1":       "2001:db8:1:2::/64",
		"2001:db8:1:2:ffff::9":  "2001:db8:1:2::/64",
		"[2001:db8:1:2::1]:993": "2001:db8:1:2::/64",
		"203.0.113.9":           "203.0.113.9",
		"203.0.113.9:5555":      "203.0.113.9",
		"::ffff:203.0.113.9":    "203.0.113.9",
		"not-an-ip":             "not-an-ip",
	}
	for in, want := range cases {
		if got := kdfSourceKey(in); got != want {
			t.Errorf("kdfSourceKey(%q) = %q, want %q", in, got, want)
		}
	}
}
