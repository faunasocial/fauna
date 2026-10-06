//! `MailRecordEnvelope` — the per-record payload bytes that ride one
//! segment-record slot for the mail kind.
//!
//! Encoded with canonical dag-cbor. Opaque to fauna-segment-store; decoded
//! nest-side (segment read path / restore handler) when it needs the sealed
//! body or sealed index hint — **and client-side**: `fauna.email.inbox.fetch`
//! / `sent.fetch` ship these exact bytes to every client, which opens them via
//! [`super::receive::open_inbound_record`]. It also crosses nest↔nest verbatim
//! on the mail relay. So this encoding is a **compatibility surface** (at-rest
//! AND wire — `version-compatibility.md` I1/I2), not nest-internal; it just
//! never crosses to the Go bridge (the MDA is served the *inner* envelope by
//! `fetch_message_ciphertext`). The two payload fields hold the HPKE-sealed
//! canonical-dag-cbor mail-record blobs (the `fauna-mls` `MailRecordEnvelope`
//! — a distinct, same-named type) verbatim as opaque bytes.
//!
//! ## Format versions (dispatch on `format_version` — see [`MailRecord::decode`])
//!
//! * **v2** — an **inline** record: the payload fields are dag-cbor **byte
//!   strings** (`serde_bytes`), so a sealed payload costs its own length plus a
//!   small constant. The format [`MailRecordEnvelope::encode`] writes.
//! * **v3** — a **continuation HEAD**, not an inline record: the sealed body is
//!   too large for one frame-sized record, so it rests as N *part* records
//!   (raw ciphertext ranges of the one seal, in their own segment slots) and
//!   this head pins their ordered CID list + the total sealed body length. The
//!   head carries no inline `encrypted_body`; the serve path concatenates the
//!   parts before the inline-or-`body_ref` split
//!   (`message-segment-store.md` § Continuation records — owner;
//!   `smtp-server.md` § Message size limits — mail's constants). A v3 head
//!   decodes to [`MailRecord::Head`], **not** a [`MailRecordEnvelope`]: the
//!   struct has no body field to hold, and a caller that expected an inline
//!   body must resolve the parts first — so [`MailRecordEnvelope::decode`]
//!   returns an honest error on v3, and the version-complete [`MailRecord::decode`]
//!   is the entry point the writer + serve-join paths use.
//!
//! v2 and v3 are distinct map shapes (a head has no `encrypted_body` key, an
//! inline record no `part_cids` key — pinned by tests below), which is why
//! [`MailRecord::decode`] dispatches on the version field. Any other version —
//! a future one this binary predates, or the retired **v1** — fails with an
//! honest per-record decode error, never a misread.
//!
//! **v1 is gone.** It wrote the payloads as bare `Vec<u8>` (a dag-cbor array of
//! integers, ~1.91× at rest) and stopped being the write format on 2026-07-18;
//! its decoder stayed only to read records stored before that flip. No such
//! record exists anywhere (the compat-remnant sweep's baseline reset —
//! `version-compatibility.md` § Dimension 2), so the decoder was removed and a
//! v1 record is refused like any other unsupported version.

use serde::{Deserialize, Serialize};

/// The retired v1 inline shape's version tag. Nothing reads or writes v1; the
/// constant names the value [`MailRecord::decode`] refuses as retired.
const MAIL_ENVELOPE_FORMAT_VERSION_V1_RETIRED: u16 = 1;
pub const MAIL_ENVELOPE_FORMAT_VERSION_V2: u16 = 2;
/// A **continuation HEAD** record — the sealed body rests as N part records and
/// this head pins their ordered CID list + total. Decodes to
/// [`MailRecord::Head`], never a [`MailRecordEnvelope`] (module doc).
pub const MAIL_ENVELOPE_FORMAT_VERSION_V3: u16 = 3;

/// The format [`MailRecordEnvelope::encode`] writes: v2, the only inline
/// format this binary reads or writes. Moving it is a deliberate, gated
/// at-rest format migration (`version-compatibility.md` § 2.1) — a record in a
/// format some deployed reader lacks is an honest per-record decode error
/// there, mail invisible until that reader updates — never a change riding
/// another diff.
pub const MAIL_ENVELOPE_WRITE_FORMAT: u16 = MAIL_ENVELOPE_FORMAT_VERSION_V2;

/// Decoded mail segment-record envelope. `format_version` records the wire
/// shape this value was decoded from (or will be encoded to —
/// [`Self::new`] stamps [`MAIL_ENVELOPE_WRITE_FORMAT`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailRecordEnvelope {
    pub format_version: u16,
    /// Sealed under the recipient's MLS read key (the kind's inner
    /// seal — see `encryption-at-rest.md` § Mail body row).
    pub encrypted_body: Vec<u8>,
    /// Sealed under the owner's content-index key. Opaque to the nest.
    pub encrypted_index_hint: Vec<u8>,
}

/// v2 wire shape — byte-string payloads (`serde_bytes`).
#[derive(Serialize, Deserialize)]
struct WireV2 {
    format_version: u16,
    #[serde(with = "serde_bytes")]
    encrypted_body: Vec<u8>,
    #[serde(with = "serde_bytes")]
    encrypted_index_hint: Vec<u8>,
}

/// v3 wire shape — a **continuation HEAD**. `part_cids` is the flat
/// concatenation of the ordered part CID digests (32 bytes each, so its length
/// is always a multiple of 32); one `serde_bytes` byte string keeps it canonical
/// and compact. There is no inline `encrypted_body` — the body is the
/// concatenation of the part records in `part_cids` order. `total_body_len` is
/// the sum of the part lengths (= the rejoined sealed body length), which the
/// serve-join pins the concatenation against and fails closed on mismatch.
#[derive(Serialize, Deserialize)]
struct WireV3 {
    format_version: u16,
    #[serde(with = "serde_bytes")]
    part_cids: Vec<u8>,
    total_body_len: u64,
    #[serde(with = "serde_bytes")]
    encrypted_index_hint: Vec<u8>,
}

/// Minimal probe that reads only `format_version` (serde skips the payload
/// fields regardless of their major type), so [`MailRecordEnvelope::decode`]
/// can dispatch to the right wire shape.
#[derive(Deserialize)]
struct VersionProbe {
    format_version: u16,
}

impl MailRecordEnvelope {
    pub fn new(encrypted_body: Vec<u8>, encrypted_index_hint: Vec<u8>) -> Self {
        Self {
            format_version: MAIL_ENVELOPE_WRITE_FORMAT,
            encrypted_body,
            encrypted_index_hint,
        }
    }

    /// Encode in this envelope's `format_version` — v2, the only inline
    /// format. A value re-stamped to any other version fails loudly via the
    /// fallthrough error below.
    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        match self.format_version {
            MAIL_ENVELOPE_FORMAT_VERSION_V2 => fauna_cbor::encode_canonical(&WireV2 {
                format_version: self.format_version,
                encrypted_body: self.encrypted_body.clone(),
                encrypted_index_hint: self.encrypted_index_hint.clone(),
            }),
            v => Err(fauna_cbor::EncodeError::SchemaInvalid(format!(
                "mail record envelope format_version {v} has no encoder in this binary"
            ))),
        }
    }

    /// Re-stamp this envelope at `version` (used only by tests).
    /// The payload bytes are untouched — only the wire shape changes.
    pub fn with_format_version(mut self, version: u16) -> Self {
        self.format_version = version;
        self
    }

    /// Decode an **inline** mail record envelope (v2). This is the
    /// entry point for every caller that expects a
    /// self-contained body; it delegates to the version-complete
    /// [`MailRecord::decode`] and returns an honest error on a v3
    /// continuation head (which has no inline body — the caller must resolve
    /// the parts via [`MailRecord::decode`] + the nest-side serve-join). A
    /// version this binary predates likewise fails loudly and honestly — never
    /// a misread (`message-segment-store.md`: a future on-disk version FAILS
    /// loudly).
    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        match MailRecord::decode(bytes)? {
            MailRecord::Inline(env) => Ok(env),
            MailRecord::Head(_) => Err(fauna_cbor::DecodeError::SchemaMismatch(
                "mail record envelope is a v3 continuation head with no inline body — \
                 resolve its continuation parts first (decode via MailRecord::decode and \
                 concatenate the parts before opening the body)"
                    .to_string(),
            )),
        }
    }
}

/// Cheaply read just the `format_version` of a mail record envelope without
/// decoding its (possibly large) payload. The client feed uses it to spot a v3
/// continuation head among the inline records it otherwise ships verbatim,
/// without paying a full decode per record.
pub fn peek_format_version(bytes: &[u8]) -> Result<u16, fauna_cbor::DecodeError> {
    let probe: VersionProbe = fauna_cbor::decode_strict(bytes)?;
    Ok(probe.format_version)
}

/// A decoded mail segment record, version-dispatched: either a self-contained
/// **inline** record (v2) or a continuation **head** (v3) whose body rests
/// as separate part records. Callers that must handle both — the nest-side
/// serve-join and the continuation writer — decode via [`Self::decode`]; the
/// inline-only callers keep using [`MailRecordEnvelope::decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailRecord {
    /// v2 — the sealed body rides inline in this record.
    Inline(MailRecordEnvelope),
    /// v3 — a continuation head: the sealed body is the concatenation of the
    /// part records this head pins.
    Head(MailContinuationHead),
}

impl MailRecord {
    /// Version-complete decode: dispatch on the `format_version` probe. v2
    /// decodes to [`MailRecord::Inline`]; v3 to [`MailRecord::Head`]. The
    /// retired v1 and a version this binary predates both fail loudly and
    /// honestly — never a misread (`message-segment-store.md`: a future on-disk
    /// version FAILS loudly).
    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        let probe: VersionProbe = fauna_cbor::decode_strict(bytes)?;
        match probe.format_version {
            MAIL_ENVELOPE_FORMAT_VERSION_V2 => {
                let w: WireV2 = fauna_cbor::decode_strict(bytes)?;
                Ok(MailRecord::Inline(MailRecordEnvelope {
                    format_version: w.format_version,
                    encrypted_body: w.encrypted_body,
                    encrypted_index_hint: w.encrypted_index_hint,
                }))
            }
            MAIL_ENVELOPE_FORMAT_VERSION_V3 => {
                let w: WireV3 = fauna_cbor::decode_strict(bytes)?;
                Ok(MailRecord::Head(MailContinuationHead::from_wire(w)?))
            }
            v if v > MAIL_ENVELOPE_FORMAT_VERSION_V3 => {
                Err(fauna_cbor::DecodeError::SchemaMismatch(format!(
                    "mail record envelope format_version {v} is newer than this binary \
                     supports (max {MAIL_ENVELOPE_FORMAT_VERSION_V3}) — update this \
                     software to read it"
                )))
            }
            v => Err(fauna_cbor::DecodeError::SchemaMismatch(format!(
                "mail record envelope format_version {v} is not a supported format \
                 (v{MAIL_ENVELOPE_FORMAT_VERSION_V1_RETIRED} is retired; this binary \
                 reads v{MAIL_ENVELOPE_FORMAT_VERSION_V2} inline records and \
                 v{MAIL_ENVELOPE_FORMAT_VERSION_V3} continuation heads)"
            ))),
        }
    }
}

/// A decoded **continuation head** (envelope v3): the sealed body is the
/// ordered concatenation of the part records named by `part_digests`, which
/// total `total_body_len` bytes. `encrypted_index_hint` stays inline (it is
/// small). Owned by `message-segment-store.md` § Continuation records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailContinuationHead {
    pub format_version: u16,
    /// Ordered part CID **digests** (32 bytes each). Each part record is read
    /// back via `Cid::from_digest_dag_cbor(digest)`, exactly as a normal mail
    /// record id resolves to its CID.
    pub part_digests: Vec<[u8; 32]>,
    /// The rejoined sealed body length = sum of the part lengths. The serve-join
    /// pins the concatenation against this and fails closed on mismatch.
    pub total_body_len: u64,
    /// Sealed under the owner's content-index key. Opaque to the nest. Carried
    /// inline on the head exactly as on an inline record.
    pub encrypted_index_hint: Vec<u8>,
}

impl MailContinuationHead {
    /// Build a head from its ordered part digests + total + index hint. Stamps
    /// [`MAIL_ENVELOPE_FORMAT_VERSION_V3`].
    pub fn new(
        part_digests: Vec<[u8; 32]>,
        total_body_len: u64,
        encrypted_index_hint: Vec<u8>,
    ) -> Self {
        Self {
            format_version: MAIL_ENVELOPE_FORMAT_VERSION_V3,
            part_digests,
            total_body_len,
            encrypted_index_hint,
        }
    }

    /// Encode as the v3 wire shape (canonical dag-cbor). Flattens the part
    /// digests into one byte string.
    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        let mut part_cids = Vec::with_capacity(self.part_digests.len() * 32);
        for d in &self.part_digests {
            part_cids.extend_from_slice(d);
        }
        fauna_cbor::encode_canonical(&WireV3 {
            format_version: self.format_version,
            part_cids,
            total_body_len: self.total_body_len,
            encrypted_index_hint: self.encrypted_index_hint.clone(),
        })
    }

    /// Decode directly from bytes, erroring if the record is not a v3 head.
    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        match MailRecord::decode(bytes)? {
            MailRecord::Head(h) => Ok(h),
            MailRecord::Inline(_) => Err(fauna_cbor::DecodeError::SchemaMismatch(
                "expected a v3 continuation head, got an inline (v2) mail record".to_string(),
            )),
        }
    }

    fn from_wire(w: WireV3) -> Result<Self, fauna_cbor::DecodeError> {
        if !w.part_cids.len().is_multiple_of(32) {
            return Err(fauna_cbor::DecodeError::SchemaMismatch(format!(
                "mail continuation head part_cids length {} is not a multiple of 32 \
                 (each part CID digest is 32 bytes)",
                w.part_cids.len()
            )));
        }
        // The multiple-of-32 check above means the remainder is empty. `as_chunks`
        // hands back `&[u8; 32]` directly, so no copy_from_slice dance is needed.
        let part_digests = w.part_cids.as_chunks::<32>().0.to_vec();
        Ok(Self {
            format_version: w.format_version,
            part_digests,
            total_body_len: w.total_body_len,
            encrypted_index_hint: w.encrypted_index_hint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ciphertext_ish(len: usize) -> Vec<u8> {
        // High-entropy-ish bytes ≥ 24 — the worst case for a dag-cbor integer
        // array, which v2's byte strings avoid.
        (0..len)
            .map(|i| (i as u8).wrapping_mul(37).wrapping_add(200))
            .collect()
    }

    #[test]
    fn v2_round_trip_including_empty() {
        for (body, hint) in [
            (ciphertext_ish(64), ciphertext_ish(32)),
            (vec![], vec![]),
            (ciphertext_ish(1), vec![]),
        ] {
            let env = MailRecordEnvelope::new(body, hint)
                .with_format_version(MAIL_ENVELOPE_FORMAT_VERSION_V2);
            let bytes = env.encode().expect("encode v2");
            let env2 = MailRecordEnvelope::decode(&bytes).expect("decode v2");
            assert_eq!(env, env2);
        }
    }

    /// The retired v1 shape (payloads as dag-cbor integer arrays) is refused
    /// with an honest decode error, never misread — the decoder was removed by
    /// the compat-remnant sweep (module doc). Built from the exact bytes the v1
    /// writer produced.
    #[test]
    fn v1_record_is_refused() {
        // Canonical dag-cbor map, keys sorted (encrypted_body, format_version,
        // encrypted_index_hint by RFC 8949 §4.2.1 length-then-bytewise order):
        // payloads as ARRAYS of ints — the v1 shape.
        let v1_bytes: Vec<u8> = vec![
            0xa3, // map(3)
            0x6e, // text(14)
            b'e', b'n', b'c', b'r', b'y', b'p', b't', b'e', b'd', b'_', b'b', b'o', b'd', b'y',
            0x84, 0x00, 0x17, 0x18, 0x18, 0x18, 0xff, // array(4): 0, 23, 24, 255
            0x6e, // text(14)
            b'f', b'o', b'r', b'm', b'a', b't', b'_', b'v', b'e', b'r', b's', b'i', b'o', b'n',
            0x01, // 1
            0x74, // text(20)
            b'e', b'n', b'c', b'r', b'y', b'p', b't', b'e', b'd', b'_', b'i', b'n', b'd', b'e',
            b'x', b'_', b'h', b'i', b'n', b't', 0x81, 0x18, 0xfe, // array(1): 254
        ];
        assert_eq!(
            peek_format_version(&v1_bytes).expect("the version probe still reads it"),
            MAIL_ENVELOPE_FORMAT_VERSION_V1_RETIRED
        );
        let err = MailRecord::decode(&v1_bytes).expect_err("v1 must be refused");
        assert!(err.to_string().contains("is retired"), "got: {err}");
        let err = MailRecordEnvelope::decode(&v1_bytes).expect_err("v1 must be refused");
        assert!(err.to_string().contains("is retired"), "got: {err}");
        // A v1-stamped value has no encoder either.
        assert!(
            MailRecordEnvelope::new(vec![1], vec![2])
                .with_format_version(MAIL_ENVELOPE_FORMAT_VERSION_V1_RETIRED)
                .encode()
                .is_err()
        );
    }

    /// A record from a future format (v4, now that v3 is supported) fails
    /// loudly and honestly — never a misread, never a panic.
    #[test]
    fn future_format_version_fails_honestly() {
        #[derive(Serialize)]
        struct WireV4ish {
            format_version: u16,
            #[serde(with = "serde_bytes")]
            encrypted_body: Vec<u8>,
            #[serde(with = "serde_bytes")]
            encrypted_index_hint: Vec<u8>,
            something_new: u64,
        }
        let bytes = fauna_cbor::encode_canonical(&WireV4ish {
            format_version: 4,
            encrypted_body: vec![1, 2, 3],
            encrypted_index_hint: vec![],
            something_new: 7,
        })
        .unwrap();
        let err = MailRecord::decode(&bytes).expect_err("must refuse v4");
        assert!(
            err.to_string().contains("newer than this binary"),
            "got: {err}"
        );
        // The inline-only entry point also refuses v4 (surfaced from
        // MailRecord::decode).
        let err2 = MailRecordEnvelope::decode(&bytes).expect_err("must refuse v4");
        assert!(
            err2.to_string().contains("newer than this binary"),
            "got: {err2}"
        );
    }

    #[test]
    fn v3_head_round_trips_via_mail_record() {
        let head = MailContinuationHead::new(
            vec![[0xAA; 32], [0xBB; 32], [0x11; 32]],
            3 * 1_048_576,
            b"sealed-hint".to_vec(),
        );
        assert_eq!(head.format_version, MAIL_ENVELOPE_FORMAT_VERSION_V3);
        let bytes = head.encode().expect("encode v3 head");
        // Round-trips both ways: the direct head decoder and the enum decoder.
        assert_eq!(
            MailContinuationHead::decode(&bytes).expect("decode head"),
            head
        );
        match MailRecord::decode(&bytes).expect("decode record") {
            MailRecord::Head(h) => assert_eq!(h, head),
            MailRecord::Inline(_) => panic!("v3 must decode as a head, not inline"),
        }
    }

    /// A v3 head has no inline body, so the backward-compatible inline decoder
    /// must refuse it with an honest error — never silently serve an empty body
    /// (the forbidden failure mode `message-segment-store.md` § Continuation
    /// records calls out).
    #[test]
    fn inline_decode_refuses_a_v3_head() {
        let head = MailContinuationHead::new(vec![[0x01; 32]], 1024, b"h".to_vec());
        let bytes = head.encode().expect("encode");
        let err = MailRecordEnvelope::decode(&bytes).expect_err("inline decode must refuse a head");
        assert!(err.to_string().contains("continuation head"), "got: {err}");
    }

    /// A v2-shaped record stamped with an unknown older version is refused
    /// by the version dispatch, not read through the v2 shape.
    #[test]
    fn version_zero_is_refused() {
        let bytes = fauna_cbor::encode_canonical(&WireV2 {
            format_version: 0,
            encrypted_body: vec![1],
            encrypted_index_hint: vec![],
        })
        .unwrap();
        let err = MailRecord::decode(&bytes).expect_err("v0 must be refused");
        assert!(
            err.to_string().contains("not a supported format"),
            "got: {err}"
        );
    }

    /// A head with a `part_cids` blob that is not a multiple of 32 is corrupt —
    /// fail honestly rather than silently drop a partial digest.
    #[test]
    fn v3_head_with_ragged_part_cids_fails() {
        let bytes = fauna_cbor::encode_canonical(&WireV3 {
            format_version: MAIL_ENVELOPE_FORMAT_VERSION_V3,
            part_cids: vec![0u8; 33], // 33 is not a multiple of 32
            total_body_len: 1,
            encrypted_index_hint: vec![],
        })
        .unwrap();
        let err = MailContinuationHead::decode(&bytes).expect_err("ragged part_cids must fail");
        assert!(err.to_string().contains("multiple of 32"), "got: {err}");
    }

    /// v2 and v3 are mutually indecodable (a head has no `encrypted_body` key;
    /// v2 has no `part_cids` key), so a version-dispatched decode is the sole
    /// authority.
    #[test]
    fn v3_and_v2_are_mutually_indecodable() {
        let head = MailContinuationHead::new(vec![[0x02; 32]], 32, vec![]);
        let v3_bytes = head.encode().unwrap();
        assert!(fauna_cbor::decode_strict::<WireV2>(&v3_bytes).is_err());
        let v2_bytes = MailRecordEnvelope::new(vec![1, 2, 3], vec![4])
            .encode()
            .unwrap();
        assert!(fauna_cbor::decode_strict::<WireV3>(&v2_bytes).is_err());
    }

    /// Golden bytes: v3 is a compatibility surface (heads relay nest↔nest and,
    /// once the client-feed reference leg lands, are read by clients). Pin the
    /// exact canonical shape so a serde/field reorder can't silently change it.
    #[test]
    fn v3_encoding_is_frozen_golden_bytes() {
        let head = MailContinuationHead::new(vec![[0x01; 32], [0x02; 32]], 0x0102, vec![0xfe]);
        let bytes = head.encode().expect("encode");
        // Canonical dag-cbor map(4). Keys sorted by RFC 8949 §4.2.1
        // (length-then-bytewise): encrypted_index_hint(20), format_version(14),
        // part_cids(9), total_body_len(15) → sorted: format_version(14) <
        // part_cids(9)? No — length first: "part_cids"(9) < "format_version"(14)
        // < "total_body_len"(14)? both 14 → bytewise; < "encrypted_index_hint"(20).
        // So order: part_cids, format_version, total_body_len, encrypted_index_hint.
        let mut expected: Vec<u8> = vec![
            0xa4, // map(4)
            0x69, // text(9) "part_cids"
            b'p', b'a', b'r', b't', b'_', b'c', b'i', b'd', b's', 0x58,
            0x40, // byte string(64)
        ];
        expected.extend_from_slice(&[0x01; 32]);
        expected.extend_from_slice(&[0x02; 32]);
        expected.extend_from_slice(&[
            0x6e, // text(14) "format_version"
            b'f', b'o', b'r', b'm', b'a', b't', b'_', b'v', b'e', b'r', b's', b'i', b'o', b'n',
            0x03, // 3
            0x6e, // text(14) "total_body_len"
            b't', b'o', b't', b'a', b'l', b'_', b'b', b'o', b'd', b'y', b'_', b'l', b'e', b'n',
            0x19, 0x01, 0x02, // uint 0x0102
            0x74, // text(20) "encrypted_index_hint"
            b'e', b'n', b'c', b'r', b'y', b'p', b't', b'e', b'd', b'_', b'i', b'n', b'd', b'e',
            b'x', b'_', b'h', b'i', b'n', b't', 0x41, 0xfe, // byte string(1): 0xfe
        ]);
        assert_eq!(
            bytes, expected,
            "the v3 head wire shape changed — heads are a nest↔nest + client compat surface"
        );
    }

    /// The point of v2: a sealed payload costs its own length plus a small
    /// constant, not the ~1.91× of a dag-cbor integer array.
    #[test]
    fn v2_costs_payload_plus_a_small_constant() {
        let body = ciphertext_ish(100_000);
        let hint = ciphertext_ish(1_000);
        let payload = body.len() + hint.len();
        let v2 = MailRecordEnvelope::new(body, hint)
            .with_format_version(MAIL_ENVELOPE_FORMAT_VERSION_V2)
            .encode()
            .unwrap();
        assert!(
            v2.len() < payload + 128,
            "v2 must cost payload + small constant, got {} for {payload}",
            v2.len()
        );
    }

    /// The write format is v2. A tripwire: it pins the write format against an
    /// *accidental* change riding another diff. Moving it is a deliberate,
    /// gated migration step — see MAIL_ENVELOPE_WRITE_FORMAT's doc.
    #[test]
    fn write_format_is_v2() {
        assert_eq!(MAIL_ENVELOPE_WRITE_FORMAT, MAIL_ENVELOPE_FORMAT_VERSION_V2);
        let env = MailRecordEnvelope::new(vec![1], vec![2]);
        assert_eq!(env.format_version, MAIL_ENVELOPE_FORMAT_VERSION_V2);
    }
}
