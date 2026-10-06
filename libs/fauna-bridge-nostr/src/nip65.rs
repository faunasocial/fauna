use crate::types::{Event, kind};

/// How a relay is used by a user (NIP-65).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayUsage {
    Read,
    Write,
    ReadWrite,
}

/// A relay entry from a kind-10002 event.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayEntry {
    pub url: String,
    pub usage: RelayUsage,
}

/// Parse a kind-10002 relay list metadata event (NIP-65).
///
/// Each "r" tag is: `["r", "<relay_url>"]` (read+write) or
/// `["r", "<relay_url>", "read"]` or `["r", "<relay_url>", "write"]`.
pub fn parse_relay_list(event: &Event) -> Vec<RelayEntry> {
    if event.kind != kind::RELAY_LIST {
        return vec![];
    }
    event
        .tags
        .iter()
        .filter(|t| t.name() == Some("r"))
        .filter_map(|t| {
            let url = t.value()?.to_string();
            let usage = match t.get(2) {
                Some("read") => RelayUsage::Read,
                Some("write") => RelayUsage::Write,
                _ => RelayUsage::ReadWrite,
            };
            Some(RelayEntry { url, usage })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    #[test]
    fn parse_relay_list_full() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: kind::RELAY_LIST,
            tags: vec![
                Tag::new(vec!["r".into(), "wss://relay1.example.com".into()]),
                Tag::new(vec![
                    "r".into(),
                    "wss://relay2.example.com".into(),
                    "read".into(),
                ]),
                Tag::new(vec![
                    "r".into(),
                    "wss://relay3.example.com".into(),
                    "write".into(),
                ]),
            ],
            content: "".into(),
            sig: "s".repeat(128),
        };

        let relays = parse_relay_list(&event);
        assert_eq!(relays.len(), 3);
        assert_eq!(relays[0].url, "wss://relay1.example.com");
        assert_eq!(relays[0].usage, RelayUsage::ReadWrite);
        assert_eq!(relays[1].url, "wss://relay2.example.com");
        assert_eq!(relays[1].usage, RelayUsage::Read);
        assert_eq!(relays[2].url, "wss://relay3.example.com");
        assert_eq!(relays[2].usage, RelayUsage::Write);
    }

    #[test]
    fn parse_relay_list_wrong_kind_returns_empty() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: 1,
            tags: vec![Tag::new(vec!["r".into(), "wss://relay.com".into()])],
            content: "".into(),
            sig: "s".repeat(128),
        };
        assert!(parse_relay_list(&event).is_empty());
    }

    #[test]
    fn parse_relay_list_empty_tags() {
        let event = Event {
            id: "x".repeat(64),
            pubkey: "y".repeat(64),
            created_at: 1000,
            kind: kind::RELAY_LIST,
            tags: vec![],
            content: "".into(),
            sig: "s".repeat(128),
        };
        assert!(parse_relay_list(&event).is_empty());
    }
}
