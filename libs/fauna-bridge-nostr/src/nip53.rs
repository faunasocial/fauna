use crate::types::{Event, kind};

/// A parsed NIP-53 live activity (kind 30311).
#[derive(Debug, Clone, PartialEq)]
pub struct LiveActivity {
    /// Value of the "d" tag — the activity's unique identifier.
    pub identifier: String,
    /// Human-readable title from the "title" tag.
    pub title: Option<String>,
    /// Short summary from the "summary" tag.
    pub summary: Option<String>,
    /// HLS or other streaming URL from the "streaming" tag.
    pub streaming_url: Option<String>,
    /// Activity status from the "status" tag (e.g. "live", "ended", "planned").
    pub status: String,
    /// Unix timestamp of scheduled start from the "starts" tag.
    pub starts_at: Option<u64>,
    /// Current viewer/participant count from the "current_participants" tag.
    pub current_participants: Option<u64>,
    /// Cover image URL from the "image" tag.
    pub image: Option<String>,
    /// Hashtags from "t" tags.
    pub hashtags: Vec<String>,
}

/// Parse a kind-30311 event into a [`LiveActivity`].
///
/// Returns an error if the event is not kind 30311 or the required `status` tag is missing.
pub fn parse_live_activity(event: &Event) -> anyhow::Result<LiveActivity> {
    if event.kind != kind::LIVE_ACTIVITY {
        anyhow::bail!("expected kind {}, got {}", kind::LIVE_ACTIVITY, event.kind);
    }

    let find_tag = |name: &str| -> Option<&str> {
        event
            .tags
            .iter()
            .find(|t| t.name() == Some(name))
            .and_then(|t| t.value())
    };

    let status = find_tag("status")
        .ok_or_else(|| anyhow::anyhow!("live activity missing required `status` tag"))?
        .to_string();

    let identifier = find_tag("d").unwrap_or("").to_string();
    let title = find_tag("title").map(|s| s.to_string());
    let summary = find_tag("summary").map(|s| s.to_string());
    let streaming_url = find_tag("streaming").map(|s| s.to_string());
    let image = find_tag("image").map(|s| s.to_string());

    let starts_at = find_tag("starts").and_then(|s| s.parse::<u64>().ok());
    let current_participants = find_tag("current_participants").and_then(|s| s.parse::<u64>().ok());

    let hashtags = event
        .tags
        .iter()
        .filter(|t| t.name() == Some("t"))
        .filter_map(|t| t.value())
        .map(|s| s.to_string())
        .collect();

    Ok(LiveActivity {
        identifier,
        title,
        summary,
        streaming_url,
        status,
        starts_at,
        current_participants,
        image,
        hashtags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn make_live_activity(tags: Vec<Tag>) -> Event {
        Event {
            id: "a".repeat(64),
            pubkey: "b".repeat(64),
            created_at: 1_700_000_000,
            kind: kind::LIVE_ACTIVITY,
            tags,
            content: String::new(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn parse_live_activity_all_fields() {
        let event = make_live_activity(vec![
            Tag::new(vec!["d".into(), "my-stream-001".into()]),
            Tag::new(vec!["title".into(), "Rust Programming Live".into()]),
            Tag::new(vec!["summary".into(), "Live coding session".into()]),
            Tag::new(vec![
                "streaming".into(),
                "https://stream.example.com/live.m3u8".into(),
            ]),
            Tag::new(vec!["status".into(), "live".into()]),
            Tag::new(vec!["starts".into(), "1700000000".into()]),
            Tag::new(vec!["current_participants".into(), "42".into()]),
            Tag::new(vec!["image".into(), "https://example.com/cover.jpg".into()]),
            Tag::new(vec!["t".into(), "rust".into()]),
            Tag::new(vec!["t".into(), "programming".into()]),
        ]);

        let activity = parse_live_activity(&event).unwrap();

        assert_eq!(activity.identifier, "my-stream-001");
        assert_eq!(activity.title, Some("Rust Programming Live".into()));
        assert_eq!(activity.summary, Some("Live coding session".into()));
        assert_eq!(
            activity.streaming_url,
            Some("https://stream.example.com/live.m3u8".into())
        );
        assert_eq!(activity.status, "live");
        assert_eq!(activity.starts_at, Some(1_700_000_000));
        assert_eq!(activity.current_participants, Some(42));
        assert_eq!(activity.image, Some("https://example.com/cover.jpg".into()));
        assert_eq!(
            activity.hashtags,
            vec!["rust".to_string(), "programming".to_string()]
        );
    }

    #[test]
    fn parse_live_activity_minimal_fields() {
        let event = make_live_activity(vec![
            Tag::new(vec!["d".into(), "minimal-stream".into()]),
            Tag::new(vec!["status".into(), "planned".into()]),
        ]);

        let activity = parse_live_activity(&event).unwrap();

        assert_eq!(activity.identifier, "minimal-stream");
        assert!(activity.title.is_none());
        assert!(activity.summary.is_none());
        assert!(activity.streaming_url.is_none());
        assert_eq!(activity.status, "planned");
        assert!(activity.starts_at.is_none());
        assert!(activity.current_participants.is_none());
        assert!(activity.image.is_none());
        assert!(activity.hashtags.is_empty());
    }

    #[test]
    fn parse_live_activity_status_values() {
        for status in &["live", "ended", "planned"] {
            let event = make_live_activity(vec![
                Tag::new(vec!["d".into(), "test".into()]),
                Tag::new(vec!["status".into(), (*status).into()]),
            ]);
            let activity = parse_live_activity(&event).unwrap();
            assert_eq!(&activity.status, status);
        }
    }

    #[test]
    fn parse_live_activity_missing_status_errors() {
        let event = make_live_activity(vec![
            Tag::new(vec!["d".into(), "no-status".into()]),
            Tag::new(vec!["title".into(), "Some Title".into()]),
        ]);
        assert!(parse_live_activity(&event).is_err());
    }

    #[test]
    fn parse_live_activity_wrong_kind_errors() {
        let mut event = make_live_activity(vec![Tag::new(vec!["status".into(), "live".into()])]);
        event.kind = 1;
        assert!(parse_live_activity(&event).is_err());
    }
}
