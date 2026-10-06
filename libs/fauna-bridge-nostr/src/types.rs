use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A signed Nostr event (NIP-01).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// 32-byte lowercase hex SHA-256 of the canonical serialization.
    pub id: String,
    /// 32-byte lowercase hex x-only public key of the event creator.
    pub pubkey: String,
    /// Unix timestamp in seconds.
    pub created_at: u64,
    /// Event kind number.
    pub kind: u64,
    /// Ordered list of tags.
    pub tags: Vec<Tag>,
    /// Arbitrary string content.
    pub content: String,
    /// 64-byte lowercase hex schnorr signature over the event ID.
    pub sig: String,
}

/// An unsigned event, ready to be signed.
#[derive(Debug, Clone)]
pub struct UnsignedEvent {
    /// 32-byte x-only public key.
    pub pubkey: [u8; 32],
    /// Unix timestamp in seconds.
    pub created_at: u64,
    /// Event kind number.
    pub kind: u64,
    /// Ordered list of tags.
    pub tags: Vec<Tag>,
    /// Arbitrary string content.
    pub content: String,
}

/// A tag is a JSON array of strings: ["tag_name", "value1", "value2", ...].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag(pub Vec<String>);

impl Tag {
    pub fn new(fields: Vec<String>) -> Self {
        Self(fields)
    }

    /// The tag name (first element), e.g. "e", "p", "t".
    pub fn name(&self) -> Option<&str> {
        self.0.first().map(|s| s.as_str())
    }

    /// The first value (second element).
    pub fn value(&self) -> Option<&str> {
        self.0.get(1).map(|s| s.as_str())
    }

    /// Get element at index.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A subscription filter (NIP-01).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Filter {
    /// Event IDs to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    /// Author pubkeys to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    /// Event kinds to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<u64>>,
    /// Minimum created_at (inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    /// Maximum created_at (inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    /// Maximum number of events to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// NIP-50 full-text search over event `content`. A named field (not the
    /// tag flatten below) both gives it semantics and keeps a real client's
    /// string-valued `"search"` key from choking the `Vec<String>` flatten.
    /// Consumers derive their verdict via [`crate::nip50`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Generic tag filters. Key is the single-letter tag name (without #).
    /// e.g., key "e" matches events with tag ["e", <value>].
    #[serde(flatten)]
    pub tags: HashMap<String, Vec<String>>,
}

/// Well-known Nostr event kinds.
pub mod kind {
    pub const METADATA: u64 = 0;
    pub const TEXT_NOTE: u64 = 1;
    pub const CONTACT_LIST: u64 = 3;
    pub const DELETION: u64 = 5;
    pub const REPOST: u64 = 6;
    pub const REACTION: u64 = 7;
    pub const RELAY_LIST: u64 = 10002;
    pub const SEAL: u64 = 13;
    pub const PRIVATE_DM: u64 = 14;
    pub const GIFT_WRAP: u64 = 1059;
    pub const AUTH: u64 = 22242;
    pub const LONG_FORM: u64 = 30023;
    pub const COMMUNITY_DEFINITION: u64 = 34550;
    pub const COMMUNITY_APPROVED: u64 = 4550;
    pub const BADGE_AWARD: u64 = 8;
    pub const BADGE_DEFINITION: u64 = 30009;
    pub const LIVE_ACTIVITY: u64 = 30311;
    pub const CLASSIFIED: u64 = 30402;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_accessors() {
        let tag = Tag::new(vec![
            "e".to_string(),
            "abc123".to_string(),
            "wss://relay.example.com".to_string(),
            "reply".to_string(),
        ]);
        assert_eq!(tag.name(), Some("e"));
        assert_eq!(tag.value(), Some("abc123"));
        assert_eq!(tag.get(2), Some("wss://relay.example.com"));
        assert_eq!(tag.get(3), Some("reply"));
        assert_eq!(tag.len(), 4);
    }

    #[test]
    fn filter_default_is_empty() {
        let f = Filter::default();
        assert!(f.ids.is_none());
        assert!(f.authors.is_none());
        assert!(f.kinds.is_none());
        assert!(f.since.is_none());
        assert!(f.until.is_none());
        assert!(f.limit.is_none());
        assert!(f.tags.is_empty());
    }

    #[test]
    fn event_serde_roundtrip() {
        let event = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1234567890,
            kind: 1,
            tags: vec![Tag::new(vec!["p".into(), "c".repeat(64)])],
            content: "hello world".into(),
            sig: "d".repeat(128),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, decoded);
    }
}
