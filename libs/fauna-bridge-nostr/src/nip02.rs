use crate::types::{Event, Tag, UnsignedEvent, kind};

/// A contact in a NIP-02 contact list.
#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    /// Hex pubkey of the followed user.
    pub pubkey: String,
    /// Recommended relay URL for this contact.
    pub relay_url: Option<String>,
    /// Pet name / alias.
    pub petname: Option<String>,
}

/// Build a kind-3 contact list event from a list of follows.
pub fn build_contact_list(
    follows: &[Contact],
    author_pubkey: &[u8; 32],
    created_at: u64,
) -> UnsignedEvent {
    let tags: Vec<Tag> = follows
        .iter()
        .map(|c| {
            Tag::new(vec![
                "p".into(),
                c.pubkey.clone(),
                c.relay_url.clone().unwrap_or_default(),
                c.petname.clone().unwrap_or_default(),
            ])
        })
        .collect();

    UnsignedEvent {
        pubkey: *author_pubkey,
        created_at,
        kind: kind::CONTACT_LIST,
        tags,
        content: String::new(),
    }
}

/// Parse a kind-3 contact list event into contacts.
pub fn parse_contact_list(event: &Event) -> Vec<Contact> {
    if event.kind != kind::CONTACT_LIST {
        return vec![];
    }
    event
        .tags
        .iter()
        .filter(|t| t.name() == Some("p"))
        .map(|t| Contact {
            pubkey: t.value().unwrap_or("").to_string(),
            relay_url: t.get(2).filter(|s| !s.is_empty()).map(|s| s.to_string()),
            petname: t.get(3).filter(|s| !s.is_empty()).map(|s| s.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_contact_list_roundtrip() {
        let contacts = vec![
            Contact {
                pubkey: "a".repeat(64),
                relay_url: Some("wss://relay.example.com".into()),
                petname: Some("alice".into()),
            },
            Contact {
                pubkey: "b".repeat(64),
                relay_url: None,
                petname: None,
            },
        ];
        let unsigned = build_contact_list(&contacts, &[1u8; 32], 1000);
        assert_eq!(unsigned.kind, kind::CONTACT_LIST);
        assert_eq!(unsigned.tags.len(), 2);

        // Simulate a signed event for parsing
        let event = Event {
            id: "x".repeat(64),
            pubkey: hex::encode([1u8; 32]),
            created_at: unsigned.created_at,
            kind: unsigned.kind,
            tags: unsigned.tags,
            content: unsigned.content,
            sig: "s".repeat(128),
        };

        let parsed = parse_contact_list(&event);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].pubkey, "a".repeat(64));
        assert_eq!(
            parsed[0].relay_url.as_deref(),
            Some("wss://relay.example.com")
        );
        assert_eq!(parsed[0].petname.as_deref(), Some("alice"));
        assert_eq!(parsed[1].pubkey, "b".repeat(64));
        assert!(parsed[1].relay_url.is_none());
        assert!(parsed[1].petname.is_none());
    }

    #[test]
    fn parse_wrong_kind_returns_empty() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: 1, // not kind 3
            tags: vec![Tag::new(vec!["p".into(), "z".repeat(64)])],
            content: "".into(),
            sig: "s".repeat(128),
        };
        assert!(parse_contact_list(&event).is_empty());
    }
}
