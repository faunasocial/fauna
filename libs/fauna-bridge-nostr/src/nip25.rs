use crate::types::{Event, Tag, UnsignedEvent, kind};

/// Build a kind-7 reaction event.
pub fn build_reaction_event(
    target_id: &str,
    target_pubkey: &str,
    emoji: &str,
    reactor_pubkey: &[u8; 32],
    created_at: u64,
) -> UnsignedEvent {
    UnsignedEvent {
        pubkey: *reactor_pubkey,
        created_at,
        kind: kind::REACTION,
        tags: vec![
            Tag::new(vec!["e".into(), target_id.into()]),
            Tag::new(vec!["p".into(), target_pubkey.into()]),
        ],
        content: emoji.to_string(),
    }
}

/// Parse a kind-7 reaction event. Returns (target_event_id, emoji).
pub fn parse_reaction(event: &Event) -> Option<(String, String)> {
    if event.kind != kind::REACTION {
        return None;
    }
    let target_id = event
        .tags
        .iter()
        .rev()
        .find(|t| t.name() == Some("e"))
        .and_then(|t| t.value())
        .map(|s| s.to_string())?;

    let emoji = if event.content.is_empty() {
        "+".to_string() // default reaction per NIP-25
    } else {
        event.content.clone()
    };

    Some((target_id, emoji))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_reaction_produces_kind_7() {
        let reactor = [1u8; 32];
        let event = build_reaction_event("eid", "pk", "🤙", &reactor, 1000);
        assert_eq!(event.kind, kind::REACTION);
        assert_eq!(event.content, "🤙");
        assert!(
            event
                .tags
                .iter()
                .any(|t| t.name() == Some("e") && t.value() == Some("eid"))
        );
        assert!(
            event
                .tags
                .iter()
                .any(|t| t.name() == Some("p") && t.value() == Some("pk"))
        );
    }

    #[test]
    fn parse_reaction_extracts_target_and_emoji() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: kind::REACTION,
            tags: vec![
                Tag::new(vec!["e".into(), "target123".into()]),
                Tag::new(vec!["p".into(), "pk".into()]),
            ],
            content: "❤️".into(),
            sig: "s".repeat(128),
        };
        let (target, emoji) = parse_reaction(&event).unwrap();
        assert_eq!(target, "target123");
        assert_eq!(emoji, "❤️");
    }

    #[test]
    fn parse_reaction_empty_content_defaults_to_plus() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: kind::REACTION,
            tags: vec![Tag::new(vec!["e".into(), "target123".into()])],
            content: "".into(),
            sig: "s".repeat(128),
        };
        let (_, emoji) = parse_reaction(&event).unwrap();
        assert_eq!(emoji, "+");
    }

    #[test]
    fn parse_reaction_wrong_kind_returns_none() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![Tag::new(vec!["e".into(), "target123".into()])],
            content: "+".into(),
            sig: "s".repeat(128),
        };
        assert!(parse_reaction(&event).is_none());
    }
}
