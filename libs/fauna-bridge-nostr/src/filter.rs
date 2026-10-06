use crate::types::{Event, Filter};

impl Filter {
    /// Check if an event matches this filter.
    /// All present fields are AND conditions.
    pub fn matches(&self, event: &Event) -> bool {
        if let Some(ref ids) = self.ids
            && !ids.iter().any(|id| event.id.starts_with(id))
        {
            return false;
        }

        if let Some(ref authors) = self.authors
            && !authors.iter().any(|a| event.pubkey.starts_with(a))
        {
            return false;
        }

        if let Some(ref kinds) = self.kinds
            && !kinds.contains(&event.kind)
        {
            return false;
        }

        if let Some(since) = self.since
            && event.created_at < since
        {
            return false;
        }

        if let Some(until) = self.until
            && event.created_at > until
        {
            return false;
        }

        // NIP-50 search: the case-folded token-containment approximation of
        // the relay store's authoritative FTS5 verdict (`crate::nip50` module
        // docs). Every non-extension token must appear in the content; an
        // empty/extensions-only search matches everything, mirroring the
        // no-MATCH-clause behavior of `nip50::fts5_match_query`.
        if let Some(ref search) = self.search {
            let content = event.content.to_lowercase();
            if !crate::nip50::search_tokens(search)
                .iter()
                .all(|tok| content.contains(tok.as_str()))
            {
                return false;
            }
        }

        // Tag filters: keys starting with "#" in JSON become just the letter.
        // In our Filter struct, keys in the tags map are single letters (e.g., "e", "p").
        for (tag_name, values) in &self.tags {
            // Strip leading '#' if present (for JSON compat)
            let name = tag_name.strip_prefix('#').unwrap_or(tag_name);
            let has_match = event.tags.iter().any(|tag| {
                tag.name() == Some(name)
                    && tag.value().is_some_and(|v| values.iter().any(|fv| fv == v))
            });
            if !has_match {
                return false;
            }
        }

        true
    }
}

/// Check if an event matches any of the given filters (OR semantics).
pub fn matches_any(filters: &[Filter], event: &Event) -> bool {
    if filters.is_empty() {
        return true;
    }
    filters.iter().any(|f| f.matches(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    fn make_event(id: &str, pubkey: &str, kind: u64, created_at: u64, tags: Vec<Tag>) -> Event {
        Event {
            id: id.to_string(),
            pubkey: pubkey.to_string(),
            created_at,
            kind,
            tags,
            content: "test".to_string(),
            sig: "s".repeat(128),
        }
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = Filter::default();
        let e = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        assert!(f.matches(&e));
    }

    #[test]
    fn filter_by_ids() {
        let f = Filter {
            ids: Some(vec!["a".repeat(64)]),
            ..Default::default()
        };
        let e1 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        let e2 = make_event(
            "c".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        assert!(f.matches(&e1));
        assert!(!f.matches(&e2));
    }

    #[test]
    fn filter_by_authors() {
        let f = Filter {
            authors: Some(vec!["b".repeat(64)]),
            ..Default::default()
        };
        let e1 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        let e2 = make_event(
            "a".repeat(64).as_str(),
            "c".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        assert!(f.matches(&e1));
        assert!(!f.matches(&e2));
    }

    #[test]
    fn filter_by_kinds() {
        let f = Filter {
            kinds: Some(vec![1, 7]),
            ..Default::default()
        };
        let e1 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        let e2 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            3,
            1000,
            vec![],
        );
        assert!(f.matches(&e1));
        assert!(!f.matches(&e2));
    }

    #[test]
    fn filter_by_since_until() {
        let f = Filter {
            since: Some(500),
            until: Some(1500),
            ..Default::default()
        };
        let e1 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        let e2 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            2000,
            vec![],
        );
        let e3 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            100,
            vec![],
        );
        assert!(f.matches(&e1));
        assert!(!f.matches(&e2));
        assert!(!f.matches(&e3));
    }

    #[test]
    fn filter_by_tags() {
        let mut tags_map = std::collections::HashMap::new();
        tags_map.insert("#e".to_string(), vec!["event123".to_string()]);

        let f = Filter {
            tags: tags_map,
            ..Default::default()
        };

        let e1 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![Tag::new(vec!["e".into(), "event123".into()])],
        );
        let e2 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![Tag::new(vec!["e".into(), "other".into()])],
        );
        let e3 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );

        assert!(f.matches(&e1));
        assert!(!f.matches(&e2));
        assert!(!f.matches(&e3));
    }

    #[test]
    fn filter_by_multi_letter_tag_name() {
        // NIP-01 tag filter keys are `#<single-letter>` by spec, but
        // `Filter::matches` checks any literal tag name generically — this is
        // the authoritative in-memory check that closes the gap left by the
        // nest's SQL layer, which only indexes/filters single-letter tags
        // (`bins/fauna-nest/src/nostr/store.rs`
        // `a_non_single_letter_tag_filter_is_not_narrowed_by_sql_alone`).
        let mut tags_map = std::collections::HashMap::new();
        tags_map.insert("subject".to_string(), vec!["keep".to_string()]);
        let f = Filter {
            tags: tags_map,
            ..Default::default()
        };

        let matching = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![Tag::new(vec!["subject".into(), "keep".into()])],
        );
        let wrong_value = make_event(
            "c".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![Tag::new(vec!["subject".into(), "discard".into()])],
        );
        let no_tag = make_event(
            "d".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );

        assert!(f.matches(&matching));
        assert!(!f.matches(&wrong_value));
        assert!(!f.matches(&no_tag));
    }

    #[test]
    fn multiple_filters_or_semantics() {
        let f1 = Filter {
            kinds: Some(vec![1]),
            ..Default::default()
        };
        let f2 = Filter {
            kinds: Some(vec![7]),
            ..Default::default()
        };

        let e1 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        let e2 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            7,
            1000,
            vec![],
        );
        let e3 = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            3,
            1000,
            vec![],
        );

        assert!(matches_any(&[f1.clone(), f2.clone()], &e1));
        assert!(matches_any(&[f1, f2], &e2));
        let f1b = Filter {
            kinds: Some(vec![1]),
            ..Default::default()
        };
        let f2b = Filter {
            kinds: Some(vec![7]),
            ..Default::default()
        };
        assert!(!matches_any(&[f1b, f2b], &e3)); // kind 3 matches neither
    }

    #[test]
    fn empty_filters_matches_all() {
        let e = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        assert!(matches_any(&[], &e));
    }

    #[test]
    fn prefix_matching_for_ids() {
        let f = Filter {
            ids: Some(vec!["abcd".to_string()]),
            ..Default::default()
        };
        let e = make_event("abcdef1234", "b".repeat(64).as_str(), 1, 1000, vec![]);
        assert!(f.matches(&e));
    }

    fn event_with_content(content: &str) -> Event {
        let mut e = make_event(
            "a".repeat(64).as_str(),
            "b".repeat(64).as_str(),
            1,
            1000,
            vec![],
        );
        e.content = content.to_string();
        e
    }

    fn search_filter(q: &str) -> Filter {
        Filter {
            search: Some(q.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn search_matches_case_folded_token_containment() {
        let e = event_with_content("Company picnic on Saturday!");
        assert!(search_filter("PICNIC").matches(&e));
        assert!(search_filter("picnic saturday").matches(&e));
        assert!(!search_filter("picnic sunday").matches(&e));
        assert!(!search_filter("bbq").matches(&e));
    }

    #[test]
    fn search_extension_tokens_are_ignored() {
        let e = event_with_content("a lovely picnic");
        // The extension is stripped; only the free-text token decides.
        assert!(search_filter("picnic include:spam").matches(&e));
        // Extensions-only search matches everything, like an absent search.
        assert!(search_filter("include:spam").matches(&e));
        assert!(search_filter("").matches(&e));
    }

    #[test]
    fn search_combines_and_wise_with_other_dimensions() {
        let e = event_with_content("picnic");
        let mut f = search_filter("picnic");
        f.kinds = Some(vec![7]); // event is kind 1
        assert!(!f.matches(&e));
        f.kinds = Some(vec![1]);
        assert!(f.matches(&e));
    }
}
