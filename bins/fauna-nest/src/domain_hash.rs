//! Shared byte-framing for domain-separated content/idempotency hashes.
//! Every current caller — [`crate::db::bridge_audit::session_close_idempotency_hash`],
//! [`crate::db::bridge_audit::auth_event_idempotency_hash`],
//! [`crate::db::bridge_dav_common::derive_dav_content_id`],
//! [`crate::bridge_routing_handlers::rejected_scan_message_id`],
//! [`crate::db::CacheDb::bridge_content_id`], and the two
//! tamper-evidence chain hashes ([`crate::db::admin::audit_on_conn`]'s
//! `entry_hash`, [`crate::db::CacheDb::compute_pending_action_hash`]'s
//! `chain_hash`) — hand-rolled
//! the same "length-prefixed domain tag, then a field sequence" idiom over
//! two different hash primitives (`sha2::Sha256`, `blake3::Hasher`), which
//! don't share a common trait; [`write_fields`] takes a `FnMut(&[u8])` sink
//! instead so either primitive's `update` method plugs in directly.

/// One field of a domain-hash input. Two variants write bytes with no
/// length framing — safe only under the constraint their names carry rather
/// than a shared prose comment: `Fixed32` takes a compile-time-fixed
/// `&[u8; 32]`, so it can appear anywhere in a field sequence — its width
/// never varies, so an adjacent field's bytes can never be mistaken for
/// part of it — while `Trailing` takes an unframed variable-length slice
/// and is safe **only as the last field**, which [`write_fields`] asserts
/// positionally (nothing follows it, so there is no boundary left to
/// blur). `LenPrefixed` writes a little-endian `u32` byte count ahead of
/// variable-length bytes so two differently-split concatenations can't
/// alias; `I64` writes 8 little-endian bytes with no framing (fixed-width,
/// same reasoning as `Fixed32`); `OptStr` writes a one-byte presence flag
/// then a length-prefixed string, or a single `0` byte for `None`.
pub(crate) enum HashField<'a> {
    Fixed32(&'a [u8; 32]),
    Trailing(&'a [u8]),
    LenPrefixed(&'a [u8]),
    I64(i64),
    OptStr(Option<&'a str>),
}

/// Writes `domain_tag` (length-prefixed) then every field in order to
/// `sink` — the caller's hasher `update` method, called once per framed
/// chunk. Deterministic per input, so a caller feeding the identical domain
/// tag + fields to the identical hash primitive always gets the same
/// digest — the idempotency/content-identity property every current caller
/// relies on.
///
/// Panics (debug builds only) if a [`HashField::Trailing`] appears anywhere
/// but the last position — the one part of the framing contract a type
/// alone can't enforce, since `Trailing`'s safety is positional, not
/// structural.
pub(crate) fn write_fields(domain_tag: &[u8], fields: &[HashField], mut sink: impl FnMut(&[u8])) {
    debug_assert!(
        fields
            .iter()
            .enumerate()
            .all(|(i, f)| !matches!(f, HashField::Trailing(_)) || i == fields.len() - 1),
        "HashField::Trailing must be the last field — an unframed variable-length \
         field followed by anything else lets bytes shift across the boundary \
         without changing the hash"
    );
    sink(&(domain_tag.len() as u32).to_le_bytes());
    sink(domain_tag);
    for field in fields {
        match field {
            HashField::Fixed32(bytes) => sink(bytes.as_slice()),
            HashField::Trailing(bytes) => sink(bytes),
            HashField::LenPrefixed(bytes) => {
                sink(&(bytes.len() as u32).to_le_bytes());
                sink(bytes);
            }
            HashField::I64(n) => sink(&n.to_le_bytes()),
            HashField::OptStr(Some(s)) => {
                sink(&[1u8]);
                sink(&(s.len() as u32).to_le_bytes());
                sink(s.as_bytes());
            }
            HashField::OptStr(None) => sink(&[0u8]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(fields: &[HashField]) -> Vec<u8> {
        let mut out = Vec::new();
        write_fields(b"test.tag", fields, |b| out.extend_from_slice(b));
        out
    }

    #[test]
    #[should_panic(expected = "HashField::Trailing must be the last field")]
    fn trailing_not_last_panics() {
        digest(&[HashField::Trailing(b"oops"), HashField::I64(1)]);
    }

    #[test]
    fn trailing_last_is_fine() {
        // Must not panic.
        digest(&[HashField::I64(1), HashField::Trailing(b"fine")]);
    }

    #[test]
    fn lenprefixed_before_fixed32_prevents_boundary_aliasing() {
        // The exact shape `bridge_content_id` fixes: a variable-length field
        // immediately followed by a fixed-32 field, then a trailing field.
        // Two decompositions below share the identical 38-byte raw
        // concatenation — an unframed hasher (the pre-fix bug) would
        // collide them despite `fixed_a != fixed_b`.
        let variable_a: &[u8] = b"XY";
        let fixed_a: [u8; 32] = [0u8; 32];
        let trailing_a: &[u8] = b"tail";

        let variable_b: &[u8] = b"X";
        let mut fixed_b = [0u8; 32];
        fixed_b[0] = b'Y';
        let trailing_b: &[u8] = b"\0tail";

        let raw_concat = |v: &[u8], f: &[u8; 32], t: &[u8]| -> Vec<u8> {
            let mut out = v.to_vec();
            out.extend_from_slice(f);
            out.extend_from_slice(t);
            out
        };
        assert_eq!(
            raw_concat(variable_a, &fixed_a, trailing_a),
            raw_concat(variable_b, &fixed_b, trailing_b),
            "both decompositions must share one raw byte string for this test \
             to demonstrate anything"
        );

        let framed = |v: &[u8], f: &[u8; 32], t: &[u8]| {
            digest(&[
                HashField::LenPrefixed(v),
                HashField::Fixed32(f),
                HashField::Trailing(t),
            ])
        };
        assert_ne!(
            framed(variable_a, &fixed_a, trailing_a),
            framed(variable_b, &fixed_b, trailing_b),
            "framing must break the alias the raw concatenation shares"
        );
    }
}
