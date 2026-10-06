package mta

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// mustConfigChangedPayload encodes the push body nest fans on an admin edit.
// The reason is advisory — the bridge re-fetches the whole snapshot — so any
// token exercises the same path.
func mustConfigChangedPayload(t *testing.T) []byte {
	t.Helper()
	b, err := dagcbor.Marshal(wsrpc.BridgeConfigChangedPush{Reason: "mail_enabled"})
	if err != nil {
		t.Fatalf("encode BridgeConfigChangedPush: %v", err)
	}
	return b
}

// idleRebindCaller answers fetch_config with a snapshot the test can change,
// and captures the push handler Run installs while idling so the test can
// deliver a `config_changed` through the production seam.
type idleRebindCaller struct {
	mu      sync.Mutex
	snap    wsrpc.ConfigSnapshot
	onPush  wsrpc.PushHandler
	pushSet chan struct{}
}

func newIdleRebindCaller(initial wsrpc.ConfigSnapshot) *idleRebindCaller {
	return &idleRebindCaller{snap: initial, pushSet: make(chan struct{}, 1)}
}

func (c *idleRebindCaller) Call(_ context.Context, method string, _, reply any) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if method != wsrpc.MethodFetchConfig {
		return fmt.Errorf("idleRebindCaller: unexpected method %q", method)
	}
	if p, ok := reply.(*wsrpc.ConfigSnapshot); ok {
		*p = c.snap
	}
	return nil
}

func (c *idleRebindCaller) SetOnPush(h wsrpc.PushHandler) {
	c.mu.Lock()
	c.onPush = h
	c.mu.Unlock()
	select {
	case c.pushSet <- struct{}{}:
	default:
	}
}

func (c *idleRebindCaller) setSnapshot(s wsrpc.ConfigSnapshot) {
	c.mu.Lock()
	c.snap = s
	c.mu.Unlock()
}

func (c *idleRebindCaller) pushConfigChanged(t *testing.T) {
	t.Helper()
	select {
	case <-c.pushSet:
	case <-time.After(2 * time.Second):
		t.Fatal("an idling mta.Run never installed a push handler — it is not subscribed to config_changed")
	}
	c.mu.Lock()
	h := c.onPush
	c.mu.Unlock()
	h(wsrpc.PushKindConfigChanged, mustConfigChangedPayload(t), 1)
}

// TestRunExitsForRebindWhenMailIsEnabledWhileIdling — the cold-started-idle half
// of the exit-for-rebind contract, which only the warm (already-serving) half
// had before.
//
// The negative control is structural: this test NEVER cancels ctx. Run returning
// at all therefore proves the gate-open path fired, since the only other way out
// of the idle branch is a process cancel that never happens here. A regression
// that drops the subscription hangs until the 5s deadline instead of passing.
func TestRunExitsForRebindWhenMailIsEnabledWhileIdling(t *testing.T) {
	t.Parallel()
	caller := newIdleRebindCaller(wsrpc.ConfigSnapshot{
		MailEnabled:   false, // ← the gate that idles this Run
		LocalDomains:  []string{"test.example.com"},
		PrimaryDomain: "test.example.com",
	})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel() // safety net only — the assertion is that Run returns first

	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{
				MailEnabled:   false,
				LocalDomains:  []string{"test.example.com"},
				PrimaryDomain: "test.example.com",
			},
			BridgeID: "test-mta-idle-1",
			Client:   caller,
			// A malformed bind addr sharpens the assert: had Run reached the
			// bind path in-process instead of exiting for a rebind, net.Listen
			// would reject it and Run would return an ERROR, not nil.
			MTABindAddr: "not-an-address",
		})
	}()

	// The admin enables mail from the app: nest persists the toggle, then fans
	// the config_changed push to every approved bridge.
	caller.setSnapshot(wsrpc.ConfigSnapshot{
		MailEnabled:   true,
		LocalDomains:  []string{"test.example.com"},
		PrimaryDomain: "test.example.com",
	})
	caller.pushConfigChanged(t)

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run returned %v; want nil (a clean exit-for-rebind, not a bind attempt)", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("an idling mta.Run did not exit for rebind after mail was enabled")
	}
}

// TestRunExitsForRebindWhenADomainIsAddedWhileIdling — the same contract on the
// MTA's OTHER idle gate. mail-bridge-lifecycle.md § Default-off records this
// exact case as a footgun ("a domain added while they idle is invisible until
// they restart"); one watcher covering both gates closes it, so an
// enabled-but-domainless box comes live on `add_local_domain` too.
func TestRunExitsForRebindWhenADomainIsAddedWhileIdling(t *testing.T) {
	t.Parallel()
	caller := newIdleRebindCaller(wsrpc.ConfigSnapshot{MailEnabled: true})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{
				MailEnabled:  true,
				LocalDomains: nil, // ← no mail_domains rows yet: the second idle gate
			},
			BridgeID:    "test-mta-idle-2",
			Client:      caller,
			MTABindAddr: "not-an-address",
		})
	}()

	caller.setSnapshot(wsrpc.ConfigSnapshot{
		MailEnabled:   true,
		LocalDomains:  []string{"added.example.com"},
		PrimaryDomain: "added.example.com",
	})
	caller.pushConfigChanged(t)

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run returned %v; want nil", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("an idling mta.Run did not exit for rebind after a mail domain was added")
	}
}

// TestBindableNeedsBothGates pins the predicate the idle branches and the idle
// watcher share. Reading it from one place is what keeps them from disagreeing:
// a Bindable that were laxer than the idle branches would restart-loop (wake,
// idle, wake), and one stricter would never wake at all.
func TestBindableNeedsBothGates(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		snap wsrpc.ConfigSnapshot
		want bool
	}{
		{"neither", wsrpc.ConfigSnapshot{}, false},
		{"mail only", wsrpc.ConfigSnapshot{MailEnabled: true}, false},
		{"domain only", wsrpc.ConfigSnapshot{LocalDomains: []string{"d.example"}}, false},
		{"both", wsrpc.ConfigSnapshot{MailEnabled: true, LocalDomains: []string{"d.example"}}, true},
	}
	for _, tc := range cases {
		if got := Bindable(tc.snap); got != tc.want {
			t.Errorf("Bindable(%s) = %v, want %v", tc.name, got, tc.want)
		}
	}
}
