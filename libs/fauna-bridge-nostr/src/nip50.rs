//! NIP-50 search — shared tokenization + FTS5 MATCH-query construction.
//!
//! Both consumers of a filter's `search` field derive their verdict from the
//! same tokenization here, so the two stay as close as two matchers can be
//! (`docs/goal/ui/nostr.md` § The relay event store — NIP-50 bullet):
//!
//! - the nest relay's store-read path builds a sanitized SQLite FTS5 `MATCH`
//!   string via [`fts5_match_query`] — the authoritative verdict for the
//!   search dimension on REQ replay and COUNT;
//! - [`crate::types::Filter::matches`] applies the case-folded
//!   token-containment approximation over [`search_tokens`] — the only
//!   verdict available on the live-broadcast path (including ephemeral
//!   events, which never reach a store).
//!
//! Divergence between the two is a completeness miss (an event not
//! delivered), never a leak.

/// Hard cap on the bytes of a `search` string this module will tokenize —
/// anything past it is ignored. A hard-coded protocol constant, never a
/// configuration surface. Security bound, not a UX limit: the `search`
/// field arrives on an **unauthenticated** REQ/COUNT and feeds an FTS5
/// `MATCH`, so an unbounded string is an unauthenticated CPU
/// sink.
/// Generous for any human query; the relay endpoint additionally rejects
/// an over-cap search loudly (`CLOSED`) before querying — the truncation
/// here is the structural backstop no future serving verb can forget.
pub const MAX_SEARCH_BYTES: usize = 256;

/// Hard cap on the searchable tokens taken from a `search` string; the
/// same structural-bound rationale as [`MAX_SEARCH_BYTES`].
pub const MAX_SEARCH_TOKENS: usize = 16;

/// True when a `search` string exceeds the relay caps — the loud-rejection
/// predicate serving endpoints check *before* querying (the counterpart of
/// the silent structural truncation inside [`search_tokens`]).
pub fn exceeds_search_caps(query: &str) -> bool {
    if query.len() > MAX_SEARCH_BYTES {
        return true;
    }
    query
        .split_whitespace()
        .filter(|tok| !is_extension_token(tok))
        .count()
        > MAX_SEARCH_TOKENS
}

/// Lowercased free-text tokens of a NIP-50 `search` string.
///
/// `word:value` extension tokens (e.g. `include:spam`, `language:en`) are
/// stripped — NIP-50 says relays SHOULD ignore extensions they don't
/// support, and this relay supports none. A token is an extension when a
/// `:` splits it into a non-empty all-ASCII-alphabetic key and a non-empty
/// value; anything else (`12:30`, a lone `:`) stays a literal search token.
///
/// Structurally bounded: only the first [`MAX_SEARCH_BYTES`] of the query
/// are tokenized and at most [`MAX_SEARCH_TOKENS`] tokens are returned, so
/// no consumer ([`fts5_match_query`], `Filter::matches`) can be driven to
/// unbounded work by a hostile search string — even on a path that forgot
/// the loud [`exceeds_search_caps`] gate.
pub fn search_tokens(query: &str) -> Vec<String> {
    fauna_core::encoding::truncate_to_char_boundary(query, MAX_SEARCH_BYTES)
        .split_whitespace()
        .filter(|tok| !is_extension_token(tok))
        .map(|tok| tok.to_lowercase())
        .take(MAX_SEARCH_TOKENS)
        .collect()
}

fn is_extension_token(token: &str) -> bool {
    match token.split_once(':') {
        Some((key, value)) => {
            !key.is_empty() && !value.is_empty() && key.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// The sanitized SQLite FTS5 `MATCH` string for a NIP-50 `search` query:
/// each token individually double-quoted (embedded `"` doubled — FTS5's own
/// escape), joined by spaces (FTS5 implicit AND). Quoting neutralizes every
/// FTS5 query operator (`OR`, `NOT`, `NEAR(`, `col:`, `*`, `^`), so a hostile
/// search string can narrow results but never error the query or reach
/// operator semantics.
///
/// `None` when no searchable tokens remain (empty or extensions-only query) —
/// the caller then adds no MATCH clause at all: an empty search matches
/// everything, it doesn't match nothing. [`crate::types::Filter::matches`]
/// mirrors that by passing when [`search_tokens`] is empty.
pub fn fts5_match_query(query: &str) -> Option<String> {
    let tokens = search_tokens(query);
    if tokens.is_empty() {
        return None;
    }
    let quoted: Vec<String> = tokens
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    Some(quoted.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_lowercase_and_split_on_whitespace() {
        assert_eq!(
            search_tokens("Hello  WORLD\tpic-nic"),
            vec!["hello", "world", "pic-nic"]
        );
    }

    #[test]
    fn extension_tokens_are_stripped() {
        assert_eq!(
            search_tokens("picnic include:spam language:en"),
            vec!["picnic"]
        );
    }

    #[test]
    fn non_extension_colon_tokens_survive() {
        // Numeric key, empty key, empty value, bare colon: all literal.
        assert_eq!(
            search_tokens("12:30 :x x: :"),
            vec!["12:30", ":x", "x:", ":"]
        );
    }

    #[test]
    fn empty_and_extensions_only_yield_no_match_query() {
        assert_eq!(fts5_match_query(""), None);
        assert_eq!(fts5_match_query("   "), None);
        assert_eq!(fts5_match_query("include:spam nsfw:false"), None);
    }

    #[test]
    fn match_query_quotes_every_token() {
        assert_eq!(
            fts5_match_query("Hello world").as_deref(),
            Some("\"hello\" \"world\"")
        );
    }

    #[test]
    fn huge_single_token_search_is_structurally_bounded() {
        // A no-whitespace multi-MB string (the cheapest hostile shape) must
        // never reach an FTS5 MATCH at full size — the tokenizer truncates
        // to MAX_SEARCH_BYTES before any consumer sees it.
        let hostile = "x".repeat(1024 * 1024);
        let m = fts5_match_query(&hostile).expect("one token survives");
        // Quoted + doubled-quote overhead can at most double the capped input.
        assert!(
            m.len() <= 2 * MAX_SEARCH_BYTES + 2,
            "MATCH string must be bounded, got {} bytes",
            m.len()
        );
        assert_eq!(search_tokens(&hostile)[0].len(), MAX_SEARCH_BYTES);
    }

    #[test]
    fn huge_token_count_search_is_structurally_bounded() {
        let hostile = "a ".repeat(100_000);
        assert_eq!(search_tokens(&hostile).len(), MAX_SEARCH_TOKENS);
        let m = fts5_match_query(&hostile).expect("tokens survive");
        assert_eq!(m.matches("\"a\"").count(), MAX_SEARCH_TOKENS);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        // A multi-byte char straddling the byte cap must not panic the slice.
        let mut q = "x".repeat(MAX_SEARCH_BYTES - 1);
        q.push('é'); // 2 bytes — straddles the 256-byte boundary
        let toks = search_tokens(&q);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].len(), MAX_SEARCH_BYTES - 1, "é dropped whole");
    }

    #[test]
    fn exceeds_search_caps_boundaries() {
        // At the caps: fine.
        assert!(!exceeds_search_caps(&"x".repeat(MAX_SEARCH_BYTES)));
        assert!(!exceeds_search_caps(&"a ".repeat(MAX_SEARCH_TOKENS)));
        // One past either cap: rejected.
        assert!(exceeds_search_caps(&"x".repeat(MAX_SEARCH_BYTES + 1)));
        assert!(exceeds_search_caps(&"a ".repeat(MAX_SEARCH_TOKENS + 1)));
        // Extension tokens don't count toward the token cap (they're
        // stripped before matching) — but bytes always count.
        let ext_heavy = "include:spam ".repeat(MAX_SEARCH_BYTES / 13 + 1); // >256 bytes
        assert!(exceeds_search_caps(&ext_heavy), "byte cap still applies");
        assert!(!exceeds_search_caps("picnic include:spam language:en"));
    }

    #[test]
    fn fts5_operators_are_neutralized_by_quoting() {
        // Operator words and syntax become quoted literals, never operators.
        assert_eq!(
            fts5_match_query("a OR b").as_deref(),
            Some("\"a\" \"or\" \"b\"")
        );
        assert_eq!(fts5_match_query("NEAR(x").as_deref(), Some("\"near(x\""));
        // An embedded double quote is doubled — the FTS5 string escape.
        assert_eq!(fts5_match_query("a\"b").as_deref(), Some("\"a\"\"b\""));
    }
}
