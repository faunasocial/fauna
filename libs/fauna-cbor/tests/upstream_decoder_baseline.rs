//! What the **pinned upstream decoder does on its own** — the baseline the
//! pre-parse validator is measured against.
//!
//! `decode_strict_rejects.rs` pins OUR validator. This file pins the other
//! half: raw `serde_ipld_dagcbor::from_slice` with `validate_canonical`
//! deliberately *not* in front of it, so `serialization.md` § *How
//! decode-strictness is actually enforced (Rust)* can state per axis what
//! upstream catches and what only the validator catches, backed by a test
//! instead of by a reading of upstream's changelog.
//!
//! **How this observes the dependency.** By *calling* it, never by reading
//! its source — the dependency-source rule (`release-integrity.md`
//! § *Reviewing untrusted source without being subverted*) forbids the
//! latter outside the pinned containment venue. Everything asserted here is
//! black-box observable behaviour of our own call.
//!
//! **The finding these assertions encode (2026-08-24, at `serde_ipld_dagcbor`
//! 0.6.4 / `cbor4ii` 0.2.14).** Upstream's strictness is *target-type
//! dependent*: the same non-canonical bytes are rejected when decoded into
//! one Rust type and silently accepted when decoded into another, because
//! part of the checking lives in the target's `Deserialize` impl rather than
//! in the decoder. The pre-parse validator is target-independent, which is
//! the architectural reason it stays the boundary regardless of which codec
//! version is pinned.
//!
//! **A version bump is expected to flip several of these** (upstream's 0.7.0
//! strictness release, 2026-08-03, claims to close the remaining axes). That
//! is the point: when these assertions fail, the goal doc's per-axis table
//! has gone stale and must be re-measured in the same change.

use ipld_core::ipld::Ipld;
use serde::Deserialize;
use std::collections::BTreeMap;

/// `{"a": 5, "a": 6}` — a duplicate map key, canonical in every other respect.
const DUPLICATE_KEYS: [u8; 7] = [0xa2, 0x61, 0x61, 0x05, 0x61, 0x61, 0x06];

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OneField {
    a: u64,
}

fn raw<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, serde_ipld_dagcbor::DecodeError<core::convert::Infallible>> {
    serde_ipld_dagcbor::from_slice(bytes)
}

// --- axes upstream catches on its own (the validator is redundant here) ---

#[test]
fn upstream_rejects_floats() {
    // 0xf9 0x3c 0x00 = half(1.0).
    assert!(raw::<Ipld>(&[0xf9, 0x3c, 0x00]).is_err());
}

#[test]
fn upstream_rejects_indefinite_length() {
    // 0x9f … 0xff = indefinite-length array.
    assert!(raw::<Ipld>(&[0x9f, 0x01, 0xff]).is_err());
}

#[test]
fn upstream_rejects_reserved_additional_info() {
    // 0x1c = major 0, additional info 28 (reserved, 28–30).
    assert!(raw::<Ipld>(&[0x1c]).is_err());
}

#[test]
fn upstream_rejects_trailing_data() {
    // Two complete top-level items where one is expected.
    assert!(raw::<Ipld>(&[0x05, 0x05]).is_err());
}

#[test]
fn upstream_rejects_tags_other_than_42() {
    // 0xd8 0x29 0x05 = tag(41) wrapping 5. Rejected on both an untyped and a
    // typed target — so the goal doc must NOT list non-42 tags as silently
    // accepted "opaque tagged values"; at this pin they are an error.
    assert!(raw::<Ipld>(&[0xd8, 0x29, 0x05]).is_err());
    assert!(raw::<u64>(&[0xd8, 0x29, 0x05]).is_err());
}

// --- axes ONLY the pre-parse validator catches ---

#[test]
fn upstream_accepts_non_shortest_form_integer() {
    // 0x18 0x05 — 1-byte extension carrying 5; canonical is the single byte 0x05.
    assert_eq!(raw::<Ipld>(&[0x18, 0x05]).unwrap(), Ipld::Integer(5));
}

#[test]
fn upstream_accepts_non_shortest_form_map_length() {
    // 0xb8 0x01 — map whose length 1 rides a 1-byte extension; canonical is 0xa1.
    // Length encodings are a distinct axis from value encodings: a decoder can
    // normalise integers and still let a container header through.
    let decoded = raw::<Ipld>(&[0xb8, 0x01, 0x61, 0x61, 0x05]).unwrap();
    assert_eq!(
        decoded,
        Ipld::Map(BTreeMap::from([("a".to_string(), Ipld::Integer(5))]))
    );
}

#[test]
fn upstream_accepts_map_keys_out_of_canonical_order() {
    // {"aa":5, "b":6} — "aa" before "b" violates length-first ordering.
    // Note the decoded value sorts on the way into `Ipld`'s BTreeMap, so the
    // violation is invisible downstream: nothing but a pre-parse byte walk can
    // see it. This is the axis with no possible after-the-fact detection.
    let decoded = raw::<Ipld>(&[0xa2, 0x62, 0x61, 0x61, 0x05, 0x61, 0x62, 0x06]).unwrap();
    assert_eq!(
        decoded,
        Ipld::Map(BTreeMap::from([
            ("aa".to_string(), Ipld::Integer(5)),
            ("b".to_string(), Ipld::Integer(6)),
        ]))
    );
}

// --- the target-dependent axis: duplicate map keys ---

#[test]
fn upstream_duplicate_key_handling_is_target_type_dependent() {
    // The same bytes, three decode targets, three different outcomes. This is
    // the finding that makes a flat "upstream silently accepts duplicate map
    // keys" line wrong in either direction.

    // (a) Untyped node — rejected.
    assert!(
        raw::<Ipld>(&DUPLICATE_KEYS).is_err(),
        "an untyped Ipld target rejects duplicate keys at this pin"
    );

    // (b) Derive-generated struct — rejected, by serde's own duplicate-field
    //     check rather than by anything dag-cbor-specific.
    assert!(
        raw::<OneField>(&DUPLICATE_KEYS).is_err(),
        "a derive-generated struct target rejects duplicate keys via serde"
    );

    // (c) Plain map target — SILENTLY ACCEPTED, last value wins. No error, and
    //     the collision leaves no trace in the decoded value.
    let as_map: BTreeMap<String, u64> =
        raw(&DUPLICATE_KEYS).expect("a plain map target accepts duplicate keys at this pin");
    assert_eq!(
        as_map,
        BTreeMap::from([("a".to_string(), 6)]),
        "last key wins, silently"
    );
}

#[test]
fn the_validator_closes_the_target_dependent_gap() {
    // The reason (c) above is not a live hole: `decode_strict` runs the
    // pre-parse validator first, so the plain-map target rejects the same
    // bytes the raw decoder waved through. The tree's one plain-map decode
    // target is `fauna_mls::engine`'s snapshot read, which sits on the
    // security path — it is covered here, not upstream.
    let via_strict = fauna_cbor::decode_strict::<BTreeMap<String, u64>>(&DUPLICATE_KEYS);
    assert!(
        matches!(
            via_strict,
            Err(fauna_cbor::DecodeError::NotCanonical { .. })
        ),
        "expected NotCanonical through decode_strict, got {via_strict:?}"
    );
}
