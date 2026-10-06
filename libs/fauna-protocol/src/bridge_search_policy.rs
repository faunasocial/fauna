//! The uniform per-bridge Search-corpus policy — the shared half.
//!
//! Owner doc: `docs/goal/behavior/content-index.md` § Bridge content in the
//! Search corpus (ratified 2026-07-22, user-ruled). The rule that shapes this
//! module: the policy is **one policy for every bridge, no per-bridge special
//! cases**, so the two controls are *registry-supplied* — assembled here, once,
//! from constants — and never copied into a provider's own settings list.
//!
//! What lives here (shared, because both the nest that serves the rows and any
//! client that reasons about them need the same answers): the two setting keys,
//! their labels, the `number` setting type, the default cap, the content-bridge
//! predicate, and the `content_fts` content-type string. What deliberately does
//! **not** live here: persistence and enforcement (nest state + the ingest
//! hooks), which are nest-side by construction.

use crate::bridges_ui::BridgeSetting;
use fauna_cbor::Value;

/// Setting key: does this bridge's public content enter the Search corpus.
pub const SHOW_IN_SEARCH_KEY: &str = "show_in_search";

/// Setting key: how many of this bridge's newest posts stay indexed.
pub const SEARCH_POST_LIMIT_KEY: &str = "limit_posts_in_search";

/// The additive `setting_type` the cap needs (`BridgeSetting.setting_type` was
/// bool/text/enum before this). Additive evolution: a client that does not know
/// it renders the row read-only rather than breaking — the editable control is
/// the follow-on per-app slice.
pub const SETTING_TYPE_NUMBER: &str = "number";

/// Default cap — a hard Rust constant, per the works-out-of-the-box invariant
/// (content-index.md § Bridge content in the Search corpus → *Defaults*:
/// "order 1000"). Generous enough that a normal bridge feed's search horizon
/// feels complete, bounded enough that a firehose-grade bridge cannot balloon
/// the nest's FTS table.
pub const DEFAULT_SEARCH_POST_LIMIT: u32 = 1000;

/// Default for the toggle: **ON** (same § *Defaults*).
pub const DEFAULT_SHOW_IN_SEARCH: bool = true;

/// Upper bound accepted for the cap. Nest-side range validation, so a client
/// cannot set a cap that defeats the whole point of the control.
pub const MAX_SEARCH_POST_LIMIT: u32 = 100_000;

/// Whether this bridge is a **content** bridge — one that relays, stores or
/// translates public posts/events, and therefore carries the two search-policy
/// rows. Mail is the all-private case: it contributes nothing to `content_fts`
/// (its search rides the sealed per-user index), so it gets no rows.
pub fn is_content_bridge(bridge_id: &str) -> bool {
    matches!(bridge_id, "nostr" | "bluesky" | "activitypub")
}

/// The `content_fts` content-type (a.k.a. `schema`) for a bridge's corpus rows.
/// The indexed row's key is `blake3("{content_type}:{natural_id}")` — the same
/// keying every other document in that table uses — where the natural id is the
/// bridge's own (nostr: event id; bluesky: at-uri; activitypub: object id).
///
/// Two ingest paths that see the same event therefore land on the same key, so
/// double-surfacing is prevented by construction rather than filtered later.
pub fn content_type_for_bridge(bridge_id: &str) -> String {
    format!("bridge.{bridge_id}")
}

/// The `LIKE` pattern matching every bridge-corpus content type.
pub const BRIDGE_CONTENT_TYPE_PREFIX: &str = "bridge.";

/// Whether a `content_fts` schema string is a bridge-corpus row.
pub fn is_bridge_content_type(schema: &str) -> bool {
    schema.starts_with(BRIDGE_CONTENT_TYPE_PREFIX)
}

/// Clamp a client-supplied cap into the accepted range.
pub fn clamp_post_limit(requested: u32) -> u32 {
    requested.min(MAX_SEARCH_POST_LIMIT)
}

/// The two registry-supplied rows, in the order they render. Called once per
/// content bridge from the `fauna.bridges.list` handler — never from a
/// provider, which is what keeps "no per-provider copies" structural.
pub fn search_policy_settings(show_in_search: bool, post_limit: u32) -> Vec<BridgeSetting> {
    vec![
        BridgeSetting {
            key: SHOW_IN_SEARCH_KEY.to_string(),
            label: "Show in search".to_string(),
            setting_type: "bool".to_string(),
            value: Value::Bool(show_in_search),
            options: None,
            extra: Default::default(),
        },
        BridgeSetting {
            key: SEARCH_POST_LIMIT_KEY.to_string(),
            label: "Limit posts in search".to_string(),
            setting_type: SETTING_TYPE_NUMBER.to_string(),
            value: Value::Integer(post_limit as i128),
            options: None,
            extra: Default::default(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_is_not_a_content_bridge() {
        // The all-private case: mail contributes nothing to `content_fts`, so
        // it must not carry the rows at all (content-index.md § Bridge content
        // in the Search corpus, the class split).
        assert!(!is_content_bridge("mail"));
        assert!(is_content_bridge("nostr"));
        assert!(is_content_bridge("bluesky"));
        assert!(is_content_bridge("activitypub"));
    }

    #[test]
    fn content_type_is_the_documented_shape() {
        assert_eq!(content_type_for_bridge("nostr"), "bridge.nostr");
        assert!(is_bridge_content_type("bridge.nostr"));
        assert!(!is_bridge_content_type("post/text"));
        assert!(!is_bridge_content_type("profile"));
    }

    #[test]
    fn the_two_rows_are_the_documented_pair() {
        let rows = search_policy_settings(true, DEFAULT_SEARCH_POST_LIMIT);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, SHOW_IN_SEARCH_KEY);
        assert_eq!(rows[0].value, Value::Bool(true));
        assert_eq!(rows[1].key, SEARCH_POST_LIMIT_KEY);
        assert_eq!(rows[1].setting_type, SETTING_TYPE_NUMBER);
        assert_eq!(rows[1].value, Value::Integer(1000));
    }

    #[test]
    fn cap_clamps_to_the_accepted_range() {
        assert_eq!(clamp_post_limit(0), 0);
        assert_eq!(clamp_post_limit(500), 500);
        assert_eq!(clamp_post_limit(u32::MAX), MAX_SEARCH_POST_LIMIT);
    }
}
