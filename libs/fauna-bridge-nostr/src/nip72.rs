use crate::types::{Event, kind};

/// A parsed NIP-72 community definition (kind 34550).
#[derive(Debug, Clone, PartialEq)]
pub struct Community {
    /// Value of the "d" tag — the community's unique identifier.
    pub identifier: String,
    /// Human-readable name from the "name" tag.
    pub name: Option<String>,
    /// Description from the "description" tag.
    pub description: Option<String>,
    /// Image URL from the "image" tag.
    pub image: Option<String>,
    /// Pubkeys of moderators (from "p" tags with marker "moderator").
    pub moderators: Vec<String>,
    /// Community rules from the "rules" tag.
    pub rules: Option<String>,
}

/// Parse a kind-34550 event into a `Community`.
pub fn parse_community(event: &Event) -> anyhow::Result<Community> {
    if event.kind != kind::COMMUNITY_DEFINITION {
        anyhow::bail!(
            "expected kind {}, got {}",
            kind::COMMUNITY_DEFINITION,
            event.kind
        );
    }

    let find_tag = |name: &str| -> Option<&str> {
        event
            .tags
            .iter()
            .find(|t| t.name() == Some(name))
            .and_then(|t| t.value())
    };

    let identifier = find_tag("d").unwrap_or("").to_string();
    let name = find_tag("name").map(|s| s.to_string());
    let description = find_tag("description").map(|s| s.to_string());
    let image = find_tag("image").map(|s| s.to_string());
    let rules = find_tag("rules").map(|s| s.to_string());

    // Collect "p" tags that have "moderator" as a marker.
    // NIP-72 format: ["p", <pubkey>, <relay-hint>, "moderator"]
    // The relay hint may be omitted: ["p", <pubkey>, "moderator"]
    let moderators = event
        .tags
        .iter()
        .filter(|t| {
            t.name() == Some("p")
                && (t.get(2) == Some("moderator") || t.get(3) == Some("moderator"))
        })
        .filter_map(|t| t.value())
        .map(|s| s.to_string())
        .collect();

    Ok(Community {
        identifier,
        name,
        description,
        image,
        moderators,
        rules,
    })
}

/// Check whether an event is a post to a NIP-72 community.
///
/// Returns `Some(identifier)` where `identifier` is the last colon-separated
/// segment of the matching `a` tag value (e.g. `"34550:<pubkey>:<id>"` → `"<id>"`).
/// Returns `None` if the event has no matching `a` tag.
pub fn is_community_post(event: &Event) -> Option<String> {
    event.tags.iter().find_map(|t| {
        if t.name() != Some("a") {
            return None;
        }
        let value = t.value()?;
        if !value.starts_with("34550:") {
            return None;
        }
        // The community identifier is the last segment after the final colon.
        let identifier = value.rsplit(':').next()?;
        Some(identifier.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn make_community_event(
        identifier: &str,
        name: Option<&str>,
        description: Option<&str>,
        image: Option<&str>,
        moderators: &[&str],
        rules: Option<&str>,
    ) -> Event {
        let mut tags = vec![Tag::new(vec!["d".into(), identifier.into()])];

        if let Some(n) = name {
            tags.push(Tag::new(vec!["name".into(), n.into()]));
        }
        if let Some(d) = description {
            tags.push(Tag::new(vec!["description".into(), d.into()]));
        }
        if let Some(img) = image {
            tags.push(Tag::new(vec!["image".into(), img.into()]));
        }
        for pubkey in moderators {
            tags.push(Tag::new(vec![
                "p".into(),
                (*pubkey).into(),
                "".into(), // relay hint
                "moderator".into(),
            ]));
        }
        if let Some(r) = rules {
            tags.push(Tag::new(vec!["rules".into(), r.into()]));
        }

        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: kind::COMMUNITY_DEFINITION,
            tags,
            content: String::new(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn parse_community_all_fields() {
        let mod1 = "c".repeat(64);
        let mod2 = "d".repeat(64);
        let event = make_community_event(
            "rust-developers",
            Some("Rust Developers"),
            Some("A community for Rust enthusiasts"),
            Some("https://example.com/rust.png"),
            &[mod1.as_str(), mod2.as_str()],
            Some("Be respectful. No spam."),
        );

        let community = parse_community(&event).unwrap();

        assert_eq!(community.identifier, "rust-developers");
        assert_eq!(community.name, Some("Rust Developers".into()));
        assert_eq!(
            community.description,
            Some("A community for Rust enthusiasts".into())
        );
        assert_eq!(community.image, Some("https://example.com/rust.png".into()));
        assert_eq!(community.moderators, vec![mod1, mod2]);
        assert_eq!(community.rules, Some("Be respectful. No spam.".into()));
    }

    #[test]
    fn parse_community_minimal() {
        let event = make_community_event("minimal-community", None, None, None, &[], None);

        let community = parse_community(&event).unwrap();

        assert_eq!(community.identifier, "minimal-community");
        assert!(community.name.is_none());
        assert!(community.description.is_none());
        assert!(community.image.is_none());
        assert!(community.moderators.is_empty());
        assert!(community.rules.is_none());
    }

    #[test]
    fn parse_community_wrong_kind_returns_error() {
        let event = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![Tag::new(vec!["d".into(), "some-id".into()])],
            content: String::new(),
            sig: "s".repeat(128),
        };
        assert!(parse_community(&event).is_err());
    }

    #[test]
    fn is_community_post_detects_community_post() {
        let pubkey = "b".repeat(64);
        let a_value = format!("34550:{}:rust-developers", pubkey);
        let event = Event {
            id: "a".repeat(64),
            pubkey: pubkey.clone(),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![Tag::new(vec!["a".into(), a_value])],
            content: "Hello community!".into(),
            sig: "s".repeat(128),
        };

        let result = is_community_post(&event);
        assert_eq!(result, Some("rust-developers".into()));
    }

    #[test]
    fn is_community_post_non_community_returns_none() {
        let event = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![Tag::new(vec![
                "a".into(),
                "30023:somekey:some-article".into(),
            ])],
            content: "Just a regular post".into(),
            sig: "s".repeat(128),
        };

        assert!(is_community_post(&event).is_none());
    }

    #[test]
    fn is_community_post_no_a_tag_returns_none() {
        let event = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            tags: vec![Tag::new(vec!["e".into(), "someref".into()])],
            content: "Post with no a tag".into(),
            sig: "s".repeat(128),
        };

        assert!(is_community_post(&event).is_none());
    }
}
