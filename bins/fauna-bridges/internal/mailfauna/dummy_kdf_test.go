package mailfauna

import (
	"testing"
	"time"
)

// F3 (network-exposure.md § Rulings F3): the dummy KDF must ACTUALLY run an
// Argon2id, so an unknown-user AUTH-fail incurs the same cost as a real
// wrong-password unwrap. One interactive Argon2id (64 MiB, t=2) takes well over
// 5ms; an absent/no-op KDF returns in microseconds. The bound is one-sided —
// Argon2id is memory-hard and never gets *faster* under load — so it is not
// flaky. RED if the unknown-user path skips the KDF.
func TestDummyCredentialKDFRunsArgon2id(t *testing.T) {
	PrewarmDummyKDF() // mint the blob (one-time seal) up front, off the clock
	if dummyArgon2idBlob == nil {
		t.Fatal("dummy blob was not minted (SealWrappedMsekBlob failed?)")
	}

	start := time.Now()
	DummyCredentialKDF([]byte("any-password"), KdfKindArgon2id, "198.51.100.3")
	elapsed := time.Since(start)

	const floor = 5 * time.Millisecond
	if elapsed < floor {
		t.Fatalf("dummy Argon2id too fast (%v < %v) — the KDF is not running, "+
			"so the F3 timing gap is not closed", elapsed, floor)
	}
}

// The HKDF arm (OAUTHBEARER) is microsecond-cheap and has no meaningful timing
// gap, so DummyCredentialKDF must be a no-op there — we don't want to burn a
// full Argon2id on every OAUTHBEARER unknown-user probe. Asserted on the gate's
// admitted-run counter, not a wall-clock ceiling: an upper time bound fails on a
// loaded machine (it did, at 2 ms), while the counter reads the same under any
// load. RED if the HKDF arm runs an Argon2id.
func TestDummyCredentialKDFHkdfIsNoop(t *testing.T) {
	PrewarmDummyKDF()
	before := KDFRunCount()
	if err := DummyCredentialKDF([]byte("token"), KdfKindHkdf, "198.51.100.3"); err != nil {
		t.Fatalf("HKDF dummy: %v", err)
	}
	if ran := KDFRunCount() - before; ran != 0 {
		t.Fatalf("HKDF dummy ran %d Argon2id, want 0", ran)
	}
}
