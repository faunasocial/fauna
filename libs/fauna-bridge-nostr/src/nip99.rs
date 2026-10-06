use crate::types::{Event, kind};

/// A parsed NIP-99 classified listing (kind 30402).
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedListing {
    /// Value of the "d" tag — the listing's unique identifier.
    pub identifier: String,
    /// Human-readable title from the "title" tag (required).
    pub title: String,
    /// Short summary from the "summary" tag.
    pub summary: Option<String>,
    /// Price string from the "price" tag (e.g. "10 USD").
    pub price: Option<String>,
    /// Location string from the "location" tag.
    pub location: Option<String>,
    /// Image URLs from "image" tags.
    pub images: Vec<String>,
    /// Item condition from the "condition" tag (e.g. "new", "used").
    pub condition: Option<String>,
    /// Full listing description (event content).
    pub content: String,
}

/// Parse a kind-30402 event into a [`ClassifiedListing`].
///
/// Returns an error if the event is not kind 30402 or the required `title` tag is missing.
pub fn parse_classified(event: &Event) -> anyhow::Result<ClassifiedListing> {
    if event.kind != kind::CLASSIFIED {
        anyhow::bail!("expected kind {}, got {}", kind::CLASSIFIED, event.kind);
    }

    let find_tag = |name: &str| -> Option<&str> {
        event
            .tags
            .iter()
            .find(|t| t.name() == Some(name))
            .and_then(|t| t.value())
    };

    let title = find_tag("title")
        .ok_or_else(|| anyhow::anyhow!("classified listing missing required `title` tag"))?
        .to_string();

    let identifier = find_tag("d").unwrap_or("").to_string();
    let summary = find_tag("summary").map(|s| s.to_string());
    let price = find_tag("price").map(|s| s.to_string());
    let location = find_tag("location").map(|s| s.to_string());
    let condition = find_tag("condition").map(|s| s.to_string());

    let images = event
        .tags
        .iter()
        .filter(|t| t.name() == Some("image"))
        .filter_map(|t| t.value())
        .map(|s| s.to_string())
        .collect();

    Ok(ClassifiedListing {
        identifier,
        title,
        summary,
        price,
        location,
        images,
        condition,
        content: event.content.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn make_classified(tags: Vec<Tag>, content: &str) -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: kind::CLASSIFIED,
            tags,
            content: content.to_string(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn parse_classified_all_fields() {
        let event = make_classified(
            vec![
                Tag::new(vec!["d".into(), "listing-001".into()]),
                Tag::new(vec!["title".into(), "Vintage Bicycle".into()]),
                Tag::new(vec!["summary".into(), "A beautiful 1970s road bike".into()]),
                Tag::new(vec!["price".into(), "250 USD".into()]),
                Tag::new(vec!["location".into(), "Portland, OR".into()]),
                Tag::new(vec!["image".into(), "https://example.com/bike1.jpg".into()]),
                Tag::new(vec!["image".into(), "https://example.com/bike2.jpg".into()]),
                Tag::new(vec!["condition".into(), "used".into()]),
            ],
            "Full description of the vintage bicycle listing.",
        );

        let listing = parse_classified(&event).unwrap();

        assert_eq!(listing.identifier, "listing-001");
        assert_eq!(listing.title, "Vintage Bicycle");
        assert_eq!(listing.summary, Some("A beautiful 1970s road bike".into()));
        assert_eq!(listing.price, Some("250 USD".into()));
        assert_eq!(listing.location, Some("Portland, OR".into()));
        assert_eq!(
            listing.images,
            vec![
                "https://example.com/bike1.jpg".to_string(),
                "https://example.com/bike2.jpg".to_string(),
            ]
        );
        assert_eq!(listing.condition, Some("used".into()));
        assert_eq!(
            listing.content,
            "Full description of the vintage bicycle listing."
        );
    }

    #[test]
    fn parse_classified_missing_optional_fields() {
        let event = make_classified(
            vec![
                Tag::new(vec!["d".into(), "minimal-listing".into()]),
                Tag::new(vec!["title".into(), "Old Chair".into()]),
            ],
            "Just a chair.",
        );

        let listing = parse_classified(&event).unwrap();

        assert_eq!(listing.identifier, "minimal-listing");
        assert_eq!(listing.title, "Old Chair");
        assert!(listing.summary.is_none());
        assert!(listing.price.is_none());
        assert!(listing.location.is_none());
        assert!(listing.images.is_empty());
        assert!(listing.condition.is_none());
        assert_eq!(listing.content, "Just a chair.");
    }

    #[test]
    fn parse_classified_missing_title_errors() {
        let event = make_classified(
            vec![Tag::new(vec!["d".into(), "no-title".into()])],
            "content",
        );
        assert!(parse_classified(&event).is_err());
    }

    #[test]
    fn parse_classified_wrong_kind_errors() {
        let mut event = make_classified(
            vec![Tag::new(vec!["title".into(), "Test".into()])],
            "content",
        );
        event.kind = 1;
        assert!(parse_classified(&event).is_err());
    }
}
