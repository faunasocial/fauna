// PrepareIdle — the IDLE push subscription is live before `+ idling`.
//
// tier_2 like idle_test.go (whose fakes and helpers these reuse): the real
// per-Session code path over a stubbed wsrpc.Caller and metadata fetcher.
// The wire half — that the fork calls PrepareIdle before writing the
// continuation — is pinned in the fork's idle_prepare_fauna_test.go.
package imap

import (
	"context"
	"errors"
	"log/slog"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestIdle_PrepareSubscribesBeforeIdleRuns pins the ordering the fork's
// IDLE-prepare seam (FORK.md row 20) relies on: PrepareIdle registers the
// nest subscription AND its router route, so a push arriving before the IDLE
// loop has even started is buffered and emitted, not dropped as an unknown
// subscription. Causal: the push is injected after PrepareIdle returns and
// before idle starts; idle consumes the prepared registration (one subscribe
// in all) and releases its route on return.
func TestIdle_PrepareSubscribesBeforeIdleRuns(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 41}
	router := newNotificationRouter(slog.Default())
	fetcher := &metadataFetcherFake{}
	fetcher.setUIDs([]uint32{1, 2})
	s := sessionForIDLE(t, "INBOX", caller, router, fetcher)
	w := &updateWriterFake{}

	if err := s.PrepareIdle(); err != nil {
		t.Fatalf("PrepareIdle: %v", err)
	}
	// The other mail app's change lands now — after the continuation could
	// have been written, before the IDLE loop runs.
	pushEvent(t, router, 41, wsrpc.MailboxStateEvent{
		Kind: wsrpc.MailboxStateEventAppend, Uid: 2, Modseq: 7,
	})

	stop := make(chan struct{})
	done := make(chan error, 1)
	go func() { done <- s.idle(context.Background(), w, stop) }()
	deadline := time.Now().Add(5 * time.Second)
	for {
		if exists, _, _ := w.snapshot(); len(exists) > 0 {
			if exists[0] != 2 {
				t.Errorf("WriteNumMessages: %v, want [2]", exists)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the push that arrived between PrepareIdle and the IDLE loop was never emitted")
		}
		time.Sleep(5 * time.Millisecond)
	}
	close(stop)
	if err := <-done; err != nil {
		t.Fatalf("idle: %v", err)
	}
	caller.mu.Lock()
	calls := caller.subscribeCalls
	caller.mu.Unlock()
	if calls != 1 {
		t.Errorf("subscribe calls = %d, want 1 (idle consumes the prepared subscription)", calls)
	}
	router.mu.Lock()
	_, routed := router.routes[41]
	router.mu.Unlock()
	if routed {
		t.Error("the route outlived the IDLE")
	}
}

// TestIdle_PrepareFailureSurfacesError — a failed subscribe in PrepareIdle
// returns the error (the fork then refuses the IDLE before `+`) and leaves
// no route behind.
func TestIdle_PrepareFailureSurfacesError(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscribeErr: errors.New("simulated nest error")}
	router := newNotificationRouter(slog.Default())
	s := sessionForIDLE(t, "INBOX", caller, router, &metadataFetcherFake{})

	if err := s.PrepareIdle(); err == nil {
		t.Fatal("PrepareIdle: want error, got nil")
	}
	router.mu.Lock()
	n := len(router.routes)
	router.mu.Unlock()
	if n != 0 {
		t.Errorf("routes after a failed PrepareIdle = %d, want 0", n)
	}
}

// TestSession_CloseUnregistersPreparedIdle — a prepared registration whose
// IDLE never ran (the continuation write failed, the connection dropped) is
// released when the session closes.
func TestSession_CloseUnregistersPreparedIdle(t *testing.T) {
	t.Parallel()
	caller := &idleSubscribeCaller{subscriptionID: 43}
	router := newNotificationRouter(slog.Default())
	s := sessionForIDLE(t, "INBOX", caller, router, &metadataFetcherFake{})

	if err := s.PrepareIdle(); err != nil {
		t.Fatalf("PrepareIdle: %v", err)
	}
	_ = s.Close()
	router.mu.Lock()
	_, routed := router.routes[43]
	router.mu.Unlock()
	if routed {
		t.Error("Close left the prepared IDLE route registered")
	}
}
