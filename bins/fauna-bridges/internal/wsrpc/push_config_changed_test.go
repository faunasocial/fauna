package wsrpc

import (
	"context"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// fakeFetchCaller is a Caller that answers fetch_config with a canned
// snapshot and counts the calls. Any other method is an error (the
// reloader must only ever issue fetch_config).
type fakeFetchCaller struct {
	mu     sync.Mutex
	fetchN int
	snap   ConfigSnapshot
	err    error
}

func (f *fakeFetchCaller) Call(_ context.Context, method string, _ any, reply any) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if method != MethodFetchConfig {
		return fmt.Errorf("fakeFetchCaller: unexpected method %q", method)
	}
	f.fetchN++
	if f.err != nil {
		return f.err
	}
	if p, ok := reply.(*ConfigSnapshot); ok {
		*p = f.snap
	}
	return nil
}

func (f *fakeFetchCaller) calls() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.fetchN
}

func configChangedPayload(t *testing.T, reason string) []byte {
	t.Helper()
	b, err := dagcbor.Marshal(BridgeConfigChangedPush{Reason: reason})
	if err != nil {
		t.Fatalf("encode BridgeConfigChangedPush: %v", err)
	}
	return b
}

// TestConfigReloader_PushTriggersRefetchAndApply — a config_changed push
// makes the reloader re-fetch the whole snapshot and fan it to every
// registered applier. The push payload's `reason` is advisory: the
// applied snapshot is the *re-fetched* one, not anything in the push.
func TestConfigReloader_PushTriggersRefetchAndApply(t *testing.T) {
	caller := &fakeFetchCaller{snap: ConfigSnapshot{IMAP: ImapPolicy{IdleTimeoutSecs: 99}}}
	r := NewConfigReloader(caller, time.Second, nil)

	applied := make(chan ConfigSnapshot, 1)
	r.Register(func(s ConfigSnapshot) { applied <- s })

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go r.Run(ctx)

	// reason names the IMAP policy edit; the reloader ignores it and
	// re-fetches everything regardless.
	r.Handle(PushKindConfigChanged, configChangedPayload(t, configChangeReasonIMAPPolicy), 1)

	select {
	case got := <-applied:
		if got.IMAP.IdleTimeoutSecs != 99 {
			t.Fatalf("applied IdleTimeoutSecs = %d, want 99 (the re-fetched value)", got.IMAP.IdleTimeoutSecs)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("applier never ran after a config_changed push")
	}
	if n := caller.calls(); n != 1 {
		t.Fatalf("fetch_config calls = %d, want 1", n)
	}
}

// TestConfigReloader_IgnoresOtherKinds — a non-config_changed push must
// not trigger a re-fetch (the dispatcher already routes by kind, but the
// reloader guards too, since it may be installed directly in tests).
func TestConfigReloader_IgnoresOtherKinds(t *testing.T) {
	caller := &fakeFetchCaller{}
	r := NewConfigReloader(caller, time.Second, nil)
	r.Register(func(ConfigSnapshot) { t.Fatal("applier ran for a non-config_changed kind") })

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go r.Run(ctx)

	r.Handle(BridgeMailboxStatePushKind, nil, 1)

	time.Sleep(150 * time.Millisecond)
	if n := caller.calls(); n != 0 {
		t.Fatalf("fetch_config calls = %d, want 0 for a non-config_changed kind", n)
	}
}

// TestConfigReloader_ApplyFansWithoutFetch — Apply is the seam the
// reconnect path uses: it pushes an already-fetched snapshot straight to
// the appliers, with no fetch_config round-trip. Unifies push-prompted
// and reconnect-prompted hot-apply through one applier list.
func TestConfigReloader_ApplyFansWithoutFetch(t *testing.T) {
	caller := &fakeFetchCaller{}
	r := NewConfigReloader(caller, time.Second, nil)

	var got1, got2 ConfigSnapshot
	r.Register(func(s ConfigSnapshot) { got1 = s })
	r.Register(func(s ConfigSnapshot) { got2 = s })

	r.Apply(ConfigSnapshot{PrimaryDomain: "example.test"})

	if got1.PrimaryDomain != "example.test" || got2.PrimaryDomain != "example.test" {
		t.Fatalf("Apply did not fan to both appliers: %q %q", got1.PrimaryDomain, got2.PrimaryDomain)
	}
	if n := caller.calls(); n != 0 {
		t.Fatalf("Apply issued %d fetch_config calls, want 0", n)
	}
}
