package wsrpc

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"
)

// gateWatchCaller is a Caller whose fetch_config answer can CHANGE between
// calls — the whole point of these tests, since an idling bridge's gates open
// only because a later snapshot differs from the one it cold-booted on. It also
// captures the push handler installed via SetOnPush, so a test can deliver a
// `config_changed` push through the same seam the production
// *ReconnectingClient uses and exercise dispatcher → reloader → applier end to
// end rather than poking the reloader directly.
type gateWatchCaller struct {
	mu      sync.Mutex
	snap    ConfigSnapshot
	fetchN  int
	failN   int // remaining Call()s to fail before answering normally
	onPush  PushHandler
	pushSet chan struct{}
}

func newGateWatchCaller(initial ConfigSnapshot) *gateWatchCaller {
	return &gateWatchCaller{snap: initial, pushSet: make(chan struct{}, 1)}
}

func (c *gateWatchCaller) Call(_ context.Context, method string, _ any, reply any) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if method != MethodFetchConfig {
		return fmt.Errorf("gateWatchCaller: unexpected method %q", method)
	}
	c.fetchN++
	if c.failN > 0 {
		c.failN--
		return fmt.Errorf("gateWatchCaller: simulated transient fetch failure")
	}
	if p, ok := reply.(*ConfigSnapshot); ok {
		*p = c.snap
	}
	return nil
}

// SetOnPush satisfies the seam IdleUntilGatesOpen probes for. Signalling
// pushSet lets a test wait for the subscription to be installed instead of
// sleeping — the install is what "an idling bridge stays subscribed" means, so
// waiting on it is waiting on the property under test.
func (c *gateWatchCaller) SetOnPush(h PushHandler) {
	c.mu.Lock()
	c.onPush = h
	c.mu.Unlock()
	select {
	case c.pushSet <- struct{}{}:
	default:
	}
}

func (c *gateWatchCaller) setSnapshot(s ConfigSnapshot) {
	c.mu.Lock()
	c.snap = s
	c.mu.Unlock()
}

// pushConfigChanged delivers a config_changed push through the installed
// handler, waiting for the install first.
func (c *gateWatchCaller) pushConfigChanged(t *testing.T) {
	t.Helper()
	select {
	case <-c.pushSet:
	case <-time.After(2 * time.Second):
		t.Fatal("IdleUntilGatesOpen never installed a push handler — an idling bridge is not subscribed")
	}
	c.mu.Lock()
	h := c.onPush
	c.mu.Unlock()
	if h == nil {
		t.Fatal("push handler was signalled but is nil")
	}
	h(PushKindConfigChanged, configChangedPayload(t, configChangeReasonMailEnabled), 1)
}

// mailOn is the gatesOpen predicate these tests watch with: the simplest
// possible stand-in for a role's real startup gate.
func mailOn(s ConfigSnapshot) bool { return s.MailEnabled }

// TestIdleUntilGatesOpen_PushThatOpensGatesEndsTheIdle is the regression this
// whole helper exists for. A bridge that cold-booted with mail disabled used to
// block on `<-ctx.Done()` with no config_changed subscription at all, so an
// admin's later enable was invisible to it forever. Now the enable's push makes
// it re-fetch, see the open gate, and return `true` — which the role turns into
// a nil return → exit 0 → supervisor rebind.
func TestIdleUntilGatesOpen_PushThatOpensGatesEndsTheIdle(t *testing.T) {
	t.Parallel()
	caller := newGateWatchCaller(ConfigSnapshot{MailEnabled: false})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan bool, 1)
	go func() {
		done <- IdleUntilGatesOpen(ctx, caller, nil, time.Second, nil, mailOn)
	}()

	// The admin enables mail: nest persists the toggle, then fans the push.
	caller.setSnapshot(ConfigSnapshot{MailEnabled: true})
	caller.pushConfigChanged(t)

	select {
	case opened := <-done:
		if !opened {
			t.Fatal("IdleUntilGatesOpen returned false; want true (gates opened, not a process cancel)")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("IdleUntilGatesOpen never returned after a push that opened its gates")
	}
}

// TestIdleUntilGatesOpen_PushThatLeavesGatesShutKeepsIdling — the negative half.
// A config_changed for an unrelated edit (a policy tweak while mail stays off)
// must NOT end the idle: exiting there would make the supervisor restart a
// process that only idles again, i.e. a restart loop paced by admin edits.
func TestIdleUntilGatesOpen_PushThatLeavesGatesShutKeepsIdling(t *testing.T) {
	t.Parallel()
	caller := newGateWatchCaller(ConfigSnapshot{MailEnabled: false})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan bool, 1)
	go func() {
		done <- IdleUntilGatesOpen(ctx, caller, nil, time.Second, nil, mailOn)
	}()

	// A real edit that leaves the gate shut: the snapshot changes, mail doesn't.
	caller.setSnapshot(ConfigSnapshot{MailEnabled: false, IMAP: ImapPolicy{IdleTimeoutSecs: 77}})
	caller.pushConfigChanged(t)

	// Latency-independent negative assert (e2e-conventions.md convention 14):
	// the causal barrier is the re-fetch the push provoked — once the fetch
	// count has advanced, the applier has run and its verdict is final, so a
	// still-blocked helper is blocked for good, not merely slow.
	deadline := time.Now().Add(5 * time.Second)
	for {
		caller.mu.Lock()
		n := caller.fetchN
		caller.mu.Unlock()
		if n > 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the push never provoked a re-fetch — the idle subscription is not wired")
		}
		time.Sleep(5 * time.Millisecond)
	}
	select {
	case <-done:
		t.Fatal("IdleUntilGatesOpen returned on a push that left its gates shut (a restart loop)")
	default:
	}

	// And it still honours a real process cancel.
	cancel()
	select {
	case opened := <-done:
		if opened {
			t.Fatal("returned true on a process cancel; want false")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("IdleUntilGatesOpen did not return within 5s of ctx cancel")
	}
}

// TestIdleUntilGatesOpen_TransientFetchFailureRetries is finding's
// red-first pin. An admin's enable makes the nest send exactly ONE
// config_changed push; if the fetch that push provokes fails transiently
// (this test simulates it once), the idle role must not sit stuck forever —
// unlike a SERVING role, it has no last-good config to fall back on, so
// dropping the retry silently loses the admin's enable until an unrelated
// second push or a process restart. Red-verify: comment out
// IdleUntilGatesOpen's `reloader.onFetchFailure = ...` wiring (or
// reloadOnce's `onFetchFailure()` call) and this test times out.
func TestIdleUntilGatesOpen_TransientFetchFailureRetries(t *testing.T) {
	t.Parallel()
	caller := newGateWatchCaller(ConfigSnapshot{MailEnabled: false})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan bool, 1)
	go func() {
		done <- IdleUntilGatesOpen(ctx, caller, nil, time.Second, nil, mailOn)
	}()

	// The admin enables mail; the push-provoked re-fetch fails once before
	// the (already-updated) snapshot is answered normally.
	caller.setSnapshot(ConfigSnapshot{MailEnabled: true})
	caller.mu.Lock()
	caller.failN = 1
	caller.mu.Unlock()
	caller.pushConfigChanged(t)

	select {
	case opened := <-done:
		if !opened {
			t.Fatal("IdleUntilGatesOpen returned false; want true (a retried fetch found the open gate)")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("a transient fetch failure on the gate-opening push left the idle role stuck forever")
	}
	caller.mu.Lock()
	n := caller.fetchN
	caller.mu.Unlock()
	if n < 2 {
		t.Fatalf("want at least 2 fetch attempts (the failure plus a retry), got %d", n)
	}
}

// TestIdleUntilGatesOpen_ReconnectApplyOpensGates — the reconnect seam. A gate
// that opens while the WS is down produces no push (the emit is best-effort and
// a disconnected bridge misses it); the reconnect's own re-fetch is what
// catches up. Publishing the reloader is what routes that snapshot through the
// same applier, so a gate opened during a blip is not lost.
func TestIdleUntilGatesOpen_ReconnectApplyOpensGates(t *testing.T) {
	t.Parallel()
	caller := newGateWatchCaller(ConfigSnapshot{MailEnabled: false})

	published := make(chan *ConfigReloader, 1)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan bool, 1)
	go func() {
		done <- IdleUntilGatesOpen(ctx, caller, func(r *ConfigReloader) { published <- r }, time.Second, nil, mailOn)
	}()

	var reloader *ConfigReloader
	select {
	case reloader = <-published:
	case <-time.After(2 * time.Second):
		t.Fatal("IdleUntilGatesOpen never published its reloader — the reconnect re-fetch has nowhere to land")
	}

	// main.go's OnConnect hands its freshly re-fetched snapshot to Apply.
	reloader.Apply(ConfigSnapshot{MailEnabled: true})

	select {
	case opened := <-done:
		if !opened {
			t.Fatal("returned false; want true (the reconnect snapshot opened the gates)")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a reconnect Apply that opened the gates did not end the idle")
	}
}

// TestIdleUntilGatesOpen_NilClientStillHonoursCancel — a Deps with no Client
// (unit-test fixtures, a wiring bug) must degrade to the old block-until-
// shutdown behaviour rather than nil-panicking inside the reloader.
func TestIdleUntilGatesOpen_NilClientStillHonoursCancel(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan bool, 1)
	go func() {
		done <- IdleUntilGatesOpen(ctx, nil, nil, time.Second, nil, mailOn)
	}()
	cancel()
	select {
	case opened := <-done:
		if opened {
			t.Fatal("returned true with no client; want false")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("IdleUntilGatesOpen with a nil client did not return on ctx cancel")
	}
}
