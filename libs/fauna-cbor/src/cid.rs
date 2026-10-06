use blake3;
use multibase::Base;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const BLAKE3_256_HASH_CODE: u8 = 0x1e;
const BLAKE3_256_HASH_LEN: u8 = 32;
const CID_VERSION_1: u8 = 0x01;

/// IPLD CID, fixed shape: v1 + (dag-cbor 0x71 OR raw 0x55) + blake3-256
/// (0x1e) + 32 hash bytes.
///
/// Layout (36 bytes total):
///   byte 0:      version (0x01)
///   byte 1:      codec (0x71 = dag-cbor, 0x55 = raw)
///   byte 2:      multihash code (0x1e = blake3-256)
///   byte 3:      multihash digest length (0x20 = 32)
///   bytes 4-35:  32-byte BLAKE3 digest
///
/// # Codec choice
///
/// Use [`Cid::DAG_CBOR`] (and the `*_dag_cbor` constructors) for any CID
/// whose payload IS canonical dag-cbor bytes — Fauna's native serialized
/// shape. Use [`Cid::RAW`] (and the `*_raw` constructors) for content
/// addressing of opaque byte streams that are NOT dag-cbor: media blobs,
/// mail RFC-5322 wire bytes, files imported as-is. The codec byte rides
/// inside the CID at position 1; otherwise the two flavors are
/// indistinguishable on the wire.
///
/// # Serde
///
/// Serializes as an IPLD link: under canonical dag-cbor, tag 42 over a
/// byte string of `0x00` followed by the 36 bytes `Cid::as_bytes()`
/// returns, with no byte-string fallback on decode
/// (`docs/goal/architecture/serialization.md`, the raw-byte shape
/// decision). Frozen for good from the 2026-10 baseline on.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cid([u8; 36]);

impl Cid {
    /// Multicodec byte for IPLD-dag-cbor (canonical CBOR with the
    /// rust-ipld constraint set). The default for Fauna's native shapes.
    pub const DAG_CBOR: u8 = 0x71;

    /// Multicodec byte for raw bytes — opaque payloads that are NOT
    /// dag-cbor (media blobs, mail wire bytes, imported files).
    pub const RAW: u8 = 0x55;

    /// Compute the CID of `bytes` under the dag-cbor codec (0x71) using
    /// BLAKE3-256. Use for payloads that ARE canonical dag-cbor.
    pub fn of_dag_cbor(bytes: &[u8]) -> Self {
        Self::of_with_codec(bytes, Self::DAG_CBOR)
    }

    /// Compute the CID of `bytes` under the raw codec (0x55) using
    /// BLAKE3-256. Use for opaque byte streams whose canonical form is
    /// the byte sequence itself (media blobs, mail wire bytes, …).
    pub fn of_raw(bytes: &[u8]) -> Self {
        Self::of_with_codec(bytes, Self::RAW)
    }

    fn of_with_codec(bytes: &[u8], codec: u8) -> Self {
        let digest = blake3::hash(bytes);
        let mut buf = [0u8; 36];
        buf[0] = CID_VERSION_1;
        buf[1] = codec;
        buf[2] = BLAKE3_256_HASH_CODE;
        buf[3] = BLAKE3_256_HASH_LEN;
        buf[4..].copy_from_slice(digest.as_bytes());
        Cid(buf)
    }

    /// Reconstruct a CID from its 36 on-wire bytes. Validates the
    /// version (0x01), codec (DAG_CBOR 0x71 OR RAW 0x55), multihash
    /// code (blake3-256 0x1e) and digest length (32). Wrong-codec or
    /// wrong-hash CIDs are NOT this type.
    ///
    /// The digest is NOT verified against any byte string here; use
    /// [`Cid::matches`] for that. This constructor is the inverse of
    /// [`Cid::as_bytes`].
    pub fn from_bytes(bytes: [u8; 36]) -> Result<Self, crate::error::DecodeError> {
        if bytes[0] != CID_VERSION_1 {
            return Err(crate::error::DecodeError::NotValidCbor);
        }
        if bytes[1] != Self::DAG_CBOR && bytes[1] != Self::RAW {
            return Err(crate::error::DecodeError::NotValidCbor);
        }
        if bytes[2] != BLAKE3_256_HASH_CODE || bytes[3] != BLAKE3_256_HASH_LEN {
            return Err(crate::error::DecodeError::NotValidCbor);
        }
        Ok(Cid(bytes))
    }

    /// Build a dag-cbor-coded CID from a known 32-byte BLAKE3 digest,
    /// by prepending the standard `v1 + dag-cbor + blake3-256 + len 32`
    /// prefix.
    ///
    /// Use this when adapting a 32-byte hash from a foreign protocol
    /// (Nostr event IDs, ActivityPub digests, legacy wire bytes the
    /// bridge layer translates into Fauna kinds). The result is a
    /// well-formed CID — no need to also call `from_bytes` to validate.
    ///
    /// This does NOT compute any hash; the caller asserts that
    /// `digest` is the BLAKE3-256 hash of some canonical dag-cbor
    /// payload they hold elsewhere. If you have the bytes themselves,
    /// use [`Cid::of_dag_cbor`] instead.
    pub fn from_digest_dag_cbor(digest: [u8; 32]) -> Self {
        Self::from_digest_with_codec(digest, Self::DAG_CBOR)
    }

    /// Build a raw-coded CID from a known 32-byte BLAKE3 digest, by
    /// prepending the standard `v1 + raw + blake3-256 + len 32` prefix.
    ///
    /// Use this when wrapping a 32-byte content-addressing digest of
    /// an opaque byte stream (media blob, mail body, imported file) —
    /// the codec byte signals "this is NOT dag-cbor".
    pub fn from_digest_raw(digest: [u8; 32]) -> Self {
        Self::from_digest_with_codec(digest, Self::RAW)
    }

    fn from_digest_with_codec(digest: [u8; 32], codec: u8) -> Self {
        let mut buf = [0u8; 36];
        buf[0] = CID_VERSION_1;
        buf[1] = codec;
        buf[2] = BLAKE3_256_HASH_CODE;
        buf[3] = BLAKE3_256_HASH_LEN;
        buf[4..].copy_from_slice(&digest);
        Cid(buf)
    }

    /// Returns true iff `blake3(bytes)` equals this CID's digest. No encoder involved.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        let digest = blake3::hash(bytes);
        self.0[4..] == *digest.as_bytes()
    }

    pub fn as_bytes(&self) -> &[u8; 36] {
        &self.0
    }

    /// The 32-byte BLAKE3 digest payload of the CID — bytes 4..36 of
    /// the on-wire layout. Centralizes the previous `as_bytes()[4..]`
    /// boundary strip pattern used by callers that need to write the
    /// digest into a 32-byte SQLite column, a Nostr event id, a CARv2
    /// index entry, etc.
    #[inline]
    pub fn digest(&self) -> [u8; 32] {
        // self.0 is 36 bytes by construction; [4..36] is exactly 32 → infallible.
        self.0[4..36]
            .try_into()
            .expect("Cid digest slice is 32 bytes")
    }

    pub fn codec(&self) -> u8 {
        self.0[1]
    }

    pub fn multihash_code(&self) -> u8 {
        self.0[2]
    }

    /// Base32-lowercase multibase form ('b' prefix + base32 body).
    pub fn to_base32(&self) -> String {
        multibase::encode(Base::Base32Lower, self.0)
    }

    pub fn from_base32(s: &str) -> Result<Self, crate::error::DecodeError> {
        let (base, decoded) =
            multibase::decode(s).map_err(|_| crate::error::DecodeError::NotValidCbor)?;
        if base != Base::Base32Lower {
            return Err(crate::error::DecodeError::NotValidCbor);
        }
        if decoded.len() != 36 {
            return Err(crate::error::DecodeError::NotValidCbor);
        }
        let mut buf = [0u8; 36];
        buf.copy_from_slice(&decoded);
        // Delegate codec/version/multihash validation to from_bytes — keeps
        // the accepted-codec set in one place.
        Cid::from_bytes(buf)
    }
}

impl std::fmt::Debug for Cid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cid({})", self.to_base32())
    }
}

impl std::fmt::Display for Cid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_base32())
    }
}

impl Cid {
    /// The same CID as the `cid` crate's type — the vehicle that carries
    /// it through serde as an IPLD link.
    fn to_link(self) -> cid::Cid {
        cid::Cid::try_from(&self.0[..]).expect("a Cid's 36 bytes are a well-formed CIDv1")
    }
}

impl Serialize for Cid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // An IPLD link: under canonical dag-cbor, tag 42 over a byte string
        // of `0x00` + the 36 bytes `Cid::as_bytes` returns.
        self.to_link().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Cid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The link form only: the bare byte-string spelling is refused (no
        // fallback — one value, one canonical byte form).
        let link = cid::Cid::deserialize(d)?;
        let bytes = link.to_bytes();
        let buf: [u8; 36] = bytes.as_slice().try_into().map_err(|_| {
            serde::de::Error::invalid_length(
                bytes.len(),
                &"36-byte CID (v1 + (dag-cbor|raw) + blake3-256 + 32-byte digest)",
            )
        })?;
        Cid::from_bytes(buf).map_err(|_| {
            serde::de::Error::custom(
                "invalid CID prefix (expected v1 + (dag-cbor 0x71 | raw 0x55) + blake3-256 0x1e + len 0x20)",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_canonical() {
        let cid = Cid::of_dag_cbor(b"hello world");
        // `Cid` serializes as a tag-42 link; strict decode round-trips it.
        let bytes = crate::encode_canonical(&cid).unwrap();
        let decoded: Cid = crate::decode_strict(&bytes).unwrap();
        assert_eq!(decoded, cid);
    }

    #[test]
    fn serde_rejects_wrong_length() {
        // A link to a CID with a 31-byte digest must fail `Cid`
        // deserialization: only the fixed 36-byte shape is this type.
        let mh = ::multihash_codetable::Multihash::wrap(0x1e, &[0u8; 31]).unwrap();
        let too_short = crate::Value::Link(::cid::Cid::new_v1(0x71, mh));
        let bytes = crate::encode_canonical(&too_short).unwrap();
        let result: Result<Cid, _> = crate::decode_strict(&bytes);
        assert!(result.is_err());
    }
}
