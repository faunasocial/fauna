package davauth

import (
	"testing"
)

// TestSessionMlkemEkAccessor pins the leg-D2a session plumbing: the AUTH'd
// actor's ML-KEM ek (cached at auth beside the MLS pubkey) is surfaced via
// MlkemEk() to the CalDAV and CardDAV body seals (put.go,
// encrypted_metadata.go), which pass it to mailfauna.EncryptToRecipientHybrid
// to seal X-Wing. nil only on a session with no key cached — the synthetic
// NewSession a terminator's tests build. MlkemEk() returns a copy so callers
// cannot mutate session state — same contract as MLSPubkey()/IndexKey().
func TestSessionMlkemEkAccessor(t *testing.T) {
	t.Run("nil on a session with no key cached", func(t *testing.T) {
		s := &Session{}
		if got := s.MlkemEk(); got != nil {
			t.Fatalf("MlkemEk() on a session with no ek = %x, want nil", got)
		}
	})

	t.Run("returns a defensive copy", func(t *testing.T) {
		ek := make([]byte, 1184)
		for i := range ek {
			ek[i] = byte(i)
		}
		s := &Session{actorMlkemEk: ek}

		got := s.MlkemEk()
		if len(got) != 1184 {
			t.Fatalf("MlkemEk() len = %d, want 1184", len(got))
		}
		for i := range got {
			if got[i] != byte(i) {
				t.Fatalf("MlkemEk()[%d] = %d, want %d", i, got[i], byte(i))
			}
		}
		// Mutating the returned slice must not affect session state.
		got[0] ^= 0xFF
		if again := s.MlkemEk(); again[0] != 0 {
			t.Fatalf("MlkemEk() returned an aliased slice — mutation leaked into session state (got[0]=%d)", again[0])
		}
	})
}
