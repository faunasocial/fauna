package wsrpc

import (
	"sync/atomic"
	"testing"
)

// TestOutboundReadyHandler_FiresTrigger — an outbound_ready push calls the
// wrapped trigger exactly once. This is the seam mta.go wires to
// OutboundWorker.Trigger so a client send relays promptly.
func TestOutboundReadyHandler_FiresTrigger(t *testing.T) {
	var fired atomic.Int32
	h := NewOutboundReadyHandler(func() { fired.Add(1) }, nil)

	h.Handle(PushKindOutboundReady, nil, 1)

	if got := fired.Load(); got != 1 {
		t.Fatalf("trigger fired %d times, want 1", got)
	}
}

// TestOutboundReadyHandler_IgnoresOtherKinds — a non-outbound_ready push
// must not fire the trigger (the dispatcher routes by kind, but the handler
// guards too since it may be installed directly).
func TestOutboundReadyHandler_IgnoresOtherKinds(t *testing.T) {
	var fired atomic.Int32
	h := NewOutboundReadyHandler(func() { fired.Add(1) }, nil)

	h.Handle(PushKindConfigChanged, nil, 1)
	h.Handle(BridgeMailboxStatePushKind, nil, 2)

	if got := fired.Load(); got != 0 {
		t.Fatalf("trigger fired %d times for non-outbound_ready kinds, want 0", got)
	}
}

// TestOutboundReadyHandler_RoutesThroughDispatcher — registered on the
// PushDispatcher (as mta.go does), the handler fires only for its kind.
func TestOutboundReadyHandler_RoutesThroughDispatcher(t *testing.T) {
	var fired atomic.Int32
	h := NewOutboundReadyHandler(func() { fired.Add(1) }, nil)
	d := NewPushDispatcher(nil)
	d.Register(PushKindOutboundReady, h.Handle)

	d.Handle("some.other.kind", nil, 1) // dropped — no route
	d.Handle(PushKindOutboundReady, nil, 2)

	if got := fired.Load(); got != 1 {
		t.Fatalf("trigger fired %d times via dispatcher, want 1", got)
	}
}
