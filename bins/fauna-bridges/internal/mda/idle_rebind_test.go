package mda

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// idleRebindCaller answers fetch_config with a snapshot the test can change,
// and captures the push handler an idling Run installs so the test can deliver
// a `config_changed` through the production seam. Twin of the MTA package's.
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
		t.Fatal("an idling mda.Run never installed a push handler — it is not subscribed to config_changed")
	}
	c.mu.Lock()
	h := c.onPush
	c.mu.Unlock()
	b, err := dagcbor.Marshal(wsrpc.BridgeConfigChangedPush{Reason: "mail_enabled"})
	if err != nil {
		t.Fatalf("encode BridgeConfigChangedPush: %v", err)
	}
	h(wsrpc.PushKindConfigChanged, b, 1)
}

// TestRunExitsForRebindWhenAProtocolIsEnabledWhileIdling — the all-off MDA is
// the state row 86 traced the fleet-wide `dedicated_mail_nest` breakage to: it
// idled with no config_changed subscription, so the client's enable-mail UI step
// could never bring it live and every consuming test saw a bridge with no
// listeners. It now exits for rebind, the same way step 5's gating watcher does
// once serving.
//
// ctx is deliberately never cancelled: Run returning at all is what proves the
// gate-open path fired.
func TestRunExitsForRebindWhenAProtocolIsEnabledWhileIdling(t *testing.T) {
	t.Parallel()
	caller := newIdleRebindCaller(wsrpc.ConfigSnapshot{})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel() // safety net only

	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			// Every protocol off → the all-off idle gate.
			Snapshot: wsrpc.ConfigSnapshot{},
			BridgeID: "test-mda-idle-1",
			Client:   caller,
			// Listen addrs left empty on purpose: had Run fallen through to the
			// bind path instead of exiting for a rebind, the empty-addr wiring
			// check would return an ERROR, so a nil return is a sharp assert.
		})
	}()

	// The admin enables mail from the app.
	caller.setSnapshot(wsrpc.ConfigSnapshot{MailEnabled: true})
	caller.pushConfigChanged(t)

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run returned %v; want nil (a clean exit-for-rebind, not a bind attempt)", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("an idling mda.Run did not exit for rebind after mail was enabled")
	}
}

// TestRunExitsForRebindOnADAVOnlyEnable — the DAV axes are independent of mail
// (caldav-server.md § Independent enablement), so a box that turns on ONLY
// CalDAV must wake the MDA too. This is the arm the `caldav_only` fixtures
// exercise, and the reason the idle predicate is "any protocol", not "mail".
func TestRunExitsForRebindOnADAVOnlyEnable(t *testing.T) {
	t.Parallel()
	caller := newIdleRebindCaller(wsrpc.ConfigSnapshot{})

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Deps{
			Snapshot: wsrpc.ConfigSnapshot{},
			BridgeID: "test-mda-idle-2",
			Client:   caller,
		})
	}()

	caller.setSnapshot(wsrpc.ConfigSnapshot{CalDAVEnabled: true})
	caller.pushConfigChanged(t)

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Run returned %v; want nil", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("an idling mda.Run did not exit for rebind after CalDAV alone was enabled")
	}
}

// TestBindableIsAnyProtocol pins the predicate the all-off idle branch and its
// watcher share — including WebDAV, whose omission would strand a WebDAV-only
// box idling forever.
func TestBindableIsAnyProtocol(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		snap wsrpc.ConfigSnapshot
		want bool
	}{
		{"all off", wsrpc.ConfigSnapshot{}, false},
		{"mail", wsrpc.ConfigSnapshot{MailEnabled: true}, true},
		{"caldav", wsrpc.ConfigSnapshot{CalDAVEnabled: true}, true},
		{"carddav", wsrpc.ConfigSnapshot{CardDAVEnabled: true}, true},
		{"webdav", wsrpc.ConfigSnapshot{WebDAVEnabled: true}, true},
	}
	for _, tc := range cases {
		if got := Bindable(tc.snap); got != tc.want {
			t.Errorf("Bindable(%s) = %v, want %v", tc.name, got, tc.want)
		}
	}
}
