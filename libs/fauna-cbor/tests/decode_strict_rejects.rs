//! Negative-corpus: every input here is non-canonical dag-cbor that the
//! pre-parse validator must reject as `DecodeError::NotCanonical`.
//!
//! Each helper expects the bytes to round-trip to whatever Rust type would
//! naturally hold the value if the input *were* canonical. We pick `u64`
//! for ints/floats and `serde_ipld_dagcbor::DecodeError`-friendly catch-all
//! `ipld_core::ipld::Ipld` for the structural cases (arrays/maps/tags) so the
//! validator's rejection beats the deserializer.

use fauna_cbor::{DecodeError, decode_strict};
use ipld_core::ipld::Ipld;

fn assert_not_canonical_int(bytes: &[u8]) {
    let r = decode_strict::<u64>(bytes);
    assert!(
        matches!(r, Err(DecodeError::NotCanonical { .. })),
        "expected NotCanonical for {:02x?}, got {:?}",
        bytes,
        r
    );
}

fn assert_not_canonical_neg(bytes: &[u8]) {
    let r = decode_strict::<i64>(bytes);
    assert!(
        matches!(r, Err(DecodeError::NotCanonical { .. })),
        "expected NotCanonical for {:02x?}, got {:?}",
        bytes,
        r
    );
}

fn assert_not_canonical_ipld(bytes: &[u8]) {
    let r = decode_strict::<Ipld>(bytes);
    assert!(
        matches!(r, Err(DecodeError::NotCanonical { .. })),
        "expected NotCanonical for {:02x?}, got {:?}",
        bytes,
        r
    );
}

// --- non-shortest-form integers (positive) ---

#[test]
fn rejects_one_byte_ext_for_value_5() {
    // 0x18 0x05 — 1-byte extension carrying value 5; canonical is 0x05.
    assert_not_canonical_int(&[0x18, 0x05]);
}

#[test]
fn rejects_two_byte_ext_for_value_5() {
    // 0x19 0x00 0x05 — 2-byte extension carrying 5.
    assert_not_canonical_int(&[0x19, 0x00, 0x05]);
}

#[test]
fn rejects_four_byte_ext_for_value_5() {
    // 0x1a 0x00 0x00 0x00 0x05 — 4-byte extension carrying 5.
    assert_not_canonical_int(&[0x1a, 0x00, 0x00, 0x00, 0x05]);
}

#[test]
fn rejects_eight_byte_ext_for_value_5() {
    // 0x1b 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x05 — 8-byte extension.
    assert_not_canonical_int(&[0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05]);
}

// --- non-shortest-form integers (negative) ---

#[test]
fn rejects_negative_one_byte_ext_for_minus_6() {
    // 0x38 0x05 — negint with 1-byte extension carrying 5 (means -6).
    // Canonical is 0x25 (negint inline 5 = -6).
    assert_not_canonical_neg(&[0x38, 0x05]);
}

// --- map ordering: length-first ---

#[test]
fn rejects_map_wrong_bytewise_order_same_length() {
    // {"b":5, "a":6} — same-length keys but "b" (0x62) > "a" (0x61),
    // canonical wants "a" first.
    let bytes = [0xa2, 0x61, 0x62, 0x05, 0x61, 0x61, 0x06];
    assert_not_canonical_ipld(&bytes);
}

#[test]
fn rejects_map_shorter_key_should_come_first() {
    // {"aa":5, "b":6} — "aa" first violates length-first rule
    // (encoded "b" is 2 bytes, encoded "aa" is 3 bytes).
    let bytes = [0xa2, 0x62, 0x61, 0x61, 0x05, 0x61, 0x62, 0x06];
    assert_not_canonical_ipld(&bytes);
}

// --- duplicate keys ---

#[test]
fn rejects_duplicate_map_keys() {
    // {"a":5, "a":6}
    let bytes = [0xa2, 0x61, 0x61, 0x05, 0x61, 0x61, 0x06];
    assert_not_canonical_ipld(&bytes);
}

// --- floats ---

#[test]
fn rejects_half_float() {
    // 0xf9 0x3c 0x00 = half(1.0)
    assert_not_canonical_ipld(&[0xf9, 0x3c, 0x00]);
}

#[test]
fn rejects_single_float() {
    // 0xfa 0x3f 0x80 0x00 0x00 = single(1.0)
    assert_not_canonical_ipld(&[0xfa, 0x3f, 0x80, 0x00, 0x00]);
}

#[test]
fn rejects_double_float() {
    // 0xfb 0x3f 0xf0 0x00 0x00 0x00 0x00 0x00 0x00 = double(1.0)
    assert_not_canonical_ipld(&[0xfb, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
}

// --- non-42 tags ---

#[test]
fn rejects_tag_41() {
    // 0xd8 0x29 0x05 — tag(41) wrapping value 5
    assert_not_canonical_ipld(&[0xd8, 0x29, 0x05]);
}

// --- indefinite-length ---

#[test]
fn rejects_indefinite_array() {
    // 0x9f 0x01 0xff — indefinite-length array containing 1
    assert_not_canonical_ipld(&[0x9f, 0x01, 0xff]);
}

#[test]
fn rejects_indefinite_map() {
    // 0xbf 0x61 0x61 0x05 0xff — indefinite-length map {"a":5}
    assert_not_canonical_ipld(&[0xbf, 0x61, 0x61, 0x05, 0xff]);
}

#[test]
fn rejects_indefinite_text_string() {
    // 0x7f 0x61 0x61 0xff — indefinite-length text string "a"
    assert_not_canonical_ipld(&[0x7f, 0x61, 0x61, 0xff]);
}

// --- nested non-canonical ---

#[test]
fn rejects_array_containing_non_canonical_int() {
    // [non_shortest(5)] — outer array length OK, inner int not.
    let bytes = [0x81, 0x18, 0x05];
    assert_not_canonical_ipld(&bytes);
}

#[test]
fn rejects_map_with_non_canonical_value() {
    // {"a": non_shortest(5)} — outer map OK, value not.
    let bytes = [0xa1, 0x61, 0x61, 0x18, 0x05];
    assert_not_canonical_ipld(&bytes);
}

// --- positive sanity: canonical short int still decodes ---

#[test]
fn accepts_canonical_short_int() {
    let v: u64 = decode_strict(&[0x05]).expect("canonical 5 must decode");
    assert_eq!(v, 5);
}

// --- truncation: validator must defer, not classify as non-canonical ---

#[test]
fn truncated_byte_string_payload_is_not_classified_as_non_canonical() {
    // 0x42 0x01 — declared 2-byte string with only 1 payload byte.
    // Validator must NOT classify this as NotCanonical; downstream
    // decoder catches it as NotValidCbor (truncated).
    let r = decode_strict::<Ipld>(&[0x42, 0x01]);
    assert!(
        matches!(r, Err(DecodeError::NotValidCbor)),
        "expected NotValidCbor (truncation), got {r:?}"
    );
}

#[test]
fn truncated_text_string_payload_is_not_classified_as_non_canonical() {
    // 0x62 0x61 — declared 2-byte text string with only 1 payload byte.
    let r = decode_strict::<Ipld>(&[0x62, 0x61]);
    assert!(
        matches!(r, Err(DecodeError::NotValidCbor)),
        "expected NotValidCbor (truncation), got {r:?}"
    );
}

#[test]
fn truncated_string_inside_array_is_not_classified_as_non_canonical() {
    // [int(1), bytes(declared 2, only 1 byte)]
    let r = decode_strict::<Ipld>(&[0x82, 0x01, 0x42, 0x01]);
    assert!(
        matches!(r, Err(DecodeError::NotValidCbor)),
        "expected NotValidCbor (truncation), got {r:?}"
    );
}

// --- recursion depth limit: hostile deeply-nested input ---

#[test]
fn rejects_deeply_nested_arrays_with_depth_limit() {
    // 2000 levels of [..[..]..] wrapping. Each level adds one 0x81 byte
    // (array-of-1) and the leaf is 0x00 (uint 0).
    const DEPTH: usize = 2000;
    let mut bytes = vec![0x81u8; DEPTH];
    bytes.push(0x00);

    let r = decode_strict::<Ipld>(&bytes);
    assert!(
        matches!(r, Err(DecodeError::NotCanonical { .. })),
        "expected NotCanonical (depth limit), got {r:?}"
    );
}

// --- non-shortest-form integers (negative), wider extensions ---
// The existing coverage stops at the 1-byte negint extension; assert the
// 2/4/8-byte extension arms of `read_length_value` reject through major 1.

#[test]
fn rejects_negative_two_byte_ext_for_minus_6() {
    // 0x39 0x00 0x05 — negint 2-byte ext carrying 5 (means -6); value 5 < 256
    // so the 2-byte encoding is non-shortest. Canonical is 0x25.
    assert_not_canonical_neg(&[0x39, 0x00, 0x05]);
}

#[test]
fn rejects_negative_four_byte_ext_for_minus_6() {
    // 0x3a 0x00 0x00 0x00 0x05 — negint 4-byte ext carrying 5; 5 < 65536.
    assert_not_canonical_neg(&[0x3a, 0x00, 0x00, 0x00, 0x05]);
}

#[test]
fn rejects_negative_eight_byte_ext_for_minus_6() {
    // 0x3b 00 00 00 00 00 00 00 05 — negint 8-byte ext carrying 5; 5 < 2^32.
    assert_not_canonical_neg(&[0x3b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05]);
}

// --- non-shortest-form LENGTH encoding across the other majors ---
// `read_length_value`'s shortest-form check is shared, but the existing tests
// only exercise it through majors 0/1 (integer values). These reach it through
// the byte-string/text-string/array/map *length* fields and the tag ID, which
// are distinct call sites in `walk_item`.

#[test]
fn rejects_non_shortest_byte_string_length() {
    // 0x58 0x05 <5 bytes> — byte string whose length (5) is carried in a
    // 1-byte extension; 5 < 24 so canonical is 0x45 <5 bytes>.
    assert_not_canonical_ipld(&[0x58, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05]);
}

#[test]
fn rejects_non_shortest_text_string_length() {
    // 0x78 0x05 "hello" — text string length 5 via 1-byte ext; canonical 0x65.
    assert_not_canonical_ipld(&[0x78, 0x05, 0x68, 0x65, 0x6c, 0x6c, 0x6f]);
}

#[test]
fn rejects_non_shortest_array_length() {
    // 0x98 0x01 0x00 — array of one element (uint 0) with length via 1-byte
    // ext; canonical is 0x81 0x00.
    assert_not_canonical_ipld(&[0x98, 0x01, 0x00]);
}

#[test]
fn rejects_non_shortest_map_length() {
    // 0xb8 0x01 0x61 0x61 0x00 — one-pair map {"a":0} with length via 1-byte
    // ext; canonical is 0xa1 ....
    assert_not_canonical_ipld(&[0xb8, 0x01, 0x61, 0x61, 0x00]);
}

#[test]
fn rejects_non_shortest_tag_id() {
    // 0xd9 0x00 0x2a 0x05 — tag(42) whose ID is carried in a 2-byte ext; 42 <
    // 256 so canonical is the 1-byte-ext form 0xd8 0x2a. The non-shortest tag
    // ID is rejected before the "tag != 42" check ever runs.
    assert_not_canonical_ipld(&[0xd9, 0x00, 0x2a, 0x05]);
}

// --- reserved additional-info values (28..=30) ---
// Rejected for every major type; assert the three reserved codes on major 0.

#[test]
fn rejects_reserved_info_28() {
    assert_not_canonical_int(&[0x1c]);
}

#[test]
fn rejects_reserved_info_29() {
    assert_not_canonical_int(&[0x1d]);
}

#[test]
fn rejects_reserved_info_30() {
    assert_not_canonical_int(&[0x1e]);
}

// --- indefinite-length byte string (major 2 completes the 2..=5 set) ---

#[test]
fn rejects_indefinite_byte_string() {
    // 0x5f 0x41 0x01 0xff — indefinite-length byte string with one 1-byte chunk.
    assert_not_canonical_ipld(&[0x5f, 0x41, 0x01, 0xff]);
}

// --- simple values (major 7, info 24) ---

#[test]
fn rejects_one_byte_simple_value_below_32() {
    // 0xf8 0x00 — 1-byte simple value carrying 0, which duplicates the inline
    // encoding (info 0..=23); only simple values >= 32 are legal in this form.
    assert_not_canonical_ipld(&[0xf8, 0x00]);
}

// --- bare break stop code (major 7, info 31) outside any indefinite item ---

#[test]
fn rejects_bare_break_stop_code() {
    assert_not_canonical_ipld(&[0xff]);
}

// --- trailing data after a complete top-level item ---

#[test]
fn rejects_trailing_data_after_top_level_int() {
    // 0x01 0x02 — two back-to-back top-level ints; the first is complete, the
    // second is trailing garbage at the top level.
    assert_not_canonical_int(&[0x01, 0x02]);
}

#[test]
fn rejects_trailing_data_after_top_level_container() {
    // 0x81 0x00 0x00 — array-of-one [0], then a trailing top-level uint 0.
    assert_not_canonical_ipld(&[0x81, 0x00, 0x00]);
}
