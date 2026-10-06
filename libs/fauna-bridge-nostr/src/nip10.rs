use crate::types::Tag;

/// Parsed thread references from NIP-10 `e` and `p` tags.
#[derive(Debug, Clone, Default)]
pub struct ThreadRefs {
    /// The root event of the thread.
    pub root: Option<String>,
    /// The event being directly replied to.
    pub reply: Option<String>,
    /// Mentioned event IDs (neither root nor reply).
    pub mentions: Vec<String>,
    /// Pubkeys involved in the thread.
    pub pubkeys: Vec<String>,
}

/// Parse NIP-10 threading tags from an event's tag list.
///
/// Supports both the preferred marker style (`["e", <id>, <relay>, "root"]`)
/// and the deprecated positional style (first `e` tag = root, last = reply).
pub fn parse_thread_tags(tags: &[Tag]) -> ThreadRefs {
    let mut refs = ThreadRefs::default();
    let mut e_tags: Vec<&Tag> = Vec::new();
    let mut has_markers = false;

    for tag in tags {
        match tag.name() {
            Some("e") => {
                e_tags.push(tag);
                // Check if this tag has a marker (3rd or 4th position)
                if let Some(marker) = tag.get(3) {
                    has_markers = true;
                    let id = tag.value().unwrap_or("").to_string();
                    match marker {
                        "root" => refs.root = Some(id),
                        "reply" => refs.reply = Some(id),
                        "mention" => refs.mentions.push(id),
                        _ => {}
                    }
                }
            }
            Some("p") => {
                if let Some(pk) = tag.value() {
                    refs.pubkeys.push(pk.to_string());
                }
            }
            _ => {}
        }
    }

    // Fallback to positional style if no markers were found
    if !has_markers && !e_tags.is_empty() {
        if e_tags.len() == 1 {
            refs.reply = e_tags[0].value().map(|s| s.to_string());
        } else {
            refs.root = e_tags[0].value().map(|s| s.to_string());
            refs.reply = e_tags.last().and_then(|t| t.value()).map(|s| s.to_string());
            for tag in &e_tags[1..e_tags.len() - 1] {
                if let Some(id) = tag.value() {
                    refs.mentions.push(id.to_string());
                }
            }
        }
    }

    refs
}

/// Build NIP-10 reply tags with root/reply markers.
pub fn build_reply_tags(root_id: Option<&str>, reply_id: &str, reply_pubkey: &str) -> Vec<Tag> {
    let mut tags = Vec::new();

    if let Some(root) = root_id {
        if root != reply_id {
            tags.push(Tag::new(vec![
                "e".into(),
                root.into(),
                "".into(),
                "root".into(),
            ]));
            tags.push(Tag::new(vec![
                "e".into(),
                reply_id.into(),
                "".into(),
                "reply".into(),
            ]));
        } else {
            // Replying directly to root
            tags.push(Tag::new(vec![
                "e".into(),
                root.into(),
                "".into(),
                "root".into(),
            ]));
        }
    } else {
        // No root — this reply creates a new thread
        tags.push(Tag::new(vec![
            "e".into(),
            reply_id.into(),
            "".into(),
            "reply".into(),
        ]));
    }

    tags.push(Tag::new(vec!["p".into(), reply_pubkey.into()]));
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_marker_style() {
        let tags = vec![
            Tag::new(vec!["e".into(), "root1".into(), "".into(), "root".into()]),
            Tag::new(vec!["e".into(), "reply1".into(), "".into(), "reply".into()]),
            Tag::new(vec![
                "e".into(),
                "mention1".into(),
                "".into(),
                "mention".into(),
            ]),
            Tag::new(vec!["p".into(), "pk1".into()]),
        ];
        let refs = parse_thread_tags(&tags);
        assert_eq!(refs.root.as_deref(), Some("root1"));
        assert_eq!(refs.reply.as_deref(), Some("reply1"));
        assert_eq!(refs.mentions, vec!["mention1"]);
        assert_eq!(refs.pubkeys, vec!["pk1"]);
    }

    #[test]
    fn parse_positional_style_single() {
        let tags = vec![Tag::new(vec!["e".into(), "event1".into()])];
        let refs = parse_thread_tags(&tags);
        assert_eq!(refs.root, None);
        assert_eq!(refs.reply.as_deref(), Some("event1"));
    }

    #[test]
    fn parse_positional_style_multiple() {
        let tags = vec![
            Tag::new(vec!["e".into(), "root1".into()]),
            Tag::new(vec!["e".into(), "mid1".into()]),
            Tag::new(vec!["e".into(), "reply1".into()]),
        ];
        let refs = parse_thread_tags(&tags);
        assert_eq!(refs.root.as_deref(), Some("root1"));
        assert_eq!(refs.reply.as_deref(), Some("reply1"));
        assert_eq!(refs.mentions, vec!["mid1"]);
    }

    #[test]
    fn build_reply_with_root() {
        let tags = build_reply_tags(Some("root1"), "reply1", "pk1");
        assert_eq!(tags.len(), 3); // root + reply + p
        assert_eq!(tags[0].get(3), Some("root"));
        assert_eq!(tags[1].get(3), Some("reply"));
        assert_eq!(tags[2].name(), Some("p"));
    }

    #[test]
    fn build_reply_to_root_directly() {
        let tags = build_reply_tags(Some("root1"), "root1", "pk1");
        assert_eq!(tags.len(), 2); // root + p (no separate reply when same as root)
        assert_eq!(tags[0].get(3), Some("root"));
    }

    #[test]
    fn build_reply_no_root() {
        let tags = build_reply_tags(None, "reply1", "pk1");
        assert_eq!(tags.len(), 2); // reply + p
        assert_eq!(tags[0].get(3), Some("reply"));
    }
}
