package wsrpc

import (
	"bytes"
	"sync/atomic"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// TestSpamBaselinePublishHandler_FiresPokeWithRunID — a spam_baseline_publish
// push decodes the run_id and hands it to the wrapped poke exactly once. This
// is the seam mda.go wires to the spam-baseline drain so an admin publish
// drains against exactly the nest's pending run.
func TestSpamBaselinePublishHandler_FiresPokeWithRunID(t *testing.T) {
	runID := []byte{0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12}
	var fired atomic.Int32
	var got []byte
	h := NewSpamBaselinePublishHandler(func(r []byte) {
		fired.Add(1)
		got = r
	}, nil)

	payload, err := dagcbor.Marshal(BridgeSpamBaselinePublishPush{RunID: runID})
	if err != nil {
		t.Fatalf("marshal push: %v", err)
	}
	h.Handle(PushKindSpamBaselinePublish, payload, 1)

	if n := fired.Load(); n != 1 {
		t.Fatalf("poke fired %d times, want 1", n)
	}
	if !bytes.Equal(got, runID) {
		t.Fatalf("poke got run_id %x, want %x", got, runID)
	}
}

// TestSpamBaselinePublishHandler_DropsUndecodableOrEmptyRunID — a malformed or
// empty-run_id payload must not poke (the run times out nest-side → honest
// skip; nothing is lost, but a bogus poke would pull an empty worklist for a
// phantom run).
func TestSpamBaselinePublishHandler_DropsUndecodableOrEmptyRunID(t *testing.T) {
	var fired atomic.Int32
	h := NewSpamBaselinePublishHandler(func([]byte) { fired.Add(1) }, nil)

	h.Handle(PushKindSpamBaselinePublish, []byte{0xFF, 0xFF, 0xFF}, 1) // undecodable CBOR
	emptyRun, err := dagcbor.Marshal(BridgeSpamBaselinePublishPush{RunID: nil})
	if err != nil {
		t.Fatalf("marshal empty-run push: %v", err)
	}
	h.Handle(PushKindSpamBaselinePublish, emptyRun, 2)

	if n := fired.Load(); n != 0 {
		t.Fatalf("poke fired %d times for undecodable/empty run_id, want 0", n)
	}
}

// TestSpamBaselinePublishHandler_IgnoresOtherKinds — a non-matching push must
// not fire the poke (the dispatcher routes by kind, but the handler guards too
// since it may be installed directly).
func TestSpamBaselinePublishHandler_IgnoresOtherKinds(t *testing.T) {
	var fired atomic.Int32
	h := NewSpamBaselinePublishHandler(func([]byte) { fired.Add(1) }, nil)

	h.Handle(PushKindConfigChanged, nil, 1)
	h.Handle(PushKindRescoreReady, nil, 2)

	if n := fired.Load(); n != 0 {
		t.Fatalf("poke fired %d times for non-matching kinds, want 0", n)
	}
}

// TestSpamBaselinePublishHandler_RoutesThroughDispatcher — registered on the
// PushDispatcher (as mda.go does), the handler fires only for its kind.
func TestSpamBaselinePublishHandler_RoutesThroughDispatcher(t *testing.T) {
	runID := []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	var fired atomic.Int32
	h := NewSpamBaselinePublishHandler(func([]byte) { fired.Add(1) }, nil)
	d := NewPushDispatcher(nil)
	d.Register(PushKindSpamBaselinePublish, h.Handle)

	payload, err := dagcbor.Marshal(BridgeSpamBaselinePublishPush{RunID: runID})
	if err != nil {
		t.Fatalf("marshal push: %v", err)
	}
	d.Handle("some.other.kind", nil, 1) // dropped — no route
	d.Handle(PushKindSpamBaselinePublish, payload, 2)

	if n := fired.Load(); n != 1 {
		t.Fatalf("poke fired %d times via dispatcher, want 1", n)
	}
}
