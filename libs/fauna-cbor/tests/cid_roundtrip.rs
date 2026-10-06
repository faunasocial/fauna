use fauna_cbor::Cid;

#[test]
fn cid_from_bytes_is_36_bytes() {
    let cid = Cid::of_dag_cbor(b"hello world");
    assert_eq!(cid.as_bytes().len(), 36);
}

#[test]
fn cid_matches_its_source_bytes() {
    let payload = b"the canonical bytes";
    let cid = Cid::of_dag_cbor(payload);
    assert!(cid.matches(payload));
}

#[test]
fn cid_does_not_match_other_bytes() {
    let cid = Cid::of_dag_cbor(b"the canonical bytes");
    assert!(!cid.matches(b"different bytes"));
}

#[test]
fn cid_base32_roundtrip() {
    let cid = Cid::of_dag_cbor(b"round trip me");
    let s = cid.to_base32();
    let parsed = Cid::from_base32(&s).expect("valid base32");
    assert_eq!(cid, parsed);
}

#[test]
fn cid_base32_starts_with_b_prefix() {
    // Multibase prefix 'b' = base32 lowercase.
    let cid = Cid::of_dag_cbor(b"anything");
    assert!(cid.to_base32().starts_with('b'));
}

#[test]
fn cid_carries_dag_cbor_codec_0x71() {
    let cid = Cid::of_dag_cbor(b"x");
    assert_eq!(cid.codec(), 0x71);
}

#[test]
fn cid_carries_blake3_multihash_0x1e() {
    let cid = Cid::of_dag_cbor(b"x");
    assert_eq!(cid.multihash_code(), 0x1e);
}

// ── Codec-parametric Cid (Layer 3 Task 3.7) ──────────────────────────

#[test]
fn cid_of_dag_cbor_codec_byte_is_0x71() {
    assert_eq!(Cid::of_dag_cbor(b"x").codec(), 0x71);
    assert_eq!(Cid::DAG_CBOR, 0x71);
}

#[test]
fn cid_of_raw_codec_byte_is_0x55() {
    assert_eq!(Cid::of_raw(b"x").codec(), 0x55);
    assert_eq!(Cid::RAW, 0x55);
}

#[test]
fn cid_digest_returns_blake3_of_payload() {
    let payload = b"hello";
    let cid = Cid::of_dag_cbor(payload);
    let expected = *blake3::hash(payload).as_bytes();
    assert_eq!(cid.digest(), expected);
    // Codec choice does not affect the digest — same payload, same hash.
    assert_eq!(Cid::of_raw(payload).digest(), expected);
}

#[test]
fn cid_digest_matches_on_wire_tail() {
    // The new `digest()` helper centralizes what callers used to write
    // as a manual 4..36 slice of `as_bytes()`. Pin the equivalence so
    // the helper can never drift away from the on-wire layout.
    let cid = Cid::of_dag_cbor(b"digest tail");
    let on_wire = *cid.as_bytes();
    let mut tail = [0u8; 32];
    tail.copy_from_slice(&on_wire[4..36]);
    assert_eq!(cid.digest(), tail);
}

#[test]
fn cid_from_bytes_accepts_dag_cbor() {
    let cid = Cid::of_dag_cbor(b"dag-cbor round-trip");
    let bytes = *cid.as_bytes();
    let parsed = Cid::from_bytes(bytes).expect("dag-cbor accepted");
    assert_eq!(parsed, cid);
    assert_eq!(parsed.codec(), Cid::DAG_CBOR);
}

#[test]
fn cid_from_bytes_accepts_raw() {
    let cid = Cid::of_raw(b"raw round-trip");
    let bytes = *cid.as_bytes();
    let parsed = Cid::from_bytes(bytes).expect("raw accepted");
    assert_eq!(parsed, cid);
    assert_eq!(parsed.codec(), Cid::RAW);
}

#[test]
fn cid_from_bytes_rejects_unknown_codec() {
    // 0xFF is neither DAG_CBOR (0x71) nor RAW (0x55).
    let mut bytes = [0u8; 36];
    bytes[0] = 0x01;
    bytes[1] = 0xFF;
    bytes[2] = 0x1e;
    bytes[3] = 32;
    let err = Cid::from_bytes(bytes).unwrap_err();
    assert!(format!("{err:?}").contains("NotValidCbor"));
}

#[test]
fn cid_from_bytes_rejects_wrong_multihash() {
    // Multihash code 0xAA is not blake3-256 (0x1e); reject.
    let mut bytes = [0u8; 36];
    bytes[0] = 0x01;
    bytes[1] = Cid::DAG_CBOR;
    bytes[2] = 0xAA;
    bytes[3] = 32;
    assert!(Cid::from_bytes(bytes).is_err());
}

#[test]
fn cid_from_base32_accepts_dag_cbor() {
    let cid = Cid::of_dag_cbor(b"b32 dag-cbor");
    let s = cid.to_base32();
    let parsed = Cid::from_base32(&s).expect("dag-cbor base32 accepted");
    assert_eq!(parsed, cid);
}

#[test]
fn cid_from_base32_accepts_raw() {
    let cid = Cid::of_raw(b"b32 raw");
    let s = cid.to_base32();
    let parsed = Cid::from_base32(&s).expect("raw base32 accepted");
    assert_eq!(parsed, cid);
    assert_eq!(parsed.codec(), Cid::RAW);
}

#[test]
fn cid_from_digest_dag_cbor_round_trips() {
    let cid = Cid::of_dag_cbor(b"original payload");
    let rebuilt = Cid::from_digest_dag_cbor(cid.digest());
    assert_eq!(rebuilt, cid);
    assert_eq!(rebuilt.codec(), Cid::DAG_CBOR);
}

#[test]
fn cid_from_digest_raw_round_trips() {
    let cid = Cid::of_raw(b"original payload");
    let rebuilt = Cid::from_digest_raw(cid.digest());
    assert_eq!(rebuilt, cid);
    assert_eq!(rebuilt.codec(), Cid::RAW);
}

#[test]
fn cid_of_dag_cbor_and_raw_differ_by_one_byte() {
    // Same payload, different codec → distinct CIDs differing only in
    // byte position 1 (the codec byte). Confirms the codec byte rides
    // inside the CID, not externally.
    let payload = b"same payload";
    let dag = Cid::of_dag_cbor(payload);
    let raw = Cid::of_raw(payload);
    assert_ne!(dag, raw);
    // Bytes 0, 2, 3 and 4..36 match; only byte 1 differs.
    assert_eq!(dag.as_bytes()[0], raw.as_bytes()[0]);
    assert_ne!(dag.as_bytes()[1], raw.as_bytes()[1]);
    assert_eq!(dag.as_bytes()[2], raw.as_bytes()[2]);
    assert_eq!(dag.as_bytes()[3], raw.as_bytes()[3]);
    assert_eq!(dag.digest(), raw.digest());
}

// ── Cid encodes as an IPLD link (tag 42) ─────────────────────────────
// `docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor, the
// raw-byte shape decision's closing ruling on `Cid`: tag 42 over a byte
// string of `0x00` followed by the 36 CID bytes, no byte-string fallback.

/// The exact dag-cbor bytes of `cid` as a link: tag 42 (`d8 2a`), a 37-byte
/// byte string (`58 25`), the identity-multibase prefix `00`, the CID.
fn link_spelling(cid: &Cid) -> Vec<u8> {
    let mut out = vec![0xd8, 0x2a, 0x58, 0x25, 0x00];
    out.extend_from_slice(cid.as_bytes());
    out
}

#[test]
fn cid_encodes_as_tag42_link() {
    for cid in [Cid::of_dag_cbor(b"link me"), Cid::of_raw(b"raw link me")] {
        let bytes = fauna_cbor::encode_canonical(&cid).unwrap();
        assert_eq!(bytes, link_spelling(&cid), "a Cid is a tag-42 link");
        let back: Cid = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, cid);
    }
}

#[test]
fn cid_decodes_as_generic_value_link() {
    // A generic IPLD reader sees the field as a link it can follow.
    let cid = Cid::of_dag_cbor(b"followable");
    let bytes = fauna_cbor::encode_canonical(&cid).unwrap();
    let v: fauna_cbor::Value = fauna_cbor::decode_strict(&bytes).unwrap();
    match v {
        fauna_cbor::Value::Link(link) => assert_eq!(link.to_bytes(), cid.as_bytes().to_vec()),
        other => panic!("expected a link, got {other:?}"),
    }
}

#[test]
fn cid_refuses_the_plain_byte_string_spelling() {
    // The pre-ruling spelling — a bare 36-byte byte string — is refused:
    // one logical value has one canonical byte form.
    let cid = Cid::of_dag_cbor(b"no fallback");
    let plain = fauna_cbor::encode_canonical(&serde_bytes::Bytes::new(cid.as_bytes())).unwrap();
    assert!(fauna_cbor::decode_strict::<Cid>(&plain).is_err());
}

#[test]
fn cid_refuses_a_link_to_a_foreign_cid() {
    // A well-formed link whose CID is not Fauna's shape (codec dag-pb
    // 0x70) is refused, as `Cid::from_bytes` refuses it.
    use multihash_codetable::{Code, MultihashDigest};
    let foreign = cid::Cid::new_v1(0x70, Code::Blake3_256.digest(b"foreign"));
    let bytes = fauna_cbor::encode_canonical(&fauna_cbor::Value::Link(foreign)).unwrap();
    assert!(fauna_cbor::decode_strict::<Cid>(&bytes).is_err());
}

#[test]
fn cid_in_a_struct_field_is_a_link() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Rec {
        parent: Cid,
        prev: Option<Cid>,
        deps: Vec<Cid>,
    }
    let a = Cid::of_dag_cbor(b"a");
    let r = Rec {
        parent: a,
        prev: Some(Cid::of_raw(b"b")),
        deps: vec![a],
    };
    let bytes = fauna_cbor::encode_canonical(&r).unwrap();
    let back: Rec = fauna_cbor::decode_strict(&bytes).unwrap();
    assert_eq!(back, r);
    let needle = link_spelling(&a);
    assert!(bytes.windows(needle.len()).any(|w| w == needle.as_slice()));
}
