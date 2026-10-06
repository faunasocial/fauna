package wsrpc

import (
	"sync"
	"sync/atomic"
	"testing"
)

// TestPushDispatcher_RoutesByKind — a frame is delivered only to the
// handler registered for its kind, with payload + seq passed through
// verbatim.
func TestPushDispatcher_RoutesByKind(t *testing.T) {
	d := NewPushDispatcher(nil)

	var gotKind string
	var gotPayload []byte
	var gotSeq uint64
	d.Register("kind.a", func(kind string, payload []byte, seq uint64) {
		gotKind, gotPayload, gotSeq = kind, payload, seq
	})

	var otherCalled bool
	d.Register("kind.b", func(string, []byte, uint64) { otherCalled = true })

	d.Handle("kind.a", []byte{0x01, 0x02}, 42)

	if gotKind != "kind.a" {
		t.Fatalf("kind = %q, want kind.a", gotKind)
	}
	if string(gotPayload) != string([]byte{0x01, 0x02}) {
		t.Fatalf("payload = %v, want [1 2]", gotPayload)
	}
	if gotSeq != 42 {
		t.Fatalf("seq = %d, want 42", gotSeq)
	}
	if otherCalled {
		t.Fatal("kind.b handler ran for a kind.a frame")
	}
}

// TestPushDispatcher_UnknownKindDropped — a frame for an unregistered
// kind is dropped without panic and without invoking any handler.
func TestPushDispatcher_UnknownKindDropped(t *testing.T) {
	d := NewPushDispatcher(nil)
	var called bool
	d.Register("kind.a", func(string, []byte, uint64) { called = true })

	d.Handle("kind.unregistered", []byte{0xff}, 7) // must not panic

	if called {
		t.Fatal("registered handler ran for an unregistered kind")
	}
}

// TestPushDispatcher_RegisterReplaces — re-registering a kind replaces
// the prior handler (last-writer-wins; in practice each kind has one
// owner, but the contract must be deterministic).
func TestPushDispatcher_RegisterReplaces(t *testing.T) {
	d := NewPushDispatcher(nil)
	var first, second bool
	d.Register("kind.a", func(string, []byte, uint64) { first = true })
	d.Register("kind.a", func(string, []byte, uint64) { second = true })

	d.Handle("kind.a", nil, 1)

	if first {
		t.Fatal("first handler ran after being replaced")
	}
	if !second {
		t.Fatal("replacement handler did not run")
	}
}

// TestPushDispatcher_ConcurrentRegisterHandle — Register (per-session
// goroutines) races Handle (the single reader goroutine) without a data
// race (run under -race). Mirrors the notificationRouter concurrency
// contract: Register on session goroutines, Handle on the reader.
func TestPushDispatcher_ConcurrentRegisterHandle(t *testing.T) {
	d := NewPushDispatcher(nil)
	var hits int64
	d.Register("kind.a", func(string, []byte, uint64) { atomic.AddInt64(&hits, 1) })

	var wg sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(2)
		go func() { defer wg.Done(); d.Register("kind.x", func(string, []byte, uint64) {}) }()
		go func() { defer wg.Done(); d.Handle("kind.a", nil, 1) }()
	}
	wg.Wait()

	if atomic.LoadInt64(&hits) != 8 {
		t.Fatalf("kind.a hits = %d, want 8", atomic.LoadInt64(&hits))
	}
}
