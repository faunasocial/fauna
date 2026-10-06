use crate::types::{Event, kind};

/// A parsed NIP-58 badge definition (kind 30009).
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeDefinition {
    /// Value of the "d" tag — the badge's unique identifier (slug).
    pub identifier: String,
    /// Human-readable name from the "name" tag.
    pub name: Option<String>,
    /// Description from the "description" tag.
    pub description: Option<String>,
    /// Full-size badge image URL from the "image" tag.
    pub image: Option<String>,
    /// Thumbnail image URL from the "thumb" tag.
    pub thumb: Option<String>,
}

/// A parsed NIP-58 badge award (kind 8).
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeAward {
    /// The "a" tag value referencing the badge definition (kind 30009).
    pub badge_definition: String,
    /// Public keys of the awardees, from "p" tags.
    pub awardees: Vec<String>,
}

/// Parse a kind-30009 event into a [`BadgeDefinition`].
///
/// Returns an error if the event is not kind 30009.
pub fn parse_badge_definition(event: &Event) -> anyhow::Result<BadgeDefinition> {
    if event.kind != kind::BADGE_DEFINITION {
        anyhow::bail!(
            "expected kind {}, got {}",
            kind::BADGE_DEFINITION,
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
    let thumb = find_tag("thumb").map(|s| s.to_string());

    Ok(BadgeDefinition {
        identifier,
        name,
        description,
        image,
        thumb,
    })
}

/// Parse a kind-8 event into a [`BadgeAward`].
///
/// Returns an error if the event is not kind 8 or if the required `a` tag is missing.
pub fn parse_badge_award(event: &Event) -> anyhow::Result<BadgeAward> {
    if event.kind != kind::BADGE_AWARD {
        anyhow::bail!("expected kind {}, got {}", kind::BADGE_AWARD, event.kind);
    }

    let badge_definition = event
        .tags
        .iter()
        .find(|t| t.name() == Some("a"))
        .and_then(|t| t.value())
        .ok_or_else(|| anyhow::anyhow!("badge award missing required `a` tag"))?
        .to_string();

    let awardees = event
        .tags
        .iter()
        .filter(|t| t.name() == Some("p"))
        .filter_map(|t| t.value())
        .map(|s| s.to_string())
        .collect();

    Ok(BadgeAward {
        badge_definition,
        awardees,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn make_badge_definition(tags: Vec<Tag>) -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: kind::BADGE_DEFINITION,
            tags,
            content: String::new(),
            sig: "s".repeat(128),
        }
    }

    fn make_badge_award(tags: Vec<Tag>) -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: kind::BADGE_AWARD,
            tags,
            content: String::new(),
            sig: "s".repeat(128),
        }
    }

    // ── BadgeDefinition ──────────────────────────────────────────────────────

    #[test]
    fn parse_badge_definition_all_fields() {
        let event = make_badge_definition(vec![
            Tag::new(vec!["d".into(), "top-contributor".into()]),
            Tag::new(vec!["name".into(), "Top Contributor".into()]),
            Tag::new(vec![
                "description".into(),
                "Awarded for outstanding contributions".into(),
            ]),
            Tag::new(vec!["image".into(), "https://example.com/badge.png".into()]),
            Tag::new(vec![
                "thumb".into(),
                "https://example.com/badge-thumb.png".into(),
            ]),
        ]);

        let badge = parse_badge_definition(&event).unwrap();

        assert_eq!(badge.identifier, "top-contributor");
        assert_eq!(badge.name, Some("Top Contributor".into()));
        assert_eq!(
            badge.description,
            Some("Awarded for outstanding contributions".into())
        );
        assert_eq!(badge.image, Some("https://example.com/badge.png".into()));
        assert_eq!(
            badge.thumb,
            Some("https://example.com/badge-thumb.png".into())
        );
    }

    #[test]
    fn parse_badge_definition_minimal() {
        let event = make_badge_definition(vec![Tag::new(vec!["d".into(), "minimal-badge".into()])]);

        let badge = parse_badge_definition(&event).unwrap();

        assert_eq!(badge.identifier, "minimal-badge");
        assert!(badge.name.is_none());
        assert!(badge.description.is_none());
        assert!(badge.image.is_none());
        assert!(badge.thumb.is_none());
    }

    #[test]
    fn parse_badge_definition_wrong_kind_errors() {
        let mut event = make_badge_definition(vec![Tag::new(vec!["d".into(), "test".into()])]);
        event.kind = 1;
        assert!(parse_badge_definition(&event).is_err());
    }

    // ── BadgeAward ───────────────────────────────────────────────────────────

    #[test]
    fn parse_badge_award_with_awardees() {
        let pubkey = "b".repeat(64);
        let a_ref = format!("30009:{}:top-contributor", pubkey);
        let awardee1 = "c".repeat(64);
        let awardee2 = "d".repeat(64);

        let event = make_badge_award(vec![
            Tag::new(vec!["a".into(), a_ref.clone()]),
            Tag::new(vec!["p".into(), awardee1.clone()]),
            Tag::new(vec!["p".into(), awardee2.clone()]),
        ]);

        let award = parse_badge_award(&event).unwrap();

        assert_eq!(award.badge_definition, a_ref);
        assert_eq!(award.awardees, vec![awardee1, awardee2]);
    }

    #[test]
    fn parse_badge_award_missing_a_tag_errors() {
        let event = make_badge_award(vec![Tag::new(vec!["p".into(), "c".repeat(64)])]);
        assert!(parse_badge_award(&event).is_err());
    }

    #[test]
    fn parse_badge_award_wrong_kind_errors() {
        let pubkey = "b".repeat(64);
        let a_ref = format!("30009:{}:some-badge", pubkey);
        let mut event = make_badge_award(vec![Tag::new(vec!["a".into(), a_ref])]);
        event.kind = 30009;
        assert!(parse_badge_award(&event).is_err());
    }
}
