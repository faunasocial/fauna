// Package dagcbor is the bridge's canonical DAG-CBOR codec.
//
// Wire-byte canonicality matters: the WS-RPC envelopes the Rust nest
// produces (libs/fauna-protocol/src/{codec,envelope}.rs) must round-trip
// through the Go bridge byte-for-byte, because hashes and Ed25519
// signatures cover the raw encoded bytes. Any drift in encoding
// (different key sort, an indefinite-length marker, a tagged value, an
// integer with non-shortest form) is a wire-protocol incompatibility.
//
// "DAG-CBOR" here means the IPLD strict subset
// (https://ipld.io/specs/codecs/dag-cbor/spec/#strictness):
//
//   - Map keys are sorted **length-first then bytewise** on the encoded
//     key bytes (fxamacker/cbor's [cbor.SortLengthFirst]). This is *not*
//     the same as RFC 8949 §4.2.1 "Core Deterministic Encoding"'s pure
//     bytewise-lexicographic sort ([cbor.SortBytewiseLexical]); the IPLD
//     spec deliberately diverges from RFC 8949 here. The unit test
//     `TestMapKeySortedRoundTrip` pins this on the wire bytes so a
//     mistake fails loudly. Rust-side parity is partial today: ciborium
//     (used by libs/fauna-protocol/src/codec.rs) does not currently
//     enforce length-first key sort on Marshal — serde-emitted order
//     wins. This bites only for string-keyed maps with mixed-length
//     keys; the WS-RPC envelopes use integer keys 0..8 which all encode
//     as one byte, so envelope round-trip is unaffected. Fixing the
//     Rust side is tracked internally.
//   - No floats (Marshal walks the input via reflect and rejects any
//     Float32/Float64 field). The bridge's WS-RPC envelopes carry
//     ints/bytes/strings/maps only; the wrapped-blob crypto design
//     (tracked internally) forbids floats on the wire because they make canonical hashing
//     across implementations brittle.
//   - No NaN / Inf (the encoder rejects them too, but we set
//     [cbor.NaNConvertReject] and [cbor.InfConvertReject] defensively in
//     case a float slips past the reflect walker on a future change).
//   - No indefinite-length encodings on either side
//     ([cbor.IndefLengthForbidden]).
//   - No CBOR tags ([cbor.TagsForbidden]). IPLD DAG-CBOR uses tag 42 for
//     CIDs, but the bridge's wire format does not embed CIDs (envelopes
//     carry payload bytes, correlation IDs, kinds, …); accepting tags
//     would only widen the attack surface. Every Rust `fauna_cbor::Cid`
//     (and `ContentHash`) field rides as a tag-42 link, so a Go mirror
//     of a struct carrying one must first admit tag 42 here (see
//     docs/goal/architecture/serialization.md, the raw-byte shape
//     decision). The ipld/codec-fixtures CI
//     gate's `cid-*` and tag-bearing fixtures are programmatically
//     classified as expected-skip, not exercised.
//   - No duplicate map keys on decode
//     ([cbor.DupMapKeyEnforcedAPF]).
//   - Nil Go slices/maps/[]byte encode as the canonical EMPTY container
//     ([] / {} / empty byte string), never CBOR null
//     ([cbor.NilContainerAsEmpty]). Canonical dag-cbor has no "null
//     list": nest strict-decode rejects null for a `Vec<T>` field, so a
//     nil Go slice marshaled as null fails the RPC (the UID SEARCH ALL /
//     plain-EXPUNGE bug). Fields whose *absence* is semantically
//     meaningful (a Rust `Option<Vec>`/`Option<ByteBuf>`, `None` ≠ empty)
//     are Go pointers, whose nil still encodes as null — so this never
//     turns a real `None` into an empty container.
//
// Public API: [Marshal] and [Unmarshal]. The generics on Unmarshal save
// the caller a type assertion and let the float walker fire on the
// decode path too (a malformed peer could send floats we'd otherwise
// surface as `any`).
package dagcbor

import (
	"errors"
	"fmt"
	"reflect"

	"github.com/fxamacker/cbor/v2"
)

// encMode is the singleton encoder configured for canonical DAG-CBOR.
// Configured once at init; encMode.Marshal is goroutine-safe per
// fxamacker/cbor's documented contract.
var encMode cbor.EncMode

// decMode is the singleton decoder configured for canonical DAG-CBOR.
// decMode.Unmarshal is goroutine-safe per fxamacker/cbor's documented
// contract.
var decMode cbor.DecMode

func init() {
	em, err := cbor.EncOptions{
		// IPLD DAG-CBOR spec: length-first then bytewise sort on encoded
		// key bytes. NOT SortBytewiseLexical (RFC 8949 §4.2.1).
		Sort: cbor.SortLengthFirst,
		// Encode a nil Go slice/map/[]byte as the canonical EMPTY container
		// ([] = 0x80 / {} = 0xa0 / empty byte string = 0x40), never CBOR
		// null (0xf6). Canonical dag-cbor has no "null list": nest
		// strict-decodes a `Vec<T>` / `Vec<u8>` field and rejects null, so
		// a nil Go slice marshaled as null fails the RPC (`ok=false`) —
		// this is the UID SEARCH ALL / plain-EXPUNGE bug class. The flip is
		// safe because every bridge-marshaled field whose *absence* is
		// semantically meaningful (a Rust `Option<Vec>`/`Option<ByteBuf>`,
		// where `None` ≠ empty) is a Go POINTER, whose nil still encodes as
		// null — so this never turns a real `None` into an empty container.
		// See internal/wsrpc/methods.go (queryEventsRequest.AfterEventID)
		// for the marshaled-field audit.
		NilContainers: cbor.NilContainerAsEmpty,
		// Defensive: floats are rejected by the reflect walker on Marshal
		// before they reach the encoder, but if a future code path
		// bypasses the walker we still want NaN/Inf to fail rather than
		// silently round-trip.
		ShortestFloat: cbor.ShortestFloatNone,
		NaNConvert:    cbor.NaNConvertReject,
		InfConvert:    cbor.InfConvertReject,
		// Forbid indefinite-length encodings on the wire (canonical form
		// requires definite length).
		IndefLength: cbor.IndefLengthForbidden,
		// No tags on the wire. Times encoded as Unix-epoch numerics so
		// they're representable without tag 0/1.
		Time:    cbor.TimeUnixDynamic,
		TimeTag: cbor.EncTagNone,
	}.EncMode()
	if err != nil {
		// Misconfiguration at init time is a programmer error, not a
		// runtime condition; panic rather than carry an err-mode codec.
		panic(fmt.Sprintf("dagcbor: bad EncOptions: %v", err))
	}
	encMode = em

	dm, err := cbor.DecOptions{
		// Reject duplicate map keys (allowed-per-first = "applied per
		// field"; same as Rust ciborium's behavior).
		DupMapKey: cbor.DupMapKeyEnforcedAPF,
		// Mirror the encode-side bans.
		IndefLength: cbor.IndefLengthForbidden,
		TagsMd:      cbor.TagsForbidden,
	}.DecMode()
	if err != nil {
		panic(fmt.Sprintf("dagcbor: bad DecOptions: %v", err))
	}
	decMode = dm
}

// Marshal encodes v as canonical DAG-CBOR.
//
// Returns an error if v contains a float (anywhere in the value's
// reflected shape — map keys, map values, struct fields, slice/array
// elements, pointer targets). DAG-CBOR encoding only happens after the
// float check passes, so a partially-encoded buffer is never returned.
func Marshal(v any) ([]byte, error) {
	if err := rejectFloats(reflect.ValueOf(v), nil); err != nil {
		return nil, err
	}
	return encMode.Marshal(v)
}

// Unmarshal decodes canonical DAG-CBOR bytes into a value of type T.
//
// The decoder rejects indefinite-length encodings, CBOR tags, and
// duplicate map keys per the package's strict-determinism contract.
// Note: the decoder will not surface floats as an error on its own
// (fxamacker/cbor decodes them happily); if T is `any` and the wire
// contains a float, Unmarshal returns the float in the result and the
// caller is responsible for treating that as a protocol violation.
// Bridge code never decodes into `any`; it decodes into concrete struct
// types where a float field would be a type-mismatch error at the
// wire-shape level.
func Unmarshal[T any](b []byte) (T, error) {
	var zero T
	if err := ValidateCanonical(b); err != nil {
		return zero, err
	}
	var out T
	if err := decMode.Unmarshal(b, &out); err != nil {
		return zero, err
	}
	return out, nil
}

// rejectFloats walks v recursively and returns an error if any reachable
// element is a Float32/Float64. path is the breadcrumb (struct-field /
// map-key / slice-index) used to build the error message.
//
// Recurses through pointer/interface unwrapping, struct fields, slice +
// array elements, and map keys + values. Other kinds (string, int, bool,
// bytes-as-[]byte, etc.) are leaves and accepted.
func rejectFloats(v reflect.Value, path []string) error {
	// Walk through pointer / interface indirection.
	for v.IsValid() && (v.Kind() == reflect.Pointer || v.Kind() == reflect.Interface) {
		if v.IsNil() {
			return nil
		}
		v = v.Elem()
	}
	if !v.IsValid() {
		return nil
	}
	switch v.Kind() {
	case reflect.Float32, reflect.Float64:
		return floatError(path)
	case reflect.Struct:
		t := v.Type()
		for i := 0; i < v.NumField(); i++ {
			f := t.Field(i)
			if !f.IsExported() {
				continue
			}
			if err := rejectFloats(v.Field(i), append(path, f.Name)); err != nil {
				return err
			}
		}
	case reflect.Slice, reflect.Array:
		// Byte slices/arrays are leaves (CBOR major type 2); skip the
		// per-element walk for them.
		if v.Type().Elem().Kind() == reflect.Uint8 {
			return nil
		}
		for i := 0; i < v.Len(); i++ {
			if err := rejectFloats(v.Index(i), append(path, fmt.Sprintf("[%d]", i))); err != nil {
				return err
			}
		}
	case reflect.Map:
		iter := v.MapRange()
		for iter.Next() {
			if err := rejectFloats(iter.Key(), append(path, "<key>")); err != nil {
				return err
			}
			// Stringify the key for the value's path breadcrumb if it's
			// representable; otherwise fall back to "<value>".
			var keyPath string
			k := iter.Key()
			if k.Kind() == reflect.Interface {
				k = k.Elem()
			}
			if k.IsValid() && k.Kind() == reflect.String {
				keyPath = k.String()
			} else {
				keyPath = "<value>"
			}
			if err := rejectFloats(iter.Value(), append(path, keyPath)); err != nil {
				return err
			}
		}
	}
	return nil
}

func floatError(path []string) error {
	if len(path) == 0 {
		return errors.New("dagcbor: float values are forbidden by DAG-CBOR strictness; reject at the type system, not the wire")
	}
	return fmt.Errorf("dagcbor: float at %v is forbidden by DAG-CBOR strictness", path)
}
