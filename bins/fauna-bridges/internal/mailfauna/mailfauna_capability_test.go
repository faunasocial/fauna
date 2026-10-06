package mailfauna

import (
	"strings"
	"testing"
)

// Slice 4 smoke test — confirms the Go wrapper round-trips through the UniFFI
// surface to fauna_ffi::unseal_capability_grant. Cryptographic correctness (a
// real seal→unseal round-trip over every scope key) is pinned in Rust at
// libs/fauna-ffi/tests/wrapped_blob_ffi.rs (capability_grant_round_trips_*);
// this file only proves the real cgo path is invocable end-to-end and surfaces
// errors cleanly (the capability holder loop's unit tests use a seam for the
// unseal step, so this closes that gap). Design § Phase 2 Step 2 § 2.3.

// A malformed grant blob → a clean decode error through the real cgo boundary
// (not a panic). Guards the FFI plumbing itself.
func TestUnsealCapabilityGrantMalformedBlobErrors(t *testing.T) {
	_, err := UnsealCapabilityGrant([]byte{0x00, 0x01, 0x02}, make([]byte, 32), nil)
	if err == nil {
		t.Fatal("expected a decode error on a malformed grant blob; got nil")
	}
	if !strings.Contains(strings.ToLower(err.Error()), "decode") {
		t.Fatalf("expected a decode error to surface; got %q", err.Error())
	}
}

// A wrong-length holder secret → a clean length error through the FFI.
func TestUnsealCapabilityGrantShortSecretErrors(t *testing.T) {
	_, err := UnsealCapabilityGrant([]byte{0x00}, make([]byte, 31), nil)
	if err == nil {
		t.Fatal("expected a length error on a 31-byte holder secret; got nil")
	}
	if !strings.Contains(err.Error(), "32") {
		t.Fatalf("expected a 32-byte length error to surface; got %q", err.Error())
	}
}
