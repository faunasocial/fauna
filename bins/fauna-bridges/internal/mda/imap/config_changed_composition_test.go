package imap

import (
	"context"
	"fmt"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// fetchConfigCaller is a wsrpc.Caller that answers fetch_config with a
// canned snapshot — the seam a real ReconnectingClient occupies in
// production. Any other method is an error.
type fetchConfigCaller struct {
	mu   sync.Mutex
	snap wsrpc.ConfigSnapshot
}

func (f *fetchConfigCaller) Call(_ context.Context, method string, _ any, reply any) error {
	if method != wsrpc.MethodFetchConfig {
		return fmt.Errorf("fetchConfigCaller: unexpected method %q", method)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if p, ok := reply.(*wsrpc.ConfigSnapshot); ok {
		*p = f.snap
	}
	return nil
}

// TestConfigChangedHotReload_ComposesEndToEnd wires the three primitives
// exactly as mda.Run does — PushDispatcher (single installed handler) →
// ConfigReloader (config_changed consumer, re-fetch worker) → backend
// ApplyConfig (idle-timeout hot-swap) — and drives a config_changed push
// through the dispatcher's Handle. It asserts the observable end result a
// real IMAP client would see: a connection opened *after* the push gets the
// re-fetched IDLE timeout, with no restart. This is the in-process proof of
// the composition (mda.Run itself binds TCP and is covered by the tier_3
// e2e); it guards the wiring contract independent of the listener lifecycle.
func TestConfigChangedHotReload_ComposesEndToEnd(t *testing.T) {
	caller := &fetchConfigCaller{snap: wsrpc.ConfigSnapshot{IMAP: wsrpc.ImapPolicy{IdleTimeoutSecs: 7}}}

	pushDispatcher := wsrpc.NewPushDispatcher(slog.Default())
	b := NewBackend(caller, slog.Default(), 0, time.Second, 0, pushDispatcher, nil).(*backend)

	reloader := wsrpc.NewConfigReloader(caller, time.Second, nil)
	reloader.Register(b.ApplyConfig)
	pushDispatcher.Register(wsrpc.PushKindConfigChanged, reloader.Handle)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go reloader.Run(ctx)

	// Before the push: a fresh session uses the boot IDLE timeout (1s).
	if got := b.NewSession(nil).(*Session).idleTimeout; got != time.Second {
		t.Fatalf("pre-push session idleTimeout = %v, want 1s", got)
	}

	// A config_changed push lands on the installed dispatcher → routed to the
	// reloader → off-goroutine re-fetch → ApplyConfig swaps the live value.
	payload, err := dagcbor.Marshal(wsrpc.BridgeConfigChangedPush{Reason: "imap_policy"})
	if err != nil {
		t.Fatalf("encode push: %v", err)
	}
	pushDispatcher.Handle(wsrpc.PushKindConfigChanged, payload, 1)

	// The apply is asynchronous (worker goroutine). Poll a fresh session's
	// timeout until it reflects the re-fetched 7s, bounded.
	deadline := time.Now().Add(2 * time.Second)
	for {
		if got := b.NewSession(nil).(*Session).idleTimeout; got == 7*time.Second {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("IDLE timeout never hot-applied to 7s after config_changed push (got %v)",
				b.NewSession(nil).(*Session).idleTimeout)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
