use crate::types::{Event, Tag, UnsignedEvent, kind};

/// Build a kind-6 repost event wrapping the original event JSON.
pub fn build_repost_event(
    reposted_event: &Event,
    reposter_pubkey: &[u8; 32],
    created_at: u64,
) -> UnsignedEvent {
    let tags = vec![
        Tag::new(vec!["e".into(), reposted_event.id.clone(), "".into()]),
        Tag::new(vec!["p".into(), reposted_event.pubkey.clone()]),
    ];

    UnsignedEvent {
        pubkey: *reposter_pubkey,
        created_at,
        kind: kind::REPOST,
        tags,
        content: serde_json::to_string(reposted_event).unwrap_or_default(),
    }
}

/// Build tags for a quote repost (kind 1 with a `q` tag).
pub fn build_quote_tags(quoted_id: &str, quoted_pubkey: &str) -> Vec<Tag> {
    vec![
        Tag::new(vec!["q".into(), quoted_id.into()]),
        Tag::new(vec!["p".into(), quoted_pubkey.into()]),
    ]
}

/// Check if an event is a repost (kind 6).
pub fn is_repost(event: &Event) -> bool {
    event.kind == kind::REPOST
}

/// Extract the reposted event ID from a kind-6 repost.
pub fn parse_repost_target(event: &Event) -> Option<String> {
    if event.kind != kind::REPOST {
        return None;
    }
    event
        .tags
        .iter()
        .find(|t| t.name() == Some("e"))
        .and_then(|t| t.value())
        .map(|s| s.to_string())
}

/// Extract the quoted event ID from a `q` tag.
pub fn parse_quote_target(event: &Event) -> Option<String> {
    event
        .tags
        .iter()
        .find(|t| t.name() == Some("q"))
        .and_then(|t| t.value())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: "original post".into(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn build_repost_produces_kind_6() {
        let original = sample_event();
        let reposter = [1u8; 32];
        let repost = build_repost_event(&original, &reposter, 2000);
        assert_eq!(repost.kind, kind::REPOST);
        assert_eq!(repost.created_at, 2000);
        // Should have e and p tags
        assert!(repost.tags.iter().any(|t| t.name() == Some("e")));
        assert!(repost.tags.iter().any(|t| t.name() == Some("p")));
        // Content should contain the original event JSON
        assert!(repost.content.contains("original post"));
    }

    #[test]
    fn parse_repost_target_extracts_id() {
        let original = sample_event();
        let reposter = [1u8; 32];
        let unsigned = build_repost_event(&original, &reposter, 2000);
        let event = Event {
            id: "x".repeat(64),
            pubkey: hex::encode(reposter),
            created_at: unsigned.created_at,
            kind: unsigned.kind,
            tags: unsigned.tags,
            content: unsigned.content,
            sig: "s".repeat(128),
        };
        assert_eq!(
            parse_repost_target(&event).as_deref(),
            Some(original.id.as_str())
        );
    }

    #[test]
    fn build_quote_tags_has_q_and_p() {
        let tags = build_quote_tags("event123", "pubkey456");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].name(), Some("q"));
        assert_eq!(tags[0].value(), Some("event123"));
        assert_eq!(tags[1].name(), Some("p"));
        assert_eq!(tags[1].value(), Some("pubkey456"));
    }

    #[test]
    fn parse_quote_target_extracts_from_q_tag() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![
                Tag::new(vec!["q".into(), "quoted_id".into()]),
                Tag::new(vec!["p".into(), "pk".into()]),
            ],
            content: "quoting someone".into(),
            sig: "s".repeat(128),
        };
        assert_eq!(parse_quote_target(&event).as_deref(), Some("quoted_id"));
    }
}
