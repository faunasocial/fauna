use crate::types::{Event, Tag, UnsignedEvent};

/// Kind number for NIP-23 long-form articles.
pub const KIND_LONG_FORM: u64 = 30023;

/// A parsed NIP-23 long-form article.
#[derive(Debug, Clone, PartialEq)]
pub struct Article {
    pub title: String,
    pub summary: Option<String>,
    pub image: Option<String>,
    pub published_at: Option<u64>,
    pub content_markdown: String,
    pub hashtags: Vec<String>,
    /// Value of the "d" tag (unique identifier for replaceable events).
    pub identifier: String,
}

/// Parse a kind-30023 event into an Article.
pub fn parse_article(event: &Event) -> anyhow::Result<Article> {
    if event.kind != KIND_LONG_FORM {
        anyhow::bail!("expected kind {KIND_LONG_FORM}, got {}", event.kind);
    }

    let find_tag = |name: &str| -> Option<&str> {
        event
            .tags
            .iter()
            .find(|t| t.name() == Some(name))
            .and_then(|t| t.value())
    };

    let title = find_tag("title").unwrap_or("").to_string();
    let summary = find_tag("summary").map(|s| s.to_string());
    let image = find_tag("image").map(|s| s.to_string());
    let published_at = find_tag("published_at").and_then(|s| s.parse::<u64>().ok());
    let identifier = find_tag("d").unwrap_or("").to_string();

    let hashtags = event
        .tags
        .iter()
        .filter(|t| t.name() == Some("t"))
        .filter_map(|t| t.value())
        .map(|s| s.to_string())
        .collect();

    Ok(Article {
        title,
        summary,
        image,
        published_at,
        content_markdown: event.content.clone(),
        hashtags,
        identifier,
    })
}

/// Build a kind-30023 unsigned event from an Article.
pub fn build_article_event(article: &Article, pubkey: &[u8; 32], created_at: u64) -> UnsignedEvent {
    let mut tags = Vec::new();

    tags.push(Tag::new(vec!["d".into(), article.identifier.clone()]));
    tags.push(Tag::new(vec!["title".into(), article.title.clone()]));

    if let Some(summary) = &article.summary {
        tags.push(Tag::new(vec!["summary".into(), summary.clone()]));
    }

    if let Some(image) = &article.image {
        tags.push(Tag::new(vec!["image".into(), image.clone()]));
    }

    if let Some(published_at) = article.published_at {
        tags.push(Tag::new(vec![
            "published_at".into(),
            published_at.to_string(),
        ]));
    }

    for hashtag in &article.hashtags {
        tags.push(Tag::new(vec!["t".into(), hashtag.clone()]));
    }

    UnsignedEvent {
        pubkey: *pubkey,
        created_at,
        kind: KIND_LONG_FORM,
        tags,
        content: article.content_markdown.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_article_event(
        identifier: &str,
        title: &str,
        summary: Option<&str>,
        image: Option<&str>,
        published_at: Option<u64>,
        hashtags: &[&str],
        content: &str,
    ) -> Event {
        let mut tags = vec![
            Tag::new(vec!["d".into(), identifier.into()]),
            Tag::new(vec!["title".into(), title.into()]),
        ];
        if let Some(s) = summary {
            tags.push(Tag::new(vec!["summary".into(), s.into()]));
        }
        if let Some(img) = image {
            tags.push(Tag::new(vec!["image".into(), img.into()]));
        }
        if let Some(ts) = published_at {
            tags.push(Tag::new(vec!["published_at".into(), ts.to_string()]));
        }
        for ht in hashtags {
            tags.push(Tag::new(vec!["t".into(), (*ht).into()]));
        }

        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: KIND_LONG_FORM,
            tags,
            content: content.into(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn parse_full_article() {
        let event = make_article_event(
            "my-article",
            "My Article Title",
            Some("A brief summary"),
            Some("https://example.com/image.png"),
            Some(1_699_000_000),
            &["rust", "nostr"],
            "# Hello\n\nThis is the body.",
        );

        let article = parse_article(&event).unwrap();

        assert_eq!(article.identifier, "my-article");
        assert_eq!(article.title, "My Article Title");
        assert_eq!(article.summary, Some("A brief summary".into()));
        assert_eq!(article.image, Some("https://example.com/image.png".into()));
        assert_eq!(article.published_at, Some(1_699_000_000));
        assert_eq!(article.hashtags, vec!["rust", "nostr"]);
        assert_eq!(article.content_markdown, "# Hello\n\nThis is the body.");
    }

    #[test]
    fn parse_article_missing_optional_fields() {
        let event = make_article_event(
            "minimal",
            "Minimal Article",
            None,
            None,
            None,
            &[],
            "Just content.",
        );

        let article = parse_article(&event).unwrap();

        assert_eq!(article.identifier, "minimal");
        assert_eq!(article.title, "Minimal Article");
        assert!(article.summary.is_none());
        assert!(article.image.is_none());
        assert!(article.published_at.is_none());
        assert!(article.hashtags.is_empty());
        assert_eq!(article.content_markdown, "Just content.");
    }

    #[test]
    fn wrong_kind_returns_error() {
        let event = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![],
            content: "not an article".into(),
            sig: "s".repeat(128),
        };
        assert!(parse_article(&event).is_err());
    }

    #[test]
    fn build_article_event_roundtrip() {
        let original = Article {
            title: "Roundtrip Test".into(),
            summary: Some("Testing roundtrip".into()),
            image: Some("https://example.com/img.jpg".into()),
            published_at: Some(1_700_000_000),
            content_markdown: "## Section\n\nParagraph here.".into(),
            hashtags: vec!["test".into(), "nostr".into()],
            identifier: "roundtrip-test".into(),
        };

        let pubkey = [7u8; 32];
        let created_at = 1_700_100_000u64;

        let unsigned = build_article_event(&original, &pubkey, created_at);

        assert_eq!(unsigned.kind, KIND_LONG_FORM);
        assert_eq!(unsigned.content, original.content_markdown);
        assert_eq!(unsigned.created_at, created_at);

        // Reconstruct a signed Event to parse back
        let signed = Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: unsigned.created_at,
            kind: unsigned.kind,
            tags: unsigned.tags.clone(),
            content: unsigned.content.clone(),
            sig: "s".repeat(128),
        };

        let parsed = parse_article(&signed).unwrap();

        assert_eq!(parsed.identifier, original.identifier);
        assert_eq!(parsed.title, original.title);
        assert_eq!(parsed.summary, original.summary);
        assert_eq!(parsed.image, original.image);
        assert_eq!(parsed.published_at, original.published_at);
        assert_eq!(parsed.hashtags, original.hashtags);
        assert_eq!(parsed.content_markdown, original.content_markdown);
    }

    #[test]
    fn content_markdown_preserved_exactly() {
        let complex_md = "# Title\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n> A blockquote\n\n* item 1\n* item 2\n";

        let event = make_article_event(
            "code-article",
            "Code Article",
            None,
            None,
            None,
            &[],
            complex_md,
        );

        let article = parse_article(&event).unwrap();
        assert_eq!(article.content_markdown, complex_md);
    }
}
