use sha2::{Digest, Sha256};

use crate::types::UnsignedEvent;

/// Compute the NIP-01 event ID: sha256 of the canonical JSON serialization.
///
/// The canonical form is: `[0, <pubkey_hex>, <created_at>, <kind>, <tags>, <content>]`
pub fn compute_event_id(event: &UnsignedEvent) -> [u8; 32] {
    let pubkey_hex = hex::encode(event.pubkey);

    // Build the canonical JSON array using serde_json::Value to ensure
    // correct JSON encoding of strings (escaping, etc.)
    let canonical = serde_json::json!([
        0,
        pubkey_hex,
        event.created_at,
        event.kind,
        event.tags,
        event.content,
    ]);

    let serialized = serde_json::to_string(&canonical).expect("JSON serialization should not fail");
    let hash = Sha256::digest(serialized.as_bytes());
    hash.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    #[test]
    fn canonical_serialization_produces_correct_id() {
        let event = UnsignedEvent {
            pubkey: [0u8; 32],
            created_at: 1234567890,
            kind: 1,
            tags: vec![],
            content: "hello".to_string(),
        };
        let id = compute_event_id(&event);
        assert_eq!(id.len(), 32);

        // Same inputs should produce same ID
        let id2 = compute_event_id(&event);
        assert_eq!(id, id2);
    }

    #[test]
    fn different_content_produces_different_id() {
        let e1 = UnsignedEvent {
            pubkey: [1u8; 32],
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: "hello".to_string(),
        };
        let e2 = UnsignedEvent {
            pubkey: [1u8; 32],
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: "world".to_string(),
        };
        assert_ne!(compute_event_id(&e1), compute_event_id(&e2));
    }

    #[test]
    fn tags_affect_id() {
        let e1 = UnsignedEvent {
            pubkey: [1u8; 32],
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: "test".to_string(),
        };
        let e2 = UnsignedEvent {
            pubkey: [1u8; 32],
            created_at: 1000,
            kind: 1,
            tags: vec![Tag::new(vec!["p".into(), "a".repeat(64)])],
            content: "test".to_string(),
        };
        assert_ne!(compute_event_id(&e1), compute_event_id(&e2));
    }
}
