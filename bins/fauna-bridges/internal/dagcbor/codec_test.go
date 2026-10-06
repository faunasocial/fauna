package dagcbor

import (
	"bytes"
	"strings"
	"testing"
)

// TestMapKeySortedRoundTrip pins the IPLD DAG-CBOR length-first map-key
// sort by asserting the wire bytes. If a future refactor changes Sort
// to SortBytewiseLexical, this test catches it before anyone hashes a
// non-canonical envelope and signs it.
func TestMapKeySortedRoundTrip(t *testing.T) {
	t.Parallel()
	in := map[string]any{"b": uint64(2), "a": uint64(1), "c": uint64(3)}
	b, err := Marshal(in)
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}
	// 0xA3 = major type 5 (map), 3 entries.
	if b[0] != 0xA3 {
		t.Fatalf("first byte = %#x, want 0xA3 (map of 3)", b[0])
	}
	// Three single-char keys all encode as length-1 text strings (0x61),
	// so length-first and bytewise-lexicographic happen to agree here on
	// the *first* byte; the discriminator is the key bytes themselves.
	// Expected layout: 0xA3 0x61 'a' 0x01 0x61 'b' 0x02 0x61 'c' 0x03.
	want := []byte{0xA3, 0x61, 'a', 0x01, 0x61, 'b', 0x02, 0x61, 'c', 0x03}
	if !bytes.Equal(b, want) {
		t.Fatalf("wire bytes:\n got  %x\n want %x", b, want)
	}

	// Length-first sort: a longer key sorts AFTER a shorter one, even
	// when the longer one's bytes would precede the shorter under pure
	// bytewise comparison. {"aa": 0, "z": 1} must encode "z" first.
	mixed := map[string]any{"aa": uint64(0), "z": uint64(1)}
	b2, err := Marshal(mixed)
	if err != nil {
		t.Fatalf("Marshal mixed: %v", err)
	}
	// Expected: 0xA2 0x61 'z' 0x01 0x62 'a' 'a' 0x00 — "z" (len 1)
	// before "aa" (len 2), per length-first.
	want2 := []byte{0xA2, 0x61, 'z', 0x01, 0x62, 'a', 'a', 0x00}
	if !bytes.Equal(b2, want2) {
		t.Fatalf("length-first map sort wrong:\n got  %x\n want %x\n(if got pure bytewise this means cbor.SortBytewiseLexical leaked in; the IPLD DAG-CBOR spec mandates length-first)", b2, want2)
	}

	// Round-trip value check.
	got, err := Unmarshal[map[string]uint64](b)
	if err != nil {
		t.Fatalf("Unmarshal: %v", err)
	}
	if got["a"] != 1 || got["b"] != 2 || got["c"] != 3 {
		t.Fatalf("round-trip mismatch: %v", got)
	}
}

// TestRejectsFloats asserts the reflect-walker float-rejection on
// Marshal fires for every reachable float position.
func TestRejectsFloats(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		v    any
	}{
		{"top-level float", float64(1.5)},
		{"map value", map[string]any{"x": 1.5}},
		{"nested map", map[string]any{"x": map[string]any{"y": 2.5}}},
		{"slice element", []any{uint64(1), 2.5, uint64(3)}},
		{"struct field", struct{ F float64 }{F: 1.5}},
		{"pointer to float", func() any { f := 1.5; return &f }()},
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := Marshal(tc.v)
			if err == nil {
				t.Fatalf("Marshal succeeded for %v; expected float-rejection error", tc.v)
			}
			msg := err.Error()
			if !strings.Contains(strings.ToLower(msg), "float") && !strings.Contains(strings.ToUpper(msg), "DAG-CBOR") {
				t.Fatalf("error %q does not mention 'float' or 'DAG-CBOR'", msg)
			}
		})
	}
}

// TestRejectsIndefiniteLength asserts the decoder rejects an
// indefinite-length array (major type 4, 0x9F leader, 0xFF terminator).
func TestRejectsIndefiniteLength(t *testing.T) {
	t.Parallel()
	// 0x9F = array, indefinite length; 0x01 = uint 1; 0xFF = break.
	wire := []byte{0x9F, 0x01, 0xFF}
	_, err := Unmarshal[[]any](wire)
	if err == nil {
		t.Fatalf("Unmarshal accepted indefinite-length array; want error")
	}
}

// TestCanonicalRoundTrip asserts that two equal inputs marshal to
// byte-identical output. Encoders that defer key sorting until decode
// (or that randomise map iteration) fail this test.
func TestCanonicalRoundTrip(t *testing.T) {
	t.Parallel()
	a := map[string]any{
		"correlation_id": uint64(42),
		"kind":           "fauna.bridges.fetch_config",
		"idempotency":    []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16},
		"payload":        map[string]any{"role": "mta", "nested": map[string]any{"k": uint64(7)}},
		"replay":         true,
	}
	b1, err := Marshal(a)
	if err != nil {
		t.Fatalf("Marshal #1: %v", err)
	}
	// Build the second map with a different insertion order so a sort
	// regression (e.g. "sort only on first encode") would surface.
	b := map[string]any{
		"replay":         true,
		"payload":        map[string]any{"nested": map[string]any{"k": uint64(7)}, "role": "mta"},
		"idempotency":    []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16},
		"kind":           "fauna.bridges.fetch_config",
		"correlation_id": uint64(42),
	}
	b2, err := Marshal(b)
	if err != nil {
		t.Fatalf("Marshal #2: %v", err)
	}
	if !bytes.Equal(b1, b2) {
		t.Fatalf("canonical-round-trip not byte-equal:\n #1: %x\n #2: %x", b1, b2)
	}
	// Sanity-check: encode is non-trivial.
	if len(b1) < 30 {
		t.Fatalf("encoded payload looks suspiciously short: %x", b1)
	}
}
