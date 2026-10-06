package mta

import (
	"bytes"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// TestInviteStagePanicKeepsDeliveredMail pins the MTA half of the non-ASCII datetime panic: the
// invitation stage runs AFTER the recipient's copy is delivered, so a panic
// crossing the UniFFI boundary from the shared-Rust reader must not unwind into
// go-smtp's connection-level recover — that would turn the delivered message
// into a 421 and every sender retry into a duplicate delivery. The transaction
// still succeeds (250) and the local copy is ingested exactly once.
//
// Not parallel: it swaps the package-level reader, and a sequential test
// finishes before any parallel test's body runs.
func TestInviteStagePanicKeepsDeliveredMail(t *testing.T) {
	calls := 0
	orig := inviteFromMail
	inviteFromMail = func([]byte, int64) *mailfauna.InboundInvite {
		calls++
		panic("hostile invitation")
	}
	t.Cleanup(func() { inviteFromMail = orig })

	mc := newMockCaller()
	s := forwardingSession(t, mc, bytes.Repeat([]byte{0x42}, 32), "alice@example.org")

	if err := s.Data(bytes.NewReader([]byte(filterForwardBody))); err != nil {
		t.Fatalf("a post-delivery invite panic must not fail the transaction: %v", err)
	}
	if calls != 1 {
		t.Fatalf("the invite stage ran %d times, want 1 (the seam was not reached)", calls)
	}
	if got := len(mc.ingestRequests); got != 1 {
		t.Fatalf("local copy ingested %d times, want exactly 1", got)
	}
}
