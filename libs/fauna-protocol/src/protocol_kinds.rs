//! Protocol-level kinds shipped by Spec Y itself. Test-only / infrastructure.
//!
//! `fauna.protocol.echo` is the canonical end-to-end RPC test surface.
//! `fauna.protocol.resync_required` is the backpressure marker (its
//! payload type lives in push_events.rs alongside the other push payloads).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use fauna_cbor::Value;

/// `fauna.protocol.echo` request — server returns the same bytes.
/// Used by tests/e2e-unified/tests/test_api_helpers.py.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EchoRequest {
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EchoReply {
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    #[test]
    fn echo_request_round_trip() {
        let r = EchoRequest {
            data: vec![1, 2, 3],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: EchoRequest = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }

    #[test]
    fn echo_reply_round_trip() {
        let r = EchoReply {
            data: vec![1, 2, 3],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&r).unwrap();
        let decoded: EchoReply = decode(&bytes).unwrap();
        assert_eq!(decoded, r);
    }
}
