package wsrpc

import (
	"sync/atomic"
	"testing"
)

// TestRescoreReadyHandler_FiresPoke — a rescore_ready push calls the wrapped
// poke exactly once. This is the seam mda.go wires to rescoreDrain.poke so a
// freshly-delivered mail's per-user obligation drains promptly.
func TestRescoreReadyHandler_FiresPoke(t *testing.T) {
	var fired atomic.Int32
	h := NewRescoreReadyHandler(func() { fired.Add(1) }, nil)

	h.Handle(PushKindRescoreReady, nil, 1)

	if got := fired.Load(); got != 1 {
		t.Fatalf("poke fired %d times, want 1", got)
	}
}

// TestRescoreReadyHandler_IgnoresOtherKinds — a non-rescore_ready push must not
// fire the poke (the dispatcher routes by kind, but the handler guards too since
// it may be installed directly).
func TestRescoreReadyHandler_IgnoresOtherKinds(t *testing.T) {
	var fired atomic.Int32
	h := NewRescoreReadyHandler(func() { fired.Add(1) }, nil)

	h.Handle(PushKindConfigChanged, nil, 1)
	h.Handle(PushKindOutboundReady, nil, 2)

	if got := fired.Load(); got != 0 {
		t.Fatalf("poke fired %d times for non-rescore_ready kinds, want 0", got)
	}
}

// TestRescoreReadyHandler_RoutesThroughDispatcher — registered on the
// PushDispatcher (as mda.go does), the handler fires only for its kind.
func TestRescoreReadyHandler_RoutesThroughDispatcher(t *testing.T) {
	var fired atomic.Int32
	h := NewRescoreReadyHandler(func() { fired.Add(1) }, nil)
	d := NewPushDispatcher(nil)
	d.Register(PushKindRescoreReady, h.Handle)

	d.Handle("some.other.kind", nil, 1) // dropped — no route
	d.Handle(PushKindRescoreReady, nil, 2)

	if got := fired.Load(); got != 1 {
		t.Fatalf("poke fired %d times via dispatcher, want 1", got)
	}
}
