//! Wire envelope types. Per spec § 1.1.
//!
//! Every WebSocket binary frame is one canonical DAG-CBOR map. Four
//! frame types — Request/Reply/Push/Cancel — discriminated by integer
//! field `0`. Integer keys throughout for hot-path framing.

use std::fmt;

use bytes::Bytes;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

use crate::codec::encode_canonical;

// ── Frame types ────────────────────────────────────────────────────

/// `Request.5: replay_forbidden` is a wire-level mirror of the per-kind
/// CDDL annotation; client crate sets it from KindRegistry metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// `0` — type discriminant: always 0 for Request.
    pub ty: u8,
    /// `1` — correlation_id (per-connection ascending u64).
    pub correlation_id: u64,
    /// `2` — kind ("fauna.<area>.<verb>").
    pub kind: String,
    /// `3` — idempotency_key (16 random bytes per call).
    pub idempotency_key: [u8; 16],
    /// `4` — payload (kind-specific; CDDL-typed elsewhere).
    pub payload: Value,
    /// `5` — replay_forbidden hint (default false).
    pub replay_forbidden: Option<bool>,
    /// `6` — deadline_ms (request-level timeout override).
    pub deadline_ms: Option<u32>,
}

impl Request {
    pub const TYPE: u8 = 0;
}

impl Serialize for Request {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut len = 5; // 0..4 always present
        if self.replay_forbidden.is_some() {
            len += 1;
        }
        if self.deadline_ms.is_some() {
            len += 1;
        }
        let mut m = ser.serialize_map(Some(len))?;
        m.serialize_entry(&0u8, &self.ty)?;
        m.serialize_entry(&1u8, &self.correlation_id)?;
        m.serialize_entry(&2u8, &self.kind)?;
        m.serialize_entry(&3u8, serde_bytes::Bytes::new(&self.idempotency_key))?;
        m.serialize_entry(&4u8, &self.payload)?;
        if let Some(rf) = self.replay_forbidden {
            m.serialize_entry(&5u8, &rf)?;
        }
        if let Some(dm) = self.deadline_ms {
            m.serialize_entry(&6u8, &dm)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Request;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("Request map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Request, A::Error> {
                let mut ty: Option<u8> = None;
                let mut correlation_id: Option<u64> = None;
                let mut kind: Option<String> = None;
                let mut idempotency_key: Option<[u8; 16]> = None;
                let mut payload: Option<Value> = None;
                let mut replay_forbidden: Option<bool> = None;
                let mut deadline_ms: Option<u32> = None;
                while let Some(k) = map.next_key::<i64>()? {
                    match k {
                        0 => ty = Some(map.next_value()?),
                        1 => correlation_id = Some(map.next_value()?),
                        2 => kind = Some(map.next_value()?),
                        3 => {
                            let bb: serde_bytes::ByteBuf = map.next_value()?;
                            let arr: [u8; 16] =
                                bb.into_vec().try_into().map_err(|v: Vec<u8>| {
                                    de::Error::invalid_length(v.len(), &"16 bytes")
                                })?;
                            idempotency_key = Some(arr);
                        }
                        4 => payload = Some(map.next_value()?),
                        5 => replay_forbidden = Some(map.next_value()?),
                        6 => deadline_ms = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(Request {
                    ty: ty.ok_or_else(|| de::Error::missing_field("0"))?,
                    correlation_id: correlation_id.ok_or_else(|| de::Error::missing_field("1"))?,
                    kind: kind.ok_or_else(|| de::Error::missing_field("2"))?,
                    idempotency_key: idempotency_key
                        .ok_or_else(|| de::Error::missing_field("3"))?,
                    payload: payload.ok_or_else(|| de::Error::missing_field("4"))?,
                    replay_forbidden,
                    deadline_ms,
                })
            }
        }
        de.deserialize_map(V)
    }
}

/// Server reply for a Request.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// `0` — type discriminant: always 1 for Reply.
    pub ty: u8,
    /// `1` — correlation_id (matches request).
    pub correlation_id: u64,
    /// `4` — payload (kind-specific result, OR encoded RpcError when ok=false).
    pub payload: Value,
    /// `7` — ok (true = success, false = error).
    pub ok: bool,
}

impl Reply {
    pub const TYPE: u8 = 1;
}

impl Serialize for Reply {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(Some(4))?;
        m.serialize_entry(&0u8, &self.ty)?;
        m.serialize_entry(&1u8, &self.correlation_id)?;
        m.serialize_entry(&4u8, &self.payload)?;
        m.serialize_entry(&7u8, &self.ok)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for Reply {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Reply;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("Reply map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Reply, A::Error> {
                let mut ty: Option<u8> = None;
                let mut correlation_id: Option<u64> = None;
                let mut payload: Option<Value> = None;
                let mut ok: Option<bool> = None;
                while let Some(k) = map.next_key::<i64>()? {
                    match k {
                        0 => ty = Some(map.next_value()?),
                        1 => correlation_id = Some(map.next_value()?),
                        4 => payload = Some(map.next_value()?),
                        7 => ok = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(Reply {
                    ty: ty.ok_or_else(|| de::Error::missing_field("0"))?,
                    correlation_id: correlation_id.ok_or_else(|| de::Error::missing_field("1"))?,
                    payload: payload.ok_or_else(|| de::Error::missing_field("4"))?,
                    ok: ok.ok_or_else(|| de::Error::missing_field("7"))?,
                })
            }
        }
        de.deserialize_map(V)
    }
}

/// Server-initiated unsolicited push event.
#[derive(Debug, Clone, PartialEq)]
pub struct Push {
    /// `0` — type discriminant: always 2 for Push.
    pub ty: u8,
    /// `2` — kind.
    pub kind: String,
    /// `4` — payload (kind-specific).
    pub payload: Value,
    /// `8` — seq (per-connection ascending; gap = drop signal).
    pub seq: u64,
}

impl Push {
    pub const TYPE: u8 = 2;
}

impl Serialize for Push {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(Some(4))?;
        m.serialize_entry(&0u8, &self.ty)?;
        m.serialize_entry(&2u8, &self.kind)?;
        m.serialize_entry(&4u8, &self.payload)?;
        m.serialize_entry(&8u8, &self.seq)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for Push {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Push;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("Push map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Push, A::Error> {
                let mut ty: Option<u8> = None;
                let mut kind: Option<String> = None;
                let mut payload: Option<Value> = None;
                let mut seq: Option<u64> = None;
                while let Some(k) = map.next_key::<i64>()? {
                    match k {
                        0 => ty = Some(map.next_value()?),
                        2 => kind = Some(map.next_value()?),
                        4 => payload = Some(map.next_value()?),
                        8 => seq = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(Push {
                    ty: ty.ok_or_else(|| de::Error::missing_field("0"))?,
                    kind: kind.ok_or_else(|| de::Error::missing_field("2"))?,
                    payload: payload.ok_or_else(|| de::Error::missing_field("4"))?,
                    seq: seq.ok_or_else(|| de::Error::missing_field("8"))?,
                })
            }
        }
        de.deserialize_map(V)
    }
}

/// Client-initiated cancel of an in-flight Request.
#[derive(Debug, Clone, PartialEq)]
pub struct Cancel {
    /// `0` — type discriminant: always 3 for Cancel.
    pub ty: u8,
    /// `1` — correlation_id of the request to cancel.
    pub correlation_id: u64,
}

impl Cancel {
    pub const TYPE: u8 = 3;
}

impl Serialize for Cancel {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(Some(2))?;
        m.serialize_entry(&0u8, &self.ty)?;
        m.serialize_entry(&1u8, &self.correlation_id)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for Cancel {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Cancel;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("Cancel map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Cancel, A::Error> {
                let mut ty: Option<u8> = None;
                let mut correlation_id: Option<u64> = None;
                while let Some(k) = map.next_key::<i64>()? {
                    match k {
                        0 => ty = Some(map.next_value()?),
                        1 => correlation_id = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(Cancel {
                    ty: ty.ok_or_else(|| de::Error::missing_field("0"))?,
                    correlation_id: correlation_id.ok_or_else(|| de::Error::missing_field("1"))?,
                })
            }
        }
        de.deserialize_map(V)
    }
}

/// One of the four frame types. Decoded by inspecting the `0` field.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Request(Request),
    Reply(Reply),
    Push(Push),
    Cancel(Cancel),
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("decode: {0}")]
    Decode(#[from] fauna_cbor::DecodeError),
    #[error("encode: {0}")]
    Encode(#[from] fauna_cbor::EncodeError),
    #[error("missing or invalid type discriminant (key 0)")]
    MissingType,
    #[error("unknown frame type discriminant: {0}")]
    UnknownType(u8),
}

/// Encode a `Frame` to canonical CBOR bytes.
pub fn encode_frame(frame: &Frame) -> Result<Bytes, FrameError> {
    match frame {
        Frame::Request(r) => Ok(encode_canonical(r)?),
        Frame::Reply(r) => Ok(encode_canonical(r)?),
        Frame::Push(p) => Ok(encode_canonical(p)?),
        Frame::Cancel(c) => Ok(encode_canonical(c)?),
    }
}

/// Just the integer-key-`0` type discriminant of a frame map. The wire
/// envelope uses integer map keys (key `0` = type, key `1` = correlation_id,
/// …), which `Value` (string-keyed dag-cbor) cannot represent — so we peek the
/// discriminant with a purpose-built typed deserializer rather than a generic
/// node. Reads key `0` as the discriminant, skips every other entry. Errors
/// (via serde `missing_field`) when key `0` is absent.
struct FrameDiscriminant(u8);

impl<'de> Deserialize<'de> for FrameDiscriminant {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = FrameDiscriminant;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("frame map with integer key 0")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<FrameDiscriminant, A::Error> {
                let mut ty: Option<u8> = None;
                while let Some(k) = map.next_key::<i64>()? {
                    if k == 0 {
                        ty = Some(map.next_value()?);
                    } else {
                        let _: de::IgnoredAny = map.next_value()?;
                    }
                }
                Ok(FrameDiscriminant(
                    ty.ok_or_else(|| de::Error::missing_field("0"))?,
                ))
            }
        }
        de.deserialize_map(V)
    }
}

/// Decode bytes to a `Frame`. Strict-decodes the integer-key-`0` type
/// discriminant, then strict-decodes the matching typed frame.
pub fn decode_frame(bytes: &[u8]) -> Result<Frame, FrameError> {
    // Peek the type discriminant via a strict typed decode (no generic node —
    // the frame map has integer keys `Value` can't carry). Missing key 0 maps
    // to `MissingType`; any other malformed-CBOR error surfaces as `Decode`.
    let ty = match fauna_cbor::decode_strict::<FrameDiscriminant>(bytes) {
        Ok(d) => d.0,
        Err(fauna_cbor::DecodeError::SchemaMismatch(msg)) if msg.contains("missing field") => {
            return Err(FrameError::MissingType);
        }
        Err(e) => return Err(FrameError::from(e)),
    };

    match ty {
        0 => Ok(Frame::Request(fauna_cbor::decode_strict::<Request>(bytes)?)),
        1 => Ok(Frame::Reply(fauna_cbor::decode_strict::<Reply>(bytes)?)),
        2 => Ok(Frame::Push(fauna_cbor::decode_strict::<Push>(bytes)?)),
        3 => Ok(Frame::Cancel(fauna_cbor::decode_strict::<Cancel>(bytes)?)),
        n => Err(FrameError::UnknownType(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> Request {
        Request {
            ty: Request::TYPE,
            correlation_id: 42,
            kind: "fauna.bridges.link".into(),
            idempotency_key: [0u8; 16],
            payload: Value::Map(Default::default()),
            replay_forbidden: None,
            deadline_ms: None,
        }
    }

    fn reply_ok() -> Reply {
        Reply {
            ty: Reply::TYPE,
            correlation_id: 42,
            payload: Value::Null,
            ok: true,
        }
    }

    fn reply_err() -> Reply {
        Reply {
            ty: Reply::TYPE,
            correlation_id: 42,
            payload: Value::String("encoded_error".into()),
            ok: false,
        }
    }

    fn push() -> Push {
        Push {
            ty: Push::TYPE,
            kind: "fauna.knock".into(),
            payload: Value::Null,
            seq: 1,
        }
    }

    fn cancel() -> Cancel {
        Cancel {
            ty: Cancel::TYPE,
            correlation_id: 42,
        }
    }

    #[test]
    fn request_round_trip() {
        let f = Frame::Request(req());
        let bytes = encode_frame(&f).unwrap();
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(f, decoded);
    }

    #[test]
    fn reply_ok_round_trip() {
        let f = Frame::Reply(reply_ok());
        let bytes = encode_frame(&f).unwrap();
        match decode_frame(&bytes).unwrap() {
            Frame::Reply(r) => {
                assert!(r.ok);
                assert_eq!(r.correlation_id, 42);
            }
            other => panic!("expected Reply, got {:?}", other),
        }
    }

    #[test]
    fn reply_err_round_trip() {
        let f = Frame::Reply(reply_err());
        let bytes = encode_frame(&f).unwrap();
        match decode_frame(&bytes).unwrap() {
            Frame::Reply(r) => {
                assert!(!r.ok);
            }
            other => panic!("expected Reply, got {:?}", other),
        }
    }

    #[test]
    fn push_round_trip() {
        let f = Frame::Push(push());
        let bytes = encode_frame(&f).unwrap();
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(f, decoded);
    }

    #[test]
    fn cancel_round_trip() {
        let f = Frame::Cancel(cancel());
        let bytes = encode_frame(&f).unwrap();
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(f, decoded);
    }

    #[test]
    fn unknown_frame_type_errors() {
        // Synthesize a canonical CBOR map with integer key 0 → type=99 (an
        // unknown frame type) and key 1 → 0. `Value` can't carry integer keys,
        // so build the map with a tiny serde helper that serializes the same
        // integer-keyed shape the real frames use.
        struct UnknownFrame;
        impl Serialize for UnknownFrame {
            fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                let mut m = ser.serialize_map(Some(2))?;
                m.serialize_entry(&0u8, &99u8)?;
                m.serialize_entry(&1u8, &0u8)?;
                m.end()
            }
        }
        let bytes = encode_canonical(&UnknownFrame).unwrap();
        let result = decode_frame(&bytes);
        assert!(matches!(result, Err(FrameError::UnknownType(99))));
    }
}
