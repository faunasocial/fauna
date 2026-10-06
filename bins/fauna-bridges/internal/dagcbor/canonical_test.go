package dagcbor

import (
	"strings"
	"testing"
)

// TestValidateCanonicalSmoke compile-checks ValidateCanonical and
// asserts a few hand-crafted canonical byte strings pass. The full
// negative corpus mirroring libs/fauna-cbor/tests/decode_strict_rejects.rs
// lands in Task 2.12.
func TestValidateCanonicalSmoke(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		b    []byte
	}{
		// 0 (positive int, inline).
		{"int-0", []byte{0x00}},
		// 23 (positive int, inline max).
		{"int-23", []byte{0x17}},
		// 24 (positive int, 1-byte ext — shortest form for >= 24).
		{"int-24", []byte{0x18, 0x18}},
		// Empty byte string.
		{"bstr-empty", []byte{0x40}},
		// "abc" text string.
		{"tstr-abc", []byte{0x63, 'a', 'b', 'c'}},
		// Empty array.
		{"arr-empty", []byte{0x80}},
		// Empty map.
		{"map-empty", []byte{0xA0}},
		// Map {"a": 1, "b": 2} — length-first sorted, no duplicates.
		{"map-sorted", []byte{0xA2, 0x61, 'a', 0x01, 0x61, 'b', 0x02}},
		// Map {"z": 1, "aa": 0} — length-first puts "z" before "aa".
		{"map-length-first", []byte{0xA2, 0x61, 'z', 0x01, 0x62, 'a', 'a', 0x00}},
		// false/true/null/undefined (simple values inline).
		{"false", []byte{0xF4}},
		{"true", []byte{0xF5}},
		{"null", []byte{0xF6}},
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if err := ValidateCanonical(tc.b); err != nil {
				t.Fatalf("ValidateCanonical(%x) = %v, want nil", tc.b, err)
			}
		})
	}
}

// TestValidateCanonicalRejectsTrailingData spot-checks one rejection
// axis to confirm the validator actually returns errors (the negative
// corpus below covers the full axis matrix).
func TestValidateCanonicalRejectsTrailingData(t *testing.T) {
	t.Parallel()
	// Two consecutive complete items: 0x00 then 0x00. The first item
	// consumes one byte; the second is trailing data.
	b := []byte{0x00, 0x00}
	err := ValidateCanonical(b)
	if err == nil {
		t.Fatalf("ValidateCanonical(%x) = nil, want trailing-data error", b)
	}
	if !strings.Contains(err.Error(), "trailing data") {
		t.Fatalf("ValidateCanonical(%x) = %v, want substring %q", b, err, "trailing data")
	}
}

// --- Negative corpus, one-to-one mirror of
// libs/fauna-cbor/tests/decode_strict_rejects.rs. Each Go test below
// has a sibling Rust #[test] with the same byte input and the same
// rejection axis. Go's emitted axis strings are read off canonical.go;
// they differ slightly in wording from the Rust validator but mean the
// same thing. The contract under test is "Go and Rust reject the same
// bytes on the same axis", not "Go and Rust emit identical strings".

// assertCanonicalViolation runs ValidateCanonical on bytes and asserts
// the error message contains wantAxis as a substring. Mirrors Rust's
// assert_canonical_violation helper.
func assertCanonicalViolation(t *testing.T, bytes []byte, wantAxis string) {
	t.Helper()
	err := ValidateCanonical(bytes)
	if err == nil {
		t.Fatalf("ValidateCanonical(%x) = nil, want canonical-violation containing %q", bytes, wantAxis)
	}
	if !strings.Contains(err.Error(), wantAxis) {
		t.Fatalf("ValidateCanonical(%x) = %q, want substring %q", bytes, err.Error(), wantAxis)
	}
}

// --- non-shortest-form integers (positive) ---

// TestRejectOneByteExtForValue5 mirrors rejects_one_byte_ext_for_value_5.
// 0x18 0x05 — 1-byte extension carrying value 5; canonical is 0x05.
func TestRejectOneByteExtForValue5(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0x18, 0x05}, "non-shortest-form integer")
}

// TestRejectTwoByteExtForValue5 mirrors rejects_two_byte_ext_for_value_5.
// 0x19 0x00 0x05 — 2-byte extension carrying 5.
func TestRejectTwoByteExtForValue5(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0x19, 0x00, 0x05}, "non-shortest-form integer")
}

// TestRejectFourByteExtForValue5 mirrors rejects_four_byte_ext_for_value_5.
// 0x1a 0x00 0x00 0x00 0x05 — 4-byte extension carrying 5.
func TestRejectFourByteExtForValue5(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0x1a, 0x00, 0x00, 0x00, 0x05}, "non-shortest-form integer")
}

// TestRejectEightByteExtForValue5 mirrors rejects_eight_byte_ext_for_value_5.
// 0x1b 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x05 — 8-byte extension.
func TestRejectEightByteExtForValue5(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t,
		[]byte{0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05},
		"non-shortest-form integer")
}

// --- non-shortest-form integers (negative) ---

// TestRejectNegativeOneByteExtForMinus6 mirrors rejects_negative_one_byte_ext_for_minus_6.
// 0x38 0x05 — negint with 1-byte extension carrying 5 (means -6).
// Canonical is 0x25 (negint inline 5 = -6).
func TestRejectNegativeOneByteExtForMinus6(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0x38, 0x05}, "non-shortest-form integer")
}

// --- map ordering: length-first ---

// TestRejectMapWrongBytewiseOrderSameLength mirrors
// rejects_map_wrong_bytewise_order_same_length.
// {"b":5, "a":6} — same-length keys but "b" (0x62) > "a" (0x61),
// canonical wants "a" first.
func TestRejectMapWrongBytewiseOrderSameLength(t *testing.T) {
	t.Parallel()
	bytes := []byte{0xa2, 0x61, 0x62, 0x05, 0x61, 0x61, 0x06}
	assertCanonicalViolation(t, bytes, "map keys not in length-first bytewise ascending order")
}

// TestRejectMapShorterKeyShouldComeFirst mirrors
// rejects_map_shorter_key_should_come_first.
// {"aa":5, "b":6} — "aa" first violates length-first rule
// (encoded "b" is 2 bytes, encoded "aa" is 3 bytes).
func TestRejectMapShorterKeyShouldComeFirst(t *testing.T) {
	t.Parallel()
	bytes := []byte{0xa2, 0x62, 0x61, 0x61, 0x05, 0x61, 0x62, 0x06}
	assertCanonicalViolation(t, bytes, "map keys not in length-first bytewise ascending order")
}

// --- duplicate keys ---

// TestRejectDuplicateMapKeys mirrors rejects_duplicate_map_keys.
// {"a":5, "a":6}
func TestRejectDuplicateMapKeys(t *testing.T) {
	t.Parallel()
	bytes := []byte{0xa2, 0x61, 0x61, 0x05, 0x61, 0x61, 0x06}
	assertCanonicalViolation(t, bytes, "duplicate map key")
}

// --- floats ---

// TestRejectHalfFloat mirrors rejects_half_float.
// 0xf9 0x3c 0x00 = half(1.0)
func TestRejectHalfFloat(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0xf9, 0x3c, 0x00}, "half-precision float")
}

// TestRejectSingleFloat mirrors rejects_single_float.
// 0xfa 0x3f 0x80 0x00 0x00 = single(1.0)
func TestRejectSingleFloat(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0xfa, 0x3f, 0x80, 0x00, 0x00}, "single-precision float")
}

// TestRejectDoubleFloat mirrors rejects_double_float.
// 0xfb 0x3f 0xf0 0x00 0x00 0x00 0x00 0x00 0x00 = double(1.0)
func TestRejectDoubleFloat(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t,
		[]byte{0xfb, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00},
		"double-precision float")
}

// --- non-42 tags ---

// TestRejectTag41 mirrors rejects_tag_41.
// 0xd8 0x29 0x05 — tag(41) wrapping value 5
func TestRejectTag41(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0xd8, 0x29, 0x05}, "tag 41 not allowed")
}

// --- indefinite-length ---

// TestRejectIndefiniteArray mirrors rejects_indefinite_array.
// 0x9f 0x01 0xff — indefinite-length array containing 1
func TestRejectIndefiniteArray(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0x9f, 0x01, 0xff}, "indefinite-length item")
}

// TestRejectIndefiniteMap mirrors rejects_indefinite_map.
// 0xbf 0x61 0x61 0x05 0xff — indefinite-length map {"a":5}
func TestRejectIndefiniteMap(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0xbf, 0x61, 0x61, 0x05, 0xff}, "indefinite-length item")
}

// TestRejectIndefiniteTextString mirrors rejects_indefinite_text_string.
// 0x7f 0x61 0x61 0xff — indefinite-length text string "a"
func TestRejectIndefiniteTextString(t *testing.T) {
	t.Parallel()
	assertCanonicalViolation(t, []byte{0x7f, 0x61, 0x61, 0xff}, "indefinite-length item")
}

// --- nested non-canonical ---

// TestRejectArrayContainingNonCanonicalInt mirrors
// rejects_array_containing_non_canonical_int.
// [non_shortest(5)] — outer array length OK, inner int not.
func TestRejectArrayContainingNonCanonicalInt(t *testing.T) {
	t.Parallel()
	bytes := []byte{0x81, 0x18, 0x05}
	assertCanonicalViolation(t, bytes, "non-shortest-form integer")
}

// TestRejectMapWithNonCanonicalValue mirrors
// rejects_map_with_non_canonical_value.
// {"a": non_shortest(5)} — outer map OK, value not.
func TestRejectMapWithNonCanonicalValue(t *testing.T) {
	t.Parallel()
	bytes := []byte{0xa1, 0x61, 0x61, 0x18, 0x05}
	assertCanonicalViolation(t, bytes, "non-shortest-form integer")
}

// --- positive sanity: canonical short int still validates ---

// TestAcceptCanonicalShortInt mirrors accepts_canonical_short_int.
// The Rust sibling decodes; here we just assert the validator accepts
// the bytes (decoding happens downstream).
func TestAcceptCanonicalShortInt(t *testing.T) {
	t.Parallel()
	if err := ValidateCanonical([]byte{0x05}); err != nil {
		t.Fatalf("ValidateCanonical(0x05) = %v, want nil", err)
	}
}

// --- truncation: validator must defer, not classify as non-canonical ---
//
// The Rust validator returns NotValidCbor for these via the downstream
// decoder. The Go validator has the same policy but no in-test
// downstream: it advances the cursor and returns nil, leaving the
// malformed-bytes classification to whatever calls ValidateCanonical
// next (typically fxamacker/cbor). So the Go assertion is "ValidateCanonical
// returns nil" — same contract, observed at a different layer.

// TestTruncatedByteStringPayloadIsNotClassifiedAsNonCanonical mirrors
// truncated_byte_string_payload_is_not_classified_as_non_canonical.
// 0x42 0x01 — declared 2-byte string with only 1 payload byte.
func TestTruncatedByteStringPayloadIsNotClassifiedAsNonCanonical(t *testing.T) {
	t.Parallel()
	if err := ValidateCanonical([]byte{0x42, 0x01}); err != nil {
		t.Fatalf("ValidateCanonical(truncated bstr) = %v, want nil (deferred to downstream)", err)
	}
}

// TestTruncatedTextStringPayloadIsNotClassifiedAsNonCanonical mirrors
// truncated_text_string_payload_is_not_classified_as_non_canonical.
// 0x62 0x61 — declared 2-byte text string with only 1 payload byte.
func TestTruncatedTextStringPayloadIsNotClassifiedAsNonCanonical(t *testing.T) {
	t.Parallel()
	if err := ValidateCanonical([]byte{0x62, 0x61}); err != nil {
		t.Fatalf("ValidateCanonical(truncated tstr) = %v, want nil (deferred to downstream)", err)
	}
}

// TestTruncatedStringInsideArrayIsNotClassifiedAsNonCanonical mirrors
// truncated_string_inside_array_is_not_classified_as_non_canonical.
// [int(1), bytes(declared 2, only 1 byte)]
func TestTruncatedStringInsideArrayIsNotClassifiedAsNonCanonical(t *testing.T) {
	t.Parallel()
	if err := ValidateCanonical([]byte{0x82, 0x01, 0x42, 0x01}); err != nil {
		t.Fatalf("ValidateCanonical([1, truncated bstr]) = %v, want nil (deferred to downstream)", err)
	}
}

// --- recursion depth limit: hostile deeply-nested input ---

// TestRejectDeeplyNestedArraysWithDepthLimit mirrors
// rejects_deeply_nested_arrays_with_depth_limit.
// Rust's validator caps at 256 (cbor4ii default); Go's caps at 32
// (fxamacker/cbor MaxNestedLevels default). The axis is the same:
// nesting too deep. 2000 levels overshoots either cap.
func TestRejectDeeplyNestedArraysWithDepthLimit(t *testing.T) {
	t.Parallel()
	const depth = 2000
	bytes := make([]byte, 0, depth+1)
	for i := 0; i < depth; i++ {
		bytes = append(bytes, 0x81)
	}
	bytes = append(bytes, 0x00)
	assertCanonicalViolation(t, bytes, "nesting too deep")
}
