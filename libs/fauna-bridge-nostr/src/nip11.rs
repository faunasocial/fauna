use serde::{Deserialize, Serialize};

/// Maximum accepted size of one event's wire JSON, in bytes. The single source
/// of truth for both the NIP-11 `max_message_length` advertisement (below) and
/// the relay's EVENT-size enforcement (nest-side), so the two can never drift.
/// A hard-coded protocol constant, never configurable (the one-configuration
/// -surface invariant — no operator, no env/flag).
pub const MAX_EVENT_SIZE: usize = 64 * 1024;

/// Transport-layer cap on one Nostr WebSocket message (and frame), in bytes —
/// the single source of truth for BOTH directions, so they can never drift:
/// the nest's inbound `/nostr` endpoint (a stranger's REQ/EVENT frames) and
/// the outbound [`crate::relay_client::RelayClient`] every relay dial goes
/// through (a user-chosen relay's replies). Strictly above
/// [`MAX_EVENT_SIZE`] so an honest event, wrapped in its `EVENT` envelope, is
/// never clipped; far below tungstenite's / axum's ~64 MiB default, which let
/// an unauthenticated client stream multi-MB REQ frames into filter parsing,
/// and a hostile relay pin up to 64 MiB per message per connection. A
/// hard-coded constant, never a configuration surface.
pub const MAX_WS_MESSAGE_BYTES: usize = 512 * 1024;

/// Maximum concurrent REQ subscriptions per connection. Single source of
/// truth for the NIP-11 `max_subscriptions` advertisement and the relay's
/// enforcement (nest-side), like [`MAX_EVENT_SIZE`]. Bounds the per-broadcast
/// re-evaluation work a single connection can pin (each live event is matched
/// against every subscription). Hard-coded, never configurable.
pub const MAX_SUBSCRIPTIONS_PER_CONN: usize = 32;

/// Maximum filters one REQ/COUNT may carry. Single source of truth for the
/// NIP-11 `max_filters` advertisement and the relay's enforcement — each
/// filter is a store query, so an uncapped list is an unauthenticated
/// query-amplification vector even under a frame-size cap. Hard-coded,
/// never configurable.
pub const MAX_FILTERS_PER_REQ: usize = 16;

/// NIP-11 relay information document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub supported_nips: Vec<u64>,
    pub software: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitation: Option<RelayLimitation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayLimitation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_required: Option<bool>,
    /// Writes are restricted to authenticated local accounts (NIP-11
    /// `restricted_writes`), while reads are open — the outbox-relay posture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restricted_writes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_message_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_subscriptions: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_filters: Option<u64>,
}

impl RelayInfo {
    /// Serialize to JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("RelayInfo serialization should not fail")
    }
}

/// Build the default relay info for a Fauna nest.
///
/// Advertises the outbox-relay posture truthfully: reads are open
/// (`auth_required: false`) and writes are restricted to authenticated local
/// accounts (`restricted_writes: true`), with the real per-event size limit the
/// relay enforces ([`MAX_EVENT_SIZE`]).
pub fn fauna_relay_info(name: &str, version: &str) -> RelayInfo {
    RelayInfo {
        name: name.to_string(),
        description: Some("Fauna nest Nostr relay".to_string()),
        // 9/40/45 built slice C (deletion/expiration/count); 17/59 were
        // already built in slice B (gift-wrap DM inbox) but missing here —
        // an honesty gap of the same kind slice A's NIP-11 fixes closed.
        supported_nips: vec![1, 2, 5, 9, 10, 11, 17, 18, 19, 25, 40, 42, 45, 50, 59],
        software: "fauna-nest".to_string(),
        version: version.to_string(),
        limitation: Some(RelayLimitation {
            auth_required: Some(false),
            restricted_writes: Some(true),
            payment_required: Some(false),
            max_message_length: Some(MAX_EVENT_SIZE as u64),
            max_subscriptions: Some(MAX_SUBSCRIPTIONS_PER_CONN as u64),
            max_filters: Some(MAX_FILTERS_PER_REQ as u64),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_info_to_json_has_required_fields() {
        let info = fauna_relay_info("test.fauna.social", "0.1.0");
        let json = info.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "test.fauna.social");
        assert!(parsed["supported_nips"].is_array());
        assert_eq!(parsed["software"], "fauna-nest");
        assert_eq!(parsed["version"], "0.1.0");
        // Truthful outbox posture: reads open, writes restricted, real size cap.
        assert_eq!(parsed["limitation"]["auth_required"], false);
        assert_eq!(parsed["limitation"]["restricted_writes"], true);
        assert_eq!(
            parsed["limitation"]["max_message_length"],
            MAX_EVENT_SIZE as u64
        );
        assert_eq!(
            parsed["limitation"]["max_subscriptions"],
            MAX_SUBSCRIPTIONS_PER_CONN as u64
        );
        assert_eq!(
            parsed["limitation"]["max_filters"],
            MAX_FILTERS_PER_REQ as u64
        );
    }

    #[test]
    fn relay_info_serde_roundtrip() {
        let info = fauna_relay_info("test", "1.0");
        let json = info.to_json();
        let decoded: RelayInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "test");
        assert_eq!(decoded.supported_nips, info.supported_nips);
    }
}
