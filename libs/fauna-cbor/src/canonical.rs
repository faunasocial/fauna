//! Pre-parse canonical-form validator for IPLD-dag-cbor.
//!
//! Walks the raw byte stream before `serde_ipld_dagcbor::from_slice` ever
//! sees it. Rejects every non-canonical input axis the spec calls out:
//!
//! - Non-shortest-form integer length-encoding (positive ints, negative
//!   ints, byte/text string lengths, array lengths, map lengths, tag IDs).
//! - Map keys not in length-first-then-bytewise ascending order.
//! - Duplicate map keys.
//! - Floats (major 7 with additional info 25/26/27).
//! - Tags other than 42 (major 6).
//! - Indefinite-length items (any major with additional info 31).
//! - Reserved additional-info values (28–30).
//! - Trailing bytes after a complete top-level item.
//!
//! Truncation policy: if the walker can't tell because input is truncated
//! mid-item, return `Ok(())` and let `serde_ipld_dagcbor::from_slice`
//! reject it as `NotValidCbor`. This keeps the canonical/non-canonical vs
//! malformed-bytes split clean.
//!
//! No serde dependency — pure byte-level walk.

use crate::error::DecodeError;

/// Validate that `bytes` is canonical IPLD-dag-cbor. Does not decode values.
pub(crate) fn validate_canonical(bytes: &[u8]) -> Result<(), DecodeError> {
    let mut pos = 0usize;
    walk_item(bytes, &mut pos, 0)?;
    if pos < bytes.len() {
        return Err(not_canonical("trailing data"));
    }
    Ok(())
}

/// Maximum nesting depth accepted by the validator. A hostile input shaped
/// `[[[[…]]]]` could otherwise consume one stack frame per level and crash
/// `decode_strict` (which sits on the security path) with a stack overflow.
/// 256 matches `cbor4ii::SliceReader`'s default `step_in` limit (the depth
/// guard the downstream decoder uses), so we reject the same hostile shapes
/// the downstream rejects — just earlier and with a typed error instead of
/// a stack-overflow abort. Well above any plausible legitimate wire payload.
const MAX_NESTING_DEPTH: u16 = 256;

fn not_canonical(reason: &str) -> DecodeError {
    DecodeError::NotCanonical {
        reason: reason.to_string(),
    }
}

/// Walk a single CBOR item starting at `*pos`. Advances `*pos` past the item.
///
/// On truncation (not enough bytes to cover a length-encoding or payload)
/// advances `*pos` to `bytes.len()` and returns `Ok(())`, leaving the
/// malformed-bytes classification to the downstream decoder. (Advancing the
/// cursor avoids a spurious "trailing data" classification at the top level.)
///
/// `depth` counts container nesting; recursive calls into arrays, maps, and
/// tag inner items pass `depth + 1`. Exceeding `MAX_NESTING_DEPTH` is rejected
/// as `NotCanonical("nesting too deep")`.
fn walk_item(bytes: &[u8], pos: &mut usize, depth: u16) -> Result<(), DecodeError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(not_canonical("nesting too deep"));
    }
    let start = *pos;
    if start >= bytes.len() {
        // Truncation: defer to NotValidCbor downstream.
        return Ok(());
    }
    let initial = bytes[start];
    let major = initial >> 5;
    let info = initial & 0x1f;
    *pos = start + 1;

    // Reject reserved info values (28–30) for every major type.
    if (28..=30).contains(&info) {
        return Err(not_canonical("reserved additional-info"));
    }

    // Reject indefinite-length (info 31) for major types 2..=5.
    // Major 7 with info 31 is the "break" stop code; only legal inside an
    // indefinite item, which we reject anyway. So treat it as non-canonical.
    if info == 31 {
        if major == 7 {
            // A bare break with no enclosing indefinite item is structural
            // garbage; classify as non-canonical for symmetry with the
            // indefinite-length rejection.
            return Err(not_canonical("break stop code outside indefinite item"));
        }
        return Err(not_canonical("indefinite-length item"));
    }

    match major {
        0 => {
            // Positive integer.
            read_length_value(bytes, pos, info)?;
            Ok(())
        }
        1 => {
            // Negative integer.
            read_length_value(bytes, pos, info)?;
            Ok(())
        }
        2 => {
            // Byte string: length, then `length` bytes.
            let len = match read_length_value(bytes, pos, info)? {
                Some(v) => v,
                None => return Ok(()), // truncated — defer
            };
            let len_usize = len as usize;
            if bytes.len().saturating_sub(*pos) < len_usize {
                // Truncated payload — defer. Advance cursor to end so the
                // top-level trailing-data check doesn't misfire.
                *pos = bytes.len();
                return Ok(());
            }
            *pos += len_usize;
            Ok(())
        }
        3 => {
            // Text string: same shape as byte string.
            let len = match read_length_value(bytes, pos, info)? {
                Some(v) => v,
                None => return Ok(()),
            };
            let len_usize = len as usize;
            if bytes.len().saturating_sub(*pos) < len_usize {
                *pos = bytes.len();
                return Ok(());
            }
            *pos += len_usize;
            Ok(())
        }
        4 => {
            // Array: length, then that many items.
            let len = match read_length_value(bytes, pos, info)? {
                Some(v) => v,
                None => return Ok(()),
            };
            for _ in 0..len {
                if *pos >= bytes.len() {
                    return Ok(());
                }
                walk_item(bytes, pos, depth + 1)?;
            }
            Ok(())
        }
        5 => {
            // Map: length, then 2*length items. Track key encodings to
            // verify length-first then bytewise ascending order, with
            // strict inequality (rejects duplicates).
            let len = match read_length_value(bytes, pos, info)? {
                Some(v) => v,
                None => return Ok(()),
            };
            let mut prev_key: Option<&[u8]> = None;
            for _ in 0..len {
                if *pos >= bytes.len() {
                    return Ok(());
                }
                let key_start = *pos;
                walk_item(bytes, pos, depth + 1)?;
                let key_end = *pos;
                // Guard against the truncation case where walk_item returned
                // Ok without advancing past a partial item.
                if key_end > bytes.len() {
                    return Ok(());
                }
                let key_slice = &bytes[key_start..key_end];
                if let Some(prev) = prev_key
                    && !key_strictly_greater(prev, key_slice)
                {
                    if prev == key_slice {
                        return Err(not_canonical("duplicate map key"));
                    }
                    return Err(not_canonical(
                        "map keys not in length-first bytewise ascending order",
                    ));
                }
                prev_key = Some(key_slice);
                if *pos >= bytes.len() {
                    return Ok(());
                }
                walk_item(bytes, pos, depth + 1)?;
            }
            Ok(())
        }
        6 => {
            // Tag: read tag ID (shortest-form-checked), require == 42, recurse.
            let tag_id = match read_length_value(bytes, pos, info)? {
                Some(v) => v,
                None => return Ok(()),
            };
            if tag_id != 42 {
                return Err(not_canonical("tag other than 42"));
            }
            if *pos >= bytes.len() {
                return Ok(());
            }
            walk_item(bytes, pos, depth + 1)?;
            Ok(())
        }
        7 => {
            // Simple value / float.
            // info 0..=19: simple values inline.
            // info 20=false, 21=true, 22=null, 23=undefined.
            // info 24: next byte is simple value (must be >= 32, else
            //   it's a duplicate of the inline encoding and non-canonical).
            // info 25/26/27: half/single/double float — REJECT.
            match info {
                0..=23 => Ok(()),
                24 => {
                    if *pos >= bytes.len() {
                        return Ok(());
                    }
                    let v = bytes[*pos];
                    *pos += 1;
                    if v < 32 {
                        return Err(not_canonical(
                            "1-byte simple value duplicates inline encoding",
                        ));
                    }
                    Ok(())
                }
                25 => Err(not_canonical("half-precision float")),
                26 => Err(not_canonical("single-precision float")),
                27 => Err(not_canonical("double-precision float")),
                _ => unreachable!("info 28..=31 already rejected above"),
            }
        }
        _ => unreachable!("major type is 3 bits, 0..=7"),
    }
}

/// Read the length/value field for an item with `additional info = info`,
/// enforcing shortest-form encoding. Returns:
/// - `Ok(Some(v))` — `v` is the length/value, shortest-form OK.
/// - `Ok(None)`     — truncated; defer to downstream decoder.
/// - `Err(_)`       — non-shortest-form (canonical violation).
fn read_length_value(bytes: &[u8], pos: &mut usize, info: u8) -> Result<Option<u64>, DecodeError> {
    match info {
        0..=23 => Ok(Some(info as u64)),
        24 => {
            if *pos + 1 > bytes.len() {
                return Ok(None);
            }
            let v = bytes[*pos] as u64;
            *pos += 1;
            // Shortest-form: 1-byte ext only valid for v >= 24.
            if v < 24 {
                return Err(not_canonical(
                    "non-shortest-form integer (1-byte ext for value < 24)",
                ));
            }
            Ok(Some(v))
        }
        25 => {
            if *pos + 2 > bytes.len() {
                return Ok(None);
            }
            let v = u64::from(u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]]));
            *pos += 2;
            // Shortest-form: 2-byte ext only valid for v >= 256.
            if v < 256 {
                return Err(not_canonical(
                    "non-shortest-form integer (2-byte ext for value < 256)",
                ));
            }
            Ok(Some(v))
        }
        26 => {
            if *pos + 4 > bytes.len() {
                return Ok(None);
            }
            let v = u64::from(u32::from_be_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]));
            *pos += 4;
            // Shortest-form: 4-byte ext only valid for v >= 65536.
            if v < 0x10000 {
                return Err(not_canonical(
                    "non-shortest-form integer (4-byte ext for value < 65536)",
                ));
            }
            Ok(Some(v))
        }
        27 => {
            if *pos + 8 > bytes.len() {
                return Ok(None);
            }
            let v = u64::from_be_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
                bytes[*pos + 4],
                bytes[*pos + 5],
                bytes[*pos + 6],
                bytes[*pos + 7],
            ]);
            *pos += 8;
            // Shortest-form: 8-byte ext only valid for v >= 0x1_0000_0000.
            if v < 0x1_0000_0000 {
                return Err(not_canonical(
                    "non-shortest-form integer (8-byte ext for value < 2^32)",
                ));
            }
            Ok(Some(v))
        }
        _ => unreachable!("info 28..=31 handled by caller"),
    }
}

/// Strict "key b > key a" comparator: length-first, then bytewise.
/// Returns true iff b is strictly greater than a (so equal keys → false,
/// surfacing duplicates).
fn key_strictly_greater(a: &[u8], b: &[u8]) -> bool {
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => a < b,
    }
}
