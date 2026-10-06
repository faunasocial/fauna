// Pre-parse canonical-form validator for IPLD-dag-cbor.
//
// Walks the raw byte stream before fxamacker/cbor's decoder ever sees
// it. Rejects every non-canonical input axis the spec calls out:
//
//   - Non-shortest-form integer length-encoding (positive ints, negative
//     ints, byte/text string lengths, array lengths, map lengths, tag IDs).
//   - Map keys not in length-first-then-bytewise ascending order.
//   - Duplicate map keys.
//   - Floats (major 7 with additional info 25/26/27).
//   - Tags other than 42 (major 6).
//   - Indefinite-length items (any major with additional info 31).
//   - Reserved additional-info values (28-30).
//   - Trailing bytes after a complete top-level item.
//
// Truncation policy: if the walker can't tell because input is truncated
// mid-item, advance the cursor to len(bytes) and return nil; let the
// downstream typed decoder reject it as malformed CBOR. This keeps the
// canonical/non-canonical vs malformed-bytes split clean.
//
// This is the Go-side mirror of libs/fauna-cbor/src/canonical.rs. The
// two implementations must reject the same bytes with the same axes;
// keep them in lockstep when either side changes.
//
// No fxamacker/cbor calls inside this file — pure byte-level walk.

package dagcbor

import (
	"errors"
	"fmt"
)

// maxNestingDepth caps container recursion in the validator.
//
// fxamacker/cbor's DecOptions.MaxNestedLevels defaults to 32, and
// codec.go does not override it. We mirror that cap so the validator
// rejects the same hostile [[[[...]]]] shapes the downstream decoder
// would reject — just earlier and with a typed error instead of a
// nested-too-deep decode failure. The Rust validator caps at 256 to
// match cbor4ii's default; do not raise this above the Go decoder's
// actual MaxNestedLevels or the two layers will disagree.
const maxNestingDepth = 32

// ValidateCanonical reports whether b is canonical IPLD-dag-cbor. It
// does not decode values. A nil return means b is canonical (or
// truncated mid-item — the downstream decoder will surface the
// truncation as a decode error).
//
// Error messages are prefixed "dagcbor: not canonical: <reason>" and
// the <reason> substring matches the Rust validator's reason strings
// where reasonable, so callers can substring-match on axes (e.g.
// strings.Contains(err.Error(), "duplicate map key")).
func ValidateCanonical(b []byte) error {
	pos := 0
	if err := walkItem(b, &pos, 0); err != nil {
		return err
	}
	if pos < len(b) {
		return notCanonical("trailing data")
	}
	return nil
}

func notCanonical(reason string) error {
	return errors.New("dagcbor: not canonical: " + reason)
}

// walkItem walks a single CBOR item starting at *pos and advances *pos
// past the item.
//
// On truncation (not enough bytes to cover a length-encoding or
// payload), walkItem advances *pos to len(bytes) and returns nil,
// leaving the malformed-bytes classification to the downstream decoder.
// Advancing the cursor avoids a spurious "trailing data" classification
// at the top level.
//
// depth counts container nesting; recursive calls into arrays, maps,
// and tag inner items pass depth+1. Exceeding maxNestingDepth is
// rejected as "nesting too deep".
func walkItem(bytes []byte, pos *int, depth int) error {
	if depth > maxNestingDepth {
		return notCanonical("nesting too deep")
	}
	start := *pos
	if start >= len(bytes) {
		// Truncation: defer to downstream decoder.
		return nil
	}
	initial := bytes[start]
	major := initial >> 5
	info := initial & 0x1f
	*pos = start + 1

	// Reject reserved info values (28-30) for every major type.
	if info >= 28 && info <= 30 {
		return notCanonical("reserved additional-info")
	}

	// Reject indefinite-length (info 31) for major types 2..=5.
	// Major 7 with info 31 is the "break" stop code; only legal inside
	// an indefinite item, which we reject anyway. Treat it as
	// non-canonical for symmetry.
	if info == 31 {
		if major == 7 {
			return notCanonical("break stop code outside indefinite item")
		}
		return notCanonical("indefinite-length item")
	}

	switch major {
	case 0:
		// Positive integer.
		_, _, err := readLengthValue(bytes, pos, info)
		return err
	case 1:
		// Negative integer.
		_, _, err := readLengthValue(bytes, pos, info)
		return err
	case 2:
		// Byte string: length, then length bytes.
		v, ok, err := readLengthValue(bytes, pos, info)
		if err != nil {
			return err
		}
		if !ok {
			return nil // truncated — defer
		}
		lenUsize := int(v)
		if len(bytes)-*pos < lenUsize {
			// Truncated payload — defer. Advance cursor to end so the
			// top-level trailing-data check doesn't misfire.
			*pos = len(bytes)
			return nil
		}
		*pos += lenUsize
		return nil
	case 3:
		// Text string: same shape as byte string.
		v, ok, err := readLengthValue(bytes, pos, info)
		if err != nil {
			return err
		}
		if !ok {
			return nil
		}
		lenUsize := int(v)
		if len(bytes)-*pos < lenUsize {
			*pos = len(bytes)
			return nil
		}
		*pos += lenUsize
		return nil
	case 4:
		// Array: length, then that many items.
		v, ok, err := readLengthValue(bytes, pos, info)
		if err != nil {
			return err
		}
		if !ok {
			return nil
		}
		for i := uint64(0); i < v; i++ {
			if *pos >= len(bytes) {
				return nil
			}
			if err := walkItem(bytes, pos, depth+1); err != nil {
				return err
			}
		}
		return nil
	case 5:
		// Map: length, then 2*length items. Track key encodings to
		// verify length-first then bytewise ascending order, with strict
		// inequality (rejects duplicates).
		v, ok, err := readLengthValue(bytes, pos, info)
		if err != nil {
			return err
		}
		if !ok {
			return nil
		}
		var prevKey []byte
		hasPrev := false
		for i := uint64(0); i < v; i++ {
			if *pos >= len(bytes) {
				return nil
			}
			keyStart := *pos
			if err := walkItem(bytes, pos, depth+1); err != nil {
				return err
			}
			keyEnd := *pos
			// Guard against the truncation case where walkItem returned
			// nil without advancing past a partial item.
			if keyEnd > len(bytes) {
				return nil
			}
			keySlice := bytes[keyStart:keyEnd]
			if hasPrev {
				if !keyStrictlyGreater(prevKey, keySlice) {
					if equalBytes(prevKey, keySlice) {
						return notCanonical("duplicate map key")
					}
					return notCanonical("map keys not in length-first bytewise ascending order")
				}
			}
			prevKey = keySlice
			hasPrev = true
			if *pos >= len(bytes) {
				return nil
			}
			if err := walkItem(bytes, pos, depth+1); err != nil {
				return err
			}
		}
		return nil
	case 6:
		// Tag: read tag ID (shortest-form-checked), require == 42, recurse.
		tagID, ok, err := readLengthValue(bytes, pos, info)
		if err != nil {
			return err
		}
		if !ok {
			return nil
		}
		if tagID != 42 {
			return notCanonical(fmt.Sprintf("tag %d not allowed", tagID))
		}
		if *pos >= len(bytes) {
			return nil
		}
		return walkItem(bytes, pos, depth+1)
	case 7:
		// Simple value / float.
		// info 0..=19: simple values inline.
		// info 20=false, 21=true, 22=null, 23=undefined.
		// info 24: next byte is simple value (must be >= 32, else it's
		//   a duplicate of the inline encoding and non-canonical).
		// info 25/26/27: half/single/double float — REJECT.
		switch {
		case info <= 23:
			return nil
		case info == 24:
			if *pos >= len(bytes) {
				return nil
			}
			v := bytes[*pos]
			*pos++
			if v < 32 {
				return notCanonical("1-byte simple value duplicates inline encoding")
			}
			return nil
		case info == 25:
			return notCanonical("half-precision float")
		case info == 26:
			return notCanonical("single-precision float")
		case info == 27:
			return notCanonical("double-precision float")
		default:
			// info 28..=31 already rejected above.
			return notCanonical("reserved additional-info")
		}
	default:
		// Major type is 3 bits, 0..=7. Unreachable.
		return notCanonical("unreachable major type")
	}
}

// readLengthValue reads the length/value field for an item with
// additional info = info, enforcing shortest-form encoding. Returns:
//   - (v, true, nil)  — v is the length/value, shortest-form OK.
//   - (0, false, nil) — truncated; defer to downstream decoder.
//   - (0, false, err) — non-shortest-form (canonical violation).
func readLengthValue(bytes []byte, pos *int, info byte) (uint64, bool, error) {
	switch {
	case info <= 23:
		return uint64(info), true, nil
	case info == 24:
		if *pos+1 > len(bytes) {
			return 0, false, nil
		}
		v := uint64(bytes[*pos])
		*pos++
		// Shortest-form: 1-byte ext only valid for v >= 24.
		if v < 24 {
			return 0, false, notCanonical("non-shortest-form integer (1-byte ext for value < 24)")
		}
		return v, true, nil
	case info == 25:
		if *pos+2 > len(bytes) {
			return 0, false, nil
		}
		v := uint64(bytes[*pos])<<8 | uint64(bytes[*pos+1])
		*pos += 2
		// Shortest-form: 2-byte ext only valid for v >= 256.
		if v < 256 {
			return 0, false, notCanonical("non-shortest-form integer (2-byte ext for value < 256)")
		}
		return v, true, nil
	case info == 26:
		if *pos+4 > len(bytes) {
			return 0, false, nil
		}
		v := uint64(bytes[*pos])<<24 |
			uint64(bytes[*pos+1])<<16 |
			uint64(bytes[*pos+2])<<8 |
			uint64(bytes[*pos+3])
		*pos += 4
		// Shortest-form: 4-byte ext only valid for v >= 65536.
		if v < 0x10000 {
			return 0, false, notCanonical("non-shortest-form integer (4-byte ext for value < 65536)")
		}
		return v, true, nil
	case info == 27:
		if *pos+8 > len(bytes) {
			return 0, false, nil
		}
		v := uint64(bytes[*pos])<<56 |
			uint64(bytes[*pos+1])<<48 |
			uint64(bytes[*pos+2])<<40 |
			uint64(bytes[*pos+3])<<32 |
			uint64(bytes[*pos+4])<<24 |
			uint64(bytes[*pos+5])<<16 |
			uint64(bytes[*pos+6])<<8 |
			uint64(bytes[*pos+7])
		*pos += 8
		// Shortest-form: 8-byte ext only valid for v >= 0x1_0000_0000.
		if v < 0x100000000 {
			return 0, false, notCanonical("non-shortest-form integer (8-byte ext for value < 2^32)")
		}
		return v, true, nil
	default:
		// info 28..=31 handled by caller.
		return 0, false, notCanonical("reserved additional-info")
	}
}

// keyStrictlyGreater is the "key b > key a" comparator: length-first,
// then bytewise. Returns true iff b is strictly greater than a (so
// equal keys → false, surfacing duplicates).
func keyStrictlyGreater(a, b []byte) bool {
	if len(a) < len(b) {
		return true
	}
	if len(a) > len(b) {
		return false
	}
	// Equal lengths — bytewise compare.
	for i := range a {
		if a[i] < b[i] {
			return true
		}
		if a[i] > b[i] {
			return false
		}
	}
	// All bytes equal.
	return false
}

// equalBytes reports whether a and b are the same length and bytes.
// Kept local to avoid pulling bytes.Equal just for this file.
func equalBytes(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
