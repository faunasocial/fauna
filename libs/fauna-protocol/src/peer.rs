//! `fauna.peer.*` — the peer-to-peer Y.1 RPC kinds that ride the
//! substrate-agnostic peer channel (`libs/fauna-peer-channel`'s `PeerChannel`
//! over a WireGuard *or* an iroh `PeerConn`, `docs/goal/architecture/transport.md`
//! § Layers).
//!
//! Distinct from two neighbours that share the `fauna.peer.` string prefix but
//! ride other planes:
//! - the nest-level pre-identity discovery kinds ([`crate::discovery`],
//!   `fauna.nest.info` / `fauna.nest.resolve`) — nest↔client over the anonymous
//!   WS connection, not peer↔peer;
//! - the `fauna.peer.wake` / `fauna.peer.signal` **push events**
//!   ([`crate::push_events`]) — nest→client tunnel-signaling notifications, not
//!   Request/Reply RPC.
//!
//! **iroh focus (2026-07-01).** The seam's production substrate is iroh
//! (`transport.md` § Future directions; `p2p.md` § Transport seam), where a
//! peer's `NodeId` **is** its Ed25519 actor key, proven intrinsically by the
//! QUIC handshake (PT-1b — no nest peer-registry lookup). So the fields that
//! only a WireGuard peer needs (its separate x25519 key) are **optional**: an
//! iroh peer omits them, a WireGuard peer includes them. This is the
//! additive-everywhere shape (`version-compatibility.md` § 1) applied to the
//! substrate axis, not just the version axis.
//!
//! **Forward-compat.** Every payload is non-strict and carries the rule-4
//! `extra` catch-all (`transport.md` § Schema and forward-compat discipline;
//! CI-enforced by `tools/check-additive-evolution`): a peer speaking a newer
//! minor keeps its unknown fields on a round-trip, and a peer that predates a
//! kind answers it with `fauna.protocol.unknown_kind` (the `PeerChannel::serve`
//! default), never a hang. Optional fields use `#[serde(default)]`.
//!
//! Wire convention (matching [`crate::discovery`] / [`crate::auth`]): identity
//! references (`actor_id`) are **hex-encoded `String`**; the WireGuard x25519
//! key is a **base64 `String`** (the encoding the retired legacy exchange used,
//! kept so a WG-fallback revival needs no re-encoding). The dag-cbor wire
//! forbids floats (none here) and does not round-trip `Option<Option>` (none
//! used).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::Value;

/// The Y.1 wire/protocol version a peer speaks over the [`PeerChannel`] — bumped
/// only on a breaking peer-protocol change (never within a major; new kinds and
/// new optional fields are additive and leave this untouched). Surfaced in
/// [`PeerNodeInfoReply::protocol_version`] so two peers can negotiate down to a
/// shared feature set (`version-compatibility.md` § 3).
///
/// [`PeerChannel`]: https://docs.rs/fauna-peer-channel
pub const PEER_PROTOCOL_VERSION: u32 = 1;

/// WS-RPC kind: peer connectivity check over an established [`PeerChannel`].
pub const KIND_PEER_NODE_INFO: &str = "fauna.peer.node_info";
/// WS-RPC kind: peer identity exchange — the Y.1 reframe of the retired legacy
/// length-prefixed-JSON exchange (deleted 2026-08-18). **Parked with the WG
/// fallback**: its remaining job is carrying the WG x25519 registry binding,
/// which an iroh peer never needs — the `NodeId` *is* the actor key (PT-1b).
/// `p2p.md` § No pairing step, ever owns the disposition.
pub const KIND_PEER_EXCHANGE: &str = "fauna.peer.exchange";

// ── fauna.peer.node_info ─────────────────────────────────────────────────────

/// `fauna.peer.node_info` request — an empty connectivity ping. The round-trip
/// itself proves the channel is live; the reply carries the responder's version
/// and label.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerNodeInfoRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.node_info` reply — proves the channel is live and identifies the
/// responder: the peer-protocol version it speaks (additive negotiation) plus a
/// human display name. The peer's **cryptographic** identity is the
/// transport-proven `PeerConn::peer_identity()` (applied per-pair above the seam,
/// PT-2/PT-3), *not* re-sent here.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerNodeInfoReply {
    /// The [`PEER_PROTOCOL_VERSION`] the responder speaks.
    pub protocol_version: u32,
    /// The responder's human display name.
    pub display_name: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.peer.exchange ──────────────────────────────────────────────────────

/// `fauna.peer.exchange` request — the Y.1 reframe of the retired
/// length-prefixed-JSON identity exchange (deleted 2026-08-18; parked with the
/// WG fallback, see [`KIND_PEER_EXCHANGE`]). The initiator proves control of
/// its actor key by signing an out-of-band (QR-pairing) nonce.
///
/// There is no separate substrate key: the iroh `NodeId` *is* the Ed25519 actor
/// key, proven intrinsically by the QUIC handshake (PT-1b — no registry). A key
/// this build does not declare lands in [`PeerExchangeRequest::extra`]
/// (`transport.md` § forward-compat rule 4).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerExchangeRequest {
    /// The initiator's actor id — hex-encoded 32-byte Ed25519 public key.
    pub actor_id: String,
    /// The initiator's human display name.
    pub display_name: String,
    /// Hex-encoded 64-byte Ed25519 signature over the shared out-of-band nonce.
    pub nonce_signature: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.peer.exchange` reply — the acceptor's own identity. A **rejected**
/// exchange (a bad nonce signature) rides the Y.1 `ok = false` reply
/// (`RpcError`), *not* an in-band error variant — unlike the legacy TCP
/// `ExchangeError` the reframe retires.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PeerExchangeReply {
    /// The acceptor's actor id — hex-encoded 32-byte Ed25519 public key.
    pub actor_id: String,
    /// The acceptor's human display name.
    pub display_name: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_strict as decode, encode_canonical};

    #[test]
    fn node_info_request_and_reply_round_trip() {
        let req = PeerNodeInfoRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        let back: PeerNodeInfoRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);

        let reply = PeerNodeInfoReply {
            protocol_version: PEER_PROTOCOL_VERSION,
            display_name: "Alice's laptop".to_string(),
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: PeerNodeInfoReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn exchange_request_round_trips() {
        // An iroh peer carries no separate substrate key — the NodeId is the
        // Ed25519 actor key.
        let req = PeerExchangeRequest {
            actor_id: "ab".repeat(32),
            display_name: "Bob".to_string(),
            nonce_signature: "cd".repeat(64),
            ..Default::default()
        };
        let bytes = encode_canonical(&req).unwrap();
        let back: PeerExchangeRequest = decode(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn exchange_reply_round_trips() {
        let reply = PeerExchangeReply {
            actor_id: "ef".repeat(32),
            display_name: "Alice".to_string(),
            ..Default::default()
        };
        let bytes = encode_canonical(&reply).unwrap();
        let back: PeerExchangeReply = decode(&bytes).unwrap();
        assert_eq!(back, reply);
    }

    /// A field this build does not declare is **absorbed by the `extra`
    /// catch-all**, never rejected (`transport.md` § forward-compat rule 4).
    #[test]
    fn exchange_request_absorbs_an_unknown_field() {
        /// The exchange plus a field this build doesn't know.
        #[derive(Serialize)]
        struct OtherVersionExchange {
            actor_id: String,
            display_name: String,
            nonce_signature: String,
            some_future_field: u32,
        }
        let other = OtherVersionExchange {
            actor_id: "ab".repeat(32),
            display_name: "Bob".to_string(),
            nonce_signature: "cd".repeat(64),
            some_future_field: 7,
        };
        let bytes = encode_canonical(&other).unwrap();
        let decoded: PeerExchangeRequest = decode(&bytes).unwrap();

        assert_eq!(decoded.actor_id, other.actor_id);
        assert_eq!(decoded.display_name, other.display_name);
        assert_eq!(
            decoded.extra.get("some_future_field"),
            Some(&Value::Integer(7)),
            "unknown field must survive in the extra catch-all"
        );
    }
}
